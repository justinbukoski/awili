//! RVS3A-FIX2 proof tests: FIFOs and directories substituted for control
//! leaves must be rejected promptly (without blocking the store or the
//! global lock), and missing leaves must still mean "absent".
#![cfg(unix)] // TODO(windows-phase1): fake-ssh/runner fixtures are shell scripts and modes
use awili_dispatch::protocol::*;
use awili_dispatch::store::Store;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    time::{Duration, Instant},
};
use tempfile::TempDir;

/// Replace a leaf at `path` with a FIFO named the same as `path`. Uses
/// `/bin/mkfifo` so no new dependency is needed.
fn replace_with_fifo(path: &Path) {
    fs::remove_file(path).unwrap();
    let out = Command::new("mkfifo").arg(path).output().unwrap();
    assert!(
        out.status.success(),
        "mkfifo failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    // Ensure the FIFO is writable by the test user.
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
}

/// Lay out a job directory with a control directory, a real record.json, and a
/// real worker.log, so that the caller can swap individual leaves for FIFOs.
fn layout_job(root: &Path, id: &str) -> (PathBuf, PathBuf) {
    let control = root.join(id).join("control");
    fs::create_dir_all(&control).unwrap();
    let record = control.join("record.json");
    fs::write(&record, serde_json::to_string(&new_job_record(id)).unwrap()).unwrap();
    let log = control.join("worker.log");
    fs::write(&log, "seed log line\n").unwrap();
    (record, log)
}

fn new_job_record(id: &str) -> JobRecord {
    let request = BuildRequest {
        request_id: id.into(),
        turn_id: "turn-1".into(),
        conversation_id: "conv-1".into(),
        repo_id: "fixture".into(),
        baseline_sha: "0".repeat(40),
        branch: "fix/fixture-fifo".into(),
        spec_text: "fifo test".into(),
        target_id: "local".into(),
        verify_profile: "tests".into(),
    };
    JobRecord {
        target: BuildTarget {
            repo_id: "fixture".into(),
            repo_url: "file:///dev/null".into(),
            model: "fixture-local-model".into(),
            command: CommandSpec {
                program: PathBuf::from("/bin/true"),
                args: vec![],
                env: BTreeMap::new(),
            },
            verify_profile: "tests".into(),
            verify: CommandSpec {
                program: PathBuf::from("/bin/true"),
                args: vec![],
                env: BTreeMap::new(),
            },
            isolation: None,
        },
        snapshot: BuildSnapshot {
            request_id: id.into(),
            request_sha256: request.fingerprint(),
            revision: 0,
            state: BuildState::Queued,
            executor: "fixture".into(),
            requested_model: "fixture-local-model".into(),
            baseline_sha: "0".repeat(40),
            result_sha: None,
            remote_sha: None,
            model_exit: None,
            verify_exit: None,
            push_exit: None,
            worker: None,
            failure: None,
            no_op: false,
        },
        request,
    }
}

fn new_root() -> (TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("jobs");
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    (temp, root)
}

/// Run `f` on a thread and fail loudly if it does not return within `timeout`.
fn run_with_timeout<T: Send + 'static>(
    timeout: Duration,
    label: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let start = Instant::now();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = f();
        tx.send(r).unwrap();
    });
    let r = rx
        .recv_timeout(timeout)
        .unwrap_or_else(|_| panic!("{label} did not return within {:?} (blocked)", timeout));
    let elapsed = start.elapsed();
    assert!(
        elapsed < timeout,
        "{label} took {:?}, should have been well under {:?}",
        elapsed,
        timeout
    );
    r
}

#[test]
fn record_leaf_fifo_is_rejected_promptly() {
    let (_temp, root) = new_root();
    let id = uuid::Uuid::new_v4().to_string();
    let (record, _log) = layout_job(&root, &id);
    // Swap the record leaf for a FIFO with no writer.
    replace_with_fifo(&record);
    // (1) status must fail within 2 seconds, not block on the FIFO.
    let root1 = root.clone();
    let id1 = id.clone();
    let result = run_with_timeout(Duration::from_secs(2), "status", move || {
        awili_dispatch::runner::status(&root1, &id1)
    });
    assert!(
        result.is_err(),
        "status should fail when the record leaf is a FIFO, got {:?}",
        result
    );
    // (2) The global lock is released: a second call must also return
    //     promptly with an error.
    let root2 = root.clone();
    let id2 = id.clone();
    let second = run_with_timeout(Duration::from_secs(2), "second status", move || {
        awili_dispatch::runner::status(&root2, &id2)
    });
    assert!(
        second.is_err(),
        "second status call should fail, got {:?}",
        second
    );
    // (3) A direct store.read also fails.
    let store = Store::open(&root).unwrap();
    assert!(store.read(&id).is_err());
}

