//! The supervised endpoint against a fake runner and systemctl (port of the
//! Python `test-endpoint.py` and `test-endpoint-io.py`, plus the RC2-OPS2
//! regressions). Never contacts a service or a model endpoint.
#![cfg(unix)] // TODO(windows-phase1): fake-ssh/runner fixtures are shell scripts and modes
use awili_dispatch::{
    endpoint::{exchange, exchange_within, Endpoint, Meta, CAP},
    protocol::*,
};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

const RUNNER: &str = "/fake/runner";

struct State {
    calls: Vec<(Vec<String>, Option<Value>)>,
    active: String,
    group: String,
    invocation: String,
    start_error: bool,
    start_rejected: bool,
    snapshot: Option<BuildSnapshot>,
    cgroups: PathBuf,
    runner_fails: Option<&'static str>,
    status_mismatch: bool,
    cancel_mismatch: bool,
    cancel_completes: bool,
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    state: Rc<RefCell<State>>,
    request: BuildRequest,
}

fn group_path(id: &str) -> String {
    format!(r"/system.slice/system-awili\x2dbuild.slice/awili-build@{id}.service")
}

fn fake(
    state: &Rc<RefCell<State>>,
    argv: &[String],
    payload: Option<&Value>,
) -> anyhow::Result<Vec<u8>> {
    let mut s = state.borrow_mut();
    s.calls.push((argv.to_vec(), payload.cloned()));
    if argv[0] == RUNNER {
        let op = argv.last().unwrap().as_str();
        assert_ne!(op, "submit", "must never directly submit");
        if s.runner_fails == Some(op) {
            anyhow::bail!("runner {op} unavailable");
        }
        return Ok(match op {
            "validate" => serde_json::to_vec(&json!({
                "request": payload.unwrap(),
                "request_sha256": "a".repeat(64),
            }))?,
            "status" | "cancel" => {
                if op == "cancel" && s.cancel_completes {
                    s.snapshot.as_mut().unwrap().state = BuildState::AwaitingReview;
                }
                let mut snapshot = s.snapshot.clone().expect("runner record exists");
                if (op == "status" && s.status_mismatch) || (op == "cancel" && s.cancel_mismatch) {
                    snapshot.request_sha256 = "b".repeat(64);
                }
                serde_json::to_vec(&snapshot)?
            }
            "logs" => {
                let cursor = payload.unwrap()["cursor"].as_u64().unwrap();
                serde_json::to_vec(
                    &json!({"cursor": cursor, "next_cursor": cursor + 2, "bytes": [65, 66]}),
                )?
            }
            other => panic!("unexpected runner op {other}"),
        });
    }
    if argv.iter().any(|a| a == "start") {
        let id = argv
            .last()
            .unwrap()
            .trim_start_matches("awili-build@")
            .trim_end_matches(".service")
            .to_string();
        s.active = "active".into();
        s.group = group_path(&id);
        let dir = s.cgroups.join(s.group.trim_start_matches('/'));
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("cgroup.events"), "populated 1\n")?;
        if s.start_rejected {
            s.active = "failed".into();
            anyhow::bail!("start rejected");
        }
        if s.start_error {
            anyhow::bail!("lost ack");
        }
        return Ok(vec![]);
    }
    if argv.iter().any(|a| a == "stop") {
        s.active = "inactive".into();
        return Ok(vec![]);
    }
    if argv.iter().any(|a| a == "show") {
        return Ok(format!(
            "LoadState=loaded\nActiveState={}\nControlGroup={}\nInvocationID={}\n",
            s.active, s.group, s.invocation
        )
        .into_bytes());
    }
    panic!("unexpected call {argv:?}");
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let state = Rc::new(RefCell::new(State {
            calls: vec![],
            active: "inactive".into(),
            group: String::new(),
            invocation: "1".repeat(32),
            start_error: false,
            start_rejected: false,
            snapshot: None,
            cgroups: root.join("cgroup"),
            runner_fails: None,
            status_mismatch: false,
            cancel_mismatch: false,
            cancel_completes: false,
        }));
        let request = BuildRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            turn_id: "turn".into(),
            conversation_id: "conv".into(),
            repo_id: "lumi".into(),
            baseline_sha: "a".repeat(40),
            branch: "fix/test".into(),
            spec_text: "Unicode café\n$(do not execute)".into(),
            target_id: "qwen-hosta-runtime-hostb-smoke".into(),
            verify_profile: "normalize-label-smoke-v1".into(),
        };
        Self {
            _temp: temp,
            root,
            state,
            request,
        }
    }
    fn endpoint(&self) -> Endpoint<'_> {
        let state = self.state.clone();
        Endpoint {
            root: self.root.clone(),
            runner: RUNNER.into(),
            config: "/fake/config".into(),
            cgroups: self.root.join("cgroup"),
            call: Box::new(move |argv, payload| fake(&state, argv, payload)),
        }
    }
    fn handle(&self, op: &str) -> anyhow::Result<Value> {
        let payload = if op == "submit" {
            serde_json::to_value(&self.request).unwrap()
        } else {
            json!({ "request_id": self.request.request_id })
        };
        self.endpoint().handle(op, &payload)
    }
    fn handle_with(&self, op: &str, payload: Value) -> anyhow::Result<Value> {
        self.endpoint().handle(op, &payload)
    }
    fn state_of(&self, op: &str) -> String {
        self.handle(op).unwrap()["supervisor_state"]
            .as_str()
            .unwrap()
            .to_string()
    }
    fn record(&self, state: BuildState) {
        let p = self
            .root
            .join("jobs")
            .join(&self.request.request_id)
            .join("control");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("record.json"), "{}").unwrap();
        self.state.borrow_mut().snapshot = Some(BuildSnapshot {
            request_id: self.request.request_id.clone(),
            request_sha256: "a".repeat(64),
            revision: 1,
            state,
            executor: "/opt/awili-build/node".into(),
            requested_model: "qwen".into(),
            baseline_sha: "a".repeat(40),
            result_sha: None,
            remote_sha: None,
            model_exit: None,
            verify_exit: None,
            push_exit: None,
            worker: None,
            failure: None,
            no_op: false,
        });
    }
    fn starts(&self) -> usize {
        self.count("start")
    }
    fn count(&self, word: &str) -> usize {
        self.state
            .borrow()
            .calls
            .iter()
            .filter(|(argv, _)| argv.iter().any(|a| a == word))
            .count()
    }
    fn cgroup_dir(&self) -> PathBuf {
        let s = self.state.borrow();
        s.cgroups.join(s.group.trim_start_matches('/'))
    }
}

