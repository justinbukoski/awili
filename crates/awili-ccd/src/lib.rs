//! `awili-ccd` — a typed client for the Claude-Code Dispatcher (`ccd`) HTTP API
//! (slice-5 spec §B). ccd owns the local lifecycle of `claude` coding jobs on a
//! fleet host; Awili submits heavy/async work and renders its state. Awili is a
//! **read-only console** over ccd — it never supervises the coding CLI itself
//! (brief §"What Awili's jobs are", item 5).
//!
//! Contract (captured from `~/Projects/ccd`, v1.2):
//! - `POST /dispatch` `{repo, branch, spec, model, verify_profile?}` → `{id}`
//! - `GET  /jobs/:id` → job state, liveness, progress, log tail
//! - `POST /jobs/:id/kill` → contained kill (cgroup v2)
//! - `POST /jobs/:id/verify` `{verify_profile}` → verify outcome
//! - Auth: `Authorization: Bearer <token>` on every request.
//!
//! As in `awili-providers`, the **decoders are pure** (parse a recorded ccd JSON
//! body into typed values) and fully fixture-tested; the socket wiring is a thin
//! blocking-reqwest layer validated by the live canary, not by `cargo test`.

use std::io::Read;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A ccd job's lifecycle state, normalised from ccd's wire `state` string. The
/// `needs_attention` state is ccd's fail-closed terminal-ish state (a kill that
/// could not be confirmed); the UI must surface it loudly, never as "done".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Persisted the instant spawning begins, before the child is confirmed.
    Spawning,
    /// Child spawned, alive, no edits yet, within the start grace window.
    Starting,
    /// Child pid alive and doing work.
    Working,
    /// Child gone; produced edits or ran long enough to be a real run (terminal).
    Exited,
    /// Verify command has run (terminal success).
    Verified,
    /// Verify passed but the job produced zero edits — a no-op, not a success
    /// (terminal).
    NoOp,
    /// Verify ran and failed (terminal failure).
    VerifyFailed,
    /// Child gone with zero edits and a very short runtime — the "died immediately"
    /// failure SSH dispatch could not detect (terminal failure).
    DeadEarly,
    /// FAIL-CLOSED: a kill/drain could not be confirmed; lock retained; needs an
    /// operator. Terminal-ish, NEVER success.
    NeedsAttention,
    /// Any state string this client predates — surfaced verbatim, never crashes.
    #[serde(other)]
    Unknown,
}

impl JobState {
    /// Whether the job has reached a state where no further polling is useful.
    /// `NeedsAttention` counts as terminal for polling (ccd will not advance it
    /// without an operator), but is NOT success.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Exited
                | JobState::Verified
                | JobState::NoOp
                | JobState::VerifyFailed
                | JobState::DeadEarly
                | JobState::NeedsAttention
        )
    }

    /// True only for a clean success: a verified job, or an exited job from a
    /// run with no verify profile. `NoOp` (zero edits), `VerifyFailed`,
    /// `DeadEarly`, and `NeedsAttention` are NOT success.
    pub fn is_success(self) -> bool {
        matches!(self, JobState::Verified | JobState::Exited)
    }
}

/// A ccd job as reported by `GET /jobs/:id`. Extra fields ccd may send are
/// ignored; the ones the console renders are typed here.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Job {
    #[serde(rename = "job_id")]
    pub id: String,
    pub state: JobState,
    /// Edited-file count since dispatch (ccd's real-progress signal). Best-effort.
    #[serde(default)]
    pub files_touched: u32,
    /// Wall-clock runtime in seconds, if ccd reports it.
    #[serde(default)]
    pub runtime_s: u64,
    /// Process exit code, once the child is gone.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// ccd's explicit fail-closed flag (mirrors NeedsAttention).
    #[serde(default)]
    pub needs_attention: bool,
    /// Tail of the job log.
    #[serde(default)]
    pub log_tail: Option<String>,
    /// The repo the job ran against.
    #[serde(default)]
    pub repo: Option<String>,
    /// The engine that ran it (claude|deepseek|codex|…).
    #[serde(default)]
    pub engine: Option<String>,
}

