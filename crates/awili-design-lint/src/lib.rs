//! `awili-design-lint` — the design-token ratchet for the Awili UI
//! (docs/v2/PLAN.md, slice 2).
//!
//! From this slice on, no new raw colour or size may enter the Awili UI code.
//! The linter scans `*.rs` files for raw colour/size expressions and compares
//! their fingerprints against a baseline (`design-baseline.toml`) that can
//! only shrink:
//!
//! - `list <src-dir>` — print every finding.
//! - `check <src-dir> --baseline <file>` — exit 1 on any finding whose
//!   fingerprint is not in the baseline; print how many baseline entries are
//!   stale (exit unaffected).
//! - `check … --update-baseline` — rewrite the baseline keeping only the
//!   findings that still exist; refuse to write (exit 1) if any finding is
//!   NOT in the baseline (the baseline can never grow).
//! - `check … --init` — one-shot: write the baseline from the current tree,
//!   allowed only while the file does not exist yet.
//!
//! Exempt from the scan: files named `theme.rs`, `terminal_palette.rs`, `chart_palette.rs` or `profile_palette.rs` (the declared content palettes, DESIGN.md §2)
//! (the token sources), files ending `_tests.rs`, anything under a `tests/`
//! directory, and the exact byte range of a `#[cfg(test)]` attribute through
//! the closing brace of the following `mod` (the single-line form
//! `#[cfg(test)] mod t { … }` included; code on the same line after the
//! closing brace is still scanned). Comments and string-literal text are
//! ignored except for kind (f) `hex_literal`, which is *defined* on string
//! literals.
//!
//! Kind (e) `token_math` flags only DESIGN TOKENS: a `theme::NAME` constant,
//! or a bare `NAME` that a `theme.rs` in the scanned tree declares as
//! `pub const` with an `f32`, `Pixels` or `Rgba` type.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use walkdir::WalkDir;

/// Baseline format version.
pub const BASELINE_VERSION: u32 = 1;

/// The seven finding kinds, as named in the slice doc.
pub const KINDS: [&str; 7] = [
    "raw_color_fn",
    "raw_color_struct",
    "raw_px",
    "raw_z",
    "token_math",
    "hex_literal",
    "raw_opacity",
];

/// One offending expression on one line of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Path relative to the scanned directory, `/`-separated.
    pub path: String,
    /// One of [`KINDS`].
    pub kind: &'static str,
    /// 1-based line number.
    pub line: u32,
    /// The source line, trimmed, internal whitespace collapsed to single
    /// spaces.
    pub text: String,
    /// `path|kind|text|n` — `n` is the 0-based occurrence index among
    /// findings in this file with the same kind and normalised text.
    pub fingerprint: String,
}

impl Finding {
    /// The `path:line  kind  text` line the CLI prints.
    pub fn display(&self) -> String {
        format!("{}:{}  {}  {}", self.path, self.line, self.kind, self.text)
    }
}

/// The result of comparing a scan against a baseline.
#[derive(Debug, Default)]
pub struct CheckOutcome {
    /// Findings whose fingerprint is not in the baseline (new debt).
    pub new: Vec<Finding>,
    /// Baseline fingerprints that no longer appear in the tree.
    pub stale: Vec<String>,
}

/// A parsed baseline file.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Baseline {
    pub version: u32,
    /// Sorted fingerprints.
    pub findings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

/// Scan every `*.rs` file under `src_dir` and return all findings, in
/// path-then-line order. The kind (e) design-token name set is built by
/// reading every `theme.rs` in the scanned tree.
///
/// Ratchet safety (02b item 8): this FAILS rather than silently under-
/// reporting when the directory is missing, a WalkDir error occurs, or a
/// file the scanner reads (scanned or `theme.rs`) cannot be read — the CLI
/// maps scan errors to exit 2, and no baseline is ever written after one.
pub fn scan(src_dir: &Path) -> Result<Vec<Finding>> {
    if !src_dir.is_dir() {
        bail!(
            "{}: no such directory (or not a directory)",
            src_dir.display()
        );
    }
    let mut files: Vec<PathBuf> = Vec::new();
    let mut theme_files: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(src_dir) {
        let entry = entry.with_context(|| format!("walking {}", src_dir.display()))?;
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("theme.rs") {
            theme_files.push(path.to_path_buf());
            continue;
        }
        let rel = path
            .strip_prefix(src_dir)
            .with_context(|| format!("{} not under {}", path.display(), src_dir.display()))?;
        if is_exempt_path(path, rel) {
            continue;
        }
        files.push(path.to_path_buf());
    }
    files.sort();
    let design_tokens = design_token_names(&theme_files)?;

    let mut out: Vec<Finding> = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(src_dir)
            .with_context(|| format!("{} not under {}", path.display(), src_dir.display()))?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let src =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        out.extend(scan_file(&rel_str, &src, &design_tokens));
    }
    Ok(out)
}

