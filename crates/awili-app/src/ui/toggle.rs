//! `ui::Toggle` — the kit's on/off switch (KIT.md v3, batch K4).
//!
//! A `SWITCH_W` × `SWITCH_H` (44 × 26) painted track centred in the 44 px
//! hit box (`theme::target()`): `CONTRAST` fill when on, the
//! `FILL_GHOST` fill with the `CONTROL_LINE` edge when off, and the
//! `TEXT` knob (a circle) riding the track with the same inset on both
//! sides. Space and Enter toggle it (the keyboard click flows through the
//! same `on_toggle` as the mouse, guard included). The mockups carry no
//! switch face, so the part is built straight from the KIT tokens.
//!
//! The shared conventions (K4b): `kit_shared!` builders (`.hovered`,
//! `.allow_double_click`, `.disabled_if` …), `focus_wiring` (a tracked
//! handle is a tab stop; disabled is not), real pointer hover on the track
//! (`FILL_HOVER` off, `CONTRAST_HOVER` on), and disabled **dims**: the on
//! state stays readable as `CONTRAST_SOFT`, the off track keeps its edge,
//! and the knob turns `TEXT_DIM`.

use gpui::{div, prelude::*, App, ClickEvent, ElementId, IntoElement, RenderOnce, Rgba, Window};

use crate::theme::{self, palette};
use crate::ui::focus_ring::focus_ring_style;
use crate::ui::interaction::{child_id, focus_wiring, Common};

/// The part tag for the default `debug_selector`.
const TOGGLE: &str = "toggle";

/// A kit on/off switch. Build with [`Toggle::new(id, on)`] and chain
/// `.on_toggle(…)` and the shared conventions; the parent owns the
/// `on` state and re-renders the part.
#[derive(IntoElement)]
pub struct Toggle {
    common: Common,
    on: bool,
    on_toggle: Option<crate::ui::interaction::ClickHandler>,
}

impl Toggle {
    /// A switch showing its state (`on`); its accessible name defaults to
    /// the state ("On" / "Off").
    pub fn new(id: impl Into<ElementId>, on: bool) -> Self {
        Self {
            common: Common {
                id: id.into(),
                label: Some(if on { "On" } else { "Off" }.into()),
                ..Default::default()
            },
            on,
            on_toggle: None,
        }
    }

    /// The toggle's handler (fired by a mouse click or by Enter/Space
    /// on the focused switch; the kit's double-click guard applies).
    pub fn on_toggle(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Box::new(f));
        self
    }

    /// The state the part paints.
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// The knob's face: its colour (`TEXT`, `TEXT_DIM` when disabled) and
    /// its left offset in the track (`SWITCH_KNOB_PAD` off,
    /// `SWITCH_KNOB_ON_X` on — the same inset from the right edge).
    pub fn knob_face(on: bool, disabled: bool) -> (Rgba, f32) {
        let p = palette();
        (
            if disabled { p.text_dim } else { p.text },
            if on {
                theme::SWITCH_KNOB_ON_X
            } else {
                theme::SWITCH_KNOB_PAD
            },
        )
    }

    /// The track's face: (fill, edge-or-none). Disabled dims, it never
    /// deletes: on stays `CONTRAST_SOFT`, off keeps its `CONTROL_LINE`
    /// edge with no fill. `hovered` is the frozen hover (the gallery);
    /// real pointer hover paints the same.
    pub fn track_face(on: bool, disabled: bool, hovered: bool) -> (Rgba, Option<Rgba>) {
        let p = palette();
        match (on, disabled, hovered) {
            (true, true, _) => (p.contrast_soft, None),
            (false, true, _) => (p.transparent, Some(p.control_line)),
            (true, false, false) => (p.contrast, None),
            (true, false, true) => (p.contrast_hover, None),
            (false, false, false) => (p.fill_ghost, Some(p.control_line)),
            (false, false, true) => (p.fill_hover, Some(p.control_line)),
        }
    }
}

crate::kit_shared!(Toggle);

impl std::fmt::Debug for Toggle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Toggle({})", self.common.selector_for(TOGGLE))
    }
}

impl RenderOnce for Toggle {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let common = self.common;
        let selector = common.selector_for(TOGGLE);
        let disabled = common.is_disabled();
        let (track_fill, track_edge) = Self::track_face(self.on, disabled, common.hovered);
        let (hover_fill, _) = Self::track_face(self.on, disabled, true);
        let (knob_color, knob_x) = Self::knob_face(self.on, disabled);
        let knob = theme::z(theme::SWITCH_KNOB);

