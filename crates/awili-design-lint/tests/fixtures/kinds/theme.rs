//! Exempt by file name: theme.rs is the token source. The scanner reads its
//! `pub const` design tokens (f32 / Pixels / Rgba) to know which bare names
//! count for kind (e) token_math.
pub const RADIUS: f32 = 8.0;
pub const TEXT_SIZE: f32 = 14.0;
pub const LABEL_SIZE: f32 = 11.0;
pub const MARGIN: Pixels = 4.0;
pub const ACCENT: Rgba = rgb(0xff0000);
// Not design tokens: the type is not f32, Pixels or Rgba.
pub const MAX_BLOCKS: usize = 1024;
pub const MAX_BYTES: u64 = 64 * 1024;
