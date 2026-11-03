// Slice 02b item 5: `px(`, `z(` and `.opacity(` whose numeric argument is on
// a following line (whitespace or comments only in between) is a finding,
// reported on the line of the call.

pub fn s1() {
    let a = px(
        12.0,
    );
}

// a trailing comment between the call and its argument is fine.
pub fn s2() {
    let b = theme::z( // split
        8.0);
}

// only comments between the call and the argument.
pub fn s3() {
    let c = z(
        // an intervening comment line
        8.0,
    );
}

pub fn s4(d: &gpui::Div) {
    d.opacity(
        0.5,
    );
}

// negatives: the argument is a constant, or the line after the call does not
// start with a numeric literal.
pub fn neg1() {
    let d = px(theme::RADIUS);
}
pub fn neg2() {
    let e = px(
        theme::RADIUS,
    );
}
pub fn neg3(d: &gpui::Div, alpha: f32) {
    d.opacity(
        alpha,
    );
}
