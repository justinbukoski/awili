//! The kit's shared interactive conventions (KIT.md v3, batch K2).
//!
//! Every interactive part (Button, IconButton, Chip, Row, Section, Tabs and
//! their sub-controls) carries a [`Common`] state block and the same
//! builder methods, expanded by the [`kit_shared`] macro:
//!
//! - `.selector(&'static str)` — the kit wrapper around `debug_selector`
//!   (a no-op in release builds; tests find parts by it). Every part sets a
//!   default selector derived from its `ElementId`, so a part without an
//!   explicit `.selector` is still selectable in tests.
//! - `.on_click(…)` — the parent's `cx.listener(…)`. The kit drops the
//!   second click of a double click (`click_count() > 1`) unless
//!   `.allow_double_click()` is set. Disabled parts never call it.
//! - `.disabled(reason)` — **no disabled without a reason**: reduced
//!   emphasis (`TEXT_DIM`, no fill), not clickable, skipped by the keyboard
//!   (not focusable, not a tab stop), and the reason becomes the tooltip.
//! - `.tooltip(text)` — through `icons::tip`. `.label(text)` — the
//!   accessible name; it is the tooltip when no tooltip is set (and the
//!   reason's tooltip when disabled).
//! - `.focused(bool)` — the parent's keyboard-focus flag, drawn as
//!   `ui::focus_ring` (the always-reserving 2 px `CONTRAST` outer box).
//!   Parts that receive GPUI's own keyboard focus also apply
//!   `focus_visible(ui::focus_ring_style)`.
//! - `.tab_index(isize)` — pass-through tab order.
//! - Enter/Space activation: the part's root is a **stateful focusable**
//!   div, and GPUI synthesizes a keyboard `ClickEvent` for Enter/Space on
//!   the focused element — so activation flows through the same
//!   `on_click` as the mouse, guard included.
//! - Geometry: the hit box is at least `theme::target()` in both
//!   directions (the painted face may be smaller, centred in it).
//!
//! One exception, declared in KIT.md § Conventions: `ScrollBar` keeps its
//! 20 px pointer target, because scrolling also works by wheel, trackpad
//! and keys.

use gpui::{ClickEvent, ElementId, SharedString, Window};

/// A kit part's click-handler signature (KIT.md § Conventions): the
/// parent's `cx.listener(…)`; fired by a mouse click, by a keyboard
/// click (Enter/Space on the focused part), or by the part's own
/// sub-control (a chip's ×, a tab's ×).
pub type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut gpui::App)>;

#[derive(Clone, Debug)]
pub struct Common {
    /// The stateful element id (`new(id, …)`): required for the focusable
    /// root (GPUI keeps the focus handle in the element's state) and the
    /// source of the default `debug_selector`.
    pub id: ElementId,
    /// `.selector(…)`: a `debug_selector` override (release builds: none).
    pub selector: Option<&'static str>,
    /// `.tooltip(…)`: the explicit tooltip text.
    pub tooltip: Option<SharedString>,
    /// `.label(…)`: the accessible name; the tooltip when none is set.
    pub label: Option<SharedString>,
    /// `.focused(…)`: the parent's keyboard-focus flag.
    pub focused: bool,
    /// `.tab_index(…)`: the tab order; `None` = default order.
    pub tab_index: Option<isize>,
    /// `.disabled(reason)`: `Some(reason)` ⇒ disabled; the reason is the
    /// tooltip. There is no disabled without a reason.
    pub disabled_reason: Option<SharedString>,
    /// `.allow_double_click()`: by default the kit drops the second click
    /// of a double click.
    pub allow_double_click: bool,
    /// `.hovered(…)`: the parent's hover flag (the part is a stateless
    /// `RenderOnce`, so hover state lives in the parent; the gallery pins
    /// it to show a frozen hover).
    pub hovered: bool,
    /// `.focus_handle(&…)`: the parent's focus handle; the part tracks it
    /// so the app's focus navigation (and the kit's keyboard tests) can
    /// focus it and Enter/Space activate it.
    pub focus_handle: Option<gpui::FocusHandle>,
    /// `.pointer_only()`: clickable but not a tab stop — a sub-control whose
    /// parent is the keyboard stop (a Stepper's − and +).
    pub pointer_only: bool,
}

