//! `awili-code` — the engine-agnostic core of Awili's native Code mode.
//!
//! Code mode runs a coding-agent CLI (`claude` or `codex`) and, instead of
//! showing its raw terminal, renders the conversation natively. The two CLIs
//! emit different newline-delimited JSON event streams; this crate normalises
//! both onto one [`CodeEvent`] vocabulary and maps that onto the app's existing
//! [`ProviderEvent`] stream (the same one Chat/Work mode already render).
//!
//! Why this is its own crate, like `awili-providers`/`awili-ccd`: the decoders are
//! **pure** (a stream line in, normalised events out) and fixture-tested against
//! output captured verbatim from the real CLIs — no PTY, no GPUI, no network.
//! The process lifecycle (PTY spawn, stdin turns, resume) lives in the app layer
//! on top of this; the difference between Claude's persistent stdin session and
//! Codex's per-turn `exec` is a driver concern, not a parsing one.
//!
//! GROUND TRUTH (captured live 2026-06-24, official CLIs; confirmed against the
//! opcode/Claudia reference, which drives Claude the same way):
//!   - `claude -p <prompt> --output-format stream-json --verbose` authenticates
//!     on the Max subscription over **plain pipes — no PTY** (apiKeySource none,
//!     five-hour window; supersedes the stale "non-TTY → 401" of mem 1745 for
//!     CLI v2.1.170). Multi-turn continuity is `--resume <session_id>`, NOT a
//!     persistent stdin process — each turn is a fresh spawn. Events:
//!     `system`(init/hook), `assistant`(message.content[] text|tool_use),
//!     `user`(tool_result), `result`, `rate_limit_event`.
//!   - `codex exec --json --skip-git-repo-check` emits: `thread.started`,
//!     `turn.started`, `item.completed`(item.type agent_message|reasoning|
//!     command_execution|file_change|…), `turn.completed`, `turn.failed`/`error`.
//!     Continuity is `codex exec resume <thread_id>`. Same per-turn shape as
//!     Claude, so one driver lifecycle serves both.
//!   - RC2-CXS: Codex turns now run over `codex app-server` (JSON-RPC lines;
//!     the reply text streams as it is written) — [`app_server`]. `exec
//!     --json` stays the fallback when that Codex has no app server.

pub mod app_server;
pub mod stream;

use awili_providers::{ProviderEvent, Usage};
use serde_json::Value;

/// Executor-independent transcript context and CLI resume eligibility.
pub mod continuity;

mod denial;

/// Which coding-agent CLI a Code session runs. The UI toggle selects this; it
/// determines both how the process is launched and which parser decodes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeEngine {
    Claude,
    Codex,
    /// GLM via ZCode, Z.ai's official harness (executor-selection design of
    /// record 2026-08-22: each OAuth mode runs its own vendor binary; GLM
    /// through Anthropic's `claude` is prohibited).
    Glm,
    /// DeepSeek via the `dsh` harness (daily-driver P9): a first-class routine
    /// lane under the standing vendor-binary rule (2026-09-14) — each vendor is
    /// spent only through its own binary. Headless one-shot profile: NO
    /// session id, NO model flag, NO resume flag on the argv; the model travels
    /// in the `DSH_MODEL` environment variable and continuity travels in the
    /// prompt (the bounded context pack).
    Dsh,
}

