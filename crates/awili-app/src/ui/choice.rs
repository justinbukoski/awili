//! `ui::Choice` — the kit's segmented choice (KIT.md v3, batch K4).
//!
//! Segments on a `RAISE` track (`RADIUS_M`, `SP_4` padding and gap,
//! `target()`-tall segments with `SP_12` horizontal padding and
//! `RADIUS_S` corners); the selected segment sits on `PANEL_SOLID` with
//! the 2 px `CONTRAST` underline — the same face as the kit's `Tabs`
//! (the spec shares the look). The strip is the tab stop: the arrow
//! keys move the selection (wrapping), Enter/Space confirm it, and a
//! mouse click on a segment chooses it.
//!
//! **The confirm_to guard:** `.confirm_to(Some(n))` paints the armed
//! face on segment `n` — the v1 "Click again to switch" text (the
//! parent's `press_settings_choice` two-press state machine: the first
//! press arms, the second fires; the part is stateless and paints the
//! parent's state, firing `on_change` on every press).
//!
//! The shared conventions (K4b): `kit_shared!` builders, `focus_wiring`,
//! real pointer hover on each unselected segment (`FILL_HOVER`, its label
//! lifts to `TEXT`; `.hovered(true)` freezes it on the first unselected
//! segment for the gallery), a full-text tooltip on each enabled
//! segment's label, and disabled **dims**: the selection stays marked by a
//! `CONTROL_LINE` underline.

use gpui::{
    div, prelude::*, App, ClickEvent, ElementId, IntoElement, KeyDownEvent, KeyboardClickEvent,
    RenderOnce, SharedString, Window,
};

use crate::theme::{self, palette};
use crate::ui::focus_ring::{focus_ring, focus_ring_style};
use crate::ui::interaction::{child_id, focus_wiring, Common, HOVER_GROUP};
use crate::ui::text;

/// The part tag for the default `debug_selector`.
const CHOICE: &str = "choice";
/// The armed segment's face (the v1 word, kept verbatim).
pub const CHOICE_ARMED_FACE: &str = "Click again to switch";

/// A kit segmented choice. Build with [`Choice::new`] (the options and
/// the selected index) and chain `.on_change(…)` and the shared
/// conventions; the parent owns the selection and the guard's arming.
#[derive(IntoElement)]
pub struct Choice {
    common: Common,
    options: Vec<SharedString>,
    selected: usize,
    /// The `confirm_to` guard's armed segment (the parent's state).
    confirm_to_armed: Option<usize>,
    on_change: Option<ChangeHandler>,
}

/// The change handler (cloned into every segment's listener).
pub type ChangeHandler = std::sync::Arc<dyn Fn(usize, &ClickEvent, &mut Window, &mut App)>;

impl Choice {
    /// A choice over the given options, with `selected` chosen.
    pub fn new(id: impl Into<ElementId>, options: Vec<SharedString>, selected: usize) -> Self {
        let selected = selected.min(options.len().saturating_sub(1));
        Self {
            common: Common {
                id: id.into(),
                label: Some(options.get(selected).cloned().unwrap_or_default()),
                ..Default::default()
            },
            options,
            selected,
            confirm_to_armed: None,
            on_change: None,
        }
    }

    /// The change handler (the segment's index; fired by a mouse click
    /// on the segment, by Enter/Space on the focused strip, or by an
    /// arrow-key move — the kit's double-click guard rides on the
    /// segment clicks).
    pub fn on_change(
        mut self,
        f: impl Fn(usize, &ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(std::sync::Arc::new(f));
        self
    }

    /// The options, in paint order.
    pub fn options(&self) -> &[SharedString] {
        &self.options
    }

    /// The segment the part paints as selected.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The **confirm_to** guard's armed segment: while `Some(n)`,
    /// segment `n` paints the "Click again to switch" face (the
    /// parent's second press is pending on it).
    pub fn confirm_to(mut self, armed: Option<usize>) -> Self {
        self.confirm_to_armed = armed;
        self
    }

    /// The `confirm_to` guard's armed segment, if any.
    pub fn is_confirm_to_armed(&self) -> Option<usize> {
        self.confirm_to_armed
    }

    /// The segment's label as painted: the armed face while the
    /// `confirm_to` guard waits on its second press on it.
    pub fn segment_face(&self, index: usize) -> SharedString {
        match self.confirm_to_armed {
            Some(n) if n == index => CHOICE_ARMED_FACE.into(),
            _ => self.options.get(index).cloned().unwrap_or_default(),
        }
    }

    /// A segment's paint: (fill, underline-or-none, label colour), for
    /// selected / disabled / (frozen) hovered.
    pub fn segment_paint(
        selected: bool,
        disabled: bool,
        hovered: bool,
    ) -> (gpui::Rgba, Option<gpui::Rgba>, gpui::Rgba) {
        let p = palette();
        match (selected, disabled) {
            // Disabled dims, it never deletes: the selection keeps its
            // underline (in CONTROL_LINE) so the state stays readable.
            (true, true) => (p.transparent, Some(p.control_line), p.text_dim),
            (false, true) => (p.transparent, None, p.text_dim),
            (true, false) => (p.panel_solid, Some(p.contrast), p.text),
            (false, false) if hovered => (p.fill_hover, None, p.text),
            (false, false) => (p.transparent, None, p.text_dim),
        }
    }
}

crate::kit_shared!(Choice);

impl std::fmt::Debug for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Choice({})", self.common.selector_for(CHOICE))
    }
}

