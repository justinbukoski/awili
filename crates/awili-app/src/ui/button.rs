//! `ui::Button` — the kit's text button (KIT.md v3, batch K2).
//!
//! One line of `type_::BODY` text, four faces, the kit's 44 px hit box
//! (`theme::target()`). The look matches the owner-approved mockup's
//! `.btn`: 16 px horizontal padding, `RADIUS_M` corners, 8 px icon gap,
//! weight-600 on the `primary` and `lead` faces — with the kit's shared
//! interactive conventions (see [`super::interaction`]).
//!
//! Every enabled face has a real pointer hover (K2c): the primary fill
//! lightens to `CONTRAST_HOVER` (the mockup's `#5592f7`), ghost and quiet
//! wash to `FILL_HOVER` (quiet lifts its text and icon to `TEXT`), and lead
//! steps to `CONTRAST_SOFT_HOVER`.

use gpui::{
    div, prelude::*, App, ClickEvent, ElementId, IntoElement, RenderOnce, Rgba, SharedString,
    Window,
};

use crate::icons::{self, Icon};
use crate::theme::{self, palette};
use crate::ui::focus_ring::{focus_ring_r, focus_ring_style};
use crate::ui::interaction::BUTTON;
use crate::ui::interaction::{child_id, focus_wiring, paint_probe, ClickHandler, HOVER_GROUP};

/// A button face (KIT.md § Button).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Variant {
    /// `CONTRAST` fill, `ON_CONTRAST` weight-600 text.
    Primary,
    /// (default) `FILL_GHOST` fill, 1 px `CONTROL_LINE` edge, `TEXT`.
    #[default]
    Ghost,
    /// No fill, no edge, `TEXT_DIM`; hover `FILL_HOVER` + `TEXT`.
    Quiet,
    /// `CONTRAST_SOFT` fill, `CONTRAST_TEXT` weight-600 text.
    Lead,
}

/// The kit's shared button face table (KIT.md § Button — "IconButton: the
/// variants are the same as `Button`"): (fill, edge, text colour,
/// weight-600). Disabled wins: `TEXT_DIM`, no fill, no edge. `hovered`
/// is the frozen hover the gallery pins; real pointer hover paints the
/// same values through [`hover_paint`].
pub fn face_paint(
    variant: Variant,
    disabled: bool,
    hovered: bool,
) -> (Rgba, Option<Rgba>, Rgba, bool) {
    let p = palette();
    if disabled {
        return (p.transparent, None, p.text_dim, false);
    }
    let (rest_fill, edge, rest_text, strong) = match variant {
        Variant::Primary => (p.contrast, None, p.on_contrast, true),
        Variant::Ghost => (p.fill_ghost, Some(p.control_line), p.text, false),
        Variant::Quiet => (p.transparent, None, p.text_dim, false),
        Variant::Lead => (p.contrast_soft, None, p.contrast_text, true),
    };
    if hovered {
        let (fill, text) = hover_paint(variant);
        (fill, edge, text, strong)
    } else {
        (rest_fill, edge, rest_text, strong)
    }
}

/// An enabled face's hover paint: (fill, text colour). Every face has one
/// (the mockup's `:hover` rules; the lead step is the kit's — see
/// `theme::CONTRAST_SOFT_HOVER`).
pub fn hover_paint(variant: Variant) -> (Rgba, Rgba) {
    let p = palette();
    match variant {
        Variant::Primary => (p.contrast_hover, p.on_contrast),
        Variant::Ghost => (p.fill_hover, p.text),
        Variant::Quiet => (p.fill_hover, p.text),
        Variant::Lead => (p.contrast_soft_hover, p.contrast_text),
    }
}

/// A kit text button. Build with [`Button::new`] (a ghost face) and chain
/// the face and the shared conventions; the parent owns every state the
/// part paints (click, hover, focus, selection) and re-renders it.
#[derive(IntoElement)]
pub struct Button {
    common: crate::ui::interaction::Common,
    label: SharedString,
    variant: Variant,
    icon: Option<Icon>,
    selected: bool,
    /// The leading check's room is reserved (see `reserve_selected_check`).
    reserve_check: bool,
    full_width: bool,
    in_pill: bool,
    bounds: Option<std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>>,
    on_click: Option<ClickHandler>,
}