/// The bare constant names the kind (e) detector accepts: every `pub const`
/// in a `theme.rs` whose type is `f32`, `Pixels` or `Rgba`. A `theme.rs`
/// that cannot be read is a scan error (02b item 8): a silently empty token
/// set would let `token_math` findings slip past the ratchet.
pub fn design_token_names(theme_files: &[PathBuf]) -> Result<HashSet<String>> {
    let mut names: HashSet<String> = HashSet::new();
    for path in theme_files {
        let src =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        for line in src.lines() {
            for caps in re_design_const().captures_iter(line) {
                names.insert(caps[1].to_string());
            }
        }
    }
    Ok(names)
}

/// A file is exempt when its name is `theme.rs` or a declared content palette (`terminal_palette.rs`, `chart_palette.rs`, `profile_palette.rs`),
/// it ends `_tests.rs`, or it sits under a `tests/` directory.
fn is_exempt_path(abs: &Path, rel: &Path) -> bool {
    matches!(
        abs.file_name().and_then(|n| n.to_str()),
        Some("theme.rs")
            | Some("terminal_palette.rs")
            | Some("chart_palette.rs")
            | Some("profile_palette.rs")
    ) || abs
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with("_tests.rs"))
        || rel
            .parent()
            .map(|p| p.components().any(|c| c.as_os_str() == "tests"))
            .unwrap_or(false)
}