impl CodeEngine {
    /// The chip label shown on the toggle.
    pub fn label(self) -> &'static str {
        match self {
            CodeEngine::Claude => "Claude",
            CodeEngine::Codex => "Codex",
            CodeEngine::Glm => "GLM",
            CodeEngine::Dsh => "DeepSeek",
        }
    }

    /// Whether this engine's harness ENFORCES the Ask planning step: an
    /// `AskFirst` turn here is made read-only by the vendor CLI itself (U4 D4),
    /// so nothing in the step can change anything without an approved
    /// follow-up.
    ///
    /// Probes of record (2026-09-29, live CLIs): Claude and Codex enforce it
    /// (Claude `--permission-mode plan` refuses tools that change things;
    /// Codex `exec --sandbox read-only` refused writes at the OS sandbox, and
    /// RC1-M's "writes" MCP approval refuses MCP tools that change things). ZCode's plan mode does NOT enforce it — a probe that asked the
    /// agent to call an MCP tool wrote a file through it (plan mode "was not
    /// enforced at the permission layer for this MCP tool"). dsh's
    /// `DSH_PERMISSION_MODE=read-only` preset was NOT probed (the live run
    /// needs a paid key), so it is treated as not enforceable until probed.
    pub fn enforces_ask(self) -> bool {
        matches!(self, CodeEngine::Claude | CodeEngine::Codex)
    }

    /// The CLI binary name (the driver resolves it to an absolute path).
    pub fn bin(self) -> &'static str {
        match self {
            CodeEngine::Claude => "claude",
            CodeEngine::Codex => "codex",
            // The /opt/homebrew/bin/zcode wrapper (bring-up addendum): runs the
            // app bundle's CLI under Electron-as-node and scrubs ANTHROPIC_*.
            CodeEngine::Glm => "zcode",
            CodeEngine::Dsh => "dsh",
        }
    }

    /// Whether this engine emits ONE document on stdout at end-of-turn rather
    /// than a line-per-event stream. The driver buffers the whole output and
    /// hands it to [`parse_whole`](Self::parse_whole) at EOF.
    pub fn buffers_whole_output(self) -> bool {
        matches!(self, CodeEngine::Glm | CodeEngine::Dsh)
    }

    /// The full argument vector (everything after the bin name) to run one turn:
    /// the static streaming flags, the resume token if continuing, the permission
    /// posture, and the user `prompt`. The driver spawns `bin()` with these over
    /// plain pipes (no PTY) and decodes stdout with [`parse_line`](Self::parse_line).
    ///
    /// Engine-specific assembly (verified against the real CLIs 2026-06-24):
    ///   - Claude: `-p --output-format stream-json --verbose
    ///     --include-partial-messages [--resume <id>] <permission…> <prompt>`.
    ///     `--verbose` is required with `--print` + stream-json; partial
    ///     messages stream the reply text (claude 2.1.282, 2026-10-06); the prompt is the trailing positional; `--resume` is a
    ///     plain option so order among the flags is free.
    ///   - Codex: `exec <sandbox…> [resume <id>] --json --skip-git-repo-check
    ///     <prompt>`. Resuming is the `exec resume <id>` subcommand form. The
    ///     permission/sandbox flags come right after `exec` and BEFORE `resume`:
    ///     codex-cli 0.155 rejects `--sandbox` after `resume` (exit 2, probed
    ///     2026-09-29), so a `--sandbox read-only` planning turn must have the
    ///     flag precede the subcommand.
    pub fn build_args(
        self,
        prompt: &str,
        autonomy: Autonomy,
        resume_id: Option<&str>,
        model: Option<&str>,
    ) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        match self {
            CodeEngine::Claude => {
                args.push("-p".to_string());
                // RC2-RUN (R3-20): `--include-partial-messages` streams the
                // reply text while it is written (`stream::StreamParser`).
                for f in [
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--include-partial-messages",
                ] {
                    args.push(f.to_string());
                }
                if let Some(id) = resume_id {
                    args.push("--resume".to_string());
                    args.push(id.to_string());
                }
                if let Some(m) = model {
                    args.push("--model".to_string());
                    args.push(m.to_string());
                }
                args.extend(self.permission_args(autonomy));
                args.push(prompt.to_string());
            }
            CodeEngine::Codex => {
                // The sandbox flag must come BEFORE `resume`: codex-cli 0.155
                // rejects `exec resume --sandbox …` (exit 2) but parses
                // `exec --sandbox … resume <id>` (probed 2026-09-29).
                args.push("exec".to_string());
                args.extend(self.permission_args(autonomy));
                if let Some(id) = resume_id {
                    args.push("resume".to_string());
                    args.push(id.to_string());
                }
                for f in ["--json", "--skip-git-repo-check"] {
                    args.push(f.to_string());
                }
                if let Some(m) = model {
                    args.push("-m".to_string());
                    args.push(m.to_string());
                }
                args.push(prompt.to_string());
            }
            // ZCode headless (verified live 2026-08-23 on 0.16.3): `--prompt`
            // + `--json` prints one JSON document with sessionId/response/usage
            // at end-of-turn; `--resume sess_…` continues a session. There is
            // NO per-run model mechanism: the model has no CLI flag and the
            // help-documented `--settings` is rejected by the parser (verified
            // — it fails the run with printed help). GLM runs on ZCode's own
            // configured model; `model` is deliberately ignored here.
            CodeEngine::Glm => {
                args.push("--prompt".to_string());
                args.push(prompt.to_string());
                args.push("--json".to_string());
                if let Some(id) = resume_id {
                    args.push("--resume".to_string());
                    args.push(id.to_string());
                }
                args.extend(self.permission_args(autonomy));
            }
            // dsh headless (verified 2026-09-18 on 0.1.5-rc.1): `dsh
            // --profile headless <task text>` answers one task and exits;
            // stdout is the final message only (plain text, no envelope).
            // `model` and `resume_id` are deliberately IGNORED on the argv:
            // the headless profile has neither a model flag nor a
            // resume/session flag (verified — there is nothing to pass).
            // The model travels in the `DSH_MODEL` environment variable (the
            // driver sets it from the picker's choice) and continuity
            // travels in the prompt (the bounded context pack), so a Dsh
            // conversation keeps its history without any session id. Never
            // pass `--resume` to dsh.
            CodeEngine::Dsh => {
                args.push("--profile".to_string());
                args.push("headless".to_string());
                args.push(prompt.to_string());
            }
        }
        args
    }

    /// RC2-CXS: the arguments that start Codex's app server for one turn
    /// (`app-server` over stdio). The posture is set on the command line —
    /// the same `-c` overrides the owner's config takes, so the sandbox
    /// stays visible on the argv — and again on the thread request
    /// ([`app_server`]): `sandbox_mode` read-only for the planning step,
    /// full access otherwise; autonomous approvals use Codex's automatic
    /// reviewer, while planning keeps `never`. Only Codex has one;
    /// every other engine gets an empty vector.
    pub fn app_server_args(self, autonomy: Autonomy) -> Vec<String> {
        if self != CodeEngine::Codex {
            return Vec::new();
        }
        vec![
            "app-server".to_string(),
            "-c".to_string(),
            format!("sandbox_mode=\"{}\"", app_server::sandbox_mode(autonomy)),
            "-c".to_string(),
            format!(
                "approval_policy=\"{}\"",
                app_server::approval_policy(autonomy)
            ),
            "-c".to_string(),
            format!(
                "approvals_reviewer=\"{}\"",
                app_server::approvals_reviewer(autonomy)
            ),
        ]
    }

    /// Decode a whole-output engine's single end-of-turn document: ZCode's
    /// `--json` envelope, or dsh's plain-text final message. Unparseable
    /// output becomes an [`Error`] carrying a bounded tail of what the
    /// process actually printed — auth failures and crashes surface as
    /// themselves, never as silence. Line-streaming engines never reach this
    /// (the driver calls it only for whole-output engines).
    pub fn parse_whole(self, output: &str) -> Vec<CodeEvent> {
        match self {
            CodeEngine::Glm => self.parse_whole_glm(output),
            // dsh headless prints exactly the final assistant message to
            // stdout (plain text, no envelope); the reasoning stream goes to
            // stderr and is never parsed here (spec P9 A.5). There is no
            // session id — never a `SessionStarted`.
            CodeEngine::Dsh => {
                let trimmed = output.trim();
                if trimmed.is_empty() {
                    // Fail closed: empty/whitespace-only stdout is a FAILED
                    // turn, never a clean empty answer.
                    return vec![CodeEvent::Error("dsh printed no final message".to_string())];
                }
                // Exactly one answer event (internal newlines preserved —
                // `trim` strips only the ends), then the terminal token.
                vec![
                    CodeEvent::AssistantText(trimmed.to_string()),
                    CodeEvent::TurnComplete,
                ]
            }
            CodeEngine::Claude | CodeEngine::Codex => Vec::new(),
        }
    }

    /// The ZCode `--json` envelope of a single end-of-turn document.
    /// Unparseable output becomes an [`Error`] carrying a bounded tail of
    /// what the process actually printed — auth failures and crashes
    /// surface as themselves, never as silence.
    fn parse_whole_glm(&self, output: &str) -> Vec<CodeEvent> {
        let trimmed = output.trim();
        let parsed = glm_envelope(trimmed);
        let Some(v) = parsed else {
            let tail: String = trimmed
                .chars()
                .rev()
                .take(400)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return vec![CodeEvent::Error(format!(
                "engine produced no parseable result: …{tail}"
            ))];
        };
        let mut events = Vec::new();
        if let Some(sid) = v.get("sessionId").and_then(|s| s.as_str()) {
            events.push(CodeEvent::SessionStarted {
                session_id: sid.to_string(),
            });
        }
        // STRICT validation (ocr/codex review R1): success requires a string
        // `response`. An explicit error field, or any envelope without one,
        // is an Error — a changed schema or error payload must never
        // masquerade as a clean empty run.
        if let Some(err) = v.get("error").and_then(|e| {
            e.as_str()
                .map(str::to_string)
                .or_else(|| Some(e.to_string()))
        }) {
            events.push(CodeEvent::Error(format!("engine reported: {err}")));
            events.push(CodeEvent::TurnComplete);
            return events;
        }
        match v.get("response").and_then(|s| s.as_str()) {
            Some(resp) => events.push(CodeEvent::AssistantText(resp.to_string())),
            None => {
                events.push(CodeEvent::Error(
                    "engine envelope has no response field — treating the turn as failed"
                        .to_string(),
                ));
                events.push(CodeEvent::TurnComplete);
                return events;
            }
        }
        if let Some(u) = v.get("usage") {
            events.push(CodeEvent::Usage {
                cached_input_tokens: None,
                input_tokens: u.get("inputTokens").and_then(|x| x.as_u64()).unwrap_or(0),
                output_tokens: u.get("outputTokens").and_then(|x| x.as_u64()).unwrap_or(0),
            });
        }
        events.push(CodeEvent::TurnComplete);
        events
    }

    /// The flags that set this turn's permission posture.
    ///
    /// CRITICAL (Owner, 2026-06-24): NEVER SANDBOX. Awili runs on the owner's own
    /// fleet; every turn that can change anything (Autonomous — the approved
    /// follow-up included) must have full filesystem AND network access — the
    /// network reach is what lets it use the Foreman memory service, the web,
    /// and the fleet. A network-blocking sandbox (codex `workspace-write`) is
    /// exactly the Claude-app friction Owner is leaving behind. The one
    /// exception is the read-only planning step (U4 D5): it changes nothing, so
    /// the vendor harness may hold it read-only.
    ///
    /// rc.4 S7 (owner request 2026-11-01): the one OPT-IN exception. With the
    /// setting `[work.sandbox] enabled` ON (default OFF), Awili starts the
    /// Autonomous process under `/usr/bin/sandbox-exec` (`awili-app`
    /// `agent_sandbox`): file WRITES are limited to the working folder, temp and
    /// the agent's own state; reads and the network stay fully open. The flags
    /// below are unchanged either way, and with the setting off the launch is
    /// byte-for-byte what it was.
    ///
    /// `Autonomous` runs with no prompts and full access —
    /// `--dangerously-skip-permissions` (Claude) /
    /// `--dangerously-bypass-approvals-and-sandbox` (Codex). `AskFirst` is the
    /// READ-ONLY PLANNING STEP of "Ask before changes": the vendor harness
    /// enforces the read-only posture itself (Claude plan mode, Codex read-only
    /// sandbox), so the planning step cannot change anything even if the agent
    /// tries. One approved plan releases one follow-up turn as Autonomous
    /// (U4 D3/D10).
    pub fn permission_args(self, autonomy: Autonomy) -> Vec<String> {
        let flags: &[&str] = match (self, autonomy) {
            (CodeEngine::Claude, Autonomy::Autonomous) => &["--dangerously-skip-permissions"],
            // The planning step (RC1-M): Claude Code's own plan mode with no
            // interactive prompts (`-p` is non-interactive), and every tool the owner has — no allowlist, no MCP
            // cut. Plan mode is what holds the step: read-only tools run,
            // anything that changes things (built-in or MCP) is refused until
            // the owner approves the plan (live 2026-10-03, claude 2.1.282: a
            // tool its server marks read-only ran, one that writes was denied
            // even when its server was allowed). The launch adds the
            // `--allowedTools=mcp__<server>` entries that let read-only MCP
            // tools run on every model.
            // Older installed CLIs reject `--permission-prompts none`. Plan
            // mode still enforces read-only; print mode cannot prompt.
            (CodeEngine::Claude, Autonomy::AskFirst) => &["--permission-mode", "plan"],
            (CodeEngine::Codex, Autonomy::Autonomous) => {
                &["--dangerously-bypass-approvals-and-sandbox"]
            }
            // The planning step (RC1-M): the owner's own Codex config and the
            // read-only sandbox. MCP tools run outside that sandbox, so the
            // launch also sets every MCP server's approval to "writes": a tool
            // its server marks read-only runs, any other one is refused (live
            // 2026-10-03, codex 0.159: "MCP tool call requires approval, but
            // approval policy is never").
            (CodeEngine::Codex, Autonomy::AskFirst) => &["--sandbox", "read-only"],
            // ZCode: yolo is its no-prompt full-access mode. AskFirst here is a
            // fallback only — the app never dispatches a planning step to an
            // engine whose `enforces_ask` is false; `--mode plan` is merely the
            // safer posture if a caller gets it wrong (ZCode's plan mode does
            // not stop its plugins, so this is not a real gate).
            (CodeEngine::Glm, Autonomy::Autonomous) => &["--mode", "yolo"],
            (CodeEngine::Glm, Autonomy::AskFirst) => &["--mode", "plan"],
            // dsh headless: there is NO permission/sandbox flag on the
            // headless profile (verified 2026-09-18) — nothing to pass in
            // either posture. Permission posture is governed by
            // `DSH_PERMISSION_MODE` in the child's environment, which Awili
            // does NOT set: the profile's own default stands.
            (CodeEngine::Dsh, _) => &[],
        };
        flags.iter().map(|s| s.to_string()).collect()
    }

    /// Decode one line of this engine's event stream into zero or more normalised
    /// [`CodeEvent`]s. A blank line, an unrecognised event, or an
    /// already-rendered-elsewhere event (hooks, rate-limit pings) yields an empty
    /// vec. A line that is not valid JSON also yields empty — a malformed frame
    /// must never panic or poison the session.
    pub fn parse_line(self, line: &str) -> Vec<CodeEvent> {
        self.parse_line_for_model(line, None)
    }

    /// [`Self::parse_line`] knowing the model the turn was asked to run, so a
    /// Claude result that lists several models (a helper on a bigger window)
    /// reports the window of the one that ran the turn.
    pub fn parse_line_for_model(self, line: &str, model: Option<&str>) -> Vec<CodeEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        match self {
            CodeEngine::Claude => parse_claude(&v, model),
            CodeEngine::Codex => parse_codex(&v),
            // Whole-output engine: per-line parsing yields nothing; the driver
            // buffers stdout and calls [`parse_whole`](Self::parse_whole).
            CodeEngine::Glm | CodeEngine::Dsh => Vec::new(),
        }
    }
}