impl Default for Common {
    fn default() -> Self {
        Self {
            id: ElementId::Name(SharedString::from("ui")),
            selector: None,
            tooltip: None,
            label: None,
            focused: false,
            tab_index: None,
            disabled_reason: None,
            allow_double_click: false,
            hovered: false,
            focus_handle: None,
            pointer_only: false,
        }
    }
}

impl Common {
    /// Whether the part is disabled (it carries a reason).
    pub fn is_disabled(&self) -> bool {
        self.disabled_reason.is_some()
    }

    /// Whether the frozen focus ring paints: `.focused(true)` on an enabled
    /// part. A disabled part takes no focus, so it never shows a ring.
    pub fn frozen_focus(&self) -> bool {
        self.focused && !self.is_disabled()
    }

    /// The effective tooltip: when the part is **disabled**, the disabled
    /// reason (ahead of any `.tooltip()` or `.label()` — a disabled part
    /// explains why it is dead, not what it is); otherwise the explicit
    /// tooltip, else the label.
    pub fn tooltip(&self) -> Option<SharedString> {
        if let Some(reason) = &self.disabled_reason {
            return Some(reason.clone());
        }
        self.tooltip.clone().or_else(|| self.label.clone())
    }

    /// The `debug_selector` text: `.selector(…)`, else a default derived
    /// from the part name and the element id.
    pub fn selector_for(&self, part: &str) -> String {
        self.selector
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("ui-{part}-{}", self.id))
    }

    /// Whether a click event should activate the part: the double-click
    /// guard — the second click of a double click is dropped unless the
    /// part opts out.
    pub fn activates_on(&self, ev: &ClickEvent) -> bool {
        self.allow_double_click || ev.click_count() <= 1
    }
}

/// Expand the shared kit-convention builder methods on an interactive part
/// type. The part must carry `common: Common`; its `new(id, …)` seeds the
/// element id. With `skip_label` the part provides its own `.label`
/// (Button's label is the visible text), so the shared one is skipped.
#[macro_export]
macro_rules! kit_shared {
    ($t:ty) => {
        $crate::kit_shared_common!($t);
        impl $t {
            /// The accessible name. Required for icon-only parts (checked
            /// at paint in debug builds); it is the tooltip when the part
            /// has no explicit one.
            pub fn label(mut self, text: impl Into<gpui::SharedString>) -> Self {
                self.common.label = Some(text.into());
                self
            }
        }
    };
    ($t:ty, skip_label) => {
        $crate::kit_shared_common!($t);
    };
}

