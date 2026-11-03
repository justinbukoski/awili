//! `ui::Menu` — the kit's dropdown menu (KIT.md v3, batch K3).
//!
//! A floating surface on the shared panel face, `MENU_W` (280 px) wide
//! (the mockup's `.rmenu`), built from kit `Row`s. A row is an optional
//! `ICON` icon, a `type_::UI` label and an optional dim `type_::LABEL`
//! shortcut at the right; `.separator()` is a `LINE` rule. Keyboard:
//! Up and Down move the selection, skipping disabled items (wrapping);
//! Enter selects; Escape closes. On choose the menu **closes, restores
//! focus to the trigger, then runs the action** — in that order (the
//! parent owns the open state through `.on_close(…)` and hands the menu
//! the trigger's focus handle through `.trigger_focus(…)`).
//!
//! The menu is one tab stop (the kit's `Tabs` contract): the parent
//! focuses the menu's focus handle when it opens, and the arrow keys
//! work no matter which row (descendant) holds focus, because the key
//! events propagate up to the menu's root.
//!
//! Built on top of [`super::popover::Popover`] (anchoring, outside-click
//! close and the window margin).

use gpui::{
    div, prelude::*, App, Bounds, ClickEvent, ElementId, IntoElement, KeyDownEvent, Pixels, Point,
    RenderOnce, SharedString, Size, Window,
};

use crate::icons::{self, Icon};
use crate::theme::{self, palette};
use crate::ui::focus_ring::focus_ring_style_on;
use crate::ui::interaction::{child_id, ClickHandler, MENU};

/// The parent's move/close callbacks (KIT.md § Menu: the parent owns
/// the selection and the close).
type MoveHandler = Box<dyn Fn(usize, &mut Window, &mut App) + 'static>;
type CloseHandler = Box<dyn Fn(&mut Window, &mut App) + 'static>;

/// A single menu row (KIT.md § Menu: an optional icon, a label, an
/// optional shortcut and an optional `.disabled(reason)`).
#[derive(Clone)]
pub struct MenuItem {
    label: SharedString,
    icon: Option<Icon>,
    shortcut: Option<SharedString>,
    disabled_reason: Option<SharedString>,
    /// The action, shared through `Rc` so a chosen row (cloned for the
    /// key handlers) keeps one closure.
    on_click: Option<std::rc::Rc<ClickHandler>>,
}

impl std::fmt::Debug for MenuItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuItem")
            .field("label", &self.label)
            .field("disabled", &self.is_disabled())
            .finish()
    }
}

impl MenuItem {
    /// A menu row with the given label (also its accessible name).
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            shortcut: None,
            disabled_reason: None,
            on_click: None,
        }
    }

    /// A leading `ICON` icon, tinted with the label colour.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// A dim `type_::LABEL` shortcut at the right end.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        // Spelled out ("Ctrl+N") off macOS; identity on macOS.
        let shortcut: SharedString = shortcut.into();
        self.shortcut = Some(SharedString::from(
            crate::shortcuts::localize(&shortcut).into_owned(),
        ));
        self
    }

    /// Disable the row with its reason (dim, inert, the reason is the
    /// tooltip; the arrow keys skip it).
    pub fn disabled(mut self, reason: impl Into<SharedString>) -> Self {
        self.disabled_reason = Some(reason.into());
        self
    }

    /// The action, run when the row is chosen (a click or Enter, after
    /// the menu has closed and restored focus).
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(std::rc::Rc::new(Box::new(f)));
        self
    }

    /// The row's label.
    pub fn label_text(&self) -> &SharedString {
        &self.label
    }

    /// Whether the row is disabled (carries a reason).
    pub fn is_disabled(&self) -> bool {
        self.disabled_reason.is_some()
    }

    /// The disabled reason (the tooltip), if any.
    pub fn disabled_reason(&self) -> Option<SharedString> {
        self.disabled_reason.clone()
    }
}

/// One entry of the menu's body: a row or a separator.
#[derive(Clone, Debug)]
pub(crate) enum MenuEntry {
    Item(MenuItem),
    Separator,
}

/// A kit dropdown menu. Build with [`Menu::new`] and chain `.item(…)`,
/// `.separator(…)`, `.selected(…)`, `.on_move(…)`, `.on_close(…)` and
/// `.trigger_focus(…)`; the parent owns the open state and the
/// keyboard selection.
#[derive(IntoElement)]
pub struct Menu {
    common: crate::ui::interaction::Common,
    anchor: Option<Bounds<Pixels>>,
    anchor_point: Option<Point<Pixels>>,
    items: Vec<MenuEntry>,
    selected: usize,
    wrap_navigation: bool,
    on_move: Option<MoveHandler>,
    on_close: Option<CloseHandler>,
    trigger: Option<gpui::FocusHandle>,
    popover_state: Option<super::popover::PopoverState>,
}

