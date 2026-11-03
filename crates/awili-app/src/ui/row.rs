//! `ui::Row` — the kit's list row (KIT.md v3, batch K2).
//!
//! One line of `type_::UI` `TEXT` plus an optional `type_::LABEL` `TEXT_DIM`
//! subtitle (two stacked lines), an optional leading element and an
//! optional dim `type_::LABEL` value at the right — on a `RADIUS_M`
//! rectangle, 12 px horizontal and 8 px vertical padding, the `target()`
//! minimum height. Hover is the `FILL_HOVER` wash; the selected row is
//! `RAISE` (the hover adds no fill when the row is selected), and the
//! selected state is the parent's (a stateful row is a future batch).

use gpui::{
    div, prelude::*, AnyElement, App, ClickEvent, ElementId, IntoElement, RenderOnce, Rgba,
    SharedString, Window,
};
use std::ops::Range;

use crate::icons::{self, Icon};
use crate::theme::{self, palette};
use crate::ui::focus_ring::{focus_ring, focus_ring_style};
use crate::ui::interaction::ROW;
use crate::ui::interaction::{focus_wiring, ClickHandler, HOVER_GROUP};
use crate::ui::text;

/// A kit list row. Build with [`Row::new`] and chain the face options and
/// the shared conventions; the parent owns the click, hover and selection
/// state and re-renders the row.
#[derive(IntoElement)]
pub struct Row {
    common: crate::ui::interaction::Common,
    title: Option<SharedString>,
    title_selector: Option<SharedString>,
    title_color_override: Option<Rgba>,
    title_highlight_ranges: Vec<Range<usize>>,
    subtitle: Option<SharedString>,
    leading: Option<AnyElement>,
    leading_icon: Option<Icon>,
    trailing: Option<AnyElement>,
    wrap_trailing: bool,
    selected: bool,
    wrap: bool,
    strong: bool,
    on_click: Option<ClickHandler>,
}

