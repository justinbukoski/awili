//! `ui::NavRail` — the kit's floating left rail (KIT.md v3, batch K4).
//!
//! The look matches the owner-approved mockup's `.rail` (`studio.css`,
//! `rail.js`): a `PANEL` floating face (`RADIUS_L`, the `float_shadow()`,
//! the 1 px `HIGHLIGHT` top edge, the 1 px `CONTROL_LINE` edge) with
//! `SP_8` padding and `SP_4` gaps, holding the v2 items — Workshop,
//! Council, Design, Backstage, then a divider, then More — as
//! **`RAIL_ITEM_W` × `RAIL_ITEM_H` (64 × 58) items: the `ICON_L` glyph
//! with its `type_::CAPTION` label underneath** (touch needs words, not
//! just icons).
//!
//! Item states (parent-driven, like every part): rest is `TEXT_DIM`,
//! hover is the `FILL_HOVER` wash + `TEXT`; the active item is `RAISE`
//! with `TEXT`. A `BADGE` dot sits at the item's top right, ringed by
//! a halo in the `PANEL_SOLID` surface (the mockup's `.rbadge`).
//!
//! Below `theme::COMPACT_W` the rail shows icons only, with the labels
//! as tooltips.
//!
//! The More item's menu (the mockup's `.rmenu`) is the parent's
//! [`super::menu::Menu`] — pass it into `.more(…)` and it settles at the
//! rail's lower right, outside the face.

use gpui::{
    div, prelude::*, AnyElement, App, BoxShadow, ClickEvent, ElementId, IntoElement, Pixels, Point,
    RenderOnce, Rgba, SharedString, Window,
};

use crate::icons::{self, Icon};
use crate::theme::{self, palette, Tone};
use crate::ui::focus_ring::focus_ring_style;
use crate::ui::interaction::{focus_wiring, Common, HOVER_GROUP};
use crate::ui::text;

/// The part tag for the default `debug_selector`.
const NAV_RAIL: &str = "nav-rail";
/// The item-tag prefix for the per-item `debug_selector`s.
const NAV_RAIL_ITEM: &str = "nav-rail-item";
/// The badge dot's ring width, in logical px before zoom (a 2 px halo of
/// the panel-solid surface, the mockup's `.rbadge` ring).
const BADGE_RING: f32 = theme::BADGE_RING;

/// One rail item (KIT.md § NavRail: `.item(id, Icon, label, active)`).
pub struct RailItem {
    id: ElementId,
    icon: Icon,
    label: SharedString,
    active: bool,
}

impl RailItem {
    /// A rail item with its icon and caption label.
    pub fn new(
        id: impl Into<ElementId>,
        icon: Icon,
        label: impl Into<SharedString>,
        active: bool,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            label: label.into(),
            active,
        }
    }

    /// The item's `ElementId` (feeds the per-item `debug_selector`s).
    pub fn id(&self) -> &ElementId {
        &self.id
    }

    /// The item's icon.
    pub fn icon(&self) -> Icon {
        self.icon
    }

    /// The item's caption label.
    pub fn label(&self) -> &SharedString {
        &self.label
    }

    /// Whether the item is the active page.
    pub fn is_active(&self) -> bool {
        self.active
    }
}

impl std::fmt::Debug for RailItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RailItem")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("active", &self.active)
            .finish()
    }
}

/// A kit nav rail. Build with [`NavRail::new`] and chain `.item(…)`,
/// `.divider()`, `.badge(…)`, `.more(…)` and the callbacks; the parent
/// owns the active page and the More menu.
#[derive(IntoElement)]
pub struct NavRail {
    common: Common,
    items: Vec<RailItem>,
    /// The item indices a divider precedes (`.divider()` before the next
    /// `.item(…)`).
    dividers_before: Vec<usize>,
    /// (item index, tone) badges.
    badges: Vec<(usize, Tone)>,
    /// A frozen hover on one item (the gallery; real pointer hover needs
    /// nothing from the parent).
    hovered_item: Option<usize>,
    /// The More menu (the parent's `Menu`), at the rail's lower right.
    more: Option<AnyElement>,
    scroll: Option<gpui::ScrollHandle>,
    item_focus: Vec<(usize, gpui::FocusHandle)>,
    on_item_click: Option<ItemClickHandler>,
}

