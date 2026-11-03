//! Durable submission, observation and cautious cancellation.
use crate::{process, protocol::*, store::Store};
use anyhow::{ensure, Result};
use std::{
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};

pub fn submit(
    config: &RunnerConfig,
    request: BuildRequest,
    executable: &Path,
) -> Result<BuildSnapshot> {
    request.validate()?;
    let store = Store::open(&config.job_root)?;
    let _lock = store.lock()?;
    if store.job_exists(&request.request_id)? {
        if store.record_exists(&request.request_id)? {
            let record = store.read(&request.request_id)?;
            ensure!(
                record.request == request,
                "request_id already has different payload"
            );
            return Ok(record.snapshot);
        }
        // A submission that stopped before its record was written never
        // spawned a worker (the worker starts only after the record): clear
        // the empty job directory so this retry can submit for real.
        store.remove_unstarted_job(&request.request_id)?;
    }
    let target = config.target(&request)?;
    // Reserve branch ownership across requests before any model can run.
    let key = digest(format!("{}\0{}", target.repo_url, request.branch).as_bytes());
    if let Some(owner) = store.branch_owner(&key)? {
        ensure!(
            owner == request.request_id || reservation_is_spent(&store, &owner)?,
            "branch already reserved by another request"
        );
    }
    store.create_job(&request.request_id)?;
    let mut record = JobRecord {
        snapshot: BuildSnapshot {
            request_id: request.request_id.clone(),
            request_sha256: request.fingerprint(),
            revision: 0,
            state: BuildState::Queued,
            executor: target.command.program.to_string_lossy().into_owned(),
            requested_model: target.model.clone(),
            baseline_sha: request.baseline_sha.clone(),
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
        target,
    };
    // Reservation and record land together, or the job directory is removed
    // again: a failure here never leaves a job that a retry cannot read.
    let prepared = store
        .reserve_branch(&key, &record.request.request_id)
        .and_then(|()| store.write(&mut record));
    if let Err(err) = prepared {
        let _ = store.release_branch(&key, &record.request.request_id);
        let _ = store.remove_unstarted_job(&record.request.request_id);
        return Err(err);
    }
    std::fs::File::open(&store.root)?.sync_all()?;
    // The worker log is a control leaf, never part of the model's view.
    let log = store.create_log(&record.request.request_id)?;
    let spawned = Command::new(executable)
        .arg("--root")
        .arg(&store.root)
        .arg("worker")
        .arg(&record.request.request_id)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn();
    match spawned {
        Ok(mut child) => {
            record.snapshot.worker = process::identity(child.id());
            if record.snapshot.worker.is_none() {
                // Without an identity no later cancel can reach this worker,
                // so stop it now, while it is still our unreaped child (its
                // PID and group cannot be reused) and still waiting for the
                // store lock this call holds, so it has run nothing.
                stop_unidentified_worker(&mut child);
                record.snapshot.state = BuildState::Failed;
                record.snapshot.failure = Some(
                    "worker identity unavailable; the worker was stopped before it ran".into(),
                );
            } else {
                // Reap when this caller is long-lived; detached worker survives caller exit.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
        }
        Err(_) => {
            record.snapshot.state = BuildState::Failed;
            record.snapshot.failure = Some("worker spawn failed".into());
        }
    }
    store.write(&mut record)?;
    Ok(record.snapshot)
}

/// A branch reservation is spent when its owner can no longer publish: the
/// owner never wrote a record (a submission that stopped early), or it ended
/// Failed or Stopped without ever attempting a push. A job that pushed,
/// awaits review, needs attention or is still active keeps its branch.
fn reservation_is_spent(store: &Store, owner: &str) -> Result<bool> {
    if !store.job_exists(owner)? || !store.record_exists(owner)? {
        return Ok(true);
    }
    let snapshot = store.read(owner)?.snapshot;
    let alive = snapshot.worker.as_ref().is_some_and(process::alive);
    Ok(
        matches!(snapshot.state, BuildState::Failed | BuildState::Stopped)
            && snapshot.push_exit.is_none()
            && snapshot.remote_sha.is_none()
            && !alive,
    )
}

/// Kill and reap a spawned worker whose identity could not be recorded.
fn stop_unidentified_worker(child: &mut std::process::Child) {
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Whether `worker` is still the running process it names: false once it has
/// exited (a zombie included) and for a reused PID (the identity carries the
/// process start time). The same check the runner applies before trusting or
/// signalling a worker.
pub fn worker_running(worker: &WorkerIdentity) -> bool {
    process::alive(worker)
}

pub fn status(root: &Path, id: &str) -> Result<BuildSnapshot> {
    let store = Store::open(root)?;
    let _lock = store.lock()?;
    let mut record = store.read(id)?;
    if record.snapshot.state.active() {
        let live = record.snapshot.worker.as_ref().is_some_and(process::alive);
        if !live {
            let confirmed_stop = record.snapshot.state == BuildState::CancelRequested
                && record
                    .snapshot
                    .worker
                    .as_ref()
                    .is_some_and(|w| !process::group_exists(w.pid));
            record.snapshot.state = if confirmed_stop {
                BuildState::Stopped
            } else {
                BuildState::NeedsAttention
            };
            record.snapshot.failure = Some(
                if confirmed_stop {
                    "owned process group exited after cancellation"
                } else {
                    "worker lost; no automatic redispatch"
                }
                .into(),
            );
            store.write(&mut record)?;
        }
    }
    Ok(record.snapshot)
}
pub fn cancel(root: &Path, id: &str) -> Result<BuildSnapshot> {
    let store = Store::open(root)?;
    let _lock = store.lock()?;
    let mut record = store.read(id)?;
    if !record.snapshot.state.active() {
        // A terminal record is not trusted while its worker group is alive:
        // it may have been forged out-of-band. Resolve the race to
        // NeedsAttention, never to success.
        if let Some(w) = record.snapshot.worker.as_ref() {
            if process::alive(w) {
                let terminated = process::terminate(w, id).is_ok();
                record.snapshot.state = BuildState::NeedsAttention;
                record.snapshot.failure = Some(format!(
                    "terminal record while its worker was alive; worker group {}",
                    if terminated {
                        "terminated"
                    } else {
                        "termination unconfirmed"
                    }
                ));
                store.write(&mut record)?;
                return Ok(record.snapshot);
            }
        }
        return Ok(record.snapshot);
    }
    record.snapshot.state = BuildState::CancelRequested;
    store.write(&mut record)?;
    let result = record
        .snapshot
        .worker
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no worker identity"))
        .and_then(|w| process::terminate(w, id));
    if result.is_err() {
        record.snapshot.state = BuildState::NeedsAttention;
        record.snapshot.failure = Some("cancellation ownership or delivery unconfirmed".into());
        store.write(&mut record)?;
    }
    Ok(record.snapshot)
}
pub fn run_worker(root: &Path, id: &str) -> Result<()> {
    crate::worker::run(root, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F0480: a worker whose identity could not be recorded is stopped with
    /// its whole process group and reaped before `submit` returns, so no
    /// unreachable worker or descendant is left running.
    #[test]
    fn rc2ops1_an_unidentified_worker_and_its_group_are_stopped() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 30 & wait"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(process::group_exists(pid), "fixture group did not start");
        stop_unidentified_worker(&mut child);
        assert!(child.try_wait().unwrap().is_some(), "worker was not reaped");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while process::group_exists(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "a descendant of the unidentified worker kept running"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
