//! U2b — executor status and silence labels (pure).
//!
//! The status run has to name the executor the user actually picked and how
//! that lane is doing (RC1 usability F14: it read "● gateway" with GLM
//! selected, F09: a silent whole-output turn showed nothing). This module is
//! the pure decision surface behind those two segments:
//!
//! - [`subject_for`] picks what the status run describes for the current mode
//!   (Work → the selected Work executor, Code → the active Code tab's
//!   engine/model, anything else → nothing);
//! - [`executor_part`] renders that subject with its health as a status-run
//!   part `(String, gpui::Rgba)` (the same tuple shape `status_run_parts`
//!   produces; the U2c hook extends that function's output with it);
//! - [`idle_part`] / [`fmt_silence`] render the visible-silence label for a
//!   run with no observable activity ([`IDLE_LABEL_AFTER`] and up);
//! - [`merged_draft`] decides what goes back into the composer after a send
//!   the gateway-lane gate refused (the text is restored, never retyped).
//!
//! U2b is pure: it owns no state and spawns no threads, so its tests are the
//! whole proof. The U2c app layer reads `HealthWatch` / `ActivityWatch` and
//! feeds the results of these functions into the status run and the Progress
//! card.

use std::time::Duration;

use awili_code::CodeEngine;
use gpui::Rgba;

use crate::gateway_health::{Reach, GATEWAY_UNREACHABLE};
use crate::theme::{ACCENT, CHIP_WARN, MUTED};
use crate::work::{Mode, WorkExecutor};

/// Which executor the status run describes.
///
/// The subject is the *selected* executor, not the gateway lane: a Work run
/// with GLM picked reads "GLM …", never "gateway". `installed` is the
/// caller's cached `installed_binary(engine).is_some()` answer for that
/// engine (U2c's `HealthWatch::installed` slot), or `None` when not checked
/// yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Subject {
    /// The in-app engine on this Mac (Work, in-app executor selected, L1b).
    InApp,
    /// The fleet gateway (Work, gateway executor selected).
    Gateway,
    /// A vendor coding-agent harness (its own binary, its own OAuth).
    Agent {
        engine: CodeEngine,
        model: Option<String>,
        installed: Option<bool>,
    },
    /// A local Build lane, already resolved to its `Build · <model> on
    /// <ssh_host>` label (or the bare lane name).
    Build { label: String },
}

/// Which executor the status run describes right now:
///
/// - Work → the selected Work executor; a `LocalBuild` uses `build_label`
///   when the caller could resolve it, else the bare "build";
/// - Code → the active Code tab's `(engine, model)`, or `None` without a tab;
/// - every other mode → `None`.
///
/// `installed` is the caller's cached answer for the engine (`None` when not
/// checked yet); it is consulted only for the Agent subject, so the closure
/// is never called for Work/Code modes with no agent subject.
pub(crate) fn subject_for(
    mode: Mode,
    executor: &WorkExecutor,
    code_tab: Option<(CodeEngine, Option<String>)>,
    build_label: Option<String>,
    installed: impl Fn(CodeEngine) -> Option<bool>,
) -> Option<Subject> {
    match mode {
        Mode::Work => Some(match executor {
            WorkExecutor::InApp | WorkExecutor::InAppExtra { .. } => Subject::InApp,
            WorkExecutor::Gateway => Subject::Gateway,
            WorkExecutor::Agent { engine, model } => Subject::Agent {
                engine: *engine,
                model: model.clone(),
                installed: installed(*engine),
            },
            WorkExecutor::LocalBuild { .. } => Subject::Build {
                label: build_label.unwrap_or_else(|| "build".to_string()),
            },
        }),
        Mode::Code => code_tab.map(|(engine, model)| Subject::Agent {
            engine,
            model,
            installed: installed(engine),
        }),
        _ => None,
    }
}

