//! `ui::Tabs` — the kit's tab strip (KIT.md v3, batch K2).
//!
//! A `RAISE` track, `RADIUS_M`, `SP_4` padding and gap (the mockup's 3 px
//! and 2 px, snapped to the spacing grid); each tab is a `target()`-tall
//! hit with `SP_12` horizontal padding and `RADIUS_S` corners. The
//! selected tab sits on `PANEL_SOLID` with a 2 px `CONTRAST` underline;
//! disabled tabs carry their reason (dim, inert, the reason is the
//! tooltip) — and a disabled **strip** disables every tab and every
//! close ×. Each tab paints its `Dot` (always — a disabled tab dims, it
//! never deletes) with the tab's label and a unique id under the tab's
//! id, and carries its own tooltip (the reason when disabled, the label
//! otherwise). Keyboard: the strip is the tab stop — the arrow keys (and
//! up/down) move the selection between **enabled** tabs (wrapping),
//! Enter/Space select the current tab (a no-op when it is disabled).
//! Item clicks, the close × and the + all honour the strip's
//! `.allow_double_click()`. `.on_new`
//! adds the trailing `+` `IconButton`.
//! Overflow: the strip scrolls horizontally — with a `.scroll_handle(…)`
//! the parent's handle tracks the overflow and the strip paints `RAISE`
//! edge fades while the content overflows.
//!
//! Token-grid adaptations of the mockup (noted in the report):
//! `.tabs{padding:3px;gap:2px}` becomes `SP_4`/`SP_4`, and the 38 px
//! `.tab` face maps to the kit's `target()` hit height (the convention
//! wins).

use std::sync::Arc;

use gpui::{canvas, div, px, quad, size};
use gpui::{
    prelude::*, App, BorderStyle, Bounds, ClickEvent, Edges, ElementId, Hsla, IntoElement,
    KeyDownEvent, KeyboardClickEvent, Keystroke, Pixels, RenderOnce, Rgba, ScrollHandle,
    SharedString, Window,
};

use crate::icons::{self, Icon};
use crate::theme::{self, palette, Tone};
use crate::ui::dot::Dot;
use crate::ui::focus_ring::{focus_ring, focus_ring_style};
use crate::ui::interaction::{child_id, focus_wiring, with_alpha, TAB, TABS, TAB_CLOSE, TAB_NEW};
use crate::ui::text;

/// The width of the scroll edge fade, logical px before zoom.
const FADE_W: f32 = theme::TABS_FADE_W;
/// The number of stepped alpha quads that make up the edge fade (GPUI has
/// no gradient primitive; the steps are imperceptible at 6 px).
const FADE_STEPS: usize = theme::TABS_FADE_STEPS;

/// A single tab (KIT.md § Tabs: each item has an id, a label, an optional
/// `Dot`, an optional close × and an optional `.disabled(reason)`).
#[derive(Clone)]
pub struct TabItem {
    id: ElementId,
    label: SharedString,
    dot: Option<Tone>,
    closable: Option<CloseHandler>,
    close_focus: Option<gpui::FocusHandle>,
    close_bounds: Option<std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>>,
    disabled_reason: Option<SharedString>,
}

impl std::fmt::Debug for TabItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TabItem")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("disabled", &self.is_disabled())
            .finish()
    }
}

impl TabItem {
    /// A tab with the given id (it feeds the close ×'s `ElementId`) and
    /// label.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            dot: None,
            closable: None,
            close_focus: None,
            close_bounds: None,
            disabled_reason: None,
        }
    }

    /// A `Dot` at the tab's left (a status marker).
    /// Let the caller retain the close control's keyboard identity.
    /// Capture the actual close face without its decorative ring.
    pub fn close_bounds_handle(
        mut self,
        bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    ) -> Self {
        self.close_bounds = Some(bounds);
        self
    }

    pub fn close_focus_handle(mut self, handle: &gpui::FocusHandle) -> Self {
        self.close_focus = Some(handle.clone());
        self
    }

    pub fn dot(mut self, tone: Tone) -> Self {
        self.dot = Some(tone);
        self
    }

    /// Make the tab closable: a trailing × whose click fires `on_close`
    /// (and never the strip's `on_select`).
    pub fn closable(
        mut self,
        on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.closable = Some(Arc::new(on_close));
        self
    }

    /// Disable the tab with its reason (dim, inert, the reason is the
    /// tooltip; arrow keys skip it).
    pub fn disabled(mut self, reason: impl Into<SharedString>) -> Self {
        self.disabled_reason = Some(reason.into());
        self
    }

    /// The tab's label.
    pub fn label_text(&self) -> &SharedString {
        &self.label
    }

    /// Whether the tab is disabled (carries a reason).
    pub fn is_disabled(&self) -> bool {
        self.disabled_reason.is_some()
    }

    /// The disabled reason (the tooltip), if any.
    pub fn disabled_reason(&self) -> Option<SharedString> {
        self.disabled_reason.clone()
    }

    /// Whether the tab shows its close ×.
    pub fn is_closable(&self) -> bool {
        self.closable.is_some()
    }

    /// The tab's `ElementId` (the parent uses it for its own nested ids).
    pub fn id(&self) -> &ElementId {
        &self.id
    }

    /// The tab dot's label (the tab's label — the dot sits in the tab,
    /// next to the text it marks), when the tab carries a dot.
    pub fn dot_label(&self) -> Option<SharedString> {
        self.dot.map(|_| self.label.clone())
    }

    /// The tab dot's `ElementId` — a named child of the tab's id — when
    /// the tab carries a dot.
    pub fn dot_id(&self) -> Option<ElementId> {
        self.dot.map(|_| child_id(self.id(), "dot"))
    }
}

