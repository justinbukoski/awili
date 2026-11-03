//! Unit 4 item 6 — the executor seam: the closed executor vocabulary, the
//! host-controlled accepted-roots record, and the scripted bring-up path
//! (design r2 §"Settled by arbitration"/§"Roots acceptance", carried
//! through r3–r7; the binding order is verdict r7 item 6).
//!
//! `awili-seam` cannot name the item-5 coordinator facade's type — the
//! coordinator lives in the worker composition, which depends on THIS
//! crate — so the seam defines [`McpExecutor`], the facade's dispatch
//! surface, and the composition implements it for its facade. The trait
//! is the arbitration's "the enum does NOT own the raw runtime handle"
//! made structural: it exposes submit-one-call, poll-and-cancel that one
//! call ([`McpCall`]), and publish-a-validated-epoch. Nothing else — no
//! runtime handle, no event receiver, not even the coordinator's lifecycle
//! view
//! (which is how the ADVERTISED roots epoch stays unreachable from
//! dispatch: the seam literally cannot read it).
//!
//! In-flight cancellation IS seam scope (c6 review finding 2): the seam
//! polls its live call in slices so a host stop reaches a call already in
//! the runtime, submits `Cancel { intent_id }`, and drains the
//! coordinator's linearized winner.
//!
//! Unit 4 ships no live launch/reconnect wiring: the worker and the
//! acceptance runner compose [`Executors::builtin_only`], and bring-up
//! ([`bring_up_roots`]) is driven by tests against scripted channels.
//! Instantiating the real runtime, launching catalog servers, and the
//! live bring-up (real policy source, unit-5 executable identity) are
//! unit 4b.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use awili_engine::events::ToolResultRecord;
use awili_engine::generation::ConnectionGeneration;
use awili_engine::{ToolCall, ToolOrigin};
use awili_engine_log::IntentTicket;
use awili_tools::runtime::{CallDisposition, RefusalKind};

use crate::SeamError;

// ============================================================================
// The executor surface
// ============================================================================

/// The seam-facing surface of the item-5 MCP coordinator facade (unit 4
/// item 6). Implemented by the composition for its facade — in this tree,
/// `awili-engine-worker` implements it for `McpFacade<IntentTicket>`.
///
/// Object-safe and `Send + Sync`: the seam stores it boxed behind the
/// [`crate::Executor`] routing, and the worker composition carries it
/// across its spawn boundary. Deliberately minimal (see the module docs):
/// this is the facade, command-side only.
pub trait McpExecutor: Send + Sync {
    /// Submit one already-authorized, already-intent-logged call. The
    /// coordinator (item 5) allocates the `SubmissionId`, registers the
    /// waiter before `submit_until`, and performs the bounded lossless
    /// retry itself — so this returns exactly one enqueue outcome.
    ///
    /// `Ok` hands back the LIVE call ([`McpCall`]), which the seam polls
    /// and may cancel. `Err` is the final submit refusal, and it carries
    /// the ticket back **by move**: nothing was enqueued, so the only
    /// lossless owner left is the caller (design r2 §"Lossless submission",
    /// r3 Delta 1). An implementation that drops it is destroying a
    /// non-`Clone` ticket, not conserving one.
    fn dispatch_call(
        &self,
        dispatch: McpDispatch,
    ) -> Result<Box<dyn McpCall + '_>, McpSubmitRefused>;

    /// Publish a validated acceptance record's epoch for an origin. The
    /// coordinator is the sole submitter, so the bring-up path
    /// ([`bring_up_roots`]) also crosses here; `UpdateAcceptedRoots`
    /// carries no ticket, so refusal is ticket-free by construction.
    fn publish_roots(
        &self,
        origin: ToolOrigin,
        generation: ConnectionGeneration,
        epoch: u64,
    ) -> Result<(), RefusalKind>;
}

/// One live MCP call: the seam's half of in-flight cancellation (design r2
/// §"Cancellation", carried through r3 Delta 3).
///
/// The seam never blocks unboundedly on a call — it polls in slices so it
/// can observe `TurnHost::cancelled()` while the call is in flight, submit
/// `Cancel { intent_id }` under the same absolute deadline as the original
/// submit, and then keep polling for the coordinator's LINEARIZED winner
/// (design r3 Delta 3): a real terminal wins only if the pump commits it
/// before the cancel-submission failure commits; a later terminal is
/// quarantined and cannot revise the returned failure. The seam never
/// synthesizes a terminal.
pub trait McpCall {
    /// Poll for this call's single outcome, bounded by `deadline`. `None`
    /// means the deadline passed with no outcome yet — the call is still
    /// live and must be polled again. Exactly one `Some` is ever produced.
    fn poll_outcome(&mut self, deadline: Instant) -> Option<McpDispatchOutcome>;

