//! The real Workshop composer, built from the existing shared kit.
//!
//! Every Workshop turn takes the one tool-capable path of the thread's owner
//! (a Work helper, or a native Code tab). There is no answer-or-job choice: a
//! plain question simply uses no tools.
use super::*;
use gpui::AnyElement;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ModelOwner {
    Native(u64),
    Work(String),
}

pub(crate) struct ComposerMenus {
    pub(crate) model: Option<ModelOwner>,
    placement: ui::PopoverState,
    selected: usize,
    bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    focus: gpui::FocusHandle,
}

impl ComposerMenus {
    pub(crate) fn new(cx: &mut Context<AwiliApp>) -> Self {
        Self {
            model: None,
            placement: ui::PopoverState::new(),
            selected: 0,
            bounds: Default::default(),
            focus: cx.focus_handle(),
        }
    }
}

/// The vendor model family named in `raw`, when it is a known one.
pub(crate) fn known_model_family(raw: &str) -> Option<&'static str> {
    let name = raw.to_ascii_lowercase();
    [
        ("sonnet", "Sonnet"),
        ("opus", "Opus"),
        ("haiku", "Haiku"),
        ("claude", "Claude"),
        ("codex", "Codex"),
        ("gpt", "GPT"),
        ("gemma", "Gemma"),
        ("qwen", "Qwen"),
        ("deepseek", "DeepSeek"),
        ("glm", "GLM"),
    ]
    .into_iter()
    .find(|(needle, _)| name.contains(needle))
    .map(|(_, label)| label)
}

pub(crate) fn model_family(raw: &str) -> String {
    known_model_family(raw)
        .unwrap_or("Configured model")
        .to_string()
}

impl AwiliApp {
    pub(crate) fn cancel_composer_navigation(&mut self) {
        self.shell.composer_menus.placement.reset();
        self.shell.composer_menus.model = None;
    }

    /// A project folder is a binding for a Workshop thread unless it is only
    /// the HOME fallback. In Code, a tab-owned non-HOME folder is a binding;
    /// legacy HOME bindings count only when HOME is an authoritative saved
    /// project, because `code_cwd()` can fall back there.
    pub(crate) fn code_project_folder_is_bound(&self, folder: &str) -> bool {
        let folder = std::path::Path::new(folder);
        if folder.as_os_str().is_empty() {
            return false;
        }
        let is_home =
            crate::home::home_os().is_some_and(|home| folder == std::path::Path::new(&home));
        !is_home
            || self
                .work
                .projects
                .iter()
                .any(|project| std::path::Path::new(&project.folder) == folder)
    }

    pub(crate) fn composer_model_owner(&self) -> Option<ModelOwner> {
        match self.mode {
            Mode::Code => self
                .work
                .code_tabs
                .get(self.work.code_active)
                .map(|tab| ModelOwner::Native(tab.id)),
            Mode::Work => Some(ModelOwner::Work(self.sidebar.active_id().to_string())),
            _ => None,
        }
    }

