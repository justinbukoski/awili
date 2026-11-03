//! P10/F4 acceptance proofs: the durable `submitting` row is enqueued BEFORE
//! the submit, and a submit whose same-id `status` fallback also fails stays
//! as an `unknown` row on the SAME request id (never a fresh submit).
//!
//! Reuses the fake-runner fixtures from [`crate::build_dispatch_tests`].

#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

use awili_dispatch::protocol::BuildState;

use crate::build_dispatch::{
    build_request, close_row, enqueue_submitting, record_submit_outcome, ResolvedBuild,
    STATE_SUBMITTING, STATE_UNKNOWN,
};
use crate::build_dispatch_tests::{fake_ssh, input, snapshot, targets, SCRIPT_FIXTURE};
use crate::persistence::{PersistCmd, Persistence};

/// A fake persistence handle with a channel the test drains synchronously. The
/// serialized worker thread is not needed to observe the ENQUEUE order — which
/// is exactly the invariant (`PersistCmd`s are queued in call order).
pub(crate) fn recording_persist() -> (Persistence, std::sync::mpsc::Receiver<PersistCmd>) {
    let (tx, rx) = std::sync::mpsc::channel();
    (
        Persistence {
            tx,
            failed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
        rx,
    )
}

/// A fake `ssh` where EVERY operation exits nonzero: a submit and its same-id
/// `status` fallback both fail (the P10/F4 uncertain window).
fn fake_ssh_always_fails(root: &Path, exit: i32) -> PathBuf {
    let script = root.join("ssh");
    // RC1-G2: never `fs::write` an executable a test will exec (ETXTBSY).
    crate::exec_fixture::write_exec_fixture(
        &script,
        &format!(
            r#"#!/bin/sh
for arg do operation="$arg"; done
printf '%s\n' "$operation" >> '{0}/calls'
printf '%s\n' "$@" >> '{0}/argv'
cat > '{0}/last-request'
exit {1}
"#,
            root.display(),
            exit
        ),
        0o755,
    );
    script
}

/// The durable SUBMITTING row is enqueued BEFORE the dispatcher is called: at
/// the moment the fake dispatcher runs, the row is already on the queue.
#[test]
fn build_row_persisted_before_submit() {
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let resolved = ResolvedBuild::resolve_with(&targets(), "hosta", fake_ssh(root, 0)).unwrap();
    let request = build_request(&resolved, &input("turn-1", "task".into())).unwrap();
    fs::write(
        root.join("snapshot.json"),
        serde_json::to_vec(&snapshot(&request, BuildState::Queued)).unwrap(),
    )
    .unwrap();
    let (persist, rx) = recording_persist();

    let mut order: Vec<&str> = Vec::new();
    enqueue_submitting(Some(&persist), &request, &resolved.target_id);
    order.push("persist:submitting");
    // The fake dispatcher: the SUBMITTING command must already be queued.
    match rx.try_recv().expect("the pre-submit row is queued") {
        PersistCmd::UpsertBuildJob {
            state,
            request_id,
            revision,
            failure,
            ..
        } => {
            assert_eq!(state, STATE_SUBMITTING);
            assert_eq!(request_id, request.request_id);
            assert_eq!(revision, 0, "no runner revision exists yet");
            assert!(failure.is_none());
        }
        _ => panic!("expected the SUBMITTING UpsertBuildJob"),
    }
    order.push("submit");
    let outcome = resolved.submit(&request);
    assert!(outcome.is_ok(), "{outcome:?}");
    order.push("persist:outcome");
    record_submit_outcome(Some(&persist), &request, &resolved.target_id, &outcome);
    assert_eq!(
        order,
        ["persist:submitting", "submit", "persist:outcome"],
        "the durable row is enqueued before the submit"
    );
    assert!(matches!(
        rx.try_recv(),
        Ok(PersistCmd::UpsertBuildJob { state, .. }) if state == "queued"
    ));
    assert_eq!(fs::read_to_string(root.join("calls")).unwrap(), "submit\n");
}

/// Both the submit and its same-id `status` fallback fail: the row stays
/// `unknown` on the SAME request id with the error text, and reconciliation
/// (a later Send / restart reattach) is `status` of that id — no fresh submit.
#[test]
fn submit_and_status_both_fail_keeps_row_same_id() {
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let resolved =
        ResolvedBuild::resolve_with(&targets(), "hosta", fake_ssh_always_fails(root, 255)).unwrap();
    let request = build_request(&resolved, &input("turn-1", "task".into())).unwrap();
    let (persist, rx) = recording_persist();

    enqueue_submitting(Some(&persist), &request, &resolved.target_id);
    let outcome = resolved.submit(&request);
    assert!(
        outcome.is_err(),
        "submit and its same-id status fallback both fail"
    );
    record_submit_outcome(Some(&persist), &request, &resolved.target_id, &outcome);

    // Drop the SUBMITTING command, then read the UNKNOWN outcome.
    assert!(matches!(
        rx.try_recv(),
        Ok(PersistCmd::UpsertBuildJob { state, .. }) if state == STATE_SUBMITTING
    ));
    match rx.try_recv().expect("the unknown outcome row is queued") {
        PersistCmd::UpsertBuildJob {
            state,
            request_id,
            revision,
            failure,
            ..
        } => {
            assert_eq!(state, STATE_UNKNOWN);
            assert_eq!(request_id, request.request_id, "the same id is retained");
            assert_eq!(revision, 0);
            assert!(
                failure
                    .as_deref()
                    .is_some_and(|f| f.contains("submission uncertain")),
                "the submit error text is persisted: {failure:?}"
            );
        }
        _ => panic!("expected the UNKNOWN UpsertBuildJob"),
    }

    // Reconciliation is `status` of the SAME id: the retry adds no submit.
    let _ = resolved.status(&request.request_id);
    assert_eq!(
        fs::read_to_string(root.join("calls")).unwrap(),
        "submit\nstatus\nstatus\n",
        "one submit; every retry is a status of the same id"
    );
}

/// P2f: closing a row is ONE upsert on the SAME turn and request id, to the
/// terminal `needs_attention` token, carrying the app's `stale: ` verdict.
#[test]
fn close_row_writes_terminal_needs_attention() {
    let (persist, rx) = recording_persist();
    close_row(
        Some(&persist),
        "turn-1",
        "req-1",
        "hosta",
        4,
        "stale: gone".into(),
    );
    match rx.try_recv().expect("one close command") {
        PersistCmd::UpsertBuildJob {
            turn_id,
            request_id,
            target_id,
            revision,
            state,
            failure,
            ..
        } => {
            assert_eq!((turn_id.as_str(), request_id.as_str()), ("turn-1", "req-1"));
            assert_eq!((target_id.as_str(), revision), ("hosta", 4));
            assert_eq!(state, "needs_attention");
            assert!(failure.is_some_and(|f| f.starts_with("stale: ")));
        }
        _ => panic!("expected an UpsertBuildJob"),
    }
    assert!(rx.try_recv().is_err(), "exactly one command");
}

/// P2f: a closed row is terminal on disk — a real `Store` no longer returns
/// it from `open_build_jobs`, so no relaunch reattaches it.
#[test]
fn closed_row_is_not_reopened() {
    let temp = tempfile::tempdir().unwrap();
    let store = awili_persist::Store::open(&temp.path().join("awili.db")).unwrap();
    let apply = |cmd: PersistCmd| match cmd {
        PersistCmd::UpsertBuildJob {
            turn_id,
            request_id,
            target_id,
            revision,
            state,
            result_sha,
            failure,
            updated_at,
        } => store
            .upsert_build_job(&awili_persist::build_jobs::BuildJobRow {
                turn_id,
                request_id,
                target_id,
                revision,
                state,
                result_sha,
                failure,
                updated_at,
            })
            .unwrap(),
        _ => panic!("expected an UpsertBuildJob"),
    };
    let (persist, rx) = recording_persist();
    let resolved = ResolvedBuild::resolve_with(&targets(), "hosta", "ssh".into()).unwrap();
    let request = build_request(&resolved, &input("turn-1", "task".into())).unwrap();
    enqueue_submitting(Some(&persist), &request, "hosta");
    apply(rx.try_recv().unwrap());
    assert_eq!(store.open_build_jobs().unwrap().len(), 1, "open before");
    close_row(
        Some(&persist),
        "turn-1",
        &request.request_id,
        "hosta",
        0,
        "stale: gone".into(),
    );
    apply(rx.try_recv().unwrap());
    assert!(
        store.open_build_jobs().unwrap().is_empty(),
        "closed for good"
    );
    let row = store.build_job_for_turn("turn-1").unwrap().unwrap();
    assert_eq!(row.request_id, request.request_id, "same row, same id");
}
