//! `ui::Level` — the kit's level slider (KIT.md v3, batch K4).
//!
//! A canvas-drawn slider matching `Control::Level { min, max, notches }`
//! (the v1 settings' Level control): a `RAISE` track, the `CONTRAST`
//! fill up to the thumb, and the 20 px `TEXT` thumb drawn in a 44 px
//! hit box (`theme::target()`).
//!
//! **Scale:** linear by default; `.log()` switches to the logarithmic
//! scale the v1 control already uses (the step factor
//! `(max/min)^(1/notches)` and the `ln` position — `cat::step` /
//! `cat::display` for `Control::Level`). `.notches(n)` (default 16) sets
//! the step granularity: one arrow key steps one notch,
//! Page Up / Page Down step ten.
//!
//! The part is stateless: it fires `on_change(new_value)` for every
//! input (click, drag, key) and the parent re-renders it with the new
//! value. The `on_change` event is a `ClickEvent::default()`
//! (keyboard-default) placeholder — the value is the part's entire
//! state, and both the mouse and the keys land there.

use gpui::{
    canvas, div, prelude::*, px, quad, size, App, BorderStyle, ClickEvent, Edges, ElementId, Hsla,
    IntoElement, KeyDownEvent, KeyboardClickEvent, Pixels, Point, Rgba, SharedString, Window,
};

use crate::theme::{self, palette};
use crate::ui::focus_ring::{focus_ring, focus_ring_style};
use std::cell::Cell;
use std::rc::Rc;

use gpui::Bounds;

use crate::ui::interaction::{child_id, focus_wiring, Common};

/// The part tag for the default `debug_selector`.
const LEVEL: &str = "level";
/// The thumb's painted diameter, in logical px before zoom (the spec's
/// 20 px thumb).
const THUMB: f32 = theme::LEVEL_THUMB;
/// The track's painted height, in logical px before zoom.
const TRACK_H: f32 = theme::LEVEL_TRACK_H;
/// The track's border width (a real 1 px hairline, like every
/// `border_1()` in the app).
const TRACK_EDGE: f32 = theme::LEVEL_TRACK_EDGE;
/// The fill's minimum painted width (a non-zero sliver).
const MIN_FILL: f32 = theme::LEVEL_MIN_FILL;
/// The default notch count (the v1 `Control::Level` default).
const DEFAULT_NOTCHES: u32 = theme::LEVEL_DEFAULT_NOTCHES;
/// The Page Up / Page Down step (×10 the arrow key).
const PAGE_STEP: i64 = theme::LEVEL_PAGE_STEP;

/// A kit level slider. Build with [`Level::new`] and chain
/// `.notches(…)`, `.log()`, `.on_change(…)` and the shared
/// conventions; the parent owns the value.
#[derive(IntoElement)]
pub struct Level {
    common: Common,
    value: f64,
    min: f64,
    max: f64,
    notches: u32,
    log: bool,
    on_change: Option<ChangeHandler>,
}

/// The change handler (the mouse listeners and the key handler share it).
pub type ChangeHandler = std::sync::Arc<dyn Fn(f64, &ClickEvent, &mut Window, &mut App)>;

impl Level {
    /// A slider over `[min, max]` showing `value` (clamped).
    pub fn new(id: impl Into<ElementId>, value: f64, min: f64, max: f64) -> Self {
        Self {
            common: Common {
                id: id.into(),
                ..Default::default()
            },
            value: value.clamp(min, max),
            min,
            max,
            notches: DEFAULT_NOTCHES,
            log: false,
            on_change: None,
        }
    }

    /// The notch count (the step granularity): one arrow key steps one
    /// notch. Defaults to the v1 `Control::Level` 16.
    pub fn notches(mut self, notches: u32) -> Self {
        self.notches = notches.max(1);
        self
    }

    /// The notch count the part steps in.
    pub fn notch_count(&self) -> u32 {
        self.notches
    }

    /// The logarithmic scale (the v1 `Control::Level` behaviour: the
    /// step factor `(max/min)^(1/notches)`, the `ln` position).
    pub fn log(mut self) -> Self {
        self.log = true;
        self
    }

