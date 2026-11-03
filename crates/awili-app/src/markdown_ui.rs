//! GPUI half of markdown rendering (slice 6): render-ready
//! [`crate::markdown_plan`] blocks in, styled elements out. Text styling is
//! plain gpui text runs (`StyledText` + highlights) — no webview, no HTML.
//! Every styling/link/copy decision was resolved in the pure plan layer, so
//! this file only assembles elements: per-frame cost is element construction
//! plus refcounted [`SharedString`] clones and bounded range-vector clones.
//! Every color comes from a [`theme`] token via the per-role
//! [`BubblePalette`].

use gpui::{
    combine_highlights, div, prelude::*, AnyElement, App, ClipboardItem, ElementId, FontWeight,
    HighlightStyle, InteractiveText, Rgba, SharedString, StyledText,
};

pub use crate::markdown_plan::{assistant, user};
use crate::markdown_plan::{link_allowed, BubblePalette, PlanBlock, TextPlan};
#[cfg(test)]
pub use crate::markdown_plan::{ASSISTANT, USER};
use crate::search;
use crate::selectable_message::TextCollector;
use crate::theme;

/// Unique element ids for the interactive pieces (links, copy buttons) of one
/// message's markdown tree.
struct Ids {
    msg: usize,
    /// Content version — the caller passes something that CHANGES when the
    /// plan is rebuilt (the row's parsed_len), so an element id never
    /// outlives the content it was minted for. InteractiveText keys its
    /// INTERACTIVITY state (hover, click ranges) by this id; a stable id
    /// across a reparse would carry that state onto different text. (Text
    /// LAYOUT state is created per render, so this is hygiene, not a fix for
    /// the 2026-08-24 measurement bug.)
    version: usize,
    next: usize,
}

impl Ids {
    fn next(&mut self) -> ElementId {
        self.next += 1;
        format!("md-{}-{}-{}", self.msg, self.version, self.next).into()
    }
}

/// Render a message's planned blocks as the bubble body. Text size, line
/// height and the default text color are inherited from the bubble div.
/// Recursion depth is bounded by the parser's depth budget
/// ([`crate::markdown::MAX_DEPTH`]).
///
/// `highlight` is the active search term (slice 16 §3): every case-insensitive
/// occurrence in the message's text runs gets an accent fill behind it. Code
/// blocks are left alone — their plain-string body has no highlight channel,
/// and dimming source text would hurt more than it helps.
pub fn message_blocks(
    selection_scope: &str,
    msg_idx: usize,
    version: usize,
    blocks: &[PlanBlock],
    palette: &BubblePalette,
    highlight: Option<&str>,
) -> AnyElement {
    let mut ids = Ids {
        msg: msg_idx,
        version,
        next: 0,
    };
    let selection_id: ElementId =
        format!("selectable-message-{selection_scope}-{msg_idx}-{version}").into();
    let mut text = TextCollector::new(palette.selection);
    if blocks.is_empty() {
        // Keep the empty streaming bubble one line tall while awaiting tokens.
        return div().child(" ").into_any_element();
    }
    let body = div()
        .flex()
        .flex_col()
        // w_full is LOAD-BEARING (2026-08-24): without a definite width the
        // block column reports its max-content size, and a paragraph's
        // max-content height is ONE LINE. Ancestors that resolve their height
        // from that intrinsic pass then commit a one-line-tall row while the
        // children later lay out wrapped — the row's content bled over the
        // next row and the scroll extent came up short.
        .w_full()
        .gap(theme::z(theme::SPACE_L))
        .children(blocks.iter().enumerate().map(|(index, b)| {
            let el = block(&mut ids, &mut text, b, palette, highlight);
            let el = if index > 0
                && matches!(
                    b,
                    PlanBlock::Text {
                        heading: Some(_),
                        ..
                    }
                ) {
                div()
                    .pt(theme::z(theme::SP_12))
                    .child(el)
                    .into_any_element()
            } else {
                el
            };
            // Prose blocks wrap at the reading column (O1); code and
            // rules keep the full bubble width. Nested blocks are not
            // wrapped again.
            //
            // The clamp is a flex ROW whose one child grows to at most the
            // measure (M2a-FIX1). A `w_full().max_w(..)` box in this column
            // is LOAD-BEARING wrong: taffy's intrinsic height pass resolves
            // its 100% width WITHOUT the max, so the paragraph was measured
            // at the bubble width and then painted at 560 px, spilling past
            // its box. Main-axis max width is applied in every pass.
            if reading_clamped(b) {
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .child(
                        div()
                            // Test-only lookup key (a no-op outside test builds).
                            .debug_selector(|| "md-reading-column".into())
                            .flex_1()
                            .min_w(theme::z(0.))
                            .max_w(theme::z(theme::READING_MEASURE))
                            .child(el),
                    )
                    .into_any_element()
            } else {
                el
            }
        }))
        .into_any_element();
    text.wrap_in_transcript(selection_id, body, selection_scope, msg_idx)
        .into_any_element()
}

