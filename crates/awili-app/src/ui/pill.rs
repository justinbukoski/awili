//! `ui::Pill` — the kit's floating capsule (KIT.md v3, batch K3).
//!
//! A `RADIUS_PILL` capsule on the translucent `PANEL` fill, a 1 px
//! `CONTROL_LINE` edge and the kit's `float_shadow()`, with `SP_8`
//! vertical and `SP_16` horizontal padding and an `SP_8` gap (the
//! mockup's `.pill`: gap 10 → `SP_8`). Used for the title pill, the
//! activity island and the task bar (mockups `A-studio.html` `.doc`,
//! `.island`, `.taskbar`). The task bar's buttons are `quiet`; its lead
//! action is a `ghost` button with a `CONTRAST_SOFT` fill and a
//! `CONTRAST_TEXT` label (the mockup's `.btn.lead` inside `.pill`).
//!
//! The pill is a stateless container: its children (Dots, text, buttons)
//! are built by the parent, which owns every state — including a
//! pulsing `ActNow` dot's `.phase(n)` (the parent advances it on the
//! app's animation tick, and passes the dot its `.id(…)` and `.label(…)`
//! — the kit's "dot with meaning" rule). The pill itself never shifts
//! layout between pulse frames: the halo is outer box-shadow paint.

use gpui::{
    div, prelude::*, AnyElement, App, ElementId, IntoElement, RenderOnce, Rgba, SharedString,
    Window,
};

use crate::theme::{self, palette};
use crate::ui::interaction::PILL;

/// A kit floating capsule. Build with [`Pill::new`] and chain `.child(…)`
/// / `.children(…)` and the shared conventions; the parent owns the
/// children's state and re-renders the pill.
#[derive(IntoElement)]
pub struct Pill {
    common: crate::ui::interaction::Common,
    children: Vec<AnyElement>,
    shape: PillShape,
    bounded: bool,
    scroll_through: bool,
}

/// The pill's shape (the mockup's three pills, their padding snapped to the
/// 4 px grid — DESIGN.md §10.3).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum PillShape {
    /// The title pill (`.doc`): `SP_16` before, `SP_8` after, `target()`
    /// tall.
    #[default]
    Title,
    /// The activity island (`.island`): `SP_16` before, `SP_4` after,
    /// `ISLAND_H` tall.
    Island,
    /// The task bar (`.taskbar`): `SP_4` all round, `SP_4` between.
    Bar,
}

impl Pill {
    /// An empty capsule.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            children: Vec::new(),
            shape: PillShape::Title,
            bounded: false,
            scroll_through: false,
        }
    }

    /// The activity island shape.
    pub fn island(mut self) -> Self {
        self.shape = PillShape::Island;
        self
    }

    /// Constrain a title capsule to its parent's available width. Text slots
    /// keep their full label in their own tooltip when they truncate.
    pub fn bounded(mut self) -> Self {
        self.bounded = true;
        self
    }

    /// Let scroll events reach a scrollable surface behind this floating pill.
    /// Pointer clicks remain occluded by the pill and its children.
    pub fn scroll_through(mut self) -> Self {
        self.scroll_through = true;
        self
    }

    /// The task bar shape.
    pub fn bar(mut self) -> Self {
        self.shape = PillShape::Bar;
        self
    }

    /// The pill's shape.
    pub fn shape(&self) -> PillShape {
        self.shape
    }

    /// Append a child (a Dot, a line of `ui::text`, a Button …).
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.children.push(child.into_any_element());
        self
    }

    /// Replace the children with the given iterable.
    pub fn children(mut self, children: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.children = children
            .into_iter()
            .map(IntoElement::into_any_element)
            .collect();
        self
    }

    /// The number of children (for the part's tests).
    pub fn children_len(&self) -> usize {
        self.children.len()
    }

    /// The capsule's paint: (fill, edge) — the floating fill
    /// (`Palette::float_fill`: `PANEL` over a blur, else `PANEL_SOLID`) and
    /// the `CONTROL_LINE` edge.
    pub fn paint(&self) -> (Rgba, Rgba) {
        let p = palette();
        (p.float_fill(), p.control_line)
    }
}

