//! `ui::IconButton` — the kit's icon-only button (KIT.md v3, batch K2).
//!
//! One `ICON` (or `ICON_L` on the primary face) wireframe glyph centred in
//! the 44 px hit box (`theme::target()`), the `Button` face table, an
//! optional `BADGE` dot at the top right, and a required `.label(…)` —
//! the accessible name, checked at paint in debug builds.
//!
//! The mockup's `.ibtn` is `border-radius: 50%` (a circle); the spec says
//! the hit box is a `target()` **square** by default with `.round()` for
//! the circle, so the kit default is the `RADIUS_M` square and every
//! mockup `.ibtn` maps to `.round()`.

use gpui::{
    div, prelude::*, App, BoxShadow, ClickEvent, ElementId, IntoElement, Pixels, Point, RenderOnce,
    Rgba, SharedString, Window,
};

use crate::icons::{self, Icon};
use crate::theme::{self, palette, Tone};
use crate::ui::button::{face_paint, hover_paint, Variant};
use crate::ui::focus_ring::{focus_ring, focus_ring_style};
use crate::ui::interaction::ICON_BUTTON;
use crate::ui::interaction::{child_id, focus_wiring, paint_probe, ClickHandler, HOVER_GROUP};

/// The badge's ring width, in logical px before zoom: a 2 px halo of the
/// panel-solid surface so the badge reads off the face it sits on (the
/// mockup's `.rbadge` ring is `#222120` = `PANEL_SOLID`).
const BADGE_RING: f32 = theme::BADGE_RING;

/// A kit icon-only button. Build with [`IconButton::new`] (a quiet face)
/// and always chain `.label(…)`.
#[derive(IntoElement)]
pub struct IconButton {
    common: crate::ui::interaction::Common,
    icon: Icon,
    variant: Variant,
    round: bool,
    badge: Option<Tone>,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    /// A `quiet` icon button (the default face) with the given icon.
    pub fn new(id: impl Into<ElementId>, icon: Icon) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            icon,
            variant: Variant::Quiet,
            round: false,
            badge: None,
            on_click: None,
        }
    }

    /// The `CONTRAST`-filled primary face (`ICON_L` glyph).
    pub fn primary(mut self) -> Self {
        self.variant = Variant::Primary;
        self
    }

    /// The default face: `FILL_GHOST` fill, 1 px `CONTROL_LINE` edge.
    pub fn ghost(mut self) -> Self {
        self.variant = Variant::Ghost;
        self
    }

    /// The quiet face: no fill, `TEXT_DIM` glyph, hover `FILL_HOVER`.
    pub fn quiet(mut self) -> Self {
        self.variant = Variant::Quiet;
        self
    }

    /// The lead face: `CONTRAST_SOFT` fill, `CONTRAST_TEXT` glyph.
    pub fn lead(mut self) -> Self {
        self.variant = Variant::Lead;
        self
    }

    /// A circular face (`.round()`); the default is the `RADIUS_M` square.
    pub fn round(mut self) -> Self {
        self.round = true;
        self
    }

    /// The icon the button paints.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = icon;
        self
    }

    /// A `BADGE`-sized status dot at the top right, ringed by the
    /// `PANEL_SOLID` surface.
    pub fn badge(mut self, tone: Tone) -> Self {
        self.badge = Some(tone);
        self
    }

    /// The current face.
    pub fn variant(&self) -> Variant {
        self.variant
    }

    /// Whether the face is the circle.
    pub fn is_round(&self) -> bool {
        self.round
    }

    /// The badge tone, if any.
    pub fn badge_tone(&self) -> Option<Tone> {
        self.badge
    }

    /// The click handler; the kit's double-click guard applies. Enter/Space
    /// on the focused button fire it too.
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    /// The glyph size: `ICON_L` on the primary face, `ICON` otherwise.
    pub fn icon_size(&self) -> f32 {
        if self.variant == Variant::Primary {
            theme::ICON_L
        } else {
            theme::ICON
        }
    }

    /// The face's paint: the shared button face table.
    pub fn paint(&self) -> (Rgba, Option<Rgba>, Rgba, bool) {
        face_paint(self.variant, self.common.is_disabled(), self.common.hovered)
    }
}