    /// Submit `Cancel { intent_id }` for this call, bounded by `deadline`.
    /// `Cancel` carries no ticket, so refusal is ticket-free by
    /// construction (r3 Delta 3's command-recovery asymmetry). On refusal
    /// the coordinator wakes this waiter with
    /// [`McpDispatchOutcome::CancelSubmitRefused`], so the caller keeps
    /// polling for the winner rather than assuming the call is over.
    fn cancel(&mut self, deadline: Instant) -> Result<(), RefusalKind>;
}

/// A FINAL submit refusal (deadline exhausted, or the runtime closed),
/// carrying the B2 ticket back by move. The command never entered the
/// runtime, so no rejection or terminal can arrive for it and this is the
/// only remaining owner of the ticket.
#[derive(Debug)]
pub struct McpSubmitRefused {
    pub kind: RefusalKind,
    pub ticket: IntentTicket,
}

/// One MCP dispatch as the seam frames it: everything the coordinator
/// needs to submit one call, plus the seam's one absolute submit deadline
/// (the bounded lossless-retry bound, item 1's `submit_until`).
pub struct McpDispatch {
    pub deadline: Instant,
    /// The durable B2 intent's ticket — moved in, and moved back inside
    /// every outcome that returns one. Never cloned (it cannot be).
    pub ticket: IntentTicket,
    /// The B2 intent id the call is keyed by (cancellation and duplicate
    /// admission key on it; the ticket itself is opaque cargo).
    pub intent_id: String,
    pub origin: ToolOrigin,
    pub call: ToolCall,
    /// The ACCEPTED generation from the host-controlled record (unit 4b,
    /// impl review r1 P1-1): dispatch is bound to the exact (generation,
    /// epoch) pair — a binding retained across an identical reconnect
    /// must refuse, not silently survive into the fresh child.
    pub generation: ConnectionGeneration,
    /// The ACCEPTED root-capability epoch from the host-controlled record
    /// (see [`RootsAcceptance`]) — never the server's advertised proposal.
    pub root_epoch: u64,
}

/// The single outcome of one [`McpDispatch`] — the coordinator's waiter
/// outcome vocabulary (item 5's `SubmissionOutcome`) and final submit
/// refusal mapped one-to-one into seam terms, with the B2 ticket type
/// pinned to production.
#[derive(Debug)]
pub enum McpDispatchOutcome {
    /// The intent's single terminal; the runtime returned the ticket.
    /// The seam's disposition rule: `Definite` → `dispatch_finalized`
    /// then the reducer; `Uncertain` → NO finalize, the error result
    /// still reaches the reducer (the turn continues).
    Terminal {
        ticket: IntentTicket,
        disposition: CallDisposition,
        result: ToolResultRecord,
    },
    /// THIS submission was rejected as a duplicate (the runtime's
    /// `pending ∪ recently_terminal` admission). Not a terminal: the
    /// original intent's single `CallFinished` is untouched, and the
    /// duplicate's own ticket returns here — conserved, typed, and carried
    /// onward into the seam's typed failure.
    Rejected {
        ticket: IntentTicket,
        reason: String,
    },
    /// A `Cancel` submission finally refused after this intent's waiter
    /// was committed (item 5's linearization rule). The seam DOES submit
    /// cancels — in-flight cancellation is seam scope, not 4b — so this is
    /// a live path: the coordinator wakes the waiter with it, and the seam
    /// still refuses to synthesize a terminal.
    CancelSubmitRefused { kind: RefusalKind },
    /// The runtime event stream closed while this call was live. Started,
    /// unfinalized intents are left for replay to classify; no synthetic
    /// terminals, ever.
    StreamClosed,
}

/// The closed executor vocabulary (unit 4 item 6, settled by arbitration):
/// exactly three classes, matched exhaustively at the seam's dispatch site —
/// a third executor class is a compile error. The `Mcp` variant holds the
/// coordinator FACADE (through [`McpExecutor`]), never the raw runtime
/// handle and never the event receiver.
pub enum Executor<'a> {
    /// Local, synchronous filesystem execution — `run_builtin_fs`, byte
    /// for byte unchanged — for the registry's reserved `builtin:*`
    /// origin space.
    BuiltinFs,
    /// Local, authenticated, read-only metadata lookup over the turn catalog.
    Catalog,
    /// MCP execution through the item-5 coordinator facade, for every
    /// other origin.
    Mcp(&'a dyn McpExecutor),
}

/// Hand-written so [`McpExecutor`] does not have to require `Debug`. The
/// facade is deliberately opaque here: the routing decision is the whole
/// content of this value, and printing anything about the coordinator
/// would widen a surface the module docs keep narrow on purpose.
impl std::fmt::Debug for Executor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BuiltinFs => f.write_str("BuiltinFs"),
            Self::Catalog => f.write_str("Catalog"),
            Self::Mcp(_) => f.write_str("Mcp(<facade>)"),
        }
    }
}

/// The default MCP submit deadline: how long the coordinator's lossless
/// retry fights channel backpressure before the typed refusal. Long
/// enough to ride out a wedged consumer; the refusal, not the wait, is
/// the decision point.
pub const DEFAULT_MCP_SUBMIT_DEADLINE: Duration = Duration::from_secs(30);

