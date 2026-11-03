// Char literals vs lifetimes (slice 02b item 2): `'x'`, `'\n'`, `'\''`,
// `'\u{1F600}'` are char literals and must blank their whole content;
// `'a` in `&'a T` / `impl<'a>` is a lifetime and stays code. Code after a
// char literal on the same line is still scanned.

pub fn f() {
    let c = 'x';
    let n = '\n';
    let q = '\'';
    let e = '\u{1F600}';
    let s = ' ';
    let y = px(12.);
}

pub fn g<'a>(t: &'a u32, s: &str) -> &'a u32 {
    let _ = s;
    let c = 'x'; let y = px(12.);
    t
}
