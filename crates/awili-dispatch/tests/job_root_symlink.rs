#![cfg(unix)]
// TODO(windows-phase1): fake-ssh/runner fixtures are shell scripts and modes
// RVS3-INT1: every test here launches a model through the Linux bwrap
// profile (S3(b)); on other targets the job fails closed, so gate the file.
#![cfg(target_os = "linux")]
//! RVS3A-FIX6: the job root is trusted, so its own symlinked ancestors
//! (e.g. macOS `/var` -> `/private/var`) are resolved once at open time;
//! nothing below the root may ever follow a symlink.
use awili_dispatch::{protocol::*, runner, store::Store};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

/// Minimal git fixture: a bare remote plus a working seed clone whose HEAD
/// is the baseline sha. `model.sh` and `verify.sh` are the executable stubs.
struct Fixture {
    #[allow(dead_code)]
    temp: TempDir,
    config: RunnerConfig,
    request: BuildRequest,
}

fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

impl Fixture {
    /// Build the fixture with the job root exactly at `job_root` (which may
    /// itself be reached through a symlinked ancestor).
    fn new_at(job_root: &Path) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let seed = temp.path().join("seed");
        fs::create_dir(&seed).unwrap();
        git(&seed, &["init", "-q"]);
        fs::write(seed.join("baseline.txt"), "baseline").unwrap();
        git(&seed, &["add", "."]);
        git(
            &seed,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@localhost",
                "commit",
                "-qm",
                "baseline",
            ],
        );
        let sha = git(&seed, &["rev-parse", "HEAD"]);
        let bare = temp.path().join("remote.git");
        git(
            temp.path(),
            &[
                "clone",
                "--bare",
                "--quiet",
                seed.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        let model = temp.path().join("model.sh");
        fs::write(
            &model,
            "printf '%s' \"$1\n\" > request.txt\nprintf '%s' \"$1\n\"\nprintf 'implementation' > result.rs\nexit 0\n",
        )
        .unwrap();
        // S3(c): verifier runs over the committed source export, which holds
        // the model's committed result.rs, and checks the frozen result SHA.
        let verify = temp.path().join("verify.sh");
        fs::write(
            &verify,
            "test -f result.rs || exit 23\ntest -n \"$AWILI_RESULT_SHA\" || exit 24\nexit 0\n",
        )
        .unwrap();
        let sandbox_script = temp.path().join("sandbox-model.sh");
        fs::copy(&model, &sandbox_script).unwrap();
        let command = |script: &Path| CommandSpec {
            program: "/usr/bin/sh".into(),
            args: vec![script.to_str().unwrap().into()],
            env: BTreeMap::new(),
        };
        let profile = BwrapProfile {
            bwrap: PathBuf::from("/usr/bin/bwrap"),
            readonly: vec![
                PathBuf::from("/usr"),
                PathBuf::from("/lib"),
                PathBuf::from("/lib64"),
                sandbox_script.clone(),
                verify.clone(),
            ],
            writable: vec![],
            env: BTreeMap::new(),
        };
        // S3(c): the verifier role's own fixed profile (export read-only,
        // its scratch writable); the fixture verify script runs over the
        // committed source export, not the model's live checkout.
        let verify_profile = BwrapProfile {
            bwrap: PathBuf::from("/usr/bin/bwrap"),
            readonly: vec![
                PathBuf::from("/usr"),
                PathBuf::from("/lib"),
                PathBuf::from("/lib64"),
                verify.clone(),
            ],
            writable: vec![],
            env: BTreeMap::new(),
        };
        let target = BuildTarget {
            repo_id: "fixture".into(),
            repo_url: bare.to_str().unwrap().into(),
            model: "fixture-local-model".into(),
            command: command(&sandbox_script),
            verify_profile: "tests".into(),
            verify: command(&verify),
            isolation: Some(IsolationSpec {
                bwrap: profile,
                verifier: verify_profile,
            }),
        };
        let config = RunnerConfig {
            job_root: job_root.to_path_buf(),
            targets: BTreeMap::from([("local".into(), target)]),
        };
        let request = BuildRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            turn_id: "turn-1".into(),
            conversation_id: "conv-1".into(),
            repo_id: "fixture".into(),
            baseline_sha: sha,
            branch: "fix/fixture-build".into(),
            spec_text: "do the work".into(),
            target_id: "local".into(),
            verify_profile: "tests".into(),
        };
        Self {
            temp,
            config,
            request,
        }
    }

    fn exec() -> &'static Path {
        Path::new(env!("CARGO_BIN_EXE_awili-build-runner"))
    }
}

