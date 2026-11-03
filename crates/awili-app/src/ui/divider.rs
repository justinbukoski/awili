//! `ui::Divider` — a 1 px `LINE` rule (KIT.md v3, batch K1).
//!
//! Horizontal by default; `.vertical()` for the vertical rule. The hairline
//! is a real 1 px border (hairlines do not scale with interface zoom — the
//! same convention as every `border_1()` in the app).

use gpui::{div, prelude::*, App, IntoElement, RenderOnce, Window};

use crate::theme::palette;

/// A 1 px `LINE` divider.
#[derive(Clone, Copy, Debug, IntoElement)]
pub struct Divider {
    vertical: bool,
}

impl Divider {
    /// A horizontal 1 px `LINE` rule.
    pub fn new() -> Self {
        Self { vertical: false }
    }

    /// The vertical rule (1 px wide, filling the parent's height).
    pub fn vertical(mut self) -> Self {
        self.vertical = true;
        self
    }

    pub fn is_vertical(&self) -> bool {
        self.vertical
    }
}

impl Default for Divider {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for Divider {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let line = palette().line;
        if self.vertical {
            // A zero-size box with a 1 px left border paints a 1 px-wide
            // rule; `h_full()` stretches it to the parent's height (the
            // block's width is the border alone).
            div()
                .flex_none()
                .h_full()
                .border_l_1()
                .border_color(line)
                .debug_selector(|| "ui-divider-v".to_string())
        } else {
            // Same trick horizontally: 1 px tall, stretched full width by
            // the flex parent's default cross-axis stretch.
            div()
                .border_t_1()
                .border_color(line)
                .debug_selector(|| "ui-divider-h".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use gpui::TestAppContext;

    /// The divider paints the `LINE` token at 1 px, horizontally and
    /// vertically (pinned headless: the rule takes exactly its border's
    /// space in the axis it cuts).
    #[gpui::test]
    fn divider_is_a_one_pixel_line_in_both_orientations(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        cx.draw(
            gpui::point(gpui::Pixels::ZERO, gpui::Pixels::ZERO),
            gpui::size(
                gpui::AvailableSpace::Definite(theme::z(theme::PANEL_W)),
                gpui::AvailableSpace::Definite(theme::z(theme::EDGE)),
            ),
            |_w, _cx| {
                div()
                    .flex()
                    .flex_col()
                    .w(theme::z(theme::PANEL_W))
                    .child(Divider::new())
                    .child(
                        div()
                            .flex()
                            .h(theme::z(theme::EDGE))
                            .child(Divider::new().vertical()),
                    )
            },
        );
        let h = cx
            .debug_bounds("ui-divider-h")
            .expect("horizontal divider bounds");
        let v = cx
            .debug_bounds("ui-divider-v")
            .expect("vertical divider bounds");
        assert_eq!(
            f32::from(h.size.height),
            1.0,
            "the horizontal rule is exactly 1 px tall: {h:?}"
        );
        assert_eq!(
            f32::from(h.size.width),
            f32::from(theme::z(theme::PANEL_W)),
            "the horizontal rule stretches the parent's width: {h:?}"
        );
        assert_eq!(
            f32::from(v.size.width),
            1.0,
            "the vertical rule is exactly 1 px wide: {v:?}"
        );
        assert_eq!(
            f32::from(v.size.height),
            f32::from(theme::z(theme::EDGE)),
            "the vertical rule fills the parent's height: {v:?}"
        );
        assert!(!Divider::new().is_vertical());
        assert!(Divider::new().vertical().is_vertical());
    }
}
