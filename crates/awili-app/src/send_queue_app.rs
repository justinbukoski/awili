//! RC3-S5: the app side of the send queue — Enter on a busy Work/Chat
//! composer, draining when a turn ends, the Edit / Cancel / Send now /
//! Resume verbs, and the list under the composer. State is in `send_queue.rs`.
use super::*;
use crate::ask_mode::AskKey;
use crate::send_queue::{PauseReason, QueueItem, QueueSnapshot, QueueState, MAX_QUEUED};
use crate::work::WorkExecutor;
use awili_gateway::run_events::RunStatus;
use gpui::AnyElement;
use std::path::PathBuf;

impl AwiliApp {
    /// Enter on a busy composer in Work/Chat: the draft joins this
    /// conversation's queue instead of being refused, together with what the
    /// owner had chosen (helper, folder, ask/auto) and the files on the tray.
    /// Other modes keep the M2e explanation.
    pub(crate) fn queue_busy_draft(&mut self, cx: &mut Context<Self>) {
        if self.mode != Mode::Work || self.shell.archived_reader.is_some() {
            self.explain_busy_composer(cx);
            return;
        }
        let conv = self.sidebar.active_id().to_string();
        if self.work.send_queue.queued_count(&conv) >= MAX_QUEUED {
            self.show_toast(
                "Too many messages are waiting. Cancel one or wait for the run.",
                cx,
            );
            return;
        }
        let text = self.composer.update(cx, |c, cx| {
            cx.notify();
            c.take()
        });
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.invalidate_dictation();
        self.prompt_history.record(&text);
        // The files on the tray belong to this message: take them off now so
        // the next one does not pick them up.
        let attachments = self
            .search_store
            .as_ref()
            .and_then(|store| store.get_staged_attachments(&conv).ok())
            .unwrap_or_default();
        if !attachments.is_empty() {
            let cleared = self
                .search_store
                .as_ref()
                .map_or(Ok(()), |store| store.clear_staged_attachments(&conv));
            if let Err(error) = cleared {
                // Nothing was queued: the message and its files stay as they were.
                self.restore_draft(&text, cx);
                self.show_sticky_toast(
                    format!("Could not queue that message with its files — {error}"),
                    cx,
                );
                return;
            }
            self.refresh_staged_attachments();
        }
        let snap = QueueSnapshot {
            body: crate::attachments::compose_message_with_attachments(&text, &attachments),
            target: self.queue_current_target(&conv),
            folder: self.work.folder.clone(),
            mode: self.work.ask_modes.mode_for(&AskKey::Work(conv.clone())),
            attachment_ids: attachments.iter().map(|a| a.id).collect(),
        };
        self.work.send_queue.enqueue(&conv, text, snap);
        if self.work.running {
            self.work.send_queue.watching = Some(conv);
        }
        cx.notify();
    }

    /// The helper `conv`'s next message would use right now.
    fn queue_current_target(&self, conv: &str) -> WorkExecutor {
        self.work
            .conversation_routes
            .get(conv)
            .map(|route| route.next_target().clone())
            .or_else(|| self.work.chip_picked.get(conv).cloned())
            .unwrap_or_else(|| self.work.executor.clone())
    }

    /// The folder `conv` works in: the shown one, or its own.
    fn queue_conv_folder(&self, conv: &str) -> Option<PathBuf> {
        if conv == self.sidebar.active_id() {
            return self.work.folder.clone();
        }
        self.work.folder_picked.get(conv).cloned().or_else(|| {
            self.conversations
                .get(conv)
                .and_then(|c| c.cwd.as_deref())
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
        })
    }

    /// Whether the chooser still offers `target` as runnable.
    fn queue_target_available(&self, target: &WorkExecutor) -> bool {
        crate::executor_labels::target_sections(&crate::executor_labels::TargetReach {
            engine: &self.work.engine,
            extras: &self.work.extra_engines,
            models: &self.work.models_cfg,
            agents: &self.work.agents,
            build_targets: &self.work.build_targets,
            gateway_set_up: self.work.hermes.enabled || self.work.gateway.enabled,
        })
        .iter()
        .flat_map(|section| section.rows.iter())
        .any(|row| &row.target == target && row.unavailable.is_none())
    }