impl Menu {
    /// An empty menu.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            anchor: None,
            anchor_point: None,
            items: Vec::new(),
            selected: 0,
            wrap_navigation: true,
            on_move: None,
            on_close: None,
            trigger: None,
            popover_state: None,
        }
    }

    /// Anchor the menu to the trigger's bounds (window coordinates).
    /// Without an anchor the menu paints at its natural (top-left)
    /// position — the gallery's pin-up use.
    pub fn anchor(mut self, anchor: Bounds<Pixels>) -> Self {
        self.anchor = Some(anchor);
        self.anchor_point = None;
        self
    }

    /// Position the menu at a pointer location with normal window-edge
    /// clamping, rather than beside a trigger control.
    pub fn anchor_point(mut self, point: Point<Pixels>) -> Self {
        self.anchor = None;
        self.anchor_point = Some(point);
        self
    }

    /// Append a row.
    pub fn item(mut self, item: MenuItem) -> Self {
        self.items.push(MenuEntry::Item(item));
        self
    }

    /// Replace the rows with the given items (separators via
    /// `.separator(…)`).
    pub fn items(mut self, items: impl IntoIterator<Item = MenuItem>) -> Self {
        for item in items {
            self.items.push(MenuEntry::Item(item));
        }
        self
    }

    /// Append a `LINE` separator rule.
    pub fn separator(mut self) -> Self {
        self.items.push(MenuEntry::Separator);
        self
    }

    /// The keyboard-selected row (an item index; the parent re-renders
    /// the menu with the new value when `.on_move(…)` fires).
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Whether Up/Down wrap at the first and last enabled items. Defaults to
    /// `true`; native context menus can opt out to preserve platform behavior.
    pub fn wrap_navigation(mut self, wrap: bool) -> Self {
        self.wrap_navigation = wrap;
        self
    }

    /// The current keyboard selection (an item index).
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The number of rows (separators excluded).
    pub fn item_count(&self) -> usize {
        self.items
            .iter()
            .filter(|e| matches!(e, MenuEntry::Item(_)))
            .count()
    }

    /// The arrow-key callback: the new item index (the parent re-renders
    /// the menu with `.selected(…)`). Disabled rows are skipped.
    pub fn on_move(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_move = Some(Box::new(f));
        self
    }

    /// The close callback: Escape, an outside click, and the close step
    /// of a choose. The parent sets its open state to closed there.
    pub fn on_close(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Box::new(f));
        self
    }

    /// The trigger's focus handle: on choose (and on Escape), the menu
    /// restores focus to it before running the chosen action.
    pub fn trigger_focus(mut self, handle: &gpui::FocusHandle) -> Self {
        self.trigger = Some(handle.clone());
        self
    }

    /// The anchored menu's placement state, owned by the parent (re-create
    /// or `.reset()` it when the anchor moves). Without it the menu keeps
    /// its own under its id, so the measured placement survives re-render
    /// either way.
    pub fn popover_state(mut self, state: super::popover::PopoverState) -> Self {
        self.popover_state = Some(state);
        self
    }

    /// The next (or previous, `dir = -1`) **enabled** item index from
    /// `from`, wrapping; `None` when there is no enabled row.
    pub fn step(&self, from: usize, dir: isize) -> Option<usize> {
        menu_step(&self.items, from, dir, self.wrap_navigation)
    }
}

crate::kit_shared!(Menu);

impl std::fmt::Debug for Menu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Menu({})", self.common.selector_for(MENU))
    }
}

/// The choose sequence (KIT.md § Menu): close, restore focus to the
/// trigger, then run the action — in that order.
fn choose(
    item: &MenuItem,
    on_close: Option<&CloseHandler>,
    trigger: Option<&gpui::FocusHandle>,
    ev: &ClickEvent,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(on_close) = on_close {
        on_close(window, cx);
    }
    if let Some(trigger) = trigger {
        window.focus(trigger, cx);
    }
    if let Some(on_click) = &item.on_click {
        on_click(ev, window, cx);
    }
}

impl RenderOnce for Menu {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let p = palette();
        let common = self.common;
        let selector = common.selector_for(MENU);
        let anchor = self.anchor;
        let anchor_point = self.anchor_point;
        let anchored = anchor.is_some() || anchor_point.is_some();
        let selected = self.selected;
        let wrap_navigation = self.wrap_navigation;
        // The callbacks are shared by several closures (the rows'
        // choose, the keyboard handlers): one closure each, shared
        // through `Rc` (the handler itself is not `Clone`).
        let on_move = self.on_move.map(|h| std::rc::Rc::new(h));
        let on_close = self.on_close.map(|h| std::rc::Rc::new(h));
        let trigger = self.trigger.clone();
        let disabled = common.is_disabled();
        let menu_reason = common.disabled_reason.clone();
        let menu_off = menu_reason.is_some();

