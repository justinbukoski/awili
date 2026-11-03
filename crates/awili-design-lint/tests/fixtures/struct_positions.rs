// Slice 02b item 4: `Rgba {` / `Hsla {` (optionally `gpui::`-qualified) is a
// struct literal in every expression position and NOT a literal in a type
// position.
use gpui::Rgba;

// match arms: `=> Rgba {` and `=> Hsla {` are literals.
pub fn arms(x: u32) -> Rgba {
    match x {
        1 => Rgba { r: 0.1, g: 0.1, b: 0.1, a: 1.0 },
        _ => gpui::Hsla { r: 0.2, g: 0.2, b: 0.2, a: 1.0 },
    }
}

// `return Rgba {` is a literal.
pub fn ret(c: bool) -> Rgba {
    if c {
        return Rgba { r: 0.3, g: 0.3, b: 0.3, a: 1.0 };
    }
    gpui::Rgba { r: 0.4, g: 0.4, b: 0.4, a: 1.0 }
}

// a closure body: `|c| Rgba {` is a literal.
pub fn closure() -> impl Fn(bool) -> Rgba {
    |c| Rgba { r: 0.5, g: 0.5, b: 0.5, a: 1.0 }
}

// a field value: `tone: Rgba {` is a literal.
pub struct Foo { tone: Rgba }
pub fn field() -> Foo {
    Foo { tone: Rgba { r: 0.6, g: 0.6, b: 0.6, a: 1.0 } }
}

// type positions are NOT literals: `-> Rgba {` (the return type before the
// body brace), `: Rgba,` / `: Rgba)` / `: Rgba>`, and `impl … for Rgba {`.
pub fn type_one() -> Rgba { Rgba { r: 0.7, g: 0.7, b: 0.7, a: 1.0 } }
pub struct TypeTwo { a: Rgba, b: Hsla }
pub fn type_three() -> (Rgba, Rgba) { gpui::Rgba { r: 0.8, g: 0.8, b: 0.8, a: 1.0 } }
pub fn type_four() -> Vec<Rgba> { gpui::Rgba { r: 0.8, g: 0.8, b: 0.8, a: 0.9 } }
pub trait TypeFive {}
impl TypeFive for Rgba { fn f() { let _ = 1.0; } }

// line start: a line starting with `Rgba {` continues an expression.
pub fn line_start() -> Rgba {
    Rgba { r: 0.9, g: 0.9, b: 0.9, a: 1.0 }
}
