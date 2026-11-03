//! Run lifecycle types — the Work-mode reducer's event vocabulary.
//!
//! These are the typed run/status/approval shapes the Work surface
//! (`awili-app::work`) builds its `RunPanel` from. They were the last thing
//! `awili-app` pulled from the now-deleted `awili-hermes` crate (strangler plan
//! unit 17): the `/v1/runs` REST worker is gone (unit 16), so the only surviving
//! use of this vocabulary is the gateway worker's `gateway_event_to_update`
//! adapter, which *synthesizes* [`RunEvent`]s from typed gateway pushes so the
//! existing `apply_event` reducer keeps working unchanged.
//!
//! Ported 1:1 from `awili-hermes` (behavior-preserving): [`RunStatus`] keeps the
//! same fail-closed discipline (a non-`Completed` terminal is never a success),
//! and [`decode_run_event`] keeps the same lenient hyphen-normalizing decode so
//! an unrecognised event flows through as [`RunEvent::Unknown`] rather than
//! breaking the stream.
//!
//! [`ApprovalRisk`] (S2(a), correction RVS2A-FIX1) also lives here — the
//! closed risk vocabulary of the approval gate — and `awili-gateway`
//! re-exports it. The seam and the run events both carry it TYPED: a
//! missing, non-string, or unknown wire value decodes to
//! [`ApprovalRisk::Privileged`] (classification may only escalate, never
//! silently under-classify), so an unclassified risk string never rides the
//! offer path.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ============================================================================
// Run status
// ============================================================================

/// A run's lifecycle status. Mirrors `awili-ccd`'s `JobState` fail-closed
/// discipline: a non-`Completed` terminal state is **never** a success.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    /// Accepted; the server is booting the agent (the `202` body's status).
    Started,
    /// The agent loop is doing work.
    Running,
    /// Finished cleanly; `output` carries the result (terminal **success**).
    Completed,
    /// The agent errored out (terminal failure).
    Failed,
    /// Interrupted via `stop` (terminal, NOT success).
    Stopped,
    /// Any status string this client predates. Treated as non-terminal, non-success
    /// so polling keeps going; never crashes the decode. The original status string
    /// is **not** preserved — `#[serde(other)]` on a unit variant discards it.
    #[serde(other)]
    Unknown,
}

impl RunStatus {
    /// Whether polling can stop. `Completed`, `Failed`, and `Stopped` are terminal.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Stopped
        )
    }

    /// True **only** for a clean `Completed`. `Failed` and `Stopped` are terminal
    /// but NOT success — the same hollow-win discipline as ccd's `JobState`.
    pub fn is_success(self) -> bool {
        matches!(self, RunStatus::Completed)
    }
}

// ============================================================================
// Run lifecycle events
// ============================================================================