/// The shared conventions except `.label` (see [`kit_shared`]).
#[macro_export]
macro_rules! kit_shared_common {
    ($t:ty) => {
        impl $t {
            /// The kit's `debug_selector` wrapper (KIT.md § Conventions):
            /// a no-op in release builds; tests find the part by it.
            pub fn selector(mut self, selector: &'static str) -> Self {
                self.common.selector = Some(selector);
                self
            }

            /// The explicit tooltip text (through `icons::tip` at paint).
            pub fn tooltip(mut self, text: impl Into<gpui::SharedString>) -> Self {
                self.common.tooltip = Some(text.into());
                self
            }

            /// The parent's keyboard-focus flag; draws `ui::focus_ring`
            /// (the always-reserving 2 px `CONTRAST` outer box).
            pub fn focused(mut self, focused: bool) -> Self {
                self.common.focused = focused;
                self
            }

            /// The tab order (`None` = the default order, which the part
            /// still joins — every enabled part is a tab stop).
            pub fn tab_index(mut self, index: isize) -> Self {
                self.common.tab_index = Some(index);
                self
            }

            /// Disable the part with its reason: reduced emphasis
            /// (`TEXT_DIM`, no fill), not clickable, skipped by the
            /// keyboard, and the reason shown in the tooltip. There is no
            /// disabled without a reason.
            pub fn disabled(mut self, reason: impl Into<gpui::SharedString>) -> Self {
                self.common.disabled_reason = Some(reason.into());
                self
            }

            /// Disabled when `reason` is `Some` — how a container (a
            /// `SettingRow` with an unmet dependency, a disabled row) passes
            /// its reason down, so the control itself is dimmed, inert and
            /// off the Tab order (a part cannot switch off an arbitrary
            /// child from outside). `None` leaves the part as it is.
            pub fn disabled_if(mut self, reason: Option<gpui::SharedString>) -> Self {
                if let Some(reason) = reason {
                    self.common.disabled_reason = Some(reason);
                }
                self
            }

            /// Clickable but not a tab stop (a sub-control whose parent is
            /// the keyboard stop).
            pub fn pointer_only(mut self) -> Self {
                self.common.pointer_only = true;
                self
            }

            /// The kit's double-click guard on or off (a container passes
            /// its own setting down to its buttons).
            pub fn allow_double_click_if(mut self, allow: bool) -> Self {
                self.common.allow_double_click = allow;
                self
            }

            /// Let the second click of a double click through. The kit's
            /// default is to drop it.
            pub fn allow_double_click(mut self) -> Self {
                self.common.allow_double_click = true;
                self
            }

            /// The parent's hover flag (frozen hover for the gallery;
            /// otherwise the parent tracks `on_hover` and re-renders).
            pub fn hovered(mut self, hovered: bool) -> Self {
                self.common.hovered = hovered;
                self
            }

            /// The parent's focus handle: the part tracks it (GPUI's
            /// keyboard focus), so the app's focus navigation can reach
            /// it and Enter/Space activate it through the same handler.
            /// An enabled part makes the handle a tab stop (with its
            /// `.tab_index(…)`, if set), so Tab reaches it without the
            /// parent marking it; a disabled part is never registered,
            /// however the handle is marked.
            pub fn focus_handle(mut self, handle: &gpui::FocusHandle) -> Self {
                self.common.focus_handle = Some(handle.clone());
                self
            }

            /// The shared interactive state (read by the part's tests).
            pub fn common(&self) -> &$crate::ui::interaction::Common {
                &self.common
            }

            /// Whether the part is disabled (carries a reason).
            pub fn is_disabled(&self) -> bool {
                self.common.is_disabled()
            }

            /// The accessible name, if set.
            pub fn label_text(&self) -> Option<gpui::SharedString> {
                self.common.label.clone()
            }

            /// The effective tooltip: when disabled, the reason (ahead of
            /// everything); otherwise the explicit tooltip, else the
            /// label.
            pub fn tooltip_text(&self) -> Option<gpui::SharedString> {
                self.common.tooltip()
            }
        }
    };
}

/// A palette colour with a new alpha (a kit part fades a palette colour
/// — the tab strip's scroll edge — by rescaling the alpha of a token read
/// from `theme::palette()`, never a raw colour).
pub(crate) fn with_alpha(color: gpui::Rgba, alpha: f32) -> gpui::Rgba {
    gpui::Hsla::from(color).alpha(alpha).into()
}

/// The `ElementId` of a named child of `parent` (the chip's text, the × of
/// a removable chip, the + of a tab strip, the close × of a panel, the dot
/// of a card …): unique
/// under the parent's id, so an embedded stateful element keeps its own
/// id (KIT.md § Long text: a truncating `ui::text` or a meaning-carrying
/// `Dot` shows its full text / label as a tooltip only with an id).
pub(crate) fn child_id(parent: &gpui::ElementId, tag: &str) -> gpui::ElementId {
    gpui::ElementId::NamedChild(std::sync::Arc::new(parent.clone()), tag.into())
}