crate::kit_shared!(IconButton);

impl std::fmt::Debug for IconButton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IconButton({})", self.common.selector_for(ICON_BUTTON))
    }
}

impl RenderOnce for IconButton {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // The API forces the rule: an icon-only part must carry its
        // accessible name (checked in debug builds at paint).
        debug_assert!(
            self.common.label.is_some(),
            "IconButton requires .label(…): the accessible name of an icon-only part"
        );

        let (fill, edge, text_color, _strong) = self.paint();
        let disabled = self.common.is_disabled();
        let (hover_fill, hover_text) = hover_paint(self.variant);
        let face_id = child_id(&self.common.id, "face");
        let glyph_selector = format!("{}-glyph", self.common.selector_for(ICON_BUTTON));

        let icon = self.icon;
        let badge = self.badge;
        // The glyph is always painted: disabled dims it (`text_color` is
        // `TEXT_DIM` from the face table when disabled) but never deletes
        // it.
        let face = div()
            .flex()
            .items_center()
            .justify_center()
            .w(theme::target())
            .h(theme::target())
            .rounded(theme::z(if self.round {
                theme::RADIUS_PILL
            } else {
                theme::RADIUS_M
            }))
            .bg(fill)
            .when(!disabled && edge.is_some(), |d| {
                d.border_1()
                    .border_color(edge.unwrap_or_else(|| palette().transparent))
            })
            .relative()
            .text_color(text_color)
            .child({
                // An svg paints its own colour (it does not inherit), so
                // the glyph takes the face group's hover colour itself.
                let svg = icons::icon(icon, self.icon_size(), text_color)
                    .debug_selector(|| glyph_selector);
                if disabled {
                    svg
                } else {
                    svg.group_hover(HOVER_GROUP, move |s| s.text_color(hover_text))
                }
            })
            .children(paint_probe(&face_id))
            .when(badge.is_some(), |d| {
                // A layout-neutral top-right dot with its 2 px surface ring,
                // painted as an outer box shadow so it never shifts the hit
                // box. Disabled dims it to `TEXT_DIM`; it never deletes.
                d.child(
                    div()
                        .flex()
                        .absolute()
                        .top(Pixels::ZERO)
                        .right(Pixels::ZERO)
                        .size(theme::z(theme::BADGE))
                        .rounded(theme::z(theme::RADIUS_PILL))
                        .debug_selector(|| "ui-badge".to_string())
                        .bg(if disabled {
                            palette().text_dim
                        } else {
                            theme::tone_color(badge.unwrap())
                        })
                        .shadow(vec![BoxShadow {
                            color: palette().panel_solid.into(),
                            offset: Point {
                                x: Pixels::ZERO,
                                y: Pixels::ZERO,
                            },
                            blur_radius: Pixels::ZERO,
                            spread_radius: theme::z(BADGE_RING),
                            inset: false,
                        }]),
                )
            });

        // Real pointer hover on every enabled face: the face is the hover
        // group — it washes to its hover fill and lifts the glyph (above);
        // the frozen `.hovered(bool)` state paints the same values. The
        // face carries its own id (a named child of the part's id), so it
        // is stateful and GPUI tracks its hover state.
        let common = self.common;
        let on_click = self.on_click;
        let selector = common.selector_for(ICON_BUTTON);
        let focused = common.frozen_focus();
        let allow_double = common.allow_double_click;
        let tip = common.tooltip();

        let mut root = div()
            .id(common.id.clone())
            .flex()
            .items_center()
            .justify_center()
            .debug_selector(|| selector.clone())
            .child(if !disabled {
                face.id(face_id)
                    .group(HOVER_GROUP)
                    .hover(move |s| s.bg(hover_fill).text_color(hover_text))
            } else {
                face.id(face_id)
            });

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

        focus_ring(root, focused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// Disabled dims, never deletes: a disabled icon button keeps its badge.
    #[gpui::test]
    fn disabled_icon_button_keeps_its_badge(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, || {
            IconButton::new("i", Icon::Gear)
                .label("Settings")
                .badge(Tone::Attention)
                .disabled("Sign in first")
        });
        assert!(wcx.debug_bounds("ui-badge").is_some(), "the badge stays");
    }

