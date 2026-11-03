//! Typed decode of the gateway's stringly-typed server-push events.
//!
//! The raw [`GatewayEvent`](crate::GatewayEvent) is `{event_type, session_id,
//! payload}` — consumers (`work.rs::gateway_event_to_update`,
//! `gateway_chat.rs::chat_gateway_step`) re-parse the untyped `payload` by hand,
//! duplicating the event vocabulary across crates. This module is the **single
//! typed event surface** for the gateway: [`decode_event`] turns one raw event
//! into a [`GatewayEventKind`], and the consumers match on the enum instead of
//! poking at `serde_json::Value`.
//!
//! Faithful to the Hermes desktop event router (porting-map §1.6, slice-0 §3.4):
//! every event type the existing `work.rs` reducer matches decodes to a
//! non-[`Unknown`](GatewayEventKind::Unknown) variant. The decode is **additive
//! and behavior-preserving** — it carries exactly the fields the reducers read,
//! leaving thresholds, status parsing, and rendering to the consumers so the
//! later re-base (plan units 3 & 4) is a mechanical 1:1.
//!
//! There is **no `run.completed`** in this protocol. Turn end is the conjunction
//! of `message.complete` *and* `session.info running:false` — see [`is_turn_end`]
//! and [`TurnState`] (slice-0 §3.4 / spec line 236: "treat these two together").

use serde_json::Value;

use crate::GatewayEvent;

// S2(a) correction (RVS2A-FIX1): the closed risk vocabulary lives in
// `awili-work` now (the seam `WorkEvent::ApprovalRequested.risk` is TYPED, so
// the enum is next to the seam vocabulary) and is re-exported here so the
// existing `awili_gateway::events::ApprovalRisk` path keeps compiling.
pub use awili_work::run_events::ApprovalRisk;

// ============================================================================
// Typed event surface
// ============================================================================

/// The phase of a non-terminal `tool.*` event. `tool.start`, `tool.progress`,
/// and `tool.generating` are handled identically by the current reducer but kept
/// distinct here so a future consumer can tell first-touch from mid-run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPhase {
    Start,
    Progress,
    Generating,
}

/// The phase of a `subagent.*` event. Mirrors the upstream subagent lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentPhase {
    SpawnRequested,
    Start,
    Thinking,
    Tool,
    Progress,
    Complete,
    /// A `subagent.*` event whose suffix this build does not model.
    Other,
}

