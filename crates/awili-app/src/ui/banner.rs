//! `ui::Banner` — the kit's one-line status strip (KIT.md v3, batch K2).
//!
//! A single line: a `DOT` in the tone colour, one `type_::UI` sentence of
//! `TEXT`, an optional quiet `Button` at the end — on a `tone_soft` wash
//! with a 1 px `tone_edge` line, `RADIUS_M` corners, 12 px top/bottom and
//! 16 px left/right. The state is painted (never an icon); the only
//! conventions the banner carries are `.selector` and `.label` (it is not
//! interactive, so no click, no focus).

use gpui::{
    div, prelude::*, AnyElement, App, ElementId, IntoElement, RenderOnce, Rgba, SharedString,
    Window,
};

use crate::theme::{self, palette, Tone};
use crate::ui::button::Button;
use crate::ui::dot::Dot;
use crate::ui::interaction::{child_id, Common};
use crate::ui::text;

/// The tone's meaning in the KIT's wording (DESIGN.md §2 — the same table
/// [`Dot`] documents): what a dot of this tone tells the owner.
pub fn tone_meaning(tone: Tone) -> &'static str {
    match tone {
        Tone::Neutral => "running or done",
        Tone::Attention => "result ready or needs a look",
        Tone::ActNow => "blocked on the owner or must-fix",
        Tone::Contrast => "a selection or a numbered marker",
    }
}

/// The part tag for the default `debug_selector` (a banner is not
/// interactive, so it carries only the shared state's selectors).
const BANNER: &str = "banner";

/// A kit status banner. Non-interactive: the state is painted, the
/// optional `Button` at the right carries the action.
#[derive(IntoElement)]
pub struct Banner {
    common: Common,
    tone: Tone,
    message: SharedString,
    action: Option<AnyElement>,
    /// `.quiet_dot()`: the dot carries no tooltip of its own (inside a
    /// disabled container, whose reason must be the only tooltip).
    quiet_dot: bool,
}

impl Banner {
    /// A one-line status banner in the given tone.
    pub fn new(id: impl Into<ElementId>, tone: Tone, text: impl Into<SharedString>) -> Self {
        let message = text.into();
        Self {
            common: Common {
                id: id.into(),
                label: Some(message.clone()),
                ..Default::default()
            },
            tone,
            message,
            action: None,
            quiet_dot: false,
        }
    }

    /// The current tone.
    pub fn tone_of(&self) -> Tone {
        self.tone
    }

    /// The message the banner paints.
    pub fn message(&self) -> &SharedString {
        &self.message
    }

    /// The accessible label (the message, unless set otherwise).
    pub fn label(mut self, text: impl Into<SharedString>) -> Self {
        self.common.label = Some(text.into());
        self
    }

    /// The `debug_selector` override (KIT.md § Conventions).
    pub fn selector(mut self, selector: &'static str) -> Self {
        self.common.selector = Some(selector);
        self
    }

    /// An action `Button` at the right end (a "Fix now" kind of action).
    /// The dot keeps its label but shows no tooltip of its own (a
    /// disabled container's reason is then the only tooltip).
    pub fn quiet_dot(mut self) -> Self {
        self.quiet_dot = true;
        self
    }

    pub fn action(mut self, button: Button) -> Self {
        self.action = Some(button.into_any_element());
        self
    }

    /// The accessible label (the message, unless set otherwise).
    pub fn label_text(&self) -> Option<SharedString> {
        self.common.label.clone()
    }

    /// Whether the banner carries an action button.
    pub fn has_action(&self) -> bool {
        self.action.is_some()
    }

    /// The paint: (wash fill, edge colour, dot colour).
    pub fn paint(&self) -> (Rgba, Rgba, Rgba) {
        let (soft, edge, color) = (
            theme::tone_soft(self.tone),
            theme::tone_edge(self.tone),
            theme::tone_color(self.tone),
        );
        (soft, edge, color)
    }

    /// The dot's label: the tone's meaning (the dot is the tone's signal;
    /// the message sits right next to it).
    pub fn dot_label(&self) -> &'static str {
        tone_meaning(self.tone)
    }

    /// The dot's `ElementId` — a named child of the banner's id, so the
    /// dot is stateful and can carry its meaning as a tooltip.
    pub fn dot_id(&self) -> ElementId {
        child_id(&self.common.id, "dot")
    }
}

impl std::fmt::Debug for Banner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Banner({})", self.common.selector_for(BANNER))
    }
}

impl RenderOnce for Banner {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let (fill, edge, _) = self.paint();
        // The dot carries its label (the tone's meaning — the debug-build
        // rule that a meaning-carrying dot needs one) and a unique id
        // (with both, it shows the meaning as its tooltip).
        let dot_label = self.dot_label();
        let dot_id = self.dot_id();
        let common = self.common;
        let action = self.action;
        let dot = Dot::new(self.tone).label(dot_label);
        let dot = if self.quiet_dot { dot } else { dot.id(dot_id) };

