//! GrantSet (contract §5): pure reconstruction from the two record streams
//! (session log + per-user grant store), precedence, constraint matching.

use awili_engine::events::ToolCall;
use awili_engine_log::authz::{AuthzGate, GrantRef, RiskClass};
use awili_engine_log::schema::LogRecordV1;

use crate::risk::registry_floor_from_str;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGrant {
    pub approval_ref: String,
    pub scope: String, // "session" | "folder" | "always" — once never stored
    pub tool: String,
    pub risk_class: RiskClass,
    pub session_id: String,
    pub folder_root: Option<String>,
    /// JSON object of exact-match argument constraints; `{}` = any args.
    pub constraints: serde_json::Map<String, serde_json::Value>,
}

/// Pure function of the two record streams (contract §5): deterministic,
/// revocations applied (append-only, latest wins).
pub struct GrantSet {
    grants: Vec<StoredGrant>,
}

impl GrantSet {
    /// Gate-gated (contract §9): grant reconstruction is part of the
    /// authorization boundary; only the gate holder loads it.
    pub fn load(
        _gate: &AuthzGate,
        session_records: &[LogRecordV1],
        grant_store_records: &[LogRecordV1],
    ) -> Self {
        let mut grants = Vec::new();
        let mut revoked = std::collections::BTreeSet::new();
        let installs = awili_engine_log::replay::install_state(grant_store_records);
        let mut ingest = |recs: &[LogRecordV1], from_store: bool| {
            for rec in recs {
                match rec {
                    LogRecordV1::ApprovalRecorded {
                        approval_ref,
                        decision,
                        scope,
                        session_id,
                        tool,
                        risk_class,
                        folder_root,
                        constraints,
                        ..
                    } if decision == "approved" && scope != "once" => {
                        // Store-routing mirror of B2's write-side rule.
                        let belongs = match scope.as_str() {
                            "session" => !from_store,
                            // "install" (unit 5): per-user grant-store
                            // lane only, like folder/always.
                            "folder" | "always" | "install" => from_store,
                            _ => false,
                        };
                        if !belongs {
                            continue;
                        }
                        // RC2-ENGF2: a standalone install approval counts
                        // only when THE shared rule (`install_state`)
                        // admits it — the rule replay applies too — so a
                        // malformed binding holds no grant here either.
                        let admitted_install = installs
                            .admitted
                            .get(approval_ref)
                            .is_some_and(|a| !a.from_transition);
                        if scope == "install" && !admitted_install {
                            continue;
                        }
                        // FAIL CLOSED (Codex C1p2 P0): malformed or
                        // non-object constraints invalidate the grant
                        // entirely — a tampered record must never broaden
                        // authority into "matches everything".
                        let Some(constraints) =
                            serde_json::from_str::<serde_json::Value>(constraints)
                                .ok()
                                .and_then(|v| v.as_object().cloned())
                        else {
                            continue;
                        };
                        grants.push(StoredGrant {
                            approval_ref: approval_ref.clone(),
                            scope: scope.clone(),
                            tool: tool.clone(),
                            risk_class: registry_floor_from_str(risk_class),
                            session_id: session_id.clone(),
                            folder_root: folder_root.clone(),
                            constraints,
                        });
                    }
                    // Unit 5 (impl review r1 P1-2): an install
                    // transition is admitted WHOLE or not at all.
                    // RC2-ENG3 (F0588): whether it is admitted — and what
                    // it retires — is decided by THE shared rule
                    // (`awili_engine_log::replay::install_state`), which
                    // replay and the seeder also use: a supersedes list
                    // naming a standalone install approval is judged the
                    // same live and on replay. A transition the rule does
                    // not admit contributes no grant and retires nothing
                    // (fail closed). Grant-store lane only.
                    LogRecordV1::InstallTransition {
                        approval_ref,
                        session_id,
                        tool,
                        risk_class,
                        constraints,
                        ..
                    } if from_store
                        && installs
                            .admitted
                            .get(approval_ref)
                            .is_some_and(|a| a.from_transition) =>
                    {
                        let Some(cmap) = install_binding(constraints) else {
                            continue; // unreachable: admitted means strict
                        };
                        grants.push(StoredGrant {
                            approval_ref: approval_ref.clone(),
                            scope: "install".into(),
                            tool: tool.clone(),
                            risk_class: registry_floor_from_str(risk_class),
                            session_id: session_id.clone(),
                            folder_root: None,
                            constraints: cmap,
                        });
                    }
                    LogRecordV1::GrantRevoked { approval_ref } => {
                        revoked.insert(approval_ref.clone());
                    }
                    _ => {}
                }
            }
        };
        ingest(session_records, false);
        ingest(grant_store_records, true);
        revoked.extend(installs.superseded.iter().cloned());
        // S6: a Session grant the owner revoked from the Allowed actions
        // list is dropped too (in memory: the session log belongs to the
        // worker that owns it).
        grants.retain(|g| {
            !revoked.contains(&g.approval_ref)
                && (g.scope != "session"
                    || !awili_engine_log::remembered::is_session_revoked(&g.approval_ref))
        });
        Self { grants }
    }

