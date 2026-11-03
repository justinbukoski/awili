//! U1d (S1-F04): list and reopen Code conversations.
//!
//! The pure parts — the sidebar kind for a mode, the tab lookups and the
//! blank-tab reuse rule, the reopen plan parsed from the `code_conv:<id>`
//! meta, and the transcript/engine/resume restore into a [`CodeSession`] —
//! live here so they are testable without a UI. The app glue
//! (`open_if_code_conversation` / `open_code_conversation` /
//! `finish_code_reopen` / `new_code_tab_like_active`, the `impl AwiliApp`
//! block at the bottom) lives here too; `main.rs` only calls it. It switches
//! modes, focuses or adds a tab and reads the meta through the ordered
//! persistence worker, dropping a reply the user has already moved away from.

use awili_code::CodeEngine;
use awili_core::{ConvKind, ConversationState};
use gpui::Context;

use crate::code_history::{code_conv_key, CodeConvMeta};
use crate::conversation_route::ConversationRoute;
use crate::persistence::PersistCmd;
use crate::work::WorkExecutor;
use crate::work::{CodeSession, Mode};

/// The conversation kind the sidebar lists (and "+ New" would create) for a
/// mode: Code mode → Code, Council → Council, every other mode → Work (the
/// one Workshop thread kind; stored Chat rows are read as Work).
pub(crate) fn sidebar_kind_for(mode: Mode) -> ConvKind {
    match mode {
        Mode::Work => ConvKind::Work,
        Mode::Code => ConvKind::Code,
        Mode::Council => ConvKind::Council,
        Mode::Design
        | Mode::Skills
        | Mode::Cron
        | Mode::Sessions
        | Mode::CommandCenter
        | Mode::Messaging => ConvKind::Work,
    }
}

/// The index of the tab already bound to `id` (a double click must not open
/// two tabs). `None` when no tab carries the conversation.
pub(crate) fn tab_for_conv(tabs: &[CodeSession], id: &str) -> Option<usize> {
    tabs.iter()
        .position(|s| s.history.conv_id.as_deref() == Some(id))
}

/// The active tab if it is unused (no rows, not streaming, no conversation):
/// a reopen fills it instead of adding a tab. `None` otherwise.
pub(crate) fn reusable_blank_tab(tabs: &[CodeSession], active: usize) -> Option<usize> {
    (tabs
        .get(active)
        .is_some_and(|t| t.rows.is_empty() && !t.streaming && t.history.conv_id.is_none()))
    .then_some(active)
}

/// What a reopen applies to the tab: the engine and model to run on, the
/// project folder and the resume session the recorded engine belonged to.
/// The folder and the slot are restored together on purpose —
/// `submit_code_turn`'s existing folder-match rule then drops the resume id
/// if the active project has changed since, so an old session never resumes
/// in another repo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReopenPlan {
    /// The engine the tab runs on after the reopen.
    pub(crate) engine: CodeEngine,
    /// The model pin to run on (`None` = the engine's own default).
    pub(crate) model: Option<String>,
    /// The project folder the recorded session belongs to.
    pub(crate) folder: Option<String>,
    /// The engine session/thread id to `--resume` (when the engine supports it).
    pub(crate) session: Option<String>,
    /// `false` when the meta was missing or unusable: the tab falls back to
    /// Claude and the caller says so.
    pub(crate) engine_known: bool,
    pub(crate) route: ConversationRoute,
}

/// The plan a reopen applies, from the `code_conv:<id>` `app_state` value.
/// Missing or unusable (malformed JSON, non-`agent:` executor) → Claude with
/// no model, folder or session and `engine_known: false`.
pub(crate) fn reopen_plan(meta_json: Option<&str>) -> ReopenPlan {
    let Some(meta) = meta_json.and_then(CodeConvMeta::from_json) else {
        return ReopenPlan {
            engine: CodeEngine::Claude,
            model: None,
            folder: None,
            session: None,
            engine_known: false,
            route: ConversationRoute::new(WorkExecutor::Agent {
                engine: CodeEngine::Claude,
                model: None,
            }),
        };
    };
    let Some((engine, model)) = meta.engine_model() else {
        return ReopenPlan {
            engine: CodeEngine::Claude,
            model: None,
            folder: None,
            session: None,
            engine_known: false,
            route: ConversationRoute::new(WorkExecutor::Agent {
                engine: CodeEngine::Claude,
                model: None,
            }),
        };
    };
    let legacy_route = ConversationRoute::from_legacy(
        WorkExecutor::Agent {
            engine,
            model: model.clone(),
        },
        meta.folder.clone(),
        meta.session.clone(),
        awili_code::continuity::TranscriptCursor(0),
    );
    ReopenPlan {
        engine,
        model,
        folder: meta.folder,
        session: meta.session,
        engine_known: true,
        route: meta
            .route
            .as_deref()
            .and_then(|raw| ConversationRoute::from_persisted_json(raw).ok())
            .unwrap_or(legacy_route),
    }
}

