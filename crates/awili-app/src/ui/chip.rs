//! `ui::Chip` — the kit's small labelled control (KIT.md v3, batch K2).
//!
//! A single `type_::LABEL` line of `TEXT_DIM` text (hover `TEXT`), the
//! `FILL_HOVER` wash, `RADIUS_S` corners — the mockup's `.chip` with the
//! kit's 44 px hit height for clickable chips. Optional leading `ICON_S`
//! icon, a semantic tone, a file face (`Icon::for_extension` + a
//! middle-truncated name, full name in the tooltip), and a removable
//! trailing × (a 44 px hit `IconButton.quiet`).
//!
//! Two token-grid adaptations of the mockup: its `padding: 0 10px`
//! becomes `SP_8` and its `gap: 6px` becomes `SP_4` (10 and 6 are not on
//! the v2 spacing grid; DESIGN.md §10.3 snaps them down).

use gpui::{
    div, prelude::*, px, App, ClickEvent, ElementId, IntoElement, RenderOnce, Rgba, SharedString,
    Window,
};

use crate::icons::{self, Icon};
use crate::theme::{self, palette, Tone};
use crate::ui::focus_ring::{focus_ring, focus_ring_style};
use crate::ui::interaction::ClickHandler;
use crate::ui::interaction::CHIP;
use crate::ui::text;

/// The most characters a file name displays, ellipsis included.
const FILE_NAME_MAX: usize = theme::CHIP_FILE_NAME_MAX;
/// The characters kept at the end of a long file name (the extension and
/// a little of the stem).
const FILE_NAME_TAIL: usize = theme::CHIP_FILE_NAME_TAIL;

/// Middle-truncate a file name: `head…tail`, keeping the front of the
/// name and its end (the extension). Names of at most `FILE_NAME_MAX`
/// characters pass through unchanged.
pub fn middle_truncate(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= FILE_NAME_MAX {
        return name.to_string();
    }
    let head = FILE_NAME_MAX - FILE_NAME_TAIL - 1;
    let head: String = chars.iter().take(head).collect();
    let tail: String = chars.iter().skip(chars.len() - FILE_NAME_TAIL).collect();
    format!("{head}…{tail}")
}

/// A kit chip. Build with [`Chip::new`] and chain the face options and
/// the shared conventions; clickable chips are menu triggers with a
/// `target()`-tall hit box.
#[derive(IntoElement)]
pub struct Chip {
    common: crate::ui::interaction::Common,
    label: SharedString,
    icon: Option<Icon>,
    dropdown: bool,
    outline: Option<Tone>,
    dot: Option<Tone>,
    file_name: Option<SharedString>,
    tone: Option<Tone>,
    removable: Option<ClickHandler>,
    on_click: Option<ClickHandler>,
    compact: bool,
    bounds: Option<std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>>,
}

