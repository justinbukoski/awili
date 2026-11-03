//! Nonblocking pipe transport: one deadline covers process, writes, reads and EOF.
#[cfg(unix)]
mod imp {
    use anyhow::{bail, ensure, Context, Result};
    use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
    use std::{
        io::{ErrorKind, Read, Write},
        os::unix::process::CommandExt,
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            // A reaped leader's numeric PID may have been reused: never signal its group.
            // Closing our nonblocking pipes releases all local I/O without joining readers.
            if self.0.try_wait().ok().flatten().is_none() {
                if let Some(pid) = rustix::process::Pid::from_raw(self.0.id() as i32) {
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                }
                let _ = self.0.kill();
                let reap_deadline = Instant::now() + Duration::from_millis(100);
                while Instant::now() < reap_deadline {
                    if self.0.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
    /// `cap` bounds the response; a child that closes its input early is named as such.
    pub fn exchange(
        mut command: Command,
        payload: &[u8],
        timeout: Duration,
        cap: usize,
    ) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut child = OwnedChild(
            command
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?,
        );
        let input = child.0.stdin.take().context("command stdin unavailable")?;
        let mut output = child
            .0
            .stdout
            .take()
            .context("command stdout unavailable")?;
        fcntl_setfl(&input, fcntl_getfl(&input)? | OFlags::NONBLOCK)?;
        fcntl_setfl(&output, fcntl_getfl(&output)? | OFlags::NONBLOCK)?;
        let mut input = Some(input);
        let mut written = 0;
        let mut bytes = Vec::new();
        let mut eof = false;
        let mut status = None;
        loop {
            ensure!(
                Instant::now() < deadline,
                "command deadline exceeded; outcome may be uncertain"
            );
            if let Some(pipe) = &mut input {
                match pipe.write(&payload[written..]) {
                    Ok(n) => {
                        written += n;
                        if written == payload.len() {
                            input.take();
                        }
                    }
                    Err(e)
                        if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                    Err(e) if e.kind() == ErrorKind::BrokenPipe => {
                        bail!("command closed its input before reading the whole request; outcome may be uncertain")
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            if !eof {
                let mut buffer = [0; 8192];
                match output.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        bytes.extend_from_slice(&buffer[..n]);
                        ensure!(bytes.len() <= cap, "command response exceeded limit");
                    }
                    Err(e)
                        if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            if status.is_none() {
                status = child.0.try_wait()?;
            }
            if let Some(status) = status {
                if !status.success() {
                    bail!("command failed; outcome may be uncertain");
                }
                if eof && input.is_none() {
                    return Ok(bytes);
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
#[cfg(unix)]
pub use imp::exchange;

/// Windows: the same contract with threads instead of nonblocking descriptors.
/// One deadline covers the whole exchange; on timeout the child is killed.
/// TODO(windows-phase1): kill the whole process tree with a Job Object (the
/// Unix build kills the process group); today only the direct child is killed.
#[cfg(windows)]
pub fn exchange(
    mut command: std::process::Command,
    payload: &[u8],
    timeout: std::time::Duration,
    cap: usize,
) -> anyhow::Result<Vec<u8>> {
    use anyhow::{bail, ensure, Context};
    use std::io::{Read, Write};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Instant;

    let deadline = Instant::now() + timeout;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = child.stdin.take().context("command stdin unavailable")?;
    let output = child.stdout.take().context("command stdout unavailable")?;
    let payload = payload.to_vec();
    let (wtx, wrx) = mpsc::channel::<std::io::Result<()>>();
    std::thread::spawn(move || {
        let r = input.write_all(&payload).and_then(|_| input.flush());
        drop(input);
        let _ = wtx.send(r);
    });
    let (rtx, rrx) = mpsc::channel::<std::io::Result<Vec<u8>>>();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let r = output
            .take(cap as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = rtx.send(r);
    });
    let fail = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
    };
    let wait_left = || deadline.saturating_duration_since(Instant::now());
    match wrx.recv_timeout(wait_left()) {
        Ok(Ok(())) => {}
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => {
            fail(&mut child);
            bail!("command closed its input before reading the whole request; outcome may be uncertain")
        }
        Ok(Err(e)) => {
            fail(&mut child);
            return Err(e.into());
        }
        Err(_) => {
            fail(&mut child);
            bail!("command deadline exceeded; outcome may be uncertain")
        }
    }
    let bytes = match rrx.recv_timeout(wait_left()) {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => {
            fail(&mut child);
            return Err(e.into());
        }
        Err(_) => {
            fail(&mut child);
            bail!("command deadline exceeded; outcome may be uncertain")
        }
    };
    if bytes.len() > cap {
        fail(&mut child);
        bail!("command response exceeded limit");
    }
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "command failed; outcome may be uncertain");
            return Ok(bytes);
        }
        if Instant::now() >= deadline {
            fail(&mut child);
            bail!("command deadline exceeded; outcome may be uncertain");
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