#[test]
fn log_leaf_fifo_is_rejected_promptly() {
    let (_temp, root) = new_root();
    let id = uuid::Uuid::new_v4().to_string();
    let (_record, log) = layout_job(&root, &id);
    replace_with_fifo(&log);
    let store = Store::open(&root).unwrap();
    // logs() must fail within 2 seconds.
    let store2 = store;
    let id2 = id.clone();
    let result = run_with_timeout(Duration::from_secs(2), "logs", move || {
        store2.logs(LogsQuery {
            request_id: id2,
            cursor: 0,
            max_bytes: 64,
        })
    });
    assert!(
        result.is_err(),
        "logs should fail when the log leaf is a FIFO, got {:?}",
        result
    );
    // log_for_append must also fail.
    let store3 = Store::open(&root).unwrap();
    assert!(store3.log_for_append(&id).is_err());
}

#[test]
fn branch_reservation_directory_is_rejected() {
    let (_temp, root) = new_root();
    let store = Store::open(&root).unwrap();
    let key = "branch-key-1";
    // Reserve a real branch first to create the branches directory.
    store
        .reserve_branch(key, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let branch_leaf = root.join("branches").join(key);
    assert!(branch_leaf.is_file());
    // Replace the reservation leaf with a directory.
    fs::remove_file(&branch_leaf).unwrap();
    fs::create_dir(&branch_leaf).unwrap();
    // branch_reserved must now ERROR, not report the directory as reserved.
    let result = store.branch_reserved(key);
    assert!(
        result.is_err(),
        "branch_reserved should reject a directory, got {:?}",
        result
    );
    // reserve_branch must also fail.
    assert!(store
        .reserve_branch(key, &uuid::Uuid::new_v4().to_string())
        .is_err());
}

#[test]
fn missing_leaves_still_mean_absent() {
    let (_temp, root) = new_root();
    let id = uuid::Uuid::new_v4().to_string();
    let store = Store::open(&root).unwrap();
    // (a) Control directory missing -> read fails.
    assert!(store.read(&id).is_err());
    // (b) Job exists with a control dir but the record leaf is missing.
    layout_job(&root, &id);
    let control = root.join(&id).join("control");
    fs::remove_file(control.join("record.json")).unwrap();
    assert!(store.read(&id).is_err());
    // (c) Log leaf missing -> empty page (not an error).
    fs::write(
        control.join("record.json"),
        serde_json::to_string(&new_job_record(&id)).unwrap(),
    )
    .unwrap();
    fs::remove_file(control.join("worker.log")).unwrap();
    let page = store
        .logs(LogsQuery {
            request_id: id.clone(),
            cursor: 0,
            max_bytes: 64,
        })
        .unwrap();
    assert!(page.bytes.is_empty());
    // (d) Branch reservation leaf missing -> Ok(false), not an error.
    let key = "missing-branch";
    assert!(!store.branch_reserved(key).unwrap());
}

/// Snapshot of every path under `root`, relative to `root`.
fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn rec(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().into_owned());
            if p.is_dir() {
                rec(&p, root, out);
            }
        }
    }
    rec(root, root, &mut out);
    out.sort();
    out
}

/// Inputs that must be rejected by name validation, not treated as
/// "absent" or "reserved": multi-component, parent, absolute, empty and
/// NUL-containing keys.
const BAD_KEYS: [&str; 5] = ["a/b", "..", "/etc/passwd", "", "a\0b"];

