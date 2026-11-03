//! `ui::Dot` — a status dot in a semantic tone (KIT.md v3, batch K1).
//!
//! Meaning (DESIGN.md §2): `Neutral` = running or done; `Attention` = result
//! ready or needs a look; `ActNow` = blocked on the owner or must-fix;
//! `Contrast` = a selection or a numbered marker. The dot paints its tone's
//! colour at `DOT` (or `DOT_L` with `.large()`); a pulsing `ActNow` dot
//! carries a `tone_soft` halo, with the 2.4 s breath driven by the parent
//! re-rendering on `theme::motion::PULSE_MS` — the same parent-driven phase
//! model as [`super::thinking_dot::ThinkingDot`].

use gpui::{
    div, prelude::*, AnyElement, App, BoxShadow, ElementId, IntoElement, Pixels, Point, RenderOnce,
    Rgba, SharedString, Window,
};

use crate::theme::{self, Tone};

/// A colour read from the palette — the kit's name for GPUI's colour value
/// (the parts never construct one themselves).
type PaletteColor = Rgba;

/// The pulse halo's width, in logical px before zoom. Part-local constant
/// (not a KIT token): the halo is an outer `tone_soft` ring, painted as a
/// box shadow so it never changes the dot's layout box.
const HALO: f32 = theme::DOT_HALO;

/// A status dot in a semantic tone.
#[derive(Clone, Debug, IntoElement)]
pub struct Dot {
    tone: Tone,
    large: bool,
    pulse: bool,
    phase: u8,
    id: Option<ElementId>,
    label: Option<SharedString>,
    dimmed: bool,
}

impl Dot {
    /// A `DOT`-sized dot in `tone`.
    pub fn new(tone: Tone) -> Self {
        Self {
            tone,
            large: false,
            pulse: false,
            phase: 0,
            id: None,
            label: None,
            dimmed: false,
        }
    }

    /// `DOT_L` instead of `DOT`.
    pub fn large(mut self) -> Self {
        self.large = true;
        self
    }

    /// The 2.4 s act-now pulse (`theme::motion::PULSE_MS`). `ActNow` only —
    /// the pulse is the act-now signal, and nothing else wears it.
    pub fn pulse(mut self) -> Self {
        debug_assert!(
            self.tone == Tone::ActNow,
            ".pulse() is for ActNow only (the pulse is the act-now signal)"
        );
        self.pulse = true;
        self
    }

    /// The pulse frame, `0..PULSE_FRAMES`, advanced by the parent's animation
    /// tick every `PULSE_MS / PULSE_FRAMES` (100 ms). Ignored unless `.pulse()`.
    pub fn phase(mut self, phase: u8) -> Self {
        self.phase = phase % theme::motion::PULSE_FRAMES;
        self
    }

    /// An element id: with a `.label()`, the dot shows the label as its tooltip
    /// (GPUI tooltips need a stateful element). Parts pass a unique id.
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// The accessible name, and the tooltip when the dot has none. Required
    /// when the dot is the only signal of its meaning: the kit enforces it in
    /// debug builds for every meaning-carrying dot (any non-Neutral tone).
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Paint `TEXT_DIM` instead of the tone: a dot inside a disabled part
    /// dims with it (disabled dims, it never deletes).
    pub fn dimmed(mut self) -> Self {
        self.dimmed = true;
        self
    }

    /// Whether the dot has its own id (and so, with a label, its own tooltip).
    pub fn has_id(&self) -> bool {
        self.id.is_some()
    }

    /// Whether the dot is dimmed.
    pub fn is_dimmed(&self) -> bool {
        self.dimmed
    }

    pub fn tone(&self) -> Tone {
        self.tone
    }

    /// The accessible name, if set.
    pub fn label_text(&self) -> Option<SharedString> {
        self.label.clone()
    }

    /// The kit's "dot with meaning" rule: a non-Neutral tone is a signal in
    /// its own right and must carry a label (checked here, and enforced at
    /// paint in debug builds).
    pub fn has_meaning_label(&self) -> bool {
        self.tone == Tone::Neutral || self.label.is_some()
    }

    pub fn is_large(&self) -> bool {
        self.large
    }

    pub fn is_pulsing(&self) -> bool {
        self.pulse
    }

    /// The painted diameter, in logical px (before zoom).
    pub fn size(&self) -> f32 {
        if self.large {
            theme::DOT_L
        } else {
            theme::DOT
        }
    }

    /// The dot's colour for its tone (`TEXT_DIM` when dimmed).
    pub fn color(&self) -> PaletteColor {
        if self.dimmed {
            return theme::palette().text_dim;
        }
        theme::tone_color(self.tone)
    }
}