impl Chip {
    /// A quiet chip with the given label.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                label: Some(label.clone()),
                ..Default::default()
            },
            label,
            icon: None,
            dropdown: false,
            outline: None,
            dot: None,
            file_name: None,
            tone: None,
            removable: None,
            on_click: None,
            compact: false,
            bounds: None,
        }
    }

    /// One status-strip row tall (the composer footer's model chip): the
    /// strip's 11 px type, a 4 px inset, no taller than the row.
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// A persistent semantic outline without changing the label or icon colour.
    pub fn outlined(mut self, tone: Tone) -> Self {
        self.outline = Some(tone);
        self
    }

    /// An outlined menu trigger with a persistent trailing chevron.
    pub fn dropdown(mut self) -> Self {
        self.dropdown = true;
        self
    }

    /// Show dropdown affordances only for controls that open a menu.
    pub fn dropdown_if(mut self, enabled: bool) -> Self {
        self.dropdown = enabled;
        self
    }

    /// Record the actual chip face for caller-owned popup placement.
    pub fn bounds_handle(
        mut self,
        bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    ) -> Self {
        self.bounds = Some(bounds);
        self
    }

    /// A leading status `Dot` (the mockup's status chips, e.g. "All good");
    /// it carries the chip's label as its meaning.
    pub fn dot(mut self, tone: Tone) -> Self {
        self.dot = Some(tone);
        self
    }

    /// The status dot as painted: labelled with the chip's label; enabled,
    /// it has its own child id (so it tooltips its meaning, the kit's
    /// embedded-dot rule); disabled, it dims with the chip and has no id, so
    /// no child tooltip covers the chip's reason.
    pub(crate) fn status_dot(&self) -> Option<crate::ui::dot::Dot> {
        let tone = self.dot?;
        let dot = crate::ui::dot::Dot::new(tone).label(self.label.clone());
        Some(if self.common.is_disabled() {
            dot.dimmed()
        } else {
            dot.id(child_id(&self.common.id, "dot"))
        })
    }

    /// A leading `ICON_S` icon.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The file face: a `Icon::for_extension`-chosen file icon plus the
    /// name, middle-truncated; the full name is the tooltip.
    pub fn file(mut self, name: impl Into<SharedString>) -> Self {
        let name = name.into();
        self.file_name = Some(name.clone());
        self.common.label = Some(name);
        self
    }

    /// The file name (the `.file(…)` face), if any.
    pub fn file_name(&self) -> Option<SharedString> {
        self.file_name.clone()
    }

    /// A semantic tone: the text and the icon take the tone's tokens.
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    /// The current tone, if any.
    pub fn tone_of(&self) -> Option<Tone> {
        self.tone
    }

    /// Make the chip removable: a trailing × (a 44 px hit
    /// `IconButton.quiet`) whose click fires `on_remove` — and never the
    /// chip's own `on_click`.
    pub fn removable(
        mut self,
        on_remove: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.removable = Some(Box::new(on_remove));
        self
    }

    /// Whether the chip shows its remove ×.
    pub fn is_removable(&self) -> bool {
        self.removable.is_some()
    }

    /// The chip's own click handler (the menu trigger); the kit's
    /// double-click guard applies.
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    /// The name the chip paints: the middle-truncated file name for the
    /// `.file(…)` face, else the label.
    pub fn display_name(&self) -> SharedString {
        self.file_name
            .as_ref()
            .map(|n| SharedString::from(middle_truncate(n)))
            .unwrap_or_else(|| self.label.clone())
    }

    /// The `ElementId` of the chip's text slot: the truncating `ui::text`
    /// shows the full (untruncated) name as its tooltip only with an id —
    /// KIT.md § Long text.
    pub fn text_id(&self) -> gpui::ElementId {
        crate::ui::interaction::child_id(&self.common.id, "text")
    }

    /// The chip's paint: (fill, text colour, icon colour).
    pub fn paint(&self) -> (Rgba, Rgba, Rgba) {
        self.paint_at(self.common.hovered)
    }

    /// The paint at a given hover state (the real pointer hover paints
    /// `paint_at(true)` through the face's hover group).
    pub fn paint_at(&self, hovered: bool) -> (Rgba, Rgba, Rgba) {
        let p = palette();
        let disabled = self.common.is_disabled();
        let hovered = hovered && !disabled;
        if disabled {
            return (p.transparent, p.text_dim, p.text_dim);
        }
        let fill = if hovered {
            p.fill_hover
        } else if self.dropdown {
            p.fill_ghost
        } else {
            p.transparent
        };
        let text = if let Some(tone) = self.tone {
            theme::tone_text(tone)
        } else if hovered || self.dropdown {
            p.text
        } else {
            p.text_dim
        };
        let icon_color = if let Some(tone) = self.tone {
            theme::tone_color(tone)
        } else if hovered || self.dropdown {
            p.text
        } else {
            p.text_dim
        };
        (fill, text, icon_color)
    }
}

crate::kit_shared!(Chip);

use crate::ui::interaction::{child_id, focus_wiring, HOVER_GROUP};

impl std::fmt::Debug for Chip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Chip({})", self.common.selector_for(CHIP))
    }
}

