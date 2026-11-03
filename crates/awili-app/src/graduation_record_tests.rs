//! P8b required proof for `graduation_record` (offline, temp dirs only).

use std::sync::atomic::{AtomicUsize, Ordering};

use awili_code::CodeEngine;
use awili_providers::Usage;

use super::*;

pub(super) static TEMP_SEQ: AtomicUsize = AtomicUsize::new(0);

pub(super) fn temp_ledger(tag: &str) -> String {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
    // RC2-APP (K-flake2): under this test process's own scratch root (an
    // empty folder every time; ended processes' leftovers are swept).
    let dir = crate::test_tmp::fresh_dir(&format!("graduation-{tag}-{seq}"));
    dir.join("ledger.jsonl").to_string_lossy().into_owned()
}

pub(super) fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-23T20:00:00Z")
        .expect("test timestamp")
        .with_timezone(&Utc)
}

pub(super) fn agent(engine: CodeEngine, model: Option<&str>) -> WorkRunIdentity {
    WorkRunIdentity::Agent {
        engine,
        model: model.map(str::to_string),
    }
}

pub(super) fn panel(identity: Option<WorkRunIdentity>, status: Option<RunStatus>) -> RunPanel {
    RunPanel {
        requested_identity: identity,
        status,
        ..RunPanel::default()
    }
}

fn record<'a>(panel: &'a RunPanel, previous: Option<&'a PriorRun>) -> RunRecord<'a> {
    RunRecord {
        conv_id: "conv-1",
        turn_token: "tok7",
        panel,
        reported_cost_usd: None,
        previous,
    }
}

fn failed_local() -> PriorRun {
    PriorRun {
        class: Class::Local,
        executor: "local-build".into(),
        model: "qwen@hosta".into(),
        status: RunStatus::Failed,
        error: Some("verify FAIL".into()),
    }
}

#[test]
fn classify_every_identity_variant() {
    let c = classify(Some(&agent(CodeEngine::Claude, Some("opus"))), None);
    assert_eq!(
        (c.executor.as_str(), c.model.as_str(), c.class),
        ("claude", "opus", Class::Frontier)
    );
    let c = classify(Some(&agent(CodeEngine::Dsh, None)), None);
    assert_eq!(
        (c.executor.as_str(), c.model.as_str()),
        ("deepseek", "default")
    );
    let c = classify(Some(&WorkRunIdentity::HermesGateway), None);
    assert_eq!((c.executor.as_str(), c.class), ("gateway", Class::Frontier));
    let c = classify(
        Some(&WorkRunIdentity::OwnedEngine {
            model: "llama".into(), engine_id: None,
        }),
        None,
    );
    assert_eq!(
        (c.executor.as_str(), c.model.as_str(), c.class),
        ("owned", "llama", Class::Unknown)
    );
    let c = classify(
        Some(&WorkRunIdentity::LocalBuild {
            model: "qwen".into(),
            host: "hosta".into(),
        }),
        None,
    );
    assert_eq!(
        (c.executor.as_str(), c.model.as_str(), c.class),
        ("local-build", "qwen@hosta", Class::Local)
    );
    let c = classify(None, None);
    assert_eq!((c.executor.as_str(), c.class), ("unknown", Class::Unknown));
    // The panel's detected model wins over the identity's request.
    let c = classify(
        Some(&agent(CodeEngine::Codex, Some("requested"))),
        Some("gpt-6-sol"),
    );
    assert_eq!(c.model, "gpt-6-sol");
}

#[test]
fn completed_frontier_is_success() {
    let p = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Completed),
    );
    let e = to_entry(&record(&p, None), now());
    assert_eq!(e.outcome, Outcome::Success);
    assert_eq!(e.task_id, "work:conv-1:tok7");
    assert_eq!(e.recorded_at, "2026-09-23T20:00:00Z");
}

#[test]
fn completed_frontier_after_failed_local_is_fallback_with_reason() {
    let p = panel(
        Some(agent(CodeEngine::Codex, None)),
        Some(RunStatus::Completed),
    );
    let prev = failed_local();
    let e = to_entry(&record(&p, Some(&prev)), now());
    assert_eq!(
        e.outcome,
        Outcome::Fallback {
            reason: "after local local-build/qwen@hosta failed: verify FAIL".into()
        }
    );
}

#[test]
fn completed_frontier_after_completed_local_is_success() {
    let p = panel(
        Some(agent(CodeEngine::Codex, None)),
        Some(RunStatus::Completed),
    );
    let mut prev = failed_local();
    prev.status = RunStatus::Completed;
    assert_eq!(
        to_entry(&record(&p, Some(&prev)), now()).outcome,
        Outcome::Success
    );
    // A local run after a failed local run is not a fallback either.
    let local = panel(
        Some(WorkRunIdentity::LocalBuild {
            model: "qwen".into(),
            host: "hosta".into(),
        }),
        Some(RunStatus::Completed),
    );
    let prev = failed_local();
    assert_eq!(
        to_entry(&record(&local, Some(&prev)), now()).outcome,
        Outcome::Success
    );
}