impl RenderOnce for Choice {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let p = palette();
        let options = self.options.clone();
        let selected = self.selected;
        let armed = self.confirm_to_armed;
        let common = self.common;
        let selector = common.selector_for(CHOICE);
        let disabled = common.is_disabled();
        let on_change = self.on_change.clone();
        let allow_double = common.allow_double_click;
        // The frozen hover (the gallery) sits on the first unselected
        // segment.
        let frozen = common
            .hovered
            .then(|| (0..options.len()).find(|i| *i != selected))
            .flatten();

        // The segment's painted label: the armed face while the
        // `confirm_to` guard waits on its second press on it.
        let face_of = |i: usize| -> SharedString {
            match armed {
                Some(n) if n == i => CHOICE_ARMED_FACE.into(),
                _ => options.get(i).cloned().unwrap_or_default(),
            }
        };

        let mut track = div()
            .id(child_id(&common.id, "track"))
            .flex()
            .items_center()
            .gap(theme::z(theme::SP_4))
            .p(theme::z(theme::SP_4))
            .rounded(theme::z(theme::RADIUS_M))
            .bg(p.raise);

        for i in 0..options.len() {
            let is_selected = i == selected;
            let (fill, underline, text_color) =
                Self::segment_paint(is_selected, disabled, frozen == Some(i));
            let mut label = text::label(face_of(i)).text_color_override(text_color);
            if !disabled {
                // An enabled segment's label tooltips its full text (a
                // disabled one has no tooltip of its own: the reason
                // wins).
                label = label.id(child_id(&common.id, &format!("segment-{i}-text")));
                if !is_selected {
                    label = label.hover_color(p.text);
                }
            }

            let mut segment = div()
                .id(child_id(&common.id, &format!("segment-{i}")))
                .debug_selector(|| format!("{selector}-segment-{i}"))
                .flex()
                .items_center()
                .px(theme::z(theme::SP_12))
                .h(theme::target())
                .min_w(theme::target())
                .rounded(theme::z(theme::RADIUS_S))
                .bg(fill)
                .when_some(underline, |d, c| d.border_b_2().border_color(c))
                .child(label);

            if !disabled {
                segment = segment.group(HOVER_GROUP);
                if !is_selected {
                    segment = segment.hover(|s| s.bg(palette().fill_hover));
                }
                if let Some(on_change) = on_change.clone() {
                    segment = segment.on_click(move |ev, window, cx| {
                        if allow_double || ev.click_count() <= 1 {
                            on_change(i, ev, window, cx);
                        }
                    });
                }
            }
            track = track.child(segment);
        }

        let mut root = div()
            .id(common.id.clone())
            .flex()
            .debug_selector(|| selector.clone())
            .child(track);

