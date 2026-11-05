//! Unix process-group ownership and deadline-bounded pipe collection.
use super::{finish, Answer};
use nix::errno::Errno;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::{read, Pid};
use std::os::fd::{AsFd, OwnedFd};
use std::process::Child;
use std::time::{Duration, Instant};

pub(super) fn run_unix(command: &str, limit: Duration) -> anyhow::Result<Answer> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let deadline = Instant::now()
        .checked_add(limit)
        .ok_or_else(|| anyhow::anyhow!("timeout duration out of range"))?;
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c");
    cmd.arg(command);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // P0-weekend-repair.md item 2: own process group; the deadline kill
    // reaches every descendant that may hold the pipes.
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn failed: {e}"))?;
    // Standard-library pipes use close-on-exec, preventing unrelated concurrent
    // subprocesses from inheriting this probe's handles.
    drop(cmd);
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        kill_group(&mut child);
        return Err(anyhow::anyhow!("capture pipes unavailable"));
    };
    let out_read: OwnedFd = stdout.into();
    let err_read: OwnedFd = stderr.into();
    for fd in [&out_read, &err_read] {
        if let Err(error) = fcntl(fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)) {
            kill_group(&mut child);
            return Err(anyhow::anyhow!("configure pipe failed: {error}"));
        }
    }

    let mut out_bytes: Vec<u8> = Vec::new();
    let mut err_bytes: Vec<u8> = Vec::new();
    let mut out_open = true;
    let mut err_open = true;
    let mut buf = [0u8; 16 * 1024];

    let status = loop {
        // Keep the leader unreaped until every captured pipe closes. Its PID
        // then cannot be reused while a timeout may still signal the group.
        if !out_open && !err_open {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => return Err(anyhow::anyhow!("wait failed: {error}")),
            }
        }
        let now = Instant::now();
        if now >= deadline {
            kill_group(&mut child);
            return Err(anyhow::anyhow!("timeout"));
        }
        if out_open || err_open {
            // The deadline includes pipe completion: poll, bounded by the
            // remaining time, instead of joining readers.
            let remaining = deadline - now;
            let cap = Duration::from_millis(i32::MAX as u64);
            let timeout = PollTimeout::try_from(remaining.min(cap)).unwrap_or(PollTimeout::MAX);
            let mut fds = Vec::with_capacity(2);
            if out_open {
                fds.push(PollFd::new(out_read.as_fd(), PollFlags::POLLIN));
            }
            if err_open {
                fds.push(PollFd::new(err_read.as_fd(), PollFlags::POLLIN));
            }
            let stderr_index = usize::from(out_open);
            match poll(&mut fds, timeout) {
                Ok(_) => {
                    if out_open && ready(&fds[0]) {
                        match drain_once(&out_read, &mut buf, &mut out_bytes) {
                            Drain::Eof => out_open = false,
                            Drain::More => {}
                            Drain::Fail(e) => {
                                kill_group(&mut child);
                                return Err(anyhow::anyhow!("read stdout failed: {e}"));
                            }
                        }
                    }
                    if out_bytes.len() + err_bytes.len() > 1024 * 1024 {
                        kill_group(&mut child);
                        return Err(anyhow::anyhow!("command output exceeds size limit"));
                    }
                    if err_open && ready(&fds[stderr_index]) {
                        match drain_once(&err_read, &mut buf, &mut err_bytes) {
                            Drain::Eof => err_open = false,
                            Drain::More => {}
                            Drain::Fail(e) => {
                                kill_group(&mut child);
                                return Err(anyhow::anyhow!("read stderr failed: {e}"));
                            }
                        }
                    }
                    if out_bytes.len() + err_bytes.len() > 1024 * 1024 {
                        kill_group(&mut child);
                        return Err(anyhow::anyhow!("command output exceeds size limit"));
                    }
                }
                Err(Errno::EINTR) => continue,
                Err(e) => {
                    kill_group(&mut child);
                    return Err(anyhow::anyhow!("poll failed: {e}"));
                }
            }
        } else {
            // Both pipes at EOF while the child still lives (it may close
            // its own stdout while running): bounded tick, deadline rechecks.
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    finish(status, &out_bytes, &err_bytes)
}

/// Whether `poll` reported the fd readable (or at EOF/hangup).
fn ready(fd: &PollFd<'_>) -> bool {
    fd.revents()
        .is_some_and(|r| r.intersects(PollFlags::POLLIN | PollFlags::POLLHUP))
}

/// One bounded read from a poll-ready pipe; `nix::unistd::read` returns 0
/// at EOF and `EINTR` if interrupted (cannot hang once `poll` says ready).
enum Drain {
    Eof,
    More,
    Fail(std::io::Error),
}

fn drain_once(reader: &OwnedFd, buf: &mut [u8], sink: &mut Vec<u8>) -> Drain {
    match read(reader, buf) {
        Ok(0) => Drain::Eof,
        Ok(n) => {
            sink.extend_from_slice(&buf[..n]);
            Drain::More
        }
        Err(Errno::EINTR | Errno::EAGAIN) => Drain::More,
        Err(e) => Drain::Fail(std::io::Error::other(e.to_string())),
    }
}

/// Kill the child's whole process group (deadline/error path) and reap the
/// immediate child when it has not been reaped already.
fn kill_group(child: &mut Child) {
    let pgid = Pid::from_raw(child.id() as i32);
    // ESRCH is expected when the group is already gone (best-effort).
    let _ = killpg(pgid, Signal::SIGKILL);
    // The unreaped child still owns its PID, but may have moved to another
    // group. Kill that child directly too; never signal its new group.
    let _ = child.kill();
    let _ = child.wait();
}
