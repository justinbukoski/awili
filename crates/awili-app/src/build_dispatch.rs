//! Daily-driver P2d — the app binding of the local Build lane to the durable
//! awili-dispatch SSH client.
//!
//! GPUI-free core: the Work UI ([`crate::work`]) resolves the selected
//! `[build_targets.<id>]` executor to a [`ResolvedBuild`] here, submits ONCE,
//! persists the durable request id BEFORE any Running state is shown, and
//! then polls `status` on the SAME id. A lost acknowledgement is reconciled
//! by the client's own same-id `status` — never by a fresh submission — and
//! no lane other than the manual `LocalBuild` override ever reaches this
//! code (Ask/Plan/Review keep the gateway/agent lanes).

#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use awili_config::BuildTargetConfig;
use awili_dispatch::{
    protocol::{BuildRequest, BuildSnapshot, BuildState},
    ssh::SshClient,
};

use crate::work::WorkExecutor;

/// The fixed location of the trusted-administrator runner configuration on
/// the dispatch host. The app-side target names the host and the runner
/// executable; the runner's own repository/command/model bindings live in
/// THIS file, owned by the administrator (P2c runner acceptance) — prompt
/// text can never choose executables, repositories, or models.
pub(crate) const RUNNER_CONFIG_PATH: &str = "/etc/awili/build-runner.json";

/// The deadline for one SSH submit/status exchange.
pub(crate) const SSH_TIMEOUT: Duration = Duration::from_secs(15);

/// How often the UI polls `status` for a submitted (or reattached) build.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// The app-local build-job token persisted BEFORE the runner acknowledges a
/// submit (P10/F4). It is NOT a runner `BuildState` — the dispatch protocol is
/// frozen — so the app-local states live in the persistence layer's widened
/// `build_jobs.state` CHECK set.
pub(crate) const STATE_SUBMITTING: &str = "submitting";

/// The app-local build-job token persisted when the submit (and its same-id
/// `status` fallback) failed. The request id stays durable so a later
/// reattach reconciles by `status` of the SAME id, never a fresh submit.
pub(crate) const STATE_UNKNOWN: &str = "unknown";

/// A local-build dispatch error, surfaced verbatim on the Work panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BuildDispatchError {
    /// The selected target id is not a configured `[build_targets.<id>]` — a
    /// config error naming the id, with ZERO outbound calls.
    UnknownTarget(String),
    /// The working folder cannot supply the repository baseline/branch the
    /// runner must own.
    RepoState(String),
    /// The runner refused the request, or answered with a conflicting or
    /// invalid payload (never a success).
    Runner(String),
}

impl std::fmt::Display for BuildDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildDispatchError::UnknownTarget(id) => {
                write!(f, "build target '{id}' is not configured ([build_targets])")
            }
            BuildDispatchError::RepoState(why) => write!(f, "build preflight failed: {why}"),
            BuildDispatchError::Runner(why) => write!(f, "build runner: {why}"),
        }
    }
}

/// The single routing gate for the local Build lane: ONLY the manual
/// `LocalBuild` override (the picker's BUILD section) dispatches here. The
/// gateway (Ask/Plan) and agent (Review) lanes never reach the dispatcher —
/// no other executor variant can start a build.
pub(crate) fn dispatches_for(executor: &WorkExecutor) -> bool {
    matches!(executor, WorkExecutor::LocalBuild { .. })
}

/// One local-build submit's inputs: the durable turn identity, the owning
/// conversation, the frozen spec text (the user's task, verbatim), and the
/// working folder's git baseline and task branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BuildInputs {
    pub turn_id: String,
    pub conversation_id: String,
    pub spec_text: String,
    pub baseline_sha: String,
    pub branch: String,
}

/// A configured target resolved for dispatch: the frozen binding plus the
/// fixed SSH client that talks to it. The SSH client is moved into the
/// driving task (one client per drive — no cloning, no sharing).
pub(crate) struct ResolvedBuild {
    pub target_id: String,
    pub target: BuildTargetConfig,
    client: SshClient,
}

impl ResolvedBuild {
    /// Resolve `target_id` against the configured targets. An unknown id is a
    /// config error naming the id: no client is usable and ZERO outbound
    /// calls are made.
    pub(crate) fn resolve(
        targets: &BTreeMap<String, BuildTargetConfig>,
        target_id: &str,
    ) -> Result<Self, BuildDispatchError> {
        Self::resolve_with(targets, target_id, PathBuf::from("ssh"))
    }