/// The keyboard wiring of an **enabled** part's root (KIT.md §
/// Conventions; disabled parts never call it, so they are not tab stops).
/// A parent's tracked handle is made a tab stop here — `.focusable()` or
/// `track_focus` alone is not one in this GPUI fork — with the part's
/// `.tab_index(…)` when set; without a handle the root is focusable with a
/// default tab index (a parent `.tab_index(…)` overrides it).
///
/// GPUI keeps a handle's tab-stop flag and index in shared focus state,
/// so this marks the parent's handle itself: give each part its own
/// handle (one handle per tab stop, as GPUI expects).
pub(crate) fn focus_wiring(
    root: gpui::Stateful<gpui::Div>,
    common: &Common,
) -> gpui::Stateful<gpui::Div> {
    use gpui::{InteractiveElement, StatefulInteractiveElement};
    if common.pointer_only {
        // Clickable, never a tab stop: the parent is the keyboard stop.
        return root;
    }
    match common.focus_handle.clone() {
        Some(handle) => {
            let handle = handle.tab_stop(true);
            let handle = match common.tab_index {
                Some(index) => handle.tab_index(index),
                None => handle,
            };
            root.track_focus(&handle)
        }
        None => root.focusable().tab_index(common.tab_index.unwrap_or(0)),
    }
}

/// The group name a part's face registers so its children's
/// `group_hover` (the brighten rule: text and glyphs lift on real pointer
/// hover) resolves to the nearest enclosing part.
pub(crate) const HOVER_GROUP: &str = "ui-hover";

/// A test-only, layout-neutral probe: at paint it records the text colour
/// GPUI actually resolved for its parent (inherited through the parent's
/// style, including any `.hover()` / `.group_hover()` refinement) under
/// `key`, so the hover tests check paint, not a re-render. Release and
/// non-test builds get nothing.
pub(crate) fn paint_probe(key: &gpui::ElementId) -> Option<gpui::AnyElement> {
    #[cfg(test)]
    {
        use gpui::{IntoElement, Styled};
        let key = key.to_string();
        Some(
            gpui::canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    test_util::record_paint(&key, window.text_style().color);
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        )
    }
    #[cfg(not(test))]
    {
        let _ = key;
        None
    }
}