/// The item-click handler (cloned into each item's listener).
pub type ItemClickHandler = std::sync::Arc<dyn Fn(usize, &ClickEvent, &mut Window, &mut App)>;

impl NavRail {
    /// An empty rail (the shared floating face).
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            common: Common {
                id: id.into(),
                ..Default::default()
            },
            items: Vec::new(),
            dividers_before: Vec::new(),
            badges: Vec::new(),
            hovered_item: None,
            more: None,
            scroll: None,
            item_focus: Vec::new(),
            on_item_click: None,
        }
    }

    /// Add a 64 × 58 item (the `ICON_L` glyph over its `type_::CAPTION`
    /// label).
    pub fn item(
        mut self,
        id: impl Into<ElementId>,
        icon: Icon,
        label: impl Into<SharedString>,
        active: bool,
    ) -> Self {
        self.items.push(RailItem::new(id, icon, label, active));
        self
    }

    /// Insert a 1 px `LINE` divider before the next `.item(…)` (the
    /// mockup's `.sep` between Backstage and More).
    pub fn divider(mut self) -> Self {
        if !self.items.is_empty() {
            self.dividers_before.push(self.items.len());
        }
        self
    }

    /// A stable parent-owned handle for a particular item, including More.
    /// This overrides the shared handle, even on the active item: callers
    /// must focus the item handle when supplying an override for that item.
    pub fn item_focus_handle(mut self, index: usize, handle: &gpui::FocusHandle) -> Self {
        self.item_focus.push((index, handle.clone()));
        self
    }

    /// The number of items so far (`.divider()` marks the position).
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// A `BADGE`-sized dot at the top right of the item at `index`
    /// (ringed by the `PANEL_SOLID` surface).
    pub fn badge(mut self, index: usize, tone: Tone) -> Self {
        self.badges.push((index, tone));
        self
    }

    /// Freeze the hover face on the item at `index` (the gallery).
    pub fn item_hovered(mut self, index: usize) -> Self {
        self.hovered_item = Some(index);
        self
    }

    /// The More menu (usually the parent's [`super::menu::Menu`]):
    /// settled at the rail's lower right, outside the face.
    pub fn more(mut self, menu: impl IntoElement) -> Self {
        self.more = Some(menu.into_any_element());
        self
    }

    /// Parent-owned scroll for a short window or large zoom.
    pub fn scroll_handle(mut self, scroll: gpui::ScrollHandle) -> Self {
        self.scroll = Some(scroll);
        self
    }

    /// The item-click handler (the item's index and the click); the
    /// kit's double-click guard applies. Enter/Space on the focused item
    /// fire it too.
    pub fn on_item_click(
        mut self,
        f: impl Fn(usize, &ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_item_click = Some(std::sync::Arc::new(f));
        self
    }

    /// The item at `index` (or none).
    pub fn item_at(&self, index: usize) -> Option<&RailItem> {
        self.items.get(index)
    }
}

crate::kit_shared!(NavRail);

impl std::fmt::Debug for NavRail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NavRail({})", self.common.selector_for(NAV_RAIL))
    }
}

/// An item's paint: (fill, text colour). Active is `RAISE` + `TEXT`; the
/// (frozen) hover is the `FILL_HOVER` wash + `TEXT`; rest is `TEXT_DIM`.
/// Disabled dims, it never deletes: the active item keeps its `RAISE`
/// marker, every label goes `TEXT_DIM`.
fn item_paint(active: bool, hovered: bool, disabled: bool) -> (Rgba, Rgba) {
    let p = palette();
    let fill = if active {
        p.raise
    } else if hovered && !disabled {
        p.fill_hover
    } else {
        p.transparent
    };
    let color = if disabled {
        p.text_dim
    } else if active || hovered {
        p.text
    } else {
        p.text_dim
    };
    (fill, color)
}

