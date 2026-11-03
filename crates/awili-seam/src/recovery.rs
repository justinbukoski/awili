//! RC1-N-FIX3: crash recovery from the durable log — separable on purpose.
//!
//! A worker starts from the model history the app kept. That history carries
//! its checkpoint, [`ContextHistory::log_turn`]: the seam stamps it when a
//! turn's history is kept, after placing every call the turn finalized in it
//! (`run_turn`), so the stamp says exactly which logged turns the history
//! holds. Recovery reads that stamp; it never infers coverage:
//!
//! - stamped: every call finalized in a turn logged AFTER the stamp;
//! - not stamped (a history saved before RC1-N-FIX3, or none kept yet), or a
//!   stamp this log does not hold: every call finalized anywhere in the log
//!   that the history does not hold (conservative: a call compaction folded
//!   away is listed again rather than risk a repeat).
//!
//! Cutting crash recovery out means deleting this module, its re-export in
//! `lib.rs`, and the worker's one call at startup; `run_turn`'s keep/drop
//! decision and the stamp do not depend on it.

use crate::{finalized_calls, step_result_text, ContextGroup, ContextHistory};
use awili_engine_log::schema::LogRecordV1;

/// RC1-N-FIX2: the label of recovered steps that ran.
pub const RECOVERED_WORK_LABEL: &str = "[Steps you already completed earlier in this conversation, \
recovered from Awili's log because the model history of their turn was not kept (the app was stopped \
or closed first). They ran; do not repeat them.]";

/// RC1-N-FIX2: the label of recovered steps that ended with an error — a
/// failed step, or one that never reached its tool.
pub const RECOVERED_FAILED_LABEL: &str = "[Steps from earlier in this conversation, recovered \
from Awili's log, that ended with an error; they may not have run. Check before trying them again.]";

/// The calls finalized in the log that `history` does not hold, as one note
/// for the model in log order (consecutive steps of one kind share a
/// label), or `None`. See the module docs for which turns are read.
pub fn recovered_work_note(records: &[LogRecordV1], history: &ContextHistory) -> Option<String> {
    let opened: Vec<&str> = records
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::TurnOpened { turn_id, .. } => Some(turn_id.as_str()),
            _ => None,
        })
        .collect();
    let after_stamp: Option<&[&str]> = history
        .log_turn()
        .and_then(|stamp| opened.iter().position(|t| *t == stamp))
        .map(|i| &opened[i + 1..]);
    let held: std::collections::HashSet<&str> = history
        .groups()
        .iter()
        .flat_map(|g| match g {
            ContextGroup::Exchange { calls, .. } => {
                calls.iter().map(|c| c.awili_call_id.0.as_str()).collect()
            }
            _ => Vec::new(),
        })
        .collect();
    let mut groups: Vec<(bool, Vec<String>)> = Vec::new();
    for f in finalized_calls(records) {
        let in_scope = match after_stamp {
            Some(turns) => turns.contains(&f.turn_id.as_str()),
            None => true,
        };
        if !in_scope || held.contains(f.call.awili_call_id.0.as_str()) {
            continue;
        }
        let shown = step_result_text(&f.content);
        let line = if f.ok {
            format!("- {} {}\n  result: {shown}", f.call.name, f.call.arguments)
        } else {
            format!("- {} {}\n  error: {shown}", f.call.name, f.call.arguments)
        };
        match groups.last_mut() {
            Some((kind, lines)) if *kind == f.ok => lines.push(line),
            _ => groups.push((f.ok, vec![line])),
        }
    }
    if groups.is_empty() {
        return None;
    }
    Some(
        groups
            .iter()
            .map(|(ok, lines)| {
                let label = if *ok {
                    RECOVERED_WORK_LABEL
                } else {
                    RECOVERED_FAILED_LABEL
                };
                format!("{label}\n{}", lines.join("\n"))
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

/// At a worker's start: add [`recovered_work_note`] to `history` (one user
/// message) and stamp `history` with the newest logged turn — every call
/// finalized up to there is now in it, as an exchange or in the note.
/// Returns whether a note was added.
pub fn recover_completed_work(records: &[LogRecordV1], history: &mut ContextHistory) -> bool {
    let note = recovered_work_note(records, history);
    let added = note.map(|n| history.push_user(n)).is_some();
    let newest = records.iter().rev().find_map(|r| match r {
        LogRecordV1::TurnOpened { turn_id, .. } => Some(turn_id.clone()),
        _ => None,
    });
    if let Some(turn_id) = newest {
        history.set_log_turn(turn_id);
    }
    added
}
