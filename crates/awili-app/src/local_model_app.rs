//! L1e2 — the in-app pre-send gate, the down card, the one-click cloud
//! switch, and the status segment's data.
//!
//! The Work submit chain (one entry, `dispatch_composer_text`):
//! `submit_work_local_first` (this module: the in-app lane only) →
//! `submit_work_gated` (U2c: the fleet-gateway lane only) →
//! `submit_work_run` (S1 instructions, S5 memory gate, U4 plan gate, the
//! executor's own path). The two pre-send gates never both apply: each
//! checks its own executor.
//!
//! Nothing here switches executor or resends by itself. A down or
//! unconfigured model keeps the message on the card; only the card's
//! "Send with <cloud> instead" click switches (persisted, like a picker
//! pick) and sends that one message once.

use std::time::Instant;

use gpui::{
    div, Context, FontWeight, InteractiveElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled,
};

use crate::gateway_health::{PendingSend, PROBE_TIMEOUT};
use crate::local_model_health::{
    after_local_probe, down_body, down_title, escalation_target, gate_applies,
    probe_model_endpoint, AfterLocalProbe, DownReason, LocalDown, LocalGate, LOCAL_RECHECK_AFTER,
};
use crate::theme;
use crate::work::{Mode, WorkExecutor};
use crate::work_run_identity::WorkRunIdentity;

/// The toast while an earlier in-app send still waits for its probe.
pub(crate) const STILL_CHECKING: &str = "Checking the local model \u{2014} try again in a moment";

/// The toast when a new in-app send meets a card that still holds a message.
pub(crate) const CARD_HOLDS_MESSAGE: &str = "The local model isn't answering \u{2014} send, retry \
     or edit the message on the card first. Your new text is back in the composer.";

/// L1e2a-FIX1: the toast when this send would need the card, but another
/// conversation's card still holds a message (it is never replaced).
pub(crate) const OTHER_CARD_HOLDS_MESSAGE: &str = "The local model can't take this message \
     \u{2014} another conversation's card still holds one. Your new text is back in the composer.";

/// The toast when the executor changed while the local model was checked.
pub(crate) const LOCAL_EXECUTOR_CHANGED: &str = "Not sent \u{2014} the executor changed while the \
     local model was checked. Your text is back in the composer.";

/// The test lookup key of the down card.
pub(crate) const LOCAL_DOWN_CARD_SELECTOR: &str = "l1e2-local-down-card";

impl crate::AwiliApp {
    /// The lane `conv_id`'s next send takes: its saved next-turn target, the
    /// one authority the chooser changes. `None` when that target is
    /// unavailable; with no route yet, the ambient executor. A send parked
    /// behind a health probe is judged by this when
    /// the probe answers, never by the process-wide executor.
    pub(crate) fn parked_lane(&self, conv_id: &str) -> Option<WorkExecutor> {
        if self
            .work
            .unavailable_conversation_routes
            .contains_key(conv_id)
        {
            return None;
        }
        self.composer_target_for(&crate::workshop_composer::ModelOwner::Work(
            conv_id.to_string(),
        ))
        // A conversation with no route of its own yet (an unsaved fixture, a
        // thread opened before its first send) takes the ambient lane.
        .or_else(|| Some(self.work.executor.clone()))
    }

