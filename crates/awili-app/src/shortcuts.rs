//! The ⌘? keyboard-shortcut reference's pure state and content (slice 20 §2).
//! No GPUI here — the toggle/dismiss rules and the table content are the
//! invariants the spec asks to unit-test, so they live where a test can drive
//! them without a window (the `PaletteState` pattern).

/// The sheet's title.
pub const TITLE: &str = "Keyboard Shortcuts";

/// Every shortcut the app binds, in display order: `(combo, action)`. The
/// sheet lists ⌘? itself (spec §2a). On Linux the platform chord is Ctrl;
/// the ⌘ glyph stands for "the platform modifier" exactly like the palette's
/// row hints already do.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("⌘N", "New conversation"),
    ("⌘K", "Command palette"),
    ("⇧⌘K", "Switch conversation (the title menu)"),
    ("⌘\\", "Show or hide the Conversations panel"),
    ("⌥⌘C", "Copy the conversation"),
    ("⌘F", "Full-text search"),
    ("⌘O", "Set working folder by path"),
    ("⌘E", "Choose the helper"),
    ("⌘,", "Settings"),
    ("⌘⇧C", "Save conversation to Downloads"),
    ("⌘⇧S", "Save conversation to a folder you choose"),
    ("⌘V", "Attach from clipboard (file/image)"),
    ("⌘. / Esc", "Stop streaming / cancel Work run"),
    ("⌘W", "Hide the window (runs keep going)"),
    ("⌘Q", "Quit (asks first if runs are in progress)"),
    ("⌘H", "Hide Awili"),
    ("⌘Z / ⌘⇧Z", "Undo / redo"),
    ("⌥← ⌥→  ⌘← ⌘→", "Word / line jump — add ⇧ to select"),
    ("⌥⌫ / ⌘⌫", "Delete word / to line start"),
    ("Right-click or ⌃-click", "Cut, Copy, Paste, Select All"),
    ("⌘?", "This shortcut reference"),
    (
        "Hold ⌥ Option (or click the mic)",
        "Dictate — release to insert",
    ),
];

/// Windows and Linux spell the chords out ("Ctrl+Shift+K"): there is no ⌘, and
/// the app's main modifier there is Ctrl. This turns the macOS glyph spelling
/// every hint and tooltip is written in into that, in the order Ctrl, Alt,
/// Shift. Pure, so it is tested on every platform; [`localize`] applies it only
/// off macOS.
pub fn translate(s: &str) -> String {
    // The command catalog spells its hints "Cmd+N" (on every platform today).
    let s = s.replace("Cmd+", "Ctrl+");
    let mut out = String::with_capacity(s.len() + 8);
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    let flush = |out: &mut String, ctrl: &mut bool, alt: &mut bool, shift: &mut bool| {
        if *ctrl {
            out.push_str("Ctrl+");
        }
        if *alt {
            out.push_str("Alt+");
        }
        if *shift {
            out.push_str("Shift+");
        }
        *ctrl = false;
        *alt = false;
        *shift = false;
    };
    for c in s.chars() {
        match c {
            '⌘' | '⌃' => ctrl = true,
            '⌥' => alt = true,
            '⇧' => shift = true,
            '⌫' => {
                flush(&mut out, &mut ctrl, &mut alt, &mut shift);
                out.push_str("Backspace");
            }
            other => {
                flush(&mut out, &mut ctrl, &mut alt, &mut shift);
                out.push(other);
            }
        }
    }
    // A trailing modifier with no key ("Ctrl-click" forms) stays readable.
    flush(&mut out, &mut ctrl, &mut alt, &mut shift);
    // "Return" becomes "Enter" only as the key of a chord ('Ctrl+Return'), never in
    // prose: toasts and tooltips go through here too ("Return to the list").
    out.replace("Ctrl+Return", "Ctrl+Enter")
        .replace("Alt+Return", "Alt+Enter")
        .replace("Shift+Return", "Shift+Enter")
}

/// A hint or tooltip as it should read on this platform: unchanged on macOS,
/// spelled out ("Ctrl+N") elsewhere.
pub fn localize(s: &str) -> std::borrow::Cow<'_, str> {
    if cfg!(target_os = "macos") {
        std::borrow::Cow::Borrowed(s)
    } else {
        std::borrow::Cow::Owned(translate(s))
    }
}

/// [`localize`] for the `&'static str` shortcut slots of the context menus.
/// The few chords those menus show are spelled out by name; macOS gets the
/// string back unchanged.
pub fn localized_static(s: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        return s;
    }
    match s {
        "⌘X" => "Ctrl+X",
        "⌘C" => "Ctrl+C",
        "⌘V" => "Ctrl+V",
        "⌘A" => "Ctrl+A",
        // Not expected; kept correct rather than wrong (a few bytes, once).
        other => Box::leak(translate(other).into_boxed_str()),
    }
}

/// The shortcut sheet's rows for this platform. macOS: exactly [`SHORTCUTS`].
/// Elsewhere the rows that only exist on a Mac (Hide, ⌃-click, the Option
/// dictation key) are dropped or reworded and every chord is spelled out.
pub fn rows() -> Vec<(String, &'static str)> {
    SHORTCUTS
        .iter()
        .filter_map(|(combo, action)| {
            if cfg!(target_os = "macos") {
                return Some(((*combo).to_string(), *action));
            }
            match *combo {
                "⌘H" => None,
                "Right-click or ⌃-click" => Some(("Right-click".to_string(), *action)),
                "Hold ⌥ Option (or click the mic)" => {
                    Some(("Hold Alt (or click the mic)".to_string(), *action))
                }
                other => Some((translate(other), *action)),
            }
        })
        .collect()
}