/// Scan one file's source into findings (fingerprint `n` already assigned).
fn scan_file(rel_path: &str, src: &str, design_tokens: &HashSet<String>) -> Vec<Finding> {
    let mut lines = classify(src);
    // Blank the exact `#[cfg(test)] mod … { }` ranges (item 7) so that no
    // code — and no string content — inside them is ever detected; code on
    // the same line after the closing brace stays intact.
    let skip_ranges = cfg_test_skip_ranges(&lines);
    blank_skip_ranges(&mut lines, &skip_ranges);

    // Collect (line idx, kind) hits first; the occurrence index n is per
    // (kind, normalised text) within the file.
    let mut hits: Vec<(usize, &'static str)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        // The "previous line" for `raw_color_struct`'s continuation check
        // is the last line above `i` that has any code: a line-start
        // `Rgba {` separated from the expression by comment-only lines
        // (`//`, `///`, `/* … */`) or blank lines still continues it
        // (02c item 1). Line 0 has no previous line at all.
        let prev_code = lines[..i]
            .iter()
            .rev()
            .find(|l| !l.code.trim().is_empty())
            .map(|l| l.code.as_str());
        for kind in KINDS {
            for _ in 0..detect_line(
                kind,
                &line.code,
                &line.original,
                &line.content_mask,
                prev_code,
                design_tokens,
            ) {
                hits.push((i, kind));
            }
        }
    }

    // `px(` / `z(` / `.opacity(` whose numeric argument is on a following
    // line, after whitespace or comments only (item 5): report it on the
    // line of the call.
    for (i, line) in lines.iter().enumerate() {
        if let Some(kind) = split_call_kind(line.code.trim_end()) {
            if numeric_argument_follows(&lines, i) {
                hits.push((i, kind));
            }
        }
    }

    let mut counts: HashMap<(&'static str, String), usize> = HashMap::new();
    hits.into_iter()
        .map(|(i, kind)| {
            let text = norm_line(&lines[i].original);
            let n = *counts.entry((kind, text.clone())).or_insert(0);
            *counts.get_mut(&(kind, text.clone())).unwrap() += 1;
            Finding {
                path: rel_path.to_string(),
                kind,
                line: (i + 1) as u32,
                text: text.clone(),
                fingerprint: format!("{rel_path}|{kind}|{text}|{n}"),
            }
        })
        .collect()
}

/// Trim and collapse internal whitespace runs to single spaces.
fn norm_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Lexical classification (comments / strings / chars)
// ---------------------------------------------------------------------------

struct Line {
    /// The source line with every non-code character (comments, string and
    /// char literal content and delimiters) replaced by a space. One entry
    /// per source character, so offsets index into `original`.
    code: String,
    /// One entry per character of the line: `true` where the character is
    /// *string literal content* (kind (f) hex literals look inside strings).
    content_mask: Vec<bool>,
    /// The original source line.
    original: String,
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Code,
    LineComment,
    BlockComment(u32),
    String,
    RawString(usize),
    Char,
}

fn is_ident(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Walk the source character by character, splitting it into per-line
/// code-only text plus string-content masks. Every source character of a
/// line yields exactly one entry in `code` / `content_mask`, so mask
/// offsets index straight into the line's original text. `original` is
/// sliced verbatim from the source — fingerprints must use the original
/// line, never a reconstruction. A char literal cannot span a newline, so
/// the `Char` state resets at every line end.
fn classify(src: &str) -> Vec<Line> {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    // Byte offset of each char index (plus the end of the file), so
    // `original` can be sliced straight out of the source for every line.
    let mut byte_at: Vec<usize> = Vec::with_capacity(n + 1);
    let mut off = 0usize;
    for c in &chars {
        byte_at.push(off);
        off += c.len_utf8();
    }
    byte_at.push(off);

    let mut lines: Vec<Line> = Vec::new();
    let mut code: String = String::new();
    let mut mask: Vec<bool> = Vec::new();
    let mut state = State::Code;
    let mut i = 0usize;
    let mut line_start = 0usize;

    while i < n {
        let c = chars[i];
        if c == '\n' {
            lines.push(Line {
                code: std::mem::take(&mut code),
                content_mask: std::mem::take(&mut mask),
                original: src[byte_at[line_start]..byte_at[i]].to_string(),
            });
            line_start = i + 1;
            // A line comment and a char literal cannot span a newline; a
            // string, raw string or block comment can.
            match state {
                State::Char | State::LineComment => state = State::Code,
                _ => {}
            }
            i += 1;
            continue;
        }

        let next = chars.get(i + 1).copied();
        match state {
            State::Code => {
                if c == '/' && next == Some('/') {
                    state = State::LineComment;
                    blank(&mut code, &mut mask);
                    i += 1;
                    continue;
                }
                if c == '/' && next == Some('*') {
                    // Consume BOTH `/` and `*` (as `*/` is consumed): leaving
                    // the opener's `*` in the stream would let it join a
                    // following `/` — the nested opener's slash — and close
                    // the comment one level too early.
                    state = State::BlockComment(1);
                    blank(&mut code, &mut mask);
                    blank(&mut code, &mut mask);
                    i += 2;
                    continue;
                }
                if c == '"' {
                    // Raw string? Count the `#` run immediately before the
                    // quote; it is raw iff an `r` precedes the run and the
                    // character before that `r` is not an identifier char.
                    let mut k = 0usize;
                    let mut j = i;
                    while j > 0 && chars[j - 1] == '#' {
                        k += 1;
                        j -= 1;
                    }
                    let raw = j > 0 && chars[j - 1] == 'r' && (j == 1 || !is_ident(chars[j - 2]));
                    state = if raw {
                        State::RawString(k)
                    } else {
                        State::String
                    };
                    blank(&mut code, &mut mask);
                    i += 1;
                    continue;
                }
                if c == '\'' {
                    // Distinguish a char literal from a lifetime:
                    //  - a char literal is `'` + one character or an escape
                    //    (`\x`, `\u{…}`) + a closing `'` — so it is always
                    //    closed two characters later, and an escape always
                    //    opens one;
                    //  - a lifetime is `'` + identifier chars without a
                    //    closing quote (`&'a T`, `impl<'a>`).
                    let char_lit = matches!(
                        (chars.get(i + 1), chars.get(i + 2)),
                        (Some('\''), _) | (Some('\\'), _) | (Some(_), Some('\''))
                    );
                    if char_lit {
                        state = State::Char;
                    }
                    code.push(c);
                    mask.push(false);
                    i += 1;
                    continue;
                }
                code.push(c);
                mask.push(false);
                i += 1;
            }
            State::LineComment => {
                blank(&mut code, &mut mask);
                i += 1;
            }
            State::BlockComment(depth) => {
                if c == '/' && next == Some('*') {
                    // Consume BOTH `/` and `*` of the nested opener, as the
                    // outer opener and the `*/` closer already do.
                    state = State::BlockComment(depth + 1);
                    blank(&mut code, &mut mask);
                    blank(&mut code, &mut mask);
                    i += 2;
                    continue;
                }
                if c == '*' && next == Some('/') {
                    // Consume BOTH `*` and `/`: the closing slash must not
                    // leak into the code as a stray `/`.
                    state = if depth <= 1 {
                        State::Code
                    } else {
                        State::BlockComment(depth - 1)
                    };
                    blank(&mut code, &mut mask);
                    blank(&mut code, &mut mask);
                    i += 2;
                    continue;
                }
                blank(&mut code, &mut mask);
                i += 1;
            }
            State::String => {
                if c == '\\' {
                    if next.is_some() && next != Some('\n') {
                        // The backslash AND the escaped char each get one
                        // entry, so `content_mask` stays 1:1 with `original`;
                        // the escaped char is not raw hex content.
                        blank(&mut code, &mut mask);
                        blank(&mut code, &mut mask);
                        i += 2;
                    } else {
                        blank(&mut code, &mut mask);
                        i += 1; // backslash-newline line continuation
                    }
                    continue;
                }
                if c == '"' {
                    state = State::Code;
                    blank(&mut code, &mut mask);
                    i += 1;
                    continue;
                }
                blank_content(&mut code, &mut mask);
                i += 1;
            }
            State::RawString(k) => {
                if c == '"' {
                    let mut j = i + 1;
                    for _ in 0..k {
                        if j < n && chars[j] == '#' {
                            j += 1;
                        }
                    }
                    if j == i + 1 + k {
                        for _ in 0..=k {
                            blank(&mut code, &mut mask);
                        }
                        state = State::Code;
                        i = j;
                        continue;
                    }
                }
                blank_content(&mut code, &mut mask);
                i += 1;
            }
            State::Char => {
                if c == '\\' {
                    // As in `String`: one entry per consumed char, so
                    // `content_mask` stays 1:1 with `original`.
                    if next.is_some() && next != Some('\n') {
                        blank(&mut code, &mut mask);
                        blank(&mut code, &mut mask);
                        i += 2;
                    } else {
                        blank(&mut code, &mut mask);
                        i += 1;
                    }
                    continue;
                }
                if c == '\'' {
                    state = State::Code;
                    blank(&mut code, &mut mask);
                    i += 1;
                    continue;
                }
                blank(&mut code, &mut mask);
                i += 1;
            }
        }
    }
    lines.push(Line {
        code: std::mem::take(&mut code),
        content_mask: std::mem::take(&mut mask),
        original: src[byte_at[line_start]..byte_at[n]].to_string(),
    });
    lines
}

/// Replace one character's worth of output with a space (no string content).
fn blank(code: &mut String, mask: &mut Vec<bool>) {
    code.push(' ');
    mask.push(false);
}

/// Blank one character of output but mark it as string-literal content.
fn blank_content(code: &mut String, mask: &mut Vec<bool>) {
    code.push(' ');
    mask.push(true);
}

// ---------------------------------------------------------------------------
// #[cfg(test)] module skip RANGES (char offsets, not lines)
// ---------------------------------------------------------------------------

static CFG_TEST_ATTR_RE: OnceLock<regex::Regex> = OnceLock::new();
static MOD_OPEN_RE: OnceLock<regex::Regex> = OnceLock::new();

fn cfg_test_attr_re() -> &'static regex::Regex {
    CFG_TEST_ATTR_RE.get_or_init(|| regex::Regex::new(r"#\[\s*cfg\s*\(\s*test\s*\)\s*\]").unwrap())
}

fn mod_open_re() -> &'static regex::Regex {
    MOD_OPEN_RE.get_or_init(|| {
        regex::Regex::new(r"^(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*\{")
            .unwrap()
    })
}