        if !disabled {
            root = focus_wiring(root, &common)
                .when(!common.focused, |d| d.focus_visible(focus_ring_style));
            let on_change_keys = on_change.clone();
            let n = options.len();
            root = root.on_key_down(move |ev: &KeyDownEvent, window, cx| {
                // The arrow keys move the selection (wrapping, the
                // spec's "the arrow keys move"); the choice has no
                // disabled segments to skip.
                let dir = match ev.keystroke.key.as_str() {
                    "left" | "up" => -1,
                    "right" | "down" => 1,
                    _ => return,
                };
                if n == 0 {
                    return;
                }
                let next = (selected as isize + dir).rem_euclid(n as isize) as usize;
                if let Some(on_change) = &on_change_keys {
                    window.prevent_default();
                    cx.stop_propagation();
                    on_change(
                        next,
                        &ClickEvent::Keyboard(KeyboardClickEvent::default()),
                        window,
                        cx,
                    );
                }
            });
            let on_change_click = on_change.clone();
            root = root.on_click(move |ev, window, cx| {
                // The keyboard click synthesis (Enter/Space on the
                // focused strip) confirms the current selection; a
                // mouse click on the track's background does nothing
                // (the segments carry their own clicks).
                if ev.is_keyboard() {
                    if let Some(on_change) = &on_change_click {
                        on_change(selected, ev, window, cx);
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

    const OPTIONS: [&str; 3] = ["Ask", "Ask before changes", "Go ahead"];

    fn sample_choice() -> Choice {
        Choice::new(
            "mode",
            OPTIONS.iter().map(|s| SharedString::from(*s)).collect(),
            0,
        )
    }

    /// The strip lays out headless with every segment (a `target()`
    /// hit each); the selected segment carries the 2 px `CONTRAST`
    /// underline (checked through the face: selected on `PANEL_SOLID`,
    /// the rest transparent).
    #[gpui::test]
    fn choice_renders_its_segments(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || div().flex().child(sample_choice()));
        let strip = wcx
            .debug_bounds("ui-choice-mode")
            .unwrap_or_else(|| panic!("the strip recorded no bounds"));
        assert!(
            f32::from(strip.size.height) >= f32::from(theme::target()),
            "the strip is at least target() tall: {strip:?}"
        );
        for sel in [
            "ui-choice-mode-segment-0",
            "ui-choice-mode-segment-1",
            "ui-choice-mode-segment-2",
        ] {
            let b = wcx
                .debug_bounds(sel)
                .unwrap_or_else(|| panic!("{sel} recorded no bounds"));
            assert!(
                f32::from(b.size.height) >= f32::from(theme::target()),
                "{sel}: the segment is a target-sized hit: {b:?}"
            );
        }
    }

    /// The segment click fires with the segment's index (the
    /// double-click guard rides on the segment); Enter/Space confirm
    /// the selected segment through the keyboard click; the arrow keys
    /// move the selection (wrapping).
    #[gpui::test]
    fn choice_clicks_and_keys_fire_the_parent(cx: &mut TestAppContext) {
        // The parent owns the selection (the part is stateless): the
        // factory re-builds the strip with it, and every fire re-renders
        // before the next input lands (the kit's K2 keyboard pattern).
        let sel = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let hits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (sel_in, hits_in) = (sel.clone(), hits.clone());
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let make = move || {
            let (sel_in, hits_in) = (sel_in.clone(), hits_in.clone());
            let selected = sel_in.get();
            div().flex().child(
                Choice::new(
                    "keys",
                    OPTIONS.iter().map(|s| SharedString::from(*s)).collect(),
                    selected,
                )
                .focus_handle(&h)
                .on_change(move |i, ev, _w, _c| {
                    hits_in.borrow_mut().push((i, ev.is_keyboard()));
                    sel_in.set(i);
                }),
            )
        };
        let (_root, wcx) = render_part(cx, make);
        let reflow = |wcx: &mut gpui::VisualTestContext| {
            wcx.update(|w, _cx| w.refresh());
            wcx.run_until_parked();
        };
        // Mouse: click segment 2 (index 1) twice — fires once.
        let at = wcx
            .debug_bounds("ui-choice-keys-segment-1")
            .unwrap_or_else(|| panic!("the second segment recorded no bounds"))
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
        assert_eq!(
            &*hits.borrow(),
            &[(1, false)],
            "the segment click fired once"
        );
        reflow(wcx); // the parent re-renders with the new selection

        // Keyboard: focus the strip; Enter confirms the current
        // selection (index 1 after the click); → walks to 2; ← back
        // to 1; ← to 0.
        wcx.update(|w, cx| w.focus(&handle, cx));
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        reflow(wcx);
        let mut key = |k: &str| {
            wcx.simulate_event(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse(k).unwrap(),
                is_held: false,
                prefer_character_input: false,
            });
            reflow(wcx);
        };
        key("right");
        assert_eq!(
            &*hits.borrow(),
            &[(1, false), (1, true), (2, true)],
            "Enter confirms, right walks on: {hits:?}"
        );
        key("left");
        key("left");
        assert_eq!(
            &*hits.borrow(),
            &[(1, false), (1, true), (2, true), (1, true), (0, true)],
            "the arrows walk the selection back: {hits:?}"
        );
    }

    /// The confirm_to guard's armed face: while armed on a segment,
    /// that segment paints the v1 "Click again to switch" text; a
    /// disabled choice carries the reason and fires nothing.
    #[gpui::test]
    fn choice_armed_face_and_disabled(cx: &mut TestAppContext) {
        let armed = sample_choice().confirm_to(Some(1));
        assert_eq!(
            armed.segment_face(1).as_str(),
            CHOICE_ARMED_FACE,
            "the armed segment reads the v1 face"
        );
        assert_eq!(
            armed.segment_face(0).as_str(),
            "Ask",
            "the other segments keep their labels"
        );
        assert_eq!(
            sample_choice().segment_face(1).as_str(),
            "Ask before changes",
            "disarmed the segment keeps its label"
        );

        let dead = std::rc::Rc::new(std::cell::Cell::new(0));
        let dead_in = dead.clone();
        let (_root, wcx) = render_part(cx, move || {
            let dead_in = dead_in.clone();

            div().flex().child(
                sample_choice()
                    .on_change(move |_i, _e, _w, _c| {
                        dead_in.set(1);
                    })
                    .disabled("The model does not support it"),
            )
        });
        let at = wcx
            .debug_bounds("ui-choice-mode-segment-0")
            .unwrap_or_else(|| panic!("the segment recorded no bounds"))
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
        assert_eq!(dead.get(), 0, "a disabled choice fires nothing");
        // K2c precedence: a disabled part's tooltip is its reason.
        assert_eq!(
            sample_choice().disabled("Locked").tooltip_text().as_deref(),
            Some("Locked"),
            "the disabled reason wins the tooltip"
        );
    }

    fn opts() -> Vec<SharedString> {
        vec!["Light".into(), "Dark".into(), "System".into()]
    }

    /// K4b: real pointer hover on an unselected segment lifts its label
    /// `TEXT_DIM` → `TEXT` (read from the paint probe) and drops back.
    #[gpui::test]
    fn choice_hover_paints(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::{hover, painted, same_color, unhover};
        let p = palette();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .pt(theme::z(theme::SP_40))
                .child(Choice::new("c", opts(), 0))
        });
        let label = child_id(&ElementId::from("c"), "segment-1-text");
        unhover(wcx);
        assert!(same_color(painted(&label).unwrap(), p.text_dim), "rest");
        hover(wcx, "ui-choice-c-segment-1");
        assert!(same_color(painted(&label).unwrap(), p.text), "hover lifts");
        unhover(wcx);
        assert!(same_color(painted(&label).unwrap(), p.text_dim), "leave");
    }

    /// K4b: the shared conventions — a plain tracked handle is a tab stop,
    /// a disabled choice is not (even with an explicit tab index), its
    /// reason is the tooltip that opens over a segment, and the disabled
    /// selection stays marked.
    #[gpui::test]
    fn choice_follows_the_shared_conventions(cx: &mut TestAppContext) {
        let on = cx.update(|cx| cx.focus_handle());
        let off = cx.update(|cx| cx.focus_handle());
        let (h_on, h_off) = (on.clone(), off.clone());
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .pt(theme::z(theme::SP_40))
                .child(
                    Choice::new("off", opts(), 1)
                        .focus_handle(&h_off)
                        .tab_index(0)
                        .disabled("Set by your workspace"),
                )
                .child(Choice::new("on", opts(), 0).focus_handle(&h_on))
        });
        for _ in 0..2 {
            wcx.update(|w, cx| w.focus_next(cx));
            wcx.run_until_parked();
            assert_eq!(wcx.update(|w, cx| w.focused(cx)), Some(on.clone()));
        }
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at(wcx, "ui-choice-off-segment-0")
                .as_deref(),
            Some("Set by your workspace"),
            "no segment tooltip covers the reason"
        );
        let p = palette();
        assert_eq!(
            Choice::segment_paint(true, true, false),
            (p.transparent, Some(p.control_line), p.text_dim),
            "the disabled selection keeps its underline"
        );
        let _ = off;
    }

    /// K4b round 2 (Codex): the selection as painted — the selected
    /// label paints `TEXT`, the others `TEXT_DIM` (read from the paint
    /// probe, not the paint table).
    #[gpui::test]
    fn choice_selection_paints(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::{painted, same_color, unhover};
        let p = palette();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .pt(theme::z(theme::SP_40))
                .child(Choice::new("sel", opts(), 2))
        });
        unhover(wcx);
        let id = ElementId::from("sel");
        for (i, want) in [(0, p.text_dim), (1, p.text_dim), (2, p.text)] {
            let got = painted(&child_id(&id, &format!("segment-{i}-text"))).unwrap();
            assert!(same_color(got, want), "segment {i}: {got:?}");
        }
    }
}
