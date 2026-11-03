//! Daily-driver P8e: the `Recorder`'s path for terminal runs that never had a
//! panel or an armed turn — the re-attached local Build rows that do not drive
//! the panel. Split from `graduation_record.rs` to keep both under the
//! 400-line rule; the shared append tail lives in the parent module
//! (`append_entry`) so `observe` and `record_detached` share the exact same
//! behaviour (parent-dir creation, append, `last_terminal` update,
//! `last_error` handling) and cannot drift.

use awili_selftest::graduation::LedgerEntry;
use awili_work::run_events::RunStatus;
use chrono::{DateTime, Utc};

use super::{append_entry, classify, to_entry, PriorRun, Recorder, RunPanel, RunRecord};
use crate::work_run_identity::WorkRunIdentity;

// U1 (S1-F04): `record_code` is the Code-tab entry point of this shared
// path; U1c wires its production callers.
impl Recorder {
    /// Record a terminal run that has no panel and no armed turn: a re-attached
    /// local Build row that does not drive the Work panel (P8e) ends here.
    /// `identity` is the row's configured binding, `status` its terminal status
    /// and `error` its failure text (`None` for a success).
    ///
    /// A foreground turn may be armed at the same time, so this path must NOT
    /// touch `armed`, `turn_token`, `armed_conv` or `cost` — it mints its own
    /// fresh token (so every reattached row gets a distinct `task_id`) and is
    /// otherwise built exactly like [`observe`](Self::observe): `previous` from
    /// `last_terminal`, `reported_cost_usd: None`, then the shared append tail.
    ///
    /// Returns `None` only when the ledger is disabled, `Some(Ok(entry))`
    /// after a successful append, `Some(Err(text))` when the append failed.
    pub(crate) fn record_detached(
        &mut self,
        conv_id: &str,
        identity: WorkRunIdentity,
        status: RunStatus,
        error: Option<String>,
        now: DateTime<Utc>,
    ) -> Option<Result<LedgerEntry, String>> {
        self.record_unarmed(conv_id, identity, status, error, None, now)
    }

    /// U1: record one terminal Code-tab turn. Built exactly like
    /// [`record_detached`](Self::record_detached) (fresh token, `previous` from
    /// `last_terminal`, shared append tail) plus the turn's summed usage, so a
    /// Code turn's frontier tokens land in the ledger. Never touches the armed
    /// Work turn (`armed`, `turn_token`, `armed_conv`, `cost`).
    pub(crate) fn record_code(
        &mut self,
        conv_id: &str,
        identity: WorkRunIdentity,
        status: RunStatus,
        error: Option<String>,
        usage: Option<awili_providers::Usage>,
        now: DateTime<Utc>,
    ) -> Option<Result<LedgerEntry, String>> {
        self.record_unarmed(conv_id, identity, status, error, usage, now)
    }

    /// The shared body of [`record_detached`](Self::record_detached) and
    /// [`record_code`](Self::record_code): a fresh-token terminal run built
    /// like [`observe`](Self::observe) that never touches the armed Work turn.
    fn record_unarmed(
        &mut self,
        conv_id: &str,
        identity: WorkRunIdentity,
        status: RunStatus,
        error: Option<String>,
        usage: Option<awili_providers::Usage>,
        now: DateTime<Utc>,
    ) -> Option<Result<LedgerEntry, String>> {
        let path = self.path.clone()?;
        let panel = RunPanel {
            requested_identity: Some(identity),
            status: Some(status),
            error,
            usage,
            ..RunPanel::default()
        };
        let previous = self.last_terminal.get(conv_id).cloned();
        let record = RunRecord {
            conv_id,
            turn_token: &uuid::Uuid::new_v4().simple().to_string(),
            panel: &panel,
            reported_cost_usd: None,
            previous: previous.as_ref(),
        };
        let entry = to_entry(&record, now);
        let who = classify(panel.requested_identity.as_ref(), panel.model.as_deref());
        Some(append_entry(
            self,
            path,
            conv_id,
            entry,
            PriorRun {
                class: who.class,
                executor: who.executor,
                model: who.model,
                status,
                error: panel.error.clone(),
            },
        ))
    }
}

#[cfg(test)]
#[path = "graduation_record_code_tests.rs"]
mod graduation_record_code_tests;