/// The part-name tags for the default `debug_selector`s, so a part built
/// with `.id("submit")` answers to `ui-button-submit` in a test.
pub(crate) const BUTTON: &str = "button";
pub(crate) const ICON_BUTTON: &str = "icon-button";
pub(crate) const CHIP: &str = "chip";
pub(crate) const ROW: &str = "row";
pub(crate) const SECTION: &str = "section";
pub(crate) const TAB: &str = "tab";
pub(crate) const TABS: &str = "tabs";
pub(crate) const TAB_NEW: &str = "tab-new";
pub(crate) const TAB_CLOSE: &str = "tab-close";
pub(crate) const SCROLLBAR: &str = "scrollbar";
pub(crate) const PANEL: &str = "panel";
pub(crate) const PANEL_CLOSE: &str = "panel-close";
pub(crate) const PILL: &str = "pill";
pub(crate) const POPOVER: &str = "popover";
pub(crate) const POPOVER_SCRIM: &str = "popover-scrim";
pub(crate) const POPOVER_PANEL: &str = "popover-panel";
pub(crate) const MENU: &str = "menu";
pub(crate) const MENU_SEP: &str = "menu-sep";
pub(crate) const PALETTE: &str = "palette";
pub(crate) const PALETTE_FIELD: &str = "palette-field";
pub(crate) const DIALOG: &str = "dialog";
pub(crate) const DIALOG_PRIMARY: &str = "dialog-primary";
pub(crate) const DIALOG_CANCEL: &str = "dialog-cancel";
pub(crate) const DIALOG_SECONDARY: &str = "dialog-secondary";
pub(crate) const SHEET: &str = "sheet";
pub(crate) const SHEET_SCRIM: &str = "sheet-scrim";
pub(crate) const NOTICE: &str = "notice";
pub(crate) const NOTICE_CLOSE: &str = "notice-close";
pub(crate) const NOTICE_PREF: &str = "notice-pref";
pub(crate) const NOTICE_STACK: &str = "notice-stack";

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{ClickEvent, ParentElement, Styled};

    /// A mouse click event with a given click count (1 = first, 2 = the
    /// second click of a double click).
    fn mouse_click(click_count: usize) -> ClickEvent {
        let down = gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: gpui::point(gpui::Pixels::ZERO, gpui::Pixels::ZERO),
            modifiers: Default::default(),
            click_count,
            first_mouse: false,
        };
        let up = gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: down.position,
            modifiers: down.modifiers,
            click_count,
        };
        ClickEvent::Mouse(gpui::MouseClickEvent { down, up })
    }

    /// A keyboard activation (Enter/Space on the focused part).
    fn keyboard_click() -> ClickEvent {
        ClickEvent::Keyboard(Default::default())
    }

    /// The guard: the second click of a double click is dropped, the first
    /// and keyboard activations pass — and `.allow_double_click()` lets the
    /// second through.
    #[test]
    fn double_click_guard_drops_the_second_click() {
        let c = Common::default();
        assert!(c.activates_on(&mouse_click(1)), "the first click activates");
        assert!(
            !c.activates_on(&mouse_click(2)),
            "the second click of a double click is dropped"
        );
        assert!(
            c.activates_on(&keyboard_click()),
            "Enter/Space activations are single clicks"
        );
        let c = Common {
            allow_double_click: true,
            ..Common::default()
        };
        assert!(
            c.activates_on(&mouse_click(2)),
            "the opt-out lets it through"
        );
    }

    /// The tooltip precedence when the part is **disabled**: the reason
    /// wins, ahead of any explicit tooltip or label (KIT.md § Conventions:
    /// a disabled part shows its reason in the tooltip).
    #[test]
    fn disabled_reason_wins_the_tooltip() {
        let c = Common {
            label: Some("Send".into()),
            tooltip: Some("Explicit".into()),
            disabled_reason: Some("The model is offline".into()),
            ..Common::default()
        };
        assert_eq!(
            c.tooltip().as_deref(),
            Some("The model is offline"),
            "the disabled reason wins, ahead of the tooltip and the label"
        );
        let c = Common {
            label: Some("Send".into()),
            disabled_reason: Some("Offline".into()),
            ..Common::default()
        };
        assert_eq!(
            c.tooltip().as_deref(),
            Some("Offline"),
            "a labelled part does not hide the reason"
        );
    }

    /// The tooltip precedence when the part is **enabled**: explicit
    /// tooltip > label > (no reason to show).
    #[test]
    fn enabled_tooltip_precedence_is_tooltip_then_label() {
        let mut c = Common::default();
        assert!(c.tooltip().is_none());
        c.label = Some("Send".into());
        assert_eq!(
            c.tooltip().as_deref(),
            Some("Send"),
            "the label is the tooltip"
        );
        c.tooltip = Some("Explicit".into());
        assert_eq!(
            c.tooltip().as_deref(),
            Some("Explicit"),
            "…the explicit one wins"
        );
    }

    /// Disabled is not a tab stop (KIT.md § Conventions): in this GPUI
    /// fork `.focusable()` alone does not create a tab stop, so enabled
    /// interactive parts get a tab index by default and a disabled part
    /// is never registered — Tab skips it and reaches the enabled one.
    #[gpui::test]
    fn tab_navigation_skips_disabled_parts(cx: &mut gpui::TestAppContext) {
        use crate::ui::button::Button;

        // The parent owns the enabled button's focus handle — a plain
        // `cx.focus_handle()`; the part makes it a tab stop (K2c).
        let h_on = cx.update(|cx| cx.focus_handle());
        let h_in = h_on.clone();
        let (_root, wcx) = crate::ui::interaction::test_util::render_part(cx, move || {
            let h_in = h_in.clone();
            gpui::div()
                .flex()
                .flex_col()
                // Paint order: disabled, disabled (with an explicit
                // .tab_index — the K2 bug applied it unconditionally),
                // enabled.
                .child(Button::new("off", "Stop").disabled("The job is running"))
                .child(
                    Button::new("off2", "Pause")
                        .disabled("The job is running")
                        .tab_index(2),
                )
                .child(Button::new("on", "Run").focus_handle(&h_in))
        });

        // From no focus, Tab lands on the first tab stop: the disabled
        // parts are not registered at all (no focusable, no tab index),
        // so the focus lands on the enabled button.
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            Some(h_on.clone()),
            "Tab skips the disabled parts and reaches the enabled one"
        );
        // Tab again: the only tab stop is the enabled button, so the focus
        // wraps back to it.
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            Some(h_on),
            "…and stays with the enabled one"
        );
    }

    /// The default selector derives from the part name and the element id.
    #[test]
    fn default_selector_comes_from_the_part_and_id() {
        let c = Common {
            id: ElementId::from("submit"),
            ..Common::default()
        };
        assert_eq!(c.selector_for("button"), "ui-button-submit");
        let c = Common {
            selector: Some("send-btn"),
            ..c.clone()
        };
        assert_eq!(c.selector_for("button"), "send-btn");
    }
}

