//! Daily-driver P8b: turn every terminal Work run into one graduation-ledger
//! line (`awili_selftest::graduation`), exactly once per run.
//!
//! Pure except for [`Recorder::observe`]'s single append. The app calls
//! `observe` from every terminal path; the once-per-`run_seq` guard makes
//! duplicate calls harmless, and a write failure is reported as a string the
//! app shows in the panel — it never fails or retries the run.
//!
//! Source spec: `docs/daily-driver/specs/P8b-app-ledger-append.md`.

use std::collections::HashMap;
use std::path::PathBuf;

use awili_selftest::graduation::{Ledger, LedgerEntry, Outcome, LEDGER_VERSION};
use awili_work::run_events::RunStatus;
use chrono::{DateTime, SecondsFormat, Utc};

use crate::work::{expand_home, status_label, RunPanel};
use crate::work_run_identity::WorkRunIdentity;

/// Whether a run's tokens count against the frontier budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// Vendor CLI or the Hermes gateway: frontier tokens.
    Frontier,
    /// A local Build binding: local tokens.
    Local,
    /// The owned engine: the model's home is not known here; both counts 0.
    Unknown,
}

/// The executor/model/class a run identity resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Classified {
    pub(crate) executor: String,
    pub(crate) model: String,
    pub(crate) class: Class,
}

/// Resolve `identity` (and the panel's detected model, which wins when
/// present) to the ledger's executor / model / class triple.
pub(crate) fn classify(
    identity: Option<&WorkRunIdentity>,
    resolved_model: Option<&str>,
) -> Classified {
    let (executor, model, class) = match identity {
        Some(WorkRunIdentity::Agent { engine, model }) => (
            engine.label().to_ascii_lowercase(),
            model.clone().unwrap_or_else(|| "default".to_string()),
            Class::Frontier,
        ),
        Some(WorkRunIdentity::HermesGateway) => (
            "gateway".to_string(),
            "default".to_string(),
            Class::Frontier,
        ),
        Some(WorkRunIdentity::OwnedEngine { model, .. }) => {
            ("owned".to_string(), model.clone(), Class::Unknown)
        }
        Some(WorkRunIdentity::LocalBuild { model, host }) => (
            "local-build".to_string(),
            format!("{model}@{host}"),
            Class::Local,
        ),
        None => ("unknown".to_string(), "unknown".to_string(), Class::Unknown),
    };
    let model = match resolved_model {
        Some(m) if !m.trim().is_empty() => m.to_string(),
        _ => model,
    };
    Classified {
        executor,
        model,
        class,
    }
}

/// What the recorder remembers about the previous terminal run of a
/// conversation: enough to name a frontier retry as a fallback.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PriorRun {
    pub(crate) class: Class,
    pub(crate) executor: String,
    pub(crate) model: String,
    pub(crate) status: RunStatus,
    pub(crate) error: Option<String>,
}

/// Everything one ledger line is built from.
pub(crate) struct RunRecord<'a> {
    pub(crate) conv_id: &'a str,
    /// The recorder's per-arm token (not the app's `run_seq`, which is not
    /// minted per turn, and not a counter, which restarts with the app).
    pub(crate) turn_token: &'a str,
    pub(crate) panel: &'a RunPanel,
    pub(crate) reported_cost_usd: Option<f64>,
    pub(crate) previous: Option<&'a PriorRun>,
}

/// P8d: map one ledger line to the prior run it tells us about.
///
/// `task_id` is the CLI-shaped `work:<conv>:<token>`: the conversation is
/// everything between the leading `work:` and the LAST `:` (the token
/// itself contains no `:`). Entries whose id has no `work:` prefix or no
/// token are not app writes and are skipped.
pub(crate) fn prior_run_from_entry(entry: &LedgerEntry) -> Option<(String, PriorRun)> {
    let (conv, _) = entry.task_id.strip_prefix("work:")?.rsplit_once(':')?;
    if conv.is_empty() {
        return None;
    }
    let class = match entry.executor.as_str() {
        "local-build" => Class::Local,
        "owned" | "unknown" => Class::Unknown,
        _ => Class::Frontier,
    };
    let (status, error) = match &entry.outcome {
        Outcome::Success => (RunStatus::Completed, None),
        Outcome::Stopped => (RunStatus::Stopped, Some("stopped by user".to_string())),
        // Legacy: pre-F16 ledgers wrote a stop as Failed{"stopped by user"}.
        // Keep reading those as Stopped so old ledgers still seed correctly.
        Outcome::Failed { reason } if reason == "stopped by user" => {
            (RunStatus::Stopped, Some(reason.clone()))
        }
        Outcome::Failed { reason } => (RunStatus::Failed, Some(reason.clone())),
        Outcome::Fallback { reason } => (RunStatus::Failed, Some(reason.clone())),
    };
    Some((
        conv.to_string(),
        PriorRun {
            class,
            executor: entry.executor.clone(),
            model: entry.model.clone(),
            status,
            error,
        },
    ))
}

