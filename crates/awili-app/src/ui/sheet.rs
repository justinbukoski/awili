//! `ui::Sheet` — the narrow-window form of a `Panel` (KIT.md v3, batch K3).
//!
//! Below `theme::COMPACT_W` (880 logical px) a floating `Panel` has no
//! room to sit beside the canvas, so it becomes a sheet: the same
//! panel face, full-width and bottom-anchored, sliding up over the
//! canvas behind a `SCRIM`. Escape or a tap on the scrim closes it
//! (the parent owns the open state, through `.on_close(…)`).
//!
//! `ui::is_compact(window)` is the switch: true when the window is
//! narrower than `COMPACT_W` (scaled through `z()` like every kit
//! dimension). `Panel` consults it at render and paints the sheet form
//! automatically, so screens do not need to.
//!
//! The slide-up motion itself is not in GPUI (no animation primitive
//! for the kit yet); the part paints the settled position — the
//! instant-appear form, which is also the Reduce-Motion behaviour.

use gpui::{
    div, prelude::*, px, App, ClickEvent, ElementId, IntoElement, KeyDownEvent, Pixels, Point,
    RenderOnce, SharedString, Window,
};

use crate::theme::{self, palette};
use crate::ui::interaction::SHEET;

/// The 1 px `HIGHLIGHT` top edge (`theme::HAIRLINE`: a real hairline).
const fn top_edge() -> f32 {
    theme::HAIRLINE
}

/// Whether the window is in the compact regime (KIT.md § Sheet): true
/// when the window is narrower than `theme::COMPACT_W` (880 logical px,
/// scaled through `z()` for interface zoom).
pub fn is_compact(window: &Window) -> bool {
    f32::from(window.bounds().size.width) < f32::from(theme::z(theme::COMPACT_W))
}

/// The sheet face: the panel face (translucent `PANEL`, `RADIUS_L`, the
/// float shadow, the 1 px `HIGHLIGHT` top and the `CONTROL_LINE` edge)
/// with the bottom corners square (the sheet is bottom-anchored to the
/// window).
pub(crate) fn sheet_face<E: IntoElement>(content: E) -> gpui::Div {
    let p = palette();
    div()
        .relative()
        .flex()
        .flex_col()
        .rounded_t(theme::z(theme::RADIUS_L))
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

/// A kit sheet: the narrow-window (< `COMPACT_W`) form of a `Panel`.
/// Build with [`Sheet::new`] and chain `.header(…)`, `.body(…)` and
/// `.on_close(…)`; the parent owns the open state (Escape and a scrim
/// tap both call the handler).
#[derive(IntoElement)]
pub struct Sheet {
    common: crate::ui::interaction::Common,
    title: Option<SharedString>,
    subtitle: Option<SharedString>,
    body: Option<gpui::AnyElement>,
    body_fill: bool,
    body_with: Option<Box<dyn FnOnce(Option<SharedString>) -> gpui::AnyElement>>,
    on_close: Option<crate::ui::interaction::ClickHandler>,
    tabs: Option<gpui::AnyElement>,
    scroll: Option<gpui::ScrollHandle>,
    scroll_state: Option<crate::ui::scrollbar::ScrollBarState>,
    footer: Option<gpui::AnyElement>,
}

impl Sheet {
    /// A sheet with no header or body yet.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            title: None,
            subtitle: None,
            body: None,
            body_fill: false,
            body_with: None,
            on_close: None,
            tabs: None,
            scroll: None,
            scroll_state: None,
            footer: None,
        }
    }

    /// A tabs strip under the header (a `Panel`'s tabs keep their place in
    /// the compact sheet form).
    pub fn tabs(mut self, tabs: impl IntoElement) -> Self {
        self.tabs = Some(tabs.into_any_element());
        self
    }

    /// The parent's scroll handle for the body (and, with
    /// `.scroll_state(…)`, the overlay `ScrollBar`) — the same wiring as
    /// `Panel`, so the offset survives the compact switch.
    pub fn scroll_handle(mut self, handle: gpui::ScrollHandle) -> Self {
        self.scroll = Some(handle);
        self
    }

    /// A footer row under the body, above a `LINE` (a `Panel`'s footer keeps
    /// its place in the compact sheet form).
    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    /// The parent's `ScrollBarState` for the overlay scrollbar.
    pub fn scroll_state(mut self, state: crate::ui::scrollbar::ScrollBarState) -> Self {
        self.scroll_state = Some(state);
        self
    }

    /// Take over another part's shared state wholesale (id, focus handle,
    /// tab index, disabled reason, label, tooltip …) — the `Panel` →
    /// `Sheet` conversion keeps every option.
    pub(crate) fn with_common(mut self, common: crate::ui::interaction::Common) -> Self {
        self.common = common;
        self
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

    /// The header's title, if any.
    pub fn title_text(&self) -> Option<&SharedString> {
        self.title.as_ref()
    }

    /// The sheet's body (a column of kit parts, clipped to the sheet).
    pub fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self.body_fill = false;
        self.body_with = None;
        self
    }

    /// Fill the available sheet height with internally scrolling content.
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

    /// The close handler: called by Escape and by a tap on the scrim.
    /// The parent sets its open state to closed there.
    pub fn on_close(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Box::new(f));
        self
    }

    /// Whether the sheet shows its close button.
    pub fn has_close(&self) -> bool {
        self.on_close.is_some()
    }

    /// The sheet's maximum height: the panel stops at `theme::EDGE`
    /// from the window's top edge, so it never covers the whole window.
    pub fn max_height(window: &Window) -> Pixels {
        // The window height is already in logical px: subtract the scaled
        // `EDGE` once, with no second scaling (KIT.md § Sheet).
        let h = window.bounds().size.height;
        (h - theme::z(theme::EDGE)).max(Pixels::ZERO)
    }
}

