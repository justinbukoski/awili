//! Usage-chart content colours: chart data, not UI chrome; exempt from
//! design-lint.
//!
//! The usage bar chart's series colours are CONTENT (they encode which
//! series is which, like the terminal's ANSI palette in
//! `terminal_palette.rs`), so they are declared here as raw hex instead of
//! theme tokens and stop borrowing the UI palette: the input series no
//! longer reads `theme::ACCENT` and the output series no longer reads
//! `theme::ACCENT_SECONDARY` (sage). The values move from the theme byte-
//! identical (KIT.md v3, slice K batch K1), so the chart is unchanged.
#![allow(clippy::disallowed_methods)]

use gpui::Rgba;

/// Opaque hex → `Rgba` (content module: the raw decode, like the ANSI
/// palette, not a theme token).
const fn rgb(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 16) & 0xFF) as f32 / 255.0,
        g: ((hex >> 8) & 0xFF) as f32 / 255.0,
        b: (hex & 0xFF) as f32 / 255.0,
        a: 1.0,
    }
}

/// The input-token series (was `theme::ACCENT`, value unchanged).
pub const INPUT: Rgba = rgb(0x3b82f6);
/// The output-token series (was `theme::ACCENT_SECONDARY`, value unchanged).
pub const OUTPUT: Rgba = rgb(0x6dba8e);