/// Character offset of each line's first source character (the source is the
/// lines' `original` text joined by newlines).
fn line_starts(lines: &[Line]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(lines.len());
    let mut acc = 0usize;
    for line in lines {
        starts.push(acc);
        acc += line.original.chars().count() + 1; // + the newline
    }
    starts
}

/// Walks the code-only characters of the classified lines (comments, string
/// and char-literal text already blanked), reporting the source char offset
/// of each.
struct CodeIter<'a> {
    lines: &'a [Line],
    line: usize,
    col: usize,
}

impl<'a> CodeIter<'a> {
    fn new(lines: &'a [Line], line: usize, col: usize) -> Self {
        CodeIter { lines, line, col }
    }
}

impl Iterator for CodeIter<'_> {
    type Item = (usize, usize, char); // (line idx, col, char)

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.line >= self.lines.len() {
                return None;
            }
            let len = self.lines[self.line].code.chars().count();
            if self.col >= len {
                self.line += 1;
                self.col = 0;
                continue;
            }
            let ch = self.lines[self.line].code.chars().nth(self.col).unwrap();
            let (line, col) = (self.line, self.col);
            self.col += 1;
            return Some((line, col, ch));
        }
    }
}

/// The char index of byte offset `byte` in `s`. The `regex` crate reports
/// byte offsets, while the line starts, the `CodeIter` columns and the
/// per-line masks below are char-based (one `code` entry per source
/// character): a multi-byte character before a match on the same line must
/// not shift a range (02c item 2).
fn byte_to_char(s: &str, byte: usize) -> usize {
    s[..byte].chars().count()
}

/// The exact skip ranges: for each `#[cfg(test)]` attribute, from the
/// attribute's `#` through the closing brace of the following `mod … { … }`
/// block (single-line form included), or just the attribute itself when it
/// is not followed by a `mod` block (e.g. on a `fn`, a `struct`, or a
/// `mod x;` declaration). Ranges are half-open `[start, end)` char offsets
/// into the source.
fn cfg_test_skip_ranges(lines: &[Line]) -> Vec<(usize, usize)> {
    let starts = line_starts(lines);
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        for m in cfg_test_attr_re().find_iter(&line.code) {
            // Byte offsets → char indices before meeting `starts`
            // (char-based) and the `CodeIter` column (char-based).
            let start_ch = byte_to_char(&line.code, m.start());
            let end_ch = byte_to_char(&line.code, m.end());
            let attr_start = starts[i] + start_ch;
            let attr_end = starts[i] + end_ch;
            let close = mod_block_close(lines, &starts, i, end_ch);
            ranges.push(match close {
                Some(close) => (attr_start, close + 1),
                None => (attr_start, attr_end),
            });
        }
    }
    ranges
}