/// How long the seam waits for a live call's outcome before looking up to
/// check whether the host asked to stop. Short enough that a cancellation is
/// acted on promptly, long enough that a normal call is not a poll loop —
/// [`McpCall::poll_outcome`] sleeps out the slice, it does not spin.
pub const MCP_POLL_SLICE: Duration = Duration::from_millis(25);

/// The composition's executor routing (unit 4 item 6): which
/// closed-vocabulary executor owns which resolved origin. The reserved
/// `builtin:catalog` routes to [`Executor::Catalog`], the remaining
/// `builtin:*` origin space routes to [`Executor::BuiltinFs`]; every
/// other origin routes to [`Executor::Mcp`]. Held by the composition,
/// borrowed per turn.
pub struct Executors {
    mcp: Option<Box<dyn McpExecutor>>,
    submit_deadline: Duration,
}

impl Executors {
    /// A builtin-only composition. Every non-builtin origin is then a
    /// typed composition error — unit 4 ships no live MCP wiring, so the
    /// worker and the acceptance runner compose exactly this until 4b.
    pub fn builtin_only() -> Self {
        Self {
            mcp: None,
            submit_deadline: DEFAULT_MCP_SUBMIT_DEADLINE,
        }
    }

    /// A composition with an MCP executor (the item-5 coordinator facade).
    pub fn mcp(facade: impl McpExecutor + 'static) -> Self {
        Self {
            mcp: Some(Box::new(facade)),
            submit_deadline: DEFAULT_MCP_SUBMIT_DEADLINE,
        }
    }

    /// Override the MCP submit deadline (tests ride a short refusal bound
    /// instead of [`DEFAULT_MCP_SUBMIT_DEADLINE`]).
    pub fn with_submit_deadline(mut self, deadline: Duration) -> Self {
        self.submit_deadline = deadline;
        self
    }

    /// The MCP submit deadline this composition dispatches under.
    pub fn submit_deadline(&self) -> Duration {
        self.submit_deadline
    }

    /// Route ONE resolved origin to its executor — the value the
    /// dispatch-site match is exhaustive over. An origin outside the
    /// reserved builtin space with no MCP executor configured is a
    /// composition misconfiguration: a typed harness error, never a
    /// silent builtin fallback (the mirror of item 3's no-fallback rule
    /// on the routing side).
    pub fn route(&self, origin: &ToolOrigin) -> Result<Executor<'_>, SeamError> {
        if origin.origin == "builtin:catalog" {
            Ok(Executor::Catalog)
        } else if origin.origin.starts_with("builtin:") {
            Ok(Executor::BuiltinFs)
        } else if let Some(facade) = self.mcp.as_deref() {
            Ok(Executor::Mcp(facade))
        } else {
            Err(SeamError::Harness(format!(
                "no executor for origin {:?}: non-builtin tools are registered but this \
                 composition provides no MCP executor",
                origin.origin
            )))
        }
    }
}

// ============================================================================
// Host-controlled roots acceptance
// ============================================================================

/// HOST-side root material for one origin (unit 4b, design r9 §4.1). This
/// is what the HOST proposes to bind — nothing the server claims lives
/// here anymore: the claimed name, advertised epoch, and tools all come
/// from the sink-issued [`ConnectedLaunch`] observation, which cannot be
/// forged or re-paired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRootProposal {
    pub origin: ToolOrigin,
    pub proposed_roots: Vec<String>,
}

/// Why a proposal was refused. Every shape leaves the origin UNAVAILABLE:
/// no record exists, nothing is published, and the runtime's
/// refuse-until-published rule refuses the origin's calls regardless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootsRejection {
    /// No epoch was advertised (absent or malformed — never coerced to 0).
    Malformed { detail: String },
    /// The record's expected protocol name does not match what the server
    /// claimed at handshake (a PROTOCOL-NAME comparison; design r9 §4.1).
    IdentityMismatch { expected: String, claimed: String },
    /// Host root policy refused the proposed root set.
    Policy { detail: String },
    /// The host proposal names a different origin than the observation
    /// (four-way closure leg 2 vs leg 1; design r9 §4.1).
    OriginMismatch { detail: String },
    /// Generation disagreement between the inputs.
    GenerationMismatch { detail: String },
    /// The launch is unverified and the policy did not admit it
    /// (default-deny; design r9 §4.2).
    UnverifiedLaunch {
        reason: awili_tools::attest::UnverifiedReason,
    },
}

impl std::fmt::Display for RootsRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed { detail } => write!(f, "malformed roots proposal: {detail}"),
            Self::IdentityMismatch { expected, claimed } => write!(
                f,
                "protocol name mismatch: record expects {expected:?}, server claimed {claimed:?}"
            ),
            Self::Policy { detail } => write!(f, "host root policy refused: {detail}"),
            Self::OriginMismatch { detail } => write!(f, "origin mismatch: {detail}"),
            Self::GenerationMismatch { detail } => write!(f, "generation mismatch: {detail}"),
            Self::UnverifiedLaunch { reason } => {
                write!(f, "unverified launch not admitted by policy: {reason:?}")
            }
        }
    }
}

