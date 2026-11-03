//! M1c (S3-F03): export the conversation on screen, to a file that never overwrites.

use std::io::Write;
use std::path::{Path, PathBuf};

use awili_persist::ExportedConversation;

use crate::work::Mode;

/// Refusal: nothing exportable is on screen (Required behaviour 1).
pub(crate) const NOT_A_CONVERSATION: &str = "Open a conversation to export it";
/// Refusal: the transcript on screen is still streaming (Required behaviour 1).
pub(crate) const STILL_STREAMING: &str = "Wait for the reply to finish before exporting";
/// Refusal: the Code tab on screen has no messages yet (Required behaviour 1).
pub(crate) const NOTHING_YET: &str = "Nothing to export yet";
/// Refusal: the Chat/Work conversation could not be read back.
pub(crate) const NOT_IN_DATABASE: &str = "Export failed — conversation not in database";

/// Which surface a conversation can currently be exported from.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExportSource {
    /// Chat or Work: the conversation selected in the sidebar.
    Stored(String),
    /// Code mode: the in-memory transcript of one code tab (by tab index).
    CodeTab(usize),
    /// No conversation is on screen to export.
    NotAConversation,
}

/// The export target for what the user is looking at:
/// Chat | Work → `Stored(active_id)`; Code → `CodeTab(i)` when a tab is
/// active, else `NotAConversation`; every other mode → `NotAConversation`.
pub(crate) fn export_source(
    mode: Mode,
    active_id: &str,
    code_active: Option<usize>,
) -> ExportSource {
    match mode {
        // RC2-CNCL15: a council exports from the database too.
        Mode::Work | Mode::Council => ExportSource::Stored(active_id.to_string()),
        Mode::Code => match code_active {
            Some(i) => ExportSource::CodeTab(i),
            None => ExportSource::NotAConversation,
        },
        _ => ExportSource::NotAConversation,
    }
}

/// A Code tab's in-memory transcript as an export (Required behaviour 4):
/// the title is the first user message's first line, trimmed, capped at 60
/// chars (no user message at all → `"<engine_label> session"`); `created_at`
/// is `now_unix`; the messages keep their order, with the role/kind wire
/// strings, the `ts` as seconds, a positional `id`, and no supersession.
pub(crate) fn code_tab_export(
    engine_label: &str,
    messages: &[awili_core::Message],
    now_unix: i64,
) -> ExportedConversation {
    let title = messages
        .iter()
        .find(|m| m.role == awili_core::Role::User)
        .and_then(|m| {
            let first_line = m.text.lines().next().unwrap_or("").trim();
            let count = first_line.chars().count();
            if count == 0 {
                None
            } else if count > 60 {
                // The ellipsis counts toward the 60-character cap.
                let t: String = first_line.chars().take(59).collect();
                Some(format!("{t}…"))
            } else {
                Some(first_line.to_string())
            }
        })
        .unwrap_or_else(|| format!("{engine_label} session"));
    ExportedConversation {
        title,
        created_at: now_unix,
        messages: messages
            .iter()
            .enumerate()
            .map(|(i, m)| awili_persist::StoredMessage {
                id: i as i64,
                role: role_str(&m.role).to_string(),
                text: m.text.clone(),
                ts: m.ts as i64,
                kind: m.kind.as_persist_str().to_string(),
                superseded_by: None,
            })
            .collect(),
    }
}

/// The wire string for a role, as persisted (same mapping as `main::role_str`).
fn role_str(role: &awili_core::Role) -> &'static str {
    match role {
        awili_core::Role::User => "user",
        awili_core::Role::Assistant => "assistant",
    }
}

/// The file names an export may take, in order and each exactly once:
/// `first`, then `<stem>-2.<ext>`, `<stem>-3.<ext>`, …, `<stem>-99.<ext>`
/// beside it (no extension → `<stem>-2` …). 99 paths.
pub(crate) fn export_candidates(first: &Path) -> Vec<PathBuf> {
    let stem = first
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| first.to_string_lossy().into_owned());
    let ext = first
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = vec![first.to_path_buf()];
    for n in 2..=99 {
        let file = if ext.is_empty() {
            format!("{stem}-{n}")
        } else {
            format!("{stem}-{n}.{ext}")
        };
        out.push(first.with_file_name(file));
    }
    out
}

