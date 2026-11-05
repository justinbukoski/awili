//! P2f required proof on the real `AwiliApp` (GPUI): Cancel run on a local
//! build stops its poll, closes its row once and records `Stopped` once; a
//! stale verdict after that writes no second line; a stale live run records
//! `Failed { stale: … }` once; an orphaned row closes at reattach; a terminal
//! initial snapshot never enters polling (round 2); an orphaned first row
//! does not keep a valid later row from driving the panel (round 3).
//!
//! No real ssh: the `isolated` harness cannot host a polling local build
//! without a runner, so each test seeds `WorkState` the way the
//! submit/reattach paths leave it (declared in P2f-DEVIATIONS.md). The round-2
//! test drives the real fresh-submit task against the fake `ssh` runner.

use super::gateway_queue_tests::{isolated, CONV};
use super::*;
use crate::build_staleness::{LocalBuildHandle, CANCEL_REASON};
use crate::persistence::PersistCmd;
use awili_selftest::graduation::{Ledger, LedgerEntry, Outcome};

type Rx = std::sync::mpsc::Receiver<PersistCmd>;

/// A Work session whose persistence is a recording queue and whose ledger
/// lives in the private home.
fn wire(app: &mut AwiliApp, home: &std::path::Path) -> Rx {
    let (persist, rx) = crate::build_dispatch_p10_tests::recording_persist();
    app.persist = Some(persist);
    app.work
        .graduation
        .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
    app.views.insert(
        CONV.into(),
        crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
    );
    app.set_active_conversation(CONV);
    app.work.conv_id = Some(CONV.into());
    rx
}

/// A live panel-driving local build: running, armed, with a handle whose
/// stop flag is the run's cancel flag. Returns the persist queue, the stop
/// flag and the run id.
fn seed(app: &mut AwiliApp, home: &std::path::Path) -> (Rx, Arc<AtomicBool>, u64) {
    let rx = wire(app, home);
    app.work.run_seq += 1;
    app.work.running = true;
    app.work.panel.requested_identity = Some(WorkRunIdentity::LocalBuild {
        model: "qwen".into(),
        host: "hosta".into(),
    });
    app.work.graduation.arm(CONV);
    let stop = Arc::new(AtomicBool::new(false));
    app.work.cancel = stop.clone();
    app.work.local_build = Some(LocalBuildHandle {
        turn_id: "turn-1".into(),
        request_id: "req-1".into(),
        target_id: "hosta".into(),
        revision: 7,
        stop: stop.clone(),
    });
    (rx, stop, app.work.run_seq)
}

fn ledger(home: &std::path::Path) -> Vec<LedgerEntry> {
    Ledger::read(&home.join("ledger.jsonl")).unwrap_or_default()
}

/// Every build-row write on the queue as `(turn, state, revision, failure)`.
fn build_writes(rx: &Rx) -> Vec<(String, String, u64, Option<String>)> {
    rx.try_iter()
        .filter_map(|cmd| match cmd {
            PersistCmd::UpsertBuildJob {
                turn_id,
                state,
                revision,
                failure,
                ..
            } => Some((turn_id, state, revision, failure)),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn cancel_closes_row_and_records_once(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::build_staleness_tests::cancel_closes_row_and_records_once")
    else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (rx, stop, _) = seed(app, &home);
        app.cancel_work_run(cx);
        assert!(stop.load(Ordering::SeqCst), "the poll task is told to stop");
        assert!(!app.work.running);
        assert!(app.work.local_build.is_none());
        assert_eq!(
            build_writes(&rx),
            vec![(
                "turn-1".to_string(),
                "needs_attention".to_string(),
                7,
                Some(CANCEL_REASON.to_string())
            )],
            "one close, at the last observed revision"
        );
        let lines = ledger(&home);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].outcome, Outcome::Stopped);
    });
}

#[gpui::test]
fn stale_after_cancel_writes_no_second_line(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::build_staleness_tests::stale_after_cancel_writes_no_second_line")
    else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (rx, _, run_id) = seed(app, &home);
        app.cancel_work_run(cx);
        // A poll task that raced the Cancel reaches its stale verdict, then
        // the user clicks Cancel run again: neither writes anything.
        app.fail_stale_local_build(run_id, "stale: late".into(), cx);
        app.cancel_work_run(cx);
        assert_eq!(build_writes(&rx).len(), 1, "the row is closed once");
        let lines = ledger(&home);
        assert_eq!(lines.len(), 1, "still the one Stopped line");
        assert_eq!(lines[0].outcome, Outcome::Stopped);
    });
}

