//! `ui::Popover` — the kit's anchored floating surface (KIT.md v3,
//! batch K3).
//!
//! A panel face anchored to a rect (the trigger button's bounds):
//! it places itself below or above, or right or left of the anchor,
//! staying inside the window with a `theme::EDGE` margin, and closes
//! on an outside click or on Escape. The parent owns the open state:
//! the part calls `.on_close(…)` for outside clicks and Escape, and
//! the parent hands it the surface's focus handle (`.focus_handle(…)`),
//! focusing it when the surface opens (the kit's one-tab-stop contract,
//! the same as `Tabs`).
//!
//! **Placement settles after the first paint.** The panel's content
//! height is only known once the content lays out, so the first frame
//! paints the panel at the provisional (below-the-anchor) position, and
//! the part's `on_children_prepainted` hook measures the real size and
//! re-places it — flipping above/left when there is no room, clamping
//! inside the window — on the next frame. The parent owns the
//! [`PopoverState`] (the part is a stateless `RenderOnce`); re-create
//! it (or call `.reset(…)`) when the anchor moves.
//!
//! `Menu` and the command palette are built on top of this part.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, App, Bounds, ClickEvent, ElementId, IntoElement, KeyDownEvent, Pixels,
    Point, RenderOnce, SharedString, Size, Window,
};

use crate::theme::{self, palette};
use crate::ui::focus_ring::focus_ring_style_on;
use crate::ui::interaction::{child_id, POPOVER};

/// The preferred side of the anchor a popover opens on. The part flips
/// to the opposite side (and clamps) when the window has no room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PopoverPlacement {
    /// (default) below the anchor, left-aligned with it.
    #[default]
    Below,
    /// above the anchor, left-aligned with it.
    Above,
    /// right of the anchor, top-aligned with it (the rail's More menu).
    Right,
    /// left of the anchor, right-aligned with it.
    Left,
}

/// The parent-owned placement state of a [`Popover`]: the part is a
/// stateless `RenderOnce`, so the measured size and the settled origin
/// live here (like [`super::scrollbar::ScrollBarState`]).
#[derive(Clone, Debug, Default)]
pub struct PopoverState {
    /// The panel's last measured bounds (root coordinates).
    panel_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The settled origin (None until the first measurement).
    origin: Rc<Cell<Option<Point<Pixels>>>>,
}

impl PopoverState {
    /// The empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the settled position (call when the anchor moves).
    pub fn reset(&self) {
        self.panel_bounds.set(None);
        self.origin.set(None);
    }

    /// The settled origin, if the part has measured and placed the panel.
    pub fn origin(&self) -> Option<Point<Pixels>> {
        self.origin.get()
    }
}

/// The gap between the anchor and the popover, in logical px before
/// zoom (the mockup's `.rmenu` sits 10 px away → `SP_8`).
const fn gap() -> f32 {
    theme::SP_8
}

/// The pure placement (unit-tested): the popover's origin for an
/// anchor, its size, the window size and the preferred side — flipped
/// to the opposite side and clamped when the window has no room, always
/// keeping a `theme::EDGE` margin from the window's edges.
pub fn place(
    anchor: Bounds<Pixels>,
    size: Size<Pixels>,
    window: Size<Pixels>,
    preferred: PopoverPlacement,
) -> Point<Pixels> {
    place_with_gap(anchor, size, window, preferred, theme::z(gap()))
}

