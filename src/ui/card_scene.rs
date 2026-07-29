//! Turns a card into flat, world-space drawing primitives shared by the direct
//! painter and the SVG sprite encoder.

use super::card::{
    ATTACHMENT_ROW_HEIGHT, COLLAPSED_PROMPT_LINES, CanvasImageAsset, CanvasNode,
    EXPANDED_PROMPT_LINES, MEDIA_GAP, OutputLayout, PROMPT_LINE_HEIGHT, SHOW_MORE_HEIGHT,
    card_height_from_metadata, status_area_height,
};
use super::theme;
use crate::layout::CARD_WIDTH;
use crate::model::NodeStatus;
use gpui::{SharedString, TextAlign};

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
        /// Draw an unrecognizable, heavily blurred version so an in-progress
        /// generation never spoils its final image.
        blurred: bool,
    },
}

#[derive(Default)]
pub struct CardScene {
    pub height: f32,
    pub primitives: Vec<CardPrimitive>,
    /// The media area of a running card, in card-local coordinates. The canvas
    /// paints the animated generating shimmer over this rectangle.
    pub generating_media: Option<CardRect>,
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
        self.primitives.push(CardPrimitive::Image {
            asset,
            bounds,
            fit,
            radius,
            blurred,
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
        generating_media: None,
    };
    // While the generation runs, any displayed image is an intermediate
    // attempt; blur it so the card marks progress without spoiling the result.
    let running = node.status == NodeStatus::Running;
    let blur_outputs = running && node.images.is_empty();
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
                false,
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

    let media_top = cursor_y;
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
                scene.image(
                    image.asset.clone(),
                    bounds,
                    CardImageFit::Contain,
                    0.,
                    blur_outputs,
                );
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
                scene.image(
                    hero.asset.clone(),
                    hero_bounds,
                    CardImageFit::Contain,
                    0.,
                    blur_outputs,
                );
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
                scene.image(
                    image.asset.clone(),
                    bounds,
                    CardImageFit::Cover,
                    0.,
                    blur_outputs,
                );
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
    if running {
        let media_height = canvas_node.output_layout.height();
        if media_height > 0. {
            scene.generating_media =
                Some(CardRect::new(0., media_top, CARD_WIDTH, media_height));
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
