//! `ui::Stepper` — the kit's numeric stepper (KIT.md v3, batch K4).
//!
//! − value +: two `IconButton.quiet` (`Minus` / `Plus`, a 44 px hit
//! each) around the value in mono `type_::UI` (tabular, centred, a 64 px
//! floor — the v1 settings' own stepper geometry). At the bounds the step
//! button is disabled with its reason ("At the minimum" / "At the
//! maximum" — no disabled without a reason).
//!
//! The part reports the **new value**: `value ± step`, clamped to
//! `[min, max]`, through `.on_change(…)`; the parent stores it and
//! re-renders. **Holding** a button repeats the step (after
//! [`HOLD_DELAY`], every [`HOLD_EVERY`]) until the release; each repeat
//! builds on the last value it reported, so it never waits on the
//! parent's re-render.
//!
//! The stepper itself is one tab stop (`focus_wiring`): Up/Right step
//! up, Down/Left step down. `.unit(text)` appends the unit to the value
//! ("0.6 s"); `.format(f)` replaces the display entirely (the parent's
//! `settings_catalog::display` face) and wins over `.unit`. Disabled
//! dims the value and makes the reason the tooltip.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    div, prelude::*, App, ClickEvent, ElementId, IntoElement, KeyDownEvent, KeyboardClickEvent,
    RenderOnce, SharedString, Window,
};

use crate::icons::Icon;
use crate::theme::{self, palette};
use crate::ui::focus_ring::{focus_ring, focus_ring_style};
use crate::ui::icon_button::IconButton;
use crate::ui::interaction::{child_id, focus_wiring, Common};
use crate::ui::text;

/// The change handler: the new value (cloned into every listener).
pub type ChangeHandler = Rc<dyn Fn(i64, &ClickEvent, &mut Window, &mut App)>;

/// The part tag for the default `debug_selector`.
const STEPPER: &str = "stepper";
/// The value's minimum width (the v1 settings' 64 px floor), logical px
/// before zoom.
const VALUE_W: f32 = theme::STEPPER_VALUE_W;
/// How long a button is held before it starts repeating.
pub const HOLD_DELAY: Duration = Duration::from_millis(theme::motion::HOLD_DELAY_MS);
/// The repeat interval while held.
pub const HOLD_EVERY: Duration = Duration::from_millis(theme::motion::HOLD_EVERY_MS);
/// The disabled reasons at the bounds (no disabled without a reason).
pub const AT_MINIMUM: &str = "At the minimum";
pub const AT_MAXIMUM: &str = "At the maximum";

/// A kit numeric stepper. Build with [`Stepper::new`] and chain
/// `.unit(…)` / `.format(…)`, `.on_change(…)` and the shared
/// conventions; the parent owns the value.
#[derive(IntoElement)]
pub struct Stepper {
    common: Common,
    value: i64,
    min: i64,
    max: i64,
    step: i64,
    unit: Option<SharedString>,
    format: Option<Box<dyn Fn(i64) -> SharedString + 'static>>,
    on_change: Option<ChangeHandler>,
}

impl Stepper {
    /// A stepper over `[min, max]` in `step` increments, showing
    /// `value`.
    pub fn new(id: impl Into<ElementId>, value: i64, min: i64, max: i64, step: i64) -> Self {
        Self {
            common: Common {
                id: id.into(),
                ..Default::default()
            },
            value,
            min,
            max,
            step: step.max(1),
            unit: None,
            format: None,
            on_change: None,
        }
    }

    /// The value's unit (appended to the value: `" s"` → `0.6 s`).
    pub fn unit(mut self, unit: impl Into<SharedString>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    /// The value's display override (the parent's formatter — e.g. the
    /// v1 `settings_catalog::display` face); it wins over `.unit`.
    pub fn format(mut self, f: impl Fn(i64) -> SharedString + 'static) -> Self {
        self.format = Some(Box::new(f));
        self
    }

    /// The change handler: the new value (`value ± step`, clamped),
    /// fired by a click, by each hold repeat and by the arrow keys.
    pub fn on_change(
        mut self,
        f: impl Fn(i64, &ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(f));
        self
    }

    /// The value the part paints.
    pub fn value(&self) -> i64 {
        self.value
    }

    /// The value one step up (`up`) or down from `value`, clamped.
    pub fn stepped(&self, value: i64, up: bool) -> i64 {
        let next = if up {
            value.saturating_add(self.step)
        } else {
            value.saturating_sub(self.step)
        };
        next.clamp(self.min, self.max)
    }

    /// The value's painted display (`.format` wins over `.unit`).
    pub fn display(&self) -> SharedString {
        if let Some(f) = &self.format {
            return f(self.value);
        }
        match &self.unit {
            Some(unit) => format!("{}{unit}", self.value).into(),
            None => self.value.to_string().into(),
        }
    }

    /// Whether the − button is at its bound (disabled).
    pub fn at_minimum(&self) -> bool {
        self.value <= self.min
    }

    /// Whether the + button is at its bound (disabled).
    pub fn at_maximum(&self) -> bool {
        self.value >= self.max
    }
}

crate::kit_shared!(Stepper);

impl std::fmt::Debug for Stepper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Stepper({})", self.common.selector_for(STEPPER))
    }
}