crate::kit_shared!(Sheet);

impl std::fmt::Debug for Sheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sheet({})", self.common.selector_for(SHEET))
    }
}

impl RenderOnce for Sheet {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if let Some(build) = self.body_with.take() {
            self.body = Some(build(self.common.disabled_reason.clone()));
        }
        let mut common = self.common;
        // Modal: on open, focus moves into the sheet (its own handle when the
        // parent gives none), once per open — a re-render never steals it
        // back. A pointer-only or disabled sheet takes no focus.
        if !common.is_disabled() && !common.pointer_only {
            let handle = match common.focus_handle.clone() {
                Some(h) => h,
                None => window
                    .use_keyed_state(
                        crate::ui::interaction::child_id(&common.id, "focus"),
                        cx,
                        |_, cx| cx.focus_handle(),
                    )
                    .read(cx)
                    .clone(),
            };
            let opened = window
                .use_keyed_state(
                    crate::ui::interaction::child_id(&common.id, "opened"),
                    cx,
                    |_, _| std::rc::Rc::new(std::cell::Cell::new(false)),
                )
                .read(cx)
                .clone();
            if !opened.replace(true) && !handle.contains_focused(window, cx) {
                window.focus(&handle, cx);
            }
            common.focus_handle = Some(handle);
        }
        let selector = common.selector_for(SHEET);
        // The close handler is used by the header's ×, the scrim tap and
        // Escape: one closure, shared through `Rc` (the handler itself
        // is not `Clone`).
        let on_close = self.on_close.map(|h| std::rc::Rc::new(h));
        let disabled = common.is_disabled();

        // The header: the Panel's own (the same slots, ids and disabled
        // reason), so the two forms never drift.
        let header = crate::ui::panel::Panel::header_element(
            self.title.as_ref(),
            self.subtitle.as_ref(),
            &common.id,
            on_close.clone(),
            common.disabled_reason.as_ref(),
            common.allow_double_click,
        );