/// A kit tab strip. Build with [`Tabs::new`] (the spec's signature: an
/// id, the items and the selected index) and chain `.on_select(…)`,
/// `.on_new(…)` and the shared conventions; the parent owns the selected
/// index and re-renders the strip.
#[derive(IntoElement)]
pub struct Tabs {
    common: crate::ui::interaction::Common,
    items: Vec<TabItem>,
    selected: usize,
    on_select: Option<SelectHandler>,
    on_new: Option<CloseHandler>,
    new_focus: Option<gpui::FocusHandle>,
    new_bounds: Option<std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>>,
    scroll: Option<ScrollHandle>,
}

/// A tab's dot as painted: labelled with the tab; enabled it has its own id
/// (its tooltip); disabled it dims and has none, so its tooltip never covers
/// the reason (disabled dims, never deletes).
pub(crate) fn tab_dot(
    tone: Option<Tone>,
    label: &SharedString,
    item_id: &ElementId,
    disabled: bool,
) -> Option<Dot> {
    let dot = Dot::new(tone?).label(label.clone());
    Some(if disabled {
        dot.dimmed()
    } else {
        dot.id(child_id(item_id, "dot"))
    })
}

impl Tabs {
    /// A tab strip with the given items and selected index.
    pub fn new(id: impl Into<ElementId>, items: Vec<TabItem>, selected: usize) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            items,
            selected,
            on_select: None,
            on_new: None,
            new_focus: None,
            new_bounds: None,
            scroll: None,
        }
    }

    /// The selection callback: the new index, and the click that chose it
    /// (a keyboard `ClickEvent` for the arrow keys / Enter / Space).
    pub fn on_select(
        mut self,
        f: impl Fn(usize, &ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Arc::new(f));
        self
    }

    /// The trailing `+` button (an `IconButton` with its own `ElementId`
    /// under the strip's id).
    /// Let the caller retain the New control's keyboard identity.
    /// Capture the actual New face without its decorative ring.
    pub fn new_bounds_handle(
        mut self,
        bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    ) -> Self {
        self.new_bounds = Some(bounds);
        self
    }

    pub fn new_focus_handle(mut self, handle: &gpui::FocusHandle) -> Self {
        self.new_focus = Some(handle.clone());
        self
    }

    pub fn on_new(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_new = Some(Arc::new(f));
        self
    }

    /// Whether the strip shows its `+` button.
    pub fn has_new(&self) -> bool {
        self.on_new.is_some()
    }

    /// The scroll handle of the strip's horizontal overflow (the parent
    /// owns it; with it the strip paints the `RAISE` edge fades).
    pub fn scroll_handle(mut self, handle: ScrollHandle) -> Self {
        self.scroll = Some(handle);
        self
    }

    /// The selected index.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The number of tabs.
    pub fn tab_count(&self) -> usize {
        self.items.len()
    }

    /// The next (or previous, `dir = -1`) **enabled** index from `from`,
    /// wrapping; `None` when there is no enabled tab.
    pub fn step(&self, from: usize, dir: isize) -> Option<usize> {
        step_index(&self.items, from, dir)
    }

    /// The edge fades: (show left fade, show right fade) for a scroll
    /// offset within `[0, max]` (GPUI's horizontal offsets are negative
    /// while scrolled right).
    pub fn edge_fades(offset: f32, max: f32) -> (bool, bool) {
        (offset < 0.0, offset > -max)
    }

    /// The tooltip of the tab at `index` (the per-tab tooltip the strip
    /// paints): the tab's disabled reason when the tab is disabled, the
    /// strip's reason when the strip is disabled (it disables every
    /// tab), else the tab's label.
    pub fn tab_tooltip(&self, index: usize) -> Option<SharedString> {
        let item = self.items.get(index)?;
        tab_tooltip_of(item, &self.common.disabled_reason)
    }
}

/// The tooltip of a tab: the tab's disabled reason, the strip's disabled
/// reason (a disabled strip disables every tab), else the tab's label.
fn tab_tooltip_of(item: &TabItem, strip_reason: &Option<SharedString>) -> Option<SharedString> {
    if let Some(reason) = item.disabled_reason.clone() {
        return Some(reason);
    }
    if let Some(reason) = strip_reason.clone() {
        return Some(reason);
    }
    Some(item.label.clone())
}

/// The selection walk: the next (or previous, `dir = -1`) enabled index
/// from `from`, wrapping; `None` when there is no enabled tab.
fn step_index(items: &[TabItem], from: usize, dir: isize) -> Option<usize> {
    let n = items.len();
    if n == 0 {
        return None;
    }
    let mut i = from;
    for _ in 0..n {
        i = (i as isize + dir).rem_euclid(n as isize) as usize;
        if !items[i].is_disabled() {
            return Some(i);
        }
    }
    None
}

/// A tab's close handler (cloned into the strip so every × works).
type CloseHandler = Arc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// The strip's selection handler (cloned into the tab listeners).
type SelectHandler = Arc<dyn Fn(usize, &ClickEvent, &mut Window, &mut App)>;

/// The strip's "new tab" (+) handler.
type NewHandler = Arc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

crate::kit_shared!(Tabs);

impl std::fmt::Debug for Tabs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tabs({})", self.common.selector_for(TABS))
    }
}

