//! Fixed supervised service endpoint (Rust port of the reviewed Python
//! `build-endpoint.py`). It never asks the runner to submit or to run a
//! worker: it reserves the request, starts only `awili-build@<UUID>.service`
//! through `sudo -n systemctl`, and answers from the runner's read-only
//! `validate`/`status`/`logs` and its `cancel`.
use crate::{
    protocol::*,
    supervised::{SupervisedLogs, SupervisedSnapshot, SupervisorState, ValidatedRequest},
};
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub const ROOT: &str = "/srv/awili-build";
pub const RUNNER: &str = "/opt/awili-build/awili-build-runner";
pub const CONFIG: &str = "/opt/awili-build/runner.json";
pub const CGROUPS: &str = "/sys/fs/cgroup";
/// Request and response transport cap (the runner's request cap).
pub const CAP: usize = MAX_REQUEST_JSON_BYTES;
/// One subprocess exchange (runner or systemctl) may take this long.
pub const CALL_DEADLINE: Duration = Duration::from_secs(35);
/// ONLY the bounded operator acceptance fixture, not generic Build.
pub const ELIGIBLE: &[(&str, &str)] =
    &[("qwen-hosta-runtime-hostb-smoke", "normalize-label-smoke-v1")];
const SYSTEMCTL: &str = "/usr/bin/systemctl";
const SUDO: &str = "/usr/bin/sudo";
const UNAVAILABLE: &str = "service or cgroup observation unavailable";
const UNCONFIRMED: &str = "cancellation termination unconfirmed";

/// A request or response that does not belong to this reservation. It is
/// never folded into a `needs_attention` answer: the caller gets an error.
#[derive(Debug)]
pub struct Conflict(pub &'static str);
impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Conflict {}
fn conflict<T>(what: &'static str) -> Result<T> {
    Err(Conflict(what).into())
}

/// Runs one fixed command: argv plus an optional JSON stdin; returns stdout.
pub type Call<'a> = Box<dyn FnMut(&[String], Option<&Value>) -> Result<Vec<u8>> + 'a>;

/// The real call: bounded nonblocking exchange, own process group, stderr
/// discarded, nonzero exit reported without echoing output.
pub fn exchange(argv: &[String], payload: Option<&Value>) -> Result<Vec<u8>> {
    exchange_within(argv, payload, CALL_DEADLINE)
}
pub fn exchange_within(
    argv: &[String],
    payload: Option<&Value>,
    deadline: Duration,
) -> Result<Vec<u8>> {
    let data = match payload {
        Some(value) => serde_json::to_vec(value)?,
        None => Vec::new(),
    };
    ensure!(data.len() <= CAP, "request too large");
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("empty command"))?;
    let mut command = Command::new(program);
    command.args(args);
    crate::transport::exchange(command, &data, deadline, CAP)
}

/// Reservation metadata, private to the endpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub request_sha256: String,
    pub launch_attempted: bool,
    pub cancel_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_group: Option<String>,
}

pub struct Endpoint<'a> {
    pub root: PathBuf,
    pub runner: String,
    pub config: String,
    pub cgroups: PathBuf,
    pub call: Call<'a>,
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