/// One typed lifecycle event, dispatched on the payload's `type` field (falling
/// back to the SSE `event:` name). Unrecognised types land in
/// [`RunEvent::Unknown`] carrying the raw payload, so a newer server's events
/// flow through the UI rather than breaking the stream.
#[derive(Clone, Debug, PartialEq)]
pub enum RunEvent {
    /// The agent published (or revised) its plan.
    Plan(PlanEvent),
    /// A tool started / progressed / finished.
    ToolProgress(ToolProgressEvent),
    /// The agent is blocked on a human decision — a deletion or dangerous command
    /// gate. Resolve via the gateway worker's approval channel.
    Approval(ApprovalEvent),
    /// An incremental token of assistant output.
    Token(TokenEvent),
    /// The run reached a terminal state; carries the final status and output.
    Completion(CompletionEvent),
    /// An event type this client does not model: the type string plus raw payload.
    Unknown { event_type: String, payload: Value },
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct PlanEvent {
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ToolProgressEvent {
    #[serde(default)]
    pub tool: Option<String>,
    /// e.g. `"started"`, `"running"`, `"completed"`, `"failed"`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ApprovalEvent {
    /// The id echoed back to resolve the approval.
    #[serde(default)]
    pub approval_id: Option<String>,
    /// What is being gated, e.g. `"deletion"`, `"dangerous_command"`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Human-readable description of the pending action.
    #[serde(default)]
    pub prompt: Option<String>,
    /// The TYPED risk class of the gated call (P3a, S2(a): closed
    /// vocabulary — `"read_only"`/`"mutating"`/`"irreversible"`/
    /// `"privileged"`). The card offers approval scopes from this;
    /// `always` is never offered for irreversible/privileged. Missing,
    /// non-string, or unknown wire values decode to
    /// [`ApprovalRisk::Privileged`] — the decode is conservative and never
    /// fails, so the approval event is never demoted to `Unknown` by a
    /// bad risk value.
    #[serde(default, deserialize_with = "deserialize_approval_risk")]
    pub risk: ApprovalRisk,
}

/// Lenient deserializer for [`ApprovalEvent::risk`]: reads the raw JSON value
/// (any shape — string, number, `null`, absent) and classifies it through the
/// conservative closed-set decode. It must NEVER error: a bad risk value
/// classifies as [`ApprovalRisk::Privileged`] and the event still decodes as
/// `RunEvent::Approval` — if it returned `Err`, the `typed!` macro in
/// `decode_run_event` would demote the whole approval to
/// `RunEvent::Unknown` and the gate would never show.
fn deserialize_approval_risk<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<ApprovalRisk, D::Error> {
    let raw: Option<serde_json::Value> = serde::Deserialize::deserialize(d)?;
    Ok(ApprovalRisk::decode(raw.as_ref().and_then(Value::as_str)))
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct TokenEvent {
    #[serde(default)]
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct CompletionEvent {
    #[serde(default)]
    pub status: Option<RunStatus>,
    #[serde(default)]
    pub output: Option<String>,
}

/// Parse one SSE `data:` payload (plus optional `event:` name) into a [`RunEvent`].
/// Pure and lenient: a payload whose `type` is unknown, or whose body does not fit
/// the modelled shape, becomes [`RunEvent::Unknown`] rather than an error — the
/// stream must never break on an event the client did not anticipate.
pub fn decode_run_event(event_name: Option<&str>, data: &str) -> RunEvent {
    let value: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => {
            return RunEvent::Unknown {
                event_type: event_name.unwrap_or("").to_string(),
                payload: Value::String(data.to_string()),
            }
        }
    };
    // Prefer the payload's own `type`; fall back to the SSE event name.
    let raw_type = value
        .get("type")
        .and_then(Value::as_str)
        .or(event_name)
        .unwrap_or("")
        .to_string();
    let normalized = raw_type.replace('-', "_");

    // Lenient typed decode: on a shape mismatch, fall back to Unknown.
    macro_rules! typed {
        ($variant:ident) => {
            match serde_json::from_value(value.clone()) {
                Ok(ev) => RunEvent::$variant(ev),
                Err(_) => RunEvent::Unknown {
                    event_type: raw_type.clone(),
                    payload: value.clone(),
                },
            }
        };
    }

    match normalized.as_str() {
        "plan" => typed!(Plan),
        "tool_progress" | "tool" => typed!(ToolProgress),
        "approval" | "approval_request" | "approval_required" => typed!(Approval),
        "token" | "tokens" | "delta" => typed!(Token),
        "completion" | "completed" | "done" => typed!(Completion),
        _ => RunEvent::Unknown {
            event_type: raw_type,
            payload: value,
        },
    }
}

// ============================================================================
// Approvals
// ============================================================================

/// The closed risk vocabulary of the approval gate (S2(a)): one type, four
/// classes — the decision path never carries an unclassified risk string: a
/// missing, non-string, or unknown wire value decodes to
/// [`ApprovalRisk::Privileged`] (classification may only escalate, never
/// silently under-classify).
///
/// The seam (`WorkEvent::ApprovalRequested.risk`) and the run event
/// (`ApprovalEvent.risk`) both carry this TYPED enum (S2(a), correction
/// RVS2A-FIX1): the gateway decode, the owned-engine seam, and the app's
/// `ApprovalCard` all speak the same closed set, so an unclassified string
/// never rides the offer path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ApprovalRisk {
    /// Read-only call: no state is touched.
    ReadOnly,
    /// Mutating call: state changes but reversibly.
    Mutating,
    /// Irreversible call: the effect cannot be undone.
    Irreversible,
    /// Privileged call, or any call whose declared class the gateway cannot
    /// recognize (missing / non-string / unknown wire value — S2(a): the
    /// decode is conservative by construction).
    #[default]
    Privileged,
}

impl ApprovalRisk {
    /// The closed vocabulary as wire strings (for the decode tests below —
    /// the decode is the only ingest path; `always` is offered only for the
    /// two reversible classes).
    pub const WIRE_VALUES: [&str; 4] = ["read_only", "mutating", "irreversible", "privileged"];

    /// Decode a wire value into the closed set: ASCII-whitespace-trimmed,
    /// case-insensitive. `None` (missing), a non-string value, or an unknown
    /// string all classify as [`ApprovalRisk::Privileged`] — never an
    /// unclassified string.
    pub fn decode(wire: Option<&str>) -> Self {
        match wire {
            None => Self::Privileged,
            Some(s) => Self::from_wire(s.trim_ascii().to_lowercase().as_str()),
        }
    }

    /// The conservative parse used by the decode: recognized (lowercase,
    /// trimmed) values map to their class; anything else is `Privileged`.
    fn from_wire(lower_trimmed: &str) -> Self {
        match lower_trimmed {
            "read_only" => Self::ReadOnly,
            "mutating" => Self::Mutating,
            "irreversible" => Self::Irreversible,
            "privileged" => Self::Privileged,
            _ => Self::Privileged,
        }
    }

    /// The frozen wire string for this class (C1 contract v2 §3) — the
    /// canonical vocabulary the seam (`WorkEvent::ApprovalRequested.risk`)
    /// carries as the typed enum.
    pub fn to_wire(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Mutating => "mutating",
            Self::Irreversible => "irreversible",
            Self::Privileged => "privileged",
        }
    }
}

/// A resolution for a pending approval gate (P3a: scoped approval).
///
/// The five variants map 1:1 onto the exact scope strings the engines
/// understand over the wire — see [`ApprovalDecision::scope_str`]. The
/// surface MUST send the verbatim scope string (never relabel an
/// unsupported choice as `once`), and the authorizer stays the minting
/// authority: it refuses `always` for irreversible/privileged risk and
/// `install` unconditionally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalDecision {
    /// Approve this call once (`"once"`).
    Approve,
    /// Approve for the rest of this session (`"session"`).
    Session,
    /// Approve for the bound folder (`"folder"`).
    Folder,
    /// Approve permanently (`"always"`).
    Always,
    /// Deny (`"deny"`).
    Deny,
}