/// The hold-to-repeat state, kept under the stepper's id: the latest
/// value (the parent's at render, then each repeat's), which hold is
/// live (its generation), and whether a button is held.
#[derive(Default)]
struct Hold {
    value: Cell<i64>,
    generation: Cell<u64>,
    held: Cell<bool>,
}

impl RenderOnce for Stepper {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette();
        let dec_at_bound = self.at_minimum();
        let inc_at_bound = self.at_maximum();
        let display = self.display();
        let (min, max, step) = (self.min, self.max, self.step);
        let common = self.common;
        let selector = common.selector_for(STEPPER);
        let disabled = common.is_disabled();
        let on_change = self.on_change.clone();
        let allow_double = common.allow_double_click;
        let hold = window
            .use_keyed_state(child_id(&common.id, "hold"), cx, |_, _| {
                Rc::new(Hold::default())
            })
            .read(cx)
            .clone();
        // Disabling ends a live hold (its repeat stops at the next tick).
        if disabled {
            hold.held.set(false);
        }
        if !hold.held.get() {
            hold.value.set(self.value);
        }
        let stepped = move |value: i64, up: bool| -> i64 {
            let next = if up {
                value.saturating_add(step)
            } else {
                value.saturating_sub(step)
            };
            next.clamp(min, max)
        };

        // One step button: a quiet IconButton (the bound or the part's
        // reason when disabled), wrapped so a press can start the hold.
        let button = |up: bool, at_bound: bool| -> gpui::AnyElement {
            let (tag, icon, label, bound_reason) = if up {
                ("inc", Icon::Plus, "Increase", AT_MAXIMUM)
            } else {
                ("dec", Icon::Minus, "Decrease", AT_MINIMUM)
            };
            // Pointer-only: the stepper itself is the one tab stop (arrow
            // keys); the buttons honour the stepper's double-click setting.
            let mut b = IconButton::new(child_id(&common.id, tag), icon)
                .label(label)
                .quiet()
                .pointer_only()
                .allow_double_click_if(allow_double);
            let reason = common
                .disabled_reason
                .clone()
                .or_else(|| at_bound.then(|| SharedString::from(bound_reason)));
            match (reason, on_change.clone()) {
                (Some(reason), _) => b.disabled(reason).into_any_element(),
                (None, None) => b.into_any_element(),
                (None, Some(on_change)) => {
                    let on_click = on_change.clone();
                    let hold_click = hold.clone();
                    b = b.on_click(move |ev, window, cx| {
                        if allow_double || ev.click_count() <= 1 {
                            let next = stepped(hold_click.value.get(), up);
                            hold_click.value.set(next);
                            on_click(next, ev, window, cx);
                        }
                    });
                    let hold_down = hold.clone();
                    let hold_up = hold.clone();
                    let hold_out = hold.clone();
                    div()
                        .id(child_id(&common.id, &format!("{tag}-hold")))
                        .on_mouse_down(gpui::MouseButton::Left, move |_ev, window, cx| {
                            let generation = hold_down.generation.get() + 1;
                            hold_down.generation.set(generation);
                            hold_down.held.set(true);
                            let hold = hold_down.clone();
                            let on_change = on_change.clone();
                            window
                                .spawn(cx, async move |cx| {
                                    let mut wait = HOLD_DELAY;
                                    loop {
                                        cx.background_executor().timer(wait).await;
                                        if !hold.held.get() || hold.generation.get() != generation {
                                            break;
                                        }
                                        let next = stepped(hold.value.get(), up);
                                        if next == hold.value.get() {
                                            break;
                                        }
                                        hold.value.set(next);
                                        let ok = cx.update(|window, cx| {
                                            on_change(next, &ClickEvent::default(), window, cx);
                                        });
                                        if ok.is_err() {
                                            break;
                                        }
                                        wait = HOLD_EVERY;
                                    }
                                })
                                .detach();
                        })
                        .on_mouse_up(gpui::MouseButton::Left, move |_ev, _window, _cx| {
                            hold_up.held.set(false);
                        })
                        .on_mouse_up_out(gpui::MouseButton::Left, move |_ev, _window, _cx| {
                            hold_out.held.set(false);
                        })
                        .child(b)
                        .into_any_element()
                }
            }
        };
        let dec = button(false, dec_at_bound);
        let inc = button(true, inc_at_bound);

