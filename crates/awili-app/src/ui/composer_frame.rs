//! `ui::ComposerFrame` — the kit's composer chrome (KIT.md v3, batch K4).
//!
//! The floating capsule that holds the existing `Composer` entity (its
//! editor, IME and behaviour are unchanged — the parent passes the
//! rendered editor into `.field(…)`): `PANEL_SOLID`, `RADIUS_XL`,
//! `float_shadow()`, a `CONTROL_LINE` edge, at most `COMPOSER_MAX` wide.
//!
//! The look matches the owner-approved mockup's `.composer`
//! (`A-studio.html`): a row (attach, the text field, the trailing
//! actions) over a chip footer, bottom-aligned. The mockup's off-grid
//! paddings are snapped to the spacing grid (its `6px` top/right and
//! `4px`/`2px` become `SP_8` / `SP_4`; its `4px` row and `2px` footer
//! gaps become `SP_4`).
//!
//! The spec's canonical trailing actions are built by the parent
//! (stateless kit): **Send** is `IconButton.round().primary()` with
//! `ArrowUp`, **Stop** is `IconButton.round().ghost()` with `Square`
//! (not red: it is the owner's own action).

use gpui::{
    div, prelude::*, AnyElement, App, ElementId, IntoElement, RenderOnce, SharedString, Window,
};

use crate::theme;
use crate::ui::chip::Chip;
use crate::ui::icon_button::IconButton;
use crate::ui::interaction::Common;

/// The part tag for the default `debug_selector`.
const COMPOSER_FRAME: &str = "composer-frame";

/// The kit's composer chrome. Build with [`ComposerFrame::new`] and chain
/// the slots; the parent owns the `Composer` entity and every button.
#[derive(IntoElement)]
pub struct ComposerFrame {
    common: Common,
    field: Option<AnyElement>,
    leading: Vec<AnyElement>,
    trailing: Vec<AnyElement>,
    footer: Vec<AnyElement>,
    footer_end: Vec<AnyElement>,
    actions_below_field: bool,
}

impl ComposerFrame {
    /// An empty composer frame (a `PANEL_SOLID` `RADIUS_XL` capsule at
    /// most `COMPOSER_MAX` wide).
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            common: Common {
                id: id.into(),
                ..Default::default()
            },
            field: None,
            leading: Vec::new(),
            trailing: Vec::new(),
            footer: Vec::new(),
            footer_end: Vec::new(),
            actions_below_field: false,
        }
    }

    /// The text field: the parent's rendered `Composer` element (or any
    /// other editor element — the frame is chrome only).
    pub fn field(mut self, field: impl IntoElement) -> Self {
        self.field = Some(field.into_any_element());
        self
    }

    /// Whether the frame holds a field.
    pub fn has_field(&self) -> bool {
        self.field.is_some()
    }

    /// Preserve an editor row when the caller's exact available width cannot
    /// fit the field and all action targets side by side.
    pub fn actions_below_field(mut self, below: bool) -> Self {
        self.actions_below_field = below;
        self
    }

    /// The leading actions (the mockup's attach button) — a `target()`
    /// hit each, bottom-aligned in the row.
    pub fn leading(mut self, buttons: Vec<IconButton>) -> Self {
        self.leading = buttons.into_iter().map(|b| b.into_any_element()).collect();
        self
    }

    /// The trailing actions (the model chip row, the talk button, the
    /// Send / Stop pair) — a `target()` hit each, bottom-aligned in the
    /// row.
    pub fn trailing(mut self, items: Vec<AnyElement>) -> Self {
        self.trailing = items;
        self
    }

    /// The footer items (chips, hints, text) under the
    /// field), left-aligned.
    pub fn footer<E: IntoElement>(mut self, items: Vec<E>) -> Self {
        self.footer = items.into_iter().map(|c| c.into_any_element()).collect();
        self
    }

    /// Footer chips at the footer's right end (the mockup's status chip,
    /// after its `flex: 1` spacer).
    pub fn footer_end<E: IntoElement>(mut self, items: Vec<E>) -> Self {
        self.footer_end = items.into_iter().map(|c| c.into_any_element()).collect();
        self
    }

    /// The `debug_selector` override (KIT.md § Conventions).
    pub fn selector(mut self, selector: &'static str) -> Self {
        self.common.selector = Some(selector);
        self
    }

    /// The accessible label (the tooltip).
    pub fn label(mut self, text: impl Into<SharedString>) -> Self {
        self.common.label = Some(text.into());
        self
    }
}

impl std::fmt::Debug for ComposerFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ComposerFrame({})",
            self.common.selector_for(COMPOSER_FRAME)
        )
    }
}

impl RenderOnce for ComposerFrame {
    fn render(mut self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let p = crate::theme::palette();
        let footer_len_even = self.footer.len().is_multiple_of(2);
        let common = self.common;
        let selector = common.selector_for(COMPOSER_FRAME);

        // The row: leading, the field (growing), trailing — bottom-
        // aligned (the mockup's `.crow` `align-items: flex-end`).
        let mut row = div()
            .id(crate::ui::interaction::child_id(&common.id, "row"))
            .debug_selector(|| format!("{selector}-row"))
            .flex()
            .flex_shrink(1.)
            .min_h(theme::target())
            .w_full()
            .items_end()
            .gap(theme::z(theme::SP_4));
        if self.actions_below_field {
            row = row
                .flex_col()
                .items_stretch()
                .min_h(theme::composer_stacked_row_min());
        } else {
            row = row.children(self.leading.drain(..));
        }
        if let Some(field) = self.field {
            // The field grows to fill the row (the mockup's
            // `.crow > .cbody { flex: 1 }`) — wrapped so the grow sits
            // on a `Div`, not on the parent's element.
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .self_stretch()
                    .min_w_0()
                    .min_h_0()
                    .when(self.actions_below_field, |d| d.min_h(theme::target()))
                    .child(field),
            );
        }
        if self.actions_below_field {
            row = row.child(
                div()
                    .debug_selector(|| format!("{selector}-actions"))
                    .flex()
                    .flex_none()
                    .flex_wrap()
                    .w_full()
                    .items_center()
                    .gap(theme::z(theme::SP_4))
                    .children(self.leading)
                    .children(self.trailing),
            );
        } else {
            row = row.children(self.trailing);
        }

