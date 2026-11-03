//! B2-owned authorization machinery (C1 contract v2 r12, §2/§4/§9).
//!
//! These types live in the LOG crate — not the authz crate — so the C1
//! policy engine can depend on B2 without a cycle, and so no caller can
//! reach `intent()` with fabricated evidence:
//!
//! - [`HostApprovalCapability`] / [`AuthzGate`]: issue-once capabilities.
//!   The dialog controller holds the first; the C1 authorizer holds the
//!   second. Startup-ordering invariant: all bundle members are issued in
//!   `main()` before any engine/tool/skill code loads (contract §4 threat
//!   model — pre-main arbitrary code is explicitly out of scope).
//! - [`RegistryAuthority`] / [`ResolvedTool`] (unit 4, design r4/r5): the
//!   registry half of the same sole-issue bundle. The authority mints
//!   resolution tokens; no `ResolvedTool` can exist without it.
//! - [`OnceProof`] / [`GrantProof`] (via [`MintedProof`]): minted only by
//!   `ExecutionLog::record_approval`, call-bound, non-clonable.
//! - [`AuthzEvidence`]: private-field struct; the ONLY values accepted by
//!   the public `ExecutionLog::intent`. Construction requires the gate or
//!   a minted proof; every value is bound to exactly one
//!   (turn_id, awili_call_id, normalized-call hash).

use std::sync::atomic::{AtomicU8, Ordering};
#[cfg(feature = "test-authority")]
use std::sync::OnceLock;

use awili_engine::events::ToolCall;
use awili_engine::ToolIdentity;

use awili_engine::ToolOrigin;

use crate::writer::{call_fingerprint, canonicalize_args};
use crate::LogError;

/// All four issue-once flags live in ONE atomic word so a caller that
/// needs the whole authority bundle can claim it in a single
/// compare-exchange.
///
/// Independent flags made the bundle non-atomic: a caller that took the
/// gate and then lost the capability stranded the gate forever — consumed,
/// unreachable, and (via the caller's `OnceLock`) cached as a permanent
/// failure for the process lifetime. One word, one CAS, no partial claim
/// (Codex P1, 2026-08-13; three bits by unit 4 r5; four by unit 5 ruling 24).
/// Rolling a flag back was rejected: releasing a claimed bit would make
/// "issued once" observably false to a racing thread, and it cannot repair
/// the case where another owner already holds another bit.
static ISSUED: AtomicU8 = AtomicU8::new(0);

const GATE_BIT: u8 = 0b01;
const CAP_BIT: u8 = 0b10;
const REGISTRY_BIT: u8 = 0b100;
/// Unit 5 (design r9, verdict rulings 24/33): the install-seed floor
/// authority — the fourth bundle member, confined after issuance to the
/// process-long catalog owner in `awili-engine-worker`.
const INSTALL_BIT: u8 = 0b1000;

/// Claim every bit in `bits`, or none of them. `Err` means at least one was
/// already taken — nothing is consumed on the failure path.
fn claim(bits: u8) -> Result<(), AlreadyIssued> {
    let mut current = ISSUED.load(Ordering::Acquire);
    loop {
        if current & bits != 0 {
            return Err(AlreadyIssued);
        }
        match ISSUED.compare_exchange_weak(
            current,
            current | bits,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Ok(()),
            Err(observed) => current = observed,
        }
    }
}

/// Issue ALL FOUR process-global capabilities atomically: the C1 gate, the
/// host approval capability, the registry authority (unit 4, design
/// r5/r7), and the install-seed authority (unit 5, rulings 24/33).
///
/// The only correct way to obtain any of them. Claiming the bits in
/// separate calls is a bug: an earlier success paired with a later failure
/// strands the earlier capabilities — consumed with no holder, and (via the
/// caller's `OnceLock`) cached as a permanent failure for the process
/// lifetime — see [`ISSUED`]. One word, one CAS: a process either holds the
/// whole bundle or nothing changed.
pub fn issue_bundle() -> Result<
    (
        AuthzGate,
        HostApprovalCapability,
        RegistryAuthority,
        InstallSeedAuthority,
    ),
    AlreadyIssued,
