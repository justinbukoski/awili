//! Markdown → renderable blocks for message bubbles (slice 6).
//!
//! This module is the PURE half of markdown rendering: pulldown-cmark events in,
//! a small [`Block`]/[`Inline`] tree out. No GPUI types anywhere, so the whole
//! parser-to-runs mapping is unit-testable on any platform. The GPUI half
//! (`markdown_ui`) turns these blocks into styled text runs and elements.
//!
//! HTML is never rendered: raw HTML in a message is shown verbatim as plain
//! text. A message that defeats the parser entirely (a panic, or non-empty
//! input producing no blocks) falls back to one plain-text paragraph — the
//! user's words must never disappear.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::ops::Range;

// ---- budgets for untrusted input (review slice 6, blocker 1 + SF2) ---------
//
// Model output is untrusted: a single line of `> > > …` nests arbitrarily deep
// and the renderer recurses per level, so an unbounded tree can exhaust the UI
// thread's stack — and stack overflow aborts the process, it does not unwind
// into `parse_or_plain`'s catch. Every budget degrades, never drops words:
// excess nesting flattens into its parent, excess styling merges into the
// previous run, and an oversize source renders as truncated plain text.

/// Maximum accepted List/Blockquote nesting. Deeper containers flatten into
/// their parent as plain blocks. 32 levels is far beyond legible markdown and
/// keeps render recursion trivially within any thread stack.
pub const MAX_DEPTH: usize = 32;

/// Maximum total blocks in one message's tree; further blocks are dropped.
/// Only absurd input reaches this (the source cap admits ~3 bytes/block).
pub const MAX_BLOCKS: usize = 4096;

/// Maximum inline spans per paragraph; further style changes merge into the
/// previous span (text kept, styling degraded).
pub const MAX_SPANS: usize = 1024;

/// Maximum source size given to the markdown parser. A larger message renders
/// as a truncated plain paragraph — the per-message Copy still carries the
/// full source.
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;

// ---- streaming reparse cadence (review slice 6, SF2) ------------------------

/// Below this, a streaming reply reparses on every chunk (smooth live render
/// where replies actually live; worst-case cumulative work is ~LIVE²/chunk).
pub const STREAM_LIVE_BYTES: usize = 16 * 1024;

/// Past the live window, reparse only per this much new source. The display
/// advances in steps, but cumulative parse work stays linear-ish instead of
/// O(n²) across an n-byte reply.
pub const STREAM_STEP_BYTES: usize = 2 * 1024;

/// Whether a streaming row whose plan reflects `parsed_len` source bytes
/// should reparse now that the source has grown to `len`. Once the display is
/// the oversize fallback it never reparses mid-stream (the truncated prefix no
/// longer changes); the caller still does one exact refresh at stream end.
pub fn stream_reparse_due(parsed_len: usize, len: usize) -> bool {
    if len <= parsed_len || parsed_len >= MAX_SOURCE_BYTES {
        return false;
    }
    len <= STREAM_LIVE_BYTES || len - parsed_len >= STREAM_STEP_BYTES
}

/// One run of inline text with uniform styling.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inline {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// Inline `code` span (monospace, subtle background).
    pub code: bool,
    /// Link destination; rendered underlined and clickable.
    pub link: Option<String>,
}

impl Inline {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    fn same_style(&self, other: &Inline) -> bool {
        self.bold == other.bold
            && self.italic == other.italic
            && self.code == other.code
            && self.link == other.link
    }
}

/// One block-level element of a rendered message.
#[allow(clippy::enum_variant_names)] // `CodeBlock` names the kind, not the enum
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Image {
        mime: String,
        bytes: Vec<u8>,
    },
    Paragraph(Vec<Inline>),
    Heading {
        level: u8,
        spans: Vec<Inline>,
    },
    /// Fenced or indented code block. `lang` is the fence's first info word.
    CodeBlock {
        lang: Option<String>,
        code: String,
    },
    /// `ordered` is the start number for a numbered list, `None` for bullets.
    /// Each item is itself a sequence of blocks (nested lists, paragraphs).
    List {
        ordered: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Blockquote(Vec<Block>),
    Rule,
}

