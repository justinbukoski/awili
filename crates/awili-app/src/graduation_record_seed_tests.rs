//! P8d required proof: seed the fallback detector from the ledger at startup
//! (`Recorder::seed_from_ledger`, `prior_run_from_entry`). Offline, temp dirs
//! only. A sibling of `graduation_record_recorder_tests.rs` to keep both
//! files under the 400-line rule.

use awili_code::CodeEngine;

use super::graduation_record_tests::{agent, now, panel, temp_ledger};
use super::*;

/// One ledger line with the app's CLI-shaped `task_id` (`work:<conv>:<tok>`).
fn entry(conv: &str, tok: &str, executor: &str, model: &str, outcome: Outcome) -> LedgerEntry {
    LedgerEntry {
        v: LEDGER_VERSION,
        recorded_at: "2026-09-23T19:00:00Z".to_string(),
        task_id: format!("work:{conv}:{tok}"),
        executor: executor.to_string(),
        model: model.to_string(),
        outcome,
        frontier_tokens: 0,
        local_tokens: 0,
        notes: None,
    }
}

fn seed(rec: &mut Recorder) -> Result<usize, String> {
    rec.seed_from_ledger()
}

#[test]
fn seed_missing_or_disabled_ledger_is_zero() {
    // Disabled (`""` config): no path, nothing to read.
    let mut rec = Recorder::from_config("");
    assert_eq!(seed(&mut rec).expect("disabled is Ok"), 0);
    assert!(!rec.is_armed());
    // Enabled, but the file does not exist yet: `Ledger::read` sees an empty
    // ledger, so the map stays empty and the count is zero.
    let ledger = temp_ledger("seed-missing");
    let mut rec = Recorder::from_config(&ledger);
    assert!(!std::path::Path::new(&ledger).exists());
    assert_eq!(seed(&mut rec).expect("missing is Ok"), 0);
}

#[test]
fn seed_takes_last_entry_per_conversation() {
    let ledger = temp_ledger("seed-last");
    let entries = vec![
        entry(
            "a",
            "t1",
            "local-build",
            "qwen@hosta",
            Outcome::Failed {
                reason: "first".into(),
            },
        ),
        entry("b", "t1", "claude", "opus", Outcome::Success),
        entry(
            "a",
            "t2",
            "local-build",
            "qwen@hosta",
            Outcome::Failed {
                reason: "second".into(),
            },
        ),
        entry(
            "a",
            "t3",
            "local-build",
            "qwen@hosta",
            Outcome::Failed {
                reason: "stopped by user".into(),
            },
        ),
    ];
    for e in &entries {
        Ledger::append(std::path::Path::new(&ledger), e).expect("append");
    }
    let mut rec = Recorder::from_config(&ledger);
    assert_eq!(seed(&mut rec).expect("read ok"), 2);
    let prev_a = rec.last_terminal.get("a").expect("a seeded");
    assert_eq!(
        (prev_a.status, prev_a.error.as_deref()),
        (RunStatus::Stopped, Some("stopped by user")),
        "a keeps its last entry, not its first"
    );
    let prev_b = rec.last_terminal.get("b").expect("b seeded");
    assert_eq!(prev_b.status, RunStatus::Completed);
}

#[test]
fn seed_maps_outcomes_to_status() {
    // One conversation per outcome (the seed keeps only each conversation's
    // LAST entry, which seed_takes_last_entry_per_conversation covers).
    let cases = [
        (
            "s1",
            "seed-outcome-s1",
            Outcome::Success,
            RunStatus::Completed,
            None::<&str>,
        ),
        (
            "s2",
            "seed-outcome-s2",
            Outcome::Failed {
                reason: "stopped by user".into(),
            },
            RunStatus::Stopped,
            Some("stopped by user"),
        ),
        (
            "s3",
            "seed-outcome-s3",
            Outcome::Failed {
                reason: "engine died".into(),
            },
            RunStatus::Failed,
            Some("engine died"),
        ),
        (
            "s4",
            "seed-outcome-s4",
            Outcome::Fallback {
                reason: "after local ...".into(),
            },
            RunStatus::Failed,
            Some("after local ..."),
        ),
    ];
    for (conv, tag, outcome, status, error) in cases {
        let ledger = temp_ledger(tag);
        Ledger::append(
            std::path::Path::new(&ledger),
            &entry(conv, "t1", "claude", "opus", outcome),
        )
        .expect("append");
        let mut rec = Recorder::from_config(&ledger);
        seed(&mut rec).expect("read ok");
        let prev = rec.last_terminal.get(conv).expect("seeded");
        assert_eq!(
            (prev.status, prev.error.as_deref()),
            (status, error),
            "{conv}: {prev:?}"
        );
    }
}

