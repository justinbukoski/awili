//! ExecutionLog: the write API. Enforces the write-ahead intent invariant in
//! the type flow — `intent()` returns an [`IntentTicket`] only AFTER the
//! durable append succeeds, and the host needs the ticket to record dispatch.

use awili_engine::events::{AgentEvent, ToolCall};
use awili_engine::EngineError;

use crate::authz::{
    AuthzEvidence, CallBinding, GrantProof, HostApprovalCapability, MintedProof, OnceProof,
    RiskClass,
};

use crate::frame;
use crate::schema::{decode_record, CanonicalEventV1, DecodedRecord, LogRecordV1, SCHEMA_VERSION};
use crate::storage::AppendOnlyLog;
use crate::LogError;

fn trailing_seq(id: &str) -> Option<u64> {
    id.rsplit('-').next()?.parse().ok()
}

/// RC2-ENG1 (F0570): a record from a newer build is skipped, but the ids it
/// carries are still consumed — a later intent or approval of this build
/// must never be minted with the same id.
fn reserve_unknown_ids(
    value: &serde_json::Value,
    next_intent_seq: &mut u64,
    next_approval_seq: &mut u64,
) {
    if let Some(n) = value
        .get("intent_id")
        .and_then(|v| v.as_str())
        .and_then(trailing_seq)
    {
        *next_intent_seq = (*next_intent_seq).max(n + 1);
    }
    if let Some(n) = value
        .get("approval_ref")
        .and_then(|v| v.as_str())
        .and_then(trailing_seq)
    {
        *next_approval_seq = (*next_approval_seq).max(n + 1);
    }
}

/// Reserve the approval sequence past every REF that names an imported
/// approval (S5(c), F0565): `ApprovalRecorded` and `InstallTransition`
/// refs, BOTH sides of a `MigrationAlias` (the old legacy ref is what
/// execution-log evidence still names), and — the orphan case — a
/// `GrantRevoked` ref that no approval in the store answers to. The
/// store keeps such a revocation verbatim for audit, but its sequence
/// must still be consumed: otherwise a later grant can be minted with the
/// same ref and the orphan revocation silently cancels a live grant.
/// `resync` re-runs the same rule over current durable state.
fn reserve_approval_refs(recs: &[LogRecordV1], next_approval_seq: &mut u64) {
    let mut known: std::collections::BTreeSet<String> = recs
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::ApprovalRecorded { approval_ref, .. }
            | LogRecordV1::InstallTransition { approval_ref, .. } => Some(approval_ref.clone()),
            LogRecordV1::MigrationAlias { new_ref, .. } => Some(new_ref.clone()),
            _ => None,
        })
        .collect();
    for rec in recs {
        let refs: Vec<String> = match rec {
            LogRecordV1::ApprovalRecorded { approval_ref, .. }
            | LogRecordV1::InstallTransition { approval_ref, .. } => {
                vec![approval_ref.clone()]
            }
            LogRecordV1::MigrationAlias {
                old_ref, new_ref, ..
            } => {
                vec![old_ref.clone(), new_ref.clone()]
            }
            // Orphan revocation: the ref names no approval and no alias —
            // its sequence is reserved anyway (F0565).
            LogRecordV1::GrantRevoked { approval_ref } if !known.contains(approval_ref) => {
                known.insert(approval_ref.clone());
                vec![approval_ref.clone()]
            }
            _ => Vec::new(),
        };
        for r in refs {
            if let Some(n) = trailing_seq(&r) {
                *next_approval_seq = (*next_approval_seq).max(n + 1);
            }
        }
    }
}

/// Advisory call fingerprint (Set A doctrine): SHA-256 hex over
/// `tool + 0x00 + canonical_args`. Log-computed, never caller-supplied,
/// NEVER a dedupe gate — `intent_id` is the sole dedupe authority.
pub fn call_fingerprint(tool: &str, canonical_args: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(tool.as_bytes());
    h.update([0u8]);
    h.update(canonical_args.as_bytes());
    format!("{:x}", h.finalize())
}

