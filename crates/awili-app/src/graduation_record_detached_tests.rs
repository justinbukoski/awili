//! P8e required proof (offline, temp dirs): `Recorder::record_detached`, the
//! ledger line for a terminal re-attached local Build row that does not drive
//! the panel.

use std::path::Path;

use awili_code::CodeEngine;
use awili_selftest::graduation::{Ledger, Outcome};
use awili_work::run_events::RunStatus;

use super::graduation_record_tests::{agent, now, panel, temp_ledger};
use super::{Recorder, WorkRunIdentity};

#[test]
fn record_detached_appends_local_build_line() {
    let ledger = temp_ledger("detached");
    let mut rec = Recorder::from_config(&ledger);
    let identity = WorkRunIdentity::LocalBuild {
        model: "qwen".into(),
        host: "hosta".into(),
    };
    let e = rec
        .record_detached("c", identity, RunStatus::Completed, None, now())
        .expect("enabled")
        .expect("ok");
    assert_eq!(e.outcome, Outcome::Success);
    assert_eq!(e.executor, "local-build");
    assert_eq!(e.model, "qwen@hosta");
    assert!(e.task_id.starts_with("work:c:"), "{}", e.task_id);
    assert_eq!(Ledger::read(Path::new(&ledger)).expect("read").len(), 1);
}

#[test]
fn record_detached_failed_carries_error() {
    let ledger = temp_ledger("detached-fail");
    let mut rec = Recorder::from_config(&ledger);
    let identity = WorkRunIdentity::LocalBuild {
        model: "qwen".into(),
        host: "hosta".into(),
    };
    let e = rec
        .record_detached(
            "c",
            identity,
            RunStatus::Failed,
            Some("Build r1: failed — verify FAIL".into()),
            now(),
        )
        .expect("enabled")
        .expect("ok");
    assert_eq!(
        e.outcome,
        Outcome::Failed {
            reason: "Build r1: failed — verify FAIL".into()
        }
    );
}

#[test]
fn record_detached_does_not_disturb_an_armed_turn() {
    let ledger = temp_ledger("detached-armed");
    let mut rec = Recorder::from_config(&ledger);
    rec.arm("x");
    let identity = WorkRunIdentity::LocalBuild {
        model: "qwen".into(),
        host: "hosta".into(),
    };
    let e1 = rec
        .record_detached("y", identity, RunStatus::Completed, None, now())
        .expect("enabled")
        .expect("ok");
    assert!(e1.task_id.starts_with("work:y:"), "{}", e1.task_id);
    // The armed turn is untouched: still armed, still attributed to "x".
    assert!(rec.is_armed());
    let x = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Completed),
    );
    let e2 = rec.observe(&x, now()).expect("recorded").expect("ok");
    assert!(e2.task_id.starts_with("work:x:"), "{}", e2.task_id);
    assert_ne!(e1.task_id, e2.task_id, "distinct task ids");
}

#[test]
fn record_detached_updates_prior_for_fallback() {
    let ledger = temp_ledger("detached-prior");
    let mut rec = Recorder::from_config(&ledger);
    let identity = WorkRunIdentity::LocalBuild {
        model: "qwen".into(),
        host: "hosta".into(),
    };
    rec.record_detached(
        "c",
        identity,
        RunStatus::Failed,
        Some("verify FAIL".into()),
        now(),
    )
    .expect("enabled")
    .expect("ok");
    rec.arm("c");
    let frontier = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Completed),
    );
    let e = rec
        .observe(&frontier, now())
        .expect("recorded")
        .expect("ok");
    assert!(
        matches!(e.outcome, Outcome::Fallback { .. }),
        "{:?}",
        e.outcome
    );
}

#[test]
fn record_detached_disabled_ledger_is_none() {
    let mut rec = Recorder::from_config("");
    let identity = WorkRunIdentity::LocalBuild {
        model: "qwen".into(),
        host: "hosta".into(),
    };
    assert!(rec
        .record_detached("c", identity, RunStatus::Completed, None, now())
        .is_none());
}

fn stale_build() -> WorkRunIdentity {
    WorkRunIdentity::LocalBuild {
        model: "qwen".into(),
        host: "hosta".into(),
    }
}

/// P2f: a stale panel-driving local build (armed turn) reaches the ledger as
/// `Failed` with the `stale: ` reason.
#[test]
fn stale_failure_reaches_ledger_armed() {
    let ledger = temp_ledger("stale-armed");
    let mut rec = Recorder::from_config(&ledger);
    rec.arm("c");
    let mut p = panel(Some(stale_build()), Some(RunStatus::Failed));
    p.error = Some("stale: build host unreachable for 10m after 3 failed status polls".into());
    let e = rec.observe(&p, now()).expect("recorded").expect("ok");
    assert!(
        matches!(&e.outcome, Outcome::Failed { reason } if reason.starts_with("stale:")),
        "{:?}",
        e.outcome
    );
    assert_eq!(e.executor, "local-build");
}

/// P2f: a stale background (or orphaned) row reaches the ledger through
/// `record_detached` the same way.
#[test]
fn stale_failure_reaches_ledger_detached() {
    let ledger = temp_ledger("stale-detached");
    let mut rec = Recorder::from_config(&ledger);
    let reason = "stale: build target 'gone' is no longer configured".to_string();
    let e = rec
        .record_detached(
            "c",
            stale_build(),
            RunStatus::Failed,
            Some(reason.clone()),
            now(),
        )
        .expect("enabled")
        .expect("ok");
    assert_eq!(e.outcome, Outcome::Failed { reason });
    assert_eq!(Ledger::read(Path::new(&ledger)).expect("read").len(), 1);
}