> {
    claim(GATE_BIT | CAP_BIT | REGISTRY_BIT | INSTALL_BIT)?;
    Ok((
        AuthzGate { _priv: () },
        HostApprovalCapability { _priv: () },
        RegistryAuthority { _priv: () },
        InstallSeedAuthority { _priv: () },
    ))
}

/// The shared test fixture (unit 4 design r7, Delta A‴): the process-global
/// authority bundle, claimed once through [`issue_bundle`] and handed out as
/// a shared reference forever after.
///
/// Exists only under the `test-authority` feature, which rides
/// `[dev-dependencies]` edges and never the production graph. Repeated
/// calls return the SAME reference, so no test can even attempt a second
/// issuance — crates that today cache a gate in their own `OnceLock` drop
/// that local cache in favor of this one. Exhaustion coverage (partial-claim
/// atomicity) stays inside this crate against the `pub(crate)` standalone
/// paths; a test composition that must OWN a member (e.g. constructing an
/// `Authorizer`) propagates possession from the shared reference via the
/// feature-gated `Clone` — possession copied, never re-issued.
#[cfg(feature = "test-authority")]
pub fn test_bundle() -> &'static (
    AuthzGate,
    HostApprovalCapability,
    RegistryAuthority,
    InstallSeedAuthority,
) {
    static BUNDLE: OnceLock<(
        AuthzGate,
        HostApprovalCapability,
        RegistryAuthority,
        InstallSeedAuthority,
    )> = OnceLock::new();
    BUNDLE.get_or_init(|| issue_bundle().expect("the sole authority bundle of this test process"))
}

#[derive(Debug)]
pub struct AlreadyIssued;

/// Held by the approval-dialog controller alone. Required to mint proofs.
///
/// `Clone` is feature-gated to `test-authority` for the fixture's
/// shared-reference hand-off; a production build keeps the capability
/// linear. Cloning presupposes possession — issuance counts are unaffected.
#[derive(Debug)]
#[cfg_attr(feature = "test-authority", derive(Clone))]
pub struct HostApprovalCapability {
    _priv: (),
}

impl HostApprovalCapability {
    /// In-crate test fixture (unit 5 s1, per the r6-verdict "confined
    /// test-only constructor" precedent): the grant-service tests exercise
    /// `record_approval`, whose capability parameter is a WHO-gate, not
    /// data. cfg(test) only — never a production or cross-crate surface,
    /// and it claims no bit, so it cannot race the issuance tests.
    #[cfg(test)]
    pub(crate) fn test_only() -> Self {
        Self { _priv: () }
    }

    /// Standalone issuance for the exhaustion tests only (unit 4 design r5):
    /// external production code obtains this capability only as part of
    /// [`issue_bundle`]. Visible to the in-crate suite and — under the
    /// dev-only `test-authority` feature, which never rides the production
    /// graph — to the standalone `failed_bundle_claim_consumes_no_free_bit`
    /// integration target, which proves the bundle's no-partial-claim
    /// guarantee from a deliberately partial state in a fresh process.
    #[cfg(any(test, feature = "test-authority"))]
    pub fn issue() -> Result<Self, AlreadyIssued> {
        claim(CAP_BIT)?;
        Ok(Self { _priv: () })
    }
}

/// Held by the C1 authorizer alone. Required to construct policy evidence
/// and to load the grant set (contract §9's AuthzGate).
///
/// `Clone` is feature-gated to `test-authority` — see
/// [`HostApprovalCapability`] for the rationale.
#[derive(Debug)]
#[cfg_attr(feature = "test-authority", derive(Clone))]
pub struct AuthzGate {
    _priv: (),
}

impl AuthzGate {
    /// Standalone issuance for the exhaustion tests only (unit 4 design r5):
    /// external production code obtains the gate only as part of
    /// [`issue_bundle`]. Visible to the in-crate suite and — under the
    /// dev-only `test-authority` feature — to the standalone
    /// `failed_bundle_claim_consumes_no_free_bit` integration target
    /// (see [`HostApprovalCapability::issue`]).
    #[cfg(any(test, feature = "test-authority"))]
    pub fn issue() -> Result<Self, AlreadyIssued> {
        claim(GATE_BIT)?;
        Ok(Self { _priv: () })
    }

