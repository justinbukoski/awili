//! `ui::ScrollBar` — the kit's overlay scrollbar (KIT.md v3, batch K2).
//!
//! The declared geometry exception of the kit: a 20 px pointer target
//! (`SCROLLBAR_HIT`), because scrolling also works by wheel, trackpad and
//! keys. It paints a 6 px (`SCROLLBAR_THUMB`) `CONTROL_LINE` pill thumb
//! (`TEXT_DIM` while dragging) over the scroll area's right edge and
//! drags to scroll — the `conversation_scrollbar` pattern from `main.rs`,
//! promoted to a kit part (replacing that function is a later slice, not
//! part of K2).
//!
//! Visibility, per the spec: shown on scroll and hover, and the bar
//! schedules its own hide 1 s after the last activity (an offset change,
//! a drag, a hover) — a generation-guarded timer, so a newer activity
//! cancels the pending hide. The hide fades the thumb out over
//! `theme::motion::BASE_MS` (GPUI has no alpha-fade primitive, so the
//! fade is timer-stepped: the thumb's alpha steps down while the state
//! re-paints); under Reduce Motion the hide is instant instead. The bar
//! is pointer-only: it is never a tab stop and never takes the keyboard
//! (scrolling also works by wheel, trackpad and keys — the 20 px
//! `SCROLLBAR_HIT` target is the kit's declared geometry exception).
//! The parent owns the [`ScrollBarState`] (the part is a stateless
//! `RenderOnce`), mirroring how the app owns `conversation_scroll_drag`.
//!
//! K2 ships the vertical bar only; the horizontal bar is deferred (noted
//! in the report).

use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{
    canvas, div, prelude::*, px, quad, size, AsyncApp, BorderStyle, Bounds, DispatchPhase,
    HitboxBehavior, Hsla, IntoElement, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    RenderOnce, ScrollHandle, Window,
};

use crate::theme;
use crate::ui::interaction::{with_alpha, SCROLLBAR};

/// How long after the last interaction the bar hides.
const HIDE_AFTER: Duration = Duration::from_millis(theme::motion::SCROLLBAR_HIDE_MS);
/// The smallest grabbable thumb, logical px before zoom.
const MIN_THUMB: f32 = theme::SCROLLBAR_MIN_THUMB;
/// The track's top/bottom inset, logical px before zoom (`SP_8`).
const TRACK_INSET: f32 = theme::SCROLLBAR_TRACK_INSET;

/// The parent-owned state of a [`ScrollBar`]: the part is a stateless
/// `RenderOnce`, so the in-progress drag, the visibility and the
/// "hide-after" generation live here (like the app's
/// `conversation_scroll_drag`).
#[derive(Clone, Debug, Default)]
pub struct ScrollBarState {
    /// The in-progress drag: the grab offset from the thumb's top, or
    /// `None` when not dragging.
    drag: Rc<Cell<Option<Pixels>>>,
    /// Whether the bar is currently shown.
    visible: Rc<Cell<bool>>,
    /// The last scroll offset seen at paint (change detection for the
    /// "shows on scroll" rule).
    last_offset: Rc<Cell<Option<f32>>>,
    /// The "hide-after" generation: any activity bumps it, cancelling
    /// the pending hide (and the in-flight fade).
    hide_gen: Rc<Cell<u64>>,
    /// The thumb's opacity while fading out (1.0 = fully opaque).
    alpha: Rc<Cell<f32>>,
}

/// How many steps the fade takes over `theme::motion::BASE_MS`.
const FADE_STEPS: u32 = theme::SCROLLBAR_FADE_STEPS;

impl ScrollBarState {
    /// The empty state (hidden, not dragging).
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the bar is shown.
    pub fn is_visible(&self) -> bool {
        self.visible.get()
    }

    /// Whether a drag is in progress.
    pub fn is_dragging(&self) -> bool {
        self.drag.get().is_some()
    }

    /// The thumb's current opacity (the fade is stepped, so this is read
    /// at paint and rescaled onto the thumb colour).
    pub fn alpha(&self) -> f32 {
        self.alpha.get()
    }

