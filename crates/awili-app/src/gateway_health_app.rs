//! U2c — wire the gateway health into the live app.
//!
//! U2a owns the pure health state (`GatewayHealth`, `ActivityWatch`, the
//! timing constants); U2b owns the pure status/silence rendering. This
//! module is the thin app layer between them and the app: it owns the
//! per-app `HealthWatch` state (a `WorkState` field), decides what a 1 s tick
//! may do ([`tick_plan`]), and glues the decisions to the app — the gated
//! Work submit, the off-thread dashboard probe (the `test_hermes_connection`
//! worker-thread + `async_channel` pattern), the refusal that restores the
//! text to the composer, and the status-run / Progress-card segments.
//!
//! No blocking network I/O ever runs on the UI thread; no executor is ever
//! switched or a send re-dispatched by this layer (the no-picking rule).

use std::time::{Duration, Instant};

use awili_code::CodeEngine;
use awili_config::EngineKind;
use gpui::{div, Context, InteractiveElement, IntoElement, ParentElement, Rgba};

use crate::executor_status::merged_draft;
use crate::gateway_health::{
    activity_fingerprint, probe_dashboard, ActivityWatch, GatewayHealth, PROBE_TIMEOUT,
};
use crate::harness_setup::installed_binary;
use crate::work::{gateway_turn_in_flight, Mode, WorkExecutor};
use crate::work_run_identity::WorkRunIdentity;

/// What one 1 s health tick may do, in one place the tests can decide on.
///
/// Computed by [`tick_plan`]; the app's `health_tick` executes each field
/// and nothing else.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TickPlan {
    /// Probe the dashboard now (off-thread).
    pub(crate) probe: bool,
    /// Fail the live gateway turn with this message (the existing `Failed`
    /// arm records the ledger line, retires the worker and clears the
    /// session).
    pub(crate) fail: Option<String>,
    /// Re-check the four `installed_binary` slots.
    pub(crate) refresh_installed: bool,
}

/// Everything U2 keeps per app (a `WorkState` field).
#[derive(Debug, Default)]
pub(crate) struct HealthWatch {
    /// The gateway reachability + parked-send state (U2a core).
    pub(crate) gateway: GatewayHealth,
    /// The visible-silence clock (observed from the run's fingerprint).
    pub(crate) activity: ActivityWatch,
    /// Cached `installed_binary(engine).is_some()`, index =
    /// [`engine_slot`]; `None` = not checked yet.
    pub(crate) installed: [Option<bool>; 4],
    /// When the four `installed` slots were last checked (`None` = never).
    pub(crate) installed_checked_at: Option<Instant>,
    /// Whether the 1 s health ticker is running (idempotency guard).
    pub(crate) ticker_running: bool,
    /// R1e: the Test button asked for the probe in flight; its result is
    /// toasted when it completes.
    pub(crate) test_requested: bool,
}

/// The `installed` slot for a vendor engine: Claude 0, Codex 1, Glm 2,
/// Dsh 3.
pub(crate) fn engine_slot(engine: CodeEngine) -> usize {
    match engine {
        CodeEngine::Claude => 0,
        CodeEngine::Codex => 1,
        CodeEngine::Glm => 2,
        CodeEngine::Dsh => 3,
    }
}

/// The pre-send gate applies ONLY to a fresh gateway-lane Work submit:
/// nothing running, no Stop in flight, the Work executor is the gateway,
/// the gateway is configured, and the configured engine IS the gateway
/// worker. Every existing `submit_work_run_in` early return and the
/// Stop-parking path keep their charge on every other lane.
pub(crate) fn gate_applies(
    running: bool,
    stop_pending: bool,
    executor: &WorkExecutor,
    gateway_enabled: bool,
    engine_kind: EngineKind,
) -> bool {
    !running
        && !stop_pending
        && matches!(executor, WorkExecutor::Gateway)
        && gateway_enabled
        && engine_kind == EngineKind::HermesGateway
}

