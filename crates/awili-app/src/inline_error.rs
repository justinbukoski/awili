//! Transient, explicitly owned ErrorCard presentation. Durable replies and
//! existing dispatch/Retry/folder rules remain authoritative.
use std::{path::PathBuf, sync::Arc};

use awili_core::{MessageKind, Role};
use gpui::{div, prelude::*, AnyElement, Context, PathPromptOptions, Window};

use crate::{conv_view::TurnToken, theme, ui, AwiliApp};

/// Shown when the cause is not something the owner can fix from this Mac.
pub(crate) const NOT_FIXABLE_HERE: &str =
    "This is not something you can fix here. Try again, or choose another model.";

#[derive(Clone)]
pub(crate) struct ReplyAnchor {
    pub(crate) index: usize,
    revision: TurnToken,
    ts: u64,
    kind: MessageKind,
    text: String,
}
impl std::fmt::Debug for ReplyAnchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplyAnchor")
            .field("index", &self.index)
            .field("ts", &self.ts)
            .finish_non_exhaustive()
    }
}
impl PartialEq for ReplyAnchor {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
            && self.revision.matches(&other.revision)
            && self.ts == other.ts
            && self.kind == other.kind
            && self.text == other.text
    }
}
impl Eq for ReplyAnchor {}

#[derive(Clone, Debug)]
pub(crate) struct ErrorPresentation {
    identity: Arc<()>,
    pub(crate) request: String,
    pub(crate) owner: String,
    pub(crate) run_seq: u64,
    pub(crate) cause: String,
    /// S3: the short id of the run that failed.
    pub(crate) run_id: String,
    pub(crate) reply: Option<ReplyAnchor>,
}
impl PartialEq for ErrorPresentation {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
            && self.owner == other.owner
            && self.run_seq == other.run_seq
            && self.cause == other.cause
            && self.reply == other.reply
    }
}
impl Eq for ErrorPresentation {}

impl AwiliApp {
    /// Synchronize an error without assigning ownership from the visible tab.
    /// Completion passes its finalized reply, after all durable text changes.
    pub(crate) fn capture_work_error(&mut self, reply: Option<usize>) {
        let (Some(owner), Some(cause)) = (self.work.conv_id.clone(), self.work.panel.error.clone())
        else {
            self.work.error_presentation = None;
            return;
        };
        let existing = self
            .work
            .error_presentation
            .as_ref()
            .filter(|e| e.owner == owner && e.run_seq == self.work.run_seq && e.cause == cause);
        let mut presentation = existing.cloned().unwrap_or_else(|| ErrorPresentation {
            identity: Arc::new(()),
            request: uuid::Uuid::new_v4().to_string(),
            owner: owner.clone(),
            run_seq: self.work.run_seq,
            cause,
            run_id: Some(self.work.graduation.run_id())
                .filter(|id| !id.is_empty())
                .unwrap_or_else(crate::run_error::new_run_id),
            reply: None,
        });
        if let Some(index) = reply.or(self.work.stream_row_idx) {
            presentation.reply = self.views.get(&owner).and_then(|view| {
                let message = view
                    .convo
                    .message(index)
                    .filter(|m| m.role == Role::Assistant)?;
                Some(ReplyAnchor {
                    index,
                    revision: view.reply_presentation_token.clone(),
                    ts: message.ts,
                    kind: message.kind,
                    text: message.text.clone(),
                })
            });
        }
        self.work.error_presentation = Some(presentation);
    }

    pub(crate) fn current_work_error(&self) -> Option<&ErrorPresentation> {
        self.work.error_presentation.as_ref().filter(|e| {
            e.run_seq == self.work.run_seq
                && self.views.contains_key(&e.owner)
                && self
                    .conversations
                    .get(&e.owner)
                    .is_some_and(|c| !c.archived)
        })
    }

    pub(crate) fn work_error_matches(&self, expected: &ErrorPresentation) -> bool {
        self.current_work_error()
            .is_some_and(|now| Arc::ptr_eq(&now.identity, &expected.identity))
    }

    pub(crate) fn work_error_reply(&self) -> Option<usize> {
        let error = self.current_work_error()?;
        let anchor = error.reply.as_ref()?;
        let view = self.views.get(&error.owner)?;
        let msg = view.convo.message(anchor.index)?;
        let live = (self.work.running || self.work.gateway_stop_pending)
            && self.work.conv_id.as_deref() == Some(error.owner.as_str())
            && self.work.stream_row_idx == Some(anchor.index);
        (view.reply_presentation_token.matches(&anchor.revision)
            && msg.role == Role::Assistant
            && msg.superseded_by.is_none()
            && msg.ts == anchor.ts
            && msg.kind == anchor.kind
            && (live || msg.text == anchor.text)
            && view
                .rows
                .get(anchor.index)
                .is_some_and(|r| r.role == Role::Assistant))
        .then_some(anchor.index)
    }

