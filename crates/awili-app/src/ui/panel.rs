//! `ui::Panel` — the kit's floating panel (KIT.md v3, batch K3).
//!
//! The look matches the owner-approved mockup's `.panel`
//! (`A-studio.html` inspector): `RADIUS_L` corners, the translucent
//! `PANEL` fill, the kit's `float_shadow()`, a 1 px `HIGHLIGHT` top edge
//! (the mockup's `0 0 0 1px var(--highlight) inset`) and a 1 px
//! `CONTROL_LINE` edge. The header is the mockup's `.phead` (12 px dim
//! caption over a 19 px weight-600 heading, 20/8/12/4 px padding → the
//! spacing grid), the body the mockup's `.pbody` (16 px horizontal,
//! 12 px bottom padding on the spacing grid).
//!
//! **Compact windows:** below `theme::COMPACT_W` (880 px) the panel has
//! no room beside the canvas, so `ui::is_compact(window)` makes the part
//! paint the [`super::sheet::Sheet`] form automatically — full-width,
//! bottom-anchored, behind a `SCRIM`, closing on Escape or a scrim tap —
//! with no change to the part's API (screens do not need to).
//!
//! The body is scrollable when given a scroll handle (the parent owns a
//! [`super::scrollbar::ScrollBarState`] and paints the overlay
//! [`super::scrollbar::ScrollBar`] over the same handle, the kit's
//! established pattern).

use gpui::{
    div, prelude::*, px, AnyElement, App, Div, ElementId, IntoElement, Pixels, RenderOnce, Rgba,
    ScrollHandle, SharedString, Window,
};

use crate::theme::{self, palette};
use crate::ui::focus_ring::{focus_ring, focus_ring_style_on};
use crate::ui::interaction::{child_id, PANEL};

/// The 1 px `HIGHLIGHT` top edge (`theme::HAIRLINE`: a real hairline).
const fn top_edge() -> f32 {
    theme::HAIRLINE
}

/// The kit's floating face: the translucent `PANEL` fill, `RADIUS_L`
/// corners, the `float_shadow()`, the 1 px `HIGHLIGHT` top edge and the
/// 1 px `CONTROL_LINE` edge. Shared by [`Panel`],
/// [`super::popover::Popover`], [`super::dialog::Dialog`] and
/// [`super::notice::Notice`] — every colour from `theme::palette()`.
///
/// Returns a `Div` so the caller can chain `.id(…)`, `.w(…)`, focus and
/// key handlers on the face.
pub(crate) fn float_face<E: IntoElement>(content: E) -> Div {
    let p = palette();
    div()
        .relative()
        .flex()
        .flex_col()
        .rounded(theme::z(theme::RADIUS_L))
        .overflow_hidden()
        .bg(p.float_fill())
        .border_1()
        .border_color(p.control_line)
        .shadow(theme::float_shadow())
        // A click on a floating face never reaches what is behind it.
        .occlude()
        .child(
            div()
                .absolute()
                .top(Pixels::ZERO)
                .left(Pixels::ZERO)
                .w_full()
                .h(px(top_edge()))
                .bg(p.highlight),
        )
        .child(content)
}

/// A scroll area's content, kept at its own size: the scroll areas are
/// flex columns, and a flex child with a set height would otherwise shrink
/// to fit (5000 px squeezed into the window) instead of scrolling.
pub(crate) fn scroll_content(content: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        // At least the area's width (rows stretch), wider when the content
        // is — so a wide child stays scrollable on the x axis.
        .min_w(gpui::relative(1.))
        .self_start()
        .child(content)
}

/// The tabs strip row under a panel or sheet header.
pub(crate) fn tabs_row(tabs: AnyElement) -> AnyElement {
    div()
        .flex()
        .w_full()
        .px(theme::z(theme::SP_12))
        .pb(theme::z(theme::SP_4))
        .child(tabs)
        .into_any_element()
}

/// A panel or sheet footer row under the body, above a `LINE` (the job
/// panel's note and Stop) — shared so the compact sheet form keeps it.
pub(crate) fn footer_row(footer: AnyElement) -> AnyElement {
    div()
        .flex()
        .w_full()
        .items_center()
        .justify_between()
        // The mockup's `padding: 10px 16px 16px`; its off-grid 10 snaps down
        // to `SP_8` (DESIGN.md §10.3).
        .gap(theme::z(theme::SP_8))
        .px(theme::z(theme::SP_16))
        .pt(theme::z(theme::SP_8))
        .pb(theme::z(theme::SP_16))
        .border_t_1()
        .border_color(palette().line)
        .child(footer)
        .into_any_element()
}

