//! One ordered tab model for Workshop; execution remains in the owned adapters.
use std::collections::{BTreeMap, HashMap, HashSet};

use awili_core::ConvKind;
use gpui::{div, prelude::*, px, AnyElement, Context, FocusHandle, Pixels, SharedString, Window};

use crate::{
    job_panel::BarLevel,
    code_reopen::{apply_reopen, reopen_plan, SavedCodeTab},
    icons::Icon,
    theme::{self, Tone},
    ui,
    work::{Mode, WorkExecutor},
    AwiliApp,
};

pub(crate) const TABS_KEY: &str = "workshop_tabs";
/// The `app_state` key remembering whether the Conversations panel is
/// collapsed ("collapsed" or "open"; absent means open).
pub(crate) const HISTORY_PANEL_KEY: &str = "workshop_history_panel";
/// The docked Conversations panel's width: the same as the Job panel it yields
/// to, and as the left overlay it replaced.
pub(crate) const HISTORY_W: f32 = theme::PANEL_W;
/// The open-thread list scrolls past this height so the catalog keeps its room.
const HISTORY_OPEN_MAX_H: f32 = 176.0;
/// The title switcher lists this many recent conversations.
const SWITCHER_MAX: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Backing {
    Conversation(String),
    Agent(u64),
}

/// A history reader is transient; it never becomes an execution tab or a
/// persisted open thread. The previous owner remains available on return.
pub(crate) struct ArchivedReader {
    pub(crate) id: String,
    return_conversation: String,
    return_mode: Mode,
    return_tab: Option<String>,
}

#[cfg(test)]
#[path = "workshop_state_tests.rs"]
mod tests;

#[derive(Clone, Debug)]
pub(crate) struct Tab {
    pub(crate) id: String,
    pub(crate) backing: Backing,
}

pub(crate) struct Workshop {
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: Option<String>,
    pub(crate) project_import_notice: Option<String>,
    saved: Option<String>,
    queued: Option<(u64, String)>,
    save_generation: u64,
    pub(crate) save_error: Option<String>,
    pub(crate) restore_notice: bool,
    /// Failed or newer snapshots are retained verbatim, never overwritten by
    /// an ordinary render. Recovery requires an atomic backup and replacement.
    pub(crate) unreadable: Option<String>,
    pub(crate) read_failed: bool,
    pub(crate) scroll: gpui::ScrollHandle,
    pub(crate) focus: FocusHandle,
    pub(crate) menu: Option<WorkshopMenu>,
    menu_selected: usize,
    menu_focus: FocusHandle,
    menu_placement: ui::PopoverState,
    pub(crate) header_action_focus: [FocusHandle; 7],
    pub(crate) header_action_bounds:
        [std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>; 7],
    /// The Conversations panel: the owner's collapse choice (persisted), whether
    /// it was changed before the saved value was read, and the header toggle.
    pub(crate) history_collapsed: bool,
    history_touched: bool,
    history_restore_started: bool,
    /// The sheet was on screen at the last frame, and was opened from a
    /// collapsed panel (so a dismissal puts the collapse back).
    history_sheet_shown: bool,
    sheet_from_collapsed: bool,
    /// The open-thread list was last scrolled for this selection and count.
    open_scroll_key: Option<(Option<String>, usize)>,
    pending_open_scroll: Option<usize>,
    history_focus: FocusHandle,
    history_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    close_focus: std::collections::BTreeMap<String, FocusHandle>,
    pub(crate) engine_focus: FocusHandle,
    project_focus: FocusHandle,
    /// The "more" menu trigger, and the title (conversation switcher) face.
    pub(crate) more_focus: FocusHandle,
    more_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    title_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// The run capsule's once-a-second repaint is running.
    clock_running: bool,
    /// The Conversations panel's title filter.
    pub(crate) history_query: String,
    pub(crate) engine_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    project_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    pub(crate) delete_project: Option<(i64, String, String)>,
    managed_project: Option<awili_persist::StoredProject>,
    pub(crate) resources: crate::workshop_resources::WorkshopResources,
    delete_cancel_focus: FocusHandle,
    delete_confirm_focus: FocusHandle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkshopMenu {
    Engine,
    Project,
    ProjectList,
    ProjectActions,
    Files,
    Outputs,
    RecentSessions,
    /// The title chip's conversation switcher.
    Switcher,
    /// The bar's more (⋯) menu.
    More,
}

impl Workshop {
    pub(crate) fn new(cx: &mut Context<AwiliApp>) -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            project_import_notice: None,
            saved: None,
            queued: None,
            save_generation: 0,
            save_error: None,
            restore_notice: false,
            unreadable: None,
            read_failed: false,
            scroll: gpui::ScrollHandle::new(),
            focus: cx.focus_handle(),
            menu: None,
            menu_selected: 0,
            menu_focus: cx.focus_handle(),
            menu_placement: ui::PopoverState::new(),
            header_action_focus: std::array::from_fn(|_| cx.focus_handle()),
            header_action_bounds: std::array::from_fn(|_| Default::default()),
            history_collapsed: false,
            history_touched: false,
            history_restore_started: false,
            history_sheet_shown: false,
            sheet_from_collapsed: false,
            open_scroll_key: None,
            pending_open_scroll: None,
            history_focus: cx.focus_handle(),
            history_bounds: Default::default(),
            close_focus: Default::default(),
            engine_focus: cx.focus_handle(),
            project_focus: cx.focus_handle(),
            more_focus: cx.focus_handle(),
            more_bounds: Default::default(),
            title_bounds: Default::default(),
            clock_running: false,
            history_query: String::new(),
            engine_bounds: Default::default(),
            project_bounds: Default::default(),
            delete_project: None,
            managed_project: None,
            resources: Default::default(),
            delete_cancel_focus: cx.focus_handle(),
            delete_confirm_focus: cx.focus_handle(),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedWorkshop {
    version: u32,
    tabs: Vec<SavedTab>,
    active: Option<String>,
    #[serde(default)]
    work_helpers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedTab {
    id: String,
    backing: SavedBacking,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
enum SavedBacking {
    Conversation(String),
    Agent(SavedCodeTab),
}

impl SavedWorkshop {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let saved: Self = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        if saved.version != 1 {
            return Err("unsupported Workshop state version".into());
        }
        let mut ids = HashSet::new();
        let mut conversations = HashSet::new();
        for tab in &saved.tabs {
            if uuid::Uuid::parse_str(&tab.id).is_err() || !ids.insert(tab.id.clone()) {
                return Err("invalid or repeated Workshop tab identity".into());
            }
            let conv = match &tab.backing {
                SavedBacking::Conversation(id) => Some(id),
                SavedBacking::Agent(agent) => {
                    if !matches!(
                        WorkExecutor::from_persist(&agent.executor),
                        Some(WorkExecutor::Agent { .. })
                    ) {
                        return Err("unknown Workshop agent".into());
                    }
                    agent.conv.as_ref()
                }
            };
            if let Some(conv) = conv {
                if conv.is_empty() || !conversations.insert(conv.clone()) {
                    return Err("invalid or repeated Workshop conversation".into());
                }
            }
        }
        // A missing active id may refer to a conversation deleted since the
        // snapshot. Restore picks another surviving tab without changing order.
        for (id, helper) in &saved.work_helpers {
            if !saved
                .tabs
                .iter()
                .any(|tab| matches!(&tab.backing, SavedBacking::Conversation(conv) if conv == id))
                || !matches!(WorkExecutor::from_persist(helper), Some(exec) if !matches!(exec, WorkExecutor::LocalBuild { .. }))
            {
                return Err("invalid Workshop thread helper".into());
            }
        }
        Ok(saved)
    }

    pub(crate) fn agent_conversations(&self) -> Vec<String> {
        self.tabs
            .iter()
            .filter_map(|tab| match &tab.backing {
                SavedBacking::Agent(agent) => agent.conv.clone(),
                SavedBacking::Conversation(_) => None,
            })
            .collect()
    }
}

impl AwiliApp {
    pub(crate) fn workshop_conversation_backing(&self, id: &str) -> Backing {
        // Once a native thread has crossed through the in-app adapter, its
        // stable Workshop tab is conversation-backed. Keep that tab identity
        // rather than reviving a second Agent tab merely because the native
        // session remains available for a later turn.
        if self
            .shell
            .tabs
            .tabs
            .iter()
            .any(|tab| matches!(&tab.backing, Backing::Conversation(conv) if conv == id))
        {
            return Backing::Conversation(id.to_string());
        }
        self.work
            .code_tabs
            .iter()
            .find(|s| s.history.conv_id.as_deref() == Some(id))
            .map(|s| Backing::Agent(s.id))
            .unwrap_or_else(|| Backing::Conversation(id.to_string()))
    }

    /// A native-backed tab can dispatch its next turn through the existing
    /// Work adapter. Keep the same Workshop tab and stable conversation id;
    /// only its presentation backing changes. The native session map remains
    /// on the conversation route for a later return to the harness.
    pub(crate) fn workshop_convert_agent_backing(
        &mut self,
        session_id: u64,
        conversation_id: &str,
    ) {
        for tab in &mut self.shell.tabs.tabs {
            if tab.backing == Backing::Agent(session_id) {
                tab.backing = Backing::Conversation(conversation_id.to_string());
            }
        }
        self.sync_workshop_tabs();
    }

    pub(crate) fn workshop_agent_holds_draft(&self, backing: &Backing, cx: &Context<Self>) -> bool {
        let Backing::Agent(session) = backing else {
            return false;
        };
        let key = crate::composer_drafts::code_tab_key(*session);
        self.drafts.has_draft(&key)
            || (self.drafts.is_shown(&key) && !self.composer.read(cx).text().trim().is_empty())
    }

    pub(crate) fn refuse_archived_workshop_send(
        &mut self,
        id: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .conversations
            .get(id)
            .is_some_and(|c| c.archived && c.kind != ConvKind::Council)
        {
            self.refuse_submit(
                "Restore the archived thread in Conversations before sending.".into(),
                cx,
            );
            true
        } else {
            false
        }
    }

    pub(crate) fn open_workshop_archive(&mut self, id: &str, cx: &mut Context<Self>) {
        // A stored reader has no visible voice-input controls. Retire queued
        // transcripts, stop capture, and disarm holds before hiding the editor.
        self.voice.ptt_guard.key_down();
        self.voice.ptt_held = false;
        self.invalidate_dictation();
        self.voice.ptt_session = false;
        self.voice.conversation_mode = false;
        self.voice.first_send_confirmed = false;
        self.stop_voice_listening_internal();
        self.stop_speaking_internal();
        self.voice.status = None;
        self.composer.update(cx, |c, cx| c.dictation_cancel(cx));
        self.work.code_reopen_generation = self.work.code_reopen_generation.wrapping_add(1);
        let prior = self.shell.archived_reader.take();
        self.shell.archived_reader = Some(ArchivedReader {
            id: id.to_string(),
            return_conversation: prior
                .as_ref()
                .map(|p| p.return_conversation.clone())
                .unwrap_or_else(|| self.sidebar.active_id().to_string()),
            return_mode: prior.as_ref().map(|p| p.return_mode).unwrap_or(self.mode),
            return_tab: prior
                .as_ref()
                .and_then(|p| p.return_tab.clone())
                .or_else(|| self.shell.tabs.active.clone()),
        });
        self.set_active_conversation(id);
        // The common stored transcript is read without constructing a native
        // Code runtime or changing the Work runner/helper/folder.
        self.mode = Mode::Work;
        self.shell.tabs.menu = None;
        cx.notify();
    }

    pub(crate) fn close_workshop_archive(&mut self, cx: &mut Context<Self>) {
        let Some(prior) = self.shell.archived_reader.take() else {
            return;
        };
        self.sync_workshop_tabs();
        if crate::consolidation::workshop_mode(prior.return_mode) {
            self.mode = prior.return_mode;
            // A blank native runtime has no durable ID to select yet. Do not
            // leave the transient archived row as its sidebar/attachment owner.
            let return_conversation = self
                .conversations
                .get(&prior.return_conversation)
                .filter(|c| !c.archived && c.kind != ConvKind::Council)
                .map(|c| c.id.clone())
                .or_else(|| {
                    self.conversations
                        .active()
                        .into_iter()
                        .find(|c| c.kind != ConvKind::Council)
                        .map(|c| c.id.clone())
                })
                .unwrap_or_else(|| self.create_conversation(ConvKind::Work));
            self.set_active_conversation(&return_conversation);
            let target = prior
                .return_tab
                .filter(|id| self.shell.tabs.tabs.iter().any(|tab| &tab.id == id))
                .or_else(|| self.shell.tabs.active.clone());
            if let Some(id) = target {
                self.select_workshop_tab(&id, cx);
            } else {
                let (id, _) = self.fresh_conversation(ConvKind::Work);
                self.open_conversation_from_history(&id, cx);
            }
            self.refresh_staged_attachments();
            self.save_active_conversation();
            return;
        }
        if self
            .conversations
            .get(&prior.return_conversation)
            .is_some_and(|c| !c.archived)
        {
            self.set_active_conversation(&prior.return_conversation);
        }
        self.set_mode(prior.return_mode, cx);
        if prior.return_mode == Mode::Work {
            self.activate_kind(crate::code_reopen::sidebar_kind_for(prior.return_mode), cx);
        }
        self.sync_composer_draft(cx);
        cx.notify();
    }

    pub(crate) fn workshop_archive_view(
        &self,
        header: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = self.shell.archived_reader.as_ref().unwrap().id.clone();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                ui::Banner::new(
                    "awili-archived-reader",
                    Tone::Neutral,
                    "Archived transcript — restore this thread to send again.",
                )
                .selector("awili-archive-readonly")
                .action(
                    ui::Button::new("awili-archive-restore", "Restore thread").on_click(
                        cx.listener(move |app, _, window, cx| {
                            // Restore chooses this thread directly. Back's
                            // survivor/fresh-thread routing is a separate action.
                            if let Some(reader) = app.shell.archived_reader.take() {
                                // The reader uses a transient Chat rendering
                                // mode. Restore normal mode-transition setup.
                                app.mode = reader.return_mode;
                            }
                            app.unarchive_conversation(&id, cx);
                            app.open_conversation_from_history(&id, cx);
                            app.focus_composer(window, cx);
                        }),
                    ),
                ),
            )
            .child(
                ui::Button::new("awili-archive-back", "Back to Workshop")
                    .quiet()
                    .on_click(cx.listener(|app, _, window, cx| {
                        app.close_workshop_archive(cx);
                        app.focus_composer(window, cx);
                    })),
            )
            .child(self.message_list_with_header(header, cx))
            .into_any_element()
    }

    /// Temporary composer shortcuts select an open Workshop thread. Closed
    /// transcripts stay in history until the user explicitly reopens them.
    pub(crate) fn open_workshop_adapter(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.close_workshop_archive(cx);
        let kind = match mode {
            Mode::Work => ConvKind::Work,
            _ => return,
        };
        let open = self
            .shell
            .tabs
            .tabs
            .iter()
            .rev()
            .find_map(|tab| match &tab.backing {
                Backing::Conversation(id)
                    if self.conversations.get(id).is_some_and(|c| c.kind == kind) =>
                {
                    Some(tab.id.clone())
                }
                _ => None,
            });
        if let Some(id) = open {
            self.select_workshop_tab(&id, cx);
        } else {
            if mode == Mode::Work && self.workshop_work_unsettled() {
                self.show_toast(
                    "Open the running job's thread before changing Work helpers.",
                    cx,
                );
                return;
            }
            let (id, _) = self.fresh_conversation(kind);
            self.open_conversation_from_history(&id, cx);
            self.sync_composer_draft(cx);
        }
        cx.notify();
    }

    pub(crate) fn dismiss_workshop_project_import(&mut self, cx: &mut Context<Self>) {
        if let Some(signature) = self.shell.tabs.project_import_notice.take() {
            if let Some(persist) = &self.persist {
                persist.set_state(crate::persistence::PROJECT_IMPORT_DISMISSED_KEY, signature);
            }
            cx.notify();
        }
    }
    pub(crate) fn close_workshop_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.inspector.check = None;
        if let Some(menu) = self.shell.tabs.menu.take() {
            self.shell.tabs.menu_placement.reset();
            let trigger = match menu {
                WorkshopMenu::Engine => &self.shell.tabs.engine_focus,
                WorkshopMenu::Switcher => &self.shell.tabs.header_action_focus[0],
                WorkshopMenu::More => &self.shell.tabs.more_focus,
                WorkshopMenu::Project
                | WorkshopMenu::ProjectList
                | WorkshopMenu::ProjectActions
                | WorkshopMenu::Files
                | WorkshopMenu::Outputs
                | WorkshopMenu::RecentSessions => &self.shell.tabs.project_focus,
            };
            window.focus(trigger, cx);
            cx.notify();
        }
    }