impl RenderOnce for Tabs {
    fn render(self, _window: &mut Window, _cx: &mut gpui::App) -> impl IntoElement {
        let p = palette();
        let common = self.common;
        let selected = self.selected;
        let on_select = self.on_select;
        let on_new = self.on_new;
        let scroll = self.scroll;
        let disabled = common.is_disabled();

        let allow_double = common.allow_double_click;
        // The strip's disabled reason (it disables every tab, and their
        // tooltips carry it while the items carry their own reasons).
        let strip_reason = common.disabled_reason.clone();

        let track_id = child_id(&common.id, "track");
        let mut track = div()
            .id(track_id)
            .flex()
            .items_center()
            .gap(theme::z(theme::SP_4))
            .p(theme::z(theme::SP_4))
            .rounded(theme::z(theme::RADIUS_M))
            .bg(p.raise)
            // The track fills the strip's width and may shrink below its
            // content, so an overflowing strip actually scrolls (the
            // parent's `track_scroll` handle drives it and the edge fades
            // follow the real offset — KIT.md § Tabs: overflow).
            .flex_grow(1.)
            .min_w_0()
            .overflow_x_scroll();
        // The parent's scroll handle drives the overflow (and the edge
        // fades read its offset while the strip paints).
        if let Some(scroll) = scroll.clone() {
            track = track.track_scroll(&scroll);
        }

        for (i, item) in self.items.iter().enumerate() {
            let is_selected = i == selected;
            // A disabled strip disables every item (and every close ×).
            let item_disabled = item.is_disabled() || disabled;
            let text_color = if item_disabled {
                p.text_dim
            } else if is_selected {
                p.text
            } else {
                p.text_dim
            };

            let id = item.id.clone();
            let closable = item.closable.clone();
            let dot = item.dot;
            // The reason a disabled tab (or a tab of a disabled strip)
            // shows — on the tab and on its dimmed close ×.
            let reason = item
                .disabled_reason
                .clone()
                .or_else(|| strip_reason.clone());

            let mut tab = div()
                .id(item.id.clone())
                // The tab's default `debug_selector`: `ui-tab-<id>`
                // (KIT.md § Conventions).
                .debug_selector(|| format!("ui-{TAB}-{}", item.id))
                .flex()
                .items_center()
                // A tab never squishes: a crowded strip scrolls (the
                // track's `overflow_x_scroll` + the parent's handle)
                // instead of compressing the labels below their `target()`
                // hit size.
                .flex_shrink_0()
                .gap(theme::z(theme::SP_8))
                .px(theme::z(theme::SP_12))
                .h(theme::target())
                .min_w(theme::target())
                .max_w(theme::z(theme::TAB_MAX_W))
                .rounded(theme::z(theme::RADIUS_S))
                .bg(if is_selected && !item_disabled {
                    p.panel_solid
                } else {
                    p.transparent
                })
                .when(is_selected && !item_disabled, |d| {
                    d.border_b_2().border_color(p.contrast)
                })
                // The dot is always painted (a disabled tab dims, it never
                // deletes) and carries the tab's label. An enabled tab's
                // dot also gets a unique id (with both, it shows the label
                // as its tooltip); a disabled tab's dot gets none, so its
                // tooltip never covers the disabled reason.
                .when_some(
                    tab_dot(dot, &item.label, &item.id, item_disabled),
                    |d, dot| d.child(dot),
                )
                .child(text::label(item.label.clone()).text_color_override(text_color))
                .when_some(closable, |d, on_close| {
                    // Disabled dims, it never deletes: a disabled tab keeps
                    // its ×, dimmed, carrying the reason.
                    let mut close = crate::ui::icon_button::IconButton::new(
                        child_id(&id, &format!("{TAB_CLOSE}-btn")),
                        Icon::X,
                    )
                    .label("Close tab")
                    .quiet()
                    .allow_double_click_if(allow_double)
                    .when_some(item.close_focus.as_ref(), |button, handle| {
                        button.focus_handle(handle)
                    });
                    if item_disabled {
                        close = close.disabled(
                            reason
                                .clone()
                                .unwrap_or_else(|| SharedString::from("Disabled")),
                        );
                    } else {
                        close = close.on_click(move |ev, w, cx| {
                            if allow_double || ev.click_count() <= 1 {
                                on_close(ev, w, cx);
                            }
                        });
                    }
                    d.child(
                        div()
                            .when_some(item.close_bounds.clone(), |d, bounds| {
                                d.on_children_prepainted(move |children, _, _| {
                                    bounds.set(
                                        children
                                            .first()
                                            .map(|b| b.inset(theme::focus_ring_overhang())),
                                    );
                                })
                            })
                            .id(child_id(&id, TAB_CLOSE))
                            .on_click(|_ev, window, cx| {
                                // The × carries its own click (the
                                // IconButton above); this listener only
                                // keeps the close from also selecting the
                                // tab.
                                window.prevent_default();
                                cx.stop_propagation();
                            })
                            .child(close),
                    )
                });

            // The tab's tooltip: the disabled reason when the tab (or the
            // strip) is disabled, the tab's label otherwise.
            if let Some(tip) = tab_tooltip_of(item, &strip_reason) {
                tab = tab.tooltip(icons::tip(tip.to_string()));
            }
            if !item_disabled {
                let on_select_tab = on_select.clone();
                // The kit's double-click guard applies (`.allow_double_click()`
                // on the strip lets the second click through).
                tab = tab.on_click(move |ev, window, cx| {
                    if allow_double || ev.click_count() <= 1 {
                        if let Some(on_select) = &on_select_tab {
                            on_select(i, ev, window, cx);
                        }
                    }
                });
            }
            track = track.child(tab);
        }

        if let Some(on_new) = on_new.clone() {
            // A disabled strip keeps its +, dimmed, with the strip's
            // reason (disabled dims, it never deletes).
            let mut new_tab =
                crate::ui::icon_button::IconButton::new(child_id(&common.id, TAB_NEW), Icon::Plus)
                    .label("New tab")
                    .quiet()
                    .allow_double_click_if(allow_double)
                    .when_some(self.new_focus.as_ref(), |button, handle| {
                        button.focus_handle(handle)
                    });
            if let Some(reason) = strip_reason.clone() {
                new_tab = new_tab.disabled(reason);
            } else {
                new_tab = new_tab.on_click(move |ev, w, cx| {
                    if allow_double || ev.click_count() <= 1 {
                        on_new(ev, w, cx);
                    }
                });
            }
            track = if let Some(bounds) = self.new_bounds {
                track.child(
                    div()
                        .flex_none()
                        .on_children_prepainted(move |children, _, _| {
                            bounds.set(
                                children
                                    .first()
                                    .map(|b| b.inset(theme::focus_ring_overhang())),
                            );
                        })
                        .child(new_tab),
                )
            } else {
                track.child(new_tab)
            };
        }

        let selector = common.selector_for(TABS);
        let focused = common.frozen_focus();

        let mut root = div()
            .id(common.id.clone())
            .flex()
            .relative()
            // The strip's width: `w_full()` of the ring wrapper's content
            // box. When the strip's parent fixes the wrapper's width (the
            // wrapper stretches in a column, or the parent is a block),
            // the strip fills it and an overflowing track scrolls inside
            // (KIT.md § Tabs: overflow); otherwise the strip hugs its
            // content, like every other kit part. `min_w_0()` lets the
            // wrapper shrink the strip below the tabs' natural width.
            // (The K1 ring wrapper is the part's outer element, so it is
            // the element the strip's parent actually lays out.)
            .w_full()
            .min_w_0()
            .debug_selector(|| selector.clone())
            .child(track);

        // The edge fades (only with a scroll handle: the fades read the
        // offset from it while the strip paints).
        if let Some(scroll) = scroll.clone() {
            let fade_color = p.raise;
            let fade_w = f32::from(theme::z(FADE_W));
            root = root.child(
                canvas(|_bounds, _window, _cx| (), {
                    let scroll = scroll.clone();
                    move |bounds, (), window, _cx| {
                        let offset = f32::from(scroll.offset().x);
                        let max = f32::from(scroll.max_offset().x);
                        let (left, right) = Tabs::edge_fades(offset, max);
                        if !left && !right {
                            return;
                        }
                        let step_w = fade_w / FADE_STEPS as f32;
                        if left {
                            for step in 0..FADE_STEPS {
                                // The inner edge (right of the strip) is
                                // opaque, fading out to the left.
                                let alpha = (step as f32 + 1.0) / FADE_STEPS as f32;
                                let x = bounds.origin.x + px(step as f32 * step_w);
                                window.paint_quad(quad(
                                    Bounds::new(
                                        gpui::point(x, bounds.origin.y),
                                        size(px(step_w), bounds.size.height),
                                    ),
                                    0.,
                                    with_alpha(fade_color, alpha),
                                    Edges::default(),
                                    Hsla::transparent_black(),
                                    BorderStyle::default(),
                                ));
                            }
                        }
                        if right {
                            for step in 0..FADE_STEPS {
                                // The inner edge (left of the strip) is
                                // opaque, fading out to the right.
                                let alpha = 1.0 - (step as f32 + 1.0) / FADE_STEPS as f32;
                                let x = bounds.origin.x + bounds.size.width
                                    - px((step as f32 + 1.0) * step_w);
                                window.paint_quad(quad(
                                    Bounds::new(
                                        gpui::point(x, bounds.origin.y),
                                        size(px(step_w), bounds.size.height),
                                    ),
                                    0.,
                                    with_alpha(fade_color, alpha),
                                    Edges::default(),
                                    Hsla::transparent_black(),
                                    BorderStyle::default(),
                                ));
                            }
                        }
                    }
                })
                .absolute()
                .top(Pixels::ZERO)
                .left(Pixels::ZERO)
                .size_full(),
            );
        }

        if !disabled {
            root = focus_wiring(root, &common)
                .when(!common.focused, |d| d.focus_visible(focus_ring_style));
            let on_select_keys = on_select.clone();
            let items = self.items.clone();

            root = root.on_key_down(move |ev: &KeyDownEvent, window, cx| {
                let dir = match ev.keystroke.key.as_str() {
                    "left" | "up" => -1,
                    "right" | "down" => 1,
                    _ => return,
                };
                if let Some(next) = step_index(&items, selected, dir) {
                    if let Some(on_select) = &on_select_keys {
                        window.prevent_default();
                        cx.stop_propagation();
                        on_select(
                            next,
                            &ClickEvent::Keyboard(KeyboardClickEvent::default()),
                            window,
                            cx,
                        );
                    }
                }
            });
            let on_select_click = on_select.clone();
            // Whether the currently selected tab is disabled (the strip's
            // disabled state disables every tab).
            let selected_disabled = self
                .items
                .get(selected)
                .map(|it| it.is_disabled())
                .unwrap_or(true);
            root = root.on_click(move |ev, window, cx| {
                // The keyboard click synthesis (Enter/Space on the focused
                // strip) selects the current tab — unless the selected tab
                // is disabled (a disabled selection is inert, like a
                // disabled part anywhere in the kit); a mouse click on the
                // track's background does nothing.
                if ev.is_keyboard() && !selected_disabled {
                    if let Some(on_select) = &on_select_click {
                        on_select(selected, ev, window, cx);
                    }
                }
            });
        }
        if let Some(tip) = common.tooltip() {
            root = root.tooltip(icons::tip(tip.to_string()));
        }

        focus_ring(root, focused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// The strip's double-click setting reaches its close × (and its +): with
    /// `.allow_double_click()` a double click closes twice; without, once.
    #[gpui::test]
    fn tab_close_follows_the_double_click_setting(cx: &mut TestAppContext) {
        for allow in [false, true] {
            let closed = std::rc::Rc::new(std::cell::Cell::new(0u32));
            let c = closed.clone();
            let (_root, wcx) = render_part(cx, move || {
                let c = c.clone();
                Tabs::new(
                    "t",
                    vec![
                        TabItem::new("a", "Answer").closable(move |_e, _w, _cx| c.set(c.get() + 1))
                    ],
                    0,
                )
                .allow_double_click_if(allow)
            });
            let at = wcx
                .debug_bounds("ui-icon-button-a-tab-close-btn")
                .unwrap()
                .center();
            for count in [1usize, 2] {
                wcx.simulate_event(gpui::MouseDownEvent {
                    button: gpui::MouseButton::Left,
                    position: at,
                    modifiers: gpui::Modifiers::default(),
                    click_count: count,
                    first_mouse: false,
                });
                wcx.simulate_event(gpui::MouseUpEvent {
                    button: gpui::MouseButton::Left,
                    position: at,
                    modifiers: gpui::Modifiers::default(),
                    click_count: count,
                });
            }
            assert_eq!(closed.get(), if allow { 2 } else { 1 }, "allow={allow}");
        }
    }

    /// A disabled tab's dot dims and has no id (no tooltip over the reason);
    /// an enabled tab's keeps its tone and its own id.
    #[test]
    fn disabled_tab_dot_dims_and_has_no_id() {
        let label = SharedString::from("Sources");
        let id = ElementId::from("t");
        let on = tab_dot(Some(Tone::Attention), &label, &id, false).unwrap();
        assert!(on.has_id() && !on.is_dimmed());
        assert_eq!(on.color(), theme::tone_color(Tone::Attention));
        let off = tab_dot(Some(Tone::Attention), &label, &id, true).unwrap();
        assert!(!off.has_id() && off.is_dimmed());
        assert_eq!(off.color(), theme::palette().text_dim);
        assert!(tab_dot(None, &label, &id, false).is_none());
    }

    fn sample_items() -> Vec<TabItem> {
        vec![
            TabItem::new("t0", "Main").dot(Tone::Neutral),
            TabItem::new("t1", "Fishing").closable(|_e, _w, _c| {}),
            TabItem::new("t2", "Locked").disabled("Read only"),
        ]
    }

    /// The hit box: the track is at least a `target()`-tall tab plus the
    /// track padding, at every tested zoom.
    #[gpui::test]
    fn tabs_hit_box_is_at_least_target_at_every_zoom(cx: &mut TestAppContext) {
        for scale in [0.5_f32, 1.0, 1.25] {
            theme::set_ui_scale(scale);
            let (_root, wcx) = render_part(cx, move || {
                div().flex().child(Tabs::new("tabs", sample_items(), 0))
            });
            let b = wcx
                .debug_bounds("ui-tabs-tabs")
                .unwrap_or_else(|| panic!("the strip recorded no bounds at zoom {scale}"));
            assert!(
                f32::from(b.size.height)
                    >= f32::from(theme::target() + 2.0 * theme::z(theme::SP_4)),
                "the track is ≥ a target-tall tab at zoom {scale}: {b:?}",
            );
        }
        theme::set_ui_scale(1.0);
    }
    /// The selection walk: the arrow keys skip disabled tabs and wrap.
    #[test]
    fn tabs_step_skips_disabled_and_wraps() {
        let tabs = Tabs::new("s", sample_items(), 0);
        // t0 Main (enabled), t1 Fishing (enabled), t2 Locked (disabled):
        // steps land on enabled tabs only and wrap around the ring.
        assert_eq!(tabs.step(0, 1), Some(1), "right lands on t1");
        assert_eq!(
            tabs.step(1, 1),
            Some(0),
            "right wraps past the disabled t2 to t0"
        );
        assert_eq!(
            tabs.step(0, -1),
            Some(1),
            "left wraps past the disabled t2 to t1"
        );
        assert_eq!(tabs.step(1, -1), Some(0), "left lands on t0");
        let all_disabled = Tabs::new("s", vec![TabItem::new("a", "A").disabled("off")], 0);
        assert_eq!(all_disabled.step(0, 1), None, "no enabled tab, no step");
        assert_eq!(
            Tabs::new("s", vec![], 0).step(0, 1),
            None,
            "no tabs, no step"
        );
    }

    /// The arrow keys move the selection (the parent's `on_select`), and
    /// Enter selects the current tab (the keyboard click synthesis).
    #[gpui::test]
    fn tabs_arrow_keys_move_the_selection(cx: &mut TestAppContext) {
        // The strip is the tab stop (the parent hands it its focus handle),
        // the arrow keys move the selection through the enabled tabs only,
        // and Enter/Space select through the keyboard click.
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        // The parent owns the selection (the part is stateless): the
        // factory re-builds the strip with it every frame, and each
        // selection re-renders the parent.
        let sel = std::rc::Rc::new(std::cell::RefCell::new(0usize));
        let chosen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sel_in = sel.clone();
        let chosen_in = chosen.clone();
        let (_root, wcx) = render_part(cx, move || {
            let sel_in = sel_in.clone();
            let chosen_in = chosen_in.clone();
            let selected = *sel_in.borrow();
            div().flex().child(
                Tabs::new("kbd", sample_items(), selected)
                    .focus_handle(&h)
                    .on_select(move |i, _ev, _w, _cx| {
                        chosen_in.borrow_mut().push(i);
                        *sel_in.borrow_mut() = i;
                    }),
            )
        });
        wcx.update(|w, cx| w.focus(&handle, cx));
        assert!(
            wcx.update(|w, cx| w.focused(cx).is_some()),
            "the strip is focused"
        );
        let mut key = |k: &str| {
            wcx.simulate_event(KeyDownEvent {
                keystroke: Keystroke::parse(k).unwrap(),
                is_held: false,
                prefer_character_input: false,
            });
            // The parent re-renders with the new selection before the
            // next key is dispatched.
            wcx.update(|w, _cx| w.refresh());
            wcx.run_until_parked();
        };
        key("right");
        assert_eq!(&*chosen.borrow(), &[1], "right lands on t1");
        key("right");
        assert_eq!(
            &*chosen.borrow(),
            &[1, 0],
            "right wraps past the disabled tab"
        );
        key("left");
        assert_eq!(&*chosen.borrow(), &[1, 0, 1], "left wraps back");
        key("down");
        assert_eq!(&*chosen.borrow(), &[1, 0, 1, 0], "down moves like right");
        key("up");
        assert_eq!(&*chosen.borrow(), &[1, 0, 1, 0, 1], "up moves like left");
        wcx.simulate_event(KeyDownEvent {
            keystroke: Keystroke::parse("enter").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent {
            keystroke: Keystroke::parse("enter").unwrap(),
        });
        assert_eq!(
            &*chosen.borrow(),
            &[1, 0, 1, 0, 1, 1],
            "Enter selects the current tab"
        );
    }
    /// A tab click selects it — a double click once (the kit's guard) —
    /// and a disabled tab's click fires nothing.
    #[gpui::test]
    fn tab_click_selects_once_and_disabled_is_inert(cx: &mut TestAppContext) {
        let chosen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let chosen_in = chosen.clone();
        let (_root, wcx) = render_part(cx, move || {
            let chosen_in = chosen_in.clone();

            // Single-character labels: the tabs are exactly
            // `target()` wide (the min wins), so the geometry is exact.
            div().flex().child(
                Tabs::new(
                    "click",
                    vec![
                        TabItem::new("a", "A"),
                        TabItem::new("b", "B"),
                        TabItem::new("c", "C").disabled("off"),
                    ],
                    0,
                )
                .on_select(move |i, _ev, _w, _cx| {
                    chosen_in.borrow_mut().push(i);
                }),
            )
        });
        let strip = wcx
            .debug_bounds("ui-tabs-click")
            .unwrap_or_else(|| panic!("the strip recorded no bounds"));
        // The first tab sits after the track's SP_4 padding and is
        // target()-tall; click its centre.
        let at = gpui::point(
            strip.origin.x + theme::z(theme::SP_4) + theme::target() / 2.,
            strip.center().y,
        );
        for clicks in [1usize, 2] {
            wcx.simulate_event(gpui::MouseDownEvent {
                button: gpui::MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::default(),
                click_count: clicks,
                first_mouse: false,
            });
            wcx.simulate_event(gpui::MouseUpEvent {
                button: gpui::MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::default(),
                click_count: clicks,
            });
        }
        assert_eq!(&*chosen.borrow(), &[0], "the double click selects once");

        // The disabled third tab: SP_4 + two tabs + two gaps, plus half a
        // tab.
        let third_x = strip.origin.x
            + theme::z(theme::SP_4) * 3.
            + theme::target() * 2.
            + theme::target() / 2.;
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: gpui::point(third_x, strip.center().y),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: gpui::point(third_x, strip.center().y),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert_eq!(
            &*chosen.borrow(),
            &[0],
            "a disabled tab's click fires nothing"
        );
    }
    /// The edge fades: left while scrolled right, right while not at the
    /// end, neither exactly at the ends.
    #[test]
    fn edge_fades_track_the_offset() {
        assert_eq!(Tabs::edge_fades(0.0, 100.0), (false, true));
        assert_eq!(Tabs::edge_fades(-50.0, 100.0), (true, true));
        assert_eq!(Tabs::edge_fades(-100.0, 100.0), (true, false));
        assert_eq!(Tabs::edge_fades(0.0, 0.0), (false, false));
    }

    /// The + button renders with its own id under the strip's id, and a
    /// disabled strip carries its reason.
    #[gpui::test]
    fn tabs_new_button_and_disabled(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .child(Tabs::new("with-new", sample_items(), 0).on_new(|_e, _w, _c| {}))
        });
        assert!(
            wcx.debug_bounds("ui-icon-button-with-new-tab-new")
                .is_some(),
            "the + button laid out with its nested id under the strip's id"
        );
        let strip = Tabs::new("s", sample_items(), 0).disabled("The job is running");
        assert_eq!(
            strip.tooltip_text().as_deref(),
            Some("The job is running"),
            "the disabled reason is the tooltip"
        );
    }

    /// A disabled strip disables every item: the tab clicks fire nothing,
    /// and the close × stays, dimmed (disabled dims, it never deletes):
    /// clicking it fires nothing and its tooltip is the strip's reason.
    #[gpui::test]
    fn disabled_strip_disables_every_item_and_close(cx: &mut TestAppContext) {
        let chosen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        let (chosen_in, closed_in) = (chosen.clone(), closed.clone());
        let (_root, wcx) = render_part(cx, move || {
            let chosen_in = chosen_in.clone();
            let closed_in = closed_in.clone();
            div().flex().child(
                Tabs::new(
                    "strip",
                    vec![
                        TabItem::new("a", "A"),
                        TabItem::new("b", "B").closable(move |_e, _w, _cx| closed_in.set(true)),
                    ],
                    0,
                )
                .disabled("The job is running")
                .on_select(move |i, _ev, _w, _cx| {
                    chosen_in.borrow_mut().push(i);
                }),
            )
        });
        let strip = wcx
            .debug_bounds("ui-tabs-strip")
            .unwrap_or_else(|| panic!("the strip recorded no bounds"));
        // The first tab (single character) is exactly `target()` wide:
        // click its centre — nothing fires.
        let at = gpui::point(
            strip.origin.x + theme::z(theme::SP_4) + theme::target() / 2.,
            strip.center().y,
        );
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
        assert!(
            chosen.borrow().is_empty(),
            "a disabled strip's tab clicks fire nothing"
        );
        let close = wcx
            .debug_bounds("ui-icon-button-b-tab-close-btn")
            .unwrap_or_else(|| panic!("the disabled strip deleted the close ×"));
        let at = close.center();
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
        assert!(!closed.get(), "the dimmed close × fires nothing");
        assert!(chosen.borrow().is_empty(), "…and selects nothing");
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at(wcx, "ui-icon-button-b-tab-close-btn")
                .as_deref(),
            Some("The job is running"),
            "the dimmed × carries the strip's reason"
        );

        // The enabled strip renders the ×.
        let (_root2, wcx2) = render_part(cx, move || {
            div().flex().child(Tabs::new(
                "strip2",
                vec![TabItem::new("b", "B").closable(|_e, _w, _cx| {})],
                0,
            ))
        });
        assert!(
            wcx2.debug_bounds("ui-icon-button-b-tab-close-btn")
                .is_some(),
            "the enabled strip renders the close ×"
        );
    }

