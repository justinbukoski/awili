// Kind (f) hex_literal: a string literal matching exactly "#RRGGBB" or "#RRGGBBAA".

pub fn f1() -> &'static str { "#ff0000" }
pub fn f2() -> &'static str { "#FF804080" }
pub fn f3() -> &'static str { "#fff" }
pub fn f4() -> &'static str { "#123456789" }
pub fn f5() -> &'static str { "#12345g" }
pub fn f6() -> &'static str { "x#ff0000" }
pub fn f7() -> &'static str { r#"#0a0b0c"# }
