//! U3a1: the one context menu, a gpui overlay (D1). A field calls [`open`] with
//! its rows; the root view mounts [`host_element`] once. U3a2 reuses this API.

use std::rc::Rc;

use gpui::{
    prelude::*, AnyElement, App, FocusHandle, Global, Modifiers, MouseButton, Pixels, Point,
    SharedString, Window,
};

/// A row's action; it runs after the menu closed and focus went back.
pub type MenuRun = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub enum MenuEntry {
    Item {
        label: SharedString,
        shortcut: Option<&'static str>,
        enabled: bool,
        run: MenuRun,
    },
    Separator,
}

impl MenuEntry {
    pub fn item(label: &'static str, key: Option<&'static str>, on: bool, run: MenuRun) -> Self {
        let (label, shortcut, enabled) = (label.into(), key, on);
        MenuEntry::Item {
            label,
            shortcut,
            enabled,
            run,
        }
    }
}

/// Keyboard navigation over the enabled items; no wrap at the ends (NSMenu).
pub struct MenuModel {
    entries: Vec<MenuEntry>,
    highlighted: Option<usize>,
}

impl MenuModel {
    pub fn new(entries: Vec<MenuEntry>) -> Self {
        let highlighted = None;
        Self {
            entries,
            highlighted,
        }
    }