impl RenderOnce for NavRail {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette();
        let common = self.common;
        let selector = common.selector_for(NAV_RAIL);
        let disabled = common.is_disabled();
        let reason = common.disabled_reason.clone();
        // Narrow windows: icons only, the labels become tooltips.
        let labels_on = !crate::ui::sheet::is_compact(window);
        let active_index = self.items.iter().position(|it| it.active);

        let max_height =
            (window.viewport_size().height - theme::z(theme::EDGE) - theme::z(theme::EDGE))
                .max(theme::target());
        let scroll = self.scroll.clone().unwrap_or_else(|| {
            window
                .use_keyed_state(
                    crate::ui::interaction::child_id(&common.id, "scroll-handle"),
                    cx,
                    |_, _| gpui::ScrollHandle::new(),
                )
                .read(cx)
                .clone()
        });
        let mut column = div()
            .id(crate::ui::interaction::child_id(&common.id, "scroll"))
            .max_h(max_height)
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .items_center()
            .gap(theme::z(theme::SP_4))
            .p(theme::z(theme::SP_8));
        let last_focus = window.use_keyed_state(
            crate::ui::interaction::child_id(&common.id, "last-focus"),
            cx,
            |_, _| None::<usize>,
        );
        let mut focused_item = None;
        let mut child_index = 0;