/// The run's terminal status: the panel's, else `Failed` when an error is
/// set, else `Completed`. `None` for a run that has not ended.
pub(crate) fn terminal_status(panel: &RunPanel) -> Option<RunStatus> {
    match panel.status {
        Some(s @ (RunStatus::Completed | RunStatus::Failed | RunStatus::Stopped)) => Some(s),
        Some(_) => None,
        None if panel.error.is_some() => Some(RunStatus::Failed),
        None => Some(RunStatus::Completed),
    }
}

/// RC1-F4b: the in-app engine counts as local when its address is at home
/// ([`crate::engine_locality::engine_is_local`], decided once per config).
fn local_owned(who: Classified, owned_local: bool) -> Classified {
    match who.class {
        Class::Unknown if owned_local && who.executor == "owned" => Classified {
            class: Class::Local,
            ..who
        },
        _ => who,
    }
}

/// Build the ledger entry for a terminal run. Caller guarantees
/// [`terminal_status`] is `Some`; a non-terminal panel is recorded as
/// `Failed { "terminal status <s>" }` rather than dropped.
pub(crate) fn to_entry(record: &RunRecord<'_>, now: DateTime<Utc>) -> LedgerEntry {
    to_entry_with(record, now, false)
}

/// [`to_entry`] with RC1-F4b's in-app locality.
pub(crate) fn to_entry_with(
    record: &RunRecord<'_>,
    now: DateTime<Utc>,
    owned_local: bool,
) -> LedgerEntry {
    let panel = record.panel;
    let who = local_owned(
        classify(panel.requested_identity.as_ref(), panel.model.as_deref()),
        owned_local,
    );
    let status = terminal_status(panel)
        .or(panel.status)
        .unwrap_or(RunStatus::Unknown);
    let error_text = || {
        panel
            .error
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(str::to_string)
    };
    let outcome = match status {
        RunStatus::Completed => match record.previous {
            Some(prev)
                if who.class == Class::Frontier
                    && prev.class == Class::Local
                    && matches!(prev.status, RunStatus::Failed | RunStatus::Stopped) =>
            {
                Outcome::Fallback {
                    reason: format!(
                        "after local {}/{} {}: {}",
                        prev.executor,
                        prev.model,
                        status_label(prev.status),
                        prev.error.as_deref().unwrap_or("no error text")
                    ),
                }
            }
            _ => Outcome::Success,
        },
        RunStatus::Failed => Outcome::Failed {
            reason: error_text().unwrap_or_else(|| "failed without error text".to_string()),
        },
        RunStatus::Stopped => Outcome::Stopped,
        other => Outcome::Failed {
            reason: format!("terminal status {}", status_label(other)),
        },
    };
    let total = panel
        .usage
        .map(|u| u64::from(u.input_tokens).saturating_add(u64::from(u.output_tokens)));
    let (frontier_tokens, local_tokens) = match (who.class, total) {
        (Class::Frontier, Some(t)) => (t, 0),
        (Class::Local, Some(t)) => (0, t),
        _ => (0, 0),
    };
    let notes = match (total, record.reported_cost_usd, who.class) {
        (Some(_), _, _) | (None, _, Class::Unknown) => None,
        (None, Some(cost), _) => Some(format!("cost only: ${cost:.4}")),
        (None, None, _) => Some(format!("tokens not reported by {}", who.executor)),
    };
    LedgerEntry {
        v: LEDGER_VERSION,
        recorded_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        task_id: format!("work:{}:{}", record.conv_id, record.turn_token),
        executor: who.executor,
        model: who.model,
        outcome,
        frontier_tokens,
        local_tokens,
        notes,
    }
}