/// A styled byte range over the text [`flatten`] produces — the exact shape a
/// GPUI highlight run needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanRange {
    pub range: Range<usize>,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<String>,
}

impl SpanRange {
    /// Whether this range needs any styling beyond the bubble default.
    pub fn is_styled(&self) -> bool {
        self.bold || self.italic || self.code || self.link.is_some()
    }
}

/// Concatenate a paragraph's spans into one string plus the styled byte ranges
/// over it (UTF-8 byte offsets, in order, non-overlapping).
pub fn flatten(spans: &[Inline]) -> (String, Vec<SpanRange>) {
    let mut text = String::new();
    let mut ranges = Vec::with_capacity(spans.len());
    for span in spans {
        let start = text.len();
        text.push_str(&span.text);
        ranges.push(SpanRange {
            range: start..text.len(),
            bold: span.bold,
            italic: span.italic,
            code: span.code,
            link: span.link.clone(),
        });
    }
    (text, ranges)
}

/// Parse message markdown, falling back to a single plain-text paragraph if the
/// parser panics or non-empty input yields no blocks (e.g. a message that is
/// only a link-reference definition). The fallback preserves the raw source.
/// A source past [`MAX_SOURCE_BYTES`] is not parsed at all: it renders as a
/// truncated plain paragraph plus a notice (the Copy button keeps the full
/// text), bounding parse and render cost for any restored or streamed message.
pub fn parse_or_plain(source: &str) -> Vec<Block> {
    let content = awili_image_content::split(source);
    let source = content.text.as_str();
    if source.len() > MAX_SOURCE_BYTES {
        return oversize_fallback(source);
    }
    let mut parsed = std::panic::catch_unwind(|| parse(source)).unwrap_or_default();
    for (mime, bytes) in content.images {
        parsed.push(Block::Image { mime, bytes });
    }
    if parsed.is_empty() && !source.trim().is_empty() {
        return vec![Block::Paragraph(vec![Inline::plain(source)])];
    }
    parsed
}

/// RC2-APP (F0169): the last block of a message with more blocks than are
/// shown.
pub const BLOCKS_CUT_NOTICE: &str =
    "… the rest of this message is too long to show — Copy carries the full text";

/// The display for a message too large to format: its prefix as plain text
/// (cut on a char boundary) and a truncation notice.
fn oversize_fallback(source: &str) -> Vec<Block> {
    let mut cut = MAX_SOURCE_BYTES;
    while !source.is_char_boundary(cut) {
        cut -= 1;
    }
    vec![
        Block::Paragraph(vec![Inline::plain(&source[..cut])]),
        Block::Paragraph(vec![Inline::plain(
            "… message too large to format — Copy carries the full text",
        )]),
    ]
}

/// Parse CommonMark into blocks. Tables/footnotes/strikethrough extensions are
/// off — plain CommonMark only (the slice-6 element set).
pub fn parse(source: &str) -> Vec<Block> {
    Builder::default().run(Parser::new_ext(source, Options::empty()))
}

/// One open list: its start number, the items finished so far, and whether it
/// opened beyond [`MAX_DEPTH`] (suppressed — its contents flatten into the
/// enclosing container as plain blocks instead of nesting).
struct ListEntry {
    ordered: Option<u64>,
    items: Vec<Vec<Block>>,
    suppressed: bool,
}