    /// Whether the part is on the log scale.
    pub fn is_log(&self) -> bool {
        self.log
    }

    /// The value the part paints.
    pub fn value(&self) -> f64 {
        self.value
    }

    /// The change handler: every input (click, drag, arrow key,
    /// Page Up / Page Down) fires it with the new value.
    pub fn on_change(
        mut self,
        f: impl Fn(f64, &ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(std::sync::Arc::new(f));
        self
    }

    /// The value's position in `[0, 1]` (the pure mapping the canvas
    /// and the mouse share; clamped).
    pub fn position_of(value: f64, min: f64, max: f64, log: bool) -> f32 {
        if max <= min {
            return 0.0;
        }
        let pos = if log {
            (value.max(min) / min).ln() / (max / min).ln()
        } else {
            (value - min) / (max - min)
        };
        pos.clamp(0.0, 1.0) as f32
    }

    /// The value at a position in `[0, 1]` (the inverse of
    /// [`Self::position_of`], clamped).
    pub fn value_at(position: f32, min: f64, max: f64, log: bool) -> f64 {
        if max <= min {
            return min;
        }
        let p = position.clamp(0.0, 1.0) as f64;
        if log {
            min * (max / min).powf(p)
        } else {
            min + p * (max - min)
        }
    }

    /// One stepped value: `dir` notches up or down (±1 for the arrow
    /// keys, ±10 for Page Up / Down), clamped to `[min, max]` — the
    /// linear `(max − min)/notches` step or the log
    /// `(max/min)^(1/notches)` factor (the v1 `cat::step` faces).
    pub fn step_value(value: f64, min: f64, max: f64, notches: u32, log: bool, dir: i64) -> f64 {
        if dir == 0 {
            return value.clamp(min, max);
        }
        let next = if log {
            let factor = (max / min).powf(1.0 / notches.max(1) as f64);
            if dir > 0 {
                value * factor.powi(dir as i32)
            } else {
                value / factor.powi(-dir as i32)
            }
        } else {
            value + (dir as f64) * ((max - min) / notches.max(1) as f64)
        };
        next.clamp(min, max)
    }
}

crate::kit_shared!(Level);

impl std::fmt::Debug for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Level({})", self.common.selector_for(LEVEL))
    }
}

/// The slider's pointer state, kept in element state under its id (the
/// part is a stateless `RenderOnce`): whether a drag began **on this
/// slider**, its last painted track bounds (to map the pointer during a
/// drag), and the real pointer hover.
#[derive(Default)]
struct PointerState {
    dragging: Cell<bool>,
    bounds: Cell<Option<Bounds<Pixels>>>,
    hovered: Cell<bool>,
}

/// The value under the pointer: the pointer maps to the thumb's centre
/// travel (`thumb/2 … width − thumb/2`), the same geometry the thumb is
/// painted with.
fn value_under(x: Pixels, bounds: Bounds<Pixels>, min: f64, max: f64, log: bool) -> f64 {
    let d = f32::from(theme::z(THUMB));
    let travel = (f32::from(bounds.size.width) - d).max(1.0);
    let pos = ((f32::from(x - bounds.origin.x) - d / 2.0) / travel).clamp(0.0, 1.0);
    Level::value_at(pos, min, max, log)
}

impl RenderOnce for Level {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette();
        let common = self.common;
        let selector = common.selector_for(LEVEL);
        let disabled = common.is_disabled();
        let pointer = window
            .use_keyed_state(child_id(&common.id, "pointer"), cx, |_, _| {
                Rc::new(PointerState::default())
            })
            .read(cx)
            .clone();
        // Disabling ends a live drag (no further values are reported).
        if disabled {
            pointer.dragging.set(false);
        }
        let hovered = !disabled && (common.hovered || pointer.hovered.get());