/// Whether the sheet is showing — the spec's `show_shortcuts` flag, carried
/// as a tiny state struct so the toggle/dismiss rules are testable.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShortcutsState {
    pub open: bool,
}

impl ShortcutsState {
    pub fn new() -> Self {
        Self { open: false }
    }

    /// ⌘? — open if closed, close if open (spec §2b).
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// Escape or the ✕ button dismiss the sheet.
    pub fn close(&mut self) {
        self.open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_overlay_toggle() {
        let mut s = ShortcutsState::new();
        assert!(!s.open, "closed by default");
        s.toggle();
        assert!(s.open, "first ⌘? opens");
        s.toggle();
        assert!(!s.open, "second ⌘? closes");
    }

    #[test]
    fn shortcuts_sheet_opens_on_cmd_question() {
        let mut s = ShortcutsState::new();
        s.toggle();
        assert!(s.open);
    }

    #[test]
    fn shortcut_overlay_escape_closes() {
        let mut s = ShortcutsState::new();
        s.toggle();
        s.close();
        assert!(!s.open);
    }

    #[test]
    fn shortcuts_sheet_closes_on_escape() {
        let mut s = ShortcutsState { open: true };
        s.close();
        assert!(!s.open);
        // A second Escape (already closed) stays closed.
        s.close();
        assert!(!s.open);
    }

    #[test]
    fn shortcuts_sheet_lists_cmd_n() {
        assert!(SHORTCUTS
            .iter()
            .any(|(combo, action)| *combo == "⌘N" && *action == "New conversation"));
    }

    #[test]
    fn u3b2_shortcuts_row_is_hold_to_dictate() {
        // U3b2: the PTT row now documents hold-to-talk — a *hold* (not a
        // tap) starts dictation and releasing inserts.
        assert!(SHORTCUTS.iter().any(|(combo, action)| *combo
            == "Hold ⌥ Option (or click the mic)"
            && *action == "Dictate — release to insert"));
        // And no stale "toggle" row is left behind.
        assert!(!SHORTCUTS
            .iter()
            .any(|(_, action)| *action == "Toggle voice input"));
    }

    #[test]
    fn shortcuts_sheet_lists_itself() {
        // Spec §2a: ⌘? appears in its own table.
        assert!(SHORTCUTS.iter().any(|(combo, _)| *combo == "⌘?"));
        // And the stop shortcut added by the same slice is listed too.
        assert!(SHORTCUTS
            .iter()
            .any(|(combo, action)| combo.contains("⌘.") && action.contains("Stop")));
    }
}

#[cfg(test)]
mod platform_spelling_tests {
    use super::*;

    #[test]
    fn glyph_chords_become_spelled_out_chords() {
        assert_eq!(translate("⌘N"), "Ctrl+N");
        assert_eq!(translate("⇧⌘K"), "Ctrl+Shift+K");
        assert_eq!(translate("⌘⇧C"), "Ctrl+Shift+C");
        assert_eq!(translate("⌥⌘C"), "Ctrl+Alt+C");
        assert_eq!(translate("⌘,"), "Ctrl+,");
        assert_eq!(translate("⌘?"), "Ctrl+?");
        assert_eq!(translate("⌘\\"), "Ctrl+\\");
        assert_eq!(translate("⌘. / Esc"), "Ctrl+. / Esc");
        assert_eq!(translate("⌘Z / ⌘⇧Z"), "Ctrl+Z / Ctrl+Shift+Z");
        assert_eq!(translate("⌥⌫ / ⌘⌫"), "Alt+Backspace / Ctrl+Backspace");
        assert_eq!(translate("⌥← ⌥→  ⌘← ⌘→"), "Alt+← Alt+→  Ctrl+← Ctrl+→");
        assert_eq!(translate("Stop — ⌘. or Esc"), "Stop — Ctrl+. or Esc");
        assert_eq!(translate("Open Work (⌘Return)"), "Open Work (Ctrl+Enter)");
        assert_eq!(translate("⌘⇧Return"), "Ctrl+Shift+Enter");
        // Prose that merely contains the word is left alone.
        assert_eq!(
            translate("Return to the list, or press Return"),
            "Return to the list, or press Return"
        );
        assert_eq!(
            translate("Press Return to send (⌘Return opens Work)"),
            "Press Return to send (Ctrl+Enter opens Work)"
        );
        // Text without glyphs is untouched.
        assert_eq!(translate("Settings"), "Settings");
        // The command catalog's own "Cmd+N" spelling.
        assert_eq!(translate("Cmd+N"), "Ctrl+N");
        assert_eq!(translate("Cmd+,"), "Ctrl+,");
    }

    #[test]
    fn localize_is_the_identity_on_macos() {
        if cfg!(target_os = "macos") {
            assert_eq!(localize("⌘N"), "⌘N");
        } else {
            assert_eq!(localize("⌘N"), "Ctrl+N");
        }
    }

    #[test]
    fn the_sheet_rows_match_the_platform() {
        let r = rows();
        if cfg!(target_os = "macos") {
            assert_eq!(r.len(), SHORTCUTS.len());
            assert_eq!(r[0].0, "⌘N");
        } else {
            assert!(r
                .iter()
                .all(|(c, _)| !c.contains('⌘') && !c.contains('⌥') && !c.contains('⇧')));
            assert!(r
                .iter()
                .any(|(c, a)| c == "Ctrl+N" && *a == "New conversation"));
            assert!(!r.iter().any(|(_, a)| *a == "Hide Awili"));
        }
    }
}
