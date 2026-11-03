// Kind (g) raw_opacity: .opacity( followed by a numeric literal.

pub fn g1(d: &gpui::Div, alpha: f32) {
    d.opacity(0.55);
    d.opacity(0.);
    d.opacity(1);
    // negative: the argument is a variable.
    d.opacity(alpha);
}
