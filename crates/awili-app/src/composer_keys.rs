//! Pure keystroke → caret-motion classification for the composer (U3b1-FIX1b).
//!
//! No GPUI state, no buffer access: one keystroke in, a [`KeyEdit`] out. The
//! `mac` flag selects the chord table — macOS (option = word, command = line /
//! document) vs Linux (control = word). `function` is ignored entirely: on
//! macOS every arrow, Home and End key-down carries it. "Plain" means no
//! control, alt or platform; `shift` sets `extend` on Move and Vertical and is
//! ignored on the delete keys. Any combination not in the table is `Other` —
//! the caller falls through to the existing `key_to_action` path.

use awili_core::Motion;
use gpui::Keystroke;

/// What a classified keystroke asks the composer to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyEdit {
    /// Caret motion; `extend` grows a selection (shift).
    Move { motion: Motion, extend: bool },
    /// Visual-row Up/Down; shift extends.
    Vertical { down: bool, extend: bool },
    /// Backward deletion.
    DeleteBack(DeleteUnit),
    /// Forward deletion.
    DeleteFwd(DeleteUnit),
    /// Not handled here; fall through to `key_to_action`.
    Other,
}

/// The unit a delete key removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeleteUnit {
    /// One character.
    Char,
    /// One word (option / control delete).
    Word,
    /// The whole line behind the caret (⌘⌫, macOS only).
    Line,
}

/// RC2-R4 (D-U3, U3 list "Emacs keys"): whether a key-down is an app chord
/// (new conversation, search, the palettes, paste…). On macOS only ⌘
/// is: a ⌃ key belongs to the text field (⌃A, ⌃E…) and never opens
/// anything. Elsewhere Ctrl is the chord key (and ⌘, which tests send).
pub(crate) fn app_chord(m: &gpui::Modifiers, mac: bool) -> bool {
    m.platform || (!mac && m.control)
}

/// [`app_chord`] for this build's platform.
pub(crate) fn is_app_chord(m: &gpui::Modifiers) -> bool {
    app_chord(m, cfg!(target_os = "macos"))
}

/// Exactly the platform's main modifier and no other one of Control, Command
/// (the other of the pair): ⌘ alone on macOS, Ctrl alone elsewhere. For chords
/// that must not fire when the "other" key is also held (⌘F, ⌘J).
pub(crate) fn main_modifier_only(m: &gpui::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        m.platform && !m.control
    } else {
        m.control && !m.platform
    }
}

/// RC2-R4 (D-U3): the macOS text-field ⌃ keys (Emacs bindings), as the plain
/// key each one stands for: ⌃A line start, ⌃E line end, ⌃B / ⌃F a
/// character back / forward, ⌃P / ⌃N a row up / down, ⌃D delete forward,
/// ⌃H delete back. Shift is kept (it extends a move). `None` for every other
/// key and off macOS.
pub(crate) fn emacs_alias(ks: &Keystroke, mac: bool) -> Option<Keystroke> {
    let m = &ks.modifiers;
    if !mac || !m.control || m.alt || m.platform {
        return None;
    }
    let key = match ks.key.as_str() {
        "a" => "home",
        "e" => "end",
        "b" => "left",
        "f" => "right",
        "p" => "up",
        "n" => "down",
        "d" => "delete",
        "h" => "backspace",
        _ => return None,
    };
    Some(Keystroke {
        modifiers: gpui::Modifiers {
            shift: m.shift,
            ..Default::default()
        },
        key: key.to_string(),
        key_char: None,
    })
}

