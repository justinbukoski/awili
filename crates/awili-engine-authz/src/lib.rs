//! awili-engine-authz — C1: the authorization & tool-risk policy engine.
//!
//! Implements docs/contracts/c1-authorization-contract-v2.md (r12). Pure:
//! decisions are a function of (call, context, policy floors, grants) — no
//! IO, no clocks. The B2-owned proof/evidence machinery lives in
//! `awili-engine-log`; this crate holds the gate, produces [`Decision`]s,
//! and runs the capability-gated approval resolution flow.
//!
//! Division of enforcement (contract §6): this crate is POLICY over
//! strings — textual screening, risk classification, grant matching. The
//! runtime security property against filesystem races belongs to C3's
//! descriptor-relative execution.

pub mod grants;
pub mod policy;
pub mod risk;

use awili_engine::events::ToolCall;
use awili_engine_log::authz::{
    AuthzEvidence, AuthzGate, HostApprovalCapability, ResolvedTool, RiskClass,
};
use awili_engine_log::storage::AppendOnlyLog;
use awili_engine_log::{ExecutionLog, LogError};

pub use grants::{GrantSet, StoredGrant};
pub use policy::{screen, Screening};
pub use risk::{base_risk, classify_risk, classify_risk_from, registry_floor_from_str};

/// RC2-R2G1: whether `path` names a secret file (`.env`, keys, tokens,
/// credentials, …): the same test a path argument meets (contract §8).
/// A search's executor uses it to keep the secret files it finds out of
/// what it shows unless the owner allowed them.
pub fn is_secret_path(path: &str) -> bool {
    risk::touches_secrets(path)
}

/// U4 (D7): the exact constraints JSON every auto-mode approval record
/// carries — the audit signature that an approval was minted by Auto mode,
/// not a user dialog (each record is scope `"once"`).
pub const AUTO_APPROVAL_CONSTRAINTS: &str = r#"{"__approved_by":"auto_mode"}"#;

/// Everything a decision needs. Grants are loaded once per session
/// (replayed from the logs) and updated as approvals land.
///
/// Unit 5 (r3 verdict rev 6 / ruling 15): the context OWNS the optional
/// per-call install witness and `authorize` consumes the context BY VALUE
/// — a witness-bearing context can never be borrowed into two
/// authorizations, and `PendingApproval` retains nothing install-related.
pub struct AuthzContext<'a, 'w> {
    pub turn_id: &'a str,
    pub session_id: &'a str,
    /// Canonical absolute path of the bound folder (session root), if any.
    pub folder_root: Option<&'a str>,
    pub grants: &'a GrantSet,
    /// C2 registry-declared floor for this tool, when known (§3: registry
    /// metadata is the floor; classification only escalates from it).
    pub registry_floor: Option<RiskClass>,
    /// The authority-minted resolution token (unit 4, design r3 Delta 2 /
    /// r4 Delta A): the seam resolves the registry BEFORE authorization
    /// and passes the minted identity in. `authorize` consumes the token —
    /// C1 can never be handed a bare caller-selected identity, so every
    /// evidence it produces is bound to the registry's resolution.
    pub resolved_tool: &'a ResolvedTool,
    /// The per-call install witness (unit 5), when the current authority
    /// query issued one — moved in, consumed by this one authorization.
    /// `None` for every non-install caller. Its brand lifetime is
    /// independent of the borrow lifetime, so a callback-scoped witness
    /// can enter a context over longer-lived borrows.
    pub install_witness: Option<awili_attest_types::InstallOriginWitness<'w>>,
}

/// The consuming per-call authority callback handed to
/// [`InstallAuthorityQuery::with_witness`]: it receives the minted witness
/// (or `None` when none was issued) and returns the decision. Factored out
/// of the trait signature so the HRTB + boxed trait-object spelling is
/// readable in one place.
pub type InstallWitnessCallback<'cb> =
    Box<dyn for<'s> FnOnce(Option<awili_attest_types::InstallOriginWitness<'s>>) -> Decision + 'cb>;

/// Unit 5 (design r5 §3 as ratified, rulings 19/26): the lower-layer
/// per-call authority query, implemented app-side over the CURRENT
/// configuration, setup records, and coordinator state; threaded from the
/// worker's config through `run_turn` into `execute_one`. The callback is
/// CONSUMING (`FnOnce` — an implementation cannot invoke it twice), the
/// witness is scoped to it, and at most one witness backs one
/// authorization.
pub trait InstallAuthorityQuery: Send + Sync {
    fn with_witness(
        &self,
        origin: &awili_engine::ToolOrigin,
        f: InstallWitnessCallback<'_>,
    ) -> Decision;
}