    /// Every tab carries its default `debug_selector` (`ui-tab-<id>`).
    #[gpui::test]
    fn tabs_carry_their_default_selectors(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(Tabs::new("tabs", sample_items(), 0))
        });
        for sel in ["ui-tab-t0", "ui-tab-t1", "ui-tab-t2"] {
            assert!(
                wcx.debug_bounds(sel).is_some(),
                "{sel} recorded bounds under its default selector"
            );
        }
    }

    /// The per-tab tooltip: the tab's disabled reason, the strip's
    /// disabled reason (a disabled strip disables every tab), else the
    /// tab's label.
    #[test]
    fn tab_tooltip_precedence() {
        let tabs = Tabs::new("s", sample_items(), 0);
        assert_eq!(
            tabs.tab_tooltip(0).as_deref(),
            Some("Main"),
            "the enabled tab's tooltip is its label"
        );
        assert_eq!(tabs.tab_tooltip(1).as_deref(), Some("Fishing"));
        assert_eq!(
            tabs.tab_tooltip(2).as_deref(),
            Some("Read only"),
            "the disabled tab's tooltip is its reason"
        );
        assert_eq!(tabs.tab_tooltip(99), None, "out of range, no tooltip");
        let strip = Tabs::new("s", sample_items(), 0).disabled("The job is running");
        assert_eq!(
            strip.tab_tooltip(0).as_deref(),
            Some("The job is running"),
            "a disabled strip's reason is every tab's tooltip"
        );
        assert_eq!(
            strip.tab_tooltip(2).as_deref(),
            Some("Read only"),
            "the tab's own reason wins over the strip's"
        );
    }

    /// `.allow_double_click()` on the strip lets the second click of a
    /// double click through (the default is to drop it).
    #[gpui::test]
    fn allow_double_click_lets_the_second_click_through(cx: &mut TestAppContext) {
        let chosen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let chosen_in = chosen.clone();
        let (_root, wcx) = render_part(cx, move || {
            let chosen_in = chosen_in.clone();
            div().flex().child(
                Tabs::new("dbl", vec![TabItem::new("a", "A")], 0)
                    .allow_double_click()
                    .on_select(move |i, _ev, _w, _cx| {
                        chosen_in.borrow_mut().push(i);
                    }),
            )
        });
        let strip = wcx
            .debug_bounds("ui-tabs-dbl")
            .unwrap_or_else(|| panic!("the strip recorded no bounds"));
        let at = gpui::point(
            strip.origin.x + theme::z(theme::SP_4) + theme::target() / 2.,
            strip.center().y,
        );
        for clicks in [1usize, 2] {
            wcx.simulate_event(gpui::MouseDownEvent {
                button: gpui::MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::default(),
                click_count: clicks,
                first_mouse: false,
            });
            wcx.simulate_event(gpui::MouseUpEvent {
                button: gpui::MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::default(),
                click_count: clicks,
            });
        }
        assert_eq!(
            &*chosen.borrow(),
            &[0, 0],
            "with .allow_double_click() the double click selects twice"
        );
    }

    /// Enter/Space are a no-op when the selected tab is disabled (the
    /// strip's selection is inert, like a disabled part anywhere).
    #[gpui::test]
    fn enter_is_inert_when_the_selected_tab_is_disabled(cx: &mut TestAppContext) {
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let chosen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let chosen_in = chosen.clone();
        let (_root, wcx) = render_part(cx, move || {
            let chosen_in = chosen_in.clone();
            // The selection is the disabled third tab (sample_items:
            // t0 Main, t1 Fishing, t2 Locked "Read only").
            div().flex().child(
                Tabs::new("kbd", sample_items(), 2)
                    .focus_handle(&h)
                    .on_select(move |i, _ev, _w, _cx| {
                        chosen_in.borrow_mut().push(i);
                    }),
            )
        });
        wcx.update(|w, cx| w.focus(&handle, cx));
        for key in ["enter", "space"] {
            wcx.simulate_event(KeyDownEvent {
                keystroke: Keystroke::parse(key).unwrap(),
                is_held: false,
                prefer_character_input: false,
            });
            wcx.simulate_event(gpui::KeyUpEvent {
                keystroke: Keystroke::parse(key).unwrap(),
            });
            wcx.run_until_parked();
        }
        assert!(
            chosen.borrow().is_empty(),
            "Enter/Space on a disabled selection fire nothing"
        );
    }

    /// The tab dot's label is the tab's label and its id is a named
    /// child of the tab's id (the dot is unique per tab).
    #[test]
    fn tab_dot_carries_the_label_and_unique_id() {
        let item = TabItem::new("t0", "Main").dot(Tone::Attention);
        assert_eq!(item.dot_label().as_deref(), Some("Main"));
        assert_eq!(
            item.dot_id(),
            Some(child_id(&ElementId::Name("t0".into()), "dot")),
        );
        let plain = TabItem::new("t1", "Fishing");
        assert_eq!(plain.dot_label(), None, "no dot, no dot label");
        assert_eq!(plain.dot_id(), None, "no dot, no dot id");
    }

    /// With a `.scroll_handle(…)`, the track calls `track_scroll` with the
    /// parent's handle, so the handle tracks the real overflow and a real
    /// wheel scroll shifts the tabs within the strip (the edge fades read
    /// this same handle, so they follow the real scroll position).
    ///
    /// The parent is a fixed-width **column**: the strip's outer element is
    /// the K1 ring wrapper, which hugs its content on the main axis but
    /// stretches to the parent's width on the cross axis; in a column
    /// wrapper that fixes the strip's width, so an overflowing track
    /// scrolls instead of stretching the strip. (In a row parent the strip
    /// hugs its tabs, like every other kit part.)
    #[gpui::test]
    fn track_scroll_drives_the_strip(cx: &mut TestAppContext) {
        let h = ScrollHandle::new();
        let h_in = h.clone();
        // A parent narrower than the tabs, so the content overflows.
        let (_root, wcx) = render_part(cx, move || {
            let h_in = h_in.clone();
            div()
                .flex()
                .flex_col()
                .w(px(140.))
                .child(Tabs::new("scrolly", sample_items(), 0).scroll_handle(h_in))
        });
        assert!(
            h.max_offset().x > px(40.),
            "the tabs overflow the 140 px parent: max offset {}",
            f32::from(h.max_offset().x),
        );
        let before = wcx
            .debug_bounds("ui-tab-t1")
            .unwrap_or_else(|| panic!("t1 recorded no bounds"));
        // A real wheel event over the track: the track's
        // `overflow_x_scroll` listener scrolls the (handle-tracked)
        // offset, and the tabs shift within the strip.
        wcx.simulate_event(gpui::ScrollWheelEvent {
            position: gpui::point(px(70.), px(30.)),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(-60.), px(0.))),
            modifiers: Default::default(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        wcx.run_until_parked();
        assert!(
            h.offset().x < px(-30.),
            "the wheel moved the handle-tracked offset: {}",
            f32::from(h.offset().x),
        );
        let after = wcx
            .debug_bounds("ui-tab-t1")
            .unwrap_or_else(|| panic!("t1 recorded no bounds after the scroll"));
        assert!(
            f32::from(after.origin.x) < f32::from(before.origin.x) - 30.,
            "scrolling the strip shifted the tabs: before {before:?}, after {after:?}",
        );
    }

    /// Every tone renders in a tab without tripping the dot's label rule,
    /// and the tooltip that actually opens over an enabled tab's dot is
    /// the tab's label; over a disabled tab's dot it is the reason (the
    /// disabled dot carries no tooltip of its own) (K2c).
    #[gpui::test]
    fn tabs_render_every_tone_and_dot_tooltips(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::tooltip_at_point;
        let (_root, wcx) = render_part(cx, move || {
            div().flex().pt(theme::z(theme::SP_40)).child(Tabs::new(
                "tones",
                vec![
                    TabItem::new("tn", "Quiet").dot(Tone::Neutral),
                    TabItem::new("ta", "Ready").dot(Tone::Attention),
                    TabItem::new("tx", "Sign in").dot(Tone::ActNow),
                    TabItem::new("tc", "Step 2").dot(Tone::Contrast),
                    TabItem::new("td", "Locked")
                        .dot(Tone::Attention)
                        .disabled("Read only"),
                ],
                0,
            ))
        });
        let dot_at = |wcx: &mut gpui::VisualTestContext, sel: &'static str| {
            let tab = wcx
                .debug_bounds(sel)
                .unwrap_or_else(|| panic!("{sel} recorded no bounds"));
            gpui::point(
                tab.origin.x + theme::z(theme::SP_12) + theme::z(theme::DOT) / 2.,
                tab.center().y,
            )
        };
        for (sel, label) in [
            ("ui-tab-ta", "Ready"),
            ("ui-tab-tx", "Sign in"),
            ("ui-tab-tc", "Step 2"),
        ] {
            let at = dot_at(wcx, sel);
            assert_eq!(
                tooltip_at_point(wcx, at).as_deref(),
                Some(label),
                "{sel}: the dot's tooltip is the tab's label"
            );
        }
        assert!(wcx.debug_bounds("ui-tab-tn").is_some(), "Neutral renders");
        let at = dot_at(wcx, "ui-tab-td");
        assert_eq!(
            tooltip_at_point(wcx, at).as_deref(),
            Some("Read only"),
            "a disabled tab's dot does not cover the reason"
        );
    }

    /// Tab reaches a strip tracking a plain parent handle (K2c).
    #[gpui::test]
    fn tracked_handle_is_a_tab_stop(cx: &mut TestAppContext) {
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .child(Tabs::new("k", sample_items(), 0).focus_handle(&h))
        });
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert_eq!(wcx.update(|w, cx| w.focused(cx)), Some(handle));
    }

    /// Every tab keeps the `target()` hit width, even a one-letter tab in
    /// a squeezed strip (it scrolls instead) (K2c).
    #[gpui::test]
    fn tab_min_width_is_the_target(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().w(gpui::px(60.)).child(Tabs::new(
                "sq",
                vec![TabItem::new("x", "X"), TabItem::new("y", "Y")],
                0,
            ))
        });
        for sel in ["ui-tab-x", "ui-tab-y"] {
            let b = wcx.debug_bounds(sel).unwrap();
            assert!(f32::from(b.size.width) >= 44.0, "{sel}: {b:?}");
        }
    }
}
