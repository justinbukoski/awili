// The return-type case: `-> gpui::Rgba {` is a function's return type
// followed by its body brace. It is NOT a struct literal: not a finding.
use gpui::Rgba;

pub fn neg_one() -> gpui::Rgba {
    Rgba { r: 0.5, g: 0.5, b: 0.5, a: 1.0 }
}

pub fn neg_two() -> gpui::Rgba {
    gpui::Rgba {
        r: 0.5,
        g: 0.5,
        b: 0.5,
        a: 1.0,
    }
}
