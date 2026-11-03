//! P8b required proof, part 2: the `Recorder` (arming, once-per-turn, disabled
//! path, append failure, parent directory). Split from
//! `graduation_record_tests.rs` to keep both files under the 400-line rule.

use std::path::Path;

use awili_code::CodeEngine;
use awili_selftest::graduation::Ledger;

use super::graduation_record_tests::{agent, now, panel, temp_ledger};
use super::*;

#[test]
fn recorder_appends_exactly_once_per_run_and_remembers_prior() {
    let ledger = temp_ledger("once");
    let mut rec = Recorder::from_config(&ledger);
    let mut local = panel(
        Some(WorkRunIdentity::LocalBuild {
            model: "qwen".into(),
            host: "hosta".into(),
        }),
        Some(RunStatus::Failed),
    );
    local.error = Some("verify FAIL".into());
    assert!(rec.observe(&local, now()).is_none(), "not armed: nothing");
    rec.arm("c");
    assert!(matches!(rec.observe(&local, now()), Some(Ok(_))));
    assert!(
        rec.observe(&local, now()).is_none(),
        "second call is a no-op"
    );
    assert!(!rec.is_armed());
    let frontier = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Completed),
    );
    rec.note_cost(0.25); // ignored: not armed
    rec.arm("c");
    rec.note_cost(0.5);
    let e = rec
        .observe(&frontier, now())
        .expect("recorded")
        .expect("ok");
    assert!(
        matches!(e.outcome, Outcome::Fallback { .. }),
        "{:?}",
        e.outcome
    );
    assert_eq!(e.notes.as_deref(), Some("cost only: $0.5000"));
    // A different conversation has no prior.
    rec.arm("other");
    let e = rec
        .observe(&frontier, now())
        .expect("recorded")
        .expect("ok");
    assert_eq!(e.outcome, Outcome::Success);
    assert!(e.task_id.starts_with("work:other:"), "{}", e.task_id);
    // Re-arming records a fourth turn on the same conversation.
    rec.arm("c");
    let e = rec
        .observe(&frontier, now())
        .expect("recorded")
        .expect("ok");
    assert!(e.task_id.starts_with("work:c:"), "{}", e.task_id);
    assert_eq!(e.outcome, Outcome::Success);
    let lines = Ledger::read(Path::new(&ledger)).expect("read");
    assert_eq!(lines.len(), 4);
    let ids: std::collections::HashSet<&str> = lines.iter().map(|l| l.task_id.as_str()).collect();
    assert_eq!(ids.len(), 4, "task ids are unique per armed turn");
    assert!(lines[0].task_id.starts_with("work:c:"));
    assert!(rec.last_error.is_none());
}

#[test]
fn disabled_ledger_writes_nothing_and_non_terminal_is_skipped() {
    let mut rec = Recorder::from_config("   ");
    assert!(rec.path().is_none());
    rec.arm("c");
    let p = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Completed),
    );
    assert!(rec.observe(&p, now()).is_none());
    let ledger = temp_ledger("nonterminal");
    let mut rec = Recorder::from_config(&ledger);
    rec.arm("c");
    let running = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Running),
    );
    assert!(rec.observe(&running, now()).is_none());
    assert!(rec.is_armed(), "a non-terminal panel keeps the turn armed");
    assert!(!Path::new(&ledger).exists());
    // Tilde expansion happens at set time.
    let mut rec = Recorder::default();
    rec.set_ledger("~/x/ledger.jsonl");
    assert!(!rec.path().expect("path").to_string_lossy().starts_with('~'));
}

#[test]
fn append_failure_is_reported_not_fatal() {
    // RC2-APP (K-flake2): a fresh folder under this process's scratch root.
    let dir = crate::test_tmp::fresh_dir("graduation-dir");
    // A directory as the ledger path cannot be opened for append.
    let mut rec = Recorder::from_config(&dir.to_string_lossy());
    rec.arm("c");
    let p = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Completed),
    );
    let err = rec
        .observe(&p, now())
        .expect("attempted")
        .expect_err("fails");
    assert!(err.starts_with("graduation ledger: "), "{err}");
    assert_eq!(rec.last_error.as_deref(), Some(err.as_str()));
    assert!(rec.observe(&p, now()).is_none(), "not retried");
}

#[test]
fn terminal_while_disabled_disarms_and_is_never_flushed_later() {
    // Codex round 3: a run that ended while the ledger was off must not be
    // appended later (under a different conversation) once the ledger is on.
    let mut rec = Recorder::from_config("");
    rec.arm("old-conv");
    let done = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Completed),
    );
    assert!(rec.observe(&done, now()).is_none());
    assert!(!rec.is_armed(), "terminal while disabled still disarms");
    let ledger = temp_ledger("enable-later");
    rec.set_ledger(&ledger);
    assert!(
        rec.observe(&done, now()).is_none(),
        "nothing armed, nothing flushed"
    );
    assert!(!Path::new(&ledger).exists());
    // The line is attributed to the conversation the turn was armed under.
    rec.arm("submitted-conv");
    let e = rec.observe(&done, now()).expect("recorded").expect("ok");
    assert!(
        e.task_id.starts_with("work:submitted-conv:"),
        "{}",
        e.task_id
    );
}

#[test]
fn missing_parent_directory_is_created_on_first_append() {
    // Codex round 4: the default ~/.local/share/awili does not exist on a
    // fresh machine; the first terminal run must create it, not fail.
    // RC2-APP (K-flake2): a folder that does not exist yet, under this
    // process's scratch root.
    let root = crate::test_tmp::fresh_dir("graduation-parent").join("missing");
    let ledger = root.join("share").join("awili").join("graduation.jsonl");
    assert!(!root.exists());
    let mut rec = Recorder::from_config(&ledger.to_string_lossy());
    rec.arm("c");
    let p = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Completed),
    );
    rec.observe(&p, now()).expect("recorded").expect("ok");
    assert_eq!(Ledger::read(&ledger).expect("read").len(), 1);
}
