//! The application palette.
//!
//! Card sprites are encoded as SVG, so every colour has to be expressible both
//! as a GPUI `Hsla` and as an SVG `#rrggbb` string. Both are derived from the
//! constants here; they used to be written out twice and could drift apart
//! without anything noticing.

use gpui::{Hsla, rgb};

pub const BACKGROUND: u32 = 0x0d0e12;
pub const RAISED: u32 = 0x14161c;
pub const HOVER: u32 = 0x1b1e26;
pub const LINE: u32 = 0x262a35;
pub const INK: u32 = 0xe8eaf0;
pub const DIM: u32 = 0x8b90a0;
pub const FAINT: u32 = 0x5a5f6e;
pub const ACCENT: u32 = 0x7c8cff;
pub const ACCENT_STRONG: u32 = 0x5666f7;
pub const DANGER: u32 = 0xff6b6b;

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
