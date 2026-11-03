//! `ui::Section` — the kit's collapsible block (KIT.md v3, batch K2).
//!
//! A header line — a chevron (closed `ChevronRight`, open `ChevronDown`,
//! which is the closed glyph rotated 90° — GPUI's style has no
//! transform), a strong `type_::UI` title, a 48 px minimum height — over
//! the body (its children) with 12 px padding, shown when open. The
//! header is the interactive part: `.on_toggle(…)` on the 48 px hit box,
//! the shared conventions, Enter/Space. The 48 px header height is the
//! mockup's summary-header size; the kit's `RAIL_BUTTON` token covers it
//! and the `target()` clamp keeps it a 44 px hit box at every zoom.

use gpui::{
    div, prelude::*, App, ClickEvent, ElementId, IntoElement, Pixels, RenderOnce, Rgba,
    SharedString, Window,
};

use crate::icons::{self, Icon};
use crate::theme::{self, palette};
use crate::ui::focus_ring::{focus_ring, focus_ring_style};
use crate::ui::interaction::SECTION;
use crate::ui::interaction::{focus_wiring, ClickHandler};
use crate::ui::text;

/// A kit section. Build with [`Section::new`] and chain `.open(…)`,
/// `.nerd()`, `.body(…)` and the shared conventions; the parent owns the
/// open state (re-rendered through `.open`) and the toggle handler.
#[derive(IntoElement)]
pub struct Section {
    common: crate::ui::interaction::Common,
    title: SharedString,
    open: bool,
    nerd: bool,
    on_toggle: Option<ClickHandler>,
    body: Vec<gpui::AnyElement>,
}

impl Section {
    /// A section with the given title (closed, no body).
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        let title = title.into();
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                label: Some(title.clone()),
                ..Default::default()
            },
            title,
            open: false,
            nerd: false,
            on_toggle: None,
            body: Vec::new(),
        }
    }

    /// Whether the body is shown.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Whether the section is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Mark the section as the nerd section (the parent paints its own
    /// marker from [`Section::is_nerd`]).
    pub fn nerd(mut self) -> Self {
        self.nerd = true;
        self
    }

    /// Whether this is the nerd section.
    pub fn is_nerd(&self) -> bool {
        self.nerd
    }

    /// The body children (shown while open).
    pub fn body(mut self, children: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.body = children
            .into_iter()
            .map(IntoElement::into_any_element)
            .collect();
        self
    }

    /// The number of body children.
    pub fn body_len(&self) -> usize {
        self.body.len()
    }

    /// The toggle handler (the header click, Enter/Space); the kit's
    /// double-click guard applies.
    pub fn on_toggle(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Box::new(f));
        self
    }

    /// The header's minimum height: the mockup's 48 px summary header, as
    /// `max(RAIL_BUTTON, target())` so it is a 44 px hit box at every zoom.
    pub fn header_height() -> Pixels {
        theme::z(theme::RAIL_BUTTON).max(theme::target())
    }

    /// The chevron the header paints: closed `ChevronRight`, open
    /// `ChevronDown` (the same glyph, rotated 90°).
    pub fn chevron(&self) -> Icon {
        if self.open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        }
    }

    /// The header title's colour: `TEXT`, or `TEXT_DIM` when the section
    /// is disabled (the disabled dims, it never deletes).
    pub fn title_color(&self) -> Rgba {
        let p = palette();
        if self.common.is_disabled() {
            p.text_dim
        } else {
            p.text
        }
    }
}

crate::kit_shared!(Section);

impl std::fmt::Debug for Section {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Section({})", self.common.selector_for(SECTION))
    }
}

impl RenderOnce for Section {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let p = palette();
        let disabled = self.common.is_disabled();
        let chevron = self.chevron();
        // The title colour is read from the common state (disabled dims
        // it to `TEXT_DIM`), before the common state moves.
        let title_color = self.title_color();
        let title = self.title;
        let common = self.common;
        let on_toggle = self.on_toggle;
        let body = self.body;
        let selector = common.selector_for(SECTION);
        let focused = common.frozen_focus();
        let allow_double = common.allow_double_click;
        let tip = common.tooltip();
        let reason = common.disabled_reason.clone();