/// A panel or sheet body: padded, and with the parent's scroll handle a
/// scrolling area (plus the overlay `ScrollBar` when the parent passes
/// its state) — shared so the compact sheet form keeps the same scroll.
pub(crate) fn panel_body(
    id: &ElementId,
    body: Option<AnyElement>,
    scroll: Option<ScrollHandle>,
    scroll_state: Option<crate::ui::scrollbar::ScrollBarState>,
) -> AnyElement {
    match (body, scroll) {
        (Some(body), Some(scroll)) => {
            // Stateful (`.id()` first): on this GPUI fork the scroll
            // methods are `StatefulInteractiveElement` methods.
            let mut area = div()
                .flex()
                .flex_col()
                .flex_grow(1.)
                .id(child_id(id, "body"))
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .px(theme::z(theme::SP_12))
                .pb(theme::z(theme::SP_12))
                .child(scroll_content(body));
            if let Some(state) = scroll_state {
                // The overlay scrollbar (the K2 part) over the same handle;
                // the parent owns the state.
                area = area.child(crate::ui::scrollbar::ScrollBar::new(
                    child_id(id, "scrollbar"),
                    scroll,
                    state,
                ));
            }
            area.into_any_element()
        }
        (Some(body), None) => div()
            .flex()
            .flex_col()
            .flex_grow(1.)
            .px(theme::z(theme::SP_12))
            .pb(theme::z(theme::SP_12))
            .child(scroll_content(body))
            .into_any_element(),
        (None, _) => div().into_any_element(),
    }
}

/// A bounded slot for content that owns its own scroll viewport.
pub(crate) fn fill_body(body: AnyElement) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .w_full()
        .px(theme::z(theme::SP_12))
        .pb(theme::z(theme::SP_12))
        .child(body)
}

/// A kit floating panel. Build with [`Panel::new`] (a `PANEL_W`-wide
/// panel) and chain `.header(…)`, `.tabs(…)`, `.body(…)` and the shared
/// conventions; the parent owns every state (open, selection, scroll)
/// and re-renders the panel.
#[derive(IntoElement)]
pub struct Panel {
    common: crate::ui::interaction::Common,
    width: Option<f32>,
    title: Option<SharedString>,
    subtitle: Option<SharedString>,
    on_close: Option<crate::ui::interaction::ClickHandler>,
    tabs: Option<AnyElement>,
    body: Option<AnyElement>,
    body_fill: bool,
    body_with: Option<Box<dyn FnOnce(Option<SharedString>) -> AnyElement>>,
    scroll: Option<ScrollHandle>,
    scroll_state: Option<crate::ui::scrollbar::ScrollBarState>,
    full_height: bool,
    force_sheet: bool,
    footer: Option<AnyElement>,
}