impl Row {
    /// An empty row; set its `.title(…)` (also the accessible name).
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            title: None,
            title_selector: None,
            title_color_override: None,
            title_highlight_ranges: Vec::new(),
            subtitle: None,
            leading: None,
            leading_icon: None,
            trailing: None,
            wrap_trailing: false,
            selected: false,
            wrap: false,
            strong: false,
            on_click: None,
        }
    }

    /// The `type_::UI` title line (also the accessible name).
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self.common.label = self.title.clone();
        self
    }

    /// Preserve a screen's measured title selector on the actual title
    /// Text slot; the row id and long-title tooltip remain unchanged.
    pub fn title_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.title_selector = Some(selector.into());
        self
    }

    /// Override an enabled title with a colour from the palette. Disabled
    /// rows keep the kit's `TEXT_DIM` title colour.
    pub fn title_color_override(mut self, color: Rgba) -> Self {
        self.title_color_override = Some(color);
        self
    }

    /// Highlight caller-selected UTF-8 byte ranges in the title. The shared
    /// Text slot validates the ranges against the final title before painting.
    pub fn title_highlight_ranges(
        mut self,
        ranges: impl IntoIterator<Item = Range<usize>>,
    ) -> Self {
        self.title_highlight_ranges = ranges.into_iter().collect();
        self
    }

    /// A weight-600 title (the mockup's Job History steps, `.step .tt`).
    pub fn strong(mut self) -> Self {
        self.strong = true;
        self
    }

    /// Whether the title is weight 600.
    pub fn is_strong(&self) -> bool {
        self.strong
    }

    /// The title slot as painted: `type_::UI` in the title colour, weight 600
    /// when `.strong()`. It carries the row's title id (KIT.md § Row and
    /// § Long text: a truncating slot with an id shows the full text in its
    /// tooltip) unless the row is disabled, so no child tooltip covers the
    /// reason; `.wrap()` opts it into wrapping.
    pub(crate) fn title_part(&self) -> text::Text {
        let mut title = text::ui_text(self.title.clone().unwrap_or_default());
        if self.common.is_disabled() || self.title_color_override.is_some() {
            title = title.text_color_override(self.paint_title_color());
        }
        title = title.highlight_ranges(self.title_highlight_ranges.clone());
        if !self.common.is_disabled() {
            title = title.id(self.title_id());
        }
        if self.strong {
            title = title.strong();
        }
        if self.wrap {
            title = title.wrap();
        }
        if let Some(selector) = &self.title_selector {
            title = title.selector(selector.clone());
        }
        title
    }

    /// The title, if any.
    pub fn title_text(&self) -> Option<SharedString> {
        self.title.clone()
    }

    /// A `TEXT_DIM` second line under the title.
    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// The subtitle, if any.
    pub fn subtitle_text(&self) -> Option<SharedString> {
        self.subtitle.clone()
    }

    /// Opt the title/subtitle slots into wrapping (KIT.md § Row: the slots
    /// truncate with "…" unless `.wrap()`; the slots carry their ids, so a
    /// truncating slot shows the full text in its tooltip).
    pub fn wrap(mut self) -> Self {
        self.wrap = true;
        self
    }

    /// Whether the title/subtitle slots wrap.
    pub fn wraps(&self) -> bool {
        self.wrap
    }

    /// The `ElementId` of the title slot: `<row>-title` (the truncating
    /// `ui::text` shows the full title in its tooltip).
    pub fn title_id(&self) -> ElementId {
        crate::ui::interaction::child_id(&self.common.id, "title")
    }

    /// The `ElementId` of the subtitle slot: `<row>-subtitle`.
    pub fn subtitle_id(&self) -> ElementId {
        crate::ui::interaction::child_id(&self.common.id, "subtitle")
    }

    /// A leading `ICON` icon, tinted with the title colour (`TEXT_DIM`
    /// when disabled — dimmed, never deleted).
    pub fn icon(mut self, icon: Icon) -> Self {
        self.leading_icon = Some(icon);
        self.leading = None;
        self
    }

    /// A leading element of the parent's choosing (an avatar, a flag …).
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self.leading_icon = None;
        self
    }

    /// Whether the row shows a leading element.
    pub fn has_leading(&self) -> bool {
        self.leading.is_some() || self.leading_icon.is_some()
    }

    /// An element at the right end (a shortcut, a Dot, a chevron or a
    /// count — usually a dim `type_::LABEL` value).
    pub fn trailing(mut self, trailing: impl IntoElement) -> Self {
        self.trailing = Some(trailing.into_any_element());
        self
    }

    /// Let a long trailing control group wrap onto its own line in narrow
    /// rows. The default content-sized trailing slot is unchanged.
    pub fn wrap_trailing(mut self) -> Self {
        self.wrap_trailing = true;
        self
    }

    /// Whether the row shows a trailing element.
    pub fn has_trailing(&self) -> bool {
        self.trailing.is_some()
    }

    /// The selected state (painted `RAISE`; the hover adds no fill).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Whether the row is painted selected.
    pub fn is_selected(&self) -> bool {
        self.selected
    }

    /// The click handler; the kit's double-click guard applies. Enter/Space
    /// on the focused row fire it too.
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    /// The row's paint: the fill — `RAISE` when selected (hover adds
    /// nothing), `FILL_HOVER` on hover, none otherwise; disabled paints
    /// the dim text and no fill.
    pub fn paint_fill(&self) -> Rgba {
        let p = palette();
        if self.common.is_disabled() {
            return p.transparent;
        }
        if self.selected {
            return p.raise;
        }
        if self.common.hovered {
            return p.fill_hover;
        }
        p.transparent
    }

    /// The title's colour: the caller override or `TEXT`, unless disabled,
    /// when `TEXT_DIM` always wins.
    pub fn paint_title_color(&self) -> Rgba {
        let p = palette();
        if self.common.is_disabled() {
            p.text_dim
        } else {
            self.title_color_override.unwrap_or(p.text)
        }
    }

    /// The subtitle's colour: `TEXT_DIM` at rest, `TEXT_2` when the row is
    /// hovered (the parent's `.hovered(…)` state) or selected; disabled
    /// stays `TEXT_DIM`.
    pub fn paint_subtitle_color(&self) -> Rgba {
        let p = palette();
        if self.common.is_disabled() {
            p.text_dim
        } else if self.selected || self.common.hovered {
            p.text_2
        } else {
            p.text_dim
        }
    }
}

crate::kit_shared!(Row);

impl std::fmt::Debug for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Row({})", self.common.selector_for(ROW))
    }
}

/// A slot wrapper's id: a different id when disabled, so GPUI drops the
/// slot children's element state on the switch — an already-open child
/// tooltip cannot survive the row becoming disabled.
fn slot_id(row: &ElementId, tag: &str, disabled: bool) -> ElementId {
    crate::ui::interaction::child_id(
        row,
        &format!("{tag}-{}", if disabled { "off" } else { "on" }),
    )
}

