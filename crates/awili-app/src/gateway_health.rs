//! U2a — gateway reachability probe and health state (pure core).
//!
//! The fleet gateway (the Studio dashboard behind the SSH forward) is the
//! lane Work submits ride on. This module owns the decision surface for three
//! situations U2 must handle: a Work send that needs to check the gateway
//! first (`send_gate`), an idle app that must re-probe on a timer
//! (`idle_probe_due`), and a live gateway turn that must fail when the link
//! is dead (`run_watch`). All the timing constants are declared here so the
//! app layer and the tests share one source of truth.
//!
//! U2a is the pure core: the U2c app layer
//! (`crates/awili-app/src/gateway_health_app.rs`) wires this into the Work
//! state and the 1 s health tick. Until U2c lands, the public surface is
//! exercised by this module's tests only, so the module is kept out of the
//! production dead-code radar.

#![allow(dead_code)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use crate::work::RunPanel;

/// User-facing copy for a refused send, a timed-out turn and the status
/// segment when the gateway cannot be reached.
/// RC1-F2: plain words for a household member (no "tunnel", no "Studio").
pub(crate) const GATEWAY_UNREACHABLE: &str =
    "Fleet gateway unreachable — check that this Mac is online and the gateway's computer is on";

/// How long a single probe may take before it counts as failed.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// A probe result older than this must be re-checked before trusting it.
pub(crate) const FRESH_FOR: Duration = Duration::from_secs(20);

/// Idle re-probe cadence while the gateway is the selected Work executor.
pub(crate) const IDLE_PROBE_EVERY: Duration = Duration::from_secs(15);

/// A live gateway turn is probed once it has been silent for this long.
pub(crate) const RUN_PROBE_AFTER_SILENCE: Duration = Duration::from_secs(10);

/// Re-probe cadence for a live gateway turn while it stays silent.
pub(crate) const RUN_PROBE_EVERY: Duration = Duration::from_secs(10);

/// Consecutive failed probes that end a live gateway turn through `Failed`.
pub(crate) const RUN_FAIL_AFTER: u32 = 2;

/// `"{GATEWAY_UNREACHABLE} ({detail})"`, or the bare constant when `detail`
/// is blank.
pub(crate) fn unreachable_message(detail: &str) -> String {
    let detail = detail.trim();
    if detail.is_empty() {
        GATEWAY_UNREACHABLE.to_string()
    } else {
        format!("{GATEWAY_UNREACHABLE} ({detail})")
    }
}

/// Blocking (worker threads only): GET `<url without trailing '/'>/` with a
/// reqwest blocking client built with `.timeout(timeout)`,
/// `.connect_timeout(timeout)` and `.redirect(reqwest::redirect::Policy::none())`
/// so a 3xx is the final answer (reachable per U2a) and is never followed to
/// another host. `Ok(())` on a 2xx/3xx status; otherwise
/// `Err("HTTP <code>")` or `Err(<reqwest error display>)`.
pub(crate) fn probe_dashboard(url: &str, timeout: Duration) -> Result<(), String> {
    let url = format!("{}/", url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(&url).send().map_err(|e| e.to_string())?;
    let status = response.status();
    if (200..300).contains(&status.as_u16()) || (300..400).contains(&status.as_u16()) {
        Ok(())
    } else {
        Err(format!("HTTP {status}"))
    }
}

/// The last known health of the fleet gateway.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Reach {
    /// No probe has finished yet (or the state was reset).
    #[default]
    Unknown,
    /// The last probe succeeded.
    Up,
    /// The last probe failed, with its error detail.
    Down(String),
}

/// What a Work submit should do right now on the gateway lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SendGate {
    /// The last result is fresh and healthy: send.
    Send,
    /// The last result is fresh and unhealthy: refuse with the message.
    Refuse(String),
    /// Nothing fresh to decide on: probe first.
    Probe,
}

/// What a 1 s health tick should do about a live, silent gateway turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RunWatch {
    /// Keep waiting; nothing to do this tick.
    Nothing,
    /// Probe the gateway now.
    Probe,
    /// The turn is dead: fail it with this message.
    Fail(String),
}

/// What to do once a probe behind a parked send has finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AfterProbe {
    /// Gateway is up and the user is still where they sent from.
    Submit(String),
    /// Gateway is down: restore the text with this message.
    Refuse { text: String, message: String },
    /// Still in the same place but the state is unknown: restore the text.
    Restore(String),
    /// The user left Work or changed conversation while the probe ran: the
    /// text is NOT put into the (now different) composer. The full unsent
    /// text goes to the system clipboard (never lost) and `notice` says so.
    MovedOn { clipboard: String, notice: String },
}

