//! A small embedded icon set served to GPUI's `svg()` element.
//!
//! Icons are 24×24 stroke glyphs in the style of Lucide (ISC licence). GPUI
//! paints an SVG as a coverage mask and tints it with the element's text
//! colour, so the stroke colour in the source does not matter.

use anyhow::Result;
use gpui::{AssetSource, SharedString, Svg, prelude::*, px, svg};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    ArrowUp,
    AspectRatio,
    Boards,
    Branch,
    Chat,
    ChevronDown,
    Compare,
    Copies,
    Download,
    Duplicate,
    ExternalLink,
    Folder,
    Gallery,
    Locate,
    Mix,
    Paperclip,
    Pencil,
    Plus,
    Retry,
    Settings,
    Sparkles,
    Stop,
    Trash,
    X,
}

impl Icon {
    const ALL: [Icon; 24] = [
        Icon::ArrowUp,
        Icon::AspectRatio,
        Icon::Boards,
        Icon::Branch,
        Icon::Chat,
        Icon::ChevronDown,
        Icon::Compare,
        Icon::Copies,
        Icon::Download,
        Icon::Duplicate,
        Icon::ExternalLink,
        Icon::Folder,
        Icon::Gallery,
        Icon::Locate,
        Icon::Mix,
        Icon::Paperclip,
        Icon::Pencil,
        Icon::Plus,
        Icon::Retry,
        Icon::Settings,
        Icon::Sparkles,
        Icon::Stop,
        Icon::Trash,
        Icon::X,
    ];

