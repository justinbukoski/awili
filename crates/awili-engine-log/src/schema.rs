//! Versioned persistence DTOs — the FROZEN B2 interface C1 depends on.
//!
//! Explicit serde tags everywhere: renaming a Rust identifier must never be
//! able to change the wire form. Runtime types (`awili_engine::*`) convert at
//! the boundary; they are not serialized directly (Codex B1 review P2).
//! Vocabulary is append-only: new variants may be added, existing tags are
//! immutable forever.

use serde::{Deserialize, Serialize};

/// Bumped only for incompatible framing/record changes. Readers accept
/// `<= SCHEMA_VERSION` and must tolerate unknown record tags within a
/// version (skip + count, never error).
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum LogRecordV1 {
    /// First record of every log file.
    #[serde(rename = "header")]
    Header { version: u32, session_id: String },

    #[serde(rename = "turn_opened")]
    TurnOpened {
        turn_id: String,
        provider: String,
        model: String,
    },

    /// Canonical transcript append. `seq_in_turn` is per-turn monotonic.
    #[serde(rename = "event")]
    EventAppended {
        turn_id: String,
        seq_in_turn: u64,
        event: CanonicalEventV1,
    },

    /// WRITE-AHEAD INTENT: durably appended BEFORE the side-effect dispatch.
    ///
    /// Dedupe doctrine (adopted from Set A, arbitration 2026-08-06):
    /// `intent_id` is the SOLE execution-dedupe authority. `call_fingerprint`
    /// is log-computed (SHA-256 of tool + canonical_args), advisory-only —
    /// correlation heuristics, never re-dispatch gating. `raw_arguments` is
    /// verbatim; malformed JSON is persisted for audit with
    /// `canonical_args: None` and is NOT dispatch-eligible.
    #[serde(rename = "intent")]
    Intent {
        turn_id: String,
        intent_id: String,
        awili_call_id: String,
        tool: String,
        /// Verbatim argument JSON exactly as received.
        raw_arguments: String,
        /// Sorted-key canonical form; None when raw JSON is malformed.
        canonical_args: Option<String>,
        /// Advisory SHA-256 hex over tool + canonical_args; None if malformed.
        call_fingerprint: Option<String>,
        argument_validation: ArgumentValidationV1,
        /// C1 approval reference ("" until C1 exists).
        approval_ref: String,
        /// C1 risk class; replay policy keys on it. "unknown" = max caution.
        risk_class: String,
        /// "policy" | "grant" | "ask" | "" (pre-C1). C1 contract §7.
        #[serde(default)]
        decision_basis: String,
        /// Evidence-bound normalized-call hash; "" pre-C1. Replay recomputes
        /// from (turn_id, awili_call_id, tool, canonical_args) and classifies
        /// mismatch AuthzInvalid (C1 contract §9).
        #[serde(default)]
        evidence_call_hash: String,
        /// Canonical digest (SHA-256 hex) of the registry-resolved tool
        /// identity the evidence bound (unit 4, design r3 Delta 2) — the
        /// origin+name+descriptor-digest form, evidence-borne (never a free
        /// caller argument), uniform across the builtin and mcp legs. ""
        /// pre-unit-4; replay exposes the persisted value.
        #[serde(default)]
        identity_digest: String,
    },

    #[serde(rename = "dispatch_started")]
    DispatchStarted { intent_id: String },

    #[serde(rename = "dispatch_finalized")]
    DispatchFinalized {
        intent_id: String,
        ok: bool,
        /// Result content (or error text), preserved for the transcript.
        content: String,
    },

    /// C1 writes these. Shape per C1 contract v2 §5 (pre-freeze amendment
    /// 2026-08-06 — the LAST amendment; schema is FROZEN from this commit).
    #[serde(rename = "approval")]
    ApprovalRecorded {
        approval_ref: String,
        decision: String, // "approved" | "denied"
        scope: String,    // "once" | "session" | "folder" | "always"
        #[serde(default)]
        turn_id: String,
        #[serde(default)]
        awili_call_id: String,
        /// Over (tool + canonicalized arguments) of the originating call.
        #[serde(default)]
        normalized_call_hash: String,
        #[serde(default)]
        session_id: String,
        #[serde(default)]
        tool: String,
        #[serde(default)]
        risk_class: String,
        #[serde(default)]
        folder_root: Option<String>,
        /// JSON: argument patterns the grant is limited to.
        #[serde(default)]
        constraints: String,
        /// When the approval was given, milliseconds since the Unix epoch.
        /// Additive (S6): absent on records written before it existed, and
        /// omitted from the bytes when absent so old records re-encode
        /// unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        granted_at_ms: Option<u64>,
    },

    /// C1 denial audit record (contract §7): richer than a tool-result error.
    #[serde(rename = "authz_denied")]
    AuthzDenied {
        turn_id: String,
        awili_call_id: String,
        tool: String,
        risk_class: String,
        reason: String,
    },

    /// Grant revocation (grant store only; append-only, latest wins).
    #[serde(rename = "grant_revoked")]
    GrantRevoked { approval_ref: String },

    /// Unit 5 (design r9, arbitrated schema amendment): one per approval
    /// imported by the per-user store migration. Old execution logs whose
    /// evidence still names a legacy `approval_ref` resolve through these;
    /// an old ref matching ZERO or MULTIPLE aliases is authorization-
    /// invalid, never authorizing (r5 verdict ruling 10). Grant store only.
    #[serde(rename = "migration_alias")]
    MigrationAlias {
        /// Full-width source identity:
        /// sha256(canonical_path_bytes || 0x00 || sha256(file_bytes)), hex.
        source_id: String,
        old_ref: String,
        new_ref: String,
    },

    /// Unit 5 (design r3 §3 as ratified, verdict ruling 12): ONE framed
    /// record makes an install seed + its supersessions atomic — a torn
    /// write leaves the complete OLD state (the tail is repaired before
    /// the next append), so no dangling supersession or half transition
    /// can ever be reconstructed. Grant-store lane ONLY. The embedded
    /// approval is implicitly `decision:"approved", scope:"install"`;
    /// `superseded_by` for every ref in `supersedes` is implicitly this
    /// record's `approval_ref` — machine supersession, a DISTINCT kind
    /// from user `GrantRevoked`. One record per (origin, tool): per-key
    /// transitions are individually atomic.
    #[serde(rename = "install_transition")]
    InstallTransition {
        approval_ref: String,
        session_id: String,
        tool: String,
        risk_class: String,
        /// The strict three-key identity binding:
        /// {"__origin", "__setup_id", "__pinned_requirement"}.
        constraints: String,
        /// OLDER active install approvals of the same (origin, tool),
        /// machine-retired by this transition.
        supersedes: Vec<String>,
    },

    /// Unit 5: the migration commit marker, written LAST inside the staged
    /// file so "store exists => migration committed" holds by construction
    /// (atomic rename). Grant store only; at most one per store.
    #[serde(rename = "store_migrated")]
    StoreMigrated {
        /// Sorted by `source_id`; identities stored explicitly, never
        /// recomputed on read (r8 verdict revision 1).
        sources: Vec<MigratedSource>,
        total: u64,
    },

    /// Periodic recovery accelerator: serialized TurnState DTO. Opaque JSON
    /// here; the writer validates on restore.
    #[serde(rename = "checkpoint")]
    Checkpoint { turn_id: String, state_json: String },

    /// Terminal turn marker (completed/cancelled/failed + detail).
    #[serde(rename = "turn_closed")]
    TurnClosed { turn_id: String, outcome: String },
}

