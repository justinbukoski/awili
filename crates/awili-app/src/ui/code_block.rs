//! `ui::CodeBlock` — the kit's fenced code block (KIT.md v3, batch K4).
//!
//! The header is a `RAISE` strip: the language label (`type_::CAPTION`,
//! `TEXT_DIM`) at the left and a 44 px hit **Copy `IconButton.quiet`** at
//! the right. The body is the `code_bg` fill with mono `type_::BODY`
//! text; the whole block carries the `code_border` edge and `RADIUS_M`
//! corners. Copying is self-contained: the Copy button writes the code
//! to the clipboard (the parent needs no handler).

use gpui::{
    div, prelude::*, App, ClipboardItem, ElementId, IntoElement, RenderOnce, SharedString, Window,
};

use crate::icons::{self, Icon};
use crate::theme::{self, palette};
use crate::ui::interaction::Common;
use crate::ui::text;

/// The part tag for the default `debug_selector`.
const CODE_BLOCK: &str = "code-block";
/// The child-id tag for the Copy button.
const CODE_BLOCK_COPY: &str = "copy";

/// A kit fenced code block. Build with [`CodeBlock::new`] — the code is
/// the part's content; the language label is the header's text.
#[derive(IntoElement)]
pub struct CodeBlock {
    common: Common,
    lang: SharedString,
    code: SharedString,
}

impl CodeBlock {
    /// A code block with its language label and code.
    pub fn new(
        id: impl Into<ElementId>,
        lang: impl Into<SharedString>,
        code: impl Into<SharedString>,
    ) -> Self {
        let lang = lang.into();
        let code = code.into();
        Self {
            common: Common {
                id: id.into(),
                label: Some(format!("Code: {lang}").into()),
                ..Default::default()
            },
            lang,
            code,
        }
    }

    /// The header's language label.
    pub fn language(&self) -> &SharedString {
        &self.lang
    }

    /// The code text (what the Copy button copies).
    pub fn code_text(&self) -> &SharedString {
        &self.code
    }

    /// The `debug_selector` override (KIT.md § Conventions).
    pub fn selector(mut self, selector: &'static str) -> Self {
        self.common.selector = Some(selector);
        self
    }

    /// The accessible name (it defaults to "Code: `<lang>`").
    pub fn label(mut self, text: impl Into<SharedString>) -> Self {
        self.common.label = Some(text.into());
        self
    }
}

impl std::fmt::Debug for CodeBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CodeBlock({:?})", self.common.selector_for(CODE_BLOCK))
    }
}

impl RenderOnce for CodeBlock {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let p = palette();
        let common = self.common;
        let selector = common.selector_for(CODE_BLOCK);
        let lang = self.lang;
        let code = self.code;
        let code_text = code.clone();

        // The header: RAISE strip, the language label, the 44 px Copy
        // hit (the kit's double-click guard rides on the IconButton).
        let header = div()
            .id(crate::ui::interaction::child_id(&common.id, "header"))
            .debug_selector(|| format!("{selector}-header"))
            .flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap(theme::z(theme::SP_8))
            .px(theme::z(theme::SP_12))
            .py(theme::z(theme::SP_8))
            .bg(p.raise)
            .border_b_1()
            .border_color(p.code_border)
            .child(
                div().flex_1().min_w_0().child(
                    text::caption(lang.clone())
                        .dim()
                        .mono()
                        .id(crate::ui::interaction::child_id(&common.id, "lang")),
                ),
            )
            .child(
                crate::ui::icon_button::IconButton::new(
                    crate::ui::interaction::child_id(&common.id, CODE_BLOCK_COPY),
                    Icon::Copy,
                )
                .label("Copy")
                .quiet()
                .on_click(move |_ev, _window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(code_text.to_string()));
                }),
            );

        // The body: the code_bg fill, the mono code (the kit's long-text
        // rule: code wraps, nothing scrolls inside the block).
        let body = div()
            .id(crate::ui::interaction::child_id(&common.id, "body"))
            .debug_selector(|| format!("{selector}-body"))
            .flex()
            .w_full()
            .flex_col()
            .px(theme::z(theme::SP_12))
            .py(theme::z(theme::SP_8))
            .bg(p.code_bg)
            .child(text::body(code).mono().wrap());

        div()
            .id(common.id.clone())
            .flex()
            .flex_col()
            .w_full()
            .rounded(theme::z(theme::RADIUS_M))
            .overflow_hidden()
            .border_1()
            .border_color(p.code_border)
            .debug_selector(|| selector.clone())
            .child(header)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// The block lays out headless with the header (RAISE strip) and the
    /// mono body; the Copy button's hit box is at least `target()`.
    #[gpui::test]
    fn code_block_renders_header_and_body(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(CodeBlock::new(
                "rs",
                "rust",
                "fn main() {\n    println!(\"Awili\");\n}",
            ))
        });
        let block = wcx
            .debug_bounds("ui-code-block-rs")
            .unwrap_or_else(|| panic!("the block recorded no bounds"));
        assert!(
            f32::from(block.size.width) > 0.0 && f32::from(block.size.height) > 0.0,
            "the block has size: {block:?}"
        );
        assert!(
            wcx.debug_bounds("ui-code-block-rs-header").is_some(),
            "the header laid out"
        );
        assert!(
            wcx.debug_bounds("ui-code-block-rs-body").is_some(),
            "the body laid out"
        );
        let copy = wcx
            .debug_bounds("ui-icon-button-rs-copy")
            .unwrap_or_else(|| panic!("the copy button recorded no bounds"));
        assert!(
            f32::from(copy.size.width) >= f32::from(theme::target())
                && f32::from(copy.size.height) >= f32::from(theme::target()),
            "the copy hit box is at least the target: {copy:?}"
        );
    }

    /// Copying the code is self-contained: a click on the Copy button
    /// lands the exact code text on the clipboard.
    #[gpui::test]
    fn code_block_copy_writes_the_code_to_the_clipboard(cx: &mut TestAppContext) {
        let code = "let kit = awili::ui::CodeBlock::new(\"id\", \"rust\", \"\");";
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(CodeBlock::new("copyme", "rust", code))
        });
        let at = wcx
            .debug_bounds("ui-icon-button-copyme-copy")
            .unwrap_or_else(|| panic!("the copy button recorded no bounds"))
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
        assert_eq!(
            cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
            Some(code),
            "the copy button writes the exact code text"
        );
    }
}