/// The permission posture for a Code session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Autonomy {
    /// Full access with no prompts: the agent runs commands and edits files
    /// freely. Used for the Auto mode and for the approved plan follow-up
    /// (one approval releases one such turn, U4 D3/D10). The default: it's the
    /// owner's app on the owner's fleet — full access, no friction. Never
    /// sandboxed unless the owner opts in
    /// to the write-only shell sandbox (`[work.sandbox]`, rc.4 S7).
    #[default]
    Autonomous,
    /// The READ-ONLY PLANNING STEP of "Ask before changes". The vendor harness
    /// enforces the read-only posture itself (RC1-M: Claude plan mode; Codex
    /// `read-only` sandbox with "writes" MCP approval), so the step can read,
    /// search, fetch and use read-only tools but change nothing.
    /// The turn ends with a plan when changes are needed; one approval
    /// releases one Autonomous follow-up turn (U4 D3/D10).
    AskFirst,
}

impl Autonomy {
    /// Whether this posture is the read-only planning step — the driver
    /// prepends [`PLAN_FIRST_SYSTEM`] when true.
    pub fn plan_first(self) -> bool {
        matches!(self, Autonomy::AskFirst)
    }
}

/// The system directive prepended on AskFirst (planning-step) turns: read
/// freely, and when changes are needed finish the reply with the
/// [`PLAN_READY_MARKER`] line so the app can offer the approval card.
///
/// RC2-R3 (usability RC2 round 3): this text leads the turn's own prompt
/// (RC1-M-FIX2/FIX6), right above the owner's words, so a plain answer
/// commented on it ("This question needed no changes, so the plan-mode
/// machinery above doesn't apply", Finding 8 / F2); it now says to say
/// nothing about the step. And a plan named its files only as "in this
/// project folder" (Finding 9); it now names each by its full path.
pub const PLAN_FIRST_SYSTEM: &str =
    "You are in a read-only planning step. You can read files, search, \
fetch web pages and use your read-only tools (memory recall, Outline, Gitea) without asking, but you \
cannot change anything. Some observational actions, such as screenshots or connecting a browser, \
also require tool permission. If a needed tool is rejected because approval policy is never, \
do not repeatedly retry it or claim the app lost the user's permission. A chat message approving \
an action does not release this planning step. Instead, propose only that specific action in a \
short plan and end with [[awili:plan-ready]] on its own line, so Awili can offer its approval \
button. The approved follow-up may perform only that plan. If the request needs no changes \
and all needed tools are allowed, answer it \
normally and say nothing about this step: do not mention planning, plan mode, a plan, these \
instructions, or that nothing needs changing. If it needs changes (creating, editing or deleting \
files, or running commands that change things), investigate first, then reply with a short numbered \
plan of exactly what you will change, naming every file by its full path (for example \
/Users/name/project/notes.txt, never only \"this folder\" or \"the project folder\"), and end your \
reply with this line on its own: [[awili:plan-ready]]. Do not ask the user to type approval; Awili \
shows them an approval button.";