    pub(crate) fn open_workshop_helper_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shell.tabs.menu_placement.reset();
        self.shell.tabs.menu = Some(WorkshopMenu::Engine);
        self.ptt_key_down(cx);
        self.shell.tabs.menu_selected = 0;
        window.focus(&self.shell.tabs.menu_focus, cx);
        cx.notify();
    }

    /// Choosing an incompatible helper creates a new thread. Existing runner
    /// identities, transcripts, permissions and resume tokens stay with their
    /// original tabs; this operation never starts a turn.
    pub(crate) fn new_workshop_with(&mut self, executor: WorkExecutor, cx: &mut Context<Self>) {
        self.new_workshop_with_choice(executor, true, cx);
    }

    /// Start a project thread on its resolved default without treating that
    /// automatic seed as a manual helper pick.
    #[cfg(test)]
    pub(crate) fn new_workshop_from_default(
        &mut self,
        executor: WorkExecutor,
        cx: &mut Context<Self>,
    ) {
        self.new_workshop_with_choice(executor, false, cx);
    }

    fn new_workshop_with_choice(
        &mut self,
        executor: WorkExecutor,
        remember_choice: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_workshop_archive(cx);
        let folder = match self.mode {
            Mode::Code => self
                .work
                .code_tabs
                .get(self.work.code_active)
                .and_then(|tab| tab.project_folder.as_deref())
                .filter(|folder| self.code_project_folder_is_bound(folder))
                .map(str::to_string),
            Mode::Work => self
                .conversations
                .get(self.sidebar.active_id())
                .and_then(|conversation| conversation.cwd.clone()),
            _ => None,
        };
        match executor {
            WorkExecutor::Agent { engine, model } => {
                self.new_code_tab(engine);
                let tab = self.work.code_tabs.last_mut().unwrap();
                tab.model = model.clone();
                tab.route
                    .set_next_target(WorkExecutor::Agent { engine, model });
                tab.project_folder = folder.clone();
                self.set_mode(Mode::Code, cx);
                self.activate_code_tab(self.work.code_tabs.len() - 1);
                if !engine.enforces_ask() {
                    self.show_info_toast(
                        crate::ask_mode::no_ask_toast(
                            crate::ask_mode::code_support(engine),
                            engine.label(),
                        ),
                        cx,
                    );
                }
            }
            executor => {
                if self.workshop_work_unsettled() {
                    self.show_toast(
                        "Finish or stop the current job before opening another Work helper.",
                        cx,
                    );
                    return;
                }
                let open = self
                    .shell
                    .tabs
                    .tabs
                    .iter()
                    .filter_map(|tab| match &tab.backing {
                        Backing::Conversation(id) => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
                let (id, _) = self.fresh_conversation_except(ConvKind::Work, None, &open);
                if let Some(folder) = &folder {
                    self.conversations.get_mut(&id).unwrap().cwd = Some(folder.clone());
                    self.work.folder_picked.insert(id.clone(), folder.into());
                    if let Some(p) = &self.persist {
                        p.send(crate::PersistCmd::SetCwd {
                            id: id.clone(),
                            cwd: Some(folder.clone()),
                        });
                    }
                }
                self.open_conversation_from_history(&id, cx);
                if remember_choice {
                    self.pick_work_executor(executor, cx);
                } else {
                    self.work.chip_picked.insert(id, executor.clone());
                    self.apply_starting_helper(executor);
                }
            }
        }
        self.sync_workshop_tabs();
        self.sync_composer_draft(cx);
        cx.notify();
    }

    pub(crate) fn workshop_work_unsettled(&self) -> bool {
        self.workshop_work_in_flight()
            || self.conversations.all().iter().any(|c| {
                c.kind == ConvKind::Work
                    && self
                        .work
                        .plan_gates
                        .open_for(&crate::ask_mode::AskKey::Work(c.id.clone()))
                        .is_some()
            })
    }

    pub(crate) fn workshop_work_in_flight(&self) -> bool {
        self.workshop_work_runner_unsettled()
            || self.work.queued_after_stop.is_some()
            || self.work.local.health.pending_send.is_some()
            || self.work.health.gateway.pending_send.is_some()
    }

    pub(crate) fn workshop_pending_for(&self, id: &str) -> bool {
        self.work
            .local
            .health
            .pending_send
            .as_ref()
            .is_some_and(|p| p.conv_id == id)
            || self
                .work
                .health
                .gateway
                .pending_send
                .as_ref()
                .is_some_and(|p| p.conv_id == id)
            || self
                .work
                .queued_after_stop
                .as_ref()
                .is_some_and(|p| p.conv_id == id)
    }

    pub(crate) fn workshop_work_runner_unsettled(&self) -> bool {
        self.work.running
            || self.work.gateway_stop_pending
            || self.work.agent_gate_pending
            || self.work.panel.approval.is_some()
            || self.work.panel.credential.is_some()
    }

    /// Predispatch prompts own their conversation before a runner has adopted
    /// it. Remove only the selected owner's intents before a probe/Stop reply.
    pub(crate) fn cancel_workshop_pending(&mut self, cx: &mut Context<Self>) -> bool {
        let id = self.sidebar.active_id().to_string();
        self.sync_composer_draft(cx);
        let mut texts = Vec::new();
        if self
            .work
            .local
            .health
            .pending_send
            .as_ref()
            .is_some_and(|p| p.conv_id == id)
        {
            texts.push(self.work.local.health.pending_send.take().unwrap().text);
        }
        if self
            .work
            .health
            .gateway
            .pending_send
            .as_ref()
            .is_some_and(|p| p.conv_id == id)
        {
            texts.push(self.work.health.gateway.pending_send.take().unwrap().text);
        }
        if self
            .work
            .queued_after_stop
            .as_ref()
            .is_some_and(|p| p.conv_id == id)
        {
            texts.push(self.work.queued_after_stop.take().unwrap().text);
        }
        let cancelled = !texts.is_empty();
        for text in texts {
            self.hand_back_to_conversation(&id, &text, cx);
        }
        if cancelled {
            self.show_toast("Not sent. Your request is back in this thread's draft.", cx);
            self.settle_work_chip();
            self.settle_work_folder(cx);
            cx.notify();
        }
        cancelled
    }

    pub(crate) fn workshop_folder(&self) -> Option<String> {
        match self.mode {
            Mode::Code => self
                .work
                .code_tabs
                .get(self.work.code_active)
                .and_then(|tab| tab.project_folder.clone()),
            Mode::Work => self
                .conversations
                .get(self.sidebar.active_id())
                .and_then(|conversation| conversation.cwd.clone()),
            _ => None,
        }
    }

    pub(crate) fn choose_workshop_folder(&mut self, folder: String, cx: &mut Context<Self>) {
        if self.mode == Mode::Code {
            self.use_folder(&folder, cx);
            return;
        }
        if self.workshop_work_unsettled() {
            self.show_toast(
                "Wait for the current job or Stop before changing folders.",
                cx,
            );
            return;
        }
        self.use_folder(&folder, cx);
        self.sync_workshop_tabs();
    }

    #[cfg(test)]
    pub(crate) fn open_workshop_folder_box(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode != Mode::Code && self.workshop_work_unsettled() {
            self.show_toast(
                "Wait for the current job or waiting plan before changing folders.",
                cx,
            );
            return;
        }
        self.open_folder_box(window, cx);
    }

    /// A visible, native folder chooser shared by the header and Auto menu.
    pub(crate) fn pick_workshop_folder(&mut self, cx: &mut Context<Self>) {
        if self.workshop_work_unsettled() {
            self.show_toast(
                "Finish or stop the current turn before changing folders.",
                cx,
            );
            return;
        }
        let owner = self.shell.tabs.active.clone();
        let mode = self.mode;
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose project folder".into()),
        });
        cx.spawn(async move |this, cx| {
            let path = rx
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|p| p.into_iter().next());
            if let Some(path) = path {
                let _ = this.update(cx, |app, cx| {
                    if app.mode == mode && app.shell.tabs.active == owner {
                        app.choose_workshop_folder(path.to_string_lossy().into_owned(), cx);
                    }
                });
            }
        })
        .detach();
    }

    /// The top bar: where you are on the left (project, then the conversation
    /// title and its switcher), what it is doing on the right (the run
    /// capsule in its reserved slot, model, mode, New, the panel toggle and
    /// the more menu). Three control shapes only: ghost chip, accent button,
    /// tinted capsule.
    fn workshop_controls(
        &mut self,
        level: BarLevel,
        compact: bool,
        title: String,
        history_open: bool,
        job_island: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let narrow = level == BarLevel::Narrow;
        let folder = self.workshop_folder();
        let project = folder
            .as_deref()
            .and_then(|folder| self.work.projects.iter().find(|p| p.folder == folder))
            .map(|p| p.name.clone())
            .or_else(|| {
                folder.as_ref().map(|folder| {
                    std::path::Path::new(folder)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| folder.clone())
                })
            })
            .unwrap_or_else(|| "Choose folder".into());

        // Left: the project chip, a slash, the conversation title.
        let project_bounds = self.shell.tabs.project_bounds.clone();
        let project_chip = div()
            .flex_none()
            .on_children_prepainted(move |children, _, _| {
                if let Some(b) = children.first() {
                    project_bounds.set(Some(*b));
                }
            })
            .child(
                ui::Chip::new(
                    "awili-workshop-project",
                    if narrow {
                        String::new()
                    } else {
                        format!("{} \u{25be}", ui::chip::middle_truncate(&project))
                    },
                )
                .icon(Icon::Folder)
                .label(format!(
                    "Project {project} \u{2014} {}",
                    folder.unwrap_or_else(|| "Choose folder".into())
                ))
                .focus_handle(&self.shell.tabs.project_focus)
                .on_click(cx.listener(|app, _, window, cx| {
                    app.shell.tabs.menu_placement.reset();
                    app.shell.tabs.menu = Some(WorkshopMenu::Project);
                    app.ptt_key_down(cx);
                    app.shell.tabs.menu_selected = 0;
                    window.focus(&app.shell.tabs.menu_focus, cx);
                    cx.notify();
                })),
            );
        let archived = self.shell.archived_reader.is_some();
        let switcher = !archived;
        let title_bounds = self.shell.tabs.title_bounds.clone();
        let title_chip = div()
            .flex()
            .flex_shrink(1.)
            .min_w_0()
            .on_children_prepainted(move |children, _, _| {
                title_bounds.set(children.first().copied());
            })
            .child(
                ui::Chip::new(
                    "awili-workshop-title",
                    if switcher {
                        format!("{title} \u{25be}")
                    } else {
                        title.clone()
                    },
                )
                .label(format!("{title} \u{2014} switch conversation"))
                .tooltip(format!("{title} \u{2014} switch conversation (\u{21e7}\u{2318}K)"))
                .focus_handle(&self.shell.tabs.header_action_focus[0])
                .disabled_if(archived.then(|| "An archived transcript is open: restore it or go back first".into()))
                .on_click(cx.listener(|app, _, window, cx| app.open_workshop_switcher(window, cx))),
            );
        let left = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(theme::z(theme::SP_4))
            .children((!archived).then_some(project_chip))
            .children((!archived).then(|| {
                div()
                    .flex_none()
                    .child(ui::text::label("/").text_color_override(theme::p().faint))
            }))
            .child(title_chip);

        // Right: the capsule's slot, model, mode, New, panel toggle, more.
        let slot_w = level.slot_w();
        let slot = div()
            .debug_selector(|| "awili-run-slot".into())
            .flex()
            .flex_none()
            .items_center()
            .justify_end()
            .w(theme::z(slot_w))
            .min_w_0()
            .children(job_island);
        let mode = self
            .header_mode_chip(narrow, cx)
            .map(|chip| div().flex_none().child(chip));
        let new: AnyElement = if narrow {
            ui::IconButton::new("awili-new-conversation", Icon::Plus)
                .selector("ui-button-awili-new-conversation")
                .primary()
                .label("New conversation")
                .tooltip("New conversation \u{2014} \u{2318}N")
                .on_click(cx.listener(|app, _, window, cx| app.new_conversation(window, cx)))
                .into_any_element()
        } else {
            ui::Button::new("awili-new-conversation", "New conversation")
                .primary()
                .icon(Icon::Plus)
                .tooltip("New conversation \u{2014} \u{2318}N")
                .on_click(cx.listener(|app, _, window, cx| app.new_conversation(window, cx)))
                .into_any_element()
        };
        let toggle_label = if history_open {
            "Hide conversations"
        } else {
            "Show conversations"
        };
        let toggle_bounds = self.shell.tabs.history_bounds.clone();
        let toggle = ui::IconButton::new("awili-workshop-history-toggle", Icon::PanelRight)
            .selector("ui-button-awili-workshop-history-toggle")
            .label(toggle_label)
            .tooltip(format!("{toggle_label} \u{2014} \u{2318}\\"))
            .focus_handle(&self.shell.tabs.history_focus);
        // Lit while the panel is open.
        let toggle = if history_open { toggle.lead() } else { toggle.quiet() }
            .on_click(cx.listener(|app, _, _, cx| app.toggle_workshop_history(cx)));
        let more_bounds = self.shell.tabs.more_bounds.clone();
        let more = div()
            .flex_none()
            .on_children_prepainted(move |children, _, _| {
                more_bounds.set(children.first().copied());
            })
            .child(
                ui::IconButton::new("awili-workshop-more", Icon::MoreHorizontal)
                    .label("More")
                    .tooltip("More \u{2014} rename, copy, archive")
                    .focus_handle(&self.shell.tabs.more_focus)
                    .on_click(cx.listener(|app, _, window, cx| app.open_workshop_more(window, cx))),
            );
        let right = div()
            .flex()
            .items_center()
            .gap(theme::z(theme::SP_4))
            // Wide: one fixed cluster. In a compact window the cluster may wrap
            // within the width rather than leave the window.
            .when(!compact, |d| d.flex_none())
            .when(compact, |d| d.flex_wrap().min_w_0().max_w_full())
            .children((!archived).then_some(slot))
            .children(mode.filter(|_| !archived))
            .children((!archived).then(|| div().flex_none().child(new)))
            .children(self.shell.archived_reader.is_none().then(|| {
                div()
                    .flex_none()
                    .on_children_prepainted(move |children, _, _| {
                        toggle_bounds.set(children.first().copied());
                    })
                    .child(toggle)
            }))
            .children(self.shell.archived_reader.is_none().then_some(more));
        div()
            .id("awili-workshop-bar")
            .debug_selector(|| "awili-workshop-bar".into())
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .px(theme::z(theme::SP_12))
            .w_full()
            .min_w_0()
            .flex_none()
            .border_b_1()
            .border_color(theme::p().border)
            .when(compact, |d| d.flex_wrap().min_h(theme::bar_h()))
            .when(!compact, |d| d.h(theme::bar_h()))
            .child(left)
            .child(right)
            .into_any_element()
    }

    /// Repaint once a second while a run shows in the capsule, so its timer
    /// counts.
    fn start_run_clock(&mut self, cx: &mut Context<Self>) {
        self.shell.tabs.clock_running = true;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            let keep = this
                .update(cx, |app, cx| {
                    let running = app.work.running;
                    if !running {
                        app.shell.tabs.clock_running = false;
                    }
                    cx.notify();
                    running
                })
                .unwrap_or(false);
            if !keep {
                break;
            }
        })
        .detach();
    }

    /// Open the conversation switcher under the title.
    pub(crate) fn open_workshop_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shell.archived_reader.is_some() {
            return;
        }
        self.shell.tabs.menu_placement.reset();
        self.shell.tabs.menu = Some(WorkshopMenu::Switcher);
        self.ptt_key_down(cx);
        self.shell.tabs.menu_selected = 0;
        window.focus(&self.shell.tabs.menu_focus, cx);
        cx.notify();
    }

    /// Open the bar's more (⋯) menu.
    pub(crate) fn open_workshop_more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.tabs.menu_placement.reset();
        self.shell.tabs.menu = Some(WorkshopMenu::More);
        self.ptt_key_down(cx);
        self.shell.tabs.menu_selected = 0;
        window.focus(&self.shell.tabs.menu_focus, cx);
        cx.notify();
    }

    /// The stored conversation the visible thread is: a Work thread, or the
    /// conversation a Code tab has been saved as. A blank Code tab has none.
    pub(crate) fn workshop_active_conversation(&self) -> Option<String> {
        match self.workshop_backing()? {
            Backing::Conversation(id) => Some(id),
            Backing::Agent(tab) => self
                .work
                .code_tabs
                .iter()
                .find(|s| s.id == tab)
                .and_then(|s| s.history.conv_id.clone()),
        }
        .filter(|id| self.conversations.get(id).is_some_and(|c| !c.archived))
    }

    /// Rename… in the more menu: the same inline editor the panel rows use,
    /// so the panel opens first.
    pub(crate) fn rename_workshop_conversation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.workshop_active_conversation() else {
            return;
        };
        self.set_workshop_history_open(true, cx);
        self.begin_rename(&id, window, cx);
    }

    /// Delete… in the more menu: the panel row's own inline confirm.
    pub(crate) fn delete_workshop_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.workshop_active_conversation() else {
            return;
        };
        self.set_workshop_history_open(true, cx);
        self.pending_delete = Some(id);
        cx.notify();
    }

    pub(crate) fn archive_workshop_conversation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.workshop_active_conversation() {
            self.archive_conversation(&id, window, cx);
        }
    }

    /// New Code tab in this folder: a fresh native Code thread (the conversation
    /// does not carry over) in this conversation's folder, on the helper its
    /// next turn would use (Claude when that is not a Code engine). The folder
    /// is checked as the other Code entry points do: a bare HOME is not a
    /// project binding. Already in a Code tab: nothing to open.
    pub(crate) fn open_workshop_as_code_tab(&mut self, cx: &mut Context<Self>) {
        if self.mode == Mode::Code {
            return;
        }
        let executor = self
            .composer_model_owner()
            .and_then(|owner| self.composer_target_for(&owner))
            .filter(|target| matches!(target, WorkExecutor::Agent { .. }))
            .unwrap_or(WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: None,
            });
        let folder = self
            .workshop_folder()
            .filter(|folder| self.code_project_folder_is_bound(folder));
        self.new_workshop_with(executor, cx);
        if let Some(tab) = self.work.code_tabs.last_mut() {
            tab.project_folder = folder;
        }
    }

    pub(crate) fn workshop_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let kind = self.shell.tabs.menu?;
        let (anchor, trigger) = match kind {
            WorkshopMenu::Engine => (
                self.shell.tabs.engine_bounds.get()?,
                &self.shell.tabs.engine_focus,
            ),
            WorkshopMenu::Switcher => (
                self.shell.tabs.title_bounds.get()?,
                &self.shell.tabs.header_action_focus[0],
            ),
            WorkshopMenu::More => (
                self.shell.tabs.more_bounds.get()?,
                &self.shell.tabs.more_focus,
            ),
            WorkshopMenu::Project
            | WorkshopMenu::ProjectList
            | WorkshopMenu::ProjectActions
            | WorkshopMenu::Files
            | WorkshopMenu::Outputs
            | WorkshopMenu::RecentSessions => (
                self.shell.tabs.project_bounds.get()?,
                &self.shell.tabs.project_focus,
            ),
        };
        let handle = cx.entity().downgrade();
        let move_handle = handle.clone();
        let mut menu = ui::Menu::new("awili-workshop-menu")
            .anchor(anchor)
            .popover_state(self.shell.tabs.menu_placement.clone())
            .focus_handle(&self.shell.tabs.menu_focus)
            .trigger_focus(trigger)
            .selected(self.shell.tabs.menu_selected)
            .on_close(move |window, cx| {
                let _ = handle.update(cx, |app, cx| app.close_workshop_menu(window, cx));
            })
            .on_move(move |selected, _, cx| {
                let _ = move_handle.update(cx, |app, cx| {
                    app.shell.tabs.menu_selected = selected;
                    cx.notify();
                });
            });
        match kind {
            WorkshopMenu::Engine => {
                for (label, executor) in [
                    ("New thread · Awili", WorkExecutor::InApp),
                    ("New thread · Gateway", WorkExecutor::Gateway),
                    (
                        "New thread · Claude",
                        WorkExecutor::Agent {
                            engine: awili_code::CodeEngine::Claude,
                            model: None,
                        },
                    ),
                    (
                        "New thread · Codex",
                        WorkExecutor::Agent {
                            engine: awili_code::CodeEngine::Codex,
                            model: None,
                        },
                    ),
                    (
                        "New thread · GLM",
                        WorkExecutor::Agent {
                            engine: awili_code::CodeEngine::Glm,
                            model: None,
                        },
                    ),
                    (
                        "New thread · DeepSeek",
                        WorkExecutor::Agent {
                            engine: awili_code::CodeEngine::Dsh,
                            model: None,
                        },
                    ),
                ] {
                    let check = self.shell.inspector.check.clone();
                    let item = ui::MenuItem::new(label).on_click(cx.listener(
                        move |app, _, window, cx| {
                            app.choose_reply_check_helper(
                                executor.clone(),
                                check.clone(),
                                window,
                                cx,
                            );
                        },
                    ));
                    menu = menu.item(item);
                }
            }
            WorkshopMenu::Project => {
                if self.mode == Mode::Work {
                    menu =
                        menu.item(ui::MenuItem::new("Conversations").on_click(
                            cx.listener(|app, _, _, cx| app.toggle_workshop_history(cx)),
                        ))
                        .separator();
                }
                for project in &self.work.projects {
                    let folder = project.folder.clone();
                    menu = menu.item(
                        ui::MenuItem::new(project.name.clone())
                            .icon(Icon::Folder)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.choose_workshop_folder(folder.clone(), cx)
                            })),
                    );
                }
                menu = menu.separator().item(
                    ui::MenuItem::new("Choose project folder…")
                        .icon(Icon::Folder)
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.pick_workshop_folder(cx);
                        })),
                );
                if self.mode == Mode::Work {
                    menu = self.workshop_project_actions(menu, cx);
                    if !self.work.projects.is_empty() {
                        menu = menu.separator().item(
                            ui::MenuItem::new("Manage saved projects…").on_click(cx.listener(
                                |app, _, window, cx| {
                                    app.show_project_menu(WorkshopMenu::ProjectList, window, cx)
                                },
                            )),
                        );
                    }
                    menu = self.workshop_resource_actions(menu, cx);
                }
            }
            WorkshopMenu::ProjectList => {
                menu = menu
                    .item(
                        ui::MenuItem::new("Back to folder menu").on_click(cx.listener(
                            |app, _, window, cx| {
                                app.show_project_menu(WorkshopMenu::Project, window, cx)
                            },
                        )),
                    )
                    .separator();
                for project in &self.work.projects {
                    let project = project.clone();
                    menu = menu.item(
                        ui::MenuItem::new(project.name.clone())
                            .icon(Icon::Folder)
                            .on_click(cx.listener(move |app, _, window, cx| {
                                app.shell.tabs.managed_project = Some(project.clone());
                                app.show_project_menu(WorkshopMenu::ProjectActions, window, cx);
                            })),
                    );
                }
            }
            WorkshopMenu::ProjectActions => {
                menu = menu
                    .item(
                        ui::MenuItem::new("Back to saved projects").on_click(cx.listener(
                            |app, _, window, cx| {
                                app.show_project_menu(WorkshopMenu::ProjectList, window, cx)
                            },
                        )),
                    )
                    .separator();
                if let Some(project) = &self.shell.tabs.managed_project {
                    let folder = project.folder.clone();
                    menu = menu.item(
                        ui::MenuItem::new(format!("Open {}", project.name))
                            .icon(Icon::Folder)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.choose_workshop_folder(folder.clone(), cx);
                            })),
                    );
                    menu = Self::saved_project_actions(menu, project, cx);
                }
            }
            WorkshopMenu::Switcher => {
                // The same conversations, in the same order, as the panel lists; the
                // open one is always in it, checked.
                let active = self.workshop_active_conversation();
                let mut ids: Vec<String> = crate::sidebar::workshop_sections(
                    &self.conversations,
                    Some(self.profile_store.active()),
                    self.profile_store.show_all(),
                    self.sidebar.active_id(),
                )
                .into_iter()
                .flat_map(|group| group.session_ids)
                .collect();
                ids.truncate(SWITCHER_MAX);
                if let Some(active) = active.as_ref().filter(|id| !ids.contains(id)) {
                    ids.push(active.clone());
                }
                for id in ids {
                    let Some(meta) = self.conversations.get(&id) else {
                        continue;
                    };
                    let mut item = ui::MenuItem::new(meta.title.clone());
                    if active.as_deref() == Some(meta.id.as_str()) {
                        item = item.icon(Icon::Check);
                    }
                    menu = menu.item(item.on_click(cx.listener(move |app, _, window, cx| {
                        app.select_conversation(&id, window, cx);
                        app.focus_composer(window, cx);
                    })));
                }
                menu = menu.separator().item(
                    ui::MenuItem::new("All conversations\u{2026}")
                        .icon(Icon::Chat)
                        .shortcut("\u{2318}\\")
                        .on_click(cx.listener(|app, _, _, cx| app.set_workshop_history_open(true, cx))),
                );
            }
            WorkshopMenu::More => {
                let has_conversation = self.workshop_active_conversation().is_some();
                let can_copy = self.mode != Mode::Code
                    && self.active_view().is_some_and(|view| !view.rows.is_empty())
                    && !self.conversation_export_busy();
                let need_conversation = "Send a message first: this thread is not saved yet";
                let mut rename = ui::MenuItem::new("Rename conversation\u{2026}").icon(Icon::Pencil);
                rename = if has_conversation {
                    rename.on_click(cx.listener(|app, _, window, cx| {
                        app.rename_workshop_conversation(window, cx)
                    }))
                } else {
                    rename.disabled(need_conversation)
                };
                let mut copy = ui::MenuItem::new("Copy conversation")
                    .icon(Icon::Copy)
                    .shortcut("\u{2318}\u{2325}C");
                copy = if can_copy {
                    copy.on_click(cx.listener(|app, _, _, cx| app.export_conversation(cx)))
                } else {
                    copy.disabled("Nothing to copy yet, or a reply is still running")
                };
                let mut code = ui::MenuItem::new("New Code tab in this folder").icon(Icon::Terminal);
                code = if self.mode == Mode::Code {
                    code.disabled("This is already a Code tab")
                } else {
                    code.on_click(cx.listener(|app, _, _, cx| app.open_workshop_as_code_tab(cx)))
                };
                let mut archive = ui::MenuItem::new("Archive").icon(Icon::Archive);
                archive = if has_conversation {
                    archive.on_click(cx.listener(|app, _, window, cx| {
                        app.archive_workshop_conversation(window, cx)
                    }))
                } else {
                    archive.disabled(need_conversation)
                };
                let mut delete = ui::MenuItem::new("Delete\u{2026}").icon(Icon::X);
                delete = if has_conversation {
                    delete.on_click(cx.listener(|app, _, _, cx| app.delete_workshop_conversation(cx)))
                } else {
                    delete.disabled(need_conversation)
                };
                menu = menu
                    .item(rename)
                    .item(copy)
                    .item(code)
                    .item(archive)
                    .item(delete)
                    .separator()
                    .item(
                        ui::MenuItem::new("Keyboard shortcuts")
                            .shortcut("\u{2318}?")
                            .on_click(cx.listener(|app, _, window, cx| {
                                app.toggle_shortcuts(cx);
                                if app.shortcuts.open {
                                    app.focus_shortcuts_overlay(window, cx);
                                }
                            })),
                    );
            }
            WorkshopMenu::Files | WorkshopMenu::Outputs | WorkshopMenu::RecentSessions => {
                menu = self.workshop_resource_menu(menu, kind, cx);
            }
        }
        Some(menu.into_any_element())
    }

    pub(crate) fn workshop_backing(&self) -> Option<Backing> {
        match self.mode {
            Mode::Code => self
                .work
                .code_tabs
                .get(self.work.code_active)
                .filter(|s| {
                    s.history.conv_id.as_ref().is_none_or(|conv| {
                        self.conversations.get(conv).is_some_and(|c| !c.archived)
                    })
                })
                .map(|s| {
                    s.history
                        .conv_id
                        .as_ref()
                        .filter(|conv| {
                            self.shell.tabs.tabs.iter().any(
                                |tab| matches!(&tab.backing, Backing::Conversation(id) if id == *conv),
                            )
                        })
                        .map(|conv| Backing::Conversation(conv.clone()))
                        .unwrap_or(Backing::Agent(s.id))
                }),
            Mode::Work => self
                .conversations
                .get(self.sidebar.active_id())
                .filter(|c| {
                    !c.archived && matches!(c.kind, ConvKind::Chat | ConvKind::Work | ConvKind::Code)
                })
                .map(|c| Backing::Conversation(c.id.clone())),
            _ => None,
        }
    }

    /// Reconcile only live adapters and the conversation actually opened. An
    /// idle closed conversation stays in history, without reopening every frame.
    pub(crate) fn sync_workshop_tabs(&mut self) {
        // The welcome page does not own a Workshop thread. In particular its
        // default Chat adapter must not replace a restored active tab.
        if self.onboarding.is_active() {
            return;
        }
        self.shell.tabs.tabs.retain(|tab| match &tab.backing {
            Backing::Agent(id) => self.work.code_tabs.iter().any(|s| {
                s.id == *id
                    && s.history.conv_id.as_ref().is_none_or(|conv| {
                        self.conversations.get(conv).is_some_and(|c| !c.archived)
                    })
            }),
            Backing::Conversation(id) => self.conversations.get(id).is_some_and(|c| {
                !c.archived && matches!(c.kind, ConvKind::Chat | ConvKind::Work | ConvKind::Code)
            }),
        });
        if self.shell.archived_reader.is_some() {
            return;
        }
        let conversation_backings: HashSet<String> = self
            .shell
            .tabs
            .tabs
            .iter()
            .filter_map(|tab| match &tab.backing {
                Backing::Conversation(id) => Some(id.clone()),
                Backing::Agent(_) => None,
            })
            .collect();
        for id in self
            .work
            .code_tabs
            .iter()
            .filter(|s| {
                s.history.conv_id.as_ref().is_none_or(|conv| {
                    self.conversations.get(conv).is_some_and(|c| !c.archived)
                        && !conversation_backings.contains(conv)
                })
            })
            .map(|s| s.id)
            .collect::<Vec<_>>()
        {
            self.ensure_workshop_backing(Backing::Agent(id));
        }
        if let Some(backing) = self.workshop_backing() {
            let id = self.ensure_workshop_backing(backing);
            self.shell.tabs.active = Some(id);
        }
        if !self
            .shell
            .tabs
            .tabs
            .iter()
            .any(|tab| Some(&tab.id) == self.shell.tabs.active.as_ref())
        {
            self.shell.tabs.active = self.shell.tabs.tabs.first().map(|tab| tab.id.clone());
        }
    }

    fn ensure_workshop_backing(&mut self, backing: Backing) -> String {
        if let Some(tab) = self
            .shell
            .tabs
            .tabs
            .iter()
            .find(|tab| tab.backing == backing)
        {
            return tab.id.clone();
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.shell.tabs.tabs.push(Tab {
            id: id.clone(),
            backing,
        });
        id
    }

    pub(crate) fn workshop_snapshot(&self) -> SavedWorkshop {
        let agents = self.code_tabs_snapshot();
        SavedWorkshop {
            version: 1,
            tabs: self
                .shell
                .tabs
                .tabs
                .iter()
                .filter_map(|tab| {
                    let backing = match &tab.backing {
                        Backing::Conversation(id) => {
                            if self.conversations.get(id).is_none_or(|c| c.archived) {
                                return None;
                            }
                            SavedBacking::Conversation(id.clone())
                        }
                        Backing::Agent(id) => {
                            let ix = self.work.code_tabs.iter().position(|s| s.id == *id)?;
                            if self.work.code_tabs[ix]
                                .history
                                .conv_id
                                .as_ref()
                                .is_some_and(|conv| {
                                    self.conversations.get(conv).is_none_or(|c| c.archived)
                                })
                            {
                                return None;
                            }
                            SavedBacking::Agent(agents.tabs.get(ix)?.clone())
                        }
                    };
                    Some(SavedTab {
                        id: tab.id.clone(),
                        backing,
                    })
                })
                .collect(),
            active: self.shell.tabs.active.clone(),
            work_helpers: self
                .shell
                .tabs
                .tabs
                .iter()
                .filter_map(|tab| {
                    let Backing::Conversation(id) = &tab.backing else {
                        return None;
                    };
                    if !self
                        .conversations
                        .get(id)
                        .is_some_and(|conv| conv.kind == ConvKind::Work && !conv.archived)
                    {
                        return None;
                    }
                    let executor = self.work.chip_picked.get(id)?;
                    (!matches!(executor, WorkExecutor::LocalBuild { .. }))
                        .then(|| (id.clone(), executor.to_persist()))
                })
                .collect(),
        }
    }

    pub(crate) fn save_workshop_tabs_if_changed(&mut self, cx: &mut Context<Self>) {
        if self.shell.tabs.unreadable.is_some()
            || self.shell.tabs.read_failed
            || self.shell.tabs.save_error.is_some()
            || self.shell.tabs.tabs.is_empty()
        {
            return;
        }
        let Ok(raw) = serde_json::to_string(&self.workshop_snapshot()) else {
            return;
        };
        // The worker may still commit a different queued state. Compare with
        // that newest intent before the last acknowledged snapshot.
        let latest = self
            .shell
            .tabs
            .queued
            .as_ref()
            .map(|(_, raw)| raw.as_str())
            .or(self.shell.tabs.saved.as_deref());
        if latest != Some(&raw) {
            self.queue_workshop_save(raw, None, cx);
        }
    }

    fn queue_workshop_save(
        &mut self,
        raw: String,
        recovery: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let Some(persist) = &self.persist else {
            return;
        };
        let (reply, rx) = async_channel::bounded(1);
        persist.send(crate::PersistCmd::SaveWorkshopTabs {
            raw: raw.clone(),
            recovery: recovery.clone(),
            reply,
        });
        // Once another write is queued, the old acknowledgement no longer
        // proves what the database contains. A latest failure must allow Retry
        // even if the current bytes equal an earlier acknowledged snapshot.
        self.shell.tabs.saved = None;
        self.shell.tabs.save_generation += 1;
        let generation = self.shell.tabs.save_generation;
        self.shell.tabs.queued = Some((generation, raw.clone()));
        cx.spawn(async move |this, cx| {
            let result = rx
                .recv()
                .await
                .unwrap_or_else(|_| Err("Saved tabs could not be written.".into()));
            let _ = this.update(cx, |app, cx| {
                if app.shell.tabs.queued.as_ref().map(|(gen, _)| *gen) != Some(generation) {
                    return;
                }
                app.shell.tabs.queued = None;
                match result {
                    Ok(()) => {
                        app.shell.tabs.saved = Some(raw);
                        app.shell.tabs.save_error = None;
                        if let Some((expected, _)) = recovery {
                            if app.shell.tabs.unreadable.as_ref() == Some(&expected) {
                                app.shell.tabs.unreadable = None;
                            }
                        }
                    }
                    Err(error) => app.shell.tabs.save_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn recover_workshop_tabs(&mut self, cx: &mut Context<Self>) {
        if self.shell.tabs.queued.is_some() {
            return;
        }
        let Some(expected) = self.shell.tabs.unreadable.clone() else {
            return;
        };
        if serde_json::from_str::<serde_json::Value>(&expected)
            .ok()
            .and_then(|v| v.get("version").and_then(|n| n.as_u64()))
            .is_some_and(|v| v != 1)
        {
            self.show_toast(
                "These saved tabs use another Awili version. They have been preserved.",
                cx,
            );
            return;
        }
        let Ok(raw) = serde_json::to_string(&self.workshop_snapshot()) else {
            return;
        };
        self.queue_workshop_save(
            raw,
            Some((
                expected,
                format!("workshop_tabs_backup:{}", uuid::Uuid::new_v4()),
            )),
            cx,
        );
    }

    /// Returns true when canonical state was restored, so the legacy snapshot
    /// cannot add a second set of tabs. Restore never launches an agent.
    pub(crate) fn restore_workshop_tabs(
        &mut self,
        raw: Option<&str>,
        metas: &HashMap<String, String>,
    ) -> bool {
        let Some(raw) = raw else {
            return false;
        };
        let saved = match SavedWorkshop::parse(raw) {
            Ok(saved) => saved,
            Err(error) => {
                eprintln!("awili: Workshop tabs restore failed ({error}); preserving snapshot");
                self.shell.tabs.unreadable = Some(raw.to_string());
                return false;
            }
        };
        let mut fresh_agents = Vec::new();
        for (id, helper) in saved.work_helpers {
            if self
                .conversations
                .get(&id)
                .is_some_and(|conv| conv.kind == ConvKind::Work && !conv.archived)
            {
                self.work
                    .chip_picked
                    .insert(id, WorkExecutor::from_persist(&helper).unwrap());
            }
        }
        for tab in saved.tabs {
            let backing = match tab.backing {
                SavedBacking::Conversation(id) => {
                    if !self.conversations.get(&id).is_some_and(|c| {
                        !c.archived
                            && matches!(c.kind, ConvKind::Chat | ConvKind::Work | ConvKind::Code)
                    }) {
                        continue;
                    }
                    Backing::Conversation(id)
                }
                SavedBacking::Agent(agent) => {
                    let Some(WorkExecutor::Agent { engine, model }) =
                        WorkExecutor::from_persist(&agent.executor)
                    else {
                        continue;
                    };
                    if let Some(conv) = &agent.conv {
                        if !self
                            .conversations
                            .get(conv)
                            .is_some_and(|c| c.kind == ConvKind::Code && !c.archived)
                        {
                            continue;
                        }
                    }
                    // Engine and folder bind a resume session. A model choice
                    // applies from the next turn within the same thread.
                    let mut plan = reopen_plan(
                        agent
                            .conv
                            .as_ref()
                            .and_then(|id| metas.get(id))
                            .map(String::as_str),
                    );
                    if plan.engine_known
                        && agent.conv.is_some()
                        && (plan.engine != engine || plan.folder != agent.folder)
                    {
                        fresh_agents.push((engine, model, agent.folder));
                        self.shell.tabs.restore_notice = true;
                    } else {
                        // Missing metadata never supplies a borrowed session.
                        plan.engine = engine;
                        plan.model = model;
                        plan.folder = agent.folder;
                        plan.engine_known = true;
                    }
                    self.new_code_tab(plan.engine);
                    let session = self.work.code_tabs.last_mut().unwrap();
                    if let Some(conv) = agent.conv {
                        let convo = self
                            .views
                            .get(&conv)
                            .map(|v| v.convo.clone())
                            .unwrap_or_default();
                        apply_reopen(session, &conv, convo, &plan);
                    } else {
                        session.model = plan.model.clone();
                        session.route.set_next_target(WorkExecutor::Agent {
                            engine: plan.engine,
                            model: plan.model,
                        });
                        session.project_folder = plan.folder;
                    }
                    Backing::Agent(session.id)
                }
            };
            self.shell.tabs.tabs.push(Tab {
                id: tab.id,
                backing,
            });
        }
        for (engine, model, folder) in fresh_agents {
            self.new_code_tab(engine);
            let session = self.work.code_tabs.last_mut().unwrap();
            session.model = model.clone();
            session
                .route
                .set_next_target(WorkExecutor::Agent { engine, model });
            session.project_folder = folder;
            let id = session.id;
            self.ensure_workshop_backing(Backing::Agent(id));
        }
        self.shell.tabs.active = saved
            .active
            .filter(|id| self.shell.tabs.tabs.iter().any(|t| t.id == *id))
            .or_else(|| self.shell.tabs.tabs.first().map(|t| t.id.clone()));
        self.shell.tabs.saved = Some(raw.to_string());
        true
    }

    /// A failed database read is not an absent snapshot. Keep every writer
    /// blocked until a subsequent launch can read the canonical state.
    pub(crate) fn load_workshop_tabs(
        &mut self,
        state: &Result<Option<String>, String>,
        metas: &HashMap<String, String>,
    ) -> bool {
        match state {
            Ok(raw) => self.restore_workshop_tabs(raw.as_deref(), metas),
            Err(_) => {
                self.shell.tabs.read_failed = true;
                false
            }
        }
    }

    pub(crate) fn open_workshop(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.shell.tabs.active.clone() {
            self.select_workshop_tab(&id, cx);
        } else {
            self.set_mode(self.shell.workshop, cx);
        }
    }

    pub(crate) fn select_workshop_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.shell.tabs.active.as_deref() != Some(id) {
            self.cancel_composer_navigation();
        }
        self.close_workshop_archive(cx);
        let Some(tab) = self.shell.tabs.tabs.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        match tab.backing {
            Backing::Conversation(conv) => self.open_conversation_from_history(&conv, cx),
            Backing::Agent(session) => {
                let Some(ix) = self.work.code_tabs.iter().position(|s| s.id == session) else {
                    return;
                };
                self.set_mode(Mode::Code, cx);
                self.activate_code_tab(ix);
                if self.work.code_tabs[ix].history.conv_id.is_none() {
                    let live = self
                        .conversations
                        .get(self.sidebar.active_id())
                        .filter(|c| !c.archived && c.kind != ConvKind::Council)
                        .map(|c| c.id.clone())
                        .or_else(|| {
                            self.conversations
                                .active()
                                .into_iter()
                                .find(|c| c.kind != ConvKind::Council)
                                .map(|c| c.id.clone())
                        })
                        .unwrap_or_else(|| self.create_conversation(ConvKind::Work));
                    self.set_active_conversation(&live);
                }
                self.refresh_staged_attachments();
                self.save_active_conversation();
            }
        }
        self.shell.tabs.active = Some(tab.id);
        self.sync_composer_draft(cx);
        cx.notify();
    }

    pub(crate) fn workshop_tab_busy(&self, backing: &Backing) -> bool {
        match backing {
            Backing::Agent(id) => {
                self.work
                    .code_tabs
                    .iter()
                    .find(|s| s.id == *id)
                    .is_some_and(|s| s.streaming)
                    || self
                        .work
                        .plan_gates
                        .open_for(&crate::ask_mode::AskKey::CodeTab(*id))
                        .is_some()
            }
            Backing::Conversation(id) => {
                // A conversation-backed tab may retain a native session after
                // an in-app turn. Its live Stop/plan owner remains native when
                // the next turn has moved back, even though the visible tab
                // deliberately keeps its stable conversation backing.
                if self.work.code_tabs.iter().any(|session| {
                    session.history.conv_id.as_deref() == Some(id.as_str())
                        && (session.streaming
                            || self
                                .work
                                .plan_gates
                                .open_for(&crate::ask_mode::AskKey::CodeTab(session.id))
                                .is_some())
                }) {
                    return true;
                }
                // Confirmed General deletion has already removed its metadata.
                // Its pinned runner may still settle, but there is no tab owner
                // left to admit or refuse here.
                if self.conversations.get(id).is_none() {
                    return false;
                }
                if self
                    .work
                    .plan_gates
                    .open_for(&crate::ask_mode::AskKey::Work(id.clone()))
                    .is_some()
                {
                    return true;
                }
                let owner = self
                    .work
                    .conv_id
                    .as_deref()
                    .or(self.work.worker_conv.as_deref());
                let unsettled = self.work.running
                    || self.work.gateway_stop_pending
                    || self.work.agent_gate_pending
                    || self.work.panel.approval.is_some()
                    || self.work.panel.credential.is_some();
                (unsettled && owner.is_none_or(|owner| owner == id))
                    || self.work.local.down.as_ref().is_some_and(|down| {
                        down.text.is_some()
                            && down.conv_id.as_deref().is_none_or(|owner| owner == id)
                    })
                    || self
                        .work
                        .queued_after_stop
                        .as_ref()
                        .is_some_and(|q| &q.conv_id == id)
                    || self
                        .work
                        .local
                        .health
                        .pending_send
                        .as_ref()
                        .is_some_and(|p| &p.conv_id == id)
                    || self
                        .work
                        .health
                        .gateway
                        .pending_send
                        .as_ref()
                        .is_some_and(|p| &p.conv_id == id)
            }
        }
    }

    pub(crate) fn close_workshop_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(index) = self.shell.tabs.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        let tab = self.shell.tabs.tabs[index].clone();
        if self.workshop_agent_holds_draft(&tab.backing, cx) {
            self.show_toast(
                "Send or clear this thread's draft before closing its tab.",
                cx,
            );
            return;
        }
        if self.workshop_tab_busy(&tab.backing) {
            self.show_toast(
                "Stop the job or finish the waiting prompt or plan before closing this tab.",
                cx,
            );
            return;
        }
        let was_active = self.shell.tabs.active.as_deref() == Some(id);
        // Switch away before removing an adapter, so the shared draft keeps
        // the old identity. Closing a tab never deletes its saved conversation.
        let next = if was_active {
            self.shell
                .tabs
                .tabs
                .iter()
                .take(index)
                .next_back()
                .or_else(|| self.shell.tabs.tabs.get(index + 1))
                .map(|t| t.id.clone())
        } else {
            None
        };
        if self.shell.archived_reader.is_some() {
            // Closing a background return target must not navigate away from
            // the stored transcript. Back will choose a survivor or one fresh
            // fallback when the user invokes it.
            if let Backing::Agent(session) = tab.backing {
                let active = self.work.code_tabs.get(self.work.code_active).map(|s| s.id);
                self.work.code_tabs.retain(|s| s.id != session);
                self.work.code_active = active
                    .and_then(|id| self.work.code_tabs.iter().position(|s| s.id == id))
                    .unwrap_or(0);
            }
            self.shell.tabs.tabs.retain(|t| t.id != id);
            if was_active {
                self.shell.tabs.active = next;
            }
            self.sync_workshop_tabs();
            cx.notify();
            return;
        }
        if let Some(next) = &next {
            self.select_workshop_tab(next, cx);
        } else if was_active {
            self.sync_composer_draft(cx);
        }
        if let Backing::Agent(session) = tab.backing {
            let active = self.work.code_tabs.get(self.work.code_active).map(|s| s.id);
            self.work.code_tabs.retain(|s| s.id != session);
            self.work.code_active = active
                .and_then(|id| self.work.code_tabs.iter().position(|s| s.id == id))
                .unwrap_or(0);
        }
        self.shell.tabs.tabs.retain(|t| t.id != id);
        if was_active && next.is_none() {
            let kind = ConvKind::Work;
            let box_text = self.composer.read(cx).text().to_string();
            let (conv, reused) = self.fresh_conversation_with(kind, Some(&box_text));
            if reused {
                self.drop_placed_draft(&conv, cx);
            }
            self.open_conversation_from_history(&conv, cx);
            self.sync_composer_draft(cx);
        }
        self.sync_workshop_tabs();
        cx.notify();
    }

    fn workshop_tab_label(&self, tab: &Tab) -> String {
        match &tab.backing {
            Backing::Conversation(id) => self
                .conversations
                .get(id)
                .map(|c| c.title.clone())
                .unwrap_or_else(|| "Thread".into()),
            Backing::Agent(id) => self
                .work
                .code_tabs
                .iter()
                .find(|s| s.id == *id)
                .map(|s| {
                    s.history
                        .conv_id
                        .as_deref()
                        .and_then(|id| self.conversations.get(id))
                        .map(|c| c.title.clone())
                        .unwrap_or_else(|| format!("New {} thread", s.engine.label()))
                })
                .unwrap_or_else(|| "Thread".into()),
        }
    }

    #[cfg(test)]
    pub(crate) fn workshop_header_focus(&self, index: usize) -> gpui::FocusHandle {
        match index {
            0 => self.shell.tabs.focus.clone(),
            1 => self.shell.tabs.engine_focus.clone(),
            2 => self.shell.tabs.project_focus.clone(),
            3..=9 => self.shell.tabs.header_action_focus[index - 3].clone(),
            10 => self.shell.tabs.history_focus.clone(),
            11 => self.shell.tabs.close_focus[self.shell.tabs.active.as_ref().unwrap()].clone(),
            12 => self.shell.tabs.more_focus.clone(),
            _ => panic!("header focus index"),
        }
    }

    #[cfg(test)]
    pub(crate) fn workshop_header_notice_fixture(&mut self, index: usize) {
        self.shell.tabs.project_import_notice = (index == 4).then(|| "owned fixture".into());
        self.shell.tabs.restore_notice = index == 5;
        self.shell.tabs.unreadable = (index == 6).then(|| "owned unreadable fixture".into());
        self.shell.tabs.save_error = (index == 7).then(|| "owned save fixture".into());
    }

    pub(crate) fn workshop_header(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.work.running && !self.shell.tabs.clock_running {
            self.start_run_clock(cx);
        }
        let selected = self
            .shell
            .tabs
            .tabs
            .iter()
            .position(|t| Some(&t.id) == self.shell.tabs.active.as_ref())
            .unwrap_or(0);

        let title = self
            .shell
            .archived_reader
            .as_ref()
            .and_then(|reader| self.conversations.get(&reader.id))
            .map(|c| c.title.clone())
            .unwrap_or_else(|| {
                self.shell
                    .tabs
                    .tabs
                    .get(selected)
                    .map(|t| self.workshop_tab_label(t))
                    .unwrap_or_else(|| "Workshop".into())
            });

        // Reveal a newly focused control or one whose layout changed. Bounds are
        // finalized at paint, so defer the adjustment to the end of the frame.
        let scroll = ui::is_compact(window)
            .then(|| {
                if self.mode == Mode::Code {
                    self.work
                        .code_tabs
                        .get(self.work.code_active)
                        .map(|s| s.scroll.clone())
                } else {
                    self.active_view().map(|v| v.scroll.clone())
                }
            })
            .flatten();
        let history_open = self.workshop_history_open(window.viewport_size().width);
        let compact_header = ui::is_compact(window);
        let history_toggle_focus = self.shell.tabs.history_focus.clone();
        let engine_focus = self.shell.tabs.engine_focus.clone();
        let project_focus = self.shell.tabs.project_focus.clone();
        let project_bounds = self.shell.tabs.project_bounds.clone();
        let title_bounds = self.shell.tabs.title_bounds.clone();
        let history_toggle_bounds = self.shell.tabs.history_bounds.clone();
        let action_focus = self.shell.tabs.header_action_focus.clone();
        let action_bounds = self.shell.tabs.header_action_bounds.clone();
        let window_w = window.bounds().size.width;
        let level = BarLevel::for_width(
            window_w - theme::z(theme::STAGE_LEFT) - self.workshop_other_dock(window_w),
        );
        let job_island = self.job_activity_island(level, cx);
        let action_present = [
            true,
            self.shell.tabs.project_import_notice.is_some(),
            self.shell.tabs.restore_notice,
            self.shell.tabs.unreadable.is_some(),
            self.shell.tabs.save_error.is_some(),
            job_island.is_some(),
            job_island.as_ref().is_some_and(|(_, stop)| *stop),
        ];
        let island_element = job_island.map(|(element, _)| element);
        let bar = self.workshop_controls(
            level,
            compact_header,
            title,
            history_open,
            island_element,
            cx,
        );
        let app_handle = cx.entity().downgrade();
        let mode = self.mode;
        let owner = self.shell.tabs.active.clone();
        let geometry = (window.viewport_size(), theme::ui_scale());
        let last_focus = window.use_keyed_state("awili-workshop-header-focus", cx, |_, _| {
            None::<(
                Option<String>,
                Option<gpui::FocusHandle>,
                (gpui::Size<gpui::Pixels>, f32),
                Option<(gpui::Pixels, gpui::Pixels, gpui::Pixels, gpui::Pixels)>,
                Option<gpui::Size<gpui::Pixels>>,
            )>
        });
        div()
            .on_children_prepainted(move |_, window, cx| {
                let focus = if history_toggle_focus.is_focused(window) { 1 }
                    else if engine_focus.is_focused(window) { 2 }
                    else if project_focus.is_focused(window) { 3 }
                    else if let Some(index) = action_focus.iter().position(|handle| handle.is_focused(window)) { index + 4 } else { 0 };
                if focus == 0 {
                    last_focus.update(cx, |last, _| *last = None);
                    return;
                }
                let bounds = match focus {
                    1 => history_toggle_bounds.get(),
                    2 => None,
                    3 => project_bounds.get(),
                    4 => title_bounds.get(),
                    _ => action_bounds[focus - 4].get(),
                };
                let vertical_offset = scroll.as_ref().map_or(gpui::Pixels::ZERO, |scroll| scroll.offset().y);
                let normalized_bounds = bounds.map(|bounds| (
                    bounds.origin.x,
                    bounds.origin.y - vertical_offset,
                    bounds.size.width,
                    bounds.size.height,
                ));
                let transcript_viewport = scroll.as_ref().map(|scroll| scroll.bounds().size);
                let current = (
                    owner.clone(),
                    window.focused(cx),
                    geometry,
                    normalized_bounds,
                    transcript_viewport,
                );
                let changed = last_focus.update(cx, |last, _| {
                    let changed = last.as_ref() != Some(&current);
                    *last = Some(current.clone());
                    changed
                });
                if focus >= 4 && !action_present[focus - 4] {
                    let app_handle = app_handle.clone();
                    let owner = owner.clone();
                    let focused = window.focused(cx);
                    window.defer(cx, move |window, cx| {
                        let _ = app_handle.update(cx, |app, cx| {
                            if app.mode == mode && app.shell.tabs.active == owner && focused.as_ref().is_some_and(|handle| handle.is_focused(window)) {
                                app.focus_composer(window, cx);
                            }
                        });
                    });
                    return;
                }
                if !changed { return; }
                let vertical_scroll = scroll.clone();
                let last_focus = last_focus.clone();
                window.defer(cx, move |window, cx| {
                    if last_focus.read(cx).as_ref() != Some(&current) { return; }
                    let Some(bounds) = bounds else { return; };
                    let bounds = if focus >= 5 { bounds } else { bounds.inset(theme::focus_ring_overhang()) };
                    let Some(scroll) = vertical_scroll else { return; };
                    let viewport = scroll.bounds();
                    let mut offset = scroll.offset();
                    // Prepaint bounds include the ancestor transcript offset.
                    // Add only the measured delta, preserving ordinary wheel movement.
                    if bounds.top() < viewport.top() {
                        offset.y += viewport.top() - bounds.top();
                    } else if bounds.bottom() > viewport.bottom() {
                        offset.y += viewport.bottom() - bounds.bottom();
                    }
                    offset.y = offset.y.clamp(-scroll.max_offset().y, gpui::Pixels::ZERO);
                    if offset != scroll.offset() { scroll.set_offset(offset); window.refresh(); }
                });
            })
            .id("awili-workshop-header")
            .debug_selector(|| "awili-workshop-header".into())
            .flex()
            .flex_col()
            .flex_none()
            .min_w_0()
            .child(bar)
            .children(self.shell.tabs.project_import_notice.as_ref().map(|_| {
                ui::Banner::new("awili-workshop-project-import-notice", Tone::Attention,
                    "Some older saved projects could not be imported. Existing projects and the original list have been preserved.")
                    .action(ui::Button::new("awili-workshop-dismiss-project-import", "Dismiss")
                        .focus_handle(&self.shell.tabs.header_action_focus[1])
                        .bounds_handle(self.shell.tabs.header_action_bounds[1].clone()).quiet()
                        .on_click(cx.listener(|app, _, _, cx| app.dismiss_workshop_project_import(cx))))
                    .into_any_element()
            }))
            .children(self.shell.tabs.restore_notice.then(|| ui::Banner::new("awili-workshop-fresh-session", Tone::Attention,
                "A changed helper or folder opened in a new thread. The original thread keeps its saved session.")
                .action(ui::Button::new("awili-workshop-dismiss-restore", "Got it")
                        .focus_handle(&self.shell.tabs.header_action_focus[2])
                        .bounds_handle(self.shell.tabs.header_action_bounds[2].clone()).quiet()
                    .on_click(cx.listener(|app, _, _, cx| { app.shell.tabs.restore_notice = false; cx.notify(); }))).into_any_element()))
            .children(self.shell.tabs.unreadable.as_ref().map(|_| {
                ui::Banner::new(
                    "awili-workshop-restore-notice",
                    Tone::Attention,
                    "Saved tabs could not be opened. Your conversations are available in History.",
                )
                .action(
                    ui::Button::new("awili-workshop-recover", "Save current tabs")
                        .focus_handle(&self.shell.tabs.header_action_focus[3])
                        .bounds_handle(self.shell.tabs.header_action_bounds[3].clone())
                        .quiet()
                        .on_click(cx.listener(|app, _, _, cx| app.recover_workshop_tabs(cx))),
                )
                .into_any_element()
            }))
            .children(self.shell.tabs.read_failed.then(|| {
                ui::Banner::new("awili-workshop-read-notice", Tone::ActNow,
                    "Saved tabs could not be read. Reopen Awili to try again; your saved tabs have been preserved.")
                    .into_any_element()
            }))
            .children(self.shell.tabs.save_error.as_ref().map(|_| {
                ui::Banner::new(
                    "awili-workshop-save-notice",
                    Tone::ActNow,
                    "Your open tabs could not be saved.",
                )
                .action(
                    ui::Button::new("awili-workshop-retry-save", "Retry")
                        .focus_handle(&self.shell.tabs.header_action_focus[4])
                        .bounds_handle(self.shell.tabs.header_action_bounds[4].clone())
                        .quiet()
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.shell.tabs.save_error = None;
                            if app.shell.tabs.unreadable.is_some() {
                                app.recover_workshop_tabs(cx);
                            } else {
                                app.save_workshop_tabs_if_changed(cx);
                            }
                            cx.notify();
                        })),
                )
                .into_any_element()
            }))

            .into_any_element()
    }
    /// Compact headers share the owned transcript scroll; the editor stays outside.
    /// The Conversations panel docks beside the frame when it fits; otherwise it
    /// opens as a sheet from the main render (`workshop_history_sheet`).
    pub(crate) fn workshop_frame(
        &mut self,
        body: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.restore_history_pref(cx);
        let docked = self.workshop_history_docked(window.viewport_size().width);
        self.reconcile_history_presentation(docked);
        let header = (!ui::is_compact(window)).then(|| self.workshop_header(window, cx));
        let panel = (docked && !self.shell.tabs.history_collapsed)
            .then(|| self.workshop_history_panel(false, cx));
        if let Some(index) = self.shell.tabs.pending_open_scroll.take() {
            // The list's rows are measured at paint: ask again after it.
            let scroll = self.shell.tabs.scroll.clone();
            window.defer(cx, move |window, _| {
                scroll.scroll_to_item(index);
                window.refresh();
            });
        }
        div()
            .id("awili-workshop")
            .debug_selector(|| "awili-workshop".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            // The bar spans the whole stage; the docked panel sits under it.
            .children(header)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(body),
                    )
                    .children(panel),
            )
            .into_any_element()
    }
}