/// The body for `POST /dispatch`. Mirrors ccd's `DispatchReq` exactly so the wire
/// shape is the source of truth.
#[derive(Clone, Debug, Serialize)]
pub struct DispatchReq {
    pub repo: String,
    pub branch: String,
    pub spec: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_profile: Option<String>,
}

impl DispatchReq {
    /// A dispatch with ccd's default model (`claude-opus-4-8`) and no verify
    /// profile. Builders below let callers override either.
    pub fn new(
        repo: impl Into<String>,
        branch: impl Into<String>,
        spec: impl Into<String>,
    ) -> Self {
        Self {
            repo: repo.into(),
            branch: branch.into(),
            spec: spec.into(),
            model: "claude-opus-4-8".to_string(),
            verify_profile: None,
        }
    }
    pub fn model(mut self, m: impl Into<String>) -> Self {
        self.model = m.into();
        self
    }
    pub fn verify_profile(mut self, p: impl Into<String>) -> Self {
        self.verify_profile = Some(p.into());
        self
    }
}

/// The id returned by `POST /dispatch`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct DispatchResp {
    #[serde(rename = "job_id")]
    pub id: String,
}

/// The outcome of `POST /jobs/:id/verify`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct VerifyOutcome {
    /// Whether every command in the named profile succeeded.
    pub passed: bool,
    /// The profile that ran.
    #[serde(default)]
    pub profile: Option<String>,
    /// Combined output / failure detail for the console.
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Error)]
pub enum CcdError {
    #[error("ccd transport error: {0}")]
    Transport(String),
    #[error("ccd returned HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("ccd response did not parse: {0}")]
    Decode(String),
    #[error("polling job {id} exceeded {secs}s without reaching a terminal state")]
    PollTimeout { id: String, secs: u64 },
    #[error("polling cancelled")]
    Cancelled,
}

// ---- Pure decoders (fixture-tested) ----------------------------------------

/// Parse a `POST /dispatch` 200 body.
pub fn decode_dispatch(body: &str) -> Result<DispatchResp, CcdError> {
    serde_json::from_str(body).map_err(|e| CcdError::Decode(e.to_string()))
}

/// Parse a `GET /jobs/:id` 200 body.
pub fn decode_job(body: &str) -> Result<Job, CcdError> {
    serde_json::from_str(body).map_err(|e| CcdError::Decode(e.to_string()))
}

/// Parse a `POST /jobs/:id/verify` 200 body.
pub fn decode_verify(body: &str) -> Result<VerifyOutcome, CcdError> {
    serde_json::from_str(body).map_err(|e| CcdError::Decode(e.to_string()))
}

// ---- Transport (blocking reqwest; live-validated, not in cargo test) --------

/// S7c (F0343): the deadline for every ccd request. reqwest's `timeout` is
/// a whole-request deadline (connect, send and the full body), so no single
/// call can outlast it, and `poll_until_terminal` overshoots its own
/// timeout by at most one `request` deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CcdTimeouts {
    /// TCP/TLS connect.
    pub connect: Duration,
    /// `GET /jobs/:id` and `POST /jobs/:id/kill`.
    pub request: Duration,
    /// `POST /dispatch`.
    pub dispatch: Duration,
    /// `POST /jobs/:id/verify`.
    pub verify: Duration,
}

impl Default for CcdTimeouts {
    /// Production deadlines, each sized from ccd's own server-side bounds:
    /// - `connect` 10 s: same as the gateway and provider clients.
    /// - `request` 60 s: a job read is one file read; a kill waits for its
    ///   cgroup to drain for at most about 21 s (ccd `cgroup.rs`: 10 s, 8 s
    ///   and 3 s waits), so 60 s is about three times the worst case.
    /// - `dispatch` 180 s: ccd's backend preflight is capped at 120 s (+2 s
    ///   process slack, ccd `config.rs` `PREFLIGHT_TIMEOUT_MAX_SECS`), plus
    ///   about a minute for git validation and the spawn.
    /// - `verify` 660 s: ccd kills a verify run at 600 s (ccd `main.rs`
    ///   `VERIFY_TIMEOUT`); 60 s more lets its answer arrive.
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            request: Duration::from_secs(60),
            dispatch: Duration::from_secs(180),
            verify: Duration::from_secs(660),
        }
    }
}