/// RC2-ENG1 (F0570): every record tag of [`LogRecordV1`] this build knows.
/// A frame whose `t` is NOT in this list is a record from a newer build of
/// the same schema version: readers skip and count it (the contract at
/// [`SCHEMA_VERSION`]). A frame with a known tag that fails to decode is
/// still corrupt.
pub const KNOWN_RECORD_TAGS: [&str; 14] = [
    "header",
    "turn_opened",
    "event",
    "intent",
    "dispatch_started",
    "dispatch_finalized",
    "approval",
    "authz_denied",
    "grant_revoked",
    "migration_alias",
    "install_transition",
    "store_migrated",
    "checkpoint",
    "turn_closed",
];

/// RC2-ENG1 (F0570): one decoded frame.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodedRecord {
    Known(Box<LogRecordV1>),
    /// A well-formed record whose tag this build does not know.
    Unknown {
        tag: String,
        value: serde_json::Value,
    },
}

/// RC2-ENG1 (F0570): decode one frame's bytes. A JSON object whose `t` is a
/// string outside [`KNOWN_RECORD_TAGS`] is [`DecodedRecord::Unknown`];
/// anything else that does not decode is the decoder's error.
pub fn decode_record(bytes: &[u8]) -> Result<DecodedRecord, serde_json::Error> {
    match serde_json::from_slice::<LogRecordV1>(bytes) {
        Ok(rec) => Ok(DecodedRecord::Known(Box::new(rec))),
        Err(e) => match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(value) => match value.get("t").and_then(|t| t.as_str()) {
                Some(tag) if !KNOWN_RECORD_TAGS.contains(&tag) => Ok(DecodedRecord::Unknown {
                    tag: tag.to_string(),
                    value,
                }),
                _ => Err(e),
            },
            Err(_) => Err(e),
        },
    }
}