/// (tests) View plumbing for the kit's render tests.
///
/// Kit parts carry their accessible label (or the disabled reason) as a
/// tooltip, and GPUI's tooltip handlers resolve `Window::current_view`
/// while painting — and element click/keyboard listeners only dispatch
/// through the window's committed frame — so the render tests draw the
/// part as the **root view of a real test window**
/// (`TestAppContext::add_window_view`, through [`render_part`]), not
/// through a bare `VisualTestContext::draw`.
#[cfg(test)]
pub mod test_util {
    use gpui::{AnyElement, Context, Entity, IntoElement, Render, VisualTestContext};
    use std::ops::Deref;

    /// A view that re-builds its part from a factory on every frame (the
    /// window re-renders on every simulated input).
    pub struct PartView {
        factory: Box<dyn Fn() -> AnyElement>,
    }

    impl Render for PartView {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut Context<Self>,
        ) -> impl IntoElement {
            (self.factory)()
        }
    }

    /// Open a test window whose root view renders `factory()`.
    /// Returns the window context (for `debug_bounds` / `simulate_event`)
    /// and the root view handle.
    pub fn render_part<E, F>(
        cx: &mut gpui::TestAppContext,
        factory: F,
    ) -> (Entity<PartView>, &mut VisualTestContext)
    where
        E: IntoElement + 'static,
        F: Fn() -> E + 'static,
    {
        cx.add_window_view(|_w, _cx| PartView {
            factory: Box::new(move || factory().into_any_element()),
        })
    }

    /// Like [`render_part`], but the window is created at `size` (the
    /// part tests for compact windows and popover placement open a
    /// window at the size under test instead of resizing it). Returns
    /// the window's handle (for `simulate_window_resize`) and its
    /// context.
    pub fn render_part_at<E, F>(
        cx: &mut gpui::TestAppContext,
        size: gpui::Size<gpui::Pixels>,
        factory: F,
    ) -> (gpui::AnyWindowHandle, &mut VisualTestContext)
    where
        E: IntoElement + 'static,
        F: Fn() -> E + 'static,
    {
        let window = cx.open_window(size, |_w, _cx| PartView {
            factory: Box::new(move || factory().into_any_element()),
        });
        let handle = *window.deref();
        let wcx = VisualTestContext::from_window(handle, cx).into_mut();
        wcx.run_until_parked();
        (handle, wcx)
    }

    thread_local! {
        static PAINTED: std::cell::RefCell<std::collections::HashMap<String, gpui::Hsla>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }

    /// Called by [`super::paint_probe`] at paint.
    pub fn record_paint(key: &str, color: gpui::Hsla) {
        PAINTED.with(|m| m.borrow_mut().insert(key.to_string(), color));
    }

    /// The text colour last painted under the probe `key` (a part's
    /// `child_id(id, tag)` rendered with `to_string()`). Keys are element
    /// ids, so a test gives each part under test its own id (as GPUI
    /// itself expects); the map is per test thread.
    pub fn painted(key: &gpui::ElementId) -> Option<gpui::Rgba> {
        PAINTED.with(|m| m.borrow().get(&key.to_string()).map(|c| (*c).into()))
    }

    /// Move the pointer to the centre of the part `selector` and let the
    /// window re-render (real GPUI hover).
    pub fn hover(wcx: &mut VisualTestContext, selector: &str) {
        let key: &'static str = Box::leak(selector.to_string().into_boxed_str());
        let at = wcx
            .debug_bounds(key)
            .unwrap_or_else(|| panic!("{selector} recorded no bounds"))
            .center();
        wcx.simulate_event(gpui::MouseMoveEvent {
            position: at,
            pressed_button: None,
            modifiers: gpui::Modifiers::default(),
        });
        wcx.run_until_parked();
    }

    /// The tooltip GPUI actually opens when the pointer rests on the
    /// centre of `selector` (the innermost element with a tooltip under
    /// the pointer wins, exactly as on screen): hover, wait out GPUI's
    /// 500 ms show delay, and read the label the tooltip builder ran with.
    pub fn tooltip_at(wcx: &mut VisualTestContext, selector: &str) -> Option<String> {
        let key: &'static str = Box::leak(selector.to_string().into_boxed_str());
        let at = wcx
            .debug_bounds(key)
            .unwrap_or_else(|| panic!("{selector} recorded no bounds"))
            .center();
        tooltip_at_point(wcx, at)
    }

    /// Like [`tooltip_at`], at a window point (a part inside a part).
    pub fn tooltip_at_point(
        wcx: &mut VisualTestContext,
        at: gpui::Point<gpui::Pixels>,
    ) -> Option<String> {
        unhover(wcx);
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
        let tip = crate::icons::take_shown_tip().map(|s| s.to_string());
        unhover(wcx);
        tip
    }

    /// Move the pointer to the window's top-left corner, off every part.
    pub fn unhover(wcx: &mut VisualTestContext) {
        wcx.simulate_event(gpui::MouseMoveEvent {
            position: gpui::point(gpui::px(1.0), gpui::px(1.0)),
            pressed_button: None,
            modifiers: gpui::Modifiers::default(),
        });
        wcx.run_until_parked();
    }

    /// A real double click (two presses, click_count 1 then 2) at `at`.
    pub fn double_click(wcx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>) {
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

    /// A real double click at the centre of `selector`.
    pub fn double_click_on(wcx: &mut VisualTestContext, selector: &str) {
        let key: &'static str = Box::leak(selector.to_string().into_boxed_str());
        let at = wcx
            .debug_bounds(key)
            .unwrap_or_else(|| panic!("{selector} recorded no bounds"))
            .center();
        double_click(wcx, at);
    }

    /// Close enough for colours that went through `Hsla` and back.
    pub fn same_color(a: gpui::Rgba, b: gpui::Rgba) -> bool {
        (a.r - b.r).abs() < 0.01
            && (a.g - b.g).abs() < 0.01
            && (a.b - b.b).abs() < 0.01
            && (a.a - b.a).abs() < 0.01
    }
}