/// The right-hand Conversations panel. It is a view of the existing open-thread
/// model and conversation catalog: selecting, closing and creating threads call
/// the same owners as before, and nothing here touches a run or a route.
impl AwiliApp {
    /// Width the composer must leave to right-hand docks: the Job or Inspector
    /// face, or the Needs-you list, then the Conversations panel.
    pub(crate) fn workshop_right_dock(&self, width: Pixels) -> Pixels {
        let other = self.workshop_other_dock(width);
        if self.workshop_history_docked(width) && !self.shell.tabs.history_collapsed {
            other + theme::z(HISTORY_W + theme::EDGE)
        } else {
            other
        }
    }

    pub(crate) fn workshop_other_dock(&self, width: Pixels) -> Pixels {
        if ((self.mode == Mode::Work && self.shell.job_open)
            || self.shell.inspector.selection.is_some())
            && !crate::job_panel::uses_sheet_at(width)
        {
            theme::z(theme::PANEL_W + theme::EDGE)
        } else if self.mode == Mode::Work && self.feed_panel_open && !self.shell.job_open {
            theme::z(theme::NEEDS_YOU_W)
        } else {
            Pixels::ZERO
        }
    }

    /// Whether the window can hold the panel beside a readable composer and any
    /// Job/Inspector dock. Job and Inspector keep priority; the panel yields to
    /// its header toggle (and a sheet) rather than squeeze the composer controls.
    pub(crate) fn workshop_history_docked(&self, width: Pixels) -> bool {
        crate::consolidation::workshop_mode(self.mode)
            && !self.onboarding.is_active()
            && self.shell.archived_reader.is_none()
            && width >= theme::z(theme::COMPACT_W)
            && width
                - theme::z(theme::STAGE_LEFT)
                - self.workshop_other_dock(width)
                - theme::z(HISTORY_W + theme::EDGE)
                >= theme::job_composer_min_width()
    }