/// The default query: no install authority anywhere (every non-MCP
/// composition; behavior identical to pre-unit-5).
pub struct NoInstallAuthority;

impl InstallAuthorityQuery for NoInstallAuthority {
    fn with_witness(
        &self,
        _origin: &awili_engine::ToolOrigin,
        f: InstallWitnessCallback<'_>,
    ) -> Decision {
        f(None)
    }
}

/// The authorizer: sole holder of the [`AuthzGate`]. Constructed once at
/// startup wiring (the gate half of
/// [`awili_engine_log::authz::issue_bundle`]), before any tool code loads.
pub struct Authorizer {
    gate: AuthzGate,
}

/// C1's output. Never an action — the host dispatches through B2's
/// evidence-checked `intent()` using the carried evidence.
#[must_use = "a Decision carries obligations: dispatch Allow evidence, resolve Ask, record Deny"]
pub enum Decision {
    /// Authorized: pass `evidence` straight to `ExecutionLog::intent`.
    Allow { evidence: AuthzEvidence },
    /// User approval required. Render the dialog; resolve via
    /// [`Authorizer::approval_request`]'s returned [`PendingApproval`].
    Ask(PendingApproval),
    /// Denied. The payload is a must-use recorder (Codex C1p2 P1: denial
    /// durability was caller-optional) — call `.record(log)` to persist
    /// the AuthzDenied audit record and obtain the error text for the
    /// reducer's ToolResult.
    Deny(DenialRecord),
}

/// Authorizer-owned pending approval. No public constructor, not Clone —
/// it exists only between a `Decision::Ask` and its resolution, and it is
/// consumed by value at resolve time (contract §4).
pub struct PendingApproval {
    pub(crate) turn_id: String,
    pub(crate) risk: RiskClass,
    /// The exact call the dialog is about — resolve() mints call-bound
    /// proofs from THIS, not from caller-supplied data.
    pub(crate) call: ToolCall,
    /// Authorization context captured at Ask time (Codex C1p2 P0): resolve
    /// uses THESE, not caller-supplied values — a pending raised in
    /// session/folder A cannot mint a grant for B.
    pub(crate) session_id: String,
    pub(crate) folder_root: Option<String>,
    /// The registry resolution the Ask was raised against (unit 4): the
    /// evidence `resolve` mints on approval binds THIS token's identity —
    /// the one the seam minted before authorization, never a value the
    /// resolve caller supplies.
    pub(crate) resolved: ResolvedTool,
    /// Human-readable basis for the dialog (tool, args summary, risk).
    pub(crate) prompt: String,
    /// U4 (D7): this Ask was raised by a non-overridable screening floor
    /// (secret path, read outside the folder, `fs.rm`). Auto mode may NOT
    /// approve a floor call — it fails closed (D7).
    pub(crate) floor: bool,
}

impl PendingApproval {
    pub fn prompt(&self) -> &str {
        &self.prompt
    }
    /// U4 (D7): was this Ask raised by a non-overridable screening floor?
    /// Floor calls always reach a real dialog — no grant, no auto mode
    /// clears them.
    pub fn floor_required(&self) -> bool {
        self.floor
    }
    pub fn risk(&self) -> RiskClass {
        self.risk
    }
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }
}

impl Authorizer {
    /// `gate` comes from the composition root's
    /// [`awili_engine_log::authz::issue_bundle`] wiring — startup-ordering
    /// invariant (contract §4/§9 threat model).
    pub fn new(gate: AuthzGate) -> Self {
        Self { gate }
    }

    /// Load/reload the grant set (gate-gated by construction: only the
    /// authorizer holds the gate).
    pub fn load_grants(
        &self,
        session_records: &[awili_engine_log::schema::LogRecordV1],
        grant_store_records: &[awili_engine_log::schema::LogRecordV1],
    ) -> GrantSet {
        GrantSet::load(&self.gate, session_records, grant_store_records)
    }