/// One 1 s tick's decisions, computed from the watch and the run facts:
///
/// - a live gateway turn (`gateway_turn`) always decides through
///   `run_watch`: `Probe` → `probe`, `Fail(m)` → `fail = Some(m)` — a dead
///   turn is probed and failed only while it is still in flight;
/// - otherwise, the selected gateway at rest (`gateway_selected`) probes on
///   the idle cadence when `idle_probe_due`;
/// - `refresh_installed` when the slots were never checked or are at least
///   `IDLE_PROBE_EVERY` old.
///
/// `silence` is the run's visible-silence duration (ZERO when not running)
/// and is only consulted for a live gateway turn.
pub(crate) fn tick_plan(
    h: &HealthWatch,
    now: Instant,
    gateway_turn: bool,
    gateway_selected: bool,
    silence: Duration,
) -> TickPlan {
    let mut plan = TickPlan::default();
    if gateway_turn {
        match h.gateway.run_watch(now, silence) {
            crate::gateway_health::RunWatch::Probe => plan.probe = true,
            crate::gateway_health::RunWatch::Fail(msg) => plan.fail = Some(msg),
            crate::gateway_health::RunWatch::Nothing => {}
        }
    } else if gateway_selected && h.gateway.idle_probe_due(now) {
        plan.probe = true;
    }
    plan.refresh_installed = h
        .installed_checked_at
        .map(|checked| {
            now.saturating_duration_since(checked) >= crate::gateway_health::IDLE_PROBE_EVERY
        })
        .unwrap_or(true);
    plan
}

/// How the run's silence reads (`executor_status::Quiet`): GLM / DeepSeek
/// reply at the end, everything else streams. RC2-CXS: Codex streams over
/// its app server; only when a Codex turn had to run on `codex exec --json`
/// (`codex_exec_fallback`: THIS run's, `RunPanel::codex_exec` — RC2-CXSF)
/// does Codex show each message when it is complete (RC2-RUN).
fn run_quiet(
    panel: &crate::work::RunPanel,
    codex_exec_fallback: bool,
) -> crate::executor_status::Quiet<'static> {
    use crate::executor_status::Quiet;
    match &panel.requested_identity {
        Some(WorkRunIdentity::Agent { engine, .. }) if engine.buffers_whole_output() => {
            Quiet::WholeReply(engine.label())
        }
        Some(WorkRunIdentity::Agent {
            engine: awili_code::CodeEngine::Codex,
            ..
        }) if codex_exec_fallback => Quiet::WholeMessages(awili_code::CodeEngine::Codex.label()),
        _ => Quiet::Streams,
    }
}

impl crate::AwiliApp {
    /// The U2c Work submit entry (called from `dispatch_composer_text` in
    /// Work mode): the pre-send gateway gate decides between sending,
    /// refusing with the text restored to the composer, and parking the text
    /// behind an off-thread probe. Every other lane stays on
    /// `submit_work_run` (spec: the gate is the gateway lane only).
    pub(crate) fn submit_work_gated(&mut self, text: String, cx: &mut Context<Self>) {
        // (1) Everything outside a fresh gateway-lane submit keeps its
        // existing early returns and the Stop-parking path.
        if !gate_applies(
            self.work.running,
            self.work.gateway_stop_pending,
            &self.work.executor,
            self.work.gateway.enabled,
            self.work_engine_kind(),
        ) {
            self.submit_work_run(text, cx);
            return;
        }
        // (2) A send is already parked behind a probe: the text goes back to
        // the composer and the user waits for the outcome.
        if self.work.health.gateway.pending_send.is_some() {
            self.restore_draft(&text, cx);
            self.show_toast("Checking the fleet gateway — try again in a moment", cx);
            return;
        }
        // (3) The gate's decision on this fresh submit.
        match self.work.health.gateway.send_gate(Instant::now()) {
            crate::gateway_health::SendGate::Send => {
                self.submit_work_run(text, cx);
            }
            crate::gateway_health::SendGate::Refuse(msg) => {
                self.refuse_gateway_send(text, msg, cx);
            }
            crate::gateway_health::SendGate::Probe => {
                self.work.health.gateway.pending_send = Some(crate::gateway_health::PendingSend {
                    text,
                    conv_id: self.sidebar.active_id().to_string(),
                });
                self.accept_submit(); // R1a: parked behind the probe
                self.start_gateway_probe(cx);
                cx.notify();
            }
        }
    }