        let mut value_text = text::ui_text(display)
            .mono()
            .text_color_override(if disabled { p.text_dim } else { p.text });
        if !disabled {
            value_text = value_text.id(child_id(&common.id, "value-text"));
        }

        let mut root = div()
            .id(common.id.clone())
            .flex()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .debug_selector(|| selector.clone())
            .child(dec)
            .child(
                div()
                    .id(child_id(&common.id, "value"))
                    .debug_selector(|| format!("{selector}-value"))
                    .flex()
                    .justify_center()
                    .min_w(theme::z(VALUE_W))
                    .child(value_text),
            )
            .child(inc);

        if !disabled {
            root = focus_wiring(root, &common)
                .when(!common.focused, |d| d.focus_visible(focus_ring_style));
            if let Some(on_change) = on_change {
                let hold_keys = hold.clone();
                root = root.on_key_down(move |ev: &KeyDownEvent, window, cx| {
                    let up = match ev.keystroke.key.as_str() {
                        "up" | "right" => true,
                        "down" | "left" => false,
                        _ => return,
                    };
                    let next = stepped(hold_keys.value.get(), up);
                    if next != hold_keys.value.get() {
                        window.prevent_default();
                        cx.stop_propagation();
                        hold_keys.value.set(next);
                        on_change(
                            next,
                            &ClickEvent::Keyboard(KeyboardClickEvent::default()),
                            window,
                            cx,
                        );
                    }
                });
            }
        }
        if let Some(tip) = common.tooltip() {
            root = root.tooltip(crate::icons::tip(tip.to_string()));
        }
        focus_ring(root, common.frozen_focus())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    fn sample() -> Stepper {
        Stepper::new("num", 3, 0, 9, 2).label("Retries")
    }

    fn press(wcx: &mut gpui::VisualTestContext, sel: &'static str, down: bool, count: usize) {
        let at = wcx
            .debug_bounds(sel)
            .unwrap_or_else(|| panic!("{sel} recorded no bounds"))
            .center();
        if down {
            wcx.simulate_event(gpui::MouseDownEvent {
                button: gpui::MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::default(),
                click_count: count,
                first_mouse: false,
            });
        } else {
            wcx.simulate_event(gpui::MouseUpEvent {
                button: gpui::MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::default(),
                click_count: count,
            });
        }
        wcx.run_until_parked();
    }

