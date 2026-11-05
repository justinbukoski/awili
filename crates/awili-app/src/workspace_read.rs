//! One real Work diagnostic; no external selftest command or suite replay.
use crate::{work::WorkExecutor, workspace_read_fixture::Challenge, AwiliApp, PersistCmd};
use awili_selftest::app_observation::{workspace_read_outcome, Correlation};
use awili_selftest::evaluate::Outcome;
use gpui::{prelude::*, Context, SharedString, Window};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
#[cfg(test)]
#[path = "workspace_read_tests.rs"]
mod tests;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct WorkspaceRead {
    pub active: Option<Diagnostic>,
    pub notice: String,
    pub report: Option<PathBuf>,
    pub reporting: bool,
}

pub(crate) struct Diagnostic {
    pub key: Correlation,
    fixture: Challenge,
    request: String,
    answer: Option<String>,
    success: bool,
    tool_attempt_failed: bool,
    done: bool,
    stream_closed: bool,
    pub cancelled: bool,
    pub terminated: Arc<AtomicBool>,
    reading: bool,
    started: Instant,
    executor: String,
    /// RC2-APP (F0317): the run sequence the owner's Stop moved this run to
    /// (an agent Stop bumps `run_seq` and then finishes the stopped turn
    /// under the new number). Only that exact number may still supply the
    /// answer — never a later run's.
    stop_seq: Option<u64>,
    /// RC2-APP (F0316): the diagnostic's own deadline passed without the
    /// evidence it waits for; it is written up as not passed.
    timed_out: bool,
}

impl Diagnostic {
    fn matches(&self, conv: &str, seq: u64) -> bool {
        self.key.conversation == conv && self.key.run_seq == seq
    }
    fn observe_end(&mut self, conv: &str, seq: u64, done: bool) {
        if self.matches(conv, seq) {
            self.stream_closed = true;
            self.done = done;
        }
    }
    fn capture_terminal(
        &mut self,
        conv: &str,
        seq: u64,
        answer: Option<String>,
        success: bool,
        warning: bool,
    ) {
        // Freeze the original answer/status once. A later ordinary Work turn
        // (including a reused gateway epoch) must never relabel this evidence.
        if self.answer.is_some()
            || self.key.conversation != conv
            || !(self.key.run_seq == seq || self.stop_seq == Some(seq))
        {
            return;
        }
        self.answer = Some(answer.unwrap_or_default());
        self.success = success;
        self.tool_attempt_failed = warning;
    }

    /// RC2-APP (F0317): the owner's Stop moved this cancelled run from its
    /// own number to `new_seq` (the next one); that number — and only that
    /// one — may still finish it.
    fn note_stop_epoch(&mut self, conv: &str, new_seq: u64) {
        if self.cancelled
            && self.key.conversation == conv
            && self.key.run_seq.wrapping_add(1) == new_seq
        {
            self.stop_seq = Some(new_seq);
        }
    }

    /// RC2-APP (F0316): the deadline passed. The diagnostic stops waiting
    /// for evidence that has not come and is written up as not passed
    /// (cancelled or timed out), so it never holds the slot forever.
    fn time_out(&mut self) {
        self.timed_out = true;
        self.cancelled = true;
    }

    fn ready_for_readback(&self) -> bool {
        !self.reading
            && (self.timed_out
                || (self.answer.is_some()
                    && self.stream_closed
                    && self.terminated.load(Ordering::Acquire)))
    }
}