    /// The decision function. Pure; deterministic for identical inputs.
    /// Consumes the context BY VALUE (unit 5 ruling 15): the same
    /// witness-bearing context cannot back two authorizations.
    pub fn authorize(&self, call: &ToolCall, ctx: AuthzContext<'_, '_>) -> Decision {
        self.authorize_reaching(call, ctx, &[])
    }

    /// RC2-R2G1: [`Authorizer::authorize`] for a call that reaches files its
    /// arguments do not name (a search: `fs.glob`, `fs.grep`).
    /// `secret_files` are the secret files among them ([`is_secret_path`]):
    /// any at all puts the call at the screening floor, as a path argument
    /// naming a secret file does: the owner's card, which names them,
    /// whatever the risk class, grants or auto mode.
    pub fn authorize_reaching(
        &self,
        call: &ToolCall,
        ctx: AuthzContext<'_, '_>,
        secret_files: &[String],
    ) -> Decision {
        // 1. Risk classification: the starting class (RC1-M-FIX1: a
        // registry-classified non-builtin tool starts from its declared
        // class) escalated — never lowered — by argument inspection, and
        // never below the registry floor.
        let catalog_metadata =
            is_catalog_metadata_call(ctx.resolved_tool.identity(), call, ctx.registry_floor);
        let base = crate::risk::base_risk(&call.name, ctx.registry_floor);
        let mut risk = if catalog_metadata {
            // These exact, strictly parsed arguments are inert metadata. They
            // are still bound unchanged into C1 evidence and the B2 intent.
            base
        } else {
            crate::risk::classify_risk_from(call, base)
        };
        if let Some(floor) = ctx.registry_floor {
            risk = crate::risk::escalate_pub(risk, floor);
        }

        // 2. Textual screening floors (§6/§8), evaluated ONCE (Codex C1p2:
        // the double-call was an order-of-operations hazard). Deny is final.
        // RC2-ENGB1 (TOP43 #17): only the engine's own file tools resolve a
        // relative path inside the folder; the AUTHORITY-MINTED origin says
        // which tool this is, never the call's name.
        let in_folder = ctx.resolved_tool.identity().origin.origin == policy::BUILTIN_FS_ORIGIN;
        let screening = if catalog_metadata {
            Screening::Pass
        } else {
            screen(call, ctx.folder_root, risk, in_folder)
        };
        if let Screening::Deny { reason } = screening {
            return Decision::Deny(DenialRecord {
                turn_id: ctx.turn_id.to_string(),
                call: call.clone(),
                risk,
                reason,
            });
        }
        // RC2-R2G1: a search that reaches a secret file is at the floor too.
        let must_ask = screening == Screening::RequireAsk || !secret_files.is_empty();

        // 3. Grants (§5) — but RequireAsk floors are NON-OVERRIDABLE by
        // grants (Codex C1p2 P0): secrets/deletion/boundary escalations
        // always reach the dialog regardless of stored authority.
        if !must_ask {
            // Unit 5 §5: the witness's origin must equal the AUTHORITY-
            // MINTED resolution's origin — never anything caller-parsed.
            // A mismatched witness is discarded (fail closed), so the
            // install arm can only match the resolved tool's own origin.
            let witness = ctx
                .install_witness
                .as_ref()
                .filter(|w| w.origin() == &ctx.resolved_tool.identity().origin);
            if let Some(g) =
                ctx.grants
                    .matching(call, risk, ctx.session_id, ctx.folder_root, witness)
            {
                if let Ok(evidence) = awili_engine_log::authz::AuthzEvidence::from_grant(
                    &g.to_ref(&self.gate),
                    ctx.resolved_tool,
                    ctx.turn_id,
                    call,
                    ctx.folder_root,
                ) {
                    return Decision::Allow { evidence };
                }
            }
        }

        // 4. Policy: read_only calls that passed screening auto-allow.
        if risk == RiskClass::ReadOnly && !must_ask {
            if let Ok(evidence) = self.gate.policy_evidence(
                ctx.resolved_tool,
                ctx.turn_id,
                call,
                risk,
                ctx.folder_root,
            ) {
                return Decision::Allow { evidence };
            }
        }

        // 5. Everything else asks.
        Decision::Ask(PendingApproval {
            turn_id: ctx.turn_id.to_string(),
            risk,
            call: call.clone(),
            session_id: ctx.session_id.to_string(),
            folder_root: ctx.folder_root.map(str::to_string),
            resolved: ctx.resolved_tool.clone(),
            prompt: format!(
                "{} [{}] {}{}",
                call.name,
                risk.as_str(),
                summarize_args(&call.arguments),
                reaches_secrets(secret_files)
            ),
            // U4 (D7): carry the floor bit into the pending so auto mode
            // can fail closed on it (never touches the grant store).
            floor: must_ask,
        })
    }
}