    /// Same as [`resolve`](Self::resolve), with an explicit ssh binary —
    /// tests point it at a fake executable that captures argv and stdin.
    pub(crate) fn resolve_with(
        targets: &BTreeMap<String, BuildTargetConfig>,
        target_id: &str,
        ssh: PathBuf,
    ) -> Result<Self, BuildDispatchError> {
        let target = targets
            .get(target_id)
            .ok_or_else(|| BuildDispatchError::UnknownTarget(target_id.to_string()))?;
        let client = SshClient {
            ssh,
            host: target.ssh_host.clone(),
            runner: target.runner_path.clone(),
            config: RUNNER_CONFIG_PATH.into(),
            timeout: SSH_TIMEOUT,
        };
        Ok(Self {
            target_id: target_id.to_string(),
            target: target.clone(),
            client,
        })
    }

    /// Submit `request` ONCE through the SSH client. A lost acknowledgement
    /// is reconciled by the client's same-id `status` (never a new ID). The
    /// returned snapshot carries the durable request identity the caller
    /// MUST persist before any Running state is shown.
    pub(crate) fn submit(
        &self,
        request: &BuildRequest,
    ) -> Result<BuildSnapshot, BuildDispatchError> {
        self.client
            .submit(request)
            .map_err(|e| BuildDispatchError::Runner(e.to_string()))
    }

    /// Poll the durable state of `id` — the SAME id that was submitted;
    /// reconciliation is by status of that id, never a fresh submit.
    pub(crate) fn status(&self, id: &str) -> Result<BuildSnapshot, BuildDispatchError> {
        self.client
            .status(id)
            .map_err(|e| BuildDispatchError::Runner(e.to_string()))
    }
}

/// Read the working folder's git baseline (HEAD SHA) and current task branch
/// with direct `git` invocations (never a shell). A non-repo folder, an
/// unknown HEAD, or a detached state is a preflight failure: the panel
/// surfaces it and nothing is submitted.
pub(crate) fn repo_state(folder: &Path) -> Result<(String, String), BuildDispatchError> {
    let head = git_stdout(folder, &["rev-parse", "HEAD"])?
        .ok_or_else(|| BuildDispatchError::RepoState("not a git repository".to_string()))?;
    let branch = git_stdout(folder, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .ok_or_else(|| BuildDispatchError::RepoState("current branch unknown".to_string()))?;
    if branch == "HEAD" {
        return Err(BuildDispatchError::RepoState(
            "detached HEAD — check out a task branch first".to_string(),
        ));
    }
    Ok((head, branch))
}

/// One bounded `git -C <folder>` read; `None` when the command exits nonzero
/// (the caller decides the preflight message).
fn git_stdout(folder: &Path, args: &[&str]) -> Result<Option<String>, BuildDispatchError> {
    let git = crate::agent_bin::git_program().map_err(BuildDispatchError::RepoState)?;
    let out = std::process::Command::new(git)
        .arg("-C")
        .arg(folder)
        .args(args)
        .output()
        .map_err(|e| BuildDispatchError::RepoState(format!("git unavailable: {e}")))?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// Build the typed request for one local build: the durable ids come from
/// the turn, the repository/verification binding from the CONFIGURED target
/// (never prompt text), and the spec text stays literal data end-to-end.
pub(crate) fn build_request(
    resolved: &ResolvedBuild,
    inputs: &BuildInputs,
) -> Result<BuildRequest, BuildDispatchError> {
    let request = BuildRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        turn_id: inputs.turn_id.clone(),
        conversation_id: inputs.conversation_id.clone(),
        repo_id: resolved.target.repo_id.clone(),
        baseline_sha: inputs.baseline_sha.clone(),
        branch: inputs.branch.clone(),
        spec_text: inputs.spec_text.clone(),
        target_id: resolved.target_id.clone(),
        verify_profile: resolved.target.verify_profile.clone(),
    };
    request
        .validate()
        .map_err(|e| BuildDispatchError::Runner(e.to_string()))
        .map(|_| request)
}

/// The snake_case wire token for a [`BuildState`] — the `build_jobs.state`
/// CHECK set mirrors these tokens exactly.
pub(crate) fn state_token(state: BuildState) -> &'static str {
    match state {
        BuildState::Queued => "queued",
        BuildState::Running => "running",
        BuildState::Verifying => "verifying",
        BuildState::AwaitingReview => "awaiting_review",
        BuildState::Failed => "failed",
        BuildState::CancelRequested => "cancel_requested",
        BuildState::Stopped => "stopped",
        BuildState::NeedsAttention => "needs_attention",
    }
}