    pub const fn path(self) -> &'static str {
        match self {
            Icon::ArrowUp => "icons/arrow-up.svg",
            Icon::AspectRatio => "icons/aspect-ratio.svg",
            Icon::Boards => "icons/boards.svg",
            Icon::ChevronDown => "icons/chevron-down.svg",
            Icon::Copies => "icons/copies.svg",
            Icon::Gallery => "icons/gallery.svg",
            Icon::Paperclip => "icons/paperclip.svg",
            Icon::Settings => "icons/settings.svg",
            Icon::Sparkles => "icons/sparkles.svg",
            Icon::Branch => "icons/branch.svg",
            Icon::Chat => "icons/chat.svg",
            Icon::Duplicate => "icons/duplicate.svg",
            Icon::Mix => "icons/mix.svg",
            Icon::Pencil => "icons/pencil.svg",
            Icon::Retry => "icons/retry.svg",
            Icon::Stop => "icons/stop.svg",
            Icon::Trash => "icons/trash.svg",
            Icon::Plus => "icons/plus.svg",
            Icon::Compare => "icons/compare.svg",
            Icon::Download => "icons/download.svg",
            Icon::ExternalLink => "icons/external-link.svg",
            Icon::Folder => "icons/folder.svg",
            Icon::Locate => "icons/locate.svg",
            Icon::X => "icons/x.svg",
        }
    }

    const fn body(self) -> &'static str {
        match self {
            Icon::ArrowUp => r#"<path d="M12 19V5"/><path d="m5 12 7-7 7 7"/>"#,
            Icon::AspectRatio => {
                r#"<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M7 9v3"/><path d="M7 9h3"/><path d="M17 15v-3"/><path d="M17 15h-3"/>"#
            }
            Icon::Boards => {
                r#"<rect x="3" y="3" width="7" height="9" rx="1.5"/><rect x="14" y="3" width="7" height="5" rx="1.5"/><rect x="14" y="12" width="7" height="9" rx="1.5"/><rect x="3" y="16" width="7" height="5" rx="1.5"/>"#
            }
            Icon::ChevronDown => r#"<path d="m6 9 6 6 6-6"/>"#,
            Icon::Copies => {
                r#"<rect x="8" y="8" width="13" height="13" rx="2"/><path d="M4 16a1 1 0 0 1-1-1V5a2 2 0 0 1 2-2h10a1 1 0 0 1 1 1"/>"#
            }
            Icon::Gallery => {
                r#"<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.1-3.1a2 2 0 0 0-2.8 0L6 21"/>"#
            }
            Icon::Paperclip => {
                r#"<path d="m21.4 11.1-9.2 9.2a6 6 0 0 1-8.5-8.5l8.6-8.6a4 4 0 0 1 5.7 5.7l-8.6 8.6a2 2 0 0 1-2.8-2.8l8.5-8.5"/>"#
            }
            Icon::Settings => {
                r#"<path d="M20 7h-9"/><path d="M14 17H5"/><circle cx="17" cy="17" r="3"/><circle cx="7" cy="7" r="3"/>"#
            }
            Icon::Sparkles => {
                r#"<path d="M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9z"/><path d="M19 15v4"/><path d="M17 17h4"/>"#
            }
            Icon::Branch => {
                r#"<circle cx="6" cy="5" r="2"/><circle cx="6" cy="19" r="2"/><circle cx="18" cy="7" r="2"/><path d="M6 7v10"/><path d="M18 9a7 7 0 0 1-7 7H8"/>"#
            }
            Icon::Chat => r#"<path d="M21 12a8 8 0 0 1-11.8 7L4 20l1.1-4.7A8 8 0 1 1 21 12z"/>"#,
            Icon::Duplicate => {
                r#"<rect x="8" y="8" width="13" height="13" rx="2"/><path d="M4 16a1 1 0 0 1-1-1V5a2 2 0 0 1 2-2h10a1 1 0 0 1 1 1"/><path d="M14.5 12v6"/><path d="M11.5 15h6"/>"#
            }
            Icon::Mix => {
                r#"<circle cx="6" cy="5" r="2"/><circle cx="18" cy="5" r="2"/><circle cx="12" cy="19" r="2"/><path d="M6 7c0 5 6 5 6 10"/><path d="M18 7c0 5-6 5-6 10"/>"#
            }
            Icon::Pencil => {
                r#"<path d="M17 3.5a2.1 2.1 0 0 1 3 3L8 18.5 3.5 20 5 15.5z"/><path d="m15 5.5 3 3"/>"#
            }
            Icon::Retry => r#"<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/>"#,
            Icon::Stop => r#"<rect x="6" y="6" width="12" height="12" rx="2"/>"#,
            Icon::Trash => {
                r#"<path d="M3 6h18"/><path d="M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2"/><path d="M19 6l-1 14a2 2 0 0 1-2 1H8a2 2 0 0 1-2-1L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/>"#
            }
            Icon::Plus => r#"<path d="M12 5v14"/><path d="M5 12h14"/>"#,
            Icon::Compare => {
                r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M12 2v20"/><path d="m7 10-2 2 2 2"/><path d="m17 10 2 2-2 2"/>"#
            }
            Icon::Download => {
                r#"<path d="M12 4v11"/><path d="m7 10 5 5 5-5"/><path d="M4 20h16"/>"#
            }
            Icon::ExternalLink => {
                r#"<path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>"#
            }
            Icon::Folder => {
                r#"<path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.7-.9l-.8-1.2A2 2 0 0 0 7.9 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2z"/>"#
            }
            Icon::Locate => {
                r#"<circle cx="12" cy="12" r="7"/><circle cx="12" cy="12" r="2.5"/><path d="M12 2v3"/><path d="M12 19v3"/><path d="M2 12h3"/><path d="M19 12h3"/>"#
            }
            Icon::X => r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#,
        }
    }

    fn svg_source(self, stroke_width: &str) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="{stroke_width}" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
            self.body()
        )
    }
}

/// An icon element sized to `size` pixels. Callers set its colour with
/// `text_color` (and `group_hover` to follow a parent button's hover).
pub fn icon(icon: Icon, size: f32) -> Svg {
    svg().path(icon.path()).size(px(size)).flex_none()
}

/// Serves the embedded icons; every other path is reported as missing.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        // The arrow on the filled send button reads better slightly bolder.
        Ok(Icon::ALL
            .into_iter()
            .find(|icon| icon.path() == path)
            .map(|icon| {
                let stroke = if icon == Icon::ArrowUp { "2.4" } else { "2" };
                Cow::Owned(icon.svg_source(stroke).into_bytes())
            }))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Icon::ALL
            .into_iter()
            .map(Icon::path)
            .filter(|icon_path| icon_path.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{Assets, Icon};
    use gpui::AssetSource;

    #[test]
    fn every_icon_is_served_as_parseable_svg() {
        for icon in Icon::ALL {
            let bytes = Assets.load(icon.path()).unwrap().expect("icon is embedded");
            let source = std::str::from_utf8(&bytes).unwrap();
            assert!(source.starts_with("<svg") && source.ends_with("</svg>"));
        }
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }
}