    /// Claim a probe and run the blocking dashboard GET off-thread,
    /// delivering the result through a bounded `async_channel` (the
    /// `test_hermes_connection` pattern).
    pub(crate) fn start_gateway_probe(&mut self, cx: &mut Context<Self>) {
        if !self.work.health.gateway.begin_probe() {
            return;
        }
        let url = self.work.gateway.dashboard_url.clone();
        let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);
        // Test seam: the gpui test scheduler rejects wake-ups from foreign
        // threads, so a test build hands the probe's result over on this
        // thread — the `TEST_PROBE_RESULT` value, else (RC1-G1: the launch
        // and Work-entry checks of tests that set none) "not probed".
        if cfg!(test) {
            let _ = tx.try_send(test_probe_result());
        } else {
            std::thread::spawn(move || {
                let result = probe_dashboard(&url, PROBE_TIMEOUT);
                let _ = tx.send_blocking(result);
            });
        }
        self.deliver_gateway_probe(rx, cx);
    }

    /// Hand the probe's result to [`Self::on_gateway_probe`] on the UI thread.
    fn deliver_gateway_probe(
        &mut self,
        rx: async_channel::Receiver<Result<(), String>>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    this.on_gateway_probe(result, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// The probe behind the parked sends finished: record it, then resolve
    /// whatever send is waiting (if any) by where the user is now.
    fn on_gateway_probe(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        self.work
            .health
            .gateway
            .finish_probe(result, Instant::now());
        if std::mem::take(&mut self.work.health.test_requested) {
            self.show_toast(self.gateway_status_line().0, cx);
        }
        if let Some(p) = self.work.health.gateway.pending_send.take() {
            let same_place = self.mode == Mode::Work && self.sidebar.active_id() == p.conv_id;
            // U2-INT1: the send was parked for the GATEWAY lane. If that lane
            // no longer applies (the user switched executor, or the gateway
            // was disabled, while the probe ran), it is never submitted
            // through whatever is selected now. Still in the same
            // conversation: the text goes back into the composer. Moved on:
            // `after_probe`'s MovedOn below puts it on the clipboard.
            // The lane is the parked conversation's saved target, so a
            // chooser change while the probe ran hands the text back.
            let gateway_lane = self.parked_lane(&p.conv_id).is_some_and(|lane| {
                gate_applies(
                    self.work.running,
                    self.work.gateway_stop_pending,
                    &lane,
                    self.work.gateway.enabled,
                    self.work_engine_kind(),
                )
            });
            if same_place && !gateway_lane {
                self.restore_draft(&p.text, cx);
                self.show_sticky_toast(EXECUTOR_CHANGED_NOTICE, cx);
                cx.notify();
                return;
            }
            let from = p.conv_id.clone();
            match crate::gateway_health::after_probe(
                p,
                self.work.health.gateway.reach(),
                same_place,
            ) {
                crate::gateway_health::AfterProbe::Submit(t) => self.submit_work_run(t, cx),
                crate::gateway_health::AfterProbe::Refuse { text, message } => {
                    self.refuse_gateway_send(text, message, cx)
                }
                crate::gateway_health::AfterProbe::Restore(t) => {
                    self.restore_draft(&t, cx);
                    self.show_sticky_toast(
                        "Not sent — the gateway check gave no answer; your text is back in the \
                         composer",
                        cx,
                    );
                }
                // U2a-FIX2: the user moved on: the current composer belongs to
                // another conversation (or mode), so nothing is put into it.
                // The typed message is never lost: it goes to the clipboard.
                crate::gateway_health::AfterProbe::MovedOn { clipboard, notice } => {
                    // RC2-UX (S-O5): it also waits in the box of the
                    // conversation it was typed in.
                    self.hand_back_to_conversation(&from, &clipboard, cx);
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(clipboard));
                    self.show_sticky_toast(notice, cx);
                }
            }
        }
        cx.notify();
    }

    /// A gateway-lane send refused by the gate or by a failed probe: the
    /// text goes back into the composer (no retyping), the executor picker
    /// opens (another executor is one click away; Awili never picks one),
    /// and the toast carries the unreachable message.
    fn refuse_gateway_send(&mut self, text: String, message: String, cx: &mut Context<Self>) {
        self.restore_draft(&text, cx);
        self.work.executor_picker_open = true;
        self.show_sticky_toast(message, cx);
    }

    /// Put `text` back into the composer, merged over whatever is already
    /// there (U2b's `merged_draft`). R1a: the text is handed back, so the
    /// composer-submit guard has nothing left to restore.
    pub(crate) fn restore_draft(&mut self, text: &str, cx: &mut Context<Self>) {
        self.accept_submit();
        self.composer.update(cx, |c, cx| {
            let merged = merged_draft(c.text(), text);
            c.set_text(&merged, cx);
        });
    }

    /// Called once at the end of `AwiliApp::new`: start the 1 s health
    /// ticker, except under test (no timer from `new`; the app-level tests
    /// start it themselves with [`Self::start_health_ticker`]).
    pub(crate) fn ensure_health_ticker(&mut self, cx: &mut Context<Self>) {
        if !cfg!(test) {
            self.start_health_ticker(cx);
        }
    }

    /// Start the 1 s health ticker if it is not running (the
    /// `ensure_thinking_timer` pattern): a `cx.spawn` loop that times every
    /// second, drives `health_tick`, and stops when the entity is gone.
    fn start_health_ticker(&mut self, cx: &mut Context<Self>) {
        if self.work.health.ticker_running {
            return;
        }
        self.work.health.ticker_running = true;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let still_alive = this
                .update(cx, |app, cx| {
                    app.health_tick(cx);
                })
                .is_ok();
            if !still_alive {
                break;
            }
        })
        .detach();
    }

    /// One 1 s health tick: refresh the visible-silence clock, decide
    /// through `tick_plan`, execute the plan (probe off-thread, refresh the
    /// installed slots, fail a dead gateway turn through the existing
    /// `Failed` arm), and repaint when the run is alive or the reach
    /// changed.
    fn health_tick(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let running = self.work.running;
        // The row the run is streaming into: the run's pinned conversation
        // (falling back to the active one) — its text length and activity
        // line are the observable progress the fingerprint watches.
        let (row_text_len, activity_len) = if running {
            // The run's pinned conversation, falling back to the active one
            // (same rule as `work_conv_id`).
            let conv_id = self
                .work
                .conv_id
                .clone()
                .unwrap_or_else(|| self.sidebar.active_id().to_string());
            let idx = self.work.stream_row_idx;
            let view = self.views.get(&conv_id);
            let text_len = view
                .and_then(|v| idx.and_then(|i| v.convo.message(i)))
                .map(|m| m.text.len())
                .unwrap_or(0);
            let act_len = self
                .work
                .panel
                .activity
                .as_deref()
                .map(|a| a.len())
                .unwrap_or(0);
            (text_len, act_len)
        } else {
            self.work.health.activity.reset();
            (0, 0)
        };
        let silence = if running {
            self.work.health.activity.observe(
                activity_fingerprint(&self.work.panel, row_text_len, activity_len),
                now,
            )
        } else {
            Duration::ZERO
        };
        let reach_before = self.work.health.gateway.reach().clone();
        // U2c-FIX1: a gateway turn whose Stop is pending is never watched.
        // The Stop owns its end (only `Terminal { Stopped }` lifts the
        // barrier), and a `Failed` under the barrier is recorded as evidence
        // without ending anything — so a watchdog failure here would repeat
        // on every tick and set the cancel flag the Stop must not touch.
        let watched_turn = gateway_turn_in_flight(&self.work) && !self.work.gateway_stop_pending;
        let plan = tick_plan(
            &self.work.health,
            now,
            watched_turn,
            matches!(self.work.executor, WorkExecutor::Gateway) && self.work.gateway.enabled,
            silence,
        );
        if plan.probe {
            self.start_gateway_probe(cx);
        }
        if plan.refresh_installed {
            for engine in [
                CodeEngine::Claude,
                CodeEngine::Codex,
                CodeEngine::Glm,
                CodeEngine::Dsh,
            ] {
                self.work.health.installed[engine_slot(engine)] =
                    Some(installed_binary(engine, &self.work.agents).is_some());
            }
            self.work.health.installed_checked_at = Some(now);
        }
        if let Some(msg) = plan.fail {
            // The existing `Failed` arm is the terminal path: it records the
            // ledger line, drops the worker sender and clears the session.
            // No gateway Stop is pending here (`watched_turn` above).
            self.work
                .cancel
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.apply_work_update(crate::work::WorkUpdate::Failed(msg), cx);
            return;
        }
        // The status strip's reads (15 s reachability, 5 s host stats).
        self.strip_tick(cx);
        // RC2-R619: warm the in-app model ahead of the first question.
        self.maybe_warm_inapp(cx);
        // RC2-R620: and a council's members on Awili's tool engine.
        self.maybe_warm_council(cx);
        if running || self.work.health.gateway.reach() != &reach_before {
            cx.notify();
        }
    }

    /// R1e: the one gateway-reachability line every surface shows (the Work
    /// Connection card, the System tab, the Test button's toasts), read from
    /// the U2 watch alone. A probe the Test button asked for reads
    /// "checking…" even over a known state, so the press is visible.
    pub(crate) fn gateway_status_line(&self) -> (&'static str, GatewayTone) {
        let g = &self.work.health.gateway;
        let checking = g.probing()
            && (*g.reach() == crate::gateway_health::Reach::Unknown
                || self.work.health.test_requested);
        gateway_line(g.reach(), checking)
    }

    /// R1e: the Connection card's Test button. With a fleet gateway set up it
    /// runs the U2 probe (the one source of the card, the System tab and the
    /// status run), shows "checking…" in the card and a toast, and toasts the
    /// result when the probe completes ([`Self::on_gateway_probe`]). Without
    /// one, the REST Hermes test runs as before.
    pub(crate) fn test_connection_pressed(&mut self, cx: &mut Context<Self>) {
        if !self.work.gateway.enabled {
            self.test_hermes_connection(cx);
            return;
        }
        if self.onboarding.is_active() {
            return;
        }
        self.work.health.test_requested = true;
        self.start_gateway_probe(cx);
        self.show_toast(self.gateway_status_line().0, cx);
        cx.notify();
    }

    /// R1e: the System tab's notice when the Command Center cannot load (no
    /// Hermes service): see [`command_center_unavailable_text`].
    pub(crate) fn command_center_unavailable_text(&self) -> String {
        command_center_unavailable_text(self.work.gateway.enabled, self.gateway_status_line().0)
    }

    /// R1e: the System tab's notice element for the same case. RC2-R58: in
    /// the words every Hermes page uses, with what the owner can do.
    pub(crate) fn command_center_unavailable_notice(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        use crate::hermes_pages::HermesPage;
        let text = self
            .hermes_page_notice(HermesPage::System)
            .unwrap_or_else(|| self.command_center_unavailable_text());
        div()
            .debug_selector(|| CC_UNAVAILABLE_SELECTOR.to_string())
            .child(self.hermes_page_notice_element(HermesPage::System, text, cx))
            .into_any_element()
    }

    /// The whole status run (`chips_row`): `collect_status_parts` minus its
    /// legacy hermes-probe segment — `status_run_parts` renders it as
    /// "● gateway" / "○ gateway" whatever executor is selected (F14) — plus
    /// the selected executor's honest segment and the silence label.
    pub(crate) fn status_parts_with_executor(&self) -> Vec<(String, Rgba)> {
        let mut parts = self.collect_status_parts();
        parts.retain(|(text, _)| !is_legacy_gateway_part(text));
        parts.extend(self.executor_status_parts());
        parts
    }

    /// The executor status run (U2c outcome 4): the selected executor's
    /// health segment (U2b's `executor_part`), and — while a Work run is in
    /// flight — the visible-silence label (U2b's `idle_part`).
    pub(crate) fn executor_status_parts(&self) -> Vec<(String, Rgba)> {
        use crate::executor_status::{executor_part, subject_for};
        let watch = &self.work.health;
        let code_tab = active_code_tab(self);
        let subject = subject_for(
            self.mode,
            &self.work.executor,
            code_tab,
            self.work
                .executor
                .local_build_label(&self.work.build_targets),
            |e| watch.installed[engine_slot(e)],
        );
        let Some(subject) = subject else {
            return Vec::new();
        };
        // L1e2: the in-app subject shows the local model's health.
        let local = &self.work.local.health;
        let mut parts = vec![if subject == crate::executor_status::Subject::InApp {
            let eff = self.work.effective_engine();
            // The health check watches the primary only; an `[[engines]]` extra
            // is never probed ahead of a turn, so it shows no reach.
            let (reach, probing) = if matches!(self.work.executor, WorkExecutor::InAppExtra { .. })
            {
                (crate::gateway_health::Reach::Unknown, false)
            } else {
                (local.reach().clone(), local.probing())
            };
            crate::local_model_health::in_app_part(
                &eff.display_label(),
                eff.is_configured(),
                &reach,
                probing,
            )
        } else {
            executor_part(&subject, watch.gateway.reach(), watch.gateway.probing())
        }];
        if self.mode == Mode::Work && self.work.running {
            if let Some(part) = crate::executor_status::idle_part(
                watch.activity.silence(Instant::now()),
                self.work_waits_on_owner_or_stop(),
                self.work_run_quiet(),
            ) {
                parts.push(part);
            }
        }
        parts
    }

    /// The visible-silence line under the Progress card (U2c outcome 5):
    /// the same `idle_part` text the status run appends, rendered as a faint
    /// `LABEL_SIZE` row. `None` when not running or below the threshold.
    pub(crate) fn work_idle_line(&self) -> Option<gpui::Div> {
        if !self.work.running {
            return None;
        }
        let (text, color) = crate::executor_status::idle_part(
            self.work.health.activity.silence(Instant::now()),
            self.work_waits_on_owner_or_stop(),
            self.work_run_quiet(),
        )?;
        Some(
            div()
                .debug_selector(|| IDLE_LINE_SELECTOR.to_string())
                .child(crate::ui::label(text).wrap().text_color_override(
                    if color == crate::theme::p().chip_warn {
                        crate::theme::tone_text(crate::theme::Tone::Attention)
                    } else {
                        crate::theme::palette().text_dim
                    },
                )),
        )
    }

    /// How this Work run's silence reads (the status run and the Progress
    /// idle line share it).
    pub(crate) fn work_run_quiet(&self) -> crate::executor_status::Quiet<'static> {
        run_quiet(&self.work.panel, self.work.panel.codex_exec.on_exec())
    }

    /// The run's silence is expected: it waits on the owner (an approval or
    /// credential card), or it is ending after the owner's Deny (RC2-RUN,
    /// R3c-7: the Progress line says so instead).
    fn work_waits_on_owner_or_stop(&self) -> bool {
        let panel = &self.work.panel;
        panel.approval.is_some() || panel.credential.is_some() || self.work.deny_stopped.is_some()
    }
}

