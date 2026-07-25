//! The world-space model of a board card: how tall it is, where its outputs
//! sit, and the primitives that draw it either directly or as an SVG sprite.

use super::format::{done_footer, format_date, status_message};
use super::theme;
use crate::layout::CARD_WIDTH;
use crate::model::{BoardNode, NodeStatus};
use gpui::{Image, ImageFormat, SharedString, TextAlign};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use unicode_segmentation::UnicodeSegmentation;

pub const PROMPT_LINE_HEIGHT: f32 = 18.;
const HEADER_FIXED_HEIGHT: f32 = 50.;
pub const SHOW_MORE_HEIGHT: f32 = 18.;
pub const ATTACHMENT_ROW_HEIGHT: f32 = 44.;
pub const PROMPT_WRAP_COLUMNS: usize = 42;
pub const COLLAPSED_PROMPT_LINES: usize = 6;
pub const EXPANDED_PROMPT_LINES: usize = 18;
pub const MEDIA_GAP: f32 = 1.;
pub const CARD_SPRITE_WIDTHS: [f32; 4] = [85., 170., 340., 680.];
pub const NO_SPRITE_TIER: u8 = u8::MAX;

#[derive(Clone)]
pub struct CanvasImageAsset {
    pub original: Arc<Path>,
    pub thumbnail: Arc<Path>,
}

pub struct CanvasImage {
    pub url: String,
    pub asset: CanvasImageAsset,
}

pub struct CanvasNode {
    pub node: BoardNode,
    pub prompt_lines: Vec<SharedString>,
    pub collapsed_prompt_lines: Vec<SharedString>,
    pub output_layout: OutputLayout,
    pub displayed_images: Vec<CanvasImage>,
    pub attachment_images: Vec<CanvasImageAsset>,
    pub date: SharedString,
    pub done_footer: SharedString,
    pub status_message: SharedString,
    pub scene: CardScene,
    pub sprite_images: Vec<Arc<Image>>,
    pub last_ready_sprite_tier: AtomicU8,
}

impl CanvasNode {
    /// Builds everything needed to draw one card: its wrapped text, the assets
    /// behind each image, the world-space scene, and one SVG sprite per zoom
    /// tier so the canvas can blit instead of re-drawing at low zoom.
    pub fn build(
        node: &BoardNode,
        prompt_lines: Vec<SharedString>,
        output_layout: OutputLayout,
        expanded: bool,
        mut asset_for: impl FnMut(&str) -> CanvasImageAsset,
    ) -> Self {
        let prompt_lines = if prompt_lines.is_empty() {
            vec![SharedString::default()]
        } else {
            prompt_lines
        };
        let mut card = Self {
            collapsed_prompt_lines: collapse_prompt_lines(&prompt_lines),
            displayed_images: displayed_urls(node)
                .iter()
                .map(|url| CanvasImage {
                    url: url.clone(),
                    asset: asset_for(url),
                })
                .collect(),
            attachment_images: node.attachments.iter().map(|url| asset_for(url)).collect(),
            prompt_lines,
            output_layout,
            date: format_date(node.created_at).into(),
            done_footer: done_footer(node).into(),
            status_message: status_message(node).into(),
            node: node.clone(),
            scene: CardScene::default(),
            sprite_images: Vec::new(),
            last_ready_sprite_tier: AtomicU8::new(NO_SPRITE_TIER),
        };
        card.scene = build_card_scene(&card, expanded);
        card.sprite_images = CARD_SPRITE_WIDTHS
            .into_iter()
            .map(|width| {
                Arc::new(Image::from_bytes(
                    ImageFormat::Svg,
                    card_scene_svg(&card.scene, width).into_bytes(),
                ))
            })
            .collect();
        card
    }
}

/// Truncates the prompt to the collapsed height, marking the cut with an
/// ellipsis on the last visible line.
fn collapse_prompt_lines(lines: &[SharedString]) -> Vec<SharedString> {
    lines
        .iter()
        .take(COLLAPSED_PROMPT_LINES)
        .enumerate()
        .map(|(index, line)| {
            if lines.len() > COLLAPSED_PROMPT_LINES && index + 1 == COLLAPSED_PROMPT_LINES {
                SharedString::from(format!("{line}…"))
            } else {
                line.clone()
            }
        })
        .collect()
}

