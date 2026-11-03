//! U1a required proof (offline, temp dirs): `Recorder::record_code`, the
//! ledger line for one terminal Code-tab turn (S1-F04).

use std::path::Path;

use awili_code::CodeEngine;
use awili_providers::Usage;
use awili_selftest::graduation::{Ledger, Outcome};
use awili_work::run_events::RunStatus;

use crate::graduation_record::graduation_record_tests::{agent, now, temp_ledger};
use crate::graduation_record::{prior_run_from_entry, Recorder};

#[test]
fn u1a_code_success_writes_one_frontier_line() {
    let ledger = temp_ledger("u1a-success");
    let mut rec = Recorder::from_config(&ledger);
    let e = rec
        .record_code(
            "conv-c1",
            agent(CodeEngine::Glm, None),
            RunStatus::Completed,
            None,
            Some(Usage {
                cached_input_tokens: None,
                input_tokens: 1000,
                output_tokens: 500,
            }),
            now(),
        )
        .expect("enabled")
        .expect("ok");
    assert_eq!(e.executor.as_str(), "glm");
    assert_eq!(e.model.as_str(), "default");
    assert_eq!(e.outcome, Outcome::Success);
    assert_eq!(e.frontier_tokens, 1500);
    assert_eq!(e.local_tokens, 0);
    assert_eq!(e.notes, None);
    assert!(e.task_id.starts_with("work:conv-c1:"), "{}", e.task_id);
    let lines = Ledger::read(Path::new(&ledger)).expect("read");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], e);
}

#[test]
fn u1a_code_model_pin_is_recorded() {
    let ledger = temp_ledger("u1a-pin");
    let mut rec = Recorder::from_config(&ledger);
    let e = rec
        .record_code(
            "conv-c1",
            agent(CodeEngine::Claude, Some("sonnet")),
            RunStatus::Completed,
            None,
            None,
            now(),
        )
        .expect("enabled")
        .expect("ok");
    assert_eq!(e.executor, "claude");
    assert_eq!(e.model, "sonnet");
}

#[test]
fn u1a_code_stop_matches_work_stop() {
    let ledger = temp_ledger("u1a-stop");
    let mut rec = Recorder::from_config(&ledger);
    let e = rec
        .record_code(
            "conv-c1",
            agent(CodeEngine::Glm, None),
            RunStatus::Stopped,
            Some("terminated".into()),
            None,
            now(),
        )
        .expect("enabled")
        .expect("ok");
    assert_eq!(e.outcome, Outcome::Stopped);
    let (conv, prior) = prior_run_from_entry(&e).expect("parsed");
    assert_eq!(conv, "conv-c1");
    assert_eq!(prior.status, RunStatus::Stopped);
}

#[test]
fn u1a_code_failure_keeps_reason() {
    let ledger = temp_ledger("u1a-fail");
    let mut rec = Recorder::from_config(&ledger);
    let e = rec
        .record_code(
            "conv-c1",
            agent(CodeEngine::Glm, None),
            RunStatus::Failed,
            Some("zcode exited 1".into()),
            None,
            now(),
        )
        .expect("enabled")
        .expect("ok");
    assert_eq!(
        e.outcome,
        Outcome::Failed {
            reason: "zcode exited 1".into()
        }
    );
}

#[test]
fn u1a_code_without_usage_says_so() {
    let ledger = temp_ledger("u1a-nousage");
    let mut rec = Recorder::from_config(&ledger);
    let e = rec
        .record_code(
            "conv-c1",
            agent(CodeEngine::Glm, None),
            RunStatus::Completed,
            None,
            None,
            now(),
        )
        .expect("enabled")
        .expect("ok");
    assert_eq!(e.frontier_tokens, 0);
    assert_eq!(e.notes.as_deref(), Some("tokens not reported by glm"));
}

#[test]
fn u1a_code_disabled_ledger_is_none() {
    let mut rec = Recorder::default();
    let res = rec.record_code(
        "conv-c1",
        agent(CodeEngine::Glm, None),
        RunStatus::Completed,
        None,
        None,
        now(),
    );
    assert!(res.is_none());
    assert!(rec.path().is_none());
}

#[test]
fn u1a_code_leaves_armed_work_turn_alone() {
    let ledger = temp_ledger("u1a-armed");
    let mut rec = Recorder::from_config(&ledger);
    rec.arm("w1");
    let e1 = rec
        .record_code(
            "c1",
            agent(CodeEngine::Glm, None),
            RunStatus::Completed,
            None,
            None,
            now(),
        )
        .expect("enabled")
        .expect("ok");
    let e2 = rec
        .record_code(
            "c1",
            agent(CodeEngine::Glm, None),
            RunStatus::Completed,
            None,
            None,
            now(),
        )
        .expect("enabled")
        .expect("ok");
    // The armed Work turn is untouched: still armed, still attributed to "w1".
    assert!(rec.is_armed());
    assert_eq!(rec.armed_conv(), "w1");
    assert_ne!(e1.task_id, e2.task_id, "fresh token per Code turn");
}