    /// Note an activity (an offset change, a drag, a hover): show the bar
    /// at full opacity, cancel the pending hide, and schedule a fresh
    /// hide `HIDE_AFTER` from now.
    pub fn note_activity(&self, cx: &mut gpui::App) {
        self.visible.set(true);
        self.alpha.set(1.0);
        self.schedule_hide(cx);
    }

    /// Schedule the hide `HIDE_AFTER` from now (cancelling any earlier
    /// one): the hide fires only if no newer activity bumped the
    /// generation and no drag is in progress.
    fn schedule_hide(&self, cx: &mut gpui::App) {
        let gen = self.hide_gen.get() + 1;
        self.hide_gen.set(gen);
        let state = self.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            cx.background_executor().timer(HIDE_AFTER).await;
            cx.update(|app: &mut gpui::App| {
                if state.hide_gen.get() != gen || state.drag.get().is_some() {
                    return;
                }
                state.hide(app);
            });
        })
        .detach();
    }

    /// Hide the bar: instantly under Reduce Motion, otherwise fade the
    /// thumb out over `theme::motion::BASE_MS` in `FADE_STEPS` steps.
    fn hide(&self, cx: &mut gpui::App) {
        let state = self.clone();
        let gen = self.hide_gen.get();
        if crate::ui::motion::reduce_motion() {
            self.set_hidden();
            Self::refresh_all(cx);
            return;
        }
        let step_ms = theme::motion::BASE_MS / FADE_STEPS as u64;
        cx.spawn(async move |cx: &mut AsyncApp| {
            for step in 1..=FADE_STEPS {
                cx.background_executor()
                    .timer(Duration::from_millis(step_ms))
                    .await;
                cx.update(|app: &mut gpui::App| {
                    // A newer activity (or a drag) cancels the fade.
                    if state.hide_gen.get() != gen || state.drag.get().is_some() {
                        return;
                    }
                    if step == FADE_STEPS {
                        state.set_hidden();
                    } else {
                        state.alpha.set(1.0 - (step as f32) / FADE_STEPS as f32);
                    }
                    Self::refresh_all(app);
                });
            }
        })
        .detach();
    }

    /// The hidden end-state (opaque-reset so the next show is clean).
    fn set_hidden(&self) {
        self.visible.set(false);
        self.alpha.set(0.0);
    }

    /// Refresh every open window (a state change the parent's view
    /// re-reads on its next paint).
    fn refresh_all(cx: &mut gpui::App) {
        for handle in cx.windows() {
            let _ = cx.update_window(handle, |_view, w, _cx| {
                w.refresh();
            });
        }
    }
}

/// The thumb's geometry: `(top from the track's top, height)`, or `None`
/// when the bar must not show (nothing scrolls). Mirrors the app's
/// `conversation_scrollbar_metrics`.
pub fn thumb_metrics(
    viewport_height: f32,
    max_offset: f32,
    current_offset: f32,
) -> Option<(f32, f32)> {
    if viewport_height <= TRACK_INSET * 2.0 || max_offset <= 0.0 {
        return None;
    }
    let track_height = viewport_height - TRACK_INSET * 2.0;
    let content_height = viewport_height + max_offset;
    let thumb_height = (track_height * viewport_height / content_height)
        .max(MIN_THUMB)
        .min(track_height);
    let progress = current_offset.abs().clamp(0.0, max_offset) / max_offset;
    Some((progress * (track_height - thumb_height), thumb_height))
}

/// The scroll offset a pointer position over the track scrolls to. Mirrors
/// the app's `conversation_scroll_offset_for_pointer`.
pub fn offset_for_pointer(
    track_height: f32,
    thumb_height: f32,
    max_offset: f32,
    pointer: f32,
) -> Option<f32> {
    let travel = track_height - thumb_height;
    if travel <= 0.0 || max_offset <= 0.0 {
        return None;
    }
    let progress = pointer.clamp(0.0, travel) / travel;
    Some(-max_offset * progress)
}

/// A kit overlay scrollbar for a `ScrollHandle` (the kit's declared
/// 20 px pointer-target exception). Build with [`ScrollBar::new`],
/// passing an id (it feeds the default `debug_selector`), the scroll
/// handle of the area and the parent's [`ScrollBarState`].
///
/// Pointer-only: the bar is never a tab stop and never takes the
/// keyboard — it carries the shared conventions' `.id` / `.label` /
/// `.selector` only (the label is its tooltip).
#[derive(Debug, Clone, IntoElement)]
pub struct ScrollBar {
    common: crate::ui::interaction::Common,
    scroll: ScrollHandle,
    state: ScrollBarState,
}

