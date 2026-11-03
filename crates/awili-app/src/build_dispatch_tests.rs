//! Daily-driver P2d acceptance proofs for the local Build dispatch core
//! (`crate::build_dispatch`): fake `ssh` runner capturing argv and the stdin
//! JSON, against a real `awili_persist` store.
//!
//! Runs serialized (one fake-runner fixture at a time) — see
//! `SCRIPT_FIXTURE` below.

#![forbid(unsafe_code)]

use std::fs;

use awili_config::BuildTargetConfig;
use awili_dispatch::protocol::{BuildRequest, BuildSnapshot, BuildState};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::build_dispatch::{
    build_request, dispatches_for, state_token, BuildDispatchError, BuildInputs, ResolvedBuild,
    RUNNER_CONFIG_PATH,
};
use crate::work::WorkExecutor;

/// Serializes fake-runner fixture creation/spawn (a Linux fork can
/// inherit a sibling test's writable script descriptor until exec).
pub(crate) static SCRIPT_FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn targets() -> BTreeMap<String, BuildTargetConfig> {
    BTreeMap::from([(
        "hosta".to_string(),
        BuildTargetConfig {
            ssh_host: "configured-host".to_string(),
            runner_path: "/opt/awili/runner".to_string(),
            repo_id: "repo-a".to_string(),
            model: "qwen3".to_string(),
            verify_profile: "standard".to_string(),
        },
    )])
}

pub(crate) fn input(turn: &str, spec: String) -> BuildInputs {
    BuildInputs {
        turn_id: turn.to_string(),
        conversation_id: "conv-1".to_string(),
        spec_text: spec,
        baseline_sha: "a".repeat(40),
        branch: "fix/p2d-fixture".to_string(),
    }
}

pub(crate) fn snapshot(request: &BuildRequest, state: BuildState) -> BuildSnapshot {
    BuildSnapshot {
        request_id: request.request_id.clone(),
        request_sha256: request.fingerprint(),
        revision: 1,
        state,
        executor: "/opt/awili/dsh".to_string(),
        requested_model: "qwen3".to_string(),
        baseline_sha: request.baseline_sha.clone(),
        result_sha: None,
        remote_sha: None,
        model_exit: None,
        verify_exit: None,
        push_exit: None,
        worker: None,
        failure: None,
        no_op: false,
    }
}

/// A fake `ssh` executable: records each operation, argv, and the stdin
/// JSON to `root`, answers from `root/snapshot.json`, and exits with
/// `submit_exit` for `submit` (the lost-ack case).
pub(crate) fn fake_ssh(root: &Path, submit_exit: i32) -> PathBuf {
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
case "$operation" in submit) [ {1} -ne 0 ] && exit {1}; cat '{0}/snapshot.json';; status) cat '{0}/snapshot.json';; *) exit 17;; esac
"#,
            root.display(),
            submit_exit
        ),
        0o755,
    );
    script
}

fn job(turn: &str, request_id: &str, state: &str) -> awili_persist::build_jobs::BuildJobRow {
    awili_persist::build_jobs::BuildJobRow {
        turn_id: turn.into(),
        request_id: request_id.into(),
        target_id: "hosta".into(),
        revision: 1,
        state: state.into(),
        result_sha: None,
        failure: None,
        updated_at: 1000,
    }
}

/// Resolves the configured target against a fake runner, stages a
/// Queued snapshot, and submits ONCE.
fn submit_once(root: &Path, spec: &str) -> (BuildRequest, BuildSnapshot) {
    let resolved = ResolvedBuild::resolve_with(&targets(), "hosta", fake_ssh(root, 0)).unwrap();
    let request = build_request(&resolved, &input("turn-1", spec.to_string())).unwrap();
    fs::write(
        root.join("snapshot.json"),
        serde_json::to_vec(&snapshot(&request, BuildState::Queued)).unwrap(),
    )
    .unwrap();
    let snap = resolved.submit(&request).unwrap();
    (request, snap)
}

/// Build invokes the runner with the configured target binding: the argv
/// carries host/runner/runner-config/operation, the stdin JSON carries
/// the target id, verify profile, repo id, and the literal spec — with
/// the configured model on the accepted snapshot.
#[test]
fn build_invokes_with_configured_target_and_model() {
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (request, snap) = submit_once(root, "make it so $(echo x)");
    assert_eq!(snap.request_id, request.request_id);
    assert_eq!(
        snap.requested_model, "qwen3",
        "configured model on the snapshot"
    );
    let sent: BuildRequest =
        serde_json::from_slice(&fs::read(root.join("last-request")).unwrap()).unwrap();
    assert_eq!(sent, request);
    assert_eq!(
        (
            sent.target_id.as_str(),
            sent.verify_profile.as_str(),
            sent.repo_id.as_str()
        ),
        ("hosta", "standard", "repo-a")
    );
    let argv = fs::read_to_string(root.join("argv")).unwrap();
    assert!(
        argv.contains("configured-host")
            && argv.contains("/opt/awili/runner")
            && argv.contains(RUNNER_CONFIG_PATH),
        "{argv}"
    );
    assert_eq!(fs::read_to_string(root.join("calls")).unwrap(), "submit\n");
}

