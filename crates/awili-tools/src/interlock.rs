//! P4b: the typed v6.5 "interlock" adapter verbs as pure tool definitions.
//! Four authz-controlled tools under the authenticated origin [`ORIGIN`]
//! (`bootstrap`, `recall`, `remember`, `archive_search`) mirroring the
//! `awili-memory` v6.5 verbs. Pure half, builtin pattern: canonical
//! [`ToolDef`]s (schema + canonical side-effect class + executor) installed
//! through the public registration path, with STRICT argument parsing.
//! Execution lives in `awili-app` (`work.rs`), routing every verb through
//! the typed `awili_memory::v65` client — nothing here is network.
//!
//! Lane discipline (acceptance 5): results label their lanes with the
//! constants below; lane sets are disjoint — archive evidence never blends
//! into canonical recall or mandatory policy.

#![forbid(unsafe_code)]

use crate::identity::{CanonicalDescriptor, ToolOrigin};
use crate::registry::{RegistryError, ToolDef, ToolRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The authenticated origin for the interlock verbs (P4b surface).
pub const ORIGIN: &str = "connector:interlock";
/// Executor identifier carried in every interlock descriptor.
pub const EXECUTOR: &str = "connector:interlock";

/// Tool name: read the mandatory policy + ordinary items once per Work boot.
pub const BOOTSTRAP: &str = "interlock.bootstrap";
/// Tool name: typed v6.5 recall against the canonical memory lane.
pub const RECALL: &str = "interlock.recall";
/// Tool name: the sole mutating verb — a provenance-complete memory write.
pub const REMEMBER: &str = "interlock.remember";
/// Tool name: raw archive evidence search (a distinct, read-only lane).
pub const ARCHIVE_SEARCH: &str = "interlock.archive_search";

/// Lane label: mandatory policy (owner instructions and directives).
pub const LANE_MANDATORY_POLICY: &str = "mandatory_policy";
/// Lane label: ordinary canonical recall items.
pub const LANE_RECALL_ITEMS: &str = "recall_items";
/// Lane label: raw archive hits — evidence, never canonical memory.
pub const LANE_ARCHIVE_EVIDENCE: &str = "archive_evidence";
/// Lane label: the canonical memory store (remember's write target).
pub const LANE_CANONICAL_MEMORY: &str = "canonical_memory";

/// The predicate vocabulary a `remember` write may use (freeform → 4xx).
pub const PREDICATES: [&str; 1] = ["legacy.note"];
/// The authority vocabulary (mirrors `awili_memory::v65::Authority`).
pub const AUTHORITIES: [&str; 7] = [
    "owner_instruction",
    "mechanically_verified",
    "canonical_documentation",
    "repository_state",
    "trusted_agent_report",
    "inference",
    "raw_history",
];
/// The epistemic-status vocabulary (P4b ships `asserted` only).
pub const EPISTEMIC_STATUSES: [&str; 1] = ["asserted"];
/// Inclusive upper bound for `archive_search.limit` (schema- and parse-checked).
pub const ARCHIVE_LIMIT_MAX: usize = 50;
/// Hard character bound on any rendered parse error (acceptance 6 class:
/// errors stay bounded and never carry token material).
pub const MAX_PARSE_ERROR_CHARS: usize = 512;

/// The authenticated origin for the interlock verbs.
pub fn origin() -> ToolOrigin {
    ToolOrigin::new(ORIGIN)
}

fn def(name: &str, side_effect: &str, schema: Value) -> ToolDef {
    ToolDef::from_parts(
        origin(),
        name,
        CanonicalDescriptor {
            schema,
            side_effect: side_effect.into(),
            required_capabilities: Vec::new(),
            executor: EXECUTOR.into(),
        },
    )
}

fn object_schema(props: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false,
    })
}

