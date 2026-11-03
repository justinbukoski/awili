//! `ui::text` — the kit's styled text part (KIT.md v3, batch K1).
//!
//! One line of kit text: a `theme::type_` size, one colour from the palette,
//! optional tone / weight / mono / wrapping. Single-line text slots
//! truncate with "…" (the kit's long-text convention); `.wrap()` opts a slot
//! into wrapping.

use gpui::{
    div, prelude::*, AnyElement, App, ElementId, FontWeight, HighlightStyle, IntoElement,
    RenderOnce, Rgba, SharedString, Window,
};
use std::ops::Range;

use crate::theme::{self, type_, type_::TypeStyle, Tone};

/// A colour read from the palette — the kit's name for GPUI's colour value
/// (the parts never construct one themselves).
type PaletteColor = Rgba;

/// A styled line of kit text. Build with [`text`] (or a shortcut) and chain
/// the modifiers; the colour precedence is tone > contrast > reading > dim >
/// the palette body text.
#[derive(Clone, Debug, IntoElement)]
pub struct Text {
    content: SharedString,
    style: TypeStyle,
    dim: bool,
    reading: bool,
    contrast: bool,
    tone: Option<Tone>,
    strong: bool,
    mono: bool,
    wrap: bool,
    id: Option<ElementId>,
    selector: Option<SharedString>,
    color_override: Option<PaletteColor>,
    hover_color: Option<PaletteColor>,
    full_text: Option<SharedString>,
    highlight_ranges: Vec<Range<usize>>,
}

impl Text {
    /// The `TEXT_DIM` faint colour (placeholders, hints, timestamps).
    pub fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    /// The `TEXT_2` reading colour (long prose).
    pub fn reading(mut self) -> Self {
        self.reading = true;
        self
    }

    /// The `CONTRAST_TEXT` blue text colour (links, the "More · Code" lead).
    pub fn contrast(mut self) -> Self {
        self.contrast = true;
        self
    }

    /// A semantic tone: the tone's text token wins over every other colour.
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    /// Weight 600 (headings, titles, row labels).
    pub fn strong(mut self) -> Self {
        self.strong = true;
        self
    }

    /// The mono face (`theme::MONO_FONT`) — commands, code, file paths.
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// Opt this slot into wrapping; the default is the kit's single-line
    /// "…" truncation.
    pub fn wrap(mut self) -> Self {
        self.wrap = true;
        self
    }

    /// An element id. A single-line (truncating) slot with an id shows its full
    /// text as a tooltip, so truncated text is never lost (KIT.md § Long
    /// text). GPUI tooltips need a stateful element; parts pass a unique id.
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Override the debug selector used by rendered measurements while
    /// retaining this Text's element id and tooltip ownership.
    pub fn selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.selector = Some(selector.into());
        self
    }

    /// An explicit colour read from the palette (a kit part's face colour:
    /// the button's `ON_CONTRAST` on `primary`, the `CONTRAST_TEXT` on
    /// `lead`, …). It wins over the toneless modifiers below it — a
    /// semantic `.tone(…)` still wins over it.
    pub fn text_color_override(mut self, color: PaletteColor) -> Self {
        self.color_override = Some(color);
        self
    }

    /// The colour this text lifts to while the enclosing part's face is
    /// under the pointer (KIT.md § Tokens, the brighten rule: secondary
    /// text goes `TEXT_DIM` → `TEXT_2` on a hovered row). It resolves
    /// through the part's hover group (`interaction::HOVER_GROUP`), so the
    /// real pointer hover paints it.
    pub fn hover_color(mut self, color: PaletteColor) -> Self {
        self.hover_color = Some(color);
        self
    }

    /// The tooltip's text when it differs from the shown text — a chip
    /// that shows an abbreviated file name still tooltips the full name
    /// (KIT.md § Long text). Only used with `.id(…)`.
    pub fn full_text(mut self, full: impl Into<SharedString>) -> Self {
        self.full_text = Some(full.into());
        self
    }

    /// Highlight caller-selected UTF-8 byte ranges in this text. Ranges must
    /// be nonempty, in bounds, on character boundaries, sorted, and disjoint;
    /// an invalid set is ignored as a whole. The match foreground uses the
    /// shared semantic accent-text token for ordinary body text. Existing
    /// tone, contrast, reading, dim, and explicit color choices retain their
    /// foreground on matched runs.
    pub fn highlight_ranges(mut self, ranges: impl IntoIterator<Item = Range<usize>>) -> Self {
        self.highlight_ranges =
            validate_highlight_ranges(&self.content, ranges).unwrap_or_default();
        self
    }

    #[cfg(test)]
    pub(crate) fn highlighted_ranges(&self) -> &[Range<usize>] {
        &self.highlight_ranges
    }

    /// The tooltip this text shows: the full text, or its content.
    pub fn tooltip_text(&self) -> SharedString {
        self.full_text
            .clone()
            .unwrap_or_else(|| self.content.clone())
    }

    /// Whether this text is set to wrap.
    pub fn is_wrapping(&self) -> bool {
        self.wrap
    }

    /// The colour this text paints: the tone's text token, or an explicit
    /// palette-derived override, or `CONTRAST_TEXT`, or `TEXT_2` (reading),
    /// or `TEXT_DIM` (dim), or the palette body text.
    pub fn color(&self) -> PaletteColor {
        let p = theme::palette();
        if let Some(tone) = self.tone {
            return theme::tone_text(tone);
        }
        if let Some(color) = self.color_override {
            return color;
        }
        if self.contrast {
            return p.contrast_text;
        }
        if self.reading {
            return p.text_2;
        }
        if self.dim {
            return p.text_dim;
        }
        p.text
    }

    fn highlight_color(&self) -> PaletteColor {
        if self.tone.is_some()
            || self.color_override.is_some()
            || self.contrast
            || self.reading
            || self.dim
        {
            self.color()
        } else {
            theme::palette().accent_text
        }
    }

    /// The type size this text lays out at (before zoom).
    pub fn style(&self) -> TypeStyle {
        self.style
    }

    /// Whether this text is strong (weight 600).
    pub fn is_strong(&self) -> bool {
        self.strong
    }
}