/// Write `markdown` to the first free name of [`export_candidates`]; an
/// existing file is never replaced (`create_new`). Only `AlreadyExists`
/// moves on to the next name; any other open error (permission, a name the
/// filesystem rejects, …) is returned at once. The parent directory is
/// created when missing. Returns the path actually written.
pub(crate) fn write_export(first: &Path, markdown: &str) -> std::io::Result<PathBuf> {
    if let Some(parent) = first.parent() {
        awili_config::ensure_private_app_dir(parent)?;
    }
    for path in export_candidates(first) {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(markdown.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("export target already exists: {}", first.display()),
    ))
}

/// `path` with the `home` prefix replaced by `~` when `home` is a leading
/// path prefix; otherwise `path` as a string.
pub(crate) fn display_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => {
            let s = rest.to_string_lossy();
            if s.is_empty() {
                "~".to_string()
            } else {
                format!("~/{s}")
            }
        }
        None => path.to_string_lossy().into_owned(),
    }
}

/// The toast version of a title: at most 40 chars, `…` appended when the cut
/// happened (on a char boundary).
pub(crate) fn short_title(title: &str) -> String {
    let chars: Vec<char> = title.chars().collect();
    if chars.len() <= 40 {
        title.to_string()
    } else {
        let mut s: String = chars.iter().take(40).collect();
        s.push('…');
        s
    }
}

/// RC2-CNCL15: the heading of one council answer in an export: who said it,
/// in which round, and whether it kept its answer.
pub(crate) fn council_answer_heading(t: &awili_persist::CouncilTurnRow) -> String {
    let mut s = format!("{}{}", awili_persist::MEMBER_ROLE_PREFIX, t.label);
    match t.round_kind.as_str() {
        "sum_up" => s.push_str(" \u{00B7} summary"),
        _ => s.push_str(&format!(" \u{00B7} round {}", t.round)),
    }
    if let Some(st) = t
        .stance
        .as_deref()
        .and_then(awili_council::Stance::from_token)
    {
        s.push_str(&format!(" \u{00B7} {}", st.label()));
    }
    if t.status == "stopped" {
        s.push_str(" \u{00B7} stopped");
    }
    s
}

impl crate::AwiliApp {
    /// RC2-CNCL15: name the member behind each council answer (the
    /// export would otherwise say "Assistant" for all of them).
    fn name_council_members(&self, mut exp: ExportedConversation) -> ExportedConversation {
        let Some(turns) = self
            .search_store
            .as_ref()
            .and_then(|s| s.council_turns(self.sidebar.active_id()).ok())
        else {
            return exp;
        };
        for m in &mut exp.messages {
            if let Some(t) = turns.iter().find(|t| t.message_id == Some(m.id)) {
                m.role = council_answer_heading(t);
            }
        }
        exp
    }
}

impl crate::AwiliApp {
    /// THE one on-screen export path: every entry point (native menu, ⌘⇧C,
    /// toolbar, palette — all through `export_conversation` /
    /// `save_conversation`) resolves its target here, so the streaming
    /// refusal cannot be bypassed. Code tabs export the in-memory transcript
    /// (they are never in the database) and refuse while the tab streams;
    /// Chat | Work refuse through the conversation-specific
    /// `conversation_export_busy` gate, then read the store; every other mode
    /// has nothing on screen to export.
    fn export_on_screen(&self) -> Result<ExportedConversation, &'static str> {
        let code_active = (self.mode == Mode::Code
            && self.work.code_active < self.work.code_tabs.len())
        .then_some(self.work.code_active);
        match export_source(self.mode, self.sidebar.active_id(), code_active) {
            ExportSource::Stored(_) => {
                if self.conversation_export_busy()
                    || (self.mode == Mode::Council
                        && self
                            .active_council()
                            .is_some_and(|p| p.council.is_running()))
                {
                    return Err(STILL_STREAMING);
                }
                let exp = self.exported_active().ok_or(NOT_IN_DATABASE)?;
                Ok(if self.mode == Mode::Council {
                    self.name_council_members(exp)
                } else {
                    exp
                })
            }
            ExportSource::CodeTab(i) => {
                let tab = self.work.code_tabs.get(i).ok_or(NOTHING_YET)?;
                if tab.streaming {
                    return Err(STILL_STREAMING);
                }
                if tab.convo.messages().is_empty() {
                    return Err(NOTHING_YET);
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                Ok(code_tab_export(
                    tab.engine.label(),
                    tab.convo.messages(),
                    now,
                ))
            }
            ExportSource::NotAConversation => Err(NOT_A_CONVERSATION),
        }
    }