#[test]
fn seed_maps_executor_to_class() {
    let cases = [
        ("local-build", "qwen@hosta", Class::Local),
        ("owned", "llama", Class::Unknown),
        ("unknown", "unknown", Class::Unknown),
        ("claude", "opus", Class::Frontier),
    ];
    for (i, case) in cases.iter().enumerate() {
        let (executor, model, class) = case;
        let conv = format!("e{i}");
        let ledger = temp_ledger(&format!("seed-class-{i}"));
        Ledger::append(
            std::path::Path::new(&ledger),
            &entry(&conv, "t1", executor, model, Outcome::Success),
        )
        .expect("append");
        let mut rec = Recorder::from_config(&ledger);
        assert_eq!(seed(&mut rec).expect("read ok"), 1);
        let prev = rec.last_terminal.get(&conv).expect("seeded");
        assert_eq!(
            (prev.class, prev.executor.as_str(), prev.model.as_str()),
            (*class, *executor, *model),
        );
    }
}

#[test]
fn seeded_prior_enables_fallback_after_restart() {
    // The whole point of P8d: a local failure from BEFORE the restart is
    // remembered, so the frontier retry is a Fallback, not a Success.
    let ledger = temp_ledger("seed-restart");
    Ledger::append(
        std::path::Path::new(&ledger),
        &entry(
            "c",
            "abc",
            "local-build",
            "qwen@hosta",
            Outcome::Failed {
                reason: "verify FAIL".into(),
            },
        ),
    )
    .expect("append");
    let mut rec = Recorder::from_config(&ledger);
    assert_eq!(seed(&mut rec).expect("read ok"), 1);
    rec.arm("c");
    let frontier = panel(
        Some(agent(CodeEngine::Claude, None)),
        Some(RunStatus::Completed),
    );
    let e = rec
        .observe(&frontier, now())
        .expect("recorded")
        .expect("ok");
    let Outcome::Fallback { reason } = &e.outcome else {
        panic!(
            "expected Fallback after a seeded local failure, got {:?}",
            e.outcome
        );
    };
    assert!(reason.contains("verify FAIL"), "{reason}");
}

#[test]
fn seed_skips_malformed_task_ids() {
    let ledger = temp_ledger("seed-malformed");
    let mut bad = entry("x", "t1", "claude", "opus", Outcome::Success);
    bad.task_id = "x".to_string(); // no `work:` prefix, no token
    Ledger::append(std::path::Path::new(&ledger), &bad).expect("append");
    let mut rec = Recorder::from_config(&ledger);
    assert_eq!(
        seed(&mut rec).expect("a read with a skipped line still succeeds"),
        0
    );
}

#[test]
fn seed_read_error_is_reported_not_fatal() {
    let ledger = temp_ledger("seed-read-error");
    // A malformed line fails `Ledger::read` with its 1-based line number.
    std::fs::write(std::path::Path::new(&ledger), "this is not a ledger line\n").expect("write");
    let mut rec = Recorder::from_config(&ledger);
    let err = seed(&mut rec).expect_err("the malformed line is reported");
    assert!(err.contains("line"), "{err}");
    assert!(
        rec.last_terminal.is_empty(),
        "a failed read changes nothing"
    );
    // The app moves on after the skip: nothing was armed, nothing breaks.
    assert!(!rec.is_armed());
}

#[test]
fn m1b_seed_reads_new_stopped_as_stopped() {
    // F16 (M1b): a ledger written with the first-class `Stopped` outcome (not
    // the legacy Failed{"stopped by user"}) seeds as a local stop.
    let ledger = temp_ledger("m1b-seed-stopped");
    Ledger::append(
        std::path::Path::new(&ledger),
        &entry("z", "t8", "local-build", "qwen@hosta", Outcome::Stopped),
    )
    .expect("append");
    let mut rec = Recorder::from_config(&ledger);
    assert_eq!(seed(&mut rec).expect("read ok"), 1);
    let prev = rec.last_terminal.get("z").expect("z seeded");
    assert_eq!(
        (prev.status, prev.error.as_deref(), prev.class),
        (RunStatus::Stopped, Some("stopped by user"), Class::Local)
    );
    // A frontier completion after that seeded local stop is a fallback, with
    // the stop's reason surfaced in the reason text.
    let p = panel(
        Some(agent(CodeEngine::Glm, None)),
        Some(RunStatus::Completed),
    );
    let e = to_entry(
        &RunRecord {
            conv_id: "z",
            turn_token: "t9",
            panel: &p,
            reported_cost_usd: None,
            previous: rec.last_terminal.get("z"),
        },
        now(),
    );
    assert_eq!(
        e.outcome,
        Outcome::Fallback {
            reason: "after local local-build/qwen@hosta stopped: stopped by user".into()
        }
    );
}
