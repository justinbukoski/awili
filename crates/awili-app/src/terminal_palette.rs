//! Terminal content colours: ANSI, not UI chrome; exempt from design-lint
#![allow(clippy::disallowed_methods)]

use gpui::{Hsla, Rgba};

/// ANSI 16-color palette (dark variant, roughly matching iTerm2 Tango Dark).
pub const ANSI_COLORS: [u32; 16] = [
    0x000000, // 0  black
    0xcc0000, // 1  red
    0x4e9a06, // 2  green
    0xc4a000, // 3  yellow
    0x3465a4, // 4  blue
    0x75507b, // 5  magenta
    0x06989a, // 6  cyan
    0xd3d7cf, // 7  white
    0x555753, // 8  bright black
    0xef2929, // 9  bright red
    0x8ae234, // 10 bright green
    0xfce94f, // 11 bright yellow
    0x729fcf, // 12 bright blue
    0xad7fa8, // 13 bright magenta
    0x34e2e2, // 14 bright cyan
    0xeeeeec, // 15 bright white
];

pub fn rgb_to_hsla(hex: u32) -> Hsla {
    Hsla::from(Rgba {
        r: ((hex >> 16) & 0xFF) as f32 / 255.0,
        g: ((hex >> 8) & 0xFF) as f32 / 255.0,
        b: (hex & 0xFF) as f32 / 255.0,
        a: 1.0,
    })
}
