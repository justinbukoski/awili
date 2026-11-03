//! `ui::Card` — the kit's container card (KIT.md v3, batch K2).
//!
//! The mockup's `.card`: `PANEL_SOLID` surface, a 1 px `LINE` edge (the
//! tone's `tone_edge` on a toned card), `RADIUS_L` corners, 20 px
//! padding. Optional label line (a `Dot` + a strong `type_::LABEL`), a
//! `type_::HEADING` title, `type_::UI` `TEXT_2` body text (wrapped),
//! stacked children, and an action row at the bottom. A non-interactive
//! container: no click, no focus — the shared conventions it carries are
//! `.selector` and `.label`.

use gpui::{div, prelude::*, App, ElementId, IntoElement, RenderOnce, Rgba, SharedString, Window};

use crate::theme::{self, palette, Tone};
use crate::ui::dot::Dot;
use crate::ui::interaction::Common;
use crate::ui::text;

/// The part tag for the default `debug_selector`.
const CARD: &str = "card";

/// A kit card. Build with [`Card::new`] and chain the content slots;
/// interactive content is the parent's (a `Card` never takes a click).
#[derive(IntoElement, Default)]
pub struct Card {
    common: Common,
    label: Option<SharedString>,
    title: Option<SharedString>,
    tone: Option<Tone>,
    body: Option<SharedString>,
    actions: Vec<gpui::AnyElement>,
    children: Vec<gpui::AnyElement>,
    flush: bool,
}

impl Card {
    /// An empty card (the `id` feeds the default `debug_selector`).
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            common: Common {
                id: id.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// The label line: a `Dot` + a strong `type_::LABEL` (the tone's text
    /// colour when a tone is set).
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self.common.label = self.label.clone();
        self
    }

    /// The accessible label (the label line's text, unless set otherwise).
    pub fn label_override(mut self, text: impl Into<SharedString>) -> Self {
        self.common.label = Some(text.into());
        self
    }

    /// The `type_::HEADING` title line.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The tone of the card: the `tone_soft` wash and the `tone_edge`
    /// line (and the label dot's tone).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    /// The current tone, if any.
    pub fn tone_of(&self) -> Option<Tone> {
        self.tone
    }

    /// The `type_::UI` `TEXT_2` body paragraph (wrapped).
    pub fn body(mut self, body: impl Into<SharedString>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// The content slots: (label, title, body).
    pub fn content(
        &self,
    ) -> (
        Option<SharedString>,
        Option<SharedString>,
        Option<SharedString>,
    ) {
        (self.label.clone(), self.title.clone(), self.body.clone())
    }

    /// The bottom action row (buttons, chips …).
    pub fn actions(mut self, actions: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.actions = actions
            .into_iter()
            .map(IntoElement::into_any_element)
            .collect();
        self
    }

    /// The number of actions.
    pub fn actions_len(&self) -> usize {
        self.actions.len()
    }

    /// Stacked content children, between the body and the actions.
    pub fn children(mut self, children: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.children = children
            .into_iter()
            .map(IntoElement::into_any_element)
            .collect();
        self
    }

    /// The number of stacked children.
    pub fn children_len(&self) -> usize {
        self.children.len()
    }

    /// No padding and no gap: the children bring their own, edge to edge
    /// (the mockup's Council `.track`, a divided header over its body).
    pub fn flush(mut self) -> Self {
        self.flush = true;
        self
    }

    /// Whether the card is flush.
    pub fn is_flush(&self) -> bool {
        self.flush
    }

    /// The card's label, if set (the clabel line).
    pub fn label_text(&self) -> Option<SharedString> {
        self.label.clone()
    }

    /// The `debug_selector` override (KIT.md § Conventions).
    pub fn selector(mut self, selector: &'static str) -> Self {
        self.common.selector = Some(selector);
        self
    }

    /// The paint: (fill, edge colour).
    pub fn paint(&self) -> (Rgba, Rgba) {
        let p = palette();
        match self.tone {
            Some(tone) => {
                let soft = theme::tone_soft(tone);
                // Soft washes are gone for attention / act-now: the card keeps
                // the opaque PANEL and carries the tone on its edge.
                let fill = if soft.a == 0.0 { p.panel_solid } else { soft };
                (fill, theme::tone_edge(tone))
            }
            None => (p.panel_solid, p.line),
        }
    }

    /// The label dot's label: the card's label (the dot sits in the label
    /// line, next to the text it names).
    pub fn dot_label(&self) -> Option<SharedString> {
        self.label.clone()
    }

    /// The label dot's `ElementId` — a named child of the card's id, so
    /// the dot is stateful and can carry its label as a tooltip.
    pub fn dot_id(&self) -> ElementId {
        crate::ui::interaction::child_id(&self.common.id, "dot")
    }
}

impl std::fmt::Debug for Card {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Card({})", self.common.selector_for(CARD))
    }
}