impl<'a> Endpoint<'a> {
    pub fn installed() -> Endpoint<'static> {
        Endpoint {
            root: ROOT.into(),
            runner: RUNNER.into(),
            config: CONFIG.into(),
            cgroups: CGROUPS.into(),
            call: Box::new(exchange),
        }
    }

    fn runner_call<T: serde::de::DeserializeOwned>(
        &mut self,
        operation: &str,
        value: &Value,
    ) -> Result<T> {
        ensure!(
            matches!(operation, "validate" | "status" | "logs" | "cancel"),
            "unsupported runner operation"
        );
        let argv = [
            self.runner.clone(),
            "--config".into(),
            self.config.clone(),
            operation.into(),
        ];
        Ok(serde_json::from_slice(&(self.call)(&argv, Some(value))?)?)
    }

    fn validated(&mut self, request: &BuildRequest) -> Result<ValidatedRequest> {
        if !ELIGIBLE.iter().any(|(target, profile)| {
            request.target_id == *target && request.verify_profile == *profile
        }) {
            return conflict("target/profile not eligible for this endpoint");
        }
        let receipt: ValidatedRequest =
            self.runner_call("validate", &serde_json::to_value(request)?)?;
        if receipt.request != *request {
            return conflict("validation receipt mismatch");
        }
        if !is_hex(&receipt.request_sha256, 64) {
            return conflict("invalid validation fingerprint");
        }
        Ok(receipt)
    }

    pub fn unit(job_id: &str) -> Result<String> {
        valid_id(job_id)?;
        Ok(format!("awili-build@{job_id}.service"))
    }

    fn unit_status(&mut self, job_id: &str) -> Result<std::collections::BTreeMap<String, String>> {
        let argv = [
            SYSTEMCTL.to_string(),
            "show".into(),
            Self::unit(job_id)?,
            "--property=LoadState,ActiveState,SubState,ControlGroup,InvocationID".into(),
        ];
        let raw = String::from_utf8((self.call)(&argv, None)?)?;
        let props: std::collections::BTreeMap<String, String> = raw
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ensure!(
            props.get("LoadState").map(String::as_str) == Some("loaded"),
            "service unavailable"
        );
        Ok(props)
    }

    /// Only the exact template instance in the reviewed system slice.
    pub fn group(&self, job_id: &str, path: &str) -> Result<PathBuf> {
        let expected = format!(
            r"/system.slice/system-awili\x2dbuild.slice/{}",
            Self::unit(job_id)?
        );
        ensure!(path == expected, "cgroup ownership unconfirmed");
        Ok(self.cgroups.join(path.trim_start_matches('/')))
    }

    fn empty_group(&self, job_id: &str, path: Option<&str>) -> Result<bool> {
        let Some(path) = path.filter(|p| !p.is_empty()) else {
            return Ok(false);
        };
        let group = self.group(job_id, path)?;
        if !group.exists() {
            return Ok(true);
        }
        let events = fs::read_to_string(group.join("cgroup.events"))?;
        Ok(events
            .lines()
            .filter_map(|line| line.split_once(' '))
            .any(|(k, v)| k == "populated" && v.trim() == "0"))
    }

    /// The runner snapshot, or `None` before the service has created a record.
    fn snapshot(
        &mut self,
        request: &BuildRequest,
        fingerprint: &str,
    ) -> Result<Option<BuildSnapshot>> {
        let path = self
            .root
            .join("jobs")
            .join(&request.request_id)
            .join("control")
            .join("record.json");
        if !path.exists() {
            return Ok(None);
        }
        let value: BuildSnapshot = self.runner_call(
            "status",
            &serde_json::json!({ "request_id": request.request_id }),
        )?;
        Self::correlate(&value, request, fingerprint)?;
        Ok(Some(value))
    }

    fn correlate(value: &BuildSnapshot, request: &BuildRequest, fingerprint: &str) -> Result<()> {
        if value.request_id != request.request_id
            || value.request_sha256 != fingerprint
            || value.baseline_sha != request.baseline_sha
        {
            return conflict("runner response correlation mismatch");
        }
        Ok(())
    }

    fn observe(
        &mut self,
        request: &BuildRequest,
        meta: &mut Meta,
        meta_path: &Path,
    ) -> Result<SupervisedSnapshot> {
        let job_id = request.request_id.clone();
        let mut value = None;
        let mut confirmed = false;
        // F0016: the runner status call sits inside the observation guard. A
        // runner that cannot answer is "needs attention"; a runner answer
        // for another job is still a conflict and is returned as an error.
        let observed: Result<(SupervisorState, Option<&'static str>)> = (|| {
            value = self.snapshot(request, &meta.request_sha256)?;
            let unit = self.unit_status(&job_id)?;
            let invocation = unit.get("InvocationID").cloned().unwrap_or_default();
            ensure!(
                is_hex(&invocation, 32) && invocation != "0".repeat(32),
                "service invocation unavailable"
            );
            ensure!(
                meta.invocation_id
                    .as_ref()
                    .is_none_or(|known| *known == invocation),
                "service invocation changed; no ownership claim"
            );
            let group = unit.get("ControlGroup").filter(|g| !g.is_empty()).cloned();
            if let Some(group) = &group {
                self.group(&job_id, group)?;
            }
            if meta.invocation_id.is_none()
                || group
                    .as_ref()
                    .is_some_and(|g| meta.control_group.as_ref() != Some(g))
            {
                meta.invocation_id = Some(invocation);
                if let Some(group) = group {
                    meta.control_group = Some(group);
                }
                replace_json(meta_path, meta)?;
            }
            let active_state = unit.get("ActiveState").map(String::as_str).unwrap_or("");
            let active = matches!(
                active_state,
                "active" | "activating" | "deactivating" | "reloading"
            );
            confirmed = !active
                && matches!(active_state, "inactive" | "failed")
                && self.empty_group(&job_id, meta.control_group.as_deref())?;
            let terminal = value.as_ref().is_some_and(|v| !v.state.active());
            let cancelling = meta.cancel_requested
                || value
                    .as_ref()
                    .is_some_and(|v| v.state == BuildState::Stopped);
            Ok(if cancelling && !confirmed {
                (SupervisorState::NeedsAttention, Some(UNCONFIRMED))
            } else if terminal {
                // Preserve the snapshot; termination proof is separate.
                (SupervisorState::Terminal, None)
            } else if meta.cancel_requested {
                (SupervisorState::Stopped, None)
            } else if active {
                if value.is_some() {
                    (SupervisorState::Active, None)
                } else {
                    (SupervisorState::Starting, None)
                }
            } else {
                (
                    SupervisorState::NeedsAttention,
                    Some("service inactive or launch uncertain; no automatic restart"),
                )
            })
        })();
        let (state, diagnostic) = match observed {
            Ok(result) => result,
            Err(err) if err.is::<Conflict>() => return Err(err),
            Err(_) => {
                confirmed = false;
                (SupervisorState::NeedsAttention, Some(UNAVAILABLE))
            }
        };
        Ok(SupervisedSnapshot {
            request_id: job_id,
            request_sha256: meta.request_sha256.clone(),
            supervisor_state: state,
            snapshot: value,
            termination_confirmed: confirmed,
            diagnostic: diagnostic.map(str::to_string),
        })
    }

    /// One endpoint operation on a parsed JSON payload; returns the response.
    pub fn handle(&mut self, operation: &str, payload: &Value) -> Result<Value> {
        ensure!(
            matches!(operation, "submit" | "status" | "cancel" | "logs"),
            "unsupported operation"
        );
        ensure!(payload.is_object(), "JSON object required");
        let mut submitted = None;
        let mut logs = None;
        let job_id = match operation {
            "submit" => {
                let request: BuildRequest = serde_json::from_value(payload.clone())?;
                valid_id(&request.request_id)?;
                let id = request.request_id.clone();
                submitted = Some(request);
                id
            }
            "logs" => {
                let query: LogsQuery = serde_json::from_value(payload.clone()).map_err(|_| {
                    anyhow::anyhow!("only request_id, cursor and max_bytes accepted")
                })?;
                valid_id(&query.request_id)?;
                let id = query.request_id.clone();
                logs = Some(query);
                id
            }
            _ => {
                let query: JobQuery = serde_json::from_value(payload.clone())
                    .map_err(|_| anyhow::anyhow!("only request_id accepted for this operation"))?;
                valid_id(&query.request_id)?;
                query.request_id
            }
        };
        let requests = self.root.join("requests");
        let state = self.root.join("endpoint");
        // ROOT must already be the operator-provisioned private dataset.
        ensure!(self.root.is_dir(), "endpoint storage unavailable");
        for dir in [&requests, &state] {
            match fs::DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        sync_dir(&self.root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(state.join(format!("{job_id}.lock")))?;
        lock.lock()?;
        let request_path = requests.join(format!("{job_id}.json"));
        let meta_path = state.join(format!("{job_id}.json"));
        if let Some(payload) = &submitted {
            let validated = self.validated(payload)?;
            if request_path.exists() {
                let stored: BuildRequest = serde_json::from_slice(&fs::read(&request_path)?)?;
                if stored != *payload {
                    return conflict("immutable request conflict");
                }
            } else {
                ensure!(
                    !self.root.join("jobs").join(&job_id).exists() && !meta_path.exists(),
                    "existing ownership without request; inspect before acting"
                );
                create_json(&request_path, payload)?;
                let meta = Meta {
                    request_sha256: validated.request_sha256.clone(),
                    launch_attempted: true,
                    cancel_requested: false,
                    ..Meta::default()
                };
                // Persist ambiguity BEFORE privileged start, never retry after a crash.
                create_json(&meta_path, &meta)?;
                let start = [
                    SUDO.to_string(),
                    "-n".into(),
                    SYSTEMCTL.into(),
                    "start".into(),
                    Self::unit(&job_id)?,
                ];
                // Observe the same job; a failed ACK is not a fresh submission.
                let _ = (self.call)(&start, None);
            }
        }
        ensure!(
            request_path.exists(),
            "request not reserved; do not automatically resubmit"
        );
        let request: BuildRequest = serde_json::from_slice(&fs::read(&request_path)?)?;
        let validated = self.validated(&request)?;
        let mut meta = if meta_path.exists() {
            serde_json::from_slice::<Meta>(&fs::read(&meta_path)?)?
        } else {
            // Crash between immutable request and metadata cannot launch on replay.
            let meta = Meta {
                request_sha256: validated.request_sha256.clone(),
                launch_attempted: false,
                cancel_requested: false,
                ..Meta::default()
            };
            create_json(&meta_path, &meta)?;
            meta
        };
        if meta.request_sha256 != validated.request_sha256 {
            return conflict("reservation fingerprint mismatch");
        }
        if operation == "cancel" {
            if let Some(early) = self.cancel(&request, &mut meta, &meta_path)? {
                return Ok(serde_json::to_value(early)?);
            }
        }
        if let Some(query) = logs {
            ensure!(
                (1..=65536).contains(&query.max_bytes),
                "invalid log cursor or bound"
            );
            let status = self.observe(&request, &mut meta, &meta_path)?;
            let page = if status.snapshot.is_none() {
                LogsPage {
                    cursor: query.cursor,
                    next_cursor: query.cursor,
                    bytes: vec![],
                }
            } else {
                self.runner_call("logs", &serde_json::to_value(&query)?)?
            };
            ensure!(
                page.cursor == query.cursor
                    && page.bytes.len() <= query.max_bytes
                    && page.cursor.checked_add(page.bytes.len() as u64) == Some(page.next_cursor),
                "invalid log response"
            );
            return Ok(serde_json::to_value(SupervisedLogs { job: status, page })?);
        }
        Ok(serde_json::to_value(
            self.observe(&request, &mut meta, &meta_path)?,
        )?)
    }

    /// Record the cancel intent, ask the runner, then stop only the owned
    /// service instance. Returns an early answer when nothing may be stopped.
    fn cancel(
        &mut self,
        request: &BuildRequest,
        meta: &mut Meta,
        meta_path: &Path,
    ) -> Result<Option<SupervisedSnapshot>> {
        let job_id = request.request_id.clone();
        let before = self.observe(request, meta, meta_path)?;
        if before
            .snapshot
            .as_ref()
            .is_some_and(|v| matches!(v.state, BuildState::AwaitingReview | BuildState::Failed))
        {
            // The job's result is final and is kept as it is: no cancel intent
            // is recorded and the runner is not asked. Its service may still
            // be running or its cgroup may still hold processes; unless that
            // termination is confirmed, the owned invocation is stopped (never
            // a replaced or unobserved one) and the answer is observed after.
            if before.termination_confirmed || before.diagnostic.as_deref() == Some(UNAVAILABLE) {
                return Ok(Some(before));
            }
            let _ = self.stop_owned(&job_id, meta);
            return Ok(None);
        }
        meta.cancel_requested = true;
        replace_json(meta_path, meta)?;
        if before.diagnostic.as_deref() == Some(UNAVAILABLE) {
            // Never stop a replaced or unowned invocation.
            return Ok(Some(before));
        }
        // F0017: neither a runner that cannot be reached nor a mismatched
        // runner answer skips the owned service stop below.
        let mut mismatch = None;
        if before.snapshot.is_some() {
            let delivered: Result<BuildSnapshot> =
                self.runner_call("cancel", &serde_json::json!({ "request_id": job_id }));
            if let Ok(result) = delivered {
                if let Err(err) = Self::correlate(&result, request, &meta.request_sha256) {
                    mismatch = Some(err);
                }
            }
        }
        let _ = self.stop_owned(&job_id, meta);
        if let Some(err) = mismatch {
            return Err(err);
        }
        Ok(None)
    }

    /// Stop the service instance only while it is still the invocation this
    /// reservation owns; a replaced invocation is never stopped.
    fn stop_owned(&mut self, job_id: &str, meta: &Meta) -> Result<()> {
        let unit = self.unit_status(job_id)?;
        if unit.get("InvocationID") != meta.invocation_id.as_ref() {
            bail!("service invocation changed before stop");
        }
        let argv = [
            SUDO.to_string(),
            "-n".into(),
            SYSTEMCTL.into(),
            "stop".into(),
            Self::unit(job_id)?,
        ];
        (self.call)(&argv, None)?;
        Ok(())
    }
}

pub fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

/// Exclusive create, mode 0600, synced with its directory.
pub fn create_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let data = serde_json::to_vec(value)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&data)?;
    file.sync_all()?;
    sync_dir(path.parent().unwrap_or(Path::new(".")))
}

/// Replace through a fresh private temporary file in the same directory.
pub fn replace_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let temp = dir.join(format!(".endpoint-{}", uuid::Uuid::new_v4()));
    let written: Result<()> = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        sync_dir(dir)
    })();
    let _ = fs::remove_file(&temp);
    written
}