impl RenderOnce for Text {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let color = self.color();
        let tip = self.tooltip_text();
        let probe = self
            .id
            .as_ref()
            .and_then(crate::ui::interaction::paint_probe);
        let content: AnyElement = if self.highlight_ranges.is_empty() {
            #[cfg(test)]
            {
                let drawn = gpui::StyledText::new(self.content.clone());
                DRAWN_TEXTS.with(|layouts| layouts.borrow_mut().push(drawn.layout().clone()));
                drawn.into_any_element()
            }
            #[cfg(not(test))]
            {
                self.content.clone().into_any_element()
            }
        } else {
            let highlight = HighlightStyle {
                color: Some(self.highlight_color().into()),
                ..Default::default()
            };
            let runs: Vec<(Range<usize>, HighlightStyle)> = self
                .highlight_ranges
                .iter()
                .cloned()
                .map(|range| (range, highlight))
                .collect();
            #[cfg(test)]
            DRAWN_HIGHLIGHT_RUNS.with(|drawn| drawn.borrow_mut().push(runs.clone()));
            let drawn = gpui::StyledText::new(self.content.clone()).with_highlights(runs);
            #[cfg(test)]
            DRAWN_TEXTS.with(|layouts| layouts.borrow_mut().push(drawn.layout().clone()));
            drawn.into_any_element()
        };
        let mut el = div()
            .text_size(theme::z(self.style.size))
            .line_height(theme::z(self.style.line))
            .text_color(color)
            .when(self.mono, |d| d.font_family(theme::MONO_FONT.to_string()))
            .when(self.strong, |d| d.font_weight(FontWeight::SEMIBOLD))
            // Long-text convention: single-line slots truncate with "…";
            // .wrap() opts the slot into wrapping. `min_w_0` lets a narrow
            // flex parent shrink the slot either way, so it truncates or
            // wraps instead of widening the part.
            .min_w_0()
            .when(self.wrap, |d| d.whitespace_normal())
            .when(!self.wrap, |d| d.truncate())
            .relative()
            .child(content)
            .children(probe);
        if let Some(hover) = self.hover_color {
            el = el.group_hover(crate::ui::interaction::HOVER_GROUP, move |s| {
                s.text_color(hover)
            });
        }
        // A text with an id answers to `ui-text-<id>` in tests (a part's
        // slot can then be found without hard-coding its offset).
        if let Some(selector) = self.selector.clone() {
            el = el.debug_selector(move || selector.to_string());
        } else if let Some(id) = self.id.clone() {
            el = el.debug_selector(move || format!("ui-text-{id}"));
        }
        let out: AnyElement = match (self.id, self.wrap) {
            (Some(id), false) => el.id(id).tooltip(crate::icons::tip(tip)).into_any_element(),
            _ => el.into_any_element(),
        };
        out
    }
}