fn place_with_gap(
    anchor: Bounds<Pixels>,
    size: Size<Pixels>,
    window: Size<Pixels>,
    preferred: PopoverPlacement,
    gap: Pixels,
) -> Point<Pixels> {
    let edge = f32::from(theme::z(theme::EDGE));
    let gap = f32::from(gap);

    let clamp = |v: f32, max_v: f32| {
        if max_v < edge {
            edge // The popover is bigger than the window: pin it to the margin.
        } else {
            v.clamp(edge, max_v)
        }
    };

    let a_x = f32::from(anchor.origin.x);
    let a_y = f32::from(anchor.origin.y);
    let a_w = f32::from(anchor.size.width);
    let a_h = f32::from(anchor.size.height);
    let w = f32::from(size.width);
    let h = f32::from(size.height);
    let win_w = f32::from(window.width);
    let win_h = f32::from(window.height);

    match preferred {
        PopoverPlacement::Below | PopoverPlacement::Above => {
            let below_y = a_y + a_h + gap;
            let above_y = a_y - h - gap;
            let y = if preferred == PopoverPlacement::Below {
                if below_y + h > win_h - edge && above_y >= edge {
                    above_y
                } else {
                    below_y
                }
            } else if above_y < edge && below_y + h <= win_h - edge {
                below_y
            } else {
                above_y
            };
            let x = clamp(a_x, win_w - edge - w);
            Point {
                x: px(x),
                y: px(clamp(y, win_h - edge - h)),
            }
        }
        PopoverPlacement::Right | PopoverPlacement::Left => {
            let right_x = a_x + a_w + gap;
            let left_x = a_x - w - gap;
            let x = if preferred == PopoverPlacement::Right {
                if right_x + w > win_w - edge && left_x >= edge {
                    left_x
                } else {
                    right_x
                }
            } else if left_x < edge && right_x + w <= win_w - edge {
                right_x
            } else {
                left_x
            };
            let y = clamp(a_y, win_h - edge - h);
            Point {
                x: px(clamp(x, win_w - edge - w)),
                y: px(y),
            }
        }
    }
}

/// A kit anchored floating surface. Build with
/// [`Popover::new(id, anchor)`] and chain `.content(…)`, `.on_close(…)`
/// and the shared conventions; the parent owns the open state and the
/// [`PopoverState`].
#[derive(IntoElement)]
pub struct Popover {
    common: crate::ui::interaction::Common,
    anchor: Bounds<Pixels>,
    gap: Option<Pixels>,
    width: Option<f32>,
    content: Option<gpui::AnyElement>,
    placement: PopoverPlacement,
    state: Option<PopoverState>,
    on_close: Option<crate::ui::interaction::ClickHandler>,
}

impl Popover {
    /// A popover anchored to `anchor` (the trigger's bounds, in window
    /// coordinates), `MENU_W` wide.
    pub fn new(id: impl Into<ElementId>, anchor: Bounds<Pixels>) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            anchor,
            gap: None,
            width: None,
            content: None,
            placement: PopoverPlacement::default(),
            state: None,
            on_close: None,
        }
    }

    /// A popover positioned at a pointer location rather than beside a
    /// trigger. It retains the normal window-edge clamping and flipping.
    pub fn at_point(id: impl Into<ElementId>, point: Point<Pixels>) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            anchor: Bounds::new(point, Size::default()),
            gap: Some(Pixels::ZERO),
            width: None,
            content: None,
            placement: PopoverPlacement::Below,
            state: None,
            on_close: None,
        }
    }

    /// A non-default popover width, in logical px before zoom.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// The popover's content (a column of kit parts; the parent scrolls
    /// it if it is long).
    pub fn content(mut self, content: impl IntoElement) -> Self {
        self.content = Some(content.into_any_element());
        self
    }

    /// The preferred side of the anchor (default `Below`).
    pub fn placement(mut self, placement: PopoverPlacement) -> Self {
        self.placement = placement;
        self
    }

    /// The parent's placement state (the part is a stateless
    /// `RenderOnce`); the default is a fresh state.
    pub fn state(mut self, state: PopoverState) -> Self {
        self.state = Some(state);
        self
    }

    /// The close handler: called by an outside click (the transparent
    /// full-window layer behind the panel) and by Escape. The parent
    /// sets its open state to closed there.
    pub fn on_close(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Box::new(f));
        self
    }

    /// The width in logical px (`MENU_W` unless overridden).
    pub fn width_px(&self) -> f32 {
        self.width.unwrap_or(theme::MENU_W)
    }
}

crate::kit_shared!(Popover);