/// Terminal = no further runner state change is expected: `awaiting_review`
/// (the ONLY success — it never means merged or deployed), `failed`,
/// `stopped`, or `needs_attention`. Active states keep polling.
pub(crate) fn is_terminal(state: BuildState) -> bool {
    !state.active()
}

/// Persist the DURABLE pre-submit row (P10/F4): the locally minted request id
/// is on disk as `submitting` BEFORE `resolved.submit` is ever called, so a
/// crash or a never-acknowledged submit still leaves a row to reconcile.
/// Called on the UI thread, so the command is queued ahead of the submit.
pub(crate) fn enqueue_submitting(
    persist: Option<&crate::persistence::Persistence>,
    request: &BuildRequest,
    target_id: &str,
) {
    if let Some(p) = persist {
        p.send(crate::persistence::PersistCmd::UpsertBuildJob {
            turn_id: request.turn_id.clone(),
            request_id: request.request_id.clone(),
            target_id: target_id.to_string(),
            revision: 0,
            state: STATE_SUBMITTING.to_string(),
            result_sha: None,
            failure: None,
            updated_at: crate::feed::unix_now(),
        });
    }
}

/// Record the outcome of one submit attempt on the SAME row (P10/F4): the
/// runner's returned state on success, or `unknown` carrying the error text
/// when the submit (including its same-id `status` fallback) failed. The row
/// is never deleted — a later `status` of this id still reconciles it.
pub(crate) fn record_submit_outcome(
    persist: Option<&crate::persistence::Persistence>,
    request: &BuildRequest,
    target_id: &str,
    outcome: &Result<BuildSnapshot, BuildDispatchError>,
) {
    let Some(p) = persist else { return };
    let (state, revision, result_sha, failure) = match outcome {
        Ok(snapshot) => (
            state_token(snapshot.state).to_string(),
            snapshot.revision,
            snapshot.result_sha.clone(),
            snapshot.failure.clone(),
        ),
        Err(error) => (STATE_UNKNOWN.to_string(), 0, None, Some(error.to_string())),
    };
    p.send(crate::persistence::PersistCmd::UpsertBuildJob {
        turn_id: request.turn_id.clone(),
        request_id: request.request_id.clone(),
        target_id: target_id.to_string(),
        revision,
        state,
        result_sha,
        failure,
        updated_at: crate::feed::unix_now(),
    });
}

/// The panel line for a build snapshot: the ACTUAL state and outcome data
/// (state, failure, result SHA) — never a synthesized assistant answer.
pub(crate) fn snapshot_line(snapshot: &BuildSnapshot) -> String {
    let mut line = format!(
        "Build {}: {}",
        snapshot.request_id,
        state_token(snapshot.state)
    );
    if let Some(sha) = &snapshot.result_sha {
        line.push_str(&format!(" · result {sha}"));
    }
    if let Some(failure) = &snapshot.failure {
        line.push_str(&format!(" — {failure}"));
    }
    line
}

/// Close a build row with the app's own `stale: …` verdict (daily-driver
/// P2f): the SAME turn and request id move to the runner's terminal
/// `needs_attention` token (Fable ruling 2 — no new state, no migration), so
/// `open_build_jobs` never returns the row again. Never a runner snapshot:
/// the verdict is carried only in `failure`.
pub(crate) fn close_row(
    persist: Option<&crate::persistence::Persistence>,
    turn_id: &str,
    request_id: &str,
    target_id: &str,
    revision: u64,
    failure: String,
) {
    if let Some(p) = persist {
        p.send(crate::persistence::PersistCmd::UpsertBuildJob {
            turn_id: turn_id.to_string(),
            request_id: request_id.to_string(),
            target_id: target_id.to_string(),
            revision,
            state: state_token(BuildState::NeedsAttention).to_string(),
            result_sha: None,
            failure: Some(failure),
            updated_at: crate::feed::unix_now(),
        });
    }
}