impl RenderOnce for Chip {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let (fill, text_color, icon_color) = self.paint();
        let disabled = self.common.is_disabled();
        // The real pointer hover paints the frozen `.hovered` values.
        let (hover_fill, hover_text, hover_icon) = self.paint_at(true);

        let status_dot = self.status_dot();
        let icon = self.icon;
        let file_name = self.file_name.clone();
        let display = self.display_name();
        // The leading icon is always painted (disabled dims it to
        // `TEXT_DIM`, it never deletes).
        let leading = icon.or_else(|| file_name.as_deref().map(Icon::for_extension));
        let text_id = self.text_id();
        let removable = self.removable;
        let chip_id = self.common.id.clone();
        let reason = self.common.disabled_reason.clone();
        // The chip's double-click setting reaches its ×.
        let allow_double_remove = self.common.allow_double_click;

        // The text slot: the enabled chip's text carries the chip's id
        // (KIT.md § Long text: the truncating `ui::text` tooltips the full
        // name — the whole file name, not the abbreviated one shown) and
        // lifts on hover. A disabled chip's text has no id, so no child
        // tooltip can cover the disabled reason on the root.
        let compact = self.compact;
        let mut label = if compact {
            text::text(theme::STRIP_TYPE, display)
        } else {
            text::label(display)
        }
        .text_color_override(text_color);
        if !disabled {
            label = label.id(text_id).hover_color(hover_text);
            if let Some(full) = file_name.clone() {
                label = label.full_text(full);
            }
        }

        let face = div()
            .flex()
            .items_center()
            .gap(theme::z(theme::SP_4))
            .px(theme::z(if compact { theme::SP_4 } else { theme::SP_8 }))
            .h(if compact {
                theme::z(theme::STRIP_H)
            } else {
                theme::target()
            })
            .min_w(if compact {
                theme::z(theme::STRIP_H)
            } else {
                theme::target()
            })
            .rounded(theme::z(theme::RADIUS_S))
            .bg(fill)
            .when(self.dropdown || self.outline.is_some(), |d| {
                d.border_1().border_color(
                    self.outline
                        .map(theme::tone_color)
                        .unwrap_or_else(|| theme::palette().control_line),
                )
            })
            .when_some(status_dot, |d, dot| d.child(dot))
            .when_some(leading, |d, which| {
                let svg = icons::icon(which, theme::ICON_S, icon_color);
                d.child(if disabled {
                    svg
                } else {
                    svg.group_hover(HOVER_GROUP, move |s| s.text_color(hover_icon))
                })
            })
            .child(label)
            .when(self.dropdown, |d| {
                d.child(icons::icon(Icon::ChevronDown, theme::ICON_S, icon_color))
            })
            .when_some(removable, |d, on_remove| {
                // The ×: a 44 px hit IconButton.quiet. Its wrapper stops
                // propagation so a remove never fires the chip's own click
                // (the menu trigger). A disabled chip keeps its × dimmed,
                // with the chip's reason (disabled dims, it never deletes).
                let mut remove = crate::ui::icon_button::IconButton::new(
                    child_id(&chip_id, "remove-btn"),
                    Icon::X,
                )
                .label("Remove")
                .quiet()
                .allow_double_click_if(allow_double_remove);
                if let Some(reason) = reason.clone() {
                    remove = remove.disabled(reason);
                } else {
                    remove = remove.on_click(move |ev, w, cx| {
                        if allow_double_remove || ev.click_count() <= 1 {
                            on_remove(ev, w, cx);
                        }
                    });
                }
                d.child(
                    div()
                        .id(child_id(&chip_id, "remove"))
                        .on_click(|_ev, window, cx| {
                            // The × carries its own click (the IconButton
                            // above, guard and all); this listener only
                            // keeps the remove from also firing the chip's
                            // own click (the menu trigger).
                            window.prevent_default();
                            cx.stop_propagation();
                        })
                        .child(remove),
                )
            });

        let common = self.common;
        let on_click = self.on_click;

        let selector = common.selector_for(CHIP);
        let focused = common.frozen_focus();
        let allow_double = common.allow_double_click;
        let tip = common.tooltip();

