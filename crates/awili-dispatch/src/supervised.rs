//! Supervised service endpoint protocol; model identity remains requested configuration.
use crate::{protocol::*, ssh::SshClient};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

/// Shared, read-only runner validation receipt. No job is created.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatedRequest {
    pub request: BuildRequest,
    pub request_sha256: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorState {
    Starting,
    Active,
    Terminal,
    Stopped,
    NeedsAttention,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisedSnapshot {
    pub request_id: String,
    pub request_sha256: String,
    pub supervisor_state: SupervisorState,
    pub snapshot: Option<BuildSnapshot>,
    pub termination_confirmed: bool,
    pub diagnostic: Option<String>,
}
impl SupervisedSnapshot {
    fn correlate(&self, request: &BuildRequest) -> Result<()> {
        ensure!(
            self.request_id == request.request_id && self.request_sha256 == request.fingerprint(),
            "supervised response correlation mismatch"
        );
        if let Some(snapshot) = &self.snapshot {
            ensure!(
                snapshot.request_id == self.request_id
                    && snapshot.request_sha256 == self.request_sha256
                    && snapshot.baseline_sha == request.baseline_sha,
                "runner snapshot correlation mismatch"
            );
            ensure!(
                snapshot.state != BuildState::Stopped
                    || self.supervisor_state != SupervisorState::Terminal
                    || self.termination_confirmed,
                "runner stop does not prove service termination"
            );
        }
        ensure!(
            self.supervisor_state != SupervisorState::Stopped || self.termination_confirmed,
            "unconfirmed stop response"
        );
        ensure!(
            self.supervisor_state != SupervisorState::Terminal
                || self.snapshot.as_ref().is_some_and(|s| !s.state.active()),
            "terminal response lacks terminal snapshot"
        );
        Ok(())
    }
}
/// Uses the same bounded SSH transport, pointed at the fixed service endpoint,
/// never at the detached runner. Caller retains the immutable request for reattach.
pub struct SupervisedSshClient(SshClient);
impl SupervisedSshClient {
    /// Fixed deployed endpoint/config: callers cannot accidentally target the
    /// unsupervised runner. SSH executable is injectable for transport fixtures.
    pub fn new(ssh: std::path::PathBuf, host: String, timeout: std::time::Duration) -> Self {
        Self(SshClient {
            ssh,
            host,
            runner: "/opt/awili-build/awili-build-endpoint".into(),
            config: "/opt/awili-build/runner.json".into(),
            timeout,
        })
    }
    fn observe(&self, operation: &str, request: &BuildRequest) -> Result<SupervisedSnapshot> {
        request.validate()?;
        let value: SupervisedSnapshot = self.0.call(
            operation,
            &JobQuery {
                request_id: request.request_id.clone(),
            },
        )?;
        value.correlate(request)?;
        Ok(value)
    }
    pub fn submit(&self, request: &BuildRequest) -> Result<SupervisedSnapshot> {
        request.validate()?;
        let value: SupervisedSnapshot =
            match self.0.call("submit", request) {
                Ok(value) => value,
                Err(_) => return self.status(request).context(
                    "submission uncertain; retain the same request, never automatically resubmit",
                ),
            };
        value.correlate(request)?;
        Ok(value)
    }
    pub fn status(&self, request: &BuildRequest) -> Result<SupervisedSnapshot> {
        self.observe("status", request)
    }
    pub fn cancel(&self, request: &BuildRequest) -> Result<SupervisedSnapshot> {
        self.observe("cancel", request)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisedLogs {
    pub job: SupervisedSnapshot,
    pub page: LogsPage,
}
impl SupervisedSshClient {
    pub fn logs(
        &self,
        request: &BuildRequest,
        cursor: u64,
        max_bytes: usize,
    ) -> Result<SupervisedLogs> {
        request.validate()?;
        ensure!((1..=65536).contains(&max_bytes), "invalid log byte bound");
        let value: SupervisedLogs = self.0.call(
            "logs",
            &LogsQuery {
                request_id: request.request_id.clone(),
                cursor,
                max_bytes,
            },
        )?;
        value.job.correlate(request)?;
        ensure!(
            value.page.cursor == cursor
                && value.page.bytes.len() <= max_bytes
                && cursor.checked_add(value.page.bytes.len() as u64)
                    == Some(value.page.next_cursor),
            "log cursor response mismatch"
        );
        Ok(value)
    }
}