impl std::error::Error for RootsRejection {}

/// Host root policy at the acceptance boundary (unit 4b, design r9 §4.2):
/// adjudicated ONCE, class-routed by the observation's bind. The live
/// policy source (config, user policy) is composed by the app; tests
/// script it.
pub trait RootsPolicy {
    /// May the host's root set be accepted for this VERIFIED launch?
    /// `Err(detail)` refuses — the origin stays unavailable.
    fn accepts(
        &self,
        verified: &awili_tools::attest::VerifiedLaunch,
        connected: &awili_tools::runtime::ConnectedLaunch,
        host: &HostRootProposal,
    ) -> Result<(), String>;

    /// May an UNVERIFIED launch be admitted at all? Default FALSE — an
    /// admitted unverified launch produces a full-strength
    /// [`ValidatedRoots`] (it projects, reaches Ready, dispatches) but can
    /// never satisfy the Install-grant boundary. Two properties, kept
    /// independent (design r9 §4.2).
    fn admit_unverified(
        &self,
        _unverified: &awili_tools::attest::UnverifiedLaunch,
        _connected: &awili_tools::runtime::ConnectedLaunch,
    ) -> bool {
        false
    }
}

/// A host-controlled accepted-roots record (design r2 §"Roots
/// acceptance"): the binding of an origin to the root capability the HOST
/// accepted, at one epoch, for one VERIFIED server identity. Not
/// constructible outside this module — the only producer is
/// [`validate_roots_proposal`] (via [`ValidatedRoots`]) — so a record
/// cannot exist for a proposal the host never validated. LIVE record
/// construction (real policy source, unit-5 executable identity as the
/// verified-server input) lands in 4b/5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedRoots {
    origin: ToolOrigin,
    /// The connection generation this record binds (unit 4b: acceptance is
    /// per-(origin, generation); a dead generation's record is invalidated
    /// atomically with catalog retirement).
    generation: ConnectionGeneration,
    /// The protocol name the server claimed at handshake — a FACT, checked
    /// against the record's expected name during validation.
    server: String,
    /// The accepted epoch — the ONLY epoch the seam ever dispatches
    /// under, and the only value `UpdateAcceptedRoots` publication
    /// derives from.
    epoch: u64,
    /// The accepted root set/capability (opaque to the seam: C1/C3 policy
    /// material, carried for the record's own completeness).
    roots: Vec<String>,
    /// FACTS, never a token (design r9 §4.1): the observation's class and,
    /// for the verified class, its setup identity.
    attestation: awili_tools::attest::AttestationClass,
    setup_id: Option<awili_tools::attest::SetupIdentityId>,
}

