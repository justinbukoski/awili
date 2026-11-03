//! RC2-OPS1: a submission that stops before its record lands can be retried,
//! and a branch whose owner ended without publishing can be used again.
#![cfg(unix)] // TODO(windows-phase1): fake-ssh/runner fixtures are shell scripts and modes
use awili_dispatch::{protocol::*, runner, store::Store};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct Jobs {
    _temp: TempDir,
    config: RunnerConfig,
}
impl Jobs {
    /// A target whose isolation points at a missing bwrap: every job fails
    /// closed before any model runs and never pushes, on Linux and macOS.
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let missing = root.join("no-bwrap");
        let profile = BwrapProfile {
            bwrap: missing,
            readonly: vec![],
            writable: vec![],
            env: BTreeMap::new(),
        };
        let command = CommandSpec {
            program: "/usr/bin/true".into(),
            args: vec![],
            env: BTreeMap::new(),
        };
        let target = BuildTarget {
            repo_id: "fixture".into(),
            repo_url: root.join("remote.git").to_str().unwrap().into(),
            model: "fixture-local-model".into(),
            command: command.clone(),
            verify_profile: "tests".into(),
            verify: command,
            isolation: Some(IsolationSpec {
                bwrap: profile.clone(),
                verifier: profile,
            }),
        };
        let config = RunnerConfig {
            job_root: root.join("jobs"),
            targets: BTreeMap::from([("local".into(), target)]),
        };
        Self {
            _temp: temp,
            config,
        }
    }
    fn request(&self) -> BuildRequest {
        BuildRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            turn_id: "turn-1".into(),
            conversation_id: "conv-1".into(),
            repo_id: "fixture".into(),
            baseline_sha: "a".repeat(40),
            branch: "fix/fixture-build".into(),
            spec_text: "spec".into(),
            target_id: "local".into(),
            verify_profile: "tests".into(),
        }
    }
    fn submit(&self, request: &BuildRequest) -> anyhow::Result<BuildSnapshot> {
        runner::submit(
            &self.config,
            request.clone(),
            Path::new(env!("CARGO_BIN_EXE_awili-build-runner")),
        )
    }
    fn finish(&self, id: &str) -> BuildSnapshot {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let s = runner::status(&self.config.job_root, id).unwrap();
            if !s.state.active() {
                return s;
            }
            assert!(Instant::now() < deadline, "job did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn job_dir(&self, id: &str) -> PathBuf {
        self.config.job_root.join(id)
    }
    fn branches(&self) -> PathBuf {
        self.config.job_root.join("branches")
    }
    /// Rewrite the owner's record (lock held) to model how it ended.
    fn force_end(&self, id: &str, state: BuildState, push_exit: Option<i32>) {
        let store = Store::open(&self.config.job_root).unwrap();
        let _lock = store.lock().unwrap();
        let mut record = store.read(id).unwrap();
        record.snapshot.state = state;
        record.snapshot.push_exit = push_exit;
        record.snapshot.worker = None;
        store.write(&mut record).unwrap();
    }
}

/// F0479: the reservation and the record are written together. A failure
/// after the job directory is made removes it again, so the same request can
/// be retried instead of failing on "record leaf absent".
#[test]
fn rc2ops1_a_failed_submission_leaves_nothing_a_retry_trips_on() {
    let jobs = Jobs::new();
    let request = jobs.request();
    // Prepare the root, then make the reservation directory unwritable so
    // the step after the job directory is created fails.
    fs::create_dir_all(&jobs.config.job_root).unwrap();
    Store::open(&jobs.config.job_root)
        .unwrap()
        .branch_reserved("probe")
        .unwrap();
    fs::set_permissions(jobs.branches(), fs::Permissions::from_mode(0o500)).unwrap();
    let first = jobs.submit(&request);
    fs::set_permissions(jobs.branches(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(first.is_err(), "the reservation write was meant to fail");
    assert!(
        !jobs.job_dir(&request.request_id).exists(),
        "a failed submission left its job directory behind"
    );
    let retried = jobs.submit(&request).expect("the retry submits for real");
    assert_eq!(retried.request_id, request.request_id);
    assert_eq!(retried.request_sha256, request.fingerprint());
}

/// F0479: a submission that crashed after making the job directory (no
/// record, so no worker ever started) is cleared by the retry, including the
/// reservation it may already hold.
#[test]
fn rc2ops1_a_crashed_submission_is_retried_with_the_same_id() {
    let jobs = Jobs::new();
    let request = jobs.request();
    let store = Store::open(&jobs.config.job_root).unwrap();
    store.create_job(&request.request_id).unwrap();
    let key = digest(
        format!(
            "{}\0{}",
            jobs.config.targets["local"].repo_url, request.branch
        )
        .as_bytes(),
    );
    store.reserve_branch(&key, &request.request_id).unwrap();
    let snapshot = jobs
        .submit(&request)
        .expect("a record-less job directory never blocks its own retry");
    assert_eq!(snapshot.request_id, request.request_id);
    let record = store
        .read(&request.request_id)
        .expect("record now readable");
    assert_eq!(record.request, request);
    // A job directory that holds anything else is never removed.
    let other = jobs.request();
    store.create_job(&other.request_id).unwrap();
    fs::write(jobs.job_dir(&other.request_id).join("repo"), "work").unwrap();
    assert!(jobs.submit(&other).is_err());
    assert!(jobs.job_dir(&other.request_id).join("repo").exists());
}

/// F0478: a branch is reusable once its owner ended Failed or Stopped without
/// any push; a job that pushed, needs attention or awaits review keeps it.
#[test]
fn rc2ops1_a_branch_frees_up_when_its_owner_never_published() {
    let jobs = Jobs::new();
    let first = jobs.request();
    jobs.submit(&first).unwrap();
    let ended = jobs.finish(&first.request_id);
    assert_eq!(ended.state, BuildState::Failed, "{:?}", ended.failure);
    assert_eq!(ended.push_exit, None);

    for (state, push_exit) in [
        (BuildState::NeedsAttention, None),
        (BuildState::AwaitingReview, Some(0)),
        (BuildState::Failed, Some(1)),
    ] {
        jobs.force_end(&first.request_id, state, push_exit);
        let blocked = jobs.request();
        let err = jobs
            .submit(&blocked)
            .expect_err("branch must stay reserved");
        assert!(
            err.to_string().contains("branch already reserved"),
            "{state:?}: {err}"
        );
        assert!(!jobs.job_dir(&blocked.request_id).exists());
    }

    for state in [BuildState::Stopped, BuildState::Failed] {
        jobs.force_end(&first.request_id, state, None);
        let next = jobs.request();
        let snapshot = jobs
            .submit(&next)
            .unwrap_or_else(|e| panic!("{state:?}: branch should be free: {e}"));
        assert_eq!(snapshot.request_id, next.request_id);
        jobs.finish(&next.request_id);
        // Hand the branch back to the first owner for the next round.
        let store = Store::open(&jobs.config.job_root).unwrap();
        let key = digest(
            format!("{}\0{}", jobs.config.targets["local"].repo_url, next.branch).as_bytes(),
        );
        assert_eq!(
            store.branch_owner(&key).unwrap().as_deref(),
            Some(next.request_id.as_str())
        );
        store.reserve_branch(&key, &first.request_id).unwrap();
    }
}
