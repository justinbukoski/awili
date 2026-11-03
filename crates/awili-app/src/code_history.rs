//! U1c (S1-F04): record and persist every Code-tab turn.
//!
//! The pure parts — the per-tab history state, the turn's terminal status,
//! the `code_conv:<id>` meta, the persisted-text shaping and the ordered
//! `PersistCmd` builders — live here so they are testable without a UI. The
//! app glue (`code_turn_begin` / `code_turn_usage` / `code_turn_error` /
//! `code_turn_end`, the `impl AwiliApp` block at the bottom) lives here too;
//! `main.rs` only calls it. It is all synchronous, does no I/O on the UI
//! thread (persistence goes through the fire-and-forget `persist` handle; the
//! ledger append is the same single file append Work does) and never fails
//! the turn.

use awili_code::CodeEngine;
use awili_core::{ConvKind, ConversationList, ConversationMeta, MessageKind, Role};
use awili_providers::Usage;
use awili_selftest::graduation::LedgerEntry;
use awili_work::run_events::RunStatus;
use chrono::{DateTime, Utc};
use gpui::Context;
use serde::{Deserialize, Serialize};

use crate::conv_view::STOPPED_SUFFIX;
use crate::graduation_record::Recorder;
use crate::persistence::PersistCmd;
use crate::work::WorkExecutor;
use crate::work_run_identity::WorkRunIdentity;

/// The `app_state` key prefix for a Code tab's conversation:
/// `code_conv:<conv_id>` (U1).
pub(crate) const CODE_CONV_KEY_PREFIX: &str = "code_conv:";

/// The `app_state` key for one Code conversation (U1): the engine, folder
/// and resume session a reopen needs live under this key.
pub(crate) fn code_conv_key(conv_id: &str) -> String {
    format!("{CODE_CONV_KEY_PREFIX}{conv_id}")
}

/// Per-tab history state; a field of `CodeSession` (U1).
#[derive(Debug, Default)]
pub(crate) struct CodeTabHistory {
    /// The durable conversation backing this tab, if one exists.
    pub(crate) conv_id: Option<String>,
    /// The in-flight turn, if one is streaming (taken at the turn's end).
    pub(crate) turn: Option<CodeTurn>,
}

/// One in-flight Code turn: what a ledger line is built from these fields
/// (U1). The identity is frozen at submit; usage and error accumulate over
/// the stream.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CodeTurn {
    /// The assistant row the reply streams into.
    pub(crate) aidx: usize,
    /// The requested engine/model binding, frozen at submit.
    pub(crate) identity: WorkRunIdentity,
    /// The turn's summed usage frames (saturating).
    pub(crate) usage: Option<Usage>,
    /// The FIRST non-empty error text (trimmed); `None` when clean.
    pub(crate) error: Option<String>,
    /// A Stop was requested while the turn streamed.
    pub(crate) stop_requested: bool,
    /// The short id of this run (S3), minted at submit.
    pub(crate) run_id: String,
}

impl CodeTabHistory {
    /// Fold one usage frame into the in-flight turn (saturating add per
    /// field). No-op without a turn.
    pub(crate) fn note_usage(&mut self, u: Usage) {
        if let Some(turn) = self.turn.as_mut() {
            let prev = turn.usage.unwrap_or_default();
            turn.usage = Some(Usage {
                cached_input_tokens: if turn.usage.is_none() {
                    u.cached_input_tokens
                } else {
                    prev.cached_input_tokens
                        .zip(u.cached_input_tokens)
                        .map(|(a, b)| a.saturating_add(b))
                },
                input_tokens: prev.input_tokens.saturating_add(u.input_tokens),
                output_tokens: prev.output_tokens.saturating_add(u.output_tokens),
            });
        }
    }

    /// Remember the first non-empty error text (killing a child can surface
    /// several frames). No-op without a turn.
    pub(crate) fn note_error(&mut self, e: &str) {
        if let Some(turn) = self.turn.as_mut() {
            let t = e.trim();
            if !t.is_empty() {
                turn.error.get_or_insert_with(|| t.to_string());
            }
        }
    }