        for (i, item) in self.items.iter().enumerate() {
            if self.dividers_before.contains(&i) {
                column = column.child(
                    div()
                        .w_full()
                        .border_t_1()
                        .border_color(p.line)
                        .my(theme::z(theme::SP_4))
                        .debug_selector(|| format!("{selector}-sep")),
                );
                child_index += 1;
            }

            let frozen = self.hovered_item == Some(i);
            let (fill, color) = item_paint(item.active, frozen, disabled);
            let label = item.label.clone();

            let glyph = icons::icon(item.icon, theme::ICON_L, color);
            let mut caption = text::caption(label.clone()).text_color_override(color);
            if !disabled {
                caption = caption.id(crate::ui::interaction::child_id(&item.id, "caption"));
                if !item.active {
                    caption = caption.hover_color(p.text);
                }
            }
            // The face is stateful from the start (`.id` first), so the
            // focus / click / tooltip refinements all stay on the same
            // `Stateful<Div>`. Its hit box is at least the target at
            // every zoom.
            let mut face = div()
                .id(item.id.clone())
                .debug_selector(|| format!("{selector}-item-{i}"))
                .relative()
                .w(theme::z(theme::RAIL_ITEM_W).max(theme::target()))
                .min_h(theme::z(theme::RAIL_ITEM_H).max(theme::target()))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(theme::z(theme::SP_4))
                .py(theme::z(theme::SP_8))
                .rounded(theme::z(theme::RADIUS_M))
                .bg(fill)
                .child(if disabled || item.active {
                    glyph
                } else {
                    glyph.group_hover(HOVER_GROUP, move |s| s.text_color(p.text))
                })
                .when(labels_on, |d| {
                    d.child(
                        div()
                            .debug_selector(|| format!("{selector}-item-{i}-caption"))
                            .child(caption),
                    )
                });

            match &reason {
                Some(reason) => {
                    face = face.tooltip(icons::tip(reason.to_string()));
                }
                None => {
                    face = face.group(HOVER_GROUP);
                    if !item.active {
                        face = face.hover(|s| s.bg(palette().fill_hover));
                    }
                    // Each item is a tab stop (the shared wiring); the
                    // rail's own `.focus_handle` / `.tab_index` / frozen
                    // `.focused` land on the active item.
                    let is_target = active_index == Some(i);
                    let handle = self
                        .item_focus
                        .iter()
                        .find(|(index, _)| *index == i)
                        .map(|(_, handle)| handle.clone())
                        .or_else(|| common.focus_handle.clone().filter(|_| is_target))
                        .unwrap_or_else(|| {
                            window
                                .use_keyed_state(
                                    crate::ui::interaction::child_id(&item.id, "focus"),
                                    cx,
                                    |_, cx| cx.focus_handle(),
                                )
                                .read(cx)
                                .clone()
                        });
                    if !common.pointer_only && handle.is_focused(window) {
                        focused_item = Some((i, child_index));
                    }
                    let armed = window.use_keyed_state(
                        crate::ui::interaction::child_id(&item.id, "armed-key"),
                        cx,
                        |_, _| None::<gpui::KeyboardButton>,
                    );
                    if !handle.is_focused(window) {
                        armed.update(cx, |armed, _| *armed = None);
                    }
                    let item_common = Common {
                        id: item.id.clone(),
                        tab_index: common.tab_index,
                        pointer_only: common.pointer_only,
                        focus_handle: Some(handle),
                        ..Default::default()
                    };
                    let on_click = self.on_item_click.clone();
                    let allow_double = common.allow_double_click;
                    let frozen_ring = common.frozen_focus() && is_target;
                    let key_arm = armed.clone();
                    face = focus_wiring(face, &item_common)
                        .when(!frozen_ring, |d| d.focus_visible(focus_ring_style))
                        .tooltip(icons::tip(label.clone()))
                        .on_key_down(move |ev, _window, cx| {
                            let button = if ev.keystroke.modifiers.modified() {
                                None
                            } else {
                                match ev.keystroke.key.as_str() {
                                    "enter" => Some(gpui::KeyboardButton::Enter),
                                    "space" | " " => Some(gpui::KeyboardButton::Space),
                                    _ => None,
                                }
                            };
                            key_arm.update(cx, |armed, _| *armed = button);
                        })
                        .on_click(move |ev, window, cx| {
                            if let ClickEvent::Keyboard(event) = ev {
                                let matched =
                                    armed.update(cx, |armed, _| armed.take() == Some(event.button));
                                if !matched {
                                    cx.stop_propagation();
                                    return;
                                }
                            }
                            if allow_double || ev.click_count() <= 1 {
                                if let Some(on_click) = &on_click {
                                    on_click(i, ev, window, cx);
                                }
                            }
                        });
                }
            }

            // The badge dot (a layout-neutral top-right dot with its 2 px
            // surface ring, the same pattern as `IconButton::badge`); a
            // disabled rail dims it, never deletes it.
            if let Some((_, tone)) = self.badges.iter().find(|(b, _)| *b == i) {
                let dot = if disabled {
                    p.text_dim
                } else {
                    theme::tone_color(*tone)
                };
                face = face.child(
                    div()
                        .flex()
                        .absolute()
                        .top(theme::z(theme::SP_4))
                        .right(theme::z(theme::SP_12))
                        .size(theme::z(theme::BADGE))
                        .rounded(theme::z(theme::RADIUS_PILL))
                        .bg(dot)
                        .debug_selector(|| format!("{selector}-item-{i}-badge"))
                        .shadow(vec![BoxShadow {
                            color: p.panel_solid.into(),
                            offset: Point {
                                x: Pixels::ZERO,
                                y: Pixels::ZERO,
                            },
                            blur_radius: Pixels::ZERO,
                            spread_radius: theme::z(BADGE_RING),
                            inset: false,
                        }]),
                );
            }

            let face = if common.frozen_focus() && active_index == Some(i) {
                crate::ui::focus_ring::focus_ring(face, true).into_any_element()
            } else {
                face.into_any_element()
            };
            column = column.child(face);
            child_index += 1;
        }
        let focus_index = focused_item.map(|(item, _)| item);
        let focus_changed = last_focus.update(cx, |last, _| {
            let changed = *last != focus_index;
            *last = focus_index;
            changed
        });
        if focus_changed {
            if let Some((_, child)) = focused_item {
                scroll.scroll_to_item(child);
            }
        }