/// From just after a `#[cfg(test)]` attribute (line `line_idx`, char `col`),
/// skip blanks (which include comments — they are blanked in the code) and
/// further `#[…]` attributes. If the code that remains is a `mod NAME {`
/// declaration, return the source char offset of its matching closing brace.
fn mod_block_close(lines: &[Line], starts: &[usize], line_idx: usize, col: usize) -> Option<usize> {
    // Find the first code character after blanks/attributes.
    let mut it = CodeIter::new(lines, line_idx, col);
    let mut first: Option<(usize, usize)> = None;
    while let Some((l, c, ch)) = it.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch == '#' {
            // A further attribute: skip to its closing `]`.
            for (_l, _c, c2) in it.by_ref() {
                if c2 == ']' {
                    break;
                }
            }
            continue;
        }
        first = Some((l, c));
        break;
    }
    let (l0, c0) = first?;

    // A slice of the code stream from that character (up to 160 chars),
    // line boundaries rendered as spaces, with the (line, col) of every
    // slice character.
    let (slice, pos): (String, Vec<(usize, usize)>) = {
        let (mut s, mut p) = (String::new(), Vec::new());
        let mut prev_line: Option<usize> = None;
        for (l, c, ch) in CodeIter::new(lines, l0, c0).take(160) {
            if prev_line.is_some_and(|p| p + 1 == l) {
                s.push(' ');
                p.push((l, 0));
            }
            s.push(ch);
            p.push((l, c));
            prev_line = Some(l);
        }
        (s, p)
    };
    let m = mod_open_re().find(&slice)?;
    // The match is `mod NAME {` — ASCII per the regex — so its byte end
    // equals a char position here; convert explicitly anyway, so byte
    // offsets never meet char-based positions by accident.
    let (bl, bc) = pos[byte_to_char(&slice, m.end()) - 1]; // last char is `{`

    // Brace-match on the code-only text so braces in comments or strings
    // cannot unbalance the count.
    let mut depth = 0i32;
    for (l, c, ch) in CodeIter::new(lines, bl, bc) {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(starts[l] + c);
                }
            }
            _ => {}
        }
    }
    None // unbalanced braces: do not skip past the attribute
}

/// Blank every character of `lines` that falls inside one of the skip
/// ranges — both the code and the string-content mask, so no finding kind
/// can fire inside a `#[cfg(test)] mod … { }`. Code outside the ranges
/// (including the same line after the closing brace) is untouched.
fn blank_skip_ranges(lines: &mut [Line], ranges: &[(usize, usize)]) {
    if ranges.is_empty() {
        return;
    }
    let starts = line_starts(lines);
    // The exact source char offsets to blank, as a set (ranges may overlap).
    let mut skip: HashSet<usize> = HashSet::new();
    for &(a, b) in ranges {
        skip.extend(a..b);
    }
    for (i, line) in lines.iter_mut().enumerate() {
        let s = starts[i];
        let mut chars: Vec<char> = line.code.chars().collect();
        for (k, ch) in chars.iter_mut().enumerate() {
            if skip.contains(&(s + k)) {
                *ch = ' ';
                line.content_mask[k] = false;
            }
        }
        line.code = chars.into_iter().collect();
    }
}

// ---------------------------------------------------------------------------
// Kind detectors
// ---------------------------------------------------------------------------

static RE_COLOR_FN: OnceLock<regex::Regex> = OnceLock::new();
static RE_COLOR_STRUCT: OnceLock<regex::Regex> = OnceLock::new();
static RE_PX: OnceLock<regex::Regex> = OnceLock::new();
static RE_Z: OnceLock<regex::Regex> = OnceLock::new();
static RE_TOKEN_MATH_L: OnceLock<regex::Regex> = OnceLock::new();
static RE_TOKEN_MATH_R: OnceLock<regex::Regex> = OnceLock::new();
static RE_HEX: OnceLock<regex::Regex> = OnceLock::new();
static RE_OPACITY: OnceLock<regex::Regex> = OnceLock::new();
static RE_DESIGN_CONST: OnceLock<regex::Regex> = OnceLock::new();

/// A `pub const NAME: f32 | Pixels | Rgba` declaration in a `theme.rs`.
fn re_design_const() -> &'static regex::Regex {
    RE_DESIGN_CONST.get_or_init(|| {
        regex::Regex::new(r"^\s*pub\s+const\s+([A-Z][A-Z0-9_]*)\s*:\s*(?:f32|Pixels|Rgba)\s*=")
            .unwrap()
    })
}

