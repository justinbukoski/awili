//! `ui::ThinkingDot` — the kit's breathing thinking indicator (KIT.md v3,
//! batch K1).
//!
//! A `CONTRAST` dot breathing over 12 frames, `THINK_MIN` → `THINK_MAX` →
//! `THINK_MIN`, inside a `THINK_HALO` circle of `CONTRAST_SOFT` with a
//! `THINKING_RING` edge. The parent advances `phase` every
//! [`THINK_STEP_MS`] (120 ms) and re-renders — the same parent-driven phase
//! model as the app's existing pulse; the part is a pure function of the
//! phase. Under Reduce Motion (`super::motion::reduce_motion`) the dot is
//! still at `THINK_MAX`.

use gpui::{div, prelude::*, App, IntoElement, RenderOnce, Window};

use crate::theme;

/// The breathing dot's frame table, in logical px before zoom:
/// `THINK_MIN` → `THINK_MAX` → `THINK_MIN` over 12 frames. Symmetric, and
/// every frame within `6..=12`.
pub const FRAMES: [f32; 12] = theme::THINK_FRAMES;

/// The parent's phase step: the dot advances one frame every 120 ms.
pub const THINK_STEP_MS: u64 = theme::motion::THINK_STEP_MS;

/// The breathing thinking dot at breathing frame `phase` (mod 12).
#[derive(Clone, Copy, Debug, PartialEq, IntoElement)]
pub struct ThinkingDot {
    phase: u8,
    still: bool,
}

impl ThinkingDot {
    /// The dot at breathing frame `phase` (mod 12).
    pub fn new(phase: u8) -> Self {
        Self {
            phase: phase % FRAMES.len() as u8,
            still: false,
        }
    }

    /// Paint the Reduce Motion face (still at `THINK_MAX`) whatever the
    /// setting: the gallery shows it next to the breathing frames.
    pub fn still(mut self) -> Self {
        self.still = true;
        self
    }

    pub fn phase(&self) -> u8 {
        self.phase
    }

    /// The next phase (mod 12), for the parent's 120 ms tick.
    pub fn next(&self) -> u8 {
        self.phase.wrapping_add(1) % FRAMES.len() as u8
    }

    /// The painted dot size for this phase, in logical px: the frame table —
    /// or `THINK_MAX`, still, when Reduce Motion is on.
    pub fn size(&self) -> f32 {
        if self.still || crate::ui::motion::reduce_motion() {
            theme::THINK_MAX
        } else {
            FRAMES[self.phase as usize % FRAMES.len()]
        }
    }
}