        // The rail's face is the shared floating face (`float_fill`,
        // occluding, the 1 px `HIGHLIGHT` top, the `CONTROL_LINE` edge,
        // `RADIUS_L`, the float shadow). It clips its own corners, so the
        // More menu sits **beside** it in an unclipped wrapper.
        let face = crate::ui::panel::float_face(column)
            .shadow(vec![]) // docked rail: no shadow
            // Owner 2026-11-01: the rail is outlined in the Observatory purple
            // (RAIL_EDGE = the identity token), 1 px opaque.
            .border_color(crate::theme::palette().identity)
            .id(common.id.clone())
            .debug_selector(|| selector.clone());
        let face = match &reason {
            Some(reason) => face.tooltip(icons::tip(reason.to_string())),
            None => face,
        };

        let rail_width = theme::z(theme::RAIL_ITEM_W).max(theme::target())
            + theme::z(theme::SP_8)
            + theme::z(theme::SP_8);
        let menu_width = (window.viewport_size().width
            - theme::z(theme::EDGE)
            - theme::z(theme::EDGE)
            - rail_width
            - theme::z(theme::SP_8))
        .max(theme::target());
        let mut root = div().relative().flex().flex_col().child(face);
        // The More menu, at the rail's lower right, outside the face
        // (the mockup's `.rmenu` `left: calc(100% + 10px); bottom: 0`;
        // the 10 px gap snaps to `SP_8`). A disabled rail shows no menu.
        if let Some(more) = self.more.filter(|_| !disabled) {
            root = root.child(
                div()
                    .debug_selector(|| format!("{selector}-more"))
                    .flex()
                    .absolute()
                    .bottom(Pixels::ZERO)
                    // Anchored to the face's real right edge (it widens to
                    // keep 44 px items at small zooms), so it never overlaps.
                    .left(gpui::relative(1.))
                    .ml(theme::z(theme::SP_8))
                    .w(menu_width)
                    .child(more),
            );
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Size, TestAppContext};

    use crate::ui::interaction::test_util::{render_part, render_part_at};

    fn sample_rail() -> NavRail {
        NavRail::new("rail")
            .label("Main")
            .item("workshop", Icon::MessageCircle, "Workshop", true)
            .item("council", Icon::Users, "Council", false)
            .badge(1, Tone::Attention)
            .item("design", Icon::Image, "Design", false)
            .item("backstage", Icon::Layers, "Backstage", false)
            .divider()
            .item("more", Icon::MoreHorizontal, "More", false)
    }