    /// decision_basis = "policy": rule-based allow, no user interaction.
    /// Call-bound; risk class travels inside the evidence. The identity is
    /// read out of the authority-minted [`ResolvedTool`] (unit 4, design
    /// r4 Delta A): C1 can never be handed a bare caller-selected identity.
    pub fn policy_evidence(
        &self,
        resolved: &ResolvedTool,
        turn_id: &str,
        call: &ToolCall,
        risk_class: RiskClass,
        scope_root: Option<&str>,
    ) -> Result<AuthzEvidence, LogError> {
        Ok(AuthzEvidence {
            basis: Basis::Policy,
            binding: CallBinding::for_call(turn_id, call)?,
            risk_class,
            approval_ref: String::new(),
            scope_root: scope_root.map(str::to_string),
            identity: resolved.identity().clone(),
        })
    }
}

/// The registry half of the authority bundle (unit 4, design r4/r5 Delta
/// A/A′): sole-issue through the same [`ISSUED`] word as the gate and the
/// capability, obtainable only from [`issue_bundle`]. Held by the
/// composition root and threaded to the seam — registry resolution is the
/// ONLY site that may mint a [`ResolvedTool`].
///
/// `Clone` is feature-gated to `test-authority` — see
/// [`HostApprovalCapability`] for the rationale (possession propagated
/// from the shared fixture's reference, never a re-issuance).
#[cfg_attr(feature = "test-authority", derive(Clone))]
pub struct RegistryAuthority {
    _priv: (),
}

impl RegistryAuthority {
    /// Resolve an identity into the unforgeable token C1 and the executor
    /// seam consume (unit 4 item 3 onward). No IO, no lookup — the caller
    /// supplies the identity the registry resolved; the type's job is to
    /// make a caller-FABRICATED identity unrepresentable downstream.
    pub fn mint(&self, identity: ToolIdentity) -> ResolvedTool {
        ResolvedTool { identity }
    }
}

/// Unit 5 (design r9 §§1-2 of the r5-r8 verdict line): the install-seed
/// floor authority — the fourth bundle member.
///
/// Custody contract (rulings 23/29-32): `issue_bundle` is the ONLY
/// constructor; `awili-engine-worker`'s `authority()` moves this value
/// straight into its crate-private, process-long catalog owner, and no
/// public API anywhere returns, borrows, or exposes it afterwards. The
/// externally reachable operation is that owner's `accepted_floors(origin)`
/// alone. NOT `Clone` in production; the feature-gated `Clone` follows the
/// sibling capabilities' fixture rationale.
#[cfg_attr(feature = "test-authority", derive(Clone))]
pub struct InstallSeedAuthority {
    _priv: (),
}

/// The committed accepted catalog's floor set for ONE origin at ONE
/// generation (verdict rulings 17/22): private fields, no constructor from
/// caller data without the confined authority, not `Clone`, read-only
/// accessors. Provenance is structural — the only value of
/// [`InstallSeedAuthority`] in existence lives inside the catalog owner,
/// whose mint site reads the committed `LiveEntry` under its own lock.
/// Pinned by compile-fail doctests:
///
/// ```compile_fail
/// // error[E0451]: fields are private — no raw floors construction.
/// let f = awili_engine_log::authz::AcceptedFloors {
///     origin: panic!(), generation: panic!(), floors: panic!(),
/// };
/// ```
///
/// ```compile_fail
/// // error[E0451]: no authority value is constructible outside
/// // issue_bundle — there is no second mint site to hold one.
/// let a = awili_engine_log::authz::InstallSeedAuthority { _priv: () };
/// ```
pub struct AcceptedFloors {
    origin: ToolOrigin,
    generation: awili_engine::generation::ConnectionGeneration,
    floors: Vec<(String, RiskClass)>,
}

