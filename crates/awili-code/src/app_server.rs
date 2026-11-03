//! One Codex turn over Codex's app-server protocol (RC2-CXS, R3-20 Codex
//! part): `codex app-server` speaks JSON-RPC as one JSON object per line on
//! stdin/stdout (no `"jsonrpc"` field). Unlike `codex exec --json`, which
//! prints an agent message only once it is complete (codex-cli 0.160.1,
//! checked live 2026-10-06), the app server sends the reply text as
//! `item/agentMessage/delta` notifications while it is written.
//!
//! This is the pure half: lines in, the lines to send back and normalised
//! [`CodeEvent`]s out. The process (spawn, pipes, Stop) is the app's
//! (`awili-app::codex_app_server`). One turn is: `initialize` →
//! `initialized` → `thread/start` (or `thread/resume <id>` to continue a
//! conversation; thread ids from `codex exec` resume too) → `turn/start` →
//! notifications until `turn/completed`.
//!
//! Posture (the same as the exec path's flags): approval policy `never`
//! (nothing ever waits on a prompt Awili cannot show), sandbox `read-only`
//! for the read-only planning step of "Ask before changes" and the Chat,
//! `danger-full-access` otherwise (never sandboxed — see
//! [`CodeEngine::permission_args`](crate::CodeEngine::permission_args)).
//! A request the server still sends (an approval, an MCP elicitation) is
//! declined, as `exec` refuses it under `never`.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::{first_line, Autonomy, CodeEvent};

/// The request ids of one turn (each is sent once).
const INITIALIZE: u64 = 1;
const THREAD: u64 = 2;
const FORK: u64 = 4;
const TURN: u64 = 3;

/// The approval policy every turn runs with.
pub const APPROVAL_POLICY: &str = "never";

/// The sandbox a turn runs in: read-only for the planning step (and the
/// Chat), full access otherwise.
pub fn sandbox_mode(autonomy: Autonomy) -> &'static str {
    match autonomy {
        Autonomy::AskFirst => "read-only",
        Autonomy::Autonomous => "danger-full-access",
    }
}

/// What a turn is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnSpec {
    /// The folder the turn works in.
    pub cwd: String,
    /// The whole prompt text for this turn.
    pub prompt: String,
    /// The thread to continue (a conversation's earlier Codex session).
    pub resume_id: Option<String>,
    /// The model chosen for this turn (`None` = Codex's own default).
    pub model: Option<String>,
    pub autonomy: Autonomy,
}

/// Where the opening handshake stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handshake {
    /// `initialize` sent, no answer yet.
    Pending,
    /// The server answered `initialize`: the turn runs here.
    Ready,
    /// The server refused `initialize`, or printed something that is not a
    /// JSON-RPC message before answering it: this Codex cannot serve the
    /// turn (the app then runs `codex exec --json`).
    Unavailable,
}

/// The result of one line from the server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// Lines to write to the server's stdin, in order.
    pub send: Vec<String>,
    /// Events for the transcript.
    pub events: Vec<CodeEvent>,
}

/// One Codex turn's protocol state.
pub struct AppServerTurn {
    spec: TurnSpec,
    handshake: Handshake,
    thread_id: Option<String>,
    ownership_fork_attempted: bool,
    turn_id: Option<String>,
    /// Reply text was already shown, so the next message starts after a
    /// blank line (the exec path's boundary between messages).
    message_seen: bool,
    /// Agent messages already shown (streamed or complete): never twice.
    shown: HashSet<String>,
    /// This request's usage, computed from cumulative snapshots, not the
    /// entire resumed thread. Input includes cached input; output includes reasoning.
    /// The context window Codex reported for this thread (`modelContextWindow`).
    context_window: Option<u64>,
    usage: Option<(u64, u64)>,
    usage_snapshot: Option<(u64, u64)>,
    cached_snapshot: Option<u64>,
    cached_usage: u64,
    cached_unknown: bool,
    usage_unknown: bool,
    error_shown: bool,
    finished: bool,
    completed: bool,
    /// RC2-R2F9: a warm turn — the thread opens ahead of the question
    /// and `turn/start` waits for [`AppServerTurn::ask`].
    hold: bool,
    /// RC2-R615: agent messages Codex marks `phase: "commentary"` (its
    /// "I'll check …" preamble), with their text so far: progress, never
    /// the answer.
    commentary: std::collections::HashMap<String, String>,
    /// RC2-R615: reasoning summaries so far, by item, and the titles
    /// already said.
    thinking: std::collections::HashMap<String, String>,
    thoughts_said: HashSet<String>,
}