impl AcceptedRoots {
    pub fn origin(&self) -> &ToolOrigin {
        &self.origin
    }
    pub fn generation(&self) -> ConnectionGeneration {
        self.generation
    }
    pub fn server(&self) -> &str {
        &self.server
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn roots(&self) -> &[String] {
        &self.roots
    }
    pub fn attestation(&self) -> awili_tools::attest::AttestationClass {
        self.attestation
    }
    pub fn setup_id(&self) -> Option<&awili_tools::attest::SetupIdentityId> {
        self.setup_id.as_ref()
    }
}

/// A proposal that PASSED validation. No public constructor and a private
/// payload: the only way to obtain one is [`validate_roots_proposal`], and
/// the only consumer is [`RootsAcceptance::accept`] — acceptance cannot
/// outrun validation. `Debug` is derived for diagnostics only; it reveals
/// the record but confers no way to build one.
#[derive(Debug)]
pub struct ValidatedRoots(AcceptedRoots);

/// The validation seam (unit 4b, design r9 §4.1): the FOUR-WAY origin
/// closure over the sink-issued observation. All four must be identical:
/// (1) the observation's origin (lifecycle-sourced, unforgeable), (2) the
/// host proposal's origin, (3) the bound token's origin — identical to (1)
/// BY CONSTRUCTION after token-sourced identity, the check degenerating
/// safely rather than being removed — and (4) the privately constructed
/// record's origin, a postcondition of this sole producer. The setup
/// record's origin is NOT a fifth input: it was folded into the token at
/// mint (ruling A lineage).
///
/// Class is adjudicated ONCE, here: a verified launch checks the record's
/// expected protocol name against what the server claimed and consults
/// `RootsPolicy::accepts`; an unverified launch is admitted only by
/// `RootsPolicy::admit_unverified` (default false). Reconnect proposals
/// get no shortcut — the epoch NUMBER is never consulted here, so
/// monotonic comparison alone can never widen authority.
pub fn validate_roots(
    connected: &awili_tools::runtime::ConnectedLaunch,
    host: &HostRootProposal,
    policy: &dyn RootsPolicy,
) -> Result<ValidatedRoots, RootsRejection> {
    // Leg 2 vs leg 1.
    if host.origin != *connected.origin() {
        return Err(RootsRejection::OriginMismatch {
            detail: format!(
                "host proposes {:?} but the observation is {:?}",
                host.origin.origin,
                connected.origin().origin
            ),
        });
    }
    // The advertised epoch comes from the OBSERVATION — absent or
    // malformed is no proposal, never 0.
    let Some(epoch) = connected.advertised_epoch() else {
        return Err(RootsRejection::Malformed {
            detail: "no roots epoch advertised (absent or malformed); never coerced to 0".into(),
        });
    };
    // Leg 3: the one authority-bearing read. Identical to leg 1 by
    // construction (token-sourced identity); the bind is still the value
    // every class decision routes through.
    let (attestation, setup_id) = match connected.bind() {
        awili_tools::attest::BoundLaunch::Verified(verified) => {
            if verified.expected_protocol_name() != connected.claimed_server() {
                return Err(RootsRejection::IdentityMismatch {
                    expected: verified.expected_protocol_name().to_string(),
                    claimed: connected.claimed_server().to_string(),
                });
            }
            if let Err(detail) = policy.accepts(verified, connected, host) {
                return Err(RootsRejection::Policy { detail });
            }
            (
                awili_tools::attest::AttestationClass::Verified,
                Some(verified.setup_id().clone()),
            )
        }
        awili_tools::attest::BoundLaunch::Unverified(unverified) => {
            if !policy.admit_unverified(unverified, connected) {
                return Err(RootsRejection::UnverifiedLaunch {
                    reason: unverified.reason(),
                });
            }
            (awili_tools::attest::AttestationClass::Unverified, None)
        }
    };
    // Leg 4: the record's origin is copied from the observation — a
    // postcondition of this sole producer.
    Ok(ValidatedRoots(AcceptedRoots {
        origin: connected.origin().clone(),
        generation: connected.generation(),
        server: connected.claimed_server().to_string(),
        epoch,
        roots: host.proposed_roots.clone(),
        attestation,
        setup_id,
    }))
}

impl ValidatedRoots {
    /// Read the validated record's facts (the capability itself stays
    /// move-only into [`RootsAcceptance::accept`]).
    pub fn record(&self) -> &AcceptedRoots {
        &self.0
    }
}

/// The host-controlled acceptance table the seam's MCP leg reads the
/// accepted epoch from — never the advertised proposal, which this type
/// cannot even represent.
#[derive(Debug, Default, Clone)]
pub struct RootsAcceptance {
    records: BTreeMap<String, AcceptedRoots>,
}

impl RootsAcceptance {
    /// Accept a VALIDATED record — the only mutation path. Within ONE
    /// generation acceptance is monotonic (a revalidated OLDER epoch never
    /// replaces a newer one); a record for a NEW generation replaces the
    /// old generation's outright, because the old generation's authority
    /// died with it; a record for an OLDER generation is ignored (RC2-ENG5,
    /// F0802). Monotonicity never WIDENS authority — every record
    /// already passed full validation.
    pub fn accept(&mut self, validated: ValidatedRoots) {
        let record = validated.0;
        let key = record.origin.origin.clone();
        match self.records.get(&key) {
            // RC2-ENG5 (F0802): an OLDER generation never replaces a newer
            // one — that generation's authority is already gone.
            Some(current) if record.generation < current.generation => {}
            Some(current)
                if current.generation == record.generation && record.epoch < current.epoch => {}
            _ => {
                self.records.insert(key, record);
            }
        }
    }

    /// The accepted (generation, epoch) for an origin — the ONLY authority
    /// the seam dispatches under. `None` = the origin is unavailable.
    pub fn accepted(&self, origin: &ToolOrigin) -> Option<(ConnectionGeneration, u64)> {
        self.records
            .get(&origin.origin)
            .map(|r| (r.generation, r.epoch))
    }

    /// The accepted record for an origin (bring-up/revalidation input).
    pub fn record(&self, origin: &ToolOrigin) -> Option<&AcceptedRoots> {
        self.records.get(&origin.origin)
    }

    /// Invalidate the record for `(origin, generation)` — a no-op when the
    /// held record is for a DIFFERENT generation (a newer acceptance must
    /// not be torn down by a stale retirement). Unit 4b's reconciliation
    /// calls this atomically with catalog retirement, under one lock.
    pub fn invalidate(&mut self, origin: &ToolOrigin, generation: ConnectionGeneration) {
        if let Some(current) = self.records.get(&origin.origin) {
            if current.generation == generation {
                self.records.remove(&origin.origin);
            }
        }
    }
}

/// Why a bring-up failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BringUpError {
    /// The proposal was refused — the origin stays unavailable (no record
    /// accepted, nothing published).
    Rejected(RootsRejection),
    /// Publishing the validated epoch through the coordinator was finally
    /// refused. Nothing was accepted, so the origin keeps its previous
    /// authority — or none.
    PublishRefused(RefusalKind),
}

impl std::fmt::Display for BringUpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(rejection) => write!(f, "roots bring-up rejected: {rejection}"),
            Self::PublishRefused(kind) => {
                write!(f, "publishing accepted roots was refused: {kind:?}")
            }
        }
    }
}

