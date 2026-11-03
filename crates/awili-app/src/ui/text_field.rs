//! `ui::TextField` — the kit's text field frame (KIT.md v3, batch K4).
//!
//! The style and frame for the existing text editors: `PANEL_SOLID`,
//! `RADIUS_M`, the `CONTROL_LINE` edge (the `CONTRAST` edge while the
//! parent reports its editor `.focused(true)`), `type_::BODY` text, the
//! `target()` minimum height.
//!
//! **The editor slot (K4b).** Editing — caret, selection, IME, keys —
//! is the app's existing editor entity (the `Composer` pattern); the
//! parent passes it with `.editor(|reason| element)` (built with the
//! field's disabled reason, so a disabled field's editor goes read-only)
//! and the frame holds it.
//! Without an editor the frame shows `.value(…)` (or the placeholder) as
//! read-only text, its full text as the tooltip.
//!
//! **`.secret(has_value)`** masks a stored value with a fixed • run (a
//! secret never shows its value, not even its length) — and shows "Not
//! set" when nothing is stored — with two actions:
//!
//! - **Replace** fires `.on_replace(…)`; the parent clears its editor and
//!   re-renders with `.editing(true)`, and the frame then holds the editor
//!   instead of the mask.
//! - **Clear** removes the stored value **with a confirm step**: the first
//!   press arms it ("Click again to clear"), the second fires
//!   `.on_clear(…)`; the arming lapses after [`CLEAR_ARMED_FOR`].
//!
//! Disabled dims the frame's text, disables both actions with the reason
//! and makes the reason the tooltip.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    div, prelude::*, AnyElement, App, ClickEvent, ElementId, IntoElement, RenderOnce, Rgba,
    SharedString, Window,
};

use crate::theme::{self, palette};
use crate::ui::button::Button;
use crate::ui::interaction::{child_id, ClickHandler, Common};
use crate::ui::text;

/// The part tag for the default `debug_selector`.
const TEXT_FIELD: &str = "text-field";
/// The child-id tags for the secret's actions.
const TEXT_FIELD_REPLACE: &str = "replace";
const TEXT_FIELD_CLEAR: &str = "clear";
/// The secret's mask: a fixed-length • run (the saved value — and its
/// length — is never shown).
const SECRET_MASK: &str = "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}";
/// The secret's face when nothing is stored.
pub const NOT_SET: &str = "Not set";
/// The Clear action's armed face (its confirm step).
pub const CLEAR_ARMED: &str = "Click again to clear";
/// How long Clear stays armed waiting for its second press.
pub const CLEAR_ARMED_FOR: Duration = Duration::from_millis(theme::motion::CLEAR_ARMED_MS);

/// The editor builder: it receives the field's disabled reason.
type EditorBuilder = Box<dyn FnOnce(Option<SharedString>) -> AnyElement>;

/// A kit text field frame. Build with [`TextField::new(id,
/// placeholder)`] and chain `.editor(…)` or `.value(…)`, `.secret(…)`,
/// `.focused(…)` and the actions; the parent owns the text and the editor.
#[derive(IntoElement)]
pub struct TextField {
    common: Common,
    placeholder: SharedString,
    value: Option<SharedString>,
    editor: Option<EditorBuilder>,
    /// `Some(has_value)` for a secret field.
    secret: Option<bool>,
    editing: bool,
    on_replace: Option<ClickHandler>,
    on_clear: Option<Rc<ClickHandler>>,
}

impl TextField {
    /// The field frame's edge, shared with `SearchField`: `CONTRAST` while
    /// focused, `TEXT_DIM` while hovered (`hovered` is the frozen hover, the
    /// gallery; real pointer hover paints the same), else `CONTROL_LINE`. A
    /// disabled field keeps its `CONTROL_LINE` edge.
    pub fn edge(disabled: bool, focused: bool, hovered: bool) -> Rgba {
        let p = palette();
        match (disabled, focused, hovered) {
            (true, _, _) => p.control_line,
            (false, true, _) => p.contrast,
            (false, false, true) => p.text_dim,
            (false, false, false) => p.control_line,
        }
    }