#[test]
fn endpoint_immutable_replay_and_conflict() {
    let f = Fixture::new();
    assert_eq!(f.state_of("submit"), "starting");
    f.handle("submit").unwrap();
    assert_eq!(f.starts(), 1);
    let mut altered = f.request.clone();
    altered.spec_text = "different".into();
    assert!(f
        .handle_with("submit", serde_json::to_value(&altered).unwrap())
        .is_err());
    assert_eq!(f.starts(), 1);
    let path = f
        .root
        .join("requests")
        .join(format!("{}.json", f.request.request_id));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let stored: BuildRequest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored, f.request);
}

#[test]
fn endpoint_lost_start_ack_observes_without_relaunch() {
    let f = Fixture::new();
    f.state.borrow_mut().start_error = true;
    assert_eq!(f.state_of("submit"), "starting");
    f.handle("submit").unwrap();
    assert_eq!(f.starts(), 1);
}

#[test]
fn endpoint_partial_reservation_never_starts() {
    let f = Fixture::new();
    let p = f.root.join("requests");
    fs::create_dir(&p).unwrap();
    fs::write(
        p.join(format!("{}.json", f.request.request_id)),
        serde_json::to_vec(&f.request).unwrap(),
    )
    .unwrap();
    assert_eq!(f.state_of("submit"), "needs_attention");
    assert_eq!(f.starts(), 0);
}

#[test]
fn endpoint_dead_service_not_restarted() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    f.state.borrow_mut().active = "failed".into();
    assert_eq!(f.state_of("submit"), "needs_attention");
    assert_eq!(f.starts(), 1);
}

#[test]
fn endpoint_start_failure_is_not_retried() {
    let f = Fixture::new();
    f.state.borrow_mut().start_rejected = true;
    assert_eq!(f.state_of("submit"), "needs_attention");
    f.handle("submit").unwrap();
    assert_eq!(f.starts(), 1);
}

