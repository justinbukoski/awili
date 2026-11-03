//! L1e1 — the in-app model's health: the pure core and the blocking probe.
//!
//! The in-app engine talks to an OpenAI-compatible server (`[engine]
//! base_url`, e.g. the Studio's dedicated mlx_lm). Before an in-app send the
//! app checks `GET <base_url>/v1/models` (mlx_lm answers 404 on `/`, so
//! U2a's dashboard probe does not fit). A down or unconfigured model never
//! sends and never switches executor by itself: the L1e2 card keeps the
//! message and offers one click to the cloud target.
//!
//! The reachability bookkeeping reuses U2a's `GatewayHealth` (reach, the
//! single in-flight probe, freshness, the parked send); its gateway wording
//! is never shown for the local model.

use std::time::{Duration, Instant};

use awili_code::CodeEngine;
use awili_config::EngineConfig;
use gpui::Rgba;

use crate::gateway_health::{GatewayHealth, PendingSend, Reach};
use crate::theme::{ACCENT, CHIP_WARN, MUTED};
use crate::work::WorkExecutor;

/// `"{base_url without trailing '/'}/v1/models"`.
pub(crate) fn models_url(base_url: &str) -> String {
    format!("{}/v1/models", base_url.trim().trim_end_matches('/'))
}

/// Blocking (worker threads only): GET [`models_url`] with a reqwest
/// blocking client built with `.timeout(timeout)`, `.connect_timeout(timeout)`
/// and no redirects. A 2xx is `Ok` unless the server's `/health` answers 503
/// (`Err(NOT_WORKING)`, R7-A3 (RC2)); any other status is `Err("HTTP <code>")`;
/// a transport error is `Err(<error display>)`.
pub(crate) fn probe_model_endpoint(base_url: &str, timeout: Duration) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(models_url(base_url))
        .send()
        .map_err(|e| e.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    // R7-A3 (RC2): `/v1/models` still answers once mlx_lm's
    // generation thread has died (it never touches it); `/health` then says
    // 503. A server without `/health` (404, or no answer) is judged by
    // `/v1/models` alone, as before.
    let broken = client
        .get(health_url(base_url))
        .send()
        .is_ok_and(|r| r.status().as_u16() == 503);
    if broken {
        return Err(NOT_WORKING.to_string());
    }
    Ok(())
}

/// R7-A3 (RC2): `"{base_url without trailing '/'}/health"`.
pub(crate) fn health_url(base_url: &str) -> String {
    format!("{}/health", base_url.trim().trim_end_matches('/'))
}

/// R7-A3 (RC2): why a server that answers is down (its `/health` says 503),
/// in the down card's "didn't answer (…)".
pub(crate) const NOT_WORKING: &str =
    "it is running but says it is not working; it may need a restart";

/// Why the in-app model cannot take a send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DownReason {
    /// `[engine] base_url` or `model` is blank.
    NotConfigured,
    /// The probe failed, with its detail.
    Unreachable(String),
}

/// The down card's state: why, the kept message (None after a failed turn,
/// which keeps nothing to resend), and the conversation it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalDown {
    pub(crate) reason: DownReason,
    pub(crate) text: Option<String>,
    pub(crate) conv_id: Option<String>,
}

/// What a fresh in-app submit does right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LocalGate {
    /// A fresh `Up`: send.
    Send,
    /// Nothing fresh to decide on: park the text and probe first.
    Probe,
    /// Not configured, or a fresh `Down`: nothing is sent.
    Refuse(DownReason),
}

/// What to do with the parked send once the probe finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AfterLocalProbe {
    /// Up, and the user is still where they sent from: send it.
    Submit(String),
    /// Down (or no answer) in the same place: show the card with the text.
    Park(LocalDown),
    /// The user left Work or changed conversation: the text goes to the
    /// clipboard (never into another conversation's composer, U2a-FIX2).
    MovedOn { clipboard: String, notice: String },
}

/// RC2-RUN (round 3 Minor 12): how long after one missed check (made on its
/// own, not for a send) the local model is checked again.
pub(crate) const LOCAL_RECHECK_AFTER: Duration = Duration::from_secs(10);

/// The notice when the user moved on before the local check finished.
pub(crate) const MOVED_ON_NOTICE: &str = "Not sent \u{2014} you moved on before the local model \
     check finished. Your message is back in the box of the conversation you wrote it in, and was \
     copied to the clipboard.";

/// Everything L1e keeps per app (a `WorkState` field).
#[derive(Debug, Default)]
pub(crate) struct LocalModelHealth {
    /// Reach, the single in-flight probe, freshness and the parked send.
    pub(crate) health: GatewayHealth,
    /// The down card, when one is showing.
    pub(crate) down: Option<LocalDown>,
    /// The running probe was started by a failed in-app turn.
    pub(crate) after_failure: bool,
    /// `[work].cloud_escalation`, set from config.
    pub(crate) cloud_escalation: Option<String>,
    /// L1e2a-FIX1: bumped when `[engine] base_url` changes. A probe carries
    /// the value it started under; an answer from an older value is dropped.
    pub(crate) endpoint_gen: u64,
    /// RC2-RUN (round 3 Minor 12): a check made on its own missed once and
    /// is being made again before the status says "not answering".
    pub(crate) rechecking: bool,
}