        let mut header = div()
            .id(common.id.clone())
            .flex()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .px(theme::z(theme::SP_12))
            .min_h(Self::header_height())
            .min_w(theme::target())
            .rounded(theme::z(theme::RADIUS_M))
            // The mockup's `summary`: the title, then the chevron at the
            // right (it turns when open).
            .justify_between()
            .child(
                // The title dims when the section is disabled (it never
                // deletes).
                text::ui_text(title)
                    .strong()
                    .text_color_override(title_color),
            )
            .child(icons::icon(chevron, theme::ICON_S, p.text_dim))
            .when(disabled, |d| {
                d.child(text::caption(
                    reason
                        .clone()
                        .map(|r| r.to_string())
                        .unwrap_or_else(|| "disabled".into()),
                ))
            });

        // The header is interactive only when it can do something: with a
        // toggle handler (KIT.md § Section) — and only then is it a tab
        // stop (a disabled header, or one without a toggle, is not).
        if !disabled {
            if let Some(on_toggle) = on_toggle {
                header = focus_wiring(header, &common)
                    .when(!common.focused, |d| d.focus_visible(focus_ring_style));
                header = header.on_click(move |ev, window, cx| {
                    if allow_double || ev.click_count() <= 1 {
                        on_toggle(ev, window, cx);
                    }
                });
            }
        }
        if let Some(tip) = tip {
            header = header.tooltip(icons::tip(tip.to_string()));
        }
        let header = focus_ring(header, focused);