#[gpui::test]
fn stale_live_run_records_failed_once(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::build_staleness_tests::stale_live_run_records_failed_once")
    else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (rx, _, run_id) = seed(app, &home);
        let reason = "stale: build host unreachable for 10m after 3 failed status polls \
                      (last error: timeout)";
        app.fail_stale_local_build(run_id, reason.into(), cx);
        assert!(!app.work.running);
        assert!(app.work.local_build.is_none());
        assert_eq!(app.work.panel.status, Some(RunStatus::Failed));
        assert_eq!(app.work.panel.error.as_deref(), Some(reason));
        // A later Cancel run has nothing live to stop or record.
        app.cancel_work_run(cx);
        assert!(build_writes(&rx).is_empty(), "the poll task closed the row");
        let lines = ledger(&home);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].outcome,
            Outcome::Failed {
                reason: reason.into()
            }
        );
    });
}

#[gpui::test]
fn orphaned_row_closes_at_reattach(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::build_staleness_tests::orphaned_row_closes_at_reattach")
    else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let rx = wire(app, &home);
        app.work.build_targets.clear();
        app.work.open_builds = vec![awili_persist::build_jobs::BuildJobRow {
            turn_id: "turn-9".into(),
            request_id: "req-9".into(),
            target_id: "gone".into(),
            revision: 2,
            state: "running".into(),
            result_sha: None,
            failure: None,
            updated_at: 1,
        }];
        app.reattach_open_builds(cx);
        let reason = "stale: build target 'gone' is no longer configured";
        assert_eq!(
            build_writes(&rx),
            vec![(
                "turn-9".to_string(),
                "needs_attention".to_string(),
                2,
                Some(reason.to_string())
            )]
        );
        assert!(!app.work.running, "an orphan never drives the panel");
        let lines = ledger(&home);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].model, "unresolved@gone");
        assert_eq!(
            lines[0].outcome,
            Outcome::Failed {
                reason: reason.into()
            }
        );
    });
}

/// P2f round 3 (R2): the panel-driving row is the first open row whose target
/// resolves. An orphan sorted first is closed and skipped; the valid row after
/// it still gets the panel identity, the Cancel handle and the armed recorder,
/// so Cancel run closes THAT row.
#[gpui::test]
fn orphan_first_row_does_not_block_the_driver(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::build_staleness_tests::orphan_first_row_does_not_block_the_driver")
    else {
        return;
    };
    let row = |turn: &str, target: &str, revision: u64, updated_at: i64| {
        awili_persist::build_jobs::BuildJobRow {
            turn_id: turn.into(),
            request_id: format!("req-{turn}"),
            target_id: target.into(),
            revision,
            state: "running".into(),
            result_sha: None,
            failure: None,
            updated_at,
        }
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let rx = wire(app, &home);
        app.work.build_targets = crate::build_dispatch_tests::targets();
        // Newest first after the reattach sort: the orphan is index 0.
        app.work.open_builds = vec![row("turn-v", "hosta", 3, 1), row("turn-o", "gone", 2, 2)];
        app.reattach_open_builds(cx);
        let orphan = "stale: build target 'gone' is no longer configured";
        assert_eq!(
            build_writes(&rx),
            vec![(
                "turn-o".to_string(),
                "needs_attention".to_string(),
                2,
                Some(orphan.to_string())
            )],
            "only the orphan is closed at reattach"
        );
        let lines = ledger(&home);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].model, "unresolved@gone");
        // The valid row drives the panel.
        assert!(app.work.running, "the valid row drives the running state");
        assert_eq!(
            app.work.panel.requested_identity,
            Some(WorkRunIdentity::LocalBuild {
                model: "qwen3".into(),
                host: "configured-host".into(),
            })
        );
        assert_eq!(
            app.work.local_build.as_ref().map(|h| h.turn_id.as_str()),
            Some("turn-v"),
            "the Cancel handle is the valid row's"
        );
        assert!(app.work.graduation.is_armed(), "the recorder is armed");
        // Cancel run closes THAT row, once, and records the Stop.
        app.cancel_work_run(cx);
        assert!(app.work.local_build.is_none());
        assert_eq!(
            build_writes(&rx),
            vec![(
                "turn-v".to_string(),
                "needs_attention".to_string(),
                3,
                Some(CANCEL_REASON.to_string())
            )]
        );
        let lines = ledger(&home);
        assert_eq!(lines.len(), 2, "the orphan line and the one Stopped line");
        assert_eq!(lines[1].outcome, Outcome::Stopped);
    });
}

