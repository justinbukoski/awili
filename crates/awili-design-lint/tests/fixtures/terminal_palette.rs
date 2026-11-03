//! Exempt by file name: terminal_palette.rs holds ANSI terminal colours.
pub const ANSI_COLORS: [u32; 16] = [0x000000, 0xcc0000];
pub fn rgb_to_hsla(hex: u32) -> Hsla { Hsla::from(gpui::Rgba { r: 0.1, g: 0.2, b: 0.3, a: 1.0 }) }
pub fn z_demo() { let _ = theme::z(8.); }
