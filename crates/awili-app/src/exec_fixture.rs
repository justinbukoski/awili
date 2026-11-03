//! RC1-G2: executable test fixtures that cannot fail with ETXTBSY.
//!
//! On Linux `execve` refuses a file that any process still holds open for
//! writing ("Text file busy", os error 26). A fixture written by this test
//! process is open for writing for a moment, and a sibling test that forks a
//! child in that moment hands the child a copy of the descriptor until the
//! child execs. The fixture's own exec then fails at random, more often under
//! load: `build_dispatch_p10_tests` lost its `submit` (the runner saw only the
//! `status` fallback) and the fake `dsh` never ran (RC-16). Here a separate
//! `cp` process creates the executable, so no thread of this process ever
//! holds it open for writing.
//!
//! Also the shared time budget for stand-ins that must ANSWER, and the
//! poison-tolerant lock for the shared fixture mutexes, and conversation
//! metadata for synthetic transcript owners.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// How long a test waits for a stand-in CLI that is expected to **answer**
/// (start, reply, write its output file). On macOS the first exec of a
/// freshly written script can stall for seconds while the system scans it,
/// worst while the gate builds in parallel; a 1-3 s wait then fails as
/// "deadline exceeded" / `Unknown` / a missing output file although nothing
/// is wrong. Only a failure bound: a stand-in that answers ends the wait at
/// once. Tests whose point is a deadline or a hang keep their own short
/// deadlines and bounds.
pub(crate) const ANSWER_DEADLINE: Duration = Duration::from_secs(15);

/// Lock a shared fixture mutex, poison-tolerant: one failing test must not
/// fail every later test that shares the fixture.
pub(crate) fn lock<T>(fixture: &Mutex<T>) -> MutexGuard<'_, T> {
    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write `body` to `path` as an executable with `mode`, without this process
/// ever opening `path` for writing (the bytes go through `<path>.staging`,
/// which is never executed).
pub(crate) fn write_exec_fixture(path: &Path, body: &str, mode: u32) {
    let staging = path.with_extension("staging");
    std::fs::write(&staging, body).unwrap();
    let status = std::process::Command::new("cp")
        .arg(&staging)
        .arg(path)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "cp {} {}",
        staging.display(),
        path.display()
    );
    std::fs::remove_file(&staging).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = mode;
}

/// Seed the metadata half of a synthetic transcript owner. Real startup and
/// conversation creation always supply both; fixture-only views must do so too.
pub(crate) fn seed_conversation_metadata(
    app: &mut crate::AwiliApp,
    id: &str,
    kind: awili_core::ConvKind,
) {
    if app.conversations.get(id).is_some() {
        return;
    }
    let mut items = app.conversations.all().to_vec();
    items.push(awili_core::ConversationMeta {
        id: id.into(),
        title: id.into(),
        created_ts: 0,
        updated_ts: 0,
        archived: false,
        title_is_custom: false,
        cwd: None,
        profile: None,
        kind,
    });
    app.conversations = awili_core::ConversationList::from_items(items);
}