/// Load a persisted conversation into `session`: engine, model, project
/// folder, the resume slot (set under its lock), the transcript and rows
/// rebuilt from it, the error marks cleared, streaming off, and the tab's
/// history bound to the conversation (no in-flight turn — a reopen is a
/// fresh start).
pub(crate) fn apply_reopen(
    session: &mut CodeSession,
    conv_id: &str,
    convo: ConversationState,
    plan: &ReopenPlan,
) {
    session.engine = plan.engine;
    session.model = plan.model.clone();
    session.route = plan.route.clone();
    session.project_folder = plan.folder.clone();
    if let Ok(mut g) = session.session_slot.lock() {
        *g = plan.session.clone();
    }
    session.reply_presentation_token = crate::conv_view::TurnToken::fresh();
    session.convo = convo;
    session.rows = session
        .convo
        .messages()
        .iter()
        .map(crate::MessageRow::new)
        .collect();
    session.errored.clear();
    session.streaming = false;
    // No external session marker can establish which stored answer turns it
    // saw. Re-ground once after reopening, then resume normally after success.
    session.context_gap = !session.convo.messages().is_empty();
    session.history.conv_id = Some(conv_id.to_string());
    session.history.turn = None;
}

/// Whether the `code_conv` meta reply for `requested` still applies when it
/// arrives: the app is still in Code mode and `requested` is still the
/// selected conversation. A reply the user has moved away from (another
/// conversation selected, or Code mode left) is dropped, so it can never
/// focus an old Code conversation.
pub(crate) fn reopen_reply_is_current(requested: &str, mode: Mode, active_id: &str) -> bool {
    mode == Mode::Code && active_id == requested
}

/// U1d app glue: reopening Code conversations. `new_conversation`,
/// `select_conversation` and `open_conversation_from_history` (main.rs) call
/// these.
impl crate::AwiliApp {
    /// Open `id` through the Code-tab path when it is a Code conversation and
    /// return `true`; `false` (nothing done) for any other kind.
    pub(crate) fn open_if_code_conversation(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let is_code = self
            .conversations
            .get(id)
            .is_some_and(|c| c.kind == ConvKind::Code);
        if is_code {
            self.open_code_conversation(id, cx);
        }
        is_code
    }