/// S7c: the most bytes of one ccd response Awili reads. 8 MiB: a job body
/// carries a log tail that ccd itself caps at 256 KiB (ccd `progress.rs`
/// `MAX_TAIL_BYTES`), and a verify answer carries the verify output; 8 MiB
/// is far above both while a broken endpoint can no longer grow a reply
/// without limit.
pub const MAX_CCD_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// S7c: the owner-visible error for a 2xx body over [`MAX_CCD_BODY_BYTES`].
pub const CCD_BODY_TOO_LARGE: &str =
    "The job service sent a reply larger than 8 MB, so Awili stopped reading it.";

/// S7c: the owner-visible error for a job id that cannot be sent.
pub const CCD_BAD_JOB_ID: &str =
    "Awili did not send this request: the job id is empty or only dots.";

/// S7c: the owner-visible error when the HTTP client cannot be built.
pub const CCD_NO_CLIENT: &str = "Awili could not set up its connection to the job service.";

/// S7c: a job id as exactly ONE URL path segment. Every byte outside the
/// RFC 3986 unreserved set (`A-Z a-z 0-9 - . _ ~`) is percent-encoded; an
/// empty id, `.` or `..` is refused before any request is built, because
/// the URL parser resolves dot segments onto a different route.
fn job_segment(id: &str) -> Result<String, CcdError> {
    if id.is_empty() || id == "." || id == ".." {
        return Err(CcdError::Transport(CCD_BAD_JOB_ID.to_string()));
    }
    let mut out = String::with_capacity(id.len());
    for b in id.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(out)
}

/// S7c: read at most `cap + 1` bytes from `reader` and never more.
/// Returns the first `cap` bytes and whether the body was longer than `cap`.
pub fn read_capped<R: Read>(reader: R, cap: u64) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    reader.take(cap + 1).read_to_end(&mut bytes)?;
    let over = bytes.len() as u64 > cap;
    if over {
        bytes.truncate(cap as usize);
    }
    Ok((bytes, over))
}

/// A ccd HTTP client. One per configured ccd endpoint.
pub struct CcdClient {
    base_url: String,
    token: String,
    timeouts: CcdTimeouts,
    /// `None` only if the bounded client could not be built; every call then
    /// fails with [`CCD_NO_CLIENT`]. There is NO fallback to a default client
    /// without deadlines (S7c).
    http: Option<reqwest::blocking::Client>,
}