/// Exact, installed metadata identity and strict argument shape. This narrow
/// exception only prevents inert search text from being mistaken for a path
/// or destructive flag; normal read-only grants and evidence still apply.
fn is_catalog_metadata_call(
    identity: &awili_engine::ToolIdentity,
    call: &ToolCall,
    floor: Option<RiskClass>,
) -> bool {
    if identity.origin.origin != "builtin:catalog"
        || identity.name != "tools.find"
        || floor != Some(RiskClass::ReadOnly)
    {
        return false;
    }
    is_catalog_metadata_arguments(&call.arguments)
}

/// Strict, bounded arguments for the read-only tool catalog. Missing fields use small defaults.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogArguments {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub select: Vec<String>,
    #[serde(default = "catalog_default_limit")]
    pub limit: u64,
    #[serde(default)]
    pub offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogArgumentsError {
    InvalidShape,
    QueryTooLong,
    TooManyAliases,
    AliasTooLong,
    DuplicateAlias,
    LimitOutOfRange,
    OffsetOutOfRange,
}
impl CatalogArgumentsError {
    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidShape => {
                "catalog arguments must be an object with only query, select, limit, and offset"
            }
            Self::QueryTooLong => "catalog query exceeds 256 characters",
            Self::TooManyAliases => "catalog selection exceeds 20 aliases",
            Self::AliasTooLong => "catalog alias exceeds 256 characters",
            Self::DuplicateAlias => "catalog selection contains a duplicate alias",
            Self::LimitOutOfRange => "catalog limit must be between 1 and 20",
            Self::OffsetOutOfRange => "catalog offset must be between 0 and 4294967295",
        }
    }
}
fn catalog_default_limit() -> u64 {
    10
}

/// Parse the metadata-only shape shared by C1 and the local handler. A closed
/// serde struct rejects unknown and duplicate keys; the object check also
/// rejects serde's sequence representation for structs.
pub fn parse_catalog_metadata_arguments(
    arguments: &str,
) -> Result<CatalogArguments, CatalogArgumentsError> {
    if !matches!(
        serde_json::from_str::<serde_json::Value>(arguments),
        Ok(serde_json::Value::Object(_))
    ) {
        return Err(CatalogArgumentsError::InvalidShape);
    }
    let args = serde_json::from_str::<CatalogArguments>(arguments)
        .map_err(|_| CatalogArgumentsError::InvalidShape)?;
    if args.query.chars().count() > 256 {
        return Err(CatalogArgumentsError::QueryTooLong);
    }
    if args.select.len() > 20 {
        return Err(CatalogArgumentsError::TooManyAliases);
    }
    if args.select.iter().any(|alias| alias.chars().count() > 256) {
        return Err(CatalogArgumentsError::AliasTooLong);
    }
    let unique: std::collections::BTreeSet<&str> = args.select.iter().map(String::as_str).collect();
    if unique.len() != args.select.len() {
        return Err(CatalogArgumentsError::DuplicateAlias);
    }
    if !(1..=20).contains(&args.limit) {
        return Err(CatalogArgumentsError::LimitOutOfRange);
    }
    if args.offset > u32::MAX as u64 {
        return Err(CatalogArgumentsError::OffsetOutOfRange);
    }
    Ok(args)
}
pub fn is_catalog_metadata_arguments(arguments: &str) -> bool {
    parse_catalog_metadata_arguments(arguments).is_ok()
}

#[cfg(test)]
mod catalog_metadata_tests {
    use super::*;
    use awili_engine::{DescriptorDigest, ToolOrigin};

    fn identity(origin: &str, name: &str) -> awili_engine::ToolIdentity {
        awili_engine::ToolIdentity::new(ToolOrigin::new(origin), name, DescriptorDigest([7; 32]))
    }

    fn call(arguments: &str) -> ToolCall {
        ToolCall {
            awili_call_id: awili_engine::events::AwiliCallId("catalog-test".into()),
            provider_call_id: Some("provider-catalog-test".into()),
            name: "tools.find".into(),
            arguments: arguments.into(),
        }
    }