impl std::error::Error for BringUpError {}

/// The bring-up path (unit 4: SCRIPTED — tests drive it against scripted
/// channels; the live launcher is 4b): validate the proposal against the
/// VERIFIED server identity and host root policy, publish the validated
/// epoch through the executor, then accept the record.
///
/// Publication derives from the validated record only, and PUBLISHES
/// BEFORE ACCEPTING: on success the runtime and the record agree; on
/// refusal nothing was accepted and the origin's previous authority (or
/// none) stands. A stale publish is not an error return — the runtime
/// refuses it loudly as `RootsUpdateIgnored` (traced by the coordinator's
/// pump, item 5) and [`RootsAcceptance::accept`]'s monotonic guard keeps
/// the record at the newer epoch.
pub fn bring_up_roots(
    executor: &dyn McpExecutor,
    roots: &mut RootsAcceptance,
    connected: &awili_tools::runtime::ConnectedLaunch,
    host: &HostRootProposal,
    policy: &dyn RootsPolicy,
) -> Result<(), BringUpError> {
    let validated = validate_roots(connected, host, policy).map_err(BringUpError::Rejected)?;
    executor
        .publish_roots(
            validated.0.origin.clone(),
            validated.0.generation,
            validated.0.epoch,
        )
        .map_err(BringUpError::PublishRefused)?;
    roots.accept(validated);
    Ok(())
}

// ============================================================================
// The typed MCP dispatch failure
// ============================================================================

/// How an MCP dispatch ended WITHOUT a terminal (unit 4 item 6). In every
/// shape the intent was durably logged and `dispatch_started`, and is
/// deliberately LEFT UNFINALIZED — the seam never synthesizes a terminal,
/// and B2 replay classifies `dispatch_started`-without-`dispatch_finalized`
/// as Uncertain, which is the honest truth for a call whose outcome this
/// process cannot know through the failed path.
///
/// Two of the four shapes OWN the B2 ticket that came back from the
/// coordinator. The seam is then the ticket's last lossless owner: it holds
/// it inside the typed failure so a caller can see (and, in tests, prove)
/// that the exact ticket which went in came back out. Dropping it here
/// instead would be destruction relabelled as conservation.
#[derive(Debug)]
pub enum McpDispatchFailure {
    /// The submit was finally refused (backpressure to the deadline, or
    /// the runtime closed). Nothing was enqueued, so the ticket comes all
    /// the way back and lives here.
    SubmitRefused {
        kind: RefusalKind,
        ticket: IntentTicket,
    },
    /// The submission was rejected as a duplicate. The seam mints a fresh
    /// B2 intent id per call and the coordinator is the sole submitter,
    /// so this is an invariant violation in normal unit-4 flow — surfaced
    /// loudly, never papered over. The duplicate's own ticket returned
    /// with the rejection and lives here.
    SubmissionRejected {
        reason: String,
        ticket: IntentTicket,
    },
    /// A cancel submission failed after this intent's waiter was
    /// committed (item 5's linearization rule). Ticket-FREE by contract:
    /// `Cancel` never carried one, and the call's own ticket is still with
    /// the runtime, so there is nothing here to conserve (r3 Delta 3's
    /// command-recovery asymmetry).
    CancelSubmitRefused { kind: RefusalKind },
    /// The runtime event stream closed with the call live. Ticket-free for
    /// the same reason: the command WAS enqueued, so the ticket is with the
    /// runtime and this process cannot recover it — the runtime contract's
    /// one admitted loss path, not a new one.
    StreamClosed,
}

impl std::fmt::Display for McpDispatchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SubmitRefused { kind, ticket } => write!(
                f,
                "call submission finally refused ({kind:?}); the complete command, ticket \
                 {} included, came back intact",
                ticket.intent_id()
            ),
            Self::SubmissionRejected { reason, .. } => {
                write!(f, "submission rejected as a duplicate: {reason}")
            }
            Self::CancelSubmitRefused { kind } => {
                write!(f, "cancel submission finally refused ({kind:?})")
            }
            Self::StreamClosed => {
                write!(f, "the runtime event stream closed before a terminal")
            }
        }
    }
}

