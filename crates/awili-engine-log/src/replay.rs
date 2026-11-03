//! Replay classifier — pure policy over decoded records (CLAUDE-owned per
//! plan r3.1: which states auto-resume vs require user confirmation).
//!
//! THE invariant (PS-ENG-03 / PS-XIV-03): an intent whose dispatch started
//! but never finalized is UNCERTAIN — surfaced for confirmation, never
//! silently re-executed. Unknown or mutating risk classes get maximum
//! caution.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::schema::LogRecordV1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntentStatus {
    /// The intent's authorization evidence fails log verification: bound
    /// hash mismatch, dangling/mismatched approval_ref, once-grant reuse,
    /// or basis/ref inconsistency. NEVER resumable; surfaced as a security
    /// alert on recovery (C1 contract §9).
    AuthzInvalid { reason: String },
    /// DispatchFinalized present: nothing to do on replay.
    Clean { ok: bool },
    /// Intent logged, dispatch never started: safe to re-approach — but
    /// mutating/unknown tools still re-enter through C1 approval, they are
    /// NOT auto-fired.
    NeverDispatched { auto_resume: bool },
    /// DispatchStarted without DispatchFinalized: the side-effect may or may
    /// not have happened. ALWAYS requires user confirmation.
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentRecovery {
    pub intent_id: String,
    pub turn_id: String,
    pub tool: String,
    /// Verbatim raw arguments (always present, audit-grade).
    pub raw_arguments: String,
    /// None = malformed JSON; such intents are never dispatch-eligible.
    pub canonical_args: Option<String>,
    /// Advisory fingerprint (never a dedupe gate).
    pub call_fingerprint: Option<String>,
    pub risk_class: String,
    /// Canonical digest of the evidence-bound tool identity (unit 4):
    /// replay EXPOSES the persisted binding — "" on pre-unit-4 records.
    pub identity_digest: String,
    pub status: IntentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRecovery {
    pub turn_id: String,
    /// TurnClosed outcome if present; open turns have None and are the
    /// interrupted ones.
    pub closed: Option<String>,
    pub intents: Vec<IntentRecovery>,
}

/// Risk classes that may auto-resume when the dispatch never started.
/// Everything else — including the empty string and "unknown" — requires
/// C1/user involvement. Conservative by construction.
fn read_only_class(risk_class: &str) -> bool {
    matches!(risk_class, "read" | "read_only" | "pure")
}

pub fn classify(records: &[LogRecordV1]) -> Vec<TurnRecovery> {
    classify_with_grants(records, &[])
}

/// `grant_records` = the per-user grant store's records (C1 contract §5),
/// consulted for `decision_basis = "grant"` reference validation.
pub fn classify_with_grants(
    records: &[LogRecordV1],
    grant_records: &[LogRecordV1],
) -> Vec<TurnRecovery> {
    #[derive(Default)]
    struct IntentAcc {
        turn_id: String,
        awili_call_id: String,
        tool: String,
        raw_arguments: String,
        canonical_args: Option<String>,
        call_fingerprint: Option<String>,
        risk_class: String,
        decision_basis: String,
        evidence_call_hash: String,
        identity_digest: String,
        approval_ref: String,
        started: bool,
        finalized: Option<bool>,
        /// order of arrival, for once-reuse detection
        arrival: usize,
    }

    // Approval identity map: approval_ref -> (scope, normalized_call_hash,
    // turn_id, awili_call_id). Session log + grant store both contribute.
    #[derive(PartialEq, Clone, Copy)]
    enum Store {
        Session,
        GrantStore,
    }
    struct Approval {
        scope: String,
        hash: String,
        turn_id: String,
        awili_call_id: String,
        store: Store,
    }
    let mut approvals: std::collections::BTreeMap<String, Approval> = Default::default();
    let mut revoked: std::collections::BTreeSet<String> = Default::default();
    // Unit 5: legacy execution logs reference PRE-migration approval refs;
    // the grant store's MigrationAlias records are the durable translation
    // table. Grant-store lane only — an alias in a session log is ignored.
    let mut aliases: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    // RC2-ENG3 (F0588): which install approvals the grant store admits and
    // which it machine-retires — ONE rule ([`install_state`]) shared with
    // the live loader and the seeder.
    let installs = install_state(grant_records);
    let mut ingest = |recs: &[LogRecordV1], store: Store| {
        for rec in recs {
            match rec {
                LogRecordV1::ApprovalRecorded {
                    approval_ref,
                    decision,
                    scope,
                    normalized_call_hash,
                    turn_id,
                    awili_call_id,
                    ..
                } if decision == "approved"
                    // RC2-ENGF2: a standalone install approval counts only
                    // when [`install_state`] admits it — the rule the live
                    // loader applies too. A malformed one contributes
                    // nothing, so a step naming it is AuthzInvalid.
                    && (scope != "install"
                        || installs
                            .admitted
                            .get(approval_ref)
                            .is_some_and(|a| !a.from_transition)) =>
                {
                    approvals.insert(
                        approval_ref.clone(),
                        Approval {
                            scope: scope.clone(),
                            hash: normalized_call_hash.clone(),
                            turn_id: turn_id.clone(),
                            awili_call_id: awili_call_id.clone(),
                            store,
                        },
                    );
                }
                LogRecordV1::GrantRevoked { approval_ref } => {
                    revoked.insert(approval_ref.clone());
                }
                LogRecordV1::MigrationAlias {
                    old_ref, new_ref, ..
                } if store == Store::GrantStore => {
                    aliases
                        .entry(old_ref.clone())
                        .or_default()
                        .push(new_ref.clone());
                }
                // Unit 5 (schema: install_transition): the embedded
                // approval contributes exactly like a standalone
                // install-scope ApprovalRecorded; each superseded ref is
                // machine-retired (revoked-equivalent for evidence
                // validation). Grant-store lane only — on the session
                // stream the synthesized scope fails the store check and
                // never authorizes. A transition [`install_state`] does not
                // admit contributes nothing (its embedded ref then dangles,
                // which the evidence check classifies AuthzInvalid).
                LogRecordV1::InstallTransition { approval_ref, .. }
                    if store == Store::GrantStore
                        && installs
                            .admitted
                            .get(approval_ref)
                            .is_some_and(|a| a.from_transition) =>
                {
                    approvals.insert(
                        approval_ref.clone(),
                        Approval {
                            scope: "install".into(),
                            hash: String::new(),
                            turn_id: String::new(),
                            awili_call_id: String::new(),
                            store,
                        },
                    );
                }
                _ => {}
            }
        }
    };
    ingest(records, Store::Session);
    ingest(grant_records, Store::GrantStore);
    revoked.extend(installs.superseded.iter().cloned());
    // Resolve an evidence ref: a direct hit is always unambiguous (open()
    // advances the mint sequence past every aliased old ref, so new refs
    // never collide with them). Otherwise: exactly one alias resolves;
    // zero stays dangling (the existing AuthzInvalid); MULTIPLE aliases is
    // authorization-evidence ambiguity — AuthzInvalid outright, never an
    // arbitrary choice (r5 verdict ruling 10).
    // Ok(Some(ref)) = usable; Ok(None) = dangling; Err(reason) = ambiguous.
    let resolve = |r: &str| -> Result<Option<String>, String> {
        if approvals.contains_key(r) {
            return Ok(Some(r.to_string()));
        }
        match aliases.get(r).map(Vec::as_slice) {
            Some(many) if many.len() > 1 => Err(format!(
                "ambiguous migrated approval reference {r:?} ({} aliases)",
                many.len()
            )),
            Some([one]) if approvals.contains_key(one) => Ok(Some(one.clone())),
            _ => Ok(None),
        }
    };
    // S5(c) / F0592: BOTH sides of the revocation comparison go through
    // the SAME alias rule. The revocation side is resolved up front;
    // ambiguous or dangling (zero-alias) revocation refs FAIL CLOSED and
    // invalidate every grant-basis intent, so an old-ref revocation can
    // never silently miss a live grant that still names it.
    let mut ambiguous_revocations: Vec<String> = Vec::new();
    for raw in revoked.iter() {
        match resolve(raw) {
            Ok(Some(_)) => {}
            Ok(None) => {
                if !aliases.contains_key(raw) {
                    // No alias at all: a genuine dangling ref — it
                    // revokes nothing and cannot invalidate a live grant.
                    continue;
                }
                ambiguous_revocations.push(format!(
                    "revocation {raw:?} names an alias with no live target"
                ));
            }
            Err(reason) => ambiguous_revocations.push(reason),
        }
    }
    let ambiguous_revocations = if ambiguous_revocations.is_empty() {
        None
    } else {
        Some(ambiguous_revocations.join("; "))
    };
    // S5(c) / F0592: build the set of RESOLVED revocation targets so a
    // revocation that names an old (aliased) ref also catches the live
    // grant it actually cancels. A revocation that resolves to a live
    // ref adds that ref; a revocation that is genuinely dangling adds
    // nothing (it revokes no live grant).
    let mut resolved_revoked: std::collections::BTreeSet<String> = revoked.clone();
    for raw in revoked.iter() {
        if let Ok(Some(resolved)) = resolve(raw) {
            if resolved != *raw {
                resolved_revoked.insert(resolved);
            }
        }
    }
    let mut intents: BTreeMap<String, IntentAcc> = BTreeMap::new();
    let mut turn_order: Vec<String> = Vec::new();
    // RC2-ENG1 (F0593): turns that hold intents but no `TurnOpened` (a
    // malformed log), in order of their first intent — reported after the
    // opened turns, never dropped.
    let mut orphan_order: Vec<String> = Vec::new();
    let mut duplicate_intent_ids: std::collections::BTreeSet<String> = Default::default();
    let mut closed: BTreeMap<String, String> = BTreeMap::new();

    for rec in records {
        match rec {
            LogRecordV1::TurnOpened { turn_id, .. } if !turn_order.contains(turn_id) => {
                turn_order.push(turn_id.clone());
            }
            LogRecordV1::Intent {
                turn_id,
                intent_id,
                awili_call_id,
                tool,
                raw_arguments,
                canonical_args,
                call_fingerprint,
                risk_class,
                decision_basis,
                evidence_call_hash,
                identity_digest,
                approval_ref,
                ..
            } => {
                if !orphan_order.contains(turn_id) {
                    orphan_order.push(turn_id.clone());
                }
                let arrival = intents.len();
                if intents.contains_key(intent_id) {
                    duplicate_intent_ids.insert(intent_id.clone());
                }
                intents.insert(
                    intent_id.clone(),
                    IntentAcc {
                        turn_id: turn_id.clone(),
                        awili_call_id: awili_call_id.clone(),
                        tool: tool.clone(),
                        raw_arguments: raw_arguments.clone(),
                        canonical_args: canonical_args.clone(),
                        call_fingerprint: call_fingerprint.clone(),
                        risk_class: risk_class.clone(),
                        decision_basis: decision_basis.clone(),
                        evidence_call_hash: evidence_call_hash.clone(),
                        identity_digest: identity_digest.clone(),
                        approval_ref: approval_ref.clone(),
                        started: false,
                        finalized: None,
                        arrival,
                    },
                );
            }
            LogRecordV1::DispatchStarted { intent_id } => {
                if let Some(acc) = intents.get_mut(intent_id) {
                    acc.started = true;
                }
            }
            LogRecordV1::DispatchFinalized { intent_id, ok, .. } => {
                if let Some(acc) = intents.get_mut(intent_id) {
                    acc.finalized = Some(*ok);
                }
            }
            LogRecordV1::TurnClosed { turn_id, outcome } => {
                closed.insert(turn_id.clone(), outcome.clone());
            }
            _ => {}
        }
    }

    // Once-reuse detection: refs used by more than one intent, in arrival
    // order — every use after the first is invalid (C1 contract §5).
    let mut once_use: std::collections::BTreeMap<String, Vec<(usize, String)>> = Default::default();
    for (intent_id, acc) in intents.iter() {
        if !acc.approval_ref.is_empty() {
            if let Some(a) = approvals.get(&acc.approval_ref) {
                if a.scope == "once" {
                    once_use
                        .entry(acc.approval_ref.clone())
                        .or_default()
                        .push((acc.arrival, intent_id.clone()));
                }
            }
        }
    }
    let mut once_invalid: std::collections::BTreeSet<String> = Default::default();
    for uses in once_use.values_mut() {
        uses.sort();
        for (_, id) in uses.iter().skip(1) {
            once_invalid.insert(id.clone());
        }
    }

    // Universal-binding + basis/ref verification (C1 contract §7/§9).
    // Legacy pre-C1 intents (empty decision_basis) skip authz checks.
    fn authz_reason(
        intent_id: &str,
        acc: &IntentAcc,
        approvals: &std::collections::BTreeMap<String, Approval>,
        resolved_revoked: &std::collections::BTreeSet<String>,
        once_invalid: &std::collections::BTreeSet<String>,
        duplicates: &std::collections::BTreeSet<String>,
        ambiguous_revocations: Option<&str>,
    ) -> Option<String> {
        if duplicates.contains(intent_id) {
            return Some("duplicate intent_id in log".into());
        }
        if acc.decision_basis.is_empty() {
            // Legacy (pre-C1) exemption — Codex C1p1 P1: only a FULLY
            // pre-authz record qualifies; partial authz fields = tamper.
            if acc.evidence_call_hash.is_empty() && acc.approval_ref.is_empty() {
                return None;
            }
            return Some("empty decision_basis with authz fields present".into());
        }
        // Binding: recompute the content hash from the record's own fields.
        if let Some(canonical) = &acc.canonical_args {
            let recomputed = crate::writer::call_fingerprint(&acc.tool, canonical);
            if recomputed != acc.evidence_call_hash {
                return Some("evidence_call_hash does not match record contents".into());
            }
        }
        match acc.decision_basis.as_str() {
            "policy" => {
                if !acc.approval_ref.is_empty() {
                    return Some("policy basis with non-empty approval_ref".into());
                }
                None
            }
            "ask" => {
                let Some(a) = approvals.get(&acc.approval_ref) else {
                    return Some("ask basis with dangling approval_ref".into());
                };
                if a.hash != acc.evidence_call_hash
                    || a.turn_id != acc.turn_id
                    || a.awili_call_id != acc.awili_call_id
                {
                    return Some("approval record bound to a different call".into());
                }
                if once_invalid.contains(intent_id) {
                    return Some("once-scoped approval reused".into());
                }
                None
            }
            "grant" => {
                let Some(a) = approvals.get(&acc.approval_ref) else {
                    return Some("grant basis with dangling approval_ref".into());
                };
                if a.scope == "once" {
                    return Some("grant basis referencing once-scoped record".into());
                }
                // Store identity (Codex C1p1 P0): session grants live in
                // the session log; folder/always grants in the grant store.
                let store_ok = match a.scope.as_str() {
                    "session" => a.store == Store::Session,
                    // "install" (unit 5): per-user grant-store lane ONLY —
                    // an install record arriving on the session stream
                    // never authorizes (design r2 §1 store routing).
                    "folder" | "always" | "install" => a.store == Store::GrantStore,
                    _ => false,
                };
                if !store_ok {
                    return Some(format!(
                        "grant scope {:?} resolved from the wrong store",
                        a.scope
                    ));
                }
                // S5(c) / F0592: a revocation that could not be resolved
                // through the alias rule (ambiguous or dangling alias)
                // fails closed and invalidates every grant-basis intent.
                if let Some(reason) = ambiguous_revocations {
                    return Some(format!(
                        "revocation could not be resolved: {reason} (grant invalidated fail-closed)"
                    ));
                }
                if resolved_revoked.contains(&acc.approval_ref) {
                    return Some("grant basis referencing revoked grant".into());
                }
                None
            }
            other => Some(format!("unknown decision_basis {other:?}")),
        }
    }

    let mut by_turn: BTreeMap<String, Vec<IntentRecovery>> = BTreeMap::new();
    for (intent_id, mut acc) in intents {
        // Unit 5 pre-resolution: rewrite a legacy evidence ref through the
        // alias table before verification; ambiguity is AuthzInvalid here
        // and now. Dangling refs flow through unchanged into the existing
        // dangling-ref AuthzInvalid arms.
        if !acc.approval_ref.is_empty() {
            match resolve(&acc.approval_ref) {
                Ok(Some(r)) => acc.approval_ref = r,
                Ok(None) => {}
                Err(reason) => {
                    by_turn
                        .entry(acc.turn_id.clone())
                        .or_default()
                        .push(IntentRecovery {
                            intent_id,
                            turn_id: acc.turn_id,
                            tool: acc.tool,
                            raw_arguments: acc.raw_arguments,
                            canonical_args: acc.canonical_args,
                            call_fingerprint: acc.call_fingerprint,
                            risk_class: acc.risk_class,
                            identity_digest: acc.identity_digest,
                            status: IntentStatus::AuthzInvalid { reason },
                        });
                    continue;
                }
            }
        }
        if let Some(reason) = authz_reason(
            &intent_id,
            &acc,
            &approvals,
            &resolved_revoked,
            &once_invalid,
            &duplicate_intent_ids,
            ambiguous_revocations.as_deref(),
        ) {
            by_turn
                .entry(acc.turn_id.clone())
                .or_default()
                .push(IntentRecovery {
                    intent_id,
                    turn_id: acc.turn_id,
                    tool: acc.tool,
                    raw_arguments: acc.raw_arguments,
                    canonical_args: acc.canonical_args,
                    call_fingerprint: acc.call_fingerprint,
                    risk_class: acc.risk_class,
                    identity_digest: acc.identity_digest,
                    status: IntentStatus::AuthzInvalid { reason },
                });
            continue;
        }
        let status = match (acc.started, acc.finalized) {
            // Finalized wins even if the DispatchStarted record was lost to a
            // torn write: the outcome is known.
            (_, Some(ok)) => IntentStatus::Clean { ok },
            (true, None) => IntentStatus::Uncertain,
            // Malformed-argument intents (canonical None) are audit records:
            // never auto-resumable regardless of risk class.
            (false, None) => IntentStatus::NeverDispatched {
                auto_resume: read_only_class(&acc.risk_class) && acc.canonical_args.is_some(),
            },
        };
        by_turn
            .entry(acc.turn_id.clone())
            .or_default()
            .push(IntentRecovery {
                intent_id,
                turn_id: acc.turn_id,
                tool: acc.tool,
                raw_arguments: acc.raw_arguments,
                canonical_args: acc.canonical_args,
                call_fingerprint: acc.call_fingerprint,
                risk_class: acc.risk_class,
                identity_digest: acc.identity_digest,
                status,
            });
    }

    let orphans: Vec<String> = orphan_order
        .into_iter()
        .filter(|t| !turn_order.contains(t))
        .collect();
    turn_order
        .into_iter()
        .chain(orphans)
        .map(|turn_id| TurnRecovery {
            closed: closed.get(&turn_id).cloned(),
            intents: by_turn.remove(&turn_id).unwrap_or_default(),
            turn_id,
        })
        .collect()
}

/// RC2-ENG3 (F0588): an install binding parsed STRICTLY (impl review r2 P1)
/// — exactly `__origin`, `__setup_id` and `__pinned_requirement`, all
/// strings, no extras — as (origin, setup id, pinned requirement). The live
/// loader (`awili_engine_authz::GrantSet::load`) and the seeder
/// (`ExecutionLog::record_install_grants`) apply the same rule.
pub fn strict_install_binding(constraints: &str) -> Option<(String, String, String)> {
    let v = serde_json::from_str::<serde_json::Value>(constraints).ok()?;
    let m = v.as_object()?;
    if m.len() != 3 {
        return None;
    }
    let get = |k: &str| m.get(k).and_then(|v| v.as_str()).map(str::to_string);
    Some((
        get("__origin")?,
        get("__setup_id")?,
        get("__pinned_requirement")?,
    ))
}

/// RC2-ENG3 (F0588): one install approval the grant store admits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallApproval {
    pub tool: String,
    pub origin: String,
    pub setup_id: String,
    pub pinned_requirement: String,
    /// Embedded in an [`LogRecordV1::InstallTransition`] (false: a
    /// standalone install-scope `ApprovalRecorded`).
    pub from_transition: bool,
}

