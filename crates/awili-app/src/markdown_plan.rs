//! Pure render-plan layer between [`crate::markdown`] blocks and the GPUI
//! elements (review slice 6, SF2/SF4): every styling decision — highlight
//! runs, mono-font ranges, clickable link ranges and their destinations, copy
//! payloads, the untrusted-link scheme policy, per-role colors — is resolved
//! HERE, once per parse, into plain data. The element half (`markdown_ui`)
//! only assembles divs from a finished plan, so per-frame work is element
//! construction plus refcounted [`SharedString`] clones, and every decision at
//! the GPUI/security/copy boundary is unit-testable headless.
//!
//! Plan depth mirrors the parse tree, which the parser caps at
//! [`crate::markdown::MAX_DEPTH`] — recursion here and in the element builder
//! is bounded by construction.

use std::ops::Range;
use std::sync::Arc;

use gpui::{FontStyle, FontWeight, HighlightStyle, Rgba, SharedString, UnderlineStyle};

use crate::markdown::{flatten, Block, Inline};
use crate::theme;

/// Per-role colors for markdown inside a bubble. Assistant bubbles sit on
/// charcoal; the user's own sit on the Awili-blue accent fill, where bright-blue
/// links would vanish — so links there use the bubble ink + underline.
#[derive(Clone, Copy)]
pub struct BubblePalette {
    pub link: Rgba,
    /// Code language label + copy chips.
    pub muted: Rgba,
    /// Copy-chip hover. Per-palette (review SF3): the assistant-surface accent
    /// would sink into the user bubble's blue-tinted code fill exactly while
    /// the chip is targeted.
    pub copy_hover: Rgba,
    pub quote_bar: Rgba,
    /// Native transcript-selection fill, chosen against the bubble surface.
    pub selection: Rgba,
}

#[cfg(test)]
pub const ASSISTANT: BubblePalette = BubblePalette {
    link: theme::LINK,
    muted: theme::MUTED,
    copy_hover: theme::ACCENT,
    quote_bar: theme::QUOTE_BAR,
    selection: theme::SELECTION,
};

#[cfg(test)]
pub const USER: BubblePalette = BubblePalette {
    link: theme::LINK_ON_ACCENT,
    muted: theme::ON_ACCENT,
    // Bone TEXT pops on the black-tinted blue code fill; blue ACCENT would
    // lose contrast right as the chip is hovered (review SF3).
    copy_hover: theme::TEXT,
    quote_bar: theme::LINK_ON_ACCENT,
    selection: theme::SELECTION_ON_ACCENT,
};

/// The current role palette; callers resolve it when planning and painting.
pub fn assistant() -> BubblePalette {
    let p = theme::p();
    BubblePalette {
        link: p.link,
        muted: p.muted,
        copy_hover: p.accent_text,
        quote_bar: p.quote_bar,
        selection: p.selection,
    }
}

pub fn user() -> BubblePalette {
    let p = theme::p();
    BubblePalette {
        link: p.link_on_accent,
        muted: p.on_accent,
        copy_hover: p.copy_hover_on_accent,
        quote_bar: p.link_on_accent,
        selection: p.selection_on_accent,
    }
}

/// One paragraph/heading's render-ready text: flattened once at plan time,
/// with resolved highlight runs and clickable ranges. Per-frame cost is
/// cloning bounded vectors and bumping refcounts — never re-flattening.
#[derive(Clone, Debug)]
pub struct TextPlan {
    pub text: SharedString,
    pub highlights: Vec<(Range<usize>, HighlightStyle)>,
    /// Mono-family overrides for inline-code ranges.
    pub fonts: Vec<(Range<usize>, SharedString)>,
    /// Clickable byte ranges; index-aligned with `urls`.
    pub link_ranges: Vec<Range<usize>>,
    /// Destination per clickable range, shared with the click closure.
    pub urls: Arc<Vec<String>>,
}

/// A render-ready block: the [`Block`] tree with all text flattened, styled,
/// and copy payloads resolved.
#[derive(Clone, Debug)]
pub enum PlanBlock {
    Image(Arc<gpui::Image>),
    /// Paragraph (`heading: None`) or heading text.
    Text {
        heading: Option<u8>,
        text: TextPlan,
    },
    Code {
        /// Language label for the header row (empty when none).
        label: SharedString,
        /// What the body shows: trailing newline trimmed, never empty (an
        /// empty block keeps one space so the frame stays one line tall).
        display: SharedString,
        /// What the copy chip writes — the EXACT code text, untrimmed.
        payload: SharedString,
    },
    List {
        ordered: Option<u64>,
        items: Vec<Vec<PlanBlock>>,
    },
    Quote(Vec<PlanBlock>),
    Rule,
}

