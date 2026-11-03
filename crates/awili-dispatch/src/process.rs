//! Fail-closed process identity checks before signalling a worker group.
//!
//! RC2-R4 (TOP43 #14): on Linux, where the runner runs (the model and the
//! verifier start only under the Linux bwrap profile), cancellation never
//! signals a process by number after its checks. It opens a pidfd for the
//! worker and for every process in the worker's group, proves each pidfd
//! names the process it checked (start time and group read again after the
//! open), proves the worker was still running after the scan (so its group
//! id could not have been reused by then), and signals through those pidfds
//! only. A reused PID or group id can never receive the signal. If the worker
//! exits during the scan, nothing is signalled and the cancel is reported as
//! unconfirmed. On other systems (tests only) the checks run as before and
//! the group signal is sent directly.
use crate::protocol::WorkerIdentity;
use anyhow::{ensure, Result};
use std::process::{Command, Stdio};

/// Start time (clock ticks since boot) and process group of `pid`, from
/// `/proc/<pid>/stat`; `None` when gone or a zombie.
#[cfg(target_os = "linux")]
fn stat_of(pid: u32) -> Option<(String, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    if fields.first() == Some(&"Z") {
        return None;
    }
    let pgrp = fields.get(2)?.parse().ok()?;
    Some((fields.get(19)?.to_string(), pgrp))
}