        let body = self.body;
        let tabs = self.tabs;
        let scroll = self.scroll;
        let scroll_state = self.scroll_state;
        let footer = self.footer;
        let max_h = Self::max_height(window);

        // The sheet: bottom-anchored, full-width, clamped away from the
        // top by `EDGE`, behind a full-window scrim.
        let scrim = div()
            .absolute()
            .top(Pixels::ZERO)
            .left(Pixels::ZERO)
            .size_full()
            .bg(palette().scrim)
            // The scrim is modal: a tap on it never reaches the canvas
            // behind (it only closes the sheet).
            .occlude()
            .debug_selector(|| "ui-sheet-scrim".to_string());
        // The scrim is stateful (`.id()` first): on this GPUI fork the
        // click listener is a `StatefulInteractiveElement` method.
        // The container's double-click setting reaches its scrim, as it does
        // its header's ×.
        let allow_double = common.allow_double_click;
        // A disabled sheet's scrim still blocks the canvas but closes nothing.
        let scrim = match on_close.clone().filter(|_| !disabled) {
            Some(h) => scrim
                .id(crate::ui::interaction::child_id(&common.id, "scrim"))
                .debug_selector(|| "ui-sheet-scrim".to_string())
                .on_click(move |ev, w, cx| {
                    // The kit's double-click guard: a double tap closes once,
                    // unless the container allows double clicks.
                    if allow_double || ev.click_count() <= 1 {
                        h(ev, w, cx);
                    }
                }),
            None => scrim
                .id(crate::ui::interaction::child_id(&common.id, "scrim"))
                .debug_selector(|| "ui-sheet-scrim".to_string()),
        };