        // The rows (and separators), in one column with the mockup's
        // 6 px (→ `SP_8`) padding.
        // Anchored, the popover's face supplies the padding (one face).
        let scroll = window
            .use_keyed_state(child_id(&common.id, "scroll-handle"), cx, |_, _| {
                gpui::ScrollHandle::new()
            })
            .read(cx)
            .clone();
        let max_height =
            (window.viewport_size().height - theme::z(theme::EDGE) - theme::z(theme::EDGE))
                .max(theme::target());
        let mut body = div()
            .id(child_id(&common.id, "scroll"))
            .max_h(max_height)
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .when(!anchored, |d| d.p(theme::z(theme::SP_8)));
        let mut item_index = 0usize;
        for (entry_index, entry) in self.items.iter().enumerate() {
            match entry {
                MenuEntry::Separator => {
                    body = body.child(crate::ui::divider::Divider::new());
                }
                MenuEntry::Item(item) => {
                    let is_selected = item_index == selected;
                    let row_id = child_id(&common.id, &format!("item-{item_index}"));
                    let mut row = crate::ui::row::Row::new(row_id.clone())
                        .title(item.label.clone())
                        // The menu's double-click setting reaches its rows.
                        .allow_double_click_if(common.allow_double_click)
                        .on_click({
                            let item = item.clone();
                            let on_close = on_close.clone();
                            let trigger = trigger.clone();
                            move |ev, window, cx| {
                                if item.is_disabled() || menu_off {
                                    return;
                                }
                                choose(
                                    &item,
                                    on_close.as_deref(),
                                    trigger.as_ref(),
                                    ev,
                                    window,
                                    cx,
                                );
                            }
                        });
                    if let Some(icon) = item.icon {
                        row = row.icon(icon);
                    }
                    if let Some(shortcut) = item.shortcut.clone() {
                        // The brighten rule: on the selected row's wash, or under the
                        // pointer, the shortcut lifts to `TEXT_2` (`TEXT_DIM` is below
                        // 4.5:1 on `FILL_HOVER`).
                        let s =
                            crate::ui::text::caption(shortcut).id(child_id(&row_id, "shortcut"));
                        row = row.trailing(if is_selected {
                            s.text_color_override(p.text_2)
                        } else {
                            s.dim().hover_color(p.text_2)
                        });
                    }
                    // The keyboard selection paints the hover wash (the
                    // mockup's `.mrow:hover`), not the row's `RAISE`
                    // selected face.
                    if is_selected {
                        row = row.hovered(true);
                    }
                    // A disabled menu disables every row with its reason (so no
                    // row fires or takes focus); else a row's own reason.
                    if let Some(reason) =
                        menu_reason.clone().or_else(|| item.disabled_reason.clone())
                    {
                        row = row.disabled(reason);
                    }
                    // Pointer hover is also a selection change, so a menu
                    // opened without a keyboard selection can be committed
                    // with Return after the user points at a row. Keep the
                    // listener on a full-width, geometry-neutral wrapper:
                    // the Row retains its own selector and hit target.
                    let mut hover = div()
                        .id(child_id(&common.id, &format!("hover-{item_index}")))
                        .w_full();
                    if !item.is_disabled() && !menu_off {
                        if let Some(on_move) = on_move.clone() {
                            hover = hover.on_hover(move |hovered, window, cx| {
                                if *hovered && selected != item_index {
                                    on_move(item_index, window, cx);
                                }
                            });
                        }
                    }
                    body = body.child(hover.child(row));
                    item_index += 1;
                }
            }
            let _ = entry_index;
        }