impl std::fmt::Debug for Popover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Popover({})", self.common.selector_for(POPOVER))
    }
}

impl RenderOnce for Popover {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        // Read the width first: a `&self` method call is a whole-self
        // borrow, and `common`/`on_close` move out of `self` below.
        // An oversized popover is constrained to the window less `EDGE` on
        // each side, and its content scrolls inside (KIT.md § Popover).
        // Window bounds include the native frame on some platforms; layout
        // and hit testing use the content viewport.
        let win = window.viewport_size();
        let edge2 = theme::z(theme::EDGE) * 2.;
        let max_w = (win.width - edge2).max(Pixels::ZERO);
        let max_h = (win.height - edge2).max(Pixels::ZERO);
        let w = theme::z(self.width_px()).min(max_w);
        let common = self.common;
        let selector = common.selector_for(POPOVER);
        // The close handler is shared by the scrim tap and Escape: one
        // closure, through `Rc` (the handler itself is not `Clone`).
        let on_close = self.on_close.map(|h| std::rc::Rc::new(h));
        let anchor = self.anchor;
        let placement = self.placement;
        let gap_override = self.gap;
        // The placement state persists across renders: the parent's, else
        // the popover's own (element state under its id) — a fresh state
        // per render would never settle.
        let state = match self.state {
            Some(state) => state,
            None => window
                .use_keyed_state(child_id(&common.id, "placement"), cx, |_, _| {
                    PopoverState::new()
                })
                .read(cx)
                .clone(),
        };
        let content = self.content.unwrap_or_else(|| div().into_any_element());
        let disabled = common.is_disabled();

        // The settled origin (measured on the previous frame), else the
        // provisional below-the-anchor position (height 0: no flip yet).
        let origin = state.origin.get().unwrap_or_else(|| {
            let win = window.viewport_size();
            place_with_gap(
                anchor,
                Size {
                    width: w,
                    height: Pixels::ZERO,
                },
                win,
                placement,
                gap_override.unwrap_or_else(|| theme::z(gap())),
            )
        });

        // The transparent layer behind the panel: an outside click closes
        // (a click on the panel itself never reaches it — it paints on
        // top).
        // The scrim is stateful (`.id()` first): on this GPUI fork the
        // click listener is a `StatefulInteractiveElement` method.
        let scrim = div()
            .absolute()
            .top(Pixels::ZERO)
            .left(Pixels::ZERO)
            .size_full()
            .bg(palette().transparent)
            .id(child_id(&common.id, "scrim"));
        // The popover's double-click setting reaches its outside-click layer.
        let allow_double = common.allow_double_click;
        // A disabled popover closes on nothing (KIT: disabled never acts).
        let scrim = match on_close.clone().filter(|_| !disabled) {
            Some(h) => scrim.on_click(move |ev, w, cx| {
                // The kit's double-click guard: a double tap closes once.
                if allow_double || ev.click_count() <= 1 {
                    h(ev, w, cx);
                }
            }),
            None => scrim,
        };

        // The panel face.
        let mut panel = super::panel::float_face(
            div()
                .id(child_id(&common.id, "scroll"))
                .flex()
                .flex_col()
                .w_full()
                .min_h_0()
                .flex_shrink(1.)
                // Both axes: a constrained popover's wide content stays
                // reachable, not clipped.
                .overflow_scroll()
                .p(theme::z(theme::SP_8))
                .child(super::panel::scroll_content(content)),
        )
        .max_h(max_h)
        .debug_selector(|| format!("ui-popover-{id}-panel", id = common.id))
        // The panel swallows its own clicks (mouse events bubble
        // through the hit-test path on this fork): without this, a tap
        // on the panel would also reach the scrim and close the popover
        // (the v1 picker's card uses the same trick).
        .occlude()
        .id(child_id(&common.id, "panel"))
        .absolute()
        .top(origin.y)
        .left(origin.x)
        .w(w);
        // A disabled popover takes no focus: it tracks no handle, is not
        // focusable, and paints neither the live keyboard ring nor the one
        // frozen by `.focused(true)` (the gallery).
        if !disabled {
            // The shared wiring: a tab stop on the parent's handle (or its
            // own), with `.tab_index`; `.pointer_only()` makes it none.
            panel = crate::ui::interaction::focus_wiring(panel, &common);
            panel = panel.focus_visible(focus_ring_style_on(palette().panel_solid));
        }
        if common.focused && !disabled {
            panel = crate::ui::focus_ring::freeze_ring(panel, palette().panel_solid);
        }
        if !disabled {
            panel = panel.on_key_down(move |ev, window, cx| {
                if ev.keystroke.key == "escape" {
                    if let Some(on_close) = &on_close {
                        let ev = ClickEvent::Keyboard(gpui::KeyboardClickEvent::default());
                        window.prevent_default();
                        cx.stop_propagation();
                        on_close(&ev, window, cx);
                    }
                }
            });
        }
        if let Some(tip) = common.tooltip() {
            panel = panel.tooltip(crate::icons::tip(tip.to_string()));
        }

