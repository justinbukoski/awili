// Kind (b) raw_color_struct: Rgba/Hsla struct literals, optionally gpui::-qualified.
use gpui::Rgba;

pub fn b1() -> Rgba { Rgba { r: 0.1, g: 0.2, b: 0.3, a: 1.0 } }
pub fn b2() -> Rgba { let x = Rgba { r: 0.1, g: 0.2, b: 0.3, a: 0.0 }; x }
pub fn b3() -> (Rgba, Rgba) { (Rgba { r: 0.9, g: 0.9, b: 0.9, a: 1.0 }, Rgba { r: 0.8, g: 0.8, b: 0.8, a: 1.0 }) }
pub fn b4() -> Hsla { Hsla::from(gpui::Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }) }
// negative: a return type followed by its body brace (see return_type.rs).
pub fn b5() -> gpui::Rgba { Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 } }