    #[cfg(test)]
    fn run_at(&self, i: usize) -> Option<MenuRun> {
        match &self.entries[i] {
            MenuEntry::Item { enabled, run, .. } if *enabled => Some(run.clone()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn down(&mut self) {
        let from = self.highlighted.map_or(0, |h| h + 1);
        let next = (from..self.entries.len()).find(|&i| self.run_at(i).is_some());
        self.highlighted = next.or(self.highlighted);
    }

    #[cfg(test)]
    pub fn up(&mut self) {
        let to = self.highlighted.unwrap_or(self.entries.len());
        let prev = (0..to).rev().find(|&i| self.run_at(i).is_some());
        self.highlighted = prev.or(self.highlighted);
    }

    #[cfg(test)]
    pub fn highlighted_run(&self) -> Option<MenuRun> {
        self.highlighted.and_then(|i| self.run_at(i))
    }
}

/// The open menu, if any (an app global).
#[derive(Default)]
pub struct ContextMenuHost(Option<OpenMenu>);

impl Global for ContextMenuHost {}

struct OpenMenu {
    model: MenuModel,
    position: Point<Pixels>,
    focus: FocusHandle,
    restore: Option<FocusHandle>,
    popover_state: crate::ui::PopoverState,
}

fn menu(cx: &mut App) -> &mut Option<OpenMenu> {
    &mut cx.default_global::<ContextMenuHost>().0
}

/// `Right` with any modifiers, or (macOS only) a plain control-Left click.
pub fn is_context_click(button: MouseButton, m: &Modifiers, mac: bool) -> bool {
    button == MouseButton::Right
        || (mac && button == MouseButton::Left && m.control && !m.platform && !m.alt)
}

/// Show `entries` at `position` with the keyboard on the menu. Replacing an
/// open menu keeps the focus it will restore.
pub fn open(position: Point<Pixels>, entries: Vec<MenuEntry>, window: &mut Window, cx: &mut App) {
    let previous = menu(cx).take();
    let restore = previous.map_or_else(|| window.focused(cx), |m| m.restore);
    let (model, focus) = (MenuModel::new(entries), cx.focus_handle());
    window.focus(&focus, cx);
    *menu(cx) = Some(OpenMenu {
        model,
        position,
        focus,
        restore,
        popover_state: crate::ui::PopoverState::new(),
    });
    window.refresh();
}

/// Drop the menu and give focus back to the field that opened it.
pub fn close(window: &mut Window, cx: &mut App) {
    if let Some(open) = menu(cx).take() {
        open.restore
            .iter()
            .for_each(|restore| window.focus(restore, cx));
        window.refresh();
    }
}

pub fn is_open(cx: &App) -> bool {
    cx.try_global::<ContextMenuHost>()
        .is_some_and(|h| h.0.is_some())
}

/// The menu layer for the root render. `None` while closed; an inactive
/// window closes the menu (it never survives app deactivation).
pub fn host_element(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    if !is_open(cx) || !window.is_window_active() {
        close(window, cx);
        return None;
    }
    let open = menu(cx).as_ref()?;
    let mut kit_menu = crate::ui::Menu::new("context-menu")
        .selector("context-menu")
        .anchor_point(open.position)
        .popover_state(open.popover_state.clone())
        .selected(open.model.highlighted.unwrap_or(usize::MAX))
        .wrap_navigation(false)
        .focus_handle(&open.focus)
        .on_move(|index, window, cx| {
            if let Some(open) = menu(cx).as_mut() {
                open.model.highlighted = Some(index);
                window.refresh();
            }
        })
        .on_close(close);
    if let Some(trigger) = &open.restore {
        kit_menu = kit_menu.trigger_focus(trigger);
    }
    for entry in &open.model.entries {
        match entry {
            MenuEntry::Separator => kit_menu = kit_menu.separator(),
            MenuEntry::Item {
                label,
                shortcut,
                enabled,
                run,
            } => {
                let mut item = crate::ui::MenuItem::new(label.clone());
                if let Some(shortcut) = shortcut {
                    item = item.shortcut(*shortcut);
                }
                if !*enabled {
                    item = item.disabled("This action is unavailable.");
                }
                let run = run.clone();
                item = item.on_click(move |_, window, cx| run(window, cx));
                kit_menu = kit_menu.item(item);
            }
        }
    }
    // Native context menus dismiss when the user presses a key the menu does
    // not handle. Keep this behavior in the adapter so the shared KIT Menu
    // retains its reusable keyboard contract. The wrapper is an absolute
    // full-window host so it stays out of page flow and preserves the Menu's
    // window-coordinate anchor.
    let host = gpui::div()
        .absolute()
        .top(Pixels::ZERO)
        .left(Pixels::ZERO)
        .size_full()
        .on_key_down(
            |ev: &gpui::KeyDownEvent, window, cx| match ev.keystroke.key.as_str() {
                "escape" | "up" | "down" | "left" | "right" | "enter" | "space" | " " => {}
                _ => {
                    window.prevent_default();
                    cx.stop_propagation();
                    close(window, cx);
                }
            },
        )
        .child(kit_menu);
    Some(host.into_any_element())
}

/// Test view of the open menu: `(label, enabled)` per row, `"-"` for a separator.
#[cfg(test)]
pub fn rows(cx: &App) -> Option<Vec<(String, bool)>> {
    let open = cx.try_global::<ContextMenuHost>()?.0.as_ref()?;
    let row = |e: &MenuEntry| match e {
        MenuEntry::Item { label, enabled, .. } => (label.to_string(), *enabled),
        MenuEntry::Separator => ("-".to_string(), false),
    };
    Some(open.model.entries.iter().map(row).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &'static str, enabled: bool) -> MenuEntry {
        MenuEntry::item(label, None, enabled, Rc::new(|_, _| {}))
    }

    #[test]
    fn u3a1_context_click_predicate() {
        let none = Modifiers::default();
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        let cmd_ctrl = Modifiers {
            control: true,
            platform: true,
            ..Default::default()
        };
        let alt_ctrl = Modifiers {
            control: true,
            alt: true,
            ..Default::default()
        };
        for mac in [true, false] {
            assert!(is_context_click(MouseButton::Right, &none, mac));
            assert!(is_context_click(MouseButton::Right, &shift, mac));
            assert!(is_context_click(MouseButton::Right, &ctrl, mac));
            assert!(!is_context_click(MouseButton::Left, &none, mac));
            assert!(!is_context_click(MouseButton::Left, &cmd_ctrl, mac));
            assert!(!is_context_click(MouseButton::Left, &alt_ctrl, mac));
            assert!(!is_context_click(MouseButton::Middle, &ctrl, mac));
        }
        assert!(is_context_click(MouseButton::Left, &ctrl, true));
        assert!(!is_context_click(MouseButton::Left, &ctrl, false));
    }

    #[test]
    fn u3a1_menu_model_skips_disabled_and_separators() {
        let mut model = MenuModel::new(vec![
            item("Cut", false),
            item("Copy", false),
            item("Paste", true),
            MenuEntry::Separator,
            item("Select All", true),
        ]);
        assert!(model.highlighted_run().is_none(), "nothing highlighted yet");
        model.down();
        assert_eq!(model.highlighted, Some(2), "first Down highlights Paste");
        model.down();
        assert_eq!(model.highlighted, Some(4), "Down skips the separator");
        model.down();
        assert_eq!(model.highlighted, Some(4), "Down stops at the end");
        model.up();
        assert_eq!(model.highlighted, Some(2), "Up skips the separator");
        model.up();
        assert_eq!(
            model.highlighted,
            Some(2),
            "Up skips disabled rows and stops"
        );
        assert!(model.highlighted_run().is_some());
        let mut fresh = MenuModel::new(vec![item("A", true), item("B", true)]);
        fresh.up();
        assert_eq!(fresh.highlighted, Some(1), "Up from nothing: last enabled");
    }
}