/// P2f round 2 (Codex review 1): a submit whose snapshot is already terminal
/// is finalized at once. The drive never polls `status` afterwards, so no
/// stale close can overwrite the closed row, and the ledger keeps the one
/// original outcome.
#[gpui::test]
fn terminal_initial_snapshot_skips_polling(cx: &mut gpui::TestAppContext) {
    use crate::build_dispatch::{build_request, ResolvedBuild, POLL_INTERVAL};
    use crate::build_dispatch_tests::{fake_ssh, input, snapshot, targets, SCRIPT_FIXTURE};
    use awili_dispatch::protocol::BuildState;

    let Some(home) =
        isolated("work::build_staleness_tests::terminal_initial_snapshot_skips_polling")
    else {
        return;
    };
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let runner = home.join("runner");
    std::fs::create_dir_all(&runner).unwrap();
    let resolved = ResolvedBuild::resolve_with(&targets(), "hosta", fake_ssh(&runner, 0)).unwrap();
    let request = build_request(&resolved, &input("turn-1", "task".into())).unwrap();
    let sha = "b".repeat(40);
    let mut done = snapshot(&request, BuildState::AwaitingReview);
    done.revision = 4;
    done.result_sha = Some(sha.clone());
    std::fs::write(
        runner.join("snapshot.json"),
        serde_json::to_vec(&done).unwrap(),
    )
    .unwrap();

    let entity = cx.new(AwiliApp::new);
    let rx = entity.update(cx, |app, cx| {
        // The state `run_work_via_local_build` leaves before its drive.
        let rx = wire(app, &home);
        app.work.run_seq += 1;
        let run_id = app.work.run_seq;
        app.work.running = true;
        app.work.panel.requested_identity = Some(WorkRunIdentity::LocalBuild {
            model: "qwen3".into(),
            host: "configured-host".into(),
        });
        app.work.graduation.arm(CONV);
        let stop = Arc::new(AtomicBool::new(false));
        app.work.cancel = stop.clone();
        let key = LocalBuildHandle {
            turn_id: "turn-1".into(),
            request_id: request.request_id.clone(),
            target_id: "hosta".into(),
            revision: 0,
            stop,
        };
        app.work.local_build = Some(key.clone());
        app.drive_local_build(run_id, resolved, request, key, cx);
        rx
    });
    cx.run_until_parked();
    assert!(
        !runner.join("calls").exists(),
        "runner waits for commit acknowledgement"
    );
    let reply = match rx.try_recv().expect("tracking row queued") {
        PersistCmd::CommitBuildJob { row, reply } => {
            assert_eq!(row.state, "submitting");
            reply
        }
        _ => panic!("expected commit request"),
    };
    reply.send_blocking(Ok(())).unwrap();
    cx.run_until_parked();
    // Well past the poll interval (and many polls' worth of it).
    for _ in 0..5 {
        cx.executor().advance_clock(POLL_INTERVAL * 2);
        cx.run_until_parked();
    }

    assert_eq!(
        std::fs::read_to_string(runner.join("calls")).unwrap(),
        "submit\n",
        "zero status calls after a terminal submit"
    );
    let writes: Vec<_> = rx
        .try_iter()
        .filter_map(|cmd| match cmd {
            PersistCmd::UpsertBuildJob {
                state, result_sha, ..
            } => Some((state, result_sha)),
            _ => None,
        })
        .collect();
    assert_eq!(
        writes,
        vec![("awaiting_review".to_string(), Some(sha))],
        "the row keeps its terminal state and result_sha; no stale close"
    );
    entity.update(cx, |app, _| {
        assert!(!app.work.running);
        assert!(app.work.local_build.is_none());
        assert_eq!(app.work.panel.status, Some(RunStatus::Completed));
    });
    let lines = ledger(&home);
    assert_eq!(lines.len(), 1, "one ledger line");
    assert_eq!(lines[0].outcome, Outcome::Success);
}