/// Ask/Plan/Review (the gateway and agent lanes) never reach the
/// dispatcher: only the manual LocalBuild override does.
#[test]
fn other_executors_never_reach_the_dispatcher() {
    assert!(dispatches_for(&WorkExecutor::LocalBuild {
        target_id: "hosta".into()
    }));
    assert!(!dispatches_for(&WorkExecutor::Gateway));
    assert!(!dispatches_for(&WorkExecutor::Agent {
        engine: awili_code::CodeEngine::Claude,
        model: Some("sonnet".into()),
    }));
    assert!(!dispatches_for(&WorkExecutor::Agent {
        engine: awili_code::CodeEngine::Glm,
        model: None,
    }));
}

/// Shell metacharacters in the spec text arrive as DATA: the runner sees
/// the literal string in the stdin JSON, and nothing is shell-evaluated
/// on the app side.
#[test]
fn shell_metacharacters_in_spec_text_arrive_as_data() {
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let spec = format!(
        "$(touch {}/NEVER); `touch {}/NEVER2` && rm -rf /",
        root.display(),
        root.display()
    );
    let (request, _) = submit_once(root, &spec);
    assert_eq!(request.spec_text, spec, "spec must round-trip literally");
    assert!(
        !root.join("NEVER").exists() && !root.join("NEVER2").exists(),
        "spec text must never be shell-evaluated"
    );
}

/// An unknown target id is a config error naming the id, making ZERO
/// outbound calls (the fake runner is never even invoked).
#[test]
fn unknown_target_id_makes_zero_outbound_calls() {
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let err = match ResolvedBuild::resolve_with(&targets(), "missing", fake_ssh(root, 0)) {
        Ok(_) => panic!("unknown target must not resolve"),
        Err(e) => e,
    };
    match err {
        BuildDispatchError::UnknownTarget(id) => assert_eq!(id, "missing"),
        other => panic!("expected UnknownTarget, got {other:?}"),
    }
    assert!(
        !root.join("calls").exists(),
        "zero outbound calls for an unknown target"
    );
}

/// A lost submit acknowledgement: exactly one submit, reconciled by
/// `status` of the SAME request id — one worker, one persisted row.
#[test]
fn lost_acknowledgement_reconciles_same_request_id() {
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let resolved = ResolvedBuild::resolve_with(&targets(), "hosta", fake_ssh(root, 255)).unwrap();
    let request = build_request(&resolved, &input("turn-1", "task".into())).unwrap();
    fs::write(
        root.join("snapshot.json"),
        serde_json::to_vec(&snapshot(&request, BuildState::Queued)).unwrap(),
    )
    .unwrap();
    let snap = resolved
        .submit(&request)
        .expect("a lost ack reconciles by status of the same id");
    assert_eq!(snap.request_id, request.request_id);
    assert_eq!(
        fs::read_to_string(root.join("calls")).unwrap(),
        "submit\nstatus\n",
        "one submit; the retry is a status of the SAME id"
    );
    let store = awili_persist::Store::open(&root.join("awili.db")).unwrap();
    store
        .upsert_build_job(&job("turn-1", &snap.request_id, state_token(snap.state)))
        .unwrap();
    let row = store
        .build_job_for_turn("turn-1")
        .unwrap()
        .expect("the durable row exists before any Running state");
    assert_eq!(row.request_id, request.request_id);
    assert_eq!(store.open_build_jobs().unwrap().len(), 1);
}

/// A response with a conflicting fingerprint fails — and nothing is
/// accepted (the persist layer rejects a second row for that id too).
#[test]
fn conflicting_response_fails_without_second_row() {
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let resolved = ResolvedBuild::resolve_with(&targets(), "hosta", fake_ssh(root, 0)).unwrap();
    let request = build_request(&resolved, &input("turn-1", "task".into())).unwrap();
    let mut forged = snapshot(&request, BuildState::Queued);
    forged.request_sha256 = "conflicting".into();
    fs::write(
        root.join("snapshot.json"),
        serde_json::to_vec(&forged).unwrap(),
    )
    .unwrap();
    assert!(
        resolved.submit(&request).is_err(),
        "a conflicting response can never be accepted"
    );
    let store = awili_persist::Store::open(&root.join("awili.db")).unwrap();
    store
        .upsert_build_job(&job("turn-1", &request.request_id, "queued"))
        .unwrap();
    assert!(
        store
            .upsert_build_job(&job("turn-2", &request.request_id, "queued"))
            .is_err(),
        "same request id under another turn: no second row"
    );
    assert_eq!(store.open_build_jobs().unwrap().len(), 1);
}