        let mut root = div()
            .id(common.id.clone())
            .flex()
            .flex_col()
            .flex_shrink(1.)
            .min_h_0()
            .w_full()
            .max_w(theme::z(theme::COMPOSER_MAX))
            .gap(theme::z(theme::SP_4))
            .px(theme::z(theme::SP_8))
            .pt(theme::z(theme::SP_8))
            .pb(theme::z(theme::SP_4))
            .pl(theme::z(theme::SP_12))
            .rounded(theme::z(theme::RADIUS_XL))
            .bg(p.panel_solid)
            .border_1()
            .border_color(p.control_line)
            // A click on the floating composer never reaches what is behind.
            .occlude()
            .debug_selector(|| selector.clone())
            .child(row)
            .when(
                !self.footer.is_empty() || !self.footer_end.is_empty(),
                |d| {
                    d.child(
                        div()
                            .id(crate::ui::interaction::child_id(&common.id, "footer"))
                            .debug_selector(|| format!("{selector}-footer"))
                            .when(self.actions_below_field, |d| d.grid().grid_cols(2))
                            .when(!self.actions_below_field, |d| d.flex().flex_wrap())
                            .flex_none()
                            .w_full()
                            .items_center()
                            .gap(theme::z(theme::SP_4))
                            .px(theme::z(theme::SP_4))
                            .children(self.footer.drain(..))
                            .when(
                                self.actions_below_field
                                    && footer_len_even
                                    && !self.footer_end.is_empty(),
                                |d| d.child(div()),
                            )
                            .when(!self.actions_below_field, |d| d.child(div().flex_1()))
                            .children(self.footer_end.drain(..)),
                    )
                },
            );

        if let Some(tip) = common.tooltip() {
            root = root.tooltip(crate::icons::tip(tip.to_string()));
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::icons::Icon;
    use crate::ui::interaction::test_util::render_part;

    fn sample_frame() -> ComposerFrame {
        ComposerFrame::new("compose")
            .label("Message Awili")
            .field(div().h(theme::target()).min_w(theme::z(theme::EDGE)).flex())
            .leading(vec![IconButton::new("attach", Icon::Plus)
                .label("Attach")
                .quiet()])
            .trailing(vec![
                Chip::new("model", "Gemma · Mac Studio").into_any_element(),
                IconButton::new("send", Icon::ArrowUp)
                    .label("Send")
                    .round()
                    .primary()
                    .into_any_element(),
            ])
            .footer(vec![
                Chip::new("model", "Gemma · Mac Studio"),
                Chip::new("status", "All good"),
            ])
    }

    /// The frame lays out headless with every slot filled, and the hit
    /// row is at least `target()` tall (the buttons' floor).
    #[gpui::test]
    fn composer_frame_renders_its_slots(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || div().flex().child(sample_frame()));
        let frame = wcx
            .debug_bounds("ui-composer-frame-compose")
            .unwrap_or_else(|| panic!("the frame recorded no bounds"));
        assert!(
            f32::from(frame.size.width) > 0.0 && f32::from(frame.size.height) > 0.0,
            "the frame has size: {frame:?}"
        );
        let row = wcx
            .debug_bounds("ui-composer-frame-compose-row")
            .unwrap_or_else(|| panic!("the row recorded no bounds"));
        assert!(
            f32::from(row.size.height) >= f32::from(theme::target()),
            "the row is at least the target tall: {row:?}"
        );
        assert!(
            wcx.debug_bounds("ui-composer-frame-compose-footer")
                .is_some(),
            "the footer laid out"
        );
    }

    /// The width is clamped at `COMPOSER_MAX` (the mockup's `max-width:
    /// 720px`): in a wide window the frame never grows past it.
    #[gpui::test]
    fn composer_frame_clamps_at_the_composer_max(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || div().flex().child(sample_frame()));
        let frame = wcx
            .debug_bounds("ui-composer-frame-compose")
            .unwrap_or_else(|| panic!("the frame recorded no bounds"));
        assert!(
            f32::from(frame.size.width) <= f32::from(theme::z(theme::COMPOSER_MAX)) + 2.0,
            "the frame is at most COMPOSER_MAX wide: {frame:?}"
        );
    }

    /// K4b: the floating composer occludes — a click on it never reaches
    /// the button behind it.
    #[gpui::test]
    fn composer_frame_occludes(cx: &mut TestAppContext) {
        let hits = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let h = hits.clone();
        let (_root, wcx) = render_part(cx, move || {
            let h = h.clone();
            div()
                .relative()
                .size_full()
                .child(
                    div()
                        .id("behind")
                        .absolute()
                        .size_full()
                        .on_click(move |_e, _w, _c| h.set(h.get() + 1)),
                )
                .child(
                    div()
                        .absolute()
                        .child(ComposerFrame::new("cf").field(div().h(theme::target()))),
                )
        });
        let at = wcx.debug_bounds("ui-composer-frame-cf").unwrap().center();
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
        assert_eq!(hits.get(), 0);
    }
}
