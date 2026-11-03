//! Typed request, frozen administrator configuration, and factual job state.
#[cfg(unix)]
use anyhow::Context;
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Raw spec is bounded separately; this cap also accommodates JSON control escapes.
/// The canonical `request_id` must be a valid UUID.
pub const MAX_REQUEST_JSON_BYTES: usize = 640 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildRequest {
    pub request_id: String,
    pub turn_id: String,
    pub conversation_id: String,
    pub repo_id: String,
    pub baseline_sha: String,
    pub branch: String,
    pub spec_text: String,
    pub target_id: String,
    pub verify_profile: String,
}
impl BuildRequest {
    pub fn validate(&self) -> Result<()> {
        valid_id(&self.request_id)?;
        ensure!(
            self.baseline_sha.len() == 40
                && self.baseline_sha.bytes().all(|c| c.is_ascii_hexdigit()),
            "baseline must be full SHA1"
        );
        ensure!(
            ["fix/", "feat/", "chore/"]
                .iter()
                .any(|p| self.branch.starts_with(p)),
            "branch must be an owned task branch"
        );
        ensure!(
            self.branch.len() <= 160
                && self
                    .branch
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"/-_.".contains(&c)),
            "invalid branch characters"
        );
        ensure!(
            self.branch.split('/').all(|s| !s.is_empty()
                && !s.starts_with('.')
                && !s.ends_with('.')
                && !s.ends_with(".lock")
                && !s.contains("..")),
            "invalid branch components"
        );
        ensure!(
            !self.spec_text.is_empty()
                && self.spec_text.len() <= 96 * 1024
                && !self.spec_text.contains('\0'),
            "spec must be 1..98304 bytes"
        );
        for value in [
            &self.turn_id,
            &self.conversation_id,
            &self.repo_id,
            &self.target_id,
            &self.verify_profile,
        ] {
            ensure!(
                !value.is_empty() && value.len() <= 256 && !value.contains('\0'),
                "invalid identifier"
            );
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_REQUEST_JSON_BYTES,
            "serialized request too large"
        );
        Ok(())
    }
    pub fn fingerprint(&self) -> String {
        // Struct field order is fixed and strings are always serializable.
        digest(&serde_json::to_vec(self).expect("request serialization"))
    }
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn valid_id(id: &str) -> Result<()> {
    let parsed = uuid::Uuid::parse_str(id)?;
    ensure!(
        parsed.hyphenated().to_string() == id,
        "request ID must be canonical UUID"
    );
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}
/// Sandbox path where the worker binds each job's private scratch directory
/// (S3(b)). The model's HOME and TMPDIR live under it. Reserved: no
/// administrator bind may use it.
pub const SANDBOX_SCRATCH: &str = "/awili-scratch";
/// Fixed bubblewrap isolation profile for one command role. The model is the
/// only role the worker enforces (S3(b)); the verifier profile is reserved
/// for S3(c) and must be validated by its own code before use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BwrapProfile {
    /// Absolute path of the bubblewrap executable. Missing or non-executable
    /// bwrap fails the job before the model runs; the worker never falls back
    /// to a direct command.
    pub bwrap: PathBuf,
    /// Absolute host paths bound read-only into the model's filesystem view.
    pub readonly: Vec<PathBuf>,
    /// Absolute host paths bound writable at the same path. Shared writable
    /// state crosses jobs, so the deploy profile lists none; each job's
    /// private scratch is created and bound by the worker at
    /// `SANDBOX_SCRATCH`.
    pub writable: Vec<PathBuf>,
    /// Environment variables forwarded into the sandbox beyond the target's
    /// declared model environment; fixed names, administrator-configured
    /// values.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}