/// RC2-ENG3 (F0588): the install approvals a grant store admits, and the
/// refs its admitted transitions machine-retire.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallState {
    pub admitted: BTreeMap<String, InstallApproval>,
    pub superseded: std::collections::BTreeSet<String>,
}

/// RC2-ENG3 (F0588): THE install-supersession rule, shared by replay, the
/// live loader (`awili_engine_authz::GrantSet::load`) and the seeder
/// (`ExecutionLog::record_install_grants`), so the three can never disagree.
/// In record order over the grant store:
/// - an approved install-scope `ApprovalRecorded` with a strict binding
///   ([`strict_install_binding`]) is admitted;
/// - an `InstallTransition` with a strict binding is admitted WHOLE or not at
///   all (impl review r1 P1-2): every ref it supersedes must name an
///   EARLIER admitted install approval of the same tool and origin with a
///   DIFFERENT identity; only then are those refs retired.
pub fn install_state(grant_records: &[LogRecordV1]) -> InstallState {
    let mut state = InstallState::default();
    for rec in grant_records {
        match rec {
            LogRecordV1::ApprovalRecorded {
                approval_ref,
                decision,
                scope,
                tool,
                constraints,
                ..
            } if decision == "approved" && scope == "install" => {
                if let Some((origin, setup_id, pinned_requirement)) =
                    strict_install_binding(constraints)
                {
                    state.admitted.insert(
                        approval_ref.clone(),
                        InstallApproval {
                            tool: tool.clone(),
                            origin,
                            setup_id,
                            pinned_requirement,
                            from_transition: false,
                        },
                    );
                }
            }
            LogRecordV1::InstallTransition {
                approval_ref,
                tool,
                constraints,
                supersedes,
                ..
            } => {
                let Some((origin, setup_id, pinned_requirement)) =
                    strict_install_binding(constraints)
                else {
                    continue;
                };
                let valid = supersedes.iter().all(|sref| {
                    state.admitted.get(sref).is_some_and(|a| {
                        a.tool == *tool
                            && a.origin == origin
                            && (a.setup_id != setup_id
                                || a.pinned_requirement != pinned_requirement)
                    })
                });
                if !valid {
                    continue;
                }
                state.superseded.extend(supersedes.iter().cloned());
                state.admitted.insert(
                    approval_ref.clone(),
                    InstallApproval {
                        tool: tool.clone(),
                        origin,
                        setup_id,
                        pinned_requirement,
                        from_transition: true,
                    },
                );
            }
            _ => {}
        }
    }
    state
}