    /// A text field with its placeholder (shown while the value is
    /// empty).
    pub fn new(id: impl Into<ElementId>, placeholder: impl Into<SharedString>) -> Self {
        let placeholder = placeholder.into();
        Self {
            common: Common {
                id: id.into(),
                label: Some(placeholder.clone()),
                ..Default::default()
            },
            placeholder,
            value: None,
            editor: None,
            secret: None,
            editing: false,
            on_replace: None,
            on_clear: None,
        }
    }

    /// The app's editor (the existing text-input entity's element) held
    /// in the frame: it does the editing. It is built with the field's
    /// disabled reason, so a disabled field's editor goes read-only (the
    /// frame cannot switch off another view's input from outside).
    pub fn editor<E: IntoElement + 'static>(
        mut self,
        build: impl FnOnce(Option<SharedString>) -> E + 'static,
    ) -> Self {
        self.editor = Some(Box::new(move |r| build(r).into_any_element()));
        self
    }

    /// The read-only value shown when there is no editor (empty shows
    /// the placeholder).
    pub fn value(mut self, value: impl Into<SharedString>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// The displayed value, if any.
    pub fn value_text(&self) -> Option<SharedString> {
        self.value.clone()
    }

    /// The secret face: `has_value` is whether a value is stored (the
    /// fixed • mask), else "Not set"; plus the Replace and Clear actions.
    pub fn secret(mut self, has_value: bool) -> Self {
        self.secret = Some(has_value);
        self
    }

    /// Whether the part paints the secret face.
    pub fn is_secret(&self) -> bool {
        self.secret.is_some()
    }

    /// A secret field being replaced: the frame holds the editor instead
    /// of the mask (the parent sets it after Replace).
    pub fn editing(mut self, editing: bool) -> Self {
        self.editing = editing;
        self
    }

    /// The text the field displays without an editor: the • mask for a
    /// stored secret, "Not set" for an empty one, else the value (or the
    /// placeholder while empty).
    pub fn secret_display(&self) -> SharedString {
        match self.secret {
            Some(true) => SECRET_MASK.into(),
            Some(false) => NOT_SET.into(),
            None => self
                .value
                .clone()
                .filter(|v| !v.as_str().is_empty())
                .unwrap_or_else(|| self.placeholder.clone()),
        }
    }

    /// The **Replace** action's handler (the parent clears its editor,
    /// re-renders with `.editing(true)` and focuses it).
    pub fn on_replace(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_replace = Some(Box::new(f));
        self
    }

    /// The **Clear** action's handler: fired by the **second** press (the
    /// first arms the confirm step).
    pub fn on_clear(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_clear = Some(Rc::new(Box::new(f)));
        self
    }
}

crate::kit_shared!(TextField);

impl std::fmt::Debug for TextField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TextField({})", self.common.selector_for(TEXT_FIELD))
    }
}

/// Clear's confirm step under the field's id: armed, and which arming.
#[derive(Default)]
struct ClearArm {
    armed: Cell<bool>,
    generation: Cell<u64>,
}