crate::kit_shared!(Pill);

impl std::fmt::Debug for Pill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pill({})", self.common.selector_for(PILL))
    }
}

impl RenderOnce for Pill {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let (fill, edge) = self.paint();

        let common = self.common;
        let selector = common.selector_for(PILL);
        let focused = common.frozen_focus();
        let tip = common.tooltip();
        let children = self.children;

        let (pl, pr, gap, min_h) = match self.shape {
            PillShape::Title => (theme::SP_16, theme::SP_8, theme::SP_8, theme::target()),
            PillShape::Island => (
                theme::SP_16,
                theme::SP_4,
                theme::SP_8,
                theme::z(theme::ISLAND_H),
            ),
            PillShape::Bar => (theme::SP_4, theme::SP_4, theme::SP_4, theme::target()),
        };
        let mut root = div()
            .id(common.id.clone())
            .when(self.bounded, |d| d.min_w_0().max_w_full())
            .flex()
            .items_center()
            .gap(theme::z(gap))
            .pl(theme::z(pl))
            .pr(theme::z(pr))
            .when(self.shape == PillShape::Bar, |d| {
                d.py(theme::z(theme::SP_4))
            })
            .min_h(min_h)
            .rounded(theme::z(theme::RADIUS_PILL))
            .bg(fill)
            .border_1()
            .border_color(edge)
            // A click on the pill never reaches what is behind it. A caller
            // may allow wheel events through while keeping that click shield.
            .when(self.scroll_through, |d| d.block_mouse_except_scroll())
            .when(!self.scroll_through, |d| d.occlude())
            .debug_selector(|| selector.clone());
        for child in children {
            root = root.child(child);
        }

        if !common.is_disabled() {
            root = crate::ui::interaction::focus_wiring(root, &common).when(!common.focused, |d| {
                d.focus_visible(crate::ui::focus_ring::focus_ring_style_on(fill))
            });
        }
        if let Some(tip) = tip {
            root = root.tooltip(crate::icons::tip(tip.to_string()));
        }

        crate::ui::focus_ring::focus_ring_r(root, focused, theme::RADIUS_PILL)
            .when(self.bounded, |d| d.min_w_0().max_w_full())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::theme::Tone;
    use crate::ui::dot::Dot;
    use crate::ui::interaction::test_util::render_part;
    use crate::ui::text;

    /// The mockup's activity island: a pulsing `ActNow` dot (with id and
    /// label), a line of text and a `lead` action — the dot's pulse is
    /// driven by the parent's `.phase(n)`, and the pill's layout is
    /// unchanged between pulse frames (the halo is outer paint).
    fn island() -> Pill {
        Pill::new("island")
            .child(
                Dot::new(Tone::ActNow)
                    .label("Waiting on the owner")
                    .id("island-dot")
                    .pulse(),
            )
            .child(text::label("Fix the zcode wrapper is waiting for you"))
            .child(crate::ui::button::Button::new("island-review", "Review").lead())
    }