#[gpui::test]
fn rejected_tracking_commit_never_invokes_runner(cx: &mut gpui::TestAppContext) {
    use crate::build_dispatch::{build_request, ResolvedBuild};
    use crate::build_dispatch_tests::{fake_ssh, input, targets, SCRIPT_FIXTURE};
    let Some(home) =
        isolated("work::build_staleness_tests::rejected_tracking_commit_never_invokes_runner")
    else {
        return;
    };
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let runner = home.join("runner-failed-commit");
    std::fs::create_dir_all(&runner).unwrap();
    let resolved = ResolvedBuild::resolve_with(&targets(), "stryx", fake_ssh(&runner, 0)).unwrap();
    let request = build_request(&resolved, &input("turn-failed", "task".into())).unwrap();
    let entity = cx.new(AwiliApp::new);
    let rx = entity.update(cx, |app, cx| {
        let rx = wire(app, &home);
        app.work.run_seq += 1;
        let run_id = app.work.run_seq;
        app.work.running = true;
        let key = LocalBuildHandle {
            turn_id: request.turn_id.clone(),
            request_id: request.request_id.clone(),
            target_id: resolved.target_id.clone(),
            revision: 0,
            stop: Arc::new(AtomicBool::new(false)),
        };
        app.work.local_build = Some(key.clone());
        app.drive_local_build(run_id, resolved, request, key, cx);
        rx
    });
    cx.run_until_parked();
    assert!(!runner.join("calls").exists());
    match rx.try_recv().unwrap() {
        PersistCmd::CommitBuildJob { reply, .. } => {
            reply.send_blocking(Err("disk full".into())).unwrap()
        }
        _ => panic!("expected tracking commit"),
    }
    cx.run_until_parked();
    assert!(!runner.join("calls").exists());
    entity.read_with(cx, |app, _| {
        assert!(!app.work.running);
        assert!(app
            .work
            .panel
            .error
            .as_deref()
            .unwrap()
            .contains("disk full"));
    });
}

#[gpui::test]
fn residency_refusal_finishes_and_persists_accepted_turn(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::build_staleness_tests::residency_refusal_finishes_and_persists_accepted_turn",
    ) else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    let rx = entity.update(cx, |app, cx| {
        let rx = wire(app, &home);
        app.work.running = true;
        app.work_push(crate::Role::Assistant, String::new());
        app.work.stream_row_idx = Some(0);
        let engine = app.guard_engine();
        app.finish_guard(
            Some(("Fixture residency refusal".into(), None)),
            engine,
            Arc::new(AtomicBool::new(false)),
            |_, _| panic!("must not dispatch"),
            cx,
        );
        assert!(!app.work.running);
        assert!(app.work.stream_row_idx.is_none());
        assert_eq!(app.work.panel.status, Some(RunStatus::Failed));
        assert!(app.views[CONV]
            .convo
            .message(0)
            .unwrap()
            .text
            .contains("Fixture residency refusal"));
        rx
    });
    assert_eq!(rx.try_iter().filter(|cmd| matches!(cmd, PersistCmd::Append { text, .. } if text.contains("Fixture residency refusal"))).count(), 1);
}

