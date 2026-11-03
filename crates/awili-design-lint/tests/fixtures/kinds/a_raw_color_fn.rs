// Kind (a) raw_color_fn: raw colour constructors, optionally gpui::-qualified.
pub fn a1() -> Rgba { gpui::rgb(0xff0000) }
pub fn a2() -> Rgba { gpui::rgba(0x00ff00) }
pub fn a3() -> Hsla { gpui::hsla(0.5, 0.5, 0.5, 1.0) }
pub fn a4() -> Rgba { rgb(0x0000ff) }
pub fn a5() -> Rgba { rgba(0x11223344) }
pub fn a6() -> Hsla { hsla(0.1, 0.2, 0.3, 0.4) }
// negatives: a differently-named fn, and a call with no arguments.
pub fn a7() -> Rgba { myrgb(0xff0000) }
pub fn a8() -> Rgba { rgb() }