    /// Whether the panel's filter lets this row show. The filter never hides the
    /// open conversation or a row being renamed or confirmed for delete: the
    /// more menu and ⌘N act on those.
    pub(crate) fn history_row_shown(&self, id: &str, title: &str) -> bool {
        let query = self.shell.tabs.history_query.trim().to_lowercase();
        if !crate::consolidation::workshop_mode(self.mode) || query.is_empty() {
            return true;
        }
        id == self.sidebar.active_id()
            || self.workshop_active_conversation().as_deref() == Some(id)
            || self.sidebar.renaming.as_deref() == Some(id)
            || self.pending_delete.as_deref() == Some(id)
            || title.to_lowercase().contains(&query)
    }

    /// Whether the panel is showing now, docked or as a sheet.
    pub(crate) fn workshop_history_open(&self, width: Pixels) -> bool {
        if self.workshop_history_docked(width) {
            !self.shell.tabs.history_collapsed
        } else {
            self.shell.conversations
        }
    }

    /// Show or hide the panel. Only presentation changes: the current thread,
    /// open tabs, drafts and any active run are untouched.
    /// The one collapse intent, saved: docked and sheet forms both follow it.
    fn set_history_collapsed(&mut self, collapsed: bool) {
        self.shell.tabs.history_touched = true;
        if collapsed {
            // A hidden panel forgets its filter, so it never reopens showing a
            // partial list.
            self.shell.tabs.history_query.clear();
        }
        if self.shell.tabs.history_collapsed == collapsed {
            return;
        }
        self.shell.tabs.history_collapsed = collapsed;
        if let Some(persist) = &self.persist {
            persist.set_state(
                HISTORY_PANEL_KEY,
                if collapsed { "collapsed" } else { "open" }.to_string(),
            );
        }
    }