#[cfg(test)]
type HighlightRun = (Range<usize>, HighlightStyle);

#[cfg(test)]
type HighlightRuns = Vec<HighlightRun>;

#[cfg(test)]
thread_local! {
    /// Layouts of actual kit text elements: tests read text and painted bounds.
    pub(crate) static DRAWN_TEXTS: std::cell::RefCell<Vec<gpui::TextLayout>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Exact highlight ranges and styles passed to rendered `StyledText` nodes.
    pub(crate) static DRAWN_HIGHLIGHT_RUNS: std::cell::RefCell<Vec<HighlightRuns>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The text of a drawn layout, or `None` when the layout was never measured.
/// A kit text under a `display: none` ancestor (a hidden row action, a collapsed
/// slot) is built every frame but GPUI never measures it, and reading such a
/// layout panics; a test that scans every drawn text asks through this.
#[cfg(test)]
pub(crate) fn measured_text(layout: &gpui::TextLayout) -> Option<String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| layout.text())).ok()
}

/// A line of kit text at a `theme::type_` size.
pub fn text(style: TypeStyle, content: impl Into<SharedString>) -> Text {
    Text {
        content: content.into(),
        style,
        dim: false,
        reading: false,
        contrast: false,
        tone: None,
        strong: false,
        mono: false,
        wrap: false,
        id: None,
        selector: None,
        color_override: None,
        hover_color: None,
        full_text: None,
        highlight_ranges: Vec::new(),
    }
}

fn validate_highlight_ranges(
    text: &str,
    ranges: impl IntoIterator<Item = Range<usize>>,
) -> Option<Vec<Range<usize>>> {
    let ranges: Vec<_> = ranges.into_iter().collect();
    let mut previous_end = 0;
    for (index, range) in ranges.iter().enumerate() {
        if range.start >= range.end
            || range.end > text.len()
            || !text.is_char_boundary(range.start)
            || !text.is_char_boundary(range.end)
            || (index > 0 && range.start < previous_end)
        {
            return None;
        }
        previous_end = range.end;
    }
    Some(ranges)
}

/// `TEXT_DIM`: metadata — timestamps, counts, hints.
pub fn caption(content: impl Into<SharedString>) -> Text {
    text(type_::CAPTION, content).dim()
}

/// Labels on controls (button faces, chip faces).
pub fn label(content: impl Into<SharedString>) -> Text {
    text(type_::LABEL, content)
}

/// Dense UI text — the default UI size.
pub fn ui_text(content: impl Into<SharedString>) -> Text {
    text(type_::UI, content)
}

/// Short prose (bubbles).
pub fn body(content: impl Into<SharedString>) -> Text {
    text(type_::BODY, content)
}

/// Long reading text, at the reading colour (`TEXT_2`).
pub fn read(content: impl Into<SharedString>) -> Text {
    text(type_::READ, content).reading()
}

/// Section headings — strong.
pub fn heading(content: impl Into<SharedString>) -> Text {
    text(type_::HEADING, content).strong()
}