impl ScrollBar {
    /// An overlay scrollbar for the given scroll handle. The parent owns
    /// the state (re-created parts share it through the `Rc`s).
    pub fn new(
        id: impl Into<gpui::ElementId>,
        scroll: ScrollHandle,
        state: ScrollBarState,
    ) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            scroll,
            state,
        }
    }

    /// The `debug_selector` override (KIT.md § Conventions).
    pub fn selector(mut self, selector: &'static str) -> Self {
        self.common.selector = Some(selector);
        self
    }

    /// The accessible name (the bar's tooltip).
    pub fn label(mut self, text: impl Into<gpui::SharedString>) -> Self {
        self.common.label = Some(text.into());
        self
    }

    /// The accessible name, if set.
    pub fn label_text(&self) -> Option<gpui::SharedString> {
        self.common.label.clone()
    }

    /// The effective tooltip (the label — the bar has no disabled state).
    pub fn tooltip_text(&self) -> Option<gpui::SharedString> {
        self.common.tooltip()
    }

    /// The state the bar is driving (read by the parent and the tests).
    pub fn state(&self) -> &ScrollBarState {
        &self.state
    }

    /// The scroll handle the bar drives.
    pub fn scroll_handle(&self) -> &ScrollHandle {
        &self.scroll
    }
}