/// One migrated legacy source's identity, stored explicitly in
/// [`LogRecordV1::StoreMigrated`] (unit 5 design r9 §1 / r8 verdict rev 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigratedSource {
    pub source_id: String,
    /// Canonical filesystem path bytes, lossless where the platform allows;
    /// stored as a string for the frozen NDJSON framing.
    pub canonical_path: String,
    /// sha256 of the exact file bytes that were parsed, hex.
    pub sha256_bytes: String,
    pub imported_record_count: u64,
}

/// Whether an intent's raw argument JSON parsed (Set A model).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v")]
pub enum ArgumentValidationV1 {
    #[serde(rename = "valid")]
    Valid,
    #[serde(rename = "malformed")]
    Malformed {
        /// Bounded diagnostic (writer truncates).
        diagnostic: String,
    },
}

/// Storage DTO for canonical events. Deliberately COARSER than the runtime
/// `AgentEvent`: per-token Text/Reasoning deltas are coalesced by the writer
/// (audit value low, cost high — design sketch Q2); everything with audit
/// value is verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k")]
pub enum CanonicalEventV1 {
    #[serde(rename = "turn_started")]
    TurnStarted,
    /// Coalesced assistant text for a segment.
    #[serde(rename = "text")]
    Text { content: String },
    /// Continuation artifact: opaque, provenance-tagged, never interpreted.
    #[serde(rename = "blob")]
    ContinuationBlob {
        provider: String,
        model: String,
        turn: u32,
        kind: String,
        /// Verbatim provider bytes (JSON array encoding; opaque).
        payload: Vec<u8>,
    },
    #[serde(rename = "tool_call")]
    ToolCallReady {
        awili_call_id: String,
        provider_call_id: Option<String>,
        name: String,
        arguments: String,
    },
    #[serde(rename = "tool_call_invalid")]
    ToolCallInvalid {
        awili_call_id: String,
        name: Option<String>,
        raw_arguments: String,
        reason: String,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        awili_call_id: String,
        content: String,
        is_error: bool,
    },
    #[serde(rename = "usage")]
    Usage {
        prompt_tokens: u64,
        completion_tokens: u64,
    },
    #[serde(rename = "input_rejected")]
    InputRejected { reason: String },
    #[serde(rename = "completed")]
    Completed { stop: String },
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "failed")]
    Failed { error: String },
}