        div()
            .flex()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .px(theme::z(theme::SP_16))
            .py(theme::z(theme::SP_12))
            .rounded(theme::z(theme::RADIUS_M))
            .bg(fill)
            .border_1()
            .border_color(edge)
            .debug_selector(|| common.selector_for(BANNER))
            .child(dot)
            // The message wraps: a long one is never cut off (KIT.md § Long text).
            .child(
                div().flex_1().min_w_0().child(
                    text::ui_text(self.message)
                        .text_color_override(palette().text)
                        .wrap(),
                ),
            )
            .when(action.is_some(), |d| {
                d.child(div().ml_auto().flex().child(action.unwrap()))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// A long message wraps (KIT.md § Long text): in a narrow banner it takes
    /// more height than a short one, so none of it is cut off.
    #[gpui::test]
    fn long_banner_message_wraps(cx: &mut TestAppContext) {
        let height = |cx: &mut TestAppContext, msg: &'static str| {
            let (_root, wcx) = render_part(cx, move || {
                div()
                    .w(theme::z(theme::PANEL_W))
                    .child(Banner::new("b", Tone::Attention, msg))
            });
            wcx.debug_bounds("ui-banner-b").unwrap().size.height
        };
        let short = height(cx, "Saved");
        let long = height(
            cx,
            "The config file has an error on line 12, and the last good copy is from yesterday",
        );
        assert!(
            long > short,
            "the long message wrapped: {long:?} vs {short:?}"
        );
    }

    /// The paint: the tone's soft wash, the tone's edge line, the tone's
    /// dot colour.
    #[test]
    fn banner_paint_uses_the_tone_tokens() {
        let p = palette();
        let attention = Banner::new("b", Tone::Attention, "The session expired");
        assert_eq!(
            attention.paint(),
            (p.attention_soft, p.attention_edge, p.attention)
        );
        let act = Banner::new("b", Tone::ActNow, "Sign in");
        assert_eq!(act.paint(), (p.act_now_soft, p.act_now_edge, p.act_now));
        let neutral = Banner::new("b", Tone::Neutral, "Up to date");
        assert_eq!(
            neutral.paint(),
            (p.transparent, p.line, p.text_dim),
            "neutral tone has no soft fill (the transparent token)"
        );
    }

    /// The label defaults to the message and can be overridden.
    #[test]
    fn banner_label_defaults_to_the_message() {
        let b = Banner::new("b", Tone::Attention, "The session expired");
        assert_eq!(b.label_text().as_deref(), Some("The session expired"));
        let b = b.label("Session expired");
        assert_eq!(b.label_text().as_deref(), Some("Session expired"));
    }

    /// The rendered banner is laid out and carries its dot, message and
    /// action.
    #[gpui::test]
    fn banner_renders_dot_message_and_action(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().flex_col().child(
                Banner::new("session", Tone::Attention, "The session expired.")
                    .selector("session-banner")
                    .action(Button::new("banner-action", "Sign in again")),
            )
        });
        let b = wcx
            .debug_bounds("session-banner")
            .unwrap_or_else(|| panic!("the banner recorded no bounds"));
        assert!(
            f32::from(b.size.height) >= f32::from(theme::z(theme::SP_24)),
            "the banner is at least its vertical padding + a text line: {b:?}",
        );
        let banner = Banner::new("x", Tone::Attention, "x").action(Button::new("a", "Fix now"));
        assert!(banner.has_action());
    }

    /// The dot's label is the tone's meaning (the debug-build rule every
    /// meaning-carrying dot needs a label) — and every tone renders
    /// without tripping that rule.
    #[test]
    fn banner_dot_carries_the_tone_meaning() {
        for tone in [Tone::Neutral, Tone::Attention, Tone::ActNow, Tone::Contrast] {
            let b = Banner::new("b", tone, "m");
            assert_eq!(
                b.dot_label(),
                tone_meaning(tone),
                "the {tone:?} dot explains the tone"
            );
            assert!(!b.dot_label().is_empty(), "the label is never empty");
        }
        // Unique under the banner's id.
        assert_ne!(
            Banner::new("a", Tone::Attention, "m").dot_id(),
            Banner::new("b", Tone::Attention, "m").dot_id(),
        );
        assert_eq!(
            Banner::new("a", Tone::Attention, "m").dot_id(),
            child_id(&ElementId::Name("a".into()), "dot"),
        );
    }

    /// Every tone renders headless without tripping the dot's
    /// debug-assert (a non-Neutral tone without a label would panic).
    #[gpui::test]
    fn banner_renders_every_tone(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .child(Banner::new("t-neutral", Tone::Neutral, "Up to date"))
                .child(Banner::new("t-attention", Tone::Attention, "Result ready"))
                .child(Banner::new("t-actnow", Tone::ActNow, "Sign in to continue"))
                .child(Banner::new("t-contrast", Tone::Contrast, "Step 2 of 5"))
        });
        for sel in [
            "ui-banner-t-neutral",
            "ui-banner-t-attention",
            "ui-banner-t-actnow",
            "ui-banner-t-contrast",
        ] {
            assert!(
                wcx.debug_bounds(sel).is_some(),
                "{sel} laid out without tripping the dot's label rule"
            );
        }
    }
}
