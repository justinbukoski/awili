//! `ui::EmptyState` — the kit's empty state (KIT.md v3, batch K4).
//!
//! Centred: the optional glyph (the mockup's `.empty .glyph` — a 72 px
//! `CONTRAST_SOFT` tile with the `CONTRAST_TEXT` icon), the heading
//! (`type_::TITLE`, weight-600) and the `TEXT_2` body — with the
//! optional `.action(Button)`.
//!
//! The look matches the owner-approved mockup (`A-design.html`): the
//! `12vh` top margin is snapped to `SP_40` (GPUI has no viewport
//! percentages), and the mockup's 14 px stack gap and 520 px measure
//! are snapped to `SP_12` / `EMPTY_MAX` (the off-grid 14 and 520 are
//! not on the v2 tokens).

use gpui::{div, prelude::*, App, ElementId, IntoElement, RenderOnce, SharedString, Window};

use crate::icons::{self, Icon};
use crate::theme::{self, palette};
use crate::ui::button::Button;
use crate::ui::interaction::Common;
use crate::ui::text;

/// The part tag for the default `debug_selector`.
const EMPTY_STATE: &str = "empty-state";
/// The glyph tile's side, in logical px before zoom (the mockup's
/// 72 px).
const GLYPH: f32 = theme::EMPTY_GLYPH;
/// The glyph tile's corner rounding (the mockup's 22 px, snapped to a
/// local const — 22 is off the v2 radius grid).
const GLYPH_RADIUS: f32 = theme::EMPTY_GLYPH_RADIUS;
/// The glyph's icon size (the mockup's 34 px).
const GLYPH_ICON: f32 = theme::EMPTY_GLYPH_ICON;
/// The empty state's maximum width (the mockup's 520 px measure,
/// snapped to a local const — 520 is off the v2 tokens).
const EMPTY_MAX: f32 = theme::EMPTY_MAX;

/// A kit empty state. Build with [`EmptyState::new(id, title, body)`]
/// and chain the optional `.glyph(Icon)` and `.action(Button)`.
#[derive(IntoElement)]
pub struct EmptyState {
    common: Common,
    title: SharedString,
    body: SharedString,
    glyph: Option<Icon>,
    action: Option<Button>,
    top: Option<gpui::Pixels>,
}

impl EmptyState {
    /// An empty state with its heading and its `TEXT_2` body.
    pub fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        body: impl Into<SharedString>,
    ) -> Self {
        let title = title.into();
        Self {
            common: Common {
                id: id.into(),
                label: Some(title.clone()),
                ..Default::default()
            },
            title,
            body: body.into(),
            glyph: None,
            action: None,
            top: None,
        }
    }

    /// The glyph: the `CONTRAST_SOFT` tile with the `CONTRAST_TEXT`
    /// icon (the mockup's `.empty .glyph`).
    /// The top margin, when the empty state sits in a region other than
    /// the window (the default is `EMPTY_TOP` of the window height).
    pub fn top(mut self, top: gpui::Pixels) -> Self {
        self.top = Some(top);
        self
    }

    pub fn glyph(mut self, icon: Icon) -> Self {
        self.glyph = Some(icon);
        self
    }

    /// The glyph's icon, if any.
    pub fn glyph_icon(&self) -> Option<Icon> {
        self.glyph
    }

    /// The optional action (usually a `Button.lead` — the "add one"
    /// CTA).
    pub fn action(mut self, action: Button) -> Self {
        self.action = Some(action);
        self
    }

    /// Whether the part carries an action.
    pub fn has_action(&self) -> bool {
        self.action.is_some()
    }

    /// The heading.
    pub fn title_text(&self) -> &SharedString {
        &self.title
    }

    /// The body.
    pub fn body_text(&self) -> &SharedString {
        &self.body
    }

    /// The `debug_selector` override (KIT.md § Conventions).
    pub fn selector(mut self, selector: &'static str) -> Self {
        self.common.selector = Some(selector);
        self
    }

    /// The accessible label (defaults to the heading).
    pub fn label(mut self, text: impl Into<SharedString>) -> Self {
        self.common.label = Some(text.into());
        self
    }
}

impl std::fmt::Debug for EmptyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EmptyState({})", self.common.selector_for(EMPTY_STATE))
    }
}

impl RenderOnce for EmptyState {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let p = palette();
        let common = self.common;
        let selector = common.selector_for(EMPTY_STATE);
        // The mockup's `margin: 12vh auto 0`: centred on its own, a share
        // of the window height above.
        let top = self
            .top
            .unwrap_or_else(|| window.bounds().size.height * theme::EMPTY_TOP);

        let mut stack = div()
            .id(common.id.clone())
            .flex()
            .flex_col()
            .items_center()
            .text_center()
            // The mockup's `gap: 14px` snaps down to the 4 px grid (DESIGN.md
            // §10.3, departure 3).
            .gap(theme::z(theme::SP_12))
            .w_full()
            .max_w(theme::z(EMPTY_MAX))
            .mx_auto()
            .pt(top)
            .debug_selector(|| selector.clone());

