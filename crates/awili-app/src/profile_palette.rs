//! Profile content colours: the agent-profile accent palette; exempt from
//! design-lint.
//!
//! Profile accents are CONTENT (they distinguish the owner's own agent
//! contexts, like the terminal's ANSI palette in `terminal_palette.rs`),
//! not UI chrome, so the hex values live here instead of a screen helper
//! (KIT.md v3: "Profile colours stay content, in their own module"). The
//! values move from `main.rs`'s `profile_color_to_hsla` byte-identical
//! (slice K batch K1), so the profile rail is unchanged.
#![allow(clippy::disallowed_methods)]

use gpui::{Hsla, Rgba};

use crate::profile::ProfileColor;

/// The accent hex for a profile colour (byte-identical to the previous
/// in-`main` match table).
pub const fn hex(color: ProfileColor) -> u32 {
    match color {
        ProfileColor::Blue => 0x3b82f6,
        ProfileColor::Green => 0x22c55e,
        ProfileColor::Orange => 0xf97316,
        ProfileColor::Purple => 0xa855f7,
        ProfileColor::Red => 0xef4444,
        ProfileColor::Teal => 0x14b8a6,
        ProfileColor::Yellow => 0xeab308,
        ProfileColor::Pink => 0xec4899,
    }
}

/// The accent as an `Rgba` (opaque).
pub const fn rgba(color: ProfileColor) -> Rgba {
    let h = hex(color);
    Rgba {
        r: ((h >> 16) & 0xFF) as f32 / 255.0,
        g: ((h >> 8) & 0xFF) as f32 / 255.0,
        b: (h & 0xFF) as f32 / 255.0,
        a: 1.0,
    }
}

/// The accent as an `Hsla`, for `gpui` paint sites (the previous
/// `profile_color_to_hsla` conversion, unchanged).
pub fn color(color: ProfileColor) -> Hsla {
    Hsla::from(rgba(color))
}