impl Button {
    /// A `ghost` button (the default face) with the given visible label
    /// (which doubles as the accessible name).
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                label: Some(label.clone()),
                ..Default::default()
            },
            label,
            variant: Variant::Ghost,
            icon: None,
            selected: false,
            reserve_check: false,
            full_width: false,
            in_pill: false,
            bounds: None,
            on_click: None,
        }
    }

    /// The `CONTRAST`-filled primary face.
    /// Capture the actual face for caller-owned layout/reveal.
    pub fn bounds_handle(
        mut self,
        bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    ) -> Self {
        self.bounds = Some(bounds);
        self
    }

    pub fn primary(mut self) -> Self {
        self.variant = Variant::Primary;
        self
    }

    /// The default face: `FILL_GHOST` fill, 1 px `CONTROL_LINE` edge.
    pub fn ghost(mut self) -> Self {
        self.variant = Variant::Ghost;
        self
    }

    /// The quiet face: no fill, `TEXT_DIM` text, hover `FILL_HOVER`.
    pub fn quiet(mut self) -> Self {
        self.variant = Variant::Quiet;
        self
    }

    /// The lead face: `CONTRAST_SOFT` fill, `CONTRAST_TEXT` text.
    pub fn lead(mut self) -> Self {
        self.variant = Variant::Lead;
        self
    }

    /// Replace the visible label (and the accessible name).
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self.common.label = Some(self.label.clone());
        self
    }

    /// The current face (for the part's tests and the gallery).
    pub fn variant(&self) -> Variant {
        self.variant
    }

    /// A leading `ICON` icon, tinted with the button's text colour.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The selected state: a trailing `Check` icon and a `CONTRAST` edge
    /// (on the ghost face).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Reserve the selected check's room, the check leading the label: while
    /// unselected the check is there unseen, and the border is reserved on
    /// every face. Selecting the button then never moves its label or the
    /// controls beside it (the Settings choice faces). Off by default: a
    /// selected button trails its check, and an unselected one has none.
    pub fn reserve_selected_check(mut self, reserve: bool) -> Self {
        self.reserve_check = reserve;
        self
    }

    /// The face a button takes inside a `Pill` (the mockup's `.pill .btn`):
    /// fully rounded, with `type_::LABEL` text.
    pub fn in_pill(mut self) -> Self {
        self.in_pill = true;
        self
    }

    /// The face's corner radius (logical px): `RADIUS_PILL` in a pill, else
    /// `RADIUS_M`. The frozen focus ring's corners follow it.
    pub fn face_radius(&self) -> f32 {
        if self.in_pill {
            theme::RADIUS_PILL
        } else {
            theme::RADIUS_M
        }
    }

    /// Stretch the button to its parent's width.
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// The click handler (the parent's `cx.listener(…)`); the kit's
    /// double-click guard applies. Enter/Space on the focused button fire
    /// it too.
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    /// Whether the button shows its `Check` + `CONTRAST`-edge selected face.
    pub fn is_selected(&self) -> bool {
        self.selected
    }

    /// The face's paint: (fill, edge, text colour, weight-600) — the
    /// shared button face table. Disabled wins: `TEXT_DIM`, no fill, no
    /// edge.
    pub fn paint(&self) -> (Rgba, Option<Rgba>, Rgba, bool) {
        face_paint(self.variant, self.common.is_disabled(), self.common.hovered)
    }

    /// The edge colour actually painted: `CONTRAST` when the button is
    /// selected (enabled), else the face's edge.
    pub fn edge(&self) -> Option<Rgba> {
        if self.selected && !self.common.is_disabled() {
            return Some(palette().contrast);
        }
        self.paint().1
    }
}

crate::kit_shared!(Button, skip_label);

impl std::fmt::Debug for Button {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Button({})", self.common.selector_for(BUTTON))
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let (fill, _face_edge, text_color, strong) = self.paint();
        // The painted edge: `edge()`, not `paint().1` — a selected button
        // paints its `CONTRAST` edge (KIT.md § Button).
        let edge = self.edge();
        let disabled = self.common.is_disabled();
        let (hover_fill, hover_text) = hover_paint(self.variant);