    /// Mark the in-flight turn as stopped. No-op without a turn.
    pub(crate) fn note_stop(&mut self) {
        if let Some(turn) = self.turn.as_mut() {
            turn.stop_requested = true;
        }
    }
}

/// A turn's terminal status: a Stop wins over an error (killing the child can
/// itself surface an error); else any error is `Failed`; else `Completed`.
pub(crate) fn turn_status(stop_requested: bool, error: Option<&str>) -> RunStatus {
    if stop_requested {
        RunStatus::Stopped
    } else if error.is_some() {
        RunStatus::Failed
    } else {
        RunStatus::Completed
    }
}

impl CodeTurn {
    /// The ledger line for this turn (U1a `record_code`). Returns `None` when
    /// the ledger is disabled, `Some(Err(text))` when the append failed.
    pub(crate) fn record(
        self,
        rec: &mut Recorder,
        conv_id: &str,
        now: DateTime<Utc>,
    ) -> Option<Result<LedgerEntry, String>> {
        rec.record_code(
            conv_id,
            self.identity,
            turn_status(self.stop_requested, self.error.as_deref()),
            self.error,
            self.usage,
            now,
        )
    }
}

/// The `app_state` value under `code_conv:<id>`: what reopening a Code
/// conversation needs (U1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CodeConvMeta {
    /// The persisted executor string (`agent:<engine>[:<model>]`, the exact
    /// Work-lane format, so a Code and a Work binding record identically).
    pub(crate) executor: String,
    /// The project folder the resume session belongs to.
    pub(crate) folder: Option<String>,
    /// The engine's session/thread id for `--resume`.
    pub(crate) session: Option<String>,
    /// Versioned M2 route state. Older rows omit this and are seeded from the
    /// three legacy fields above; malformed routes remain explicitly
    /// unavailable rather than becoming a global-default selection.
    #[serde(default)]
    pub(crate) route: Option<String>,
}

impl CodeConvMeta {
    /// Build the meta from a turn's engine/model/folder/session. The
    /// `executor` is the Work-lane persistence format for the same binding.
    pub(crate) fn for_turn(
        engine: CodeEngine,
        model: Option<String>,
        folder: Option<String>,
        session: Option<String>,
    ) -> Self {
        Self {
            executor: WorkExecutor::Agent { engine, model }.to_persist(),
            folder,
            session,
            route: None,
        }
    }

    /// Serialize for `app_state`. Infallible for this shape (strings only),
    /// so the only failure mode is a real allocation failure.
    pub(crate) fn to_json(&self) -> String {
        serde_json::to_string(self).expect("CodeConvMeta is JSON-infallible")
    }

    /// Parse an `app_state` value; `None` on any malformed JSON.
    #[allow(dead_code)]
    // U1d re-reads the meta via `PersistCmd::ReadState` (the reopen path);
    // until then the `u1c_` tests are the only callers.
    pub(crate) fn from_json(raw: &str) -> Option<Self> {
        serde_json::from_str(raw).ok()
    }

    /// The engine and model a reopen should run on. Only an `agent:` executor
    /// yields one; `gateway` / `local-build:` / garbage give `None` (the
    /// caller falls back to Claude and says so).
    #[allow(dead_code)]
    // Same as `from_json`: consumed by U1d's reopen path.
    pub(crate) fn engine_model(&self) -> Option<(CodeEngine, Option<String>)> {
        match WorkExecutor::from_persist(&self.executor)? {
            WorkExecutor::Agent { engine, model } => Some((engine, model)),
            _ => None,
        }
    }
}