        // Real pointer hover: the enabled chip's face is the hover group —
        // it washes `FILL_HOVER` and its text and icon lift (above). The
        // face carries its own id (a named child of the chip's id), so it
        // is stateful and GPUI tracks its hover state.
        let face_id = child_id(&common.id, "face");
        let mut root = div()
            .when_some(self.bounds, |d, bounds| {
                d.on_children_prepainted(move |children, window, cx| {
                    let next = children.first().copied();
                    if bounds.replace(next) != next {
                        window.defer(cx, |window, _| window.refresh());
                    }
                })
            })
            .id(common.id.clone())
            .flex()
            .items_center()
            .min_w(theme::target())
            .debug_selector(|| selector.clone())
            .child(if !disabled {
                face.id(face_id)
                    .group(HOVER_GROUP)
                    .hover(move |s| s.bg(hover_fill))
            } else {
                face.id(face_id)
            });

        if !disabled {
            root = focus_wiring(root, &common)
                .when(!common.focused, |d| d.focus_visible(focus_ring_style));
            if let Some(on_click) = on_click {
                root = root.on_click(move |ev, window, cx| {
                    if allow_double || ev.click_count() <= 1 {
                        on_click(ev, window, cx);
                    }
                });
            }
        }
        if let Some(tip) = tip {
            root = root.tooltip(icons::tip(tip.to_string()));
        }

        // The ring box may shrink to the hit target in a narrow parent, so
        // a long label truncates instead of widening the part.
        focus_ring(root, focused).min_w(theme::target())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::{render_part, tooltip_at};

    /// The chip's double-click setting reaches its remove ×: with
    /// `.allow_double_click()` a double click removes twice; without, once.
    #[gpui::test]
    fn chip_remove_follows_the_double_click_setting(cx: &mut TestAppContext) {
        for allow in [false, true] {
            let removed = std::rc::Rc::new(std::cell::Cell::new(0u32));
            let r = removed.clone();
            let (_root, wcx) = render_part(cx, move || {
                let r = r.clone();
                Chip::new("d", "notes.md")
                    .removable(move |_e, _w, _cx| r.set(r.get() + 1))
                    .allow_double_click_if(allow)
            });
            let at = wcx
                .debug_bounds("ui-icon-button-d-remove-btn")
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
            assert_eq!(removed.get(), if allow { 2 } else { 1 }, "allow={allow}");
        }
    }

    /// The status dot: enabled it keeps its tone and its own id; disabled it
    /// dims to `TEXT_DIM` with the chip and has no id (no tooltip over the
    /// reason). Rendering lays it out in both states.
    #[gpui::test]
    fn status_dot_dims_when_disabled(cx: &mut TestAppContext) {
        let on = Chip::new("s", "Waiting for you").dot(Tone::ActNow);
        let dot = on.status_dot().expect("a dot");
        assert_eq!(dot.color(), theme::tone_color(Tone::ActNow));
        assert!(!dot.is_dimmed());
        assert!(
            dot.has_id(),
            "enabled: the dot owns an id, so its own tooltip"
        );
        let off = Chip::new("s", "Waiting for you")
            .dot(Tone::ActNow)
            .disabled("The job ended");
        let dot = off.status_dot().expect("a dot");
        assert!(dot.is_dimmed());
        assert_eq!(dot.color(), theme::palette().text_dim);
        assert!(!dot.has_id(), "disabled: no id, so no tooltip of its own");
        assert!(Chip::new("n", "None").status_dot().is_none());
        let (_root, wcx) = render_part(cx, || {
            Chip::new("s", "Waiting for you")
                .dot(Tone::ActNow)
                .disabled("The job ended")
        });
        assert!(wcx.debug_bounds("ui-chip-s").is_some());
        // Hovered, the disabled chip's dot shows the chip's reason (no tooltip
        // of its own covers it) ...
        assert_eq!(tooltip_at(wcx, "ui-dot").as_deref(), Some("The job ended"));
        // ... and the enabled chip's dot, with its own id, tooltips its meaning.
        let (_root, wcx) = render_part(cx, || Chip::new("s", "Waiting for you").dot(Tone::ActNow));
        assert_eq!(
            tooltip_at(wcx, "ui-dot").as_deref(),
            Some("Waiting for you")
        );
    }

