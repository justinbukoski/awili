//! Probe executors (P0b): the single trait plus the two P0b implementations.
//!
//! P0b ships exactly two executors: [`ScriptedExecutor`] (canned answers
//! from a fixture TOML, for tests) and [`CommandExecutor`] (runs a shell
//! command template with `{prompt}` and/or `{prompt_id}` substituted,
//! shell-quoted, through `sh -c`; drives `claude -p`, `codex exec`,
//! `zcode --prompt` from the CLI). The Hermes/gateway executor is P0c.
//!
//! [`CommandExecutor`] captures stdout as [`Answer::text`] and stderr
//! alongside it; the runner appends the stderr to the evidence (it is never
//! part of `text`, which is what [`crate::evaluate::evaluate`] judges).
//!
//! Process safety (P0-weekend-repair.md items 1+2): on Unix each executor
//! leads its own process group; the deadline covers the child's lifetime
//! AND pipe completion (bounded nonblocking `poll` via nix), so a background
//! descendant holding the pipes cannot wedge the ask past the deadline;
//! on deadline or error the whole group is killed and the immediate child is
//! reaped. Deliberately detached descendants may escape group signalling,
//! but their pipe handles cannot extend the ask deadline. Diagnostics are redacted before truncation, and error messages
//! never include the expanded shell command (which would echo the prompt).

use serde::{Deserialize, Serialize};
use std::time::Duration;
#[cfg(unix)]
#[path = "executor_process.rs"]
mod process;
#[cfg(unix)]
use process::run_unix;

use crate::redact::redact;
use crate::report::clamp_evidence;

/// One answer from an executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// Final assistant text (stdout for [`CommandExecutor`]).
    pub text: String,
    /// The child's stderr, when captured; kept out of `text` so evaluation
    /// sees only the answer (the runner appends it to the evidence).
    pub stderr: String,
    /// Input tokens, when the executor reports them.
    pub tokens_in: Option<u64>,
    /// Output tokens, when the executor reports them.
    pub tokens_out: Option<u64>,
    /// Tool the answer came from, when known.
    pub tool_origin: Option<String>,
}

/// Anything that can answer a probe prompt.
pub trait ProbeExecutor {
    /// Ask `prompt`, waiting at most `timeout`; timeouts and spawn/execution
    /// failures are `Err` (the runner turns them into `Outcome::Error`).
    fn ask(&self, prompt: &str, timeout: Duration) -> anyhow::Result<Answer>;
}

/// A scripted answer from a fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptedAnswer {
    /// Exact prompt text this answer is keyed by.
    pub prompt: String,
    /// Scripted final assistant text.
    pub text: String,
    /// Scripted tool origin, when the fixture provides one.
    pub tool_origin: Option<String>,
}

/// A fixture of scripted answers, keyed by exact prompt text. Fixture format
/// (TOML): `[[answers]]` entries with `prompt`, `text`, and optional
/// `tool_origin`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScriptedExecutor {
    /// The scripted answers.
    pub answers: Vec<ScriptedAnswer>,
}

impl ScriptedExecutor {
    /// Parse a fixture from a TOML string.
    pub fn from_toml_str(toml: &str) -> Result<Self, String> {
        toml::from_str(toml).map_err(|e| format!("scripted fixture TOML: {e}"))
    }
}

impl ProbeExecutor for ScriptedExecutor {
    fn ask(&self, prompt: &str, _timeout: Duration) -> anyhow::Result<Answer> {
        let answer = self
            .answers
            .iter()
            .find(|a| a.prompt == prompt)
            .ok_or_else(|| anyhow::anyhow!("no scripted answer for prompt: {prompt}"))?;
        Ok(Answer {
            text: answer.text.clone(),
            stderr: String::new(),
            tokens_in: None,
            tokens_out: None,
            tool_origin: answer.tool_origin.clone(),
        })
    }
}

/// Runs a shell command template with `{prompt}` and/or `{prompt_id}`
/// substituted (shell-quoted) through `sh -c`, as the shell's exec target —
/// the spawned child IS the command, so a deadline kill stops the command
/// (not a wrapper shell) and the exit status is the shell's. stdout becomes
/// [`Answer::text`]; stderr goes to [`Answer::stderr`] for the evidence. A
/// child — or a background descendant holding the pipes — that outlives the
/// deadline is killed with the process group and the ask fails with
/// `"timeout"`. The template should be a single command (spec uses:
/// `claude -p {prompt}`, `codex exec …`, `cat …`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandExecutor {
    /// Shell template; `{prompt}`/`{prompt_id}` are shell-quoted and
    /// substituted before `sh -c` runs it (as its exec target).
    pub template: String,
    /// Max time this executor gives a single ask; the effective deadline is
    /// `min(this, the per-call timeout)`. `Duration::MAX` disables the cap
    /// so the per-probe manifest `timeout_s` governs.
    pub timeout: Duration,
}