/// A Work send parked behind a gateway probe.
///
/// Read by the U2c app layer (`gateway_health_app`) once the probe behind it
/// finishes; U2a's own tests exercise it through [`after_probe`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingSend {
    /// The composer text that will be sent once the probe says the gateway is up.
    pub(crate) text: String,
    /// The conversation the send was made in; a different one means the user moved.
    pub(crate) conv_id: String,
}

/// The per-app gateway health state (U2a core; U2c embeds it in `WorkState`
/// and reads `pending_send` when the probe behind a parked send finishes).
#[derive(Debug, Default)]
pub(crate) struct GatewayHealth {
    reach: Reach,
    checked_at: Option<Instant>,
    probing: bool,
    failures: u32,
    /// A send waiting on an in-flight probe, if any (read by the U2c app layer).
    pub(crate) pending_send: Option<PendingSend>,
}

impl GatewayHealth {
    /// The last known health of the gateway.
    pub(crate) fn reach(&self) -> &Reach {
        &self.reach
    }

    /// Whether a probe is currently in flight.
    pub(crate) fn probing(&self) -> bool {
        self.probing
    }

    /// Claim a probe. Returns `false` (and changes nothing) when a probe is
    /// already in flight.
    pub(crate) fn begin_probe(&mut self) -> bool {
        if self.probing {
            return false;
        }
        self.probing = true;
        true
    }

    /// RC2-RUN (round 3 Minor 12): the probe in flight ended without a
    /// result worth recording (a local check that will be made again):
    /// `probing = false`, nothing else changes.
    pub(crate) fn abandon_probe(&mut self) {
        self.probing = false;
    }

    /// Record a finished probe: `probing = false`, `checked_at = now`;
    /// `Ok(())` → `Up` with `failures = 0`, `Err(e)` → `Down(e)` with
    /// `failures += 1` (saturating).
    pub(crate) fn finish_probe(&mut self, result: Result<(), String>, now: Instant) {
        self.probing = false;
        self.checked_at = Some(now);
        match result {
            Ok(()) => {
                self.reach = Reach::Up;
                self.failures = 0;
            }
            Err(e) => {
                self.reach = Reach::Down(e);
                self.failures = self.failures.saturating_add(1);
            }
        }
    }

    /// Whether the last probe result is still within [`FRESH_FOR`].
    pub(crate) fn is_fresh(&self, now: Instant) -> bool {
        self.checked_at
            .map(|checked| now.saturating_duration_since(checked) < FRESH_FOR)
            .unwrap_or(false)
    }

    /// Decide a Work submit on the gateway lane: a fresh `Up` sends, a fresh
    /// `Down(e)` refuses with the unreachable message, anything else (Unknown,
    /// stale, or a probe in flight) probes first — a probe in flight never
    /// gets to decide a send on its own.
    pub(crate) fn send_gate(&self, now: Instant) -> SendGate {
        if self.probing || !self.is_fresh(now) {
            return SendGate::Probe;
        }
        match &self.reach {
            Reach::Up => SendGate::Send,
            Reach::Down(detail) => SendGate::Refuse(unreachable_message(detail)),
            Reach::Unknown => SendGate::Probe,
        }
    }

    /// Whether an idle re-probe is due: no probe in flight and either never
    /// checked or the last check is at least [`IDLE_PROBE_EVERY`] old.
    pub(crate) fn idle_probe_due(&self, now: Instant) -> bool {
        if self.probing {
            return false;
        }
        match self.checked_at {
            None => true,
            Some(checked) => now.saturating_duration_since(checked) >= IDLE_PROBE_EVERY,
        }
    }

    /// Decide the health tick for a live gateway turn that has been silent
    /// for `silence`. Two or more consecutive failed probes fail the turn;
    /// after [`RUN_PROBE_AFTER_SILENCE`] of silence (and on
    /// [`RUN_PROBE_EVERY`] afterwards) it probes; otherwise it waits.
    pub(crate) fn run_watch(&self, now: Instant, silence: Duration) -> RunWatch {
        if self.failures >= RUN_FAIL_AFTER {
            let detail = match &self.reach {
                Reach::Down(detail) => detail.as_str(),
                _ => "",
            };
            return RunWatch::Fail(unreachable_message(detail));
        }
        let stale = self
            .checked_at
            .map(|checked| now.saturating_duration_since(checked))
            .unwrap_or(RUN_PROBE_EVERY);
        if silence >= RUN_PROBE_AFTER_SILENCE && !self.probing && stale >= RUN_PROBE_EVERY {
            RunWatch::Probe
        } else {
            RunWatch::Nothing
        }
    }
}