/// Classify a keystroke under the `mac` (true) or Linux (false) chord table.
pub(crate) fn classify(ks: &Keystroke, mac: bool) -> KeyEdit {
    let m = &ks.modifiers;
    // `function` is ignored on purpose (macOS arrow/Home/End carry it).
    let plain = !m.control && !m.alt && !m.platform;
    let extend = m.shift;
    match (ks.key.as_str(), m.control, m.alt, m.platform) {
        ("left", true, false, false) if !mac => {
            return KeyEdit::Move {
                motion: Motion::WordLeft,
                extend,
            }
        }
        ("left", false, true, false) if mac => {
            return KeyEdit::Move {
                motion: Motion::WordLeft,
                extend,
            }
        }
        ("left", false, false, true) if mac => {
            return KeyEdit::Move {
                motion: Motion::LineStart,
                extend,
            }
        }
        ("right", true, false, false) if !mac => {
            return KeyEdit::Move {
                motion: Motion::WordRight,
                extend,
            }
        }
        ("right", false, true, false) if mac => {
            return KeyEdit::Move {
                motion: Motion::WordRight,
                extend,
            }
        }
        ("right", false, false, true) if mac => {
            return KeyEdit::Move {
                motion: Motion::LineEnd,
                extend,
            }
        }
        ("home", true, false, false) if !mac => {
            return KeyEdit::Move {
                motion: Motion::DocStart,
                extend,
            }
        }
        ("end", true, false, false) if !mac => {
            return KeyEdit::Move {
                motion: Motion::DocEnd,
                extend,
            }
        }
        ("up", false, false, true) if mac => {
            return KeyEdit::Move {
                motion: Motion::DocStart,
                extend,
            }
        }
        ("down", false, false, true) if mac => {
            return KeyEdit::Move {
                motion: Motion::DocEnd,
                extend,
            }
        }
        // Plain (no control / alt / platform; function ignored) navigates on
        // both platforms.
        ("left" | "right" | "home" | "end" | "up" | "down", false, false, false) if plain => {
            return match ks.key.as_str() {
                "left" => KeyEdit::Move {
                    motion: Motion::CharLeft,
                    extend,
                },
                "right" => KeyEdit::Move {
                    motion: Motion::CharRight,
                    extend,
                },
                "home" => KeyEdit::Move {
                    motion: Motion::LineStart,
                    extend,
                },
                "end" => KeyEdit::Move {
                    motion: Motion::LineEnd,
                    extend,
                },
                "up" => KeyEdit::Vertical {
                    down: false,
                    extend,
                },
                _ => KeyEdit::Vertical { down: true, extend },
            }
        }
        ("backspace", true, false, false) if !mac => return KeyEdit::DeleteBack(DeleteUnit::Word),
        ("backspace", false, true, false) if mac => return KeyEdit::DeleteBack(DeleteUnit::Word),
        ("backspace", false, false, true) if mac => return KeyEdit::DeleteBack(DeleteUnit::Line),
        ("backspace", false, false, false) if plain => {
            return KeyEdit::DeleteBack(DeleteUnit::Char)
        }
        ("delete", false, true, false) if mac => return KeyEdit::DeleteFwd(DeleteUnit::Word),
        ("delete", true, false, false) if !mac => return KeyEdit::DeleteFwd(DeleteUnit::Word),
        ("delete", false, false, false) if plain => return KeyEdit::DeleteFwd(DeleteUnit::Char),
        _ => {}
    }
    KeyEdit::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(
        key: &str,
        shift: bool,
        alt: bool,
        platform: bool,
        control: bool,
        function: bool,
    ) -> Keystroke {
        Keystroke {
            modifiers: gpui::Modifiers {
                shift,
                alt,
                platform,
                control,
                function,
            },
            key: key.to_string(),
            key_char: None,
        }
    }

    #[test]
    fn u3b1_classify_mac_and_linux_tables() {
        for mac in [true, false] {
            // left/right: plain.
            assert_eq!(
                classify(&k("left", false, false, false, false, false), mac),
                KeyEdit::Move {
                    motion: Motion::CharLeft,
                    extend: false
                }
            );
            assert_eq!(
                classify(&k("right", false, false, false, false, false), mac),
                KeyEdit::Move {
                    motion: Motion::CharRight,
                    extend: false
                }
            );
            // left/right: alt only.
            assert_eq!(
                classify(&k("left", false, true, false, false, false), mac),
                if mac {
                    KeyEdit::Move {
                        motion: Motion::WordLeft,
                        extend: false,
                    }
                } else {
                    KeyEdit::Other
                }
            );
            assert_eq!(
                classify(&k("right", false, true, false, false, false), mac),
                if mac {
                    KeyEdit::Move {
                        motion: Motion::WordRight,
                        extend: false,
                    }
                } else {
                    KeyEdit::Other
                }
            );
            // left/right: control only.
            assert_eq!(
                classify(&k("left", false, false, false, true, false), mac),
                if mac {
                    KeyEdit::Other
                } else {
                    KeyEdit::Move {
                        motion: Motion::WordLeft,
                        extend: false,
                    }
                }
            );
            assert_eq!(
                classify(&k("right", false, false, false, true, false), mac),
                if mac {
                    KeyEdit::Other
                } else {
                    KeyEdit::Move {
                        motion: Motion::WordRight,
                        extend: false,
                    }
                }
            );
            // left/right: platform only.
            assert_eq!(
                classify(&k("left", false, false, true, false, false), mac),
                if mac {
                    KeyEdit::Move {
                        motion: Motion::LineStart,
                        extend: false,
                    }
                } else {
                    KeyEdit::Other
                }
            );
            assert_eq!(
                classify(&k("right", false, false, true, false, false), mac),
                if mac {
                    KeyEdit::Move {
                        motion: Motion::LineEnd,
                        extend: false,
                    }
                } else {
                    KeyEdit::Other
                }
            );
            // home/end: plain.
            assert_eq!(
                classify(&k("home", false, false, false, false, false), mac),
                KeyEdit::Move {
                    motion: Motion::LineStart,
                    extend: false
                }
            );
            assert_eq!(
                classify(&k("end", false, false, false, false, false), mac),
                KeyEdit::Move {
                    motion: Motion::LineEnd,
                    extend: false
                }
            );
            // home/end: control only.
            assert_eq!(
                classify(&k("home", false, false, false, true, false), mac),
                if mac {
                    KeyEdit::Other
                } else {
                    KeyEdit::Move {
                        motion: Motion::DocStart,
                        extend: false,
                    }
                }
            );
            assert_eq!(
                classify(&k("end", false, false, false, true, false), mac),
                if mac {
                    KeyEdit::Other
                } else {
                    KeyEdit::Move {
                        motion: Motion::DocEnd,
                        extend: false,
                    }
                }
            );
            // up/down: plain.
            assert_eq!(
                classify(&k("up", false, false, false, false, false), mac),
                KeyEdit::Vertical {
                    down: false,
                    extend: false
                }
            );
            assert_eq!(
                classify(&k("down", false, false, false, false, false), mac),
                KeyEdit::Vertical {
                    down: true,
                    extend: false
                }
            );
            // up/down: platform only.
            assert_eq!(
                classify(&k("up", false, false, true, false, false), mac),
                if mac {
                    KeyEdit::Move {
                        motion: Motion::DocStart,
                        extend: false,
                    }
                } else {
                    KeyEdit::Other
                }
            );
            assert_eq!(
                classify(&k("down", false, false, true, false, false), mac),
                if mac {
                    KeyEdit::Move {
                        motion: Motion::DocEnd,
                        extend: false,
                    }
                } else {
                    KeyEdit::Other
                }
            );
            // backspace: plain / alt only / platform only / control only.
            assert_eq!(
                classify(&k("backspace", false, false, false, false, false), mac),
                KeyEdit::DeleteBack(DeleteUnit::Char)
            );
            assert_eq!(
                classify(&k("backspace", false, true, false, false, false), mac),
                if mac {
                    KeyEdit::DeleteBack(DeleteUnit::Word)
                } else {
                    KeyEdit::Other
                }
            );
            assert_eq!(
                classify(&k("backspace", false, false, true, false, false), mac),
                if mac {
                    KeyEdit::DeleteBack(DeleteUnit::Line)
                } else {
                    KeyEdit::Other
                }
            );
            assert_eq!(
                classify(&k("backspace", false, false, false, true, false), mac),
                if mac {
                    KeyEdit::Other
                } else {
                    KeyEdit::DeleteBack(DeleteUnit::Word)
                }
            );
            // delete: plain / alt only / control only.
            assert_eq!(
                classify(&k("delete", false, false, false, false, false), mac),
                KeyEdit::DeleteFwd(DeleteUnit::Char)
            );
            assert_eq!(
                classify(&k("delete", false, true, false, false, false), mac),
                if mac {
                    KeyEdit::DeleteFwd(DeleteUnit::Word)
                } else {
                    KeyEdit::Other
                }
            );
            assert_eq!(
                classify(&k("delete", false, false, false, true, false), mac),
                if mac {
                    KeyEdit::Other
                } else {
                    KeyEdit::DeleteFwd(DeleteUnit::Word)
                }
            );
            // enter and a plain letter are always Other.
            assert_eq!(
                classify(&k("enter", false, false, false, false, false), mac),
                KeyEdit::Other
            );
            assert_eq!(
                classify(&k("a", false, false, false, false, false), mac),
                KeyEdit::Other
            );
        }
        // shift sets `extend` and never changes the motion.
        let shift_moves = [
            (
                k("left", true, false, false, false, false),
                Motion::CharLeft,
            ),
            (
                k("right", true, false, false, false, false),
                Motion::CharRight,
            ),
            (
                k("home", true, false, false, false, false),
                Motion::LineStart,
            ),
            (k("end", true, false, false, false, false), Motion::LineEnd),
        ];
        for (ks, m) in shift_moves {
            for mac in [true, false] {
                assert_eq!(
                    classify(&ks, mac),
                    KeyEdit::Move {
                        motion: m,
                        extend: true
                    }
                );
            }
        }
        for mac in [true, false] {
            assert_eq!(
                classify(&k("left", true, true, false, false, false), mac),
                if mac {
                    KeyEdit::Move {
                        motion: Motion::WordLeft,
                        extend: true,
                    }
                } else {
                    KeyEdit::Other
                }
            );
            assert_eq!(
                classify(&k("up", true, false, false, false, false), mac),
                KeyEdit::Vertical {
                    down: false,
                    extend: true
                }
            );
            assert_eq!(
                classify(&k("down", true, false, false, false, false), mac),
                KeyEdit::Vertical {
                    down: true,
                    extend: true
                }
            );
        }
        // function is completely ignored.
        for mac in [true, false] {
            assert_eq!(
                classify(&k("left", false, false, false, false, true), mac),
                classify(&k("left", false, false, false, false, false), mac)
            );
            assert_eq!(
                classify(&k("up", false, false, false, false, true), mac),
                classify(&k("up", false, false, false, false, false), mac)
            );
            assert_eq!(
                classify(&k("home", false, false, false, false, true), mac),
                classify(&k("home", false, false, false, false, false), mac)
            );
        }
        // ⌃⌘← is Other on both platforms.
        for mac in [true, false] {
            assert_eq!(
                classify(&k("left", false, false, true, true, false), mac),
                KeyEdit::Other
            );
        }
    }

    #[test]
    fn u3b1_shift_extends_every_move_chord() {
        // (key, alt, platform, control, expected motion) per platform table.
        let mac_rows: [(&str, bool, bool, bool, Motion); 10] = [
            ("left", false, false, false, Motion::CharLeft),
            ("right", false, false, false, Motion::CharRight),
            ("home", false, false, false, Motion::LineStart),
            ("end", false, false, false, Motion::LineEnd),
            ("left", true, false, false, Motion::WordLeft),
            ("right", true, false, false, Motion::WordRight),
            ("left", false, true, false, Motion::LineStart),
            ("right", false, true, false, Motion::LineEnd),
            ("up", false, true, false, Motion::DocStart),
            ("down", false, true, false, Motion::DocEnd),
        ];
        let linux_rows: [(&str, bool, bool, bool, Motion); 8] = [
            ("left", false, false, false, Motion::CharLeft),
            ("right", false, false, false, Motion::CharRight),
            ("home", false, false, false, Motion::LineStart),
            ("end", false, false, false, Motion::LineEnd),
            ("left", false, false, true, Motion::WordLeft),
            ("right", false, false, true, Motion::WordRight),
            ("home", false, false, true, Motion::DocStart),
            ("end", false, false, true, Motion::DocEnd),
        ];
        for (mac, rows) in [(true, &mac_rows[..]), (false, &linux_rows[..])] {
            for &(key, alt, platform, control, motion) in rows {
                // macOS arrow/Home/End key-downs carry `function`; it must not matter.
                for function in [false, true] {
                    for shift in [false, true] {
                        assert_eq!(
                            classify(&k(key, shift, alt, platform, control, function), mac),
                            KeyEdit::Move {
                                motion,
                                extend: shift
                            },
                            "mac={mac} key={key} alt={alt} cmd={platform} ctrl={control} fn={function} shift={shift}"
                        );
                    }
                }
            }
        }
        // Shift+Down/Up stay Vertical with extend on both platforms.
        for mac in [true, false] {
            for (key, down) in [("up", false), ("down", true)] {
                assert_eq!(
                    classify(&k(key, true, false, false, false, false), mac),
                    KeyEdit::Vertical { down, extend: true }
                );
            }
        }
    }

    /// RC2-R4 (D-U3): ⌘ is the app chord on macOS, never ⌃; Ctrl (and
    /// ⌘) elsewhere. The macOS ⌃ Emacs keys stand for plain keys (shift
    /// kept); other keys and other systems have none.
    #[test]
    fn rc2r4_control_is_not_command_on_macos() {
        let m = |platform, control| gpui::Modifiers {
            platform,
            control,
            ..Default::default()
        };
        assert!(app_chord(&m(true, false), true));
        assert!(!app_chord(&m(false, true), true));
        assert!(app_chord(&m(false, true), false));
        assert!(app_chord(&m(true, false), false));
        assert!(!app_chord(&m(false, false), false));
        for (key, plain) in [
            ("a", "home"),
            ("e", "end"),
            ("b", "left"),
            ("f", "right"),
            ("p", "up"),
            ("n", "down"),
            ("d", "delete"),
            ("h", "backspace"),
        ] {
            let got = emacs_alias(&k(key, true, false, false, true, false), true).unwrap();
            assert_eq!(got.key, plain);
            assert!(got.modifiers.shift && !got.modifiers.control);
            assert!(emacs_alias(&k(key, false, false, false, true, false), false).is_none());
            assert!(emacs_alias(&k(key, false, false, true, false, false), true).is_none());
        }
        assert!(emacs_alias(&k("k", false, false, false, true, false), true).is_none());
    }
}

#[cfg(test)]
#[path = "composer_keys_app_tests.rs"]
mod app_tests;

/// Test helper: a simulated keystroke string written with ⌘ (`cmd-…`), as the
/// platform's main modifier: unchanged on macOS, `ctrl-…` elsewhere (where
/// `secondary` is Ctrl), so one test covers all three OSes.
#[cfg(test)]
pub(crate) fn plat_keys(keys: &str) -> String {
    if cfg!(target_os = "macos") {
        keys.to_string()
    } else {
        keys.replace("cmd-", "ctrl-")
    }
}
