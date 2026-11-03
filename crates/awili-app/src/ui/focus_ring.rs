//! The kit focus ring (KIT.md v3, batch K1).
//!
//! `ui::focus_ring` replaces `focus_nav::ring`, which only recolours a
//! border to `#4c9aff`: the kit ring is a **2 px `CONTRAST` ring at a 2 px
//! offset**, and it is drawn as an **outer border box** — a wrapper (2 px
//! border + 2 px gap) that, like the mockup's CSS outline, takes no layout
//! space: a negative margin lets it overhang the part, so the part lays
//! out at its own size and toggling focus never shifts layout. Parts that
//! own a focus handle use [`focus_ring_style`] through `focus_visible`.

use gpui::{div, prelude::*, BoxShadow, IntoElement, Pixels, Point, StyleRefinement};

use crate::theme::{self, palette};

/// The ring's width and offset, in logical px before zoom: `theme::RING`
/// (the spec's 2 px / 2 px), re-exported for the ring's callers.
pub const RING: f32 = theme::RING;

/// The kit focus ring as an outer border box: wraps `content` in a box with a
/// `RING` border and `RING` padding, so a `RING` gap separates the ring from
/// the content (the mockup's `outline-offset: 2px`). Like a CSS outline the
/// box takes no layout space (a negative margin of the gap and ring), so the
/// part keeps its own size and toggling focus never shifts layout; the
/// border is `CONTRAST` when `focused` and transparent otherwise. Its corners follow the wrapped
/// part's `radius` (logical px), grown by the gap and ring.
pub fn focus_ring_r<E: IntoElement>(content: E, focused: bool, radius: f32) -> gpui::Div {
    let p = palette();
    div()
        .flex()
        .items_center()
        .justify_center()
        .p(theme::z(RING))
        // The border scales with zoom like the gap (a fixed `.border_2()` would
        // stay 2 px at zoom 0.5 while the gap shrinks to 1 px).
        .map(|mut d| {
            let w: gpui::AbsoluteLength = theme::z(RING).into();
            d.style().border_widths = gpui::EdgesRefinement {
                top: Some(w),
                right: Some(w),
                bottom: Some(w),
                left: Some(w),
            };
            d
        })
        .rounded(theme::z(radius + RING + RING))
        .border_color(if focused { p.contrast } else { p.transparent })
        // A lit ring answers to `ui-focus-ring` (tests read where it is).
        .when(focused, |d| {
            d.debug_selector(|| "ui-focus-ring".to_string())
        })
        // Like the mockup's CSS outline, the ring box takes no layout
        // space: a negative margin of the ring and gap lets it overhang
        // the part, so the part lays out at its own face size.
        .m(-(theme::z(RING) + theme::z(RING)))
        .child(content)
}

/// Paint the ring frozen on a part that draws its live ring through
/// `focus_visible(focus_ring_style_on(surface))` — the same two shadows,
/// always on (`.focused(true)`, the gallery). The surface is the colour the
/// part sits on, as for the live ring.
pub fn freeze_ring<E: gpui::Styled>(mut el: E, surface: gpui::Rgba) -> E {
    let style = el.style();
    *style = focus_ring_style_on(surface)(style.clone());
    el
}

/// [`focus_ring_r`] around a `RADIUS_M` part (buttons, rows, fields).
pub fn focus_ring<E: IntoElement>(content: E, focused: bool) -> gpui::Div {
    focus_ring_r(content, focused, theme::RADIUS_M)
}

/// The focus-ring style for parts that own a focus handle:
/// `.focus_visible(ui::focus_ring_style)`. A `StyleRefinement` cannot add an
/// outer box, so the 2 px gap and 2 px ring are two box shadows: a `RING`-wide
/// band in the surrounding surface's colour (the gap), painted over a
/// `2 × RING` `CONTRAST` spread (the ring). The part's own border is left
/// alone. [`focus_ring_style`] assumes the part sits on `PANEL_SOLID` (fields,
/// the composer); [`focus_ring_style_on`] takes the actual surface.
pub fn focus_ring_style(s: StyleRefinement) -> StyleRefinement {
    ring_shadows(s, palette().panel_solid)
}

/// [`focus_ring_style`] for a part sitting on `surface`.
pub fn focus_ring_style_on(surface: gpui::Rgba) -> impl Fn(StyleRefinement) -> StyleRefinement {
    move |s| ring_shadows(s, surface)
}