impl CommandExecutor {
    /// Make one from a template string (e.g. `claude -p {prompt}`) and an
    /// overall ask timeout.
    pub fn new(template: impl Into<String>, timeout: Duration) -> Self {
        Self {
            template: template.into(),
            timeout,
        }
    }

    /// Ask with `{prompt_id}` substituted in addition to `{prompt}`: the
    /// probe id is shell-quoted independently (CLI path, item 5).
    pub fn ask_probe(
        &self,
        prompt: &str,
        probe_id: &str,
        timeout: Duration,
    ) -> anyhow::Result<Answer> {
        self.run(Some(prompt), Some(probe_id), timeout)
    }
}

impl ProbeExecutor for CommandExecutor {
    fn ask(&self, prompt: &str, timeout: Duration) -> anyhow::Result<Answer> {
        // Plain `ask` leaves `{prompt_id}` untouched (existing API); the
        // CLI uses [`ask_probe`](CommandExecutor::ask_probe).
        self.run(Some(prompt), None, timeout)
    }
}

/// Quote `value` for safe inclusion in a `sh -c` command line.
pub fn shell_quote(value: &str) -> String {
    // Standard POSIX single-quote escaping: wrap in ' and end-quote any
    // embedded single quote as '\'' .
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Render `template`, replacing `{prompt}` with `shell_quote(prompt)` and
/// `{prompt_id}` with `shell_quote(probe_id)` in a single left-to-right
/// pass. Substituted values are opaque (never re-substituted); a `None`
/// binding leaves its placeholder untouched.
pub fn render_template(template: &str, prompt: Option<&str>, probe_id: Option<&str>) -> String {
    const PROMPT: &str = "{prompt}";
    const PROMPT_ID: &str = "{prompt_id}";
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    loop {
        let (pos, token) = match (rest.find(PROMPT), rest.find(PROMPT_ID)) {
            (None, None) => break,
            (Some(a), Some(b)) if a < b => (a, PROMPT),
            (Some(_), Some(b)) => (b, PROMPT_ID),
            (Some(a), None) => (a, PROMPT),
            (None, Some(b)) => (b, PROMPT_ID),
        };
        out.push_str(&rest[..pos]);
        let value = if token == PROMPT_ID { probe_id } else { prompt };
        match value {
            Some(v) => out.push_str(&shell_quote(v)),
            None => out.push_str(token),
        }
        rest = &rest[pos + token.len()..];
    }
    out.push_str(rest);
    out
}

impl CommandExecutor {
    /// Spawn the rendered template and drive it to completion under
    /// `self.timeout.min(timeout)`.
    fn run(
        &self,
        prompt: Option<&str>,
        probe_id: Option<&str>,
        timeout: Duration,
    ) -> anyhow::Result<Answer> {
        // `exec` makes the child the command itself, so a deadline kill
        // stops its group rather than orphaning a pipe-holding grandchild.
        let substituted = render_template(&self.template, prompt, probe_id);
        let command = format!("exec {substituted}");
        let limit = self.timeout.min(timeout);
        #[cfg(unix)]
        let result = run_unix(&command, limit);
        #[cfg(not(unix))]
        let result = run_fallback(&command, limit);
        result
    }
}

/// Non-Unix hosts: the executor needs the Unix process-group + poll
/// primitives of P0-weekend-repair.md item 2 (mandated for macOS and
/// Linux); fail with an actionable error instead of a weaker kill.
#[cfg(not(unix))]
fn run_fallback(_command: &str, _limit: Duration) -> anyhow::Result<Answer> {
    Err(anyhow::anyhow!(
        "CommandExecutor requires a Unix host (process-group isolation, item 2 of \
         P0-weekend-repair.md)"
    ))
}

/// Build the ask result from a reaped child's status and captured streams.
fn finish(status: std::process::ExitStatus, out: &[u8], err: &[u8]) -> anyhow::Result<Answer> {
    if status.success() {
        Ok(Answer {
            text: lossy(out),
            stderr: lossy(err),
            tokens_in: None,
            tokens_out: None,
            tool_origin: None,
        })
    } else {
        let code = status
            .code()
            .map_or_else(|| "signalled".to_owned(), |c| c.to_string());
        // P0-weekend-repair.md item 1: redact BEFORE truncating so a secret
        // can never survive, split, in a tail-truncated diagnostic.
        let detail = clamp_evidence(&redact(&lossy(err)));
        Err(anyhow::anyhow!("command exited {code}: {detail}"))
    }
}

/// Lossy-UTF8 a captured stream for [`Answer`] fields and diagnostics.
fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