/// The four interlock tool definitions. Schemas mirror the parse structs:
/// `additionalProperties: false`, every property required, and the exact
/// wire-vocabulary enums — a foreign value is refused at parse time.
pub fn defs() -> Vec<ToolDef> {
    vec![
        def(
            BOOTSTRAP,
            "read_only",
            object_schema(Value::Object(Default::default()), &[]),
        ),
        def(
            RECALL,
            "read_only",
            object_schema(
                json!({"query": {"type": "string", "description": "The recall query."}}),
                &["query"],
            ),
        ),
        def(
            REMEMBER,
            "mutating",
            object_schema(
                json!({
                    "request_id": {"type": "string", "description": "Caller-supplied idempotency id; identical retries must reuse it."},
                    "subject": {"type": "string", "description": "The subject of the note."},
                    "predicate": {"type": "string", "enum": PREDICATES},
                    "object": {"type": "object", "description": "The JSON-object payload of the note."},
                    "authority": {"type": "string", "enum": AUTHORITIES},
                    "epistemic_status": {"type": "string", "enum": EPISTEMIC_STATUSES},
                    "source_type": {"type": "string", "description": "Which system wrote the note."},
                    "source_ref": {"type": "string", "description": "The originating reference (session, run, artifact)."},
                    "reason": {"type": "string", "description": "Why the note is being written."},
                }),
                &[
                    "request_id",
                    "subject",
                    "predicate",
                    "object",
                    "authority",
                    "epistemic_status",
                    "source_type",
                    "source_ref",
                    "reason",
                ],
            ),
        ),
        def(
            ARCHIVE_SEARCH,
            "read_only",
            object_schema(
                json!({
                    "query": {"type": "string", "description": "The archive query."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": ARCHIVE_LIMIT_MAX, "description": "Maximum raw hits to return."},
                }),
                &["query", "limit"],
            ),
        ),
    ]
}

/// Install the four interlock defs through the public registration path.
pub fn install(reg: &mut ToolRegistry) -> Result<(), RegistryError> {
    for d in defs() {
        reg.register(d)?;
    }
    Ok(())
}

/// Strict, typed `remember` arguments: the nine-field provenance shape,
/// `deny_unknown_fields`; vocabularies re-checked by [`parse`].
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RememberCall {
    /// Caller-supplied idempotency id (transport retries reuse it).
    pub request_id: String,
    /// The subject of the note.
    pub subject: String,
    /// The wire predicate (validated against [`PREDICATES`]).
    pub predicate: String,
    /// The JSON-object payload of the note.
    pub object: Value,
    /// The wire authority (validated against [`AUTHORITIES`]).
    pub authority: String,
    /// The wire epistemic status (validated against [`EPISTEMIC_STATUSES`]).
    pub epistemic_status: String,
    /// Which system wrote the note.
    pub source_type: String,
    /// The originating reference.
    pub source_ref: String,
    /// Why the note is being written.
    pub reason: String,
}

/// One parsed interlock tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterlockCall {
    /// `interlock.bootstrap` — takes no arguments.
    Bootstrap,
    /// `interlock.recall` — a query against the canonical memory lane.
    Recall { query: String },
    /// `interlock.remember` — the sole mutating verb. The provenance call is
    /// boxed so this wide variant does not bloat the whole enum.
    Remember(Box<RememberCall>),
    /// `interlock.archive_search` — raw archive evidence, bounded.
    ArchiveSearch { query: String, limit: usize },
}

/// A parse refusal: `Invalid` (known tool, bad arguments) or `UnknownTool`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterlockParseError {
    /// The tool is known but the arguments fail the strict contract.
    Invalid(String),
    /// The tool name is not one of the four interlock verbs.
    UnknownTool(String),
}

impl std::fmt::Display for InterlockParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InterlockParseError::Invalid(d) => write!(f, "invalid interlock arguments: {d}"),
            InterlockParseError::UnknownTool(n) => write!(f, "unknown interlock tool: {n}"),
        }
    }
}

impl std::error::Error for InterlockParseError {}

/// Strict arguments for `interlock.recall`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecallArgs {
    query: String,
}

/// Strict arguments for `interlock.archive_search`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveArgs {
    query: String,
    limit: usize,
}