/// One frame of the act-now pulse: the halo's spread (logical px before zoom)
/// and alpha. The ring grows from the dot to `PULSE_SPREAD` and fades out over
/// the first 70% of the 2.4 s cycle, then rests (the mockup's `@keyframes
/// pulse`). Under Reduce Motion the caller draws the still halo instead.
pub fn pulse_frame(phase: u8) -> (f32, f32) {
    let frames = f32::from(theme::motion::PULSE_FRAMES);
    let t = f32::from(phase % theme::motion::PULSE_FRAMES) / frames;
    const RISE: f32 = theme::motion::PULSE_RISE;
    const START_ALPHA: f32 = theme::motion::PULSE_START_ALPHA;
    if t < RISE {
        let k = t / RISE;
        (theme::PULSE_SPREAD * k, START_ALPHA * (1.0 - k))
    } else {
        (0.0, 0.0)
    }
}

impl RenderOnce for Dot {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // A meaning-carrying dot (a non-Neutral tone) is the only signal of
        // its state and must carry its label (KIT.md § Conventions).
        debug_assert!(
            self.tone == Tone::Neutral || self.label.is_some(),
            "a Dot with tone {:?} must carry a .label() — it is the only \
             signal of its meaning",
            self.tone
        );
        let halo = |color: gpui::Hsla, spread: f32| BoxShadow {
            color,
            offset: Point {
                x: Pixels::ZERO,
                y: Pixels::ZERO,
            },
            blur_radius: Pixels::ZERO,
            spread_radius: theme::z(spread),
            inset: false,
        };
        let shadow = if !self.pulse {
            None
        } else if crate::ui::motion::reduce_motion() {
            // Still: a fixed soft halo, no motion.
            Some(halo(theme::tone_soft(self.tone).into(), HALO))
        } else {
            let (spread, alpha) = pulse_frame(self.phase);
            let mut c: gpui::Hsla = theme::tone_color(self.tone).into();
            c.a = alpha;
            Some(halo(c, spread))
        };
        let dot = div()
            .flex_none()
            .w(theme::z(self.size()))
            .h(theme::z(self.size()))
            .rounded(theme::z(theme::RADIUS_PILL))
            .bg(self.color())
            // The halo is a box shadow, painted outside the layout box, so a
            // pulsing and a still dot of the same size lay out identically.
            .when_some(shadow, |d, s| d.shadow(vec![s]))
            .debug_selector(|| "ui-dot".to_string());
        let el: AnyElement = match (self.id, self.label) {
            (Some(id), Some(label)) => dot
                .id(id)
                .tooltip(crate::icons::tip(label.to_string()))
                .into_any_element(),
            _ => dot.into_any_element(),
        };
        el
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The act-now pulse is a real 2.4 s animation: the halo grows from the dot
    /// to PULSE_SPREAD while fading from 0.5 to 0 over the first 70% of the
    /// 24 frames, rests for the rest, and the phase wraps (Codex K1 review).
    #[test]
    fn pulse_frames_grow_fade_rest_and_wrap() {
        let n = theme::motion::PULSE_FRAMES;
        assert_eq!(
            u64::from(n) * 100,
            theme::motion::PULSE_MS,
            "24 frames of 100 ms"
        );
        let (s0, a0) = pulse_frame(0);
        assert_eq!((s0, a0), (0.0, 0.5), "starts at the dot, half visible");
        let mut last = (s0, a0);
        for p in 1..n {
            let (s, a) = pulse_frame(p);
            if s == 0.0 {
                assert_eq!(a, 0.0, "resting frames are invisible");
                continue;
            }
            assert!(s > last.0 && a < last.1, "frame {p} must grow and fade");
            assert!(s <= theme::PULSE_SPREAD);
            last = (s, a);
        }
        assert_eq!(pulse_frame(n - 1), (0.0, 0.0), "the cycle ends at rest");
        assert_eq!(pulse_frame(n), pulse_frame(0), "the phase wraps");
        assert_eq!(Dot::new(Tone::ActNow).pulse().phase(n + 3).phase, 3);
    }
    use crate::theme;
    use gpui::TestAppContext;

    /// The dot paints its tone's colour at the tone token's value (the part
    /// reads it through the palette, so a light swap retints it).
    #[test]
    fn dot_paints_its_tone_colour() {
        let p = theme::palette();
        assert_eq!(Dot::new(Tone::Neutral).color(), p.text_dim);
        assert_eq!(Dot::new(Tone::Attention).color(), p.attention);
        assert_eq!(Dot::new(Tone::ActNow).color(), p.act_now);
        assert_eq!(Dot::new(Tone::Contrast).color(), p.contrast);
    }