/// A caller-supplied slot (leading or trailing), sized by its content
/// (KIT.md § Row). The same wrapper in both states, so disabling never
/// shifts the layout; disabled, it keeps its space but is not drawn — the
/// row cannot disable an arbitrary child's clicks, tooltip or tab stop, so
/// it does not present it.
fn slot(el: AnyElement, disabled: bool, id: ElementId, constrain: bool) -> AnyElement {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .when(constrain, |d| d.min_w_0().max_w_full())
        .when(disabled, |d| d.invisible())
        .child(el)
        .into_any_element()
}

impl RenderOnce for Row {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let p = palette();
        let fill = self.paint_fill();
        let title_color = self.paint_title_color();
        let subtitle_color = self.paint_subtitle_color();
        let disabled = self.common.is_disabled();
        let selected = self.selected;
        // The title slot and the subtitle slot id (named children of the
        // row's id) are built before the common state moves.
        let title = self.title_part();
        let row_id = self.common.id.clone();
        let subtitle_id = self.subtitle_id();

        let common = self.common;
        let on_click = self.on_click;
        let leading = self.leading;
        let leading_icon = self.leading_icon;
        let subtitle = self.subtitle;
        let trailing = self.trailing;
        let wrap_trailing = self.wrap_trailing;
        let wrap = self.wrap;
        let selector = common.selector_for(ROW);
        let focused = common.frozen_focus();
        let allow_double = common.allow_double_click;
        let tip = common.tooltip();

        // The subtitle slot carries the row's id like the title (see
        // `title_part`); `.wrap()` opts it into wrapping too.
        let texts = div()
            .flex()
            .flex_col()
            .justify_center()
            .flex_grow(1.)
            .min_w_0()
            .debug_selector(|| format!("{selector}-texts"))
            .child(title)
            .when_some(subtitle, |d, subtitle| {
                let mut sub = text::label(subtitle).text_color_override(subtitle_color);
                if !disabled {
                    // The brighten rule (KIT.md § Tokens): `TEXT_DIM` at
                    // rest, `TEXT_2` under the real pointer hover.
                    sub = sub.id(subtitle_id).hover_color(p.text_2);
                }
                if wrap {
                    sub = sub.wrap();
                }
                d.child(sub)
            });

        let mut root = div()
            .id(common.id.clone())
            .flex()
            .when(wrap_trailing, |d| d.flex_wrap())
            .w_full()
            .items_center()
            .gap(theme::z(theme::SP_12))
            // The mockup's row padding: 8 px vertical / 12 px horizontal
            // (the owner-approved decision; KIT.md § Row).
            .px(theme::z(theme::SP_12))
            .py(theme::z(theme::SP_8))
            .min_h(theme::target())
            .min_w(theme::target())
            .rounded(theme::z(theme::RADIUS_M))
            .bg(fill)
            .debug_selector(|| selector.clone())
            // Disabled dims, it never deletes: the leading icon takes the
            // title colour (`TEXT_DIM` when disabled). The parent's own
            // leading and trailing elements may be interactive, and a part
            // cannot switch off a child's clicks or tab stop, so a disabled
            // row keeps their space but does not draw them (`invisible()`:
            // no paint, no listeners, no tab stop) — the row's reason is the
            // only tooltip and nothing in it can be activated.
            .when_some(leading_icon, |d, which| {
                d.child(icons::icon(which, theme::ICON, title_color))
            })
            .children(
                leading.map(|el| slot(el, disabled, slot_id(&row_id, "leading", disabled), false)),
            )
            .child(texts)
            .children(trailing.map(|el| {
                slot(
                    el,
                    disabled,
                    slot_id(&row_id, "trailing", disabled),
                    wrap_trailing,
                )
            }));

        // Real pointer hover: the enabled row is the hover group (its
        // subtitle lifts, above); an unselected row washes `FILL_HOVER` —
        // a selected row's `RAISE` fill gains nothing on hover (KIT.md §
        // Row: "the hover adds no fill").
        if !disabled {
            root = root.group(HOVER_GROUP);
            if !selected {
                root = root.hover(move |s| s.bg(p.fill_hover));
            }
        }

        if !disabled {
            root = focus_wiring(root, &common)
                .when(!common.focused, |d| d.focus_visible(focus_ring_style));
            if let Some(on_click) = on_click {
                root = root.on_click(move |ev, window, cx| {
                    if allow_double || ev.click_count() <= 1 {
                        on_click(ev, window, cx);
                    }
                });
            }
        }
        if let Some(tip) = tip {
            root = root.tooltip(icons::tip(tip.to_string()));
        }

