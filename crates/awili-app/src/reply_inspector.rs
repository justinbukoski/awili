//! A transient selection of an actual reply; recorded data, never the current helper.
use crate::{conv_view::TurnToken, theme, ui, work::Mode, AwiliApp, MessageRow, Role};
use gpui::{div, prelude::*, AnyElement, Context, FocusHandle, ScrollHandle, SharedString, Window};

#[derive(Clone)]
pub(crate) struct ReplySelection {
    pub(crate) owner: String,
    pub(crate) index: usize,
    code_session: Option<u64>,
    revision: TurnToken,
    ts: u64,
    kind: awili_core::MessageKind,
    /// The displayed copy/check payload, with presentation annotations hidden.
    text: SharedString,
    /// The exact canonical record fingerprint; presentation filtering is not identity.
    recorded_text: SharedString,
}

pub(crate) struct InspectorState {
    pub(crate) selection: Option<ReplySelection>,
    pub(crate) check: Option<ReplySelection>,
    pub(crate) details: bool,
    pub(crate) focus: FocusHandle,
    pub(crate) scroll: ScrollHandle,
    pub(crate) scrollbar: ui::scrollbar::ScrollBarState,
}

impl InspectorState {
    pub(crate) fn new(cx: &mut Context<AwiliApp>) -> Self {
        Self {
            selection: None,
            check: None,
            details: false,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            scrollbar: ui::scrollbar::ScrollBarState::new(),
        }
    }
}

impl ReplySelection {
    pub(super) fn capture(app: &AwiliApp, owner: &str, index: usize) -> Option<Self> {
        let (convo, rows, revision, code_session) = if app.mode == Mode::Code {
            let tab = app.work.code_tabs.get(app.work.code_active)?;
            if tab.history.conv_id.as_deref() != Some(owner) {
                return None;
            }
            (
                &tab.convo,
                &tab.rows,
                tab.reply_presentation_token.clone(),
                Some(tab.id),
            )
        } else {
            let view = app.views.get(owner)?;
            (
                &view.convo,
                &view.rows,
                view.reply_presentation_token.clone(),
                None,
            )
        };
        let msg = convo.message(index)?;
        (msg.role == Role::Assistant && msg.superseded_by.is_none()).then(|| Self {
            owner: owner.into(),
            index,
            code_session,
            revision,
            ts: msg.ts,
            kind: msg.kind,
            recorded_text: msg.text.clone().into(),
            text: rows
                .get(index)
                .map(|row| row.raw.clone())
                .unwrap_or_else(|| msg.text.clone().into()),
        })
    }

    fn data<'a>(
        &self,
        app: &'a AwiliApp,
    ) -> Option<(&'a awili_core::ConversationState, &'a [MessageRow], bool)> {
        if app.shell.archived_reader.is_some() {
            return None;
        }
        let (convo, rows, revision, streaming) = if let Some(id) = self.code_session {
            let tab = app.work.code_tabs.get(app.work.code_active)?;
            if app.mode != Mode::Code
                || tab.id != id
                || tab.history.conv_id.as_deref() != Some(self.owner.as_str())
            {
                return None;
            }
            (
                &tab.convo,
                tab.rows.as_slice(),
                &tab.reply_presentation_token,
                tab.streaming,
            )
        } else {
            if app.mode != Mode::Work || app.sidebar.active_id() != self.owner {
                return None;
            }
            let view = app.views.get(&self.owner)?;
            (
                &view.convo,
                view.rows.as_slice(),
                &view.reply_presentation_token,
                app.work.running && app.work.conv_id.as_deref() == Some(self.owner.as_str()),
            )
        };
        if (self.code_session.is_none() && app.conversations.get(&self.owner).is_none())
            || app
                .conversations
                .get(&self.owner)
                .is_some_and(|c| c.archived)
            || !revision.matches(&self.revision)
        {
            return None;
        }
        let msg = convo.message(self.index)?;
        (msg.role == Role::Assistant
            && msg.superseded_by.is_none()
            && msg.ts == self.ts
            && msg.kind == self.kind
            && msg.text == self.recorded_text.as_ref()
            && rows.get(self.index).is_some_and(|row| row.raw == self.text))
        .then_some((convo, rows, streaming))
    }

    pub(crate) fn matches(&self, app: &AwiliApp) -> bool {
        self.data(app).is_some()
    }
}

fn recorded_model_label(raw: &str) -> String {
    let family = crate::workshop_composer::model_family(raw);
    if family == "Configured model" {
        "Recorded model".into()
    } else {
        family
    }
}