        let mut track = div()
            .id(child_id(&common.id, "track"))
            .debug_selector(|| format!("{selector}-track"))
            .relative()
            .w(theme::z(theme::SWITCH_W))
            .h(theme::z(theme::SWITCH_H))
            .rounded(theme::z(theme::RADIUS_PILL))
            .bg(track_fill)
            .when_some(track_edge, |d, c| d.border_1().border_color(c))
            .child(
                div()
                    .flex()
                    .absolute()
                    .top(theme::z(theme::SWITCH_KNOB_PAD))
                    .left(theme::z(knob_x))
                    .size(knob)
                    .rounded(theme::z(theme::RADIUS_PILL))
                    .bg(knob_color)
                    .debug_selector(|| format!("{selector}-knob")),
            );
        // Real pointer hover on the enabled track (the hit box is the
        // hover group, so the whole 44 px target counts).
        if !disabled {
            track = track.group_hover(crate::ui::interaction::HOVER_GROUP, move |s| {
                s.bg(hover_fill)
            });
        }

        let mut root = div()
            .id(common.id.clone())
            .flex()
            .items_center()
            .justify_center()
            .min_h(theme::target())
            .min_w(theme::target())
            .debug_selector(|| selector.clone())
            .child(track);
        if !disabled {
            root = focus_wiring(root, &common)
                .group(crate::ui::interaction::HOVER_GROUP)
                .when(!common.focused, |d| d.focus_visible(focus_ring_style));
            let on_toggle = self.on_toggle;
            let allow_double = common.allow_double_click;
            root = root.on_click(move |ev, window, cx| {
                if allow_double || ev.click_count() <= 1 {
                    if let Some(on_toggle) = &on_toggle {
                        on_toggle(ev, window, cx);
                    }
                }
            });
        }
        if let Some(tip) = common.tooltip() {
            root = root.tooltip(crate::icons::tip(tip.to_string()));
        }
        crate::ui::focus_ring::focus_ring(root, common.frozen_focus())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// The switch lays out headless: the track is the 44 × 26 token
    /// pair and the hit box is at least the target in both directions.
    #[gpui::test]
    fn toggle_hit_box_and_track_size(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || div().flex().child(Toggle::new("sw", true)));
        let root = wcx
            .debug_bounds("ui-toggle-sw")
            .unwrap_or_else(|| panic!("the toggle recorded no bounds"));
        assert!(
            f32::from(root.size.height) >= f32::from(theme::target()),
            "the hit box is at least target() tall: {root:?}"
        );
        let track = wcx
            .debug_bounds("ui-toggle-sw-track")
            .unwrap_or_else(|| panic!("the track recorded no bounds"));
        assert_eq!(
            (f32::from(track.size.width), f32::from(track.size.height)),
            (
                f32::from(theme::z(theme::SWITCH_W)),
                f32::from(theme::z(theme::SWITCH_H)),
            ),
            "the track is exactly SWITCH_W × SWITCH_H: {track:?}"
        );
        assert!(
            wcx.debug_bounds("ui-toggle-sw-knob").is_some(),
            "the knob laid out"
        );
    }

    /// The face table: on is the CONTRAST fill, off is the FILL_GHOST
    /// fill with the CONTROL_LINE edge; the knob rides to the right when
    /// on; disabled paints no fill and no edge.
    #[test]
    fn toggle_face_table_matches_the_spec() {
        let p = palette();
        assert_eq!(Toggle::track_face(true, false, false), (p.contrast, None));
        assert_eq!(
            Toggle::track_face(false, false, false),
            (p.fill_ghost, Some(p.control_line)),
            "the off face carries the CONTROL_LINE edge"
        );
        assert_eq!(
            Toggle::track_face(true, false, true),
            (p.contrast_hover, None)
        );
        assert_eq!(
            Toggle::track_face(false, false, true),
            (p.fill_hover, Some(p.control_line))
        );
        assert_eq!(
            Toggle::track_face(true, true, false),
            (p.contrast_soft, None),
            "disabled dims: the on state stays readable"
        );
        assert_eq!(
            Toggle::track_face(false, true, false),
            (p.transparent, Some(p.control_line)),
            "disabled dims: the off track keeps its edge"
        );
        let (knob_on, x_on) = Toggle::knob_face(true, false);
        let (_, x_off) = Toggle::knob_face(false, false);
        assert_eq!(knob_on, p.text);
        assert_eq!(
            Toggle::knob_face(true, true).0,
            p.text_dim,
            "the disabled knob dims"
        );
        assert_eq!(
            x_on,
            theme::SWITCH_KNOB_ON_X,
            "the on knob keeps the inset from the right edge"
        );
        assert_eq!(
            x_off,
            theme::SWITCH_KNOB_PAD,
            "the off knob rides to the left"
        );
    }

    /// The toggle fires through the handler (mouse click and Enter/
    /// Space through the keyboard click); the double-click guard drops
    /// the second click; a disabled switch fires nothing.
    #[gpui::test]
    fn toggle_click_fires_once_and_disabled_is_inert(cx: &mut TestAppContext) {
        let fired = std::rc::Rc::new(std::cell::Cell::new(0));
        let fired_in = fired.clone();
        let (_root, wcx) = render_part(cx, move || {
            let fired_in = fired_in.clone();

            div()
                .flex()
                .child(Toggle::new("click", false).label("Autonomy").on_toggle(
                    move |_e, _w, _c| {
                        fired_in.set(1);
                    },
                ))
        });
        let at = wcx
            .debug_bounds("ui-toggle-click")
            .unwrap_or_else(|| panic!("the toggle recorded no bounds"))
            .center();
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
        assert_eq!(fired.get(), 1, "the double click fires the toggle once");

        let dead = std::rc::Rc::new(std::cell::Cell::new(0));
        let dead_in = dead.clone();
        let (_root, wcx) = render_part(cx, move || {
            let dead_in = dead_in.clone();

            div().flex().child(
                Toggle::new("dead", false)
                    .on_toggle(move |_e, _w, _c| {
                        dead_in.set(1);
                    })
                    .disabled("The model does not support it"),
            )
        });
        let at = wcx
            .debug_bounds("ui-toggle-dead")
            .unwrap_or_else(|| panic!("the toggle recorded no bounds"))
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
        assert_eq!(dead.get(), 0, "a disabled switch is not clickable");
        // K2c precedence: a disabled part's tooltip is its reason.
        assert_eq!(
            Toggle::new("d2", true)
                .disabled("Locked")
                .tooltip_text()
                .as_deref(),
            Some("Locked"),
            "the disabled reason wins the tooltip"
        );
    }

    /// Enter and Space toggle the focused switch through the same
    /// handler (GPUI's keyboard click).
    #[gpui::test]
    fn toggle_enter_and_space_activate_the_focused_switch(cx: &mut TestAppContext) {
        let handle = cx.update(|cx| cx.focus_handle());
        let fired = std::rc::Rc::new(std::cell::Cell::new(0));
        let fired_in = fired.clone();
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            let fired_in = fired_in.clone();

            div().flex().child(
                Toggle::new("kbd", false)
                    .label("Autonomy")
                    .focus_handle(&h)
                    .on_toggle(move |_e, _w, _c| {
                        fired_in.set(fired_in.get() + 1);
                    }),
            )
        });
        wcx.update(|w, cx| w.focus(&handle, cx));
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        assert_eq!(fired.get(), 1, "Enter on the focused switch fires once");
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("space").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("space").unwrap(),
        });
        assert_eq!(fired.get(), 2, "Space on the focused switch fires again");
    }

    /// K4b: the on knob keeps the same inset from the track's right edge
    /// as the off knob from its left (it used to sit flush at 22).
    #[gpui::test]
    fn toggle_knob_keeps_its_inset(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .child(Toggle::new("on", true))
                .child(Toggle::new("off", false))
        });
        let track = wcx.debug_bounds("ui-toggle-on-track").unwrap();
        let knob = wcx.debug_bounds("ui-toggle-on-knob").unwrap();
        let right_inset = f32::from(track.origin.x + track.size.width)
            - f32::from(knob.origin.x + knob.size.width);
        assert!(
            (right_inset - f32::from(theme::z(theme::SWITCH_KNOB_PAD))).abs() < 0.5,
            "on: right inset {right_inset}"
        );
        let track = wcx.debug_bounds("ui-toggle-off-track").unwrap();
        let knob = wcx.debug_bounds("ui-toggle-off-knob").unwrap();
        let left_inset = f32::from(knob.origin.x - track.origin.x);
        assert!(
            left_inset > 0.0,
            "off: the knob keeps its left inset {left_inset}"
        );
    }

    /// K4b: the shared conventions — a plain tracked handle is a tab stop,
    /// a disabled switch is not, the hit box is at least the target in
    /// both directions at zoom 0.5, and the reason is the tooltip that
    /// actually opens.
    #[gpui::test]
    fn toggle_follows_the_shared_conventions(cx: &mut TestAppContext) {
        let on = cx.update(|cx| cx.focus_handle());
        let off = cx.update(|cx| cx.focus_handle());
        let (h_on, h_off) = (on.clone(), off.clone());
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .pt(theme::z(theme::SP_40))
                .child(
                    Toggle::new("t-off", true)
                        .focus_handle(&h_off)
                        .disabled("Managed by IT"),
                )
                .child(Toggle::new("t-on", false).focus_handle(&h_on))
        });
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            Some(on.clone()),
            "Tab skips the disabled one"
        );
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            Some(on),
            "and only reaches the enabled one"
        );
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at(wcx, "ui-toggle-t-off").as_deref(),
            Some("Managed by IT")
        );
        theme::set_ui_scale(0.5);
        let (_root, wcx) = render_part(cx, move || div().flex().child(Toggle::new("small", true)));
        let b = wcx.debug_bounds("ui-toggle-small").unwrap();
        assert!(
            f32::from(b.size.width) >= 44.0 && f32::from(b.size.height) >= 44.0,
            "{b:?}"
        );
        theme::set_ui_scale(1.0);
        let _ = off;
    }
}