#[derive(Clone, Copy)]
pub struct CardRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl CardRect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

#[derive(Clone, Copy)]
pub enum CardColor {
    Transparent,
    Background82,
    Raised,
    Hover,
    Line,
    Ink,
    Ink90,
    Dim,
    Faint,
    Accent,
    Accent45,
    Danger,
}

impl CardColor {
    pub fn hsla(self) -> gpui::Hsla {
        match self {
            Self::Transparent => gpui::transparent_black(),
            Self::Background82 => theme::background().opacity(0.82),
            Self::Raised => theme::raised(),
            Self::Hover => theme::hover(),
            Self::Line => theme::line(),
            Self::Ink => theme::ink(),
            Self::Ink90 => theme::ink().opacity(0.9),
            Self::Dim => theme::dim(),
            Self::Faint => theme::faint(),
            Self::Accent => theme::accent(),
            Self::Accent45 => theme::accent().opacity(0.45),
            Self::Danger => theme::danger(),
        }
    }

    pub fn svg(self) -> (&'static str, f32) {
        match self {
            Self::Transparent => ("#000000", 0.),
            Self::Background82 => ("#0d0e12", 0.82),
            Self::Raised => ("#14161c", 1.),
            Self::Hover => ("#1b1e26", 1.),
            Self::Line => ("#262a35", 1.),
            Self::Ink => ("#e8eaf0", 1.),
            Self::Ink90 => ("#e8eaf0", 0.9),
            Self::Dim => ("#8b90a0", 1.),
            Self::Faint => ("#5a5f6e", 1.),
            Self::Accent => ("#7c8cff", 1.),
            Self::Accent45 => ("#7c8cff", 0.45),
            Self::Danger => ("#ff6b6b", 1.),
        }
    }
}

#[derive(Clone, Copy)]
pub enum CardImageFit {
    Contain,
    Cover,
}

pub enum CardPrimitive {
    Quad {
        bounds: CardRect,
        radius: f32,
        fill: CardColor,
        border: Option<(f32, CardColor)>,
    },
    Text {
        text: SharedString,
        bounds: CardRect,
        font_size: f32,
        line_height: f32,
        color: CardColor,
        align: TextAlign,
    },
    Image {
        asset: CanvasImageAsset,
        bounds: CardRect,
        fit: CardImageFit,
        radius: f32,
    },
}

#[derive(Default)]
pub struct CardScene {
    pub height: f32,
    pub primitives: Vec<CardPrimitive>,
}

impl CardScene {
    fn quad(
        &mut self,
        bounds: CardRect,
        radius: f32,
        fill: CardColor,
        border: Option<(f32, CardColor)>,
    ) {
        self.primitives.push(CardPrimitive::Quad {
            bounds,
            radius,
            fill,
            border,
        });
    }

    fn text(
        &mut self,
        text: impl Into<SharedString>,
        bounds: CardRect,
        font_size: f32,
        line_height: f32,
        color: CardColor,
        align: TextAlign,
    ) {
        self.primitives.push(CardPrimitive::Text {
            text: text.into(),
            bounds,
            font_size,
            line_height,
            color,
            align,
        });
    }

    fn image(&mut self, asset: CanvasImageAsset, bounds: CardRect, fit: CardImageFit, radius: f32) {
        self.primitives.push(CardPrimitive::Image {
            asset,
            bounds,
            fit,
            radius,
        });
    }
}