impl RenderOnce for TextField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette();
        let common = self.common;
        let selector = common.selector_for(TEXT_FIELD);
        let disabled = common.is_disabled();
        let text_color = if disabled { p.text_dim } else { p.text };
        let edge = Self::edge(disabled, common.focused, common.hovered);
        let display = match self.secret {
            Some(true) => SECRET_MASK.into(),
            Some(false) => SharedString::from(NOT_SET),
            None => self
                .value
                .clone()
                .filter(|v| !v.as_str().is_empty())
                .unwrap_or_else(|| self.placeholder.clone()),
        };
        let showing_placeholder =
            self.secret.is_none() && self.value.as_ref().is_none_or(|v| v.as_str().is_empty());

        let mut field = div()
            .id(child_id(&common.id, "field"))
            .flex()
            .w_full()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .px(theme::z(theme::SP_12))
            .min_h(theme::target())
            .rounded(theme::z(theme::RADIUS_M))
            .bg(p.panel_solid)
            .border_1()
            .border_color(edge)
            .debug_selector(|| format!("{selector}-frame"))
            // Real pointer hover: the enabled, unfocused edge firms up.
            .when(!disabled && !common.focused, |d| {
                d.hover(|s| s.border_color(palette().text_dim))
            });

        // The body: the editor (when held — always for a plain field with
        // an editor, and for a secret being replaced), else the read-only
        // text with its full text as the tooltip.
        let use_editor = self.editor.is_some() && (self.secret.is_none() || self.editing);
        if use_editor {
            if let Some(build) = self.editor {
                let editor = build(common.disabled_reason.clone());
                field = field.child(div().flex_1().min_w_0().child(editor));
            }
        } else {
            let mut t = text::body(display).text_color_override(if showing_placeholder {
                p.text_dim
            } else {
                text_color
            });
            if !disabled {
                t = t.id(child_id(&common.id, "value-text"));
            }
            field = field.child(div().flex_1().min_w_0().child(t));
        }

        if self.secret.is_some() && !self.editing {
            let reason = common.disabled_reason.clone();
            let mut replace = Button::new(child_id(&common.id, TEXT_FIELD_REPLACE), "Replace")
                .ghost()
                .allow_double_click_if(common.allow_double_click);
            let arm = window
                .use_keyed_state(child_id(&common.id, "clear-arm"), cx, |_, _| {
                    Rc::new(ClearArm::default())
                })
                .read(cx)
                .clone();
            let mut clear = Button::new(
                child_id(&common.id, TEXT_FIELD_CLEAR),
                if arm.armed.get() {
                    CLEAR_ARMED
                } else {
                    "Clear"
                },
            )
            .ghost()
            .allow_double_click_if(common.allow_double_click);
            match reason {
                Some(reason) => {
                    replace = replace.disabled(reason.clone());
                    clear = clear.disabled(reason);
                }
                None => {
                    let allow_double = common.allow_double_click;
                    if let Some(on_replace) = self.on_replace {
                        replace = replace.on_click(move |ev, window, cx| {
                            if allow_double || ev.click_count() <= 1 {
                                on_replace(ev, window, cx);
                            }
                        });
                    }
                    if self.secret == Some(false) {
                        clear = clear.disabled("Nothing is stored");
                    } else if let Some(on_clear) = self.on_clear {
                        clear = clear.on_click(move |ev, window, cx| {
                            if !allow_double && ev.click_count() > 1 {
                                return;
                            }
                            if arm.armed.get() {
                                arm.armed.set(false);
                                on_clear(ev, window, cx);
                            } else {
                                // The confirm step: arm, and lapse later.
                                let generation = arm.generation.get() + 1;
                                arm.generation.set(generation);
                                arm.armed.set(true);
                                let lapse = arm.clone();
                                window
                                    .spawn(cx, async move |cx| {
                                        cx.background_executor().timer(CLEAR_ARMED_FOR).await;
                                        if lapse.generation.get() == generation {
                                            lapse.armed.set(false);
                                            let _ = cx.update(|w, _| w.refresh());
                                        }
                                    })
                                    .detach();
                            }
                            window.refresh();
                        });
                    }
                }
            }
            field = field.child(replace).child(clear);
        }

        let mut root = div()
            .id(common.id.clone())
            .flex()
            .flex_col()
            .w_full()
            .min_w(theme::target())
            .debug_selector(|| selector.clone())
            .child(field);
        if let Some(tip) = common.tooltip() {
            root = root.tooltip(crate::icons::tip(tip.to_string()));
        }
        // The focus ring (DESIGN.md: every focused part) while the parent
        // reports its editor focused — with the `CONTRAST` edge above.
        crate::ui::focus_ring::focus_ring(root, !disabled && common.focused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::{render_part, tooltip_at};

    /// A focused field shows the focus ring (DESIGN.md) as well as its
    /// `CONTRAST` edge; unfocused or disabled, no ring.
    #[gpui::test]
    fn focused_field_shows_the_focus_ring(cx: &mut TestAppContext) {
        for (focused, disabled, lit) in [
            (true, false, true),
            (false, false, false),
            (true, true, false),
        ] {
            let (_root, wcx) = render_part(cx, move || {
                let f = TextField::new("f", "Name").value("Awili").focused(focused);
                if disabled {
                    f.disabled("Synced")
                } else {
                    f
                }
            });
            assert_eq!(
                wcx.debug_bounds("ui-focus-ring").is_some(),
                lit,
                "focused={focused} disabled={disabled}"
            );
        }
    }

    /// The frame edge: focus wins, the frozen hover firms the edge to
    /// `TEXT_DIM` exactly as pointer hover does, and a disabled field
    /// keeps its `CONTROL_LINE` edge whatever else is set.
    #[test]
    fn edge_shows_focus_then_hover_and_disabled_keeps_its_line() {
        let p = palette();
        assert_eq!(TextField::edge(false, false, false), p.control_line);
        assert_eq!(TextField::edge(false, false, true), p.text_dim);
        assert_eq!(TextField::edge(false, true, true), p.contrast);
        assert_eq!(TextField::edge(true, true, true), p.control_line);
    }

    fn click(wcx: &mut gpui::VisualTestContext, sel: &'static str) {
        let at = wcx
            .debug_bounds(sel)
            .unwrap_or_else(|| panic!("{sel} recorded no bounds"))
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
        wcx.run_until_parked();
    }

    /// The frame holds the parent's editor; without one it shows the
    /// value; a secret shows the mask only when something is stored.
    #[gpui::test]
    fn text_field_holds_the_editor_and_masks_secrets(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .child(
                    TextField::new("ed", "Name")
                        .editor(|_r| div().debug_selector(|| "the-editor".to_string()).child("x")),
                )
                .child(TextField::new("ro", "Name").value("Ada"))
                .child(TextField::new("key", "API key").secret(true))
        });
        assert!(
            wcx.debug_bounds("the-editor").is_some(),
            "the editor sits in the frame"
        );
        assert!(wcx.debug_bounds("ui-text-field-ed-frame").is_some());
        assert_eq!(
            TextField::new("s", "k")
                .secret(true)
                .secret_display()
                .as_str(),
            SECRET_MASK
        );
        assert_eq!(
            TextField::new("s", "k")
                .secret(false)
                .secret_display()
                .as_str(),
            NOT_SET
        );
        for sel in ["ui-button-key-replace", "ui-button-key-clear"] {
            let b = wcx.debug_bounds(sel).unwrap();
            assert!(f32::from(b.size.height) >= 44.0, "{sel}: {b:?}");
        }
    }

    /// Replace fires, the parent's `.editing(true)` swaps the mask for its
    /// editor (here the kit's real `SearchField` input), the parent
    /// focuses it, and typing then edits the new value.
    #[gpui::test]
    fn secret_replace_swaps_in_the_editor(cx: &mut TestAppContext) {
        let editing = Rc::new(Cell::new(false));
        let value = Rc::new(std::cell::RefCell::new(String::new()));
        let editor = cx.update(|cx| cx.focus_handle());
        let (e, v, h) = (editing.clone(), value.clone(), editor.clone());
        let (_root, wcx) = render_part(cx, move || {
            let (e2, v2, h2, h3) = (e.clone(), v.clone(), h.clone(), h.clone());
            let q = v.borrow().clone();
            div().flex().child(
                TextField::new("key", "API key")
                    .secret(true)
                    .editing(e.get())
                    .editor(move |r| {
                        crate::ui::search_field::SearchField::new("key-editor", "New key")
                            .query(q)
                            .focus_handle(&h2)
                            .disabled_if(r)
                            .on_query(move |new, _w, _c| *v2.borrow_mut() = new)
                    })
                    .on_replace(move |_e, w, cx| {
                        e2.set(true);
                        w.focus(&h3, cx);
                    }),
            )
        });
        assert!(
            wcx.debug_bounds("ui-search-field-key-editor").is_none(),
            "the mask first"
        );
        click(wcx, "ui-button-key-replace");
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        assert!(
            wcx.debug_bounds("ui-search-field-key-editor").is_some(),
            "the editor is in"
        );
        assert_eq!(wcx.update(|w, cx| w.focused(cx)), Some(editor), "focused");
        wcx.simulate_keystrokes("s k 1");
        assert_eq!(
            value.borrow().as_str(),
            "sk1",
            "typing edits the replacement value"
        );
    }

    /// K4b (Codex): a disabled field builds its editor with the reason
    /// (the editor goes read-only) — the reason reaches it.
    #[gpui::test]
    fn disabled_field_hands_its_reason_to_the_editor(cx: &mut TestAppContext) {
        let seen = Rc::new(std::cell::RefCell::new(None::<SharedString>));
        let s = seen.clone();
        let (_root, _wcx) = render_part(cx, move || {
            let s2 = s.clone();
            div().flex().child(
                TextField::new("ro", "Name")
                    .editor(move |r| {
                        *s2.borrow_mut() = r;
                        div().child("x")
                    })
                    .disabled("Synced from your profile"),
            )
        });
        assert_eq!(seen.borrow().as_deref(), Some("Synced from your profile"));
    }

    /// K4b (Codex P1): Clear has its confirm step — the first press arms
    /// ("Click again to clear"), the second fires; the arming lapses.
    #[gpui::test]
    fn secret_clear_confirms(cx: &mut TestAppContext) {
        let cleared = Rc::new(Cell::new(0u32));
        let c = cleared.clone();
        let (_root, wcx) = render_part(cx, move || {
            let c2 = c.clone();
            div().flex().child(
                TextField::new("key", "API key")
                    .secret(true)
                    .on_clear(move |_e, _w, _c| c2.set(c2.get() + 1)),
            )
        });
        click(wcx, "ui-button-key-clear");
        assert_eq!(cleared.get(), 0, "the first press only arms");
        assert_eq!(
            tooltip_at(wcx, "ui-button-key-clear").as_deref(),
            Some(CLEAR_ARMED),
            "the armed face"
        );
        click(wcx, "ui-button-key-clear");
        assert_eq!(cleared.get(), 1, "the second press clears");
        click(wcx, "ui-button-key-clear");
        wcx.executor()
            .advance_clock(CLEAR_ARMED_FOR + Duration::from_millis(50));
        wcx.run_until_parked();
        click(wcx, "ui-button-key-clear");
        assert_eq!(
            cleared.get(),
            1,
            "a lapsed arming needs its second press again"
        );
    }

    /// Disabled: both actions carry the reason (the tooltip that opens),
    /// and nothing fires.
    #[gpui::test]
    fn disabled_secret_is_inert(cx: &mut TestAppContext) {
        let fired = Rc::new(Cell::new(0u32));
        let f = fired.clone();
        let (_root, wcx) = render_part(cx, move || {
            let (f1, f2) = (f.clone(), f.clone());
            div().flex().pt(theme::z(theme::SP_40)).child(
                TextField::new("key", "API key")
                    .secret(true)
                    .on_replace(move |_e, _w, _c| f1.set(f1.get() + 1))
                    .on_clear(move |_e, _w, _c| f2.set(f2.get() + 1))
                    .disabled("Managed by your workspace"),
            )
        });
        click(wcx, "ui-button-key-replace");
        click(wcx, "ui-button-key-clear");
        click(wcx, "ui-button-key-clear");
        assert_eq!(fired.get(), 0);
        assert_eq!(
            tooltip_at(wcx, "ui-button-key-replace").as_deref(),
            Some("Managed by your workspace")
        );
    }
}