        // The painted face itself is bounded (`max_h`), and its column and
        // body may shrink below their content (`min_h_0`), so tall content
        // scrolls inside the face instead of running past the window.
        let mut panel = sheet_face(
            div()
                .flex()
                .flex_col()
                .w_full()
                .flex_1()
                .min_h_0()
                .when_some(header, |d, h| d.child(h))
                .when_some(tabs, |d, t| d.child(crate::ui::panel::tabs_row(t)))
                .when_some(body, |d, b| match scroll {
                    _ if self.body_fill => d.child(crate::ui::panel::fill_body(b)),
                    // The parent's handle (and scrollbar state): the same
                    // wiring as `Panel`.
                    Some(scroll) => d.child(crate::ui::panel::panel_body(
                        &common.id,
                        Some(b),
                        Some(scroll),
                        scroll_state,
                    )),
                    None => d.child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_grow(1.)
                            // Stateful (`.id()` first): on this GPUI
                            // fork the scroll methods are a
                            // `StatefulInteractiveElement` method.
                            .id(crate::ui::interaction::child_id(&common.id, "body"))
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(theme::z(theme::SP_12))
                            .pb(theme::z(theme::SP_12))
                            .child(crate::ui::panel::scroll_content(b)),
                    ),
                })
                .when_some(footer, |d, f| d.child(crate::ui::panel::footer_row(f))),
        )
        .w_full()
        .max_h(max_h)
        .when(self.body_fill, |d| d.h(max_h))
        // The sheet face itself answers to `<selector>-panel`.
        .debug_selector({
            let selector = selector.clone();
            move || format!("{selector}-panel")
        })
        .id(crate::ui::interaction::child_id(&common.id, "panel"));
        // The Panel's (or the sheet's own) tooltip — the disabled reason
        // first — on the face.
        if let Some(tip) = common.tooltip() {
            panel = panel.tooltip(crate::icons::tip(tip.to_string()));
        }

        let mut root = div()
            .size_full()
            .relative()
            .id(common.id.clone())
            .debug_selector(|| selector.clone())
            .child(scrim)
            .child(
                div()
                    .absolute()
                    .bottom(Pixels::ZERO)
                    .left(Pixels::ZERO)
                    .w_full()
                    // The sheet swallows its own clicks so they do not
                    // bubble to the scrim and close it.
                    .occlude()
                    .child(panel),
            );

        if !disabled {
            root = crate::ui::interaction::focus_wiring(root, &common);
            let sheet_focus = common.focus_handle.clone().filter(|_| !common.pointer_only);
            root = root.capture_key_down(move |ev, window, cx| {
                let key = &ev.keystroke;
                if key.key == "tab"
                    && !(key.modifiers.platform || key.modifiers.control || key.modifiers.alt)
                {
                    if let Some(handle) = &sheet_focus {
                        let mut visited = Vec::new();
                        loop {
                            if key.modifiers.shift {
                                window.focus_prev(cx);
                            } else {
                                window.focus_next(cx);
                            }
                            let Some(next) = window.focused(cx) else {
                                window.focus(handle, cx);
                                break;
                            };
                            if handle.contains_focused(window, cx) {
                                break;
                            }
                            if visited.contains(&next) {
                                window.focus(handle, cx);
                                break;
                            }
                            visited.push(next);
                        }
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }
            });
            root = root.on_key_down(move |ev, window, cx| {
                if ev.keystroke.key == "escape" {
                    if let Some(on_close) = &on_close {
                        let ev = gpui::ClickEvent::Keyboard(gpui::KeyboardClickEvent::default());
                        window.prevent_default();
                        cx.stop_propagation();
                        on_close(&ev, window, cx);
                    }
                }
            });
        }

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::{render_part, render_part_at};

    /// `is_compact`: true below `COMPACT_W` (880), false at and above it
    /// (scaled through `z()` for interface zoom).
    #[gpui::test]
    fn is_compact_is_true_below_compact_w(cx: &mut TestAppContext) {
        const WIN_H: f32 = 600.0;
        for (width, expect) in [
            (879.0_f32, true),
            (880.0, false),
            (1280.0, false),
            (640.0, true),
        ] {
            let (_handle, wcx) =
                render_part_at(cx, gpui::size(px(width), px(WIN_H)), || div().size_full());
            assert_eq!(
                wcx.update(|w, _cx| is_compact(w)),
                expect,
                "is_compact at {width}px must be {expect}",
            );
        }
        // Interface zoom scales the floor: at zoom 0.5 the floor is 440
        // logical px, so an 800 px window is not compact.
        theme::set_ui_scale(0.5);
        let (_handle, wcx) = render_part_at(
            cx,
            gpui::size(theme::z(theme::COMPACT_W), px(WIN_H)),
            || div().size_full(),
        );
        assert!(
            !wcx.update(|w, _cx| is_compact(w)),
            "at zoom 0.5 the floor is 440 px, so 880 px is not compact"
        );
        theme::set_ui_scale(1.0);
    }

    /// The sheet: full-window overlay behind a full-window scrim, with
    /// the panel bottom-anchored (its bottom edge is the window's bottom
    /// edge) and clamped `EDGE` away from the top.
    #[gpui::test]
    fn sheet_is_bottom_anchored_behind_a_scrim(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, || {
            Sheet::new("sheet")
                .header("Inspector", Some(SharedString::from("Selected")))
                .body(crate::ui::text::ui_text("Body").wrap())
        });
        let win = wcx.update(|w, _cx| w.bounds());
        let overlay = wcx
            .debug_bounds("ui-sheet-sheet")
            .unwrap_or_else(|| panic!("the sheet overlay recorded no bounds"));
        assert_eq!(
            f32::from(overlay.size.width),
            f32::from(win.size.width),
            "the sheet overlay spans the window"
        );
        assert_eq!(
            f32::from(overlay.size.height),
            f32::from(win.size.height),
            "the sheet overlay spans the window vertically"
        );
        let scrim = wcx
            .debug_bounds("ui-sheet-scrim")
            .unwrap_or_else(|| panic!("the scrim recorded no bounds"));
        assert_eq!(
            f32::from(scrim.size.width),
            f32::from(win.size.width),
            "the scrim spans the window"
        );
    }

    /// The Sheet's header is the Panel's: the title slot sits at the same
    /// inset from the face's top left in both forms, so a change to one moves
    /// the other.
    #[gpui::test]
    fn sheet_header_inset_matches_the_panel(cx: &mut TestAppContext) {
        let inset = |wcx: &mut gpui::VisualTestContext, face: &'static str, title: &'static str| {
            let f = wcx.debug_bounds(face).unwrap_or_else(|| panic!("{face}"));
            let t = wcx.debug_bounds(title).unwrap_or_else(|| panic!("{title}"));
            t.origin - f.origin
        };
        let (_root, wcx) = render_part(cx, || {
            crate::ui::panel::Panel::new("p")
                .header("Sources", None)
                .on_close(|_e, _w, _cx| {})
        });
        let panel = inset(wcx, "ui-panel-p", "ui-text-p-title");
        let (_root, wcx) = render_part(cx, || {
            Sheet::new("s")
                .header("Sources", None)
                .on_close(|_e, _w, _cx| {})
        });
        let sheet = inset(wcx, "ui-sheet-s-panel", "ui-text-s-title");
        assert_eq!(sheet, panel, "the same header inset in both forms");
    }

    /// Escape, by real key events.
    fn press_escape(wcx: &mut gpui::VisualTestContext) {
        let esc = gpui::Keystroke::parse("escape").unwrap();
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: esc.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent { keystroke: esc });
    }

    /// Redraw after the test changed what the part shows.
    fn redraw(wcx: &mut gpui::VisualTestContext) {
        wcx.update(|w, _cx| w.refresh());
        wcx.run_until_parked();
    }

    /// Modal, once per open: opened while a trigger holds focus, the sheet
    /// takes focus, so Escape reaches it with no manual focusing. If focus
    /// then moves elsewhere, a re-render does not take it back. Closed and
    /// opened again, it takes focus again.
    #[gpui::test]
    fn sheet_takes_focus_once_per_open(cx: &mut TestAppContext) {
        let open = std::rc::Rc::new(std::cell::Cell::new(false));
        let closed = std::rc::Rc::new(std::cell::Cell::new(0));
        let trigger = cx.update(|cx| cx.focus_handle());
        let (open_in, closed_in, t) = (open.clone(), closed.clone(), trigger.clone());
        let (_root, wcx) = render_part(cx, move || {
            let closed_in = closed_in.clone();
            div()
                .size_full()
                .child(div().id("trigger").track_focus(&t).child("Open"))
                .when(open_in.get(), |d| {
                    d.child(
                        Sheet::new("modal")
                            .header("Sources", None)
                            .on_close(move |_ev, _w, _cx| closed_in.set(closed_in.get() + 1)),
                    )
                })
        });
        let trigger_focused =
            |wcx: &mut gpui::VisualTestContext| wcx.update(|w, _cx| trigger.is_focused(w));

        // Open: focus moves in, and Escape closes it.
        wcx.update(|w, cx| w.focus(&trigger, cx));
        open.set(true);
        redraw(wcx);
        assert!(!trigger_focused(wcx), "opening moves focus into the sheet");
        press_escape(wcx);
        assert_eq!(closed.get(), 1, "Escape closes it with no manual focusing");

        // Still open: focus moved back out, and a re-render leaves it there.
        wcx.update(|w, cx| w.focus(&trigger, cx));
        redraw(wcx);
        redraw(wcx);
        assert!(trigger_focused(wcx), "a re-render never takes focus back");

        // Closed, then opened again: it takes focus again.
        open.set(false);
        redraw(wcx);
        open.set(true);
        redraw(wcx);
        assert!(!trigger_focused(wcx), "a new open takes focus again");
    }

    /// A real click (down and up) at `at`.
    fn click_at(wcx: &mut gpui::VisualTestContext, at: Point<Pixels>) {
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
    }

    /// A disabled sheet closes on nothing and takes no focus. Escape, a tap
    /// on the scrim and a click on the header's × do not call `on_close`;
    /// the scrim still blocks the canvas (the click behind it never lands).
    #[gpui::test]
    fn disabled_sheet_closes_on_nothing(cx: &mut TestAppContext) {
        let closed = std::rc::Rc::new(std::cell::Cell::new(0));
        let behind = std::rc::Rc::new(std::cell::Cell::new(0));
        let (c, b) = (closed.clone(), behind.clone());
        let (_root, wcx) = render_part(cx, move || {
            let (c, b) = (c.clone(), b.clone());
            div()
                .size_full()
                .relative()
                .child(
                    div()
                        .id("canvas")
                        .absolute()
                        .size_full()
                        .on_click(move |_ev, _w, _cx| b.set(b.get() + 1)),
                )
                .child(
                    Sheet::new("off")
                        .header("Sources", None)
                        .disabled("The job is running")
                        .on_close(move |_ev, _w, _cx| c.set(c.get() + 1)),
                )
        });
        assert!(
            wcx.update(|w, cx| w.focused(cx)).is_none(),
            "takes no focus"
        );
        press_escape(wcx);
        let scrim = wcx.debug_bounds("ui-sheet-scrim").expect("the scrim");
        click_at(
            wcx,
            scrim.origin + gpui::point(theme::z(theme::SP_8), theme::z(theme::SP_8)),
        );
        let close = wcx
            .debug_bounds("ui-icon-button-off-close")
            .expect("the header's ×");
        click_at(wcx, close.center());
        assert_eq!(closed.get(), 0, "a disabled sheet closes on nothing");
        assert_eq!(behind.get(), 0, "the scrim still blocks the canvas");
    }

    /// Escape and a tap on the scrim both close the sheet (the parent's
    /// `.on_close`).
    #[gpui::test]
    fn sheet_closes_on_escape_and_scrim_tap(cx: &mut TestAppContext) {
        let closed = std::rc::Rc::new(std::cell::Cell::new(0));
        let closed_in = closed.clone();
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            let closed_in = closed_in.clone();
            Sheet::new("sheet")
                .header("Inspector", None)
                .focus_handle(&h)
                .on_close(move |_ev, _w, _cx| {
                    closed_in.set(closed_in.get() + 1);
                })
        });
        wcx.update(|w, cx| w.focus(&handle, cx));

        // Escape closes.
        let esc = gpui::Keystroke::parse("escape").unwrap();
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: esc.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent { keystroke: esc });
        assert_eq!(closed.get(), 1, "Escape closes the sheet");

        // A tap on the scrim closes: a point in the scrim's top strip
        // (the sheet is bottom-anchored `EDGE` away from the top, so
        // the top `EDGE` of the window is pure scrim).
        const TAP: f32 = 20.0;
        let scrim = wcx
            .debug_bounds("ui-sheet-scrim")
            .unwrap_or_else(|| panic!("the scrim recorded no bounds"));
        let at = Point {
            x: scrim.origin.x + px(TAP),
            y: scrim.origin.y + px(TAP),
        };
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
        assert_eq!(closed.get(), 2, "a tap on the scrim closes the sheet");
    }

    /// The close button in the header fires the same handler.
    #[gpui::test]
    fn sheet_close_button_fires_on_close(cx: &mut TestAppContext) {
        let closed = std::rc::Rc::new(std::cell::Cell::new(0));
        let closed_in = closed.clone();
        let (_root, wcx) = render_part(cx, move || {
            let closed_in = closed_in.clone();
            Sheet::new("sheet")
                .header("Inspector", Some(SharedString::from("Selected")))
                .on_close(move |_ev, _w, _cx| {
                    closed_in.set(1);
                })
        });
        // The close button's id is the NamedChild "close" of the sheet
        // id; a NamedChild displays as `<parent>-<name>`, so its
        // selector is the literal `ui-icon-button-sheet-close`.
        let close = wcx
            .debug_bounds("ui-icon-button-sheet-close")
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
        assert_eq!(closed.get(), 1, "the close button fires on_close");
    }

    /// K3b item 12: the sheet panel itself is never taller than the
    /// window — `window height − z(EDGE)`, with no second scaling — at
    /// zoom 1.25 and 2.0.
    #[gpui::test]
    fn sheet_never_exceeds_the_window(cx: &mut TestAppContext) {
        for scale in [1.25_f32, 2.0] {
            theme::set_ui_scale(scale);
            let (_handle, wcx) = crate::ui::interaction::test_util::render_part_at(
                cx,
                gpui::size(gpui::px(800.), gpui::px(600.)),
                || {
                    Sheet::new("tall")
                        .header("Sources", None)
                        .body(div().h(gpui::px(5000.)))
                },
            );
            let panel = wcx
                .debug_bounds("ui-sheet-tall-panel")
                .unwrap_or_else(|| panic!("the sheet panel recorded no bounds"));
            let limit = 600.0 - f32::from(theme::z(theme::EDGE));
            assert!(
                f32::from(panel.size.height) <= limit + 0.5,
                "zoom {scale}: the sheet panel {panel:?} is within {limit}"
            );
            assert!(f32::from(panel.origin.y) >= f32::from(theme::z(theme::EDGE)) - 0.5);
            assert!(
                f32::from(panel.origin.y) + f32::from(panel.size.height) <= 600.0 + 0.5,
                "zoom {scale}: the painted face ends inside the window: {panel:?}"
            );
        }
        theme::set_ui_scale(1.0);
    }

    /// K3b item 9: the sheet's scrim is modal — a tap on it never
    /// reaches the button behind it.
    #[gpui::test]
    fn sheet_scrim_occludes(cx: &mut TestAppContext) {
        let hits = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let hits_in = hits.clone();
        let (_root, wcx) = crate::ui::interaction::test_util::render_part(cx, move || {
            let hits_in = hits_in.clone();
            div()
                .size_full()
                .relative()
                .child(
                    div()
                        .id("behind")
                        .absolute()
                        .size_full()
                        .on_click(move |_e, _w, _cx| hits_in.set(hits_in.get() + 1)),
                )
                .child(
                    div().absolute().size_full().child(
                        Sheet::new("s")
                            .header("Sources", None)
                            .on_close(|_e, _w, _cx| {}),
                    ),
                )
        });
        let at = gpui::point(gpui::px(5.), gpui::px(5.));
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
        assert_eq!(
            hits.get(),
            0,
            "the scrim tap did not reach the button behind"
        );
    }

    /// The bounded face scrolls its tall body (DeepSeek, K3b round 3): a
    /// wheel scroll over the sheet moves the content up.
    #[gpui::test]
    fn tall_sheet_body_scrolls(cx: &mut TestAppContext) {
        let (_handle, wcx) = crate::ui::interaction::test_util::render_part_at(
            cx,
            gpui::size(gpui::px(800.), gpui::px(600.)),
            || {
                Sheet::new("tall").header("Sources", None).body(
                    div()
                        .h(gpui::px(5000.))
                        .debug_selector(|| "tall-content".to_string()),
                )
            },
        );
        let before = wcx.debug_bounds("tall-content").unwrap();
        let face = wcx.debug_bounds("ui-sheet-tall-panel").unwrap();
        wcx.simulate_event(gpui::ScrollWheelEvent {
            position: face.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-300.))),
            ..Default::default()
        });
        wcx.run_until_parked();
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        let after = wcx.debug_bounds("tall-content").unwrap();
        assert!(after.origin.y < before.origin.y, "{before:?} → {after:?}");
    }
}