        // The face's label is raw text on its own div (the `type_::BODY`
        // line, `TRUNCATE` like a single-line slot) instead of a `ui::text`
        // part: `ui::text` pins its own colour, so only raw text can
        // inherit the face's text colour — including the real pointer
        // hover's. The button's root carries the label as its tooltip, so
        // the long-text convention still holds.
        let label_id = child_id(&self.common.id, "label");
        // The face radius, which the frozen ring's corners follow.
        let radius = self.face_radius();
        let ty = if self.in_pill {
            theme::type_::LABEL
        } else {
            theme::type_::BODY
        };
        // `min_w_0` lets a narrow parent shrink the label, so it truncates
        // instead of widening the button (KIT.md § Long text).
        let mut label = div()
            .text_size(theme::z(ty.size))
            .line_height(theme::z(ty.line))
            .truncate()
            .min_w_0()
            .relative()
            .child(self.label.clone())
            .children(paint_probe(&label_id));
        if strong {
            label = label.font_weight(gpui::FontWeight::SEMIBOLD);
        }

        // Icons paint their own `text_color` (an svg does not inherit), so
        // each takes the face group's hover colour itself.
        let glyph = move |which: Icon| {
            let svg = icons::icon(which, theme::ICON, text_color);
            if disabled {
                svg
            } else {
                svg.group_hover(HOVER_GROUP, move |s| s.text_color(hover_text))
            }
        };

        let icon = self.icon;
        let selected = self.selected;
        let reserve = self.reserve_check;
        // The reserved leading check: shown when selected, else kept in its
        // room unseen (invisible, so no `ui-button-check` probe for it).
        let lead_check = reserve.then(|| {
            if selected {
                glyph(Icon::Check)
                    .debug_selector(|| "ui-button-check".to_string())
                    .into_any_element()
            } else {
                div()
                    .invisible()
                    .child(glyph(Icon::Check))
                    .into_any_element()
            }
        });
        // The leading icon is always painted (disabled dims it to
        // `TEXT_DIM`, it never deletes) — the tint is `text_color`, which
        // is `TEXT_DIM` when disabled.
        let face = div()
            .flex()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .px(theme::z(theme::SP_16))
            .h(theme::target())
            .min_w(theme::target())
            .rounded(theme::z(radius))
            .bg(fill)
            .text_color(text_color)
            // A reserved button keeps its 1 px edge on every face, so the
            // selected edge never widens it.
            .when(edge.is_some() || reserve, |d| {
                d.border_1()
                    .border_color(edge.unwrap_or_else(|| palette().transparent))
            })
            .when(self.full_width, |d| d.w_full())
            .children(lead_check)
            .when_some(icon, |d, which| d.child(glyph(which)))
            .child(label)
            // The selected check stays when disabled, dimmed with the label
            // (`glyph` paints `text_color`, `TEXT_DIM` when disabled).
            .when(selected && !reserve, |d| {
                d.child(glyph(Icon::Check).debug_selector(|| "ui-button-check".to_string()))
            });
        // Real pointer hover on every enabled face: the face group washes
        // to its hover fill and lifts the text it passes down; the frozen
        // `.hovered(bool)` state paints the same values (the gallery).
        // The face carries its own id (a named child of the part's id), so
        // it is stateful and GPUI tracks and paints its hover state.
        let mut face = face.id(child_id(&self.common.id, "face"));
        if !disabled {
            face = face
                .group(HOVER_GROUP)
                .hover(move |s| s.bg(hover_fill).text_color(hover_text));
        }

        let common = self.common;
        let on_click = self.on_click;
        let selector = common.selector_for(BUTTON);
        let focused = common.frozen_focus();
        let allow_double = common.allow_double_click;
        let tip = common.tooltip();

        let mut root = div()
            .when_some(self.bounds, |d, bounds| {
                d.on_children_prepainted(move |children, window, cx| {
                    let next = children.first().copied();
                    if bounds.replace(next) != next {
                        window.defer(cx, |window, _| window.refresh());
                    }
                })
            })
            .id(common.id.clone())
            .flex()
            .items_center()
            .min_w(theme::target())
            .debug_selector(|| selector.clone())
            .child(face);