impl Panel {
    /// A floating panel at the spec width (`theme::PANEL_W`, 360 px).
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            width: None,
            title: None,
            subtitle: None,
            on_close: None,
            tabs: None,
            body: None,
            body_fill: false,
            body_with: None,
            scroll: None,
            scroll_state: None,
            full_height: false,
            force_sheet: false,
            footer: None,
        }
    }

    /// A non-default panel width, in logical px before zoom.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Require the compact Sheet face when a caller's surrounding layout
    /// cannot spare a dock. The normal compact-window policy still applies.
    pub fn sheet_when(mut self, required: bool) -> Self {
        self.force_sheet = required;
        self
    }

    /// The panel width in logical px (`PANEL_W` unless overridden).
    pub fn width_px(&self) -> f32 {
        self.width.unwrap_or(theme::PANEL_W)
    }

    /// The panel header: the `type_::HEADING` title, an optional
    /// `type_::CAPTION` dim subtitle above it (the mockup's `.phead`
    /// order), and the close `IconButton` when `.on_close(…)` is set.
    pub fn header(
        mut self,
        title: impl Into<SharedString>,
        subtitle: Option<SharedString>,
    ) -> Self {
        self.title = Some(title.into());
        self.subtitle = subtitle;
        self
    }

    /// Fill the height the parent gives the panel (the mockup's panel runs
    /// from below the pills to the bottom margin); the body takes the rest
    /// and scrolls. The parent is a flex column (the panel grows in it).
    pub fn full_height(mut self) -> Self {
        self.full_height = true;
        self
    }

    /// A footer under the body, above a `LINE` (the job panel's note and
    /// Stop).
    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    /// The header's title, if any.
    pub fn title_text(&self) -> Option<&SharedString> {
        self.title.as_ref()
    }

    /// A kit `Tabs` strip under the header (the parent builds the strip).
    pub fn tabs(mut self, tabs: impl IntoElement) -> Self {
        self.tabs = Some(tabs.into_any_element());
        self
    }

    /// The panel body (a column of kit parts; scrolls with
    /// `.scroll_handle(…)`).
    pub fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self.body_with = None;
        self.body_fill = false;
        self
    }

    /// Fill the available height with content that owns its own scrolling.
    pub fn body_fill(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self.body_with = None;
        self.body_fill = true;
        self
    }

    /// The body built with the part's disabled reason (KIT.md: containers
    /// hand their reason down): a disabled panel's content is the parent's,
    /// so the parent builds it read-only — dimmed text, disabled controls —
    /// from the same reason. Replaces `.body(…)`.
    pub fn body_with<E: IntoElement + 'static>(
        mut self,
        build: impl FnOnce(Option<SharedString>) -> E + 'static,
    ) -> Self {
        self.body_with = Some(Box::new(move |r| build(r).into_any_element()));
        self.body = None;
        self.body_fill = false;
        self
    }

    /// The scroll handle of the body area.
    pub fn scroll_handle(mut self, handle: ScrollHandle) -> Self {
        self.scroll = Some(handle);
        self
    }

    /// The parent's [`super::scrollbar::ScrollBarState`]: with it, the
    /// panel paints the overlay `ScrollBar` over the body.
    pub fn scroll_state(mut self, state: crate::ui::scrollbar::ScrollBarState) -> Self {
        self.scroll_state = Some(state);
        self
    }

    /// The close handler (the header's × and, in the compact sheet form,
    /// Escape and a scrim tap). The parent sets its open state to closed
    /// there.
    pub fn on_close(
        mut self,
        f: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_close = Some(Box::new(f));
        self
    }

    /// Whether the panel shows its close button.
    pub fn has_close(&self) -> bool {
        self.on_close.is_some()
    }

    /// The panel's paint: (fill, edge) — the `PANEL` fill and the
    /// `CONTROL_LINE` edge.
    pub fn paint(&self) -> (Rgba, Rgba) {
        let p = palette();
        (p.float_fill(), p.control_line)
    }

    /// The header element (the mockup's `.phead`), or `None` — shared by
    /// Panel and its Sheet form. Enabled, the title and subtitle slots carry
    /// child ids, so a truncated one tooltips its full text (KIT.md § Long
    /// text). With a disabled `reason` the slots dim and take no ids (no child
    /// tooltip covers the reason), and the close button is disabled with it.
    pub(crate) fn header_element(
        title: Option<&SharedString>,
        subtitle: Option<&SharedString>,
        id: &ElementId,
        on_close: Option<std::rc::Rc<crate::ui::interaction::ClickHandler>>,
        reason: Option<&SharedString>,
        allow_double: bool,
    ) -> Option<AnyElement> {
        (title.is_some() || on_close.is_some()).then(|| {
            let mut row = div()
                .flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap(theme::z(theme::SP_8));
            if let Some(title) = title {
                let mut col = div().flex().flex_col().flex_grow(1.).min_w(Pixels::ZERO);
                if let Some(subtitle) = subtitle {
                    let sub = crate::ui::text::caption(subtitle.clone());
                    col = col.child(match reason {
                        Some(_) => sub,
                        None => sub.id(child_id(id, "subtitle")),
                    });
                }
                let head = crate::ui::text::heading(title.clone());
                col = col.child(match reason {
                    Some(_) => head.dim(),
                    None => head.id(child_id(id, "title")),
                });
                row = row.child(col);
            }
            if let Some(on_close) = on_close {
                let close_id = child_id(id, "close");
                let close =
                    crate::ui::icon_button::IconButton::new(close_id, crate::icons::Icon::X)
                        .label("Close")
                        .quiet()
                        .allow_double_click_if(allow_double);
                row = row.child(match reason {
                    Some(reason) => close.disabled(reason.clone()),
                    None => close.on_click(move |ev, w, cx| {
                        if allow_double || ev.click_count() <= 1 {
                            on_close(ev, w, cx);
                        }
                    }),
                });
            }
            div()
                .flex()
                .w_full()
                .items_center()
                .gap(theme::z(theme::SP_8))
                .px(theme::z(theme::SP_20))
                .pt(theme::z(theme::SP_12))
                .pb(theme::z(theme::SP_4))
                .child(row)
                .into_any_element()
        })
    }
}

crate::kit_shared!(Panel);

impl std::fmt::Debug for Panel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Panel({})", self.common.selector_for(PANEL))
    }
}