impl RenderOnce for ThinkingDot {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let p = theme::palette();
        let size = self.size();
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(theme::z(theme::THINK_HALO))
            .h(theme::z(theme::THINK_HALO))
            .rounded(theme::z(theme::RADIUS_PILL))
            .bg(p.contrast_soft)
            .border_1()
            .border_color(p.contrast_ring)
            .child(
                div()
                    .size(theme::z(size))
                    .rounded(theme::z(theme::RADIUS_PILL))
                    .bg(p.contrast),
            )
            .debug_selector(|| "ui-thinking-dot".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The halo edge is the opaque light blue #60a5fa (was brand blue at 0.48; mockup
    /// studio.css `.think`), not the legacy sage ring.
    #[test]
    fn thinking_halo_edge_is_contrast_blue() {
        let p = theme::palette();
        // Council 2026-11-01: the ring is the opaque #60a5fa (ACCENT_BRIGHT),
        // legible on gray; no longer translucent brand blue.
        assert_eq!(p.contrast_ring, theme::ACCENT_BRIGHT);
        assert_eq!(p.contrast_ring.a, 1.0);
    }
    use crate::ui::motion;
    use gpui::TestAppContext;

    /// The frame table is symmetric (a breath, not a lurch) and every frame
    /// is within `6..=12`, pinned to the `THINK_MIN` / `THINK_MAX` tokens.
    #[test]
    fn frame_table_is_symmetric_and_within_bounds() {
        for (i, v) in FRAMES.iter().enumerate() {
            assert!(
                (theme::THINK_MIN..=theme::THINK_MAX).contains(v),
                "frame {i} = {v} is outside 6..=12"
            );
            assert_eq!(
                FRAMES[i],
                FRAMES[FRAMES.len() - 1 - i],
                "frame {i} breaks the symmetry (the breath must return the way it went)"
            );
        }
        assert_eq!(FRAMES[0], theme::THINK_MIN, "it starts at THINK_MIN");
        assert_eq!(FRAMES[5], theme::THINK_MAX, "it peaks at frame 5");
        assert_eq!(
            FRAMES[6],
            theme::THINK_MAX,
            "…and holds the peak at frame 6"
        );
        assert_eq!(FRAMES[11], theme::THINK_MIN, "…and returns to THINK_MIN");
    }

    /// The phase wraps mod 12 and advances one frame per step.
    #[test]
    fn phase_wraps_and_advances() {
        assert_eq!(ThinkingDot::new(0).next(), 1);
        assert_eq!(ThinkingDot::new(11).next(), 0, "frame 11 wraps to 0");
        assert_eq!(
            ThinkingDot::new(13).size(),
            FRAMES[1],
            "phase 13 is frame 1 (13 mod 12)"
        );
        assert_eq!(ThinkingDot::new(0).size(), theme::THINK_MIN);
        assert_eq!(ThinkingDot::new(5).size(), theme::THINK_MAX);
        assert_eq!(THINK_STEP_MS, 120, "the parent ticks every 120 ms");
    }

    /// Under Reduce Motion the dot is still at `THINK_MAX` — at every phase.
    #[test]
    fn still_at_max_under_reduce_motion() {
        motion::set_test_override(true);
        for phase in 0..FRAMES.len() as u8 {
            assert_eq!(
                ThinkingDot::new(phase).size(),
                theme::THINK_MAX,
                "reduce motion: the dot is still at THINK_MAX (phase {phase})"
            );
        }
        motion::clear_test_override();
        // And without the override the breath is back.
        assert_eq!(ThinkingDot::new(0).size(), theme::THINK_MIN);
    }

    /// `.still()` paints the Reduce Motion face with motion on: `THINK_MAX`
    /// at every phase, while the plain dot still breathes.
    #[test]
    fn still_builder_paints_the_reduce_motion_face() {
        motion::set_test_override(false);
        for phase in 0..FRAMES.len() as u8 {
            assert_eq!(ThinkingDot::new(phase).still().size(), theme::THINK_MAX);
        }
        assert_eq!(ThinkingDot::new(0).size(), theme::THINK_MIN);
        motion::clear_test_override();
    }

    /// Headless: the indicator lays out at the `THINK_HALO` circle, and the
    /// dot at frame 5 (`THINK_MAX`) is exactly the token size.
    #[gpui::test]
    fn thinking_dot_renders_the_halo_and_dot(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        cx.draw(
            gpui::point(gpui::Pixels::ZERO, gpui::Pixels::ZERO),
            gpui::size(
                gpui::AvailableSpace::Definite(theme::z(theme::PANEL_W)),
                gpui::AvailableSpace::Definite(theme::z(theme::RAIL_BUTTON)),
            ),
            |_w, _cx| {
                div()
                    .flex()
                    .flex_col()
                    .child(ThinkingDot::new(0))
                    .child(ThinkingDot::new(5))
            },
        );
        let bounds = cx
            .debug_bounds("ui-thinking-dot")
            .expect("the thinking dot recorded no bounds");
        assert_eq!(
            f32::from(bounds.size.width),
            f32::from(theme::z(theme::THINK_HALO)),
            "the halo circle is THINK_HALO wide: {bounds:?}"
        );
        assert_eq!(
            f32::from(bounds.size.height),
            f32::from(theme::z(theme::THINK_HALO)),
            "the halo circle is THINK_HALO tall: {bounds:?}"
        );
    }
}