    /// The Work submit entry (`dispatch_composer_text`): the in-app lane's
    /// pre-send gate; every other lane goes straight to U2c's
    /// `submit_work_gated`.
    pub(crate) fn submit_work_local_first(&mut self, text: String, cx: &mut Context<Self>) {
        // The lane gates below (local model, fleet gateway) judge the executor
        // this send will use: the shown conversation's next-turn target, not
        // whatever the process-wide executor was left at. Without this, a
        // thread chosen onto Claude is still held behind the in-app model's
        // health check (and a gateway thread behind the wrong lane).
        let here = self.sidebar.active_id().to_string();
        // A saved target this build cannot read is refused before any lane
        // gate parks the text behind a probe: the draft comes back and the
        // thread says what to do.
        if self
            .work
            .unavailable_conversation_routes
            .contains_key(&here)
        {
            let reason = "This conversation's saved next-turn target is unavailable; choose \
                          another target first.";
            self.work.panel.error = Some(reason.into());
            self.refuse_submit(reason.into(), cx);
            return;
        }
        // Only at rest: while a run is live (or its Stop pending) the
        // process-wide executor is that run's, and routes its approvals.
        if !self.work.running && !self.work.gateway_stop_pending {
            if let Some(target) = self.parked_lane(&here) {
                self.work.executor = target;
            }
        }
        if !gate_applies(
            self.work.running,
            self.work.gateway_stop_pending,
            &self.work.executor,
        ) {
            self.submit_work_gated(text, cx);
            return;
        }
        let here = self.sidebar.active_id().to_string();
        self.local_gate_in(text, here, cx);
    }

    /// L1-INT1: the in-app gate for a send pinned to conversation `here`:
    /// the composer's (the active one), or the one a prompt parked behind a
    /// gateway Stop was typed in (`replay_queued_after_stop`, even after the
    /// user moved on). The caller checked `gate_applies`.
    pub(crate) fn local_gate_in(&mut self, text: String, here: String, cx: &mut Context<Self>) {
        // A send already waits for the probe: the new text goes back.
        if self.work.local.health.pending_send.is_some() {
            self.local_hand_back(text, &here, STILL_CHECKING, cx);
            return;
        }
        // A card in THIS conversation still holds a message: never drop it
        // for the new one. (L1e2a-FIX1: a card held for another
        // conversation, which this one cannot see, does not block it.)
        if self
            .work
            .local
            .down
            .as_ref()
            .is_some_and(|d| d.text.is_some() && d.conv_id.as_deref().is_none_or(|c| c == here))
        {
            self.local_hand_back(text, &here, CARD_HOLDS_MESSAGE, cx);
            return;
        }
        // A text-less card (after a failed turn) is replaced by this send;
        // another conversation's card that holds a message is kept.
        if !self.local_card_holds_message() {
            self.work.local.down = None;
        }
        match self.work.local.gate(&self.work.engine, Instant::now()) {
            // U2c's gate never applies to the in-app executor: this is the
            // `submit_work_gated` → `submit_work_run` path, pinned to `here`.
            LocalGate::Send => self.submit_work_run_in(text, here, cx),
            LocalGate::Refuse(reason) => {
                self.show_local_down(
                    LocalDown {
                        reason,
                        text: Some(text),
                        conv_id: Some(here),
                    },
                    cx,
                );
                cx.notify();
            }
            LocalGate::Probe => {
                self.work.local.health.pending_send = Some(PendingSend {
                    text,
                    conv_id: here,
                });
                self.accept_submit(); // R1-INT2: parked behind the local probe
                self.start_local_probe(cx);
                cx.notify();
            }
        }
    }

    /// L1-INT1: give `text` back without sending it: into the composer with
    /// `toast` when the user is in Work in `conv_id`; otherwise to the
    /// clipboard (never into another conversation's composer, U2a-FIX2).
    fn local_hand_back(
        &mut self,
        text: String,
        conv_id: &str,
        toast: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self.mode == Mode::Work && self.sidebar.active_id() == conv_id {
            self.restore_draft(&text, cx);
            self.show_toast(toast, cx);
        } else {
            // RC2-UX (S-O5): it waits in its own conversation's box too.
            self.hand_back_to_conversation(conv_id, &text, cx);
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            self.show_toast(crate::local_model_health::MOVED_ON_NOTICE, cx);
        }
    }

