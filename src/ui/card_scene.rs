//! Turns a card into flat, world-space drawing primitives shared by the direct
//! painter and the SVG sprite encoder.

use super::card::{
    ATTACHED_TEXT_HEIGHT, ATTACHMENT_ROW_HEIGHT, COLLAPSED_PROMPT_LINES, CanvasImageAsset,
    CanvasNode, EXPANDED_PROMPT_LINES, MEDIA_GAP, MEDIA_INSET, MEDIA_RADIUS, MEDIA_WIDTH,
    OutputLayout, PROMPT_LINE_HEIGHT, SHOW_MORE_HEIGHT, TILE_GAP, attached_text_height,
    card_height_from_metadata, filmstrip_cell_x, status_area_height, wrap_prompt,
};

/// Corner radius of the smaller tiles in grids and the filmstrip.
const TILE_RADIUS: f32 = 8.;
use super::theme;
use crate::layout::CARD_WIDTH;
use crate::model::NodeStatus;
use gpui::{SharedString, TextAlign};

/// Below this many logical px, text is an illegible smear. Neither the direct
/// painter nor a sprite spends shaping work on it.
pub const MIN_LEGIBLE_FONT_SIZE: f32 = 3.;

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
    Dim,
    Faint,
    Accent,
    Accent45,
    Danger,
}

impl CardColor {
    /// The sRGB value and the alpha this colour is drawn at. Both renderers
    /// go through here, so a palette change reaches the painted card and its
    /// SVG sprite together.
    const fn parts(self) -> (u32, f32) {
        match self {
            Self::Transparent => (0x000000, 0.),
            Self::Background82 => (theme::BACKGROUND, 0.82),
            Self::Raised => (theme::RAISED, 1.),
            Self::Hover => (theme::HOVER, 1.),
            Self::Line => (theme::LINE, 1.),
            Self::Ink => (theme::INK, 1.),
            Self::Dim => (theme::DIM, 1.),
            Self::Faint => (theme::FAINT, 1.),
            Self::Accent => (theme::ACCENT, 1.),
            Self::Accent45 => (theme::ACCENT, 0.45),
            Self::Danger => (theme::DANGER, 1.),
        }
    }

    pub fn hsla(self) -> gpui::Hsla {
        let (color, opacity) = self.parts();
        gpui::Hsla::from(gpui::rgb(color)).opacity(opacity)
    }

    /// The sRGB value and fill opacity for the SVG sprite encoder, which needs
    /// them as separate attributes.
    pub fn svg(self) -> (u32, f32) {
        self.parts()
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
        /// Draw an unrecognizable, heavily blurred version so an in-progress
        /// generation never spoils its final image.
        blurred: bool,
    },
}

#[derive(Default)]
pub struct CardScene {
    pub height: f32,
    pub primitives: Vec<CardPrimitive>,
    /// Cached scene facts used by every paint. Scanning all primitives per
    /// visible node was measurable during camera movement.
    pub has_blurred_images: bool,
    pub max_high_resolution_dimension: f32,
    /// Card-local media area of a running card that has no image yet. The
    /// canvas paints the live "generating" animation over it each frame; the
    /// cached scene and sprite keep only the plain backdrop.
    pub generating_area: Option<CardRect>,
}

impl CardScene {
    pub(super) fn quad(
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

    pub(super) fn text(
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

    pub(super) fn image(
        &mut self,
        asset: CanvasImageAsset,
        bounds: CardRect,
        fit: CardImageFit,
        radius: f32,
        blurred: bool,
    ) {
        self.has_blurred_images |= blurred;
        if !blurred && asset.original.as_ref() != asset.thumbnail.as_ref() {
            self.max_high_resolution_dimension = self
                .max_high_resolution_dimension
                .max(bounds.width.max(bounds.height));
        }
        self.primitives.push(CardPrimitive::Image {
            asset,
            bounds,
            fit,
            radius,
            blurred,
        });
    }
}

/// Wraps `text` to `width` graphemes and keeps the first `max_lines`, ending
/// in an ellipsis when something was cut. The full text opens on click.
fn clamp_lines(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let mut lines = wrap_prompt(text, width);
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            last.push('…');
        }
    }
    lines
}