        // Disabled dims, it never deletes: the track and its edge stay,
        // the fill drops to CONTRAST_SOFT and the thumb to TEXT_DIM.
        // Hover lifts the fill to CONTRAST_HOVER.
        let track_fill: Rgba = p.raise;
        let edge: Rgba = p.control_line;
        let fill: Rgba = if disabled {
            p.contrast_soft
        } else if hovered {
            p.contrast_hover
        } else {
            p.contrast
        };
        let thumb: Rgba = if disabled { p.text_dim } else { p.text };
        let pos = Self::position_of(self.value, self.min, self.max, self.log);
        let on_change = self.on_change.clone();
        let (min, max, log) = (self.min, self.max, self.log);

        let mut root = div()
            .id(common.id.clone())
            .flex()
            .items_center()
            .min_h(theme::target())
            .min_w(theme::z(theme::LEVEL_MIN_W))
            .w_full()
            .debug_selector(|| selector.clone())
            .child(
                canvas(|_bounds, _window, _cx| (), {
                    let pointer = pointer.clone();
                    let on_change = on_change.clone();
                    move |bounds, (), window, _cx| {
                        pointer.bounds.set(Some(bounds));
                        let track_h = theme::z(TRACK_H);
                        let track_w = bounds.size.width;
                        let x0 = bounds.origin.x;
                        let thumb_d = theme::z(THUMB);
                        // All the geometry in f32 (the fork's `Pixels`
                        // does not multiply or divide by itself), back
                        // to `Pixels` at the paint calls.
                        let x0f = f32::from(x0);
                        let y0f = f32::from(bounds.origin.y);
                        let wf = f32::from(track_w);
                        let hf = f32::from(bounds.size.height);
                        let df = f32::from(thumb_d);
                        let y = px(y0f + hf / 2.0 - f32::from(track_h) / 2.0);
                        let thumb_cx_f = x0f + pos * (wf - df) + df / 2.0;

                        // The track (RAISE, the CONTROL_LINE edge as
                        // the 1 px border), the fill up to the thumb,
                        // the thumb.
                        window.paint_quad(quad(
                            gpui::Bounds::new(Point { x: x0, y }, size(track_w, track_h)),
                            theme::z(theme::RADIUS_PILL),
                            track_fill,
                            Edges::all(theme::z(TRACK_EDGE)),
                            edge,
                            BorderStyle::default(),
                        ));
                        let fill_w = px(thumb_cx_f.max(x0f + MIN_FILL) - x0f);
                        window.paint_quad(quad(
                            gpui::Bounds::new(Point { x: x0, y }, size(fill_w, track_h)),
                            theme::z(theme::RADIUS_PILL),
                            fill,
                            Edges::default(),
                            Hsla::transparent_black(),
                            BorderStyle::default(),
                        ));
                        window.paint_quad(quad(
                            gpui::Bounds::new(
                                Point {
                                    x: px(thumb_cx_f) - thumb_d / 2.0,
                                    y: px(y0f + hf / 2.0) - thumb_d / 2.0,
                                },
                                size(thumb_d, thumb_d),
                            ),
                            theme::z(theme::RADIUS_PILL),
                            thumb,
                            Edges::default(),
                            Hsla::transparent_black(),
                            BorderStyle::default(),
                        ));

                        // A drag that began on this slider follows the
                        // pointer anywhere until the button is released
                        // (window-wide only while that drag is live; a
                        // drag begun elsewhere never moves it).
                        if pointer.dragging.get() {
                            if let Some(on_change) = on_change.clone() {
                                let pointer_move = pointer.clone();
                                window.on_mouse_event(
                                    move |event: &gpui::MouseMoveEvent, phase, window, cx| {
                                        if phase != gpui::DispatchPhase::Bubble
                                            || !pointer_move.dragging.get()
                                        {
                                            return;
                                        }
                                        if event.pressed_button != Some(gpui::MouseButton::Left) {
                                            pointer_move.dragging.set(false);
                                            return;
                                        }
                                        if let Some(b) = pointer_move.bounds.get() {
                                            let v = value_under(event.position.x, b, min, max, log);
                                            on_change(v, &ClickEvent::default(), window, cx);
                                        }
                                    },
                                );
                            }
                            let pointer_up = pointer.clone();
                            window.on_mouse_event(
                                move |_event: &gpui::MouseUpEvent, phase, _window, _cx| {
                                    if phase == gpui::DispatchPhase::Capture {
                                        pointer_up.dragging.set(false);
                                    }
                                },
                            );
                        }
                    }
                })
                .size_full(),
            );