pub fn build_card_scene(canvas_node: &CanvasNode, expanded: bool) -> CardScene {
    let node = &canvas_node.node;
    let mut scene = CardScene {
        height: card_height_from_metadata(
            node,
            expanded,
            canvas_node.prompt_lines.len(),
            canvas_node.output_layout.height(),
        ),
        primitives: Vec::new(),
    };
    let border = if node.status == NodeStatus::Running {
        CardColor::Accent45
    } else {
        CardColor::Line
    };
    scene.quad(
        CardRect::new(0., 0., CARD_WIDTH, scene.height),
        20.,
        CardColor::Raised,
        Some((1., border)),
    );

    let prompt_clamped = canvas_node.prompt_lines.len() > COLLAPSED_PROMPT_LINES;
    let visible_prompt_lines = if expanded {
        &canvas_node.prompt_lines[..canvas_node.prompt_lines.len().min(EXPANDED_PROMPT_LINES)]
    } else {
        &canvas_node.collapsed_prompt_lines
    };
    let visible_line_count = visible_prompt_lines.len().max(1);
    let prompt_block_height = 24.
        + visible_line_count as f32 * PROMPT_LINE_HEIGHT
        + if prompt_clamped { SHOW_MORE_HEIGHT } else { 0. };
    for (index, line) in visible_prompt_lines.iter().enumerate() {
        scene.text(
            line.clone(),
            CardRect::new(
                14.,
                13. + index as f32 * PROMPT_LINE_HEIGHT,
                CARD_WIDTH - 28.,
                PROMPT_LINE_HEIGHT,
            ),
            12.5,
            PROMPT_LINE_HEIGHT,
            CardColor::Ink90,
            TextAlign::Left,
        );
    }
    if prompt_clamped {
        scene.text(
            if expanded { "Show less" } else { "Show more" },
            CardRect::new(
                14.,
                13. + visible_line_count as f32 * PROMPT_LINE_HEIGHT,
                CARD_WIDTH - 28.,
                SHOW_MORE_HEIGHT,
            ),
            10.5,
            SHOW_MORE_HEIGHT,
            CardColor::Accent,
            TextAlign::Left,
        );
    }

    let mut cursor_y = prompt_block_height;
    if !canvas_node.attachment_images.is_empty() {
        for (index, asset) in canvas_node.attachment_images.iter().enumerate() {
            scene.image(
                asset.clone(),
                CardRect::new(14. + index as f32 * 42., cursor_y + 8., 36., 36.),
                CardImageFit::Cover,
                6.,
            );
        }
        cursor_y += ATTACHMENT_ROW_HEIGHT;
    }

    scene.text(
        "❖ Me",
        CardRect::new(14., cursor_y, 42., 18.),
        10.5,
        18.,
        CardColor::Faint,
        TextAlign::Left,
    );
    if node.aspect != "auto" {
        let pill_width = (node.aspect.len() as f32 * 6.2 + 8.).max(28.);
        let pill = CardRect::new(56., cursor_y + 1., pill_width, 16.);
        scene.quad(
            pill,
            8.,
            CardColor::Transparent,
            Some((1., CardColor::Line)),
        );
        scene.text(
            node.aspect.clone(),
            pill,
            10.5,
            16.,
            CardColor::Faint,
            TextAlign::Center,
        );
    }
    scene.text(
        canvas_node.date.clone(),
        CardRect::new(140., cursor_y, CARD_WIDTH - 154., 18.),
        10.5,
        18.,
        CardColor::Faint,
        TextAlign::Right,
    );
    cursor_y += 26.;
    scene.quad(
        CardRect::new(0., cursor_y, CARD_WIDTH, MEDIA_GAP),
        0.,
        CardColor::Line,
        None,
    );
    cursor_y += MEDIA_GAP;

    match &canvas_node.output_layout {
        OutputLayout::None => {}
        OutputLayout::Tiles { height, cells } => {
            let media = CardRect::new(0., cursor_y, CARD_WIDTH, *height);
            scene.quad(media, 0., CardColor::Hover, None);
            if cells.is_empty() {
                scene.quad(media, 0., CardColor::Raised, None);
                scene.text(
                    "Generating…",
                    media,
                    12.,
                    *height,
                    CardColor::Faint,
                    TextAlign::Center,
                );
            }
            for cell in cells {
                let Some(image) = canvas_node.displayed_images.get(cell.index) else {
                    continue;
                };
                let bounds = CardRect::new(cell.x, cursor_y + cell.y, cell.width, cell.height);
                scene.quad(bounds, 0., CardColor::Raised, None);
                scene.image(image.asset.clone(), bounds, CardImageFit::Contain, 0.);
            }
            if node.images.is_empty() && !canvas_node.displayed_images.is_empty() {
                let badge = CardRect::new(CARD_WIDTH - 76., cursor_y + *height - 27., 68., 19.);
                scene.quad(badge, 5., CardColor::Background82, None);
                scene.text(
                    "Unfinalized",
                    badge,
                    10.,
                    19.,
                    CardColor::Dim,
                    TextAlign::Center,
                );
            }
            cursor_y += *height;
        }
        OutputLayout::Filmstrip {
            height,
            hero_height,
            compact_count,
            hidden_count,
            strip_cell_width,
        } => {
            scene.quad(
                CardRect::new(0., cursor_y, CARD_WIDTH, *height),
                0.,
                CardColor::Line,
                None,
            );
            if let Some(hero) = canvas_node.displayed_images.first() {
                let hero_bounds = CardRect::new(0., cursor_y, CARD_WIDTH, *hero_height);
                scene.quad(hero_bounds, 0., CardColor::Raised, None);
                scene.image(hero.asset.clone(), hero_bounds, CardImageFit::Contain, 0.);
            }
            let badge = CardRect::new(CARD_WIDTH - 57., cursor_y + *hero_height - 25., 49., 19.);
            scene.quad(badge, 5., CardColor::Background82, None);
            scene.text(
                format!("1 / {}", canvas_node.displayed_images.len()),
                badge,
                10.,
                19.,
                CardColor::Ink,
                TextAlign::Center,
            );
            let strip_y = cursor_y + *hero_height + MEDIA_GAP;
            for compact_index in 0..*compact_count {
                let Some(image) = canvas_node.displayed_images.get(compact_index + 1) else {
                    continue;
                };
                let bounds = CardRect::new(
                    compact_index as f32 * (*strip_cell_width + MEDIA_GAP),
                    strip_y,
                    *strip_cell_width,
                    *strip_cell_width,
                );
                scene.quad(bounds, 0., CardColor::Raised, None);
                scene.image(image.asset.clone(), bounds, CardImageFit::Cover, 0.);
            }
            if *hidden_count > 0 {
                let hidden = CardRect::new(
                    CARD_WIDTH - *strip_cell_width,
                    strip_y,
                    *strip_cell_width,
                    *strip_cell_width,
                );
                scene.quad(hidden, 0., CardColor::Raised, None);
                scene.text(
                    format!("+{hidden_count}"),
                    hidden,
                    12.,
                    *strip_cell_width,
                    CardColor::Dim,
                    TextAlign::Center,
                );
            }
            cursor_y += *height;
        }
    }

    let status_height = status_area_height(node);
    match node.status {
        NodeStatus::Running => {}
        NodeStatus::Done => scene.text(
            canvas_node.done_footer.clone(),
            CardRect::new(14., cursor_y, CARD_WIDTH - 28., status_height),
            10.5,
            status_height,
            CardColor::Dim,
            TextAlign::Left,
        ),
        NodeStatus::Error if canvas_node.displayed_images.is_empty() => {
            scene.text(
                "!",
                CardRect::new(0., cursor_y + 17., CARD_WIDTH, 24.),
                18.,
                24.,
                CardColor::Danger,
                TextAlign::Center,
            );
            scene.text(
                canvas_node.status_message.clone(),
                CardRect::new(18., cursor_y + 45., CARD_WIDTH - 36., 32.),
                11.5,
                16.,
                CardColor::Danger,
                TextAlign::Center,
            );
            let retry = CardRect::new(CARD_WIDTH / 2. - 29., cursor_y + 88., 58., 26.);
            scene.quad(
                retry,
                7.,
                CardColor::Transparent,
                Some((1., CardColor::Line)),
            );
            scene.text("Retry", retry, 10.5, 26., CardColor::Dim, TextAlign::Center);
        }
        NodeStatus::Error | NodeStatus::Stopped => {
            scene.text(
                canvas_node.status_message.clone(),
                CardRect::new(14., cursor_y, CARD_WIDTH - 90., status_height),
                10.8,
                status_height,
                if node.status == NodeStatus::Error {
                    CardColor::Danger
                } else {
                    CardColor::Faint
                },
                TextAlign::Left,
            );
            let retry = CardRect::new(
                CARD_WIDTH - 68.,
                cursor_y + (status_height - 26.) * 0.5,
                54.,
                26.,
            );
            scene.quad(
                retry,
                7.,
                CardColor::Transparent,
                Some((1., CardColor::Line)),
            );
            scene.text("Retry", retry, 10.5, 26., CardColor::Dim, TextAlign::Center);
        }
    }
    scene
}