/// One part of the prompt: its name, its size, and a bar for its share of the
/// whole.
fn context_part_row(
    index: usize,
    part: &crate::context_breakdown::ContextPart,
    total: u32,
) -> AnyElement {
    let frac = crate::context_breakdown::share(part.tokens, total);
    let pct = (frac * 100.0).round() as u32;
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .gap(theme::z(theme::SP_4))
        .debug_selector(move || format!("context-part-{index}"))
        .child(
            ui::Row::new(("reply-inspector-context-part", index))
                .title(part.label.clone())
                .subtitle(format!(
                    "{} \u{b7} {pct}%",
                    crate::context_breakdown::part_size(part)
                ))
                .wrap(),
        )
        .child(
            div()
                .w_full()
                .h(theme::z(theme::SP_4))
                .rounded(theme::z(theme::RADIUS_PILL))
                .bg(theme::p().line)
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(frac))
                        .rounded(theme::z(theme::RADIUS_PILL))
                        .bg(theme::p().contrast)
                        .debug_selector(move || format!("context-bar-{index}")),
                ),
        )
        .into_any_element()
}

fn links(blocks: &[crate::markdown_plan::PlanBlock], into: &mut Vec<String>) {
    use crate::markdown_plan::PlanBlock;
    for block in blocks {
        match block {
            PlanBlock::Text { text, .. } => {
                for url in text.urls.iter() {
                    if !into.contains(url) {
                        into.push(url.clone());
                    }
                }
            }
            PlanBlock::List { items, .. } => {
                for item in items {
                    links(item, into);
                }
            }
            PlanBlock::Quote(items) => links(items, into),
            PlanBlock::Code { .. } | PlanBlock::Rule | PlanBlock::Image(_) => {}
        }
    }
}

