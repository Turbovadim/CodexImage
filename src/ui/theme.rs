//! The application palette.
//!
//! Card sprites are encoded as SVG, so every colour has to be expressible both
//! as a GPUI `Hsla` and as an SVG `#rrggbb` string. Both are derived from the
//! constants here; they used to be written out twice and could drift apart
//! without anything noticing.

use gpui::{Hsla, rgb};

pub const BACKGROUND: u32 = 0x000000;
pub const RAISED: u32 = 0x101116;
pub const HOVER: u32 = 0x1a1c23;
pub const LINE: u32 = 0x262a35;
pub const INK: u32 = 0xe8eaf0;
pub const DIM: u32 = 0x8b90a0;
pub const FAINT: u32 = 0x5a5f6e;
pub const ACCENT: u32 = 0x7c8cff;
pub const ACCENT_STRONG: u32 = 0x5666f7;
pub const DANGER: u32 = 0xff6b6b;

/// The face every card label is set in, by both the direct painter and the
/// SVG sprite renderer, so a zoom settle never swaps typefaces. macOS's own
/// UI font is off limits: resvg resolves it but draws no ink from its
/// outlines, and its other names do not resolve at all. Helvetica Neue is its
/// near twin and both renderers load it. Windows keeps the system font, which
/// resvg renders under its real name.
#[cfg(target_os = "macos")]
pub const CARD_FONT_FAMILY: &str = "Helvetica Neue";
#[cfg(target_os = "macos")]
pub const CARD_FONT_SVG_FAMILIES: &str = "'Helvetica Neue', sans-serif";
#[cfg(not(target_os = "macos"))]
pub const CARD_FONT_FAMILY: &str = ".SystemUIFont";
#[cfg(not(target_os = "macos"))]
pub const CARD_FONT_SVG_FAMILIES: &str = "'Segoe UI', system-ui, sans-serif";

/// The card face's ascent and descent in em units (hhea). GPUI centres the
/// ascent-plus-descent box inside the line height and puts the baseline at
/// the ascent, so the sprite places it the same way or labels shift on swap.
#[cfg(target_os = "macos")]
pub const CARD_FONT_ASCENT: f32 = 0.952;
#[cfg(target_os = "macos")]
pub const CARD_FONT_DESCENT: f32 = 0.213;
#[cfg(not(target_os = "macos"))]
pub const CARD_FONT_ASCENT: f32 = 1.079;
#[cfg(not(target_os = "macos"))]
pub const CARD_FONT_DESCENT: f32 = 0.251;

pub fn background() -> Hsla {
    rgb(BACKGROUND).into()
}
pub fn raised() -> Hsla {
    rgb(RAISED).into()
}
pub fn hover() -> Hsla {
    rgb(HOVER).into()
}
pub fn line() -> Hsla {
    rgb(LINE).into()
}
pub fn ink() -> Hsla {
    rgb(INK).into()
}
pub fn dim() -> Hsla {
    rgb(DIM).into()
}
pub fn faint() -> Hsla {
    rgb(FAINT).into()
}
pub fn accent() -> Hsla {
    rgb(ACCENT).into()
}
pub fn accent_strong() -> Hsla {
    rgb(ACCENT_STRONG).into()
}
pub fn danger() -> Hsla {
    rgb(DANGER).into()
}