impl RenderOnce for Panel {
    fn render(mut self, window: &mut Window, _cx: &mut gpui::App) -> impl IntoElement {
        if let Some(build) = self.body_with.take() {
            self.body = Some(build(self.common.disabled_reason.clone()));
        }
        // Read every field before the moves (a `&self` method call is
        // a whole-self borrow).
        let w = theme::z(self.width_px());
        let common = self.common;
        let selector = common.selector_for(PANEL);
        // The close handler is used by the header's × and, in the
        // compact sheet form, by Escape and the scrim tap: one closure,
        // shared through `Rc` (the handler itself is not `Clone`).
        let on_close = self.on_close.map(|h| std::rc::Rc::new(h));
        let title = self.title.clone();
        let subtitle = self.subtitle.clone();
        let disabled = common.is_disabled();

        // Compact windows (< COMPACT_W): the panel is a sheet (full-width,
        // bottom-anchored, behind the scrim) that keeps **everything**: the
        // shared state (id, focus handle, tab index, disabled reason,
        // label, tooltip), the header, the tabs, the body with the
        // parent's scroll handle and scrollbar state, and the close.
        if self.force_sheet || crate::ui::sheet::is_compact(window) {
            // Only what the panel has: no blank header for a titleless panel,
            // no empty body for a bodyless one.
            let mut sheet =
                crate::ui::sheet::Sheet::new(common.id.clone()).with_common(common.clone());
            if let Some(title) = title.clone() {
                sheet = sheet.header(title, subtitle.clone());
            }
            if let Some(body) = self.body {
                sheet = if self.body_fill {
                    sheet.body_fill(body)
                } else {
                    sheet.body(body)
                };
            }
            if let Some(tabs) = self.tabs {
                sheet = sheet.tabs(tabs);
            }
            if let Some(scroll) = self.scroll {
                sheet = sheet.scroll_handle(scroll);
            }
            if let Some(state) = self.scroll_state {
                sheet = sheet.scroll_state(state);
            }
            if let Some(footer) = self.footer {
                sheet = sheet.footer(footer);
            }
            let sheet = match on_close.clone() {
                Some(h) => sheet.on_close(move |ev, w, cx| h(ev, w, cx)),
                None => sheet,
            };
            return div()
                .size_full()
                .relative()
                .debug_selector(|| selector.clone())
                .child(sheet)
                .into_any_element();
        }

        let header = Self::header_element(
            title.as_ref(),
            subtitle.as_ref(),
            &common.id,
            on_close.clone(),
            common.disabled_reason.as_ref(),
            common.allow_double_click,
        );
        let full_height = self.full_height;
        let body = self.body;
        let tabs = self.tabs;
        let scroll = self.scroll;
        let scroll_state = self.scroll_state.clone();

        let body = if self.body_fill {
            body.map(|body| fill_body(body).into_any_element())
                .unwrap_or_else(|| div().into_any_element())
        } else {
            panel_body(&common.id, body, scroll, scroll_state)
        };

        let mut panel = float_face(
            div()
                .flex()
                .flex_col()
                .w_full()
                .when_some(header, |d, h| d.child(h))
                .when(full_height, |d| d.flex_1().min_h_0())
                .when_some(tabs, |d, t| d.child(tabs_row(t)))
                .child(body)
                .when_some(self.footer, |d, f| d.child(footer_row(f))),
        )
        .shadow(vec![]) // docked panel: no shadow
        .id(common.id.clone())
        .w(w)
        .when(full_height, |d| d.h_full());
        if !disabled {
            // The shared wiring: a tab stop (tracked handle or its own),
            // `.tab_index`, and `.pointer_only()`.
            panel = crate::ui::interaction::focus_wiring(panel, &common);
            if !common.focused {
                panel = panel.focus_visible(focus_ring_style_on(palette().float_fill()));
            }
        }
        if let Some(tip) = common.tooltip() {
            panel = panel.tooltip(crate::icons::tip(tip.to_string()));
        }

        // A full-height panel passes its parent's height down through the
        // wrappers (the selector box and the ring box), so the face fills it
        // and the body scrolls instead of growing past the parent.
        let mut root = div()
            .relative()
            .debug_selector(|| selector.clone())
            .when(full_height, |d| d.flex().flex_col().h_full())
            .child(panel);

        // A disabled or pointer-only panel is never a tab stop (KIT.md).
        if let Some(index) = common
            .tab_index
            .filter(|_| !disabled && !common.pointer_only)
        {
            root = root.tab_index(index);
        }

        focus_ring(root, common.frozen_focus())
            .when(full_height, |d| d.flex_1().min_h_0())
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::{render_part, render_part_at, tooltip_at};

    const LONG_TITLE: &str = "A deliberately long panel title that has to truncate in the header";

    /// A double click (two presses, click_count 2) at `at`.
    fn double_click_at(wcx: &mut gpui::VisualTestContext, at: gpui::Point<Pixels>) {
        for count in [1usize, 2] {
            wcx.simulate_event(gpui::MouseDownEvent {
                button: gpui::MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::default(),
                click_count: count,
                first_mouse: false,
            });
            wcx.simulate_event(gpui::MouseUpEvent {
                button: gpui::MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::default(),
                click_count: count,
            });
        }
    }

    /// A disabled or pointer-only panel is never a tab stop, even with a
    /// `.tab_index(…)`.
    #[gpui::test]
    fn disabled_or_pointer_only_panel_is_not_a_tab_stop(cx: &mut TestAppContext) {
        for which in ["disabled", "pointer-only"] {
            let (_root, wcx) = render_part(cx, move || {
                let p = Panel::new("p")
                    .tab_index(0)
                    .body(crate::ui::text::ui_text("Body"));
                if which == "disabled" {
                    p.disabled("Read only")
                } else {
                    p.pointer_only()
                }
            });
            wcx.update(|w, cx| w.focus_next(cx));
            assert!(
                wcx.update(|w, cx| w.focused(cx)).is_none(),
                "{which}: Tab lands nowhere"
            );
        }
    }

    /// Compact, a panel without a title gets no blank title (only the ×).
    #[gpui::test]
    fn compact_titleless_panel_has_no_blank_title(cx: &mut TestAppContext) {
        let (_handle, wcx) = render_part_at(
            cx,
            gpui::size(
                theme::z(theme::SETTINGS_ROW_W),
                theme::z(theme::SETTINGS_ROW_W),
            ),
            || {
                Panel::new("p")
                    .body(crate::ui::text::ui_text("Body"))
                    .on_close(|_e, _w, _cx| {})
            },
        );
        assert!(wcx.debug_bounds("ui-sheet-scrim").is_some(), "compact");
        assert!(
            wcx.debug_bounds("ui-text-p-title").is_none(),
            "no title slot"
        );
        assert!(
            wcx.debug_bounds("ui-icon-button-p-close").is_some(),
            "the × stays"
        );
    }

    /// The container's double-click setting reaches its close button: with
    /// `.allow_double_click()` a double click closes twice; without, once.
    #[gpui::test]
    fn panel_close_follows_the_double_click_setting(cx: &mut TestAppContext) {
        for allow in [false, true] {
            let closed = std::rc::Rc::new(std::cell::Cell::new(0u32));
            let c = closed.clone();
            let (_root, wcx) = render_part(cx, move || {
                let c = c.clone();
                Panel::new("p")
                    .header("Sources", None)
                    .allow_double_click_if(allow)
                    .on_close(move |_e, _w, _cx| c.set(c.get() + 1))
            });
            let at = wcx.debug_bounds("ui-icon-button-p-close").unwrap().center();
            double_click_at(wcx, at);
            assert_eq!(closed.get(), if allow { 2 } else { 1 }, "allow={allow}");
        }
    }

    /// Long text: an enabled panel's truncated title tooltips its full text.
    #[gpui::test]
    fn long_header_title_tooltips_its_full_text(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, || {
            Panel::new("inspector")
                .header(LONG_TITLE, Some(SharedString::from("Inspector")))
                .body(crate::ui::text::ui_text("Body"))
                .on_close(|_e, _w, _cx| {})
        });
        assert_eq!(
            tooltip_at(wcx, "ui-text-inspector-title").as_deref(),
            Some(LONG_TITLE)
        );
    }

