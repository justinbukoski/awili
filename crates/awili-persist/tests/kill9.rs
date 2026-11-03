//! Real process-death crash-safety test (review slice 2, SF5).
//!
//! The in-crate `reopen_without_clean_close_replays_wal` test only `mem::forget`s
//! a still-live connection in the same process — it never releases OS locks or
//! terminates the process, so it cannot actually demonstrate recovery after a
//! `kill -9`. This test does the real thing: it re-executes THIS test binary as a
//! child, has the child commit rows and then spin, sends the child SIGKILL (via
//! `Child::kill`, which is SIGKILL on Unix), and reopens the database from the
//! parent — a genuinely fresh process — to prove the committed rows survived.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use awili_persist::Store;

/// Env var carrying the child's database path. When set, the test body below runs
/// in "child mode": write committed rows, signal readiness, then spin so the
/// parent can SIGKILL us mid-life.
const CHILD_DB_ENV: &str = "AWILI_KILL9_CHILD_DB";

#[test]
fn kill9_after_commit_preserves_rows_in_a_fresh_process() {
    if let Ok(db) = std::env::var(CHILD_DB_ENV) {
        run_child(&db);
        // run_child never returns (it spins until SIGKILL); unreachable.
        return;
    }
    run_parent();
}

/// CHILD: open the db, commit two rows, touch a `.ready` marker, then spin forever.
fn run_child(db: &str) {
    let store = Store::open(Path::new(db)).expect("child opens db");
    let conv = "11111111-2222-4333-8444-555555555555";
    store
        .create_conversation(conv, "kill9", 42)
        .expect("create conversation");
    store
        .append_message(conv, "user", "before the crash", 0)
        .expect("append user");
    store
        .append_message(conv, "assistant", "completed reply", 1)
        .expect("append assistant");
    // Signal the parent that the committed rows are on disk and it may SIGKILL us.
    std::fs::write(ready_marker(db), b"ready").expect("write ready marker");
    // Spin: the parent will kill -9 this process at any moment. A bounded ceiling
    // keeps a stray child from outliving the test run.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::process::exit(0);
}

/// PARENT: spawn the child, wait for readiness, SIGKILL it, reopen, assert.
fn run_parent() {
    let db = temp_db();
    let marker = ready_marker(&db.to_string_lossy());
    let exe = std::env::current_exe().expect("current test exe");

    // Re-run only this test in a child process, in child mode.
    let mut child = Command::new(exe)
        .args([
            "kill9_after_commit_preserves_rows_in_a_fresh_process",
            "--exact",
            "--nocapture",
        ])
        .env(CHILD_DB_ENV, &db)
        .spawn()
        .expect("spawn child");

    // Wait (bounded) for the child to commit its rows and signal readiness.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !marker.exists() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("child never signalled readiness");
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // SIGKILL the child while it is alive (no Drop, no checkpoint, OS locks
    // released only by the kernel on process death) — the true kill -9 scenario.
    child.kill().expect("SIGKILL child");
    child.wait().expect("reap child");

    // Reopen from the parent: a fresh process opening a file the dead child never
    // closed. WAL replay must surface every committed row.
    let reopened = Store::open(&db).expect("parent reopens db");
    let convs = reopened.conversations().expect("query conversations");
    assert_eq!(convs.len(), 1, "the child's conversation exists");
    let msgs = reopened.messages(&convs[0].id).expect("read messages");
    assert_eq!(msgs.len(), 2, "both committed rows survived the SIGKILL");
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[0].text, "before the crash");
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(msgs[1].text, "completed reply");

    cleanup(&db);
    let _ = std::fs::remove_file(&marker);
}

fn temp_db() -> PathBuf {
    std::env::temp_dir().join(format!("awili-kill9-{}.db", std::process::id()))
}

fn ready_marker(db: &str) -> PathBuf {
    PathBuf::from(format!("{db}.ready"))
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}