fn re_color_fn() -> &'static regex::Regex {
    RE_COLOR_FN.get_or_init(|| regex::Regex::new(r"(?:gpui::)?(?:rgb|rgba|hsla)\s*\(").unwrap())
}
fn re_color_struct() -> &'static regex::Regex {
    RE_COLOR_STRUCT.get_or_init(|| regex::Regex::new(r"(?:gpui::)?(?:Rgba|Hsla)\s*\{").unwrap())
}
fn re_px() -> &'static regex::Regex {
    RE_PX.get_or_init(|| regex::Regex::new(r"px\(\s*-?\d").unwrap())
}
fn re_z() -> &'static regex::Regex {
    RE_Z.get_or_init(|| regex::Regex::new(r"(?:theme::)?z\(\s*-?\d").unwrap())
}
fn re_token_math_l() -> &'static regex::Regex {
    RE_TOKEN_MATH_L
        .get_or_init(|| regex::Regex::new(r"(?:theme::)?[A-Z][A-Z0-9_]*\s*[-+*/]\s*\d").unwrap())
}
fn re_token_math_r() -> &'static regex::Regex {
    RE_TOKEN_MATH_R.get_or_init(|| {
        regex::Regex::new(r"\d+(?:\.\d+)?\s*[-+*/]\s*(?:theme::)?[A-Z][A-Z0-9_]*").unwrap()
    })
}
fn re_hex() -> &'static regex::Regex {
    RE_HEX.get_or_init(|| regex::Regex::new(r"^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$").unwrap())
}
fn re_opacity() -> &'static regex::Regex {
    RE_OPACITY.get_or_init(|| regex::Regex::new(r"\.opacity\(\s*-?\d").unwrap())
}

/// Run one kind's detector over one line and return its hit count.
/// `prev_code` is the code-only text of the last line above this one that
/// has any code (used by `raw_color_struct` to decide whether a line-start
/// `Rgba {` continues an expression), or `None` when there is none.
/// `design_tokens` is the kind (e) name set read from the scanned tree's
/// `theme.rs` files.
fn detect_line(
    kind: &'static str,
    code: &str,
    original: &str,
    content_mask: &[bool],
    prev_code: Option<&str>,
    design_tokens: &HashSet<String>,
) -> usize {
    match kind {
        // (The `regex` crate has no lookaround, so identifier boundaries are
        // checked by hand on the character preceding each match.)
        "raw_color_fn" => re_color_fn()
            .find_iter(code)
            .filter(|m| left_boundary_ok(code, m.start()))
            .filter(|m| call_has_argument(code, m.end()))
            .count(),
        "raw_color_struct" => re_color_struct()
            .find_iter(code)
            .filter(|m| struct_literal_position(code, m.start(), prev_code))
            .count(),
        "raw_px" => re_px()
            .find_iter(code)
            .filter(|m| left_boundary_ok(code, m.start()))
            .count(),
        "raw_z" => re_z()
            .find_iter(code)
            .filter(|m| left_boundary_ok(code, m.start()))
            .count(),
        "token_math" => {
            re_token_math_l()
                .find_iter(code)
                .filter(|m| left_boundary_ok(code, m.start()))
                .filter(|m| is_design_token_match(m.as_str(), true, design_tokens))
                .count()
                + re_token_math_r()
                    .find_iter(code)
                    .filter(|m| is_design_token_match(m.as_str(), false, design_tokens))
                    .count()
        }
        "hex_literal" => content_runs(content_mask)
            .into_iter()
            .map(|(a, b)| original.chars().skip(a).take(b - a).collect::<String>())
            .filter(|s| re_hex().is_match(s))
            .count(),
        "raw_opacity" => re_opacity().find_iter(code).count(),
        _ => 0,
    }
}

/// Is the `Rgba {` / `Hsla {` matched starting at `start` a struct LITERAL
/// rather than a type position?
///
/// A literal follows any of `(`, `[`, `=`, `,`, `{`, `;`, `:`, `|`, `=>`,
/// or the keywords `return` / `match` — or starts a line that continues an
/// expression from the last line with code above it (comment-only and
/// blank lines between them do not break the continuation). It is NOT a
/// literal in a type position: `-> Rgba {` (a return type before a body
/// brace), `impl … for Rgba {`, `struct`/`enum`/`trait`/`type` `Rgba {`,
/// or a type annotation (which never has a `{` directly after the name, so
/// the regex does not match it).
fn struct_literal_position(code: &str, start: usize, prev_code: Option<&str>) -> bool {
    match last_non_ws_before(code, start) {
        Some(c) => match c {
            '(' | '[' | '=' | ',' | '{' | ';' | ':' | '|' => true,
            // `=>` (a match arm) is a literal; `->` (a return type) is not.
            '>' => code[..start].trim_end().ends_with("=>"),
            _ if is_ident(c) => {
                // Only `return Rgba {` (and `match Rgba {`) among
                // identifiers introduce a literal; `for`, `struct`, … do not.
                matches!(
                    trailing_identifier(code, start).as_deref(),
                    Some("return") | Some("match")
                )
            }
            _ => false,
        },
        None => {
            // The line starts with the literal: it counts only when it
            // continues an expression from a real previous code line.
            let Some(prev) = prev_code else {
                return false;
            };
            let prev = prev.trim_end();
            if prev.is_empty() || prev.ends_with("->") {
                return false; // no expression, or a return type split over lines
            }
            !matches!(
                trailing_identifier_of(prev).as_deref(),
                Some("for" | "struct" | "enum" | "trait" | "type" | "impl")
            )
        }
    }
}