#[test]
fn endpoint_wrong_cgroup_cannot_confirm_stop() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    f.state.borrow_mut().group = "/system.slice/unrelated.service".into();
    let result = f.handle("cancel").unwrap();
    assert_eq!(result["supervisor_state"], "needs_attention");
    assert_eq!(result["termination_confirmed"], false);
}

#[test]
fn endpoint_invalid_target_no_service_operations() {
    let mut f = Fixture::new();
    f.request.target_id = "generic".into();
    assert!(f.handle("submit").is_err());
    assert!(f.state.borrow().calls.is_empty());
}

#[test]
fn endpoint_snapshot_correlation() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    f.record(BuildState::Running);
    for field in ["request_id", "request_sha256", "baseline_sha"] {
        let original = f.state.borrow().snapshot.clone();
        {
            let mut s = f.state.borrow_mut();
            let snapshot = s.snapshot.as_mut().unwrap();
            match field {
                "request_id" => snapshot.request_id = uuid::Uuid::new_v4().to_string(),
                "request_sha256" => snapshot.request_sha256 = "c".repeat(64),
                _ => snapshot.baseline_sha = "b".repeat(40),
            }
        }
        assert!(f.handle("status").is_err(), "{field} mismatch accepted");
        f.state.borrow_mut().snapshot = original;
    }
}

#[test]
fn endpoint_cancel_starting_job_requires_empty_descendant_cgroup() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    let value = f.handle("cancel").unwrap();
    assert_eq!(value["supervisor_state"], "needs_attention");
    assert_eq!(value["termination_confirmed"], false);
    fs::write(f.cgroup_dir().join("cgroup.events"), "populated 0\n").unwrap();
    assert_eq!(f.state_of("status"), "stopped");
    f.handle("submit").unwrap();
    assert_eq!(f.starts(), 1);
}

#[test]
fn endpoint_terminal_result_preserved_on_cancel() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    f.record(BuildState::AwaitingReview);
    let value = f.handle("cancel").unwrap();
    assert_eq!(value["snapshot"]["state"], "awaiting_review");
    // RC2-OPSF1: the service is still active with a populated cgroup, so the
    // owned invocation is stopped; the result itself is untouched.
    assert_eq!(f.count("stop"), 1);
}

#[test]
fn endpoint_missing_status_never_submits() {
    let f = Fixture::new();
    assert!(f.handle("status").is_err());
    assert_eq!(f.starts(), 0);
}

#[test]
fn endpoint_bounded_correlated_logs() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    f.record(BuildState::Running);
    let id = f.request.request_id.clone();
    let result = f
        .handle_with(
            "logs",
            json!({"request_id": id, "cursor": 0, "max_bytes": 2}),
        )
        .unwrap();
    assert_eq!(result["page"]["bytes"], json!([65, 66]));
    assert!(f
        .handle_with(
            "logs",
            json!({"request_id": id, "cursor": 0, "max_bytes": 65537})
        )
        .is_err());
}

#[test]
fn endpoint_raced_terminal_completion_is_not_rewritten() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    f.record(BuildState::Running);
    f.state.borrow_mut().cancel_completes = true;
    let result = f.handle("cancel").unwrap();
    assert_eq!(result["snapshot"]["state"], "awaiting_review");
    assert_eq!(result["supervisor_state"], "needs_attention");
    assert_eq!(result["termination_confirmed"], false);
}

#[test]
fn endpoint_failed_runner_cancel_still_attempts_service_stop() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    f.record(BuildState::Running);
    f.state.borrow_mut().runner_fails = Some("cancel");
    f.handle("cancel").unwrap();
    assert_eq!(f.count("stop"), 1);
}

#[test]
fn endpoint_replaced_invocation_cannot_confirm_empty_old_cgroup_or_cancel_new() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    fs::write(f.cgroup_dir().join("cgroup.events"), "populated 0\n").unwrap();
    {
        let mut s = f.state.borrow_mut();
        s.active = "inactive".into();
        s.invocation = "2".repeat(32);
    }
    let value = f.handle("cancel").unwrap();
    assert_eq!(value["supervisor_state"], "needs_attention");
    assert_eq!(value["termination_confirmed"], false);
    assert_eq!(f.count("stop"), 0);
}