/// The assistant text to persist: `None` when the bubble is blank
/// (whitespace only); otherwise the bubble exactly as the user saw it (never
/// trimmed), with `STOPPED_SUFFIX` appended to a Stopped turn unless it
/// already ends with it.
pub(crate) fn persisted_assistant_text(bubble: &str, status: RunStatus) -> Option<String> {
    if bubble.trim().is_empty() {
        return None;
    }
    if status == RunStatus::Stopped && !bubble.ends_with(STOPPED_SUFFIX) {
        Some(format!("{bubble}{STOPPED_SUFFIX}"))
    } else {
        Some(bubble.to_string())
    }
}

/// The tab's conversation if it can take another turn: it exists, is kind
/// [`ConvKind::Code`] and is not archived. `None` otherwise (the caller
/// creates a new conversation instead).
pub(crate) fn reusable_conv(current: Option<&str>, list: &ConversationList) -> Option<String> {
    let current = current?;
    let meta = list.get(current)?;
    (meta.kind == ConvKind::Code && !meta.archived).then(|| current.to_string())
}

/// Persist commands for a turn's start, in order: `CreateConversation`
/// (kind `"code"`) when `created` is `Some`, `Append` of the user turn
/// (`role:"user"`, stored with `user_kind` — RC2-RUNF: a plan-card click
/// keeps its mark), and `Rename` of the auto-title (never `custom`) when
/// `new_title` is `Some`.
pub(crate) fn begin_cmds(
    conv_id: &str,
    created: Option<&ConversationMeta>,
    user_text: &str,
    user_kind: MessageKind,
    ts: i64,
    new_title: Option<&str>,
) -> Vec<PersistCmd> {
    let mut cmds = Vec::new();
    if let Some(meta) = created {
        cmds.push(PersistCmd::CreateConversation {
            id: meta.id.clone(),
            title: meta.title.clone(),
            created_ts: meta.created_ts,
            kind: ConvKind::Code.as_str().to_string(),
        });
    }
    cmds.push(PersistCmd::Append {
        conv: conv_id.to_string(),
        role: "user",
        text: user_text.to_string(),
        ts,
        kind: user_kind,
    });
    if let Some(title) = new_title {
        cmds.push(PersistCmd::Rename {
            id: conv_id.to_string(),
            title: title.to_string(),
            custom: false,
        });
    }
    cmds
}

/// Persist commands for a turn's end, in order: `Append` of the assistant
/// reply (`role:"assistant"`) when `Some`, `SetState` of the `code_conv:<id>`
/// meta, and a `Touch` of the conversation's `updated_ts`.
pub(crate) fn end_cmds(
    conv_id: &str,
    assistant: Option<&str>,
    ts: i64,
    meta_json: &str,
    now: i64,
) -> Vec<PersistCmd> {
    let mut cmds = Vec::new();
    if let Some(text) = assistant {
        cmds.push(PersistCmd::Append {
            conv: conv_id.to_string(),
            role: "assistant",
            text: text.to_string(),
            ts,
            kind: MessageKind::Normal,
        });
    }
    cmds.push(PersistCmd::SetState {
        key: code_conv_key(conv_id),
        value: meta_json.to_string(),
    });
    cmds.push(PersistCmd::Touch {
        id: conv_id.to_string(),
        updated_ts: now,
    });
    cmds
}

/// Keep the per-id `views` map in step with a Code conversation (U1): every
/// views-keyed surface (search, export, the Sessions message count) reads
/// from it, and the mirror holds the durable transcript's shape even though
/// the Code tab itself renders from `CodeSession`. Pushes the message (of
/// `kind`, RC2-RUNF) and returns its timestamp.
pub(crate) fn mirror_to_view(
    view: &mut crate::ConvView,
    role: Role,
    text: &str,
    kind: MessageKind,
) -> i64 {
    let ts = view.convo.push_kind(role, text, kind);
    // `push` always appends a row, so the message we just pushed is the last
    // one; the index is in bounds by construction.
    if let Some(m) = view.convo.message(view.convo.len().saturating_sub(1)) {
        view.rows.push(crate::MessageRow::new(m));
    }
    ts as i64
}