/// A small "⧉ Copy" chip that writes `payload` to the clipboard on click. Used
/// per-message (raw markdown source) and per-code-block (the code text). The
/// payload is a [`SharedString`] so per-frame closure capture is a refcount
/// bump, not a copy of the message. Hover color comes from the caller (the
/// palette inside bubbles — review SF3), never a hard-coded surface accent.
pub fn copy_chip(
    id: ElementId,
    payload: SharedString,
    color: Rgba,
    hover: Rgba,
) -> impl IntoElement {
    div()
        .id(id)
        .px(theme::z(6.))
        .py(theme::z(1.))
        .rounded(theme::z(theme::RADIUS))
        .text_size(theme::z(theme::LABEL_SIZE))
        .text_color(color)
        .cursor_pointer()
        .hover(move |s| s.text_color(hover))
        .child("⧉ Copy")
        .on_click(move |_ev, _window, cx: &mut App| {
            cx.write_to_clipboard(ClipboardItem::new_string(payload.to_string()));
        })
}

fn block(
    ids: &mut Ids,
    collector: &mut TextCollector,
    block: &PlanBlock,
    p: &BubblePalette,
    hl: Option<&str>,
) -> AnyElement {
    match block {
        PlanBlock::Image(image) => gpui::img(std::sync::Arc::clone(image))
            .w_full()
            .h(theme::z(theme::IMAGE_PREVIEW_HEIGHT))
            .object_fit(gpui::ObjectFit::Contain)
            .into_any_element(),
        PlanBlock::Text {
            heading: None,
            text,
        } => text_element(ids, collector, text, hl),
        PlanBlock::Text {
            heading: Some(level),
            text,
        } => div()
            .w_full()
            .debug_selector(|| "md-heading".into())
            .text_size(theme::z(theme::heading_size(*level)))
            .line_height(theme::z(if *level == 1 {
                theme::type_::TITLE.line
            } else if *level == 2 {
                theme::type_::HEADING.line
            } else {
                theme::type_::READ.line
            }))
            .font_weight(FontWeight::SEMIBOLD)
            .child(text_element(ids, collector, text, hl))
            .into_any_element(),
        PlanBlock::Code {
            label,
            display,
            payload,
        } => code_block(ids, collector, label, display, payload, p),
        PlanBlock::List { ordered, items } => list(ids, collector, *ordered, items, p, hl),
        PlanBlock::Quote(inner) => div()
            .border_l_2()
            .border_color(p.quote_bar)
            .pl(theme::z(10.))
            .flex()
            .flex_col()
            .gap(theme::z(theme::SPACE_L))
            .children(inner.iter().map(|b| self::block(ids, collector, b, p, hl)))
            .into_any_element(),
        PlanBlock::Rule => div()
            .h(theme::z(1.))
            .my(theme::z(4.))
            .bg(theme::p().border)
            .into_any_element(),
    }
}