/// A decoded, typed view of a [`GatewayEvent`]. Each variant carries exactly the
/// fields the consumers read; the raw payload is dropped except on
/// [`Unknown`](Self::Unknown) and [`SkinChanged`](Self::SkinChanged), where the
/// shape is not yet pinned.
///
/// `Subagent` is intentionally a wide value-embedded variant: boxing its file
/// lists would ripple into every consumer of this typed surface, and the enum
/// is cloned at most once per decoded event.
// Variant sizes differ by design: this is a parsed wire event, boxed nowhere hot.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum GatewayEventKind {
    /// `message.start` — the assistant turn began. `model` is the resolved model
    /// name, when the backend includes it.
    MessageStart { model: Option<String> },
    /// `message.delta` — a streamed token chunk. `text` may be empty (the reducer
    /// drops empties).
    MessageDelta { text: String },
    /// `message.complete` — the assistant turn's final text + accounting. `status`
    /// is the raw finish word (`"complete"`/`"error"`/`"stopped"`…); `cost_usd`
    /// and `context_percent` are raw, un-normalized (the consumer applies its own
    /// thresholds). `input_tokens`/`output_tokens` are the turn's token counts
    /// (`usage.input`/`usage.input_tokens`, `usage.output`/`usage.output_tokens`)
    /// — the Chat consumer turns them into a `Usage` event; Work mode ignores them
    /// (it surfaces dollar cost, not tokens).
    MessageComplete {
        text: Option<String>,
        status: Option<String>,
        cost_usd: Option<f64>,
        context_percent: Option<f64>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    },
    /// `reasoning.delta` — the model emitted hidden-reasoning text.
    ReasoningDelta { text: String },
    /// `reasoning.available` — reasoning for the turn is complete/available.
    ReasoningAvailable,
    /// `thinking.delta` — Hermes's "face + verb" streaming status line. `text` is
    /// drawn from `text` then `status`; `None` when neither is present.
    ThinkingDelta { text: Option<String> },
    /// `tool.start` / `tool.progress` / `tool.generating` — a tool is running.
    /// `message` is `summary`/`message`/`text` (first present); `file_path` is
    /// `input.file_path`/`input.path`.
    Tool {
        phase: ToolPhase,
        name: Option<String>,
        message: Option<String>,
        file_path: Option<String>,
    },
    /// `tool.complete` — a tool finished. `message` is `summary`/`message`;
    /// `duration_s` is the raw run time (the consumer filters sub-50ms noise).
    ToolComplete {
        name: Option<String>,
        message: Option<String>,
        duration_s: Option<f64>,
    },
    /// `approval.request` — the agent is blocking on command approval.
    /// `risk` is the CLOSED risk class of the gated call (S2(a)): the wire
    /// value (`risk`/`risk_class`/`riskClass`) decodes case-insensitively
    /// after ASCII-whitespace trim; a missing, non-string, or unknown value
    /// classifies as [`ApprovalRisk::Privileged`] — never an unclassified
    /// string. The Work surface decides which approval scopes to offer from
    /// it (S2(a): `always` only for `ReadOnly`/`Mutating`).
    Approval {
        command: Option<String>,
        description: Option<String>,
        approval_id: Option<String>,
        allow_permanent: bool,
        risk: ApprovalRisk,
    },
    /// `hook.denied` — a TYPED denial fact observed at the host-approval hook
    /// (or emitted by the gateway): the `{source, reason, recovery}` triple
    /// (P3a). Kept verbatim — the consumer must render it as
    /// `Blocked by <source>: <reason>` with only the supplied recovery,
    /// never collapse it into a generic `error`.
    HookDenial {
        source: Option<String>,
        reason: Option<String>,
        recovery: Option<String>,
    },
    /// `clarify.request` — the agent is blocking on a clarifying question.
    Clarify {
        question: Option<String>,
        request_id: Option<String>,
    },
    /// `session.info` — the authoritative session state push. `running` is the
    /// turn-busy signal (there is no `run.completed`); the rest are folded for the
    /// status surfaces (porting-map §1.6), all optional.
    SessionInfo {
        running: Option<bool>,
        branch: Option<String>,
        cwd: Option<String>,
        model: Option<String>,
        provider: Option<String>,
        reasoning_effort: Option<String>,
        service_tier: Option<String>,
    },
    /// `status.update` — a transient status line. `kind` tags special states
    /// (e.g. `"compacting"`); `message` is the human text.
    StatusUpdate {
        kind: Option<String>,
        message: Option<String>,
    },
    /// `terminal.read` / `terminal.read.request` — the agent is blocking on a
    /// terminal read; the responder must reply to `request_id`.
    TerminalRead { request_id: Option<String> },
    /// `sudo.request` — blocking on a sudo password.
    Sudo { request_id: Option<String> },
    /// `secret.request` — blocking on a credential value for `env_var`.
    Secret {
        request_id: Option<String>,
        env_var: Option<String>,
        prompt: Option<String>,
    },
    /// `review.summary` — the backend's self-improvement review saved something.
    ReviewSummary { text: Option<String> },
    /// `error` — a terminal gateway/turn error. `message` is `message`/`error`.
    Error { message: Option<String> },
    /// `subagent.*` — a child agent lifecycle event. Carries the union of fields
    /// the reducer reads across phases; absent scalar fields are `None`, absent
    /// arrays are empty. The `session_id`..`current_tool` block feeds the unit-18
    /// [`subagents`](crate::subagents) reducer's `SubagentProgress`.
    Subagent {
        phase: SubagentPhase,
        id: Option<String>,
        parent_id: Option<String>,
        goal: Option<String>,
        model: Option<String>,
        name: Option<String>,
        status: Option<String>,
        text: Option<String>,
        summary: Option<String>,
        /// `costUsd` arrives as either a string or a number upstream; kept as the
        /// raw string form (a number is stringified) so the consumer parses it.
        cost_usd: Option<String>,
        /// `sessionId` — the child agent's own session, when it has one.
        session_id: Option<String>,
        /// `taskCount`/`taskIndex` — the delegation batch size and this agent's
        /// slot in it. The reducer groups siblings sharing a `task_count` (started
        /// within 5s) into one parallel `DelegationGroup`.
        task_count: Option<u64>,
        task_index: Option<u64>,
        /// `inputTokens`/`outputTokens` (top-level, falling back to `usage.*`).
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        /// `toolCount` — running count of tool calls this agent has made.
        tool_count: Option<u64>,
        /// `filesRead`/`filesWritten` — paths the agent has touched (may be
        /// empty); union-merged across events by the reducer.
        files_read: Vec<String>,
        files_written: Vec<String>,
        /// `currentTool` — the tool the agent is running right now, if any.
        current_tool: Option<String>,
    },
    /// `gateway.ready` — the connection's initial ready signal.
    GatewayReady,
    /// `skin.changed` — a cosmetic theme push. Payload shape is not yet pinned.
    SkinChanged { payload: Option<Value> },
    /// Any event type this build does not model. Preserves the raw `event_type`
    /// and `payload` so contract drift surfaces in diagnostics rather than
    /// vanishing (the same safety net the `terminal.read` rename relied on).
    Unknown {
        event_type: String,
        payload: Option<Value>,
    },
}

// --- payload field helpers --------------------------------------------------

/// Borrow a string field from the payload by key.
fn str_field<'a>(p: Option<&'a Value>, key: &str) -> Option<&'a str> {
    p.and_then(|p| p.get(key)).and_then(|v| v.as_str())
}

/// The first *present* of several keys, coerced to a string (handles upstream
/// key drift like `request_id`/`requestId`).
///
/// Faithful to the original reducer's `get(k1).or_else(|| get(k2)).and_then(as_str)`:
/// the fallback fires only when the earlier key is **absent**, not when it is
/// present with a non-string value. (Try-each-`as_str` would silently fall back on
/// a wrong-typed primary key — a behavior divergence flagged in the unit-3 review.)
fn str_any<'a>(p: Option<&'a Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| p.and_then(|p| p.get(*k)))
        .and_then(|v| v.as_str())
}

fn string_field(p: Option<&Value>, key: &str) -> Option<String> {
    str_field(p, key).map(|s| s.to_string())
}

fn string_any(p: Option<&Value>, keys: &[&str]) -> Option<String> {
    str_any(p, keys).map(|s| s.to_string())
}

fn bool_field(p: Option<&Value>, key: &str) -> Option<bool> {
    p.and_then(|p| p.get(key)).and_then(|v| v.as_bool())
}

fn f64_field(p: Option<&Value>, key: &str) -> Option<f64> {
    p.and_then(|p| p.get(key)).and_then(|v| v.as_f64())
}

/// `usage.<key>` then `usage.<alt>` — `cost_usd`/`cost` live under `usage`.
///
/// Same first-present-key-then-coerce rule as [`str_any`]: the fallback fires only
/// when the earlier key is absent, matching the original
/// `get(k1).or_else(|| get(k2)).and_then(as_f64)`.
fn usage_f64(p: Option<&Value>, keys: &[&str]) -> Option<f64> {
    let usage = p.and_then(|p| p.get("usage"));
    keys.iter()
        .find_map(|k| usage.and_then(|u| u.get(*k)))
        .and_then(|v| v.as_f64())
}