impl RenderOnce for ScrollBar {
    fn render(self, _window: &mut Window, _cx: &mut gpui::App) -> impl IntoElement {
        let common = self.common;
        let state = self.state.clone();
        let scroll = self.scroll.clone();
        let p = theme::palette();
        // The label is the bar's tooltip; the strip carries the bar's id
        // (stateful — GPUI tooltips need a stateful element).
        let selector = common.selector_for(SCROLLBAR);
        let id = common.id.clone();
        let tip = common.tooltip();

        let mut strip = div()
            .id(id)
            .absolute()
            .right(Pixels::ZERO)
            .top(theme::z(theme::SP_8))
            .bottom(theme::z(theme::SP_8))
            .w(theme::z(theme::SCROLLBAR_HIT))
            .cursor_pointer()
            .debug_selector(move || selector)
            .child(
                // The track's hitbox: a press starts a drag only while the track is
                // the topmost hit (a modal scrim or overlay in front occludes it).
                canvas(
                    |bounds, window, _cx| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                    {
                        let scroll = scroll.clone();
                        let state = state.clone();
                        move |track_bounds, hitbox: gpui::Hitbox, window, cx| {
                            // The ScrollHandle receives its bounds and max
                            // offset while the scroll area paints (after
                            // prepaint); reading it here keeps the bar correct
                            // on the first frame.
                            let viewport_height = f32::from(scroll.bounds().size.height);
                            let offset = f32::from(scroll.offset().y);
                            let changed = state
                                .last_offset
                                .get()
                                .is_none_or(|last| (last - offset).abs() > f32::EPSILON);
                            // An offset change is an activity: the bar shows
                            // and reschedules its own hide from now.
                            if changed {
                                state.last_offset.set(Some(offset));
                                state.note_activity(cx);
                            }
                            if state.drag.get().is_some() {
                                state.note_activity(cx);
                            }
                            if !state.visible.get() {
                                return;
                            }

                            let max_offset = f32::from(scroll.max_offset().y);
                            let Some((thumb_top, thumb_height)) =
                                thumb_metrics(viewport_height, max_offset, offset)
                            else {
                                return;
                            };
                            let thumb_width = theme::z(theme::SCROLLBAR_THUMB_W);
                            let thumb_bounds = Bounds::new(
                                Point {
                                    x: track_bounds.origin.x
                                        + (track_bounds.size.width - thumb_width) / 2.,
                                    y: track_bounds.origin.y + px(thumb_top),
                                },
                                size(thumb_width, px(thumb_height)),
                            );
                            let thumb_color = if state.drag.get().is_some() {
                                p.text_dim
                            } else {
                                p.control_line
                            };
                            // The thumb fades out with the state's alpha
                            // (timer-stepped over `motion::BASE_MS`, instant
                            // under Reduce Motion): the palette colour is
                            // rescaled by the alpha, never reconstructed.
                            window.paint_quad(quad(
                                thumb_bounds,
                                theme::z(theme::RADIUS_PILL),
                                with_alpha(thumb_color, state.alpha.get()),
                                gpui::Edges::default(),
                                Hsla::transparent_black(),
                                BorderStyle::default(),
                            ));

                            window.on_mouse_event({
                                let drag = state.drag.clone();
                                let scroll = scroll.clone();
                                let state = state.clone();
                                let hitbox = hitbox.clone();
                                move |event: &MouseDownEvent, phase, window, cx| {
                                    if phase == DispatchPhase::Bubble
                                        && event.button == gpui::MouseButton::Left
                                        && hitbox.is_hovered(window)
                                    {
                                        let grab_offset = if thumb_bounds.contains(&event.position)
                                        {
                                            event.position.y - thumb_bounds.origin.y
                                        } else {
                                            px(thumb_height / 2.0)
                                        };
                                        drag.set(Some(grab_offset));
                                        state.note_activity(cx);
                                        if !thumb_bounds.contains(&event.position) {
                                            let pointer = f32::from(
                                                event.position.y
                                                    - track_bounds.origin.y
                                                    - grab_offset,
                                            );
                                            if let Some(scroll_y) = offset_for_pointer(
                                                f32::from(track_bounds.size.height),
                                                thumb_height,
                                                f32::from(scroll.max_offset().y),
                                                pointer,
                                            ) {
                                                let mut offset = scroll.offset();
                                                offset.y = px(scroll_y);
                                                scroll.set_offset(offset);
                                                window.refresh();
                                            }
                                        }
                                        window.prevent_default();
                                        cx.stop_propagation();
                                    }
                                }
                            });
                            window.on_mouse_event({
                                let drag = state.drag.clone();
                                let scroll = scroll.clone();
                                let state = state.clone();
                                move |event: &MouseMoveEvent, phase, window, cx| {
                                    if phase != DispatchPhase::Bubble {
                                        return;
                                    }
                                    if !event.dragging() {
                                        drag.set(None);
                                        return;
                                    }
                                    let Some(grab_offset) = drag.get() else {
                                        return;
                                    };
                                    // Each drag move is an activity: the hide
                                    // keeps rescheduling from the latest one.
                                    state.note_activity(cx);
                                    let viewport_height = f32::from(scroll.bounds().size.height);
                                    let max_offset = f32::from(scroll.max_offset().y);
                                    let Some((_, thumb_height)) = thumb_metrics(
                                        viewport_height,
                                        max_offset,
                                        f32::from(scroll.offset().y),
                                    ) else {
                                        return;
                                    };
                                    let pointer = f32::from(
                                        event.position.y - track_bounds.origin.y - grab_offset,
                                    );
                                    let Some(scroll_y) = offset_for_pointer(
                                        f32::from(track_bounds.size.height),
                                        thumb_height,
                                        max_offset,
                                        pointer,
                                    ) else {
                                        return;
                                    };
                                    let mut offset = scroll.offset();
                                    offset.y = px(scroll_y);
                                    scroll.set_offset(offset);
                                    window.refresh();
                                    cx.stop_propagation();
                                }
                            });
                            window.on_mouse_event({
                                let drag = state.drag.clone();
                                let state = state.clone();
                                move |event: &MouseUpEvent, phase, window, cx| {
                                    if phase == DispatchPhase::Bubble
                                        && event.button == gpui::MouseButton::Left
                                        && drag.take().is_some()
                                    {
                                        state.schedule_hide(cx);
                                        window.refresh();
                                        cx.stop_propagation();
                                    }
                                }
                            });
                        }
                    },
                )
                .size_full(),
            );

        // The imperative hover API (`InteractiveElement::interactivity`,
        // not a builder — the strip is not a stateful element): an
        // activity on hover, schedule the hide when the pointer leaves.
        let hover_state = state.clone();
        strip
            .interactivity()
            .on_hover(move |hovering: &bool, _window, cx| {
                if *hovering {
                    hover_state.note_activity(cx);
                } else {
                    hover_state.schedule_hide(cx);
                }
            });

        // The label is the bar's tooltip (the shared conventions).
        if let Some(tip) = tip {
            strip = strip.tooltip(crate::icons::tip(tip.to_string()));
        }

        strip
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// The metrics mirror the app's: nothing when there is nothing to
    /// scroll; the thumb tracks the offset.
    #[test]
    fn thumb_metrics_match_the_app_formula() {
        assert_eq!(thumb_metrics(600.0, 0.0, 0.0), None, "no scroll, no bar");
        let (top, height) = thumb_metrics(600.0, 600.0, 0.0).unwrap();
        assert_eq!(top, 0.0, "the thumb starts at the top");
        assert!(
            height < 600.0 - TRACK_INSET * 2.0,
            "the thumb is smaller than the track"
        );
        let (middle, _) = thumb_metrics(600.0, 600.0, -300.0).unwrap();
        assert!(middle > 0.0, "halfway down, the thumb has moved");
        let (end, _) = thumb_metrics(600.0, 600.0, -600.0).unwrap();
        assert!(end > middle, "at the end, the thumb is lower");
        // The thumb never exceeds the track or drops under the minimum.
        let (_, h) = thumb_metrics(600.0, 10_000.0, 0.0).unwrap();
        assert!(
            h >= MIN_THUMB,
            "a huge document still gives a grabbable thumb"
        );
        assert!(h <= 600.0 - TRACK_INSET * 2.0, "…and never fills the track");
    }

    /// The pointer→offset mapping: top of the track = 0, bottom =
    /// `-max_offset`, and the pointer is clamped to the track.
    #[test]
    fn pointer_maps_to_the_scroll_offset() {
        let track = 600.0 - TRACK_INSET * 2.0;
        let thumb = 300.0;
        let travel = track - thumb;
        assert_eq!(offset_for_pointer(track, thumb, 1000.0, 0.0), Some(0.0));
        assert_eq!(
            offset_for_pointer(track, thumb, 1000.0, travel),
            Some(-1000.0),
            "the bottom of the track is the end of the document"
        );
        assert_eq!(
            offset_for_pointer(track, thumb, 1000.0, travel / 2.0),
            Some(-500.0),
            "the middle of the track is the middle of the document"
        );
        assert_eq!(
            offset_for_pointer(track, thumb, 1000.0, 10.0 * travel),
            Some(-1000.0),
            "the pointer is clamped past the track"
        );
        assert_eq!(
            offset_for_pointer(track, thumb, 0.0, 0.0),
            None,
            "nothing to scroll, no mapping"
        );
    }

    /// The hit strip is the declared 20 px pointer target (the kit's
    /// geometry exception) at every tested zoom.
    #[gpui::test]
    fn scrollbar_hit_strip_is_twenty_pixels(cx: &mut TestAppContext) {
        for scale in [0.5_f32, 1.0, 1.25] {
            theme::set_ui_scale(scale);
            let scroll = ScrollHandle::new();
            let state = ScrollBarState::new();
            let s = scroll.clone();
            let st = state.clone();
            let (_root, wcx) = render_part(cx, move || {
                let s = s.clone();
                let st = st.clone();
                // A tracked scroll area gives the handle its bounds and
                // max offset while it paints.
                let content = div()
                    .flex()
                    .flex_col()
                    .h(px(2000.))
                    .child(div().h(px(900.)))
                    .child(div().h(px(900.)));
                div()
                    .flex()
                    .relative()
                    .w_full()
                    .h(px(400.))
                    .child(
                        div()
                            .id("area-1")
                            .overflow_y_scroll()
                            .track_scroll(&s)
                            .size_full()
                            .child(content),
                    )
                    .child(ScrollBar::new("sb-1", s, st))
            });
            let strip = wcx
                .debug_bounds("ui-scrollbar-sb-1")
                .unwrap_or_else(|| panic!("the strip recorded no bounds at zoom {scale}"));
            assert_eq!(
                f32::from(strip.size.width),
                f32::from(theme::z(theme::SCROLLBAR_HIT)),
                "the hit strip is the 20 px target at zoom {scale}: {strip:?}",
            );
        }
        theme::set_ui_scale(1.0);
    }

    /// Dragging the thumb scrolls: a mouse down on the thumb, a move down
    /// the track and a mouse up leaves the scroll offset behind, and the
    /// bar shows while dragging.
    #[gpui::test]
    fn scrollbar_drag_scrolls_the_handle(cx: &mut TestAppContext) {
        let scroll = ScrollHandle::new();
        let state = ScrollBarState::new();
        let scroll_in = scroll.clone();
        let state_in = state.clone();
        let (_root, wcx) = render_part(cx, move || {
            let scroll_in = scroll_in.clone();
            let state_in = state_in.clone();
            let content = div()
                .flex()
                .flex_col()
                .h(px(2000.))
                .child(div().h(px(900.)))
                .child(div().h(px(900.)));
            div()
                .flex()
                .relative()
                .w_full()
                .h(px(400.))
                .child(
                    div()
                        .id("area-2")
                        .overflow_y_scroll()
                        .track_scroll(&scroll_in)
                        .size_full()
                        .child(content),
                )
                .child(ScrollBar::new("sb-2", scroll_in, state_in))
        });
        let strip = wcx
            .debug_bounds("ui-scrollbar-sb-2")
            .unwrap_or_else(|| panic!("the strip recorded no bounds"));
        // Grab the thumb at the top of the track and drag it down by 40
        // logical px; the offset must follow.
        let grab_y = strip.origin.y + theme::z(theme::SP_8) + theme::z(theme::SP_16);
        let x = strip.origin.x + px(f32::from(strip.size.width) / 2.);
        wcx.simulate_event(MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: Point { x, y: grab_y },
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        assert!(state.is_dragging(), "the down grabbed the thumb");
        assert!(state.is_visible(), "the bar shows while dragging");
        wcx.simulate_event(MouseMoveEvent {
            position: Point {
                x,
                y: grab_y + px(40.0),
            },
            pressed_button: Some(gpui::MouseButton::Left),
            modifiers: gpui::Modifiers::default(),
        });
        let offset_after = f32::from(scroll.offset().y);
        assert!(
            offset_after < 0.0,
            "dragging down scrolled the content: {offset_after}"
        );
        wcx.simulate_event(MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: Point {
                x,
                y: grab_y + px(40.0),
            },
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert!(!state.is_dragging(), "the up ended the drag");
        assert!(
            f32::from(scroll.offset().y) == offset_after,
            "the offset survived the release"
        );
    }

    /// A press on the track behind an occluding overlay (a modal scrim) does
    /// not start a drag: the drag starts from the track's own hitbox, which
    /// the overlay occludes.
    #[gpui::test]
    fn scrollbar_behind_an_overlay_does_not_drag(cx: &mut TestAppContext) {
        let scroll = ScrollHandle::new();
        let state = ScrollBarState::new();
        let (scroll_in, state_in) = (scroll.clone(), state.clone());
        let (_root, wcx) = render_part(cx, move || {
            let content = div()
                .flex()
                .flex_col()
                .h(px(2000.))
                .child(div().h(px(900.)));
            div()
                .flex()
                .relative()
                .w_full()
                .h(px(400.))
                .child(
                    div()
                        .id("area-3")
                        .overflow_y_scroll()
                        .track_scroll(&scroll_in)
                        .size_full()
                        .child(content),
                )
                .child(ScrollBar::new("sb-3", scroll_in.clone(), state_in.clone()))
                .child(div().id("scrim").absolute().size_full().occlude())
        });
        let strip = wcx.debug_bounds("ui-scrollbar-sb-3").expect("the strip");
        let at = Point {
            x: strip.origin.x + px(f32::from(strip.size.width) / 2.),
            y: strip.origin.y + theme::z(theme::SP_8) + theme::z(theme::SP_16),
        };
        wcx.simulate_mouse_move(at, None, gpui::Modifiers::default());
        wcx.simulate_event(MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        assert!(!state.is_dragging(), "the overlay took the press");
        assert_eq!(scroll.offset().y, Pixels::ZERO, "nothing scrolled");
    }

    /// The rendered bar is found under its default selector
    /// (`ui-scrollbar-<id>`) and carries the label as its tooltip.
    #[gpui::test]
    fn scrollbar_carries_the_shared_conventions(cx: &mut TestAppContext) {
        let scroll = ScrollHandle::new();
        let state = ScrollBarState::new();
        let scroll_in = scroll.clone();
        let state_in = state.clone();
        let (_root, wcx) = render_part(cx, move || {
            let scroll_in = scroll_in.clone();
            let state_in = state_in.clone();
            div()
                .flex()
                .relative()
                .h(px(200.))
                .child(ScrollBar::new("sb-id", scroll_in, state_in))
        });
        assert!(
            wcx.debug_bounds("ui-scrollbar-sb-id").is_some(),
            "the bar is found under its default selector (ui-scrollbar-<id>)"
        );
        let bar = ScrollBar::new("sb-id", scroll, state)
            .label("Scroll")
            .selector("my-scroll");
        assert_eq!(bar.label_text().as_deref(), Some("Scroll"));
        assert_eq!(
            bar.tooltip_text().as_deref(),
            Some("Scroll"),
            "the label is the tooltip"
        );
    }

    /// The bar hides 1 s after the last offset change (the self-scheduled,
    /// generation-guarded hide), plus the `motion::BASE_MS` fade.
    #[gpui::test]
    fn scrollbar_hides_one_second_after_the_last_offset_change(cx: &mut TestAppContext) {
        crate::ui::motion::set_test_override(false);
        let scroll = ScrollHandle::new();
        let state = ScrollBarState::new();
        let scroll_in = scroll.clone();
        let state_in = state.clone();
        let (_root, wcx) = render_part(cx, move || {
            let scroll_in = scroll_in.clone();
            let state_in = state_in.clone();
            let content = div()
                .flex()
                .flex_col()
                .h(px(2000.))
                .child(div().h(px(900.)))
                .child(div().h(px(900.)));
            div()
                .flex()
                .relative()
                .w_full()
                .h(px(400.))
                .child(
                    div()
                        .id("area-3")
                        .overflow_y_scroll()
                        .track_scroll(&scroll_in)
                        .size_full()
                        .child(content),
                )
                .child(ScrollBar::new("hide", scroll_in, state_in))
        });
        // The first paint notices the offset (the initial change): the
        // bar shows and schedules its hide. The test scheduler's virtual
        // clock drives the timers (the repo idiom: `advance_clock` +
        // `run_until_parked`).
        assert!(state.is_visible(), "the first paint shows the bar");
        wcx.executor().advance_clock(Duration::from_millis(500));
        wcx.run_until_parked();
        assert!(
            state.is_visible(),
            "the bar is still shown 0.5 s after the change"
        );
        // Past 1 s: the hide fires and the `motion::BASE_MS` fade runs to
        // completion.
        wcx.executor().advance_clock(Duration::from_millis(900));
        wcx.run_until_parked();
        assert!(
            !state.is_visible(),
            "the bar hides 1 s (+ the fade) after the last offset change"
        );
        assert_eq!(state.alpha(), 0.0, "the hidden bar is fully transparent");
        crate::ui::motion::clear_test_override();
    }

    /// A newer offset change cancels the pending hide (the generation
    /// guard): the hide fires 1 s after the *last* change, not the first.
    #[gpui::test]
    fn scrollbar_a_newer_scroll_cancels_the_pending_hide(cx: &mut TestAppContext) {
        crate::ui::motion::set_test_override(false);
        let scroll = ScrollHandle::new();
        let state = ScrollBarState::new();
        let scroll_in = scroll.clone();
        let state_in = state.clone();
        let (_root, wcx) = render_part(cx, move || {
            let scroll_in = scroll_in.clone();
            let state_in = state_in.clone();
            let content = div()
                .flex()
                .flex_col()
                .h(px(2000.))
                .child(div().h(px(900.)))
                .child(div().h(px(900.)));
            div()
                .flex()
                .relative()
                .w_full()
                .h(px(400.))
                .child(
                    div()
                        .id("area-4")
                        .overflow_y_scroll()
                        .track_scroll(&scroll_in)
                        .size_full()
                        .child(content),
                )
                .child(ScrollBar::new("gen", scroll_in, state_in))
        });
        assert!(state.is_visible(), "the first paint shows the bar");
        // A second offset change 0.5 s in reschedules the hide (the test
        // scheduler's virtual clock drives the timers).
        wcx.executor().advance_clock(Duration::from_millis(500));
        wcx.run_until_parked();
        scroll.set_offset(Point {
            x: Pixels::ZERO,
            y: px(-100.),
        });
        wcx.update(|w, _cx| w.refresh());
        wcx.run_until_parked();
        // The first hide timer has now fired, but the newer generation
        // cancels it: the bar is still shown.
        wcx.executor().advance_clock(Duration::from_millis(600));
        wcx.run_until_parked();
        assert!(
            state.is_visible(),
            "the first hide was cancelled by the newer scroll"
        );
        // The rescheduled hide (1 s after the second scroll) completes
        // (plus the fade).
        wcx.executor().advance_clock(Duration::from_millis(700));
        wcx.run_until_parked();
        assert!(
            !state.is_visible(),
            "the rescheduled hide fired 1 s after the last scroll"
        );
        crate::ui::motion::clear_test_override();
    }

    /// The hide fades the thumb: mid-fade the alpha is between 0 and 1
    /// while the bar is still shown; the fade ends hidden.
    #[gpui::test]
    fn scrollbar_the_hide_fades_the_thumb(cx: &mut TestAppContext) {
        crate::ui::motion::set_test_override(false);
        let scroll = ScrollHandle::new();
        let state = ScrollBarState::new();
        let scroll_in = scroll.clone();
        let state_in = state.clone();
        let (_root, wcx) = render_part(cx, move || {
            let scroll_in = scroll_in.clone();
            let state_in = state_in.clone();
            let content = div()
                .flex()
                .flex_col()
                .h(px(2000.))
                .child(div().h(px(900.)))
                .child(div().h(px(900.)));
            div()
                .flex()
                .relative()
                .w_full()
                .h(px(400.))
                .child(
                    div()
                        .id("area-5")
                        .overflow_y_scroll()
                        .track_scroll(&scroll_in)
                        .size_full()
                        .child(content),
                )
                .child(ScrollBar::new("fade", scroll_in, state_in))
        });
        assert!(state.is_visible(), "the first paint shows the bar");
        // The hide fires at 1 s and spawns the fade.
        wcx.executor().advance_clock(Duration::from_millis(1000));
        wcx.run_until_parked();
        // Mid-fade: at least one 40 ms alpha step has run, but not the
        // last (the fade spans `motion::BASE_MS`).
        wcx.executor().advance_clock(Duration::from_millis(60));
        wcx.run_until_parked();
        assert!(state.is_visible(), "the fade is still in flight");
        assert!(
            state.alpha() < 1.0 && state.alpha() > 0.0,
            "the fade has dimmed the thumb: alpha {}",
            state.alpha()
        );
        wcx.executor().advance_clock(Duration::from_millis(300));
        wcx.run_until_parked();
        assert!(!state.is_visible(), "the fade ends hidden");
        crate::ui::motion::clear_test_override();
    }

    /// Under Reduce Motion the hide is instant: at the moment the hide
    /// timer fires the bar is gone (no `motion::BASE_MS` fade tail).
    #[gpui::test]
    fn scrollbar_reduce_motion_hides_instantly(cx: &mut TestAppContext) {
        crate::ui::motion::set_test_override(true);
        let scroll = ScrollHandle::new();
        let state = ScrollBarState::new();
        let scroll_in = scroll.clone();
        let state_in = state.clone();
        let (_root, wcx) = render_part(cx, move || {
            let scroll_in = scroll_in.clone();
            let state_in = state_in.clone();
            let content = div()
                .flex()
                .flex_col()
                .h(px(2000.))
                .child(div().h(px(900.)))
                .child(div().h(px(900.)));
            div()
                .flex()
                .relative()
                .w_full()
                .h(px(400.))
                .child(
                    div()
                        .id("area-6")
                        .overflow_y_scroll()
                        .track_scroll(&scroll_in)
                        .size_full()
                        .child(content),
                )
                .child(ScrollBar::new("rm", scroll_in, state_in))
        });
        assert!(state.is_visible(), "the first paint shows the bar");
        // The hide timer fires at 1 s; under Reduce Motion the hide is
        // instant (no `motion::BASE_MS` fade tail).
        wcx.executor().advance_clock(Duration::from_millis(1150));
        wcx.run_until_parked();
        assert!(
            !state.is_visible(),
            "Reduce Motion hides the bar the moment the hide timer fires"
        );
        crate::ui::motion::clear_test_override();
    }
}