        // After layout, measure the panel and settle its placement: flip
        // above/left when there is no room, clamp inside the window with
        // the `EDGE` margin, and re-render if the origin changed.
        let state_in = state.clone();
        let anchor_in = anchor;
        let placement_in = placement;
        let root = div()
            .size_full()
            .relative()
            .debug_selector(|| selector.clone())
            .on_children_prepainted(move |children, window, cx| {
                let Some(bounds) = children.get(1).copied() else {
                    return;
                };
                let size = bounds.size;
                let origin = state_in.origin.get();
                let desired = place_with_gap(
                    anchor_in,
                    size,
                    window.viewport_size(),
                    placement_in,
                    gap_override.unwrap_or_else(|| theme::z(gap())),
                );
                if origin != Some(desired) {
                    state_in.panel_bounds.set(Some(bounds));
                    state_in.origin.set(Some(desired));
                    window.defer(cx, |window, _| window.refresh());
                } else {
                    state_in.panel_bounds.set(Some(bounds));
                }
            })
            .child(scrim)
            .child(panel);

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::{render_part, render_part_at};

    fn b(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds {
            origin: Point { x: px(x), y: px(y) },
            size: Size {
                width: px(w),
                height: px(h),
            },
        }
    }

    /// A small test window (the placement math under test).
    const WIN_W: f32 = 480.0;
    const WIN_H: f32 = 400.0;
    /// A typical popover content height (logical px).
    const CONTENT_H: f32 = 120.0;
    const CONTENT_H_LARGE: f32 = 160.0;
    const CONTENT_H_HUGE: f32 = 900.0;

    fn win() -> Size<Pixels> {
        Size {
            width: px(WIN_W),
            height: px(WIN_H),
        }
    }

    /// The pure placement: below the anchor by default, flipped above
    /// when there is no room below, clamped to the `EDGE` margin, and
    /// right/left when asked.
    #[test]
    fn place_below_flip_and_clamp() {
        let window = win();
        let size = |h: f32| Size {
            width: theme::z(theme::MENU_W),
            height: px(h),
        };
        // Room below: below the anchor, left-aligned.
        let at = place(
            b(40.0, 40.0, 100.0, 20.0),
            size(CONTENT_H),
            window,
            PopoverPlacement::Below,
        );
        assert_eq!(
            (f32::from(at.x), f32::from(at.y)),
            (40.0, 68.0),
            "below: anchored under the trigger (y = 40+20+8)"
        );
        // No room below (anchor at the bottom edge): flip above.
        let at = place(
            b(40.0, 360.0, 100.0, 20.0),
            size(CONTENT_H),
            window,
            PopoverPlacement::Below,
        );
        assert_eq!(
            (f32::from(at.x), f32::from(at.y)),
            (40.0, 232.0),
            "flipped above (y = 360-120-8)"
        );
        // No room either way (huge content): clamped to the margins.
        let at = place(
            b(40.0, 200.0, 100.0, 20.0),
            size(CONTENT_H_HUGE),
            window,
            PopoverPlacement::Below,
        );
        assert_eq!(
            f32::from(at.y),
            f32::from(theme::z(theme::EDGE)),
            "clamped to the top margin"
        );
        // The right edge must not pass the window: x is clamped.
        let at = place(
            b(440.0, 40.0, 20.0, 20.0),
            size(CONTENT_H),
            window,
            PopoverPlacement::Below,
        );
        assert_eq!(
            f32::from(at.x),
            WIN_W - theme::EDGE - theme::MENU_W,
            "x clamped so the panel stays inside the window"
        );
    }

