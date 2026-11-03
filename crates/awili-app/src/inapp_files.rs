//! RC1-H1: Awili's own record of the files an in-app (owned engine) turn
//! changed. It is built from the engine's tool events — a file-changing tool
//! that started and then finished successfully — and never from the model's
//! words, so a reply that only CLAIMS a change ("I have created utf8.txt …
//! Files changed: utf8.txt") is shown with "no files changed" under it.

use awili_work::WorkEvent;

/// RC1-O-FIX3/FIX4 (pre-review 6 P2, slice review P2): whether `c` would not
/// be seen as itself — the approval card's own rule
/// (`awili_engine_worker::shown_as_is`: letters, marks, numbers, punctuation,
/// symbols and the space, never a default-ignorable such as U+00AD soft
/// hyphen, a bidirectional control or a zero-width joiner).
pub(crate) fn is_hidden_char(c: char) -> bool {
    !awili_engine_worker::shown_as_is(c)
}

/// RC1-O-FIX3/FIX4: a tool or file name as the footer shows it — escaped
/// exactly as the approval card escapes it (`awili_engine_worker::shown_path`:
/// hidden characters as `\u{…}`, the backslash as `\\`), so the footer can
/// never show a name that looks like another.
pub(crate) fn shown_name(name: &str) -> String {
    awili_engine_worker::shown_path(name)
}

/// The in-app built-in tools that change a file (canonical names).
const CHANGING_TOOLS: [&str; 2] = ["fs.write", "fs.edit"];

/// RC2-R3M13: whether `name` is one of the in-app tools that change a file.
pub(crate) fn changes_a_file(name: &str) -> bool {
    CHANGING_TOOLS.contains(&name)
}

/// One in-app turn's record. Reset when a Work run is submitted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct InAppFiles {
    /// File-changing calls that started and have not finished, in start
    /// order (the engine runs a round's calls in that order).
    pending: Vec<(String, Option<String>)>,
    /// The files a successful file-changing call changed, first seen first.
    changed: Vec<String>,
    /// RC1-O: other tools (not read-only) that finished successfully, first
    /// seen first. Awili cannot tell whether or which files they touched.
    other: Vec<String>,
}

impl InAppFiles {
    /// Take one engine event into the record. Anything that is not a
    /// file-changing tool's start or finish is ignored.
    pub(crate) fn observe(&mut self, ev: &WorkEvent) {
        match ev {
            WorkEvent::ToolStarted {
                name: Some(name),
                file_path,
                ..
            } if CHANGING_TOOLS.contains(&name.as_str()) => {
                self.pending.push((name.clone(), file_path.clone()));
            }
            WorkEvent::ToolFinished {
                name: Some(name),
                ok,
                ..
            } if CHANGING_TOOLS.contains(&name.as_str()) => {
                let Some(i) = self.pending.iter().position(|(n, _)| n == name) else {
                    return;
                };
                let (_, path) = self.pending.remove(i);
                if *ok == Some(true) {
                    if let Some(path) = path {
                        let path = shown_name(&path);
                        if !self.changed.contains(&path) {
                            self.changed.push(path);
                        }
                    }
                }
            }
            WorkEvent::ToolMayHaveChangedFiles { name } => {
                let name = shown_name(name);
                if !self.other.contains(&name) {
                    self.other.push(name);
                }
            }
            _ => {}
        }
    }

    /// RC1-N: whether this turn has changed any file so far. RC1-O: a
    /// successful tool that may change files counts (a Deny line must not say
    /// the turn did nothing).
    pub(crate) fn changed_any(&self) -> bool {
        !self.changed.is_empty() || !self.other.is_empty()
    }

    /// The footer part: "no files changed", "changed a.txt",
    /// "changed a.txt, b.txt", up to three names, then "changed 4 files".
    /// RC1-O: when another tool that may change files ran, the footer says
    /// so instead of "no files changed" ("ran write_file (Awili can't tell
    /// whether it changed any files)"), after any files Awili saw change. Each part
    /// reads after "this turn " (the Deny line).
    /// RC1-O-FIX8 (whole-slice reviews of FIX6/FIX7, P1): the footer part of
    /// a turn the owner stopped. A Stop ends the turn while calls may still be
    /// in flight and while the worker's last events are still on their way,
    /// so the footer claims only what Awili saw before the Stop — never "no
    /// files changed".
    pub(crate) fn stopped_footer_part(&self) -> String {
        let seen = self.footer_part();
        if seen == "no files changed" {
            "no file changes seen before the Stop".to_string()
        } else {
            format!("{seen} \u{2014} seen before the Stop")
        }
    }

    pub(crate) fn footer_part(&self) -> String {
        let own = match self.changed.as_slice() {
            [] => None,
            names if names.len() <= 3 => Some(format!("changed {}", names.join(", "))),
            names => Some(format!("changed {} files", names.len())),
        };
        let other = match self.other.as_slice() {
            [] => None,
            [one] => Some(format!(
                "ran {one} (Awili can't tell whether it changed any files)"
            )),
            names if names.len() <= 3 => Some(format!(
                "ran {} (Awili can't tell whether they changed any files)",
                names.join(", ")
            )),
            names => Some(format!(
                "ran {} other tools (Awili can't tell whether they changed any files)",
                names.len()
            )),
        };
        match (own, other) {
            (None, None) => "no files changed".to_string(),
            (Some(a), None) | (None, Some(a)) => a,
            (Some(a), Some(b)) => format!("{a}; {b}"),
        }
    }
}