/// Resolve a parsed message into its render plan for one bubble role.
pub fn plan_blocks(blocks: &[Block], p: &BubblePalette) -> Vec<PlanBlock> {
    blocks.iter().map(|b| plan_block(b, p)).collect()
}

fn plan_block(block: &Block, p: &BubblePalette) -> PlanBlock {
    match block {
        Block::Image { mime, bytes } => {
            let format = match mime.as_str() {
                "image/jpeg" => gpui::ImageFormat::Jpeg,
                "image/gif" => gpui::ImageFormat::Gif,
                "image/webp" => gpui::ImageFormat::Webp,
                _ => gpui::ImageFormat::Png,
            };
            PlanBlock::Image(Arc::new(gpui::Image::from_bytes(format, bytes.clone())))
        }
        Block::Paragraph(spans) => PlanBlock::Text {
            heading: None,
            text: text_plan(spans, p),
        },
        Block::Heading { level, spans } => PlanBlock::Text {
            heading: Some(*level),
            text: text_plan(spans, p),
        },
        Block::CodeBlock { lang, code } => {
            let shown = code.trim_end_matches('\n');
            PlanBlock::Code {
                label: lang.clone().unwrap_or_default().into(),
                display: if shown.is_empty() {
                    " ".into()
                } else {
                    shown.to_string().into()
                },
                payload: code.clone().into(),
            }
        }
        Block::List { ordered, items } => PlanBlock::List {
            ordered: *ordered,
            items: items.iter().map(|item| plan_blocks(item, p)).collect(),
        },
        Block::Blockquote(inner) => PlanBlock::Quote(plan_blocks(inner, p)),
        Block::Rule => PlanBlock::Rule,
    }
}

/// Flatten one paragraph's spans into text plus resolved gpui styling: bold/
/// italic weights, inline-code overlay + mono range, link color + underline +
/// clickable range. Code ranges keep the bubble's text color; links take the
/// palette's.
pub fn text_plan(spans: &[Inline], p: &BubblePalette) -> TextPlan {
    let (text, ranges) = flatten(spans);
    let mut highlights = Vec::new();
    let mut fonts = Vec::new();
    let mut link_ranges = Vec::new();
    let mut urls = Vec::new();
    for r in ranges.iter().filter(|r| r.is_styled()) {
        let mut style = HighlightStyle::default();
        if r.bold {
            style.font_weight = Some(FontWeight::SEMIBOLD);
        }
        if r.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        if r.code {
            style.background_color = Some(theme::p().inline_code_bg.into());
            fonts.push((r.range.clone(), SharedString::from(theme::MONO_FONT)));
        }
        if let Some(url) = &r.link {
            style.color = Some(p.link.into());
            style.underline = Some(UnderlineStyle {
                thickness: theme::z(1.),
                color: None,
                wavy: false,
            });
            link_ranges.push(r.range.clone());
            urls.push(url.clone());
        }
        highlights.push((r.range.clone(), style));
    }
    TextPlan {
        text: text.into(),
        highlights,
        fonts,
        link_ranges,
        urls: Arc::new(urls),
    }
}