#[cfg(test)]
thread_local! {
    /// One-shot probe result for the next `start_gateway_probe` (tests only):
    /// taken instead of spawning the blocking probe thread.
    pub(crate) static TEST_PROBE_RESULT: std::cell::RefCell<Option<Result<(), String>>> =
        const { std::cell::RefCell::new(None) };
}

/// RC1-G1: what a test build's gateway checks report when no test set
/// [`TEST_PROBE_RESULT`]: never a real network call on a real thread.
pub(crate) const GATEWAY_NOT_PROBED_IN_TESTS: &str = "the gateway is not probed in a test build";

/// RC1-G1: a test build's probe result: the `TEST_PROBE_RESULT` seam's
/// one-shot value, else [`GATEWAY_NOT_PROBED_IN_TESTS`] (unreachable).
fn test_probe_result() -> Result<(), String> {
    #[cfg(test)]
    if let Some(result) = TEST_PROBE_RESULT.with(|slot| slot.borrow_mut().take()) {
        return result;
    }
    Err(GATEWAY_NOT_PROBED_IN_TESTS.to_string())
}

/// The toast when a send parked for the gateway meets a different executor
/// (U2-INT1): nothing was sent, the text is back in the composer.
pub(crate) const EXECUTOR_CHANGED_NOTICE: &str =
    "Not sent \u{2014} the executor changed while the gateway was checked. Your text is back in \
     the composer.";