        // The mockup's `details`: a quiet `HIGHLIGHT` fill on `RADIUS_M`;
        // the open body sits below the header with the mockup's
        // `0 14px 14px` padding (snapped to `SP_12`).
        let mut root = div()
            .flex()
            .flex_col()
            .rounded(theme::z(theme::RADIUS_M))
            .bg(p.highlight)
            .debug_selector(|| selector.clone())
            .child(header);
        if self.open && !body.is_empty() {
            let mut body_wrap = div()
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_8))
                .px(theme::z(theme::SP_12))
                .pb(theme::z(theme::SP_12));
            for child in body {
                body_wrap = body_wrap.child(child);
            }
            root = root.child(body_wrap);
        }

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// The header hit box: at least 44 px tall (the 48 px token clamped
    /// by `target()`) at every tested zoom.
    #[gpui::test]
    fn section_header_hit_box_is_at_least_target_at_every_zoom(cx: &mut TestAppContext) {
        for scale in [0.5_f32, 1.0, 1.25] {
            theme::set_ui_scale(scale);
            let (_root, wcx) =
                render_part(cx, move || div().flex().child(Section::new("s", "Model")));
            let b = wcx
                .debug_bounds("ui-section-s")
                .unwrap_or_else(|| panic!("the header recorded no bounds at zoom {scale}"));
            assert!(
                f32::from(b.size.height) >= 44.0,
                "the header is ≥ 44 px tall at zoom {scale}, got {b:?}",
            );
            assert!(
                f32::from(Section::header_height()) >= 44.0,
                "header_height() is ≥ 44 at zoom {scale}"
            );
        }
        theme::set_ui_scale(1.0);
        assert_eq!(
            f32::from(Section::header_height()),
            f32::from(theme::z(theme::RAIL_BUTTON))
        );
        assert_eq!(
            Section::new("s", "t").chevron(),
            Icon::ChevronRight,
            "closed shows the right chevron"
        );
        assert_eq!(
            Section::new("s", "t").open(true).chevron(),
            Icon::ChevronDown,
            "open shows the down chevron (the 90° rotation)"
        );
        assert!(Section::new("s", "t").nerd().is_nerd());
    }
    /// Toggling: a header click fires `on_toggle` once (the double-click
    /// guard), Enter/Space fire it too, and a disabled header fires
    /// nothing.
    #[gpui::test]
    fn section_toggle_fires_once(cx: &mut TestAppContext) {
        let fired = std::rc::Rc::new(std::cell::Cell::new(0));
        let fired_in = fired.clone();
        let dead = std::rc::Rc::new(std::cell::Cell::new(0));
        let dead_in = dead.clone();
        let (_root, wcx) = render_part(cx, move || {
            let fired_in = fired_in.clone();
            let dead_in = dead_in.clone();

            div()
                .flex()
                .flex_col()
                .child(Section::new("tog", "Model").on_toggle(move |_ev, _w, _cx| {
                    fired_in.set(fired_in.get() + 1);
                }))
                .child(Section::new("dead", "Pinned").disabled("Locked").on_toggle(
                    move |_ev, _w, _cx| {
                        dead_in.set(1);
                    },
                ))
        });
        let at = wcx
            .debug_bounds("ui-section-tog")
            .unwrap_or_else(|| panic!("the header recorded no bounds"))
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
        assert_eq!(fired.get(), 1, "the double click toggles once");

        wcx.update(|w, cx| w.focus_next(cx));
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        assert_eq!(fired.get(), 2, "Enter on the focused header toggles");

        let dead_at = wcx
            .debug_bounds("ui-section-dead")
            .unwrap_or_else(|| panic!("the disabled header recorded no bounds"))
            .center();
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: dead_at,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: dead_at,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert_eq!(dead.get(), 0, "a disabled header is not clickable");
    }
    /// Tab navigation skips the disabled header (a disabled part is not a
    /// tab stop) and reaches the enabled one.
    #[gpui::test]
    fn disabled_header_is_not_a_tab_stop(cx: &mut TestAppContext) {
        let h_off = cx.update(|cx| cx.focus_handle());
        let h_on = cx.update(|cx| cx.focus_handle());
        let h_off_in = h_off.clone();
        let h_on_in = h_on.clone();
        let (_root, wcx) = render_part(cx, move || {
            let h_off_in = h_off_in.clone();
            let h_on_in = h_on_in.clone();
            div()
                .flex()
                .flex_col()
                .child(
                    Section::new("off", "Off")
                        .disabled("Locked")
                        .focus_handle(&h_off_in)
                        .on_toggle(|_e, _w, _cx| {}),
                )
                .child(
                    Section::new("on", "On")
                        .focus_handle(&h_on_in)
                        .on_toggle(|_e, _w, _cx| {}),
                )
        });
        wcx.run_until_parked();
        for _ in 0..2 {
            wcx.update(|w, cx| w.focus_next(cx));
            wcx.run_until_parked();
        }
        let focused = wcx
            .update(|w, cx| w.focused(cx))
            .expect("the enabled header is focused");
        assert!(
            focused == h_on,
            "Tab lands on the enabled header, not the disabled one"
        );
    }

    /// The disabled section dims its title (`TEXT_DIM`); the enabled one
    /// paints `TEXT`.
    #[test]
    fn section_disabled_title_is_dim() {
        let p = palette();
        assert_eq!(
            Section::new("s", "Model").title_color(),
            p.text,
            "the enabled title is TEXT"
        );
        assert_eq!(
            Section::new("s", "Model").disabled("Locked").title_color(),
            p.text_dim,
            "the disabled title dims to TEXT_DIM (it never deletes)"
        );
    }

    /// The body renders only while open, and `.body(…)` stores the
    /// children.
    #[gpui::test]
    fn section_body_renders_only_while_open(cx: &mut TestAppContext) {
        let body = text::body("the body line");
        let s = Section::new("b", "Model").body([body]);
        assert_eq!(s.body_len(), 1);
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .child(Section::new("closed", "Model").body([text::body("hidden")]))
                .child(
                    Section::new("open", "Model")
                        .open(true)
                        .body([text::body("shown")]),
                )
        });
        let closed = wcx
            .debug_bounds("ui-section-closed")
            .unwrap_or_else(|| panic!("no bounds"));
        let open = wcx
            .debug_bounds("ui-section-open")
            .unwrap_or_else(|| panic!("no bounds"));
        assert!(
            f32::from(open.size.height) > f32::from(closed.size.height),
            "the open section is taller than the closed one: open {open:?}, closed {closed:?}",
        );
    }

    /// The hit box holds in a squeezed parent: the header never lays out
    /// narrower than `target()` (K2c).
    #[gpui::test]
    fn section_header_min_width_is_the_target(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .w(gpui::px(10.))
                .child(Section::new("n", "Narrow").on_toggle(|_e, _w, _cx| {}))
        });
        let b = wcx.debug_bounds("ui-section-n").unwrap();
        assert!(f32::from(b.size.width) >= 44.0, "{b:?}");
    }
}