/// One run of inline text: a single `StyledText` whose highlight ranges carry
/// bold/italic/inline-code/link styling; code ranges get the mono family. When
/// links are present the text becomes interactive and a click opens the
/// destination in the default browser. An active search term (slice 16 §3)
/// adds an accent fill behind each matching span, merged with the markdown
/// styling via [`combine_highlights`] so overlaps (a bold match, a matched
/// link) keep both.
fn text_element(
    ids: &mut Ids,
    collector: &mut TextCollector,
    plan: &TextPlan,
    hl: Option<&str>,
) -> AnyElement {
    let highlights: Vec<_> = match hl {
        Some(term) => {
            let mut marks = Vec::new();
            let mut at = 0;
            for (run, is_match) in search::highlight_spans(plan.text.as_ref(), term) {
                let end = at + run.len();
                if is_match {
                    marks.push((
                        at..end,
                        HighlightStyle {
                            background_color: Some(theme::p().search_highlight.into()),
                            ..Default::default()
                        },
                    ));
                }
                at = end;
            }
            combine_highlights(plan.highlights.clone(), marks).collect()
        }
        None => plan.highlights.clone(),
    };
    let styled = collector.capture(
        StyledText::new(plan.text.clone())
            .with_highlights(highlights)
            .with_font_family_overrides(plan.fonts.clone()),
        plan.text.as_ref(),
    );
    if plan.link_ranges.is_empty() {
        return styled.into_any_element();
    }
    let urls = plan.urls.clone();
    let link_click_allowed = collector.link_click_allowed();
    InteractiveText::new(ids.next(), styled)
        .on_click(plan.link_ranges.clone(), move |ix, _window, _cx| {
            if link_click_allowed.get() {
                open_link(&urls[ix]);
            }
        })
        .into_any_element()
}

/// Open a clicked link in the default browser, gated on the pure
/// [`link_allowed`] scheme policy (model output is untrusted — never file://
/// or arbitrary local URL handlers).
fn open_link(url: &str) {
    if !link_allowed(url) {
        eprintln!("awili: refusing to open non-web link {url:?}");
        return;
    }
    if let Err(e) = open::that_detached(url) {
        eprintln!("awili: could not open {url:?}: {e}");
    }
}

/// A fenced/indented code block: header row (language label + copy chip) over
/// the monospace body, on the code overlay fill.
fn code_block(
    ids: &mut Ids,
    collector: &mut TextCollector,
    label: &SharedString,
    display: &SharedString,
    payload: &SharedString,
    p: &BubblePalette,
) -> AnyElement {
    div()
        // Test-only lookup key (a no-op outside test builds): lets the M2a
        // layout proof measure the code block itself.
        .debug_selector(|| "md-code-block".into())
        .flex()
        .flex_col()
        .rounded(theme::z(theme::RADIUS))
        .bg(theme::p().code_bg)
        .border_1()
        .border_color(theme::p().code_border)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px(theme::z(10.))
                .pt(theme::z(5.))
                .child(
                    div()
                        .text_size(theme::z(theme::LABEL_SIZE))
                        .text_color(p.muted)
                        .child(label.clone()),
                )
                .child(copy_chip(
                    ids.next(),
                    payload.clone(),
                    p.muted,
                    p.copy_hover,
                )),
        )
        .child(
            div()
                .px(theme::z(10.))
                .pb(theme::z(8.))
                .font_family(theme::MONO_FONT)
                .text_size(theme::z(theme::CODE_TEXT_SIZE))
                .line_height(theme::z(theme::CODE_LINE_HEIGHT))
                .child(collector.capture(StyledText::new(display.clone()), display.as_ref())),
        )
        .into_any_element()
}

/// Top-level prose blocks (paragraph, heading, list, quote) are clamped to the
/// reading column (O1); code blocks and rules keep the full bubble width.
pub(crate) fn reading_clamped(block: &PlanBlock) -> bool {
    matches!(
        block,
        PlanBlock::Text { .. } | PlanBlock::List { .. } | PlanBlock::Quote(_)
    )
}