/// Event-stream state machine. Containers (the document, each open blockquote
/// and list item) form a stack of block vectors; inline spans accumulate into
/// `spans` until the enclosing block closes.
///
/// Nesting is depth-budgeted (review slice 6, blocker 1): a List or Blockquote
/// opening at [`MAX_DEPTH`] is *suppressed* — no container is pushed, so its
/// text lands in the nearest real container as plain paragraphs. Everything
/// inside a suppressed container is deeper still, hence also suppressed, which
/// keeps starts/ends paired without a separate bookkeeping stack.
#[derive(Default)]
struct Builder {
    /// Stack of open block containers; index 0 is the document.
    containers: Vec<Vec<Block>>,
    /// Inline spans of the currently open paragraph/heading/loose item text.
    spans: Vec<Inline>,
    /// Nesting depth of `**strong**` / `*emphasis*`.
    bold: u32,
    italic: u32,
    /// Innermost link destination (links don't nest in practice, but a stack
    /// keeps the builder total).
    links: Vec<String>,
    /// The currently open code block's (lang, accumulated text).
    code_block: Option<(Option<String>, String)>,
    /// Stack of open lists (suppressed ones included, so ends pair up).
    lists: Vec<ListEntry>,
    /// Suppression flag per open blockquote (true = beyond the depth budget).
    quotes: Vec<bool>,
    /// Open List + Blockquote count, suppressed levels included — the render
    /// recursion depth this tree would otherwise produce.
    depth: usize,
    /// Total blocks pushed anywhere in the tree, for the [`MAX_BLOCKS`] cap.
    total_blocks: usize,
    /// RC2-APP (F0169): some blocks were past the cap and are not shown.
    dropped_blocks: bool,
    /// Level of the currently open heading.
    heading: Option<u8>,
}

