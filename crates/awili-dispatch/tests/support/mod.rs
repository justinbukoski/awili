//! Shared helpers for the awili-dispatch integration tests.
#![allow(dead_code)] // each test crate uses only some of them

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

/// Write `body` to `path` as an executable with `mode`, without this process
/// ever opening `path` for writing (RC1-G2, as awili-app's exec_fixture).
///
/// On Linux `execve` refuses a file that any process still holds open for
/// writing (ETXTBSY, "Text file busy"). A fixture written with `fs::write` is
/// open for writing for a moment, and a sibling test that forks a child in
/// that moment hands the child a copy of the descriptor until it execs; the
/// fixture's own exec then fails at random, more often under load. Here the
/// bytes go to `<path>.staging` (never executed) and a separate `cp` process
/// creates the executable.
pub fn write_exec_fixture(path: &Path, body: impl AsRef<[u8]>, mode: u32) {
    let staging = path.with_extension("staging");
    fs::write(&staging, body).unwrap();
    copy_exec_fixture(&staging, path, mode);
    fs::remove_file(&staging).unwrap();
}

/// Copy `source` to `path` as an executable with `mode` through a separate
/// `cp` process, for the same reason as [`write_exec_fixture`] (`fs::copy`
/// opens the destination for writing in this process).
pub fn copy_exec_fixture(source: &Path, path: &Path, mode: u32) {
    let status = Command::new("cp").arg(source).arg(path).status().unwrap();
    assert!(
        status.success(),
        "cp {} {}",
        source.display(),
        path.display()
    );
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// How long a test waits for a stand-in command (fake `ssh` and the like)
/// that is expected to **answer**, as awili-app's
/// `exec_fixture::ANSWER_DEADLINE`: on macOS the first exec of a freshly
/// written script can stall for seconds while the system scans it, worst
/// while the gate builds in parallel, and a 2-3 s budget then failed as
/// "command deadline exceeded" although nothing was wrong. Only a failure
/// bound; tests whose point is a deadline or a hang keep their own short
/// ones.
pub const ANSWER_DEADLINE: Duration = Duration::from_secs(15);

/// Lock a shared fixture mutex, poison-tolerant: one failing test must not
/// fail every later test that shares the fixture.
pub fn lock<T>(fixture: &Mutex<T>) -> MutexGuard<'_, T> {
    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
