//! Scanner tests: one positive fixture per kind, the return-type negative,
//! the `#[cfg(test)]` skip, the name/directory exemptions, comments, and
//! duplicate-line indices.

use std::path::{Path, PathBuf};

use awili_design_lint::{scan, Finding};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The (line, kind) signatures of `findings` restricted to `path`.
fn sig(findings: &[Finding], path: &str) -> Vec<(u32, &'static str)> {
    findings
        .iter()
        .filter(|f| f.path == path)
        .map(|f| (f.line, f.kind))
        .collect()
}

fn expect(findings: &[Finding], path: &str, expected: &[(u32, &'static str)]) {
    let got = sig(findings, path);
    assert_eq!(got, expected, "fixture {path}: unexpected findings");
}

#[test]
fn one_positive_fixture_per_kind() {
    let findings = scan(&fixtures().join("kinds")).unwrap();

    // (a) raw_color_fn — six positives; myrgb( and rgb() (no args) are not.
    expect(
        &findings,
        "a_raw_color_fn.rs",
        &[
            (2, "raw_color_fn"),
            (3, "raw_color_fn"),
            (4, "raw_color_fn"),
            (5, "raw_color_fn"),
            (6, "raw_color_fn"),
            (7, "raw_color_fn"),
        ],
    );

    // (b) raw_color_struct — six literal sites; every `-> T {` return type is
    // not a finding (lines 4/5/7/9 type positions, plus the (Rgba, Rgba) tuple
    // type on line 6).
    expect(
        &findings,
        "b_raw_color_struct.rs",
        &[
            (4, "raw_color_struct"),
            (5, "raw_color_struct"),
            (6, "raw_color_struct"),
            (6, "raw_color_struct"),
            (7, "raw_color_struct"),
            (9, "raw_color_struct"),
        ],
    );

    // (c) raw_px — px( with a numeric literal; px(theme::RADIUS) is not.
    expect(
        &findings,
        "c_raw_px.rs",
        &[
            (4, "raw_px"),
            (5, "raw_px"),
            (6, "raw_px"),
            (7, "raw_px"),
            (8, "raw_px"),
        ],
    );

    // (d) raw_z — theme::z( / z( with a numeric literal; a constant arg is not.
    expect(
        &findings,
        "d_raw_z.rs",
        &[(3, "raw_z"), (4, "raw_z"), (5, "raw_z")],
    );

    // (e) token_math — a DESIGN TOKEN (theme::NAME, or a bare NAME declared
    // in a theme.rs in the scanned tree as `pub const` f32 / Pixels / Rgba)
    // combined with a numeric literal, in either direction. Local consts and
    // non-design-typed theme consts are not findings.
    expect(
        &findings,
        "e_token_math.rs",
        &[
            (8, "token_math"),
            (9, "token_math"),
            (10, "token_math"),
            (11, "token_math"),
            (12, "token_math"),
        ],
    );

    // (f) hex_literal — exactly "#RRGGBB" or "#RRGGBBAA" string literals,
    // including raw strings; near-misses are not.
    expect(
        &findings,
        "f_hex_literal.rs",
        &[(3, "hex_literal"), (4, "hex_literal"), (9, "hex_literal")],
    );

    // (g) raw_opacity — .opacity( with a numeric literal; a variable is not.
    expect(
        &findings,
        "g_raw_opacity.rs",
        &[(4, "raw_opacity"), (5, "raw_opacity"), (6, "raw_opacity")],
    );

    // No cross-kind noise in the kind fixtures: every finding is of its kind.
    let by_kind: Vec<(&'static str, usize)> = findings
        .iter()
        .map(|f| f.kind)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|k| (k, findings.iter().filter(|f| f.kind == k).count()))
        .collect();
    assert_eq!(
        by_kind,
        vec![
            ("hex_literal", 3),
            ("raw_color_fn", 6),
            ("raw_color_struct", 6),
            ("raw_opacity", 3),
            ("raw_px", 5),
            ("raw_z", 3),
            ("token_math", 5),
        ]
    );
}

#[test]
fn return_type_is_not_a_finding() {
    let findings = scan(&fixtures()).unwrap();
    // Only the two real literals (lines 6 and 10); the `-> gpui::Rgba {`
    // return-type lines 5 and 9 must be absent.
    expect(
        &findings,
        "return_type.rs",
        &[(6, "raw_color_struct"), (10, "raw_color_struct")],
    );
}

#[test]
fn fingerprints_use_original_line_text() {
    let findings = scan(&fixtures()).unwrap();
    // The finding text is the ORIGINAL source line, trimmed with internal
    // whitespace collapsed: the raw-string `#` marks survive, the escaped
    // `\n` keeps both characters, and the double space on the last line
    // collapses to one.
    let got: Vec<(u32, &str)> = findings
        .iter()
        .filter(|f| f.path == "raw_strings.rs")
        .map(|f| (f.line, f.text.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            (6, "let (s, x) = (r#\"a\"#, px(12.0));"),
            (7, "let t = \"esc\\naped\"; let y = z(3.0);"),
            (9, "let w = px(1.0);"),
        ],
        "finding text must be the original line, never a reconstruction"
    );
}

#[test]
fn char_literals_vs_lifetimes() {
    let findings = scan(&fixtures()).unwrap();
    // Both `px(12.)` lines are findings — including the one on the same line
    // after a char literal — and nothing else: the char literals blank their
    // whole content, while the lifetimes in `g` stay plain code.
    expect(
        &findings,
        "char_literals.rs",
        &[(12, "raw_px"), (17, "raw_px")],
    );
}

#[test]
fn block_comment_does_not_leak_code() {
    let findings = scan(&fixtures()).unwrap();
    // `let c = /* x */ Rgba { … }` is a struct literal, `px( /* w */ 12.0)`
    // is a raw px call, the nested-comment line and `SIZE /* x */ 5` (which
    // must NOT lex as `SIZE / 5`) are clean.
    expect(
        &findings,
        "block_comments.rs",
        &[(6, "raw_color_struct"), (7, "raw_px")],
    );
}

#[test]
fn cfg_test_module_is_skipped() {
    let findings = scan(&fixtures()).unwrap();
    // Live code before and after the module is scanned; every line inside
    // `#[cfg(test)] mod tests { … }` (lines 7–21) is skipped.
    expect(
        &findings,
        "cfg_test.rs",
        &[(4, "raw_color_struct"), (24, "raw_color_fn")],
    );
}

#[test]
fn theme_and_content_palette_files_are_exempt() {
    let findings = scan(&fixtures()).unwrap();
    let exempt = [
        "theme.rs",
        "terminal_palette.rs",
        "chart_palette.rs",
        "profile_palette.rs",
    ];
    assert!(
        findings.iter().all(|f| !exempt.contains(&f.path.as_str())),
        "exempt-by-name files must not produce findings"
    );
}

#[test]
fn files_ending_tests_rs_are_exempt() {
    // The fixtures directory itself contains no *_tests.rs files; verify the
    // rule directly with a generated one.
    let dir = temp_dir("exempt_tests_suffix");
    std::fs::write(
        dir.join("foo_tests.rs"),
        "pub fn f() { let _ = gpui::rgb(0x123456); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("foo.rs"),
        "pub fn f() { let _ = gpui::rgb(0x123456); }\n",
    )
    .unwrap();
    let findings = awili_design_lint::scan(&dir).unwrap();
    let paths: Vec<&str> = findings.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["foo.rs"], "only the non-_tests file is scanned");
}

#[test]
fn comments_and_string_noise() {
    let findings = scan(&fixtures()).unwrap();
    // The `Rgba {` literal and the hex string; comment mentions of rgb(/rgba(
    // /hsla(/px(/z( and the string "rgb(0x000000)" are not findings.
    expect(
        &findings,
        "comments.rs",
        &[(7, "raw_color_struct"), (8, "hex_literal")],
    );
}

#[test]
fn anything_under_a_tests_directory_is_exempt() {
    let findings = scan(&fixtures().join("nested")).unwrap();
    expect(&findings, "app.rs", &[(2, "raw_color_fn")]);
    assert!(
        findings.iter().all(|f| f.path != "tests/it.rs"),
        "tests/ directory is exempt"
    );
}

#[test]
fn struct_literals_in_every_expression_position() {
    let findings = scan(&fixtures()).unwrap();
    // Literals: match arms (9/10), `return` (17), the trailing body
    // expression (19) and the closure body (24), the field value (30), the
    // body literals of the `-> Rgba {` lines (35/37/38), and the line-start
    // continuation (44). Type positions are absent: `-> Rgba {` (7/15/23/35/
    // 37/38/43), `: Rgba,` / `: Rgba)` (36/37), `: Rgba>` (38) and
    // `impl … for Rgba {` (40).
    expect(
        &findings,
        "struct_positions.rs",
        &[
            (9, "raw_color_struct"),
            (10, "raw_color_struct"),
            (17, "raw_color_struct"),
            (19, "raw_color_struct"),
            (24, "raw_color_struct"),
            (30, "raw_color_struct"),
            (35, "raw_color_struct"),
            (37, "raw_color_struct"),
            (38, "raw_color_struct"),
            (44, "raw_color_struct"),
        ],
    );
}

#[test]
fn a_first_line_struct_literal_is_not_a_finding() {
    let findings = scan(&fixtures()).unwrap();
    // The file's first line starts with `Rgba {`: there is no previous line,
    // so it does not continue an expression. (The old code read the first
    // line's own `prev_code` via `saturating_sub`; it must be `None`.)
    expect(&findings, "struct_first_line.rs", &[]);
}

#[test]
fn split_line_arguments_are_reported_on_the_call_line() {
    let findings = scan(&fixtures()).unwrap();
    // `px(`, `theme::z(`, `z(` (with an intervening comment line) and
    // `.opacity(` whose numeric argument is on a following line: reported on
    // the line of the call. Constant arguments are not findings.
    expect(
        &findings,
        "split_args.rs",
        &[
            (6, "raw_px"),
            (13, "raw_z"),
            (19, "raw_z"),
            (26, "raw_opacity"),
        ],
    );
}

#[test]
fn cfg_test_single_line_form_is_skipped_by_range() {
    let findings = scan(&fixtures()).unwrap();
    // The skip range runs from `#[cfg(test)]` through the closing brace of
    // the mod (single-line form): `px(1.)` inside it is skipped, but the
    // `fn b(){ px(2.); }` on the SAME line after the closing brace is still
    // scanned — exactly one finding.
    expect(&findings, "cfg_test_single_line.rs", &[(1, "raw_px")]);
}

// --- docs/v2/slices/02c-lint-fixes.md (round 3): a line-start struct
// literal separated from the expression by comment-only lines, and
// UTF-8 byte/char safety of the `#[cfg(test)]` skip ranges.

#[test]
fn struct_literals_after_comment_only_lines() {
    let findings = scan(&fixtures()).unwrap();
    // A line-start `Rgba {` continues the expression from the last line
    // with CODE above it — walking back over `//`, `/* … */` and blank
    // lines. All three cases are findings; the `-> Rgba {` return-type
    // lines are not.
    expect(
        &findings,
        "struct_after_comment.rs",
        &[
            (7, "raw_color_struct"),
            (12, "raw_color_struct"),
            (18, "raw_color_struct"),
        ],
    );
}

#[test]
fn cfg_test_skip_range_is_utf8_safe() {
    let findings = scan(&fixtures()).unwrap();
    // The 02c item-2 repro: the multi-byte `fn caféé() {}` line before
    // `#[cfg(test)]` must not shift the skip range — the test module's
    // `px(1.)` is skipped and the trailing `px(2.)` is found. Multi-byte
    // characters inside a string literal and a comment before the last
    // finding pin the fingerprint text and line number.
    expect(
        &findings,
        "cfg_test_utf8.rs",
        &[(4, "raw_px"), (6, "raw_px")],
    );
    let f = findings
        .iter()
        .find(|f| f.path == "cfg_test_utf8.rs" && f.line == 6)
        .expect("the line-6 finding");
    assert_eq!(f.kind, "raw_px");
    assert_eq!(f.text, "let _c = px(3.);");
    assert_eq!(f.fingerprint, "cfg_test_utf8.rs|raw_px|let _c = px(3.);|0");
}

#[test]
fn multibyte_code_before_the_cfg_test_attr_does_not_shift_the_range() {
    let findings = scan(&fixtures()).unwrap();
    // The `#[cfg(test)]` regex match reports byte offsets, while the skip
    // ranges and `CodeIter` are char-based. Three multi-byte identifier
    // chars on the attribute line, before the attribute, shifted the range
    // right until the `mod` was missed and its test code was scanned. The
    // `px(1.)` inside the module is skipped; the trailing `px(2.)` is the
    // only finding.
    expect(&findings, "cfg_test_utf8_attr.rs", &[(3, "raw_px")]);
}

// --- Round 4 (Codex r3): the block-comment opener `/*` must be consumed in
// full — both characters — at the outer opener and at every nested opener,
// the way `*/` already is. The pre-fix opener consumed only the `/`, so the
// opener's `*` joined the nested opener's slash and closed the comment one
// level too early; the stray `*` that leaked into the code masked the
// `Rgba {` literal behind it.

#[test]
fn nested_block_comment_openers_consume_both_chars() {
    let findings = scan(&fixtures()).unwrap();
    // The exact Codex r3 repro (line 8) is nested — `/*` inside `/*` — and
    // gives exactly ONE `raw_color_struct`: the `Rgba {` after `return`
    // (the `-> Rgba {` on the same line is a return type, not a literal).
    // The `/**/` (line 10) and `/***/` (line 12) edge cases are fully
    // consumed as comments and the code after them is still detected
    // (lines 11/13). Line 15 (`/*/**/ */` — a `/**/` nested inside `/*`,
    // closed by `*/`) and line 16 (`/* *** */`) must not leak stray
    // `*`/`/` characters either.
    expect(
        &findings,
        "nested_comment_openers.rs",
        &[
            (8, "raw_color_struct"),
            (11, "raw_px"),
            (13, "raw_px"),
            (15, "raw_color_struct"),
            (16, "raw_color_struct"),
        ],
    );
    // The repro line itself: exactly one finding, kind and text pinned.
    let repro: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.path == "nested_comment_openers.rs" && f.line == 8)
        .collect();
    assert_eq!(repro.len(), 1, "exactly one finding on the repro line");
    assert_eq!(repro[0].kind, "raw_color_struct");
    assert_eq!(
        repro[0].text,
        "fn f() -> Rgba { return /*/* nested */*/ Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }; }"
    );
}