impl RenderOnce for Card {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let (fill, edge) = self.paint();
        // The label line's values (and the dot's unique id under the
        // card's id) are computed before the common state moves.
        let label = self.label.clone();
        let tone = self.tone;
        let dot_id = self.dot_id();
        let label_color = match tone {
            Some(tone) => theme::tone_text(tone),
            None => palette().text,
        };
        let flush = self.flush;
        let common = self.common;

        let mut root = div()
            .flex()
            .flex_col()
            .when(!flush, |d| {
                d.gap(theme::z(theme::SP_8)).p(theme::z(theme::SP_20))
            })
            .when(flush, |d| d.overflow_hidden())
            .rounded(theme::z(theme::RADIUS_L))
            .border_1()
            .border_color(edge)
            .when(tone == Some(Tone::ActNow), |d| d.border_l_2())
            .bg(fill)
            .w_full()
            .debug_selector(|| common.selector_for(CARD));

        if let Some(label) = label {
            // The dot carries the card's label (the debug-build rule every
            // meaning-carrying dot needs one) and a unique id (with both,
            // it shows the label as its tooltip).
            root = root.child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme::z(theme::SP_8))
                    .child(
                        Dot::new(tone.unwrap_or(Tone::Neutral))
                            .label(label.clone())
                            .id(dot_id),
                    )
                    .child(text::label(label).strong().text_color_override(label_color)),
            );
        }

        let mut mid = div().flex().flex_col().gap(theme::z(theme::SP_4));
        let mut has_mid = false;
        if self.title.is_some() {
            // The title wraps: a long one is never cut off (KIT.md § Long text).
            mid = mid.child(text::heading(self.title.clone().unwrap()).wrap());
            has_mid = true;
        }
        if self.body.is_some() {
            mid = mid.child(text::ui_text(self.body.clone().unwrap()).reading().wrap());
            has_mid = true;
        }
        if has_mid {
            root = root.child(mid);
        }

        for child in self.children {
            root = root.child(child);
        }

        if !self.actions.is_empty() {
            let mut row = div().flex().items_center().gap(theme::z(theme::SP_8));
            for action in self.actions {
                row = row.child(action);
            }
            root = root.child(row);
        }

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::button::Button;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// A long title wraps (KIT.md § Long text): taller than a short one in
    /// the same width, never cut off.
    #[gpui::test]
    fn long_card_title_wraps(cx: &mut TestAppContext) {
        let height = |cx: &mut TestAppContext, title: &'static str| {
            let (_root, wcx) = render_part(cx, move || {
                div()
                    .w(theme::z(theme::PANEL_W))
                    .child(Card::new("c").title(title))
            });
            wcx.debug_bounds("ui-card-c").unwrap().size.height
        };
        let short = height(cx, "Approve?");
        let long = height(
            cx,
            "Swap in the fixed zcode command for every agent that talks to GLM?",
        );
        assert!(
            long > short,
            "the long title wrapped: {long:?} vs {short:?}"
        );
    }

    /// A flush card adds no padding: it is its child plus the 1 px edge;
    /// a plain card adds `SP_20` each side.
    #[gpui::test]
    fn flush_card_adds_only_its_edge(cx: &mut TestAppContext) {
        let side = theme::z(theme::MENU_W);
        let build = |flush: bool| {
            move || {
                let card = Card::new(if flush { "flush" } else { "padded" })
                    .children([div().size(side).into_any_element()]);
                div().flex().items_start().w(side * 2.0).child(if flush {
                    card.flush()
                } else {
                    card
                })
            }
        };
        let (_root, wcx) = render_part(cx, build(true));
        let flush = wcx.debug_bounds("ui-card-flush").unwrap();
        assert_eq!(
            flush.size.height,
            side + gpui::px(2.0),
            "flush: child + edge"
        );
        let (_root, wcx) = render_part(cx, build(false));
        let padded = wcx.debug_bounds("ui-card-padded").unwrap();
        assert_eq!(
            padded.size.height,
            side + gpui::px(2.0) + theme::z(theme::SP_20) * 2.0,
            "padded: child + edge + SP_20 each side"
        );
    }

    /// The paint: `PANEL_SOLID` + `LINE` by default, the tone's tokens on
    /// a toned card.
    #[test]
    fn card_paint_uses_the_panel_and_tone_tokens() {
        let p = palette();
        assert_eq!(Card::new("c").paint(), (p.panel_solid, p.line));
        assert_eq!(
            Card::new("c").tone(Tone::Attention).paint(),
            (p.panel_solid, p.attention_edge)
        );
    }

    /// The content slots and the action/children counts.
    #[test]
    fn card_content_slots() {
        let c = Card::new("c")
            .label("Context")
            .title("What the agent reads")
            .body("The files, the rules and the history.")
            .actions([Button::new("a1", "Open").quiet(), Button::new("a2", "Edit")])
            .children([text::caption("12 files")]);
        assert_eq!(
            c.content(),
            (
                Some("Context".into()),
                Some("What the agent reads".into()),
                Some("The files, the rules and the history.".into()),
            )
        );
        assert_eq!(c.actions_len(), 2);
        assert_eq!(c.children_len(), 1);
        assert_eq!(c.label_text().as_deref(), Some("Context"));
    }

    /// The rendered card lays out with its label, title, body, children
    /// and actions.
    #[gpui::test]
    fn card_renders_its_slots(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().flex_col().child(
                Card::new("ctx")
                    .selector("ctx-card")
                    .label("Context")
                    .title("What the agent reads")
                    .body("The files, the rules and the history of this job.")
                    .actions([Button::new("ctx-open", "Open").quiet()]),
            )
        });
        let b = wcx
            .debug_bounds("ctx-card")
            .unwrap_or_else(|| panic!("the card recorded no bounds"));
        assert!(
            f32::from(b.size.width) > 0.0 && f32::from(b.size.height) > 0.0,
            "the card laid out: {b:?}"
        );
        // The inner button is laid out inside the card.
        assert!(wcx.debug_bounds("ui-button-ctx-open").is_some());
    }

    /// The label dot's label is the card's label, and its id is a named
    /// child of the card's id (unique per card).
    #[test]
    fn card_dot_carries_the_card_label_and_unique_id() {
        let c = Card::new("c1").label("Context").tone(Tone::Attention);
        assert_eq!(c.dot_label().as_deref(), Some("Context"));
        assert_eq!(
            c.dot_id(),
            crate::ui::interaction::child_id(&ElementId::Name("c1".into()), "dot"),
        );
        assert_ne!(
            Card::new("a").label("x").dot_id(),
            Card::new("b").label("x").dot_id(),
            "the dot ids are unique under the card's id"
        );
        // A card without a label line paints no dot (and no dot label).
        assert_eq!(Card::new("c2").dot_label(), None);
    }

    /// Every tone renders headless without tripping the dot's
    /// debug-assert (a non-Neutral tone without a label would panic).
    #[gpui::test]
    fn card_renders_every_tone(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .child(
                    Card::new("t-neutral")
                        .selector("card-t-neutral")
                        .label("Neutral state"),
                )
                .child(
                    Card::new("t-attention")
                        .selector("card-t-attention")
                        .label("Attention state")
                        .tone(Tone::Attention),
                )
                .child(
                    Card::new("t-actnow")
                        .selector("card-t-actnow")
                        .label("Act-now state")
                        .tone(Tone::ActNow),
                )
                .child(
                    Card::new("t-contrast")
                        .selector("card-t-contrast")
                        .label("Contrast state")
                        .tone(Tone::Contrast),
                )
        });
        for sel in [
            "card-t-neutral",
            "card-t-attention",
            "card-t-actnow",
            "card-t-contrast",
        ] {
            assert!(
                wcx.debug_bounds(sel).is_some(),
                "{sel} laid out without tripping the dot's label rule"
            );
        }
    }
}