    pub(crate) fn close_composer_model_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shell.composer_menus.model.take().is_some() {
            self.shell.composer_menus.placement.reset();
            window.focus(&self.shell.tabs.engine_focus, cx);
            cx.notify();
        }
    }

    /// Open the current conversation's chooser from the bar's model chip: one
    /// menu, one owner, one next-turn target.
    pub(crate) fn open_composer_model_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(owner) = self.composer_model_owner() else {
            return;
        };
        self.shell.composer_menus.placement.reset();
        self.shell.composer_menus.model = Some(owner);
        self.shell.composer_menus.selected = 0;
        self.ptt_key_down(cx);
        window.focus(&self.shell.composer_menus.focus, cx);
        cx.notify();
    }

    /// The target of a turn running now for `owner`, from the run's own fields
    /// (never the saved next-turn target).
    pub(crate) fn composer_active_target(&self, owner: &ModelOwner) -> Option<work::WorkExecutor> {
        match owner {
            ModelOwner::Native(id) => self
                .work
                .code_tabs
                .iter()
                .find(|tab| tab.id == *id && tab.streaming)
                .map(|tab| work::WorkExecutor::Agent {
                    engine: tab.engine,
                    model: tab.model.clone(),
                }),
            ModelOwner::Work(conv) => (self.work.running
                && self.work.conv_id.as_deref() == Some(conv.as_str()))
            .then(|| {
                self.work
                    .panel
                    .requested_identity
                    .as_ref()
                    .and_then(crate::executor_labels::executor_for)
            })
            .flatten(),
        }
    }

    /// Choosing a target changes only this conversation's next turn. It is
    /// allowed while a turn runs, sends nothing, and leaves the running turn,
    /// its approval and its Stop with the target it started under.
    fn select_composer_target(
        &mut self,
        owner: &ModelOwner,
        target: work::WorkExecutor,
        cx: &mut Context<Self>,
    ) {
        if self.composer_model_owner().as_ref() != Some(owner) {
            return;
        }
        if !self.set_composer_target(owner, target, cx) {
            self.show_toast("That model couldn't be selected for this conversation.", cx);
        }
        cx.notify();
    }

    pub(crate) fn composer_model_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let owner = self.shell.composer_menus.model.clone()?;
        if self.composer_model_owner().as_ref() != Some(&owner) {
            return None;
        }
        let current = self.composer_target_for(&owner);
        let sections =
            crate::executor_labels::target_sections(&crate::executor_labels::TargetReach {
                engine: &self.work.engine,
                extras: &self.work.extra_engines,
                models: &self.work.models_cfg,
                agents: &self.work.agents,
                build_targets: &self.work.build_targets,
                gateway_set_up: self.work.hermes.enabled || self.work.gateway.enabled,
            });
        let handle = cx.entity().downgrade();
        let move_handle = handle.clone();
        let mut menu = ui::Menu::new("composer-model-menu")
            .anchor(self.shell.tabs.engine_bounds.get()?)
            .popover_state(self.shell.composer_menus.placement.clone())
            .focus_handle(&self.shell.composer_menus.focus)
            .trigger_focus(&self.shell.tabs.engine_focus)
            .selected(self.shell.composer_menus.selected)
            .on_close(move |window, cx| {
                let _ = handle.update(cx, |app, cx| app.close_composer_model_menu(window, cx));
            })
            .on_move(move |selected, _, cx| {
                let _ = move_handle.update(cx, |app, cx| {
                    app.shell.composer_menus.selected = selected;
                    cx.notify();
                });
            });
        for (index, section) in sections.into_iter().enumerate() {
            if index > 0 {
                menu = menu.separator();
            }
            for row in section.rows {
                let mut item = ui::MenuItem::new(row.label.clone());
                if current.as_ref() == Some(&row.target) {
                    item = item.icon(icons::Icon::Check);
                }
                if let Some(reason) = row.unavailable {
                    item = item.disabled(reason);
                } else {
                    let owner = owner.clone();
                    let target = row.target;
                    item = item.on_click(cx.listener(move |app, _, _, cx| {
                        app.select_composer_target(&owner, target.clone(), cx)
                    }));
                }
                menu = menu.item(item);
            }
        }
        Some(menu.into_any_element())
    }

    /// A blank native helper has a runtime tab, but no stored conversation.
    /// Bind storage on its first file/question rather than borrowing the
    /// sidebar's last thread. Do not enqueue a second Create command.
    pub(crate) fn workshop_input_owner(
        &mut self,
        bind_project_folder: bool,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        if self.shell.archived_reader.is_some() {
            return None;
        }
        if self.mode != Mode::Code {
            let id = self.sidebar.active_id().to_string();
            return self
                .conversations
                .get(&id)
                .is_some_and(|meta| !meta.archived)
                .then_some(id);
        }
        self.ensure_code_tab(cx);
        let tab = self.work.code_tabs.get(self.work.code_active)?;
        let route_seed = tab.route.clone();
        if let Some(id) = &tab.history.conv_id {
            let id = id.clone();
            let is_live = self
                .conversations
                .get(&id)
                .is_some_and(|meta| !meta.archived);
            if is_live {
                if !self.work.unavailable_conversation_routes.contains_key(&id)
                    && !self.work.conversation_routes.contains_key(&id)
                {
                    self.work.conversation_routes.insert(id.clone(), route_seed);
                    self.persist_conversation_route(&id);
                }
                return Some(id);
            }
            return None;
        }
        let folder = tab
            .project_folder
            .clone()
            .or_else(|| bind_project_folder.then(|| self.code_cwd().display().to_string()));
        let binding = crate::code_history::CodeConvMeta::for_turn(
            tab.engine,
            tab.model.clone(),
            folder,
            tab.session_slot.lock().ok().and_then(|slot| slot.clone()),
        );
        let Some(store) = self.search_store.as_ref() else {
            self.show_toast("Input could not be saved — persistence unavailable", cx);
            return None;
        };
        let now = feed::unix_now();
        let meta = self.conversations.create_kind(now, ConvKind::Code);
        let key = crate::code_history::code_conv_key(&meta.id);
        if let Err(error) = store.create_conversation_with_binding(
            &meta.id,
            &meta.title,
            now,
            "code",
            binding.folder.as_deref(),
            (&key, &binding.to_json()),
        ) {
            self.conversations.remove(&meta.id);
            self.show_toast(format!("Input could not be saved — {error}"), cx);
            return None;
        }
        self.conversations.get_mut(&meta.id)?.cwd = binding.folder.clone();
        let tab = self.work.code_tabs.get_mut(self.work.code_active)?;
        tab.project_folder = binding.folder;
        tab.history.conv_id = Some(meta.id.clone());
        let route = tab.route.clone();
        self.views
            .entry(meta.id.clone())
            .or_insert_with(ConvView::new);
        // The conversation's canonical route starts as the tab's own choice, so
        // the first native turn has an owner (a missing route made it return
        // before anything was sent or stored).
        self.work
            .conversation_routes
            .entry(meta.id.clone())
            .or_insert(route);
        self.persist_conversation_route(&meta.id);
        self.sync_code_selection();
        self.sync_workshop_tabs();
        Some(meta.id)
    }

    /// The model picker chip for the composer footer's left end: the current
    /// conversation's next-turn target (label, tooltip) and the existing chooser
    /// (`open_composer_model_menu`). A quiet ghost chip: no border, no fill.
    pub(crate) fn composer_model_chip(&self, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let target_chip = self.composer_model_owner().map(|owner| {
            crate::executor_labels::target_chip(
                self.composer_target_for(&owner).as_ref(),
                self.composer_active_target(&owner).as_ref(),
                &self.work.engine,
                &self.work.extra_engines,
                &self.work.build_targets,
            )
        });
        let helper = target_chip
            .as_ref()
            .map(|chip| chip.label.clone())
            .unwrap_or_else(|| "Choose model".into());
        let helper_tip = target_chip
            .map(|chip| chip.tooltip)
            .unwrap_or_else(|| "Open a conversation to choose its model".into());
        // "Claude · haiku" folds to "haiku" when narrow.
        let helper_short = helper
            .rsplit(" \u{b7} ")
            .next()
            .unwrap_or(helper.as_str())
            .to_string();
        let engine_bounds = self.shell.tabs.engine_bounds.clone();
        let model_label = if narrow { helper_short } else { helper.clone() };
        div()
            .flex_none()
            .on_children_prepainted(move |children, _, _| {
                if let Some(b) = children.first() {
                    engine_bounds.set(Some(*b));
                }
            })
            .child(
                ui::Chip::new(
                    "awili-workshop-helper",
                    format!("{} \u{25be}", ui::chip::middle_truncate(&model_label)),
                )
                .compact()
                .label(format!(
                    "{helper} \u{2014} choose the model or helper for your next message"
                ))
                .tooltip(helper_tip)
                .focus_handle(&self.shell.tabs.engine_focus)
                .disabled_if(
                    self.composer_model_owner()
                        .is_none()
                        .then(|| "Open a conversation to choose its model".into()),
                )
                .on_click(cx.listener(|app, _, window, cx| {
                    app.open_composer_model_menu(window, cx);
                })),
            )
            .into_any_element()
    }

    /// The footer under the field, as (left, right) pieces: in a Workshop
    /// thread the status strip (state at the left, the context budget at the
    /// right); other pages keep their status chip at the right. The send and
    /// new-line hints are gone: the model, the mode and the run state live in
    /// the top bar.
    pub(crate) fn composer_footer_parts(
        &self,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, Vec<AnyElement>) {
        if !consolidation::workshop_mode(self.mode) {
            return (Vec::new(), vec![self.composer_status_chip(cx).into_any_element()]);
        }
        self.strip_elements(cx)
    }

    /// (used, window, window reported) from the newest reply that recorded
    /// how its prompt was made (the S2 figure); none before there is one, or
    /// when the figure is not how full a single window was.
    pub(crate) fn composer_context_parts(&self) -> Option<(u32, u32, bool)> {
        let rows = match self.mode {
            Mode::Code => &self.work.code_tabs.get(self.work.code_active)?.rows,
            _ => &self.active_view()?.rows,
        };
        let breakdown = rows.iter().rev().find_map(|row| row.context.as_ref())?;
        let limit = breakdown.limit?;
        let used = breakdown.total_tokens();
        if breakdown.summed_steps || used > limit {
            return None;
        }
        Some((used, limit, breakdown.limit_reported))
    }

    /// The top bar's mode chip (Plan first / Auto), from the next turn's
    /// target. `narrow` folds its label to "Plan" / "Auto".
    pub(crate) fn header_mode_chip(&self, narrow: bool, cx: &mut Context<Self>) -> Option<ui::Chip> {
        let owner = self.composer_model_owner()?;
        let next = self.composer_target_for(&owner);
        self.composer_target_ask_chip(&owner, next.as_ref(), narrow, cx)
    }

    /// Autonomy follows the next turn's target. The posture stays the owner's
    /// choice; what "Ask before changes" means follows the target (a native
    /// harness is plan-first, not a prompt per tool call).
    fn composer_target_ask_chip(
        &self,
        owner: &ModelOwner,
        next: Option<&work::WorkExecutor>,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> Option<ui::Chip> {
        let Some(next) = next else {
            return self.composer_ask_mode_chip(narrow, cx);
        };
        let key = match owner {
            ModelOwner::Native(id) => crate::ask_mode::AskKey::CodeTab(*id),
            ModelOwner::Work(conv) => crate::ask_mode::AskKey::Work(conv.clone()),
        };
        let support = crate::ask_mode::work_support(next, self.work_engine_kind());
        Some(self.ask_mode_chip(key, support, narrow, cx))
    }

    pub(crate) fn composer_banners(&self) -> Vec<AnyElement> {
        let mut banners = Vec::new();
        if self.mode == Mode::Work {
            let chip = self.memory_chips.get(self.sidebar.active_id());
            let rules = chip.and_then(|chip| chip.rules.as_ref());
            let recall = chip.and_then(|chip| chip.recall.as_ref());
            if self.memory.enabled
                && (matches!(rules, Some(RulesStatus::Rejected))
                    || (matches!(rules, Some(RulesStatus::Offline))
                        && self.rules_offline_streak >= 2)
                    || (matches!(recall, Some(RecallStatus::Offline))
                        && self.recall_offline_streak >= 2))
            {
                banners.push(
                    ui::Banner::new(
                        "composer-memory",
                        theme::Tone::Attention,
                        "Memory is unavailable for this thread. Details show what was loaded.",
                    )
                    .into_any_element(),
                );
            }
        }
        if let Some(progress) = self.voice.model.lock().unwrap().progress() {
            banners.push(
                ui::Banner::new(
                    "composer-voice-download",
                    theme::Tone::Neutral,
                    format!("Downloading voice model — {}%", (progress * 100.0) as u32),
                )
                .into_any_element(),
            );
        } else if let Some(status) = &self.voice.status {
            banners.push(
                ui::Banner::new(
                    "composer-voice-status",
                    theme::Tone::Neutral,
                    status.clone(),
                )
                .into_any_element(),
            );
        }
        banners
    }

    pub(crate) fn composer_frame_anchor(&self, frame: AnyElement) -> AnyElement {
        let bounds = self.shell.composer_menus.bounds.clone();
        div()
            .flex()
            .flex_col()
            .flex_shrink(1.)
            .min_h_0()
            .w_full()
            .on_children_prepainted(move |children, _, _| {
                if let Some(bound) = children.first() {
                    bounds.set(Some(*bound));
                }
            })
            .child(frame)
            .into_any_element()
    }

    /// Presentation only: Enter can still queue the next Council message.
    pub(crate) fn council_composer_answering(&self) -> bool {
        self.mode == Mode::Council
            && self
                .active_council()
                .is_some_and(|page| page.answering() > 0)
    }

    pub(crate) fn composer_status_chip(&self, cx: &mut Context<Self>) -> ui::Chip {
        let blocked = (self.mode == Mode::Work
            && self.work.panel_is_visible(self.sidebar.active_id())
            && (self.work.panel.approval.is_some() || self.work.panel.credential.is_some()))
            || (self.mode == Mode::Code
                && self
                    .work
                    .code_tabs
                    .get(self.work.code_active)
                    .is_some_and(|tab| {
                        self.work
                            .plan_gates
                            .open_for(&crate::ask_mode::AskKey::CodeTab(tab.id))
                            .is_some()
                    }));
        let (label, tone) = if blocked {
            ("Needs you", theme::Tone::ActNow)
        } else if self.voice.dictating || self.voice.conv_capture.is_some() {
            ("Listening", theme::Tone::Neutral)
        } else if self.mode == Mode::Work && self.workshop_pending_for(self.sidebar.active_id()) {
            ("Waiting", theme::Tone::Neutral)
        } else if self.composer_busy() || self.council_composer_answering() {
            ("Running", theme::Tone::Neutral)
        } else if (self.mode == Mode::Work
            && (self
                .current_work_error()
                .is_some_and(|e| e.owner == self.sidebar.active_id())
                || crate::work_last_run::stored_last_run(
                    self.sidebar.active_id(),
                    &self.work_last_run,
                )
                .is_some_and(|last| last.status == awili_work::run_events::RunStatus::Failed)))
            || (self.mode == Mode::Code
                && self
                    .work
                    .code_tabs
                    .get(self.work.code_active)
                    .is_some_and(|tab| {
                        tab.convo
                            .len()
                            .checked_sub(1)
                            .is_some_and(|row| tab.errored.contains(&row))
                    }))
        {
            ("Failed", theme::Tone::ActNow)
        } else if self.feed_snapshot.badge > 0 {
            ("Ready to review", theme::Tone::Attention)
        } else {
            ("Ready", theme::Tone::Neutral)
        };
        ui::Chip::new("composer-status", label)
            .tab_index(focus_nav::SEND)
            .dot(tone)
            .tooltip("Open job details")
            .on_click(cx.listener(|app, _, window, cx| {
                app.toggle_job_panel(window, cx);
            }))
    }

    pub(crate) fn composer_voice_actions(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mic = ui::IconButton::new("mic", icons::Icon::Mic)
            .tab_index(focus_nav::SEND)
            .round()
            .label(if self.voice.dictating {
                "Stop dictation"
            } else {
                "Dictate"
            })
            .tooltip("Dictate — Fn to toggle")
            .on_click(cx.listener(|app, _, _, cx| app.toggle_mic(cx)));
        let voice = ui::IconButton::new("voice-mode", icons::Icon::Speaker)
            .tab_index(focus_nav::SEND)
            .round()
            .label(if self.voice.conversation_mode {
                "Turn hands-free voice off"
            } else {
                "Turn hands-free voice on"
            })
            .on_click(cx.listener(|app, _, _, cx| app.toggle_voice_mode(cx)));
        let mut actions = vec![
            if self.voice.dictating {
                mic.lead()
            } else {
                mic
            }
            .into_any_element(),
            if self.voice.conversation_mode {
                voice.lead()
            } else {
                voice
            }
            .into_any_element(),
        ];
        if self.voice.speaking {
            actions.push(
                ui::IconButton::new("stop-speak", icons::Icon::Square)
                    .tab_index(focus_nav::SEND)
                    .round()
                    .label("Stop reading aloud")
                    .on_click(cx.listener(|app, _, _, cx| app.stop_speaking(cx)))
                    .into_any_element(),
            );
        }
        actions
    }

    pub(crate) fn composer_details(&self) -> Option<AnyElement> {
        if self.mode == Mode::Work || !self.shell.composer_details {
            return None;
        }
        let mut details = self
            .status_parts_with_executor()
            .into_iter()
            .map(|(text, _)| text)
            .collect::<Vec<_>>();
        if let Some(status) = &self.voice.status {
            details.push(status.clone());
        }
        if let Some(progress) = self.voice.model.lock().unwrap().progress() {
            details.push(format!(
                "Voice model download: {}%",
                (progress * 100.0) as u32
            ));
        }
        Some(
            ui::Card::new("composer-details")
                .title("Details for nerds")
                .body(details.join(" · "))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
impl AwiliApp {
    /// Fixtures pick the active native tab's next target the way the chooser
    /// does (the production setter), instead of writing `tab.engine` or the
    /// ambient executor, which the route would ignore and fall through to an
    /// installed CLI.
    pub(crate) fn test_choose_native_target(
        &mut self,
        engine: awili_code::CodeEngine,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.mode != Mode::Code {
            self.set_mode(Mode::Code, cx);
        }
        let tab = self.work.code_tabs[self.work.code_active].id;
        assert!(
            self.set_composer_target(
                &ModelOwner::Native(tab),
                work::WorkExecutor::Agent { engine, model },
                cx,
            ),
            "the chooser accepts a target for the shown native tab"
        );
    }
}