impl std::error::Error for McpDispatchFailure {}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_tools::attest::{
        LaunchKind, LaunchToken, SetupIdentityId, SetupRecordFacts, UnverifiedReason,
    };
    use awili_tools::runtime::{ConnectedLaunch, McpEvent, McpRuntimeHandle};

    struct Allow;
    impl RootsPolicy for Allow {
        fn accepts(
            &self,
            _v: &awili_tools::attest::VerifiedLaunch,
            _c: &ConnectedLaunch,
            _h: &HostRootProposal,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    #[allow(dead_code)] // kept as the canonical refusing policy for future tests
    struct Deny;
    impl RootsPolicy for Deny {
        fn accepts(
            &self,
            _v: &awili_tools::attest::VerifiedLaunch,
            _c: &ConnectedLaunch,
            _h: &HostRootProposal,
        ) -> Result<(), String> {
            Err("denied by scripted policy".into())
        }
    }

    /// Admits the unverified class (and allows verified) — the OPT-IN
    /// policy rows 6/24b contrast against the default-deny.
    struct AdmitUnverified;
    impl RootsPolicy for AdmitUnverified {
        fn accepts(
            &self,
            _v: &awili_tools::attest::VerifiedLaunch,
            _c: &ConnectedLaunch,
            _h: &HostRootProposal,
        ) -> Result<(), String> {
            Ok(())
        }
        fn admit_unverified(
            &self,
            _u: &awili_tools::attest::UnverifiedLaunch,
            _c: &ConnectedLaunch,
        ) -> bool {
            true
        }
    }

    fn record_facts(origin: &str, protocol: &str) -> SetupRecordFacts {
        SetupRecordFacts {
            origin: ToolOrigin::new(origin),
            setup_id: SetupIdentityId::parse("setup-1").unwrap(),
            pinned_requirement: "cdhash H\"00\"".into(),
            expected_protocol_name: protocol.into(),
            launch_kind: LaunchKind::Direct,
            program: "/usr/local/lib/awili/srv".into(),
        }
    }

    /// A LEGITIMATELY emitted observation (ruling F: tests never construct
    /// one — they obtain it from the sole issuer, the sink).
    fn observation(token: LaunchToken, claimed: &str, epoch: Option<u64>) -> ConnectedLaunch {
        let (sink, stream) = awili_tools::runtime::event_channel::<String>(4);
        let (ctx, _crx) = async_channel::bounded(1);
        let handle = McpRuntimeHandle::from_parts(ctx, stream, std::thread::spawn(|| {}));
        sink.emit_server_connected_blocking(token, claimed.into(), Vec::new(), Vec::new(), epoch)
            .expect("emit");
        // Close the stream BEFORE the handle drops: the handle's drop
        // drains events with a blocking recv, which only ends when the
        // sink is gone — and the sink, declared first, would otherwise
        // outlive the handle.
        drop(sink);
        match handle.try_recv_event() {
            Some(McpEvent::ServerConnected(c)) => c,
            other => panic!("expected the observation, got {other:?}"),
        }
    }

    fn verified_observation(origin: &str, protocol: &str, epoch: Option<u64>) -> ConnectedLaunch {
        let mut alloc = awili_engine::generation::GenerationAllocator::new();
        let token = LaunchToken::mint_verified(&record_facts(origin, protocol), alloc.next());
        observation(token, protocol, epoch)
    }

    fn host(origin: &str) -> HostRootProposal {
        HostRootProposal {
            origin: ToolOrigin::new(origin),
            proposed_roots: vec!["/tmp".into()],
        }
    }

    /// A None advertised epoch is malformed — never coerced to 0, never a
    /// policy question: there is no proposal to evaluate.
    #[test]
    fn absent_epoch_is_malformed_regardless_of_policy() {
        let c = verified_observation("mcp:t", "srv", None);
        let err = validate_roots(&c, &host("mcp:t"), &Allow).unwrap_err();
        assert!(matches!(err, RootsRejection::Malformed { .. }), "{err:?}");
    }

    /// Gate row 4: a server claiming a name the record does not expect
    /// leaves the origin unavailable — typed, no record behind.
    #[test]
    fn claimed_name_mismatch_leaves_origin_unavailable() {
        // The record expects "srv"; the server CLAIMED "impostor" at
        // handshake (the observation carries the claim verbatim).
        let mut alloc = awili_engine::generation::GenerationAllocator::new();
        let token = LaunchToken::mint_verified(&record_facts("mcp:t", "srv"), alloc.next());
        let c = observation(token, "impostor", Some(1));
        let err = validate_roots(&c, &host("mcp:t"), &Allow).unwrap_err();
        match err {
            RootsRejection::IdentityMismatch { expected, claimed } => {
                assert_eq!(expected, "srv");
                assert_eq!(claimed, "impostor");
            }
            other => panic!("identity mismatch must be typed, got {other:?}"),
        }
        let mut roots = RootsAcceptance::default();
        let _ = &mut roots;
        assert_eq!(roots.accepted(&ToolOrigin::new("mcp:t")), None);
    }

    /// Gate row 6: an unattested launch is DENIED by the default policy —
    /// admit_unverified defaults to false, and admission is an explicit
    /// opt-in that still yields full-strength validation.
    #[test]
    fn unattested_launch_is_denied_by_default_roots_policy() {
        let mut alloc = awili_engine::generation::GenerationAllocator::new();
        let token = LaunchToken::mint_unverified(
            ToolOrigin::new("mcp:t"),
            alloc.next(),
            UnverifiedReason::NoSetupRecord,
        );
        let c = observation(token, "srv", Some(1));
        let err = validate_roots(&c, &host("mcp:t"), &Allow).unwrap_err();
        assert!(
            matches!(
                err,
                RootsRejection::UnverifiedLaunch {
                    reason: UnverifiedReason::NoSetupRecord
                }
            ),
            "{err:?}"
        );

        // The explicit opt-in admits, at full strength, with the class
        // recorded as a FACT.
        let mut alloc = awili_engine::generation::GenerationAllocator::new();
        let token = LaunchToken::mint_unverified(
            ToolOrigin::new("mcp:t"),
            alloc.next(),
            UnverifiedReason::NoSetupRecord,
        );
        let c = observation(token, "srv", Some(1));
        let validated = validate_roots(&c, &host("mcp:t"), &AdmitUnverified).unwrap();
        assert_eq!(
            validated.record().attestation(),
            awili_tools::attest::AttestationClass::Unverified
        );
        assert!(validated.record().setup_id().is_none());
    }

    /// Gate row 24b (ruling F form): no fixture constructs a mis-paired
    /// observation — its fields and sole constructor are sealed. The test
    /// obtains a legitimate token-sourced observation and asserts exactly:
    /// (1) a foreign host origin is refused; (2) a successful validation's
    /// record origin equals the observation's; (3) the bound token's
    /// origin equals the observation's.
    #[test]
    fn validate_roots_refuses_when_the_four_origins_are_not_identical() {
        let c = verified_observation("mcp:t", "srv", Some(1));
        // (1) the independently variable leg: a foreign host origin.
        let err = validate_roots(&c, &host("mcp:other"), &Allow).unwrap_err();
        assert!(
            matches!(err, RootsRejection::OriginMismatch { .. }),
            "{err:?}"
        );
        // (2) success: the record's origin is the observation's.
        let validated = validate_roots(&c, &host("mcp:t"), &Allow).unwrap();
        assert_eq!(validated.record().origin(), c.origin());
        // (3) the bound token's origin is the observation's, structurally.
        match c.bind() {
            awili_tools::attest::BoundLaunch::Verified(v) => assert_eq!(v.origin(), c.origin()),
            awili_tools::attest::BoundLaunch::Unverified(_) => panic!("verified fixture"),
        }
    }

    /// The monotonic accept guard WITHIN one generation, and generation
    /// replacement across generations.
    #[test]
    fn acceptance_is_monotonic_per_origin() {
        let mut roots = RootsAcceptance::default();
        let make = |epoch: u64| {
            let c = verified_observation("mcp:t", "srv", Some(epoch));
            (
                c.generation(),
                validate_roots(&c, &host("mcp:t"), &Allow).unwrap(),
            )
        };
        // Same-generation monotonicity requires the SAME generation: use
        // one observation's generation by re-validating fresh epochs from
        // fresh observations (each fixture allocator restarts at g1, so
        // the generations coincide — deliberate here).
        let (g2, v2) = make(2);
        roots.accept(v2);
        let (g1, v1) = make(1);
        assert_eq!(g1, g2, "fixture generations coincide by construction");
        roots.accept(v1); // stale revalidation within the generation: no-op
        assert_eq!(
            roots.accepted(&ToolOrigin::new("mcp:t")),
            Some((g2, 2)),
            "older epoch never replaces newer within one generation"
        );
        let (_g5, v5) = make(5);
        roots.accept(v5); // newer: replaces
        assert_eq!(
            roots.accepted(&ToolOrigin::new("mcp:t")).map(|a| a.1),
            Some(5)
        );
        assert_eq!(
            roots.record(&ToolOrigin::new("mcp:t")).unwrap().server(),
            "srv"
        );
        // A different origin is independent — and unavailable by default.
        assert_eq!(roots.accepted(&ToolOrigin::new("mcp:other")), None);
        // Invalidation is generation-scoped: a stale retirement is a no-op.
        let held = roots
            .record(&ToolOrigin::new("mcp:t"))
            .unwrap()
            .generation();
        roots.invalidate(&ToolOrigin::new("mcp:t"), held);
        assert_eq!(roots.accepted(&ToolOrigin::new("mcp:t")), None);
    }

    /// RC2-ENG5 (F0802): a validated record of an OLDER generation never
    /// replaces the newer generation's accepted record, whatever its epoch.
    #[test]
    fn rc2eng5_an_older_generation_never_replaces_a_newer_acceptance() {
        let mut alloc = awili_engine::generation::GenerationAllocator::new();
        let g1 = alloc.next();
        let g2 = alloc.next();
        let validated = |g, epoch| {
            let token = LaunchToken::mint_verified(&record_facts("mcp:t", "srv"), g);
            let c = observation(token, "srv", Some(epoch));
            validate_roots(&c, &host("mcp:t"), &Allow).unwrap()
        };
        let mut roots = RootsAcceptance::default();
        roots.accept(validated(g2, 1));
        roots.accept(validated(g1, 9)); // late revalidation of the dead g1
        assert_eq!(roots.accepted(&ToolOrigin::new("mcp:t")), Some((g2, 1)));
        // A newer generation still replaces.
        let g3 = alloc.next();
        roots.accept(validated(g3, 1));
        assert_eq!(roots.accepted(&ToolOrigin::new("mcp:t")), Some((g3, 1)));
    }
}
