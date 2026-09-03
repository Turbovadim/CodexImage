//! The world-space model of a board card: how tall it is, where its outputs
//! sit, and the primitives that draw it either directly or as an SVG sprite.

use super::format::{attached_text_excerpt, done_footer, format_date, status_message};
use crate::model::BoardNode;
use gpui::{Image, ImageFormat, SharedString};
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU8;

pub use super::card_layout::{
    ATTACHED_TEXT_HEIGHT, OutputLayout, attached_text_height, card_height,
    card_height_from_metadata, displayed_urls, output_layout, status_area_height, wrap_prompt,
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
/// SVG widths of the far-out sprite tiers; GPUI rasterizes each at 2x. Tier
/// `i` serves zoom levels up to `width / CARD_WIDTH` (0.125, 0.25, 0.5). Above
/// the last tier cards are painted directly: few are visible there, and a
/// sprite for a 340 px card at zoom 1 would cost several megabytes each.
pub const CARD_SPRITE_WIDTHS: [f32; 3] = [42.5, 85., 170.];
pub const NO_SPRITE_TIER: u8 = u8::MAX;

#[derive(Clone)]
pub struct CanvasImageAsset {
    pub original: Arc<Path>,
    pub thumbnail: Arc<Path>,
    /// The tiny `s_` thumbnail for small sprite tiers and far-out zoom.
    pub sprite: Arc<Path>,
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
    pub attached_text: SharedString,
    pub scene: CardScene,
    /// SVG sources are created only for tiers that actually become visible.
    /// Eagerly encoding all four tiers for every node caused a large transient
    /// allocator peak and populated GPUI's asset cache with unused images.
    sprite_images: [OnceLock<Arc<Image>>; CARD_SPRITE_WIDTHS.len()],
    pub last_ready_sprite_tier: AtomicU8,
}

impl CanvasNode {
    /// Builds the stable world-space scene for one card. Raster sprite sources
    /// are encoded lazily by [`Self::sprite_image`] for visible zoom tiers.
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
            attached_text: attached_text_excerpt(node).into(),
            node: node.clone(),
            scene: CardScene::default(),
            sprite_images: std::array::from_fn(|_| OnceLock::new()),
            last_ready_sprite_tier: AtomicU8::new(NO_SPRITE_TIER),
        };
        card.scene = build_card_scene(&card, expanded);
        card
    }

    pub fn sprite_image(&self, tier: usize) -> Option<Arc<Image>> {
        let width = *CARD_SPRITE_WIDTHS.get(tier)?;
        Some(
            self.sprite_images[tier]
                .get_or_init(|| {
                    Arc::new(Image::from_bytes(
                        ImageFormat::Svg,
                        card_scene_svg(&self.scene, width).into_bytes(),
                    ))
                })
                .clone(),
        )
    }

    pub fn ready_sprite_image(&self, tier: usize) -> Option<&Image> {
        self.sprite_images.get(tier)?.get().map(AsRef::as_ref)
    }

    pub fn sprite_image_is_initialized(&self, tier: usize) -> bool {
        self.sprite_images
            .get(tier)
            .is_some_and(|image| image.get().is_some())
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

#[cfg(test)]
mod tests {
    use super::{CARD_SPRITE_WIDTHS, CanvasNode, OutputLayout};
    use crate::model::{BoardNode, NodeStatus};
    use gpui::SharedString;
    use std::sync::Arc;

    fn node() -> BoardNode {
        BoardNode {
            id: "node".into(),
            parent_id: None,
            prompt: "A quiet mountain lake".into(),
            aspect: "auto".into(),
            source_images: Vec::new(),
            attachments: Vec::new(),
            images: Vec::new(),
            image_labels: Vec::new(),
            attempts: Vec::new(),
            text: String::new(),
            status: NodeStatus::Done,
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
    fn sprite_sources_are_created_only_for_requested_tiers() {
        let card = CanvasNode::build(
            &node(),
            vec![SharedString::from("A quiet mountain lake")],
            OutputLayout::None,
            false,
            |_| unreachable!("test node has no image assets"),
        );

        assert!((0..CARD_SPRITE_WIDTHS.len()).all(|tier| !card.sprite_image_is_initialized(tier)));
        let image = card.sprite_image(2).expect("valid sprite tier");
        assert!(card.sprite_image_is_initialized(2));
        assert!(
            (0..CARD_SPRITE_WIDTHS.len())
                .filter(|&tier| tier != 2)
                .all(|tier| !card.sprite_image_is_initialized(tier))
        );
        assert!(Arc::ptr_eq(
            &image,
            &card.sprite_image(2).expect("cached sprite tier")
        ));
    }
}