        focus_ring(root, focused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, TestAppContext};

    use crate::ui::interaction::child_id;
    use crate::ui::interaction::test_util::render_part;

    /// `.strong()` paints the title slot in weight 600; a plain row's title
    /// stays regular (the render path paints `title_part`).
    #[test]
    fn strong_title_is_weight_600() {
        assert!(Row::new("r")
            .title("Swap")
            .strong()
            .title_part()
            .is_strong());
        assert!(!Row::new("r").title("Swap").title_part().is_strong());
    }

    /// The hit box: at every tested zoom the row is at least 44 px tall
    /// and fills its parent's width.
    #[gpui::test]
    fn row_hit_box_is_at_least_target_at_every_zoom(cx: &mut TestAppContext) {
        for scale in [0.5_f32, 1.0, 1.25] {
            theme::set_ui_scale(scale);
            let (_root, wcx) = render_part(cx, move || {
                div()
                    .flex()
                    .flex_col()
                    .child(
                        Row::new("r1")
                            .title("Claude Code")
                            .title_highlight_ranges([0..1, 7..11])
                            .icon(Icon::ArrowUp)
                            .trailing(text::caption("14:32")),
                    )
                    .child(
                        Row::new("r2")
                            .title("A conversation")
                            .subtitle("12 messages · today"),
                    )
            });
            for sel in ["ui-row-r1", "ui-row-r2"] {
                let b = wcx
                    .debug_bounds(sel)
                    .unwrap_or_else(|| panic!("{sel} recorded no bounds at zoom {scale}"));
                assert!(
                    f32::from(b.size.height) >= 44.0,
                    "{sel}: the row is ≥ 44 px tall at zoom {scale}, got {b:?}",
                );
            }
        }
        theme::set_ui_scale(1.0);
    }