/// Per-app recorder state: the ledger path, the once-per-turn guard, the
/// current turn's reported cost and each conversation's previous terminal run.
///
/// The guard is *arming*, not `run_seq`: the app arms the recorder at every
/// Work submission and the first terminal observation after that disarms it.
/// `run_seq` is not minted for every turn (a persistent gateway worker keeps
/// its sequence across prompts, Stop bumps it before the terminal event), so
/// it cannot identify a turn; the recorder's own `turn` counter does.
#[derive(Debug, Default)]
pub(crate) struct Recorder {
    path: Option<PathBuf>,
    armed: bool,
    turn: u64,
    /// Random token minted at `arm()`; makes `task_id` unique across app
    /// restarts (the turn counter alone restarts at zero).
    turn_token: String,
    /// The conversation the armed turn was submitted under; the line is
    /// attributed to it even if the app has since pinned another one.
    armed_conv: String,
    cost: Option<f64>,
    last_terminal: HashMap<String, PriorRun>,
    /// RC1-F4b: the configured in-app engine is at home (its tokens are local).
    owned_local: bool,
    /// The last append failure, for the panel; cleared by the next success.
    pub(crate) last_error: Option<String>,
}

#[cfg(test)]
impl Recorder {
    /// The conversation's remembered prior run (test accessor for the
    /// S5(b) `last_terminal` semantics).
    pub(crate) fn last_terminal_of(&self, conv_id: &str) -> Option<&PriorRun> {
        self.last_terminal.get(conv_id)
    }
}

impl Recorder {
    /// A recorder for the configured ledger (`""` disables; `~` expanded).
    #[cfg(test)]
    pub(crate) fn from_config(ledger: &str) -> Self {
        let mut recorder = Self::default();
        recorder.set_ledger(ledger);
        recorder
    }

    /// RC1-F4b: the configured in-app engine address (`[engine] base_url`).
    pub(crate) fn set_engine_url(&mut self, base_url: &str) {
        self.owned_local = crate::engine_locality::engine_is_local(base_url);
    }

    /// Re-point the ledger (config re-apply). Guard state is kept.
    pub(crate) fn set_ledger(&mut self, ledger: &str) {
        let ledger = ledger.trim();
        self.path = (!ledger.is_empty()).then(|| PathBuf::from(expand_home(ledger)));
    }

    /// The ledger path, `None` when recording is disabled.
    #[cfg(test)]
    pub(crate) fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// P8d: seed the per-conversation prior-run map from the ledger, so a
    /// frontier retry of a local failure that happened before the last
    /// restart is classified `Fallback` instead of `Success`.
    ///
    /// Disabled (`None` path) and missing-file cases return `Ok(0)` and
    /// change nothing. A read error is returned as its display string; the
    /// map is left untouched. Otherwise the file is walked in order and each
    /// entry's conversation is inserted, later entries replacing earlier
    /// ones; the number of conversations seeded is returned.
    pub(crate) fn seed_from_ledger(&mut self) -> Result<usize, String> {
        let Some(path) = self.path.clone() else {
            return Ok(0);
        };
        if !path.exists() {
            return Ok(0);
        }
        let entries = Ledger::read(&path).map_err(|e| format!("{e:#}"))?;
        let mut seeded: HashMap<String, PriorRun> = HashMap::new();
        for entry in &entries {
            if let Some((conv, prior)) = prior_run_from_entry(entry) {
                seeded.insert(conv, prior);
            }
        }
        let count = seeded.len();
        self.last_terminal.extend(seeded);
        Ok(count)
    }

    /// A new Work turn was submitted: the next terminal observation records
    /// it. Arming twice without a terminal in between forfeits the first
    /// turn's line, which the app's `running` guard prevents.
    pub(crate) fn arm(&mut self, conv_id: &str) {
        self.armed = true;
        self.turn = self.turn.wrapping_add(1);
        self.turn_token = uuid::Uuid::new_v4().simple().to_string();
        self.armed_conv = conv_id.to_string();
        self.cost = None;
    }

    /// The short run id of the latest armed turn: the first 8 characters of
    /// its token, so the ledger `task_id`, the log line, the error card and
    /// Inspect reply all carry the same id. Empty before any turn was armed.
    pub(crate) fn run_id(&self) -> String {
        crate::run_error::run_id_of_token(&self.turn_token)
    }

    /// Whether a submitted turn is still waiting for its terminal line.
    pub(crate) fn is_armed(&self) -> bool {
        self.armed
    }

    /// The conversation the armed turn was submitted under (`""` when no
    /// turn was ever armed or the last append consumed it). P2e reads it
    /// before `observe` so the last run is persisted under the turn's own
    /// conversation, never the one the app has since pinned.
    pub(crate) fn armed_conv(&self) -> &str {
        &self.armed_conv
    }

    /// Remember the gateway's reported dollar cost for the armed turn.
    pub(crate) fn note_cost(&mut self, cost_usd: f64) {
        if self.armed {
            self.cost = Some(cost_usd);
        }
    }