impl CcdClient {
    /// A client with the production [`CcdTimeouts`].
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self::with_timeouts(base_url, token, CcdTimeouts::default())
    }

    /// A client with explicit deadlines (tests use short ones).
    pub fn with_timeouts(
        base_url: impl Into<String>,
        token: impl Into<String>,
        timeouts: CcdTimeouts,
    ) -> Self {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(timeouts.connect)
            .timeout(timeouts.request)
            .build()
            .ok();
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            timeouts,
            http,
        }
    }

    fn http(&self) -> Result<&reqwest::blocking::Client, CcdError> {
        self.http
            .as_ref()
            .ok_or_else(|| CcdError::Transport(CCD_NO_CLIENT.to_string()))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn send(&self, rb: reqwest::blocking::RequestBuilder) -> Result<String, CcdError> {
        let resp = rb
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .map_err(|e| CcdError::Transport(e.to_string()))?;
        let status = resp.status();
        let (bytes, over) = read_capped(resp, MAX_CCD_BODY_BYTES)
            .map_err(|e| CcdError::Transport(e.to_string()))?;
        if over && status.is_success() {
            return Err(CcdError::Transport(CCD_BODY_TOO_LARGE.to_string()));
        }
        let body = String::from_utf8_lossy(&bytes).into_owned();
        if !status.is_success() {
            return Err(CcdError::Http {
                status: status.as_u16(),
                body,
            });
        }
        Ok(body)
    }

    /// `POST /dispatch`, bounded by [`CcdTimeouts::dispatch`].
    pub fn dispatch(&self, req: &DispatchReq) -> Result<DispatchResp, CcdError> {
        let body = self.send(
            self.http()?
                .post(self.url("/dispatch"))
                .timeout(self.timeouts.dispatch)
                .json(req),
        )?;
        decode_dispatch(&body)
    }

    /// `GET /jobs/:id`, bounded by [`CcdTimeouts::request`].
    pub fn job(&self, id: &str) -> Result<Job, CcdError> {
        let seg = job_segment(id)?;
        let body = self.send(self.http()?.get(self.url(&format!("/jobs/{seg}"))))?;
        decode_job(&body)
    }

    /// `POST /jobs/:id/kill`, bounded by [`CcdTimeouts::request`].
    pub fn kill(&self, id: &str) -> Result<(), CcdError> {
        let seg = job_segment(id)?;
        self.send(self.http()?.post(self.url(&format!("/jobs/{seg}/kill"))))?;
        Ok(())
    }

    /// `POST /jobs/:id/verify` with a named profile, bounded by
    /// [`CcdTimeouts::verify`].
    pub fn verify(&self, id: &str, profile: &str) -> Result<VerifyOutcome, CcdError> {
        let seg = job_segment(id)?;
        let body = self.send(
            self.http()?
                .post(self.url(&format!("/jobs/{seg}/verify")))
                .timeout(self.timeouts.verify)
                .json(&serde_json::json!({ "verify_profile": profile })),
        )?;
        decode_verify(&body)
    }

    /// Poll `GET /jobs/:id` every `interval` until the job is terminal or
    /// `timeout` elapses. `cancel` is checked before every poll so the UI's
    /// Cancel button stops the wait promptly (it does NOT kill the job — call
    /// [`CcdClient::kill`] for that).
    pub fn poll_until_terminal(
        &self,
        id: &str,
        interval: Duration,
        timeout: Duration,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Job, CcdError> {
        let start = Instant::now();
        loop {
            if cancel() {
                return Err(CcdError::Cancelled);
            }
            let job = self.job(id)?;
            if job.state.is_terminal() {
                return Ok(job);
            }
            if start.elapsed() >= timeout {
                return Err(CcdError::PollTimeout {
                    id: id.to_string(),
                    secs: timeout.as_secs(),
                });
            }
            std::thread::sleep(interval);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_req_defaults_to_opus_no_profile() {
        let r = DispatchReq::new("/home/example/Projects/awili", "feat/x", "do the thing");
        assert_eq!(r.model, "claude-opus-4-8");
        assert!(r.verify_profile.is_none());
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("verify_profile"),
            "None profile must be omitted: {json}"
        );
    }

    #[test]
    fn dispatch_req_builders() {
        let r = DispatchReq::new("repo", "br", "spec")
            .model("deepseek-flash")
            .verify_profile("rust_check");
        assert_eq!(r.model, "deepseek-flash");
        assert_eq!(r.verify_profile.as_deref(), Some("rust_check"));
    }

    #[test]
    fn dispatch_req_verify_profile_serialized_when_some() {
        let r = DispatchReq::new("r", "b", "s").verify_profile("p");
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            json.contains(r#""verify_profile""#),
            "Some profile must be present: {json}"
        );
    }

    #[test]
    fn decode_dispatch_fixture() {
        let body = include_str!("../tests/fixtures/dispatch_ok.json");
        let r = decode_dispatch(body).unwrap();
        assert_eq!(r.id, "job-1781751122-1158946-18");
    }

    #[test]
    fn decode_job_working_fixture() {
        let body = include_str!("../tests/fixtures/job_running.json");
        let j = decode_job(body).unwrap();
        assert_eq!(j.state, JobState::Working);
        assert!(!j.state.is_terminal());
        assert_eq!(j.files_touched, 3);
        assert_eq!(j.id, "job-1781751122-1158946-18");
    }

    #[test]
    fn decode_job_verified_fixture() {
        let body = include_str!("../tests/fixtures/job_done.json");
        let j = decode_job(body).unwrap();
        assert_eq!(j.state, JobState::Verified);
        assert!(j.state.is_terminal());
        assert!(j.state.is_success());
        assert_eq!(j.exit_code, Some(0));
    }

    #[test]
    fn decode_job_dead_early_is_terminal_not_success() {
        let body = include_str!("../tests/fixtures/job_dead_early.json");
        let j = decode_job(body).unwrap();
        assert_eq!(j.state, JobState::DeadEarly);
        assert!(j.state.is_terminal(), "dead_early stops polling");
        assert!(!j.state.is_success(), "dead_early is NOT success");
    }

    #[test]
    fn decode_job_needs_attention_is_terminal_not_success() {
        let body = include_str!("../tests/fixtures/job_needs_attention.json");
        let j = decode_job(body).unwrap();
        assert_eq!(j.state, JobState::NeedsAttention);
        assert!(j.state.is_terminal(), "needs_attention stops polling");
        assert!(!j.state.is_success(), "needs_attention is NOT success");
    }

    #[test]
    fn decode_job_no_op_is_terminal_not_success() {
        let body = r#"{"job_id":"x","state":"no_op","files_touched":0}"#;
        let j = decode_job(body).unwrap();
        assert_eq!(j.state, JobState::NoOp);
        assert!(j.state.is_terminal(), "no_op stops polling");
        assert!(!j.state.is_success(), "no_op is NOT success");
    }

    #[test]
    fn decode_job_verify_failed_is_terminal_not_success() {
        let body = r#"{"job_id":"x","state":"verify_failed","files_touched":2}"#;
        let j = decode_job(body).unwrap();
        assert_eq!(j.state, JobState::VerifyFailed);
        assert!(j.state.is_terminal(), "verify_failed stops polling");
        assert!(!j.state.is_success(), "verify_failed is NOT success");
    }

    #[test]
    fn decode_job_unknown_state_does_not_crash() {
        let body = r#"{"job_id":"x","state":"some_future_state","files_touched":0}"#;
        let j = decode_job(body).unwrap();
        assert_eq!(j.state, JobState::Unknown);
        assert!(!j.state.is_terminal());
    }

    #[test]
    fn decode_verify_pass_and_fail() {
        let ok = decode_verify(include_str!("../tests/fixtures/verify_pass.json")).unwrap();
        assert!(ok.passed);
        let bad = decode_verify(include_str!("../tests/fixtures/verify_fail.json")).unwrap();
        assert!(!bad.passed);
        assert!(bad.detail.is_some());
    }

    // ---- Hardening: exhaustive negative-path and contract tests ----------------

    #[test]
    fn every_known_state_variant_decodes() {
        // Every variant ccd currently emits, plus Unknown for any future string.
        let cases: &[(&str, JobState)] = &[
            ("spawning", JobState::Spawning),
            ("starting", JobState::Starting),
            ("working", JobState::Working),
            ("exited", JobState::Exited),
            ("verified", JobState::Verified),
            ("no_op", JobState::NoOp),
            ("verify_failed", JobState::VerifyFailed),
            ("dead_early", JobState::DeadEarly),
            ("needs_attention", JobState::NeedsAttention),
        ];
        for (wire, want) in cases {
            let body = format!(r#"{{"job_id":"j","state":"{wire}","files_touched":0}}"#);
            let j =
                decode_job(&body).unwrap_or_else(|e| panic!("state '{wire}' should decode: {e}"));
            assert_eq!(j.state, *want, "state '{wire}' maps to wrong variant");
        }
    }

    #[test]
    fn unknown_future_state_surfaced_verbatim_no_panic() {
        let body = r#"{"job_id":"j","state":"quantum_paused","files_touched":0}"#;
        let j = decode_job(body).unwrap();
        assert_eq!(j.state, JobState::Unknown);
        assert!(!j.state.is_terminal());
        assert!(!j.state.is_success());
    }

    #[test]
    fn is_terminal_and_is_success_exhaustive() {
        let cases: &[(JobState, bool, bool)] = &[
            (JobState::Spawning, false, false),
            (JobState::Starting, false, false),
            (JobState::Working, false, false),
            (JobState::Exited, true, true),
            (JobState::Verified, true, true),
            (JobState::NoOp, true, false),
            (JobState::VerifyFailed, true, false),
            (JobState::DeadEarly, true, false),
            (JobState::NeedsAttention, true, false),
            (JobState::Unknown, false, false),
        ];
        for (i, (state, terminal, success)) in cases.iter().enumerate() {
            assert_eq!(
                state.is_terminal(),
                *terminal,
                "[{i}] {state:?}: is_terminal should be {terminal}",
            );
            assert_eq!(
                state.is_success(),
                *success,
                "[{i}] {state:?}: is_success should be {success}",
            );
        }
    }

    #[test]
    fn job_ignores_extra_unknown_fields() {
        let body = r#"{
            "job_id":"j","state":"working",
            "files_touched":1,"runtime_s":10,
            "ccd_version":"1.3","hostname":"node-7",
            "extra_config":{"foo":true}
        }"#;
        let j = decode_job(body).unwrap();
        assert_eq!(j.state, JobState::Working);
        assert_eq!(j.files_touched, 1);
        assert_eq!(j.runtime_s, 10);
        // Extra fields are silently ignored — no error, no panic.
    }

    #[test]
    fn job_minimal_fields_get_documented_defaults() {
        let body = r#"{"job_id":"j","state":"exited"}"#;
        let j = decode_job(body).unwrap();
        assert_eq!(j.id, "j");
        assert_eq!(j.state, JobState::Exited);
        assert_eq!(j.files_touched, 0, "default files_touched=0");
        assert_eq!(j.runtime_s, 0, "default runtime_s=0");
        assert_eq!(j.exit_code, None, "default exit_code=None");
        assert!(!j.needs_attention, "default needs_attention=false");
        assert_eq!(j.log_tail, None, "default log_tail=None");
        assert_eq!(j.repo, None, "default repo=None");
        assert_eq!(j.engine, None, "default engine=None");
    }

    #[test]
    fn job_needs_attention_and_engine_round_trip() {
        let body = r#"{
            "job_id":"j","state":"needs_attention",
            "needs_attention":true,
            "engine":"deepseek",
            "files_touched":0
        }"#;
        let j = decode_job(body).unwrap();
        assert_eq!(j.state, JobState::NeedsAttention);
        assert!(j.needs_attention, "needs_attention flag is true");
        assert_eq!(j.engine.as_deref(), Some("deepseek"), "engine round-trips");
    }

    #[test]
    fn poll_until_terminal_checks_cancel_before_polling() {
        // The Cancel button must stop the wait promptly: poll_until_terminal's
        // doc promises `cancel` is checked BEFORE every network poll. We point
        // the client at an unreachable endpoint and pre-cancel; a correct
        // implementation returns Cancelled WITHOUT ever touching the socket.
        // If the cancel check were reordered after the poll, `self.job(id)?`
        // would surface a Transport error (connection refused) first and
        // Cancelled would be unreachable — so this pins the ordering, not just
        // that Cancelled exists. Runs instantly: no network, no interval sleep
        // (the sleep is only reached after a non-terminal poll).
        let client = CcdClient::new("http://127.0.0.1:1", "tok");
        let always_cancel = || true;
        let err = client
            .poll_until_terminal(
                "any-id",
                Duration::from_secs(30),
                Duration::from_secs(30),
                &always_cancel,
            )
            .expect_err("a pre-cancelled poll must return an error, not a job");
        assert!(
            matches!(err, CcdError::Cancelled),
            "pre-cancel must short-circuit to Cancelled before any HTTP call, got: {err:?}",
        );
    }

    #[test]
    fn garbage_json_returns_typed_error_not_panic() {
        let cases = &[
            "",                   // empty
            "not json",           // plain text
            "{unquoted:true}",    // syntax error
            r#"{"job_id":null}"#, // valid json but wrong type for job_id
        ];
        for body in cases {
            let result = decode_job(body);
            match result {
                Err(CcdError::Decode(_)) => {} // expected
                Err(ref e) => panic!("garbage '{body}' got wrong error: {e}"),
                Ok(j) => panic!("garbage '{body}' unexpectedly decoded: {j:?}"),
            }
        }
    }
}