    /// The hit box: square and round faces are at least 44×44 at every
    /// tested zoom.
    #[gpui::test]
    fn icon_button_hit_box_is_at_least_target_at_every_zoom(cx: &mut TestAppContext) {
        for scale in [0.5_f32, 1.0, 1.25, 2.5] {
            theme::set_ui_scale(scale);
            let (_root, wcx) = render_part(cx, move || {
                div()
                    .flex()
                    .gap(theme::z(theme::SP_8))
                    .child(IconButton::new("sq", Icon::Plus).label("New").round())
                    .child(IconButton::new("sq2", Icon::Plus).label("New"))
                    .child(IconButton::new("pr", Icon::ArrowUp).label("Send").primary())
            });
            for sel in [
                "ui-icon-button-sq",
                "ui-icon-button-sq2",
                "ui-icon-button-pr",
            ] {
                let b = wcx
                    .debug_bounds(sel)
                    .unwrap_or_else(|| panic!("{sel} recorded no bounds at zoom {scale}"));
                assert!(
                    f32::from(b.size.width) >= 44.0 && f32::from(b.size.height) >= 44.0,
                    "{sel}: the hit box is ≥ 44×44 at zoom {scale}, got {b:?}",
                );
            }
            for (sel, logical_size) in [
                ("ui-icon-button-sq-glyph", theme::ICON),
                ("ui-icon-button-sq2-glyph", theme::ICON),
                ("ui-icon-button-pr-glyph", theme::ICON_L),
            ] {
                let glyph = wcx.debug_bounds(sel).expect("the actual SVG glyph");
                let expected = theme::z(logical_size);
                assert_eq!(
                    glyph.size.width, expected,
                    "{sel}: glyph scales once at {scale}"
                );
                assert_eq!(
                    glyph.size.height, expected,
                    "{sel}: glyph scales once at {scale}"
                );
            }
        }
        theme::set_ui_scale(1.0);
    }
    /// The face table is the shared Button table, and the glyph size is
    /// `ICON_L` on the primary face and `ICON` elsewhere.
    #[test]
    fn icon_button_face_table_and_icon_sizes() {
        let p = palette();
        let quiet = IconButton::new("b", Icon::Plus).label("x");
        assert_eq!(quiet.paint(), (p.transparent, None, p.text_dim, false));
        assert_eq!(
            quiet.hovered(true).paint(),
            (p.fill_hover, None, p.text, false)
        );
        assert_eq!(
            IconButton::new("b", Icon::Plus)
                .label("x")
                .primary()
                .paint(),
            (p.contrast, None, p.on_contrast, true)
        );
        assert_eq!(
            IconButton::new("b", Icon::Plus).label("x").icon_size(),
            theme::ICON
        );
        assert_eq!(
            IconButton::new("b", Icon::Plus)
                .label("x")
                .primary()
                .icon_size(),
            theme::ICON_L
        );
        assert!(IconButton::new("b", Icon::Plus)
            .label("x")
            .round()
            .is_round());
    }