    /// Narrowest matching un-revoked grant (contract §5 precedence, unit 5
    /// r2 verdict rev 1: session > folder > install > always). A grant
    /// matches when tool and risk class are equal, its scope applies to
    /// this session/folder, and every constraint key equals the call's
    /// argument value exactly.
    ///
    /// The "install" arm (unit 5 §3.2/§5 of the design of record) matches
    /// ONLY when a per-call witness is present and its origin-bound setup
    /// identity equals the grant's strict three-key binding — absent
    /// witness or any mismatch, the arm contributes nothing and the normal
    /// Ask/deny path runs (fail closed).
    pub fn matching(
        &self,
        call: &ToolCall,
        risk: RiskClass,
        session_id: &str,
        folder_root: Option<&str>,
        install_witness: Option<&awili_attest_types::InstallOriginWitness<'_>>,
    ) -> Option<&StoredGrant> {
        let args: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str::<serde_json::Value>(&call.arguments)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
        let applies = |g: &&StoredGrant| {
            g.tool == call.name
                && g.risk_class == risk
                && match g.scope.as_str() {
                    "session" => g.session_id == session_id,
                    "folder" => match (&g.folder_root, folder_root) {
                        (Some(gr), Some(fr)) => gr == fr,
                        _ => false,
                    },
                    "always" => true,
                    "install" => install_matches(g, install_witness),
                    _ => false,
                }
                // Session grants intentionally authorize this tool+risk for
                // the rest of their session. Install grants are scoped by
                // their attested identity witness. Only folder/always grants
                // have argument constraints to compare.
                && match g.scope.as_str() {
                    "session" | "install" => true,
                    "folder" | "always" => g.constraints.iter().all(|(k, v)| {
                        if k == "__paths" {
                            // Deep path profile must match exactly (same walk
                            // as capture — nested/array paths included).
                            let profile: Vec<serde_json::Value> = crate::risk::path_profile(&call.arguments)
                                .into_iter()
                                .map(serde_json::Value::String)
                                .collect();
                            v.as_array().is_some_and(|c| *c == profile)
                        } else if k == "__commands" {
                            let profile: Vec<serde_json::Value> = crate::risk::command_profile(&call.arguments)
                                .into_iter()
                                .map(serde_json::Value::String)
                                .collect();
                            v.as_array().is_some_and(|c| *c == profile)
                        } else {
                            args.get(k) == Some(v)
                        }
                    }),
                    _ => false,
                }
        };
        for scope in ["session", "folder", "install", "always"] {
            if let Some(g) = self
                .grants
                .iter()
                .filter(|g| g.scope == scope)
                .find(applies)
            {
                return Some(g);
            }
        }
        None
    }

    pub fn len(&self) -> usize {
        self.grants.len()
    }

    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }
}

impl StoredGrant {
    /// The grant-set loader's way to a GrantRef. RC2-ENG3 (F0555): the log
    /// crate's `GrantRef::from_grant_set` is public and gate-gated, not
    /// crate-private — the gate (held only by the C1 authorizer) is the
    /// boundary, and this loader validated the grant against the records.
    pub fn to_ref(&self, gate: &AuthzGate) -> GrantRef {
        GrantRef::from_grant_set(gate, &self.approval_ref, self.risk_class)
    }
}

/// Unit 5 §3.2: the strict three-key install binding. A grant matches only
/// when its constraints are EXACTLY {"__origin", "__setup_id",
/// "__pinned_requirement"} with string values equal to the witness's
/// facts — missing, malformed, extra, or non-string keys invalidate the
/// grant (fails closed); an absent witness matches nothing.
fn install_matches(
    g: &StoredGrant,
    witness: Option<&awili_attest_types::InstallOriginWitness<'_>>,
) -> bool {
    let Some(w) = witness else { return false };
    if g.constraints.len() != 3 {
        return false;
    }
    let (Some(o), Some(sid), Some(pin)) = (
        g.constraints.get("__origin"),
        g.constraints.get("__setup_id"),
        g.constraints.get("__pinned_requirement"),
    ) else {
        return false;
    };
    let (Some(o), Some(sid), Some(pin)) = (o.as_str(), sid.as_str(), pin.as_str()) else {
        return false;
    };
    o == w.origin().origin && sid == w.setup_id().as_str() && pin == w.pinned_requirement()
}

/// Parse an install binding STRICTLY (impl review r2 P1): exactly the
/// three reserved keys, all strings, no extras — anything else is a
/// malformed record that contributes nothing and retires nothing.
fn install_binding(constraints: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let m = serde_json::from_str::<serde_json::Value>(constraints)
        .ok()
        .and_then(|v| v.as_object().cloned())?;
    if m.len() != 3 {
        return None;
    }
    for k in ["__origin", "__setup_id", "__pinned_requirement"] {
        if !m.get(k).is_some_and(|v| v.is_string()) {
            return None;
        }
    }
    Some(m)
}