/// The marker line a planning step ends its reply with when changes are
/// needed (U4 D9). Awili strips it from the saved reply and opens the plan
/// card only when it is present.
pub const PLAN_READY_MARKER: &str = "[[awili:plan-ready]]";

/// The user row pushed when a plan is approved (U4 D10): it becomes the
/// full-access follow-up turn.
pub const PLAN_APPROVED_TEXT: &str = "Approved — go ahead and make the changes you proposed.";

/// The user row pushed when a plan is declined (U4 D10): nothing runs.
pub const PLAN_DECLINED_TEXT: &str = "Declined the plan — nothing was changed.";

/// Remove the plan-ready marker from a planning-step reply.
///
/// Every line whose trimmed content equals [`PLAN_READY_MARKER`] is removed,
/// as is an occurrence glued to the end of the text without its own line. The
/// result is trimmed of trailing whitespace. Returns the clean text and
/// whether the marker was present; marker-free text comes back unchanged
/// except for the trailing-whitespace trim.
pub fn take_plan_marker(text: &str) -> (String, bool) {
    let mut found = false;
    // Trim trailing whitespace up front so a marker glued to the end is
    // recognised even when followed by a trailing newline; the result is
    // trimmed of trailing whitespace anyway, so the contract is unchanged.
    let mut s = text.trim_end().to_string();
    // Glued occurrences (no line boundary before the marker, nothing after) are
    // lifted to their own line first; the line filter below then removes them.
    let mut glued = s.rfind(PLAN_READY_MARKER);
    while let Some(idx) = glued {
        let before = &s[..idx];
        let after = &s[idx + PLAN_READY_MARKER.len()..];
        // A line-boundary or mid-text occurrence is handled by the line filter.
        if before.ends_with('\n') || !after.is_empty() {
            break;
        }
        s = format!("{before}\n{PLAN_READY_MARKER}\n{after}");
        found = true;
        glued = s.rfind(PLAN_READY_MARKER);
    }
    let mut kept: Vec<String> = Vec::new();
    for raw_line in s.split('\n') {
        let trimmed = raw_line.trim();
        if trimmed == PLAN_READY_MARKER {
            found = true;
            continue;
        }
        // A lone `\r` (CRLF line ending) is preserved on the next line.
        let (line, crlf) = match raw_line.strip_suffix('\r') {
            Some(l) => (l, true),
            None => (raw_line, false),
        };
        kept.push(if crlf {
            format!("{line}\r")
        } else {
            line.to_string()
        });
    }
    // Drop the final dangling "\r" left by the removed trailing marker line.
    if let Some(last) = kept.last_mut() {
        if last.ends_with('\r') && s.ends_with('\n') {
            last.pop();
        }
    }
    (kept.join("\n").trim_end().to_string(), found)
}

/// What of a reply may be shown (R1b): `text` with every
/// [`PLAN_READY_MARKER`] removed and, when `cut_tail` is true, also a start
/// of the marker at the very end ("…[[awili:pla"): a reply that is still
/// streaming, or was stopped or failed part-way, can end inside the marker.
/// Nothing else is trimmed or moved, and nothing is copied when nothing is
/// hidden. [`take_plan_marker`] stays the settle path that decides whether
/// the marker was there.
pub fn hide_plan_marker(text: &str, cut_tail: bool) -> std::borrow::Cow<'_, str> {
    let mut shown = std::borrow::Cow::Borrowed(text);
    while shown.contains(PLAN_READY_MARKER) {
        shown = std::borrow::Cow::Owned(shown.replace(PLAN_READY_MARKER, ""));
    }
    if cut_tail {
        let tail = (1..PLAN_READY_MARKER.len())
            .rev()
            .find(|&n| shown.ends_with(&PLAN_READY_MARKER[..n]))
            .unwrap_or(0);
        if tail > 0 {
            let keep = shown.len() - tail;
            shown = std::borrow::Cow::Owned(shown[..keep].to_string());
        }
    }
    shown
}

/// A normalised event from either engine. Richer than [`ProviderEvent`] on
/// purpose: it preserves tool/command/file structure so a future card-based
/// renderer can use it, even though today's mapping flattens most of it to text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeEvent {
    /// The context window the provider itself reported for the model that ran
    /// the turn (Claude `modelUsage[..].contextWindow`, Codex
    /// `model_context_window`), in tokens.
    ContextWindow(u64),
    /// The engine reported its session/thread id (for resume + display).
    SessionStarted { session_id: String },
    /// A chunk (or whole block) of assistant-visible text.
    AssistantText(String),
    /// Model reasoning/thinking, kept separate so it can be dimmed or hidden.
    Reasoning(String),
    /// The agent is about to run a tool/command/edit. `label` is human-readable
    /// (e.g. `Bash: cargo test`, `Edit src/main.rs`).
    ToolStarted { label: String },
    /// A tool/command/edit finished. `ok=false` marks a non-zero exit / error.
    ToolFinished { label: String, ok: bool },
    /// Bounded, nonfatal diagnostic activity from the harness.
    Diagnostic(String),
    /// End-of-turn token accounting. For Claude, `input_tokens` already includes
    /// the separately reported cache reads and writes (`cache_read_input_tokens`
    /// + `cache_creation_input_tokens`), folded in by the parser (P8c).
    Usage {
        cached_input_tokens: Option<u64>,
        input_tokens: u64,
        output_tokens: u64,
    },
    /// The turn finished cleanly.
    TurnComplete,
    /// A fatal error from the engine.
    Error(String),
}