impl ApprovalDecision {
    /// The exact scope string this decision sends over the wire:
    /// `"once" | "session" | "folder" | "always" | "deny"`.
    pub fn scope_str(self) -> &'static str {
        match self {
            ApprovalDecision::Approve => "once",
            ApprovalDecision::Session => "session",
            ApprovalDecision::Folder => "folder",
            ApprovalDecision::Always => "always",
            ApprovalDecision::Deny => "deny",
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_status_failed_and_stopped_are_terminal_not_success() {
        for (s, json) in [
            (RunStatus::Failed, r#""failed""#),
            (RunStatus::Stopped, r#""stopped""#),
        ] {
            let st: RunStatus = serde_json::from_str(json).unwrap();
            assert_eq!(st, s);
            assert!(st.is_terminal(), "{s:?} stops polling");
            assert!(!st.is_success(), "{s:?} is NOT success");
        }
    }

    #[test]
    fn run_status_completed_is_terminal_success() {
        let st: RunStatus = serde_json::from_str(r#""completed""#).unwrap();
        assert_eq!(st, RunStatus::Completed);
        assert!(st.is_terminal());
        assert!(st.is_success());
    }

    #[test]
    fn run_status_unknown_does_not_crash() {
        let st: RunStatus = serde_json::from_str(r#""some_future_state""#).unwrap();
        assert_eq!(st, RunStatus::Unknown);
        assert!(!st.is_terminal());
        assert!(!st.is_success());
    }

    #[test]
    fn decode_run_event_normalizes_hyphen_and_uses_type() {
        let ev = decode_run_event(
            None,
            r#"{"type":"tool-progress","tool":"bash","status":"running"}"#,
        );
        match ev {
            RunEvent::ToolProgress(t) => {
                assert_eq!(t.tool.as_deref(), Some("bash"));
                assert_eq!(t.status.as_deref(), Some("running"));
            }
            other => panic!("expected ToolProgress, got {other:?}"),
        }
    }

    #[test]
    fn decode_run_event_falls_back_to_event_name() {
        // No `type` in payload — the SSE event name decides the variant.
        let ev = decode_run_event(
            Some("approval"),
            r#"{"approval_id":"a1","kind":"deletion"}"#,
        );
        match ev {
            RunEvent::Approval(a) => {
                assert_eq!(a.approval_id.as_deref(), Some("a1"));
                assert_eq!(a.kind.as_deref(), Some("deletion"));
            }
            other => panic!("expected Approval, got {other:?}"),
        }
    }

    #[test]
    fn decode_run_event_unknown_type_is_preserved_not_dropped() {
        let ev = decode_run_event(None, r#"{"type":"heartbeat","seq":7}"#);
        match ev {
            RunEvent::Unknown {
                event_type,
                payload,
            } => {
                assert_eq!(event_type, "heartbeat");
                assert_eq!(payload.get("seq").and_then(Value::as_i64), Some(7));
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn decode_run_event_non_json_does_not_panic() {
        let ev = decode_run_event(Some("noise"), "not json at all");
        assert!(matches!(ev, RunEvent::Unknown { .. }));
    }

    #[test]
    fn decode_run_event_known_type_wrong_shape_falls_back_to_unknown() {
        // A payload whose `type` names a modelled variant but whose body does NOT
        // fit that variant's shape must degrade to `Unknown` (carrying the raw type
        // and untouched payload), never error/panic — the `typed!` macro's `Err(_)`
        // arm. `steps` is typed `Vec<String>`, so a string there fails the typed
        // decode. (Every other decode test feeds well-shaped data, so this is the
        // only exercise of the lenient shape-mismatch fallback documented at the
        // module head.)
        let ev = decode_run_event(None, r#"{"type":"plan","steps":"not-an-array"}"#);
        match ev {
            RunEvent::Unknown {
                event_type,
                payload,
            } => {
                assert_eq!(event_type, "plan", "raw type preserved on shape mismatch");
                assert_eq!(
                    payload.get("steps").and_then(Value::as_str),
                    Some("not-an-array"),
                    "original payload flows through untouched"
                );
            }
            other => panic!("expected Unknown on shape mismatch, got {other:?}"),
        }
    }

    #[test]
    fn decode_run_event_payload_type_wins_over_event_name() {
        // When BOTH the payload's `type` and the SSE `event:` name are present and
        // disagree, the payload's `type` decides the variant (`.get("type")` is
        // preferred, `.or(event_name)` is only the fallback). Here `type:"plan"`
        // must beat the conflicting event name `"token"`.
        let ev = decode_run_event(Some("token"), r#"{"type":"plan","steps":["draft"]}"#);
        match ev {
            RunEvent::Plan(p) => assert_eq!(p.steps, vec!["draft".to_string()]),
            other => panic!("expected Plan (payload type wins), got {other:?}"),
        }
    }

    #[test]
    fn decode_run_event_secondary_type_aliases_map_to_their_variants() {
        // Each modelled variant accepts more than one wire-format `type` spelling
        // (a deliberate server-compat contract in the `match normalized` arm). Every
        // OTHER decode test feeds only the *primary* spelling of a variant
        // (`tool-progress`, `approval`, `completion`), so the secondary aliases below
        // are undistinguished — dropping any of them from the match arm would silently
        // degrade that event to `Unknown` with no existing test failing. Pin each.
        // Non-tautological: fails the moment an alias is removed from the match arm
        // (the event would then decode to `Unknown` and miss its expected variant).
        #[allow(clippy::type_complexity)] // verbatim from awili-gateway; the table IS the test
        let cases: &[(&str, fn(&RunEvent) -> bool)] = &[
            ("tool", |e| matches!(e, RunEvent::ToolProgress(_))),
            ("approval_request", |e| matches!(e, RunEvent::Approval(_))),
            ("approval_required", |e| matches!(e, RunEvent::Approval(_))),
            ("tokens", |e| matches!(e, RunEvent::Token(_))),
            ("delta", |e| matches!(e, RunEvent::Token(_))),
            ("completed", |e| matches!(e, RunEvent::Completion(_))),
            ("done", |e| matches!(e, RunEvent::Completion(_))),
        ];
        for (alias, is_expected) in cases {
            let data = format!(r#"{{"type":"{alias}"}}"#);
            let ev = decode_run_event(None, &data);
            assert!(
                is_expected(&ev),
                "alias `{alias}` must decode to its modelled variant, got {ev:?}"
            );
        }
    }

    #[test]
    fn completion_event_defaults_status_absent() {
        // A completion carrying no explicit status decodes to None (the reducer
        // treats a statusless completion as a clean Completed).
        let ev = decode_run_event(None, r#"{"type":"completion","output":"PONG"}"#);
        match ev {
            RunEvent::Completion(c) => {
                assert_eq!(c.status, None);
                assert_eq!(c.output.as_deref(), Some("PONG"));
            }
            other => panic!("expected Completion, got {other:?}"),
        }
    }

    #[test]
    fn approval_decision_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ApprovalDecision::Approve).unwrap(),
            "\"approve\""
        );
        assert_eq!(
            serde_json::to_string(&ApprovalDecision::Deny).unwrap(),
            "\"deny\""
        );
    }

    /// P3a: the five decisions carry the EXACT scope strings the engines
    /// mint grants for — never a relabeled value.
    #[test]
    fn approval_decision_scope_strings_are_exact() {
        assert_eq!(ApprovalDecision::Approve.scope_str(), "once");
        assert_eq!(ApprovalDecision::Session.scope_str(), "session");
        assert_eq!(ApprovalDecision::Folder.scope_str(), "folder");
        assert_eq!(ApprovalDecision::Always.scope_str(), "always");
        assert_eq!(ApprovalDecision::Deny.scope_str(), "deny");
    }

    /// P3a/S2(a): `ApprovalEvent` carries the TYPED risk class when the
    /// engine reports it, and decodes leniently (absent risk = the
    /// conservative `Privileged`) for older streams.
    #[test]
    fn approval_event_carries_risk_and_decodes_leniently() {
        let with_risk: ApprovalEvent = serde_json::from_str(
            r#"{"approval_id":"a1","kind":"command","prompt":"rm","risk":"irreversible"}"#,
        )
        .unwrap();
        assert_eq!(with_risk.risk, ApprovalRisk::Irreversible);
        let no_risk: ApprovalEvent =
            serde_json::from_str(r#"{"approval_id":"a1","kind":"command"}"#).unwrap();
        assert_eq!(no_risk.risk, ApprovalRisk::Privileged);
    }

    /// S2(a) correction (RVS2A-FIX1): the lenient deserializer classifies
    /// EVERY bad risk payload as `Privileged` and the event still decodes as
    /// `RunEvent::Approval` — never `Unknown` (a deserializer error would
    /// demote the whole approval and the gate would never show).
    #[test]
    fn approval_event_risk_decodes_conservatively() {
        let cases: &[(&str, ApprovalRisk)] = &[
            (r#""Read_Only""#, ApprovalRisk::ReadOnly),
            (r#"" mutating ""#, ApprovalRisk::Mutating),
            (r#""IRREVERSIBLE""#, ApprovalRisk::Irreversible),
            (r#""privileged""#, ApprovalRisk::Privileged),
            (r#""root""#, ApprovalRisk::Privileged),
            (r#"42"#, ApprovalRisk::Privileged),
            (r#"null"#, ApprovalRisk::Privileged),
        ];
        for (payload, class) in cases {
            let data = format!(
                r#"{{"type":"approval","approval_id":"a1","prompt":"p","risk":{payload}}}"#
            );
            let ev = decode_run_event(None, &data);
            match ev {
                RunEvent::Approval(a) => {
                    assert_eq!(a.risk, *class, "risk payload {payload:?}")
                }
                other => panic!("risk {payload:?} must decode to Approval, got {other:?}"),
            }
        }
        // Key absent: the conservative class, still an Approval event.
        let data = r#"{"type":"approval","approval_id":"a1","prompt":"p"}"#;
        let ev = decode_run_event(None, data);
        match ev {
            RunEvent::Approval(a) => assert_eq!(a.risk, ApprovalRisk::Privileged),
            other => panic!("absent risk must decode to Approval, got {other:?}"),
        }
    }

    /// End-to-end coverage of the ported decoder over the representative run SSE
    /// stream fixture (moved from awili-hermes with the types). A tiny inline SSE
    /// splitter — comments (`:`) skipped, `event:`/`data:` accumulated per
    /// blank-line-delimited block — feeds each event through [`decode_run_event`],
    /// exactly as the run stream did, without depending on the deleted `SseParser`.
    #[test]
    fn run_events_sse_fixture_parses_into_typed_events() {
        let raw = include_str!("../fixtures/run-events.sse");
        let mut events: Vec<RunEvent> = Vec::new();
        let mut cur_event: Option<String> = None;
        let mut cur_data: Vec<String> = Vec::new();
        let flush = |ev: &mut Option<String>, data: &mut Vec<String>, out: &mut Vec<RunEvent>| {
            if !data.is_empty() {
                out.push(decode_run_event(ev.as_deref(), &data.join("\n")));
            }
            *ev = None;
            data.clear();
        };
        for line in raw.lines() {
            if line.starts_with(':') {
                continue; // SSE comment / keepalive
            }
            if line.is_empty() {
                flush(&mut cur_event, &mut cur_data, &mut events);
            } else if let Some(name) = line.strip_prefix("event:") {
                cur_event = Some(name.trim().to_string());
            } else if let Some(d) = line.strip_prefix("data:") {
                cur_data.push(d.trim_start().to_string());
            }
        }
        flush(&mut cur_event, &mut cur_data, &mut events);

        assert!(
            events.iter().any(|e| matches!(e, RunEvent::Plan(_))),
            "expected a plan event"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RunEvent::ToolProgress(_))),
            "expected a tool-progress event"
        );
        assert!(
            events.iter().any(|e| matches!(e, RunEvent::Approval(_))),
            "expected an approval event"
        );
        assert!(
            events.iter().any(|e| matches!(e, RunEvent::Token(_))),
            "expected a token event"
        );
        // The terminal completion carries the verified PONG output.
        let completion = events
            .iter()
            .find_map(|e| {
                if let RunEvent::Completion(c) = e {
                    Some(c)
                } else {
                    None
                }
            })
            .expect("expected a completion event");
        assert_eq!(completion.status, Some(RunStatus::Completed));
        assert_eq!(completion.output.as_deref(), Some("PONG"));
    }
}