    /// Show Conversations (docked, or as a sheet where it cannot dock) or hide
    /// it, whichever form is on screen. An explicit Show or Hide is the owner's
    /// intent; a sheet that closes because a thread was picked is not, and
    /// puts a collapsed panel back (see `reconcile_history_presentation`).
    pub(crate) fn set_workshop_history_open(&mut self, open: bool, cx: &mut Context<Self>) {
        let docked = self.workshop_history_docked(px(self.window_w));
        if open {
            if !docked && !self.shell.conversations {
                self.shell.tabs.sheet_from_collapsed = self.shell.tabs.history_collapsed;
            }
            self.shell.conversations = !docked;
            self.set_history_collapsed(false);
        } else {
            self.shell.tabs.sheet_from_collapsed = false;
            self.shell.conversations = false;
            self.set_history_collapsed(true);
        }
        self.sidebar.collapsed = false;
        cx.notify();
    }

    /// Frame-time bookkeeping between the two forms. Widening past the dock
    /// threshold with a sheet open keeps the panel open; a sheet that was
    /// opened from a collapsed panel and then closed (a thread was picked, or
    /// Escape) leaves it collapsed, so it does not reappear after a resize.
    fn reconcile_history_presentation(&mut self, docked: bool) {
        if docked {
            if self.shell.conversations {
                self.shell.conversations = false;
                self.set_history_collapsed(false);
            }
            self.shell.tabs.history_sheet_shown = false;
            self.shell.tabs.sheet_from_collapsed = false;
            return;
        }
        let shown = self.shell.conversations;
        if self.shell.tabs.history_sheet_shown && !shown && self.shell.tabs.sheet_from_collapsed {
            self.set_history_collapsed(true);
        }
        if !shown {
            self.shell.tabs.sheet_from_collapsed = false;
        }
        self.shell.tabs.history_sheet_shown = shown;
    }