/// RVS3A-FIX5: branch_reserved must reject invalid keys with the
/// name-validation error (`StoreError::BadName`), proving the key was
/// validated BEFORE any open was attempted. None of these inputs may yield
/// `Ok(true)` or `Ok(false)`; nothing outside the job root may be opened or
/// touched, and the call must create nothing — not even `branches/`.
#[test]
fn branch_reserved_rejects_invalid_keys() {
    use awili_dispatch::store::StoreError;
    let (_temp, root) = new_root();
    let store = Store::open(&root).unwrap();
    // Sentinel files immediately OUTSIDE the job root: if the key ever
    // reached the OS, these are what a traversal could touch.
    let sentinel_a = _temp.path().join("escape-a");
    let sentinel_b = _temp.path().join("etc").join("passwd");
    fs::create_dir_all(sentinel_b.parent().unwrap()).unwrap();
    fs::write(&sentinel_a, b"x").unwrap();
    fs::write(&sentinel_b, b"x").unwrap();
    // No leaf may be created inside branches/: if a bad key ever reached an
    // openat it would create or touch a leaf file. `branches/` itself is
    // created by `branches_fd()` (existing store behavior), but no leaf
    // file inside it may appear.
    let branches = root.join("branches");
    for key in BAD_KEYS {
        assert!(
            !branches.exists(),
            "fixture: branches/ must be absent before branch_reserved({key:?})"
        );
        let r = store.branch_reserved(key);
        assert!(r.is_err(), "branch_reserved({key:?}) must error, got {r:?}");
        // The error must be the name-validation variant, not an open
        // failure: validation runs BEFORE any openat.
        let err = r.unwrap_err();
        let se = err.downcast_ref::<StoreError>();
        assert!(
            matches!(se, Some(StoreError::BadName)),
            "branch_reserved({key:?}) must fail with StoreError::BadName, got {err:?}"
        );
        // RVS3A-FIX5: the call created nothing at all — not even
        // `branches/`, which `branches_fd()` would create if it ran. The
        // key must be validated before any mkdirat or openat is issued.
        assert!(
            !branches.exists(),
            "branch_reserved({key:?}) created branches/"
        );
    }
    // No traversal, no creation outside the job root: the sentinels must
    // still be untouched files, and nothing new may appear anywhere in the
    // temp root except under the job root.
    assert!(sentinel_a.is_file(), "sentinel A missing (touched)");
    assert!(sentinel_b.is_file(), "sentinel B missing (touched)");
    let temp_root = _temp.path().to_path_buf();
    let created = walk(&temp_root);
    for p in &created {
        assert!(
            matches!(p.as_str(), "escape-a" | "etc" | "etc/passwd" | "jobs")
                || p.starts_with("jobs/"),
            "unexpected path outside job root created: {p}"
        );
    }
}

/// RVS3A-FIX4: a multi-component key must be rejected by name validation
/// even when a nested directory `branches/a` containing a regular file `b`
/// exists — the key is refused before any open, so the nested file is
/// never reported as a reservation.
#[test]
fn branch_reserved_rejects_nested_key() {
    use awili_dispatch::store::StoreError;
    let (_temp, root) = new_root();
    let store = Store::open(&root).unwrap();
    // Create the populated nested fixture: `branches/a/b` as a regular file.
    let nested = root.join("branches").join("a");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("b"), b"nested").unwrap();
    assert!(
        nested.join("b").is_file(),
        "fixture: branches/a/b not a file"
    );
    // `a/b` is a multi-component key: rejected by name validation, not
    // reported reserved, regardless of the nested file's existence.
    let r = store.branch_reserved("a/b");
    assert!(r.is_err(), "branch_reserved(\"a/b\") must error, got {r:?}");
    let err = r.unwrap_err();
    assert!(
        matches!(err.downcast_ref::<StoreError>(), Some(StoreError::BadName)),
        "branch_reserved(\"a/b\") must fail with StoreError::BadName, got {err:?}"
    );
    // The nested file is untouched: validation ran before any open.
    assert!(
        nested.join("b").is_file(),
        "fixture: branches/a/b was modified by name validation"
    );
}