/// Canonicalize argument JSON: parse + re-serialize with sorted keys so
/// semantically identical calls yield identical fingerprints. Empty or
/// whitespace-only input canonicalizes to `{}` (zero-arg tools).
pub fn canonicalize_args(raw: &str) -> Result<String, LogError> {
    if raw.trim().is_empty() {
        return Ok("{}".to_string());
    }
    let v: serde_json::Value = serde_json::from_str(raw)?;
    // serde_json::Value::Object preserves insertion order by default unless
    // the "preserve_order" feature is off; sort explicitly to be safe.
    fn sort(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(m) => {
                let mut keys: Vec<_> = m.keys().cloned().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for k in keys {
                    out.insert(k.clone(), sort(&m[&k]));
                }
                serde_json::Value::Object(out)
            }
            serde_json::Value::Array(a) => serde_json::Value::Array(a.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    Ok(serde_json::to_string(&sort(&v))?)
}

/// What one install seed actually appended (unit 5): the degradation
/// surface names what was skipped and why; repeated seeds report
/// everything under `tombstoned` and append nothing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct InstallSeedOutcome {
    pub seeded: Vec<String>,
    pub skipped_irreversible: Vec<String>,
    /// Tools whose exact logical key already holds ANY approval — active,
    /// revoked, or superseded (ruling 5): never auto-reseeded.
    pub tombstoned: Vec<String>,
    /// Older active refs machine-retired by this seed's transitions.
    pub superseded: Vec<String>,
}

/// Proof that an Intent record is durable. Required to record dispatch.
/// Private fields, not Clone (Codex C1p1 P0): external code cannot
/// fabricate a ticket for dispatch_started/finalized.
#[derive(Debug, PartialEq, Eq)]
pub struct IntentTicket {
    pub(crate) intent_id: String,
    pub(crate) call_fingerprint: String,
}

impl IntentTicket {
    pub fn intent_id(&self) -> &str {
        &self.intent_id
    }
    /// Advisory correlation fingerprint (never a dedupe gate).
    pub fn call_fingerprint(&self) -> &str {
        &self.call_fingerprint
    }
}

/// Outcome of logging an intent. Malformed-argument intents are durably
/// persisted for audit but are NOT dispatch-eligible — no ticket exists.
/// Not Clone: the ticket inside is single-instance (Codex C1p1 P0).
#[derive(Debug, PartialEq, Eq)]
pub enum IntentOutcome {
    Dispatchable(IntentTicket),
    NotDispatchable { intent_id: String, reason: String },
}

/// RVS5C-FIX1: the append that poisoned a handle. `offset` is the store
/// length the handle knew before the append; `frame` is the exact framed
/// bytes it tried to write. The grant store uses both to PROVE the bytes
/// now at `offset` are this handle's own failed write before rolling them
/// back (`grant_service::rollback_own_failed_append`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailedAppend {
    pub(crate) offset: u64,
    pub(crate) frame: Vec<u8>,
}

pub struct ExecutionLog<S: AppendOnlyLog> {
    storage: S,
    session_id: String,
    /// True for the per-user grant store (folder/always grants live there;
    /// once/session grants live in session logs). Enforced in
    /// record_approval — wrong-store writes are rejected at the API.
    is_grant_store: bool,
    next_intent_seq: u64,
    next_approval_seq: u64,
    /// Per-turn transcript sequence numbers.
    seq_in_turn: std::collections::BTreeMap<String, u64>,
    /// Text/reasoning coalescing buffers per turn (schema design: per-token
    /// deltas are coalesced; everything else is verbatim).
    text_buf: std::collections::BTreeMap<String, String>,
    /// RVS5C-FIX1: the store length this handle believes is on disk (set
    /// at open/resync, advanced by each successful append). A failed append
    /// compares the real length against it to learn whether bytes landed.
    known_len: u64,
    /// RVS5C-FIX1: set when an append failed AFTER bytes may have reached
    /// storage. While set, every append is refused with
    /// `LogError::Poisoned`; only a reopen (a new handle) clears it.
    failed_append: Option<FailedAppend>,
}

impl<S: AppendOnlyLog> ExecutionLog<S> {
    /// Open a session log; writes the Header if the log is empty.
    pub fn open(storage: S, session_id: impl Into<String>) -> Result<Self, LogError> {
        Self::open_inner(storage, session_id, false)
    }

    /// Open the per-user grant store (C1 contract §5).
    pub fn open_grant_store(storage: S) -> Result<Self, LogError> {
        Self::open_inner(storage, "grants", true)
    }