    /// RC1-H1-FIX1: the line the armed turn would be booked as, built without
    /// touching the ledger (`None` when not armed or not terminal). The Work
    /// footer reads it, so an in-app reply gets its footer whether recording
    /// is on or the append succeeds. Call it before [`Self::observe`], which
    /// disarms.
    pub(crate) fn preview(&self, panel: &RunPanel, now: DateTime<Utc>) -> Option<LedgerEntry> {
        if !self.armed {
            return None;
        }
        terminal_status(panel)?;
        let conv_id = self.armed_conv.as_str();
        let record = RunRecord {
            conv_id,
            turn_token: &self.turn_token,
            panel,
            reported_cost_usd: self.cost,
            previous: self.last_terminal.get(conv_id),
        };
        Some(to_entry_with(&record, now, self.owned_local))
    }

    /// Record the armed turn if `panel` is terminal. Returns `None` when
    /// nothing was to be done (disabled, not armed, not terminal),
    /// `Some(Ok(entry))` after a successful append, `Some(Err(text))` when
    /// the append failed. A terminal observation disarms the turn whether or
    /// not recording is enabled, so a run that ended while the ledger was off
    /// can never be flushed later under another conversation.
    pub(crate) fn observe(
        &mut self,
        panel: &RunPanel,
        now: DateTime<Utc>,
    ) -> Option<Result<LedgerEntry, String>> {
        if !self.armed {
            return None;
        }
        let status = terminal_status(panel)?;
        let Some(path) = self.path.clone() else {
            self.armed = false;
            self.cost = None;
            return None;
        };
        let conv_id = std::mem::take(&mut self.armed_conv);
        let conv_id = conv_id.as_str();
        let record = RunRecord {
            conv_id,
            turn_token: &self.turn_token,
            panel,
            reported_cost_usd: self.cost,
            previous: self.last_terminal.get(conv_id),
        };
        let entry = to_entry_with(&record, now, self.owned_local);
        self.armed = false;
        self.cost = None;
        let who = local_owned(
            classify(panel.requested_identity.as_ref(), panel.model.as_deref()),
            self.owned_local,
        );
        let prior = PriorRun {
            class: who.class,
            executor: who.executor,
            model: who.model,
            status,
            error: panel.error.clone(),
        };
        // The shared append tail of `observe` and `record_detached` (P8e)
        // lives in the parent module so the two paths cannot drift.
        Some(append_entry(self, path, conv_id, entry, prior))
    }
}

/// The shared append tail of [`Recorder::observe`] and
/// [`Recorder::record_detached`] (P8e): create the ledger's parent directory
/// (the default path lives under `~/.local/share/awili`, which a fresh
/// machine does not have), append the entry, and — only once BOTH succeed —
/// remember the run as the conversation's prior (so a later frontier
/// completion names a local failure as a fallback). A failed append retains
/// the prior run (S5(b)): the line may still land on a later retry, and a
/// fallback must name the run it actually followed, not a never-landed one.
/// Failures are reported through `Recorder::last_error`.
fn append_entry(
    rec: &mut Recorder,
    path: PathBuf,
    conv_id: &str,
    entry: LedgerEntry,
    prior: PriorRun,
) -> Result<LedgerEntry, String> {
    let prepared = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            awili_config::ensure_private_app_dir(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))
        }
        _ => Ok(()),
    };
    let appended =
        prepared.and_then(|()| Ledger::append(&path, &entry).map_err(|e| format!("{e:#}")));
    match appended {
        Ok(()) => {
            // S5(b): remember the prior ONLY after the ledger line actually
            // landed — a failure above keeps the previous prior intact.
            rec.last_terminal.insert(conv_id.to_string(), prior);
            rec.last_error = None;
            Ok(entry)
        }
        Err(error) => {
            let text = format!("graduation ledger: {error}");
            rec.last_error = Some(text.clone());
            Err(text)
        }
    }
}

#[path = "graduation_record_detached.rs"]
mod graduation_record_detached;

#[cfg(test)]
#[path = "graduation_record_tests.rs"]
mod graduation_record_tests;

#[cfg(test)]
#[path = "graduation_record_recorder_tests.rs"]
mod graduation_record_recorder_tests;

#[cfg(test)]
#[path = "graduation_record_seed_tests.rs"]
mod graduation_record_seed_tests;

#[cfg(test)]
#[path = "graduation_record_detached_tests.rs"]
mod graduation_record_detached_tests;

#[cfg(test)]
#[path = "graduation_record_s5b_tests.rs"]
mod graduation_record_s5b_tests;

#[cfg(test)]
#[path = "graduation_record_f4b_tests.rs"]
mod graduation_record_f4b_tests;
