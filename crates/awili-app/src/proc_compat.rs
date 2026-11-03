//! Small cross-platform process helpers: own process group at spawn, force
//! kill by pid, liveness probes. Unix keeps the exact behaviour the call sites
//! had (process groups, `kill(2)`, `waitid`). Windows runs every vendor CLI in
//! a Job Object (`winjob`): Stop terminates the whole tree, grandchildren
//! included, and a failed stop is reported, never ignored.

use std::process::Command;

pub(crate) trait OwnProcessGroup {
    /// Put the child in its own process group (Unix: `process_group(0)`, so
    /// pgid == child pid; Windows: `CREATE_NEW_PROCESS_GROUP`, and no console
    /// window for a console CLI launched from the GUI app).
    fn own_process_group(&mut self) -> &mut Self;
}

impl OwnProcessGroup for Command {
    #[cfg(unix)]
    fn own_process_group(&mut self) -> &mut Self {
        use std::os::unix::process::CommandExt;
        self.process_group(0)
    }
    #[cfg(windows)]
    fn own_process_group(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        self.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
    }
}

/// Spawn a vendor CLI so that [`kill_pid`] can later end all of it. Unix: a
/// plain spawn (the command was put in its own process group). Windows: the
/// child is created suspended, placed in a Job Object with
/// `KILL_ON_JOB_CLOSE`, and then resumed; a child that cannot be put in a job
/// is not started.
pub(crate) fn spawn_tracked(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    {
        crate::winjob::spawn_in_job(cmd)
    }
    #[cfg(not(windows))]
    {
        cmd.spawn()
    }
}

/// Forcefully end process `pid` (a child we spawned and still track). Unix:
/// SIGKILL; a process already gone counts as stopped. Windows: terminate its
/// Job Object (the whole tree). A pid Awili did not start has no job and is
/// NEVER touched (no `taskkill` by pid, which could hit an unrelated process
/// that reused the number): that is an error. A failure is returned for the
/// caller to report.
pub(crate) fn kill_pid(pid: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: SIGKILL to a pid we spawned and still track.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(()); // already gone
        }
        Err(err)
    }
    #[cfg(windows)]
    {
        if crate::winjob::terminate(pid)? {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "process {pid} has no job: already stopped, or not started by Awili (left alone)"
            ),
        ))
    }
}

/// Report a failed [`kill_pid`] where there is no caller to return it to.
pub(crate) fn kill_pid_reporting(pid: u32) {
    match kill_pid(pid) {
        Ok(()) => {}
        // Nothing of ours to stop (already ended, or never ours): not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("awili: could not stop process {pid}: {e}"),
    }
}

/// Whether a process with this pid exists (any owner).
#[cfg(unix)]
pub(crate) fn pid_exists(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Whether a process with this pid exists. Windows: `tasklist` filtered to the
/// pid; on any failure to ask, answer "exists" so nothing is cleaned up or
/// reaped on a guess.
#[cfg(windows)]
pub(crate) fn pid_exists(pid: i32) -> bool {
    use std::process::Stdio;
    let out = Command::new(crate::agent_bin::system_tool("tasklist"))
        .args(["/NH", "/FO", "CSV", "/FI", &format!("PID eq {pid}")])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\""))
        }
        _ => true,
    }
}

/// RC2-APP (F0175): whether child `pid` has exited (an unreaped exit
/// included) or is no longer this process's child (already reaped by its
/// owner). Never reaps: `WNOWAIT` leaves the exit status for the owner.
#[cfg(unix)]
pub(crate) fn child_has_exited(pid: u32) -> bool {
    // SAFETY: an all-zero siginfo_t is a valid out-parameter; waitid only
    // writes it.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: P_PID with a pid we spawned; WNOHANG never blocks and
    // WNOWAIT leaves the child waitable.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        // ECHILD: not (or no longer) our child — its owner reaped it.
        // EINTR: ask again on the next tick.
        return std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR);
    }
    // SAFETY: waitid filled `info`; si_pid is 0 while the child still runs.
    unsafe { info.si_pid() != 0 }
}