    /// The hit box: at every tested zoom the chip is at least 44×44.
    #[gpui::test]
    fn chip_hit_box_is_at_least_target_at_every_zoom(cx: &mut TestAppContext) {
        for scale in [0.5_f32, 1.0, 1.25] {
            theme::set_ui_scale(scale);
            let (_root, wcx) = render_part(cx, move || {
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_8))
                    .child(Chip::new("c1", "Gemma · Mac Studio"))
                    .child(Chip::new("c2", "report.md").file("REPORT.md · awili-ui-fishing"))
                    .child(Chip::new("c3", "Status: all good").tone(Tone::ActNow))
            });
            for sel in ["ui-chip-c1", "ui-chip-c2", "ui-chip-c3"] {
                let b = wcx
                    .debug_bounds(sel)
                    .unwrap_or_else(|| panic!("{sel} recorded no bounds at zoom {scale}"));
                assert!(
                    f32::from(b.size.width) >= 44.0 && f32::from(b.size.height) >= 44.0,
                    "{sel}: the hit box is ≥ 44×44 at zoom {scale}, got {b:?}",
                );
            }
        }
        theme::set_ui_scale(1.0);
    }
    /// The file face middle-truncates long names and keeps the full name
    /// as the tooltip (the long-text convention).
    #[test]
    fn file_face_middle_truncates_and_keeps_the_full_name() {
        let n = "a-very-long-file-name-that-should-be-truncated.md";
        assert!(n.len() > FILE_NAME_MAX, "the test name is long enough");
        let chip = Chip::new("f", "file").file(n);
        let shown = chip.display_name();
        assert_eq!(
            shown.chars().count(),
            FILE_NAME_MAX,
            "the ellipsis is counted"
        );
        assert!(shown.starts_with(&n[..FILE_NAME_MAX - FILE_NAME_TAIL - 1]));
        assert!(
            shown.ends_with("truncated.md"),
            "the extension is kept: {shown}"
        );
        assert!(shown.contains('…'));
        assert_eq!(
            chip.tooltip_text().as_deref(),
            Some(n),
            "the full name is the tooltip"
        );
        // A short name passes through unchanged.
        assert_eq!(middle_truncate("REPORT.md"), "REPORT.md");
    }

    /// The tone colours the text and the icon with the tone's tokens, and
    /// the hover lifts a toneless chip to `TEXT`.
    #[test]
    fn tone_colours_text_and_icon() {
        let p = palette();
        let plain = Chip::new("t", "x");
        assert_eq!(plain.paint(), (p.transparent, p.text_dim, p.text_dim));
        assert_eq!(
            plain.hovered(true).paint(),
            (p.fill_hover, p.text, p.text),
            "the hover lifts the text and washes the fill"
        );
        let act = Chip::new("t", "x").tone(Tone::ActNow);
        assert_eq!(
            act.paint(),
            (p.transparent, p.act_now_text, p.act_now),
            "the tone's text and colour tokens"
        );
        let neutral = Chip::new("t", "x").tone(Tone::Neutral);
        assert_eq!(
            neutral.paint(),
            (p.transparent, p.text_dim, p.text_dim),
            "the neutral tone is the dim tokens"
        );
    }

    /// The removable chip: clicking the × fires `on_remove` and not the
    /// chip's own click (the menu trigger).
    #[gpui::test]
    fn removable_chip_remove_never_fires_the_chip_click(cx: &mut TestAppContext) {
        let chip_fired = std::rc::Rc::new(std::cell::Cell::new(0));
        let remove_fired = std::rc::Rc::new(std::cell::Cell::new(0));
        let chip_in = chip_fired.clone();
        let remove_in = remove_fired.clone();
        let (_root, wcx) = render_part(cx, move || {
            let remove_in = remove_in.clone();
            let chip_in = chip_in.clone();

            div().flex().child(
                Chip::new("rm", "ask-before-changes")
                    .removable(move |_ev, _w, _cx| {
                        remove_in.set(1);
                    })
                    .on_click(move |_ev, _w, _cx| {
                        chip_in.set(1);
                    }),
            )
        });
        let chip = wcx
            .debug_bounds("ui-chip-rm")
            .unwrap_or_else(|| panic!("the chip recorded no bounds"));
        // Click the chip body (left of the 44 px × zone): the chip fires.
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: gpui::point(chip.origin.x + theme::z(theme::SP_16), chip.center().y),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: gpui::point(chip.origin.x + theme::z(theme::SP_16), chip.center().y),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert_eq!(chip_fired.get(), 1, "the chip body fires the chip click");
        assert_eq!(remove_fired.get(), 0, "…and not the remove");

        // Click the × (the rightmost 44 px of the chip): remove fires, the
        // chip click does not.
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: gpui::point(
                chip.origin.x + px(f32::from(chip.size.width)) - theme::z(theme::SP_16),
                chip.center().y,
            ),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: gpui::point(
                chip.origin.x + px(f32::from(chip.size.width)) - theme::z(theme::SP_16),
                chip.center().y,
            ),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert_eq!(remove_fired.get(), 1, "the × fires on_remove");
        assert_eq!(chip_fired.get(), 1, "…and the chip's own click is stopped");
    }
    /// A disabled chip is inert and carries its reason.
    #[test]
    fn disabled_chip_is_inert() {
        let c = Chip::new("d", "x").disabled("Read only");
        assert!(c.is_disabled());
        let p = palette();
        assert_eq!(c.paint(), (p.transparent, p.text_dim, p.text_dim));
        assert_eq!(
            c.tooltip_text().as_deref(),
            Some("Read only"),
            "the disabled reason wins the tooltip, ahead of the label"
        );
        assert_eq!(
            c.common().disabled_reason.as_deref(),
            Some("Read only"),
            "the reason is kept for the parent"
        );
    }

    /// The chip's text slot carries the chip's id (KIT.md § Long text: the
    /// truncating `ui::text` shows the full name in its tooltip only with
    /// an id) — the id is a named child of the chip's own id.
    #[test]
    fn chip_text_carries_the_chip_id() {
        use gpui::ElementId;
        let c = Chip::new("c1", "x");
        assert_eq!(
            c.text_id(),
            ElementId::NamedChild(
                std::sync::Arc::new(ElementId::Name("c1".into())),
                "text".into(),
            ),
            "the text slot's id is unique under the chip's id"
        );
        // Two chips: two different text ids.
        assert_ne!(Chip::new("a", "x").text_id(), Chip::new("b", "x").text_id(),);
    }

    /// The text slot is painted with its id (headless render, no panic).
    #[gpui::test]
    fn chip_with_id_text_renders(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(
                Chip::new("ct", "a-very-long-file-name-to-truncate.md")
                    .file("A VERY LONG FILE NAME TO TRUNCATE.md · awili"),
            )
        });
        assert!(
            wcx.debug_bounds("ui-chip-ct").is_some(),
            "the chip with its id'd text slot laid out"
        );
    }

    /// Disabled dims, never deletes: a chip keeps its leading icon
    /// (painted `TEXT_DIM`) and lays out exactly like its enabled twin.
    #[gpui::test]
    fn disabled_chip_keeps_its_icon(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .child(Chip::new("on", "Gemma").icon(Icon::Zap))
                .child(
                    Chip::new("off", "Gemma")
                        .icon(Icon::Zap)
                        .disabled("Read only"),
                )
        });
        let on = wcx
            .debug_bounds("ui-chip-on")
            .unwrap_or_else(|| panic!("the enabled chip recorded no bounds"));
        let off = wcx
            .debug_bounds("ui-chip-off")
            .unwrap_or_else(|| panic!("the disabled chip recorded no bounds"));
        assert_eq!(
            f32::from(on.size.width),
            f32::from(off.size.width),
            "the disabled chip keeps its icon (dimmed, not deleted): \
             on {on:?}, off {off:?}"
        );
    }

    /// Real GPUI hover paints (K2c): the chip text GPUI paints lifts
    /// `TEXT_DIM` → `TEXT` under the pointer and drops back on leave (read
    /// from the paint probe); a toned chip keeps its tone text.
    #[gpui::test]
    fn chip_hover_paints_the_text(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::{hover, painted, same_color, unhover};
        let p = palette();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .gap(theme::z(theme::SP_16))
                .pt(theme::z(theme::SP_40))
                .pl(theme::z(theme::SP_40))
                .child(Chip::new("q", "Hover me"))
                .child(Chip::new("t", "Ready").tone(Tone::Attention))
        });
        let q = child_id(&ElementId::from("q"), "text");
        let t = child_id(&ElementId::from("t"), "text");
        unhover(wcx);
        assert!(
            same_color(painted(&q).unwrap(), p.text_dim),
            "rest is TEXT_DIM"
        );
        hover(wcx, "ui-chip-q");
        assert!(
            same_color(painted(&q).unwrap(), p.text),
            "hover lifts to TEXT"
        );
        unhover(wcx);
        assert!(
            same_color(painted(&q).unwrap(), p.text_dim),
            "leave drops back"
        );
        hover(wcx, "ui-chip-t");
        assert!(
            same_color(painted(&t).unwrap(), theme::tone_text(Tone::Attention)),
            "a toned chip keeps its tone text on hover"
        );
    }

    /// The file chip's text opens the **full** file name, not the
    /// abbreviated one it shows (K2c).
    #[gpui::test]
    fn file_chip_tooltip_is_the_full_name(cx: &mut TestAppContext) {
        let name = "a-really-long-file-name-for-the-middle-truncation-case.rs";
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .pt(theme::z(theme::SP_40))
                .child(Chip::new("f", "file").file(name))
        });
        assert!(
            Chip::new("f", "file").file(name).display_name() != name,
            "the case under test is abbreviated"
        );
        // Rest the pointer on the text slot itself, so the innermost
        // tooltip is the text's own.
        let text_sel: &'static str = Box::leak(
            format!("ui-text-{}", child_id(&ElementId::from("f"), "text")).into_boxed_str(),
        );
        let at = wcx
            .debug_bounds(text_sel)
            .unwrap_or_else(|| panic!("the chip text recorded no bounds"))
            .center();
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at_point(wcx, at).as_deref(),
            Some(name)
        );
    }

    /// A disabled chip: the tooltip that opens over it is the reason (its
    /// text carries no tooltip of its own), and a removable chip keeps its
    /// ×, dimmed, carrying the same reason (K2c).
    #[gpui::test]
    fn disabled_chip_opens_its_reason(cx: &mut TestAppContext) {
        use crate::ui::interaction::test_util::tooltip_at;
        let (_root, wcx) = render_part(cx, move || {
            div().flex().pt(theme::z(theme::SP_40)).child(
                Chip::new("d", "notes.md")
                    .file("notes.md")
                    .removable(|_e, _w, _cx| {})
                    .disabled("Read only"),
            )
        });
        assert_eq!(tooltip_at(wcx, "ui-chip-d").as_deref(), Some("Read only"));
        assert_eq!(
            tooltip_at(wcx, "ui-icon-button-d-remove-btn").as_deref(),
            Some("Read only"),
            "the dimmed × stays and carries the reason"
        );
    }

    /// Tab reaches a chip tracking a plain parent handle (K2c).
    #[gpui::test]
    fn tracked_handle_is_a_tab_stop(cx: &mut TestAppContext) {
        let handle = cx.update(|cx| cx.focus_handle());
        let h = handle.clone();
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(Chip::new("c", "Chip").focus_handle(&h))
        });
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        assert_eq!(wcx.update(|w, cx| w.focused(cx)), Some(handle));
    }
}