/// S3(b)/(c): the fixed bwrap isolation profiles for the two roles the
/// worker executes. The model role (S3(b)) and the verifier role (S3(c))
/// each run only through their own profile; the worker refuses to launch
/// either role when its profile is missing or unavailable (fail closed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolationSpec {
    /// The model role: the only role the worker may launch through bwrap.
    #[serde(rename = "model")]
    pub bwrap: BwrapProfile,
    /// The verifier role (S3(c)): the fixed verifier executable runs
    /// outside the model's checkout, over the source export of the exact
    /// committed result, with no push credentials and no control
    /// directory in its view.
    #[serde(rename = "verifier")]
    pub verifier: BwrapProfile,
}
impl IsolationSpec {
    /// Administrator-configured isolation must be absolute paths only; a
    /// relative bind source or bwrap path is an error, not a runtime
    /// surprise. Values are administrator configuration; this check keeps a
    /// malformed file from silently degrading the sandbox.
    pub fn validate(&self) -> Result<()> {
        let profile = &self.bwrap;
        ensure!(
            profile.bwrap.is_absolute(),
            "bwrap executable must be absolute"
        );
        for path in profile.readonly.iter().chain(profile.writable.iter()) {
            ensure!(
                path.is_absolute(),
                "isolation bind path must be absolute: {path:?}"
            );
            ensure!(
                !path.starts_with(SANDBOX_SCRATCH),
                "isolation bind path uses the reserved scratch path: {path:?}"
            );
        }
        for (k, v) in &profile.env {
            if k.contains('=') || k.contains('\0') || v.contains('\0') {
                bail!("invalid isolation environment");
            }
        }
        // RC2-R4 (TOP43 #12): both roles' variables reach bwrap itself,
        // before any isolation; a loader variable would run code there.
        for (k, _) in profile.env.iter().chain(self.verifier.env.iter()) {
            launch_env_name(k)?;
        }
        Ok(())
    }
}
/// RC2-R4 (TOP43 #12): the administrator's environment for a role is applied
/// to the sandbox launcher (bwrap) and inherited by the program inside it, so
/// it may set the program's settings but never change how a program is
/// loaded: names must be plain identifiers, and the dynamic-loader and
/// C-library hooks (`LD_*`, `DYLD_*`, `GCONV_PATH`, `LOCPATH`, `MALLOC_*`)
/// are refused.
pub fn launch_env_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    ensure!(
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "invalid environment name {name:?}"
    );
    let upper = name.to_ascii_uppercase();
    ensure!(
        !["LD_", "DYLD_", "MALLOC_"]
            .iter()
            .any(|p| upper.starts_with(p))
            && !["GCONV_PATH", "LOCPATH"].contains(&upper.as_str()),
        "environment name {name:?} changes how programs load; refused"
    );
    Ok(())
}
/// Largest number of configured arguments for one command, and the largest
/// one argument (RC2-R4, TOP43 #11).
pub const MAX_COMMAND_ARGS: usize = 64;
pub const MAX_COMMAND_ARG_BYTES: usize = 4096;
/// RC2-R4 (TOP43 #13): the repository transports a target may name — `ssh://`,
/// `https://`, `file:///…` and an absolute local path. Git's helper and
/// descriptor transports (`ext::`, `fd::`), plain `http://` and `git://`,
/// scp-style `host:path`, an option-like value and control characters or
/// spaces are refused. The worker's Git also runs with
/// `GIT_ALLOW_PROTOCOL=ssh:https:file`, so Git itself enforces the same list.
pub fn valid_repo_url(url: &str) -> Result<()> {
    ensure!(
        !url.is_empty()
            && url.len() <= 2048
            && !url.starts_with('-')
            && !url.contains("::")
            && url.chars().all(|c| !c.is_control() && !c.is_whitespace()),
        "repository URL must be one plain URL or absolute path"
    );
    let allowed = ["ssh://", "https://", "file:///"]
        .iter()
        .any(|p| url.starts_with(p) && url.len() > p.len())
        || (url.starts_with('/') && !url.starts_with("//"));
    ensure!(
        allowed,
        "repository URL transport refused (only ssh://, https://, file:/// or an absolute path): {url}"
    );
    Ok(())
}
/// The transports the worker's Git may use (`GIT_ALLOW_PROTOCOL`).
pub const GIT_ALLOWED_PROTOCOLS: &str = "ssh:https:file";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildTarget {
    pub repo_id: String,
    pub repo_url: String,
    pub model: String,
    pub command: CommandSpec,
    pub verify_profile: String,
    pub verify: CommandSpec,
    /// Fixed bwrap profile for the model role; optional because the
    /// worker enforces it on Linux and fails closed when absent.
    #[serde(default)]
    pub isolation: Option<IsolationSpec>,
}
/// Largest runner configuration file read (RC2-R4).
pub const MAX_CONFIG_BYTES: usize = 1 << 20;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerConfig {
    pub job_root: PathBuf,
    pub targets: BTreeMap<String, BuildTarget>,
}
impl RunnerConfig {
    /// RC2-R4 (TOP43 #11-#13): read the runner configuration, the
    /// administrator's trust root (it names every program, argument,
    /// variable and repository a job uses). It must be a regular file owned by
    /// root or by the user the runner runs as, and writable by neither its group
    /// nor every user (RC2-R4M1: a group of non-administrators could rewrite
    /// the commands); otherwise nothing runs. The deploy installs it root-owned, mode 0644.
    #[cfg(not(unix))]
    pub fn load(_path: &Path) -> Result<RunnerConfig> {
        // TODO(windows-phase1): the runner host is Linux-only and its trust-root
        // check is uid/mode based. Fail closed rather than skip the ownership check.
        anyhow::bail!("runner configuration loading is not supported on Windows yet")
    }
    #[cfg(unix)]
    pub fn load(path: &Path) -> Result<RunnerConfig> {
        use std::{io::Read, os::unix::fs::MetadataExt};
        let file = std::fs::File::open(path).context("config read failed")?;
        let meta = file.metadata().context("config read failed")?;
        ensure!(meta.is_file(), "config read failed: not a regular file");
        let euid = rustix::process::geteuid().as_raw();
        ensure!(
            meta.uid() == 0 || meta.uid() == euid,
            "runner configuration is owned by another user; refused"
        );
        ensure!(
            meta.mode() & 0o022 == 0,
            "runner configuration is writable by every user or by its group; refused"
        );
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .context("config read failed")?;
        ensure!(
            bytes.len() <= MAX_CONFIG_BYTES,
            "runner configuration too large"
        );
        Ok(serde_json::from_slice(&bytes)?)
    }
    pub fn target(&self, request: &BuildRequest) -> Result<BuildTarget> {
        ensure!(self.job_root.is_absolute(), "job root must be absolute");
        let target = self
            .targets
            .get(&request.target_id)
            .ok_or_else(|| anyhow::anyhow!("unknown target"))?;
        ensure!(
            target.repo_id == request.repo_id && target.verify_profile == request.verify_profile,
            "target binding mismatch"
        );
        ensure!(
            !target.model.is_empty()
                && !target.repo_url.starts_with('-')
                && !target.repo_url.is_empty(),
            "invalid target"
        );
        valid_repo_url(&target.repo_url)?;
        for command in [&target.command, &target.verify] {
            ensure!(
                command.program.is_absolute(),
                "configured executable must be absolute"
            );
            // RC2-R4 (TOP43 #11): arguments are the administrator's data for
            // the fixed program, bounded and free of NUL (which would cut an
            // argument short at exec).
            ensure!(
                command.args.len() <= MAX_COMMAND_ARGS
                    && command
                        .args
                        .iter()
                        .all(|a| a.len() <= MAX_COMMAND_ARG_BYTES && !a.contains('\0')),
                "invalid command arguments"
            );
            for (k, v) in &command.env {
                if k.contains('=') || k.contains('\0') || v.contains('\0') {
                    bail!("invalid environment");
                }
                launch_env_name(k)?;
            }
        }
        // The model's isolation profile is enforced by the worker; a
        // malformed profile must fail the job before model launch.
        if let Some(isolation) = &target.isolation {
            isolation.validate()?;
        }
        Ok(target.clone())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildState {
    Queued,
    Running,
    Verifying,
    AwaitingReview,
    Failed,
    CancelRequested,
    Stopped,
    NeedsAttention,
}
impl BuildState {
    pub fn ready_for_review(self) -> bool {
        self == Self::AwaitingReview
    }
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Running | Self::Verifying | Self::CancelRequested
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerIdentity {
    pub pid: u32,
    pub stamp: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildSnapshot {
    pub request_id: String,
    pub request_sha256: String,
    pub revision: u64,
    pub state: BuildState,
    pub executor: String,
    pub requested_model: String,
    pub baseline_sha: String,
    pub result_sha: Option<String>,
    pub remote_sha: Option<String>,
    pub model_exit: Option<i32>,
    pub verify_exit: Option<i32>,
    pub push_exit: Option<i32>,
    pub worker: Option<WorkerIdentity>,
    pub failure: Option<String>,
    pub no_op: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    pub request: BuildRequest,
    pub target: BuildTarget,
    pub snapshot: BuildSnapshot,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobQuery {
    pub request_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogsQuery {
    pub request_id: String,
    pub cursor: u64,
    pub max_bytes: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogsPage {
    pub cursor: u64,
    pub next_cursor: u64,
    pub bytes: Vec<u8>,
}