/// Windows has no zombie state: an exited child's pid is gone once its handle
/// closes, so "exited" is "no such process".
#[cfg(windows)]
pub(crate) fn child_has_exited(pid: u32) -> bool {
    !pid_exists(pid as i32)
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let dump = Command::new("powershell")
            .args(["-NoProfile", "-Command", "Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'ping|cmd' } | ForEach-Object { \"$($_.ProcessId) <- $($_.ParentProcessId) $($_.CommandLine)\" }"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        panic!("timed out waiting for {what}; processes:\n{dump}");
    }

    /// PIDs of `ping.exe` processes whose command line has `marker`.
    fn ping_pids(marker: &str) -> Vec<u32> {
        let script = format!(
            "Get-CimInstance Win32_Process -Filter \"Name='PING.EXE'\" | \
             Where-Object {{ $_.CommandLine -like '*{marker}*' }} | \
             ForEach-Object {{ $_.ProcessId }}"
        );
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }

    #[test]
    fn a_tracked_child_runs_normally_and_reports_its_exit_code() {
        let mut cmd = Command::new(r"C:\Windows\System32\cmd.exe");
        cmd.args(["/c", "echo hello & exit 7"])
            .stdout(Stdio::piped())
            .own_process_group();
        let out = spawn_tracked(&mut cmd).unwrap().wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(7));
        assert!(String::from_utf8_lossy(&out.stdout).contains("hello"));
    }

    #[test]
    fn stop_kills_the_child_and_its_children() {
        let marker = "-n 211";
        let mut cmd = Command::new(r"C:\Windows\System32\cmd.exe");
        cmd.args([
            "/c",
            &format!("C:\\Windows\\System32\\PING.EXE {marker} 127.0.0.1 >nul"),
        ])
        .stdout(Stdio::null())
        .own_process_group();
        let mut child = spawn_tracked(&mut cmd).unwrap();
        let pid = child.id();
        let mut ping = Vec::new();
        wait_until("the grandchild to start", || {
            ping = ping_pids(marker);
            !ping.is_empty()
        });
        assert!(crate::winjob::active_in_job(pid).unwrap() >= 2);
        kill_pid(pid).unwrap();
        child.wait().unwrap();
        wait_until("the grandchild to end", || {
            ping.iter().all(|p| !pid_exists(*p as i32))
        });
    }

    /// The case `taskkill /T` could not handle: the parent has already exited
    /// and its child lives on with no parent link to walk.
    #[test]
    fn stop_kills_an_orphaned_grandchild() {
        let marker = "-n 212";
        let mut cmd = Command::new(r"C:\Windows\System32\cmd.exe");
        cmd.args([
            "/c",
            &format!("start /b C:\\Windows\\System32\\PING.EXE {marker} 127.0.0.1 >nul"),
        ])
        .stdout(Stdio::null())
        .own_process_group();
        let mut child = spawn_tracked(&mut cmd).unwrap();
        let pid = child.id();
        child.wait().unwrap(); // the parent is gone
        let mut ping = Vec::new();
        wait_until("the orphan to be running", || {
            ping = ping_pids(marker);
            !ping.is_empty()
        });
        assert!(
            crate::winjob::active_in_job(pid).unwrap() >= 1,
            "the orphan is still in the job"
        );
        kill_pid(pid).unwrap();
        wait_until("the orphan to end", || {
            ping.iter().all(|p| !pid_exists(*p as i32))
        });
    }

    /// A pid Awili did not start is never touched: no `taskkill` by pid, so
    /// an unrelated process (or one that reused a number) survives.
    #[test]
    fn an_unregistered_pid_is_left_alone() {
        let mut other = Command::new(r"C:\Windows\System32\PING.EXE")
            .args(["-n", "213", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let err = kill_pid(other.id()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            other.try_wait().unwrap().is_none(),
            "it must still be running"
        );
        let _ = other.kill();
        let _ = other.wait();
        // A pid that no longer exists is likewise not an error to report.
        assert_eq!(
            kill_pid(0x0FFF_FFF0).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn leftover_briefs_are_swept_only_from_an_owner_only_folder() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("brief");
        std::fs::create_dir(&dir).unwrap();
        let stale = dir.join("brief-4000000-1-1.md");
        let other = dir.join("notes.txt");
        std::fs::write(&stale, "x").unwrap();
        std::fs::write(&other, "x").unwrap();
        // Not owner-only yet: nothing is trusted, nothing removed.
        assert_eq!(crate::code_provider::sweep_briefs_in(&dir), 0);
        assert!(stale.exists());
        awili_winacl::restrict_to_owner(&dir, true).unwrap();
        assert_eq!(crate::code_provider::sweep_briefs_in(&dir), 1);
        assert!(!stale.exists());
        assert!(other.exists(), "other names stay");
    }
}
