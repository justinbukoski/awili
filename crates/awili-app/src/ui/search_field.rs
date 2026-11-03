//! `ui::SearchField` — the kit's search input (KIT.md v3, batch K4).
//!
//! The `TextField` frame (`PANEL_SOLID`, `RADIUS_M`, the `CONTROL_LINE`
//! edge, the `CONTRAST` edge while focused, `target()` tall) with a
//! leading `Search` icon, the query (or the placeholder in `TEXT_DIM`), a
//! `CONTRAST` caret while focused, and a clear × (`IconButton.quiet`)
//! while the query is non-empty.
//!
//! **A real input (K4b).** The field is a tab stop (`focus_wiring`) and
//! edits the query the parent owns: typed characters append, Backspace
//! deletes, both reported through `.on_query(String)`; Backspace on an
//! **empty** query calls `.on_empty_backspace(…)` (the Palette's "back a
//! level"); command chords never type; the × reports an empty query. A
//! live query cell under the field's id makes several keys that arrive
//! before the parent re-renders all land. Other keys (arrows, Enter,
//! Escape) bubble to the parent — the Palette drives its list with them.
//! IME composition is the app editor's (`TextField`'s editor slot); this
//! field takes the typed characters a search needs.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{div, prelude::*, App, ElementId, IntoElement, RenderOnce, SharedString, Window};

use crate::icons::{self, Icon};
use crate::theme::{self, palette};
use crate::ui::focus_ring::focus_ring_style;
use crate::ui::icon_button::IconButton;
use crate::ui::interaction::{child_id, focus_wiring, Common};
use crate::ui::text;
use crate::ui::text_field::TextField;

/// The part tag for the default `debug_selector`.
const SEARCH_FIELD: &str = "search-field";

/// The query handler (the new query).
type QueryHandler = Rc<dyn Fn(String, &mut Window, &mut App)>;
/// The empty-Backspace handler.
type BackHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// A kit search input. Build with [`SearchField::new(id, placeholder)`]
/// and chain `.query(…)`, `.on_query(…)` and the shared conventions; the
/// parent owns the query.
#[derive(IntoElement)]
pub struct SearchField {
    common: Common,
    placeholder: SharedString,
    query: SharedString,
    on_query: Option<QueryHandler>,
    on_empty_backspace: Option<BackHandler>,
}

impl SearchField {
    /// A search field with its placeholder.
    pub fn new(id: impl Into<ElementId>, placeholder: impl Into<SharedString>) -> Self {
        let placeholder = placeholder.into();
        Self {
            common: Common {
                id: id.into(),
                label: Some(placeholder.clone()),
                ..Default::default()
            },
            placeholder,
            query: SharedString::default(),
            on_query: None,
            on_empty_backspace: None,
        }
    }

    /// The query (the parent's; the truth at each render).
    pub fn query(mut self, query: impl Into<SharedString>) -> Self {
        self.query = query.into();
        self
    }

    /// The query the part shows.
    pub fn query_text(&self) -> SharedString {
        self.query.clone()
    }

    /// Whether the query is empty (the × is hidden).
    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    /// The edit callback: the new query after a typed character, a
    /// Backspace or the clear ×.
    pub fn on_query(mut self, f: impl Fn(String, &mut Window, &mut App) + 'static) -> Self {
        self.on_query = Some(Rc::new(f));
        self
    }

    /// Backspace on an **empty** query (the Palette goes back a level).
    pub fn on_empty_backspace(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_empty_backspace = Some(Rc::new(f));
        self
    }
}

crate::kit_shared!(SearchField);

impl std::fmt::Debug for SearchField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SearchField({})", self.common.selector_for(SEARCH_FIELD))
    }
}