    /// Disabled: the close button is disabled with the panel's reason (its
    /// tooltip, and a click does nothing), and the title slot takes no id,
    /// so nothing covers the reason; `.body_with` hands the reason down.
    #[gpui::test]
    fn disabled_panel_disables_its_close_and_hands_down_the_reason(cx: &mut TestAppContext) {
        let closed = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let got = std::rc::Rc::new(std::cell::RefCell::new(None::<SharedString>));
        let (c, g) = (closed.clone(), got.clone());
        let (_root, wcx) = render_part(cx, move || {
            let (c, g) = (c.clone(), g.clone());
            Panel::new("inspector")
                .header(LONG_TITLE, None)
                .body_with(move |reason| {
                    *g.borrow_mut() = reason;
                    crate::ui::text::ui_text("Body")
                })
                .disabled("The job is running")
                .on_close(move |_e, _w, _cx| c.set(c.get() + 1))
        });
        assert_eq!(
            tooltip_at(wcx, "ui-icon-button-inspector-close").as_deref(),
            Some("The job is running")
        );
        assert!(
            wcx.debug_bounds("ui-text-inspector-title").is_none(),
            "no title id"
        );
        assert_eq!(got.borrow().as_deref(), Some("The job is running"));
        let at = wcx
            .debug_bounds("ui-icon-button-inspector-close")
            .unwrap()
            .center();
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
        assert_eq!(closed.get(), 0, "a disabled close does nothing");
    }
    use crate::ui::tabs::{TabItem, Tabs};