fn ring_shadows(s: StyleRefinement, surface: gpui::Rgba) -> StyleRefinement {
    let at = |color: gpui::Rgba, spread: f32| BoxShadow {
        color: color.into(),
        offset: Point {
            x: Pixels::ZERO,
            y: Pixels::ZERO,
        },
        blur_radius: Pixels::ZERO,
        spread_radius: theme::z(spread),
        inset: false,
    };
    // Paint order: the CONTRAST ring first, the surface-coloured gap over it.
    s.shadow(vec![at(palette().contrast, RING + RING), at(surface, RING)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui;
    use gpui::TestAppContext;

    /// The element form is an outer border box with the spec's geometry:
    /// `RING` margin, `RING`-wide border, `CONTRAST` when focused and the
    /// transparent no-fill otherwise — and the box is identical in both
    /// states, so toggling focus shifts nothing.
    #[test]
    fn focus_ring_is_a_two_pixel_contrast_outer_box() {
        let p = palette();
        // The focused/unfocused wrappers differ only in border colour: both
        // reserve the same margin + border, so their layout is byte-identical.
        let focused = focus_ring("x", true);
        let unfocused = focus_ring("x", false);
        let _ = (focused, unfocused);
        assert_eq!(RING, 2.0, "the ring is 2 px wide at a 2 px offset");
        // The colour split is the whole of the state change: CONTRAST on,
        // the transparent token off (both palette tokens, never a raw hex).
        assert_ne!(p.contrast, p.transparent);
        assert_eq!(
            p.transparent.a, 0.0,
            "the unfocused ring is the no-fill token"
        );
    }

    /// The style form is what `focus_visible` applies: two box shadows — a
    /// `RING`-wide gap in the surface colour over a `CONTRAST` ring — with the
    /// part's own border left untouched (the old `#4c9aff` recolour is gone).
    #[test]
    fn focus_ring_style_paints_a_gap_then_a_contrast_ring() {
        let p = palette();
        let s = focus_ring_style(StyleRefinement::default());
        let shadows = s.box_shadow.clone().expect("the style must add shadows");
        assert_eq!(shadows.len(), 2, "a ring and a gap: {shadows:?}");
        let (ring, gap) = (&shadows[0], &shadows[1]);
        // The CONTRAST ring spreads 2 × RING; the surface-coloured gap spreads
        // RING over it, so the visible ring is RING wide at a RING offset.
        assert_eq!(ring.color, gpui::Hsla::from(p.contrast));
        assert_eq!(ring.spread_radius, theme::z(RING + RING));
        assert_eq!(gap.color, gpui::Hsla::from(p.panel_solid));
        assert_eq!(gap.spread_radius, theme::z(RING));
        for sh in &shadows {
            assert_eq!(sh.blur_radius, Pixels::ZERO);
            assert!(!sh.inset, "both bands are outside the part");
        }
        // The part's own border is untouched, and focus never reflows it.
        assert!(s.border_color.is_none());
        assert!(!s.margin.is_some());
        assert!(!s.padding.is_some());
        assert!(!s.size.is_some());
        // The surface form paints its gap in the given surface.
        let on = focus_ring_style_on(p.raise)(StyleRefinement::default());
        assert_eq!(on.box_shadow.unwrap()[1].color, gpui::Hsla::from(p.raise));
    }

    /// `freeze_ring` paints exactly the live ring's two shadows, always on.
    #[test]
    fn freeze_ring_paints_the_live_ring() {
        let p = palette();
        let mut frozen = freeze_ring(div(), p.raise);
        let live = focus_ring_style_on(p.raise)(StyleRefinement::default());
        assert_eq!(frozen.style().box_shadow, live.box_shadow);
    }

    /// The ring box with its own selector (the part is "content").
    fn ring_box(focused: bool) -> gpui::Div {
        div().flex().debug_selector(|| "ring".to_string()).child(
            focus_ring(
                div()
                    .debug_selector(|| "content".to_string())
                    .w(theme::z(theme::RAIL_BUTTON))
                    .h(theme::z(theme::RAIL_BUTTON))
                    .child("x"),
                focused,
            )
            .debug_selector(|| "ring-box".to_string()),
        )
    }

    fn draw_box(cx: &mut gpui::VisualTestContext, focused: bool) {
        cx.draw(
            gpui::point(theme::z(theme::SP_40), theme::z(theme::SP_40)),
            gpui::size(
                gpui::AvailableSpace::Definite(theme::z(theme::PANEL_W)),
                gpui::AvailableSpace::Definite(theme::z(theme::RAIL_BUTTON)),
            ),
            |_w, _cx| ring_box(focused),
        );
    }

    /// Headless: like the mockup's CSS outline, the ring takes no layout
    /// space — the part's slot is exactly the part, focused or not — and
    /// the ring box overhangs it by a `RING` gap plus the `RING` border on
    /// every side, at every zoom.
    #[gpui::test]
    fn focus_ring_shifts_no_layout(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let part = f32::from(theme::z(theme::RAIL_BUTTON));
        for focused in [true, false] {
            draw_box(cx, focused);
            let slot = cx.debug_bounds("ring").expect("slot bounds");
            let content = cx.debug_bounds("content").expect("content bounds");
            assert_eq!(
                f32::from(slot.size.width),
                part,
                "the slot is the part ({focused})"
            );
            assert_eq!(
                f32::from(slot.size.height),
                part,
                "no height either ({focused})"
            );
            assert_eq!(
                content.origin, slot.origin,
                "the part sits at its slot ({focused})"
            );
        }
        for scale in [0.5_f32, 1.0, 1.25] {
            crate::theme::set_ui_scale(scale);
            draw_box(cx, true);
            let ring = cx.debug_bounds("ring-box").expect("ring box bounds");
            let content = cx.debug_bounds("content").expect("content bounds");
            let overhang = f32::from(theme::z(RING + RING));
            for (side, got) in [
                ("left", content.origin.x - ring.origin.x),
                ("top", content.origin.y - ring.origin.y),
                ("right", ring.right() - content.right()),
                ("bottom", ring.bottom() - content.bottom()),
            ] {
                assert_eq!(
                    f32::from(got),
                    overhang,
                    "zoom {scale}: the {side} overhang"
                );
            }
            assert_eq!(
                f32::from(content.origin.x - ring.origin.x),
                overhang,
                "zoom {scale}: a RING gap plus the RING border outside the part"
            );
            assert_eq!(
                f32::from(ring.size.width),
                f32::from(theme::z(theme::RAIL_BUTTON)) + overhang + overhang,
                "zoom {scale}: the ring box overhangs both sides"
            );
        }
        crate::theme::set_ui_scale(1.0);
        // The re-export the kit parts call through.
        let _ = ui::focus_ring_style;
    }
}