pub fn card_scene_svg(scene: &CardScene, rendered_width: f32) -> String {
    let rendered_height = scene.height * rendered_width / CARD_WIDTH;
    let mut svg = String::with_capacity(scene.primitives.len() * 180);
    write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{rendered_width}\" height=\"{rendered_height}\" viewBox=\"0 0 {CARD_WIDTH} {}\">",
        scene.height
    )
    .expect("writing to a String cannot fail");
    write!(
        svg,
        "<defs><clipPath id=\"card\"><rect x=\"0\" y=\"0\" width=\"{CARD_WIDTH}\" height=\"{}\" rx=\"20\"/></clipPath></defs><g clip-path=\"url(#card)\">",
        scene.height
    )
    .expect("writing to a String cannot fail");
    for (index, primitive) in scene.primitives.iter().enumerate() {
        match primitive {
            CardPrimitive::Quad {
                bounds,
                radius,
                fill,
                border,
            } => {
                let (fill_color, fill_opacity) = fill.svg();
                write!(
                    svg,
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{radius}\" fill=\"{fill_color}\" fill-opacity=\"{fill_opacity}\"",
                    bounds.x, bounds.y, bounds.width, bounds.height
                )
                .expect("writing to a String cannot fail");
                if let Some((width, color)) = border {
                    let (stroke, opacity) = color.svg();
                    write!(
                        svg,
                        " stroke=\"{stroke}\" stroke-opacity=\"{opacity}\" stroke-width=\"{width}\""
                    )
                    .expect("writing to a String cannot fail");
                }
                svg.push_str("/>");
            }
            CardPrimitive::Text {
                text,
                bounds,
                font_size,
                line_height,
                color,
                align,
            } => {
                let clip_id = format!("text-{index}");
                write!(
                    svg,
                    "<clipPath id=\"{clip_id}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></clipPath>",
                    bounds.x, bounds.y, bounds.width, bounds.height
                )
                .expect("writing to a String cannot fail");
                let (anchor, x) = match align {
                    TextAlign::Left => ("start", bounds.x),
                    TextAlign::Center => ("middle", bounds.x + bounds.width / 2.),
                    TextAlign::Right => ("end", bounds.x + bounds.width),
                };
                let baseline = bounds.y + (line_height - font_size) * 0.5 + font_size * 0.82;
                let (fill, opacity) = color.svg();
                write!(
                    svg,
                    "<text x=\"{x}\" y=\"{baseline}\" clip-path=\"url(#{clip_id})\" font-family=\"system-ui,sans-serif\" font-size=\"{font_size}\" font-weight=\"400\" text-anchor=\"{anchor}\" fill=\"{fill}\" fill-opacity=\"{opacity}\">"
                )
                .expect("writing to a String cannot fail");
                push_xml_escaped(&mut svg, text);
                svg.push_str("</text>");
            }
            CardPrimitive::Image {
                asset,
                bounds,
                fit,
                radius,
            } => {
                if asset.thumbnail.as_os_str().is_empty() {
                    continue;
                }
                let clip_id = format!("image-{index}");
                write!(
                    svg,
                    "<clipPath id=\"{clip_id}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{radius}\"/></clipPath><image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" clip-path=\"url(#{clip_id})\" preserveAspectRatio=\"xMidYMid {}\" href=\"",
                    bounds.x,
                    bounds.y,
                    bounds.width,
                    bounds.height,
                    bounds.x,
                    bounds.y,
                    bounds.width,
                    bounds.height,
                    match fit {
                        CardImageFit::Contain => "meet",
                        CardImageFit::Cover => "slice",
                    }
                )
                .expect("writing to a String cannot fail");
                push_xml_escaped(&mut svg, &asset.thumbnail.to_string_lossy());
                svg.push_str("\"/>");
            }
        }
    }
    svg.push_str("</g></svg>");
    svg
}