/// The untrusted-link policy: model output is untrusted, so only web/mail
/// destinations may leave the app — never `file://` or arbitrary local URL
/// handlers. Case-sensitive on purpose: an exotic `HTTPS://…` fails closed.
pub fn link_allowed(url: &str) -> bool {
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| url.starts_with(scheme))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::{self, parse_or_plain};

    fn para_plan(source: &str, p: &BubblePalette) -> TextPlan {
        match plan_blocks(&parse_or_plain(source), p)
            .into_iter()
            .next()
            .unwrap()
        {
            PlanBlock::Text {
                heading: None,
                text,
            } => text,
            other => panic!("expected paragraph plan, got {other:?}"),
        }
    }

    fn linked(text: &TextPlan, ix: usize) -> &str {
        &text.text[text.link_ranges[ix].clone()]
    }

    // ---- combined style highlights ----

    #[test]
    fn combined_bold_italic_link_highlight() {
        let plan = para_plan("**[*x*](https://e.com)**", &ASSISTANT);
        assert_eq!(plan.text.as_ref(), "x");
        let (range, style) = &plan.highlights[0];
        assert_eq!(range, &(0..1));
        assert_eq!(style.font_weight, Some(FontWeight::SEMIBOLD));
        assert_eq!(style.font_style, Some(FontStyle::Italic));
        assert_eq!(style.color, Some(theme::LINK.into()));
        assert!(style.underline.is_some());
    }

    #[test]
    fn inline_code_gets_overlay_and_mono_range() {
        let plan = para_plan("a `code` b", &ASSISTANT);
        let (range, style) = plan
            .highlights
            .iter()
            .find(|(_, s)| s.background_color.is_some())
            .unwrap();
        assert_eq!(style.background_color, Some(theme::INLINE_CODE_BG.into()));
        assert_eq!(style.color, None, "inline code keeps the bubble text color");
        assert_eq!(&plan.text[range.clone()], "code");
        assert_eq!(
            plan.fonts,
            vec![(range.clone(), SharedString::from(theme::MONO_FONT))]
        );
    }

    // ---- the clickable-range → URL mapping ----

    #[test]
    fn multiple_links_map_ranges_to_urls_in_order() {
        let plan = para_plan(
            "see [a](https://a.example) and [b](https://b.example)",
            &ASSISTANT,
        );
        assert_eq!(plan.link_ranges.len(), 2);
        assert_eq!(
            (linked(&plan, 0), plan.urls[0].as_str()),
            ("a", "https://a.example")
        );
        assert_eq!(
            (linked(&plan, 1), plan.urls[1].as_str()),
            ("b", "https://b.example")
        );
    }

    #[test]
    fn link_split_across_styled_runs_keeps_url_for_each_run() {
        // Bold flips inside the link text → two runs, one destination. A click
        // on EITHER run must resolve to that destination.
        let plan = para_plan("[plain **bold**](https://e.com/x)", &ASSISTANT);
        assert_eq!(plan.link_ranges.len(), 2);
        assert_eq!(linked(&plan, 0), "plain ");
        assert_eq!(linked(&plan, 1), "bold");
        assert_eq!(plan.urls.as_slice(), ["https://e.com/x", "https://e.com/x"]);
        // The bold run carries weight + link styling together.
        let (_, style) = plan
            .highlights
            .iter()
            .find(|(r, _)| r == &plan.link_ranges[1])
            .unwrap();
        assert_eq!(style.font_weight, Some(FontWeight::SEMIBOLD));
        assert_eq!(style.color, Some(ASSISTANT.link.into()));
    }

    #[test]
    fn multibyte_link_text_ranges_slice_cleanly() {
        let plan = para_plan("→ [héllo wörld](https://e.com) ←", &ASSISTANT);
        assert_eq!(plan.link_ranges.len(), 1);
        assert_eq!(linked(&plan, 0), "héllo wörld");
        assert!(plan.text.is_char_boundary(plan.link_ranges[0].start));
        assert!(plan.text.is_char_boundary(plan.link_ranges[0].end));
    }

    // ---- the untrusted-link scheme policy ----

    #[test]
    fn link_policy_allows_web_and_mail_only() {
        assert!(link_allowed("https://example.com/x"));
        assert!(link_allowed("http://example.com"));
        assert!(link_allowed("mailto:a@b.c"));
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "vscode://extension/install",
            "ftp://host/p",
            "data:text/html,<script>",
            "HTTPS://example.com", // case-mangled scheme fails closed
            "",
            "relative/path",
            " https://leading-space.example",
        ] {
            assert!(!link_allowed(url), "{url:?} must be refused");
        }
    }

    #[test]
    fn mixed_schemes_keep_mapping_policy_filters_at_click() {
        // Both ranges stay clickable in the plan; the policy is what refuses
        // the bad one when the click handler consults it.
        let plan = para_plan("[ok](https://e.com) [bad](file:///etc/passwd)", &ASSISTANT);
        assert_eq!(plan.urls.len(), 2);
        assert!(link_allowed(&plan.urls[0]));
        assert!(!link_allowed(&plan.urls[1]));
    }

    // ---- exact clipboard payloads ----

    fn code_plan(source: &str) -> (SharedString, SharedString, SharedString) {
        match plan_blocks(&parse_or_plain(source), &ASSISTANT)
            .into_iter()
            .next()
            .unwrap()
        {
            PlanBlock::Code {
                label,
                display,
                payload,
            } => (label, display, payload),
            other => panic!("expected code plan, got {other:?}"),
        }
    }

    #[test]
    fn code_copy_payload_is_exact_including_trailing_newline() {
        let (label, display, payload) = code_plan("```rust\nfn main() {}\n```");
        assert_eq!(payload.as_ref(), "fn main() {}\n", "payload untrimmed");
        assert_eq!(
            display.as_ref(),
            "fn main() {}",
            "display trims the newline"
        );
        assert_eq!(label.as_ref(), "rust");
    }

    #[test]
    fn empty_code_block_copies_empty_but_displays_one_space() {
        let (label, display, payload) = code_plan("```\n```");
        assert_eq!(payload.as_ref(), "");
        assert_eq!(display.as_ref(), " ");
        assert_eq!(label.as_ref(), "");
    }

    #[test]
    fn code_display_trims_every_trailing_newline_not_just_one() {
        // The display strips EVERY trailing newline (`trim_end_matches`), so a
        // block that ends in blank lines renders flush — not with dangling
        // empty rows. The copy payload keeps them all, exact. Constructed
        // directly so the pin isolates `plan_block`'s trim, not the parser's
        // trailing-newline handling. The single-newline payload test above
        // cannot distinguish trim-all from trim-one.
        let plan = plan_blocks(
            &[Block::CodeBlock {
                lang: None,
                code: "x\n\n\n".into(),
            }],
            &ASSISTANT,
        );
        match plan.into_iter().next().unwrap() {
            PlanBlock::Code {
                display, payload, ..
            } => {
                assert_eq!(display.as_ref(), "x", "all trailing newlines trimmed");
                assert_eq!(payload.as_ref(), "x\n\n\n", "payload keeps them exact");
            }
            other => panic!("expected code plan, got {other:?}"),
        }
    }

    // ---- both bubble palettes ----

    #[test]
    fn both_palettes_color_links_from_their_token() {
        let a = para_plan("[x](https://e.com)", &ASSISTANT);
        let u = para_plan("[x](https://e.com)", &USER);
        assert_eq!(a.highlights[0].1.color, Some(theme::LINK.into()));
        assert_eq!(u.highlights[0].1.color, Some(theme::LINK_ON_ACCENT.into()));
    }

    #[test]
    fn copy_chip_hover_keeps_contrast_per_palette() {
        // Assistant chips brighten to the accent; user chips must NOT take the
        // assistant accent (blue on blue-tinted code fill) — review SF3.
        assert_eq!(ASSISTANT.copy_hover, theme::ACCENT);
        assert_eq!(USER.copy_hover, theme::TEXT);
        assert_ne!(USER.copy_hover, theme::ACCENT);
    }

    #[test]
    fn transcript_selection_uses_a_surface_specific_palette() {
        assert_eq!(ASSISTANT.selection, theme::SELECTION);
        assert_eq!(USER.selection, theme::SELECTION_ON_ACCENT);
        assert_ne!(USER.selection, ASSISTANT.selection);
        assert_eq!(USER.selection.a, 0.60);
        assert_eq!(
            (USER.selection.r, USER.selection.g, USER.selection.b),
            (
                theme::SELECTION_ON_ACCENT.r,
                theme::SELECTION_ON_ACCENT.g,
                theme::SELECTION_ON_ACCENT.b
            )
        );
    }

    // ---- hostile depth through the plan (review blocker 1, render side) ----

    fn plan_depth(blocks: &[PlanBlock]) -> usize {
        blocks
            .iter()
            .map(|b| match b {
                PlanBlock::Quote(inner) => 1 + plan_depth(inner),
                PlanBlock::List { items, .. } => {
                    1 + items.iter().map(|i| plan_depth(i)).max().unwrap_or(0)
                }
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn adversarial_nesting_plans_within_depth_budget() {
        let source = format!("{}deep", "> ".repeat(50_000));
        let plan = plan_blocks(&parse_or_plain(&source), &ASSISTANT);
        assert!(plan_depth(&plan) <= markdown::MAX_DEPTH);
    }

    #[test]
    fn oversize_message_plans_as_plain_truncated_text() {
        let source = "**b** ".repeat(60_000); // 360 KiB of styled markdown
        let plan = plan_blocks(&parse_or_plain(&source), &ASSISTANT);
        match &plan[0] {
            PlanBlock::Text {
                heading: None,
                text,
            } => {
                assert!(text.highlights.is_empty(), "fallback is unstyled");
                assert!(text.text.len() <= markdown::MAX_SOURCE_BYTES);
            }
            other => panic!("expected plain text plan, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod glm_truncation_repro {
    use super::*;
    use crate::markdown;

    /// Repro for the 2026-08-24 live bug: a real GLM reply rendered truncated
    /// mid-italic ("…returned *\"Browser control is") while the stored text
    /// was complete. The plan must carry the message to its final sentence.
    #[test]
    fn real_glm_reply_plans_to_the_end() {
        let text = "Here's the accurate picture, based on an actual check I just ran rather than a guess:\n\n**No Chrome extension — but I do have built-in browser automation, though it's not available in this particular session.**\n\nDetails:\n\n- **What I normally have:** a Browser Use capability that drives a real browser — open sites, click, type, fill forms, read rendered pages, take screenshots. It comes in three flavors: ZCode's in-app browser pane (the usual one on desktop), a Chrome-*extension* backend (only when the runtime advertises it — I won't claim it exists without seeing it listed), and a headless managed Chromium (when the CLI is started with `--browser-use=headless`).\n- **What I just found:** I queried the session's browser registry and it returned *\"Browser control is unavailable for this node_repl session.\"* So right now I can't drive any browser for you — likely meaning this session was started in a mode where browser control is off, or the host didn't wire it up.\n- **What still works without a browser:** I can fetch and read specific URLs as text/markdown (WebFetch / web-reader) and do web searches — read-only, no clicking or form-filling. That covers a lot of \"use websites for me\" cases like looking things up or extracting page content.\n\nIf you want the full click-and-type browser control, the likely fix is starting a fresh ZCode session (desktop normally enables the in-app browser) or launching the CLI with `--browser-use=headless` for the managed Chromium. Want me to test whether it comes alive in a new session, or is read-only page fetching enough for what you had in mind?\n";
        let blocks = plan_blocks(&markdown::parse_or_plain(text), &ASSISTANT);
        fn collect(blocks: &[PlanBlock], out: &mut String) {
            for b in blocks {
                match b {
                    PlanBlock::Text { text, .. } => out.push_str(&text.text),
                    PlanBlock::Code { payload, .. } => out.push_str(payload),
                    PlanBlock::List { items, .. } => {
                        for item in items {
                            collect(item, out);
                        }
                    }
                    PlanBlock::Quote(inner) => collect(inner, out),
                    _ => {}
                }
            }
        }
        let mut all = String::new();
        collect(&blocks, &mut all);
        assert!(
            all.contains("what you had in mind"),
            "plan lost the tail; got …{:?}",
            &all[all.len().saturating_sub(120)..]
        );

        // Every style/font/link range must lie inside its text and on char
        // boundaries — an out-of-range or mid-char range makes gpui's text
        // layout under-measure the block (rows overlap, scroll comes short).
        fn check_ranges(blocks: &[PlanBlock]) {
            for b in blocks {
                match b {
                    PlanBlock::Text { text, .. } => {
                        let t = &text.text;
                        for (r, _) in &text.highlights {
                            assert!(r.end <= t.len(), "highlight {r:?} beyond len {}", t.len());
                            assert!(
                                t.is_char_boundary(r.start) && t.is_char_boundary(r.end),
                                "highlight {r:?} splits a char"
                            );
                        }
                        for (r, _) in &text.fonts {
                            assert!(
                                r.end <= t.len()
                                    && t.is_char_boundary(r.start)
                                    && t.is_char_boundary(r.end),
                                "font range {r:?} invalid"
                            );
                        }
                        for r in &text.link_ranges {
                            assert!(
                                r.end <= t.len()
                                    && t.is_char_boundary(r.start)
                                    && t.is_char_boundary(r.end),
                                "link range {r:?} invalid"
                            );
                        }
                    }
                    PlanBlock::List { items, .. } => {
                        for item in items {
                            check_ranges(item);
                        }
                    }
                    PlanBlock::Quote(inner) => check_ranges(inner),
                    _ => {}
                }
            }
        }
        check_ranges(&blocks);
    }
}