pub fn build_card_scene(canvas_node: &CanvasNode) -> CardScene {
    let node = &canvas_node.node;
    let expanded = canvas_node.expanded;
    let mut scene = CardScene {
        height: card_height_from_metadata(
            node,
            expanded,
            canvas_node.prompt_lines.len(),
            canvas_node.output_layout.height(),
        ),
        ..Default::default()
    };
    // While the generation runs, any displayed image is an intermediate
    // attempt; blur it so the card marks progress without spoiling the result.
    let running = node.status == NodeStatus::Running;
    let blur_outputs = running && node.images.is_empty();
    let border = if running {
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
    // The opening paragraph reads as the card's title; the rest is dimmer.
    let title_lines = visible_prompt_lines
        .iter()
        .position(|line| line.trim().is_empty())
        .unwrap_or(visible_prompt_lines.len());
    for (index, line) in visible_prompt_lines.iter().enumerate() {
        let color = if index < title_lines || title_lines == visible_prompt_lines.len() {
            CardColor::Ink
        } else {
            CardColor::Dim
        };
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
            color,
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
                false,
            );
        }
        cursor_y += ATTACHMENT_ROW_HEIGHT;
    }

    // Status at a glance: a coloured dot and what the card holds.
    let image_count = node.images.len();
    let (dot, status) = match node.status {
        NodeStatus::Running => (CardColor::Accent, "Generating".to_owned()),
        NodeStatus::Done if image_count == 1 => (CardColor::Accent45, "1 image".to_owned()),
        NodeStatus::Done => (CardColor::Accent45, format!("{image_count} images")),
        NodeStatus::Error => (CardColor::Danger, "Failed".to_owned()),
        NodeStatus::Stopped => (CardColor::Faint, "Stopped".to_owned()),
    };
    scene.quad(CardRect::new(15., cursor_y + 6., 6., 6.), 3., dot, None);
    let status_width = status.chars().count() as f32 * 6. + 4.;
    scene.text(
        status,
        CardRect::new(27., cursor_y, status_width, 18.),
        10.5,
        18.,
        CardColor::Dim,
        TextAlign::Left,
    );
    if node.aspect != "auto" {
        let pill_width = (node.aspect.len() as f32 * 6.2 + 8.).max(28.);
        let pill = CardRect::new(27. + status_width + 6., cursor_y + 1., pill_width, 16.);
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
    // Inset media needs no divider; a card without outputs keeps one above
    // its status.
    if matches!(canvas_node.output_layout, OutputLayout::None) {
        scene.quad(
            CardRect::new(0., cursor_y, CARD_WIDTH, MEDIA_GAP),
            0.,
            CardColor::Line,
            None,
        );
    }
    cursor_y += MEDIA_GAP;

    match &canvas_node.output_layout {
        OutputLayout::None => {}
        OutputLayout::Tiles { height, cells } => {
            let media = CardRect::new(MEDIA_INSET, cursor_y, MEDIA_WIDTH, *height);
            if cells.is_empty() {
                scene.quad(media, MEDIA_RADIUS, CardColor::Hover, None);
            }
            if cells.is_empty() && running {
                scene.generating_area = Some(media);
            } else if cells.is_empty() {
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
                let radius = if cells.len() == 1 {
                    MEDIA_RADIUS
                } else {
                    TILE_RADIUS
                };
                scene.quad(bounds, radius, CardColor::Hover, None);
                scene.image(
                    image.asset.clone(),
                    bounds,
                    CardImageFit::Contain,
                    radius,
                    blur_outputs,
                );
            }
            if node.images.is_empty() && !canvas_node.displayed_images.is_empty() {
                let badge = CardRect::new(
                    MEDIA_INSET + MEDIA_WIDTH - 76.,
                    cursor_y + *height - 27.,
                    68.,
                    19.,
                );
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
            if let Some(hero) = canvas_node.displayed_images.first() {
                let hero_bounds = CardRect::new(MEDIA_INSET, cursor_y, MEDIA_WIDTH, *hero_height);
                scene.quad(hero_bounds, MEDIA_RADIUS, CardColor::Hover, None);
                scene.image(
                    hero.asset.clone(),
                    hero_bounds,
                    CardImageFit::Contain,
                    MEDIA_RADIUS,
                    blur_outputs,
                );
            }
            let badge = CardRect::new(
                MEDIA_INSET + MEDIA_WIDTH - 57.,
                cursor_y + *hero_height - 25.,
                49.,
                19.,
            );
            scene.quad(badge, 5., CardColor::Background82, None);
            scene.text(
                format!("1 / {}", canvas_node.displayed_images.len()),
                badge,
                10.,
                19.,
                CardColor::Ink,
                TextAlign::Center,
            );
            let strip_y = cursor_y + *hero_height + TILE_GAP;
            for compact_index in 0..*compact_count {
                let Some(image) = canvas_node.displayed_images.get(compact_index + 1) else {
                    continue;
                };
                let bounds = CardRect::new(
                    filmstrip_cell_x(compact_index, *strip_cell_width),
                    strip_y,
                    *strip_cell_width,
                    *strip_cell_width,
                );
                scene.quad(bounds, TILE_RADIUS, CardColor::Hover, None);
                scene.image(
                    image.asset.clone(),
                    bounds,
                    CardImageFit::Cover,
                    TILE_RADIUS,
                    blur_outputs,
                );
            }
            if *hidden_count > 0 {
                let hidden = CardRect::new(
                    MEDIA_INSET + MEDIA_WIDTH - *strip_cell_width,
                    strip_y,
                    *strip_cell_width,
                    *strip_cell_width,
                );
                scene.quad(hidden, TILE_RADIUS, CardColor::Hover, None);
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
            // Two centred lines fill the 32px message block; a single line
            // sits in its middle.
            let lines = clamp_lines(&canvas_node.status_message, 50, 2);
            let first_y = cursor_y + 45. + (2 - lines.len().min(2)) as f32 * 8.;
            for (index, line) in lines.into_iter().enumerate() {
                scene.text(
                    line,
                    CardRect::new(18., first_y + index as f32 * 16., CARD_WIDTH - 36., 16.),
                    11.5,
                    16.,
                    CardColor::Danger,
                    TextAlign::Center,
                );
            }
            let text_offset = attached_text_height(node);
            if text_offset > 0. {
                scene.text(
                    canvas_node.attached_text.clone(),
                    CardRect::new(18., cursor_y + 81., CARD_WIDTH - 36., 18.),
                    10.5,
                    16.,
                    CardColor::Dim,
                    TextAlign::Center,
                );
            }
            let retry = CardRect::new(
                CARD_WIDTH / 2. - 29.,
                cursor_y + 88. + text_offset,
                58.,
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
        NodeStatus::Error | NodeStatus::Stopped => {
            let message_height = status_height - attached_text_height(node);
            scene.text(
                clamp_lines(&canvas_node.status_message, 44, 1)
                    .pop()
                    .unwrap_or_default(),
                CardRect::new(14., cursor_y, CARD_WIDTH - 90., message_height),
                10.8,
                message_height,
                if node.status == NodeStatus::Error {
                    CardColor::Danger
                } else {
                    CardColor::Faint
                },
                TextAlign::Left,
            );
            if message_height < status_height {
                scene.text(
                    canvas_node.attached_text.clone(),
                    CardRect::new(
                        14.,
                        cursor_y + message_height - 8.,
                        CARD_WIDTH - 28.,
                        ATTACHED_TEXT_HEIGHT - 2.,
                    ),
                    10.5,
                    16.,
                    CardColor::Dim,
                    TextAlign::Left,
                );
            }
            let retry = CardRect::new(
                CARD_WIDTH - 68.,
                cursor_y + (message_height - 26.) * 0.5,
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