        // Anchored (the real use): the menu sits in a popover — the
        // outside click and Escape close it there, and the popover's panel
        // is the **one** floating face (no nested border or shadow): the
        // menu's own surface is then a plain column, still the stateful
        // element the focus and key handlers live on.
        let w = theme::z(theme::MENU_W);
        let mut column = div().flex().flex_col().w_full().child(body);
        if anchored {
            // Popover permits intrinsic-wide content for horizontally
            // scrollable surfaces. Menu rows instead stay inside MENU_W;
            // their existing text slots truncate and expose full tooltips.
            let available =
                (window.bounds().size.width - theme::z(theme::EDGE) * 2.).max(gpui::Pixels::ZERO);
            let content_width =
                (w.min(available) - theme::z(theme::SP_8) * 2. - gpui::px(theme::HAIRLINE) * 2.)
                    .max(gpui::Pixels::ZERO);
            column = column.w(content_width).min_w_0();
        }
        let mut panel = if anchored {
            column
        } else {
            super::panel::float_face(column)
                // Swallow clicks on the panel itself so they do not
                // reach what is behind it.
                .occlude()
                .w(w)
                .max_w(gpui::relative(1.))
        }
        .debug_selector(|| format!("ui-menu-{id}-panel", id = common.id))
        .id(child_id(&common.id, "panel"));
        // The menu's surface is its focus element in both forms (the key
        // handlers live on it): the shared wiring makes it a tab stop with
        // the part's `.tab_index(…)` (and the parent's handle, if given).
        if !disabled {
            panel = crate::ui::interaction::focus_wiring(panel, &common);
        }
        panel = panel.focus_visible(focus_ring_style_on(p.panel_solid));
        // The frozen ring (`.focused(true)`, the gallery) paints the same.
        if common.focused && !disabled {
            panel = crate::ui::focus_ring::freeze_ring(panel, p.panel_solid);
        }
        if !disabled {
            let items = self.items.clone();
            // The key handlers keep their own copy of the close handler
            // (the anchored branch below also uses it).
            let on_close_key = on_close.clone();
            panel = panel.on_key_down(move |ev, window, cx| {
                match ev.keystroke.key.as_str() {
                    "escape" if !anchored => {
                        // The anchored form: the popover's panel handles
                        // Escape (one close, not two).
                        if on_close_key.is_some() {
                            window.prevent_default();
                            cx.stop_propagation();
                            if let Some(on_close) = &on_close_key {
                                on_close(window, cx);
                            }
                        }
                    }
                    "up" | "left" => {
                        if let Some(next) = menu_step(&items, selected, -1, wrap_navigation) {
                            scroll_to_menu_item(&scroll, &items, next);
                            window.prevent_default();
                            cx.stop_propagation();
                            if let Some(on_move) = &on_move {
                                on_move(next, window, cx);
                            }
                        }
                    }
                    "down" | "right" => {
                        if let Some(next) = menu_step(&items, selected, 1, wrap_navigation) {
                            scroll_to_menu_item(&scroll, &items, next);
                            window.prevent_default();
                            cx.stop_propagation();
                            if let Some(on_move) = &on_move {
                                on_move(next, window, cx);
                            }
                        }
                    }
                    "enter" | "space" | " " => {
                        let Some(item) = items
                            .iter()
                            .filter_map(|e| match e {
                                MenuEntry::Item(item) => Some(item),
                                _ => None,
                            })
                            .nth(selected)
                            .cloned()
                        else {
                            return;
                        };
                        if item.is_disabled() {
                            return;
                        }
                        window.prevent_default();
                        cx.stop_propagation();
                        let ev = ClickEvent::Keyboard(gpui::KeyboardClickEvent::default());
                        choose(
                            &item,
                            on_close_key.as_deref(),
                            trigger.as_ref(),
                            &ev,
                            window,
                            cx,
                        );
                    }
                    _ => {}
                }
            });
        }

        if anchor.is_some() || anchor_point.is_some() {
            // The placement state persists: the parent's, else the menu's
            // own (element state under its id) — the measured origin
            // survives the re-render the measurement requests.
            let state = self.popover_state.clone().unwrap_or_else(|| {
                window
                    .use_keyed_state(child_id(&common.id, "popover-state"), cx, |_, _| {
                        super::popover::PopoverState::new()
                    })
                    .read(cx)
                    .clone()
            });
            let popover = if let Some(point) = anchor_point {
                super::popover::Popover::at_point(common.id.clone(), point)
            } else {
                super::popover::Popover::new(
                    common.id.clone(),
                    anchor.expect("an anchored Menu has bounds or a point"),
                )
            }
            .width(theme::MENU_W)
            .state(state)
            // The menu column owns focus and keys; its popover is no tab stop.
            .pointer_only()
            // The menu's double-click setting reaches its outside-click layer.
            .allow_double_click_if(common.allow_double_click)
            .content(panel);
            // A disabled menu's popover is disabled too: an outside click and
            // Escape close nothing.
            let popover = match menu_reason.clone() {
                Some(reason) => popover.disabled(reason),
                None => popover,
            };
            let popover = match on_close {
                Some(h) => popover.on_close(move |_ev, w, cx| h(w, cx)),
                None => popover,
            };
            // The outside-click layer is the popover's full-size scrim, so
            // the wrapper fills the window too. Keep it out of normal flow:
            // menus are hosted after the page content, but their point and
            // window-edge coordinates are relative to the window origin.
            div()
                .absolute()
                .top(gpui::Pixels::ZERO)
                .left(gpui::Pixels::ZERO)
                .size_full()
                .debug_selector(|| selector.clone())
                .child(popover)
        } else {
            div()
                .relative()
                .w(w)
                .max_w(gpui::relative(1.))
                .debug_selector(|| selector.clone())
                .child(panel)
        }
    }
}