    /// The stepper lays out headless with the − / value / + faces; the
    /// step buttons are `target()` hits; at a bound the button is
    /// disabled with its reason (the tooltip that actually opens).
    #[gpui::test]
    fn stepper_renders_and_bounds_disable(cx: &mut TestAppContext) {
        assert!(Stepper::new("min", 0, 0, 5, 1).at_minimum());
        assert!(Stepper::new("max", 5, 0, 5, 1).at_maximum());
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .pt(theme::z(theme::SP_40))
                .child(sample())
                .child(Stepper::new("lo", 0, 0, 5, 1))
        });
        for sel in ["ui-icon-button-num-dec", "ui-icon-button-num-inc"] {
            let b = wcx.debug_bounds(sel).unwrap();
            assert!(
                f32::from(b.size.width) >= 44.0 && f32::from(b.size.height) >= 44.0,
                "{sel}: {b:?}"
            );
        }
        let value = wcx.debug_bounds("ui-stepper-num-value").unwrap();
        assert!(f32::from(value.size.width) >= f32::from(theme::z(VALUE_W)));
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at(wcx, "ui-icon-button-lo-dec").as_deref(),
            Some(AT_MINIMUM)
        );
    }

    /// K4b: a click reports the new value — `value ± step`, clamped
    /// (the constructor's `step` is used); the double-click guard drops
    /// the second click.
    #[gpui::test]
    fn stepper_reports_the_stepped_value(cx: &mut TestAppContext) {
        let hits = Rc::new(std::cell::RefCell::new(Vec::new()));
        let h = hits.clone();
        let (_root, wcx) = render_part(cx, move || {
            let h = h.clone();
            div()
                .flex()
                .child(sample().on_change(move |v, _e, _w, _c| h.borrow_mut().push(v)))
        });
        for count in [1usize, 2] {
            press(wcx, "ui-icon-button-num-inc", true, count);
            press(wcx, "ui-icon-button-num-inc", false, count);
        }
        assert_eq!(
            &*hits.borrow(),
            &[5],
            "one step of 2, the double click dropped"
        );
        let s = sample();
        assert_eq!(s.stepped(8, true), 9, "clamped at the max");
        assert_eq!(s.stepped(1, false), 0, "clamped at the min");
    }

    /// K4b (Codex P1): holding a button repeats after the hold delay,
    /// building on its own last value, and stops on release.
    #[gpui::test]
    fn stepper_hold_repeats_until_release(cx: &mut TestAppContext) {
        let hits = Rc::new(std::cell::RefCell::new(Vec::new()));
        let h = hits.clone();
        let (_root, wcx) = render_part(cx, move || {
            let h = h.clone();
            div().flex().child(
                Stepper::new("hold", 0, 0, 100, 1)
                    .on_change(move |v, _e, _w, _c| h.borrow_mut().push(v)),
            )
        });
        press(wcx, "ui-icon-button-hold-inc", true, 1);
        wcx.executor()
            .advance_clock(HOLD_DELAY + HOLD_EVERY * 3 + Duration::from_millis(10));
        wcx.run_until_parked();
        let held = hits.borrow().clone();
        assert!(held.len() >= 3, "the hold repeated: {held:?}");
        assert!(
            held.windows(2).all(|w| w[1] == w[0] + 1),
            "each repeat builds on the last: {held:?}"
        );
        press(wcx, "ui-icon-button-hold-inc", false, 1);
        let after_release = hits.borrow().len();
        wcx.executor().advance_clock(HOLD_EVERY * 5);
        wcx.run_until_parked();
        assert_eq!(hits.borrow().len(), after_release, "the release stopped it");
    }

    /// K4b: the stepper is one tab stop (a plain tracked handle) with
    /// arrow keys; disabled, it is not a tab stop, fires nothing, dims
    /// and shows its reason.
    #[gpui::test]
    fn stepper_keyboard_and_disabled(cx: &mut TestAppContext) {
        let hits = Rc::new(std::cell::RefCell::new(Vec::new()));
        let h = hits.clone();
        let handle = cx.update(|cx| cx.focus_handle());
        let off = cx.update(|cx| cx.focus_handle());
        let (hh, ho) = (handle.clone(), off.clone());
        let (_root, wcx) = render_part(cx, move || {
            let h = h.clone();
            div()
                .flex()
                .flex_col()
                .pt(theme::z(theme::SP_40))
                .child(
                    Stepper::new("off", 3, 0, 9, 1)
                        .focus_handle(&ho)
                        .tab_index(0)
                        .disabled("Locked by policy"),
                )
                .child(
                    sample()
                        .focus_handle(&hh)
                        .on_change(move |v, _e, _w, _c| h.borrow_mut().push(v)),
                )
        });
        wcx.update(|w, cx| w.focus(&handle, cx));
        wcx.simulate_keystrokes("up");
        wcx.simulate_keystrokes("up");
        wcx.simulate_keystrokes("left");
        assert_eq!(
            &*hits.borrow(),
            &[5, 7, 5],
            "the keys step from their own last value"
        );
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at(wcx, "ui-stepper-off-value").as_deref(),
            Some("Locked by policy")
        );
        let _ = off;
    }

    /// K4b round 2 (Codex): the stepper is **one** tab stop (its − and +
    /// are pointer-only); disabling it during a hold stops the repeat.
    #[gpui::test]
    fn stepper_is_one_stop_and_disabling_stops_a_hold(cx: &mut TestAppContext) {
        let off = Rc::new(Cell::new(false));
        let hits = Rc::new(std::cell::RefCell::new(Vec::new()));
        let (o, h) = (off.clone(), hits.clone());
        let handle = cx.update(|cx| cx.focus_handle());
        let hh = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            let h = h.clone();
            let mut s = Stepper::new("one", 0, 0, 100, 1)
                .focus_handle(&hh)
                .on_change(move |v, _e, _w, _c| h.borrow_mut().push(v));
            if o.get() {
                s = s.disabled("Paused");
            }
            div().flex().child(s)
        });
        for _ in 0..3 {
            wcx.update(|w, cx| w.focus_next(cx));
            wcx.run_until_parked();
            assert_eq!(
                wcx.update(|w, cx| w.focused(cx)),
                Some(handle.clone()),
                "one stop"
            );
        }
        press(wcx, "ui-icon-button-one-inc", true, 1);
        wcx.executor().advance_clock(HOLD_DELAY + HOLD_EVERY * 2);
        wcx.run_until_parked();
        let before = hits.borrow().len();
        assert!(
            before >= 2,
            "the hold was repeating (not only the press): {before}"
        );
        off.set(true);
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        wcx.executor().advance_clock(HOLD_EVERY * 5);
        wcx.run_until_parked();
        assert_eq!(hits.borrow().len(), before, "disabling stopped the repeat");
    }
}
