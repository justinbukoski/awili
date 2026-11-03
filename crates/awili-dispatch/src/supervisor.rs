//! One supervised request (Rust port of the reviewed Python
//! `supervised-run.py`). Runs inside `awili-build@<UUID>.service`, whose
//! cgroup outlives the submit and owns every descendant: it submits the
//! reserved request once, reconciles an uncertain submit with a status
//! check, and follows the job until it ends or the supervision window closes.
use crate::{protocol::*, runner, store::Store};
use anyhow::{bail, ensure, Context, Result};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const ROOT: &str = "/srv/awili-build";
/// How often the job is checked while it runs.
pub const POLL: Duration = Duration::from_secs(5);
/// The longest one supervised job may stay active. When it closes, the
/// supervisor asks the runner to cancel and exits; the service stop that
/// follows (KillMode=control-group) ends whatever is left.
pub const SUPERVISION_WINDOW: Duration = Duration::from_secs(6 * 60 * 60);

/// The stored job belongs to a different request than the one reserved for
/// this service instance. Never reconciled away.
#[derive(Debug)]
pub struct RequestConflict;
impl std::fmt::Display for RequestConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stored request does not match this supervised submission")
    }
}
impl std::error::Error for RequestConflict {}

/// The snapshot must answer for this request: same ID, and the stored job's
/// request and fingerprint are the reserved request's.
pub fn correlate(snapshot: &BuildSnapshot, request: &BuildRequest, job_root: &Path) -> Result<()> {
    let record = Store::open(job_root)?.read(&request.request_id)?;
    if snapshot.request_id != request.request_id
        || record.request != *request
        || snapshot.request_sha256 != record.snapshot.request_sha256
    {
        return Err(RequestConflict.into());
    }
    Ok(())
}

pub struct Supervisor {
    pub root: PathBuf,
    pub config: RunnerConfig,
    pub executable: PathBuf,
    pub poll: Duration,
    pub window: Duration,
}

impl Supervisor {
    fn checked(&self, snapshot: BuildSnapshot, request: &BuildRequest) -> Result<BuildSnapshot> {
        correlate(&snapshot, request, &self.config.job_root)?;
        Ok(snapshot)
    }

    fn status(&self, request: &BuildRequest) -> Result<BuildSnapshot> {
        let snapshot = runner::status(&self.config.job_root, &request.request_id)?;
        self.checked(snapshot, request)
    }

    /// Supervise `id` to its end. Prints each new snapshot revision as one
    /// JSON line; returns the exit code (0 only for a result awaiting review).
    pub fn run(&self, id: &str, out: &mut impl Write) -> Result<i32> {
        valid_id(id)?;
        let path = self.root.join("requests").join(format!("{id}.json"));
        let bytes = std::fs::read(&path).context("the reserved request file could not be read")?;
        let request: BuildRequest =
            serde_json::from_slice(&bytes).context("the reserved request file is malformed")?;
        ensure!(request.request_id == id, "request id mismatch");
        let submitted = runner::submit(&self.config, request.clone(), &self.executable)
            .and_then(|snapshot| self.checked(snapshot, &request));
        let mut snapshot = match submitted {
            Ok(snapshot) => snapshot,
            Err(err) if err.is::<RequestConflict>() => return Err(err),
            // Uncertain acceptance: status the same ID once, never a new
            // submission. F0003: if that check fails too, both failures are
            // reported; the status error never replaces the submit error.
            Err(submit) => self.status(&request).map_err(|status| {
                if status.is::<RequestConflict>() {
                    status
                } else {
                    anyhow::anyhow!(
                        "submission uncertain ({submit:#}); the status check that followed also failed ({status:#}); reconcile {id}"
                    )
                }
            })?,
        };
        let started = Instant::now();
        let mut last_revision = None;
        loop {
            if last_revision != Some(snapshot.revision) {
                writeln!(out, "{}", serde_json::to_string(&snapshot)?)?;
                out.flush()?;
                last_revision = Some(snapshot.revision);
            }
            if !snapshot.state.active() {
                return Ok(if snapshot.state == BuildState::AwaitingReview {
                    0
                } else {
                    1
                });
            }
            // F0004: the supervisor no longer polls without end.
            if started.elapsed() >= self.window {
                let _ = runner::cancel(&self.config.job_root, id);
                bail!(
                    "job {id} was still active when the {}-minute supervision window closed; cancellation was requested and the service stop ends what is left",
                    self.window.as_secs() / 60
                );
            }
            std::thread::sleep(self.poll.min(self.window.saturating_sub(started.elapsed())));
            snapshot = self.status(&request)?;
        }
    }
}
