//! Native application menus.
//!
//! The menu actions intentionally stay thin: window-owned actions are handled
//! by `AwiliApp`, while application/window lifecycle actions are registered once
//! at startup.

use gpui::{actions, App, Menu, MenuItem, OsAction, SystemMenuType};

actions!(
    awili_menu,
    [
        About,
        ActualSize,
        CloseWindow,
        Copy,
        Cut,
        ExportConversation,
        SaveConversationAs,
        Find,
        Hide,
        HideOthers,
        Minimize,
        NewConversation,
        OpenCommandPalette,
        OpenSettings,
        Paste,
        Quit,
        Redo,
        SelectAll,
        ShowAll,
        ShowShortcuts,
        ToggleNeedsYou,
        Undo,
        Zoom,
        ZoomIn,
        ZoomOut,
    ]
);

pub fn install(cx: &mut App) {
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Quit, cx| request_quit(cx));
    cx.set_menus(menus());
}

/// Shared by explicit Quit and non-macOS window close. Linux has no app-hide
/// implementation, so Close must follow the same confirmation as Quit.
pub(crate) fn request_quit(cx: &mut App) {
    // U3d D8: confirm before quitting when runs are in flight. The
    // deferred pattern mirrors the other app-level menu actions so the
    // update runs after the current window pass returns the window.
    let Some(handle) = cx.try_global::<crate::AwiliWindow>().map(|w| w.0) else {
        // No window yet (pre-launch) — quit directly.
        cx.quit();
        return;
    };
    cx.defer(move |cx| {
        if let Err(error) = handle.update(cx, |app, window, cx| app.request_quit(window, cx)) {
            eprintln!("Quit action failed: {error:#}");
        }
    });
}

fn menus() -> Vec<Menu> {
    vec![
        Menu::new("Awili").items([
            MenuItem::action("About Awili", About),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Awili", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Awili", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Conversation", NewConversation),
            MenuItem::separator(),
            MenuItem::action("Save Conversation to Downloads", ExportConversation),
            MenuItem::action("Save Conversation As…", SaveConversationAs),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            // Paste stays a plain action (no ⌘V key-equivalent on the menu item):
            // the composer + the capture-phase paste-attach handler already own
            // ⌘V, and an os_action key-equivalent would preempt AppKit-first and
            // break pasting a file/image from the clipboard (slice 19 §5). The
            // menu item still pastes; the shortcut works via the existing path.
            MenuItem::action("Paste", Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find…", Find),
        ]),
        Menu::new("View").items([
            // Interface zoom (A4). The ⌘+/⌘-/⌘0 chords are bound in the app's
            // root key handler (like ⌘N / ⌘K), so these items stay plain actions
            // that also work from the menu.
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::action("Actual Size", ActualSize),
            MenuItem::separator(),
            MenuItem::action("Command Palette…", OpenCommandPalette),
            MenuItem::action("Toggle Needs You", ToggleNeedsYou),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
        Menu::new("Help").items([MenuItem::action("Awili Keyboard Shortcuts", ShowShortcuts)]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_the_standard_macos_menu_set() {
        let names: Vec<_> = menus()
            .into_iter()
            .map(|menu| menu.name.to_string())
            .collect();
        assert_eq!(names, ["Awili", "File", "Edit", "View", "Window", "Help"]);
    }
}
