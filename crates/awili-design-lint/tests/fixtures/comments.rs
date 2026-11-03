// rgb(0x123456) in a line comment: not a finding.
/// rgba(0x65432100) in a doc comment: not a finding.
//! hsla(0.1, 0.2, 0.3, 0.4) in an inner doc comment: not a finding.
/* px(12.0), z(8.) and TEXT_SIZE - 1.0 in a block comment: not findings. */

pub fn ok() -> Rgba {
    let x = Rgba { r: 0.1, g: 0.1, b: 0.1, a: 1.0 }; // trailing comment: rgb(0x00ff00)
    let s = "#ff0000"; // a string that is a hex colour: a finding (kind f)
    let t = "rgb(0x000000)"; // a string mentioning rgb(: not kind (a)
    let u = "see /* not a comment */"; // a comment-looking string
    x
}