    /// Claim a probe of `[engine] base_url` and run it off-thread (U2c's
    /// `start_gateway_probe` pattern). Nothing when the model is not
    /// configured or a probe is already in flight.
    pub(crate) fn start_local_probe(&mut self, cx: &mut Context<Self>) {
        if !self.work.engine.is_configured() || !self.work.local.health.begin_probe() {
            return;
        }
        let url = self.work.engine.base_url.clone();
        // L1e2a-FIX1: the answer is tied to the endpoint it checked.
        let gen = self.work.local.endpoint_gen;
        let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);
        // Test seam: the gpui test scheduler rejects wake-ups from foreign
        // threads, so a test hands the probe's result over on this thread.
        #[cfg(test)]
        if let Some(result) = TEST_LOCAL_PROBE_RESULT.with(|slot| slot.borrow_mut().take()) {
            let _ = tx.try_send(result);
            return self.deliver_local_probe(gen, rx, cx);
        }
        std::thread::spawn(move || {
            let _ = tx.send_blocking(probe_model_endpoint(&url, PROBE_TIMEOUT));
        });
        self.deliver_local_probe(gen, rx, cx);
    }

    /// Hand the probe's result to [`Self::on_local_probe`] on the UI thread.
    fn deliver_local_probe(
        &mut self,
        gen: u64,
        rx: async_channel::Receiver<Result<(), String>>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| this.on_local_probe(gen, result, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// The local probe finished: record it, then resolve the parked send by
    /// where the user is now, or (after a failed in-app turn) show the
    /// text-less card when the model is down.
    pub(crate) fn on_local_probe(
        &mut self,
        gen: u64,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        // L1e2a-FIX1: an answer about an older `[engine] base_url` says
        // nothing about the current one. It is dropped (a failed turn's
        // check waits for an answer about the current endpoint), and a
        // parked send in the same place goes through the gate again (a
        // fresh probe of the current endpoint, or the not-set-up card).
        let current = gen == self.work.local.endpoint_gen;
        // RC2-RUN (round 3 Minor 12): a check made on its own (entering Work,
        // a launch) that misses is not yet "not answering" — a busy or
        // loading model can miss one 3 s check. It is made once more after
        // [`LOCAL_RECHECK_AFTER`]; a send or a failed turn never waits.
        if current
            && result.is_err()
            && self.work.local.health.pending_send.is_none()
            && !self.work.local.after_failure
            && !std::mem::replace(&mut self.work.local.rechecking, true)
        {
            self.work.local.health.abandon_probe();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(LOCAL_RECHECK_AFTER).await;
                this.update(cx, |app, cx| {
                    if app.work.local.endpoint_gen == gen {
                        app.start_local_probe(cx);
                    }
                })
                .ok();
            })
            .detach();
            cx.notify();
            return;
        }
        if current {
            self.work.local.rechecking = false;
            self.work.local.health.finish_probe(result, Instant::now());
        }
        let after_failure = current && std::mem::take(&mut self.work.local.after_failure);
        // L1e2a-FIX2: a failed turn's check got an older endpoint's answer:
        // check the current endpoint now (nothing when a probe of it is
        // already in flight, or the model is no longer set up).
        if !current && self.work.local.after_failure {
            self.start_local_probe(cx);
        }
        if let Some(p) = self.work.local.health.pending_send.take() {
            let same_place = self.mode == Mode::Work && self.sidebar.active_id() == p.conv_id;
            // The send was parked for the IN-APP lane: if that lane no longer
            // applies (another executor was picked, or a run started), it is
            // never sent through whatever is selected now (U2-INT1's rule).
            // The lane is the parked conversation's saved target: a chooser
            // change while the probe ran (to another harness, or to nothing
            // usable) hands the text back, it is never sent on the new target.
            let in_app = self.parked_lane(&p.conv_id).is_some_and(|lane| {
                gate_applies(self.work.running, self.work.gateway_stop_pending, &lane)
            });
            if same_place && !in_app {
                self.restore_draft(&p.text, cx);
                self.show_toast(LOCAL_EXECUTOR_CHANGED, cx);
                // RC1-O-FIX6: nothing is parked any more.
                self.settle_work_chip();
                self.settle_work_folder(cx);
                cx.notify();
                return;
            }
            if same_place && !current {
                self.submit_work_local_first(p.text, cx);
                cx.notify();
                return;
            }
            let from = p.conv_id.clone();
            match after_local_probe(p, self.work.local.health.reach(), same_place) {
                AfterLocalProbe::Submit(text) => self.submit_work_gated(text, cx),
                AfterLocalProbe::Park(down) => self.show_local_down(down, cx),
                AfterLocalProbe::MovedOn { clipboard, notice } => {
                    // RC2-UX (S-O5): the message waits in the box of the
                    // conversation it was typed in (and on the clipboard).
                    self.hand_back_to_conversation(&from, &clipboard, cx);
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(clipboard));
                    self.show_toast(notice, cx);
                    // RC1-O-FIX6 (whole-slice review P1): the parked prompt
                    // was handed back — the conversation on screen gets its
                    // own lane now.
                    self.settle_work_chip();
                    self.settle_work_folder(cx);
                }
            }
        } else if after_failure {
            if let crate::gateway_health::Reach::Down(detail) = self.work.local.health.reach() {
                let down = LocalDown {
                    reason: DownReason::Unreachable(detail.clone()),
                    text: None,
                    conv_id: self.work.conv_id.clone(),
                };
                self.show_local_down(down, cx);
            }
        }
        cx.notify();
    }

    /// L1e2a-FIX1: whether the card holds a message (in any conversation).
    fn local_card_holds_message(&self) -> bool {
        self.work
            .local
            .down
            .as_ref()
            .is_some_and(|d| d.text.is_some())
    }

    /// L1e2a-FIX1: show `down` on the card, unless the card still holds a
    /// message (in this single slot, from another conversation): that one is
    /// never replaced. The new text, if any, goes back to the composer of
    /// the conversation it was sent from (the current one) with a toast.
    fn show_local_down(&mut self, down: LocalDown, cx: &mut Context<Self>) {
        if !self.local_card_holds_message() {
            // R1-INT2: a card that keeps the message has taken the submit.
            if down.text.is_some() {
                self.accept_submit();
            }
            self.work.local.down = Some(down);
            return;
        }
        if let Some(text) = down.text {
            let conv_id = down
                .conv_id
                .unwrap_or_else(|| self.sidebar.active_id().to_string());
            self.local_hand_back(text, &conv_id, OTHER_CARD_HOLDS_MESSAGE, cx);
        }
    }

    /// The `Failed` arm's hook: a failed in-app turn triggers one check; if
    /// the model is down, the card offers the switch (it never resends).
    pub(crate) fn local_after_failure(&mut self, cx: &mut Context<Self>) {
        if matches!(self.work.executor, WorkExecutor::InApp)
            && matches!(
                self.work.panel.requested_identity,
                Some(WorkRunIdentity::OwnedEngine { .. })
            )
        {
            self.work.local.after_failure = true;
            self.start_local_probe(cx);
        }
    }

    /// "Send with <cloud> instead" / "Switch to <cloud>": the only switch.
    /// A pick like the picker's (persisted, U4f's Ask note), then the kept
    /// message — if any — is sent once through the normal Work path.
    pub(crate) fn local_use_cloud(&mut self, cx: &mut Context<Self>) {
        let target = escalation_target(self.work.local.cloud_escalation.as_deref());
        let down = self.work.local.down.take();
        self.show_toast(
            format!(
                "Work now uses {}. Pick In-app in the executor menu to go back.",
                target.label()
            ),
            cx,
        );
        // After the toast: a NoAsk toast from the pick (U4f) wins.
        self.pick_work_executor(target, cx);
        if let Some(text) = down.and_then(|d| d.text) {
            self.submit_work_gated(text, cx);
        }
        cx.notify();
    }

    /// "Try again": re-probe now (a fresh Down would refuse without one).
    pub(crate) fn local_try_again(&mut self, cx: &mut Context<Self>) {
        let Some(down) = self.work.local.down.take() else {
            return;
        };
        match down.text {
            Some(text) if !self.work.engine.is_configured() => {
                self.submit_work_local_first(text, cx)
            }
            Some(text) => {
                self.work.local.health.pending_send = Some(PendingSend {
                    text,
                    conv_id: self.sidebar.active_id().to_string(),
                });
                self.start_local_probe(cx);
            }
            None => {
                self.work.local.after_failure = true;
                self.start_local_probe(cx);
            }
        }
        cx.notify();
    }

    /// "Edit message": the kept message goes back into the composer.
    pub(crate) fn local_edit_message(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.work.local.down.take().and_then(|d| d.text) {
            self.restore_draft(&text, cx);
        }
        cx.notify();
    }

    /// The down card (above the message box, RC2-UX R2-20): None unless in Work
    /// with a card for this conversation (or one with no conversation).
    pub(crate) fn local_model_down_card(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if self.mode != Mode::Work {
            return None;
        }
        let down = self.work.local.down.as_ref()?;
        if down
            .conv_id
            .as_deref()
            .is_some_and(|c| c != self.sidebar.active_id())
        {
            return None;
        }
        let kept = down.text.is_some();
        let target = escalation_target(self.work.local.cloud_escalation.as_deref());
        let installed = match &target {
            WorkExecutor::Agent { engine, .. } => {
                crate::harness_setup::installed_binary(*engine, &self.work.agents).is_some()
            }
            _ => true,
        };
        let cloud = if !installed {
            pill(
                "local-down-choose",
                "Choose another helper\u{2026}".into(),
                true,
            )
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.work.executor_picker_open = true;
                cx.notify();
            }))
        } else {
            let label = if kept {
                format!("Send with {} instead", target.label())
            } else {
                format!("Switch to {}", target.label())
            };
            pill("local-down-cloud", label, true)
                .on_click(cx.listener(|this, _ev, _w, cx| this.local_use_cloud(cx)))
        };
        let mut buttons = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(theme::z(theme::SPACE_M))
            .child(cloud)
            .child(
                pill("local-down-retry", "Try again".into(), false)
                    .on_click(cx.listener(|this, _ev, _w, cx| this.local_try_again(cx))),
            );
        if kept {
            buttons = buttons.child(
                pill("local-down-edit", "Edit message".into(), false)
                    .on_click(cx.listener(|this, _ev, _w, cx| this.local_edit_message(cx))),
            );
        }
        Some(
            div()
                .debug_selector(|| LOCAL_DOWN_CARD_SELECTOR.to_string())
                .flex()
                .flex_col()
                .gap(theme::z(theme::SPACE_M))
                .p(theme::z(theme::SPACE_L))
                .rounded(theme::z(theme::RADIUS))
                .bg(theme::p().raised)
                .border_1()
                .border_color(theme::p().border)
                .child(
                    div()
                        .text_size(theme::z(theme::LABEL_SIZE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::p().chip_warn)
                        .child(down_title(&down.reason)),
                )
                .child(
                    div()
                        .text_size(theme::z(theme::LABEL_SIZE))
                        .text_color(theme::p().text)
                        .child(SharedString::from(down_body(
                            &down.reason,
                            &self.work.engine,
                            kept,
                        ))),
                )
                .child(buttons),
        )
    }
}

/// A card pill with a dynamic label (`work::pill_button` takes `&'static str`).
fn pill(id: &'static str, label: String, primary: bool) -> gpui::Stateful<gpui::Div> {
    let base = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .px(theme::z(12.))
        .py(theme::z(6.))
        .rounded(theme::z(theme::RADIUS))
        .border_1()
        .border_color(theme::p().accent)
        .text_size(theme::z(theme::LABEL_SIZE))
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|s| s.border_color(theme::p().accent))
        .child(SharedString::from(label));
    if primary {
        base.bg(theme::p().accent).text_color(theme::p().on_accent)
    } else {
        base.text_color(theme::p().text)
    }
}

#[cfg(test)]
#[path = "local_down_wording_tests.rs"]
mod r2d_tests;

#[cfg(test)]
thread_local! {
    /// One-shot probe result for the next `start_local_probe` (tests only):
    /// taken instead of spawning the blocking probe thread.
    pub(crate) static TEST_LOCAL_PROBE_RESULT: std::cell::RefCell<Option<Result<(), String>>> =
        const { std::cell::RefCell::new(None) };
}