    pub(crate) fn workshop_error_row(&self) -> Option<usize> {
        let error = self.current_work_error()?;
        (error.owner == self.sidebar.active_id())
            .then(|| self.work_error_reply())
            .flatten()
    }

    pub(crate) fn work_error_needs_folder(&self, error: &ErrorPresentation) -> bool {
        crate::submit_guard::is_missing_folder_notice(&error.cause)
            || matches!(
                error.cause.as_str(),
                "agent runs need a working folder — choose one first"
                    | "local builds need a working folder — choose one first"
            )
    }

    pub(crate) fn work_error_is_setup(&self, error: &ErrorPresentation) -> bool {
        self.work_error_needs_folder(error)
            || crate::agent_signin::is_signed_out_notice(&error.cause)
            || crate::work::provider_setup_hint(&error.cause).is_some()
            || crate::work::is_engine_setup_notice(&error.cause)
    }

    pub(crate) fn work_error_tone(&self, _error: &ErrorPresentation) -> theme::Tone {
        theme::Tone::ActNow
    }

    fn work_error_retry_row(&self) -> Option<usize> {
        let error = self.current_work_error()?;
        let index = self.work_error_reply()?;
        let view = self.views.get(&error.owner)?;
        (crate::conv_view::regen_target(view) == Some(index)
            && crate::prompt_history::retry_text(&view.convo).is_some())
        .then_some(index)
    }

    /// Cancel and late callbacks leave the cause and every owner's draft alone.
    pub(crate) fn apply_error_folder_pick(
        &mut self,
        expected: &ErrorPresentation,
        path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(path) = path.filter(|p| p.is_dir()) else {
            return false;
        };
        if !self.work_error_matches(expected)
            || self.sidebar.active_id() != expected.owner
            || self.shell.archived_reader.is_some()
            || self.work_error_action_busy()
            || self
                .conversations
                .get(&expected.owner)
                .is_none_or(|c| c.archived)
        {
            return false;
        }
        self.choose_work_folder(path, cx);
        if self.work_error_matches(expected) {
            if self.work.conv_id.as_deref() == Some(expected.owner.as_str())
                && self.work.panel.error.as_ref() == Some(&expected.cause)
            {
                self.work.panel.error = None;
            }
            self.work.error_presentation = None;
        }
        cx.notify();
        true
    }

    pub(crate) fn work_error_action_busy(&self) -> bool {
        self.workshop_work_runner_unsettled()
            || self.work.queued_after_stop.is_some()
            || self.work.local.health.pending_send.is_some()
            || self.work.health.gateway.pending_send.is_some()
            || self.current_work_error().is_some_and(|e| {
                self.work.local.down.as_ref().is_some_and(|down| {
                    down.text.is_some()
                        && down.conv_id.as_deref().is_none_or(|owner| owner == e.owner)
                }) || self
                    .work
                    .plan_gates
                    .open_for(&crate::ask_mode::AskKey::Work(e.owner.clone()))
                    .is_some()
            })
    }

    pub(crate) fn complete_error_folder_pick(
        &mut self,
        expected: &ErrorPresentation,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let admitted = self.apply_error_folder_pick(expected, path, cx);
        if admitted {
            self.focus_shell_page(window, cx);
        }
        admitted
    }

    fn pick_error_folder(
        &mut self,
        expected: ErrorPresentation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.work_error_matches(&expected)
            || self.sidebar.active_id() != expected.owner
            || self.work_error_action_busy()
        {
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose working folder".into()),
        });
        let window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let path = rx
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
                .and_then(|p| p.into_iter().next());
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |app, cx| {
                        app.complete_error_folder_pick(&expected, path, window, cx);
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
    }

    pub(crate) fn work_error_duplicates_toast(&self, message: &str) -> bool {
        crate::consolidation::workshop_mode(self.mode)
            && self.shell.archived_reader.is_none()
            && self
                .current_work_error()
                .is_some_and(|error| error.cause == message)
    }

    fn saved_work_error_card(&self) -> Option<AnyElement> {
        let owner = self.sidebar.active_id();
        if self.work.conv_id.as_deref() == Some(owner) && self.workshop_work_runner_unsettled()
            || self.workshop_pending_for(owner)
        {
            return None;
        }
        let last = crate::work_last_run::stored_last_run(owner, &self.work_last_run)?;
        let cause = last.error.filter(|e| !e.trim().is_empty())?;
        Some(ui::Card::new("work-error-card").tone(theme::Tone::ActNow)
            .label("Previous job failed").body(cause)
            .children([ui::label("Saved failure. Use the reply's existing actions or send a new request to continue.").wrap()])
            .into_any_element())
    }