impl CodeEvent {
    /// Map onto the app's [`ProviderEvent`] vocabulary for the native transcript.
    ///
    /// This is the MVP flattening: assistant text streams as tokens, and tool
    /// activity renders as readable bullet lines rather than dedicated cards
    /// (cards are a later pass — the structure is preserved on [`CodeEvent`] for
    /// when that lands). `SessionStarted` and `Reasoning` carry no
    /// `ProviderEvent` today (session id is consumed by the driver; reasoning is
    /// hidden to keep the transcript clean) and map to `None`.
    pub fn to_provider_event(&self) -> Option<ProviderEvent> {
        match self {
            CodeEvent::AssistantText(s) => Some(ProviderEvent::Token(s.clone())),
            CodeEvent::Diagnostic(s) => Some(ProviderEvent::Activity(s.clone())),
            // RC2-R2G8: a tool that starts is its own event, not a bare line.
            CodeEvent::ToolStarted { label } => Some(ProviderEvent::ToolStarted(label.clone())),
            CodeEvent::ToolFinished { label, ok } => {
                if *ok {
                    Some(ProviderEvent::Activity(format!("\u{2713} {label}")))
                } else {
                    Some(ProviderEvent::ToolAttemptFailed)
                }
            }
            CodeEvent::Usage {
                cached_input_tokens,
                input_tokens,
                output_tokens,
            } => Some(ProviderEvent::Usage(Usage {
                cached_input_tokens: cached_input_tokens.map(clamp_u32),
                input_tokens: clamp_u32(*input_tokens),
                output_tokens: clamp_u32(*output_tokens),
            })),
            CodeEvent::ContextWindow(w) => Some(ProviderEvent::ContextWindow(clamp_u32(*w))),
            CodeEvent::TurnComplete => Some(ProviderEvent::Done),
            CodeEvent::Error(e) => Some(ProviderEvent::Error(e.clone())),
            CodeEvent::SessionStarted { .. } | CodeEvent::Reasoning(_) => None,
        }
    }
}

/// Saturating `u64 -> u32` for token counts (mirrors `awili-providers`): a bad or
/// oversized upstream count clamps rather than wrapping and under-reporting cost.
fn clamp_u32(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Best-effort one-line summary of a Claude `tool_use` block.
pub(crate) fn claude_tool_label(name: &str, input: &Value) -> String {
    // The high-signal tools get a tailored summary; everything else shows its
    // name, which is still far more legible than raw terminal output.
    match name {
        "Bash" => input
            .get("command")
            .and_then(Value::as_str)
            .map(|c| format!("Bash: {}", first_line(c)))
            .unwrap_or_else(|| "Bash".to_string()),
        // RC2-FIXB1 (RC2-UX review P2): `MultiEdit` changes a file too.
        "Read" | "Edit" | "Write" | "MultiEdit" => input
            .get("file_path")
            .and_then(Value::as_str)
            .map(|p| format!("{name} {p}"))
            .unwrap_or_else(|| name.to_string()),
        // RC2-FIXB1: and `NotebookEdit` names its notebook.
        "NotebookEdit" => input
            .get("notebook_path")
            .and_then(Value::as_str)
            .map(|p| format!("{name} {p}"))
            .unwrap_or_else(|| name.to_string()),
        "Grep" | "Glob" => input
            .get("pattern")
            .and_then(Value::as_str)
            .map(|p| format!("{name} {p}"))
            .unwrap_or_else(|| name.to_string()),
        _ => name.to_string(),
    }
}

/// RC2-R2J (RC2-R2F review P3 on 1366a0c0): a Codex MCP tool call
/// (`server` and `tool`, on both the app server and `exec --json`), named
/// as "server: tool".
fn codex_mcp_label(item: &Value) -> String {
    let field = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or("");
    match (field("server"), field("tool")) {
        ("", "") => "tool".to_string(),
        ("", tool) => tool.to_string(),
        (server, "") => server.to_string(),
        (server, tool) => format!("{server}: {tool}"),
    }
}

/// RC2-R2K (RC2-R2F review P3 on 55d863a3): a Codex call to another agent
/// (`tool`, e.g. `spawn_agent`, on both routes), named as "agent: tool".
fn codex_agent_label(item: &Value) -> String {
    match item.get("tool").and_then(Value::as_str) {
        Some(t) if !t.is_empty() => format!("agent: {t}"),
        _ => "agent".to_string(),
    }
}

/// RC2-R2J: a Codex web search, with what it searched for.
fn codex_search_label(item: &Value) -> String {
    match item.get("query").and_then(Value::as_str).map(first_line) {
        Some(q) if !q.is_empty() => format!("web search: {q}"),
        _ => "web search".to_string(),
    }
}

/// First line of a string, trimmed and length-capped, for one-line labels.
fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() > 80 {
        let truncated: String = line.chars().take(79).collect();
        format!("{truncated}…")
    } else {
        line.to_string()
    }
}

/// RC2-R63 (usability RC2 round 5, Minor 3: "Used: $ /bin/zsh -lc 'ls -ld
/// …", "Used: Bash: ls -ld …", "✓ Bash: mv -n alpha.txt beta.txt && ls -la"):
/// how a step reads where the owner sees it. A command step is the command
/// itself — no "Bash:" or "$" in front, no shell wrapper around it
/// (`/bin/zsh -lc '…'`, `bash -c "…"`), one line, at most
/// [`PLAIN_STEP_CHARS`] characters. A mark in front ("✓ ", "⚠ outside
/// project · ") stays; every other step is unchanged. The step as the
/// engine reported it is kept as it is (footers read it); only what is shown
/// changes.
pub fn plain_step(line: &str) -> String {
    const MARKS: [&str; 2] = ["\u{26A0} outside project \u{B7} ", "\u{2713} "];
    let mut rest = line;
    let mut lead = String::new();
    while let Some(m) = MARKS.iter().find(|m| rest.starts_with(**m)) {
        lead.push_str(m);
        rest = &rest[m.len()..];
    }
    let command = rest
        .strip_prefix("Bash: ")
        .or_else(|| rest.strip_prefix("$ "));
    match command {
        Some(cmd) => format!("{lead}{}", shorten_step(&unwrap_shell(cmd))),
        None => line.to_string(),
    }
}

/// RC2-R63: the most characters a shown command step keeps.
pub const PLAIN_STEP_CHARS: usize = 60;

/// RC2-R63: `cmd` without a shell wrapper: `zsh -lc 'X'`, `/bin/bash -c "X"`,
/// `sh -c X` give `X` (unquoted; an unclosed quote — the engine already cut
/// the line — keeps what is there). Anything else comes back as it is.
fn unwrap_shell(cmd: &str) -> String {
    let cmd = cmd.trim();
    let mut words = cmd.splitn(3, ' ');
    let (Some(shell), Some(flag), Some(script)) = (words.next(), words.next(), words.next()) else {
        return cmd.to_string();
    };
    let shell_name = shell.rsplit('/').next().unwrap_or(shell);
    let is_shell = matches!(shell_name, "zsh" | "bash" | "sh" | "dash" | "fish");
    let runs_script = flag.len() > 1
        && flag.starts_with('-')
        && flag[1..].chars().all(|c| c.is_ascii_alphabetic())
        && flag.ends_with('c');
    if !is_shell || !runs_script {
        return cmd.to_string();
    }
    let script = script.trim().trim_end_matches('\u{2026}');
    if let Some(inner) = script.strip_prefix('\'') {
        let inner = inner.strip_suffix('\'').unwrap_or(inner);
        return inner.replace("'\\''", "'");
    }
    if let Some(inner) = script.strip_prefix('"') {
        let inner = inner.strip_suffix('"').unwrap_or(inner);
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(&next) = chars.peek() {
                    if matches!(next, '"' | '\\' | '$' | '`') {
                        out.push(next);
                        chars.next();
                        continue;
                    }
                }
            }
            out.push(c);
        }
        return out;
    }
    script.to_string()
}