        if !disabled {
            root = focus_wiring(root, &common)
                .when(!common.focused, |d| d.focus_visible(focus_ring_style));
            // The real pointer hover (the canvas reads it at paint).
            let pointer_hover = pointer.clone();
            root = root.on_hover(move |hovered, window, _cx| {
                pointer_hover.hovered.set(*hovered);
                window.refresh();
            });
            // A press on the slider itself (an element listener: the hit
            // test and occlusion apply) sets the value and starts a drag;
            // the kit's double-click guard drops a second press.
            if let Some(on_change) = on_change.clone() {
                let pointer_down = pointer.clone();
                let allow_double = common.allow_double_click;
                root = root.on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
                    if !allow_double && event.click_count > 1 {
                        return;
                    }
                    pointer_down.dragging.set(true);
                    if let Some(b) = pointer_down.bounds.get() {
                        let v = value_under(event.position.x, b, min, max, log);
                        on_change(v, &ClickEvent::default(), window, cx);
                    }
                    window.refresh();
                });
            }
            let (notches, value) = (self.notches, self.value);
            root = root.on_key_down(move |ev: &KeyDownEvent, window, cx| {
                let dir = match ev.keystroke.key.as_str() {
                    "left" | "down" => -1,
                    "right" | "up" => 1,
                    "pagedown" => -PAGE_STEP,
                    "pageup" => PAGE_STEP,
                    _ => return,
                };
                if let Some(on_change) = &on_change {
                    let next = Level::step_value(value, min, max, notches, log, dir);
                    if (next - value).abs() > f64::EPSILON {
                        window.prevent_default();
                        cx.stop_propagation();
                        on_change(
                            next,
                            &ClickEvent::Keyboard(KeyboardClickEvent::default()),
                            window,
                            cx,
                        );
                    }
                }
            });
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

    /// The pure mappings: the linear and the log position round-trip,
    /// and the step walks one notch (×10 for the page keys) clamped at
    /// the bounds.
    #[test]
    fn level_mappings_and_steps() {
        // Linear: the ends map to 0 and 1; the midpoint to 0.5.
        assert_eq!(Level::position_of(0.0, 0.0, 1.0, false), 0.0);
        assert_eq!(Level::position_of(1.0, 0.0, 1.0, false), 1.0);
        assert_eq!(Level::position_of(0.5, 0.0, 1.0, false), 0.5);
        assert_eq!(Level::value_at(0.25, 0.0, 10.0, false), 2.5);
        // Out of range clamps.
        assert_eq!(Level::position_of(5.0, 0.0, 1.0, false), 1.0);
        assert_eq!(Level::value_at(7.0, 0.0, 1.0, false), 1.0);

        // Log: the v1 faces (min 0.001, max 0.1, 16 notches): the
        // factor is (max/min)^(1/16) and the position is the ln ratio.
        let (min, max, n) = (0.001_f64, 0.1_f64, 16_u32);
        let factor = (max / min).powf(1.0 / n as f64);
        assert!(
            (Level::step_value(min, min, max, n, true, 1) - min * factor).abs() < 1e-12,
            "one up step multiplies by the v1 factor"
        );
        assert!(
            (Level::step_value(max, min, max, n, true, -1) - max / factor).abs() < 1e-12,
            "one down step divides by the v1 factor"
        );
        // The ln position: min → 0, max → 1, the middle notch ≈ 0.5.
        assert_eq!(Level::position_of(min, min, max, true), 0.0);
        assert_eq!(Level::position_of(max, min, max, true), 1.0);
        let mid = min * (max / min).powf(0.5);
        assert!(
            (Level::position_of(mid, min, max, true) - 0.5).abs() < 1e-9,
            "the geometric middle sits at 0.5"
        );

        // Linear step: (max-min)/notches, clamped at the bounds.
        assert_eq!(Level::step_value(0.0, 0.0, 1.0, 10, false, 1), 0.1);
        assert_eq!(
            Level::step_value(0.95, 0.0, 1.0, 10, false, 10),
            1.0,
            "the page step clamps at the max"
        );
        assert_eq!(
            Level::step_value(0.05, 0.0, 1.0, 10, false, -10),
            0.0,
            "the page step clamps at the min"
        );
        // Degenerate range: no crash, the value clamps.
        assert_eq!(Level::position_of(1.0, 2.0, 2.0, false), 0.0);
        assert_eq!(Level::value_at(0.5, 2.0, 2.0, false), 2.0);
    }

    /// The slider lays out headless: a `target()`-tall hit box with the
    /// canvas inside.
    #[gpui::test]
    fn level_renders_at_the_target(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(Level::new("lvl", 0.5, 0.0, 1.0))
        });
        let root = wcx
            .debug_bounds("ui-level-lvl")
            .unwrap_or_else(|| panic!("the level recorded no bounds"));
        assert!(
            f32::from(root.size.height) >= f32::from(theme::target()),
            "the hit box is at least target() tall: {root:?}"
        );
    }

    /// The keys walk the value: the arrow key steps one notch, Page
    /// Down walks ten (clamped at the min), and the drag (a mouse down
    /// then a move with the left button held) fires the pointer's
    /// value.
    #[gpui::test]
    fn level_keys_and_drag_fire_the_value(cx: &mut TestAppContext) {
        // The parent owns the value (the part is stateless): the
        // factory re-builds the slider with it, and each step re-renders
        // before the next key lands (the kit's K2 keyboard pattern).
        let val = std::rc::Rc::new(std::cell::Cell::new(0.0f64));
        let hits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (val_in, hits_in) = (val.clone(), hits.clone());
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let make = move || {
            let (val_in, hits_in) = (val_in.clone(), hits_in.clone());
            let v = val_in.get();
            // A block wrapper (a `flex` row lays the `w_full` slider out
            // at zero width under the test window, and the drag needs the
            // slider's real width).
            div().w_full().child(
                Level::new("keys", v, 0.0, 1.0)
                    .notches(10)
                    .focus_handle(&h)
                    .on_change(move |nv, _e, _w, _c| {
                        hits_in.borrow_mut().push(nv);
                        val_in.set(nv);
                    }),
            )
        };
        let (_root, wcx) = render_part(cx, make);
        wcx.update(|w, cx| w.focus(&handle, cx));
        let mut key = |k: &str| {
            wcx.simulate_event(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse(k).unwrap(),
                is_held: false,
                prefer_character_input: false,
            });
            // The parent re-renders with the new value before the
            // next key is dispatched.
            wcx.update(|w, _cx| w.refresh());
            wcx.run_until_parked();
        };
        key("right"); // 0.0 -> 0.1
        assert_eq!(&*hits.borrow(), &[0.1], "right steps one notch");
        key("right"); // 0.1 -> 0.2
        assert_eq!(
            &*hits.borrow(),
            &[0.1, 0.2],
            "right steps the next notch from the new value"
        );
        key("pagedown"); // 0.2 -> clamped 0.0 (ten notches back)
        assert_eq!(
            &*hits.borrow(),
            &[0.1, 0.2, 0.0],
            "Page Down walks ten notches, clamped at the min"
        );

        // The drag: a mouse down at 30 % of the width, a move to 60 %
        // with the button held (the part re-fires; the parent's value
        // would have moved — here the part still reads 0.0, so the
        // second fire is at the pointer's position of the static
        // render).
        let root = wcx
            .debug_bounds("ui-level-keys")
            .unwrap_or_else(|| panic!("the level recorded no bounds"));
        let w = f32::from(root.size.width);
        let y = root.origin.y + root.size.height / 2.;
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: Point {
                x: root.origin.x + (w * 0.3).into(),
                y,
            },
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseMoveEvent {
            position: Point {
                x: root.origin.x + (w * 0.6).into(),
                y,
            },
            pressed_button: Some(gpui::MouseButton::Left),
            modifiers: gpui::Modifiers::default(),
        });
        // K4b: the pointer maps to the thumb centre's travel (thumb/2 …
        // width − thumb/2), the geometry the thumb is painted with.
        let d = f32::from(theme::z(THUMB));
        let at = |f: f32| (((w * f) - d / 2.0) / (w - d)).clamp(0.0, 1.0) as f64;
        let got = hits.borrow();
        assert!(
            got.len() >= 5 && (got[3] - at(0.3)).abs() < 1e-4 && (got[4] - at(0.6)).abs() < 1e-4,
            "the down and the held move fire the values under the thumb: {got:?}"
        );
    }

    /// A disabled slider: the dim face, no keys, the reason is the
    /// tooltip.
    #[gpui::test]
    fn level_disabled_is_inert(cx: &mut TestAppContext) {
        let dead = std::rc::Rc::new(std::cell::Cell::new(0));
        let dead_in = dead.clone();
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            let dead_in = dead_in.clone();

            div().flex().child(
                Level::new("dead", 0.5, 0.0, 1.0)
                    .focus_handle(&h)
                    .on_change(move |_v, _e, _w, _c| {
                        dead_in.set(1);
                    })
                    .disabled("The model does not support it"),
            )
        });
        wcx.update(|w, cx| w.focus(&handle, cx));
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("right").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        let root = wcx
            .debug_bounds("ui-level-dead")
            .unwrap_or_else(|| panic!("the level recorded no bounds"));
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: root.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        assert_eq!(dead.get(), 0, "a disabled slider fires nothing");
        assert_eq!(
            Level::new("d2", 0.5, 0.0, 1.0)
                .disabled("Locked")
                .tooltip_text()
                .as_deref(),
            Some("Locked"),
            "the disabled reason is the tooltip"
        );
    }

    fn mouse(wcx: &mut gpui::VisualTestContext, kind: &str, x: Pixels, y: Pixels, held: bool) {
        let position = Point { x, y };
        match kind {
            "down" => wcx.simulate_event(gpui::MouseDownEvent {
                button: gpui::MouseButton::Left,
                position,
                modifiers: gpui::Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            "up" => wcx.simulate_event(gpui::MouseUpEvent {
                button: gpui::MouseButton::Left,
                position,
                modifiers: gpui::Modifiers::default(),
                click_count: 1,
            }),
            _ => wcx.simulate_event(gpui::MouseMoveEvent {
                position,
                pressed_button: held.then_some(gpui::MouseButton::Left),
                modifiers: gpui::Modifiers::default(),
            }),
        }
        wcx.run_until_parked();
    }

    /// K4b (Codex P1): a drag begun elsewhere never moves the slider; a
    /// drag begun on it keeps tracking past its edge (clamped) until
    /// the release; a face over it (occlusion) blocks the press.
    #[gpui::test]
    fn level_pointer_is_its_own(cx: &mut TestAppContext) {
        let hits = std::rc::Rc::new(std::cell::RefCell::new(Vec::<f64>::new()));
        let h = hits.clone();
        let (_root, wcx) = render_part(cx, move || {
            let h = h.clone();
            div()
                .flex()
                .flex_col()
                .w(px(400.))
                .p(theme::z(theme::SP_40))
                .child(
                    Level::new("lv", 0.5, 0.0, 1.0)
                        .on_change(move |v, _e, _w, _cx| h.borrow_mut().push(v)),
                )
        });
        let b = wcx.debug_bounds("ui-level-lv").unwrap();
        let y = b.center().y;
        // A drag that starts outside, then moves over the slider held.
        mouse(wcx, "down", b.origin.x - px(20.), y - px(30.), false);
        mouse(wcx, "move", b.center().x, y, true);
        mouse(wcx, "up", b.center().x, y, false);
        assert!(
            hits.borrow().is_empty(),
            "a foreign drag moved it: {:?}",
            hits.borrow()
        );
        // A drag that starts on it and leaves past the right edge.
        mouse(wcx, "down", b.center().x, y, false);
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        mouse(wcx, "move", b.origin.x + b.size.width + px(80.), y, true);
        let last = *hits.borrow().last().unwrap();
        assert!(
            (last - 1.0).abs() < 1e-9,
            "tracked past the edge, clamped: {:?}",
            hits.borrow()
        );
        mouse(wcx, "up", b.origin.x + b.size.width + px(80.), y, false);
        let n = hits.borrow().len();
        mouse(wcx, "move", b.center().x, y, true);
        assert_eq!(hits.borrow().len(), n, "the release ended the drag");
    }

    /// K4b: an occluding face over the slider blocks its press.
    #[gpui::test]
    fn level_respects_occlusion(cx: &mut TestAppContext) {
        let hits = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let h = hits.clone();
        let (_root, wcx) = render_part(cx, move || {
            let h = h.clone();
            div()
                .relative()
                .w(px(400.))
                .child(
                    Level::new("lv", 0.5, 0.0, 1.0)
                        .on_change(move |_v, _e, _w, _cx| h.set(h.get() + 1)),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .left(px(0.))
                        .size_full()
                        .occlude(),
                )
        });
        let b = wcx.debug_bounds("ui-level-lv").unwrap();
        mouse(wcx, "down", b.center().x, b.center().y, false);
        assert_eq!(hits.get(), 0, "the occluded slider took the press");
    }

    /// K4b: the shared conventions — a minimum width in a squeezed row,
    /// a plain tracked handle is a tab stop, a disabled slider is not
    /// (even with a tab index), and its reason is the tooltip.
    #[gpui::test]
    fn level_follows_the_shared_conventions(cx: &mut TestAppContext) {
        let on = cx.update(|cx| cx.focus_handle());
        let off = cx.update(|cx| cx.focus_handle());
        let (h_on, h_off) = (on.clone(), off.clone());
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .w(px(10.))
                .pt(theme::z(theme::SP_40))
                .child(
                    Level::new("off", 0.2, 0.0, 1.0)
                        .focus_handle(&h_off)
                        .tab_index(0)
                        .disabled("Needs a model"),
                )
                .child(Level::new("on", 0.2, 0.0, 1.0).focus_handle(&h_on))
        });
        let b = wcx.debug_bounds("ui-level-on").unwrap();
        assert!(
            f32::from(b.size.width) >= f32::from(theme::z(theme::LEVEL_MIN_W)) - 0.5,
            "never collapses: {b:?}"
        );
        for _ in 0..2 {
            wcx.update(|w, cx| w.focus_next(cx));
            wcx.run_until_parked();
            assert_eq!(wcx.update(|w, cx| w.focused(cx)), Some(on.clone()));
        }
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at(wcx, "ui-level-off").as_deref(),
            Some("Needs a model")
        );
        let _ = off;
    }

    /// K4b round 2 (Codex): disabling the slider mid-drag ends the drag —
    /// no further values are reported.
    #[gpui::test]
    fn disabling_ends_a_drag(cx: &mut TestAppContext) {
        let off = std::rc::Rc::new(std::cell::Cell::new(false));
        let hits = std::rc::Rc::new(std::cell::RefCell::new(Vec::<f64>::new()));
        let (o, h) = (off.clone(), hits.clone());
        let (_root, wcx) = render_part(cx, move || {
            let h = h.clone();
            let mut lv = Level::new("lv", 0.5, 0.0, 1.0)
                .on_change(move |v, _e, _w, _cx| h.borrow_mut().push(v));
            if o.get() {
                lv = lv.disabled("Locked");
            }
            div().flex().w(px(400.)).p(theme::z(theme::SP_40)).child(lv)
        });
        let b = wcx.debug_bounds("ui-level-lv").unwrap();
        let y = b.center().y;
        // A real drag first: a press off the current value, then a move
        // that reports — the drag is live before it is disabled.
        mouse(wcx, "down", b.origin.x + b.size.width * 0.25, y, false);
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        mouse(wcx, "move", b.origin.x + b.size.width * 0.75, y, true);
        assert!(
            hits.borrow().len() >= 2,
            "the drag was live: {:?}",
            hits.borrow()
        );
        off.set(true);
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        let n = hits.borrow().len();
        mouse(wcx, "move", b.origin.x + px(10.), y, true);
        assert_eq!(
            hits.borrow().len(),
            n,
            "no value after disabling: {:?}",
            hits.borrow()
        );
    }
}
