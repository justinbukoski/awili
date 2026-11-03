// Round 4 (Codex r3): the block-comment opener `/*` must be consumed in
// full — BOTH characters — at the outer opener and at every nested opener,
// the way `*/` already is. Before the fix only the `/` was consumed, so the
// opener's `*` joined the nested opener's slash and closed the comment one
// level too early; the stray `*` that leaked into the code masked the
// `Rgba {` literal behind it.

fn f() -> Rgba { return /*/* nested */*/ Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }; }

/**/
let a = px(1.0);
/***/
let b = px(2.0);

fn g() -> Rgba { return /*/**/ */ Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }; }
fn h() -> Rgba { return /* *** */ Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }; }