/// RC2-R63: the first line of `cmd`, cut to [`PLAIN_STEP_CHARS`] with "…".
fn shorten_step(cmd: &str) -> String {
    let line = cmd.lines().next().unwrap_or("").trim();
    let line = line.trim_end_matches('\u{2026}').trim_end();
    if line.chars().count() > PLAIN_STEP_CHARS {
        let cut: String = line.chars().take(PLAIN_STEP_CHARS - 1).collect();
        format!("{}\u{2026}", cut.trim_end())
    } else if cmd.lines().nth(1).is_some() || cmd.trim_end().ends_with('\u{2026}') {
        format!("{line}\u{2026}")
    } else {
        line.to_string()
    }
}

/// Decode one Claude `stream-json` event object.
fn parse_claude(v: &Value, model: Option<&str>) -> Vec<CodeEvent> {
    match v.get("type").and_then(Value::as_str) {
        Some("system") => {
            // Only the `init` subtype carries the session id; hook events and the
            // rest are rendered elsewhere / ignored.
            if v.get("subtype").and_then(Value::as_str) == Some("init") {
                if let Some(id) = v.get("session_id").and_then(Value::as_str) {
                    return vec![CodeEvent::SessionStarted {
                        session_id: id.to_string(),
                    }];
                }
            }
            Vec::new()
        }
        Some("assistant") => {
            let mut out = Vec::new();
            if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = b.get("text").and_then(Value::as_str) {
                                if !t.is_empty() {
                                    out.push(CodeEvent::AssistantText(t.to_string()));
                                }
                            }
                        }
                        Some("thinking") => {
                            if let Some(t) = b.get("thinking").and_then(Value::as_str) {
                                out.push(CodeEvent::Reasoning(t.to_string()));
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                            let empty = Value::Null;
                            let input = b.get("input").unwrap_or(&empty);
                            out.push(CodeEvent::ToolStarted {
                                label: claude_tool_label(name, input),
                            });
                        }
                        _ => {}
                    }
                }
            }
            out
        }
        Some("user") => {
            // Tool results come back on a synthetic user turn. We mark completion;
            // the detailed output is intentionally not dumped into the transcript.
            let mut out = Vec::new();
            if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
                for b in blocks {
                    if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                        let is_err = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                        out.push(CodeEvent::ToolFinished {
                            label: "done".to_string(),
                            ok: !is_err,
                        });
                        // A failed attempt is not a terminal harness failure:
                        // Claude can correct arguments and retry. Keep it as a
                        // typed warning without copying arbitrary tool output.
                        // The result envelope below determines terminal failure;
                        // historical denials alone need not make success fatal.
                    }
                }
            }
            out
        }
        Some("result") => {
            let mut out = Vec::new();
            if let Some(u) = v.get("usage") {
                out.push(claude_usage_event(u));
            }
            if let Some(w) = claude_reported_window(v, model) {
                out.push(CodeEvent::ContextWindow(w));
            }
            // Require affirmative terminal metadata before treating denials as
            // historical. Assistant/result prose is never recovery evidence.
            let has_permission_denials = v
                .get("permission_denials")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty());
            let explicit_error = v.get("is_error").and_then(Value::as_bool);
            let subtype = v.get("subtype").and_then(Value::as_str);
            let confirmed_success = explicit_error == Some(false) && subtype == Some("success");
            // RC2-SVC5 (F0375): success needs an affirmative terminal field —
            // a result with neither `is_error` nor `subtype` is a failed turn,
            // never a silent success.
            let unmarked = explicit_error.is_none() && subtype.is_none();
            let is_error = explicit_error == Some(true)
                || subtype.is_some_and(|s| s != "success")
                || (has_permission_denials && !confirmed_success)
                || unmarked;
            if is_error {
                let msg = if has_permission_denials {
                    denial::claude_permission_denial(v)
                } else if unmarked {
                    "the coding engine ended without saying whether the turn worked — \
                     treating it as failed"
                        .to_string()
                } else {
                    v.get("result")
                        .and_then(Value::as_str)
                        .unwrap_or("the coding engine reported an error")
                        .to_string()
                };
                out.push(CodeEvent::Error(msg));
            } else {
                if has_permission_denials {
                    out.push(CodeEvent::ToolFinished {
                        label: "earlier permission denial".into(),
                        ok: false,
                    });
                    out.push(CodeEvent::Diagnostic(denial::claude_historical_denial(v)));
                }
                out.push(CodeEvent::TurnComplete);
            }
            out
        }
        _ => Vec::new(),
    }
}

/// RC2-SVC5 (F0376): find ZCode's `--json` envelope in whole output that may
/// carry log text BEFORE and AFTER it. Every top-level JSON object is read
/// with a streaming parser (trailing text no longer rejects a valid object);
/// the envelope is the LAST top-level object carrying an envelope key
/// (`response`, `error` or `sessionId`), else the last top-level object.
///
/// RC2-SVCF1: a JSON LOG RECORD is never the envelope, even when it carries
/// an `error` key (`{"level":"error","error":"cleanup"}` after a good
/// envelope must not turn the turn into a failure). A log record is an object
/// with a `level` or `severity` key and neither `response` nor `sessionId`;
/// it is skipped as an envelope and as the fallback, so output made only of
/// log records surfaces as "no parseable result" with what was printed.
fn glm_envelope(text: &str) -> Option<Value> {
    let mut last_object = None;
    let mut last_envelope = None;
    let mut i = 0;
    while let Some(rel) = text[i..].find('{') {
        let start = i + rel;
        let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(v)) if v.is_object() => {
                // Skip past this whole object: its inner `{`s are not
                // top-level candidates.
                i = start + stream.byte_offset();
                if glm_log_record(&v) {
                    continue;
                }
                if ["response", "error", "sessionId"]
                    .iter()
                    .any(|k| v.get(k).is_some())
                {
                    last_envelope = Some(v.clone());
                }
                last_object = Some(v);
            }
            _ => i = start + 1,
        }
    }
    last_envelope.or(last_object)
}

/// RC2-SVCF1: true for a JSON log record (a `level` or `severity` key, and
/// neither of the result-only keys `response` / `sessionId`). ZCode's result
/// envelope never carries a log level; a logger line always does. A level
/// counts only as a string or a number (RC2-SVCG: `{"level":null,"error":…}`
/// is an error envelope, not a log line).
fn glm_log_record(v: &Value) -> bool {
    let has = |k: &str| v.get(k).is_some();
    let level = |k: &str| v.get(k).is_some_and(|x| x.is_string() || x.is_number());
    (level("level") || level("severity")) && !has("response") && !has("sessionId")
}