#[gpui::test]
fn stale_failed_commit_cannot_fail_a_newer_run(cx: &mut gpui::TestAppContext) {
    use crate::build_dispatch::{build_request, ResolvedBuild};
    use crate::build_dispatch_tests::{fake_ssh, input, targets, SCRIPT_FIXTURE};
    let Some(home) =
        isolated("work::build_staleness_tests::stale_failed_commit_cannot_fail_a_newer_run")
    else {
        return;
    };
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let runner = home.join("stale-runner");
    std::fs::create_dir_all(&runner).unwrap();
    let resolved = ResolvedBuild::resolve_with(&targets(), "stryx", fake_ssh(&runner, 0)).unwrap();
    let request = build_request(&resolved, &input("turn-old", "task".into())).unwrap();
    let entity = cx.new(AwiliApp::new);
    let rx = entity.update(cx, |app, cx| {
        let rx = wire(app, &home);
        app.work.run_seq += 1;
        let run_id = app.work.run_seq;
        app.work.running = true;
        let key = LocalBuildHandle {
            turn_id: request.turn_id.clone(),
            request_id: request.request_id.clone(),
            target_id: resolved.target_id.clone(),
            revision: 0,
            stop: Arc::new(AtomicBool::new(false)),
        };
        app.work.local_build = Some(key.clone());
        app.drive_local_build(run_id, resolved, request, key, cx);
        rx
    });
    cx.run_until_parked();
    let reply = match rx.try_recv().unwrap() {
        PersistCmd::CommitBuildJob { reply, .. } => reply,
        _ => panic!("expected tracking commit"),
    };
    entity.update(cx, |app, _| {
        app.work.run_seq += 1;
        app.work.panel.error = None;
        app.work.running = true;
    });
    reply.send_blocking(Err("old disk failure".into())).unwrap();
    cx.run_until_parked();
    assert!(!runner.join("calls").exists());
    entity.read_with(cx, |app, _| {
        assert!(app.work.running);
        assert!(app.work.panel.error.is_none());
    });
}

#[gpui::test]
fn missing_storage_never_invokes_build_runner(cx: &mut gpui::TestAppContext) {
    use crate::build_dispatch::{build_request, ResolvedBuild};
    use crate::build_dispatch_tests::{fake_ssh, input, targets, SCRIPT_FIXTURE};
    let Some(home) =
        isolated("work::build_staleness_tests::missing_storage_never_invokes_build_runner")
    else {
        return;
    };
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let runner = home.join("no-storage-runner");
    std::fs::create_dir_all(&runner).unwrap();
    let resolved = ResolvedBuild::resolve_with(&targets(), "stryx", fake_ssh(&runner, 0)).unwrap();
    let request = build_request(&resolved, &input("turn-no-store", "task".into())).unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let _ = wire(app, &home);
        app.persist = None;
        app.work.run_seq += 1;
        app.work.running = true;
        let key = LocalBuildHandle {
            turn_id: request.turn_id.clone(),
            request_id: request.request_id.clone(),
            target_id: resolved.target_id.clone(),
            revision: 0,
            stop: Arc::new(AtomicBool::new(false)),
        };
        app.work.local_build = Some(key.clone());
        app.drive_local_build(app.work.run_seq, resolved, request, key, cx);
    });
    cx.run_until_parked();
    assert!(!runner.join("calls").exists());
    entity.read_with(cx, |app, _| {
        assert!(!app.work.running);
        assert!(app
            .work
            .panel
            .error
            .as_deref()
            .unwrap()
            .contains("Storage is unavailable"));
    });
}

#[gpui::test]
fn nonwarm_worker_construction_failure_finishes_accepted_turn(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::build_staleness_tests::nonwarm_worker_construction_failure_finishes_accepted_turn",
    ) else {
        return;
    };
    let root = engine_data_root().unwrap();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("logs"), b"fixture blocks directory creation").unwrap();
    let entity = cx.new(AwiliApp::new);
    let rx = entity.update(cx, |app, cx| {
        let rx = wire(app, &home);
        app.work.engine.kind = awili_config::EngineKind::Awili;
        app.work.running = true;
        app.work_push(crate::Role::Assistant, String::new());
        app.work.stream_row_idx = Some(0);
        assert!(!app.spawn_work_worker(
            GatewayCmd::Warm { system: None },
            None,
            CONV.into(),
            Arc::new(AtomicBool::new(false)),
            false,
            cx
        ));
        assert!(!app.work.running);
        assert!(app.work.stream_row_idx.is_none());
        assert_eq!(app.work.panel.status, Some(RunStatus::Failed));
        assert!(app.views[CONV]
            .convo
            .message(0)
            .unwrap()
            .text
            .contains("cannot create"));
        rx
    });
    assert_eq!(rx.try_iter().filter(|cmd| matches!(cmd, PersistCmd::Append { text, .. } if text.contains("cannot create"))).count(), 1);
}