impl InstallSeedAuthority {
    /// Standalone issuance for the exhaustion tests only — the sibling
    /// capabilities' pattern verbatim: production obtains this member only
    /// through [`issue_bundle`], and unit 5's custody moves it straight
    /// into the worker-crate owner.
    #[cfg(any(test, feature = "test-authority"))]
    pub fn issue() -> Result<Self, AlreadyIssued> {
        claim(INSTALL_BIT)?;
        Ok(Self { _priv: () })
    }

    /// The ONE `AcceptedFloors` constructor. Data-shaped by necessity (the
    /// committed catalog lives a crate above), capability-gated by
    /// construction: no other site can name an authority value (ruling 23's
    /// "equivalent topology" clause — alternate mint sites are
    /// unrepresentable, not merely discouraged).
    pub fn mint_floors(
        &self,
        origin: ToolOrigin,
        generation: awili_engine::generation::ConnectionGeneration,
        floors: Vec<(String, RiskClass)>,
    ) -> AcceptedFloors {
        AcceptedFloors {
            origin,
            generation,
            floors,
        }
    }
}

impl AcceptedFloors {
    pub fn origin(&self) -> &ToolOrigin {
        &self.origin
    }
    pub fn generation(&self) -> awili_engine::generation::ConnectionGeneration {
        self.generation
    }
    pub fn floors(&self) -> impl Iterator<Item = (&str, RiskClass)> {
        self.floors.iter().map(|(n, r)| (n.as_str(), *r))
    }
}

/// Why an install-seed candidate refused to mint (unit 5 §5.1 of the
/// design of record).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SeedRefusal {
    #[error("floors are for {floors:?} but the verified launch is {launch:?}")]
    OriginMismatch { floors: String, launch: String },
    #[error("floors are for generation {floors} but the verified launch is {launch}")]
    GenerationMismatch {
        floors: awili_engine::generation::ConnectionGeneration,
        launch: awili_engine::generation::ConnectionGeneration,
    },
}

/// The opaque seed input the app's attestation boundary produces and the
/// worker's store owner consumes (rulings 3/16): origin, generation, and
/// setup facts copied from the VERIFIED launch — nothing caller-selected —
/// and the floor set consumed from the committed catalog artifact after an
/// origin/generation cross-check. Crosses the turn-open boundary ONCE; not
/// `Clone`, not `Copy`, no serde.
pub struct InstallSeedCandidate {
    origin: ToolOrigin,
    generation: awili_engine::generation::ConnectionGeneration,
    setup_id: awili_attest_types::SetupIdentityId,
    pinned_requirement: String,
    floors: Vec<(String, RiskClass)>,
}

impl InstallSeedCandidate {
    /// Copies identity facts from the launch (`VerifiedLaunch` carries its
    /// own origin and generation privately — ruling 14: possession
    /// authorizes only facts copied from that same launch); verifies the
    /// floors artifact names the SAME origin and generation; consumes it.
    pub fn mint(
        launch: &awili_attest_types::VerifiedLaunch,
        floors: AcceptedFloors,
    ) -> Result<Self, SeedRefusal> {
        if floors.origin() != launch.origin() {
            return Err(SeedRefusal::OriginMismatch {
                floors: floors.origin().origin.clone(),
                launch: launch.origin().origin.clone(),
            });
        }
        if floors.generation() != launch.generation() {
            return Err(SeedRefusal::GenerationMismatch {
                floors: floors.generation(),
                launch: launch.generation(),
            });
        }
        Ok(Self {
            origin: launch.origin().clone(),
            generation: launch.generation(),
            setup_id: launch.setup_id().clone(),
            pinned_requirement: launch.pinned_requirement().to_string(),
            floors: floors.floors,
        })
    }

    pub fn origin(&self) -> &ToolOrigin {
        &self.origin
    }
    pub fn generation(&self) -> awili_engine::generation::ConnectionGeneration {
        self.generation
    }
    pub fn setup_id(&self) -> &awili_attest_types::SetupIdentityId {
        &self.setup_id
    }
    pub fn pinned_requirement(&self) -> &str {
        &self.pinned_requirement
    }
    pub fn floors(&self) -> impl Iterator<Item = (&str, RiskClass)> {
        self.floors.iter().map(|(n, r)| (n.as_str(), *r))
    }
}