#[test]
fn duplicate_lines_get_distinct_indices() {
    let findings = scan(&fixtures()).unwrap();
    let dup: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.path == "duplicates.rs")
        .collect();
    assert_eq!(dup.len(), 2, "both duplicate lines are findings");
    assert_eq!(dup[0].line, 3);
    assert_eq!(dup[1].line, 4);
    assert_eq!(dup[0].kind, "raw_color_fn");
    assert_eq!(dup[0].text, dup[1].text, "same normalised line text");
    assert_ne!(
        dup[0].fingerprint, dup[1].fingerprint,
        "the occurrence index keeps duplicates distinct"
    );
    assert!(
        dup[0].fingerprint.ends_with("|0") && dup[1].fingerprint.ends_with("|1"),
        "indices are 0-based in line order: {:?}",
        [dup[0].fingerprint.as_str(), dup[1].fingerprint.as_str()]
    );
}

// --- ratchet safety (docs/v2/slices/02b-lint-fixes.md item 8): a missing or
// unreadable src-dir, a WalkDir error, or a file that cannot be read makes
// the scan FAIL (the CLI maps scan errors to exit 2). A silent partial scan
// would under-report debt.

#[test]
fn scan_fails_on_a_missing_src_dir() {
    let err = scan(Path::new("/definitely-missing")).unwrap_err();
    assert!(
        err.to_string().contains("/definitely-missing"),
        "the error names the missing directory: {err:?}"
    );
}