/// Decode one Codex `exec --json` event object.
fn parse_codex(v: &Value) -> Vec<CodeEvent> {
    if let Some(w) = codex_reported_window(v) {
        return vec![CodeEvent::ContextWindow(w)];
    }
    match v.get("type").and_then(Value::as_str) {
        Some("thread.started") => v
            .get("thread_id")
            .and_then(Value::as_str)
            .map(|id| {
                vec![CodeEvent::SessionStarted {
                    session_id: id.to_string(),
                }]
            })
            .unwrap_or_default(),
        // RC2-R2J (RC2-R2F review P3 on 1366a0c0): a command or MCP tool
        // that starts is said at once, as over the app server, so a council
        // card can name it.
        Some("item.started") => {
            let Some(item) = v.get("item") else {
                return Vec::new();
            };
            let label = match item.get("type").and_then(Value::as_str) {
                Some("command_execution") => {
                    let cmd = item
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or("command");
                    format!("$ {}", first_line(cmd))
                }
                Some("mcp_tool_call") => codex_mcp_label(item),
                // RC2-R2K: a call to another agent.
                Some("collab_tool_call") => codex_agent_label(item),
                _ => return Vec::new(),
            };
            vec![CodeEvent::ToolStarted { label }]
        }
        Some("item.completed") => {
            let Some(item) = v.get("item") else {
                return Vec::new();
            };
            match item.get("type").and_then(Value::as_str) {
                // RC2-R2J: a search is named when it ends (its query is
                // filled in then).
                Some("web_search") => vec![CodeEvent::ToolStarted {
                    label: codex_search_label(item),
                }],
                Some("agent_message") => item
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                    .map(|t| vec![CodeEvent::AssistantText(t.to_string())])
                    .unwrap_or_default(),
                Some("reasoning") => item
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|t| vec![CodeEvent::Reasoning(t.to_string())])
                    .unwrap_or_default(),
                Some("command_execution") => {
                    let cmd = item
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or("command");
                    let ok = item
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .map(|c| c == 0)
                        .unwrap_or(true);
                    vec![CodeEvent::ToolFinished {
                        label: format!("$ {}", first_line(cmd)),
                        ok,
                    }]
                }
                Some("file_change") => {
                    // RC2-FIXB1 (RC2-UX review P2): `codex exec --json` lists
                    // the files in `changes` (`{path, kind}`) with one
                    // `status` (`completed` / `failed`); a failed patch
                    // changed nothing. An older single `path` still reads.
                    let ok = item
                        .get("status")
                        .and_then(Value::as_str)
                        .is_none_or(|s| s == "completed");
                    let mut paths: Vec<&str> = item
                        .get("changes")
                        .and_then(Value::as_array)
                        .map(|changes| {
                            changes
                                .iter()
                                .filter_map(|c| c.get("path").and_then(Value::as_str))
                                .collect()
                        })
                        .unwrap_or_default();
                    if paths.is_empty() {
                        paths.push(
                            item.get("path")
                                .and_then(Value::as_str)
                                .or_else(|| item.get("file").and_then(Value::as_str))
                                .unwrap_or("file"),
                        );
                    }
                    // RC2-FIXC3 (RC2-FIXB review P3: a failed change named no
                    // file, since a failed step's label is not shown): a
                    // failed change first names each file it tried, as a
                    // step that started does ("edit <path>", no ✓), so the
                    // footer can say "tried to edit". RC2-R2J (RC2-R2F review
                    // P3 on 1366a0c0): a change that worked is named as it
                    // starts too (as over the app server), for a council card.
                    paths
                        .into_iter()
                        .flat_map(|path| {
                            let label = format!("edit {path}");
                            let started = CodeEvent::ToolStarted {
                                label: label.clone(),
                            };
                            [started, CodeEvent::ToolFinished { label, ok }]
                        })
                        .collect()
                }
                _ => Vec::new(),
            }
        }
        Some("turn.completed") => {
            let mut out = Vec::new();
            if let Some(u) = v.get("usage") {
                out.push(usage_event(u, "input_tokens", "output_tokens"));
            }
            out.push(CodeEvent::TurnComplete);
            out
        }
        Some("turn.failed") | Some("error") => {
            let msg = v
                .pointer("/error/message")
                .or_else(|| v.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("the coding engine reported an error")
                .to_string();
            vec![CodeEvent::Error(msg)]
        }
        _ => Vec::new(),
    }
}

/// Build a [`CodeEvent::Usage`] from a usage object, reading the two named token
/// fields (absent fields default to 0, per the best-effort usage contract).
fn usage_event(u: &Value, in_key: &str, out_key: &str) -> CodeEvent {
    CodeEvent::Usage {
        cached_input_tokens: u.get("cached_input_tokens").and_then(Value::as_u64),
        input_tokens: u.get(in_key).and_then(Value::as_u64).unwrap_or(0),
        output_tokens: u.get(out_key).and_then(Value::as_u64).unwrap_or(0),
    }
}

/// The context window a Claude `result` event reports for the model that ran
/// the turn: the `modelUsage` entry matching `model` (its id, or an alias such
/// as "haiku" inside the id). When no entry can be matched, the SMALLEST
/// reported window, so a helper model on a bigger window never overstates it.
fn claude_reported_window(v: &Value, model: Option<&str>) -> Option<u64> {
    let usage = v.get("modelUsage")?.as_object()?;
    let window = |m: &Value| {
        m.get("contextWindow")
            .and_then(Value::as_u64)
            .filter(|w| *w > 0)
    };
    let wanted = model
        .map(|m| m.trim().to_ascii_lowercase())
        .filter(|m| !m.is_empty() && m != "default");
    if let Some(wanted) = wanted {
        let hit = usage
            .iter()
            .filter(|(k, _)| k.to_ascii_lowercase().contains(&wanted))
            .filter_map(|(_, m)| window(m))
            .min();
        if hit.is_some() {
            return hit;
        }
    }
    usage.values().filter_map(window).min()
}

/// The window a Codex `token_count` event reports (`info.model_context_window`,
/// bare or wrapped as a rollout line's `payload` or an event's `msg`).
fn codex_reported_window(v: &Value) -> Option<u64> {
    ["/info", "/payload/info", "/msg/info"]
        .iter()
        .find_map(|p| v.pointer(&format!("{p}/model_context_window"))?.as_u64())
        .filter(|w| *w > 0)
}

/// Build a [`CodeEvent::Usage`] from a Claude `result` usage object, folding the
/// separately reported cache reads and writes into `input_tokens` (P8c) so the
/// graduation ledger counts cached input. Absent or non-number fields default to
/// 0; the sum saturates rather than wrapping.
fn claude_usage_event(u: &Value) -> CodeEvent {
    let read = |key: &str| u.get(key).and_then(Value::as_u64).unwrap_or(0);
    CodeEvent::Usage {
        cached_input_tokens: u.get("cache_read_input_tokens").and_then(Value::as_u64),
        input_tokens: read("input_tokens")
            .saturating_add(read("cache_read_input_tokens"))
            .saturating_add(read("cache_creation_input_tokens")),
        output_tokens: read("output_tokens"),
    }
}