    #[test]
    fn point_anchored_popovers_start_at_the_point_and_clamp_to_window() {
        let window = win();
        let size = Size {
            width: px(160.),
            height: px(120.),
        };
        let at_pointer = place_with_gap(
            b(40., 60., 0., 0.),
            size,
            window,
            PopoverPlacement::Below,
            px(0.),
        );
        assert_eq!((at_pointer.x, at_pointer.y), (px(40.), px(60.)));

        let near_edge = place_with_gap(
            b(470., 390., 0., 0.),
            size,
            window,
            PopoverPlacement::Below,
            px(0.),
        );
        assert!(near_edge.x >= theme::z(theme::EDGE));
        assert!(near_edge.y >= theme::z(theme::EDGE));
        assert!(near_edge.x + size.width <= window.width - theme::z(theme::EDGE));
        assert!(near_edge.y + size.height <= window.height - theme::z(theme::EDGE));
    }

    /// The pure placement: right of the anchor (the rail's More menu),
    /// flipped left when there is no room.
    #[test]
    fn place_right_and_left() {
        let window = win();
        let size = |h: f32| Size {
            width: theme::z(theme::MENU_W),
            height: px(h),
        };
        let at = place(
            b(20.0, 100.0, 64.0, 58.0),
            size(CONTENT_H_LARGE),
            window,
            PopoverPlacement::Right,
        );
        assert_eq!(
            (f32::from(at.x), f32::from(at.y)),
            (92.0, 100.0),
            "right: beside the trigger, top-aligned"
        );
        let at = place(
            b(400.0, 100.0, 60.0, 58.0),
            size(CONTENT_H_LARGE),
            window,
            PopoverPlacement::Right,
        );
        assert!(
            f32::from(at.x) < 400.0,
            "no room on the right: flipped left"
        );
        // No room on the left (the panel is wider than the space left
        // of the anchor): it pins to the left margin.
        let at = place(
            b(240.0, 100.0, 40.0, 40.0),
            size(CONTENT_H_LARGE),
            window,
            PopoverPlacement::Left,
        );
        assert_eq!(
            f32::from(at.x),
            f32::from(theme::z(theme::EDGE)),
            "left: clamped to the left margin"
        );
        // Enough room on the left: beside the trigger.
        let at = place(
            b(320.0, 100.0, 40.0, 40.0),
            size(CONTENT_H_LARGE),
            window,
            PopoverPlacement::Left,
        );
        assert_eq!(
            f32::from(at.x),
            320.0 - f32::from(theme::z(theme::MENU_W)) - gap(),
            "left: beside the trigger"
        );
    }

    /// The rendered popover: it sits below the anchor (the first frame
    /// is provisional; the measurement settles it in place).
    #[gpui::test]
    fn popover_sits_below_the_anchor(cx: &mut TestAppContext) {
        let handle = cx.update(|cx| cx.focus_handle());
        let (_root, wcx) = render_part(cx, move || {
            Popover::new("more", b(40.0, 40.0, 100.0, 20.0))
                .focus_handle(&handle)
                .content(crate::ui::text::ui_text("A menu row").wrap())
                .on_close(|_e, _w, _c| {})
        });
        wcx.run_until_parked(); // the measurement frame settles the origin
        let panel = wcx
            .debug_bounds("ui-popover-more-panel")
            .unwrap_or_else(|| panic!("the popover panel recorded no bounds"));
        let (x, y) = (f32::from(panel.origin.x), f32::from(panel.origin.y));
        assert_eq!(
            (x, y),
            (40.0, 68.0),
            "the panel is below the anchor (40, 40+20+8): {panel:?}",
        );
        assert_eq!(
            f32::from(panel.size.width),
            f32::from(theme::z(theme::MENU_W)),
            "the panel is MENU_W wide"
        );
    }