impl AwiliApp {
    pub(crate) fn sanitize_reply_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .shell
            .inspector
            .selection
            .as_ref()
            .is_some_and(|s| !s.matches(self))
        {
            let owned_focus = self.shell.inspector.focus.contains_focused(window, cx);
            self.shell.inspector.selection = None;
            if owned_focus {
                self.focus_shell_page(window, cx);
            }
        }
        if self
            .shell
            .inspector
            .check
            .as_ref()
            .is_some_and(|s| !s.matches(self))
        {
            self.shell.inspector.check = None;
            if self.shell.tabs.menu == Some(crate::workshop::WorkshopMenu::Engine) {
                self.close_workshop_menu(window, cx);
            }
        }
    }

    pub(crate) fn reply_inspect_control(
        &self,
        owner: &str,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let selected = ReplySelection::capture(self, owner, index)?;
        Some(
            div()
                .flex()
                .flex_none()
                .self_start()
                .debug_selector(move || format!("inspect-reply-{index}"))
                .child(
                    ui::Button::new(("inspect-reply", index), "Inspect reply")
                        .quiet()
                        .disabled_if(
                            selected
                                .data(self)
                                .is_some_and(|(_, _, streaming)| streaming)
                                .then_some("Wait for this reply to finish".into()),
                        )
                        .on_click(cx.listener(move |app, _, window, cx| {
                            if selected
                                .data(app)
                                .is_some_and(|(_, _, streaming)| !streaming)
                            {
                                app.shell.inspector.selection = Some(selected.clone());
                                app.shell.inspector.details = false;
                                app.shell
                                    .inspector
                                    .scroll
                                    .set_offset(gpui::Point::default());
                                app.shell.job_open = false;
                                app.shell.more = false;
                                app.shell.conversations = false;
                                app.feed_panel_open = false;
                                window.focus(&app.shell.inspector.focus, cx);
                                cx.notify();
                            }
                        })),
                )
                .into_any_element(),
        )
    }

    pub(crate) fn handle_reply_inspector_keys(
        &mut self,
        ev: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.shell.inspector.selection.is_none()
            || crate::focus_nav::overlay_open(self, cx)
            || !(crate::job_panel::uses_sheet(window)
                || self.shell.inspector.focus.contains_focused(window, cx))
        {
            return false;
        }
        let Some(key) =
            crate::transcript_keys::classify(&ev.keystroke, true, cfg!(target_os = "macos"))
        else {
            return false;
        };
        let scroll = &self.shell.inspector.scroll;
        let offset = scroll.offset();
        let y = crate::transcript_keys::target_offset_y(
            key,
            offset.y.into(),
            scroll.max_offset().y.into(),
            scroll.bounds().size.height.into(),
        );
        scroll.set_offset(gpui::point(offset.x, gpui::px(y)));
        cx.notify();
        true
    }

    pub(crate) fn close_reply_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.inspector.selection = None;
        self.focus_shell_page(window, cx);
        cx.notify();
    }

    fn reply_action_busy(&self, selected: &ReplySelection) -> bool {
        selected
            .data(self)
            .is_none_or(|(_, _, streaming)| streaming)
            || self.composer_busy()
            || self.workshop_work_unsettled()
            || self.work.local.down.as_ref().is_some_and(|down| {
                down.text.is_some()
                    && down
                        .conv_id
                        .as_deref()
                        .is_none_or(|owner| owner == selected.owner)
            })
    }

    pub(crate) fn start_reply_check(
        &mut self,
        selected: ReplySelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !selected.matches(self) || self.reply_action_busy(&selected) {
            return;
        }
        self.close_reply_inspector(window, cx);
        self.shell.inspector.check = Some(selected);
        self.open_workshop_helper_menu(window, cx);
        self.show_info_toast(
            "Choose a helper to check this reply. The new thread will wait for Send.",
            cx,
        );
    }

    pub(crate) fn choose_reply_check_helper(
        &mut self,
        executor: crate::work::WorkExecutor,
        check: Option<ReplySelection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shell.inspector.check = None;
        if let Some(selected) = check {
            if !selected.matches(self) || self.reply_action_busy(&selected) {
                return;
            }
            let question = selected
                .data(self)
                .and_then(|(convo, _, _)| {
                    crate::preceding_user_index(convo, selected.index)
                        .and_then(|index| convo.message(index))
                })
                .map(|msg| msg.text.clone())
                .unwrap_or_default();
            let request = format!("Check this reply for mistakes and explain any corrections.\n\nOriginal request:\n{question}\n\nReply to check:\n{}", selected.text);
            let original_tab = self.shell.tabs.active.clone();
            self.close_workshop_menu(window, cx);
            self.new_workshop_with(executor, cx);
            if self.shell.tabs.active != original_tab {
                self.restore_draft(&request, cx);
                self.focus_composer(window, cx);
            }
        } else {
            self.new_workshop_with(executor, cx);
            self.focus_composer(window, cx);
        }
    }

    pub(crate) fn reply_inspector_panel(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let selected = self
            .shell
            .inspector
            .selection
            .as_ref()
            .filter(|s| s.matches(self))?;
        let (convo, rows, _) = selected.data(self)?;
        let msg = convo.message(selected.index)?;
        let row = rows.get(selected.index)?;
        let model = msg
            .provenance
            .as_ref()
            .map(|p| {
                format!(
                    "{} · {}",
                    recorded_model_label(&p.model),
                    p.locality.as_str()
                )
            })
            .unwrap_or_else(|| "Not recorded for this reply".into());
        let cost = msg
            .provenance
            .as_ref()
            .and_then(|p| p.est_cost_usd)
            .map(|cost| format!("Estimated ${cost:.4}"))
            .unwrap_or_else(|| "Not recorded for this reply".into());
        let busy = self.reply_action_busy(selected);
        let run_id = self
            .run_ids
            .get(&selected.owner, selected.ts)
            .map(str::to_string);
        let mut urls = Vec::new();
        links(&row.plan, &mut urls);
        let mut body = div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap(theme::z(theme::SP_12))
            .child(
                ui::Row::new("reply-inspector-model")
                    .title("Model")
                    .subtitle(model)
                    .wrap(),
            )
            .child(
                ui::Row::new("reply-inspector-time")
                    .title("Time taken")
                    .subtitle(
                        row.footer
                            .as_ref()
                            .map(|footer| format!("Recorded summary: {footer}"))
                            .unwrap_or_else(|| "Not recorded for this reply".into()),
                    )
                    .wrap(),
            )
            .child(
                ui::Row::new("reply-inspector-cost")
                    .title("Cost")
                    .subtitle(cost)
                    .wrap(),
            )
            .child(
                ui::Row::new("reply-inspector-run-id")
                    .title("Run id")
                    .subtitle(run_id.clone().unwrap_or_else(|| "Not recorded for this reply".into()))
                    .wrap(),
            )
            .child(
                ui::Row::new("reply-inspector-sources")
                    .title("Sources")
                    .subtitle("No sources were recorded")
                    .wrap(),
            );
        if let Some(ctx) = &row.context {
            body = body.child(
                ui::Row::new("reply-inspector-context")
                    .title("What was sent to the model")
                    .subtitle(ctx.total_line())
                    .wrap(),
            );
            let total = ctx.total_tokens();
            for (i, part) in ctx.sorted().into_iter().enumerate() {
                body = body.child(context_part_row(i, part, total));
            }
        }
        // RC3-S4: one line when the last run left a rules file out or cut.
        // Live session only: nothing about it is saved with the turn.
        if let Some(line) = self
            .work
            .context_parts
            .as_ref()
            .filter(|(owner, _)| *owner == selected.owner)
            .and_then(|(_, parts)| crate::agent_launch::incomplete_summary(parts))
        {
            body = body.child(
                ui::Row::new("reply-inspector-instructions")
                    .title("Instructions")
                    .subtitle(line)
                    .wrap(),
            );
        }
        // rc.4 S7: whether the last agent run in this conversation was sandboxed.
        if let Some((owner, line)) = self.work.sandbox_note.as_ref() {
            if *owner == selected.owner {
                body = body.child(
                    ui::Row::new("reply-inspector-sandbox")
                        .title("Sandbox")
                        .subtitle(line.clone())
                        .wrap(),
                );
            }
        }
        for (index, url) in urls.into_iter().enumerate() {
            let open_url = url.clone();
            body = body.child(
                ui::Row::new(("reply-inspector-link", index))
                    .title("Link in this reply")
                    .subtitle(url)
                    .wrap()
                    .on_click(move |_, _, cx| cx.open_url(&open_url)),
            );
        }
        let mut details: Vec<AnyElement> = Vec::new();
        if let Some(p) = &msg.provenance {
            details.push(
                ui::label(format!(
                    "Recorded model: {}\nProvider: {}\nRoute: {}\nReason: {}",
                    p.model, p.provider, p.route, p.why
                ))
                .wrap()
                .into_any_element(),
            );
            if let Some(usage) = p.usage {
                details.push(
                    ui::label(format!(
                        "{} input tokens · {} cached (included in input) · {} output tokens",
                        usage.input_tokens,
                        usage
                            .cached_input_tokens
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "unreported".into()),
                        usage.output_tokens
                    ))
                    .wrap()
                    .into_any_element(),
                );
            }
        }
        if let Some(footer) = &row.footer {
            details.push(ui::label(footer.clone()).wrap().into_any_element());
        }
        for activity in &row.activity {
            details.push(ui::label(activity.clone()).wrap().into_any_element());
        }
        if details.is_empty() {
            details.push(
                ui::label("No additional details were recorded for this reply.")
                    .wrap()
                    .into_any_element(),
            );
        }
        body = body.child(
            ui::Section::new("reply-inspector-details", "Details for nerds")
                .nerd()
                .open(self.shell.inspector.details)
                .body(details)
                .on_toggle(cx.listener(|app, _, _, cx| {
                    app.shell.inspector.details = !app.shell.inspector.details;
                    cx.notify();
                })),
        );
        let owner_copy = selected.clone();
        let owner_check = selected.clone();
        let actions = div()
            .flex()
            .flex_wrap()
            .min_w_0()
            .w_full()
            .gap(theme::z(theme::SP_4))
            .child(
                ui::Button::new("reply-copy", "Copy")
                    .ghost()
                    .disabled_if(
                        selected
                            .text
                            .is_empty()
                            .then_some("This reply has no text to copy".into()),
                    )
                    .on_click(cx.listener(move |app, _, _, cx| {
                        if owner_copy.matches(app) {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                owner_copy.text.to_string(),
                            ));
                        }
                    })),
            )
            .children(run_id.map(|id| {
                ui::Button::new("reply-copy-run-id", "Copy run id")
                    .ghost()
                    .on_click(cx.listener(move |app, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(id.clone()));
                        app.show_info_toast("Run id copied", cx);
                    }))
            }))
            .child(
                ui::Button::new("reply-check", "Ask another model to check")
                    .quiet()
                    .disabled_if(
                        busy.then_some("Finish or resolve the waiting request first".into()),
                    )
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.start_reply_check(owner_check.clone(), window, cx)
                    })),
            )
            .child(
                ui::Button::new("reply-report", "Open report")
                    .quiet()
                    .disabled_if(Some("No report was recorded for this reply".into())),
            );
        let sheet = crate::job_panel::uses_sheet(window);
        Some(
            div()
                .debug_selector(|| "awili-reply-inspector".into())
                .flex_none()
                .min_h_0()
                .w(theme::z(theme::PANEL_W))
                .my(theme::z(theme::EDGE))
                .mr(theme::z(theme::EDGE))
                .when(sheet, |d| d.absolute().inset_0().w_full().h_full().m_0())
                .child(
                    ui::Panel::new("reply-inspector")
                        .sheet_when(sheet)
                        .header("Reply", Some("Inspector".into()))
                        .full_height()
                        .focus_handle(&self.shell.inspector.focus)
                        .scroll_handle(self.shell.inspector.scroll.clone())
                        .scroll_state(self.shell.inspector.scrollbar.clone())
                        .body(body)
                        .footer(
                            ui::Pill::new("reply-selection-actions")
                                .bar()
                                .bounded()
                                .child(actions),
                        )
                        .on_close(
                            cx.listener(|app, _, window, cx| app.close_reply_inspector(window, cx)),
                        ),
                )
                .into_any_element(),
        )
    }
}