    /// The panel: `PANEL_W` wide (360 logical px at zoom 1) and its
    /// header + body laid out (the close button keeps its `target()` hit
    /// box).
    #[gpui::test]
    fn panel_is_panel_w_wide_with_header_and_close(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, || {
            Panel::new("inspector")
                .header("Reply", Some(SharedString::from("Selected")))
                .body(crate::ui::text::ui_text("Body").wrap())
                .on_close(|_e, _w, _c| {})
        });
        let panel = wcx
            .debug_bounds("ui-panel-inspector")
            .unwrap_or_else(|| panic!("the panel recorded no bounds"));
        assert_eq!(
            f32::from(panel.size.width),
            f32::from(theme::z(theme::PANEL_W)),
            "the panel is exactly PANEL_W wide: {panel:?}",
        );
        let close = wcx
            .debug_bounds("ui-icon-button-inspector-close")
            .unwrap_or_else(|| panic!("the close button recorded no bounds"));
        assert!(
            f32::from(close.size.width) >= f32::from(theme::target()),
            "the close button keeps its target() hit box: {close:?}",
        );
    }

    /// A non-default width is honoured.
    #[gpui::test]
    fn panel_custom_width(cx: &mut TestAppContext) {
        const CUSTOM_W: f32 = 300.0;
        let (_root, wcx) = render_part(cx, || Panel::new("narrow").width(CUSTOM_W));
        let panel = wcx
            .debug_bounds("ui-panel-narrow")
            .unwrap_or_else(|| panic!("the panel recorded no bounds"));
        assert_eq!(
            f32::from(panel.size.width),
            f32::from(theme::z(CUSTOM_W)),
            "the custom width is honoured: {panel:?}",
        );
    }

    /// The header's close button fires `.on_close`.
    #[gpui::test]
    fn panel_close_fires_on_close(cx: &mut TestAppContext) {
        let closed = std::rc::Rc::new(std::cell::Cell::new(0));
        let closed_in = closed.clone();
        let (_root, wcx) = render_part(cx, move || {
            let closed_in = closed_in.clone();
            Panel::new("inspector")
                .header("Reply", Some(SharedString::from("Selected")))
                .on_close(move |_e, _w, _c| {
                    closed_in.set(1);
                })
        });
        let close = wcx
            .debug_bounds("ui-icon-button-inspector-close")
            .unwrap_or_else(|| panic!("the close button recorded no bounds"));
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: close.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: close.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert_eq!(closed.get(), 1, "the header's × fires on_close");
    }

    /// `.full_height()` fills the parent's height and the body scrolls
    /// inside it; without it the panel grows to its content.
    #[gpui::test]
    fn full_height_fills_the_parent_and_the_body_scrolls(cx: &mut TestAppContext) {
        let parent_h = theme::z(theme::PANEL_W);
        let build = |full: bool, scroll: ScrollHandle| {
            move || {
                let rows = div()
                    .flex()
                    .flex_col()
                    .child(div().debug_selector(|| "row-0".to_string()).child("Row 0"))
                    .children((1..40).map(|i| crate::ui::text::ui_text(format!("Row {i}"))));
                let panel = Panel::new(if full { "full" } else { "grows" })
                    .header("Reply", None)
                    .scroll_handle(scroll.clone())
                    .body(rows)
                    .footer(
                        div()
                            .debug_selector(|| "the-footer".to_string())
                            .child(crate::ui::text::caption("Footer")),
                    );
                div().flex().flex_col().h(parent_h).child(if full {
                    panel.full_height()
                } else {
                    panel
                })
            }
        };
        let scroll = ScrollHandle::new();
        let (_root, wcx) = render_part(cx, build(true, scroll.clone()));
        let full = wcx.debug_bounds("ui-panel-full").unwrap();
        assert_eq!(
            full.size.height, parent_h,
            "full height: the parent's height"
        );
        assert!(
            scroll.max_offset().y > gpui::Pixels::ZERO,
            "the 40 rows overflow the body, so it scrolls"
        );
        let row_before = wcx.debug_bounds("row-0").unwrap();
        let footer = wcx.debug_bounds("the-footer").unwrap();
        assert!(
            footer.bottom() <= full.bottom() && footer.top() > full.top(),
            "the footer is pinned inside the panel: {footer:?} in {full:?}"
        );
        // Scrolled, the body moves and the footer stays put.
        scroll.set_offset(gpui::point(gpui::Pixels::ZERO, -theme::z(theme::PANEL_W)));
        wcx.update(|w, _cx| w.refresh());
        wcx.run_until_parked();
        assert_eq!(
            scroll.offset().y,
            -theme::z(theme::PANEL_W),
            "the body scrolled"
        );
        let row_after = wcx.debug_bounds("row-0").unwrap();
        assert_eq!(
            row_after.origin.y,
            row_before.origin.y - theme::z(theme::PANEL_W),
            "the body's first row moved up by the scroll"
        );
        assert_eq!(
            wcx.debug_bounds("the-footer").unwrap(),
            footer,
            "the footer stays where it was"
        );
        let (_root, wcx) = render_part(cx, build(false, ScrollHandle::new()));
        let grows = wcx.debug_bounds("ui-panel-grows").unwrap();
        assert!(
            grows.size.height > parent_h,
            "without it the panel grows to its 40 rows: {grows:?}"
        );
    }
    /// The compact sheet form keeps the panel's footer (the job panel's note
    /// and Stop), inside the sheet face.
    #[gpui::test]
    fn compact_sheet_keeps_the_footer(cx: &mut TestAppContext) {
        let (_handle, wcx) = render_part_at(
            cx,
            gpui::size(
                theme::z(theme::SETTINGS_ROW_W),
                theme::z(theme::SETTINGS_ROW_W),
            ),
            || {
                Panel::new("p")
                    .header("This job", None)
                    .body(crate::ui::text::ui_text("Body"))
                    .footer(
                        div()
                            .debug_selector(|| "the-footer".to_string())
                            .child("Stop"),
                    )
            },
        );
        assert!(
            wcx.debug_bounds("ui-sheet-scrim").is_some(),
            "compact: a sheet"
        );
        let footer = wcx.debug_bounds("the-footer").expect("the footer survives");
        assert!(footer.size.height > gpui::Pixels::ZERO);
    }

    /// Below `COMPACT_W` the panel paints the sheet form automatically:
    /// a full-window overlay whose panel is bottom-anchored, with the
    /// same header and close.
    #[gpui::test]
    fn panel_becomes_a_sheet_below_compact_w(cx: &mut TestAppContext) {
        // A window just below COMPACT_W (880 logical px).
        const SMALL_W: f32 = 800.0;
        const SMALL_H: f32 = 600.0;
        fn panel() -> Panel {
            Panel::new("inspector")
                .header("Reply", Some(SharedString::from("Selected")))
                .body(crate::ui::text::ui_text("Body").wrap())
        }
        // The default test window is wide — the panel paints as a panel.
        let (_root, wcx) = render_part(cx, panel);
        let wide = wcx
            .debug_bounds("ui-panel-inspector")
            .unwrap_or_else(|| panic!("the wide panel recorded no bounds"));
        assert_eq!(
            f32::from(wide.size.width),
            f32::from(theme::z(theme::PANEL_W)),
            "wide: the panel keeps its PANEL_W width"
        );

        // A second window, below COMPACT_W: the same part paints a sheet.
        let (_handle, wcx_small) =
            render_part_at(cx, gpui::size(gpui::px(SMALL_W), gpui::px(SMALL_H)), panel);
        let sheet = wcx_small
            .debug_bounds("ui-panel-inspector")
            .unwrap_or_else(|| panic!("the sheet overlay recorded no bounds"));
        assert_eq!(
            f32::from(sheet.size.height),
            f32::from(gpui::px(SMALL_H)),
            "compact: the panel is the full-window sheet overlay: {sheet:?}",
        );
        // The sheet's scrim keeps the constant "ui-sheet-scrim"
        // selector (K3's sheet.rs).
        assert!(
            wcx_small.debug_bounds("ui-sheet-scrim").is_some(),
            "compact: the sheet scrim is present"
        );
    }

    /// The panel face: the floating fill with the `CONTROL_LINE` edge
    /// (read through the palette, so a light swap retints the panel). K3b
    /// item 10: without a backdrop blur the fill is the opaque
    /// `PANEL_SOLID`, never the translucent `PANEL`.
    #[test]
    fn panel_paint_uses_the_panel_tokens() {
        let p = palette();
        assert_eq!(Panel::new("x").paint(), (p.float_fill(), p.control_line));
        assert_eq!(p.float_fill(), p.panel_solid, "the solid fallback");
    }

    /// A `.tabs(…)` strip under the header.
    #[gpui::test]
    fn panel_takes_a_tabs_strip(cx: &mut TestAppContext) {
        let items = vec![
            TabItem::new("a", "About").dot(crate::theme::Tone::Neutral),
            TabItem::new("b", "Sources"),
        ];
        let (_root, wcx) = render_part(cx, move || {
            Panel::new("inspector")
                .header("Reply", None)
                .tabs(Tabs::new("inspector-tabs", items.clone(), 0))
                .body(crate::ui::text::ui_text("Body").wrap())
        });
        let tabs = wcx
            .debug_bounds("ui-tabs-inspector-tabs")
            .unwrap_or_else(|| panic!("the tabs strip recorded no bounds"));
        assert!(
            f32::from(tabs.size.height) >= f32::from(theme::target()),
            "the strip keeps its target() tab height: {tabs:?}",
        );
    }

    fn click(wcx: &mut gpui::VisualTestContext, at: gpui::Point<gpui::Pixels>) {
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
    }

    /// K3b item 9: a click on a floating face (a Panel, a Pill, a bare
    /// `float_face` as Notice uses) never reaches the button behind it.
    #[gpui::test]
    fn floating_faces_occlude_what_is_behind(cx: &mut TestAppContext) {
        for which in ["panel", "pill", "face"] {
            let hits = std::rc::Rc::new(std::cell::Cell::new(0u32));
            let hits_in = hits.clone();
            let (_root, wcx) = render_part(cx, move || {
                let hits_in = hits_in.clone();
                let face: gpui::AnyElement = match which {
                    "panel" => Panel::new("f")
                        .header("Reply", None)
                        .body(crate::ui::text::ui_text("Body"))
                        .into_any_element(),
                    "pill" => crate::ui::pill::Pill::new("f")
                        .child(crate::ui::text::label("Pill"))
                        .into_any_element(),
                    _ => float_face(div().size(px(200.)))
                        .debug_selector(|| "ui-face-f".to_string())
                        .into_any_element(),
                };
                div()
                    .size_full()
                    .relative()
                    .child(
                        div()
                            .id("behind")
                            .absolute()
                            .top(px(0.))
                            .left(px(0.))
                            .size_full()
                            .on_click(move |_e, _w, _cx| hits_in.set(hits_in.get() + 1)),
                    )
                    .child(div().absolute().top(px(40.)).left(px(40.)).child(face))
            });
            let sel: &'static str = Box::leak(format!("ui-{which}-f").into_boxed_str());
            let at = wcx
                .debug_bounds(sel)
                .unwrap_or_else(|| panic!("{which} recorded no bounds"))
                .center();
            click(wcx, at);
            assert_eq!(
                hits.get(),
                0,
                "{which}: the click did not reach the button behind"
            );
            click(wcx, gpui::point(px(5.), px(5.)));
            assert_eq!(
                hits.get(),
                1,
                "{which}: a click beside the face does reach it"
            );
        }
    }

    /// K3b item 11: the compact sheet keeps everything the panel had —
    /// the tabs, the parent's scroll handle, the focus handle (the sheet
    /// takes focus on it when it opens) and the close handler.
    #[gpui::test]
    fn compact_sheet_keeps_tabs_scroll_focus_and_close(cx: &mut TestAppContext) {
        let scroll = gpui::ScrollHandle::new();
        let handle = cx.update(|cx| cx.focus_handle());
        let closed = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let (s, h, c) = (scroll.clone(), handle.clone(), closed.clone());
        let (_handle, wcx) = render_part_at(cx, gpui::size(px(800.), px(600.)), move || {
            let c = c.clone();
            Panel::new("insp")
                .header("Reply", None)
                .tabs(crate::ui::tabs::Tabs::new(
                    "insp-tabs",
                    vec![
                        crate::ui::tabs::TabItem::new("a", "Answer"),
                        crate::ui::tabs::TabItem::new("b", "Sources"),
                    ],
                    0,
                ))
                .body(div().h(px(2000.)).child(crate::ui::text::ui_text("Long")))
                .scroll_handle(s.clone())
                .focus_handle(&h)
                .on_close(move |_e, _w, _cx| c.set(c.get() + 1))
        });
        assert!(
            wcx.debug_bounds("ui-tabs-insp-tabs").is_some(),
            "the tabs stay"
        );
        assert!(
            f32::from(scroll.bounds().size.height) > 0.0,
            "the body scrolls on the parent's handle"
        );
        // Modal: the sheet takes focus on open, on the parent's handle.
        wcx.run_until_parked();
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            Some(handle),
            "the sheet holds focus on the parent's handle"
        );
        click(wcx, gpui::point(px(5.), px(5.)));
        assert_eq!(
            closed.get(),
            1,
            "the scrim tap closes through the panel's handler"
        );
    }

    /// K3b round 2 (Codex): the compact sheet keeps the Panel's tooltip —
    /// a disabled panel's reason is the tooltip over the sheet face.
    #[gpui::test]
    fn compact_sheet_keeps_the_disabled_reason(cx: &mut TestAppContext) {
        let (_handle, wcx) = render_part_at(cx, gpui::size(px(800.), px(600.)), || {
            Panel::new("ro")
                .header("Sources", None)
                .body(crate::ui::text::ui_text("Body"))
                .disabled("Read only")
        });
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at(wcx, "ui-sheet-ro-panel").as_deref(),
            Some("Read only")
        );
    }

    /// K4b round 4 (Codex): Panel uses the shared wiring — a plain tracked
    /// handle is a tab stop, and `.pointer_only()` makes it none.
    #[gpui::test]
    fn panel_focus_follows_the_shared_wiring(cx: &mut TestAppContext) {
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .child(Panel::new("tab").header("Reply", None).focus_handle(&h))
        });
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            Some(handle),
            "a tab stop"
        );
        // The same tracked handle setup, plus `.pointer_only()`: no stop.
        let tracked = cx.update(|cx| cx.focus_handle());
        let t = tracked.clone();
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(
                Panel::new("po")
                    .header("Reply", None)
                    .focus_handle(&t)
                    .pointer_only(),
            )
        });
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert!(
            wcx.update(|w, cx| w.focused(cx)).is_none(),
            "pointer-only: none"
        );
    }
}