impl AppServerTurn {
    pub fn new(spec: TurnSpec) -> Self {
        Self {
            spec,
            handshake: Handshake::Pending,
            thread_id: None,
            ownership_fork_attempted: false,
            turn_id: None,
            message_seen: false,
            shown: HashSet::new(),
            context_window: None,
            usage: None,
            usage_snapshot: None,
            cached_snapshot: None,
            cached_usage: 0,
            cached_unknown: false,
            usage_unknown: false,
            error_shown: false,
            finished: false,
            completed: false,
            hold: false,
            commentary: Default::default(),
            thinking: Default::default(),
            thoughts_said: HashSet::new(),
        }
    }

    /// RC2-R2F9: a turn whose server is started, and its thread opened,
    /// before the question exists (`spec.prompt` is ignored until
    /// [`Self::ask`]). A council member's Codex is made ready this way while
    /// the owner reads, so his next message skips the start-up.
    pub fn warm(spec: TurnSpec) -> Self {
        let mut t = Self::new(spec);
        t.hold = true;
        t
    }

    /// RC2-R2F9: the warm turn's thread is open and waiting for a question.
    pub fn thread_ready(&self) -> bool {
        self.hold && self.thread_id.is_some() && !self.finished
    }

    /// The thread this turn runs in, once Codex opened it.
    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    /// RC2-R2F9: ask a warm turn its question: the `turn/start` line to send
    /// (none while its thread is not open).
    pub fn ask(&mut self, prompt: String) -> Vec<String> {
        self.spec.prompt = prompt;
        if !self.thread_ready() {
            return Vec::new();
        }
        self.hold = false;
        vec![self.turn_request()]
    }

    fn turn_request(&self) -> String {
        request(
            TURN,
            "turn/start",
            json!({
                "threadId": self.thread_id.as_deref().unwrap_or_default(),
                "input": awili_image_content::codex(&self.spec.prompt),
                // RC2-R615: Codex streams a short summary of its thinking,
                // so a long think shows what it is about.
                "summary": "auto",
            }),
        )
    }