/// Nonzero model exit, verify failure, push failure, remote SHA
/// mismatch, vanished worker, and unconfirmed cancel: each is terminal,
/// persists as its NON-SUCCESS BuildState token, and none is (or could
/// be) Completed — no frontier or cloud fallback exists.
#[test]
fn non_success_states_persist_never_completed() {
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let resolved = ResolvedBuild::resolve_with(&targets(), "hosta", fake_ssh(root, 0)).unwrap();
    let request = build_request(&resolved, &input("turn-1", "task".into())).unwrap();
    let store = awili_persist::Store::open(&root.join("awili.db")).unwrap();
    let cases: Vec<(BuildState, &'static str)> = vec![
        (BuildState::Failed, "model exited nonzero (1)"),
        (
            BuildState::Failed,
            "independent verification failed (exit 2)",
        ),
        (BuildState::Failed, "push failed (exit 128)"),
        (BuildState::Failed, "remote SHA mismatch: result != remote"),
        (
            BuildState::NeedsAttention,
            "worker vanished; liveness unprovable",
        ),
        (
            BuildState::CancelRequested,
            "cancel not confirmed at the containment boundary",
        ),
    ];
    for (i, (state, failure)) in cases.iter().enumerate() {
        let mut snap = snapshot(&request, *state);
        snap.model_exit = Some(1);
        snap.verify_exit = Some(2);
        snap.push_exit = Some(128);
        snap.failure = Some(failure.to_string());
        fs::write(
            root.join("snapshot.json"),
            serde_json::to_vec(&snap).unwrap(),
        )
        .unwrap();
        let observed = resolved.status(&request.request_id).unwrap();
        assert!(
            !observed.state.ready_for_review(),
            "{failure} is never the success state"
        );
        assert_eq!(
            state_token(observed.state),
            state_token(*state),
            "observed {failure}"
        );
        let mut row = job("turn-1", &request.request_id, state_token(observed.state));
        row.updated_at = 1000 + i as i64;
        store.upsert_build_job(&row).unwrap();
    }
    let row = store.build_job_for_turn("turn-1").unwrap().unwrap();
    assert_eq!(row.state, "cancel_requested", "the last observation wins");
    assert_ne!(
        row.state, "completed",
        "no Completed token exists or persists"
    );
    assert_eq!(
        fs::read_to_string(root.join("calls")).unwrap(),
        "status\n".repeat(6),
        "exactly the polled statuses — no other outbound calls"
    );
}

/// A persisted open row survives a simulated restart and reattaches by
/// `status` of the SAME request id (never a fresh submit); terminal rows
/// do not reattach.
#[test]
fn restart_reattaches_open_rows_by_status() {
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    // Built with the same helpers as every other test: fresh request id,
    // carried turn id, the configured target binding.
    let resolved = ResolvedBuild::resolve_with(&targets(), "hosta", fake_ssh(root, 0)).unwrap();
    let request = build_request(&resolved, &input("turn-1", "task".into())).unwrap();
    {
        let store = awili_persist::Store::open(&root.join("awili.db")).unwrap();
        store
            .upsert_build_job(&job("turn-1", &request.request_id, "running"))
            .unwrap();
    }
    // Simulated restart: the open row comes back; the status call
    // queries the SAME id (the fake runner records the stdin query).
    let store = awili_persist::Store::open(&root.join("awili.db")).unwrap();
    let open = store.open_build_jobs().unwrap();
    assert_eq!(open.len(), 1);
    fs::write(
        root.join("snapshot.json"),
        serde_json::to_vec(&snapshot(&request, BuildState::AwaitingReview)).unwrap(),
    )
    .unwrap();
    let resolved =
        ResolvedBuild::resolve_with(&targets(), &open[0].target_id, fake_ssh(root, 0)).unwrap();
    let snap = resolved.status(&open[0].request_id).unwrap();
    assert_eq!(snap.state, BuildState::AwaitingReview);
    let query: awili_dispatch::protocol::JobQuery =
        serde_json::from_slice(&fs::read(root.join("last-request")).unwrap()).unwrap();
    assert_eq!(
        query.request_id, open[0].request_id,
        "reattach queries the SAME id"
    );
    let mut row = job(&open[0].turn_id, &snap.request_id, state_token(snap.state));
    row.updated_at = 2000;
    store.upsert_build_job(&row).unwrap();
    assert!(
        store.open_build_jobs().unwrap().is_empty(),
        "a terminal row does not reattach"
    );
}
