//! The world-space model of a board card: how tall it is, where its outputs
//! sit, and the primitives that draw it either directly or as an SVG sprite.

use super::format::{done_footer, format_date, status_message};
use crate::model::BoardNode;
use gpui::{Image, ImageFormat, SharedString};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;

pub use super::card_layout::{
    OutputLayout, card_height, card_height_from_metadata, displayed_urls, output_layout,
    status_area_height, wrap_prompt,
};
pub use super::card_scene::{CardImageFit, CardPrimitive, CardRect, CardScene, build_card_scene};
pub use super::card_svg::card_scene_svg;

pub const PROMPT_LINE_HEIGHT: f32 = 18.;
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
    pub expanded: bool,
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
            expanded,
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