/// A registry-resolution token: tool identity that cannot be constructed
/// outside the authority. The field is private and there is no public
/// constructor — the only path to a `ResolvedTool` is
/// [`RegistryAuthority::mint`] from a held authority, unforgeability by
/// capability possession (the codebase's established idiom, `approval_cap`
/// precedent). Pinned by a compile-fail doctest:
///
/// ```compile_fail
/// // error[E0616]: field `identity` is private — construction without a
/// // held RegistryAuthority does not compile outside this crate.
/// let tool = awili_engine_log::ResolvedTool { identity: panic!() };
/// ```
#[derive(Debug, Clone)]
pub struct ResolvedTool {
    pub(crate) identity: ToolIdentity,
}

impl ResolvedTool {
    /// The registry-resolved identity (r4: C1 reads the identity out of the
    /// token; it can never be handed a bare caller-selected one).
    pub fn identity(&self) -> &ToolIdentity {
        &self.identity
    }
}

pub use awili_engine::RiskClass;

/// The (turn, call, content) triple every evidence value is bound to
/// (contract §9 universal binding rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallBinding {
    pub(crate) turn_id: String,
    pub(crate) awili_call_id: String,
    /// Over (tool + canonicalized arguments) — the same normalization the
    /// Intent record persists, so replay can re-derive it independently.
    pub(crate) normalized_call_hash: String,
}

impl CallBinding {
    pub(crate) fn for_call(turn_id: &str, call: &ToolCall) -> Result<Self, LogError> {
        let canonical = canonicalize_args(&call.arguments)?;
        Ok(Self {
            turn_id: turn_id.to_string(),
            awili_call_id: call.awili_call_id.0.clone(),
            normalized_call_hash: call_fingerprint(&call.name, &canonical),
        })
    }

    pub(crate) fn matches(&self, turn_id: &str, call: &ToolCall) -> Result<bool, LogError> {
        Ok(*self == Self::for_call(turn_id, call)?)
    }
}

/// Proof of a durably-recorded `scope="once"` approval. Single-use by
/// construction (consumed by value), call-bound.
#[derive(Debug)]
pub struct OnceProof {
    pub(crate) approval_ref: String,
    pub(crate) binding: CallBinding,
    pub(crate) risk_class: RiskClass,
}

/// Proof of a durably-recorded reusable approval (session/folder/always).
/// Its FIRST use rides `Ask` (basis "ask"); later uses have no proof and
/// resolve through the grant set as `grant` basis.
#[derive(Debug)]
pub struct GrantProof {
    pub(crate) approval_ref: String,
    pub(crate) binding: CallBinding,
    pub(crate) risk_class: RiskClass,
}

/// What `record_approval` returns; scope determines the concrete type.
#[derive(Debug)]
pub enum MintedProof {
    Once(OnceProof),
    Grant(GrantProof),
}

/// A handle to an inherited grant, minted by the gate holder's grant-set
/// loader (C1 part 2, `awili_engine_authz::grants`). `intent()` does NOT
/// re-validate the ref against the grant store (the session log cannot see
/// it); replay classifies a dangling, revoked or wrong-store ref
/// AuthzInvalid.
#[derive(Debug)]
pub struct GrantRef {
    pub(crate) approval_ref: String,
    pub(crate) risk_class: RiskClass,
}

