// Kind (e) token_math: a design-token constant combined with a numeric
// literal by + - * /. A design token is `theme::NAME`, or a bare NAME that a
// theme.rs in the scanned tree declares as `pub const` with an f32, Pixels
// or Rgba type.
const TEXT_SIZE_LOCAL: f32 = 14.0; // a local const: NOT a design token
const MAX_BYTES: u64 = 64 * 1024; // a local const: NOT a design token

pub fn e1() { let _a = TEXT_SIZE - 1.0; } // bare design token (kinds/theme.rs)
pub fn e2() { let _b = theme::LABEL_SIZE * 1.2; } // qualified
pub fn e3() { let _c = 1.5 + theme::RADIUS; } // qualified, reversed
pub fn e4() { let _d = theme::TEXT_SIZE / 2.0; } // qualified, reversed
pub fn e5() { let _e = MARGIN + 1; } // bare Pixels token
// negatives: not design tokens, or no numeric literal.
pub fn e6() { let _f = TEXT_SIZE_LOCAL - 1.0; } // local const, not in a theme.rs
pub fn e7() { let _g = MAX_BYTES + 1; } // local const, not in a theme.rs
pub fn e8() { let _h = MAX_BLOCKS - 1; } // in theme.rs, but a usize
pub fn e9() { let _i = theme::TEXT_SIZE + theme::LABEL_SIZE; } // two constants