    /// A popover anchored near the bottom of a small window flips
    /// above the anchor after its first measured frame.
    #[gpui::test]
    fn popover_flips_above_when_there_is_no_room(cx: &mut TestAppContext) {
        // The window is created at the small size: the anchor at y=360
        // of a 400 px-tall window has no room below.
        const ANCHOR_Y: f32 = 360.0;
        const ANCHOR_X: f32 = 40.0;
        const ANCHOR_W: f32 = 100.0;
        const ANCHOR_H: f32 = 20.0;
        let state = PopoverState::new();
        let (_handle, wcx) = render_part_at(cx, gpui::size(px(WIN_W), px(WIN_H)), move || {
            Popover::new("more", b(ANCHOR_X, ANCHOR_Y, ANCHOR_W, ANCHOR_H))
                .state(state.clone())
                .content(
                    div()
                        .flex()
                        .flex_col()
                        .gap(theme::z(theme::SP_8))
                        .child(crate::ui::text::ui_text("Row one").wrap())
                        .child(crate::ui::text::ui_text("Row two").wrap())
                        .child(crate::ui::text::ui_text("Row three").wrap()),
                )
                .on_close(|_e, _w, _c| {})
        });
        wcx.run_until_parked();
        // The measurement settles the placement in the next frame (the
        // part refreshes once the panel's real size is known).
        wcx.update(|w, _cx| w.refresh());
        wcx.run_until_parked();
        // The settled frame: the panel is above the anchor and inside
        // the window's EDGE margins.
        let panel = wcx
            .debug_bounds("ui-popover-more-panel")
            .unwrap_or_else(|| panic!("the popover panel recorded no bounds"));
        assert!(
            f32::from(panel.origin.y) < ANCHOR_Y,
            "the panel flipped above the anchor: {panel:?}",
        );
        assert!(
            f32::from(panel.origin.x) >= f32::from(theme::z(theme::EDGE)),
            "the panel kept the EDGE margin from the left: {panel:?}",
        );
        let win = wcx.update(|w, _cx| w.bounds());
        assert!(
            f32::from(panel.origin.x) + f32::from(panel.size.width)
                <= f32::from(win.size.width) - f32::from(theme::z(theme::EDGE)) + f32::EPSILON,
            "the panel kept the EDGE margin from the right: {panel:?}",
        );
        assert!(
            f32::from(panel.origin.y) + f32::from(panel.size.height)
                <= f32::from(win.size.height) - f32::from(theme::z(theme::EDGE)) + f32::EPSILON,
            "the panel kept the EDGE margin from the bottom: {panel:?}",
        );
    }