/// U1c app glue: the Code tab's turn hooks. `submit_code_turn`,
/// the provider loop's Usage/Error arms and `finish_code_turn` (main.rs)
/// call these.
impl crate::AwiliApp {
    /// Bind a native tab to its durable Workshop conversation before a target
    /// conversion.  This is intentionally provider-free: choosing InApp or
    /// Gateway on a blank native tab must not require a sacrificial native
    /// request just to obtain an id.
    pub(crate) fn ensure_code_conversation_identity(&mut self, tab_id: u64) -> Option<String> {
        let tab = self.work.code_tabs.iter().find(|tab| tab.id == tab_id)?;
        let route_seed = tab.route.clone();
        let current_id = tab.history.conv_id.clone();
        let clean = |folder: Option<String>| folder.filter(|p| !p.trim().is_empty());
        // Only a new identity acquires the project shown by the native
        // header. Existing records change only through explicit picker actions;
        // even an empty projection is not proof that durable history is empty.
        let visible_folder = clean(tab.project_folder.clone())
            .or_else(|| self.selected_code_folder().map(|p| p.display().to_string()));
        if let Some(id) = current_id {
            if !self.work.unavailable_conversation_routes.contains_key(&id) {
                if !self.work.conversation_routes.contains_key(&id) {
                    self.work.conversation_routes.insert(id.clone(), route_seed);
                    self.persist_conversation_route(&id);
                }
                let canonical = self.code_identity_folder(&id);
                if canonical.is_some() {
                    // Rehydrate only the native projection, never rewrite
                    // a historical conversation's durable folder here.
                    if let Some(tab) = self.work.code_tabs.iter_mut().find(|tab| tab.id == tab_id) {
                        tab.project_folder = canonical.map(|p| p.display().to_string());
                    }
                }
            }
            return Some(id);
        }
        let folder = visible_folder;
        let now = crate::feed::unix_now();
        let meta = self.conversations.create_kind(now, ConvKind::Code);
        let id = meta.id.clone();
        self.views
            .entry(id.clone())
            .or_insert_with(crate::ConvView::new);
        // Create must precede SetCwd in the persistence worker's FIFO. A
        // folder update before its row exists silently updates zero rows.
        if let Some(persist) = &self.persist {
            for cmd in begin_cmds(&id, Some(&meta), "", MessageKind::Normal, now, None) {
                if matches!(cmd, PersistCmd::CreateConversation { .. }) {
                    persist.send(cmd);
                }
            }
        }
        self.work
            .conversation_routes
            .entry(id.clone())
            .or_insert(route_seed);
        self.persist_conversation_route(&id);
        if let Some(tab) = self.work.code_tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.history.conv_id = Some(id.clone());
        }
        self.sync_code_identity_folder(tab_id, &id, folder);
        Some(id)
    }

    /// One compatibility identity's explicit folder. Never consult another
    /// thread's ambient Work state or the native provider's HOME fallback.
    pub(crate) fn code_identity_folder(&self, id: &str) -> Option<std::path::PathBuf> {
        let conv = self
            .conversations
            .get(id)
            .filter(|c| c.kind == ConvKind::Code)?;
        conv.cwd
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .or_else(|| {
                self.work
                    .code_tabs
                    .iter()
                    .find(|tab| tab.history.conv_id.as_deref() == Some(id))
                    .and_then(|tab| tab.project_folder.as_deref())
                    .filter(|p| !p.trim().is_empty())
            })
            .map(std::path::PathBuf::from)
    }

    pub(crate) fn sync_code_identity_folder(
        &mut self,
        tab_id: u64,
        id: &str,
        folder: Option<String>,
    ) {
        if let Some(tab) = self.work.code_tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.project_folder = folder.clone();
        }
        if let Some(meta) = self.conversations.get_mut(id) {
            if meta.cwd != folder {
                meta.cwd = folder.clone();
                if let Some(persist) = &self.persist {
                    persist.send(PersistCmd::SetCwd {
                        id: id.to_string(),
                        cwd: folder,
                    });
                }
            }
        }
    }

    /// A Code tab's turn has started. Bind the tab to a durable conversation
    /// (its live one, or a new `code` one), mirror the user message into the
    /// per-id `views` map (search, export, Sessions message count), queue the
    /// `begin_cmds` persistence and freeze the turn record on the tab.
    pub(crate) fn code_turn_begin(&mut self, tab_id: u64, text: &str, aidx: usize) {
        let Some(tab) = self.work.code_tabs.iter().find(|s| s.id == tab_id) else {
            return;
        };
        let engine = tab.engine;
        let model = tab.model.clone();
        let route_seed = tab.route.clone();
        let prev_conv = tab.history.conv_id.clone();
        let folder = tab.project_folder.clone();
        // The user bubble is the row before the assistant bubble `aidx`; its
        // kind (RC2-RUNF: a plan-card click) goes with it into storage.
        let user = tab.convo.message(aidx.saturating_sub(1));
        let user_ts = user.map(|m| m.ts as i64);
        let user_kind = user.map_or(MessageKind::Normal, |m| m.kind);
        let now = crate::feed::unix_now();
        let (conv_id, created) = match reusable_conv(prev_conv.as_deref(), &self.conversations) {
            Some(id) => (id, None),
            None => {
                let meta = self.conversations.create_kind(now, ConvKind::Code);
                (meta.id.clone(), Some(meta))
            }
        };
        let new_title = self.conversations.note_user_message(&conv_id, text, now);
        self.work
            .conversation_routes
            .entry(conv_id.clone())
            .or_insert(route_seed);
        let view = self
            .views
            .entry(conv_id.clone())
            .or_insert_with(crate::ConvView::new);
        mirror_to_view(view, Role::User, text, user_kind);
        if let Some(persist) = &self.persist {
            let cmds = begin_cmds(
                &conv_id,
                created.as_ref(),
                text,
                user_kind,
                user_ts.unwrap_or(now),
                new_title.as_deref(),
            );
            for cmd in cmds {
                persist.send(cmd);
            }
        }
        // RC2-UX (D-R1 m6: identical Code titles could not be told apart, all in
        // one group): a new Code conversation is filed under its project's
        // folder, as a Work conversation is under its folder.
        if let Some(folder) = folder {
            if let Some(meta) = self.conversations.get_mut(&conv_id) {
                if meta.cwd.as_deref() != Some(folder.as_str()) {
                    meta.cwd = Some(folder.clone());
                    if let Some(persist) = &self.persist {
                        persist.send(PersistCmd::SetCwd {
                            id: conv_id.clone(),
                            cwd: Some(folder),
                        });
                    }
                }
            }
        }
        let new_id = crate::run_error::new_run_id();
        crate::run_error::log_run_start(&new_id, "code", &conv_id);
        if let Some(tab) = self.work.code_tabs.iter_mut().find(|s| s.id == tab_id) {
            tab.history.conv_id = Some(conv_id);
            tab.history.turn = Some(CodeTurn {
                aidx,
                identity: WorkRunIdentity::Agent { engine, model },
                usage: None,
                error: None,
                stop_requested: false,
                run_id: new_id,
            });
        }
        // U1-INT1: a turn on the visible tab selects its conversation, so the
        // sidebar and the tab never point at different conversations.
        self.sync_code_selection();
    }

    /// RC2-FIXA3 (RC2-RUNAPP review P2): a row the Code tab writes between
    /// turns (the plan card's Deny) goes into the tab's saved conversation
    /// too: mirrored into `views`, stored with its kind at the tab row's
    /// time `ts`, and the conversation touched, so a relaunch shows it.
    pub(crate) fn code_record_row(
        &mut self,
        tab_id: u64,
        role: Role,
        text: &str,
        kind: MessageKind,
        ts: i64,
    ) {
        let Some(conv_id) = self
            .work
            .code_tabs
            .iter()
            .find(|s| s.id == tab_id)
            .and_then(|t| t.history.conv_id.clone())
        else {
            return;
        };
        let view = self
            .views
            .entry(conv_id.clone())
            .or_insert_with(crate::ConvView::new);
        mirror_to_view(view, role, text, kind);
        let now = crate::feed::unix_now();
        if let Some(persist) = &self.persist {
            persist.send(PersistCmd::Append {
                conv: conv_id.clone(),
                role: crate::role_str(role),
                text: text.to_string(),
                ts,
                kind,
            });
            persist.send(PersistCmd::Touch {
                id: conv_id.clone(),
                updated_ts: now,
            });
        }
        self.conversations.touch(&conv_id, now);
    }

    /// U1-INT1: make Code tab `ix` the active tab and let the sidebar
    /// selection follow it (the tab bar's click handler).
    pub(crate) fn select_code_tab(&mut self, ix: usize) {
        if ix < self.work.code_tabs.len() {
            self.work.code_reopen_generation = self.work.code_reopen_generation.wrapping_add(1);
            self.work.code_active = ix;
            self.sync_code_selection();
        }
    }

    /// U1-INT1: in Code mode, select the active tab's conversation in the
    /// sidebar and remember it as the active conversation. A tab with no
    /// conversation yet (never ran a turn) leaves the selection alone; its
    /// first turn selects the conversation it creates.
    pub(crate) fn sync_code_selection(&mut self) {
        if self.mode != crate::work::Mode::Code {
            return;
        }
        let Some(conv_id) = self
            .work
            .code_tabs
            .get(self.work.code_active)
            .and_then(|t| t.history.conv_id.clone())
        else {
            return;
        };
        self.set_active_conversation(&conv_id);
        self.save_active_conversation();
        self.refresh_staged_attachments();
    }

    /// A Code tab's usage frame landed: fold it into the in-flight turn.
    pub(crate) fn code_turn_usage(&mut self, tab_id: u64, u: Usage) {
        if let Some(tab) = self.work.code_tabs.iter_mut().find(|s| s.id == tab_id) {
            tab.history.note_usage(u);
        }
    }

    /// A Code tab's error frame landed: keep the first one for the ledger.
    pub(crate) fn code_turn_error(&mut self, tab_id: u64, e: &str) {
        if let Some(tab) = self.work.code_tabs.iter_mut().find(|s| s.id == tab_id) {
            tab.history.note_error(e);
        }
    }

    /// A Code tab's turn has ended. Take the turn record (a second call
    /// writes nothing), put the stop marker on a stopped bubble, mirror and
    /// persist the reply exactly as the bubble shows it plus the
    /// `code_conv:<id>` meta, touch the conversation and append the ledger
    /// line. A ledger failure is surfaced as tab activity; it never fails
    /// the turn.
    pub(crate) fn code_turn_end(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.work.code_tabs.iter_mut().find(|s| s.id == tab_id) else {
            return;
        };
        let (Some(turn), Some(conv_id)) = (tab.history.turn.take(), tab.history.conv_id.clone())
        else {
            return;
        };
        let aidx = turn.aidx;
        let status = turn_status(turn.stop_requested, turn.error.as_deref());
        let shown = tab
            .convo
            .message(aidx)
            .map(|m| m.text.clone())
            .unwrap_or_default();
        if status == RunStatus::Stopped
            && !shown.trim().is_empty()
            && !shown.ends_with(STOPPED_SUFFIX)
        {
            tab.convo.append_text(aidx, STOPPED_SUFFIX);
        }
        let bubble = tab.convo.message(aidx);
        let assistant = bubble.and_then(|m| persisted_assistant_text(&m.text, status));
        let now = crate::feed::unix_now();
        let a_ts = bubble.map_or(now, |m| m.ts as i64);
        let (engine, model) = match &turn.identity {
            WorkRunIdentity::Agent { engine, model } => (*engine, model.clone()),
            _ => (tab.engine, tab.model.clone()),
        };
        let resume = tab.session_slot.lock().ok().and_then(|g| g.clone());
        // What the shared completion hooks need, read while the tab is borrowed.
        let hook_user = tab.convo.messages()[..aidx.min(tab.convo.len())]
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.text.clone())
            .unwrap_or_default();
        let hook_model = model.clone().unwrap_or_else(|| "default model".to_string());
        let hook_reply = assistant.clone().unwrap_or_default();
        let activity = tab
            .rows
            .get(aidx)
            .map(|row| row.activity.clone())
            .unwrap_or_default();
        let mut meta =
            CodeConvMeta::for_turn(engine, model, tab.project_folder.clone(), resume.clone());
        if let (Some(text), Some(view)) = (assistant.as_deref(), self.views.get_mut(&conv_id)) {
            mirror_to_view(view, Role::Assistant, text, MessageKind::Normal);
            if let Some(row) = view.rows.last_mut() {
                row.activity = activity;
            }
        }
        // The completion cursor is measured after the exact same canonical
        // projection used for dispatch. A stale callback cannot advance a
        // later harness turn because it must own this tab's active snapshot.
        if let Some(active) = self.work.active_code_route_turns.remove(&tab_id) {
            let through = self
                .views
                .get(&conv_id)
                .map(crate::work::bridge_records_from_view)
                .map(|records| awili_code::continuity::canonical_cursor(&records))
                .unwrap_or_default();
            if let Some(route) = self.work.conversation_routes.get_mut(&conv_id) {
                let finish = if status == RunStatus::Completed {
                    crate::conversation_route::NativeTurnFinish::Completed {
                        native_id_after: resume.clone(),
                        transcript_through: through,
                    }
                } else {
                    crate::conversation_route::NativeTurnFinish::Interrupted {
                        transcript_through: through,
                    }
                };
                route.finish_turn(&active, Some(finish));
            }
        }
        self.persist_conversation_route(&conv_id);
        meta.route = self
            .work
            .conversation_routes
            .get(&conv_id)
            .and_then(|route| route.to_persisted_json().ok());
        if let Some(persist) = &self.persist {
            for cmd in end_cmds(&conv_id, assistant.as_deref(), a_ts, &meta.to_json(), now) {
                persist.send(cmd);
            }
        }
        self.conversations.touch(&conv_id, now);
        // M2f: the usage before `record` consumes the turn.
        let usage = turn.usage;
        let run_id_for_turn = turn.run_id.clone();
        match turn.record(&mut self.work.graduation, &conv_id, Utc::now()) {
            Some(Err(text)) => {
                eprintln!("awili: {text}");
                self.code_on_activity(tab_id, aidx, text, cx);
            }
            // M2f: the reply's footer, from the same ledger line.
            Some(Ok(entry)) => self.attach_code_footer(tab_id, aidx, &entry, usage),
            None => {}
        }
        // Provenance, reported spend, memory and read-aloud: the same hooks a
        // Work turn gets, for a native Code reply.
        let visible = self.mode == crate::work::Mode::Code
            && self.work.code_tabs.get(self.work.code_active).map(|t| t.id) == Some(tab_id);
        self.finish_workshop_turn(
            crate::workshop_turn::FinishedTurn {
                conv_id,
                reply_idx: aidx,
                code_tab: Some(tab_id),
                user_text: hook_user,
                reply_text: hook_reply,
                executor: engine.label().to_ascii_lowercase(),
                model: hook_model,
                class: crate::graduation_record::Class::Frontier,
                usage,
                ok: status == RunStatus::Completed,
                visible,
                run_id: run_id_for_turn,
            },
            cx,
        );
        cx.notify();
    }
}

#[cfg(test)]
#[path = "code_history_tests.rs"]
mod code_history_tests;

#[cfg(test)]
#[path = "native_route_seed_tests.rs"]
mod native_route_seed_tests;
