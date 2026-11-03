//! `ui::Skeleton` — the kit's loading placeholder (KIT.md v3, batch K4).
//!
//! `RAISE`-coloured bars for the content that is loading: `.blocks(n)`
//! paints `n` rounded full-width blocks and `Skeleton::lines(n)` (or `.line_rows(n)`) paints `n`
//! thinner full-width text lines. Non-
//! interactive, purely presentational — the parent decides when to
//! swap the skeleton for the content. (The spec has no shimmer; the
//! bars are the `RAISE` fill, which on the `PANEL` face reads as the
//! app's quiet "something is coming" tone.)

use gpui::{div, prelude::*, App, ElementId, IntoElement, RenderOnce, Window};

use crate::theme::{self, palette};
use crate::ui::interaction::Common;

/// The part tag for the default `debug_selector`.
const SKELETON: &str = "skeleton";
/// A block bar's height, in logical px before zoom.
const BLOCK_H: f32 = theme::SKELETON_BLOCK_H;
/// A text line's height, in logical px before zoom.
const LINE_H: f32 = theme::SKELETON_LINE_H;

/// A kit loading placeholder. Build with [`Skeleton::new(id)`] and
/// chain `.blocks(n)` and `.line_rows(n)` (either or both), or start from `Skeleton::lines(n)`.
#[derive(IntoElement)]
pub struct Skeleton {
    common: Common,
    blocks: u32,
    lines: u32,
}

impl Skeleton {
    /// A skeleton with no bars yet (chain `.blocks(…)` / `.line_rows(…)`).
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            common: Common {
                id: id.into(),
                ..Default::default()
            },
            blocks: 0,
            lines: 0,
        }
    }

    /// The spec's constructor (KIT.md: `Skeleton::lines(n)`): `n` static
    /// line bars.
    pub fn lines(n: u32) -> Self {
        Self::new("skeleton").line_rows(n)
    }

    /// `n` block bars (full-width).
    pub fn blocks(mut self, n: u32) -> Self {
        self.blocks = n;
        self
    }

    /// The number of block bars.
    pub fn block_count(&self) -> u32 {
        self.blocks
    }

    /// `n` thinner text-line bars (full-width).
    pub fn line_rows(mut self, n: u32) -> Self {
        self.lines = n;
        self
    }

    /// The number of text-line bars.
    pub fn line_count(&self) -> u32 {
        self.lines
    }

    /// The `debug_selector` override (KIT.md § Conventions).
    pub fn selector(mut self, selector: &'static str) -> Self {
        self.common.selector = Some(selector);
        self
    }

    /// The accessible label.
    pub fn label(mut self, text: impl Into<gpui::SharedString>) -> Self {
        self.common.label = Some(text.into());
        self
    }
}

impl std::fmt::Debug for Skeleton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Skeleton({})", self.common.selector_for(SKELETON))
    }
}

impl RenderOnce for Skeleton {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let p = palette();
        let common = self.common;
        let selector = common.selector_for(SKELETON);

        let mut stack = div()
            .id(common.id.clone())
            .flex()
            .flex_col()
            .w_full()
            .gap(theme::z(theme::SP_8))
            .debug_selector(|| selector.clone());

        for i in 0..self.blocks {
            stack = stack.child(
                div()
                    .id(crate::ui::interaction::child_id(
                        &common.id,
                        &format!("block-{i}"),
                    ))
                    .debug_selector(|| format!("{selector}-block-{i}"))
                    .w_full()
                    .h(theme::z(BLOCK_H))
                    .rounded(theme::z(theme::RADIUS_S))
                    .bg(p.raise),
            );
        }
        for i in 0..self.lines {
            stack = stack.child(
                div()
                    .id(crate::ui::interaction::child_id(
                        &common.id,
                        &format!("line-{i}"),
                    ))
                    .debug_selector(|| format!("{selector}-line-{i}"))
                    .w_full()
                    .h(theme::z(LINE_H))
                    .rounded(theme::z(theme::RADIUS_S))
                    .bg(p.raise),
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

    /// The skeleton lays out headless with its bars: the blocks are
    /// full-width and thicker than the (full-width) lines.
    #[gpui::test]
    fn skeleton_renders_its_bars(cx: &mut TestAppContext) {
        let (_root, wcx) =
            render_part(cx, move || div().flex().child(Skeleton::lines(2).blocks(3)));
        let root = wcx
            .debug_bounds("ui-skeleton-skeleton")
            .unwrap_or_else(|| panic!("the skeleton recorded no bounds"));
        let first = wcx
            .debug_bounds("ui-skeleton-skeleton-block-0")
            .unwrap_or_else(|| panic!("the first block recorded no bounds"));
        let second = wcx
            .debug_bounds("ui-skeleton-skeleton-block-1")
            .unwrap_or_else(|| panic!("the second block recorded no bounds"));
        let line = wcx
            .debug_bounds("ui-skeleton-skeleton-line-0")
            .unwrap_or_else(|| panic!("the first line recorded no bounds"));
        for (name, b) in [("block-0", first), ("block-1", second), ("line-0", line)] {
            assert!(
                (f32::from(b.size.width) - f32::from(root.size.width)).abs() < 2.0,
                "{name} is full-width: {b:?} vs {root:?}"
            );
        }
        assert!(
            f32::from(line.size.height) < f32::from(first.size.height),
            "a line is thinner than a block: {line:?} vs {first:?}"
        );
    }
}