/// The toast for a parked send the user walked away from: not sent, and the
/// message is on the clipboard. `Down(e)` names the unreachable gateway.
pub(crate) fn moved_on_notice(reach: &Reach) -> String {
    match reach {
        Reach::Down(detail) => format!(
            "Not sent \u{2014} {}. Your message is back in the box of the conversation you wrote it \
             in, and was copied to the clipboard.",
            unreachable_message(detail)
        ),
        _ => "Not sent \u{2014} you moved on before the gateway check finished. Your message is \
              back in the box of the conversation you wrote it in, and was copied to the clipboard."
            .to_string(),
    }
}

/// What to do with a send parked behind a probe: the probe has finished and
/// `reach` is final. `same_place` is checked FIRST: if the user left Work or
/// changed conversation since the send started, the outcome is always
/// `MovedOn`: nothing for the current composer; the app puts the text in the
/// box of the conversation it was typed in (RC2-UX) and on the clipboard, with
/// [`moved_on_notice`].
/// Still in the same place: `Up` submits, `Down(e)` refuses with the
/// unreachable message, `Unknown` restores.
pub(crate) fn after_probe(pending: PendingSend, reach: &Reach, same_place: bool) -> AfterProbe {
    if !same_place {
        return AfterProbe::MovedOn {
            clipboard: pending.text,
            notice: moved_on_notice(reach),
        };
    }
    match reach {
        Reach::Up => AfterProbe::Submit(pending.text),
        Reach::Down(detail) => AfterProbe::Refuse {
            text: pending.text,
            message: unreachable_message(detail),
        },
        Reach::Unknown => AfterProbe::Restore(pending.text),
    }
}

/// Silence tracking from observable run state (no hooks in the event paths).
///
/// U2c's 1 s health tick observes it; U2a's tests drive it directly.
#[derive(Debug, Default)]
pub(crate) struct ActivityWatch {
    last: Option<u64>,
    since: Option<Instant>,
}

impl ActivityWatch {
    /// A new fingerprint (or the first one) restarts the clock; returns how
    /// long the fingerprint has been unchanged.
    pub(crate) fn observe(&mut self, fingerprint: u64, now: Instant) -> Duration {
        if self.last != Some(fingerprint) {
            self.last = Some(fingerprint);
            self.since = Some(now);
        }
        self.since
            .map(|since| now.saturating_duration_since(since))
            .unwrap_or_default()
    }

    /// Forget the current clock (the run ended or is no longer observed).
    pub(crate) fn reset(&mut self) {
        self.last = None;
        self.since = None;
    }

    /// How long the current fingerprint has been unchanged (ZERO when no
    /// fingerprint was ever observed). Used by the U2c status run.
    pub(crate) fn silence(&self, now: Instant) -> Duration {
        self.since
            .map(|since| now.saturating_duration_since(since))
            .unwrap_or_default()
    }
}

/// A cheap fingerprint of the observable run state, so the health tick can
/// tell "visible progress" from silence without hooks in the event paths.
///
/// Hashes (std [`DefaultHasher`]) the lengths and done/running counts of the
/// panel's progress structures, the activity line, the terminal flags and the
/// row text lengths — the things a user actually sees moving.
pub(crate) fn activity_fingerprint(
    panel: &RunPanel,
    row_text_len: usize,
    row_activity_len: usize,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    panel.streamed.len().hash(&mut hasher);
    panel.steps.len().hash(&mut hasher);
    panel
        .steps
        .iter()
        .filter(|s| s.state == crate::work::StepState::Done)
        .count()
        .hash(&mut hasher);
    panel.tool_calls.len().hash(&mut hasher);
    panel
        .tool_calls
        .iter()
        .filter(|t| t.status != crate::work::ToolCallStatus::Running)
        .count()
        .hash(&mut hasher);
    panel.activity.as_deref().hash(&mut hasher);
    panel.output.is_some().hash(&mut hasher);
    panel.status.is_some().hash(&mut hasher);
    panel.approval.is_some().hash(&mut hasher);
    panel.credential.is_some().hash(&mut hasher);
    row_text_len.hash(&mut hasher);
    row_activity_len.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
#[path = "gateway_health_tests.rs"]
mod tests;