/// The executor's status-run part (the F14 fix lives here):
///
/// - Gateway: `Up` → `"● gateway"` (ACCENT); `Unknown` while a probe is in
///   flight → `"○ checking gateway…"` (MUTED); `Unknown` at rest →
///   `"○ gateway"` (MUTED); `Down` → `"✗ "` + [`GATEWAY_UNREACHABLE`]
///   (CHIP_WARN) — the probe's error detail stays in the toast, the segment
///   stays short.
/// - Agent: `installed = Some(false)` → `"✗ {Label} not installed"`
///   (CHIP_WARN); `Some(true)` → `"● {Label} · {model|default}"` (ACCENT);
///   `None` → `"○ {Label} · {model|default}"` (MUTED).
/// - Build: `"○ {label}"` (MUTED).
pub(crate) fn executor_part(subject: &Subject, reach: &Reach, probing: bool) -> (String, Rgba) {
    match subject {
        // L1b fallback; L1e2 renders the local model's health instead.
        Subject::InApp => ("○ in-app".to_string(), MUTED),
        Subject::Gateway => {
            let (text, color) = match reach {
                Reach::Up => ("● gateway".to_string(), ACCENT),
                Reach::Down(_) => (format!("✗ {GATEWAY_UNREACHABLE}"), CHIP_WARN),
                Reach::Unknown if probing => ("○ checking gateway…".to_string(), MUTED),
                Reach::Unknown => ("○ gateway".to_string(), MUTED),
            };
            (text, color)
        }
        Subject::Agent {
            engine,
            model,
            installed,
        } => {
            let model = model.as_deref().unwrap_or("default");
            let label = engine.label();
            match installed {
                Some(false) => (format!("✗ {label} not installed"), CHIP_WARN),
                Some(true) => (format!("● {label} · {model}"), ACCENT),
                None => (format!("○ {label} · {model}"), MUTED),
            }
        }
        Subject::Build { label } => (format!("○ {label}"), MUTED),
    }
}

/// A Work run with no observable agent activity for this long or more gets
/// the silence label (the F09 fix).
pub(crate) const IDLE_LABEL_AFTER: Duration = Duration::from_secs(20);

/// RC2-RUN (round 6 Minor 1): from this long on, a run that streams says
/// the owner may want to stop it (amber); before, the quiet is the model
/// thinking and reads as plain progress.
pub(crate) const IDLE_WARN_AFTER: Duration = Duration::from_secs(120);

/// How a run's silence reads, from the way its executor delivers the reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Quiet<'a> {
    /// The reply streams as it is written (in-app, gateway, Claude): quiet
    /// is the model thinking.
    Streams,
    /// The whole reply arrives at the end (GLM / DeepSeek).
    WholeReply(&'a str),
    /// Each message arrives when it is written in full (Codex: its CLI
    /// prints a message only once it is complete).
    WholeMessages(&'a str),
}

/// Render a silence duration for the user: `"45s"` below two minutes, else
/// minutes and zero-padded seconds (`"4m 05s"`).
pub(crate) fn fmt_silence(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 120 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// The visible-silence part of the status run (and, through the same text,
/// the line under the Progress card):
///
/// - `None` while `silence` is below [`IDLE_LABEL_AFTER`] — 20 s of silence
///   is not yet a stall;
/// - `None` while the run is waiting on the user (an approval or credential
///   prompt) — that silence is expected;
/// - a whole-output engine (e.g. "GLM"): `"no output yet — {label} replies
///   at the end ({fmt})"`, CHIP_WARN;
/// - a whole-message engine (Codex): `"still working — {label} shows each
///   message when it is written ({fmt})"`, MUTED;
/// - a streaming run (RC2-RUN, round 6 Minor 1: no orange "no activity"
///   while a model is thinking): `"still working — nothing new for {fmt}"`,
///   MUTED, until [`IDLE_WARN_AFTER`]; then `"nothing new for {fmt} — press
///   Stop if it seems stuck"`, CHIP_WARN.
pub(crate) fn idle_part(
    silence: Duration,
    waiting_on_user: bool,
    quiet: Quiet<'_>,
) -> Option<(String, Rgba)> {
    if silence < IDLE_LABEL_AFTER || waiting_on_user {
        return None;
    }
    let fmt = fmt_silence(silence);
    Some(match quiet {
        Quiet::WholeReply(label) => (
            format!("no output yet — {label} replies at the end ({fmt})"),
            CHIP_WARN,
        ),
        Quiet::WholeMessages(label) => (
            format!("still working — {label} shows each message when it is written ({fmt})"),
            MUTED,
        ),
        Quiet::Streams if silence < IDLE_WARN_AFTER => {
            (format!("still working — nothing new for {fmt}"), MUTED)
        }
        Quiet::Streams => (
            format!("nothing new for {fmt} — press Stop if it seems stuck"),
            CHIP_WARN,
        ),
    })
}

/// The draft the composer holds after a refused send: the refused text on its
/// own when the composer is blank (or whitespace), else the refused text on
/// top of what was already there, separated by a blank line — the user never
/// retypes.
pub(crate) fn merged_draft(existing: &str, refused: &str) -> String {
    if existing.trim().is_empty() {
        refused.to_string()
    } else {
        format!("{refused}\n\n{existing}")
    }
}

#[cfg(test)]
#[path = "executor_status_tests.rs"]
mod tests;