    /// The rail lays out headless with all five v2 items, the divider
    /// and the More menu slot; the item hit boxes are the 64 × 58 token
    /// pair (at least the 44 px target in both directions).
    #[gpui::test]
    fn nav_rail_renders_the_v2_items(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || div().flex().child(sample_rail()));
        for sel in [
            "ui-nav-rail-rail-item-0",
            "ui-nav-rail-rail-item-1",
            "ui-nav-rail-rail-item-2",
            "ui-nav-rail-rail-item-3",
            "ui-nav-rail-rail-item-4",
        ] {
            let b = wcx
                .debug_bounds(sel)
                .unwrap_or_else(|| panic!("{sel} recorded no bounds"));
            assert!(
                f32::from(b.size.width) >= 44.0 && f32::from(b.size.height) >= 44.0,
                "{sel}: the item is ≥ 44×44, got {b:?}",
            );
        }
        let item = wcx
            .debug_bounds("ui-nav-rail-rail-item-0")
            .unwrap_or_else(|| panic!("the item recorded no bounds"));
        assert_eq!(
            f32::from(item.size.width),
            f32::from(theme::z(theme::RAIL_ITEM_W)),
            "the item is exactly RAIL_ITEM_W wide: {item:?}"
        );
        assert!(
            f32::from(item.size.height) >= f32::from(theme::z(theme::RAIL_ITEM_H)),
            "the item is at least RAIL_ITEM_H tall: {item:?}"
        );
        assert!(
            wcx.debug_bounds("ui-nav-rail-rail-sep").is_some(),
            "the divider laid out"
        );
    }

    /// The item's click fires with its index; the double-click guard
    /// holds (two quick clicks fire once); a disabled rail fires
    /// nothing.
    #[gpui::test]
    fn nav_rail_item_click_fires_once(cx: &mut TestAppContext) {
        let hits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let hits_in = hits.clone();
        let (_root, wcx) = render_part(cx, move || {
            let hits_in = hits_in.clone();

            div().flex().child(
                NavRail::new("click")
                    .label("Main")
                    .item("workshop", Icon::MessageCircle, "Workshop", true)
                    .item("council", Icon::Users, "Council", false)
                    .on_item_click(move |i, _ev, _w, _cx| {
                        hits_in.borrow_mut().push(i);
                    }),
            )
        });
        let at = wcx
            .debug_bounds("ui-nav-rail-click-item-1")
            .unwrap_or_else(|| panic!("the second item recorded no bounds"))
            .center();
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
            &*hits.borrow(),
            &[1],
            "the double click fires the item once"
        );

        // A disabled rail: inert, the reason is the tooltip.
        let dead = std::rc::Rc::new(std::cell::Cell::new(0));
        let dead_in = dead.clone();
        let (_root, wcx) = render_part(cx, move || {
            let dead_in = dead_in.clone();

            div().flex().child(
                NavRail::new("dead")
                    .label("Main")
                    .item("more", Icon::MoreHorizontal, "More", false)
                    .on_item_click(move |_i, _ev, _w, _cx| {
                        dead_in.set(1);
                    })
                    .disabled("The rail is locked"),
            )
        });
        let at = wcx
            .debug_bounds("ui-nav-rail-dead-item-0")
            .unwrap_or_else(|| panic!("the disabled item recorded no bounds"))
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
        assert_eq!(dead.get(), 0, "a disabled rail is not clickable");
        // K2c precedence: a disabled part's tooltip is its reason.
        let locked = sample_rail().disabled("The rail is locked");
        assert_eq!(
            locked.tooltip_text().as_deref(),
            Some("The rail is locked"),
            "the disabled reason wins the tooltip"
        );
        assert_eq!(
            locked.common.disabled_reason.as_deref(),
            Some("The rail is locked"),
            "the disabled reason is kept for the parent"
        );
    }

    /// Narrow windows (< `COMPACT_W`) show icons only: the rail still
    /// lays out at its item width, but the caption labels drop out.
    #[gpui::test]
    fn nav_rail_is_icons_only_in_a_compact_window(cx: &mut TestAppContext) {
        // A fixture width below the `COMPACT_W` floor (the kit's local
        // const + `z()` rule — no raw size literal at the call).
        const NARROW_W: f32 = 800.0;
        let narrow = Size {
            width: theme::z(NARROW_W),
            height: theme::z(theme::RAIL_ITEM_H),
        };
        let (_window, wcx) = render_part_at(cx, narrow, move || div().flex().child(sample_rail()));
        let item = wcx
            .debug_bounds("ui-nav-rail-rail-item-0")
            .unwrap_or_else(|| panic!("the item recorded no bounds in the narrow window"));
        assert!(
            f32::from(item.size.height)
                <= f32::from(theme::z(theme::RAIL_ITEM_H)) + f32::from(theme::z(theme::SP_4)),
            "without the caption label the item stays at its token height: {item:?}"
        );
        let _ = _window;
    }

    /// The paint table: active is RAISE + TEXT, hover is FILL_HOVER +
    /// TEXT, rest is none + TEXT_DIM.
    #[test]
    fn nav_rail_item_paint_table() {
        let p = palette();
        assert_eq!(item_paint(false, false, false), (p.transparent, p.text_dim));
        assert_eq!(item_paint(false, true, false), (p.fill_hover, p.text));
        assert_eq!(item_paint(true, false, false), (p.raise, p.text));
        assert_eq!(
            item_paint(true, true, true),
            (p.raise, p.text_dim),
            "disabled: the active marker stays, the label dims"
        );
        assert_eq!(
            item_paint(true, true, false),
            (p.raise, p.text),
            "the active fill wins over the hover (the hover adds nothing)"
        );
    }

    /// K4b (Codex/DeepSeek P1): the More menu lays out outside the face,
    /// unclipped; real hover lifts an item's caption (the paint probe);
    /// every item is a tab stop and a ≥ 44 px hit at zoom 0.5.
    #[gpui::test]
    fn nav_rail_more_hover_tab_and_zoom(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::{hover, painted, same_color, unhover};
        let (_root, wcx) = render_part(cx, move || {
            div().flex().p(theme::z(theme::SP_40)).child(
                sample_rail().more(
                    div()
                        .w(theme::z(theme::MENU_W))
                        .h(theme::target())
                        .debug_selector(|| "the-more-menu".to_string()),
                ),
            )
        });
        let face = wcx.debug_bounds("ui-nav-rail-rail").unwrap();
        let menu = wcx.debug_bounds("the-more-menu").unwrap();
        assert!(
            menu.origin.x >= face.origin.x + face.size.width,
            "the menu sits beside the face, unclipped: {menu:?} vs {face:?}"
        );
        let p = palette();
        let caption = crate::ui::interaction::child_id(&ElementId::from("council"), "caption");
        unhover(wcx);
        assert!(same_color(painted(&caption).unwrap(), p.text_dim), "rest");
        hover(wcx, "ui-nav-rail-rail-item-1");
        assert!(
            same_color(painted(&caption).unwrap(), p.text),
            "real hover lifts the caption"
        );
        unhover(wcx);
        // Tab reaches the items in order.
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert!(
            wcx.update(|w, cx| w.focused(cx)).is_some(),
            "an item is a tab stop"
        );
        theme::set_ui_scale(0.5);
        let (_root, wcx) = render_part(cx, move || div().flex().child(sample_rail()));
        let b = wcx.debug_bounds("ui-nav-rail-rail-item-0").unwrap();
        assert!(
            f32::from(b.size.width) >= 44.0 && f32::from(b.size.height) >= 44.0,
            "{b:?}"
        );
        theme::set_ui_scale(1.0);
    }

    /// K4b: compact windows drop the captions (the selector proves it).
    #[gpui::test]
    fn nav_rail_compact_drops_captions(cx: &mut TestAppContext) {
        let narrow = Size {
            width: gpui::px(800.),
            height: gpui::px(600.),
        };
        let (_w, wcx) = render_part_at(cx, narrow, move || div().flex().child(sample_rail()));
        assert!(wcx
            .debug_bounds("ui-nav-rail-rail-item-0-caption")
            .is_none());
        let (_root, wcx) = render_part(cx, move || div().flex().child(sample_rail()));
        assert!(wcx
            .debug_bounds("ui-nav-rail-rail-item-0-caption")
            .is_some());
    }

    /// K4b: a disabled rail dims, never deletes — the badge stays — shows
    /// its reason on every item, has no tab stops and no More menu.
    #[gpui::test]
    fn nav_rail_disabled_dims(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().p(theme::z(theme::SP_40)).child(
                sample_rail()
                    .more(div().debug_selector(|| "dead-menu".to_string()).child("m"))
                    .disabled("Finish setup first"),
            )
        });
        assert!(
            wcx.debug_bounds("ui-nav-rail-rail-item-1-badge").is_some(),
            "the badge stays"
        );
        assert!(
            wcx.debug_bounds("dead-menu").is_none(),
            "no menu on a disabled rail"
        );
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at(wcx, "ui-nav-rail-rail-item-2")
                .as_deref(),
            Some("Finish setup first")
        );
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert!(wcx.update(|w, cx| w.focused(cx)).is_none(), "no tab stops");
    }

    /// K4b round 2 (Codex): the More menu is usable — a button inside it
    /// takes a click; Tab reaches every item; at zoom 0.5 the menu sits
    /// exactly `SP_8` right of the (widened) rail.
    #[gpui::test]
    fn nav_rail_more_is_usable_and_tab_walks_the_items(cx: &mut TestAppContext) {
        let hits = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let order = std::rc::Rc::new(std::cell::RefCell::new(Vec::<usize>::new()));
        let (h, o) = (hits.clone(), order.clone());
        let (_root, wcx) = render_part(cx, move || {
            let (h, o) = (h.clone(), o.clone());
            div().flex().p(theme::z(theme::SP_40)).child(
                sample_rail()
                    .on_item_click(move |i, _e, _w, _c| o.borrow_mut().push(i))
                    .more(
                        crate::ui::button::Button::new("more-settings", "Settings")
                            .pointer_only()
                            .on_click(move |_e, _w, _c| h.set(h.get() + 1)),
                    ),
            )
        });
        let at = wcx
            .debug_bounds("ui-button-more-settings")
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
        assert_eq!(hits.get(), 1, "the More menu takes clicks");
        // Tab, then Enter on each stop: the item that fires names the stop,
        // so the sequence proves five distinct stops, in item order.
        for _ in 0..5 {
            wcx.update(|w, cx| w.focus_next(cx));
            // GPUI registers the keyboard click at paint, on the focused
            // element: draw once with the new focus.
            wcx.update(|w, _| w.refresh());
            wcx.run_until_parked();
            // GPUI fires the keyboard click on the key-up.
            let ks = gpui::Keystroke::parse("enter").unwrap();
            wcx.simulate_event(gpui::KeyDownEvent {
                keystroke: ks.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            wcx.simulate_event(gpui::KeyUpEvent { keystroke: ks });
            wcx.run_until_parked();
        }
        assert_eq!(
            &*order.borrow(),
            &[0, 1, 2, 3, 4],
            "Tab walks the items in order"
        );

        theme::set_ui_scale(0.5);
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(
                sample_rail().more(
                    div()
                        .w(theme::z(theme::MENU_W))
                        .h(theme::target())
                        .debug_selector(|| "m".to_string()),
                ),
            )
        });
        let face = wcx.debug_bounds("ui-nav-rail-rail").unwrap();
        let menu = wcx.debug_bounds("m").unwrap();
        let gap = f32::from(menu.origin.x) - f32::from(face.origin.x + face.size.width);
        let want = f32::from(theme::z(theme::SP_8));
        theme::set_ui_scale(1.0);
        assert!(
            (gap - want).abs() < 1.0,
            "the menu sits exactly SP_8 right of the face: gap {gap}, want {want}"
        );
    }

    /// K4b round 4 (Codex): `.pointer_only()` reaches the rail's items —
    /// none is a tab stop, and the items still take clicks.
    #[gpui::test]
    fn pointer_only_rail_has_no_tab_stops(cx: &mut TestAppContext) {
        let order = std::rc::Rc::new(std::cell::RefCell::new(Vec::<usize>::new()));
        let o = order.clone();
        let (_root, wcx) = render_part(cx, move || {
            let o = o.clone();
            div().flex().child(
                sample_rail()
                    .pointer_only()
                    .on_item_click(move |i, _e, _w, _c| o.borrow_mut().push(i)),
            )
        });
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert!(wcx.update(|w, cx| w.focused(cx)).is_none(), "no tab stops");
        let at = wcx
            .debug_bounds("ui-nav-rail-rail-item-2")
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
        assert_eq!(&*order.borrow(), &[2], "clicks still work");
    }
}