impl LocalModelHealth {
    /// L1e2a-FIX1: `[engine] base_url` changed. The last result and any
    /// probe in flight describe the old endpoint: forget them (the parked
    /// send stays parked) and bump [`Self::endpoint_gen`]. RC2-RUNF (review
    /// P3): a missed check of the old endpoint is forgotten too, so the new
    /// endpoint's first miss still gets its second check.
    pub(crate) fn endpoint_changed(&mut self) {
        let pending = self.health.pending_send.take();
        self.health = GatewayHealth::default();
        self.health.pending_send = pending;
        self.endpoint_gen = self.endpoint_gen.wrapping_add(1);
        self.rechecking = false;
    }

    /// Not configured → `Refuse(NotConfigured)` (checked first, even when
    /// Up). A probe in flight, a stale or an Unknown result → `Probe`. A
    /// fresh Up → `Send`; a fresh `Down(d)` → `Refuse(Unreachable(d))`.
    pub(crate) fn gate(&self, engine: &EngineConfig, now: Instant) -> LocalGate {
        if !engine.is_configured() {
            return LocalGate::Refuse(DownReason::NotConfigured);
        }
        if self.health.probing() || !self.health.is_fresh(now) {
            return LocalGate::Probe;
        }
        match self.health.reach() {
            Reach::Up => LocalGate::Send,
            Reach::Down(detail) => LocalGate::Refuse(DownReason::Unreachable(detail.clone())),
            Reach::Unknown => LocalGate::Probe,
        }
    }
}

/// The local gate applies only to a fresh in-app submit: nothing running,
/// no gateway Stop in flight, and the in-app executor selected.
pub(crate) fn gate_applies(running: bool, stop_pending: bool, executor: &WorkExecutor) -> bool {
    !running && !stop_pending && matches!(executor, WorkExecutor::InApp)
}

/// The one-click cloud target: `[work].cloud_escalation` when it parses as
/// an `agent:…` executor; otherwise Claude's own CLI (`agent:claude`).
pub(crate) fn escalation_target(configured: Option<&str>) -> WorkExecutor {
    match configured.and_then(WorkExecutor::from_persist) {
        Some(agent @ WorkExecutor::Agent { .. }) => agent,
        _ => WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        },
    }
}

/// Decide the parked send. `same_place` first: moved on → `MovedOn` (the
/// clipboard). Same place: Up → `Submit`; `Down(d)` → `Park` with
/// `Unreachable(d)`; Unknown → `Park` with `Unreachable("no answer")`.
pub(crate) fn after_local_probe(
    pending: PendingSend,
    reach: &Reach,
    same_place: bool,
) -> AfterLocalProbe {
    if !same_place {
        return AfterLocalProbe::MovedOn {
            clipboard: pending.text,
            notice: MOVED_ON_NOTICE.to_string(),
        };
    }
    let detail = match reach {
        Reach::Up => return AfterLocalProbe::Submit(pending.text),
        Reach::Down(detail) => detail.clone(),
        Reach::Unknown => "no answer".to_string(),
    };
    AfterLocalProbe::Park(LocalDown {
        reason: DownReason::Unreachable(detail),
        text: Some(pending.text),
        conv_id: Some(pending.conv_id),
    })
}

/// The card's title.
pub(crate) fn down_title(reason: &DownReason) -> &'static str {
    match reason {
        DownReason::NotConfigured => "No local model is set up",
        DownReason::Unreachable(_) => "The local model isn't answering",
    }
}

/// The card's body: the cause, then what happened to the message.
pub(crate) fn down_body(reason: &DownReason, engine: &EngineConfig, kept_text: bool) -> String {
    let cause = match reason {
        // R2d: plain words for the household; the page that holds the
        // setting really exists (the new Settings has no "All fields").
        DownReason::NotConfigured => "Work is set to run on this Mac, but no local model is \
             set up here yet. Whoever looks after Awili can add one in Settings \u{2192} \
             Advanced."
            .to_string(),
        DownReason::Unreachable(detail) => format!(
            "{} at {} didn't answer ({detail}).",
            engine.display_label(),
            engine.base_url.trim()
        ),
    };
    let tail = if kept_text {
        // R2d: say where the message is and how to get it back.
        " Nothing was sent. Your message is saved on this card: press \u{201C}Edit \
         message\u{201D} to put it back in the box."
    } else {
        " Awili didn't switch to another model."
    };
    format!("{cause}{tail}")
}

/// The in-app status-run segment: not configured → "✗ No local model set
/// up" (CHIP_WARN); Up → "● In-app · {label}" (ACCENT); Down → "✗ Local
/// model not answering" (CHIP_WARN); Unknown while probing → "○ checking
/// local model…" (MUTED); Unknown → "○ In-app · {label}" (MUTED).
pub(crate) fn in_app_part(
    label: &str,
    configured: bool,
    reach: &Reach,
    probing: bool,
) -> (String, Rgba) {
    if !configured {
        return ("\u{2717} No local model set up".to_string(), CHIP_WARN);
    }
    match reach {
        Reach::Up => (format!("\u{25CF} In-app \u{00B7} {label}"), ACCENT),
        Reach::Down(_) => ("\u{2717} Local model not answering".to_string(), CHIP_WARN),
        Reach::Unknown if probing => ("\u{25CB} checking local model\u{2026}".to_string(), MUTED),
        Reach::Unknown => (format!("\u{25CB} In-app \u{00B7} {label}"), MUTED),
    }
}

#[cfg(test)]
#[path = "local_model_health_tests.rs"]
mod tests;
