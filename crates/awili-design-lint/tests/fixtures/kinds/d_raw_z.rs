// Kind (d) raw_z: z( followed by a numeric literal, including theme::z(.

pub fn d1() { let _a = theme::z(12.); }
pub fn d2() { let _b = theme::z(12); }
pub fn d3() { let _c = z(8.0); }
// negative: the argument is a constant, not a numeric literal.
pub fn d4() { let _d = theme::z(theme::RADIUS); }
