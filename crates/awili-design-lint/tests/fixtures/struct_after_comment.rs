// Slice 02c item 1: a line-start struct literal following a comment-only
// line (`//`, `/* … */`, a blank line) still continues the expression from
// the last line with code above it.

pub fn after_line_comment() -> Rgba {
    // colour
    Rgba { r: 0.1, g: 0.1, b: 0.1, a: 1.0 }
}

pub fn after_block_comment() -> Rgba {
    /* colour */
    Rgba { r: 0.2, g: 0.2, b: 0.2, a: 1.0 }
}

pub fn after_comment_and_blank() -> Rgba {
    // colour

    Rgba { r: 0.3, g: 0.3, b: 0.3, a: 1.0 }
}