    #[test]
    fn catalog_arguments_default_fields_and_allow_query_with_selection() {
        let args = parse_catalog_metadata_arguments(r#"{}"#).unwrap();
        assert_eq!(args.query, "");
        assert!(args.select.is_empty());
        assert_eq!(args.limit, 10);
        assert_eq!(args.offset, 0);
        let args = parse_catalog_metadata_arguments(
            r#"{"query":"mac-shell","select":["mac-shell.run_command"]}"#,
        )
        .unwrap();
        assert_eq!(args.query, "mac-shell");
        assert_eq!(args.select, ["mac-shell.run_command"]);
        assert!(matches!(
            parse_catalog_metadata_arguments(r#"{"limit":100}"#),
            Err(CatalogArgumentsError::LimitOutOfRange)
        ));
        for observed in [
            r#"{"query":"mac-shell","select":["mcp__mac_shell__run_command"]}"#,
            r#"{"limit":10,"query":"mac-shell","select":["mcp__mac_shell__run_command"]}"#,
            r#"{"limit":100,"query":"run_command","select":["mcp__mac_shell__run_command"]}"#,
            r#"{"limit":100,"query":"","select":[]}"#,
        ] {
            let result = parse_catalog_metadata_arguments(observed);
            if observed.contains("100") {
                assert!(matches!(
                    result,
                    Err(CatalogArgumentsError::LimitOutOfRange)
                ));
            } else {
                assert!(result.is_ok());
            }
        }
    }

    #[test]
    fn only_exact_readonly_catalog_identity_treats_query_strings_as_metadata() {
        let args = r#"{"query":"read /outside/.env ../private","select":[],"limit":5,"offset":0}"#;
        assert!(is_catalog_metadata_call(
            &identity("builtin:catalog", "tools.find"),
            &call(args),
            Some(RiskClass::ReadOnly),
        ));
        assert!(!is_catalog_metadata_call(
            &identity("mcp:server", "tools.find"),
            &call(args),
            Some(RiskClass::ReadOnly),
        ));
        assert!(!is_catalog_metadata_call(
            &identity("builtin:catalog", "tools.find"),
            &call(args),
            Some(RiskClass::Mutating),
        ));
    }

    #[test]
    fn metadata_arguments_reject_force_and_unknown_fields() {
        let id = identity("builtin:catalog", "tools.find");
        for args in [
            r#"{"query":"x","select":[],"limit":5,"offset":0,"force":true}"#,
            r#"{"query":"x","select":[],"limit":5,"offset":0,"recursive":true}"#,
            r#"{"query":"","select":["a","a"],"limit":5,"offset":0}"#,
        ] {
            assert!(!is_catalog_metadata_call(
                &id,
                &call(args),
                Some(RiskClass::ReadOnly)
            ));
        }
        assert!(!is_catalog_metadata_call(
            &id,
            &call(r#"{"query":"x","query":"/tmp","select":[],"limit":5,"offset":0}"#),
            Some(RiskClass::ReadOnly)
        ));
        let long_alias = serde_json::json!({
            "query":"", "select":["x".repeat(257)], "limit":5, "offset":0
        })
        .to_string();
        assert!(!is_catalog_metadata_call(
            &id,
            &call(&long_alias),
            Some(RiskClass::ReadOnly)
        ));
        assert!(!is_catalog_metadata_call(
            &id,
            &call(r#"["x",[],5,0]"#),
            Some(RiskClass::ReadOnly)
        ));
    }
}

/// RC2-R2G1: what a search's card adds when it reaches secret files: their
/// names (the first five, then how many more), so the owner knows what an
/// Allow shows.
fn reaches_secrets(files: &[String]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut names = files.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    if files.len() > 5 {
        names.push_str(&format!(" and {} more", files.len() - 5));
    }
    format!("{SECRET_FILES_NOTE}{names}")
}

/// RC2-R2G1: the words that lead the secret files' names in a search's
/// prompt (after them, the names), so a surface can say them its own way.
pub const SECRET_FILES_NOTE: &str = " \u{2014} it also shows secret files: ";

/// The user's dialog answer. Constructed by the dialog controller — the
/// entity holding the HostApprovalCapability; resolve() requires both.
pub enum UserVerdict {
    /// scope: "once" | "session" | "folder" | "always"
    Approve {
        scope: String,
    },
    Deny {
        reason: String,
    },
}

impl Authorizer {
    /// Resolve a pending approval (contract §4). Consumes the pending value;
    /// requires the dialog capability. Approval: durably records the grant
    /// in the CORRECT store (once/session -> session log; folder/always ->
    /// grant store) and returns call-bound evidence for THIS call. Denial:
    /// durably records AuthzDenied and returns None.
    /// Context comes from the PENDING value (bound at Ask time), never
    /// from resolve's caller (Codex C1p2 P0).
    pub fn resolve<S1: AppendOnlyLog>(
        &self,
        pending: PendingApproval,
        verdict: UserVerdict,
        cap: &HostApprovalCapability,
        session_log: &mut ExecutionLog<S1>,
        // Unit 5 (r3 verdict rev 3): the per-user grant store is reached
        // through the service trait — one locked critical section per
        // mutation, minted proof returned to the caller after release.
        grant_store: &mut dyn awili_engine_log::grant_service::GrantStoreAccess,
    ) -> Result<Option<AuthzEvidence>, LogError> {
        // Session binding (Codex C1p2 P0): a pending raised in session A
        // cannot mint records into session B's log.
        if session_log.session_id() != pending.session_id {
            return Err(LogError::EvidenceMismatch(format!(
                "pending approval bound to session {:?}, resolve offered log for {:?}",
                pending.session_id,
                session_log.session_id()
            )));
        }
        let folder_root = pending.folder_root.clone();
        let folder_root = folder_root.as_deref();
        match verdict {
            UserVerdict::Deny { reason } => {
                session_log.record_denial(
                    &pending.turn_id,
                    &pending.call,
                    pending.risk,
                    &reason,
                )?;
                Ok(None)
            }
            UserVerdict::Approve { scope } => {
                // Unit 5 (design r2 §3.1): "install" is NOT a dialog
                // answer — its only minter is the seed path through the
                // restricted store API. Unconditional refusal here keeps
                // a user click from ever minting permanent install trust.
                if scope == "install" {
                    return Err(LogError::EvidenceMismatch(
                        "'install' is not a user-grantable scope".into(),
                    ));
                }
                // Contract §8 floor re-checked here too: no 'always' for
                // irreversible/privileged (B2 also rejects; fail early).
                if scope == "always"
                    && matches!(
                        pending.risk,
                        RiskClass::Irreversible | RiskClass::Privileged
                    )
                {
                    return Err(LogError::EvidenceMismatch(
                        "irreversible/privileged cannot be granted 'always'".into(),
                    ));
                }
                // CONSERVATIVE CONSTRAINT DEFAULT for the grants that OUTLIVE
                // the session: folder/always grants above read_only
                // auto-capture the call's path-carrying argument keys as
                // exact-match constraints — approving one write to report.txt
                // "for this folder" does NOT grant writes everywhere.
                // RC1-I2 (RC1 round 3b, E2-5b: "Allow for this session" then
                // the next write asked again): a SESSION grant covers this
                // tool at this risk class for the rest of the session, any
                // path — the session root still confines every path, the
                // screening floors still force a dialog, and the grant ends
                // with the session. read_only reusable grants stay
                // unconstrained (browsing ergonomics).
                let constraints = if matches!(scope.as_str(), "folder" | "always")
                    && pending.risk != RiskClass::ReadOnly
                {
                    constraint_capture(&pending.call)
                } else {
                    "{}".to_string()
                };
                let proof = match scope.as_str() {
                    "once" | "session" => session_log.record_approval(
                        cap,
                        &scope,
                        &pending.turn_id,
                        &pending.call,
                        pending.risk,
                        None,
                        &constraints,
                    )?,
                    "folder" | "always" => grant_store.record_grant_approval(
                        cap,
                        &scope,
                        &pending.turn_id,
                        &pending.call,
                        pending.risk,
                        if scope == "folder" { folder_root } else { None },
                        &constraints,
                    )?,
                    other => {
                        return Err(LogError::EvidenceMismatch(format!(
                            "invalid approval scope {other:?}"
                        )));
                    }
                };
                Ok(Some(AuthzEvidence::from_ask(
                    proof,
                    &pending.resolved,
                    folder_root,
                )))
            }
        }
    }

    /// U4 (D7): resolve a non-floor [`Decision::Ask`] in Auto mode — no
    /// dialog, no grant store. Runs the same session-binding check as
    /// [`Self::resolve`], refuses floor-escalated calls (fail closed), and
    /// otherwise records a scope-`"once"` approval in the SESSION log with
    /// [`AUTO_APPROVAL_CONSTRAINTS`] (the auto-mode audit signature) and
    /// returns call-bound evidence for THIS call. Never touches the grant
    /// store: auto mode mints no reusable authority.
    pub fn resolve_auto<S1: AppendOnlyLog>(
        &self,
        pending: PendingApproval,
        cap: &HostApprovalCapability,
        session_log: &mut ExecutionLog<S1>,
    ) -> Result<AuthzEvidence, LogError> {
        // Session binding (same check as `resolve`, Codex C1p2 P0): a
        // pending raised in session A cannot mint records into B's log.
        if session_log.session_id() != pending.session_id {
            return Err(LogError::EvidenceMismatch(format!(
                "pending approval bound to session {:?}, auto-resolve offered log for {:?}",
                pending.session_id,
                session_log.session_id()
            )));
        }
        // D7 floors are non-overridable: the authorizer refuses to
        // auto-resolve them — secret files and reads outside the folder
        // still show the dialog.
        if pending.floor {
            return Err(LogError::EvidenceMismatch(
                "auto mode cannot approve a floor-escalated call".into(),
            ));
        }
        let proof = session_log.record_approval(
            cap,
            "once",
            &pending.turn_id,
            &pending.call,
            pending.risk,
            None,
            AUTO_APPROVAL_CONSTRAINTS,
        )?;
        Ok(AuthzEvidence::from_ask(
            proof,
            &pending.resolved,
            pending.folder_root.as_deref(),
        ))
    }
}

/// Must-use denial payload: couples the Deny decision to its audit record.
#[must_use = "denials MUST be durably recorded — call .record(session_log)"]
pub struct DenialRecord {
    pub(crate) turn_id: String,
    pub(crate) call: ToolCall,
    pub(crate) risk: RiskClass,
    pub(crate) reason: String,
}

impl DenialRecord {
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn risk(&self) -> RiskClass {
        self.risk
    }
    /// Persist the AuthzDenied record; returns the reason for the error
    /// ToolResult the reducer should receive.
    pub fn record<S: AppendOnlyLog>(
        self,
        session_log: &mut ExecutionLog<S>,
    ) -> Result<String, LogError> {
        session_log.record_denial(&self.turn_id, &self.call, self.risk, &self.reason)?;
        Ok(self.reason)
    }
}

/// Constraints from the call (Codex re-gate P0 — DEEP, not top-level):
/// exact-match top-level pathy string keys PLUS a `__paths` profile of ALL
/// normalized filesystem-path argument values, computed by the SAME walk
/// screening uses. Matching (grants.rs) recomputes the profile from the
/// candidate call and requires equality — nested/array paths cannot escape
/// the constraint. Commands are not paths, but their own exact `__commands`
/// profile prevents a constrained folder/always approval from authorizing a
/// changed command. Session and install scopes retain their existing broader
/// authorization contracts.
fn constraint_capture(call: &ToolCall) -> String {
    let mut out = serde_json::Map::new();
    if let Ok(serde_json::Value::Object(args)) =
        serde_json::from_str::<serde_json::Value>(&call.arguments)
    {
        for (k, v) in &args {
            if crate::risk::key_is_pathy_pub(k) && v.is_string() {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    let profile = crate::risk::path_profile(&call.arguments);
    if !profile.is_empty() {
        out.insert(
            "__paths".into(),
            serde_json::Value::Array(profile.into_iter().map(serde_json::Value::String).collect()),
        );
    }
    let commands = crate::risk::command_profile(&call.arguments);
    if !commands.is_empty() {
        out.insert(
            "__commands".into(),
            serde_json::Value::Array(
                commands
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    serde_json::to_string(&serde_json::Value::Object(out)).unwrap_or_else(|_| "{}".into())
}

/// The approval prompt's one-line argument summary: newlines become spaces,
/// and a summary over 120 characters keeps its first 117 plus "...".
/// RC1-F2c (F0552): counted in characters, never bytes, so a multi-byte
/// character at the cut can no longer panic the approval prompt.
fn summarize_args(args: &str) -> String {
    let s = args.replace(['\n', '\r'], " ");
    if s.chars().count() <= 120 {
        return s;
    }
    let mut cut: String = s.chars().take(117).collect();
    cut.push_str("...");
    cut
}