    /// Why the queue must not send into `conv` yet: the owner has a plan or
    /// an approval to answer there, and a new message would supersede it.
    pub(crate) fn send_queue_hold(&self, conv: &str) -> Option<&'static str> {
        let mut keys = vec![AskKey::Work(conv.to_string())];
        keys.extend(
            self.work
                .code_tabs
                .iter()
                .filter(|tab| tab.history.conv_id.as_deref() == Some(conv))
                .map(|tab| AskKey::CodeTab(tab.id)),
        );
        if keys
            .iter()
            .any(|key| self.work.plan_gates.open_for(key).is_some())
        {
            return Some("Waiting for your answer on the plan.");
        }
        if self.work.panel.approval.is_some() && self.work.conv_id.as_deref() == Some(conv) {
            return Some("Waiting for your answer on the approval.");
        }
        None
    }

    /// Observed on every app notify: when the turn we were watching has ended
    /// (and nothing else still holds the conversation), pause the queue after
    /// a failure or a Stop, or send its next message.
    pub(crate) fn send_queue_tick(&mut self, cx: &mut Context<Self>) {
        if self.work.running {
            if let Some(conv) = self
                .work
                .conv_id
                .clone()
                .or_else(|| self.work.worker_conv.clone())
            {
                self.work.send_queue.watching = Some(conv);
            }
            return;
        }
        let Some(conv) = self.work.send_queue.watching.clone() else {
            return;
        };
        // A gateway Stop still being acknowledged, or a native turn still
        // owning the conversation: wait for the next notify.
        if self.work.gateway_stop_pending
            || self.work.queued_after_stop.is_some()
            || self.work.active_work_route_turn.is_some()
            || self.work.agent_gate_pending
            || self.work.resume_gate_pending
        {
            return;
        }
        let send_now = self.work.send_queue.send_now_for.as_deref() == Some(conv.as_str());
        // A plan or approval card waits for the owner: sending would replace
        // it. Only an explicit Send now / Resume goes past.
        if !send_now && self.send_queue_hold(&conv).is_some() {
            return;
        }
        self.work.send_queue.watching = None;
        self.work.send_queue.send_now_for = None;
        let failed = self.work.panel.error.is_some()
            || matches!(self.work.panel.status, Some(RunStatus::Failed));
        let stopped =
            self.work.owner_stopped || matches!(self.work.panel.status, Some(RunStatus::Stopped));
        self.work.send_queue.prune_finished(&conv);
        if self.work.send_queue.queued_count(&conv) == 0 {
            return;
        }
        if send_now {
            self.send_queue_drain(&conv, cx);
        } else if failed {
            self.work.send_queue.pause(&conv, PauseReason::Failed);
            cx.notify();
        } else if stopped {
            self.work.send_queue.pause(&conv, PauseReason::Stopped);
            cx.notify();
        } else if self.work.send_queue.paused(&conv).is_none() {
            self.send_queue_drain(&conv, cx);
        }
    }

    /// Send the conversation's next queued message as a normal turn, with the
    /// helper, folder and ask/auto choice it was queued under.
    pub(crate) fn send_queue_drain(&mut self, conv: &str, cx: &mut Context<Self>) {
        if !self.views.contains_key(conv) {
            self.work.send_queue.forget(conv);
            return;
        }
        let Some(item) = self.work.send_queue.next_queued(conv) else {
            return;
        };
        if let Some(why) = self.queue_snapshot_problem(conv, &item) {
            self.work
                .send_queue
                .pause(conv, PauseReason::Refused(why.to_string()));
            cx.notify();
            return;
        }
        self.guard_submit(&item.snap.body);
        // The files typed for LATER messages stay off the tray while this one
        // goes out (its own are already in its text).
        let stash: Vec<i64> = self
            .search_store
            .as_ref()
            .and_then(|store| store.get_staged_attachments(conv).ok())
            .map(|list| list.iter().map(|a| a.id).collect())
            .unwrap_or_default();
        if !stash.is_empty() {
            let cleared = self
                .search_store
                .as_ref()
                .map_or(Ok(()), |store| store.clear_staged_attachments(conv));
            if cleared.is_err() {
                self.work.send_queue.pause(
                    conv,
                    PauseReason::Refused(
                        "your files for the next message could not be set aside. Press Resume to try again."
                            .to_string(),
                    ),
                );
                self.submit_guard = None;
                cx.notify();
                return;
            }
        }
        if item.snap.folder != self.work.folder {
            if conv != self.sidebar.active_id() {
                // Rebinding would change the folder of the thread on screen.
                self.work.send_queue.pause(
                    conv,
                    PauseReason::Refused(
                        "it belongs to a different folder than the one open. Open this thread and press Resume."
                            .to_string(),
                    ),
                );
                self.submit_guard = None;
                self.restage_files(conv, &stash, cx);
                cx.notify();
                return;
            }
            self.bind_work_folder_state(item.snap.folder.clone(), cx);
        }
        let route_seed = self.work.executor.clone();
        let route = self
            .work
            .conversation_routes
            .entry(conv.to_string())
            .or_insert_with(|| crate::conversation_route::ConversationRoute::new(route_seed));
        let saved_target = route.next_target().clone();
        route.set_next_target(item.snap.target.clone());
        let previous_executor = self.work.executor.clone();
        let body = item.snap.body.clone();
        if conv == self.sidebar.active_id() {
            self.submit_work_local_first(body, cx);
        } else {
            self.work.executor = item.snap.target.clone();
            if crate::local_model_health::gate_applies(
                self.work.running,
                self.work.gateway_stop_pending,
                &self.work.executor,
            ) {
                self.local_gate_in(body, conv.to_string(), cx);
            } else {
                self.submit_work_run_in(body, conv.to_string(), cx);
            }
            let started = self.work.running
                || self.work.active_work_route_turn.is_some()
                || self
                    .work
                    .active_code_route_turns
                    .values()
                    .any(|turn| turn.conversation_id == conv);
            if !started {
                self.work.executor = previous_executor;
            }
        }
        if let Some(route) = self.work.conversation_routes.get_mut(conv) {
            route.set_next_target(saved_target);
        }
        self.restage_files(conv, &stash, cx);
        match self.submit_guard.take() {
            None => self.work.send_queue.mark_sent(conv, item.id),
            Some(guard) => {
                let why = guard
                    .reason
                    .unwrap_or_else(|| "the next message could not start.".to_string());
                self.work.send_queue.pause(conv, PauseReason::Refused(why));
            }
        }
        cx.notify();
    }

    /// A plain sentence when the choices a message was queued under can no
    /// longer be honoured; `None` when it can go.
    fn queue_snapshot_problem(&self, conv: &str, item: &QueueItem) -> Option<&'static str> {
        if !self.queue_target_available(&item.snap.target) {
            return Some(
                "the helper or model it was queued for is not available now. Choose another, then press Resume.",
            );
        }
        if let Some(folder) = item.snap.folder.as_deref() {
            if !folder.is_dir() {
                return Some(
                    "the folder it was queued for is gone. Pick a folder, then press Resume.",
                );
            }
        }
        if self
            .work
            .ask_modes
            .mode_for(&AskKey::Work(conv.to_string()))
            != item.snap.mode
        {
            return Some(
                "the ask-before-changes choice changed after it was queued. Check it, then press Resume.",
            );
        }
        None
    }

    /// Edit: the message goes back into the composer (and its files back on
    /// the tray) and leaves the queue.
    pub(crate) fn send_queue_edit(&mut self, conv: &str, id: u64, cx: &mut Context<Self>) {
        if let Some(item) = self.work.send_queue.take_for_edit(conv, id) {
            self.restage_files(conv, &item.snap.attachment_ids, cx);
            self.restore_draft(&item.text, cx);
            cx.notify();
        }
    }

    /// Put files back on the tray; say so if that fails.
    fn restage_files(&mut self, conv: &str, ids: &[i64], cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        if let Some(store) = self.search_store.as_ref() {
            if let Some(error) = ids
                .iter()
                .filter_map(|id| store.stage_attachment(conv, *id).err())
                .next()
            {
                self.show_sticky_toast(
                    format!("Could not put a file back on the tray — {error}"),
                    cx,
                );
            }
        }
        self.refresh_staged_attachments();
    }

    /// Cancel keeps the row (as Cancelled) and gives its files back.
    pub(crate) fn send_queue_cancel(&mut self, conv: &str, id: u64, cx: &mut Context<Self>) {
        let files = self
            .work
            .send_queue
            .items(conv)
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.snap.attachment_ids.clone())
            .unwrap_or_default();
        if self.work.send_queue.cancel(conv, id) {
            self.restage_files(conv, &files, cx);
            cx.notify();
        }
    }

    /// Resume a paused queue; with no turn running it sends right away. A
    /// pause about changed choices is settled here: the messages take the
    /// choices now in force.
    pub(crate) fn send_queue_resume(&mut self, conv: &str, cx: &mut Context<Self>) {
        if matches!(
            self.work.send_queue.paused(conv),
            Some(PauseReason::Refused(_))
        ) {
            let target = self.queue_current_target(conv);
            let folder = self.queue_conv_folder(conv);
            let mode = self
                .work
                .ask_modes
                .mode_for(&AskKey::Work(conv.to_string()));
            self.work.send_queue.retarget(conv, &target, &folder, mode);
        }
        self.work.send_queue.resume(conv);
        if !self.work.running {
            self.work.send_queue.watching = Some(conv.to_string());
            self.work.send_queue.send_now_for = Some(conv.to_string());
        }
        cx.notify();
    }

    /// Send now: stop the running turn with the ordinary Stop, then this
    /// message goes next. With nothing running it simply goes.
    pub(crate) fn send_queue_send_now(&mut self, conv: &str, id: u64, cx: &mut Context<Self>) {
        if !self.work.send_queue.send_now(conv, id) {
            return;
        }
        let running_here = self.work.running
            && self
                .work
                .conv_id
                .as_deref()
                .or(self.work.worker_conv.as_deref())
                .is_none_or(|owner| owner == conv);
        if running_here {
            self.work.send_queue.watching = Some(conv.to_string());
            self.cancel_work_run(cx);
        } else if self.work.running {
            // Another thread owns the run: this message waits its turn and
            // no Stop-for-it is pending.
            self.work.send_queue.send_now_for = None;
            self.show_toast(
                "Another thread is running. This message will wait; try Send now again once it ends.",
                cx,
            );
        } else {
            self.work.send_queue.watching = Some(conv.to_string());
        }
        cx.notify();
    }

    /// The list under the composer: this conversation's queue.
    pub(crate) fn send_queue_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let conv = self.sidebar.active_id().to_string();
        let items = self.work.send_queue.items(&conv);
        if items.is_empty() {
            return None;
        }
        let paused = self.work.send_queue.paused(&conv).map(PauseReason::label);
        let hold = self
            .send_queue_hold(&conv)
            .filter(|_| paused.is_none() && self.work.send_queue.queued_count(&conv) > 0);
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(label) = paused {
            let resume_conv = conv.clone();
            rows.push(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(theme::z(theme::SP_8))
                    .child(
                        div()
                            .text_color(theme::p().chip_warn)
                            .child(SharedString::from(label)),
                    )
                    .child(queue_button("queue-resume", "Resume", true).on_click(
                        cx.listener(move |this, _, _, cx| this.send_queue_resume(&resume_conv, cx)),
                    ))
                    .into_any_element(),
            );
        }
        if let Some(label) = hold {
            rows.push(
                div()
                    .text_color(theme::p().chip_warn)
                    .child(SharedString::from(label))
                    .into_any_element(),
            );
        }
        for item in items {
            let (id, queued) = (item.id, item.state == QueueState::Queued);
            let mut row = div()
                .id(("queue-row", id as usize))
                .flex()
                .flex_row()
                .items_center()
                .gap(theme::z(theme::SP_8))
                .child(
                    div()
                        .flex_none()
                        .text_color(theme::p().faint)
                        .child(item.state.label()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(if queued {
                            theme::p().text
                        } else {
                            theme::p().faint
                        })
                        .child(SharedString::from(item_label(item))),
                );
            if queued {
                let (c1, c2, c3) = (conv.clone(), conv.clone(), conv.clone());
                row = row
                    .child(
                        queue_button(("queue-edit", id as usize), "Edit", false).on_click(
                            cx.listener(move |this, _, _, cx| this.send_queue_edit(&c1, id, cx)),
                        ),
                    )
                    .child(
                        queue_button(("queue-cancel", id as usize), "Cancel", false).on_click(
                            cx.listener(move |this, _, _, cx| this.send_queue_cancel(&c2, id, cx)),
                        ),
                    )
                    .child(
                        queue_button(("queue-send-now", id as usize), "Send now", true).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.send_queue_send_now(&c3, id, cx)
                            }),
                        ),
                    );
            }
            rows.push(row.into_any_element());
        }
        Some(
            div()
                .id("send-queue")
                .debug_selector(|| "send-queue".into())
                .flex()
                .flex_col()
                .flex_none()
                .gap(theme::z(theme::SP_4))
                .pb(theme::z(theme::SP_12))
                .text_size(theme::z(theme::type_::CAPTION.size))
                .children(rows)
                .into_any_element(),
        )
    }
}

fn queue_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    primary: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .px(theme::z(theme::SP_8))
        .py(theme::z(theme::SP_4))
        .rounded(theme::z(theme::RADIUS))
        .border_1()
        .border_color(theme::p().border)
        .text_color(if primary {
            theme::p().accent_text
        } else {
            theme::p().muted
        })
        .cursor_pointer()
        .hover(|s| s.border_color(theme::p().accent))
        .child(label)
}

fn item_label(item: &QueueItem) -> String {
    let text = item.text.replace('\n', " ");
    match item.snap.attachment_ids.len() {
        0 => text,
        1 => format!("{text} (1 file)"),
        n => format!("{text} ({n} files)"),
    }
}