#[test]
fn endpoint_missing_invocation_cannot_confirm_empty_group() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    {
        let mut s = f.state.borrow_mut();
        s.invocation = String::new();
        s.active = "inactive".into();
    }
    assert_eq!(f.handle("status").unwrap()["termination_confirmed"], false);
}

#[test]
fn endpoint_runner_stopped_is_not_service_termination() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    f.record(BuildState::Stopped);
    let value = f.handle("cancel").unwrap();
    assert_eq!(value["snapshot"]["state"], "stopped");
    assert_eq!(value["supervisor_state"], "needs_attention");
    assert_eq!(value["termination_confirmed"], false);
}

#[test]
fn endpoint_old_flat_cgroup_layout_is_rejected() {
    let f = Fixture::new();
    let id = &f.request.request_id;
    let flat = format!("/system.slice/{}", Endpoint::unit(id).unwrap());
    assert!(f.endpoint().group(id, &flat).is_err());
    assert!(f.endpoint().group(id, &group_path(id)).is_ok());
}

/// F0016: a runner that cannot answer `status` is a "needs attention"
/// observation with a diagnostic, not an error that escapes it.
#[test]
fn rc2ops2_a_runner_status_failure_is_needs_attention() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    f.record(BuildState::Running);
    f.state.borrow_mut().runner_fails = Some("status");
    let value = f.handle("status").expect("observation answers, not errors");
    assert_eq!(value["supervisor_state"], "needs_attention");
    assert_eq!(value["termination_confirmed"], false);
    assert_eq!(
        value["diagnostic"],
        "service or cgroup observation unavailable"
    );
    // A runner answer for another job is still refused outright.
    f.state.borrow_mut().runner_fails = None;
    f.state.borrow_mut().status_mismatch = true;
    assert!(f.handle("status").is_err());
}

/// F0017: a mismatched runner cancel answer does not skip the owned service
/// stop; the mismatch is still reported as an error afterwards.
#[test]
fn rc2ops2_a_mismatched_cancel_answer_still_stops_the_service() {
    let f = Fixture::new();
    f.handle("submit").unwrap();
    f.record(BuildState::Running);
    f.state.borrow_mut().cancel_mismatch = true;
    assert!(f.handle("cancel").is_err());
    assert_eq!(f.count("stop"), 1, "the owned service stop was skipped");
    let meta: Meta = serde_json::from_slice(
        &fs::read(
            f.root
                .join("endpoint")
                .join(format!("{}.json", f.request.request_id)),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(meta.cancel_requested);
}

/// RC2-OPSF1 (review of 09b2f50f): a finished job (awaiting review or
/// failed) whose service is still running, or whose cgroup still holds
/// processes, is stopped on cancel when the invocation is the owned one. The
/// finished result is kept, no cancel intent is recorded and the runner is
/// not asked to cancel. A confirmed termination or a replaced invocation is
/// never stopped.
#[test]
fn rc2opsf1_a_finished_job_with_a_live_service_is_stopped_on_cancel() {
    let cancel_requested = |f: &Fixture| -> bool {
        let path = f
            .root
            .join("endpoint")
            .join(format!("{}.json", f.request.request_id));
        serde_json::from_slice::<Meta>(&fs::read(path).unwrap())
            .unwrap()
            .cancel_requested
    };
    let runner_cancels = |f: &Fixture| -> usize {
        f.state
            .borrow()
            .calls
            .iter()
            .filter(|(argv, _)| argv[0] == RUNNER && argv.last().unwrap() == "cancel")
            .count()
    };
    for (state, wire) in [
        (BuildState::AwaitingReview, "awaiting_review"),
        (BuildState::Failed, "failed"),
    ] {
        // Still running: the owned service is stopped, the result is kept.
        let f = Fixture::new();
        f.handle("submit").unwrap();
        f.record(state);
        let value = f.handle("cancel").unwrap();
        assert_eq!(value["snapshot"]["state"], wire);
        assert_eq!(
            f.count("stop"),
            1,
            "{wire}: the live service was left running"
        );
        assert_eq!(f.state.borrow().active, "inactive");
        assert!(
            !cancel_requested(&f),
            "{wire}: a finished job got a cancel intent"
        );
        assert_eq!(
            runner_cancels(&f),
            0,
            "{wire}: the runner was asked to cancel"
        );

        // Inactive but the cgroup is still populated: termination is not
        // confirmed, so the owned invocation is still stopped.
        let f = Fixture::new();
        f.handle("submit").unwrap();
        f.record(state);
        f.state.borrow_mut().active = "inactive".into();
        f.handle("cancel").unwrap();
        assert_eq!(
            f.count("stop"),
            1,
            "{wire}: an unconfirmed termination was not stopped"
        );

        // Termination confirmed (inactive, empty cgroup): nothing to stop.
        let f = Fixture::new();
        f.handle("submit").unwrap();
        f.record(state);
        f.state.borrow_mut().active = "inactive".into();
        fs::write(f.cgroup_dir().join("cgroup.events"), "populated 0\n").unwrap();
        let value = f.handle("cancel").unwrap();
        assert_eq!(value["snapshot"]["state"], wire);
        assert_eq!(value["termination_confirmed"], true);
        assert_eq!(
            f.count("stop"),
            0,
            "{wire}: a confirmed termination was stopped"
        );

        // A replaced invocation is never stopped.
        let f = Fixture::new();
        f.handle("submit").unwrap();
        f.record(state);
        f.handle("status").unwrap();
        f.state.borrow_mut().invocation = "2".repeat(32);
        let value = f.handle("cancel").unwrap();
        assert_eq!(value["snapshot"]["state"], wire);
        assert_eq!(
            f.count("stop"),
            0,
            "{wire}: a replaced invocation was stopped"
        );
    }
}

fn sh(script: &str) -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into(), script.into()]
}