pub fn identity(pid: u32) -> Option<WorkerIdentity> {
    #[cfg(target_os = "linux")]
    {
        let (start, _) = stat_of(pid)?;
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        Some(WorkerIdentity {
            pid,
            stamp: format!("{}:{}", boot.trim(), start),
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = Command::new("/bin/ps")
            .args(["-o", "lstart=,stat=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let stamp = String::from_utf8(output.stdout).ok()?.trim().to_string();
        if !output.status.success()
            || stamp.is_empty()
            || stamp.split_whitespace().last()?.starts_with('Z')
        {
            return None;
        }
        // Exclude mutable process state from the creation-time identity.
        let stamp = stamp
            .split_whitespace()
            .take(5)
            .collect::<Vec<_>>()
            .join(" ");
        Some(WorkerIdentity { pid, stamp })
    }
}
pub fn alive(worker: &WorkerIdentity) -> bool {
    identity(worker.pid).as_ref() == Some(worker)
}
pub fn terminate(worker: &WorkerIdentity, job_id: &str) -> Result<()> {
    terminate_checked(worker, job_id, &mut || {})
}

/// [`terminate`], with `after_scan` run after every check and before any
/// signal (a test seam: it lets a test end the worker at that point).
#[cfg(target_os = "linux")]
pub(crate) fn terminate_checked(
    worker: &WorkerIdentity,
    job_id: &str,
    after_scan: &mut dyn FnMut(),
) -> Result<()> {
    use rustix::process::{pidfd_open, pidfd_send_signal, Pid, PidfdFlags, Signal};
    let pin =
        |pid: u32| Pid::from_raw(pid as i32).and_then(|p| pidfd_open(p, PidfdFlags::empty()).ok());
    // The pidfd names whatever process holds this PID now; the identity
    // check AFTER the open proves it is the worker.
    let leader = pin(worker.pid);
    ensure!(
        leader.is_some() && alive(worker),
        "worker identity is no longer provable"
    );
    let cmdline = std::fs::read(format!("/proc/{}/cmdline", worker.pid)).unwrap_or_default();
    ensure!(
        alive(worker)
            && cmdline
                .split(|b| *b == 0)
                .any(|arg| arg == job_id.as_bytes()),
        "worker command ownership mismatch"
    );
    // Every other process in the worker's group, each pinned and proved.
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc")?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == worker.pid {
            continue;
        }
        let Some(before) = stat_of(pid).filter(|(_, pgrp)| *pgrp == worker.pid) else {
            continue;
        };
        if let Some(fd) = pin(pid) {
            if stat_of(pid) == Some(before) {
                members.push(fd);
            }
        }
    }
    after_scan();
    // Still running after the scan: its group id was never free to reuse, so
    // every member found above belongs to the worker's group.
    ensure!(
        alive(worker),
        "worker exited during cancellation; nothing signalled"
    );
    for fd in &members {
        // A member that exited since is not an error.
        let _ = pidfd_send_signal(fd, Signal::TERM);
    }
    let leader = leader.expect("checked above");
    ensure!(
        pidfd_send_signal(&leader, Signal::TERM).is_ok(),
        "group termination could not be requested"
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn terminate_checked(
    worker: &WorkerIdentity,
    job_id: &str,
    after_scan: &mut dyn FnMut(),
) -> Result<()> {
    ensure!(alive(worker), "worker identity is no longer provable");
    let cmd = Command::new("/bin/ps")
        .args(["-o", "command=", "-p", &worker.pid.to_string()])
        .output()?;
    let text = String::from_utf8_lossy(&cmd.stdout);
    ensure!(
        text.split_whitespace().any(|s| s == job_id),
        "worker command ownership mismatch"
    );
    after_scan();
    ensure!(
        alive(worker),
        "worker exited during cancellation; nothing signalled"
    );
    let pid = rustix::process::Pid::from_raw(worker.pid as i32)
        .ok_or_else(|| anyhow::anyhow!("invalid worker pid"))?;
    ensure!(
        rustix::process::kill_process_group(pid, rustix::process::Signal::TERM).is_ok(),
        "group termination could not be requested"
    );
    Ok(())
}
pub fn group_exists(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::time::{Duration, Instant};

    /// A worker stand-in: a shell leading its own group, `job` in its
    /// arguments, with two `sleep` members. Returns once both members run.
    fn group(job: &str) -> (std::process::Child, Vec<u32>) {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 30 & sleep 30 & wait", "sh", job])
            .process_group(0)
            .spawn()
            .unwrap();
        let leader = child.id();
        let deadline = Instant::now() + Duration::from_secs(10);
        let members = loop {
            let members = members_of(leader);
            if members.len() >= 2 {
                break members;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the members never started");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        (child, members)
    }

    fn members_of(leader: u32) -> Vec<u32> {
        std::fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter(|p| *p != leader && stat_of(*p).is_some_and(|(_, g)| g == leader))
            .collect()
    }

    fn wait_gone(leader: u32) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while group_exists(leader) {
            assert!(Instant::now() < deadline, "the group is still running");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// RC2-R4 (TOP43 #14): cancel reaches the worker and every member of its
    /// group, each through its own pidfd.
    #[test]
    fn rc2r4_cancel_reaches_every_member_of_the_worker_group() {
        let job = "00000000-0000-4000-8000-0000000000a1";
        let (mut child, _) = group(job);
        let worker = identity(child.id()).expect("the worker's identity");
        terminate(&worker, job).unwrap();
        child.wait().unwrap();
        wait_gone(worker.pid);
    }

    /// RC2-R4 (TOP43 #14): when the worker exits between the checks and the
    /// signal, its group id may be free for reuse, so nothing is signalled
    /// and the cancel is unconfirmed (the status check then says
    /// NeedsAttention). Its remaining members keep running.
    #[test]
    fn rc2r4_a_worker_that_exits_mid_cancel_gets_no_signal() {
        let job = "00000000-0000-4000-8000-0000000000a2";
        let (mut child, members) = group(job);
        let worker = identity(child.id()).expect("the worker's identity");
        let err = terminate_checked(&worker, job, &mut || {
            child.kill().unwrap();
            child.wait().unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("exited during cancellation"), "{err}");
        for m in &members {
            assert!(stat_of(*m).is_some(), "member {m} was signalled");
        }
        // Clean up: the members still hold the group id, so it is theirs.
        let pid = rustix::process::Pid::from_raw(worker.pid as i32).unwrap();
        rustix::process::kill_process_group(pid, rustix::process::Signal::KILL).unwrap();
        wait_gone(worker.pid);
    }

    /// A process that is not the worker (wrong start time) or does not carry
    /// the job id is never signalled.
    #[test]
    fn rc2r4_a_process_that_is_not_the_worker_is_not_signalled() {
        let job = "00000000-0000-4000-8000-0000000000a3";
        let (mut child, _) = group(job);
        let mut stranger = identity(child.id()).unwrap();
        stranger.stamp.push_str("-other");
        assert!(terminate(&stranger, job).is_err());
        let worker = identity(child.id()).unwrap();
        assert!(terminate(&worker, "00000000-0000-4000-8000-0000000000ff").is_err());
        assert!(alive(&worker), "the worker was signalled");
        terminate(&worker, job).unwrap();
        child.wait().unwrap();
        wait_gone(worker.pid);
    }
}