/// Titles — strong.
pub fn title(content: impl Into<SharedString>) -> Text {
    text(type_::TITLE, content).strong()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use crate::ui::interaction::test_util::render_part;
    use gpui::TestAppContext;

    /// The colour precedence: tone > contrast > reading > dim > body text.
    #[test]
    fn text_color_precedence() {
        let p = theme::palette();
        assert_eq!(text(type_::UI, "x").color(), p.text);
        assert_eq!(text(type_::UI, "x").dim().color(), p.text_dim);
        assert_eq!(
            text(type_::UI, "x").reading().color(),
            p.text_2,
            "reading is the TEXT_2 reading colour"
        );
        assert_eq!(
            text(type_::UI, "x").contrast().color(),
            p.contrast_text,
            "contrast uses CONTRAST_TEXT, never the CONTRAST fill"
        );
        assert_eq!(
            text(type_::UI, "x").tone(Tone::ActNow).color(),
            p.act_now_text,
            "a tone wins over the plain modifiers"
        );
        assert_eq!(
            text(type_::UI, "x")
                .contrast()
                .tone(Tone::Attention)
                .color(),
            p.attention,
            "tone beats contrast"
        );
        assert_eq!(
            text(type_::UI, "x").dim().reading().color(),
            p.text_2,
            "reading beats dim"
        );
    }

    /// The shortcuts carry their type sizes, and heading / title are strong;
    /// `read` is the reading colour.
    #[test]
    fn shortcuts_carry_their_styles() {
        assert_eq!(caption("x").style(), type_::CAPTION);
        assert_eq!(label("x").style(), type_::LABEL);
        assert_eq!(ui_text("x").style(), type_::UI);
        assert_eq!(body("x").style(), type_::BODY);
        assert_eq!(read("x").style(), type_::READ);
        assert_eq!(heading("x").style(), type_::HEADING);
        assert_eq!(title("x").style(), type_::TITLE);

        assert!(
            heading("x").is_strong() && title("x").is_strong(),
            "heading and title are strong"
        );
        for t in [caption("x"), label("x"), ui_text("x"), body("x"), read("x")] {
            assert!(!t.is_strong(), "only heading and title are strong");
        }
        assert_eq!(read("x").color(), theme::palette().text_2);
        assert_eq!(caption("x").color(), theme::palette().text_dim);
    }

    #[test]
    fn caller_highlight_ranges_keep_discontinuous_unicode_matches() {
        let content = "Été 🧭";
        let ranges = vec![0..2, 6..10];
        let highlighted = ui_text(content).highlight_ranges(ranges.clone());
        assert_eq!(highlighted.highlighted_ranges(), ranges);
        assert_eq!(highlighted.color(), theme::palette().text);
        assert!(
            ui_text(content).highlighted_ranges().is_empty(),
            "ordinary text has no match ranges"
        );
    }

    #[test]
    fn malformed_highlight_ranges_are_ignored_as_a_set() {
        let content = "café";
        for ranges in [
            std::iter::once(3..4).collect::<Vec<_>>(), // inside the two-byte é
            vec![0..1, 0..2],                          // overlap
            vec![2..3, 0..1],                          // out of order
            std::iter::once(0..0).collect::<Vec<_>>(), // empty
            std::iter::once(0..99).collect::<Vec<_>>(), // out of bounds
        ] {
            assert!(
                ui_text(content)
                    .highlight_ranges(ranges)
                    .highlighted_ranges()
                    .is_empty(),
                "malformed ranges are not painted"
            );
        }
    }

    #[gpui::test]
    fn rendered_highlight_runs_keep_unicode_ranges_and_text_color_roles(cx: &mut TestAppContext) {
        let ranges = vec![0..2, 6..10];
        let rendered_ranges = ranges.clone();
        DRAWN_HIGHLIGHT_RUNS.with(|drawn| drawn.borrow_mut().clear());
        let p = theme::palette();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .child(ui_text("Été 🧭").highlight_ranges(rendered_ranges.clone()))
                .child(
                    ui_text("Été 🧭")
                        .dim()
                        .highlight_ranges(std::iter::once(0..2)),
                )
                .child(
                    ui_text("Été 🧭")
                        .reading()
                        .highlight_ranges(std::iter::once(0..2)),
                )
                .child(
                    ui_text("Été 🧭")
                        .contrast()
                        .highlight_ranges(std::iter::once(0..2)),
                )
                .child(
                    ui_text("Été 🧭")
                        .text_color_override(p.attention)
                        .highlight_ranges(std::iter::once(0..2)),
                )
                .child(
                    ui_text("Été 🧭")
                        .text_color_override(p.text_2)
                        .tone(Tone::Attention)
                        .highlight_ranges(std::iter::once(0..2)),
                )
                .child(
                    crate::ui::row::Row::new("normal")
                        .title("Été 🧭")
                        .title_highlight_ranges(rendered_ranges.clone()),
                )
                .child(
                    crate::ui::row::Row::new("disabled")
                        .title("Été 🧭")
                        .title_highlight_ranges(std::iter::once(0..2))
                        .disabled("Read only"),
                )
        });
        wcx.run_until_parked();

        let drawn = DRAWN_HIGHLIGHT_RUNS.with(|drawn| drawn.borrow().clone());
        let expected = [
            (ranges.clone(), p.accent_text),
            (std::iter::once(0..2).collect::<Vec<_>>(), p.text_dim),
            (std::iter::once(0..2).collect::<Vec<_>>(), p.text_2),
            (std::iter::once(0..2).collect::<Vec<_>>(), p.contrast_text),
            (std::iter::once(0..2).collect::<Vec<_>>(), p.attention),
            (
                std::iter::once(0..2).collect::<Vec<_>>(),
                theme::tone_text(Tone::Attention),
            ),
            (ranges, p.accent_text),
            (std::iter::once(0..2).collect::<Vec<_>>(), p.text_dim),
        ];
        assert_eq!(
            drawn.len(),
            expected.len(),
            "all eight highlighted Text slots rendered"
        );
        for (actual, (ranges, color)) in drawn.iter().zip(expected) {
            assert_eq!(actual.len(), ranges.len());
            for ((actual_range, style), expected_range) in actual.iter().zip(ranges) {
                assert_eq!(*actual_range, expected_range);
                assert_eq!(style.color, Some(color.into()));
            }
        }
    }

    /// All seven shortcuts render headless and take up real space (the kit
    /// parts must lay out, not just type-check).
    #[gpui::test]
    fn shortcuts_render_headless(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        cx.draw(
            gpui::point(gpui::Pixels::ZERO, gpui::Pixels::ZERO),
            gpui::size(
                gpui::AvailableSpace::Definite(theme::z(theme::PAGE_MAX)),
                gpui::AvailableSpace::Definite(theme::z(theme::RAIL_BUTTON)),
            ),
            |_w, _cx| {
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_4))
                    .child(
                        div()
                            .debug_selector(|| "caption".to_string())
                            .child(caption("caption")),
                    )
                    .child(
                        div()
                            .debug_selector(|| "label".to_string())
                            .child(label("label")),
                    )
                    .child(
                        div()
                            .debug_selector(|| "ui_text".to_string())
                            .child(ui_text("ui_text")),
                    )
                    .child(
                        div()
                            .debug_selector(|| "body".to_string())
                            .child(body("body")),
                    )
                    .child(
                        div()
                            .debug_selector(|| "read".to_string())
                            .child(read("read")),
                    )
                    .child(
                        div()
                            .debug_selector(|| "heading".to_string())
                            .child(heading("heading")),
                    )
                    .child(
                        div()
                            .debug_selector(|| "title".to_string())
                            .child(title("title")),
                    )
                    .child(
                        div()
                            .debug_selector(|| "highlighted_unicode".to_string())
                            .child(ui_text("Été 🧭").highlight_ranges([0..2, 6..10])),
                    )
            },
        );
        for key in [
            "caption",
            "label",
            "ui_text",
            "body",
            "read",
            "heading",
            "title",
            "highlighted_unicode",
        ] {
            let b = cx
                .debug_bounds(key)
                .unwrap_or_else(|| panic!("{key} recorded no bounds"));
            assert!(f32::from(b.size.height) > 0.0, "{key} has no height: {b:?}");
        }
    }

    /// `.wrap()` opts a slot into wrapping: the same long text is taller when
    /// wrapped into a narrow column than when truncated to one line.
    #[gpui::test]
    fn wrap_opts_into_wrapping(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let long = "one two three four five six seven eight nine ten eleven twelve";
        cx.draw(
            gpui::point(gpui::Pixels::ZERO, gpui::Pixels::ZERO),
            gpui::size(
                gpui::AvailableSpace::Definite(theme::z(theme::EDGE)),
                gpui::AvailableSpace::Definite(theme::z(theme::RAIL_BUTTON)),
            ),
            |_w, _cx| {
                div()
                    .flex()
                    .flex_col()
                    .w(theme::z(theme::EDGE))
                    .child(
                        div()
                            .debug_selector(|| "one".to_string())
                            .child(body(long).highlight_ranges(std::iter::once(0..3))),
                    )
                    .child(
                        div()
                            .debug_selector(|| "many".to_string())
                            .child(body(long).highlight_ranges(std::iter::once(0..3)).wrap()),
                    )
            },
        );
        let one = cx
            .debug_bounds("one")
            .unwrap_or_else(|| panic!("the single-line slot recorded no bounds"));
        let many = cx
            .debug_bounds("many")
            .unwrap_or_else(|| panic!("the wrapped slot recorded no bounds"));
        assert!(
            f32::from(many.size.height) > f32::from(one.size.height),
            "wrapped text ({many:?}) is taller than the truncated line ({one:?})"
        );
    }
}