/// R1e: good / bad / unknown — the colour family of a gateway status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GatewayTone {
    Good,
    Bad,
    Unknown,
}

impl GatewayTone {
    /// Connection health follows the kit: connected/checking are neutral;
    /// an unreachable required connection asks for action.
    pub(crate) fn color(self) -> Rgba {
        match self {
            GatewayTone::Good => crate::theme::palette().text_dim,
            GatewayTone::Bad => crate::theme::tone_text(crate::theme::Tone::ActNow),
            GatewayTone::Unknown => crate::theme::palette().text_dim,
        }
    }
}

/// R1e gateway status lines (the unreachable one is U2's
/// [`crate::gateway_health::GATEWAY_UNREACHABLE`], the status run's words).
pub(crate) const GATEWAY_CONNECTED: &str = "Fleet gateway: connected";
pub(crate) const GATEWAY_CHECKING: &str = "Fleet gateway: checking\u{2026}";
pub(crate) const GATEWAY_NOT_CHECKED: &str = "Fleet gateway: not checked yet";

/// R1e: the one gateway status line, from the U2 reach. `checking` wins (a
/// probe the user is waiting on); otherwise Up → connected (good), Down →
/// unreachable with its next step (bad), Unknown → not checked yet.
pub(crate) fn gateway_line(
    reach: &crate::gateway_health::Reach,
    checking: bool,
) -> (&'static str, GatewayTone) {
    use crate::gateway_health::Reach;
    if checking {
        return (GATEWAY_CHECKING, GatewayTone::Unknown);
    }
    match reach {
        Reach::Up => (GATEWAY_CONNECTED, GatewayTone::Good),
        Reach::Down(_) => (crate::gateway_health::GATEWAY_UNREACHABLE, GatewayTone::Bad),
        Reach::Unknown => (GATEWAY_NOT_CHECKED, GatewayTone::Unknown),
    }
}