fn push_xml_escaped(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ => output.push(character),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputCell {
    pub index: usize,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OutputLayout {
    None,
    Tiles {
        height: f32,
        cells: Vec<OutputCell>,
    },
    Filmstrip {
        height: f32,
        hero_height: f32,
        compact_count: usize,
        hidden_count: usize,
        strip_cell_width: f32,
    },
}

impl OutputLayout {
    pub fn height(&self) -> f32 {
        match self {
            Self::None => 0.,
            Self::Tiles { height, .. } | Self::Filmstrip { height, .. } => *height,
        }
    }
}

pub fn card_height(node: &BoardNode, expanded: bool, ratios: &HashMap<String, f32>) -> f32 {
    card_height_from_metadata(
        node,
        expanded,
        wrap_prompt(&node.prompt, PROMPT_WRAP_COLUMNS).len(),
        output_layout(node, ratios).height(),
    )
}

pub fn card_height_from_metadata(
    node: &BoardNode,
    expanded: bool,
    total_prompt_lines: usize,
    output_height: f32,
) -> f32 {
    let (prompt_lines, prompt_clamped) =
        prompt_metrics_from_line_count(total_prompt_lines, expanded);
    HEADER_FIXED_HEIGHT
        + prompt_lines * PROMPT_LINE_HEIGHT
        + if prompt_clamped { SHOW_MORE_HEIGHT } else { 0. }
        + if node.attachments.is_empty() {
            0.
        } else {
            ATTACHMENT_ROW_HEIGHT
        }
        + MEDIA_GAP
        + output_height
        + status_area_height(node)
}

fn prompt_metrics_from_line_count(total_lines: usize, expanded: bool) -> (f32, bool) {
    let line_limit = if expanded {
        EXPANDED_PROMPT_LINES
    } else {
        COLLAPSED_PROMPT_LINES
    };
    (
        total_lines.min(line_limit).max(1) as f32,
        total_lines > COLLAPSED_PROMPT_LINES,
    )
}

pub fn wrap_prompt(value: &str, max_graphemes: usize) -> Vec<String> {
    assert!(max_graphemes > 0, "prompt wrap width must be non-zero");
    let mut lines = Vec::new();

    for paragraph in value.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut current_len = 0;
        for word in paragraph.split_whitespace() {
            let graphemes = UnicodeSegmentation::graphemes(word, true).collect::<Vec<_>>();
            if graphemes.len() <= max_graphemes {
                let separator_len = usize::from(!current.is_empty());
                if current_len + separator_len + graphemes.len() <= max_graphemes {
                    if separator_len == 1 {
                        current.push(' ');
                    }
                    current.push_str(word);
                    current_len += separator_len + graphemes.len();
                } else {
                    lines.push(std::mem::take(&mut current));
                    current.push_str(word);
                    current_len = graphemes.len();
                }
                continue;
            }

            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
            for chunk in graphemes.chunks(max_graphemes) {
                let chunk = chunk.concat();
                if chunk.graphemes(true).count() == max_graphemes {
                    lines.push(chunk);
                } else {
                    current_len = chunk.graphemes(true).count();
                    current = chunk;
                }
            }
        }

        if !current.is_empty() {
            lines.push(current);
        } else if paragraph.chars().all(char::is_whitespace) {
            lines.push(String::new());
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub fn displayed_urls(node: &BoardNode) -> &[String] {
    if node.images.is_empty() {
        node.attempts
            .last()
            .map(std::slice::from_ref)
            .unwrap_or_default()
    } else {
        &node.images
    }
}

pub fn output_layout(node: &BoardNode, ratios: &HashMap<String, f32>) -> OutputLayout {
    let urls = displayed_urls(node);
    match urls.len() {
        0 if node.status == NodeStatus::Running => OutputLayout::Tiles {
            height: CARD_WIDTH,
            cells: Vec::new(),
        },
        0 => OutputLayout::None,
        1 => {
            let height = image_height(CARD_WIDTH, output_ratio(node, &urls[0], ratios));
            OutputLayout::Tiles {
                height,
                cells: vec![OutputCell {
                    index: 0,
                    x: 0.,
                    y: 0.,
                    width: CARD_WIDTH,
                    height,
                }],
            }
        }
        2..=4 => {
            let cell_width = (CARD_WIDTH - MEDIA_GAP) / 2.;
            let mut cells = Vec::with_capacity(urls.len());
            let mut y = 0.;
            for (row, chunk) in urls.chunks(2).enumerate() {
                let row_height = chunk
                    .iter()
                    .map(|url| image_height(cell_width, output_ratio(node, url, ratios)))
                    .fold(0_f32, f32::max);
                for (column, _) in chunk.iter().enumerate() {
                    cells.push(OutputCell {
                        index: row * 2 + column,
                        x: column as f32 * (cell_width + MEDIA_GAP),
                        y,
                        width: cell_width,
                        height: row_height,
                    });
                }
                y += row_height;
                if row * 2 + chunk.len() < urls.len() {
                    y += MEDIA_GAP;
                }
            }
            OutputLayout::Tiles { height: y, cells }
        }
        _ => {
            let hero_height = image_height(CARD_WIDTH, output_ratio(node, &urls[0], ratios));
            let compact_count = if urls.len() > 6 {
                4
            } else {
                (urls.len() - 1).min(5)
            };
            let hidden_count = urls.len() - compact_count - 1;
            let strip_cells = compact_count + usize::from(hidden_count > 0);
            let strip_cell_width = (CARD_WIDTH - MEDIA_GAP * strip_cells.saturating_sub(1) as f32)
                / strip_cells as f32;
            OutputLayout::Filmstrip {
                height: hero_height + MEDIA_GAP + strip_cell_width,
                hero_height,
                compact_count,
                hidden_count,
                strip_cell_width,
            }
        }
    }
}

pub fn status_area_height(node: &BoardNode) -> f32 {
    let has_images = !displayed_urls(node).is_empty();
    match node.status {
        NodeStatus::Running | NodeStatus::Done => 42.,
        NodeStatus::Error if has_images => 64.,
        NodeStatus::Error => 132.,
        NodeStatus::Stopped => 52.,
    }
}

fn output_ratio(node: &BoardNode, url: &str, ratios: &HashMap<String, f32>) -> f32 {
    ratios
        .get(url)
        .copied()
        .or_else(|| parse_aspect_ratio(&node.aspect))
        .unwrap_or(1.)
        .clamp(0.2, 5.)
}

fn parse_aspect_ratio(aspect: &str) -> Option<f32> {
    let (width, height) = aspect.split_once(':')?;
    let (Ok(width), Ok(height)) = (width.parse::<f32>(), height.parse::<f32>()) else {
        return None;
    };
    (width.is_finite() && height.is_finite() && width > 0. && height > 0.).then_some(width / height)
}

fn image_height(width: f32, ratio: f32) -> f32 {
    (width / ratio).clamp(width * 0.28, width * 2.)
}

#[cfg(test)]
mod tests {
    use super::{
        CARD_WIDTH, CardColor, CardRect, CardScene, OutputLayout, PROMPT_WRAP_COLUMNS, card_height,
        card_scene_svg, output_layout, prompt_metrics_from_line_count, wrap_prompt,
    };
    use crate::model::{BoardNode, NodeStatus};
    use std::collections::HashMap;
    use unicode_segmentation::UnicodeSegmentation;

    fn node(images: usize, status: NodeStatus) -> BoardNode {
        BoardNode {
            id: "node".into(),
            parent_id: None,
            prompt: "A concise prompt that should preserve its line shape while zooming".into(),
            aspect: "auto".into(),
            source_images: Vec::new(),
            attachments: Vec::new(),
            images: (0..images).map(|index| format!("/{index}.png")).collect(),
            image_labels: Vec::new(),
            attempts: Vec::new(),
            text: String::new(),
            status,
            error: None,
            stop_reason: None,
            x: None,
            y: None,
            created_at: 0,
            run_started_at: None,
            finished_at: None,
            usage: None,
        }
    }
    #[test]
    fn four_outputs_form_two_non_overlapping_columns() {
        let node = node(4, NodeStatus::Done);
        let ratios = HashMap::from([
            ("/0.png".into(), 1.0),
            ("/1.png".into(), 0.5),
            ("/2.png".into(), 2.0),
            ("/3.png".into(), 1.0),
        ]);
        let OutputLayout::Tiles { height, cells } = output_layout(&node, &ratios) else {
            panic!("expected tiled layout")
        };
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0].x, 0.);
        assert!(cells[1].x > cells[0].x + cells[0].width);
        assert!(cells[2].y > cells[0].y);
        assert!(height >= cells[3].y + cells[3].height);
    }

    #[test]
    fn large_output_sets_use_a_bounded_hero_and_filmstrip() {
        let node = node(9, NodeStatus::Done);
        let OutputLayout::Filmstrip {
            hero_height,
            compact_count,
            hidden_count,
            strip_cell_width,
            ..
        } = output_layout(&node, &HashMap::new())
        else {
            panic!("expected filmstrip layout")
        };
        assert_eq!(compact_count, 4);
        assert_eq!(hidden_count, 4);
        assert!(hero_height <= CARD_WIDTH * 2.);
        assert!(strip_cell_width < CARD_WIDTH / 4.);
    }

    #[test]
    fn empty_error_state_reserves_more_space_than_a_done_footer() {
        let error = node(0, NodeStatus::Error);
        let done = node(0, NodeStatus::Done);
        assert!(
            card_height(&error, false, &HashMap::new())
                > card_height(&done, false, &HashMap::new())
        );
    }

    #[test]
    fn prompt_wrap_is_deterministic_and_grapheme_safe() {
        let prompt = "alpha beta supercalifragilistic 🌍🌎🌏🌐";
        let lines = wrap_prompt(prompt, 8);
        assert!(lines.iter().all(|line| line.graphemes(true).count() <= 8));
        let source = prompt
            .chars()
            .filter(|character| !character.is_whitespace());
        let wrapped = lines
            .iter()
            .flat_map(|line| line.chars())
            .filter(|character| !character.is_whitespace());
        assert_eq!(source.collect::<String>(), wrapped.collect::<String>());
    }

    #[test]
    fn prompt_wrap_preserves_explicit_blank_lines() {
        assert_eq!(
            wrap_prompt("one two\n\nthree", 6),
            vec!["one", "two", "", "three"]
        );
    }

    #[test]
    fn expanded_prompt_height_uses_the_same_world_space_lines() {
        let mut node = node(0, NodeStatus::Done);
        node.prompt = std::iter::repeat_n("stable", 80)
            .collect::<Vec<_>>()
            .join(" ");
        let total_lines = wrap_prompt(&node.prompt, PROMPT_WRAP_COLUMNS).len();
        let (collapsed_lines, clamped) = prompt_metrics_from_line_count(total_lines, false);
        let (expanded_lines, expanded_clamped) = prompt_metrics_from_line_count(total_lines, true);
        assert_eq!(collapsed_lines, 6.);
        assert!(expanded_lines > collapsed_lines);
        assert!(clamped && expanded_clamped);
    }
    #[test]
    fn sprite_tiers_preserve_one_world_space_scene() {
        let mut scene = CardScene {
            height: 510.,
            primitives: Vec::new(),
        };
        scene.quad(
            CardRect::new(0., 0., CARD_WIDTH, scene.height),
            20.,
            CardColor::Raised,
            Some((1., CardColor::Line)),
        );
        scene.text(
            "A <stable> & exact card",
            CardRect::new(14., 12., CARD_WIDTH - 28., 24.),
            14.,
            18.,
            CardColor::Ink,
            gpui::TextAlign::Left,
        );

        for width in super::CARD_SPRITE_WIDTHS {
            let svg = card_scene_svg(&scene, width);
            assert!(svg.contains("viewBox=\"0 0 340 510\""));
            assert!(svg.contains("A &lt;stable&gt; &amp; exact card"));
            assert!(svg.contains(&format!("width=\"{width}\"")));
        }
    }
}