/// Map the item index to the scroll container's child, counting separators.
fn scroll_to_menu_item(scroll: &gpui::ScrollHandle, items: &[MenuEntry], selected: usize) {
    if let Some((entry, _)) = items
        .iter()
        .enumerate()
        .filter(|(_, entry)| matches!(entry, MenuEntry::Item(_)))
        .nth(selected)
    {
        scroll.scroll_to_item(entry);
    }
}

/// The arrow-key walk (the `Menu::step` logic on a slice).
fn menu_step(items: &[MenuEntry], from: usize, dir: isize, wrap: bool) -> Option<usize> {
    let n = items
        .iter()
        .filter(|e| matches!(e, MenuEntry::Item(_)))
        .count();
    if n == 0 {
        return None;
    }
    let enabled = |i: usize| -> bool {
        items
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item(item) => Some(item),
                _ => None,
            })
            .nth(i)
            .map(|item| !item.is_disabled())
            .unwrap_or(false)
    };
    if from == usize::MAX {
        return if dir > 0 {
            (0..n).find(|&i| enabled(i))
        } else {
            (0..n).rev().find(|&i| enabled(i))
        };
    }
    if wrap {
        let mut i = from;
        for _ in 0..n {
            i = (i as isize + dir).rem_euclid(n as isize) as usize;
            if enabled(i) {
                return Some(i);
            }
        }
        None
    } else if dir > 0 {
        (from.saturating_add(1)..n).find(|&i| enabled(i))
    } else {
        (0..from.min(n)).rev().find(|&i| enabled(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, TestAppContext};

    use crate::ui::interaction::test_util::render_part;

    /// A real click (down and up) at `at`.
    fn click_at(wcx: &mut gpui::VisualTestContext, at: gpui::Point<Pixels>) {
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
    }

    /// The brighten rule on shortcuts: the selected row's shortcut paints
    /// `TEXT_2`; another row's paints `TEXT_DIM`, lifting to `TEXT_2` under
    /// the pointer.
    #[gpui::test]
    fn menu_shortcuts_brighten_on_the_selected_and_hovered_rows(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::{hover, painted, same_color};
        let (_root, wcx) = render_part(cx, || {
            Menu::new("m")
                .item(MenuItem::new("Rename").shortcut("⌘R"))
                .item(MenuItem::new("Archive").shortcut("⌘A"))
                .selected(0)
        });
        wcx.run_until_parked();
        let shortcut = |i: usize| {
            child_id(
                &child_id(&ElementId::from("m"), &format!("item-{i}")),
                "shortcut",
            )
        };
        let p = theme::palette();
        assert!(
            same_color(painted(&shortcut(0)).unwrap(), p.text_2),
            "selected row"
        );
        assert!(
            same_color(painted(&shortcut(1)).unwrap(), p.text_dim),
            "resting row"
        );
        hover(wcx, "ui-row-m-item-1");
        assert!(
            same_color(painted(&shortcut(1)).unwrap(), p.text_2),
            "hovered row"
        );
    }

    /// A disabled menu acts on nothing, inline or anchored: clicking a row
    /// runs no action (every row carries the menu's reason), Tab lands on
    /// no row, and anchored, an outside click and Escape close nothing.
    #[gpui::test]
    fn disabled_menu_acts_on_nothing(cx: &mut TestAppContext) {
        for anchored in [false, true] {
            let ran = std::rc::Rc::new(std::cell::Cell::new(0));
            let closed = std::rc::Rc::new(std::cell::Cell::new(0));
            let (r, c) = (ran.clone(), closed.clone());
            let (_root, wcx) = render_part(cx, move || {
                let (r, c) = (r.clone(), c.clone());
                let m = Menu::new("m")
                    .item(MenuItem::new("Rename").on_click(move |_e, _w, _c| r.set(r.get() + 1)))
                    .disabled("Sign in first")
                    .on_close(move |_w, _c| c.set(c.get() + 1));
                let m = if anchored {
                    m.anchor(Bounds {
                        origin: gpui::point(px(40.), px(40.)),
                        size: gpui::size(px(100.), px(20.)),
                    })
                } else {
                    m
                };
                div().size_full().relative().child(m)
            });
            wcx.run_until_parked();
            let row = wcx.debug_bounds("ui-row-m-item-0").expect("the row");
            click_at(wcx, row.center());
            assert_eq!(
                ran.get(),
                0,
                "anchored={anchored}: a disabled menu runs no action"
            );
            wcx.update(|w, cx| w.focus_next(cx));
            assert!(
                wcx.update(|w, cx| w.focused(cx)).is_none(),
                "anchored={anchored}: Tab lands on no row"
            );
            if anchored {
                let esc = gpui::Keystroke::parse("escape").unwrap();
                wcx.simulate_event(gpui::KeyDownEvent {
                    keystroke: esc.clone(),
                    is_held: false,
                    prefer_character_input: false,
                });
                wcx.simulate_event(gpui::KeyUpEvent { keystroke: esc });
                const OUT: f32 = 400.0;
                click_at(wcx, gpui::point(px(OUT), px(OUT)));
            }
            assert_eq!(closed.get(), 0, "anchored={anchored}: nothing closes it");
        }
    }

    fn sample_items() -> Vec<MenuItem> {
        vec![
            MenuItem::new("Open")
                .icon(Icon::BookOpen)
                .on_click(|_e, _w, _c| {}),
            MenuItem::new("Locked").disabled("Read only"),
            MenuItem::new("Duplicate").shortcut("⌘D"),
            MenuItem::new("Delete").shortcut("⌘⇧D").icon(Icon::Archive),
        ]
    }

    /// The selection walk: the arrow keys skip disabled rows and wrap.
    #[test]
    fn menu_step_skips_disabled_and_wraps() {
        let menu = Menu::new("m").items(sample_items());
        // 0 Open, 1 Locked (disabled), 2 Duplicate, 3 Delete.
        assert_eq!(menu.step(0, 1), Some(2), "down skips the disabled row");
        assert_eq!(menu.step(2, 1), Some(3));
        assert_eq!(menu.step(3, 1), Some(0), "down wraps");
        assert_eq!(menu.step(0, -1), Some(3), "up wraps");
        assert_eq!(menu.step(3, -1), Some(2));
        assert_eq!(menu.step(2, -1), Some(0), "up skips the disabled row");
        assert_eq!(Menu::new("e").step(0, 1), None, "no rows, no step");
        // Separators are not rows: they do not count into the walk.
        let menu = Menu::new("m").items(sample_items()).separator();
        assert_eq!(menu.item_count(), 4, "the separator is not a row");
        assert_eq!(menu.step(0, 1), Some(2), "the walk ignores it");
    }

    #[test]
    fn menu_step_can_preserve_native_no_wrap_navigation() {
        let menu = Menu::new("m").items(sample_items()).wrap_navigation(false);
        assert_eq!(
            menu.step(usize::MAX, 1),
            Some(0),
            "Down starts at the first item"
        );
        assert_eq!(
            menu.step(usize::MAX, -1),
            Some(3),
            "Up starts at the last item"
        );
        assert_eq!(menu.step(0, -1), None, "Up stops at the first item");
        assert_eq!(menu.step(3, 1), None, "Down stops at the last item");
        assert_eq!(menu.step(0, 1), Some(2), "disabled rows remain skipped");
    }

    /// Arrow keys move the selection through the parent's `on_move`
    /// (skipping disabled rows); Enter chooses the selected row: the
    /// menu closes, focus is restored to the trigger, then the action
    /// runs — in that order.
    #[gpui::test]
    fn menu_keyboard_moves_and_enters_choose(cx: &mut TestAppContext) {
        let moves = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let moves_in = moves.clone();
        let events_in = events.clone();
        let trigger = cx.update(|cx| cx.focus_handle());
        let menu_focus = cx.update(|cx| cx.focus_handle());
        let t = trigger.clone();
        let mf = menu_focus.clone();
        let sel = std::rc::Rc::new(std::cell::RefCell::new(0usize));
        let sel_in = sel.clone();
        let (_root, wcx) = render_part(cx, move || {
            let moves_in = moves_in.clone();
            let events_in = events_in.clone();
            let sel_in = sel_in.clone();
            let t = t.clone();
            let mf = mf.clone();
            let selected = *sel_in.borrow();
            let mut menu = Menu::new("more")
                .selected(selected)
                .focus_handle(&mf)
                .trigger_focus(&t)
                .on_move(move |next, _w, _c| {
                    *sel_in.borrow_mut() = next;
                    moves_in.borrow_mut().push(next);
                })
                .on_close({
                    let events_in = events_in.clone();
                    move |_w, _c| {
                        events_in.borrow_mut().push("close".to_string());
                    }
                });
            for item in sample_items() {
                if item.label_text() == "Duplicate" {
                    menu = menu.item(item.on_click({
                        let events_in = events_in.clone();
                        move |_e, _w, _c| {
                            events_in.borrow_mut().push("action".to_string());
                        }
                    }));
                } else {
                    menu = menu.item(item);
                }
            }
            div().flex().child(menu)
        });
        wcx.update(|w, cx| w.focus(&menu_focus, cx));

        // The parent re-renders with the new selection before the next
        // key is dispatched (the kit's parent-owned selection model).
        fn key(wcx: &mut gpui::VisualTestContext, k: &str) {
            let ks = gpui::Keystroke::parse(k).unwrap();
            wcx.simulate_event(gpui::KeyDownEvent {
                keystroke: ks.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            wcx.simulate_event(gpui::KeyUpEvent { keystroke: ks });
            wcx.update(|w, _cx| w.refresh());
            wcx.run_until_parked();
        }

        // Down from row 0 skips the disabled row 1 and lands on row 2;
        // down again lands on row 3; up walks back to row 2.
        key(wcx, "down");
        key(wcx, "down");
        key(wcx, "up");
        assert_eq!(
            moves.borrow().as_slice(),
            [2usize, 3usize, 2usize],
            "down skips the disabled row; up walks back"
        );
        assert_eq!(
            *sel.borrow(),
            2,
            "the parent re-renders with the new selection"
        );

        // Enter on row 2 (Duplicate — the row with an action):
        // close → focus the trigger → action.
        key(wcx, "enter");
        assert_eq!(
            events.borrow().as_slice(),
            ["close", "action"],
            "the choose closes, restores focus and runs the action"
        );
        assert!(
            wcx.update(|w, cx| w.focused(cx)) == Some(trigger.clone()),
            "focus is back on the trigger"
        );
    }

    /// Hovering an enabled row moves the parent's selection, so Return
    /// chooses that row. Disabled rows never become the selection.
    #[gpui::test]
    fn menu_hover_selects_enabled_row_for_return(cx: &mut TestAppContext) {
        let selected = std::rc::Rc::new(std::cell::Cell::new(usize::MAX));
        let moves = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let menu_focus = cx.update(|cx| cx.focus_handle());
        let (selected_in, moves_in, events_in, focus_in) = (
            selected.clone(),
            moves.clone(),
            events.clone(),
            menu_focus.clone(),
        );
        let (_root, wcx) = render_part(cx, move || {
            let (selected_in, moves_in, events_in) =
                (selected_in.clone(), moves_in.clone(), events_in.clone());
            let menu = Menu::new("hover")
                .selected(selected_in.get())
                .focus_handle(&focus_in)
                .on_move(move |index, window, _cx| {
                    selected_in.set(index);
                    moves_in.borrow_mut().push(index);
                    window.refresh();
                })
                .on_close({
                    let events_in = events_in.clone();
                    move |_window, _cx| events_in.borrow_mut().push("close")
                })
                .item(MenuItem::new("Locked").disabled("Read only"))
                .item(MenuItem::new("Duplicate").on_click({
                    let events_in = events_in.clone();
                    move |_event, _window, _cx| events_in.borrow_mut().push("duplicate")
                }));
            div().flex().child(menu)
        });
        wcx.update(|window, cx| window.focus(&menu_focus, cx));

        let disabled = wcx
            .debug_bounds("ui-row-hover-item-0")
            .expect("disabled row");
        wcx.simulate_mouse_move(disabled.center(), None, gpui::Modifiers::default());
        wcx.run_until_parked();
        assert_eq!(selected.get(), usize::MAX, "disabled hover is ignored");
        assert!(
            moves.borrow().is_empty(),
            "disabled hover does not move selection"
        );

        let enabled = wcx
            .debug_bounds("ui-row-hover-item-1")
            .expect("enabled row");
        wcx.simulate_mouse_move(enabled.center(), None, gpui::Modifiers::default());
        wcx.run_until_parked();
        assert_eq!(selected.get(), 1);
        assert_eq!(moves.borrow().as_slice(), [1usize]);

        let enter = gpui::Keystroke::parse("enter").unwrap();
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: enter.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent { keystroke: enter });
        wcx.update(|window, _cx| window.refresh());
        wcx.run_until_parked();
        assert_eq!(events.borrow().as_slice(), ["close", "duplicate"]);
    }

    /// A click on a row runs the same choose sequence; a disabled row is
    /// inert.
    #[gpui::test]
    fn menu_row_click_runs_the_choose_sequence(cx: &mut TestAppContext) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let events_in = events.clone();
        let trigger = cx.update(|cx| cx.focus_handle());
        let t = trigger.clone();
        let (_root, wcx) = render_part(cx, move || {
            let events_in = events_in.clone();
            let t = t.clone();
            let menu = Menu::new("more")
                .trigger_focus(&t)
                .on_close({
                    let events_in = events_in.clone();
                    move |_w, _c| {
                        events_in.borrow_mut().push("close".to_string());
                    }
                })
                .item(MenuItem::new("Locked").disabled("Read only"))
                .item(MenuItem::new("Duplicate").on_click({
                    let events_in = events_in.clone();
                    move |_e, _w, _c| {
                        events_in.borrow_mut().push("action".to_string());
                    }
                }));
            div().flex().child(menu)
        });

        // The disabled row is inert.
        let dead = wcx
            .debug_bounds("ui-row-more-item-0")
            .unwrap_or_else(|| panic!("the disabled row recorded no bounds"));
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: dead.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: dead.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert!(events.borrow().is_empty(), "the disabled row is inert");

        // The enabled row: close → focus → action.
        let row = wcx
            .debug_bounds("ui-row-more-item-1")
            .unwrap_or_else(|| panic!("the enabled row recorded no bounds"));
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: row.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: row.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert_eq!(
            events.borrow().as_slice(),
            ["close", "action"],
            "a row click closes, restores focus and runs the action"
        );
        assert!(
            wcx.update(|w, cx| w.focused(cx)) == Some(trigger.clone()),
            "focus is back on the trigger"
        );
    }

    /// Escape closes the menu (and does not run any action).
    #[gpui::test]
    fn menu_escape_closes(cx: &mut TestAppContext) {
        let closed = std::rc::Rc::new(std::cell::Cell::new(0));
        let closed_in = closed.clone();
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            let closed_in = closed_in.clone();
            div().flex().child(
                Menu::new("more")
                    .focus_handle(&h)
                    .item(MenuItem::new("Open"))
                    .on_close(move |_w, _c| {
                        closed_in.set(1);
                    }),
            )
        });
        wcx.update(|w, cx| w.focus(&handle, cx));
        let ks = gpui::Keystroke::parse("escape").unwrap();
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: ks.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent { keystroke: ks });
        assert_eq!(closed.get(), 1, "Escape closes the menu");
    }

    /// The menu is `MENU_W` wide and its rows keep the `target()` hit
    /// height (the mockup's `.mrow`).
    #[gpui::test]
    fn menu_is_menu_w_wide_with_target_tall_rows(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, || {
            let mut menu = Menu::new("more");
            for item in sample_items() {
                menu = menu.item(item);
            }
            div().flex().child(menu)
        });
        let panel = wcx
            .debug_bounds("ui-menu-more-panel")
            .unwrap_or_else(|| panic!("the menu panel recorded no bounds"));
        assert_eq!(
            f32::from(panel.size.width),
            f32::from(theme::z(theme::MENU_W)),
            "the menu is MENU_W wide: {panel:?}",
        );
        let row = wcx
            .debug_bounds("ui-row-more-item-0")
            .unwrap_or_else(|| panic!("the first row recorded no bounds"));
        assert!(
            f32::from(row.size.height) >= f32::from(theme::target()),
            "the row keeps its target() hit height: {row:?}",
        );
    }

    /// K3b items 4–5: an anchored menu near the window's bottom settles
    /// above its anchor and **stays** there across re-renders (its
    /// placement state persists without the parent passing one); its
    /// outside-click layer fills the window; it is one floating face (the
    /// menu column sits inside the popover's panel, not a second
    /// `MENU_W` face); and Tab reaches it with its `.tab_index`.
    #[gpui::test]
    fn anchored_menu_keeps_its_placement_and_is_one_face(cx: &mut TestAppContext) {
        use gpui::px;
        let (w, h) = (480.0_f32, 400.0_f32);
        let anchor = Bounds {
            origin: gpui::point(px(40.), px(350.)),
            size: gpui::size(px(80.), px(30.)),
        };
        let (_handle, wcx) = crate::ui::interaction::test_util::render_part_at(
            cx,
            gpui::size(px(w), px(h)),
            move || {
                let mut menu = Menu::new("more").anchor(anchor).tab_index(0);
                for item in sample_items() {
                    menu = menu.item(item);
                }
                menu.on_close(|_w, _cx| {})
            },
        );
        wcx.run_until_parked();
        // The settle frame (the harness draws on refresh).
        wcx.update(|win, _| win.refresh());
        wcx.run_until_parked();
        for round in 0..3 {
            let panel = wcx
                .debug_bounds("ui-popover-more-panel")
                .unwrap_or_else(|| panic!("the popover panel recorded no bounds"));
            assert!(
                panel.origin.y + panel.size.height <= anchor.origin.y,
                "round {round}: the menu settled above its anchor and stays: {panel:?}"
            );
            let inner = wcx.debug_bounds("ui-menu-more-panel").unwrap();
            assert!(
                inner.size.width < panel.size.width && panel.contains(&inner.origin),
                "round {round}: one face — the menu column sits inside the popover \
                 panel: {inner:?} in {panel:?}"
            );
            wcx.update(|win, _| win.refresh());
            wcx.run_until_parked();
        }
        let root = wcx.debug_bounds("ui-menu-more").unwrap();
        assert_eq!(
            (f32::from(root.size.width), f32::from(root.size.height)),
            (w, h),
            "the outside-click layer fills the window"
        );
        wcx.update(|win, cx| win.focus_next(cx));
        wcx.run_until_parked();
        assert!(
            wcx.update(|win, cx| win.focused(cx)).is_some(),
            "Tab reaches the anchored menu"
        );
    }
}
