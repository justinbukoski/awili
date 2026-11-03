//! Exempt by file name: profile_palette.rs holds user-chosen profile colours.
pub fn color(c: u32) -> Hsla { Hsla::from(rgba(c)) }
pub fn tint() -> Rgba { gpui::rgba(0xef4444ff) }