impl RenderOnce for SearchField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette();
        let mut common = self.common;
        // Without the parent's handle the field keeps its own, so the one
        // handle drives both the Tab wiring and the focused look (edge, caret).
        if common.focus_handle.is_none() && !common.pointer_only {
            let own = window
                .use_keyed_state(child_id(&common.id, "focus"), cx, |_, cx| cx.focus_handle())
                .read(cx)
                .clone();
            common.focus_handle = Some(own);
        }
        let selector = common.selector_for(SEARCH_FIELD);
        let disabled = common.is_disabled();
        let focused = !disabled
            && (common.focused
                || common
                    .focus_handle
                    .as_ref()
                    .is_some_and(|h| h.is_focused(window)));

        // The live query: the parent's `.query(…)` is the truth at each
        // render; keys edit this cell in place so several keys before a
        // re-render all land.
        let live = window
            .use_keyed_state(child_id(&common.id, "query"), cx, |_, _| {
                Rc::new(RefCell::new(String::new()))
            })
            .read(cx)
            .clone();
        *live.borrow_mut() = self.query.to_string();

        let caret = || {
            div()
                .flex_none()
                .debug_selector(|| "ui-search-caret".to_string())
                .w(theme::z(theme::CARET_W))
                .h(theme::z(theme::type_::BODY.size))
                .bg(p.contrast)
        };
        let shown = if self.query.is_empty() {
            text::body(self.placeholder.clone()).dim()
        } else {
            let t = text::body(self.query.clone()).text_color_override(if disabled {
                p.text_dim
            } else {
                p.text
            });
            if disabled {
                t
            } else {
                t.id(child_id(&common.id, "query-text"))
            }
        };
        let text_row = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .when(self.query.is_empty() && focused, |d| d.child(caret()))
            .child(shown)
            .when(!self.query.is_empty() && focused, |d| d.child(caret()));

        let mut frame = div()
            .flex()
            .w_full()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .px(theme::z(theme::SP_12))
            .min_h(theme::target())
            .rounded(theme::z(theme::RADIUS_M))
            .bg(p.panel_solid)
            .border_1()
            .border_color(TextField::edge(disabled, focused, common.hovered))
            .debug_selector(|| format!("{selector}-frame"))
            // Real pointer hover: the enabled, unfocused edge firms up.
            .when(!disabled && !focused, |d| {
                d.hover(|s| s.border_color(palette().text_dim))
            })
            .child(icons::icon(Icon::Search, theme::ICON, p.text_dim))
            .child(text_row);
        if !self.query.is_empty() {
            let mut clear = IconButton::new(child_id(&common.id, "clear"), Icon::X)
                .label("Clear the search")
                .quiet()
                .allow_double_click_if(common.allow_double_click);
            match (&common.disabled_reason, self.on_query.clone()) {
                (Some(reason), _) => clear = clear.disabled(reason.clone()),
                (None, Some(on_query)) => {
                    let live = live.clone();
                    clear = clear.on_click(move |_ev, window, cx| {
                        live.borrow_mut().clear();
                        on_query(String::new(), window, cx);
                    });
                }
                (None, None) => {}
            }
            frame = frame.child(clear);
        }

        let mut root = div()
            .id(common.id.clone())
            .flex()
            .w_full()
            .min_w(theme::target())
            .debug_selector(|| selector.clone())
            .child(frame);
        if !disabled {
            root = focus_wiring(root, &common)
                .when(!common.focused, |d| d.focus_visible(focus_ring_style));
            let on_query = self.on_query.clone();
            let on_back = self.on_empty_backspace.clone();
            root = root.on_key_down(move |ev, window, cx| {
                let stop = |window: &mut Window, cx: &mut App| {
                    window.prevent_default();
                    cx.stop_propagation();
                };
                let mods = &ev.keystroke.modifiers;
                match ev.keystroke.key.as_str() {
                    "backspace" => {
                        if live.borrow().is_empty() {
                            if let Some(on_back) = &on_back {
                                stop(window, cx);
                                on_back(window, cx);
                            }
                        } else if let Some(on_query) = &on_query {
                            stop(window, cx);
                            live.borrow_mut().pop();
                            let q = live.borrow().clone();
                            on_query(q, window, cx);
                        }
                    }
                    // Navigation keys are the parent's (they bubble).
                    "up" | "down" | "left" | "right" | "enter" | "escape" | "tab" | "pageup"
                    | "pagedown" | "home" | "end" => {}
                    _ => {
                        if mods.control || mods.platform || mods.function {
                            return;
                        }
                        let Some(typed) = ev.keystroke.key_char.clone() else {
                            return;
                        };
                        if typed.chars().any(char::is_control) {
                            return;
                        }
                        if let Some(on_query) = &on_query {
                            stop(window, cx);
                            live.borrow_mut().push_str(&typed);
                            let q = live.borrow().clone();
                            on_query(q, window, cx);
                        }
                    }
                }
            });
        }
        if let Some(tip) = common.tooltip() {
            root = root.tooltip(crate::icons::tip(tip.to_string()));
        }
        // The frozen ring (`.focused(true)`, the gallery): the outline-style
        // ring box, as every part draws it; the live ring stays `focus_visible`.
        crate::ui::focus_ring::focus_ring(root, common.focused && !disabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::{render_part, tooltip_at};

    /// With no handle from the parent, Tab still reaches the field and it
    /// looks focused: the caret paints (the field keeps its own handle).
    #[gpui::test]
    fn tab_focus_without_a_parent_handle_paints_the_caret(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, || SearchField::new("s", "Search…"));
        assert!(
            wcx.debug_bounds("ui-search-caret").is_none(),
            "no caret before focus"
        );
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.update(|w, _cx| w.refresh());
        wcx.run_until_parked();
        assert!(
            wcx.update(|w, cx| w.focused(cx)).is_some(),
            "Tab reached the field"
        );
        assert!(
            wcx.debug_bounds("ui-search-caret").is_some(),
            "the caret paints"
        );
    }

    /// The frozen `.focused(true)` paints the focus ring (the gallery's focused
    /// specimen); unfocused or disabled, no ring.
    #[gpui::test]
    fn frozen_focus_paints_the_ring(cx: &mut TestAppContext) {
        for (focused, disabled, lit) in [
            (true, false, true),
            (false, false, false),
            (true, true, false),
        ] {
            let (_root, wcx) = render_part(cx, move || {
                let f = SearchField::new("s", "Search…").focused(focused);
                if disabled {
                    f.disabled("Rebuilding")
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

    /// K4b (Codex/DeepSeek P1): a real input — typed characters append
    /// (several before a re-render all land), Backspace deletes, Backspace
    /// on empty goes back, chords never type, navigation keys bubble.
    #[gpui::test]
    fn search_field_edits_its_query(cx: &mut TestAppContext) {
        let query = Rc::new(RefCell::new(String::new()));
        let backs = Rc::new(std::cell::Cell::new(0u32));
        let bubbled = Rc::new(std::cell::Cell::new(0u32));
        let handle = cx.update(|cx| cx.focus_handle());
        let (q, b, bub, h) = (
            query.clone(),
            backs.clone(),
            bubbled.clone(),
            handle.clone(),
        );
        let (_root, wcx) = render_part(cx, move || {
            let (q2, b2, bub2) = (q.clone(), b.clone(), bub.clone());
            div()
                .flex()
                .on_key_down(move |ev, _w, _cx| {
                    if ev.keystroke.key == "down" {
                        bub2.set(bub2.get() + 1);
                    }
                })
                .child(
                    SearchField::new("s", "Search…")
                        .query(q.borrow().clone())
                        .focus_handle(&h)
                        .on_query(move |new, _w, _c| *q2.borrow_mut() = new)
                        .on_empty_backspace(move |_w, _c| b2.set(b2.get() + 1)),
                )
        });
        wcx.update(|w, cx| w.focus(&handle, cx));
        wcx.simulate_keystrokes("j o b");
        assert_eq!(query.borrow().as_str(), "job");
        wcx.simulate_keystrokes("backspace");
        assert_eq!(query.borrow().as_str(), "jo");
        wcx.simulate_keystrokes("cmd-a down");
        assert_eq!(query.borrow().as_str(), "jo", "a chord does not type");
        assert_eq!(bubbled.get(), 1, "Down bubbles to the parent");
        wcx.simulate_keystrokes("backspace backspace backspace");
        assert_eq!(
            (query.borrow().as_str(), backs.get()),
            ("", 1),
            "empty: back"
        );
    }

    /// The field is a tab stop (a plain tracked handle); the × clears;
    /// disabled, it is no tab stop and shows its reason.
    #[gpui::test]
    fn search_field_conventions(cx: &mut TestAppContext) {
        let query = Rc::new(RefCell::new(String::from("old")));
        let handle = cx.update(|cx| cx.focus_handle());
        let off = cx.update(|cx| cx.focus_handle());
        let (q, h, o) = (query.clone(), handle.clone(), off.clone());
        let (_root, wcx) = render_part(cx, move || {
            let q2 = q.clone();
            div()
                .flex()
                .flex_col()
                .pt(theme::z(theme::SP_40))
                .child(
                    SearchField::new("off", "Search…")
                        .focus_handle(&o)
                        .disabled("Index is rebuilding"),
                )
                .child(
                    SearchField::new("on", "Search…")
                        .query(q.borrow().clone())
                        .focus_handle(&h)
                        .on_query(move |new, _w, _c| *q2.borrow_mut() = new),
                )
        });
        // Tab skips the disabled field and lands on the live one (its ×
        // is the next stop).
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert_eq!(wcx.update(|w, cx| w.focused(cx)), Some(handle.clone()));
        let at = wcx
            .debug_bounds("ui-icon-button-on-clear")
            .unwrap()
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
        assert_eq!(query.borrow().as_str(), "", "the × clears");
        assert_eq!(
            tooltip_at(wcx, "ui-search-field-off").as_deref(),
            Some("Index is rebuilding")
        );
        let _ = off;
    }
}
