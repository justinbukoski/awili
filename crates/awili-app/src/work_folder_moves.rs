//! RC2-R1F5 (usability RC2 round 1, N1: ⌘O in a Work conversation that
//! already had turns silently moved the whole conversation to the new
//! folder). The move stays — the next turns run in the new folder and the
//! sidebar files the conversation under it — but it is visible: the
//! transcript shows a plain note where it happened ("Now working in k3 —
//! earlier turns were in g1"), each earlier reply's footer names the folder
//! it ran in, and the sidebar row says where the conversation came from.
//! The moves are saved per conversation (`work_folder_moves:<conv_id>` in
//! `app_state`), so all of it survives a relaunch.

use gpui::prelude::*;
use gpui::SharedString;

/// One folder change in a Work conversation.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct FolderMove {
    /// How many messages the conversation held when the folder changed: the
    /// note shows before message `at`; messages before it ran in `from`.
    pub(crate) at: usize,
    /// The folder the earlier turns ran in.
    pub(crate) from: String,
    /// The folder the next turns run in.
    pub(crate) to: String,
}

/// A conversation's folder changes, oldest first.
pub(crate) type FolderMoves = Vec<FolderMove>;

/// The `app_state` key prefix of a Work conversation's saved folder moves.
pub(crate) const MOVES_KEY_PREFIX: &str = "work_folder_moves:";

/// The test lookup key of a folder-change note in the transcript.
pub(crate) const MOVE_NOTE_SELECTOR: &str = "rc2r1f-folder-move-note";

/// The test lookup key of a sidebar row's "moved" line.
pub(crate) const SIDEBAR_MOVED_SELECTOR: &str = "rc2r1f-sidebar-moved";

/// The `app_state` key holding `conv`'s folder moves.
pub(crate) fn moves_key(conv: &str) -> String {
    format!("{MOVES_KEY_PREFIX}{conv}")
}

/// Parse saved moves; anything unreadable is none (a bad row never blocks a
/// restore).
pub(crate) fn parse_moves(raw: &str) -> FolderMoves {
    serde_json::from_str(raw).unwrap_or_default()
}

/// A folder as the owner knows it: its last name ("k3"), else the path.
pub(crate) fn folder_name(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    std::path::Path::new(trimmed)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// The transcript note for one move.
pub(crate) fn note_text(m: &FolderMove) -> String {
    format!(
        "Now working in {} \u{2014} earlier turns were in {}",
        folder_name(&m.to),
        folder_name(&m.from)
    )
}

/// The folder message `idx` ran in when that is not the conversation's
/// current folder (the `from` of the first move after it), else `None`.
pub(crate) fn ran_in(moves: &[FolderMove], idx: usize) -> Option<&str> {
    moves.iter().find(|m| idx < m.at).map(|m| m.from.as_str())
}

/// A reply footer with the folder it ran in, when that was another folder
/// ("… · no files changed · in g1").
pub(crate) fn footer_with_folder(footer: SharedString, folder: Option<&str>) -> SharedString {
    match folder {
        Some(f) => SharedString::from(format!("{footer} \u{b7} in {}", folder_name(f))),
        None => footer,
    }
}

/// The sidebar row's line for a moved conversation ("Moved from g1"), from
/// its latest move.
pub(crate) fn sidebar_line(moves: &[FolderMove]) -> Option<String> {
    moves
        .last()
        .map(|m| format!("Moved from {}", folder_name(&m.from)))
}

/// The muted transcript line for a folder change.
pub(crate) fn note_line(conv_row: usize, m: &FolderMove) -> gpui::Div {
    use crate::theme;
    gpui::div()
        .debug_selector(move || format!("{MOVE_NOTE_SELECTOR}-{conv_row}"))
        .w_full()
        .flex()
        .justify_center()
        .text_size(theme::z(theme::LABEL_SIZE))
        .text_color(theme::p().muted)
        .italic()
        .child(SharedString::from(note_text(m)))
}

impl crate::AwiliApp {
    /// RC2-R1F5: the owner chose `to` for Work conversation `conv`, which
    /// ran its turns in `from`. When it has turns and the folder really
    /// changes, the move is recorded (and saved) at this point of the
    /// transcript.
    pub(crate) fn note_folder_move(&mut self, conv: &str, from: Option<String>, to: &str) {
        let Some(from) = from.filter(|f| !f.is_empty()) else {
            return;
        };
        if from.trim_end_matches('/') == to.trim_end_matches('/') {
            return;
        }
        let at = self
            .views
            .get(conv)
            .map(|v| v.convo.messages().len())
            .unwrap_or(0);
        if at == 0 {
            return;
        }
        let moves = self.work_folder_moves.entry(conv.to_string()).or_default();
        // Two changes before the next turn are one: from the folder the
        // turns ran in to the latest choice.
        if let Some(last) = moves.last_mut().filter(|m| m.at == at) {
            last.to = to.to_string();
            if last.from.trim_end_matches('/') == to.trim_end_matches('/') {
                moves.pop();
            }
        } else {
            moves.push(FolderMove {
                at,
                from,
                to: to.to_string(),
            });
        }
        let json = serde_json::to_string(&*moves).unwrap_or_default();
        if let Some(p) = &self.persist {
            p.set_state(&moves_key(conv), json);
        }
    }

    /// The folder moves of `conv` (none when it never moved).
    pub(crate) fn folder_moves(&self, conv: &str) -> &[FolderMove] {
        self.work_folder_moves
            .get(conv)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(at: usize, from: &str, to: &str) -> FolderMove {
        FolderMove {
            at,
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn rc2r1f5_the_words_and_the_folder_each_turn_ran_in() {
        let moves = vec![mv(6, "/s/g1", "/s/k3"), mv(8, "/s/k3", "/s/x1/")];
        assert_eq!(
            note_text(&moves[0]),
            "Now working in k3 \u{2014} earlier turns were in g1"
        );
        assert_eq!(ran_in(&moves, 0), Some("/s/g1"));
        assert_eq!(ran_in(&moves, 5), Some("/s/g1"));
        assert_eq!(ran_in(&moves, 6), Some("/s/k3"));
        assert_eq!(ran_in(&moves, 8), None, "the current folder needs no word");
        assert_eq!(
            footer_with_folder("In-app \u{b7} 8s".into(), ran_in(&moves, 1)).as_ref(),
            "In-app \u{b7} 8s \u{b7} in g1"
        );
        assert_eq!(sidebar_line(&moves).as_deref(), Some("Moved from k3"));
        assert_eq!(sidebar_line(&[]), None);
        assert_eq!(folder_name("/s/x1/"), "x1");
        assert_eq!(parse_moves("not json"), FolderMoves::new());
        let json = serde_json::to_string(&moves).unwrap();
        assert_eq!(parse_moves(&json), moves);
    }
}