    /// The API forces the rule: an `IconButton` without `.label` fails a
    /// debug assert at paint.
    #[gpui::test]
    fn icon_button_without_a_label_fails_a_debug_assert(cx: &mut TestAppContext) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (_root, _wcx) = render_part(cx, move || {
                div().flex().child(IconButton::new("nolabel", Icon::Plus))
            });
        }));
        // A `debug_assert`: debug builds panic, release builds still paint.
        assert_eq!(
            result.is_err(),
            cfg!(debug_assertions),
            "an IconButton without .label panics in debug builds only"
        );
        // And with a label the same paint is fine.
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .child(IconButton::new("labeled", Icon::Plus).label("New"))
        });
        assert!(wcx.debug_bounds("ui-icon-button-labeled").is_some());
    }
    /// A disabled icon button is inert: the click fires nothing and the
    /// reason is the tooltip.
    #[gpui::test]
    fn icon_button_disabled_is_inert(cx: &mut TestAppContext) {
        let fired = std::rc::Rc::new(std::cell::Cell::new(0));
        let fired_in = fired.clone();
        let (_root, wcx) = render_part(cx, move || {
            let fired_in = fired_in.clone();

            div().flex().child(
                IconButton::new("off", Icon::Plus)
                    .label("New")
                    .disabled("Nothing to add")
                    .on_click(move |_ev, _w, _cx| {
                        fired_in.set(1);
                    }),
            )
        });
        let at = wcx
            .debug_bounds("ui-icon-button-off")
            .unwrap_or_else(|| panic!("the button recorded no bounds"))
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
        assert_eq!(fired.get(), 0, "a disabled icon button is not clickable");
        assert_eq!(
            IconButton::new("o", Icon::Plus)
                .label("New")
                .disabled("Nothing to add")
                .tooltip_text()
                .as_deref(),
            Some("Nothing to add"),
            "the disabled reason wins the tooltip, ahead of the label"
        );
        assert_eq!(
            IconButton::new("o2", Icon::Plus)
                .label("New")
                .disabled("Nothing to add")
                .common()
                .disabled_reason
                .as_deref(),
            Some("Nothing to add"),
            "the reason is kept for the parent"
        );
    }

    /// Disabled dims, never deletes: the disabled button lays out its
    /// glyph (the rendered svg has bounds), tinted by the face table's
    /// `TEXT_DIM`.
    #[gpui::test]
    fn disabled_glyph_is_painted_not_deleted(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(
                IconButton::new("d", Icon::Plus)
                    .label("New")
                    .disabled("Nothing to add"),
            )
        });
        let glyph = wcx
            .debug_bounds("ui-icon-button-d-glyph")
            .unwrap_or_else(|| panic!("the disabled glyph was deleted"));
        assert!(
            f32::from(glyph.size.width) > 0.0,
            "the glyph has size: {glyph:?}"
        );
        let p = palette();
        assert_eq!(
            IconButton::new("d", Icon::Plus)
                .label("New")
                .disabled("off")
                .paint(),
            (p.transparent, None, p.text_dim, false),
            "the disabled tint is TEXT_DIM"
        );
    }

    /// Real GPUI hover paints (K2c): on an enabled quiet face the face
    /// group's hover applies — the text colour GPUI resolves inside the
    /// face lifts `TEXT_DIM` → `TEXT` (read back from the paint probe) —
    /// and drops back on leave; a disabled face does not lift.
    #[gpui::test]
    fn icon_button_hover_paints(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::{hover, painted, same_color, unhover};
        let p = palette();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .gap(theme::z(theme::SP_16))
                .pt(theme::z(theme::SP_40))
                .pl(theme::z(theme::SP_40))
                .child(IconButton::new("on", Icon::Plus).label("New"))
                .child(
                    IconButton::new("off", Icon::Plus)
                        .label("New")
                        .disabled("Nothing to add"),
                )
        });
        let on = child_id(&ElementId::from("on"), "face");
        let off = child_id(&ElementId::from("off"), "face");
        unhover(wcx);
        assert!(
            same_color(painted(&on).unwrap(), p.text_dim),
            "rest is TEXT_DIM"
        );
        hover(wcx, "ui-icon-button-on");
        assert!(
            same_color(painted(&on).unwrap(), p.text),
            "hover lifts to TEXT"
        );
        unhover(wcx);
        assert!(
            same_color(painted(&on).unwrap(), p.text_dim),
            "leave drops back"
        );
        hover(wcx, "ui-icon-button-off");
        assert!(
            same_color(painted(&off).unwrap(), p.text_dim),
            "the disabled face does not lift"
        );
    }

    /// Tab reaches an icon button tracking a plain parent handle (K2c).
    #[gpui::test]
    fn tracked_handle_is_a_tab_stop(cx: &mut TestAppContext) {
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(
                IconButton::new("i", Icon::Plus)
                    .label("New")
                    .focus_handle(&h),
            )
        });
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert_eq!(wcx.update(|w, cx| w.focused(cx)), Some(handle));
    }
}