    /// An outside click (the transparent layer) closes the popover; a
    /// click on the panel itself does not; Escape closes when the
    /// surface's focus handle holds focus.
    #[gpui::test]
    fn popover_closes_on_outside_click_and_escape(cx: &mut TestAppContext) {
        let closed = std::rc::Rc::new(std::cell::Cell::new(0));
        let closed_in = closed.clone();
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            let closed_in = closed_in.clone();
            Popover::new("more", b(40.0, 40.0, 100.0, 20.0))
                .focus_handle(&h)
                .content(crate::ui::text::ui_text("A menu row").wrap())
                .on_close(move |_e, _w, _c| {
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
        assert_eq!(closed.get(), 1, "Escape closes the popover");

        // A click on the panel content does not close.
        const TAP_IN_X: f32 = 20.0;
        const TAP_IN_Y: f32 = 10.0;
        wcx.run_until_parked();
        let panel = wcx
            .debug_bounds("ui-popover-more-panel")
            .unwrap_or_else(|| panic!("the popover panel recorded no bounds"));
        let at = Point {
            x: panel.origin.x + px(TAP_IN_X),
            y: panel.origin.y + px(TAP_IN_Y),
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
        assert_eq!(closed.get(), 1, "a click on the panel does not close it");

        // A click outside the panel (on the scrim layer) closes: a
        // point far from the panel (which sits at the anchor's side,
        // `MENU_W` wide, below y=68).
        const OUT_X: f32 = 460.0;
        const OUT_Y: f32 = 380.0;
        let at = Point {
            x: px(OUT_X),
            y: px(OUT_Y),
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
        assert_eq!(closed.get(), 2, "an outside click closes the popover");
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

    /// The shared focus wiring: given a plain handle (not marked a tab stop),
    /// the popover is still a Tab stop; `.pointer_only()` makes it none.
    #[gpui::test]
    fn popover_is_a_tab_stop_unless_pointer_only(cx: &mut TestAppContext) {
        for pointer_only in [false, true] {
            let handle = cx.update(|cx| cx.focus_handle());
            let h = handle.clone();
            let (_root, wcx) = render_part(cx, move || {
                let p = Popover::new("more", b(40.0, 40.0, 100.0, 20.0))
                    .focus_handle(&h)
                    .content(crate::ui::text::ui_text("A menu row"));
                if pointer_only {
                    p.pointer_only()
                } else {
                    p
                }
            });
            wcx.update(|w, cx| w.focus_next(cx));
            wcx.run_until_parked();
            assert_eq!(
                wcx.update(|w, _cx| handle.is_focused(w)),
                !pointer_only,
                "pointer_only={pointer_only}"
            );
        }
    }

    /// The popover is non-modal (KIT.md: only Dialog, Sheet and Palette
    /// scrims block the canvas): an outside click always reaches what is
    /// under it, and it also closes the popover only while enabled.
    #[gpui::test]
    fn outside_click_reaches_the_canvas_and_closes_only_when_enabled(cx: &mut TestAppContext) {
        for disabled in [false, true] {
            let closed = std::rc::Rc::new(std::cell::Cell::new(0));
            let behind = std::rc::Rc::new(std::cell::Cell::new(0));
            let (c, bh) = (closed.clone(), behind.clone());
            let (_root, wcx) = render_part(cx, move || {
                let (c, bh) = (c.clone(), bh.clone());
                let p = Popover::new("more", b(40.0, 40.0, 100.0, 20.0))
                    .content(crate::ui::text::ui_text("A menu row"))
                    .on_close(move |_e, _w, _c| c.set(c.get() + 1));
                div()
                    .size_full()
                    .relative()
                    .child(
                        div()
                            .id("canvas")
                            .absolute()
                            .size_full()
                            .on_click(move |_e, _w, _c| bh.set(bh.get() + 1)),
                    )
                    .child(if disabled {
                        p.disabled("Nothing to change yet")
                    } else {
                        p
                    })
            });
            const OUT_X: f32 = 460.0;
            const OUT_Y: f32 = 380.0;
            click_at(
                wcx,
                Point {
                    x: px(OUT_X),
                    y: px(OUT_Y),
                },
            );
            assert_eq!(
                behind.get(),
                1,
                "disabled={disabled}: the canvas gets the click"
            );
            assert_eq!(
                closed.get(),
                usize::from(!disabled),
                "disabled={disabled}: it closes only while enabled"
            );
        }
    }

    /// Disabled, a popover takes no focus and closes on nothing: Tab never
    /// lands on it; even with its handle focused by hand, Escape does not
    /// close it; an outside click does not close it.
    #[gpui::test]
    fn disabled_popover_closes_on_nothing_and_takes_no_focus(cx: &mut TestAppContext) {
        let closed = std::rc::Rc::new(std::cell::Cell::new(0));
        let closed_in = closed.clone();
        // A real tab stop: were the popover to track it, Tab would land on it.
        let handle = cx.update(|cx| cx.focus_handle().tab_stop(true));
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            let closed_in = closed_in.clone();
            Popover::new("more", b(40.0, 40.0, 100.0, 20.0))
                .focus_handle(&h)
                .disabled("Nothing to change yet")
                .content(crate::ui::text::ui_text("A menu row").wrap())
                .on_close(move |_e, _w, _c| closed_in.set(closed_in.get() + 1))
        });
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert!(
            !wcx.update(|w, _cx| handle.is_focused(w)),
            "Tab never lands on a disabled popover"
        );
        // Focus the handle by hand: even then Escape does not close it. This
        // asserts the observed outcome only; that the key handler is not
        // installed while disabled is in the source, not proven here.
        wcx.update(|w, cx| w.focus(&handle, cx));
        let esc = gpui::Keystroke::parse("escape").unwrap();
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: esc.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent { keystroke: esc });
        const OUT_X: f32 = 460.0;
        const OUT_Y: f32 = 380.0;
        click_at(
            wcx,
            Point {
                x: px(OUT_X),
                y: px(OUT_Y),
            },
        );
        assert_eq!(closed.get(), 0, "a disabled popover closes on nothing");
    }

    /// K3b item 6: an oversized popover is constrained to the window less
    /// `EDGE` on each side (its content scrolls inside), at zoom 1 and
    /// 1.25.
    #[gpui::test]
    fn oversized_popover_stays_inside_the_edges(cx: &mut TestAppContext) {
        for scale in [1.0_f32, 1.25] {
            theme::set_ui_scale(scale);
            let (_handle, wcx) = render_part_at(cx, gpui::size(px(WIN_W), px(WIN_H)), move || {
                Popover::new("huge", b(40., 40., 60., 30.))
                    .width(900.)
                    .content(div().w(px(800.)).h(px(CONTENT_H_HUGE)))
                    .on_close(|_e, _w, _c| {})
            });
            wcx.run_until_parked();
            // The settle frame (the harness draws on refresh).
            wcx.update(|w, _| w.refresh());
            wcx.run_until_parked();
            let panel = wcx
                .debug_bounds("ui-popover-huge-panel")
                .unwrap_or_else(|| panic!("the panel recorded no bounds"));
            let edge = f32::from(theme::z(theme::EDGE));
            let (x, y) = (f32::from(panel.origin.x), f32::from(panel.origin.y));
            let (w, h) = (f32::from(panel.size.width), f32::from(panel.size.height));
            assert!(
                x >= edge - 0.5 && y >= edge - 0.5,
                "zoom {scale}: {panel:?}"
            );
            assert!(
                x + w <= WIN_W - edge + 0.5,
                "zoom {scale}: right edge {panel:?}"
            );
            assert!(
                y + h <= WIN_H - edge + 0.5,
                "zoom {scale}: bottom edge {panel:?}"
            );
        }
        theme::set_ui_scale(1.0);
    }

    /// K3b round 2 (Codex): a width-constrained popover's wide content is
    /// reachable — a horizontal wheel scroll moves it.
    #[gpui::test]
    fn constrained_popover_scrolls_wide_content(cx: &mut TestAppContext) {
        let (_handle, wcx) = render_part_at(cx, gpui::size(px(WIN_W), px(WIN_H)), move || {
            Popover::new("wide", b(40., 40., 60., 30.))
                .width(900.)
                .content(
                    div()
                        .w(px(1200.))
                        .h(px(40.))
                        .debug_selector(|| "wide-content".to_string()),
                )
                .on_close(|_e, _w, _c| {})
        });
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        let before = wcx.debug_bounds("wide-content").unwrap();
        let panel = wcx.debug_bounds("ui-popover-wide-panel").unwrap();
        wcx.simulate_event(gpui::ScrollWheelEvent {
            position: panel.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(-200.), px(0.))),
            ..Default::default()
        });
        wcx.run_until_parked();
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        let after = wcx.debug_bounds("wide-content").unwrap();
        assert!(
            after.origin.x < before.origin.x,
            "the wide content scrolled horizontally: {before:?} → {after:?}"
        );
    }
}