    pub(crate) fn work_error_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !crate::consolidation::workshop_mode(self.mode) || self.shell.archived_reader.is_some() {
            return None;
        }
        let Some(error) = self
            .current_work_error()
            .filter(|e| e.owner == self.sidebar.active_id())
        else {
            return self.saved_work_error_card();
        };
        let needs_folder = self.work_error_needs_folder(error);
        let retry = (!needs_folder)
            .then(|| self.work_error_retry_row())
            .flatten();
        let expected = error.clone();
        let classified = crate::run_error::classify(&error.cause);
        let mut card = ui::Card::new("work-error-card")
            .tone(self.work_error_tone(error))
            .label(classified.category.title())
            .body(error.cause.clone());
        let action: Option<AnyElement> = if needs_folder {
            Some(
                ui::Button::new("error-fix-folder", "Choose folder")
                    .selector("error-fix-folder")
                    .ghost()
                    .disabled_if(
                        self.work_error_action_busy()
                            .then_some("Finish or resolve the current job first".into()),
                    )
                    .focus_handle(&self.shell.error_focus)
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.pick_error_folder(expected.clone(), window, cx)
                    }))
                    .into_any_element(),
            )
        } else if let Some(index) = retry {
            let busy = self.work_error_action_busy();
            Some(
                ui::Button::new("error-retry", "Retry")
                    .selector("error-retry")
                    .ghost()
                    .focus_handle(&self.shell.error_focus)
                    .disabled_if(busy.then_some("Finish or resolve the current job first".into()))
                    .on_click(cx.listener(move |app, _, window, cx| {
                        if app.work_error_matches(&expected)
                            && !app.work_error_action_busy()
                            && app.work_error_reply() == Some(index)
                        {
                            app.retry_work_in(expected.owner.clone(), index, cx);
                            if app.current_work_error().is_none() {
                                app.focus_shell_page(window, cx);
                            }
                        }
                    }))
                    .into_any_element(),
            )
        } else {
            None
        };
        // S3: the category's primary action (Sign in, Open settings, Try
        // another model) leads; Retry and Choose folder keep their own wiring.
        let primary = (!needs_folder)
            .then(|| {
                self.run_error_action_button(
                    "error-primary-action",
                    "error-primary-action",
                    classified.action,
                    &error.cause,
                    self.work_error_engine(),
                    cx,
                )
            })
            .flatten()
            .filter(|_| {
                matches!(
                    classified.action,
                    crate::run_error::RunErrorAction::SignIn
                        | crate::run_error::RunErrorAction::OpenSettings
                        | crate::run_error::RunErrorAction::TryAnotherModel
                )
            });
        let action = match (primary, action) {
            (Some(p), a) => Some(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(theme::z(theme::SP_4))
                    .child(p)
                    .children(a)
                    .into_any_element(),
            ),
            (None, a) => a,
        };
        let run_id_row = self.run_id_row(
            "error-run-id-copy",
            "error-run-id-copy",
            &error.run_id,
            cx,
        );
        let mut parts: Vec<AnyElement> = Vec::new();
        if !classified.category.user_can_fix() {
            parts.push(
                ui::label(NOT_FIXABLE_HERE)
                    .wrap()
                    .into_any_element(),
            );
        }
        if retry.is_some() && self.work_error_tone(error) == theme::Tone::Attention {
            parts.push(
                ui::label("Try the original request again. Your unsent draft stays in the box.")
                    .wrap()
                    .into_any_element(),
            );
        }
        let has_action = action.is_some();
        if let Some(action) = action {
            parts.push(
                div()
                    .on_children_prepainted(
                        self.reveal_work_blocking_control(format!("error:{}", error.request)),
                    )
                    .child(action)
                    .into_any_element(),
            );
        } else {
            parts.push(
                ui::label("Check the cause above, then try your request again.")
                    .wrap()
                    .into_any_element(),
            );
        }
        parts.push(run_id_row);
        card = card.children(parts);
        Some(
            div()
                .when(!has_action, |d| {
                    d.on_children_prepainted(
                        self.reveal_work_error_head(format!("error:{}", error.request)),
                    )
                })
                .id("work-error-presentation")
                .debug_selector(|| "awili-inline-error".into())
                .track_focus(&self.shell.error_focus)
                .child(card)
                .into_any_element(),
        )
    }

    pub(crate) fn focus_work_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .current_work_error()
            .is_none_or(|e| e.owner != self.sidebar.active_id())
        {
            return;
        }
        let request = format!("error:{}", self.current_work_error().unwrap().request);
        self.request_work_blocking_reveal(request);
        window.focus(&self.shell.error_focus, cx);
    }
}
