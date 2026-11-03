// Kind (c) raw_px: px( followed by a numeric literal.
use gpui::px;

pub fn c1() { let _a = px(12.); }
pub fn c2() { let _b = px(12.0); }
pub fn c3() { let _c = px(12); }
pub fn c4() { let _d = gpui::px(400.); }
pub fn c5() { let _e = px(0.5); }
// negative: the argument is a constant, not a numeric literal.
pub fn c6() { let _f = px(theme::RADIUS); }