    /// `DOT` by default, `DOT_L` with `.large()` — both theme tokens.
    #[test]
    fn dot_sizes_are_the_theme_tokens() {
        assert_eq!(Dot::new(Tone::Neutral).size(), theme::DOT);
        assert_eq!(Dot::new(Tone::Neutral).large().size(), theme::DOT_L);
        assert!(Dot::new(Tone::Neutral).large().is_large());
    }

    /// The API forces the pulse rule: `.pulse()` debug-asserts on any tone
    /// but `ActNow` (caught in debug builds; a no-op in release, where the
    /// gallery is the gate).
    #[test]
    fn pulse_is_actnow_only() {
        let ok = std::panic::catch_unwind(|| Dot::new(Tone::ActNow).pulse());
        assert!(ok.is_ok(), "ActNow may pulse");
        for tone in [Tone::Neutral, Tone::Attention, Tone::Contrast] {
            let denied = std::panic::catch_unwind(|| Dot::new(tone).pulse());
            // The rule is a `debug_assert`: it fires in debug builds and the
            // release build keeps drawing (the gallery is the gate there).
            assert_eq!(
                denied.is_err(),
                cfg!(debug_assertions),
                ".pulse() on {:?}: debug builds assert, release builds do not",
                tone
            );
        }
    }

    /// The "dot with meaning" rule: a non-Neutral tone is the only signal of
    /// its state and must carry a label (enforced at paint in debug builds).
    #[test]
    fn a_meaning_carrying_dot_requires_a_label() {
        assert!(
            !Dot::new(Tone::Attention).has_meaning_label(),
            "an unlabeled Attention dot is meaning without a label"
        );
        assert!(
            !Dot::new(Tone::ActNow).has_meaning_label(),
            "an unlabeled ActNow dot is meaning without a label"
        );
        assert!(
            Dot::new(Tone::Attention)
                .label("Result ready")
                .has_meaning_label(),
            "the label satisfies the rule"
        );
        // A Neutral dot is context-dependent (running / done) and paints
        // without a label.
        assert!(
            Dot::new(Tone::Neutral).has_meaning_label(),
            "a Neutral dot paints without a label"
        );
        // The builder API itself: the label is stored and readable back.
        let d = Dot::new(Tone::Attention).label("Result ready");
        assert_eq!(d.label_text().as_deref(), Some("Result ready"));
        assert!(d.label_text().is_some());
        assert!(Dot::new(Tone::Neutral).label_text().is_none());
    }

    /// Headless: each dot lays out at exactly its token size, and the pulse
    /// halo is outer paint — a pulsing dot and a still dot of the same size
    /// lay out identically (the halo is a box shadow, not layout).
    #[gpui::test]
    fn dot_lays_out_at_its_token_size(cx: &mut TestAppContext) {
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
                    .flex_row()
                    .items_center()
                    .gap(theme::z(theme::SP_8))
                    .child(
                        div()
                            .debug_selector(|| "plain".to_string())
                            .child(Dot::new(Tone::Attention).label("Result ready")),
                    )
                    .child(
                        div()
                            .debug_selector(|| "large".to_string())
                            .child(Dot::new(Tone::ActNow).large().label("Blocked")),
                    )
                    .child(
                        div()
                            .debug_selector(|| "pulsing".to_string())
                            .child(Dot::new(Tone::ActNow).large().label("Blocked").pulse()),
                    )
            },
        );
        let plain = cx
            .debug_bounds("plain")
            .unwrap_or_else(|| panic!("the plain dot recorded no bounds"));
        let large = cx
            .debug_bounds("large")
            .unwrap_or_else(|| panic!("the still large dot recorded no bounds"));
        let pulsing = cx
            .debug_bounds("pulsing")
            .unwrap_or_else(|| panic!("the pulsing dot recorded no bounds"));
        assert_eq!(
            f32::from(plain.size.width),
            f32::from(theme::z(theme::DOT)),
            "the plain dot is exactly DOT wide: {plain:?}"
        );
        assert_eq!(
            f32::from(large.size.width),
            f32::from(theme::z(theme::DOT_L)),
            "the large dot is exactly DOT_L wide: {large:?}"
        );
        assert_eq!(
            f32::from(pulsing.size.width),
            f32::from(large.size.width),
            "the pulse halo is outer paint: it changes nothing about layout"
        );
        assert_eq!(
            f32::from(pulsing.size.height),
            f32::from(large.size.height),
            "the pulse halo is outer paint: the height is unchanged too"
        );
    }
}