    /// The hit box: a text-only capsule is at least `target()` tall, and
    /// the island's lead button keeps its `target()` hit box, at every
    /// tested zoom.
    #[gpui::test]
    fn pill_hit_box_is_at_least_target_at_every_zoom(cx: &mut TestAppContext) {
        for scale in [0.5_f32, 1.0, 1.25] {
            theme::set_ui_scale(scale);
            let (_root, wcx) = render_part(cx, move || {
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_8))
                    .child(Pill::new("doc").child(text::label("PhotoCraft UI study")))
                    .child(island())
            });
            let pill = wcx
                .debug_bounds("ui-pill-doc")
                .unwrap_or_else(|| panic!("the text-only pill recorded no bounds at zoom {scale}"));
            assert!(
                f32::from(pill.size.height) >= f32::from(theme::target()),
                "the text-only pill is ≥ target() tall at zoom {scale}: {pill:?}",
            );
            let btn = wcx
                .debug_bounds("ui-button-island-review")
                .unwrap_or_else(|| panic!("the island's lead button recorded no bounds"));
            assert!(
                f32::from(btn.size.width) >= 44.0 && f32::from(btn.size.height) >= 44.0,
                "the island's lead button keeps its 44×44 hit box: {btn:?}",
            );
        }
        theme::set_ui_scale(1.0);
    }

    /// The island's pulsing dot: it lays out at exactly `DOT` and the
    /// pill's bounds are identical between pulse frames — the halo is a
    /// box shadow, never layout — and the dot carries its id (the tooltip
    /// needs a stateful element) and its label.
    #[gpui::test]
    fn island_pulse_driven_by_phase_keeps_the_layout(cx: &mut TestAppContext) {
        let mut frames = Vec::new();
        for phase in [0u8, 5, 16] {
            let (_root, wcx) = render_part(cx, move || {
                div().flex().child(
                    island().children(
                        [
                            Dot::new(Tone::ActNow)
                                .label("Waiting on the owner")
                                .id("island-dot")
                                .pulse()
                                .phase(phase)
                                .into_any_element(),
                            text::label("Fix the zcode wrapper is waiting for you")
                                .into_any_element(),
                            crate::ui::button::Button::new("island-review", "Review")
                                .lead()
                                .into_any_element(),
                        ]
                        .into_iter()
                        .map(IntoElement::into_any_element),
                    ),
                )
            });
            // The dot's debug_selector is the constant "ui-dot" (K2).
            let dot = wcx
                .debug_bounds("ui-dot")
                .unwrap_or_else(|| panic!("the island's dot (phase {phase}) recorded no bounds"));
            let pill = wcx
                .debug_bounds("ui-pill-island")
                .unwrap_or_else(|| panic!("the island pill (phase {phase}) recorded no bounds"));
            assert_eq!(
                f32::from(dot.size.width),
                f32::from(theme::z(theme::DOT)),
                "the dot is exactly DOT wide (phase {phase}): {dot:?}",
            );
            frames.push((pill.origin, pill.size));
        }
        assert_eq!(
            frames[0], frames[1],
            "phase 0 → 5 must not move or resize the pill"
        );
        assert_eq!(
            frames[1], frames[2],
            "phase 5 → 16 must not move or resize the pill"
        );
    }

    /// The face: the `PANEL` fill with the `CONTROL_LINE` edge (read
    /// through the palette, so a light swap retints the capsule).
    #[test]
    fn pill_paint_uses_the_panel_tokens() {
        let p = palette();
        assert_eq!(Pill::new("x").paint(), (p.float_fill(), p.control_line));
        assert_eq!(
            p.float_fill(),
            p.panel_solid,
            "the solid fallback (no blur yet)"
        );
    }

    /// The task bar's face table: the quiet buttons and the lead action
    /// (`ghost` with the `CONTRAST_SOFT` fill and `CONTRAST_TEXT` label)
    /// sit in the capsule with the `SP_8` gap.
    #[test]
    fn task_bar_uses_quiet_buttons_and_a_lead_action() {
        let p = palette();
        let bar = Pill::new("taskbar")
            .child(
                crate::ui::button::Button::new("tb-copy", "Copy")
                    .quiet()
                    .full_width(),
            )
            .child(
                crate::ui::button::Button::new("tb-run", "Run as a job")
                    .ghost()
                    .full_width(),
            )
            .child(
                crate::ui::button::Button::new("tb-report", "Open report")
                    .quiet()
                    .full_width(),
            );
        assert_eq!(
            bar.children_len(),
            3,
            "the task bar holds its three actions"
        );
        let (fill, edge) = bar.paint();
        assert_eq!((fill, edge), (p.float_fill(), p.control_line));
        // The lead face the mockup gives the task bar's emphasised action.
        let lead = crate::ui::button::Button::new("tb-lead", "Ask the council").lead();
        assert_eq!(
            lead.paint(),
            (p.contrast_soft, None, p.contrast_text, true),
            "the lead action is CONTRAST_SOFT with CONTRAST_TEXT weight-600"
        );
    }
}