impl Builder {
    fn run<'a>(mut self, events: impl Iterator<Item = Event<'a>>) -> Vec<Block> {
        self.containers.push(Vec::new());
        for event in events {
            self.event(event);
        }
        // A truncated stream (mid-stream render of a streaming reply) can end
        // with blocks still open — flush them so nothing typed disappears.
        self.flush_spans();
        if let Some((lang, code)) = self.code_block.take() {
            self.push_block(Block::CodeBlock { lang, code });
        }
        while self.containers.len() > 1 {
            let blocks = self.containers.pop().unwrap();
            match self.lists.pop() {
                Some(ListEntry {
                    ordered,
                    mut items,
                    suppressed: false,
                }) => {
                    items.push(blocks);
                    self.push_block(Block::List { ordered, items });
                }
                // A suppressed list never owned a container — its contents are
                // already flat in `blocks`; fold them into the parent.
                _ => self.containers.last_mut().unwrap().extend(blocks),
            }
        }
        let mut document = self.containers.pop().unwrap_or_default();
        // RC2-APP (F0169): blocks past the budget are not shown; say so (the
        // notice takes the one block the budget keeps for it).
        if self.dropped_blocks {
            document.push(Block::Paragraph(vec![Inline::plain(BLOCKS_CUT_NOTICE)]));
        }
        document
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => match &mut self.code_block {
                Some((_, code)) => code.push_str(&t),
                None => self.push_span(Inline::plain(t.as_ref())),
            },
            Event::Code(t) => self.push_span(Inline {
                code: true,
                ..Inline::plain(t.as_ref())
            }),
            // Raw HTML is NOT rendered — show it verbatim as plain text.
            Event::Html(t) | Event::InlineHtml(t) => self.push_span(Inline::plain(t.as_ref())),
            Event::SoftBreak | Event::HardBreak => self.push_span(Inline::plain("\n")),
            Event::Rule => self.push_block(Block::Rule),
            // Extensions are disabled; anything else carries no message text.
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.flush_spans(),
            Tag::Heading { level, .. } => {
                self.flush_spans();
                self.heading = Some(heading_level(level));
            }
            Tag::Strong => self.bold += 1,
            Tag::Emphasis => self.italic += 1,
            // An image renders as a link to its source, labelled by its alt text.
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.links.push(dest_url.to_string());
            }
            Tag::CodeBlock(kind) => {
                self.flush_spans();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().map(str::to_string)
                    }
                    CodeBlockKind::Indented => None,
                };
                self.code_block = Some((lang, String::new()));
            }
            Tag::List(start) => {
                self.flush_spans();
                let suppressed = self.depth >= MAX_DEPTH;
                self.depth += 1;
                self.lists.push(ListEntry {
                    ordered: start,
                    items: Vec::new(),
                    suppressed,
                });
            }
            Tag::Item => {
                self.flush_spans();
                // An item of a suppressed list gets no container of its own —
                // its content flattens into the enclosing container.
                if !self.lists.last().is_some_and(|l| l.suppressed) {
                    self.containers.push(Vec::new());
                }
            }
            Tag::BlockQuote(_) => {
                self.flush_spans();
                let suppressed = self.depth >= MAX_DEPTH;
                self.depth += 1;
                self.quotes.push(suppressed);
                if !suppressed {
                    self.containers.push(Vec::new());
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush_spans(),
            TagEnd::Heading(_) => {
                let level = self.heading.take().unwrap_or(1);
                let spans = std::mem::take(&mut self.spans);
                if !spans.is_empty() {
                    self.push_block(Block::Heading { level, spans });
                }
            }
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Link | TagEnd::Image => {
                self.links.pop();
            }
            TagEnd::CodeBlock => {
                if let Some((lang, code)) = self.code_block.take() {
                    self.push_block(Block::CodeBlock { lang, code });
                }
            }
            TagEnd::Item => {
                self.flush_spans();
                // Items of a suppressed list pushed no container (their
                // content already flowed into the parent) — nothing to close.
                if !self.lists.last().is_some_and(|l| l.suppressed) {
                    let item = self.containers.pop().unwrap_or_default();
                    if let Some(l) = self.lists.last_mut() {
                        l.items.push(item);
                    }
                }
            }
            TagEnd::List(_) => {
                if let Some(l) = self.lists.pop() {
                    self.depth = self.depth.saturating_sub(1);
                    if !l.suppressed {
                        self.push_block(Block::List {
                            ordered: l.ordered,
                            items: l.items,
                        });
                    }
                }
            }
            TagEnd::BlockQuote(_) => {
                self.flush_spans();
                if let Some(suppressed) = self.quotes.pop() {
                    self.depth = self.depth.saturating_sub(1);
                    if !suppressed {
                        let inner = self.containers.pop().unwrap_or_default();
                        self.push_block(Block::Blockquote(inner));
                    }
                }
            }
            _ => {}
        }
    }

    /// Append an inline span (merging with the previous span when the style is
    /// identical), tagged with the current bold/italic/link state.
    fn push_span(&mut self, mut span: Inline) {
        span.bold |= self.bold > 0;
        span.italic |= self.italic > 0;
        if span.link.is_none() {
            span.link = self.links.last().cloned();
        }
        // Span budget exhausted (hostile style-flip input): keep the text,
        // degrade the style change into the previous run.
        let at_cap = self.spans.len() >= MAX_SPANS;
        if let Some(last) = self.spans.last_mut() {
            if last.same_style(&span) || at_cap {
                last.text.push_str(&span.text);
                return;
            }
        }
        self.spans.push(span);
    }

    /// Close any dangling inline text as a paragraph (tight list items and
    /// truncated streams produce text without explicit paragraph events).
    fn flush_spans(&mut self) {
        if !self.spans.is_empty() {
            let spans = std::mem::take(&mut self.spans);
            self.push_block(Block::Paragraph(spans));
        }
    }

    fn push_block(&mut self, block: Block) {
        // Block budget exhausted: drop further blocks rather than let a hostile
        // message allocate without bound. Only reachable by absurd input — the
        // source cap admits at most ~MAX_SOURCE_BYTES/3 paragraphs.
        // RC2-APP (F0169): one block of the budget is kept for the notice
        // that says the rest is not shown.
        if self.total_blocks >= MAX_BLOCKS - 1 {
            self.dropped_blocks = true;
            return;
        }
        self.total_blocks += 1;
        self.containers
            .last_mut()
            .expect("document container always open")
            .push(block);
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(blocks: &[Block]) -> &[Inline] {
        match &blocks[0] {
            Block::Paragraph(s) => s,
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn plain_paragraph() {
        let blocks = parse("hello world");
        assert_eq!(
            blocks,
            vec![Block::Paragraph(vec![Inline::plain("hello world")])]
        );
    }

    #[test]
    fn bold_run() {
        let blocks = parse("a **bold** b");
        let s = spans(&blocks);
        assert_eq!(s.len(), 3);
        assert_eq!((s[1].text.as_str(), s[1].bold), ("bold", true));
        assert!(!s[0].bold && !s[2].bold);
    }

    #[test]
    fn italic_run() {
        let blocks = parse("an *italic* word");
        let s = spans(&blocks);
        assert_eq!((s[1].text.as_str(), s[1].italic), ("italic", true));
    }

    #[test]
    fn bold_italic_nested() {
        let blocks = parse("**bold *both***");
        let s = spans(&blocks);
        assert_eq!(s.len(), 2);
        assert!(s[0].bold && !s[0].italic);
        assert!(s[1].bold && s[1].italic);
        assert_eq!(s[1].text, "both");
    }

    #[test]
    fn inline_code() {
        let blocks = parse("run `cargo test` now");
        let s = spans(&blocks);
        assert_eq!((s[1].text.as_str(), s[1].code), ("cargo test", true));
        assert!(!s[0].code && !s[2].code);
    }

    #[test]
    fn link_span() {
        let blocks = parse("see [the docs](https://example.com/x) here");
        let s = spans(&blocks);
        assert_eq!(s[1].text, "the docs");
        assert_eq!(s[1].link.as_deref(), Some("https://example.com/x"));
        assert!(s[0].link.is_none() && s[2].link.is_none());
    }

    #[test]
    fn heading_levels() {
        let blocks = parse("# One\n\n## Two\n\n### Three");
        let levels: Vec<u8> = blocks
            .iter()
            .map(|b| match b {
                Block::Heading { level, .. } => *level,
                other => panic!("expected heading, got {other:?}"),
            })
            .collect();
        assert_eq!(levels, vec![1, 2, 3]);
        match &blocks[0] {
            Block::Heading { spans, .. } => assert_eq!(spans[0].text, "One"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn fenced_code_block_with_language() {
        let blocks = parse("```rust\nfn main() {}\n```");
        assert_eq!(
            blocks,
            vec![Block::CodeBlock {
                lang: Some("rust".into()),
                code: "fn main() {}\n".into()
            }]
        );
    }

    #[test]
    fn fenced_code_block_no_language() {
        let blocks = parse("```\nplain\n```");
        assert_eq!(
            blocks,
            vec![Block::CodeBlock {
                lang: None,
                code: "plain\n".into()
            }]
        );
    }

    #[test]
    fn bullet_list() {
        let blocks = parse("- one\n- two");
        match &blocks[0] {
            Block::List {
                ordered: None,
                items,
            } => {
                assert_eq!(items.len(), 2);
                assert_eq!(spans(&items[0])[0].text, "one");
                assert_eq!(spans(&items[1])[0].text, "two");
            }
            other => panic!("expected bullet list, got {other:?}"),
        }
    }

    #[test]
    fn numbered_list_keeps_start() {
        let blocks = parse("3. three\n4. four");
        match &blocks[0] {
            Block::List {
                ordered: Some(3),
                items,
            } => assert_eq!(items.len(), 2),
            other => panic!("expected ordered list from 3, got {other:?}"),
        }
    }

    #[test]
    fn nested_list() {
        let blocks = parse("- outer\n  - inner");
        match &blocks[0] {
            Block::List { items, .. } => match &items[0][1] {
                Block::List { items: inner, .. } => {
                    assert_eq!(spans(&inner[0])[0].text, "inner");
                }
                other => panic!("expected nested list, got {other:?}"),
            },
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn blockquote() {
        let blocks = parse("> quoted text");
        match &blocks[0] {
            Block::Blockquote(inner) => assert_eq!(spans(inner)[0].text, "quoted text"),
            other => panic!("expected blockquote, got {other:?}"),
        }
    }

    #[test]
    fn soft_break_preserved_as_newline() {
        let blocks = parse("line one\nline two");
        let s = spans(&blocks);
        let text: String = s.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(text, "line one\nline two");
    }

    #[test]
    fn raw_html_shown_verbatim() {
        let blocks = parse("a <b>not bold</b> tag");
        let s = spans(&blocks);
        let text: String = s.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(text, "a <b>not bold</b> tag");
        assert!(s.iter().all(|i| !i.bold));
    }

    // ---- flatten: the parser-to-runs byte-range mapping ----

    #[test]
    fn flatten_ranges_are_contiguous_bytes() {
        let blocks = parse("a **b** `c` [d](http://e)");
        let (text, ranges) = flatten(spans(&blocks));
        assert_eq!(text, "a b c d");
        let mut at = 0;
        for r in &ranges {
            assert_eq!(r.range.start, at);
            at = r.range.end;
        }
        assert_eq!(at, text.len());
        let styled: Vec<_> = ranges.iter().filter(|r| r.is_styled()).collect();
        assert!(styled[0].bold);
        assert!(styled[1].code);
        assert_eq!(styled[2].link.as_deref(), Some("http://e"));
    }

    #[test]
    fn flatten_multibyte_offsets() {
        let blocks = parse("héllo **wörld**");
        let (text, ranges) = flatten(spans(&blocks));
        assert_eq!(text, "héllo wörld");
        let bold = ranges.iter().find(|r| r.bold).unwrap();
        assert_eq!(&text[bold.range.clone()], "wörld");
        assert!(text.is_char_boundary(bold.range.start));
        assert!(text.is_char_boundary(bold.range.end));
    }

    // ---- malformed input: degrade, never disappear ----

    #[test]
    fn unclosed_bold_stays_literal() {
        let blocks = parse("**unclosed bold");
        let (text, _) = flatten(spans(&blocks));
        assert_eq!(text, "**unclosed bold");
    }

    #[test]
    fn unclosed_fence_still_renders_code() {
        // A streaming reply renders mid-fence; the open block must flush.
        let blocks = parse("```rust\nlet x = 1;");
        assert_eq!(
            blocks,
            vec![Block::CodeBlock {
                lang: Some("rust".into()),
                code: "let x = 1;".into()
            }]
        );
    }

    #[test]
    fn broken_link_stays_literal() {
        let blocks = parse("[dangling](no-close");
        let (text, ranges) = flatten(spans(&blocks));
        assert_eq!(text, "[dangling](no-close");
        assert!(ranges.iter().all(|r| r.link.is_none()));
    }

    #[test]
    fn parse_or_plain_falls_back_when_no_blocks() {
        // A lone link-reference definition produces zero events — the raw
        // source must come back as a plain paragraph, not an empty bubble.
        let source = "[ref]: https://example.com";
        let blocks = parse_or_plain(source);
        assert_eq!(blocks, vec![Block::Paragraph(vec![Inline::plain(source)])]);
    }

    #[test]
    fn parse_or_plain_empty_input_is_empty() {
        assert!(parse_or_plain("").is_empty());
        assert!(parse_or_plain("   \n  ").is_empty());
    }

    #[test]
    fn parse_or_plain_normal_input_parses() {
        let blocks = parse_or_plain("**hi**");
        assert!(matches!(&blocks[0], Block::Paragraph(s) if s[0].bold));
    }

    // ---- hostile input: every budget degrades, nothing crashes (blocker 1) ----

    /// Maximum container nesting (the render recursion depth a tree produces).
    fn tree_depth(blocks: &[Block]) -> usize {
        blocks
            .iter()
            .map(|b| match b {
                Block::Blockquote(inner) => 1 + tree_depth(inner),
                Block::List { items, .. } => {
                    1 + items.iter().map(|i| tree_depth(i)).max().unwrap_or(0)
                }
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    fn count_blocks(blocks: &[Block]) -> usize {
        blocks
            .iter()
            .map(|b| {
                1 + match b {
                    Block::Blockquote(inner) => count_blocks(inner),
                    Block::List { items, .. } => items.iter().map(|i| count_blocks(i)).sum(),
                    _ => 0,
                }
            })
            .sum()
    }

    /// Every piece of inline + code text in tree order.
    fn all_text(blocks: &[Block]) -> String {
        let mut out = String::new();
        for b in blocks {
            match b {
                Block::Paragraph(s) | Block::Heading { spans: s, .. } => {
                    out.extend(s.iter().map(|i| i.text.as_str()))
                }
                Block::CodeBlock { code, .. } => out.push_str(code),
                Block::List { items, .. } => out.extend(items.iter().map(|i| all_text(i))),
                Block::Blockquote(inner) => out.push_str(&all_text(inner)),
                Block::Rule | Block::Image { .. } => {}
            }
        }
        out
    }

    #[test]
    fn hostile_blockquote_depth_is_bounded() {
        // One line of repeated "> " nests a blockquote per repetition; the
        // renderer recurses per level, so an unbounded tree aborts the process.
        let source = format!("{}deep", "> ".repeat(50_000));
        let blocks = parse_or_plain(&source);
        assert!(
            tree_depth(&blocks) <= MAX_DEPTH,
            "depth {} > budget",
            tree_depth(&blocks)
        );
        // The words survive the flattening.
        assert!(all_text(&blocks).contains("deep"));
    }

    #[test]
    fn hostile_list_depth_is_bounded() {
        let mut source = String::new();
        for i in 0..200 {
            source.push_str(&"  ".repeat(i));
            source.push_str("- x\n");
        }
        let blocks = parse_or_plain(&source);
        assert!(
            tree_depth(&blocks) <= MAX_DEPTH,
            "depth {} > budget",
            tree_depth(&blocks)
        );
        assert!(all_text(&blocks).contains('x'));
    }

    #[test]
    fn hostile_mixed_quote_list_depth_is_bounded() {
        // Alternating containers so neither stack alone carries the depth.
        let mut source = String::new();
        for _ in 0..100 {
            source.push_str("> - ");
        }
        source.push_str("end");
        let blocks = parse_or_plain(&source);
        assert!(
            tree_depth(&blocks) <= MAX_DEPTH,
            "depth {} > budget",
            tree_depth(&blocks)
        );
    }

    #[test]
    fn hostile_block_count_is_bounded() {
        let source = "x\n\n".repeat(3 * MAX_BLOCKS);
        let blocks = parse(&source);
        assert!(count_blocks(&blocks) <= MAX_BLOCKS);
    }

    /// RC2-APP (F0169): when blocks are cut, the message says so in its last
    /// block (inside the budget); a message within the budget has no notice.
    #[test]
    fn rc2app_cut_blocks_are_announced() {
        let source = "x\n\n".repeat(3 * MAX_BLOCKS);
        let blocks = parse(&source);
        assert!(count_blocks(&blocks) <= MAX_BLOCKS);
        assert_eq!(all_text(&blocks[blocks.len() - 1..]), BLOCKS_CUT_NOTICE);
        let small = parse("one\n\ntwo");
        assert_eq!(small.len(), 2);
        assert!(!all_text(&small).contains(BLOCKS_CUT_NOTICE));
    }

    #[test]
    fn hostile_span_count_is_bounded_text_kept() {
        // Alternating code/plain spans defeat same-style merging.
        let source = "`a`b".repeat(3 * MAX_SPANS);
        let blocks = parse(&source);
        match &blocks[0] {
            Block::Paragraph(s) => assert!(s.len() <= MAX_SPANS, "{} spans", s.len()),
            other => panic!("expected paragraph, got {other:?}"),
        }
        // Styling degraded past the budget, but every character survives.
        assert_eq!(all_text(&blocks).len(), 2 * 3 * MAX_SPANS);
    }

    #[test]
    fn oversize_source_renders_truncated_plain_text() {
        // Past the source cap, markdown is skipped entirely: a heading marker
        // must come back as plain text, truncated, plus the notice paragraph.
        let source = format!("# {}", "a".repeat(MAX_SOURCE_BYTES + 10));
        let blocks = parse_or_plain(&source);
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            Block::Paragraph(s) => {
                assert!(s[0].text.len() <= MAX_SOURCE_BYTES);
                assert!(s[0].text.starts_with("# "));
            }
            other => panic!("expected plain paragraph, got {other:?}"),
        }
        assert!(all_text(&blocks[1..]).contains("too large"));
    }

    // ---- streaming cadence: large replies must not reparse per chunk (SF2) ----

    #[test]
    fn streaming_cadence_is_live_for_short_replies() {
        // Within the live window every chunk reparses — no visible lag where
        // almost all real replies live.
        assert!(stream_reparse_due(0, 10));
        assert!(stream_reparse_due(10, 11));
        assert!(stream_reparse_due(STREAM_LIVE_BYTES - 1, STREAM_LIVE_BYTES));
        // No new bytes -> no work.
        assert!(!stream_reparse_due(10, 10));
        assert!(!stream_reparse_due(10, 5));
    }

    #[test]
    fn streaming_cadence_steps_past_live_then_freezes_at_cap() {
        let past = STREAM_LIVE_BYTES + 100;
        assert!(
            !stream_reparse_due(past, past + 1),
            "per-chunk reparse past the live window"
        );
        assert!(stream_reparse_due(past, past + STREAM_STEP_BYTES));
        // The oversize fallback never changes mid-stream — frozen until done.
        assert!(!stream_reparse_due(
            MAX_SOURCE_BYTES,
            MAX_SOURCE_BYTES + STREAM_STEP_BYTES * 10
        ));
    }

    #[test]
    fn large_streamed_reply_has_bounded_cumulative_parse_work() {
        // Simulate a 1 MiB reply in 16-byte token chunks and account every
        // reparse at its full accumulated length — the O(n²) the review found.
        let (total, chunk) = (1024 * 1024, 16);
        let (mut len, mut parsed_len) = (0usize, 0usize);
        let (mut parses, mut cumulative) = (0usize, 0usize);
        while len < total {
            len += chunk;
            if stream_reparse_due(parsed_len, len) {
                parses += 1;
                cumulative += len;
                parsed_len = len;
            }
        }
        // Per-chunk reparse would be 65 536 parses / ~34 GiB cumulative.
        assert!(parses < 1300, "{parses} parses");
        assert!(
            cumulative < 32 * 1024 * 1024,
            "{cumulative} bytes cumulative"
        );
        // The display reached the oversize fallback, then froze.
        assert!(parsed_len >= MAX_SOURCE_BYTES);
        assert!(parsed_len < MAX_SOURCE_BYTES + STREAM_STEP_BYTES + chunk);
    }

    #[test]
    fn oversize_truncation_lands_on_char_boundary() {
        // Offset by one ASCII byte so the cap falls mid-é; the cut must walk
        // back to a boundary instead of panicking on the slice.
        let source = format!("a{}", "é".repeat(MAX_SOURCE_BYTES));
        let blocks = parse_or_plain(&source);
        match &blocks[0] {
            Block::Paragraph(s) => assert!(s[0].text.len() <= MAX_SOURCE_BYTES),
            other => panic!("expected plain paragraph, got {other:?}"),
        }
    }
}