/// The identifier ending just before `start` in `code` (skipping trailing
/// whitespace), if that character is an identifier character.
fn trailing_identifier(code: &str, start: usize) -> Option<String> {
    let before = code[..start].trim_end();
    let id: String = before.chars().rev().take_while(|c| is_ident(*c)).collect();
    if id.is_empty() {
        return None;
    }
    Some(id.chars().rev().collect())
}

/// The identifier a line ends with, if any.
fn trailing_identifier_of(line: &str) -> Option<String> {
    let id: String = line.chars().rev().take_while(|c| is_ident(*c)).collect();
    if id.is_empty() {
        return None;
    }
    Some(id.chars().rev().collect())
}

/// Kind (e) gate: the constant in a token_math match is a design token —
/// either written `theme::NAME`, or a bare `NAME` present in `tokens`.
fn is_design_token_match(s: &str, constant_is_left: bool, tokens: &HashSet<String>) -> bool {
    if s.contains("theme::") {
        return true;
    }
    let name: String = if constant_is_left {
        // The constant is the leading identifier of the match.
        s.chars().take_while(|c| is_ident(*c)).collect()
    } else {
        // The constant is the trailing identifier of the match.
        let id: String = s.chars().rev().take_while(|c| is_ident(*c)).collect();
        id.chars().rev().collect()
    };
    tokens.contains(&name)
}

/// If `code` (already end-trimmed) ends with `px(`, `z(` or `.opacity(` —
/// with a clean identifier boundary — return the kind of the call whose
/// argument may sit on a following line (item 5).
fn split_call_kind(code: &str) -> Option<&'static str> {
    if code.ends_with(".opacity(") {
        return Some("raw_opacity");
    }
    if code.ends_with("px(") {
        let start = code.len() - "px(".len();
        return left_boundary_ok(code, start).then_some("raw_px");
    }
    if code.ends_with("z(") {
        let start = code.len() - "z(".len();
        return left_boundary_ok(code, start).then_some("raw_z");
    }
    None
}

/// True when the line after `i`, skipping whitespace- and comment-only
/// lines, starts with a numeric literal.
fn numeric_argument_follows(lines: &[Line], i: usize) -> bool {
    for line in lines.iter().skip(i + 1) {
        let t = line.code.trim_start();
        if t.is_empty() {
            continue;
        }
        return t.starts_with(|c: char| c.is_ascii_digit())
            || (t.starts_with('-') && t.chars().nth(1).is_some_and(|c| c.is_ascii_digit()));
    }
    false
}

/// True if the call opened at `paren_end` (just past the `(`) takes at least
/// one argument: the next non-whitespace character is not `)`.
fn call_has_argument(code: &str, paren_end: usize) -> bool {
    !matches!(
        code[paren_end..].chars().find(|c| !c.is_whitespace()),
        Some(')')
    )
}

/// Last non-whitespace char strictly before `pos` in `code`, if any.
fn last_non_ws_before(code: &str, pos: usize) -> Option<char> {
    code[..pos].chars().rev().find(|c| !c.is_whitespace())
}

/// True when the character preceding a match is not part of the same
/// identifier (so `myrgb(` does not match `rgb(`, and `gpui::rgb(` does).
fn left_boundary_ok(code: &str, start: usize) -> bool {
    match code[..start].chars().next_back() {
        None => true,
        Some(c) => !is_ident(c),
    }
}

/// Maximal `[start, end)` runs of `true` in a content mask (char offsets).
fn content_runs(mask: &[bool]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    for (i, &b) in mask.iter().enumerate() {
        if b && start.is_none() {
            start = Some(i);
        }
        if !b {
            if let Some(s) = start.take() {
                runs.push((s, i));
            }
        }
    }
    if let Some(s) = start {
        runs.push((s, mask.len()));
    }
    runs
}

// ---------------------------------------------------------------------------
// Baseline
// ---------------------------------------------------------------------------