impl AwiliApp {
    pub(crate) fn workspace_read_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::ui::{text, Button, Card};
        let busy = self.workspace_read.active.is_some()
            || self.workspace_read.reporting
            || self.work.running;
        let mut actions = vec![Button::new("workspace-read-start", "Run diagnostic")
            .disabled_if(busy.then(|| "Finish the current job or diagnostic first".into()))
            .on_click(cx.listener(|app, _, window, cx| app.start_workspace_read(window, cx)))
            .into_any_element()];
        if let Some(path) = self.workspace_read.report.clone() {
            actions.push(
                Button::new("workspace-read-report", "Open report")
                    .on_click(move |_, _, _| {
                        let _ = open::that(&path);
                    })
                    .into_any_element(),
            );
        }
        Card::new("job-workspace-read")
            .title("Workspace read diagnostic")
            .body("Reads a private challenge through the agent to check that its answer was saved.")
            .children(vec![text::ui_text(SharedString::from(
                self.workspace_read.notice.clone(),
            ))
            .dim()
            .wrap()
            .into_any_element()])
            .actions(actions)
    }

    fn start_workspace_read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let refusal = if self.work.running
            || self.workspace_read.active.is_some()
            || self.workspace_read.reporting
        {
            Some("Busy: finish the current Work run or diagnostic first")
        } else if !matches!(self.work.executor, WorkExecutor::Agent { .. }) {
            Some("Unsupported: this diagnostic currently requires an Agent executor")
        } else if self.persist.is_none() {
            Some("Unsupported: configured persistence is required")
        } else if self.work.folder.is_none() || self.onboarding.is_active() {
            Some("Choose a working folder and complete setup first")
        } else {
            None
        };
        if let Some(message) = refusal {
            self.workspace_read.notice = message.into();
            cx.notify();
            return;
        }
        let folder = self.work.folder.clone().unwrap();
        let fixture = match Challenge::create(&folder) {
            Ok(fixture) => fixture,
            Err(_) => {
                self.workspace_read.notice = "Could not create a private challenge file".into();
                cx.notify();
                return;
            }
        };
        let request = fixture.request();
        self.new_conversation_kind(awili_core::ConvKind::Work, window, cx);
        let key = Correlation {
            fixture_id: fixture.id.clone(),
            conversation: self.sidebar.active_id().to_string(),
            run_seq: self.work.run_seq.wrapping_add(1),
        };
        self.workspace_read.active = Some(Diagnostic {
            key: key.clone(),
            fixture,
            request: request.clone(),
            answer: None,
            success: false,
            tool_attempt_failed: false,
            done: false,
            stream_closed: false,
            cancelled: false,
            terminated: Arc::new(AtomicBool::new(false)),
            reading: false,
            started: Instant::now(),
            executor: self.work.executor.to_persist(),
            stop_seq: None,
            timed_out: false,
        });
        self.workspace_read.notice = "Running workspace read diagnostic".into();
        self.workspace_read.report = None;
        self.submit_work_run(request, cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(180)).await;
            let _ = this.update(cx, |app, cx| {
                let matches = app.workspace_read.active.as_ref().is_some_and(|d| d.key == key);
                if matches && app.work.run_seq == key.run_seq && app.work.running {
                    app.cancel_work_run(cx);
                }
            });
            cx.background_executor().timer(Duration::from_secs(10)).await;
            let _ = this.update(cx, |app, cx| {
                // RC2-APP (F0316): past its deadline the diagnostic is
                // written up as not passed and the slot is freed, instead
                // of waiting forever for evidence that has not come.
                if let Some(d) = app.workspace_read.active.as_mut().filter(|d| d.key == key) {
                    d.time_out();
                    app.workspace_read.notice = "Diagnostic timed out: termination or persistence not confirmed; challenge retained".into();
                    app.finish_workspace_read(cx);
                    cx.notify();
                }
            });
        }).detach();
    }

    pub(crate) fn workspace_read_observer(&self, conv: &str, seq: u64) -> Option<Arc<AtomicBool>> {
        self.workspace_read
            .active
            .as_ref()
            .filter(|d| d.matches(conv, seq))
            .map(|d| d.terminated.clone())
    }

    pub(crate) fn workspace_read_cancel(&mut self, conv: &str, seq: u64) {
        if let Some(d) = self
            .workspace_read
            .active
            .as_mut()
            .filter(|d| d.matches(conv, seq))
        {
            d.cancelled = true;
            self.workspace_read.notice =
                "Stopped; awaiting termination and evidence. Ordinary Work remains available."
                    .into();
        }
    }

    /// RC2-APP (F0317): the owner's Stop has just moved `run_seq` on to
    /// `new_seq`; a cancelled diagnostic in `conv` accepts its answer under
    /// that exact number only.
    pub(crate) fn workspace_read_stop_epoch(&mut self, conv: &str, new_seq: u64) {
        if let Some(d) = self.workspace_read.active.as_mut() {
            d.note_stop_epoch(conv, new_seq);
        }
    }

    pub(crate) fn workspace_read_provider_end(
        &mut self,
        conv: &str,
        seq: u64,
        done: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(d) = self
            .workspace_read
            .active
            .as_mut()
            .filter(|d| d.matches(conv, seq))
        {
            d.observe_end(conv, seq, done);
        }
        self.finish_workspace_read(cx);
    }

    pub(crate) fn workspace_read_terminal(&mut self, conv: &str, cx: &mut Context<Self>) {
        let Some(d) = self.workspace_read.active.as_mut() else {
            return;
        };
        // RC2-APP (F0318): the reply that just finished — the latest
        // assistant message — not whatever sits at a fixed position.
        let answer = self
            .views
            .get(conv)
            .and_then(|v| latest_assistant_text(&v.convo));
        let success = self.work.panel.error.is_none()
            && self.work.panel.status.is_some_and(|s| s.is_success());
        d.capture_terminal(
            conv,
            self.work.run_seq,
            answer,
            success,
            self.work.panel.tool_attempt_failed,
        );
        self.finish_workspace_read(cx);
    }

    pub(crate) fn finish_workspace_read(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.workspace_read.active.as_mut() else {
            return;
        };
        if !d.ready_for_readback() {
            return;
        }
        d.reading = true;
        let key = d.key.clone();
        let (tx, rx) = async_channel::bounded(1);
        if let Some(p) = &self.persist {
            p.send(PersistCmd::ReadDiagnosticMessages {
                conv: key.conversation.clone(),
                reply: tx,
            });
        }
        cx.spawn(async move |this, cx| {
            let rows = rx.recv().await.ok().flatten();
            let (done_tx, done_rx) = async_channel::bounded(1);
            let _ = this.update(cx, |app, _| {
                if app
                    .workspace_read
                    .active
                    .as_ref()
                    .is_some_and(|d| d.key == key)
                {
                    let diagnostic = app.workspace_read.active.take().unwrap();
                    // Block only another diagnostic while evidence is committed.
                    app.workspace_read.reporting = true;
                    std::thread::spawn(move || {
                        let _ = done_tx.send_blocking(write_result(diagnostic, rows));
                    });
                }
            });
            if let Ok((notice, report)) = done_rx.recv().await {
                let _ = this.update(cx, |app, cx| {
                    app.workspace_read.reporting = false;
                    app.workspace_read.notice = notice;
                    app.workspace_read.report = report;
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

/// RC2-APP (F0318): the text of the conversation's latest assistant message.
fn latest_assistant_text(convo: &awili_core::ConversationState) -> Option<String> {
    convo
        .messages()
        .iter()
        .rev()
        .find(|m| m.role == awili_core::Role::Assistant)
        .map(|m| m.text.clone())
}

fn write_result(d: Diagnostic, rows: Option<Vec<(String, String)>>) -> (String, Option<PathBuf>) {
    let answer = d.answer.as_deref().unwrap_or("");
    let outcome = workspace_read_outcome(
        &d.fixture.nonce,
        &d.request,
        answer,
        d.success && d.done,
        d.cancelled,
        rows.as_deref(),
    );
    let passed = outcome == Outcome::Pass;
    let evidence = awili_selftest::report::clamp_evidence(&awili_selftest::redact::redact(answer));
    let report = serde_json::json!({
        "version":1, "probe_id":"workspace-read", "boundary":"Awili Work Agent",
        "fixture_id":d.key.fixture_id, "conversation":d.key.conversation, "run_seq":d.key.run_seq,
        "challenge_sha256":format!("{:x}", Sha256::digest(d.fixture.nonce.as_bytes())),
        "raw_request_sha256":format!("{:x}", Sha256::digest(d.request.as_bytes())),
        "answer_sha256":format!("{:x}", Sha256::digest(answer.as_bytes())),
        "requested_executor":d.executor, "resolved_model":null, "tokens_in":null, "tokens_out":null,
        "tool_origin":null, "duration_ms":d.started.elapsed().as_millis() as u64,
        "outcome":outcome, "evidence":evidence,
        "app_terminal_success":d.success, "tool_attempt_failed":d.tool_attempt_failed, "cancelled_or_timed_out":d.cancelled,
        "nonce_line_matched":answer.lines().any(|line| line.trim() == d.fixture.nonce),
        "persisted_pair_verified":rows.as_deref() == Some(&[("user".to_string(), d.request.clone()), ("assistant".to_string(), answer.to_string())][..]), "provider_termination_observed":d.terminated.load(Ordering::Acquire), "provider_done_observed":d.done, "timed_out":d.timed_out,
        "challenge_path":d.fixture.path, "cleanup":"retained to protect concurrent workspace changes"
    });
    let dir = dirs::data_local_dir().map(|p| p.join("awili/selftest"));
    let path = dir.and_then(|dir| {
        awili_config::ensure_private_app_dir(&dir).ok()?;
        awili_selftest::report::write_report_new(
            &dir,
            chrono::Utc::now(),
            "app",
            &d.fixture.id,
            &report.to_string(),
        )
        .ok()
    });
    if path.is_none() {
        return (
            "Diagnostic report could not be saved; challenge retained".into(),
            None,
        );
    }
    let cleaned = passed
        && d.fixture
            .cleanup(d.terminated.load(Ordering::Acquire))
            .unwrap_or(false);
    (
        format!(
            "Workspace read: {}. {}",
            if passed {
                if d.tool_attempt_failed {
                    "PASS with tool warnings"
                } else {
                    "PASS"
                }
            } else {
                if d.tool_attempt_failed {
                    "FAIL / ERROR with tool warnings"
                } else {
                    "FAIL / ERROR"
                }
            },
            if cleaned {
                "Owned challenge removed."
            } else {
                "Challenge retained for inspection."
            }
        ),
        path,
    )
}
