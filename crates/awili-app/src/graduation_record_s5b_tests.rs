//! S5(b) required proof (offline, temp dirs): `append_entry` must update
//! `last_terminal` ONLY after the directory preparation AND the ledger append
//! both succeed. A failed append keeps the prior run intact so a later retry
//! (a re-armed turn) can still land the line and name a real prior. Sibling of
//! the other graduation test files to keep each under the 400-line rule.

use awili_code::CodeEngine;
use awili_selftest::graduation::Ledger;

use super::graduation_record_tests::{agent, now, panel, temp_ledger};
use super::*;

/// A failed append must keep the prior run: `last_terminal` is untouched when
/// the ledger line never lands, and a later retry on the same conversation
/// still sees the ORIGINAL prior (not the failed run) and advances the map
/// exactly once, on success.
#[test]
fn failed_graduation_append_keeps_prior() {
    // Seed a real prior for conv "c" from the ledger, so a failure cannot be
    // mistaken for "no prior yet".
    let ledger = temp_ledger("s5b");
    let mut rec = Recorder::from_config(&ledger);
    let mut local = panel(
        Some(WorkRunIdentity::LocalBuild {
            model: "qwen".into(),
            host: "hosta".into(),
        }),
        Some(RunStatus::Failed),
    );
    local.error = Some("verify FAIL".into());
    rec.arm("c");
    let e = rec.observe(&local, now()).expect("recorded").expect("ok");
    assert!(
        matches!(e.outcome, Outcome::Failed { .. }),
        "{:?}",
        e.outcome
    );
    // The seeded prior: conv "c" = a failed local run.
    let prior_c = rec.last_terminal_of("c").expect("prior seeded");
    assert_eq!(prior_c.status, RunStatus::Failed);

    // Point the ledger at a path that CANNOT be appended: a directory
    // (the same fault the existing `append_failure_is_reported_not_fatal`
    // uses — a directory as the ledger path cannot be opened for append).
    // RC2-APP (K-flake2): a fresh folder under this process's scratch root.
    let bad = crate::test_tmp::fresh_dir("graduation-s5b-dir");
    rec.set_ledger(&bad.to_string_lossy());

    // A frontier completion on "c" would land a Fallback line, but the append
    // fails. The prior must be RETAINED (not the never-landed frontier run).
    let frontier = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Completed),
    );
    rec.arm("c");
    let err = rec
        .observe(&frontier, now())
        .expect("attempted")
        .expect_err("append fails");
    assert!(err.starts_with("graduation ledger: "), "{err}");
    assert_eq!(rec.last_error.as_deref(), Some(err.as_str()));
    // The prior is UNCHANGED: still the failed local run.
    let after_fail = rec.last_terminal_of("c").expect("prior retained");
    assert_eq!(
        after_fail.status,
        RunStatus::Failed,
        "a failed append must not record the run as prior"
    );

    // Fix the ledger and retry the same frontier completion on "c": it now
    // lands, and the outcome must be a Fallback named against the ORIGINAL
    // prior (the failed local run), proving the retry advanced the map once.
    rec.set_ledger(&ledger);
    rec.arm("c");
    let e = rec
        .observe(&frontier, now())
        .expect("recorded")
        .expect("ok");
    assert!(
        matches!(e.outcome, Outcome::Fallback { .. }),
        "the retry must name the original prior as a fallback, got {:?}",
        e.outcome
    );
    // The map advanced exactly once: now "c" = the frontier run.
    let after = rec.last_terminal_of("c").expect("prior advanced");
    assert_eq!(after.status, RunStatus::Completed);
    assert!(rec.last_error.is_none(), "the success cleared the error");
    // Exactly two ledger lines: the seeded local run and the retried frontier.
    let lines = Ledger::read(std::path::Path::new(&ledger)).expect("read");
    assert_eq!(lines.len(), 2, "{lines:?}");
}