/// `usage.<key>` then `usage.<alt>` as a u64 — token counts (`input`/`input_tokens`,
/// `output`/`output_tokens`). Same first-present-key-then-coerce rule as
/// [`usage_f64`]: a present-but-wrong-typed primary key does NOT fall back, matching
/// the original Chat reducer's `get(k1).or_else(|| get(k2)).and_then(as_u64)`.
fn usage_u64(p: Option<&Value>, keys: &[&str]) -> Option<u64> {
    let usage = p.and_then(|p| p.get("usage"));
    keys.iter()
        .find_map(|k| usage.and_then(|u| u.get(*k)))
        .and_then(|v| v.as_u64())
}

/// A top-level `u64` payload field (e.g. `taskCount`). Distinct from
/// [`usage_u64`], which reads under the `usage` object.
fn u64_field(p: Option<&Value>, key: &str) -> Option<u64> {
    p.and_then(|p| p.get(key)).and_then(|v| v.as_u64())
}

/// A payload array of strings (e.g. `filesRead`/`filesWritten`) collected into a
/// `Vec`, dropping non-string elements. Empty when the key is absent or not an
/// array. Feeds the unit-18 subagent reducer's file-touch union.
fn str_vec_field(p: Option<&Value>, key: &str) -> Vec<String> {
    p.and_then(|p| p.get(key))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

// ============================================================================
// decode
// ============================================================================

/// Decode one raw [`GatewayEvent`] into a typed [`GatewayEventKind`].
///
/// Behavior-preserving: pulls exactly the fields the existing reducers read, with
/// the same key fallbacks. Unknown event types decode to
/// [`GatewayEventKind::Unknown`] carrying the raw type + payload.
pub fn decode_event(ev: &GatewayEvent) -> GatewayEventKind {
    let p = ev.payload.as_ref();
    let t = ev.event_type.as_str();

    // subagent.* — match on prefix, derive the phase from the suffix.
    if let Some(suffix) = t.strip_prefix("subagent.") {
        let phase = match suffix {
            "spawn_requested" => SubagentPhase::SpawnRequested,
            "start" => SubagentPhase::Start,
            "thinking" => SubagentPhase::Thinking,
            "tool" => SubagentPhase::Tool,
            "progress" => SubagentPhase::Progress,
            "complete" => SubagentPhase::Complete,
            _ => SubagentPhase::Other,
        };
        // `costUsd` may be string or number upstream; normalize to a string.
        let cost_usd = p.and_then(|p| p.get("costUsd")).and_then(|v| {
            if v.is_string() {
                v.as_str().map(|s| s.to_string())
            } else if v.is_number() {
                Some(v.to_string())
            } else {
                None
            }
        });
        return GatewayEventKind::Subagent {
            phase,
            id: string_field(p, "id"),
            parent_id: string_field(p, "parentId"),
            goal: string_field(p, "goal"),
            model: string_field(p, "model"),
            name: string_field(p, "name"),
            status: string_field(p, "status"),
            text: string_field(p, "text"),
            summary: string_field(p, "summary"),
            cost_usd,
            session_id: string_field(p, "sessionId"),
            task_count: u64_field(p, "taskCount"),
            task_index: u64_field(p, "taskIndex"),
            input_tokens: u64_field(p, "inputTokens")
                .or_else(|| usage_u64(p, &["input", "input_tokens"])),
            output_tokens: u64_field(p, "outputTokens")
                .or_else(|| usage_u64(p, &["output", "output_tokens"])),
            tool_count: u64_field(p, "toolCount"),
            files_read: str_vec_field(p, "filesRead"),
            files_written: str_vec_field(p, "filesWritten"),
            current_tool: string_field(p, "currentTool"),
        };
    }

    match t {
        "message.start" => GatewayEventKind::MessageStart {
            model: string_field(p, "model"),
        },
        "message.delta" => GatewayEventKind::MessageDelta {
            text: str_field(p, "text").unwrap_or("").to_string(),
        },
        "message.complete" => GatewayEventKind::MessageComplete {
            text: string_any(p, &["text", "rendered"]),
            status: string_field(p, "status"),
            cost_usd: usage_f64(p, &["cost_usd", "cost"]),
            context_percent: usage_f64(p, &["context_percent"]),
            input_tokens: usage_u64(p, &["input", "input_tokens"]),
            output_tokens: usage_u64(p, &["output", "output_tokens"]),
        },
        "reasoning.delta" => GatewayEventKind::ReasoningDelta {
            text: str_field(p, "text").unwrap_or("").to_string(),
        },
        "reasoning.available" => GatewayEventKind::ReasoningAvailable,
        "thinking.delta" => GatewayEventKind::ThinkingDelta {
            text: string_any(p, &["text", "status"]),
        },
        "tool.start" => tool_running(ToolPhase::Start, p),
        "tool.progress" => tool_running(ToolPhase::Progress, p),
        "tool.generating" => tool_running(ToolPhase::Generating, p),
        "tool.complete" => GatewayEventKind::ToolComplete {
            name: string_field(p, "name"),
            message: string_any(p, &["summary", "message"]),
            duration_s: f64_field(p, "duration_s"),
        },
        "approval.request" => GatewayEventKind::Approval {
            command: string_field(p, "command"),
            description: string_field(p, "description"),
            approval_id: string_any(p, &["approval_id", "approvalId", "request_id"]),
            allow_permanent: bool_field(p, "allow_permanent").unwrap_or(false),
            // S2(a): closed risk vocabulary — the wire value decodes into the
            // enum (conservative: missing/non-string/unknown -> Privileged).
            risk: ApprovalRisk::decode(
                string_any(p, &["risk", "risk_class", "riskClass"]).as_deref(),
            ),
        },
        "hook.denied" => GatewayEventKind::HookDenial {
            source: string_field(p, "source"),
            reason: string_field(p, "reason"),
            recovery: string_field(p, "recovery"),
        },
        "clarify.request" => GatewayEventKind::Clarify {
            question: string_field(p, "question"),
            request_id: string_any(p, &["request_id", "requestId"]),
        },
        "session.info" => GatewayEventKind::SessionInfo {
            running: bool_field(p, "running"),
            branch: string_field(p, "branch"),
            cwd: string_field(p, "cwd"),
            model: string_field(p, "model"),
            provider: string_field(p, "provider"),
            reasoning_effort: string_any(p, &["reasoning_effort", "reasoningEffort"]),
            service_tier: string_any(p, &["service_tier", "serviceTier"]),
        },
        "status.update" => GatewayEventKind::StatusUpdate {
            kind: string_field(p, "kind"),
            message: string_field(p, "message"),
        },
        "terminal.read" | "terminal.read.request" => GatewayEventKind::TerminalRead {
            request_id: string_any(p, &["request_id", "requestId"]),
        },
        "sudo.request" => GatewayEventKind::Sudo {
            request_id: string_any(p, &["request_id", "requestId"]),
        },
        "secret.request" => GatewayEventKind::Secret {
            request_id: string_any(p, &["request_id", "requestId"]),
            env_var: string_any(p, &["env_var", "envVar"]),
            prompt: string_field(p, "prompt"),
        },
        "review.summary" => GatewayEventKind::ReviewSummary {
            text: string_field(p, "text"),
        },
        "error" => GatewayEventKind::Error {
            message: string_any(p, &["message", "error"]),
        },
        "gateway.ready" => GatewayEventKind::GatewayReady,
        "skin.changed" => GatewayEventKind::SkinChanged {
            payload: ev.payload.clone(),
        },
        other => GatewayEventKind::Unknown {
            event_type: other.to_string(),
            payload: ev.payload.clone(),
        },
    }
}

fn tool_running(phase: ToolPhase, p: Option<&Value>) -> GatewayEventKind {
    let file_path = p
        .and_then(|p| p.get("input"))
        .and_then(|i| i.get("file_path").or_else(|| i.get("path")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    GatewayEventKind::Tool {
        phase,
        name: string_field(p, "name"),
        message: string_any(p, &["summary", "message", "text"]),
        file_path,
    }
}

// ============================================================================
// Turn-end detector (plan unit 2)
// ============================================================================

/// Accumulates the two signals that, **together**, end a turn. There is no
/// `run.completed`; turn end requires both `message.complete` and
/// `session.info running:false`, in either order (slice-0 §3.4, spec line 236).
/// An `error` is independently terminal.
///
/// A completion counts only while no new assistant message or tool call has
/// started after it (P3g): a turn that makes a tool call emits one
/// `message.complete` per assistant message, so `message.start` and
/// `tool.start` clear the remembered completion — the turn continues.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnState {
    saw_message_complete: bool,
    saw_running_false: bool,
    ended: bool,
}

impl TurnState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the turn has already been declared ended.
    pub fn ended(&self) -> bool {
        self.ended
    }
}

/// Fold one decoded event into `state` and report whether the turn ends **now**.
///
/// Returns `true` exactly once — on the event that completes the
/// `message.complete` + `session.info running:false` conjunction (or on a
/// terminal `error`). Subsequent events return `false`. The order of the two
/// completion signals does not matter, but only a completion seen since the
/// last `message.start` / `tool.start` counts (P3g): a tool-call completion
/// followed by more assistant output is not the turn's completion.
pub fn is_turn_end(ev: &GatewayEventKind, state: &mut TurnState) -> bool {
    if state.ended {
        return false;
    }
    match ev {
        GatewayEventKind::Error { .. } => {
            state.ended = true;
            true
        }
        GatewayEventKind::MessageComplete { .. } => {
            state.saw_message_complete = true;
            if state.saw_running_false {
                state.ended = true;
                true
            } else {
                false
            }
        }
        GatewayEventKind::SessionInfo {
            running: Some(false),
            ..
        } => {
            state.saw_running_false = true;
            if state.saw_message_complete {
                state.ended = true;
                true
            } else {
                false
            }
        }
        // P3g: a new assistant message or tool call means the turn goes on;
        // any completion seen so far was a tool-call message's, not the
        // turn's. `running:false` is kept, so "either order" still holds for
        // the final message.
        GatewayEventKind::MessageStart { .. }
        | GatewayEventKind::Tool {
            phase: ToolPhase::Start,
            ..
        } => {
            state.saw_message_complete = false;
            false
        }
        // `running:true` means the turn is actively busy *now*, so any earlier
        // `running:false` was pre-turn idle, not this turn's end — re-arm so a
        // leading stale `session.info` can't pre-satisfy the conjunction with a
        // later `message.complete` (Codex nit, plan unit 2).
        GatewayEventKind::SessionInfo {
            running: Some(true),
            ..
        } => {
            state.saw_running_false = false;
            false
        }
        _ => false,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(event_type: &str, payload: Value) -> GatewayEvent {
        GatewayEvent {
            event_type: event_type.to_string(),
            session_id: Some("s1".to_string()),
            payload: Some(payload),
        }
    }

    #[test]
    fn message_start_model() {
        assert_eq!(
            decode_event(&ev("message.start", json!({ "model": "claude-opus" }))),
            GatewayEventKind::MessageStart {
                model: Some("claude-opus".to_string())
            }
        );
        assert_eq!(
            decode_event(&ev("message.start", json!({}))),
            GatewayEventKind::MessageStart { model: None }
        );
    }

    #[test]
    fn message_delta_text() {
        assert_eq!(
            decode_event(&ev("message.delta", json!({ "text": "hi" }))),
            GatewayEventKind::MessageDelta {
                text: "hi".to_string()
            }
        );
        // Missing text decodes to empty (reducer drops empties downstream).
        assert_eq!(
            decode_event(&ev("message.delta", json!({}))),
            GatewayEventKind::MessageDelta {
                text: String::new()
            }
        );
    }

    #[test]
    fn message_complete_fields_and_fallbacks() {
        let d = decode_event(&ev(
            "message.complete",
            json!({
                "text": "done",
                "status": "complete",
                "usage": { "cost_usd": 0.12, "context_percent": 0.9, "input": 188, "output": 30 }
            }),
        ));
        assert_eq!(
            d,
            GatewayEventKind::MessageComplete {
                text: Some("done".to_string()),
                status: Some("complete".to_string()),
                cost_usd: Some(0.12),
                context_percent: Some(0.9),
                input_tokens: Some(188),
                output_tokens: Some(30),
            }
        );
        // `rendered` falls back for text; `cost` falls back for cost_usd;
        // `*_tokens` fall back for the token counts.
        let d = decode_event(&ev(
            "message.complete",
            json!({ "rendered": "r", "usage": { "cost": 1.0, "input_tokens": 5, "output_tokens": 7 } }),
        ));
        assert_eq!(
            d,
            GatewayEventKind::MessageComplete {
                text: Some("r".to_string()),
                status: None,
                cost_usd: Some(1.0),
                context_percent: None,
                input_tokens: Some(5),
                output_tokens: Some(7),
            }
        );
    }

    /// Multi-key fallback fires only when the earlier key is ABSENT, never when
    /// it is present with the wrong type — matching the original reducer's
    /// `get(k1).or_else(|| get(k2)).and_then(as_str/as_f64)` (unit-3 review BLOCK).
    #[test]
    fn alt_keys_do_not_fall_back_on_wrong_type() {
        // text present but a number -> as_str None; rendered NOT consulted.
        assert_eq!(
            decode_event(&ev(
                "message.complete",
                json!({ "text": 123, "rendered": "ok" }),
            )),
            GatewayEventKind::MessageComplete {
                text: None,
                status: None,
                cost_usd: None,
                context_percent: None,
                input_tokens: None,
                output_tokens: None,
            }
        );
        // cost_usd present but a string -> as_f64 None; cost NOT consulted.
        // Likewise `input` present but a string -> as_u64 None; input_tokens NOT consulted.
        assert_eq!(
            decode_event(&ev(
                "message.complete",
                json!({ "usage": { "cost_usd": "bad", "cost": 1.0, "input": "bad", "input_tokens": 5 } }),
            )),
            GatewayEventKind::MessageComplete {
                text: None,
                status: None,
                cost_usd: None,
                context_percent: None,
                input_tokens: None,
                output_tokens: None,
            }
        );
        // But a genuinely ABSENT primary key DOES fall back to the alternate.
        assert_eq!(
            decode_event(&ev("message.complete", json!({ "rendered": "r" }))),
            GatewayEventKind::MessageComplete {
                text: Some("r".to_string()),
                status: None,
                cost_usd: None,
                context_percent: None,
                input_tokens: None,
                output_tokens: None,
            }
        );
    }

    #[test]
    fn reasoning_and_thinking() {
        assert_eq!(
            decode_event(&ev("reasoning.delta", json!({ "text": "ponder" }))),
            GatewayEventKind::ReasoningDelta {
                text: "ponder".to_string()
            }
        );
        assert_eq!(
            decode_event(&ev("reasoning.available", json!({}))),
            GatewayEventKind::ReasoningAvailable
        );
        // thinking.delta prefers `text`, falls back to `status`.
        assert_eq!(
            decode_event(&ev("thinking.delta", json!({ "status": "searching" }))),
            GatewayEventKind::ThinkingDelta {
                text: Some("searching".to_string())
            }
        );
    }

    #[test]
    fn tool_phases() {
        for (t, phase) in [
            ("tool.start", ToolPhase::Start),
            ("tool.progress", ToolPhase::Progress),
            ("tool.generating", ToolPhase::Generating),
        ] {
            let d = decode_event(&ev(
                t,
                json!({ "name": "bash", "summary": "ls", "input": { "file_path": "/a" } }),
            ));
            assert_eq!(
                d,
                GatewayEventKind::Tool {
                    phase,
                    name: Some("bash".to_string()),
                    message: Some("ls".to_string()),
                    file_path: Some("/a".to_string()),
                }
            );
        }
        // message falls back message->text; file_path falls back input.path.
        let d = decode_event(&ev(
            "tool.progress",
            json!({ "name": "edit", "text": "patch", "input": { "path": "/b" } }),
        ));
        assert_eq!(
            d,
            GatewayEventKind::Tool {
                phase: ToolPhase::Progress,
                name: Some("edit".to_string()),
                message: Some("patch".to_string()),
                file_path: Some("/b".to_string()),
            }
        );
    }

    #[test]
    fn tool_complete() {
        assert_eq!(
            decode_event(&ev(
                "tool.complete",
                json!({ "name": "bash", "message": "ok", "duration_s": 2.3 })
            )),
            GatewayEventKind::ToolComplete {
                name: Some("bash".to_string()),
                message: Some("ok".to_string()),
                duration_s: Some(2.3),
            }
        );
    }

    #[test]
    fn approval_request_id_fallbacks() {
        let d = decode_event(&ev(
            "approval.request",
            json!({ "command": "rm", "description": "delete", "request_id": "r9", "allow_permanent": true }),
        ));
        assert_eq!(
            d,
            GatewayEventKind::Approval {
                command: Some("rm".to_string()),
                description: Some("delete".to_string()),
                approval_id: Some("r9".to_string()),
                allow_permanent: true,
                // S2(a): no risk key on the wire -> the conservative
                // closed-set member, never an unclassified string.
                risk: ApprovalRisk::Privileged,
            }
        );
    }

    /// S2(a): `approval.request` carries the CLOSED risk class when tagged
    /// (key fallbacks included), and classifies conservatively when absent.
    #[test]
    fn approval_request_risk_fallbacks() {
        for key in ["risk", "risk_class", "riskClass"] {
            let mut payload = serde_json::Map::new();
            payload.insert("command".into(), json!("rm"));
            payload.insert(key.to_string(), json!("irreversible"));
            let d = decode_event(&ev("approval.request", Value::Object(payload)));
            assert_eq!(
                d,
                GatewayEventKind::Approval {
                    command: Some("rm".to_string()),
                    description: None,
                    approval_id: None,
                    allow_permanent: false,
                    risk: ApprovalRisk::Irreversible,
                },
                "key {key} must decode as risk"
            );
        }
        // absent -> the conservative closed-set member (never None, never a
        // raw string): the surface then offers the restricted scopes.
        let d = decode_event(&ev("approval.request", json!({ "command": "rm" })));
        match d {
            GatewayEventKind::Approval { risk, .. } => {
                assert_eq!(risk, ApprovalRisk::Privileged)
            }
            other => panic!("expected Approval, got {other:?}"),
        }
    }

    /// S2(a) required proof: the decode is a CLOSED set. Each canonical wire
    /// string (plus its ASCII-whitespace-trimmed, case-insensitive forms)
    /// decodes to its class; a missing key, a non-string value, and an
    /// unknown string all classify as `Privileged` — the decision path never
    /// carries an unclassified risk string.
    #[test]
    fn approval_risk_decodes_closed_classes() {
        // Each canonical value (and its trimmed / upper-case forms) decodes
        // to its class.
        for (wire, class) in [
            ("read_only", ApprovalRisk::ReadOnly),
            ("mutating", ApprovalRisk::Mutating),
            ("irreversible", ApprovalRisk::Irreversible),
            ("privileged", ApprovalRisk::Privileged),
        ] {
            assert_eq!(ApprovalRisk::decode(Some(wire)), class, "wire {wire}");
            assert_eq!(
                ApprovalRisk::decode(Some(&format!("  {wire}  "))),
                class,
                "trimmed {wire:?}"
            );
            let upper: String = wire.to_uppercase();
            assert_eq!(
                ApprovalRisk::decode(Some(&upper)),
                class,
                "uppercased {upper:?}"
            );
        }
        // Missing -> Privileged (conservative by construction).
        assert_eq!(ApprovalRisk::decode(None), ApprovalRisk::Privileged);
        // Unknown string -> Privileged (classification may only escalate).
        assert_eq!(
            ApprovalRisk::decode(Some("totally_unknown")),
            ApprovalRisk::Privileged
        );
        assert_eq!(ApprovalRisk::decode(Some("")), ApprovalRisk::Privileged);
        // Non-string on the wire (the decoder takes a string key): a JSON
        // number where a string is expected reads as `None` -> Privileged.
        let d = decode_event(&ev(
            "approval.request",
            json!({ "command": "rm", "risk": 42 }),
        ));
        match d {
            GatewayEventKind::Approval { risk, .. } => {
                assert_eq!(risk, ApprovalRisk::Privileged)
            }
            other => panic!("expected Approval, got {other:?}"),
        }
        // The decode is the EXACT closed set: four classes, no other.
        assert_eq!(ApprovalRisk::WIRE_VALUES.len(), 4);
    }

    /// P3a: the typed denial fact decodes to a NON-Unknown variant carrying
    /// `{source, reason, recovery}` verbatim; absent fields stay `None`
    /// (the app-side adapter applies conservative defaults, not here).
    #[test]
    fn hook_denied_decodes_typed_fact() {
        let d = decode_event(&ev(
            "hook.denied",
            json!({
                "source": "authorization",
                "reason": "denied by the user",
                "recovery": "retry with a narrower scope",
            }),
        ));
        assert_eq!(
            d,
            GatewayEventKind::HookDenial {
                source: Some("authorization".to_string()),
                reason: Some("denied by the user".to_string()),
                recovery: Some("retry with a narrower scope".to_string()),
            }
        );
        // Partial payload: no recovery supplied -> None (the surface then
        // says "No automatic recovery supplied", verbatim-safe).
        let d = decode_event(&ev(
            "hook.denied",
            json!({ "source": "gateway", "reason": "denied" }),
        ));
        assert_eq!(
            d,
            GatewayEventKind::HookDenial {
                source: Some("gateway".to_string()),
                reason: Some("denied".to_string()),
                recovery: None,
            }
        );
    }

    #[test]
    fn clarify_request() {
        assert_eq!(
            decode_event(&ev(
                "clarify.request",
                json!({ "question": "which?", "requestId": "c1" })
            )),
            GatewayEventKind::Clarify {
                question: Some("which?".to_string()),
                request_id: Some("c1".to_string()),
            }
        );
    }

    #[test]
    fn session_info_full() {
        let d = decode_event(&ev(
            "session.info",
            json!({
                "running": false, "branch": "main", "cwd": "/w", "model": "m",
                "provider": "anthropic", "reasoning_effort": "high", "service_tier": "priority"
            }),
        ));
        assert_eq!(
            d,
            GatewayEventKind::SessionInfo {
                running: Some(false),
                branch: Some("main".to_string()),
                cwd: Some("/w".to_string()),
                model: Some("m".to_string()),
                provider: Some("anthropic".to_string()),
                reasoning_effort: Some("high".to_string()),
                service_tier: Some("priority".to_string()),
            }
        );
    }

    #[test]
    fn status_terminal_sudo_secret_review_error() {
        assert_eq!(
            decode_event(&ev("status.update", json!({ "kind": "compacting" }))),
            GatewayEventKind::StatusUpdate {
                kind: Some("compacting".to_string()),
                message: None
            }
        );
        // both terminal.read spellings map to the same variant
        for t in ["terminal.read", "terminal.read.request"] {
            assert_eq!(
                decode_event(&ev(t, json!({ "requestId": "t1" }))),
                GatewayEventKind::TerminalRead {
                    request_id: Some("t1".to_string())
                }
            );
        }
        assert_eq!(
            decode_event(&ev("sudo.request", json!({ "request_id": "u1" }))),
            GatewayEventKind::Sudo {
                request_id: Some("u1".to_string())
            }
        );
        assert_eq!(
            decode_event(&ev(
                "secret.request",
                json!({ "request_id": "k1", "envVar": "API", "prompt": "key?" })
            )),
            GatewayEventKind::Secret {
                request_id: Some("k1".to_string()),
                env_var: Some("API".to_string()),
                prompt: Some("key?".to_string()),
            }
        );
        assert_eq!(
            decode_event(&ev("review.summary", json!({ "text": "saved skill" }))),
            GatewayEventKind::ReviewSummary {
                text: Some("saved skill".to_string())
            }
        );
        assert_eq!(
            decode_event(&ev("error", json!({ "error": "boom" }))),
            GatewayEventKind::Error {
                message: Some("boom".to_string())
            }
        );
    }

    #[test]
    fn subagent_phases_and_cost() {
        let d = decode_event(&ev(
            "subagent.complete",
            json!({ "id": "a1", "parentId": "p1", "summary": "did it", "costUsd": 0.5 }),
        ));
        match d {
            GatewayEventKind::Subagent {
                phase,
                id,
                parent_id,
                summary,
                cost_usd,
                ..
            } => {
                assert_eq!(phase, SubagentPhase::Complete);
                assert_eq!(id.as_deref(), Some("a1"));
                assert_eq!(parent_id.as_deref(), Some("p1"));
                assert_eq!(summary.as_deref(), Some("did it"));
                // number cost stringified
                assert_eq!(cost_usd.as_deref(), Some("0.5"));
            }
            other => panic!("expected Subagent, got {other:?}"),
        }
        // string cost preserved; unknown suffix -> Other
        let d = decode_event(&ev(
            "subagent.weird",
            json!({ "id": "x", "costUsd": "1.25" }),
        ));
        match d {
            GatewayEventKind::Subagent {
                phase, cost_usd, ..
            } => {
                assert_eq!(phase, SubagentPhase::Other);
                assert_eq!(cost_usd.as_deref(), Some("1.25"));
            }
            other => panic!("expected Subagent, got {other:?}"),
        }
        assert!(matches!(
            decode_event(&ev("subagent.spawn_requested", json!({ "id": "a" }))),
            GatewayEventKind::Subagent {
                phase: SubagentPhase::SpawnRequested,
                ..
            }
        ));
    }

    #[test]
    fn subagent_decodes_unit18_reducer_fields() {
        // The unit-18 reducer feeds off these fields: taskCount/taskIndex,
        // top-level and usage-fallback token counts, toolCount, the file arrays,
        // sessionId, and currentTool.
        let d = decode_event(&ev(
            "subagent.tool",
            json!({
                "id": "a1",
                "sessionId": "sess-9",
                "taskCount": 3,
                "taskIndex": 1,
                "inputTokens": 120,
                "toolCount": 4,
                "filesRead": ["a.rs", "b.rs", 7],
                "filesWritten": ["c.rs"],
                "currentTool": "Edit",
                "usage": { "output": 55 }
            }),
        ));
        match d {
            GatewayEventKind::Subagent {
                session_id,
                task_count,
                task_index,
                input_tokens,
                output_tokens,
                tool_count,
                files_read,
                files_written,
                current_tool,
                ..
            } => {
                assert_eq!(session_id.as_deref(), Some("sess-9"));
                assert_eq!(task_count, Some(3));
                assert_eq!(task_index, Some(1));
                assert_eq!(input_tokens, Some(120));
                // top-level absent -> usage.output fallback.
                assert_eq!(output_tokens, Some(55));
                assert_eq!(tool_count, Some(4));
                // non-string array element (7) is dropped.
                assert_eq!(files_read, vec!["a.rs".to_string(), "b.rs".to_string()]);
                assert_eq!(files_written, vec!["c.rs".to_string()]);
                assert_eq!(current_tool.as_deref(), Some("Edit"));
            }
            other => panic!("expected Subagent, got {other:?}"),
        }
    }

    #[test]
    fn subagent_suffix_maps_to_its_specific_phase() {
        // Each `subagent.<suffix>` must decode to its OWN SubagentPhase variant.
        // The `every_known_event_type_is_non_unknown` gate only checks that these
        // are !Unknown — but dropping any specific arm (`start`/`thinking`/`tool`/
        // `progress`) would silently fall through to `_ => SubagentPhase::Other`,
        // which is still a `Subagent` (not `Unknown`), so that gate would still
        // pass. Only spawn_requested/complete are pinned elsewhere; the middle
        // four phase mappings were otherwise undistinguished. This pins all six
        // plus the unmodeled-suffix -> Other fallback.
        let cases = [
            ("spawn_requested", SubagentPhase::SpawnRequested),
            ("start", SubagentPhase::Start),
            ("thinking", SubagentPhase::Thinking),
            ("tool", SubagentPhase::Tool),
            ("progress", SubagentPhase::Progress),
            ("complete", SubagentPhase::Complete),
            // a suffix this build does not model -> Other
            ("teleport", SubagentPhase::Other),
        ];
        for (suffix, want) in cases {
            match decode_event(&ev(&format!("subagent.{suffix}"), json!({ "id": "a" }))) {
                GatewayEventKind::Subagent { phase, .. } => assert_eq!(
                    phase, want,
                    "subagent.{suffix} decoded to {phase:?}, expected {want:?}"
                ),
                other => panic!("subagent.{suffix}: expected Subagent, got {other:?}"),
            }
        }
    }

    #[test]
    fn ready_skin_and_unknown() {
        assert_eq!(
            decode_event(&ev("gateway.ready", json!({}))),
            GatewayEventKind::GatewayReady
        );
        assert!(matches!(
            decode_event(&ev("skin.changed", json!({ "name": "dark" }))),
            GatewayEventKind::SkinChanged { .. }
        ));
        match decode_event(&ev("totally.new.event", json!({ "a": 1 }))) {
            GatewayEventKind::Unknown {
                event_type,
                payload,
            } => {
                assert_eq!(event_type, "totally.new.event");
                assert_eq!(payload, Some(json!({ "a": 1 })));
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    /// Acceptance gate (plan unit 1): every event type the work.rs reducer
    /// matches must decode to a non-`Unknown` variant.
    #[test]
    fn every_known_event_type_is_non_unknown() {
        let known = [
            "message.start",
            "message.delta",
            "message.complete",
            "tool.start",
            "tool.progress",
            "tool.generating",
            "tool.complete",
            "approval.request",
            "hook.denied",
            "reasoning.delta",
            "reasoning.available",
            "thinking.delta",
            "clarify.request",
            "session.info",
            "status.update",
            "terminal.read",
            "terminal.read.request",
            "sudo.request",
            "secret.request",
            "review.summary",
            "error",
            "subagent.spawn_requested",
            "subagent.start",
            "subagent.thinking",
            "subagent.tool",
            "subagent.progress",
            "subagent.complete",
            "gateway.ready",
            "skin.changed",
        ];
        for t in known {
            assert!(
                !matches!(
                    decode_event(&ev(t, json!({}))),
                    GatewayEventKind::Unknown { .. }
                ),
                "event type {t} unexpectedly decoded to Unknown"
            );
        }
    }

    // --- turn-end detector ---------------------------------------------------

    fn complete() -> GatewayEventKind {
        GatewayEventKind::MessageComplete {
            text: None,
            status: None,
            cost_usd: None,
            context_percent: None,
            input_tokens: None,
            output_tokens: None,
        }
    }
    fn running(b: bool) -> GatewayEventKind {
        GatewayEventKind::SessionInfo {
            running: Some(b),
            branch: None,
            cwd: None,
            model: None,
            provider: None,
            reasoning_effort: None,
            service_tier: None,
        }
    }

    #[test]
    fn turn_end_complete_then_running_false() {
        let mut st = TurnState::new();
        assert!(!is_turn_end(&complete(), &mut st)); // complete alone: not yet
        assert!(!is_turn_end(&running(true), &mut st)); // still busy
        assert!(is_turn_end(&running(false), &mut st)); // now both -> end
        assert!(st.ended());
        // fires only once
        assert!(!is_turn_end(&running(false), &mut st));
    }

    #[test]
    fn turn_end_running_false_then_complete() {
        let mut st = TurnState::new();
        assert!(!is_turn_end(&running(false), &mut st)); // running:false alone: not yet
        assert!(is_turn_end(&complete(), &mut st)); // both -> end
        assert!(st.ended());
    }

    #[test]
    fn turn_end_error_is_terminal() {
        let mut st = TurnState::new();
        assert!(is_turn_end(
            &GatewayEventKind::Error {
                message: Some("x".to_string())
            },
            &mut st
        ));
        assert!(st.ended());
    }

    #[test]
    fn leading_stale_running_false_is_rearmed_by_running_true() {
        // A stale idle running:false, then the turn actually starts (running:true),
        // then completes. The early false must NOT pre-satisfy the conjunction.
        let mut st = TurnState::new();
        assert!(!is_turn_end(&running(false), &mut st)); // stale pre-turn idle
        assert!(!is_turn_end(&running(true), &mut st)); // turn now busy -> re-arm
        assert!(!is_turn_end(&complete(), &mut st)); // complete, but false was cleared
        assert!(!st.ended());
        // the turn's real running:false now ends it
        assert!(is_turn_end(&running(false), &mut st));
        assert!(st.ended());
    }

    #[test]
    fn running_false_alone_does_not_end() {
        // A premature running:false with no message.complete must not end the turn.
        let mut st = TurnState::new();
        assert!(!is_turn_end(&running(false), &mut st));
        assert!(!is_turn_end(
            &GatewayEventKind::MessageDelta { text: "x".into() },
            &mut st
        ));
        assert!(!st.ended());
    }

    fn message_start() -> GatewayEventKind {
        GatewayEventKind::MessageStart { model: None }
    }
    fn tool_start() -> GatewayEventKind {
        GatewayEventKind::Tool {
            phase: ToolPhase::Start,
            name: Some("search".into()),
            message: None,
            file_path: None,
        }
    }
    fn tool_done() -> GatewayEventKind {
        GatewayEventKind::ToolComplete {
            name: Some("search".into()),
            message: None,
            duration_s: None,
        }
    }
    fn delta() -> GatewayEventKind {
        GatewayEventKind::MessageDelta { text: "x".into() }
    }

    #[test]
    fn tool_call_turn_ends_on_final_completion() {
        // P3g: the tool-call message's completion is not the turn's; the turn
        // ends once, on running:false after the final completion.
        let mut st = TurnState::new();
        let seq = [
            running(true),
            complete(), // the tool-call message
            tool_start(),
            tool_done(),
            message_start(),
            delta(),
            complete(), // the final message
        ];
        for ev in &seq {
            assert!(!is_turn_end(ev, &mut st), "ended early on {ev:?}");
        }
        assert!(is_turn_end(&running(false), &mut st));
        assert!(st.ended());
        assert!(!is_turn_end(&running(false), &mut st));
    }

    #[test]
    fn running_false_before_final_completion_waits() {
        // P3g: running:false after a tool call's completion does not end the
        // turn; the final message's completion does (either order kept).
        let mut st = TurnState::new();
        let seq = [
            running(true),
            complete(), // the tool-call message
            tool_start(),
            tool_done(),
            running(false),
            message_start(),
            delta(),
        ];
        for ev in &seq {
            assert!(!is_turn_end(ev, &mut st), "ended early on {ev:?}");
        }
        assert!(is_turn_end(&complete(), &mut st));
        assert!(st.ended());
    }

    #[test]
    fn message_start_after_completion_rearms_before_running_false() {
        // P3g: complete, then a new message starts, then running:false — the
        // earlier completion no longer counts, so the turn is still open.
        let mut st = TurnState::new();
        assert!(!is_turn_end(&complete(), &mut st));
        assert!(!is_turn_end(&message_start(), &mut st));
        assert!(!is_turn_end(&running(false), &mut st));
        assert!(!st.ended());
        assert!(is_turn_end(&complete(), &mut st));
    }
}