    /// Open a persisted Code conversation. Switch to Code mode, select and
    /// remember the conversation, then read its `code_conv:<id>` meta through
    /// the ordered persistence worker and restore a tab from it. The reply is
    /// applied only while it is still current ([`reopen_reply_is_current`]).
    pub(crate) fn open_code_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.mode != Mode::Code {
            self.set_mode(Mode::Code, cx);
        }
        self.set_active_conversation(id);
        self.save_active_conversation();
        self.work.code_reopen_generation = self.work.code_reopen_generation.wrapping_add(1);
        let generation = self.work.code_reopen_generation;
        let Some(persist) = self.persist.as_ref() else {
            self.finish_code_reopen(id, None, cx);
            return;
        };
        let (tx, rx) = async_channel::bounded(1);
        persist.send(PersistCmd::ReadState {
            key: code_conv_key(id),
            reply: tx,
        });
        let requested = id.to_string();
        cx.spawn(async move |this, cx| {
            let value = rx.recv().await.ok().flatten();
            this.update(cx, |app, cx| {
                if app.work.code_reopen_generation == generation
                    && reopen_reply_is_current(&requested, app.mode, app.sidebar.active_id())
                {
                    app.finish_code_reopen(&requested, value.as_deref(), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Apply a `code_conv` meta to a Code tab for `id`: focus an existing tab,
    /// reuse the active blank one, or add a new one; restore the transcript
    /// and the engine/folder/resume binding; toast when the engine was
    /// unrecorded.
    fn finish_code_reopen(&mut self, id: &str, meta: Option<&str>, cx: &mut Context<Self>) {
        // A double click must not open two tabs for one conversation.
        if let Some(ix) = tab_for_conv(&self.work.code_tabs, id) {
            // The same native session can have spent intervening turns on the
            // in-app adapter. Its runtime identity remains valid, but the
            // durable shared view is the transcript source of truth when the
            // user reopens this history item.
            if !self.work.code_tabs[ix].streaming {
                if let Some(convo) = self.views.get(id).map(|view| view.convo.clone()) {
                    let session = &mut self.work.code_tabs[ix];
                    session.convo = convo;
                    session.rows = session
                        .convo
                        .messages()
                        .iter()
                        .map(crate::MessageRow::new)
                        .collect();
                }
            }
            self.activate_code_tab(ix);
            cx.notify();
            return;
        }
        // The conversation was deleted while the meta was being read.
        if self.conversations.get(id).is_none() {
            return;
        }
        if let Some(saved_route) = meta
            .and_then(CodeConvMeta::from_json)
            .and_then(|saved| saved.route)
        {
            if ConversationRoute::from_persisted_json(&saved_route).is_err()
                && !self.work.conversation_routes.contains_key(id)
            {
                self.work
                    .unavailable_conversation_routes
                    .insert(id.to_string(), saved_route);
            }
        }
        let mut plan = reopen_plan(meta);
        // `conversation_route:<id>` is the canonical persisted representation.
        // CodeConvMeta.route remains read compatibility only when that key was
        // absent at startup.
        if let Some(route) = self.work.conversation_routes.get(id).cloned() {
            plan.route = route;
        } else if meta
            .and_then(CodeConvMeta::from_json)
            .is_some_and(|saved| saved.route.is_none())
        {
            let restored = self
                .views
                .get(id)
                .map(crate::work::bridge_records_from_view)
                .unwrap_or_default();
            plan.route = ConversationRoute::from_legacy(
                WorkExecutor::Agent {
                    engine: plan.engine,
                    model: plan.model.clone(),
                },
                plan.folder.clone(),
                plan.session.clone(),
                awili_code::continuity::canonical_cursor(&restored),
            );
        }
        let reusable =
            reusable_blank_tab(&self.work.code_tabs, self.work.code_active).filter(|ix| {
                !self.workshop_agent_holds_draft(
                    &crate::workshop::Backing::Agent(self.work.code_tabs[*ix].id),
                    cx,
                )
            });
        let ix = match reusable {
            Some(ix) => ix,
            None => {
                self.new_code_tab(plan.engine);
                self.work.code_tabs.len() - 1
            }
        };
        let convo = self
            .views
            .get(id)
            .map(|v| v.convo.clone())
            .unwrap_or_default();
        apply_reopen(&mut self.work.code_tabs[ix], id, convo, &plan);
        // An unreadable saved target does not prevent opening the stored
        // transcript. Keep it unavailable in the chooser and block Send
        // until an explicit replacement is selected; do not silently seed a
        // usable route while restoring this native tab.
        if !self.work.unavailable_conversation_routes.contains_key(id) {
            self.work
                .conversation_routes
                .entry(id.to_string())
                .or_insert_with(|| plan.route.clone());
        }
        self.activate_code_tab(ix);
        if !plan.engine_known {
            self.show_toast(
                "Reopened on Claude: this conversation's engine was not recorded",
                cx,
            );
        }
        cx.notify();
    }

    /// A new Code tab running the active tab's engine (Claude when there is
    /// no active tab): "+ New" and cmd+N in Code mode. RC2-UX (R2-25, D-R1
    /// M9c): a tab that has not run yet is already new, so it is kept (no
    /// second empty tab); a new tab starts in the active tab's project.
    pub(crate) fn new_code_tab_like_active(&mut self, cx: &mut Context<Self>) {
        // Even reuse is a new-thread intent; an earlier history metadata
        // reply must not replace the blank thread the user just chose.
        self.work.code_reopen_generation = self.work.code_reopen_generation.wrapping_add(1);
        if reusable_blank_tab(&self.work.code_tabs, self.work.code_active)
            .filter(|ix| {
                !self.workshop_agent_holds_draft(
                    &crate::workshop::Backing::Agent(self.work.code_tabs[*ix].id),
                    cx,
                )
            })
            .is_some()
        {
            cx.notify();
            return;
        }
        let active = self.work.code_tabs.get(self.work.code_active);
        let engine = active.map(|s| s.engine).unwrap_or(CodeEngine::Claude);
        let folder = active.and_then(|s| s.project_folder.clone());
        self.new_code_tab(engine);
        let ix = self.work.code_tabs.len() - 1;
        self.work.code_tabs[ix].project_folder = folder;
        cx.notify();
    }
}

// ---- RC2-UX (R2-17): Code tabs come back after a relaunch -------------------

/// The `app_state` key of the open Code tabs (JSON [`SavedCodeTabs`]).
pub(crate) const CODE_OPEN_TABS_KEY: &str = "code_open_tabs";

/// One open Code tab as saved: its conversation (a tab that has not run yet
/// has none), its helper (the Work-lane executor string) and its project.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedCodeTab {
    pub(crate) conv: Option<String>,
    pub(crate) executor: String,
    pub(crate) folder: Option<String>,
}

/// The open Code tabs and which one is on screen.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedCodeTabs {
    pub(crate) tabs: Vec<SavedCodeTab>,
    pub(crate) active: usize,
}

/// The conversation ids a saved list names (their `code_conv` metas are read
/// at launch with it).
pub(crate) fn saved_tab_conversations(json: &str) -> Vec<String> {
    serde_json::from_str::<SavedCodeTabs>(json)
        .map(|s| s.tabs.into_iter().filter_map(|t| t.conv).collect())
        .unwrap_or_default()
}

impl crate::AwiliApp {
    /// The open Code tabs as they would be saved.
    pub(crate) fn code_tabs_snapshot(&self) -> SavedCodeTabs {
        SavedCodeTabs {
            tabs: self
                .work
                .code_tabs
                .iter()
                .map(|t| SavedCodeTab {
                    conv: t.history.conv_id.clone(),
                    executor: crate::work::WorkExecutor::Agent {
                        engine: t.engine,
                        model: t.model.clone(),
                    }
                    .to_persist(),
                    folder: t.project_folder.clone(),
                })
                .collect(),
            active: self.work.code_active,
        }
    }

    /// Save the open Code tabs when they changed since the last save (called
    /// every frame; a write only on a change).
    #[cfg(test)]
    pub(crate) fn save_code_tabs_if_changed(&mut self) {
        if self.work.code_tabs.is_empty() && self.work.code_tabs_saved.is_none() {
            return;
        }
        let Ok(json) = serde_json::to_string(&self.code_tabs_snapshot()) else {
            return;
        };
        if self.work.code_tabs_saved.as_deref() == Some(json.as_str()) {
            return;
        }
        if let Some(persist) = self.persist.as_ref() {
            persist.set_state(CODE_OPEN_TABS_KEY, json.clone());
        }
        self.work.code_tabs_saved = Some(json);
    }

    /// At launch: open the Code tabs that were open when Awili last closed —
    /// each with its transcript, helper, project and resume session (from its
    /// `code_conv` meta in `metas`), a tab that had not run yet as an empty tab
    /// on its helper and project. A conversation deleted since is skipped.
    pub(crate) fn restore_code_tabs(
        &mut self,
        json: Option<&str>,
        metas: &std::collections::HashMap<String, String>,
    ) {
        let Some(saved) = json.and_then(|j| serde_json::from_str::<SavedCodeTabs>(j).ok()) else {
            return;
        };
        let mut active = 0;
        for (i, tab) in saved.tabs.iter().enumerate() {
            if let Some(ix) = tab
                .conv
                .as_deref()
                .and_then(|id| tab_for_conv(&self.work.code_tabs, id))
            {
                if i == saved.active {
                    active = ix;
                }
                continue;
            }
            let (engine, model) = match crate::work::WorkExecutor::from_persist(&tab.executor) {
                Some(crate::work::WorkExecutor::Agent { engine, model }) => (engine, model),
                _ => (CodeEngine::Claude, None),
            };
            match &tab.conv {
                Some(conv) => {
                    if !self
                        .conversations
                        .get(conv)
                        .is_some_and(|c| c.kind == ConvKind::Code && !c.archived)
                    {
                        continue;
                    }
                    let mut plan = reopen_plan(metas.get(conv).map(String::as_str));
                    // RC2-FIXB2 (RC2-UX review P2): the conversation's helper
                    // and folder are saved at turn end, so a relaunch in the
                    // middle of the tab's first turn finds none; the saved
                    // tab's own helper and folder stand in.
                    if !plan.engine_known {
                        plan.engine = engine;
                        plan.model = model.clone();
                        plan.folder = tab.folder.clone();
                    }
                    let convo = self
                        .views
                        .get(conv)
                        .map(|v| v.convo.clone())
                        .unwrap_or_default();
                    self.new_code_tab(plan.engine);
                    let ix = self.work.code_tabs.len() - 1;
                    apply_reopen(&mut self.work.code_tabs[ix], conv, convo, &plan);
                }
                None => {
                    self.new_code_tab(engine);
                    let ix = self.work.code_tabs.len() - 1;
                    self.work.code_tabs[ix].model = model.clone();
                    self.work.code_tabs[ix]
                        .route
                        .set_next_target(WorkExecutor::Agent { engine, model });
                    self.work.code_tabs[ix].project_folder = tab.folder.clone();
                }
            }
            if i <= saved.active {
                active = self.work.code_tabs.len() - 1;
            }
        }
        if !self.work.code_tabs.is_empty() {
            self.activate_code_tab(active);
        }
        self.work.code_tabs_saved = serde_json::to_string(&self.code_tabs_snapshot()).ok();
    }
}

#[cfg(test)]
#[path = "code_reopen_tests.rs"]
mod code_reopen_tests;
