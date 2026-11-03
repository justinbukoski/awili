//! RC2-APP (K-flake2): scratch folders for tests that do not pile up.
//!
//! Every folder a test asks for lives under ONE folder per test process,
//! `<temp>/awili-app-tests-<pid>/<label>-<n>`, made empty on first use. The
//! first use in a process also removes what ENDED test processes left behind
//! — this layout and the older pid-named ones (`awili-settings-test-<pid>-…`
//! and the rest in [`SWEPT`]) — so the temp folder no longer collects
//! thousands of leftovers. A folder is removed only when the process its
//! name carries is gone; a running test process (or a reused pid that is
//! alive) is never touched.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// This layout's per-process folder prefix.
const ROOT_PREFIX: &str = "awili-app-tests-";

/// The temp-folder names, each followed by a process id, that a test process
/// of this crate leaves behind (this layout first, then the older ones).
const SWEPT: &[&str] = &[
    ROOT_PREFIX,
    "awili-settings-test-",
    "awili-app-graduation-",
    "awili-app-graduation-dir-",
    "awili-app-graduation-parent-",
    "awili-app-graduation-s5b-dir-",
    "awili-app-u1c-ledger-",
    "awili-launch-test-",
    "awili-c2u2-",
    "awili-u3-sdc-",
    "awili-u3-cancel-",
    "awili-u3-tomb-",
    "awili-u3-race2-",
    "awili-u3-epoch-",
    "awili-attfail-",
    "awili-relaunch-",
    "awili-invalid-rec-",
    "awili-reload-rec-",
    "awili-verified-",
];

/// This test process's own scratch root (created, and the leftovers of ended
/// processes swept, on first use).
fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let temp = std::env::temp_dir();
        sweep(&temp);
        let root = temp.join(format!("{ROOT_PREFIX}{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create the test scratch root");
        root
    })
}

/// A fresh, empty folder for one test: `<root>/<label>-<n>`.
pub(crate) fn fresh_dir(label: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = root().join(format!("{label}-{}", N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create a test scratch folder");
    dir
}

/// One folder per test process for `label`, the same path on every call
/// (not created here; never emptied) — for a fixture that stands for a
/// per-process place, such as the test-only home of `agent_launch`.
pub(crate) fn process_dir(label: &str) -> PathBuf {
    root().join(label)
}

/// Remove, from `temp`, every entry named `<swept prefix><pid>` (optionally
/// followed by `-…` or `.…`) whose process has ended.
fn sweep(temp: &Path) {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(pid) = left_by(name) else {
            continue;
        };
        if process_has_ended(pid) {
            let path = entry.path();
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// The process id a swept name carries (`None` for any other name). The
/// longest matching prefix decides, so `awili-app-graduation-dir-12` is read
/// as pid 12, not as a name with no pid.
fn left_by(name: &str) -> Option<u32> {
    SWEPT
        .iter()
        .filter_map(|prefix| name.strip_prefix(prefix))
        .filter_map(|rest| {
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let (pid, tail) = rest.split_at(digits);
            (!pid.is_empty() && (tail.is_empty() || tail.starts_with(['-', '.'])))
                .then(|| pid.parse().ok())
                .flatten()
        })
        .next()
}

/// Whether no process with this id exists any more.
#[cfg(unix)]
fn process_has_ended(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 sends nothing; it only asks whether the pid exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn process_has_ended(pid: u32) -> bool {
    i32::try_from(pid).is_ok_and(|p| p > 0 && !crate::proc_compat::pid_exists(p))
}

/// RC2-APP (K-flake2): an ended process's folders (this layout and an older
/// one) are swept; a live process's — this one's included — are kept; and
/// a fresh folder starts empty under this process's root.
#[test]
fn rc2app_leftovers_of_ended_test_processes_are_swept() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let ended = child.id();
    child.wait().unwrap();
    let me = std::process::id();
    let names = [
        format!("{ROOT_PREFIX}{ended}"),
        format!("awili-settings-test-{ended}-3"),
        format!("awili-app-graduation-dir-{ended}"),
        format!("awili-settings-test-{me}-3"),
        format!("{ROOT_PREFIX}{me}"),
        "awili-settings-test-notapid".to_string(),
        "someone-elses-folder-1".to_string(),
    ];
    for name in &names {
        std::fs::create_dir_all(temp.path().join(name).join("inside")).unwrap();
    }
    assert!(process_has_ended(ended));
    sweep(temp.path());
    let left: Vec<bool> = names.iter().map(|n| temp.path().join(n).exists()).collect();
    assert_eq!(left, [false, false, false, true, true, true, true]);
    assert_eq!(left_by("awili-app-graduation-dir-12"), Some(12));
    assert_eq!(left_by("awili-app-graduation-12-tag-3"), Some(12));
    assert_eq!(left_by("awili-app-graduation-dirx"), None);

    let dir = fresh_dir("rc2app");
    assert!(dir.starts_with(root()) && dir.is_dir());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    assert!(root().ends_with(format!("{ROOT_PREFIX}{me}")));
}

/// RC2-APP (K-flake2): a per-process place is one path under this process's
/// root, the same on every call.
#[test]
fn rc2app_a_process_dir_is_one_path_per_process() {
    let a = process_dir("launch-home");
    assert_eq!(a, process_dir("launch-home"));
    assert!(a.starts_with(root()));
    assert_eq!(
        crate::agent_launch::LaunchPaths::production().map(|p| p.home),
        Some(a),
        "the test-only launch home lives there"
    );
}