impl GrantRef {
    /// RC2-ENG2 (F0555): PUBLIC and gate-gated — not crate-private. Any
    /// holder of the issue-once [`AuthzGate`] can mint a ref for any string;
    /// the gate is the whole boundary. It is held only by the C1 authorizer,
    /// whose `GrantSet` loader validates against the log records BEFORE
    /// calling this. Replay's dangling-ref / wrong-store AuthzInvalid
    /// classification is the defense in depth behind it. Without the gate
    /// this cannot be called (compile-fail pinned in the C1 acceptance
    /// suite).
    pub fn from_grant_set(_gate: &AuthzGate, approval_ref: &str, risk_class: RiskClass) -> Self {
        Self {
            approval_ref: approval_ref.to_string(),
            risk_class,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Basis {
    Policy,
    Ask,
    Grant,
}

impl Basis {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Basis::Policy => "policy",
            Basis::Ask => "ask",
            Basis::Grant => "grant",
        }
    }
}

/// The only currency `ExecutionLog::intent` accepts. No public fields, no
/// public constructor: policy evidence needs the gate, ask evidence needs a
/// minted proof, grant evidence needs a validated GrantRef + call binding.
#[derive(Debug)]
pub struct AuthzEvidence {
    pub(crate) basis: Basis,
    pub(crate) binding: CallBinding,
    pub(crate) risk_class: RiskClass,
    pub(crate) approval_ref: String,
    /// The C1->C3 scope-root handshake (contract §6): the authorized
    /// session root. C2's runtime hands this to C3, which opens it ONCE as
    /// a directory descriptor and resolves all components beneath it.
    pub(crate) scope_root: Option<String>,
    /// The registry-resolved tool identity (unit 4, design r3 Delta 2 /
    /// r4 Delta A), bound ONLY through an authority-minted
    /// [`ResolvedTool`] — every constructor takes the token, no
    /// constructor accepts a free caller identity, and the field is
    /// private, so evidence without a bound identity is unrepresentable.
    pub(crate) identity: ToolIdentity,
}

impl AuthzEvidence {
    /// Read by the C2 runtime to seed C3's descriptor-relative resolution.
    pub fn scope_root(&self) -> Option<&str> {
        self.scope_root.as_deref()
    }

    /// The evidence-bound tool identity (unit 4): the value the immediate
    /// pre-dispatch check compares the registry's CURRENT resolution
    /// against, and the value `intent()` persists in the durable record.
    pub fn tool_identity(&self) -> &ToolIdentity {
        &self.identity
    }

    /// Does this evidence authorize EXACTLY this call? Same binding check
    /// `intent()` performs, exposed so downstream enforcement layers (C3)
    /// can refuse evidence replayed from another call.
    pub fn authorizes(&self, turn_id: &str, call: &ToolCall) -> bool {
        self.binding.matches(turn_id, call).unwrap_or(false)
    }
}

impl AuthzEvidence {
    /// decision_basis = "ask": this interaction's fresh approval (any
    /// scope), consuming the just-minted, call-bound proof. The identity
    /// comes from the SAME authority-minted [`ResolvedTool`] the Ask
    /// decision captured (unit 4): resolve binds the token's identity, not
    /// a caller-supplied one.
    pub fn from_ask(
        proof: MintedProof,
        resolved: &ResolvedTool,
        scope_root: Option<&str>,
    ) -> AuthzEvidence {
        let (approval_ref, binding, risk_class) = match proof {
            MintedProof::Once(p) => (p.approval_ref, p.binding, p.risk_class),
            MintedProof::Grant(p) => (p.approval_ref, p.binding, p.risk_class),
        };
        AuthzEvidence {
            basis: Basis::Ask,
            binding,
            risk_class,
            approval_ref,
            scope_root: scope_root.map(str::to_string),
            identity: resolved.identity().clone(),
        }
    }

    /// decision_basis = "grant": inherited grant; each USE mints call-bound
    /// evidence for exactly one call. The identity is read out of the
    /// authority-minted [`ResolvedTool`] (unit 4) — a grant use binds the
    /// registry's resolution for THIS call, never a free identity.
    pub fn from_grant(
        grant: &GrantRef,
        resolved: &ResolvedTool,
        turn_id: &str,
        call: &ToolCall,
        scope_root: Option<&str>,
    ) -> Result<AuthzEvidence, LogError> {
        Ok(AuthzEvidence {
            basis: Basis::Grant,
            binding: CallBinding::for_call(turn_id, call)?,
            risk_class: grant.risk_class,
            approval_ref: grant.approval_ref.clone(),
            scope_root: scope_root.map(str::to_string),
            identity: resolved.identity().clone(),
        })
    }
}