        if let Some(glyph) = self.glyph {
            stack = stack.child(
                div()
                    .id(crate::ui::interaction::child_id(&common.id, "glyph"))
                    .debug_selector(|| format!("{selector}-glyph"))
                    .size(theme::z(GLYPH))
                    .rounded(theme::z(GLYPH_RADIUS))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(p.contrast_soft)
                    .child(icons::icon(glyph, GLYPH_ICON, p.contrast_text)),
            );
        }
        stack = stack
            // Both wrap: an empty state's words are never truncated.
            .child(
                div()
                    .debug_selector(|| format!("{selector}-title"))
                    .child(text::title(self.title.clone()).strong().wrap()),
            )
            .child(
                div()
                    .debug_selector(|| format!("{selector}-body"))
                    .child(text::read(self.body.clone()).wrap()),
            );
        if let Some(action) = self.action {
            stack = stack.child(
                div()
                    .id(crate::ui::interaction::child_id(&common.id, "action"))
                    .debug_selector(|| format!("{selector}-action"))
                    .flex()
                    .pt(theme::z(theme::SP_4))
                    .child(action),
            );
        }
        stack
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// The empty state lays out headless, centred, with the glyph
    /// (the `CONTRAST_SOFT` tile at the mockup's 72 px), the heading,
    /// the body and the action — matching the mockup's sample text
    /// ("Design is coming in a later release.").
    #[gpui::test]
    fn empty_state_renders_the_mockup_face(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(
                EmptyState::new(
                    "design",
                    "Design is coming in a later release.",
                    "This is where you'll make images, layouts and mockups with Awili.",
                )
                .glyph(Icon::Image)
                .action(Button::new("design-cta", "Try the council").lead()),
            )
        });
        let state = wcx
            .debug_bounds("ui-empty-state-design")
            .unwrap_or_else(|| panic!("the empty state recorded no bounds"));
        assert!(
            f32::from(state.size.width) > 0.0 && f32::from(state.size.height) > 0.0,
            "the empty state has size: {state:?}"
        );
        let glyph = wcx
            .debug_bounds("ui-empty-state-design-glyph")
            .unwrap_or_else(|| panic!("the glyph recorded no bounds"));
        assert_eq!(
            (f32::from(glyph.size.width), f32::from(glyph.size.height)),
            (f32::from(theme::z(GLYPH)), f32::from(theme::z(GLYPH)),),
            "the glyph tile is exactly 72 × 72: {glyph:?}"
        );
        assert!(
            wcx.debug_bounds("ui-empty-state-design-action").is_some(),
            "the action laid out"
        );
    }

    /// Without a glyph or an action the state is just the centred text
    /// (the measure is clamped at `EMPTY_MAX`).
    #[gpui::test]
    fn empty_state_without_optional_faces(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(EmptyState::new(
                "bare",
                "Nothing here yet.",
                "Jobs you start will land here.",
            ))
        });
        let state = wcx
            .debug_bounds("ui-empty-state-bare")
            .unwrap_or_else(|| panic!("the empty state recorded no bounds"));
        assert!(
            f32::from(state.size.width) <= f32::from(theme::z(EMPTY_MAX)) + 2.0,
            "the measure is at most EMPTY_MAX: {state:?}"
        );
        assert!(
            wcx.debug_bounds("ui-empty-state-bare-glyph").is_none(),
            "no glyph tile was requested"
        );
    }

    /// K4b: the empty state centres itself in a wide parent and wraps its
    /// title and body (never truncated).
    #[gpui::test]
    fn empty_state_centres_and_wraps(cx: &mut TestAppContext) {
        let (_root, wcx) = crate::ui::interaction::test_util::render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .w(theme::z(1200.0))
                .debug_selector(|| "es-parent".to_string())
                .child(EmptyState::new(
                    "es",
                    "Nothing here yet — a long title that has to wrap onto a second line",
                    "Start a conversation and it shows here. This body is long enough to wrap \
                     across more than one line at the 520 px limit.",
                ))
        });
        let parent = wcx.debug_bounds("es-parent").unwrap();
        let es = wcx.debug_bounds("ui-empty-state-es").unwrap();
        let (pc, ec) = (parent.center().x, es.center().x);
        assert!(
            (f32::from(pc) - f32::from(ec)).abs() < 1.0,
            "centred: {es:?} in {parent:?}"
        );
        assert!(
            f32::from(es.size.width) <= f32::from(theme::z(EMPTY_MAX)) + 0.5,
            "within its max width: {es:?}"
        );
        let title = wcx.debug_bounds("ui-empty-state-es-title").unwrap();
        let body = wcx.debug_bounds("ui-empty-state-es-body").unwrap();
        assert!(
            f32::from(title.size.height) >= f32::from(theme::z(theme::type_::TITLE.line)) * 1.9,
            "the title wrapped: {title:?}"
        );
        assert!(
            f32::from(body.size.height) >= f32::from(theme::z(theme::type_::READ.line)) * 1.9,
            "the body wrapped: {body:?}"
        );
    }
}