#[test]
fn scan_fails_on_an_unreadable_scanned_file() {
    let dir = temp_dir("scan-unreadable");
    let ok = dir.join("ok.rs");
    std::fs::write(&ok, "pub fn f() { let _ = px(1.0); }\n").unwrap();
    let secret = dir.join("secret.rs");
    std::fs::write(&secret, "pub fn g() { let _ = px(2.0); }\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let err = scan(&dir).unwrap_err();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644));
    }
    assert!(err.to_string().contains("secret.rs"), "{err:?}");
}

#[test]
fn scan_fails_on_an_unreadable_theme_file() {
    // A theme.rs the scanner must read to build the design-token name set is
    // as much a scanned file as anything: unreadable → fail, not a silently
    // empty token set (which would let token_math findings slip past the
    // ratchet).
    let dir = temp_dir("scan-unreadable-theme");
    std::fs::write(dir.join("a.rs"), "pub fn f() { let _ = px(1.0); }\n").unwrap();
    let theme = dir.join("theme.rs");
    std::fs::write(&theme, "pub const S: f32 = 1.0;\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&theme, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let err = scan(&dir).unwrap_err();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&theme, std::fs::Permissions::from_mode(0o644));
    }
    assert!(err.to_string().contains("theme.rs"), "{err:?}");
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("awili-design-lint-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