#[test]
fn failed_carries_error_text_and_failed_without_error_has_reason() {
    let mut p = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Failed),
    );
    p.error = Some("  engine exited with code 2 ".into());
    assert_eq!(
        to_entry(&record(&p, None), now()).outcome,
        Outcome::Failed {
            reason: "engine exited with code 2".into()
        }
    );
    p.error = None;
    assert_eq!(
        to_entry(&record(&p, None), now()).outcome,
        Outcome::Failed {
            reason: "failed without error text".into()
        }
    );
    // No status but an error (fail_local_build path) is a failure too.
    let mut p = panel(None, None);
    p.error = Some("no target".into());
    assert_eq!(terminal_status(&p), Some(RunStatus::Failed));
    assert_eq!(
        terminal_status(&panel(None, Some(RunStatus::Running))),
        None
    );
}

#[test]
fn stopped_is_failed_stopped_by_user() {
    // F16 (M1b): the name predates the Stopped outcome; kept because tests
    // are never renamed. The outcome is now the first-class `Stopped`.
    let p = panel(Some(agent(CodeEngine::Glm, None)), Some(RunStatus::Stopped));
    assert_eq!(to_entry(&record(&p, None), now()).outcome, Outcome::Stopped);
}

#[test]
fn m1b_stop_writes_stopped_for_every_identity() {
    let local = WorkRunIdentity::LocalBuild {
        model: "qwen".into(),
        host: "hosta".into(),
    };
    for identity in [
        WorkRunIdentity::HermesGateway,
        agent(CodeEngine::Glm, None),
        local,
    ] {
        let p = panel(Some(identity), Some(RunStatus::Stopped));
        let e = to_entry(&record(&p, None), now());
        assert_eq!(e.outcome, Outcome::Stopped);
        // No usage was captured on a cancelled run.
        assert_eq!(e.frontier_tokens, 0);
    }
}

#[test]
fn tokens_split_by_class() {
    let usage = Some(Usage {
        cached_input_tokens: None,
        input_tokens: 1200,
        output_tokens: 300,
    });
    let mut p = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Completed),
    );
    p.usage = usage;
    let e = to_entry(&record(&p, None), now());
    assert_eq!(
        (e.frontier_tokens, e.local_tokens, e.notes),
        (1500, 0, None)
    );
    let mut p = panel(
        Some(WorkRunIdentity::LocalBuild {
            model: "qwen".into(),
            host: "hosta".into(),
        }),
        Some(RunStatus::Completed),
    );
    p.usage = usage;
    let e = to_entry(&record(&p, None), now());
    assert_eq!((e.frontier_tokens, e.local_tokens), (0, 1500));
    let mut p = panel(
        Some(WorkRunIdentity::OwnedEngine {
            model: "llama".into(), engine_id: None,
        }),
        Some(RunStatus::Completed),
    );
    p.usage = usage;
    let e = to_entry(&record(&p, None), now());
    assert_eq!((e.frontier_tokens, e.local_tokens, e.notes), (0, 0, None));
}

#[test]
fn graduation_records_owned_failure_reason() {
    // P3i: the owned engine reports a failed turn as a `Failed` Completion
    // whose output IS the reason; `apply_event` now moves it into
    // `panel.error`, so the ledger line carries it instead of
    // `failed without error text`.
    let mut p = panel(
        Some(WorkRunIdentity::OwnedEngine {
            model: "llama".into(), engine_id: None,
        }),
        None,
    );
    p.apply_event(&awili_work::run_events::RunEvent::Completion(
        awili_work::run_events::CompletionEvent {
            status: Some(RunStatus::Failed),
            output: Some("sandbox: path outside session root".into()),
        },
    ));
    assert_eq!(
        to_entry(&record(&p, None), now()).outcome,
        Outcome::Failed {
            reason: "sandbox: path outside session root".into()
        }
    );
}

#[test]
fn no_usage_note_and_cost_only_note() {
    let p = panel(
        Some(agent(CodeEngine::Dsh, None)),
        Some(RunStatus::Completed),
    );
    let e = to_entry(&record(&p, None), now());
    assert_eq!(e.notes.as_deref(), Some("tokens not reported by deepseek"));
    let p = panel(
        Some(WorkRunIdentity::HermesGateway),
        Some(RunStatus::Completed),
    );
    let mut r = record(&p, None);
    r.reported_cost_usd = Some(0.0123);
    assert_eq!(
        to_entry(&r, now()).notes.as_deref(),
        Some("cost only: $0.0123")
    );
}