#[cfg(test)]
mod u4a_tests {
    //! U4a-FIX1 proof tests: the Codex `resume` argv order (permission flags
    //! before the subcommand) and the glued plan-ready marker. The full U4a
    //! suite (allowlisted read-only planning argv, enforces_ask, marker
    //! stripping) lives in `tests/u4a_ask.rs`.

    use super::*;

    #[test]
    fn u4a_codex_resume_puts_permission_flags_before_resume() {
        // codex-cli 0.155 rejects `exec resume --sandbox …` (exit 2) but
        // parses `exec --sandbox … resume <id>` (probed 2026-09-29): the
        // permission flags must always precede `resume`.
        for autonomy in [Autonomy::Autonomous, Autonomy::AskFirst] {
            let args = CodeEngine::Codex.build_args("fix the bug", autonomy, Some("abc"), None);
            let resume_idx = args
                .iter()
                .position(|a| a == "resume")
                .unwrap_or_else(|| panic!("expected a `resume` element: {args:?}"));
            // (a) every permission flag sits at an index LOWER than `resume`.
            for flag in CodeEngine::Codex.permission_args(autonomy) {
                let flag_idx = args
                    .iter()
                    .position(|a| a == &flag)
                    .unwrap_or_else(|| panic!("missing flag {flag:?} in {args:?}"));
                assert!(
                    flag_idx < resume_idx,
                    "flag {flag:?} must come before `resume` (probed: after `resume` the CLI exits 2): {args:?}"
                );
            }
            // (b) `resume` is followed directly by the id.
            assert_eq!(
                args.get(resume_idx + 1).map(String::as_str),
                Some("abc"),
                "`resume` must be followed directly by the id: {args:?}"
            );
            // (c) the prompt is the last element.
            assert_eq!(args.last().map(String::as_str), Some("fix the bug"));
        }
        // No resume id: no `resume` element, and the permission args sit
        // right after `exec`.
        for autonomy in [Autonomy::Autonomous, Autonomy::AskFirst] {
            let args = CodeEngine::Codex.build_args("fix the bug", autonomy, None, None);
            assert!(!args.iter().any(|a| a == "resume"), "{args:?}");
            let perm = CodeEngine::Codex.permission_args(autonomy);
            let mut tail: Vec<&str> = args[1..1 + perm.len()].iter().map(String::as_str).collect();
            let mut want: Vec<&str> = perm.iter().map(String::as_str).collect();
            tail.sort();
            want.sort();
            assert_eq!(
                tail, want,
                "permission args must come right after `exec`: {args:?}"
            );
        }
    }

    #[test]
    fn u4a_plan_marker_stripped_with_trailing_newline() {
        // A marker glued to the end of the text and followed by a trailing
        // newline / blank line / CRLF must still be stripped and detected.
        let (clean, found) = take_plan_marker(&format!("Here is the plan.{PLAN_READY_MARKER}\n"));
        assert!(found);
        assert_eq!(clean, "Here is the plan.");
        let (clean, found) =
            take_plan_marker(&format!("Here is the plan.{PLAN_READY_MARKER}\n\n  "));
        assert!(found);
        assert_eq!(clean, "Here is the plan.");
        let (clean, found) = take_plan_marker(&format!("Here is the plan.{PLAN_READY_MARKER}\r\n"));
        assert!(found);
        assert_eq!(clean, "Here is the plan.");
    }

    /// RC2-R63 (usability RC2 round 5, Minor 3): the steps as the round saw
    /// them ("$ /bin/zsh -lc 'ls -ld …", "Bash: ls -ld …", "✓ Bash: mv …")
    /// read as the command itself, shortened; other steps are unchanged.
    #[test]
    fn rc2r63_a_command_step_reads_as_the_command_itself() {
        let codex = format!(
            "$ {}",
            first_line(
                "/bin/zsh -lc 'ls -ld /Projects/night/usability-sandbox/rc2-round5/w1; if [ -e /Projects/night/usability-sandbox/rc2-round5/w1/notes.txt ]; then cat notes.txt; fi'"
            )
        );
        let shown = plain_step(&codex);
        assert!(shown.starts_with("ls -ld /Projects/night/"), "{shown}");
        assert!(!shown.contains("zsh") && !shown.contains("$ "), "{shown}");
        assert!(shown.chars().count() <= PLAIN_STEP_CHARS, "{shown}");
        assert!(shown.ends_with('\u{2026}'), "{shown}");
        assert_eq!(
            plain_step("\u{2713} Bash: mv -n alpha.txt beta.txt && ls -la"),
            "\u{2713} mv -n alpha.txt beta.txt && ls -la"
        );
        assert_eq!(plain_step("Bash: ls -ld /tmp/w1"), "ls -ld /tmp/w1");
        assert_eq!(
            plain_step("$ /bin/zsh -lc \"if [[ -e notes.txt ]]; then print \\\"yes\\\"; fi\""),
            "if [[ -e notes.txt ]]; then print \"yes\"; fi"
        );
        assert_eq!(plain_step("$ bash -c 'echo '\\''hi'\\'''"), "echo 'hi'");
        assert_eq!(plain_step("$ ls"), "ls");
        assert_eq!(
            plain_step("\u{26A0} outside project \u{B7} Bash: cat ~/notes.md"),
            "\u{26A0} outside project \u{B7} cat ~/notes.md"
        );
        // Not a command step: as it was.
        for other in [
            "Read /tmp/w1/notes.txt",
            "web search: site:btrfs.readthedocs.io RAID56",
            "\u{2713} Write /tmp/w1/notes.txt (2 characters)",
            "interlock: recall",
        ] {
            assert_eq!(plain_step(other), other);
        }
        // A long command is cut, and a shell that is not a wrapper stays.
        let long = format!("Bash: echo {}", "x".repeat(200));
        assert_eq!(plain_step(&long).chars().count(), PLAIN_STEP_CHARS);
        assert_eq!(plain_step("$ zsh script.sh"), "zsh script.sh");
    }

    #[test]
    fn r1b_hide_plan_marker_hides_whole_and_cut_markers() {
        // While streaming (cut_tail): every start of the marker at the end is
        // hidden, and a whole marker anywhere.
        let full = format!("Plan:\n1. edit\n{PLAN_READY_MARKER}");
        for n in 1..=full.len() {
            let shown = hide_plan_marker(&full[..n], true);
            assert!(!shown.contains("[["), "step {n}: {shown:?}");
        }
        assert_eq!(hide_plan_marker(&full, true), "Plan:\n1. edit\n");
        assert_eq!(hide_plan_marker("a [[awili:pl", true), "a ");
        assert_eq!(
            hide_plan_marker(&format!("x{PLAN_READY_MARKER}y"), false),
            "xy"
        );
        // A settled clean reply keeps its own brackets (no cut tail).
        assert_eq!(
            hide_plan_marker("wiki links start with [[", false),
            "wiki links start with [["
        );
        // Nothing hidden: borrowed, unchanged.
        assert!(matches!(
            hide_plan_marker("Done.", true),
            std::borrow::Cow::Borrowed("Done.")
        ));
        assert!(matches!(
            hide_plan_marker("a [b] c", true),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
