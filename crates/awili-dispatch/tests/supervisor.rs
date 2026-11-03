//! The supervised-run main process (port of the Python `test-supervisor.py`
//! plus the RC2-OPS3 regressions). No fleet access, no model.
#![cfg(unix)] // TODO(windows-phase1): fake-ssh/runner fixtures are shell scripts and modes
use awili_dispatch::{
    protocol::*,
    store::Store,
    supervisor::{correlate, RequestConflict, Supervisor},
};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

fn target(root: &Path) -> BuildTarget {
    let profile = BwrapProfile {
        bwrap: root.join("no-bwrap"),
        readonly: vec![],
        writable: vec![],
        env: BTreeMap::new(),
    };
    let command = CommandSpec {
        program: "/usr/bin/true".into(),
        args: vec![],
        env: BTreeMap::new(),
    };
    BuildTarget {
        repo_id: "lumi".into(),
        repo_url: root.join("remote.git").to_str().unwrap().into(),
        model: "fixture".into(),
        command: command.clone(),
        verify_profile: "smoke".into(),
        verify: command,
        isolation: Some(IsolationSpec {
            bwrap: profile.clone(),
            verifier: profile,
        }),
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    supervisor: Supervisor,
    request: BuildRequest,
}
impl Fixture {
    fn new(window: Duration) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let config = RunnerConfig {
            job_root: root.join("jobs"),
            targets: BTreeMap::from([("smoke".into(), target(&root))]),
        };
        let request = BuildRequest {
            request_id: "789c0d53-64a2-4070-aedb-f0e4e913f853".into(),
            turn_id: "turn".into(),
            conversation_id: "conv".into(),
            repo_id: "lumi".into(),
            baseline_sha: "a".repeat(40),
            branch: "fix/supervisor-test".into(),
            spec_text: "Unicode 猫\n\t\"\\ data".into(),
            target_id: "smoke".into(),
            verify_profile: "smoke".into(),
        };
        fs::create_dir_all(root.join("requests")).unwrap();
        fs::write(
            root.join("requests")
                .join(format!("{}.json", request.request_id)),
            serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        Self {
            supervisor: Supervisor {
                root,
                config,
                executable: env!("CARGO_BIN_EXE_awili-build-runner").into(),
                poll: Duration::from_millis(20),
                window,
            },
            _temp: temp,
            request,
        }
    }
    fn run(&self) -> (anyhow::Result<i32>, String) {
        let mut out = Vec::new();
        let result = self.supervisor.run(&self.request.request_id, &mut out);
        (result, String::from_utf8(out).unwrap())
    }
}

#[test]
fn supervisor_correlation_matches_and_conflicts() {
    let f = Fixture::new(Duration::from_secs(15));
    let (result, out) = f.run();
    // The job fails closed (no bwrap), so the supervisor exits 1 after
    // printing every revision it saw.
    assert_eq!(result.unwrap(), 1);
    let last: BuildSnapshot = serde_json::from_str(out.lines().last().unwrap()).unwrap();
    assert_eq!(last.request_id, f.request.request_id);
    let root = &f.supervisor.config.job_root;
    correlate(&last, &f.request, root).unwrap();
    // Same ID, different payload: never accept an old job after submit fails.
    let mut other = f.request.clone();
    other.spec_text = "other work".into();
    assert!(correlate(&last, &other, root)
        .unwrap_err()
        .is::<RequestConflict>());
    let mut wrong_sha = last.clone();
    wrong_sha.request_sha256 = "b".repeat(64);
    assert!(correlate(&wrong_sha, &f.request, root)
        .unwrap_err()
        .is::<RequestConflict>());
    let mut wrong_id = last.clone();
    wrong_id.request_id = uuid::Uuid::new_v4().to_string();
    assert!(correlate(&wrong_id, &f.request, root)
        .unwrap_err()
        .is::<RequestConflict>());
    // A second run of the same request observes the stored job: no new submit.
    let (again, _) = f.run();
    assert_eq!(again.unwrap(), 1);
}

/// F0003: when the submit fails and the status check that follows fails
/// too, the supervisor reports both; the original submit error survives.
#[test]
fn rc2ops3_a_failed_status_check_keeps_the_submit_error() {
    let mut f = Fixture::new(Duration::from_secs(15));
    f.supervisor.config.targets.clear();
    let (result, out) = f.run();
    let text = format!("{:#}", result.unwrap_err());
    assert!(text.contains("unknown target"), "submit error lost: {text}");
    assert!(
        text.contains("status check that followed also failed"),
        "{text}"
    );
    assert!(out.is_empty());
}

/// F0004: a job that stays active is not polled without end. When the
/// supervision window closes the supervisor asks for cancellation and exits
/// with an error naming the window.
#[test]
fn rc2ops3_the_supervisor_stops_waiting_when_its_window_closes() {
    let f = Fixture::new(Duration::from_millis(300));
    // A job that is already running with a live (but foreign) worker.
    let store = Store::open(&f.supervisor.config.job_root).unwrap();
    let mut sleeper = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let identity = wait_identity(sleeper.id());
    store.create_job(&f.request.request_id).unwrap();
    let mut record = JobRecord {
        snapshot: BuildSnapshot {
            request_id: f.request.request_id.clone(),
            request_sha256: f.request.fingerprint(),
            revision: 0,
            state: BuildState::Running,
            executor: "/usr/bin/true".into(),
            requested_model: "fixture".into(),
            baseline_sha: f.request.baseline_sha.clone(),
            result_sha: None,
            remote_sha: None,
            model_exit: None,
            verify_exit: None,
            push_exit: None,
            worker: Some(identity),
            failure: None,
            no_op: false,
        },
        target: f.supervisor.config.targets["smoke"].clone(),
        request: f.request.clone(),
    };
    store.write(&mut record).unwrap();
    let started = Instant::now();
    let (result, out) = f.run();
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    let text = format!("{:#}", result.unwrap_err());
    assert!(text.contains("supervision window closed"), "{text}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "waited past the window"
    );
    assert!(!out.is_empty());
    // The cancel request reached the runner: the record left Running.
    let after = store.read(&f.request.request_id).unwrap().snapshot;
    assert_ne!(after.state, BuildState::Running);
}

/// F0002: a malformed reserved request is a plain error, not a crash.
#[test]
fn rc2ops3_a_malformed_request_file_is_a_plain_error() {
    let f = Fixture::new(Duration::from_secs(15));
    fs::write(
        f.supervisor
            .root
            .join("requests")
            .join(format!("{}.json", f.request.request_id)),
        br#"{"request_id":"789c0d53-64a2-4070-aedb-f0e4e913f853"}"#,
    )
    .unwrap();
    let (result, _) = f.run();
    let text = format!("{:#}", result.unwrap_err());
    assert!(
        text.contains("reserved request file is malformed"),
        "{text}"
    );
}

fn wait_identity(pid: u32) -> WorkerIdentity {
    // Same identity the runner records (pid + start stamp).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = if cfg!(target_os = "linux") {
            let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(19).map(str::to_string))
                .map(|start| format!("{}:{}", boot.trim(), start))
        } else {
            let out = Command::new("/bin/ps")
                .args(["-o", "lstart=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            let stamp = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!stamp.is_empty()).then(|| {
                stamp
                    .split_whitespace()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        };
        if let Some(stamp) = output {
            return WorkerIdentity { pid, stamp };
        }
        assert!(Instant::now() < deadline, "no identity for {pid}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