/// Read a baseline file and check the format version.
pub fn read_baseline(path: &Path) -> Result<Baseline> {
    let text =
        fs::read_to_string(path).with_context(|| format!("reading baseline {}", path.display()))?;
    let b: Baseline =
        toml::from_str(&text).with_context(|| format!("parsing baseline {}", path.display()))?;
    if b.version != BASELINE_VERSION {
        bail!(
            "unsupported baseline version {} (expected {BASELINE_VERSION})",
            b.version
        );
    }
    Ok(b)
}

/// Serialize a baseline (its findings must already be sorted).
pub fn baseline_to_toml(b: &Baseline) -> String {
    const HEADER: &str = "\
# awili-design-lint baseline (docs/v2/PLAN.md slice 2).
#
# One fingerprint per line: <path relative to crates/awili-app/src>|<kind>|<line
# text, trimmed, whitespace collapsed>|<n>. The ratchet: the file can only
# SHRINK — `awili-design-lint check <src-dir> --baseline design-baseline.toml
# --update-baseline` rewrites it keeping only the findings that still exist
# and refuses to add new ones.
";
    let body = toml::to_string_pretty(b).expect("baseline serializes");
    format!("{HEADER}{body}")
}

/// Write the baseline (findings sorted and deduplicated).
pub fn write_baseline(path: &Path, findings: &[Finding]) -> Result<()> {
    let mut fps: Vec<String> = findings.iter().map(|f| f.fingerprint.clone()).collect();
    fps.sort();
    fps.dedup();
    let b = Baseline {
        version: BASELINE_VERSION,
        findings: fps,
    };
    fs::write(path, baseline_to_toml(&b))
        .with_context(|| format!("writing baseline {}", path.display()))?;
    Ok(())
}

/// Compare a scan against a baseline: new findings vs stale entries.
pub fn check(src_dir: &Path, baseline_path: &Path) -> Result<CheckOutcome> {
    let findings = scan(src_dir)?;
    let baseline = read_baseline(baseline_path)?;
    let current: HashSet<String> = findings.iter().map(|f| f.fingerprint.clone()).collect();
    let baseline_set: HashSet<&str> = baseline.findings.iter().map(String::as_str).collect();

    let new = findings
        .into_iter()
        .filter(|f| !baseline_set.contains(f.fingerprint.as_str()))
        .collect();
    let stale = baseline
        .findings
        .into_iter()
        .filter(|fp| !current.contains(fp))
        .collect();
    Ok(CheckOutcome { new, stale })
}

/// Rewrite the baseline keeping only the findings that still exist. Fails
/// (writing nothing) if any finding is not in the baseline. Returns the
/// number of entries kept.
pub fn update_baseline(src_dir: &Path, baseline_path: &Path) -> Result<usize> {
    let baseline = read_baseline(baseline_path)?;
    let findings = scan(src_dir)?;
    let baseline_set: HashSet<&str> = baseline.findings.iter().map(String::as_str).collect();
    let new = findings
        .iter()
        .filter(|f| !baseline_set.contains(f.fingerprint.as_str()))
        .count();
    if new > 0 {
        bail!(
            "refusing to update baseline: {new} finding(s) not in baseline (the baseline can only shrink)"
        );
    }
    write_baseline(baseline_path, &findings)?;
    Ok(findings.len())
}

/// The baseline fingerprints that are NOT present in `reference` — i.e. the
/// entries the working baseline ADDS on top of the committed reference
/// (02b item 9). The ratchet lets the baseline shrink (debt paid down) but
/// never grow: a local `--init`-style regeneration that records NEW debt
/// must be caught before it is committed.
pub fn reference_violations(baseline: &Baseline, reference: &Baseline) -> Vec<String> {
    let ref_set: HashSet<&str> = reference.findings.iter().map(String::as_str).collect();
    baseline
        .findings
        .iter()
        .filter(|fp| !ref_set.contains(fp.as_str()))
        .cloned()
        .collect()
}

/// One-shot initialization: write the baseline from the current tree. Fails
/// if the file already exists. Returns the number of entries written.
pub fn init_baseline(src_dir: &Path, baseline_path: &Path) -> Result<usize> {
    if baseline_path.exists() {
        bail!(
            "--init is one-shot: baseline already exists at {} (shrink it with --update-baseline)",
            baseline_path.display()
        );
    }
    let findings = scan(src_dir)?;
    write_baseline(baseline_path, &findings)?;
    Ok(findings.len())
}

// ---------------------------------------------------------------------------
// Reporting helpers
// ---------------------------------------------------------------------------

/// Per-kind totals over a scan, in [`KINDS`] order.
pub fn kind_totals(findings: &[Finding]) -> Vec<(&'static str, usize)> {
    KINDS
        .iter()
        .map(|k| (*k, findings.iter().filter(|f| f.kind == *k).count()))
        .collect()
}
