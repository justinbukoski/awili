//! Fixed remote executable + JSON stdin; uncertain submit never gets a new ID.
use crate::protocol::*;
use anyhow::{ensure, Context, Result};
use std::{path::PathBuf, process::Command, time::Duration};

pub struct SshClient {
    pub ssh: PathBuf,
    pub host: String,
    pub runner: String,
    pub config: String,
    pub timeout: Duration,
}
impl SshClient {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.host.starts_with('-')
                && !self.host.is_empty()
                && self
                    .host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._@:-".contains(&b)),
            "unsafe SSH host"
        );
        for path in [&self.runner, &self.config] {
            ensure!(
                path.starts_with('/')
                    && path
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b)),
                "remote path must be absolute and shell-safe"
            );
        }
        Ok(())
    }
    pub(crate) fn call<T: serde::de::DeserializeOwned>(
        &self,
        operation: &str,
        value: &impl serde::Serialize,
    ) -> Result<T> {
        self.validate()?;
        ensure!(!self.timeout.is_zero(), "SSH deadline must be positive");
        let payload = serde_json::to_vec(value)?;
        let mut command = Command::new(&self.ssh);
        command.args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "--",
            &self.host,
            &self.runner,
            "--config",
            &self.config,
            operation,
        ]);
        let bytes = crate::transport::exchange(command, &payload, self.timeout, 512 * 1024)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn status(&self, id: &str) -> Result<BuildSnapshot> {
        valid_id(id)?;
        let snapshot: BuildSnapshot = self.call(
            "status",
            &JobQuery {
                request_id: id.into(),
            },
        )?;
        ensure!(
            snapshot.request_id == id,
            "status response request ID mismatch"
        );
        Ok(snapshot)
    }
    pub fn submit(&self, request: &BuildRequest) -> Result<BuildSnapshot> {
        request.validate()?;
        let snapshot = match self.call("submit", request) {
            Ok(s) => s,
            Err(_) => self.status(&request.request_id).context(
                "submission uncertain: reconcile this request ID; do not generate a new one",
            )?,
        };
        ensure!(
            snapshot.request_id == request.request_id
                && snapshot.request_sha256 == request.fingerprint(),
            "request ID conflict or invalid runner response"
        );
        Ok(snapshot)
    }
    pub fn cancel(&self, id: &str) -> Result<BuildSnapshot> {
        valid_id(id)?;
        let snapshot: BuildSnapshot = self.call(
            "cancel",
            &JobQuery {
                request_id: id.into(),
            },
        )?;
        ensure!(
            snapshot.request_id == id,
            "cancel response request ID mismatch"
        );
        Ok(snapshot)
    }
    pub fn logs(&self, query: &LogsQuery) -> Result<LogsPage> {
        valid_id(&query.request_id)?;
        self.call("logs", query)
    }
}