/// RVS3A-FIX4: logs must reject a request id that would reach the store's
/// single-component rule through the same validated path. These ids are
/// invalid before any leaf is opened; none may be treated as "absent".
///
/// The id-level rejection is enforced by `valid_id` (the protocol layer),
/// which runs before the store sees the id at all. To prove the store's
/// log-leaf code itself is exercised — not just the id guard — the
/// FIX4-specific cases below use a VALID job id whose log leaf has been
/// swapped for a FIFO, a directory, or is missing.
#[test]
fn logs_rejects_invalid_keys() {
    let (_temp, root) = new_root();
    let store = Store::open(&root).unwrap();
    let sentinel_a = _temp.path().join("escape-a");
    let sentinel_b = _temp.path().join("etc").join("passwd");
    fs::create_dir_all(sentinel_b.parent().unwrap()).unwrap();
    fs::write(&sentinel_a, b"x").unwrap();
    fs::write(&sentinel_b, b"x").unwrap();
    for key in BAD_KEYS {
        let r = store.logs(LogsQuery {
            request_id: key.to_string(),
            cursor: 0,
            max_bytes: 64,
        });
        assert!(r.is_err(), "logs({key:?}) must error, got {r:?}");
    }
    assert!(sentinel_a.is_file(), "sentinel A missing (touched)");
    assert!(sentinel_b.is_file(), "sentinel B missing (touched)");
    let temp_root = _temp.path().to_path_buf();
    let created = walk(&temp_root);
    for p in &created {
        assert!(
            matches!(p.as_str(), "escape-a" | "etc" | "etc/passwd" | "jobs")
                || p.starts_with("jobs/"),
            "unexpected path outside job root created: {p}"
        );
    }
}

/// RVS3A-FIX4: with a VALID job id, the log-leaf code must be exercised:
/// a FIFO log leaf is rejected promptly (never treated as absent), a
/// directory log leaf is rejected, and a missing log leaf returns an empty
/// page (absent). These cases bypass the `valid_id` guard because the id
/// is a real UUID, so the store's nonblocking open + fstat regular-file
/// check on the log leaf is what is proven.
#[test]
fn logs_with_valid_id_exercises_leaf_check() {
    let (_temp, root) = new_root();
    let id = uuid::Uuid::new_v4().to_string();
    let (_record, log) = layout_job(&root, &id);
    let store = Store::open(&root).unwrap();

    // (a) FIFO log leaf with a valid id: must error, not return an empty
    // page — the fstat regular-file check rejects the FIFO before any read.
    replace_with_fifo(&log);
    let r = store.logs(LogsQuery {
        request_id: id.clone(),
        cursor: 0,
        max_bytes: 64,
    });
    assert!(r.is_err(), "logs with FIFO log leaf must error, got {r:?}");

    // (b) directory log leaf with a valid id: must error — the fstat check
    // rejects a directory substituted for the leaf.
    fs::remove_file(&log).unwrap();
    fs::create_dir(&log).unwrap();
    let r = store.logs(LogsQuery {
        request_id: id.clone(),
        cursor: 0,
        max_bytes: 64,
    });
    assert!(
        r.is_err(),
        "logs with directory log leaf must error, got {r:?}"
    );

    // (c) missing log leaf with a valid id: must return an empty page
    // (absent), per the store's existing contract.
    fs::remove_dir(&log).unwrap();
    let page = store
        .logs(LogsQuery {
            request_id: id.clone(),
            cursor: 0,
            max_bytes: 64,
        })
        .unwrap();
    assert!(
        page.bytes.is_empty(),
        "logs with missing log leaf must be empty, got {:?}",
        page.bytes
    );
}

/// Sanity check that a regular file is NOT rejected: the store's helpers
/// still work when the leaves are real files.
#[test]
fn regular_leaves_still_work() {
    let (_temp, root) = new_root();
    let id = uuid::Uuid::new_v4().to_string();
    layout_job(&root, &id);
    let store = Store::open(&root).unwrap();
    let rec = store.read(&id).unwrap();
    assert_eq!(rec.request.request_id, id);
    let page = store
        .logs(LogsQuery {
            request_id: id.clone(),
            cursor: 0,
            max_bytes: 64,
        })
        .unwrap();
    assert!(!page.bytes.is_empty());
    // Append to the log; it must work.
    {
        let mut append = store.log_for_append(&id).unwrap();
        writeln!(append, "appended").unwrap();
    }
    let page = store
        .logs(LogsQuery {
            request_id: id.clone(),
            cursor: 0,
            max_bytes: 64,
        })
        .unwrap();
    assert!(
        String::from_utf8_lossy(&page.bytes).contains("appended"),
        "log append lost: {:?}",
        String::from_utf8_lossy(&page.bytes)
    );
    // Branch reservation roundtrip.
    let key = "real-branch";
    store.reserve_branch(key, &id).unwrap();
    assert!(store.branch_reserved(key).unwrap());
}