/// RC2-UX (R2-26, round 2 N-M2a: "the text column is about 430 px wide inside
/// a wider reply box, leaving dead space on the right"): the width of a
/// reply's box. A reply with no code block is prose, all of it clamped to
/// [`theme::READING_MEASURE`], so its box fits that column (plus the box's
/// own padding and border, `chrome`); a reply with a code block keeps the
/// full transcript width the code needs.
pub(crate) fn reply_box_max_w(plan: &[PlanBlock], chrome: f32) -> f32 {
    if plan.iter().any(|b| matches!(b, PlanBlock::Code { .. })) {
        theme::TRANSCRIPT_CONTENT
    } else {
        (theme::READING_MEASURE + chrome).min(theme::TRANSCRIPT_CONTENT)
    }
}

/// Bulleted or numbered list: a marker column beside each item's blocks.
/// Items recurse, so nested lists indent naturally under their marker.
fn list(
    ids: &mut Ids,
    collector: &mut TextCollector,
    ordered: Option<u64>,
    items: &[Vec<PlanBlock>],
    p: &BubblePalette,
    hl: Option<&str>,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(theme::z(theme::SP_4))
        .text_size(theme::z(theme::type_::BODY.size))
        .line_height(theme::z(theme::type_::BODY.line))
        .children(items.iter().enumerate().map(|(i, item)| {
            let marker: SharedString = match ordered {
                Some(start) => format!("{}.", start + i as u64).into(),
                None => "•".into(),
            };
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(theme::z(8.))
                .child(div().flex_none().min_w(theme::z(18.)).child(marker))
                .child(
                    div()
                        .flex_1()
                        .min_w(theme::z(0.))
                        .flex()
                        .flex_col()
                        .gap(theme::z(4.))
                        .children(item.iter().map(|b| block(ids, collector, b, p, hl))),
                )
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::parse_or_plain;
    use crate::markdown_plan::plan_blocks;

    /// The blocker's render-side regression: a hostile deeply-nested message
    /// must build its element tree without exhausting the stack. Element
    /// construction needs no window, so this exercises the same recursion a
    /// frame would — bounded because the parser bounded the tree.
    #[test]
    fn adversarial_depth_builds_elements_without_overflow() {
        let quotes = format!("{}deep", "> ".repeat(50_000));
        let plan = plan_blocks(&parse_or_plain(&quotes), &ASSISTANT);
        let _el = message_blocks("depth-test", 0, 0, &plan, &ASSISTANT, None);

        let mut nested_list = String::new();
        for i in 0..200 {
            nested_list.push_str(&"  ".repeat(i));
            nested_list.push_str("- x\n");
        }
        let plan = plan_blocks(&parse_or_plain(&nested_list), &USER);
        let _el = message_blocks("depth-test", 1, 0, &plan, &USER, Some("x"));
    }
}

#[cfg(test)]
mod overlap_repro {
    use super::*;
    use crate::markdown::parse_or_plain;
    use crate::markdown_plan::{plan_blocks, PlanBlock};
    use gpui::{px, Context, IntoElement, Render, ScrollHandle, TestAppContext, Window};

    struct Repro {
        rows: Vec<Vec<PlanBlock>>,
        scroll: ScrollHandle,
        /// Whether the bubble carries a definite max width (production does).
        clamp: bool,
    }

    impl Render for Repro {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            // Byte-for-byte the production transcript nesting: scroll viewport
            // -> centered measure column (gap 16) -> per-row flex_col (gap 4)
            // -> assistant bubble (w_full, padding, hairline) -> blocks.
            let mut col = gpui::div()
                .flex()
                .flex_col()
                .gap(px(16.))
                .w_full()
                .max_w(px(760.))
                .mx_auto()
                .px(px(28.))
                .py(px(28.));
            for (i, plan) in self.rows.iter().enumerate() {
                let key: &'static str = if i == 0 { "row0" } else { "row1" };
                let mut bubble = gpui::div()
                    .w_full()
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(px(8.))
                    .border_1();
                if self.clamp {
                    bubble = bubble.max_w(px(704.));
                }
                col = col.child(
                    gpui::div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .debug_selector(move || key.to_string())
                        .child(gpui::div().child("Awili"))
                        .child(bubble.child(message_blocks(
                            "overlap-test",
                            i,
                            1,
                            plan,
                            &ASSISTANT,
                            None,
                        )))
                        // The third child matters: production rows carry a
                        // header, the bubble, and an actions row.
                        .child(gpui::div().h(px(20.)).child("actions")),
                );
            }
            gpui::div().flex().flex_col().size_full().child(
                gpui::div()
                    .id("list")
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(col),
            )
        }
    }

    /// 2026-08-24: two long assistant turns in the transcript — the second row
    /// painted OVER the first (its origin landed ~115px above the first row's
    /// end). Text measurement was proven correct, so the fault is in the row
    /// geometry. Assert the rows tile without overlapping.
    #[gpui::test]
    fn consecutive_long_rows_do_not_overlap(cx: &mut TestAppContext) {
        let long = "Here's the accurate picture, based on an actual check I just ran rather than a guess:\n\n**No Chrome extension — but I do have built-in browser automation, though it's not available in this particular session.**\n\nDetails:\n\n- **What I normally have:** a Browser Use capability that drives a real browser — open sites, click, type, fill forms, read rendered pages, take screenshots. It comes in three flavors: ZCode's in-app browser pane (the usual one on desktop), a Chrome-*extension* backend (only when the runtime advertises it — I won't claim it exists without seeing it listed), and a headless managed Chromium (when the CLI is started with `--browser-use=headless`).\n- **What I just found:** I queried the session's browser registry and it returned *\"Browser control is unavailable for this node_repl session.\"* So right now I can't drive any browser for you — likely meaning this session was started in a mode where browser control is off, or the host didn't wire it up.\n- **What still works without a browser:** I can fetch and read specific URLs as text/markdown (WebFetch / web-reader) and do web searches — read-only, no clicking or form-filling. That covers a lot of \"use websites for me\" cases like looking things up or extracting page content.\n\nIf you want the full click-and-type browser control, the likely fix is starting a fresh ZCode session (desktop normally enables the in-app browser) or launching the CLI with `--browser-use=headless` for the managed Chromium. Want me to test whether it comes alive in a new session, or is read-only page fetching enough for what you had in mind?";
        let plan_a = plan_blocks(&parse_or_plain(long), &ASSISTANT);
        let plan_b = plan_blocks(&parse_or_plain(long), &ASSISTANT);
        let (_v, cx) = cx.add_window_view(|_w, _cx| Repro {
            rows: vec![plan_a, plan_b],
            scroll: ScrollHandle::new(),
            clamp: true,
        });
        cx.simulate_resize(gpui::size(px(900.), px(600.)));
        cx.run_until_parked();
        let r0 = cx.debug_bounds("row0").expect("row0");
        let r1 = cx.debug_bounds("row1").expect("row1");
        eprintln!(
            "row0 y={:?} h={:?}\nrow1 y={:?} h={:?}",
            r0.origin.y, r0.size.height, r1.origin.y, r1.size.height
        );
        assert!(
            f32::from(r0.size.height) > 200.0,
            "row0 collapsed: {:?}",
            r0.size
        );
        assert!(
            r1.origin.y >= r0.origin.y + r0.size.height,
            "row1 overlaps row0: row0 ends {:?}, row1 starts {:?}",
            r0.origin.y + r0.size.height,
            r1.origin.y
        );
        // The row box must actually CONTAIN its bubble. The 2026-08-24 bug
        // left the row at a one-line height (95px for 295px of children) —
        // rows tiled tidily while their content bled over the next row and
        // the scroll extent came up short.
        assert!(
            f32::from(r0.size.height) >= f32::from(r1.origin.y - r0.origin.y) - 20.0,
            "row0 box is smaller than the space its content occupies"
        );
    }
}

#[cfg(test)]
mod m2a_tests {
    use super::*;
    use crate::markdown::parse_or_plain;
    use crate::markdown_plan::{plan_blocks, text_plan, ASSISTANT};
    use crate::theme;
    use gpui::{
        px, Context, IntoElement, Render, ScrollHandle, SharedString, TestAppContext, Window,
    };

    /// O1: inline code must paint a lighter fill than code blocks, and a
    /// fenced block still plans as a Code block (rendered through `code_block`
    /// with the block fill).
    #[test]
    fn m2a_inline_code_is_lighter_than_blocks() {
        assert!(theme::p().inline_code_bg.a < theme::p().code_bg.a);

        let blocks = parse_or_plain("a `x` b");
        match &blocks[0] {
            crate::markdown::Block::Paragraph(spans) => {
                let plan = text_plan(spans, &ASSISTANT);
                assert!(
                    plan.highlights
                        .iter()
                        .any(|(_, s)| s.background_color == Some(theme::p().inline_code_bg.into())),
                    "planned inline code carries the lighter fill"
                );
            }
            other => panic!("expected a paragraph, got {other:?}"),
        }

        let plan = plan_blocks(&parse_or_plain("```rs\nfn main() {}\n```"), &ASSISTANT);
        assert!(
            plan.iter().any(|b| matches!(b, PlanBlock::Code { .. })),
            "a fenced block stays a Code block"
        );
    }

    /// The reading clamp reaches every prose block kind and no code/rule block.
    #[test]
    fn m2a_reading_clamp_applies_to_prose_only() {
        let para = plan_blocks(&parse_or_plain("just a paragraph"), &ASSISTANT)[0].clone();
        assert!(reading_clamped(&para));
        let heading = plan_blocks(&parse_or_plain("# a heading"), &ASSISTANT)[0].clone();
        assert!(matches!(
            heading,
            PlanBlock::Text {
                heading: Some(1),
                ..
            }
        ));
        assert!(reading_clamped(&heading));
        let list = plan_blocks(&parse_or_plain("- one\n- two"), &ASSISTANT)[0].clone();
        assert!(matches!(list, PlanBlock::List { .. }));
        assert!(reading_clamped(&list));
        let quote = plan_blocks(&parse_or_plain("> quoted"), &ASSISTANT)[0].clone();
        assert!(matches!(quote, PlanBlock::Quote(_)));
        assert!(reading_clamped(&quote));
        assert!(!reading_clamped(&PlanBlock::Code {
            label: "rust".into(),
            display: "x".into(),
            payload: "x".into(),
        }));
        assert!(!reading_clamped(&PlanBlock::Rule));
    }

    /// A ~1000-character paragraph rendered by `message_blocks` DIRECTLY in a
    /// 884 px bubble (no 680 px parent in the test) must wrap at the 680 px
    /// reading measure, not at the bubble's 856 px content width. Row A is
    /// the bubble. Row B is an independent plain text column 680 px wide; row
    /// C is the same text 856 px wide (what the paragraph would do without
    /// the clamp). The paragraph's own box (`md-reading-column`) must be
    /// 680 px wide and as tall as B, A's height must equal B's height plus
    /// the bubble padding (10 px top + 10 px bottom), and B must be taller
    /// than C, so the proof fails if the prose clamp stops working.
    #[gpui::test]
    fn m2a_paragraph_wraps_at_reading_measure(cx: &mut TestAppContext) {
        let mut para = String::new();
        for _ in 0..200 {
            para.push_str("word");
            para.push(' ');
        }
        assert!(
            para.len() >= 900,
            "the proof paragraph is ~900 characters, not less: {}",
            para.len()
        );
        let plan = plan_blocks(&parse_or_plain(&para), &ASSISTANT);
        let bare: SharedString = match &plan[0] {
            PlanBlock::Text { text, .. } => text.text.clone(),
            other => panic!("expected a paragraph, got {other:?}"),
        };

        struct Measure {
            plan: Vec<PlanBlock>,
            bare: SharedString,
            scroll: ScrollHandle,
        }
        impl Render for Measure {
            fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                gpui::div().flex().flex_col().size_full().child(
                    gpui::div()
                        .id("list")
                        .flex_1()
                        .min_h(px(0.))
                        .w_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(
                            gpui::div()
                                .flex()
                                .flex_col()
                                .gap(px(16.))
                                .w_full()
                                .max_w(px(940.))
                                .mx_auto()
                                .px(px(28.))
                                .py(px(28.))
                                .child(
                                    // Row A: the real 884 px bubble; message_blocks
                                    // is its direct child (no width clamp here).
                                    gpui::div()
                                        .debug_selector(|| "rowA".to_string())
                                        .w_full()
                                        .max_w(px(884.))
                                        .px(px(14.))
                                        .py(px(10.))
                                        .text_size(px(theme::TEXT_SIZE))
                                        .line_height(px(theme::LINE_HEIGHT))
                                        .child(message_blocks(
                                            "m2a-measure",
                                            0,
                                            1,
                                            &self.plan,
                                            &ASSISTANT,
                                            None,
                                        )),
                                )
                                .child(
                                    // Row B: independent 680 px reference.
                                    gpui::div()
                                        .debug_selector(|| "rowB".to_string())
                                        .w(px(theme::READING_MEASURE))
                                        .text_size(px(theme::TEXT_SIZE))
                                        .line_height(px(theme::LINE_HEIGHT))
                                        .child(self.bare.clone()),
                                )
                                .child(
                                    // Row C: the unclamped width (884 - 2 * 14).
                                    gpui::div()
                                        .debug_selector(|| "rowC".to_string())
                                        .w(px(884. - 28.))
                                        .text_size(px(theme::TEXT_SIZE))
                                        .line_height(px(theme::LINE_HEIGHT))
                                        .child(self.bare.clone()),
                                ),
                        ),
                )
            }
        }

        let (_v, cx) = cx.add_window_view(|_w, _cx| Measure {
            plan,
            bare,
            scroll: ScrollHandle::new(),
        });
        cx.simulate_resize(gpui::size(px(1000.), px(800.)));
        cx.run_until_parked();
        let a = cx.debug_bounds("rowA").expect("rowA");
        let b = cx.debug_bounds("rowB").expect("rowB");
        let c = cx.debug_bounds("rowC").expect("rowC");
        let col = cx
            .debug_bounds("md-reading-column")
            .expect("md-reading-column");
        let col_w = f32::from(col.size.width);
        let col_h = f32::from(col.size.height);
        let a_w = f32::from(a.size.width);
        let a_h = f32::from(a.size.height);
        let b_h = f32::from(b.size.height);
        let c_h = f32::from(c.size.height);
        assert!(
            (a_w - 884.).abs() <= 1.0,
            "row A is the full 884 px bubble (got {a_w:.1} px)"
        );
        assert!(
            b_h >= c_h + theme::LINE_HEIGHT,
            "the proof text must wrap to more lines at 680 px than at 856 px \
             (680: {b_h:.1}, 856: {c_h:.1})"
        );
        // The paragraph's own box: 680 px wide and exactly as tall as the
        // 680 px reference (its text is laid out at the width it is boxed at).
        assert!(
            (col_w - theme::READING_MEASURE).abs() <= 1.0,
            "the paragraph box is the {} px reading column (got {col_w:.1} px)",
            theme::READING_MEASURE
        );
        assert!(
            (col_h - b_h).abs() <= 2.0,
            "the paragraph box is as tall as the 680 px reference (box {col_h:.1} vs rowB {b_h:.1})"
        );
        // Row A's bubble padding: 10 px top + 10 px bottom.
        assert!(
            (a_h - (b_h + 20.)).abs() <= 2.0,
            "paragraph must wrap at the reading measure (rowA {a_h:.1} vs rowB {b_h:.1} + 20; \
             unclamped would be rowC {c_h:.1} + 20)"
        );
    }

    #[gpui::test]
    fn response_presentation_long_heading_stays_inside_reading_column(cx: &mut TestAppContext) {
        struct HeadingRow {
            plan: Vec<PlanBlock>,
        }
        impl Render for HeadingRow {
            fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(
                    div()
                        .w_full()
                        .max_w(px(theme::READING_MEASURE))
                        .debug_selector(|| "response-reading-body".into())
                        .text_size(px(theme::type_::READ.size))
                        .line_height(px(theme::type_::READ.line))
                        .child(message_blocks(
                            "response-heading",
                            0,
                            1,
                            &self.plan,
                            &ASSISTANT,
                            None,
                        )),
                )
            }
        }
        let source = format!(
            "Opening paragraph.\n\n## {}\n\nClosing paragraph.",
            "A long heading must wrap predictably ".repeat(8)
        );
        let (_v, window) = cx.add_window_view(|_, _| HeadingRow {
            plan: plan_blocks(&parse_or_plain(&source), &ASSISTANT),
        });
        for width in [480., 1000.] {
            window.simulate_resize(gpui::size(px(width), px(900.)));
            window.run_until_parked();
            let heading = window.debug_bounds("md-heading").unwrap();
            let body = window.debug_bounds("response-reading-body").unwrap();
            assert!(heading.size.height > px(theme::type_::HEADING.line));
            assert!(heading.right() <= body.right() + px(1.));
            assert!(heading.bottom() <= body.bottom());
        }
    }

    /// A long single-line code block keeps the full bubble width: the code
    /// block's OWN bounds (the `md-code-block` selector on `code_block`) span
    /// the bubble's content width (884 - 2 * 14 = 856 px), well past the
    /// 680 px reading measure.
    #[gpui::test]
    fn m2a_code_block_keeps_bubble_width(cx: &mut TestAppContext) {
        let code: String = "x".repeat(300);
        let plan = plan_blocks(&parse_or_plain(&format!("```rs\n{code}\n```")), &ASSISTANT);
        assert!(matches!(plan.first(), Some(PlanBlock::Code { .. })));

        struct CodeRow {
            plan: Vec<PlanBlock>,
            scroll: ScrollHandle,
        }
        impl Render for CodeRow {
            fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                gpui::div().flex().flex_col().size_full().child(
                    gpui::div()
                        .id("list")
                        .flex_1()
                        .min_h(px(0.))
                        .w_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(
                            gpui::div()
                                .flex()
                                .flex_col()
                                .w_full()
                                .px(px(28.))
                                .py(px(28.))
                                .child(
                                    gpui::div()
                                        .debug_selector(|| "codeBubble".to_string())
                                        .w_full()
                                        .max_w(px(884.))
                                        .px(px(14.))
                                        .py(px(10.))
                                        .text_size(px(theme::TEXT_SIZE))
                                        .line_height(px(theme::LINE_HEIGHT))
                                        .child(message_blocks(
                                            "m2a-code", 0, 1, &self.plan, &ASSISTANT, None,
                                        )),
                                ),
                        ),
                )
            }
        }
        let (_v, cx) = cx.add_window_view(|_w, _cx| CodeRow {
            plan,
            scroll: ScrollHandle::new(),
        });
        cx.simulate_resize(gpui::size(px(1000.), px(800.)));
        cx.run_until_parked();
        let bubble = cx.debug_bounds("codeBubble").expect("codeBubble");
        let code = cx.debug_bounds("md-code-block").expect("md-code-block");
        let bubble_w = f32::from(bubble.size.width);
        let code_w = f32::from(code.size.width);
        assert!(
            (bubble_w - 884.).abs() <= 1.0,
            "the bubble is 884 px wide (got {bubble_w:.1} px)"
        );
        assert!(
            (code_w - (bubble_w - 28.)).abs() <= 1.0,
            "the code block spans the bubble content width (code {code_w:.1} px, bubble {bubble_w:.1} px - 28)"
        );
        assert!(
            code_w >= theme::READING_MEASURE + 100.,
            "the code block is not clamped to the {} px reading measure (got {code_w:.1} px)",
            theme::READING_MEASURE
        );
    }
}