    /// The first line to send: `initialize`.
    pub fn start(&self) -> Vec<String> {
        vec![request(
            INITIALIZE,
            "initialize",
            json!({
                "clientInfo": {
                    "name": "awili",
                    "title": "Awili",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
        )]
    }

    pub fn handshake(&self) -> Handshake {
        self.handshake
    }

    /// The turn has ended (completed, failed, or could not start).
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// The turn ended with Codex's own "completed".
    pub fn completed(&self) -> bool {
        self.completed
    }

    /// Handle one line the server printed. Lines that are not JSON-RPC
    /// objects are ignored.
    pub fn on_line(&mut self, line: &str) -> Step {
        let mut step = Step::default();
        if self.finished || line.trim().is_empty() {
            return step;
        }
        let parsed = serde_json::from_str::<Value>(line.trim()).ok();
        let rpc = parsed
            .as_ref()
            .is_some_and(|v| v.get("id").is_some() || v.get("method").is_some());
        // An app server prints only JSON-RPC messages; anything else before
        // it answered `initialize` (an `exec --json` event, a usage text)
        // means this binary is not one.
        if !rpc && self.handshake == Handshake::Pending {
            self.handshake = Handshake::Unavailable;
        }
        let Some(v) = parsed.filter(|_| rpc) else {
            return step;
        };
        let method = v.get("method").and_then(Value::as_str);
        match (v.get("id"), method) {
            (Some(id), Some(method)) => step.send.push(answer_server_request(id, method)),
            (Some(id), None) => self.on_response(id, &v, &mut step),
            (None, Some(method)) => self.on_notification(method, &v, &mut step),
            (None, None) => {}
        }
        step
    }

    fn on_response(&mut self, id: &Value, v: &Value, step: &mut Step) {
        let error = v
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| v.get("error").map(|e| e.to_string()));
        match id.as_u64() {
            Some(INITIALIZE) if self.handshake == Handshake::Pending => {
                if error.is_some() {
                    self.handshake = Handshake::Unavailable;
                    return;
                }
                self.handshake = Handshake::Ready;
                step.send
                    .push(json!({"method": "initialized", "params": {}}).to_string());
                step.send.push(self.thread_request());
            }
            Some(THREAD) | Some(FORK)
                if self.thread_id.is_none()
                    && (id.as_u64() == Some(FORK)) == self.ownership_fork_attempted =>
            {
                match error {
                    Some(e)
                        if id.as_u64() == Some(THREAD)
                            && self.spec.resume_id.is_some()
                            && !self.ownership_fork_attempted
                            && e.contains("already has an active writer") =>
                    {
                        // Resume never started the pending turn. Branch stored history
                        // into an independent writer; leave the other application's
                        // session untouched and submit the pending input exactly once.
                        self.ownership_fork_attempted = true;
                        step.send
                            .push(request(FORK, "thread/fork", self.thread_params()));
                    }
                    Some(e) => self.fail(step, if self.ownership_fork_attempted {
                        format!("Codex could not continue in an independent copy. Your saved conversation is unchanged. {e}")
                    } else { thread_error(&e, self.spec.resume_id.is_some()) }),
                    None => {
                        let Some(thread) = v.pointer("/result/thread/id").and_then(Value::as_str)
                        else {
                            self.fail(step, "Codex did not open a conversation.".to_string());
                            return;
                        };
                        if self.ownership_fork_attempted
                            && self.spec.resume_id.as_deref() == Some(thread)
                        {
                            self.fail(
                                step,
                                "Codex did not create an independent conversation.".into(),
                            );
                            return;
                        }
                        self.thread_id = Some(thread.to_string());
                        if self.ownership_fork_attempted {
                            step.events.push(CodeEvent::Diagnostic("Continuing in an independent copy of this conversation.".into()));
                        }
                        step.events.push(CodeEvent::SessionStarted {
                            session_id: thread.to_string(),
                        });
                        // RC2-R2F9: a warm turn waits for its question.
                        if !self.hold {
                            step.send.push(self.turn_request());
                        }
                    }
                }
            }
            Some(TURN) => match error {
                Some(e) => self.fail(step, format!("Codex could not start the reply: {e}")),
                None => {
                    self.turn_id = v
                        .pointer("/result/turn/id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
            },
            _ => {}
        }
    }

    /// `thread/start`, or `thread/resume` for a conversation that has one.
    fn thread_params(&self) -> Value {
        let mut params = json!({
            "cwd": self.spec.cwd,
            "sandbox": sandbox_mode(self.spec.autonomy),
            "approvalPolicy": APPROVAL_POLICY,
        });
        if let Some(m) = self.spec.model.as_deref().filter(|m| !m.trim().is_empty()) {
            params["model"] = json!(m);
        }
        if let Some(id) = &self.spec.resume_id {
            params["threadId"] = json!(id);
            params["excludeTurns"] = json!(true);
        }
        params
    }

    fn thread_request(&self) -> String {
        request(
            THREAD,
            if self.spec.resume_id.is_some() {
                "thread/resume"
            } else {
                "thread/start"
            },
            self.thread_params(),
        )
    }

    fn on_notification(&mut self, method: &str, v: &Value, step: &mut Step) {
        let params = v.get("params").unwrap_or(&Value::Null);
        // Only this turn's thread: a subagent's thread has its own items
        // and its own `turn/completed`.
        let thread = params.get("threadId").and_then(Value::as_str);
        if thread.is_some() && thread != self.thread_id.as_deref() {
            return;
        }
        match method {
            "item/agentMessage/delta" => {
                let item = params.get("itemId").and_then(Value::as_str).unwrap_or("");
                let Some(delta) = params
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|d| !d.is_empty())
                else {
                    return;
                };
                // RC2-R615: a commentary message is kept for its progress line.
                if let Some(note) = self.commentary.get_mut(item) {
                    note.push_str(delta);
                    return;
                }
                if self.shown.insert(item.to_string()) && self.message_seen {
                    step.events.push(CodeEvent::AssistantText("\n\n".into()));
                }
                self.message_seen = true;
                step.events
                    .push(CodeEvent::AssistantText(delta.to_string()));
            }
            // RC2-R2G8: a command or file change that starts is said at once
            // (as Claude's tools are), so a council card can name it.
            "item/started" => {
                if let Some(item) = params.get("item") {
                    // RC2-R615: Codex says up front whether a message is its
                    // answer or commentary ("I'll check …").
                    if item.get("type").and_then(Value::as_str) == Some("agentMessage")
                        && item.get("phase").and_then(Value::as_str) == Some("commentary")
                    {
                        if let Some(id) = item.get("id").and_then(Value::as_str) {
                            self.commentary.insert(id.to_string(), String::new());
                        }
                    }
                    on_item_started(item, step);
                }
            }
            "item/completed" => {
                if let Some(item) = params.get("item") {
                    self.on_item_completed(item, step);
                }
            }
            // RC2-R615 (usability RC2 round 5, Major 1: a long Codex turn
            // showed nothing for about 75 s): while Codex thinks, each topic
            // of its reasoning summary ("**Considering story composition**")
            // is said once, as progress.
            "item/reasoning/summaryTextDelta" => {
                let item = params.get("itemId").and_then(Value::as_str).unwrap_or("");
                let delta = params.get("delta").and_then(Value::as_str).unwrap_or("");
                let so_far = self.thinking.entry(item.to_string()).or_default();
                so_far.push_str(delta);
                for title in thought_titles(so_far) {
                    if self.thoughts_said.insert(format!("{item}\u{0}{title}")) {
                        step.events
                            .push(CodeEvent::Diagnostic(format!("{THINKING_PREFIX}{title}")));
                    }
                }
            }
            "turn/started" => {
                if self.turn_id.is_none() {
                    self.turn_id = params
                        .pointer("/turn/id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
            }
            "thread/tokenUsage/updated" => {
                if let Some(w) = params
                    .pointer("/tokenUsage/modelContextWindow")
                    .and_then(Value::as_u64)
                    .filter(|w| *w > 0)
                {
                    self.context_window = Some(w);
                }
                let read = |key: &str| -> Option<(u64, u64)> {
                    let value = params.pointer(key)?;
                    Some((
                        value.get("inputTokens")?.as_u64()?,
                        value.get("outputTokens")?.as_u64()?,
                    ))
                };
                let Some(total) = read("/tokenUsage/total") else {
                    return;
                };
                let active = self
                    .turn_id
                    .as_deref()
                    .is_some_and(|id| params.get("turnId").and_then(Value::as_str) == Some(id));
                // Resume/warmup can replay a previous turn's cumulative usage.
                // Keep its baseline, never charge it to the new request.
                if !active {
                    if self.turn_id.is_none() {
                        self.usage_snapshot = Some(total);
                        self.cached_snapshot = params
                            .pointer("/tokenUsage/total/cachedInputTokens")
                            .and_then(Value::as_u64);
                    }
                    return;
                }
                let delta = match self.usage_snapshot {
                    Some(prev) if prev == total => return, // duplicate snapshot
                    Some(prev) if total.0 >= prev.0 && total.1 >= prev.1 => {
                        Some((total.0 - prev.0, total.1 - prev.1))
                    }
                    // First resumed sample, or a counter reset/compaction:
                    // only this inference's explicit `last` is attributable.
                    _ if self.spec.resume_id.is_some() || self.usage_snapshot.is_some() => {
                        read("/tokenUsage/last")
                    }
                    _ => Some(total), // brand-new thread starts at zero
                };
                let cached_total = params
                    .pointer("/tokenUsage/total/cachedInputTokens")
                    .and_then(Value::as_u64);
                let cached_delta = match self.usage_snapshot {
                    Some(prev) if total.0 >= prev.0 && total.1 >= prev.1 => self
                        .cached_snapshot
                        .zip(cached_total)
                        .and_then(|(before, now)| now.checked_sub(before)),
                    _ if self.spec.resume_id.is_some() || self.usage_snapshot.is_some() => params
                        .pointer("/tokenUsage/last/cachedInputTokens")
                        .and_then(Value::as_u64),
                    _ => cached_total,
                };
                match cached_delta {
                    Some(n) => self.cached_usage = self.cached_usage.saturating_add(n),
                    None => self.cached_unknown = true,
                }
                self.cached_snapshot = cached_total;
                self.usage_snapshot = Some(total);
                if delta.is_none() {
                    self.usage_unknown = true;
                }
                if let Some(delta) = delta {
                    let previous = self.usage.unwrap_or_default();
                    self.usage = Some((
                        previous.0.saturating_add(delta.0),
                        previous.1.saturating_add(delta.1),
                    ));
                }
            }
            "error" => {
                let message = params
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex reported a problem");
                if params.get("willRetry").and_then(Value::as_bool) == Some(true) {
                    step.events.push(CodeEvent::Diagnostic(
                        "Codex lost its connection and is trying again".to_string(),
                    ));
                } else if !self.error_shown {
                    self.error_shown = true;
                    step.events.push(CodeEvent::Error(message.to_string()));
                }
            }
            "turn/completed" => {
                let turn = params.get("turn").unwrap_or(&Value::Null);
                let id = turn.get("id").and_then(Value::as_str);
                if let (Some(mine), Some(id)) = (self.turn_id.as_deref(), id) {
                    if mine != id {
                        return;
                    }
                }
                self.finished = true;
                match turn.get("status").and_then(Value::as_str) {
                    Some("completed") => {
                        self.completed = true;
                        if let Some(w) = self.context_window {
                            step.events.push(CodeEvent::ContextWindow(w));
                        }
                        if let Some((input_tokens, output_tokens)) =
                            self.usage.filter(|_| !self.usage_unknown)
                        {
                            step.events.push(CodeEvent::Usage {
                                cached_input_tokens: (!self.cached_unknown)
                                    .then_some(self.cached_usage.min(input_tokens)),
                                input_tokens,
                                output_tokens,
                            });
                        }
                        step.events.push(CodeEvent::TurnComplete);
                    }
                    Some("interrupted") => {
                        if !self.error_shown {
                            self.error_shown = true;
                            step.events.push(CodeEvent::Error(
                                "Codex stopped before the reply was finished.".to_string(),
                            ));
                        }
                    }
                    _ => {
                        if !self.error_shown {
                            self.error_shown = true;
                            let message = turn
                                .pointer("/error/message")
                                .and_then(Value::as_str)
                                .unwrap_or("Codex could not finish the reply.");
                            step.events.push(CodeEvent::Error(message.to_string()));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn on_item_completed(&mut self, item: &Value, step: &mut Step) {
        let status = item.get("status").and_then(Value::as_str);
        match item.get("type").and_then(Value::as_str) {
            Some("agentMessage") => {
                let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                // RC2-R615 (usability RC2 round 5: Codex's first line "I'll check
                // the filesystem documentation …" was its answer's first line):
                // a commentary message is progress, said once, never the answer.
                let commentary = self.commentary.remove(id).is_some()
                    || item.get("phase").and_then(Value::as_str) == Some("commentary");
                if commentary {
                    if self.shown.insert(id.to_string()) {
                        if let Some(note) = progress_note(text) {
                            step.events.push(CodeEvent::Diagnostic(note));
                        }
                    }
                    return;
                }
                // Streamed already, or a duplicate: shown once. With an id
                // missing, every complete message is its own.
                if text.is_empty() || (!id.is_empty() && !self.shown.insert(id.to_string())) {
                    return;
                }
                if self.message_seen {
                    step.events.push(CodeEvent::AssistantText("\n\n".into()));
                }
                self.message_seen = true;
                step.events.push(CodeEvent::AssistantText(text.to_string()));
            }
            Some("reasoning") => {
                let parts: Vec<&str> = item
                    .get("summary")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                if !parts.is_empty() {
                    step.events.push(CodeEvent::Reasoning(parts.join("\n")));
                }
            }
            Some("commandExecution") => {
                let command = item
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("command");
                let exit_ok = item
                    .get("exitCode")
                    .and_then(Value::as_i64)
                    .is_none_or(|c| c == 0);
                step.events.push(CodeEvent::ToolFinished {
                    label: format!("$ {}", first_line(command)),
                    ok: status == Some("completed") && exit_ok,
                });
            }
            Some("fileChange") => {
                let ok = status == Some("completed");
                for change in item
                    .get("changes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let path = change.get("path").and_then(Value::as_str).unwrap_or("file");
                    step.events.push(CodeEvent::ToolFinished {
                        label: format!("edit {path}"),
                        ok,
                    });
                }
            }
            // RC2-R2J: a search is named when it ends (its query is filled
            // in then).
            Some("webSearch") => step.events.push(CodeEvent::ToolStarted {
                label: crate::codex_search_label(item),
            }),
            _ => {}
        }
    }

    /// End the turn with one error in plain words.
    fn fail(&mut self, step: &mut Step, message: String) {
        self.finished = true;
        if !self.error_shown {
            self.error_shown = true;
            step.events.push(CodeEvent::Error(message));
        }
    }
}

fn request(id: u64, method: &str, params: Value) -> String {
    json!({"method": method, "id": id, "params": params}).to_string()
}

/// The answer to a request the server sends Awili. Approval policy
/// `never` means none should come; any that does is declined, as `codex
/// exec` refuses under `never`, and a request Awili does not know gets a
/// JSON-RPC error — the turn never waits on it.
fn answer_server_request(id: &Value, method: &str) -> String {
    let result = match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            json!({"decision": "decline"})
        }
        "execCommandApproval" | "applyPatchApproval" => json!({"decision": "denied"}),
        "mcpServer/elicitation/request" => json!({"action": "decline"}),
        "item/permissions/requestApproval" => json!({"permissions": {}}),
        "item/tool/requestUserInput" => json!({"answers": {}}),
        _ => {
            return json!({
                "id": id,
                "error": {"code": -32601, "message": "Awili does not handle this request"},
            })
            .to_string()
        }
    };
    json!({"id": id, "result": result}).to_string()
}

/// A failed `thread/start` / `thread/resume`, in plain words.
fn thread_error(message: &str, resuming: bool) -> String {
    if message.contains("already has an active writer") {
        return "This Codex conversation is open in another Codex window or run. Close it \
                there and try again."
            .to_string();
    }
    if resuming && message.contains("no rollout found") {
        return "Codex can no longer find this conversation's earlier session, so it cannot \
                carry on from it. Start a new conversation to go on."
            .to_string();
    }
    format!("Codex could not open the conversation: {message}")
}

/// RC2-R615: how a thought Codex reports reads as progress ("Thinking:
/// Considering story composition").
pub const THINKING_PREFIX: &str = "Thinking: ";

/// RC2-R615: the topics of a reasoning summary written so far: each
/// complete `**…**` title, in order.
pub fn thought_titles(summary: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = summary;
    while let Some(open) = rest.find("**") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("**") else {
            break;
        };
        let title = after[..close].trim();
        if !title.is_empty() && !title.contains('\n') {
            out.push(title.to_string());
        }
        rest = &after[close + 2..];
    }
    out
}

/// RC2-R615: a commentary message as one progress line (its first line, at
/// most 120 characters); `None` when it says nothing.
pub fn progress_note(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(if line.chars().count() > 120 {
        let cut: String = line.chars().take(119).collect();
        format!("{}\u{2026}", cut.trim_end())
    } else {
        line.to_string()
    })
}

/// RC2-R2G8 (RC2-R2F review P3): a command or file change Codex STARTS, as
/// the tool-start event Claude's tools give (`$ ls`, `edit src/a.rs`); its
/// end is still said by `item/completed` ("✓ $ ls"). Other items say
/// nothing here.
fn on_item_started(item: &Value, step: &mut Step) {
    match item.get("type").and_then(Value::as_str) {
        Some("commandExecution") => {
            let command = item
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("command");
            step.events.push(CodeEvent::ToolStarted {
                label: format!("$ {}", first_line(command)),
            });
        }
        Some("fileChange") => {
            for path in item
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|c| c.get("path").and_then(Value::as_str))
            {
                step.events.push(CodeEvent::ToolStarted {
                    label: format!("edit {path}"),
                });
            }
        }
        // RC2-R2J (RC2-R2F review P3 on 1366a0c0): an MCP tool, or a tool
        // the app gave Codex, is named as it starts too.
        Some("mcpToolCall") => step.events.push(CodeEvent::ToolStarted {
            label: crate::codex_mcp_label(item),
        }),
        Some("dynamicToolCall") => step.events.push(CodeEvent::ToolStarted {
            label: item
                .get("tool")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .unwrap_or("tool")
                .to_string(),
        }),
        // RC2-R2K (RC2-R2F review P3 on 55d863a3): handing work to another
        // agent is a tool too.
        Some("collabAgentToolCall") => step.events.push(CodeEvent::ToolStarted {
            label: crate::codex_agent_label(item),
        }),
        _ => {}
    }
}