fn invalid(detail: impl Into<String>) -> InterlockParseError {
    let detail: String = detail.into();
    InterlockParseError::Invalid(detail.chars().take(MAX_PARSE_ERROR_CHARS).collect())
}

/// A vocabulary field outside its wire list (the freeform guard at the tool
/// boundary, ahead of the service's final 4xx).
fn vocab(list: &[&str], field: &str, value: &str) -> Result<(), InterlockParseError> {
    list.contains(&value)
        .then_some(())
        .ok_or_else(|| invalid(format!("{field} '{value}' is outside the wire vocabulary")))
}

/// Parse a tool name + arguments into a typed [`InterlockCall`]. Strict:
/// unknown tools are `UnknownTool`; every verb demands an object (empty for
/// bootstrap); `deny_unknown_fields`; vocabularies and the limit bound are
/// re-checked so a hand-built call cannot bypass the schema.
pub fn parse(name: &str, arguments: &Value) -> Result<InterlockCall, InterlockParseError> {
    match name {
        BOOTSTRAP => {
            let Some(map) = arguments.as_object() else {
                return Err(invalid("bootstrap arguments must be a JSON object"));
            };
            if map.is_empty() {
                Ok(InterlockCall::Bootstrap)
            } else {
                Err(invalid("bootstrap takes no arguments"))
            }
        }
        RECALL => {
            let a: RecallArgs = serde_json::from_value(arguments.clone())
                .map_err(|e| invalid(format!("recall arguments: {e}")))?;
            Ok(InterlockCall::Recall { query: a.query })
        }
        ARCHIVE_SEARCH => {
            let a: ArchiveArgs = serde_json::from_value(arguments.clone())
                .map_err(|e| invalid(format!("archive_search arguments: {e}")))?;
            if !(1..=ARCHIVE_LIMIT_MAX).contains(&a.limit) {
                return Err(invalid(format!("limit must be 1..={ARCHIVE_LIMIT_MAX}")));
            }
            Ok(InterlockCall::ArchiveSearch {
                query: a.query,
                limit: a.limit,
            })
        }
        REMEMBER => {
            let call: RememberCall = serde_json::from_value(arguments.clone())
                .map_err(|e| invalid(format!("remember arguments: {e}")))?;
            if !call.object.is_object() {
                return Err(invalid("remember.object must be a JSON object"));
            }
            vocab(&PREDICATES, "predicate", &call.predicate)?;
            vocab(&AUTHORITIES, "authority", &call.authority)?;
            vocab(
                &EPISTEMIC_STATUSES,
                "epistemic_status",
                &call.epistemic_status,
            )?;
            Ok(InterlockCall::Remember(Box::new(call)))
        }
        other => {
            let name: String = other.chars().take(MAX_PARSE_ERROR_CHARS).collect();
            Err(InterlockParseError::UnknownTool(name))
        }
    }
}

/// A read-verb tool result: the two canonical lanes, labeled and disjoint.
/// Serialisation of the typed v6.5 values cannot fail in practice; if it ever
/// did, the lane is reported as `null` rather than panicking a tool call.
pub fn read_result(policy: &impl Serialize, items: &impl Serialize) -> Value {
    json!({
        "lanes": [LANE_MANDATORY_POLICY, LANE_RECALL_ITEMS],
        LANE_MANDATORY_POLICY: serde_json::to_value(policy).unwrap_or(Value::Null),
        LANE_RECALL_ITEMS: serde_json::to_value(items).unwrap_or(Value::Null),
    })
}

/// The `remember` tool result: the canonical-memory lane and the stored id.
pub fn remember_result(stored_id: Option<&str>) -> Value {
    json!({
        "lanes": [LANE_CANONICAL_MEMORY],
        LANE_CANONICAL_MEMORY: {"stored_id": stored_id},
    })
}

/// The `archive_search` tool result: raw evidence, its own lane only.
pub fn archive_result(hits: &impl Serialize) -> Value {
    json!({
        "lanes": [LANE_ARCHIVE_EVIDENCE],
        LANE_ARCHIVE_EVIDENCE: serde_json::to_value(hits).unwrap_or(Value::Null),
    })
}