/// Poll `runner::status` until the job leaves an active state, 15 s deadline.
fn wait(config: &RunnerConfig, id: &str) -> BuildSnapshot {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let s = runner::status(&config.job_root, id).unwrap();
        if !s.state.active() {
            return s;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "job still active after 15 s: {:?}",
            s
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The job root ITSELF is a symlink (`jobs -> real_jobs`): the store must
/// open and anchor it exactly as it would any plain directory — submit,
/// status and logs all work through the link.
#[test]
fn job_root_behind_symlinked_ancestor_works() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real_jobs");
    fs::create_dir(&real).unwrap();
    let link = temp.path().join("jobs");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let f = Fixture::new_at(&link);
    // The store can be opened through the symlinked job root at all.
    let store = Store::open(&f.config.job_root).unwrap();
    // The anchor is the canonical (resolved) directory, not the link.
    assert!(!store.root.ends_with("jobs"));
    assert!(store.root.ends_with("real_jobs"));
    let submitted = runner::submit(&f.config, f.request.clone(), Fixture::exec()).unwrap();
    assert_eq!(submitted.state, BuildState::Queued);
    // Status resolves the job through the same anchored root. The worker may
    // finish before this observation: accept queued, running OR the
    // completed/terminal state; the end-state assertion below is unchanged.
    let status = runner::status(&f.config.job_root, &f.request.request_id).unwrap();
    assert!(matches!(
        status.state,
        BuildState::Queued | BuildState::Running | BuildState::AwaitingReview
    ));
    let end = wait(&f.config, &f.request.request_id);
    assert_eq!(end.state, BuildState::AwaitingReview, "{:?}", end.failure);
    // Logs flow back through the same store and must contain the KNOWN
    // bytes the model stub printed to stdout during this run.
    let logs = store
        .logs(LogsQuery {
            request_id: f.request.request_id.clone(),
            cursor: 0,
            max_bytes: 65536,
        })
        .unwrap();
    let known = f.request.spec_text.as_bytes();
    assert!(
        logs.bytes.windows(known.len()).any(|w| w == known),
        "expected the model stub's known stdout in the job log, got {:?}",
        String::from_utf8_lossy(&logs.bytes)
    );
}

/// With the same symlinked-root store, anything BELOW the root that becomes
/// a symlink is refused exactly as before: the anchor never follows it.
#[test]
fn symlink_below_root_still_refused() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    fs::create_dir(&real).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let f = Fixture::new_at(&link.join("jobs"));
    let submitted = runner::submit(&f.config, f.request.clone(), Fixture::exec()).unwrap();
    assert_eq!(submitted.state, BuildState::Queued);
    let end = wait(&f.config, &f.request.request_id);
    assert_eq!(end.state, BuildState::AwaitingReview, "{:?}", end.failure);

    let id = &f.request.request_id;
    let root = f.config.job_root.clone();
    let job = root.join(id);
    let ctl = job.join("control");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let store = Store::open(&root).unwrap();
    assert!(store.read(id).is_ok());
    assert!(store.dir(id).is_ok());

    // (a) Control directory replaced by a symlink to a copy: refused.
    fs::rename(&ctl, job.join("control.real")).unwrap();
    std::os::unix::fs::symlink(job.join("control.real"), &ctl).unwrap();
    assert!(store.read(id).is_err());
    assert!(runner::status(&root, id).is_err());
    // (b) Record leaf replaced by a symlink pointing outside: refused.
    // Restore the real control directory; (b) now targets the leaf alone.
    fs::remove_file(&ctl).unwrap();
    fs::rename(job.join("control.real"), &ctl).unwrap();
    let record = ctl.join("record.json");
    fs::copy(&record, outside.join("record.json")).unwrap();
    fs::remove_file(&record).unwrap();
    std::os::unix::fs::symlink(outside.join("record.json"), &record).unwrap();
    assert!(store.read(id).is_err());
    assert!(runner::status(&root, id).is_err());
    // The restored genuine record is readable again through a fresh store.
    fs::remove_file(&record).unwrap();
    fs::rename(outside.join("record.json"), &record).unwrap();
    assert!(Store::open(&root).unwrap().read(id).is_ok());
    // (c) Job directory replaced by a symlink: still refused.
    fs::rename(&job, root.join(format!("{id}.real"))).unwrap();
    std::os::unix::fs::symlink(root.join(format!("{id}.real")), &job).unwrap();
    assert!(store.read(id).is_err());
    assert!(store.dir(id).is_err());
    assert!(runner::status(&root, id).is_err());
}