        if !disabled {
            root = focus_wiring(root, &common)
                .when(!common.focused, |d| d.focus_visible(focus_ring_style));
            if let Some(on_click) = on_click {
                root = root.on_click(move |ev, window, cx| {
                    if allow_double || ev.click_count() <= 1 {
                        on_click(ev, window, cx);
                    }
                });
            }
        }
        if let Some(tip) = tip {
            root = root.tooltip(icons::tip(tip.to_string()));
        }

        // The ring box may shrink to the hit target in a narrow parent, so
        // a long label truncates instead of widening the part.
        focus_ring_r(root, focused, radius).min_w(theme::target())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// Disabled dims, never deletes: a disabled selected button keeps its
    /// check.
    #[gpui::test]
    fn disabled_selected_button_keeps_its_check(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, || {
            Button::new("b", "Grid")
                .selected(true)
                .disabled("Read only")
        });
        assert!(
            wcx.debug_bounds("ui-button-check").is_some(),
            "the check stays"
        );
    }

    /// A capsule (in-pill) button's radius is `RADIUS_PILL`, a plain one's
    /// `RADIUS_M`. The render draws both the face and the frozen ring's
    /// corners from this one value.
    #[test]
    fn in_pill_button_face_radius_is_the_capsule() {
        assert_eq!(
            Button::new("p", "Copy").in_pill().face_radius(),
            theme::RADIUS_PILL
        );
        assert_eq!(Button::new("b", "Copy").face_radius(), theme::RADIUS_M);
    }

    /// The hit box: at every tested zoom the button is at least
    /// `target()` (44 px) wide and tall — the clamp holds at 0.5.
    #[gpui::test]
    fn button_hit_box_is_at_least_target_at_every_zoom(cx: &mut TestAppContext) {
        for scale in [0.5_f32, 1.0, 1.25] {
            theme::set_ui_scale(scale);
            let (_root, wcx) = render_part(cx, move || {
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_8))
                    .child(Button::new("btn", "Send"))
                    .child(Button::new("ib", "Stop"))
                    .child(Button::new("q", "Open report").quiet())
                    .child(Button::new("l", "Review").lead())
            });
            for (sel, face) in [
                ("ui-button-btn", "the default ghost"),
                ("ui-button-ib", "the ghost"),
                ("ui-button-q", "the quiet"),
                ("ui-button-l", "the lead"),
            ] {
                let b = wcx
                    .debug_bounds(sel)
                    .unwrap_or_else(|| panic!("{face} recorded no bounds at zoom {scale}"));
                assert!(
                    f32::from(b.size.width) >= 44.0 && f32::from(b.size.height) >= 44.0,
                    "{face}: the hit box is ≥ 44×44 at zoom {scale}, got {b:?}",
                );
            }
        }
        theme::set_ui_scale(1.0);
    }
    /// The face table is the spec's: fill / edge / text per variant, with
    /// the hover fills and the disabled override (TEXT_DIM, no fill, no
    /// edge).
    #[test]
    fn button_face_table_matches_the_spec() {
        let p = palette();
        let ghost = Button::new("b", "x");
        assert_eq!(
            ghost.paint(),
            (p.fill_ghost, Some(p.control_line), p.text, false)
        );
        assert_eq!(
            ghost.hovered(true).paint(),
            (p.fill_hover, Some(p.control_line), p.text, false),
            "the ghost hover is the hover fill"
        );
        let primary = Button::new("b", "x").primary();
        let base = primary.paint();
        assert_eq!(base, (p.contrast, None, p.on_contrast, true));
        assert_eq!(
            primary.hovered(true).paint(),
            (p.contrast_hover, None, p.on_contrast, true),
            "the primary hover lightens to CONTRAST_HOVER (the mockup's #5592f7)"
        );
        let quiet = Button::new("b", "x").quiet();
        assert_eq!(quiet.paint(), (p.transparent, None, p.text_dim, false));
        assert_eq!(
            quiet.hovered(true).paint(),
            (p.fill_hover, None, p.text, false),
            "the quiet hover adds the hover fill and lifts the text"
        );
        let lead = Button::new("b", "x").lead();
        assert_eq!(lead.paint(), (p.contrast_soft, None, p.contrast_text, true));
        assert_eq!(
            lead.hovered(true).paint(),
            (p.contrast_soft_hover, None, p.contrast_text, true),
            "the lead hover steps to CONTRAST_SOFT_HOVER"
        );
        assert_eq!(
            Button::new("b", "x").disabled("The job is running").paint(),
            (p.transparent, None, p.text_dim, false),
            "the disabled override is TEXT_DIM with no fill and no edge"
        );
        assert_eq!(
            Button::new("b", "x").disabled("x").selected(true).edge(),
            None,
            "a disabled button shows no selected edge"
        );
    }

    /// The selected face: the `Check` icon plus the `CONTRAST` edge.
    #[test]
    fn selected_shows_a_check_and_a_contrast_edge() {
        let p = palette();
        let ghost = Button::new("b", "x").selected(true);
        assert_eq!(ghost.edge(), Some(p.contrast), "the edge turns CONTRAST");
        assert_eq!(
            Button::new("b", "x").primary().selected(true).edge(),
            Some(p.contrast),
            "selection wins the edge on every face"
        );
        assert!(ghost.is_selected());
    }

    /// The double-click guard: two quick clicks fire the handler once;
    /// `.allow_double_click()` fires it twice.
    #[gpui::test]
    fn button_double_click_fires_once(cx: &mut TestAppContext) {
        for (allow_double, expected) in [(false, 1usize), (true, 2)] {
            let count = std::rc::Rc::new(std::cell::Cell::new(0));
            let count_in = count.clone();
            // `debug_bounds` takes a `&'static str`.
            let (id, sel) = if allow_double {
                ("dbl-yes", "ui-button-dbl-yes")
            } else {
                ("dbl-no", "ui-button-dbl-no")
            };
            let (_root, wcx) = render_part(cx, move || {
                let count_in = count_in.clone();

                {
                    let mut b = Button::new(id, "Save").on_click(move |_ev, _w, _cx| {
                        count_in.set(count_in.get() + 1);
                    });
                    if allow_double {
                        b = b.allow_double_click();
                    }
                    div().flex().child(b)
                }
            });
            let at = wcx
                .debug_bounds(sel)
                .unwrap_or_else(|| panic!("the button recorded no bounds"))
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
                count.get(),
                expected,
                "allow_double_click={allow_double}: the handler fires {expected} time(s)"
            );
        }
    }
    /// Enter and Space activate the focused button through the same
    /// handler (GPUI's keyboard click); the disabled button is not
    /// clickable and keeps its reason for the parent.
    #[gpui::test]
    fn button_enter_and_space_activate_the_focused_button(cx: &mut TestAppContext) {
        // The focused button: the parent owns the focus handle (the app's
        // Tab navigation hands parts their handles the same way), and
        // Enter/Space activate it through GPUI's keyboard click — the same
        // `on_click` handler as the mouse.
        let handle = cx.update(|cx| cx.focus_handle());
        let fired = std::rc::Rc::new(std::cell::Cell::new(0));
        let fired_in = fired.clone();
        let h = handle.clone();
        let (_root, wcx) =
            render_part(cx, move || {
                let fired_in = fired_in.clone();
                div()
                    .flex()
                    .flex_col()
                    .child(Button::new("kbd", "Run").focus_handle(&h).on_click(
                        move |_ev, _w, _cx| {
                            fired_in.set(fired_in.get() + 1);
                        },
                    ))
                    .child(Button::new("off", "Stop").disabled("The job is running"))
            });
        wcx.update(|w, cx| w.focus(&handle, cx));
        assert!(
            wcx.update(|w, cx| w.focused(cx).is_some()),
            "the focused button is focused"
        );
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        assert_eq!(
            fired.get(),
            1,
            "Enter on the focused button fires the handler once"
        );
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("space").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("space").unwrap(),
        });
        assert_eq!(
            fired.get(),
            2,
            "Space on the focused button fires the handler again"
        );

        // A disabled button: no click fires, the label is still the
        // tooltip (the part always carries its accessible name), and the
        // reason is kept for the parent.
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        let count_in = count.clone();
        let (_root, wcx) = render_part(cx, move || {
            let count_in = count_in.clone();
            div().flex().child(
                Button::new("off2", "Stop")
                    .disabled("The job is running")
                    .on_click(move |_ev, _w, _cx| {
                        count_in.set(1);
                    }),
            )
        });
        let off = wcx
            .debug_bounds("ui-button-off2")
            .unwrap_or_else(|| panic!("the disabled button recorded no bounds"));
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: off.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: off.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert_eq!(count.get(), 0, "a disabled button is not clickable");
        let off = Button::new("o", "Stop").disabled("The job is running");
        assert_eq!(
            off.tooltip_text().as_deref(),
            Some("The job is running"),
            "the disabled reason wins the tooltip, ahead of the label"
        );
        assert_eq!(
            off.common().disabled_reason.as_deref(),
            Some("The job is running"),
            "the reason is kept for the parent"
        );
    }

    /// The selected edge is painted, not just returned by the helper: the
    /// selected primary face is wider than the same unselected one by
    /// exactly the 1 px `CONTRAST` border (both sides) plus the `Check`
    /// icon and its gap.
    #[gpui::test]
    fn selected_button_paints_the_contrast_edge(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .child(Button::new("uns", "OK").primary())
                .child(Button::new("sel", "OK").primary().selected(true))
        });
        let uns = wcx
            .debug_bounds("ui-button-uns")
            .unwrap_or_else(|| panic!("the unselected button recorded no bounds"));
        let sel = wcx
            .debug_bounds("ui-button-sel")
            .unwrap_or_else(|| panic!("the selected button recorded no bounds"));
        let delta = f32::from(sel.size.width) - f32::from(uns.size.width);
        // zoom 1: z() is the identity.
        let expected = f32::from(theme::z(theme::SP_8) + theme::z(theme::ICON)) + 2.0;
        assert_eq!(
            delta, expected,
            "the selected face carries the 1 px CONTRAST border (2 px total) \
             and the check icon plus its gap: uns {uns:?}, sel {sel:?}"
        );
        // …and the edge colour is the CONTRAST token (read back through
        // the helper the render uses).
        assert_eq!(
            Button::new("x", "OK").primary().selected(true).edge(),
            Some(palette().contrast),
        );
    }

    /// The reserved check keeps the width the same selected or not: the
    /// check's room is there unseen while unselected, and the edge is
    /// reserved on the ghost face either way. The check paints only while
    /// selected.
    #[gpui::test]
    fn reserved_check_keeps_the_width_selected_or_not(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .child(Button::new("rc-off", "Choose").reserve_selected_check(true))
                .child(
                    Button::new("rc-on", "Choose")
                        .reserve_selected_check(true)
                        .selected(true),
                )
        });
        let off = wcx
            .debug_bounds("ui-button-rc-off")
            .unwrap_or_else(|| panic!("the unselected reserved button recorded no bounds"));
        let on = wcx
            .debug_bounds("ui-button-rc-on")
            .unwrap_or_else(|| panic!("the selected reserved button recorded no bounds"));
        assert_eq!(
            f32::from(off.size.width),
            f32::from(on.size.width),
            "selecting a reserved button keeps its width: off {off:?}, on {on:?}"
        );
        assert_eq!(
            f32::from(off.size.height),
            f32::from(on.size.height),
            "and its height"
        );
        assert!(
            wcx.debug_bounds("ui-button-check").is_some(),
            "the selected check paints"
        );
    }

    /// Without the reservation the default is unchanged: an unselected
    /// button has no check, and a selected one paints its check.
    #[gpui::test]
    fn default_selected_check_is_unchanged_and_unreserved(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || Button::new("plain", "Choose"));
        assert!(
            wcx.debug_bounds("ui-button-check").is_none(),
            "an unselected default button has no check"
        );
        let (_root, wcx) =
            render_part(cx, move || Button::new("plain-on", "Choose").selected(true));
        assert!(
            wcx.debug_bounds("ui-button-check").is_some(),
            "a selected default button paints its check"
        );
    }

    /// Disabled dims, never deletes: a disabled button keeps its leading
    /// icon (painted `TEXT_DIM`), so it lays out exactly like its enabled
    /// twin.
    #[gpui::test]
    fn disabled_button_keeps_its_icon(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .child(Button::new("on", "Stop").quiet().icon(Icon::Square))
                .child(
                    Button::new("off", "Stop")
                        .quiet()
                        .icon(Icon::Square)
                        .disabled("The job is running"),
                )
        });
        let on = wcx
            .debug_bounds("ui-button-on")
            .unwrap_or_else(|| panic!("the enabled button recorded no bounds"));
        let off = wcx
            .debug_bounds("ui-button-off")
            .unwrap_or_else(|| panic!("the disabled button recorded no bounds"));
        assert_eq!(
            f32::from(on.size.width),
            f32::from(off.size.width),
            "the disabled button keeps its icon (dimmed, not deleted): \
             on {on:?}, off {off:?}"
        );
    }

    /// Real GPUI hover paints (K2c): the pointer on an enabled quiet face
    /// lifts the label GPUI paints from `TEXT_DIM` to `TEXT` (read back
    /// from the paint probe, not inferred from a re-render), and it drops
    /// back when the pointer leaves; a disabled face does not lift.
    #[gpui::test]
    fn button_hover_paints_the_hover_text(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::{hover, painted, same_color, unhover};
        let p = palette();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_16))
                .pt(theme::z(theme::SP_40))
                .child(Button::new("hov-on", "Hover me").quiet())
                .child(
                    Button::new("hov-off", "Hover me")
                        .quiet()
                        .disabled("The job is running"),
                )
        });
        let on = child_id(&ElementId::from("hov-on"), "label");
        let off = child_id(&ElementId::from("hov-off"), "label");
        unhover(wcx);
        let rest = painted(&on).expect("the enabled label painted");
        assert!(same_color(rest, p.text_dim), "rest is TEXT_DIM: {rest:?}");

        hover(wcx, "ui-button-hov-on");
        let lifted = painted(&on).expect("the enabled label painted");
        assert!(
            same_color(lifted, p.text),
            "hover lifts to TEXT: {lifted:?}"
        );

        unhover(wcx);
        let back = painted(&on).expect("the enabled label painted");
        assert!(same_color(back, p.text_dim), "leaving drops back: {back:?}");

        hover(wcx, "ui-button-hov-off");
        let dim = painted(&off).expect("the disabled label painted");
        assert!(
            same_color(dim, p.text_dim),
            "the disabled face does not lift: {dim:?}"
        );
    }

    /// Tab reaches a button tracking a plain parent handle — the part
    /// makes it a tab stop (K2c: the parent need not pre-mark it).
    #[gpui::test]
    fn tracked_handle_is_a_tab_stop(cx: &mut TestAppContext) {
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .child(Button::new("off", "Stop").disabled("The job is running"))
                .child(Button::new("on", "Run").focus_handle(&h))
        });
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            Some(handle),
            "Tab reaches the enabled button's tracked handle"
        );
    }

    /// The disabled reason is the tooltip that actually opens over the
    /// button (no child covers it).
    #[gpui::test]
    fn disabled_button_opens_its_reason(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::tooltip_at;
        let (_root, wcx) = render_part(cx, move || {
            div().flex().pt(theme::z(theme::SP_40)).child(
                Button::new("off", "Run")
                    .icon(Icon::ArrowUp)
                    .disabled("The job is running"),
            )
        });
        assert_eq!(
            tooltip_at(wcx, "ui-button-off").as_deref(),
            Some("The job is running")
        );
    }

    /// The painted size matches the tokens: exactly `target()` tall at
    /// zoom 1.
    #[gpui::test]
    fn button_painted_height_is_exactly_the_target(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || div().flex().child(Button::new("size", "OK")));
        let b = wcx
            .debug_bounds("ui-button-size")
            .unwrap_or_else(|| panic!("the button recorded no bounds"));
        assert_eq!(
            f32::from(b.size.height),
            f32::from(theme::target()),
            "the painted height is exactly the target: {b:?}",
        );
    }
}