/// The System tab's notice with nothing set up at all (unchanged wording).
/// RC2-R58 (usability RC2 round 4, Major 5): what it is, that nothing else
/// needs it, and where to connect it.
pub(crate) const CC_NO_GATEWAY: &str = "Hermes is not connected to this Mac, so there is \
      nothing to show here. Chat, Work, Code and Council do not need it. To connect it, put its \
      address in Settings > Advanced > Hermes gateway address.";
/// What the System tab adds after the gateway line when the gateway is set
/// up but the Hermes service the Command Center reads is not. RC2-R58: the
/// same words on every Hermes page, and that nothing else needs it.
pub(crate) const CC_NEEDS_HERMES: &str = "This page reads Hermes's own service, whose address \
      is not set yet: add it with Edit config on the Work tab's Connection card. Chat, Work, \
      Code and Council do not need it.";

/// R1e: the System tab's notice when the Command Center cannot load (no
/// Hermes service). No gateway either → [`CC_NO_GATEWAY`]; a gateway set up
/// → its real state (`line`) and what is missing, never "Connect a gateway".
pub(crate) fn command_center_unavailable_text(gateway_enabled: bool, line: &str) -> String {
    if !gateway_enabled {
        return CC_NO_GATEWAY.to_string();
    }
    let sep = if line.ends_with('\u{2026}') {
        " "
    } else {
        ". "
    };
    format!("{line}{sep}{CC_NEEDS_HERMES}")
}

/// Test lookup keys: the Connection card's gateway line and Test button, the
/// System notice.
pub(crate) const GATEWAY_LINE_SELECTOR: &str = "r1e-gateway-line";
pub(crate) const CC_UNAVAILABLE_SELECTOR: &str = "r1e-cc-unavailable";
pub(crate) const TEST_BUTTON_SELECTOR: &str = "r1e-test-button";

/// The legacy segment `status_run_parts` renders from `hermes_status`.
fn is_legacy_gateway_part(text: &str) -> bool {
    text == "\u{25CF} gateway" || text == "\u{25CB} gateway"
}

/// The test lookup key of the Progress card's silence line.
pub(crate) const IDLE_LINE_SELECTOR: &str = "u2c-idle-line";

/// The active Code tab's `(engine, model)`, `None` when the app is not in
/// Code mode or no tab is active.
fn active_code_tab(app: &crate::AwiliApp) -> Option<(CodeEngine, Option<String>)> {
    if app.mode != Mode::Code {
        return None;
    }
    app.work
        .code_tabs
        .get(app.work.code_active)
        .map(|tab| (tab.engine, tab.model.clone()))
}

#[cfg(test)]
#[path = "gateway_health_app_tests.rs"]
mod tests;