    pub(crate) fn toggle_workshop_history(&mut self, cx: &mut Context<Self>) {
        let open = self.workshop_history_open(px(self.window_w));
        self.set_workshop_history_open(!open, cx);
    }

    /// Read the saved collapse choice once, in persistence order. A choice the
    /// owner made before the read returns wins.
    fn restore_history_pref(&mut self, cx: &mut Context<Self>) {
        if self.shell.tabs.history_restore_started {
            return;
        }
        self.shell.tabs.history_restore_started = true;
        let Some(persist) = &self.persist else {
            return;
        };
        let (reply, rx) = async_channel::bounded(1);
        persist.send(crate::PersistCmd::ReadState {
            key: HISTORY_PANEL_KEY.to_string(),
            reply,
        });
        cx.spawn(async move |this, cx| {
            let value = rx.recv().await.ok().flatten();
            let _ = this.update(cx, |app, cx| {
                if !app.shell.tabs.history_touched {
                    if let Some(value) = value {
                        app.shell.tabs.history_collapsed = value == "collapsed";
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// The sheet form, mounted at the window origin by the main render when the
    /// panel is requested but cannot dock.
    pub(crate) fn workshop_history_sheet(
        &mut self,
        width: Pixels,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        (crate::consolidation::workshop_mode(self.mode)
            && !self.onboarding.is_active()
            && self.shell.archived_reader.is_none()
            && self.shell.conversations
            && !self.workshop_history_docked(width))
        .then(|| self.workshop_history_panel(true, cx))
    }

    /// Each open Workshop tab as a kit row: busy dot, selection, and its Close.
    fn workshop_open_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.shell
            .tabs
            .close_focus
            .retain(|id, _| self.shell.tabs.tabs.iter().any(|tab| &tab.id == id));
        let active = self.shell.tabs.active.clone();
        let key = (active.clone(), self.shell.tabs.tabs.len());
        if self.shell.tabs.open_scroll_key.as_ref() != Some(&key) {
            if let Some(index) = self
                .shell
                .tabs
                .tabs
                .iter()
                .position(|tab| Some(&tab.id) == active.as_ref())
            {
                // The old strip kept the selected tab in view; so does this list.
                self.shell.tabs.scroll.scroll_to_item(index);
                self.shell.tabs.pending_open_scroll = Some(index);
            }
            self.shell.tabs.open_scroll_key = Some(key);
        }
        let mut rows = Vec::new();
        for tab in self.shell.tabs.tabs.clone() {
            let label = self.workshop_tab_label(&tab);
            let conv = match &tab.backing {
                Backing::Conversation(id) => Some(id.clone()),
                Backing::Agent(agent) => self
                    .work
                    .code_tabs
                    .iter()
                    .find(|s| s.id == *agent)
                    .and_then(|s| s.history.conv_id.clone()),
            };
            let shown = match conv {
                Some(id) => self.history_row_shown(&id, &label),
                None => self.history_row_shown("", &label) || active.as_deref() == Some(tab.id.as_str()),
            };
            if !shown {
                continue;
            }
            let busy = self.workshop_tab_busy(&tab.backing);
            let selected = active.as_deref() == Some(tab.id.as_str());
            let close_focus = self
                .shell
                .tabs
                .close_focus
                .entry(tab.id.clone())
                .or_insert_with(|| cx.focus_handle())
                .clone();
            let handle = cx.entity().downgrade();
            let close_id = tab.id.clone();
            let close = ui::IconButton::new(
                SharedString::from(format!("workshop-{}-close", tab.id)),
                Icon::X,
            )
            .quiet()
            .label(format!("Close {label}"))
            .focus_handle(&close_focus)
            .on_click(move |event, window, cx| {
                cx.stop_propagation();
                let keyboard = matches!(event, gpui::ClickEvent::Keyboard(_));
                let _ = handle.update(cx, |app, cx| {
                    let was_open = app.shell.tabs.tabs.iter().any(|tab| tab.id == close_id);
                    app.close_workshop_tab(&close_id, cx);
                    let closed =
                        was_open && !app.shell.tabs.tabs.iter().any(|tab| tab.id == close_id);
                    if closed {
                        if keyboard {
                            window.focus(&app.shell.tabs.focus, cx);
                        } else {
                            app.focus_composer(window, cx);
                        }
                    }
                });
            });
            let select_id = tab.id.clone();
            let mut row = ui::Row::new(SharedString::from(format!("workshop-{}", tab.id)))
                .title(label.clone())
                .selected(selected)
                .trailing(close)
                .on_click(
                    cx.listener(move |app, event: &gpui::ClickEvent, window, cx| {
                        let keyboard = matches!(event, gpui::ClickEvent::Keyboard(_));
                        let was_sheet = app.shell.conversations;
                        app.select_workshop_tab(&select_id, cx);
                        // A sheet covers the thread it just selected.
                        app.shell.conversations = false;
                        if !keyboard || was_sheet {
                            app.focus_composer(window, cx);
                        } else {
                            // The picked row now carries the selected row's focus
                            // handle; keep the keyboard on it.
                            window.focus(&app.shell.tabs.focus, cx);
                        }
                    }),
                );
            if busy {
                row = row.leading(
                    ui::Dot::new(Tone::Neutral)
                        .id(SharedString::from(format!("workshop-{}-dot", tab.id)))
                        .label(format!("{label} is running")),
                );
            }
            if selected {
                row = row.focus_handle(&self.shell.tabs.focus);
            }
            rows.push(row.into_any_element());
        }
        rows
    }

    pub(crate) fn workshop_history_panel(
        &mut self,
        sheet: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The catalog below has its own icon-only collapse; the panel's
        // collapse is the one owners see.
        self.sidebar.collapsed = false;
        let rows = self.workshop_open_rows(cx);
        let catalog = self.sidebar_panel(cx);
        let open = div()
            .id("awili-workshop-open")
            .debug_selector(|| "awili-workshop-open".into())
            .flex()
            .flex_col()
            .flex_none()
            .gap(theme::z(theme::SP_4))
            .max_h(theme::z(HISTORY_OPEN_MAX_H))
            .overflow_y_scroll()
            .track_scroll(&self.shell.tabs.scroll)
            .children(rows);
        let body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .gap(theme::z(theme::SP_8))
            .child(ui::text::caption("Open threads"))
            .child(open)
            .child(div().flex_1().min_h_0().w_full().child(catalog));
        let panel = ui::Panel::new("awili-conversations-panel")
            .width(HISTORY_W)
            .sheet_when(sheet)
            .focus_handle(&self.shell.conversations_focus)
            .header("Conversations", None)
            .full_height()
            .body_fill(body);
        // The docked panel has no close of its own (the bar's toggle and ⌘\ are
        // it); the overlay keeps one, since it covers the toggle.
        let panel = if sheet {
            panel.on_close(cx.listener(move |app, _, window, cx| {
                app.set_workshop_history_open(false, cx);
                app.focus_shell_page(window, cx);
            }))
        } else {
            panel
        };
        if sheet {
            div()
                .absolute()
                .top(Pixels::ZERO)
                .left(Pixels::ZERO)
                .size_full()
                .child(panel)
                .into_any_element()
        } else {
            div()
                .debug_selector(|| "awili-history-dock".into())
                .flex_none()
                .flex()
                .flex_col()
                .min_h_0()
                .w(theme::z(HISTORY_W))
                .my(theme::z(theme::EDGE))
                .mr(theme::z(theme::EDGE))
                .child(panel)
                .into_any_element()
        }
    }
}

impl AwiliApp {
    /// Folder/project actions share the existing persisted Workshop project list.
    /// Choosing a different folder keeps the established runner-identity guards.
    fn workshop_project_actions(&self, mut menu: ui::Menu, cx: &mut Context<Self>) -> ui::Menu {
        let Some(folder) = self.workshop_folder() else {
            return menu;
        };
        let project = self.work.projects.iter().find(|p| p.folder == folder);
        menu = menu.separator();
        if let Some(project) = project {
            menu = Self::saved_project_actions(menu, project, cx);
        } else {
            let save_folder = std::path::PathBuf::from(&folder);
            menu = menu.item(ui::MenuItem::new("Save folder as project").on_click(
                cx.listener(move |app, _, _, cx| app.save_project_folder(save_folder.clone(), cx)),
            ));
        }
        menu.item(
            ui::MenuItem::new(format!(
                "Open folder in {}",
                crate::agent_bin::file_manager_name()
            ))
                .icon(Icon::Folder)
                .on_click(move |_, _, _| {
                    let _ = crate::agent_bin::open_path(std::path::Path::new(&folder));
                }),
        )
    }

    pub(crate) fn show_project_menu(
        &mut self,
        kind: WorkshopMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shell.tabs.menu = Some(kind);
        self.shell.tabs.menu_selected = 0;
        self.ptt_key_down(cx);
        window.focus(&self.shell.tabs.menu_focus, cx);
        cx.notify();
    }

    fn saved_project_actions(
        mut menu: ui::Menu,
        project: &awili_persist::StoredProject,
        cx: &mut Context<Self>,
    ) -> ui::Menu {
        let id = project.id;
        let edit_folder = project.folder.clone();
        let instructions = project.instructions.clone();
        menu = menu.item(
            ui::MenuItem::new(format!("Edit {}…", project.name)).on_click(cx.listener(
                move |app, _, window, cx| {
                    app.open_project_editor(
                        id,
                        edit_folder.clone(),
                        instructions.clone(),
                        window,
                        cx,
                    )
                },
            )),
        );
        let delete_folder = project.folder.clone();
        let name = project.name.clone();
        menu = menu.item(
            ui::MenuItem::new(format!("Remove {}…", project.name)).on_click(cx.listener(
                move |app, _, window, cx| {
                    app.shell.tabs.delete_project = Some((id, delete_folder.clone(), name.clone()));
                    app.shell.job_open = false;
                    app.shell.more = false;
                    app.shell.conversations = false;
                    window.focus(&app.shell.tabs.delete_cancel_focus, cx);
                    cx.notify();
                },
            )),
        );

        menu
    }

    pub(crate) fn workshop_project_delete_dialog(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (id, folder, name) = self.shell.tabs.delete_project.clone()?;
        let handle = cx.entity().downgrade();
        Some(ui::Dialog::new(
            "awili-remove-saved-project",
            "Remove saved project?",
            ui::text::read(format!("Remove {name} from the saved project list? Its folder and files stay on disk. You can save it as a project again.")).wrap(),
        )
        .destructive()
        .trigger_focus(&self.shell.tabs.project_focus)
        .focus_handles(Some(&self.shell.tabs.delete_cancel_focus), None, Some(&self.shell.tabs.delete_confirm_focus))
        .on_close(move |_, cx| {
            let _ = handle.update(cx, |app, cx| {
                app.shell.tabs.delete_project = None;
                cx.notify();
            });
        })
        .cancel("Keep project", |_, _, _| {})
        .primary("Remove saved project", cx.listener(move |app, _, _, cx| {
            app.remove_project_by_id(id, folder.clone(), cx);
        }))
        .into_any_element())
    }
}
