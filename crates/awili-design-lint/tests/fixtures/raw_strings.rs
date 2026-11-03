// Fingerprints keep the ORIGINAL source line (slice 02b item 3): trimmed
// with internal whitespace collapsed, exactly as written — escaped
// characters and raw-string `#` marks must not change the text.

pub fn f() {
    let (s, x) = (r#"a"#, px(12.0));
    let t = "esc\naped"; let y = z(3.0);
    let u = r"plain";
    let w =  px(1.0);
}