    /// Copy only (Required behaviour 3), through `export_on_screen`.
    pub(crate) fn copy_on_screen_conversation(&mut self, cx: &mut gpui::Context<Self>) {
        match self.export_on_screen() {
            Ok(exp) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(exp.to_markdown()));
                self.show_toast(
                    format!("Copied \"{}\" to the clipboard", short_title(&exp.title)),
                    cx,
                );
            }
            Err(refusal) => self.show_toast(refusal, cx),
        }
    }

    /// File + clipboard (Required behaviour 2), through `export_on_screen`:
    /// a fresh file in Downloads that never overwrites, the same Markdown on
    /// the clipboard, and the path in the toast.
    pub(crate) fn save_on_screen_conversation(&mut self, cx: &mut gpui::Context<Self>) {
        let exp = match self.export_on_screen() {
            Ok(exp) => exp,
            Err(refusal) => {
                self.show_toast(refusal, cx);
                return;
            }
        };
        let Some(first) = crate::attachments::downloads_path(&exp.filename()) else {
            self.show_toast("Save failed — no Downloads directory", cx);
            return;
        };
        let markdown = exp.to_markdown();
        let path = match write_export(&first, &markdown) {
            Ok(path) => path,
            Err(e) => {
                self.show_toast(format!("Save failed — {e}"), cx);
                return;
            }
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(markdown));
        // R2f: say where the file went in plain words (RC1 round 2: the
        // 3-second toast was the only clue, and it was clipped). RC1-H7: long
        // enough to read, then it goes by itself (round 3: still open after
        // three minutes). RC2-UX (R2-19): with a "Show in Finder" button.
        self.show_info_toast(saved_notice(&exp.title, &path), cx);
        self.toast.set_reveal(path);
    }

    /// RC2-UX (R2-19, round 2 Minor 10): "Save Conversation As…" (⌘⇧S, File
    /// menu): the conversation on screen goes to a file the owner names in
    /// the system save panel (opened in Downloads, with the usual file name),
    /// and is copied; the toast says where it went and offers "Show in
    /// Finder". The save panel has already asked before replacing a file.
    /// Cancelling saves nothing.
    pub(crate) fn save_on_screen_conversation_as(&mut self, cx: &mut gpui::Context<Self>) {
        let exp = match self.export_on_screen() {
            Ok(exp) => exp,
            Err(refusal) => {
                self.show_toast(refusal, cx);
                return;
            }
        };
        let name = exp.filename();
        let start = crate::attachments::downloads_path(&name)
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let rx = cx.prompt_for_new_path(&start, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return;
            };
            this.update(cx, |app, cx| {
                let markdown = exp.to_markdown();
                if let Err(e) = std::fs::write(&path, &markdown) {
                    app.show_sticky_toast(format!("Save failed \u{2014} {e}"), cx);
                    return;
                }
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(markdown));
                app.show_info_toast(saved_notice(&exp.title, &path), cx);
                app.toast.set_reveal(path);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// RC2-UX (R2-19): show `path` in Finder. (Tests record it instead: the
    /// test platform cannot reveal.)
    pub(crate) fn reveal_in_finder(&mut self, path: &Path, cx: &mut gpui::Context<Self>) {
        #[cfg(not(test))]
        cx.reveal_path(path);
        #[cfg(test)]
        {
            let _ = cx;
            REVEALED.with(|r| r.borrow_mut().push(path.to_path_buf()));
        }
    }
}

#[cfg(test)]
thread_local! {
    /// RC2-UX (R2-19): the paths a test asked Finder to show.
    pub(crate) static REVEALED: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// R2f: the Save toast. Names the Downloads folder in words and the file by
/// its name; a file anywhere else is named by its `~` path.
pub(crate) fn saved_notice(title: &str, path: &Path) -> String {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let downloads = dirs::download_dir().or_else(|| dirs::home_dir().map(|h| h.join("Downloads")));
    let in_downloads = downloads.is_some_and(|d| path.parent() == Some(d.as_path()));
    let place = if in_downloads {
        format!("in your Downloads folder as {file}")
    } else {
        format!("as {}", display_path(path, dirs::home_dir().as_deref()))
    };
    format!(
        "Saved \"{}\" {place} \u{2014} also copied, ready to paste. Click to close.",
        short_title(title)
    )
}
