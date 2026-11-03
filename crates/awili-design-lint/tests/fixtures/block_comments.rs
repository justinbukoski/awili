// Block comments must not leak code (slice 02b item 1): on `*/` both
// characters are consumed, so the code around a comment scans as if the
// comment were a run of whitespace and no stray `/` remains.

pub fn a() -> Rgba {
    let c = /* x */ Rgba { r: 0.1, g: 0.2, b: 0.3, a: 1.0 };
    let d = px( /* w */ 12.0);
    let n = /* nested /* inner */ still comment */ 4.0;
    SIZE /* x */ 5
    c
}