    /// An opt-in trailing control group wraps inside a narrow row, keeping
    /// each actual hit face within the row's painted bounds.
    #[gpui::test]
    fn row_wrapped_trailing_controls_stay_within_narrow_row(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, || {
            div().w(px(250.)).child(
                Row::new("wrap")
                    .title("Connector access")
                    .subtitle("Enabled for Hermes")
                    .wrap_trailing()
                    .trailing(
                        div()
                            .flex()
                            .w_full()
                            .max_w_full()
                            .flex_row()
                            .flex_wrap()
                            .gap(theme::z(theme::SP_4))
                            .child(crate::ui::Button::new("wrap-a", "Enabled"))
                            .child(
                                crate::ui::Button::new(
                                    "wrap-b",
                                    "A deliberately long action label",
                                )
                                .full_width(),
                            ),
                    ),
            )
        });
        let row = wcx.debug_bounds("ui-row-wrap").unwrap();
        for selector in ["ui-button-wrap-a", "ui-button-wrap-b"] {
            let face = wcx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} has painted bounds"));
            assert!(
                face.left() >= row.left(),
                "{selector} left is in row: {face:?} / {row:?}"
            );
            assert!(
                face.right() <= row.right(),
                "{selector} right is in row: {face:?} / {row:?}"
            );
        }
    }
    /// The fill table: selected is RAISE (the hover adds nothing), hover
    /// is FILL_HOVER, otherwise none; the disabled row paints no fill.
    #[test]
    fn row_fill_table() {
        let p = palette();
        let row = Row::new("r").title("x");
        assert_eq!(row.paint_fill(), p.transparent);
        let row = Row::new("r").title("x");
        assert_eq!(row.hovered(true).paint_fill(), p.fill_hover);
        let row = Row::new("r").title("x");
        assert_eq!(row.selected(true).paint_fill(), p.raise);
        assert_eq!(
            Row::new("r")
                .title("x")
                .selected(true)
                .hovered(true)
                .paint_fill(),
            p.raise,
            "the selected fill wins over the hover"
        );
        assert_eq!(
            Row::new("r")
                .title("x")
                .disabled("Read only")
                .hovered(true)
                .paint_fill(),
            p.transparent
        );
    }

    #[test]
    fn title_color_override_applies_to_enabled_highlights_but_disabled_wins() {
        let p = palette();
        let muted = p.text_dim;
        let overridden = Row::new("muted")
            .title("Archived")
            .title_color_override(muted)
            .title_highlight_ranges(std::iter::once(0..2));
        assert_eq!(overridden.paint_title_color(), muted);
        assert_eq!(overridden.title_part().color(), muted);
        assert_eq!(overridden.title_part().highlighted_ranges(), vec![0..2]);

        let disabled = Row::new("disabled")
            .title("Archived")
            .title_color_override(p.text)
            .disabled("Unavailable");
        assert_eq!(disabled.paint_title_color(), p.text_dim);
        assert_eq!(disabled.title_part().color(), p.text_dim);

        assert_eq!(
            Row::new("plain").title("Active").paint_title_color(),
            p.text
        );
    }

    /// A click on the row fires the handler once (the double-click guard
    /// holds), and a disabled row fires nothing.
    #[gpui::test]
    fn row_click_fires_once_and_disabled_is_inert(cx: &mut TestAppContext) {
        let fired = std::rc::Rc::new(std::cell::Cell::new(0));
        let fired_in = fired.clone();
        let dead = std::rc::Rc::new(std::cell::Cell::new(0));
        let dead_in = dead.clone();
        let (_root, wcx) =
            render_part(cx, move || {
                let fired_in = fired_in.clone();
                let dead_in = dead_in.clone();

                div()
                    .flex()
                    .flex_col()
                    .child(Row::new("click").title("Open the report").on_click(
                        move |_ev, _w, _cx| {
                            fired_in.set(fired_in.get() + 1);
                        },
                    ))
                    .child(
                        Row::new("dead")
                            .title("Archive")
                            .disabled("The report is locked")
                            .on_click(move |_ev, _w, _cx| {
                                dead_in.set(1);
                            }),
                    )
            });
        let row = wcx
            .debug_bounds("ui-row-click")
            .unwrap_or_else(|| panic!("the row recorded no bounds"));
        let at = row.center();
        for clicks in [1usize, 2] {
            wcx.simulate_event(gpui::MouseDownEvent {
                button: gpui::MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::default(),
                click_count: clicks,
                first_mouse: false,
            });
            wcx.simulate_event(gpui::MouseUpEvent {
                button: gpui::MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::default(),
                click_count: clicks,
            });
        }
        assert_eq!(fired.get(), 1, "the double click fires the row once");

        let dead_row = wcx
            .debug_bounds("ui-row-dead")
            .unwrap_or_else(|| panic!("the disabled row recorded no bounds"));
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: dead_row.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: dead_row.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert_eq!(dead.get(), 0, "a disabled row is not clickable");
    }

    /// The title/subtitle slots carry the row's ids — `<row>-title` /
    /// `<row>-subtitle` (KIT.md § Row: a truncating slot with an id shows
    /// the full text in its tooltip).
    #[test]
    fn row_text_slots_carry_the_row_ids() {
        let r = Row::new("r9").title("x").subtitle("y");
        assert_eq!(
            r.title_id(),
            ElementId::NamedChild(
                std::sync::Arc::new(ElementId::Name("r9".into())),
                "title".into()
            ),
        );
        assert_eq!(
            r.subtitle_id(),
            ElementId::NamedChild(
                std::sync::Arc::new(ElementId::Name("r9".into())),
                "subtitle".into(),
            ),
        );
        assert_ne!(
            Row::new("a").title_id(),
            Row::new("b").title_id(),
            "the ids are unique under the row's id"
        );
    }

    /// A compatibility selector measures the actual title text while the
    /// default id-derived selector and title tooltip remain available.
    #[gpui::test]
    fn row_title_selector_targets_text_without_changing_tooltip(cx: &mut TestAppContext) {
        const TITLE: &str = "A measured conversation title that may be truncated";
        let (_root, wcx) = render_part(cx, || {
            div().flex().flex_col().child(
                Row::new("compat")
                    .title(TITLE)
                    .title_selector("legacy-title")
                    .subtitle("opening turn"),
            )
        });
        wcx.run_until_parked();
        let text = wcx.debug_bounds("legacy-title").expect("actual title text");
        let row = wcx.debug_bounds("ui-row-compat").expect("row face");
        assert!(
            text.size.height < row.size.height,
            "selector is the title slot: {text:?} / {row:?}"
        );
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at(wcx, "legacy-title").as_deref(),
            Some(TITLE),
            "the override leaves the id-owned tooltip intact"
        );

        let (_root, default_wcx) = render_part(cx, || {
            div().child(Row::new("default-title").title("Default selector"))
        });
        assert!(
            default_wcx
                .debug_bounds("ui-text-default-title-title")
                .is_some(),
            "rows without an override keep their id-derived title selector"
        );
    }

    #[test]
    fn row_title_passes_valid_ranges_and_leaves_plain_titles_unchanged() {
        let matches = vec![0..1, 3..5, 6..10];
        let matched = Row::new("r")
            .title("café 🧭")
            .title_highlight_ranges(matches.clone());
        assert_eq!(matched.title_part().highlighted_ranges(), matches);
        assert_eq!(
            matched.title_part().tooltip_text().as_ref(),
            "café 🧭",
            "highlighting keeps the full title available for its tooltip"
        );

        let ordinary = Row::new("r").title("café 🧭");
        assert!(ordinary.title_part().highlighted_ranges().is_empty());
        assert_eq!(ordinary.title_text().unwrap().as_ref(), "café 🧭");

        let malformed = Row::new("r")
            .title("café 🧭")
            .title_highlight_ranges(std::iter::once(3..4));
        assert!(malformed.title_part().highlighted_ranges().is_empty());
    }

    /// The slots wrap when `.wrap()` is set (and truncate otherwise).
    #[test]
    fn row_wrap_opts_the_slots_into_wrapping() {
        assert!(!Row::new("r").title("x").wraps());
        assert!(Row::new("r").title("x").wrap().wraps());
    }

    /// The subtitle colour rule: `TEXT_DIM` at rest, `TEXT_2` when the row
    /// is hovered or selected, `TEXT_DIM` again when disabled.
    #[test]
    fn row_subtitle_color_table() {
        let p = palette();
        let row = Row::new("r").title("x").subtitle("y");
        assert_eq!(row.paint_subtitle_color(), p.text_dim, "rest is TEXT_DIM");
        assert_eq!(
            Row::new("r")
                .title("x")
                .subtitle("y")
                .hovered(true)
                .paint_subtitle_color(),
            p.text_2,
            "hovered is TEXT_2"
        );
        assert_eq!(
            Row::new("r")
                .title("x")
                .subtitle("y")
                .selected(true)
                .paint_subtitle_color(),
            p.text_2,
            "selected is TEXT_2"
        );
        assert_eq!(
            Row::new("r")
                .title("x")
                .subtitle("y")
                .disabled("Read only")
                .hovered(true)
                .paint_subtitle_color(),
            p.text_dim,
            "disabled stays TEXT_DIM"
        );
    }

    /// The mockup padding, pinned on the render: 8 px vertical and 12 px
    /// horizontal (zoom 1), the 44 px minimum height.
    #[gpui::test]
    fn row_padding_follows_the_mockup(cx: &mut TestAppContext) {
        // zoom 1: z() is the identity.
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .w(px(400.))
                .child(Row::new("pad1").title("Title"))
                .child(Row::new("pad2").title("Title").subtitle("Subtitle"))
        });
        let single = wcx
            .debug_bounds("ui-row-pad1")
            .unwrap_or_else(|| panic!("the single-line row recorded no bounds"));
        assert_eq!(
            f32::from(single.size.height),
            f32::from(theme::target()),
            "content (20 + 2×8 = 36) is under the minimum: the row is target() tall"
        );
        let double = wcx
            .debug_bounds("ui-row-pad2")
            .unwrap_or_else(|| panic!("the two-line row recorded no bounds"));
        let expected_height = f32::from(
            theme::z(theme::type_::UI.line)
                + theme::z(theme::type_::LABEL.line)
                + 2.0 * theme::z(theme::SP_8),
        );
        assert_eq!(
            f32::from(double.size.height),
            expected_height,
            "two lines + 2 × SP_8 vertical padding: the padding is the mockup's 8 px"
        );
        // Horizontal: the texts block starts at the row's left edge + SP_12
        // and ends SP_12 before the right edge (no leading/trailing here).
        let texts = wcx
            .debug_bounds("ui-row-pad1-texts")
            .unwrap_or_else(|| panic!("the texts block recorded no bounds"));
        assert_eq!(
            f32::from(texts.origin.x) - f32::from(single.origin.x),
            f32::from(theme::z(theme::SP_12)),
            "the texts block starts SP_12 in from the row's left edge"
        );
        assert_eq!(
            f32::from(single.size.width)
                - f32::from(texts.size.width)
                - (f32::from(texts.origin.x) - f32::from(single.origin.x)),
            f32::from(theme::z(theme::SP_12)),
            "…and ends SP_12 before the right edge"
        );
    }

    /// Real GPUI hover: a pointer move into the enabled, unselected row
    /// re-renders (the row's `.group()`/`.hover()` wiring notifies the
    /// view); the disabled row and the selected row (the hover adds no
    /// fill) have no hover wiring and do not re-render.
    #[gpui::test]
    fn row_hover_renders_on_pointer_move(cx: &mut TestAppContext) {
        for (disabled, selected, should_re_render) in [
            (false, false, true),
            (true, false, false),
            (false, true, false),
        ] {
            let calls = std::rc::Rc::new(std::cell::Cell::new(0u32));
            let calls_in = calls.clone();
            let (_root, wcx) = render_part(cx, move || {
                let calls_in = calls_in.clone();
                calls_in.set(calls_in.get() + 1);
                let mut r = Row::new("hovr").title("Hover me");
                if selected {
                    r = r.selected(true);
                }
                if disabled {
                    r = r.disabled("Read only");
                }
                div().flex().child(r)
            });
            let before = calls.get();
            let at = wcx
                .debug_bounds("ui-row-hovr")
                .unwrap_or_else(|| panic!("the row recorded no bounds"))
                .center();
            wcx.simulate_event(gpui::MouseMoveEvent {
                position: at,
                pressed_button: None,
                modifiers: gpui::Modifiers::default(),
            });
            wcx.run_until_parked();
            assert_eq!(
                calls.get() > before,
                should_re_render,
                "disabled={disabled} selected={selected}: the factory re-ran on \
                 pointer entry iff the row carries the real hover wiring \
                 (calls: {before} → {})",
                calls.get()
            );
        }
    }

    /// The brighten rule under real pointer hover (K2c): the subtitle GPUI
    /// paints goes `TEXT_DIM` → `TEXT_2` while the pointer is on the row
    /// and back on leave (read from the paint probe); a selected row's
    /// subtitle is `TEXT_2` at rest.
    #[gpui::test]
    fn row_subtitle_brightens_under_real_hover(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::{hover, painted, same_color, unhover};
        let p = palette();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .w(px(320.))
                .pt(theme::z(theme::SP_40))
                .child(
                    Row::new("h")
                        .title("A conversation")
                        .subtitle("12 messages"),
                )
                .child(
                    Row::new("s")
                        .title("Selected")
                        .subtitle("3 messages")
                        .selected(true),
                )
        });
        let sub = child_id(&ElementId::from("h"), "subtitle");
        unhover(wcx);
        assert!(
            same_color(painted(&sub).unwrap(), p.text_dim),
            "rest is TEXT_DIM"
        );
        hover(wcx, "ui-row-h");
        assert!(
            same_color(painted(&sub).unwrap(), p.text_2),
            "hover lifts to TEXT_2"
        );
        unhover(wcx);
        assert!(
            same_color(painted(&sub).unwrap(), p.text_dim),
            "leave drops back"
        );
        let sel = child_id(&ElementId::from("s"), "subtitle");
        assert!(
            same_color(painted(&sel).unwrap(), p.text_2),
            "a selected row's subtitle is TEXT_2"
        );
    }

    /// A disabled row: the tooltip that opens over its (truncating) title
    /// is the reason — the slots carry no tooltip of their own — and the
    /// leading icon is still laid out (dimmed, never deleted).
    #[gpui::test]
    fn disabled_row_opens_its_reason(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::tooltip_at;
        let (_root, wcx) = render_part(cx, move || {
            div().flex().w(px(320.)).pt(theme::z(theme::SP_40)).child(
                Row::new("d")
                    .title("A conversation with a very long title that truncates")
                    .subtitle("12 messages")
                    .icon(Icon::ArrowUp)
                    .disabled("Archived"),
            )
        });
        assert_eq!(
            tooltip_at(wcx, "ui-row-d-texts").as_deref(),
            Some("Archived")
        );
        let texts = wcx.debug_bounds("ui-row-d-texts").unwrap();
        let row = wcx.debug_bounds("ui-row-d").unwrap();
        assert!(
            texts.origin.x > row.origin.x + theme::z(theme::SP_12),
            "the leading icon still takes its place: row {row:?}, texts {texts:?}"
        );
    }

    /// The hit box holds in a squeezed parent: a row never lays out
    /// narrower than `target()`.
    #[gpui::test]
    fn row_min_width_is_the_target(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().w(px(10.)).child(Row::new("n").title("Narrow"))
        });
        let b = wcx.debug_bounds("ui-row-n").unwrap();
        assert!(f32::from(b.size.width) >= 44.0, "{b:?}");
    }

    /// Tab reaches a row tracking a plain parent handle (K2c).
    #[gpui::test]
    fn tracked_handle_is_a_tab_stop(cx: &mut TestAppContext) {
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .child(Row::new("t").title("Row").focus_handle(&h))
        });
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert_eq!(wcx.update(|w, cx| w.focused(cx)), Some(handle));
    }

    /// A disabled row presents nothing that can be activated (K2c round
    /// 3): its caller-supplied trailing IconButton keeps its space but is
    /// not drawn — a click on it fires nothing, Tab never reaches its
    /// handle, and the only tooltip there is the row's reason.
    #[gpui::test]
    fn disabled_row_slots_are_inert(cx: &mut TestAppContext) {
        use crate::ui::icon_button::IconButton;
        use crate::ui::interaction::test_util::tooltip_at_point;
        let fired = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let fired_in = fired.clone();
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            let fired_in = fired_in.clone();
            div().flex().w(px(320.)).pt(theme::z(theme::SP_40)).child(
                Row::new("d")
                    .title("Archived chat")
                    .trailing(
                        IconButton::new("more", Icon::X)
                            .label("More")
                            .focus_handle(&h)
                            .on_click(move |_e, _w, _cx| fired_in.set(fired_in.get() + 1)),
                    )
                    .disabled("Archived"),
            )
        });
        let row = wcx.debug_bounds("ui-row-d").unwrap();
        let at = gpui::point(
            row.origin.x + row.size.width - theme::z(theme::SP_12) - theme::target() / 2.,
            row.center().y,
        );
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        wcx.run_until_parked();
        assert_eq!(fired.get(), 0, "the hidden trailing button fires nothing");
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            None,
            "Tab reaches nothing inside a disabled row"
        );
        assert_eq!(tooltip_at_point(wcx, at).as_deref(), Some("Archived"));
        assert!(
            row.contains(&at) && at.x > row.origin.x + theme::z(theme::SP_12) + theme::target(),
            "the probed point is the slot's kept space inside the row: {row:?} {at:?}"
        );
    }

    /// Disabling a row never shifts its layout: the caller slots sit in
    /// the same content-sized wrapper either way, so the texts land in the
    /// same place.
    #[gpui::test]
    fn disabling_a_row_keeps_its_layout(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .w(px(320.))
                .child(
                    Row::new("e")
                        .title("Chat")
                        .leading(text::caption("A"))
                        .trailing(text::caption("14:32")),
                )
                .child(
                    Row::new("x")
                        .title("Chat")
                        .leading(text::caption("A"))
                        .trailing(text::caption("14:32"))
                        .disabled("Archived"),
                )
        });
        let (e, x) = (
            wcx.debug_bounds("ui-row-e-texts").unwrap(),
            wcx.debug_bounds("ui-row-x-texts").unwrap(),
        );
        assert_eq!((e.origin.x, e.size), (x.origin.x, x.size), "{e:?} vs {x:?}");
    }

    /// The enabled → disabled switch (Codex round 4): a slot child's
    /// tooltip that is already open does not survive the row becoming
    /// disabled while the pointer stays on it.
    #[gpui::test]
    fn an_open_slot_tooltip_closes_when_the_row_disables(cx: &mut TestAppContext) {
        use crate::ui::icon_button::IconButton;
        let off = std::rc::Rc::new(std::cell::Cell::new(false));
        let off_in = off.clone();
        let (_root, wcx) = render_part(cx, move || {
            let mut row = Row::new("t")
                .title("Chat")
                .trailing(IconButton::new("more", Icon::X).label("More"));
            if off_in.get() {
                row = row.disabled("Archived");
            }
            div()
                .flex()
                .w(px(320.))
                .pt(theme::z(theme::SP_40))
                .child(row)
        });
        let at = wcx
            .debug_bounds("ui-icon-button-more")
            .unwrap_or_else(|| panic!("the trailing button recorded no bounds"))
            .center();
        let _ = crate::icons::take_shown_tip();
        wcx.simulate_event(gpui::MouseMoveEvent {
            position: at,
            pressed_button: None,
            modifiers: gpui::Modifiers::default(),
        });
        wcx.run_until_parked();
        wcx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        wcx.run_until_parked();
        assert_eq!(
            crate::icons::take_shown_tip().as_deref(),
            Some("More"),
            "the slot's tooltip is open while the row is enabled"
        );

        off.set(true);
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        let _ = crate::icons::take_shown_tip();
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        assert_ne!(
            crate::icons::take_shown_tip().as_deref(),
            Some("More"),
            "the slot's tooltip did not survive the row disabling"
        );
    }
}