    fn open_inner(
        mut storage: S,
        session_id: impl Into<String>,
        is_grant_store: bool,
    ) -> Result<Self, LogError> {
        let session_id = session_id.into();
        let existing = storage.read_all()?;
        let mut known_len = existing.len() as u64;
        // Codex C1p1 P0: sequences must continue across reopen or approval/
        // intent ids get reissued. Derive from existing records.
        let (mut next_intent_seq, mut next_approval_seq) = (0u64, 0u64);
        if !existing.is_empty() {
            let frames = match frame::decode_all(&existing)? {
                frame::DecodeResult::Clean(frames) => frames,
                frame::DecodeResult::TornTail { .. }
                | frame::DecodeResult::UncertainTail { .. } => {
                    // S5(c) / F0595 + RVS5C-FIX1: a provable OR uncertain
                    // tail before ANY append is refused — open has no repair
                    // path (the grant store's explicit, narrower repair runs
                    // BEFORE open, in grant_service::repair_torn_tail).
                    return Err(LogError::Corrupt {
                        seq: 0,
                        reason: "torn tail at open: no repair path; refusing".into(),
                    });
                }
            };
            let mut recs = Vec::with_capacity(frames.len());
            let mut unknown_approval_seq = 0u64;
            for (i, f) in frames.iter().enumerate() {
                // Codex verify P1: an undecodable whole frame means the log
                // is corrupt — refuse to open rather than risk reissuing
                // colliding ids from stale counters. RC2-ENG1 (F0570): a
                // record of a newer build is skipped, its ids reserved.
                let rec = match decode_record(f).map_err(|e| LogError::Corrupt {
                    seq: i as u64,
                    reason: format!("undecodable record at open: {e}"),
                })? {
                    DecodedRecord::Known(rec) => *rec,
                    DecodedRecord::Unknown { value, .. } => {
                        reserve_unknown_ids(
                            &value,
                            &mut next_intent_seq,
                            &mut unknown_approval_seq,
                        );
                        continue;
                    }
                };
                if let LogRecordV1::Intent { intent_id, .. } = &rec {
                    if let Some(n) = trailing_seq(intent_id) {
                        next_intent_seq = next_intent_seq.max(n + 1);
                    }
                }
                recs.push(rec);
            }
            reserve_approval_refs(&recs, &mut next_approval_seq);
            next_approval_seq = next_approval_seq.max(unknown_approval_seq);
        }
        if existing.is_empty() {
            let rec = LogRecordV1::Header {
                version: SCHEMA_VERSION,
                session_id: session_id.clone(),
            };
            let bytes = frame::encode(&serde_json::to_vec(&rec)?);
            storage.append(&bytes, true)?;
            known_len += bytes.len() as u64;
        }
        Ok(Self {
            storage,
            session_id,
            is_grant_store,
            next_intent_seq,
            next_approval_seq,
            seq_in_turn: Default::default(),
            text_buf: Default::default(),
            known_len,
            failed_append: None,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Consume the log, returning the underlying storage (tests/migration).
    pub fn into_storage(self) -> S {
        self.storage
    }

    /// RVS5C-FIX1: true once an append on this handle failed after bytes
    /// may have reached storage. A poisoned handle refuses every append;
    /// reopen the log to continue.
    pub fn is_poisoned(&self) -> bool {
        self.failed_append.is_some()
    }

    /// The append that poisoned this handle, if any (grant-store rollback).
    pub(crate) fn failed_append(&self) -> Option<&FailedAppend> {
        self.failed_append.as_ref()
    }

    /// Test seam: reach a live handle's storage for fault injection.
    #[cfg(test)]
    pub(crate) fn storage_mut_for_tests(&mut self) -> &mut S {
        &mut self.storage
    }

    /// Re-derive the reference sequences from CURRENT durable state — the
    /// grant-store service calls this inside a critical section whenever
    /// another process's append is detected (unit 5, r3 verdict rev 3).
    /// Applies the SAME torn-tail refusal as `open` (S5(c) / F0595): a
    /// resync that sees a torn tail refuses rather than append behind it,
    /// and reserves the approval sequence over the same ref set
    /// (including orphan revocations) — the same rule as open.
    /// RVS5C-FIX1: an uncertain tail is refused the same way; resync never
    /// clears a poisoned handle.
    pub fn resync(&mut self) -> Result<(), LogError> {
        let existing = self.storage.read_all()?;
        let frames = match frame::decode_all(&existing)? {
            frame::DecodeResult::Clean(frames) => frames,
            frame::DecodeResult::TornTail { .. } | frame::DecodeResult::UncertainTail { .. } => {
                return Err(LogError::Corrupt {
                    seq: 0,
                    reason: "torn tail at resync: no repair path; refusing".into(),
                });
            }
        };
        let mut recs = Vec::with_capacity(frames.len());
        let mut unknown_approval_seq = 0u64;
        for (i, f) in frames.iter().enumerate() {
            let rec = match decode_record(f).map_err(|e| LogError::Corrupt {
                seq: i as u64,
                reason: format!("undecodable record at resync: {e}"),
            })? {
                DecodedRecord::Known(rec) => *rec,
                // RC2-ENG1 (F0570): skipped, its ids reserved (as at open).
                DecodedRecord::Unknown { value, .. } => {
                    reserve_unknown_ids(
                        &value,
                        &mut self.next_intent_seq,
                        &mut unknown_approval_seq,
                    );
                    continue;
                }
            };
            if let LogRecordV1::Intent { intent_id, .. } = &rec {
                if let Some(n) = trailing_seq(intent_id) {
                    self.next_intent_seq = self.next_intent_seq.max(n + 1);
                }
            }
            recs.push(rec);
        }
        reserve_approval_refs(&recs, &mut self.next_approval_seq);
        self.next_approval_seq = self.next_approval_seq.max(unknown_approval_seq);
        self.known_len = existing.len() as u64;
        Ok(())
    }

    /// The ONE storage write path of a live handle. RVS5C-FIX1 (review
    /// RVS5C-sp P1): a handle whose earlier append failed AFTER bytes may
    /// have landed refuses every later append, so no frame is ever written
    /// behind torn bytes. A failure that left the length unchanged wrote
    /// nothing, keeps the framing intact, and leaves the handle usable
    /// (the seam's audited close appends TurnClosed after such a failure).
    fn append_record(&mut self, rec: &LogRecordV1, sync: bool) -> Result<(), LogError> {
        if let Some(failed) = &self.failed_append {
            return Err(LogError::Poisoned {
                offset: failed.offset,
            });
        }
        let bytes = frame::encode(&serde_json::to_vec(rec)?);
        match self.storage.append(&bytes, sync) {
            Ok(()) => {
                self.known_len += bytes.len() as u64;
                Ok(())
            }
            Err(e) => {
                // Only an unchanged length proves no byte landed. A changed
                // length (partial write, or a complete frame whose fsync
                // failed) or an unreadable store poisons the handle.
                let clean = matches!(
                    self.storage.read_all(),
                    Ok(now) if now.len() as u64 == self.known_len
                );
                if !clean {
                    self.failed_append = Some(FailedAppend {
                        offset: self.known_len,
                        frame: bytes,
                    });
                }
                Err(e)
            }
        }
    }

    fn append_event(
        &mut self,
        turn_id: &str,
        event: CanonicalEventV1,
        sync: bool,
    ) -> Result<(), LogError> {
        let seq = self.seq_in_turn.get(turn_id).copied().unwrap_or(0);
        let rec = LogRecordV1::EventAppended {
            turn_id: turn_id.to_string(),
            seq_in_turn: seq,
            event,
        };
        self.append_record(&rec, sync)?;
        // RC2-ENG1 (F0599): the sequence advances only once the record is
        // written — a failed append leaves no gap.
        self.seq_in_turn.insert(turn_id.to_string(), seq + 1);
        Ok(())
    }

    /// Flush the coalesced text buffer for a turn (called before any record
    /// that must be ordered after the text, and at turn close).
    /// RC2-ENG1 (F0599): a failed append keeps the text for the next flush
    /// instead of dropping it.
    fn flush_text(&mut self, turn_id: &str) -> Result<(), LogError> {
        let Some(buf) = self.text_buf.remove(turn_id) else {
            return Ok(());
        };
        if buf.is_empty() {
            return Ok(());
        }
        let event = CanonicalEventV1::Text {
            content: buf.clone(),
        };
        if let Err(e) = self.append_event(turn_id, event, false) {
            self.text_buf.insert(turn_id.to_string(), buf);
            return Err(e);
        }
        Ok(())
    }

    pub fn turn_opened(
        &mut self,
        turn_id: &str,
        provider: &str,
        model: &str,
    ) -> Result<(), LogError> {
        self.append_record(
            &LogRecordV1::TurnOpened {
                turn_id: turn_id.to_string(),
                provider: provider.to_string(),
                model: model.to_string(),
            },
            true,
        )
    }

    /// Ingest a canonical runtime event. Text/Reasoning deltas coalesce;
    /// everything else is written verbatim (deltas flushed first to keep
    /// transcript order).
    pub fn record_event(&mut self, turn_id: &str, ev: &AgentEvent) -> Result<(), LogError> {
        match ev {
            AgentEvent::TextDelta(t) => {
                self.text_buf
                    .entry(turn_id.to_string())
                    .or_default()
                    .push_str(t);
                Ok(())
            }
            // Reasoning deltas are NOT logged: the blob at segment end is the
            // durable form (opaque, provenance-tagged).
            AgentEvent::ReasoningDelta(_) => Ok(()),
            other => {
                self.flush_text(turn_id)?;
                let dto = to_dto(other);
                match dto {
                    Some(d) => self.append_event(turn_id, d, false),
                    None => Ok(()),
                }
            }
        }
    }

    /// Evidence-checked WRITE-AHEAD INTENT — the ONLY public intent path
    /// (C1 contract §9). Verifies the evidence's call binding against the
    /// actual call (write-time rejection), persists basis + bound hash for
    /// replay verification, and derives risk_class/approval_ref from the
    /// evidence — caller-selected values are unrepresentable. The bound
    /// tool identity (unit 4, design r3 Delta 2) also arrives INSIDE the
    /// evidence — evidence-borne only, never a free caller argument — and
    /// is persisted as its canonical digest in the durable record for both
    /// the builtin and the mcp leg.
    pub fn intent(
        &mut self,
        turn_id: &str,
        call: &ToolCall,
        evidence: AuthzEvidence,
    ) -> Result<IntentOutcome, LogError> {
        if !evidence.binding.matches(turn_id, call)? {
            return Err(LogError::EvidenceMismatch(format!(
                "evidence bound to {:?}/{:?}, presented for {:?}/{:?}",
                evidence.binding.turn_id,
                evidence.binding.awili_call_id,
                turn_id,
                call.awili_call_id.0
            )));
        }
        self.intent_unchecked(
            turn_id,
            call,
            &evidence.approval_ref,
            evidence.risk_class.as_str(),
            evidence.basis.as_str(),
            &evidence.binding.normalized_call_hash,
            &evidence.identity.digest_hex(),
        )
    }

    /// Raw storage-layer path, NOT evidence-checked. In-crate only: the
    /// kill-point suite exercises storage safety below the authz layer.
    // Eight positional fields mirror the intent record one-to-one; a
    // params struct here would only restate the row.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn intent_unchecked(
        &mut self,
        turn_id: &str,
        call: &ToolCall,
        approval_ref: &str,
        risk_class: &str,
        decision_basis: &str,
        evidence_call_hash: &str,
        identity_digest: &str,
    ) -> Result<IntentOutcome, LogError> {
        use crate::schema::ArgumentValidationV1;
        self.flush_text(turn_id)?;
        let (canonical, validation) = match canonicalize_args(&call.arguments) {
            Ok(c) => (Some(c), ArgumentValidationV1::Valid),
            Err(e) => {
                let mut diag = e.to_string();
                diag.truncate(200); // bounded diagnostic
                (None, ArgumentValidationV1::Malformed { diagnostic: diag })
            }
        };
        let fp = canonical
            .as_deref()
            .map(|c| call_fingerprint(&call.name, c));
        let intent_id = format!("{}-intent-{:04}", turn_id, self.next_intent_seq);
        self.next_intent_seq += 1;
        let rec = LogRecordV1::Intent {
            turn_id: turn_id.to_string(),
            intent_id: intent_id.clone(),
            awili_call_id: call.awili_call_id.0.clone(),
            tool: call.name.clone(),
            raw_arguments: call.arguments.clone(),
            canonical_args: canonical,
            call_fingerprint: fp.clone(),
            argument_validation: validation,
            approval_ref: approval_ref.to_string(),
            risk_class: risk_class.to_string(),
            decision_basis: decision_basis.to_string(),
            evidence_call_hash: evidence_call_hash.to_string(),
            identity_digest: identity_digest.to_string(),
        };
        self.append_record(&rec, true)?; // fsync BEFORE the ticket exists
        Ok(match fp {
            Some(call_fingerprint) => IntentOutcome::Dispatchable(IntentTicket {
                intent_id,
                call_fingerprint,
            }),
            None => IntentOutcome::NotDispatchable {
                intent_id,
                reason: "malformed argument JSON".into(),
            },
        })
    }

    pub fn dispatch_started(&mut self, ticket: &IntentTicket) -> Result<(), LogError> {
        self.append_record(
            &LogRecordV1::DispatchStarted {
                intent_id: ticket.intent_id.clone(),
            },
            true,
        )
    }

    pub fn dispatch_finalized(
        &mut self,
        ticket: &IntentTicket,
        ok: bool,
        content: &str,
    ) -> Result<(), LogError> {
        self.append_record(
            &LogRecordV1::DispatchFinalized {
                intent_id: ticket.intent_id.clone(),
                ok,
                content: content.to_string(),
            },
            true,
        )
    }

    /// Durably records an interactive approval and mints the only values
    /// `AuthzEvidence::from_ask` accepts (C1 contract §4). Capability-gated:
    /// every proof-minting path runs through the dialog controller.
    #[allow(clippy::too_many_arguments)]
    pub fn record_approval(
        &mut self,
        _cap: &HostApprovalCapability,
        scope: &str, // "once" | "session" | "folder" | "always"
        turn_id: &str,
        call: &ToolCall,
        risk_class: RiskClass,
        folder_root: Option<&str>,
        constraints: &str,
    ) -> Result<MintedProof, LogError> {
        if !matches!(scope, "once" | "session" | "folder" | "always") {
            // "install" is DELIBERATELY absent (impl review r1 P1-1): the
            // seed-only scope is mintable through record_install_grants
            // alone — the general approval path, reachable by any
            // capability holder, must never write it.
            return Err(LogError::EvidenceMismatch(format!(
                "invalid scope {scope:?}"
            )));
        }
        // Contract §8: irreversible/privileged may not be granted "always".
        if scope == "always"
            && matches!(risk_class, RiskClass::Irreversible | RiskClass::Privileged)
        {
            return Err(LogError::EvidenceMismatch(
                "irreversible/privileged tools cannot be granted 'always'".into(),
            ));
        }
        match serde_json::from_str::<serde_json::Value>(constraints) {
            Ok(serde_json::Value::Object(_)) => {}
            _ => {
                return Err(LogError::EvidenceMismatch(
                    "constraints must be a JSON object (shape validation is C1's)".into(),
                ))
            }
        }
        // folder_root is required for folder scope, prohibited otherwise.
        match (scope, folder_root.is_some()) {
            ("folder", true)
            | ("once", false)
            | ("session", false)
            | ("always", false)
            | ("install", false) => {}
            ("folder", false) => {
                return Err(LogError::EvidenceMismatch(
                    "folder scope requires folder_root".into(),
                ))
            }
            _ => {
                return Err(LogError::EvidenceMismatch(
                    "folder_root only valid for folder scope".into(),
                ))
            }
        }
        // Store routing (contract §5): once/session -> session log;
        // folder/always -> grant store.
        let belongs_here = match scope {
            "once" | "session" => !self.is_grant_store,
            _ => self.is_grant_store,
        };
        if !belongs_here {
            return Err(LogError::EvidenceMismatch(format!(
                "scope {scope:?} does not belong in this store"
            )));
        }
        let binding = CallBinding::for_call(turn_id, call)?;
        let approval_ref = format!("appr-{}-{:04}", self.session_id, self.next_approval_seq);
        self.next_approval_seq += 1;
        let rec = LogRecordV1::ApprovalRecorded {
            approval_ref: approval_ref.clone(),
            decision: "approved".into(),
            scope: scope.to_string(),
            turn_id: binding.turn_id.clone(),
            awili_call_id: binding.awili_call_id.clone(),
            normalized_call_hash: binding.normalized_call_hash.clone(),
            session_id: self.session_id.clone(),
            tool: call.name.clone(),
            risk_class: risk_class.as_str().to_string(),
            folder_root: folder_root.map(str::to_string),
            constraints: constraints.to_string(),
            granted_at_ms: Some(crate::remembered::now_ms()),
        };
        self.append_record(&rec, true)?; // fsync BEFORE any proof exists
        if scope == "session" {
            crate::remembered::note_session_grant(&rec);
        }
        Ok(if scope == "once" {
            MintedProof::Once(OnceProof {
                approval_ref,
                binding,
                risk_class,
            })
        } else {
            MintedProof::Grant(GrantProof {
                approval_ref,
                binding,
                risk_class,
            })
        })
    }

    /// Unit 5 (design r9 + rulings 3/5/6/12): mint the install-scope
    /// grants for one seed candidate — ONLY scope:"install", risk from
    /// the candidate's accepted descriptor floors (never caller-selected),
    /// one atomic [`LogRecordV1::InstallTransition`] per (origin, tool).
    ///
    /// The TOMBSTONE rule (ruling 5): ANY existing approval for the exact
    /// logical key (origin, tool, setup_id, pinned_requirement) — active,
    /// user-revoked, or machine-superseded — blocks automatic re-seeding
    /// forever. A NEW setup identity is a new key and may seed; doing so
    /// machine-retires every OLDER ACTIVE install approval of the same
    /// (origin, tool) inside the same framed record.
    ///
    /// Repeated seeds under an unchanged identity and catalog append
    /// NOTHING (the append-nothing invariant). Grant store only; called by
    /// `GrantStoreService` inside its locked critical section.
    pub fn record_install_grants(
        &mut self,
        _cap: &HostApprovalCapability,
        candidate: crate::authz::InstallSeedCandidate,
    ) -> Result<InstallSeedOutcome, LogError> {
        if !self.is_grant_store {
            return Err(LogError::EvidenceMismatch(
                "install grants belong in the per-user grant store".into(),
            ));
        }
        // Reconstruct current install state: logical-key tombstones and
        // the per-tool ACTIVE approvals (not revoked, not superseded).
        let (records, _torn) = self.read_records()?;
        let origin = candidate.origin().origin.clone();
        // Tombstones (ruling 5) stay deliberately broad: ANY record naming
        // the logical key — admitted or not — blocks automatic re-seeding.
        let mut tombstones: std::collections::BTreeSet<(String, String, String, String)> =
            Default::default();
        let mut revoked: std::collections::BTreeSet<String> = Default::default();
        for rec in &records {
            let (tool, constraints) = match rec {
                LogRecordV1::ApprovalRecorded {
                    decision,
                    scope,
                    tool,
                    constraints,
                    ..
                } if decision == "approved" && scope == "install" => (tool, constraints),
                LogRecordV1::InstallTransition {
                    tool, constraints, ..
                } => (tool, constraints),
                LogRecordV1::GrantRevoked { approval_ref } => {
                    revoked.insert(approval_ref.clone());
                    continue;
                }
                _ => continue,
            };
            let Ok(serde_json::Value::Object(c)) =
                serde_json::from_str::<serde_json::Value>(constraints)
            else {
                continue;
            };
            let get = |k: &str| c.get(k).and_then(|v| v.as_str()).map(str::to_string);
            if let (Some(o), Some(sid), Some(pin)) = (
                get("__origin"),
                get("__setup_id"),
                get("__pinned_requirement"),
            ) {
                tombstones.insert((o, tool.clone(), sid, pin));
            }
        }
        // RC2-ENG3 (F0588): the ACTIVE set — what a new transition may
        // supersede — is exactly what the readers admit
        // ([`crate::replay::install_state`]), so every transition written
        // here is one replay and the live loader both accept.
        let installs = crate::replay::install_state(&records);
        // (tool -> [(approval_ref)]) active install approvals for THIS origin.
        let mut active: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for (aref, a) in &installs.admitted {
            if a.origin == origin && !installs.superseded.contains(aref) && !revoked.contains(aref)
            {
                active.entry(a.tool.clone()).or_default().push(aref.clone());
            }
        }

        let constraints = serde_json::json!({
            "__origin": candidate.origin().origin,
            "__setup_id": candidate.setup_id().as_str(),
            "__pinned_requirement": candidate.pinned_requirement(),
        })
        .to_string();
        let mut out = InstallSeedOutcome::default();
        for (tool, floor) in candidate.floors() {
            if matches!(floor, RiskClass::Irreversible) {
                out.skipped_irreversible.push(tool.to_string());
                continue;
            }
            let key = (
                candidate.origin().origin.clone(),
                tool.to_string(),
                candidate.setup_id().as_str().to_string(),
                candidate.pinned_requirement().to_string(),
            );
            if tombstones.contains(&key) {
                out.tombstoned.push(tool.to_string());
                continue;
            }
            let supersedes = active.get(tool).cloned().unwrap_or_default();
            let approval_ref = format!("appr-{}-{:04}", self.session_id, self.next_approval_seq);
            self.next_approval_seq += 1;
            let rec = LogRecordV1::InstallTransition {
                approval_ref,
                session_id: self.session_id.clone(),
                tool: tool.to_string(),
                risk_class: floor.as_str().to_string(),
                constraints: constraints.clone(),
                supersedes: supersedes.clone(),
            };
            self.append_record(&rec, true)?; // fsync per transition
            out.superseded.extend(supersedes);
            out.seeded.push(tool.to_string());
        }
        Ok(out)
    }

    /// Denial audit record (C1 contract §7). No proof, no capability —
    /// recording a denial grants nothing.
    pub fn record_denial(
        &mut self,
        turn_id: &str,
        call: &ToolCall,
        risk_class: RiskClass,
        reason: &str,
    ) -> Result<(), LogError> {
        self.append_record(
            &LogRecordV1::AuthzDenied {
                turn_id: turn_id.to_string(),
                awili_call_id: call.awili_call_id.0.clone(),
                tool: call.name.clone(),
                risk_class: risk_class.as_str().to_string(),
                reason: reason.to_string(),
            },
            true,
        )
    }

    /// Append a grant revocation (grant store; append-only, latest wins).
    pub fn append_grant_revoked(&mut self, approval_ref: &str) -> Result<(), LogError> {
        self.append_record(
            &LogRecordV1::GrantRevoked {
                approval_ref: approval_ref.to_string(),
            },
            true,
        )
    }

    pub fn turn_closed(&mut self, turn_id: &str, outcome: &str) -> Result<(), LogError> {
        self.flush_text(turn_id)?;
        self.append_record(
            &LogRecordV1::TurnClosed {
                turn_id: turn_id.to_string(),
                outcome: outcome.to_string(),
            },
            true,
        )
    }

    /// Read + decode every whole record (a torn or uncertain tail is
    /// reported in the count, never repaired here; proven corruption is an
    /// error).
    pub fn read_records(&self) -> Result<(Vec<LogRecordV1>, usize), LogError> {
        self.read_records_counted()
            .map(|(records, torn, _unknown)| (records, torn))
    }

    /// RC2-ENG1 (F0570): [`Self::read_records`] plus the number of records
    /// of a newer build that were skipped (their tag is unknown here) —
    /// (records, torn bytes, skipped records).
    pub fn read_records_counted(&self) -> Result<(Vec<LogRecordV1>, usize, usize), LogError> {
        let buf = self.storage.read_all()?;
        let (frames, torn) = match frame::decode_all(&buf)? {
            frame::DecodeResult::Clean(frames) => (frames, 0),
            frame::DecodeResult::TornTail { frames, torn } => (frames, torn),
            // RVS5C-FIX1: read-only — the uncertain bytes are reported, not
            // destroyed; every mutating path refuses them.
            frame::DecodeResult::UncertainTail { frames, tail } => (frames, tail),
        };
        let mut out = Vec::with_capacity(frames.len());
        let mut unknown = 0usize;
        for (i, f) in frames.iter().enumerate() {
            match decode_record(f) {
                Ok(DecodedRecord::Known(r)) => out.push(*r),
                // RC2-ENG1 (F0570): skip and count, never error.
                Ok(DecodedRecord::Unknown { .. }) => unknown += 1,
                Err(e) => {
                    return Err(LogError::Corrupt {
                        seq: i as u64,
                        reason: format!("undecodable record: {e}"),
                    })
                }
            }
        }
        if let Some(LogRecordV1::Header { version, .. }) = out.first() {
            if *version > SCHEMA_VERSION {
                return Err(LogError::Version {
                    found: *version,
                    max: SCHEMA_VERSION,
                });
            }
        }
        Ok((out, torn, unknown))
    }
}

fn to_dto(ev: &AgentEvent) -> Option<CanonicalEventV1> {
    Some(match ev {
        AgentEvent::TurnStarted => CanonicalEventV1::TurnStarted,
        AgentEvent::ToolCallReady(c) => CanonicalEventV1::ToolCallReady {
            awili_call_id: c.awili_call_id.0.clone(),
            provider_call_id: c.provider_call_id.clone(),
            name: c.name.clone(),
            arguments: c.arguments.clone(),
        },
        AgentEvent::ToolCallInvalid {
            awili_call_id,
            name,
            raw_arguments,
            reason,
            ..
        } => CanonicalEventV1::ToolCallInvalid {
            awili_call_id: awili_call_id.0.clone(),
            name: name.clone(),
            raw_arguments: raw_arguments.clone(),
            reason: reason.clone(),
        },
        AgentEvent::ToolResultIngested(r) => CanonicalEventV1::ToolResult {
            awili_call_id: r.awili_call_id.0.clone(),
            content: r.content.clone(),
            is_error: r.is_error,
        },
        AgentEvent::ContinuationBlobCaptured(a) => CanonicalEventV1::ContinuationBlob {
            provider: a.provenance.provider.clone(),
            model: a.provenance.model.clone(),
            turn: a.provenance.turn,
            kind: format!("{:?}", a.kind),
            payload: a.payload.clone(),
        },
        AgentEvent::UsageReported(u) => CanonicalEventV1::Usage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
        },
        AgentEvent::InputRejected { reason } => CanonicalEventV1::InputRejected {
            reason: reason.clone(),
        },
        AgentEvent::TurnCompleted { stop, .. } => CanonicalEventV1::Completed {
            stop: format!("{stop:?}"),
        },
        AgentEvent::TurnCancelled { .. } => CanonicalEventV1::Cancelled,
        AgentEvent::TurnFailed(e) => CanonicalEventV1::Failed {
            error: render_error(e),
        },
        AgentEvent::TextDelta(_) | AgentEvent::ReasoningDelta(_) => return None,
    })
}

fn render_error(e: &EngineError) -> String {
    format!("{e}")
}