#[test]
fn endpoint_io_literal_json() {
    let value = json!({"text": "café\n$(never)"});
    let out = exchange(&sh("cat"), Some(&value)).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), value);
}

#[test]
fn endpoint_io_output_limit() {
    let script = format!("head -c {} /dev/zero | tr '\\0' x", CAP + 1);
    assert!(exchange(&sh(&script), None).is_err());
}

#[test]
fn endpoint_io_pipe_holding_descendant_does_not_exceed_deadline() {
    let start = Instant::now();
    assert!(exchange_within(&sh("sleep 2 & exit 0"), None, Duration::from_millis(200)).is_err());
    assert!(start.elapsed() < Duration::from_millis(1500));
}

#[test]
fn endpoint_io_nonzero_output_not_exposed() {
    let err = exchange(&sh("echo synthetic-private-value; exit 7"), None).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("command failed"), "{text}");
    assert!(!text.contains("synthetic-private-value"));
}

/// F0015: a command that closes its input early gets its own diagnostic
/// (the child is still cleaned up by the exchange).
#[test]
fn rc2ops2_a_closed_input_is_named() {
    let big = json!({ "text": "x".repeat(256 * 1024) });
    let err = exchange(&sh("exec 0<&-; sleep 1"), Some(&big)).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("closed its input"), "{text}");
}

/// The installed binary refuses any argv but the fixed one and never echoes
/// its input on failure.
#[test]
fn rc2ops2_the_endpoint_binary_takes_only_the_fixed_invocation() {
    use std::io::Write;
    let run = |args: &[&str]| {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_awili-build-endpoint"))
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // The binary refuses a wrong argv before it reads stdin, so it may
        // already have exited and closed the pipe: a broken pipe here is that
        // refusal winning the race, not a failure (the checks below decide).
        let written = child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"request_id":"synthetic-private-value"}"#);
        if let Err(e) = written {
            assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "{e}");
        }
        child.wait_with_output().unwrap()
    };
    for args in [
        vec!["--config", "/tmp/other.json", "status"],
        vec!["status"],
        vec![
            "--config",
            "/opt/awili-build/runner.json",
            "status",
            "extra",
        ],
    ] {
        let out = run(&args);
        assert!(!out.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("supervised endpoint failed"), "{stderr}");
        assert!(!stderr.contains("synthetic-private-value"));
        assert!(out.stdout.is_empty());
    }
}
