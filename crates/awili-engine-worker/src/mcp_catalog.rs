//! Unit 4b host projection + unit 5 r9 §1: the committed-catalog STATE and
//! every pure projection over it live in the WORKER crate, so the
//! process-long floor owner can register a concrete `Arc<WorkMcpState>`
//! and derive floors from `live` under its own lock — no publicly
//! implementable source exists (r7 verdict ruling 39). The app keeps the
//! facade-composing drive (`drive_reconcile_once`) and the attestation-
//! composing grant input (`install_grant_input`), consuming owned
//! projection values only.
//!
//! Unit 4b — the host projection: from a validated observation to the
//! per-turn tool catalog (design r9 §3, §5).
//!
//! `project_advertised` is the ONLY producer of [`AcceptedCatalog`]: both
//! generation values are inputs, nothing is invented, and every rejection
//! is typed. Catalog-level failures (generation/origin mismatch, duplicate
//! names over kept ++ capture-dropped, nothing projected from a non-empty
//! advertisement) refuse the WHOLE generation; tool-local failures
//! (unusable name, unparsable or non-object schema) drop ONLY that tool
//! and are recorded, never silent.
//!
//! Projection does NOT re-adjudicate attestation class (design r9 §3):
//! its rejections are protocol and shape failures applying identically to
//! both classes — the class rides along as a fact.

use awili_engine::generation::ConnectionGeneration;
use awili_seam::ValidatedRoots;
use awili_tools::attest::{AttestationClass, SetupIdentityId};
use awili_tools::runtime::ConnectedLaunch;
use awili_tools::{CanonicalDescriptor, ToolDef, ToolOrigin};

/// The projected, host-accepted catalog for ONE generation of ONE origin.
/// Facts only — no token, no launch value, no borrow of the observation
/// (design r9 §3): grant liveness comes from the boundary's own facade
/// query against the observation, never from here.
#[derive(Debug)]
pub struct AcceptedCatalog {
    origin: ToolOrigin,
    generation: ConnectionGeneration,
    tools: Vec<ToolDef>,
    problems: Vec<(String, ProjectionProblem)>,
    attestation: AttestationClass,
    setup_id: Option<SetupIdentityId>,
}

impl AcceptedCatalog {
    pub fn origin(&self) -> &ToolOrigin {
        &self.origin
    }
    pub fn generation(&self) -> ConnectionGeneration {
        self.generation
    }
    pub fn tools(&self) -> &[ToolDef] {
        &self.tools
    }
    /// Tool-local drops, recorded per name — one malformed tool degrades
    /// one tool and is never silent.
    pub fn problems(&self) -> &[(String, ProjectionProblem)] {
        &self.problems
    }
    pub fn attestation(&self) -> AttestationClass {
        self.attestation
    }
    pub fn setup_id(&self) -> Option<&SetupIdentityId> {
        self.setup_id.as_ref()
    }
}

/// Catalog-level refusals: the whole generation projects nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogRejection {
    /// The validated record and the observation disagree about which
    /// generation this is.
    GenerationMismatch {
        validated: ConnectionGeneration,
        observed: ConnectionGeneration,
    },
    /// The validated record and the observation disagree about which
    /// origin this is.
    OriginMismatch { validated: String, observed: String },
    /// A name appears twice across kept ++ capture-dropped (ruled
    /// necessary and sufficient; capture-dropped NAMES exist exactly so
    /// this check cannot be blinded by the capture bound).
    DuplicateName { name: String },
    /// A non-empty advertisement projected zero tools: the origin is
    /// unavailable rather than silently toolless.
    NothingProjected,
}

/// Tool-local problems: the tool drops, the catalog survives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionProblem {
    SchemaUnparsable {
        detail: String,
    },
    SchemaNotAnObject,
    /// Empty, NUL, or ASCII control characters — the registry imposes no
    /// broader grammar, so 4b invents none (design r9 §3).
    NameUnusable,
}

/// The sole producer of [`AcceptedCatalog`] (design r9 §3).
pub fn project_advertised(
    validated: &ValidatedRoots,
    connected: &ConnectedLaunch,
) -> Result<AcceptedCatalog, CatalogRejection> {
    let record = validated.record();

    // (1) Catalog-level: generation and origin equality between the
    // validated record and the observation.
    if record.generation() != connected.generation() {
        return Err(CatalogRejection::GenerationMismatch {
            validated: record.generation(),
            observed: connected.generation(),
        });
    }
    if record.origin() != connected.origin() {
        return Err(CatalogRejection::OriginMismatch {
            validated: record.origin().origin.clone(),
            observed: connected.origin().origin.clone(),
        });
    }

    // (2) Catalog-level: duplicate names over kept ++ capture-dropped.
    let mut seen = std::collections::HashSet::new();
    for name in connected
        .tools()
        .iter()
        .map(|t| t.name.as_str())
        .chain(connected.dropped_at_capture().iter().map(|s| s.as_str()))
    {
        if !seen.insert(name) {
            return Err(CatalogRejection::DuplicateName {
                name: name.to_string(),
            });
        }
    }

    // (3) Tool-local: name, then schema — dropped and recorded.
    let mut tools = Vec::new();
    let mut problems = Vec::new();
    for t in connected.tools() {
        if t.name.is_empty() || t.name.chars().any(|c| c.is_ascii_control()) {
            problems.push((t.name.clone(), ProjectionProblem::NameUnusable));
            continue;
        }
        let schema: serde_json::Value = match serde_json::from_str(&t.arg_schema_canonical) {
            Ok(v) => v,
            Err(e) => {
                problems.push((
                    t.name.clone(),
                    ProjectionProblem::SchemaUnparsable {
                        detail: e.to_string(),
                    },
                ));
                continue;
            }
        };
        if !schema.is_object() {
            problems.push((t.name.clone(), ProjectionProblem::SchemaNotAnObject));
            continue;
        }
        tools.push(
            ToolDef::from_parts(
                connected.origin().clone(),
                t.name.clone(),
                CanonicalDescriptor {
                    schema,
                    side_effect: t.risk.as_str().to_string(),
                    required_capabilities: vec!["mcp".into()],
                    executor: connected.origin().origin.clone(),
                },
            )
            // RC1-R: the server's own words reach the model.
            .with_description(t.description.clone()),
        );
    }

    // (4) Catalog-level: a non-empty advertisement that projected nothing
    // leaves the origin unavailable.
    if tools.is_empty() && (connected.tools().len() + connected.dropped_at_capture().len()) > 0 {
        return Err(CatalogRejection::NothingProjected);
    }

    Ok(AcceptedCatalog {
        origin: connected.origin().clone(),
        generation: connected.generation(),
        tools,
        problems,
        attestation: connected.attestation_class(),
        setup_id: match connected.bind() {
            awili_tools::attest::BoundLaunch::Verified(v) => Some(v.setup_id().clone()),
            awili_tools::attest::BoundLaunch::Unverified(_) => None,
        },
    })
}

// ============================================================================
// WorkMcpState — per-worker authority (design r9 §5.1–§5.4)
// ============================================================================

use crate::mcp_coordinator::OriginStatus;
use awili_seam::{HostRootProposal, RootsAcceptance, RootsPolicy, RootsRejection};

/// Per-worker MCP authority (design r9 §5.1): the pending/live catalogs,
/// this worker's own accepted roots, and the revision fence — one mutex,
/// nothing under the guard but map/record operations. `reconcile` holds
/// the lock for compare, retire, promote, and publication bookkeeping;
/// `prepare` runs with NO lock held (ruling B); `commit` re-checks under
/// the lock and installs atomically.
#[derive(Debug, Default)]
pub struct WorkMcpState {
    inner: std::sync::Mutex<WorkAuthorityState>,
    /// Set ONCE by the floor owner when a successor's registration retires
    /// this state (unit 5 ruling 40): a retired catalog can never re-enter
    /// registration, and its retained handle mints nothing.
    retired: std::sync::atomic::AtomicBool,
}

/// One-shot registration token (unit 5 rulings 40/46): minted ONLY by
/// [`WorkMcpState::new_registered`] — exactly one per constructed state,
/// bound to that exact `Arc`. Not `Clone`, not reconstructible; consumed
/// by `crate::floor_owner::register_catalog`.
pub struct RegistrationBrand {
    state: std::sync::Arc<WorkMcpState>,
}

impl RegistrationBrand {
    pub(crate) fn into_state(self) -> std::sync::Arc<WorkMcpState> {
        self.state
    }
}

/// NOT `HostAuthority`: `awili_seam::HostAuthority` already exists and is
/// `run_turn`'s first parameter (design r9 §5.1).
#[derive(Debug, Default)]
struct WorkAuthorityState {
    pending: std::collections::HashMap<String, PendingEntry>,
    live: std::collections::HashMap<String, LiveEntry>,
    roots: RootsAcceptance,
    as_of_revision: u64,
    /// Fail-closed observations (impossible cells, stale candidates) —
    /// bounded, drained by the drive for surfacing.
    diagnostics: Vec<String>,
}

#[derive(Debug)]
struct PendingEntry {
    generation: ConnectionGeneration,
    epoch: u64,
    catalog: AcceptedCatalog,
    /// The one addition over the design's r5 lineage that makes a refused
    /// publication a RETRYABLE state instead of a permanent strand
    /// (design r9 §5.3, T8).
    published: bool,
}

#[derive(Debug)]
struct LiveEntry {
    generation: ConnectionGeneration,
    epoch: u64,
    catalog: AcceptedCatalog,
}

/// What the caller must do next (design r9 §5.3). Retirement and
/// promotion are INTERNAL effects — never caller obligations.
#[derive(Debug)]
pub enum ReconcileOutcome {
    /// Includes "I just retired a stale generation" and "I just promoted".
    Nothing,
    Connect,
    /// Carries the PHASE, so the caller never re-matches a status it read
    /// separately and never supplies an epoch of its own.
    Prepare {
        generation: ConnectionGeneration,
        phase: CommitPhase,
    },
    /// pending(g) is committed but its epoch was never published (a prior
    /// publication was refused). The caller publishes and, on success,
    /// calls [`WorkMcpState::note_published`].
    Publish(PublishRequest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitPhase {
    /// AwaitingAcceptance(g): commit to `pending`, then publish.
    Pending,
    /// Ready(g,e) with nothing held: commit straight to `live` — worker B,
    /// and any worker whose pending entry was retired under it.
    Live { epoch: u64 },
}

/// The only publish input the facade ever receives from this state.
/// Fields private; minted only inside `commit`/`reconcile` — the caller
/// passes the accessors' values verbatim and can neither alter nor
/// fabricate a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishRequest {
    origin: ToolOrigin,
    generation: ConnectionGeneration,
    epoch: u64,
}

impl PublishRequest {
    pub fn origin(&self) -> &ToolOrigin {
        &self.origin
    }
    pub fn generation(&self) -> ConnectionGeneration {
        self.generation
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// A prepared-but-uncommitted candidate. Every field is set by
/// [`prepare`] from the `Prepare` outcome — none is caller-supplied,
/// which is what makes "the caller never names a generation it read
/// separately" structural (design r9 §5.3).
#[derive(Debug)]
pub struct Candidate {
    origin: ToolOrigin,
    generation: ConnectionGeneration,
    phase: CommitPhase,
    epoch: u64,
    catalog: AcceptedCatalog,
    validated: awili_seam::ValidatedRoots,
    prepared_at_revision: u64,
}

#[derive(Debug)]
pub enum PrepareRejection {
    /// Ruling C: an origin whose server advertised no epoch cannot reach
    /// Ready — no route, no implicit epoch.
    NoAdvertisedEpoch,
    /// Live-phase three-way identity (design r9 §5.2): the observation's
    /// advertised epoch must equal the observed Ready epoch.
    EpochMismatch {
        advertised: u64,
        expected: u64,
    },
    Roots(RootsRejection),
    Catalog(CatalogRejection),
}

#[derive(Debug)]
pub enum CommitOutcome {
    /// Entry and roots installed atomically under the one lock. `publish`
    /// is `Some` — carrying the ONLY publish request this commit will
    /// ever produce — iff the phase was Pending.
    Committed { publish: Option<PublishRequest> },
    /// Nothing installed, nothing published, nothing to roll back.
    Refused { reason: CommitRefusal },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitRefusal {
    /// The state moved past the revision this candidate was prepared from.
    RevisionChanged,
    /// An entry (any generation) already holds the origin.
    Occupied,
    /// Live: phase epoch, candidate epoch, and roots epoch not identical.
    EpochMismatch,
}

/// Both of `run_turn`'s MCP parameters from ONE locked read, as owned
/// values so no guard spans the turn (design r9 §5.4).
#[derive(Debug)]
pub struct TurnMcpInputs {
    pub extra_tools: Vec<ToolDef>,
    pub roots: RootsAcceptance,
    /// Production-surfaced degradation lines (impl review r1 P2-5, design
    /// §5.5): per-origin status with generation/class/tool counts,
    /// per-tool projection drops, and the reconcile diagnostics — never
    /// silent, delivered to the worker's event stream each turn open.
    pub degradations: Vec<String>,
}

/// Phase 1 — OUTSIDE any lock (ruling B). Validates and projects from
/// borrowed facts; touches no shared state.
pub fn prepare(
    connected: &ConnectedLaunch,
    host: &HostRootProposal,
    policy: &dyn RootsPolicy,
    phase: CommitPhase,
    revision: u64,
) -> Result<Candidate, PrepareRejection> {
    let Some(advertised) = connected.advertised_epoch() else {
        return Err(PrepareRejection::NoAdvertisedEpoch);
    };
    if let CommitPhase::Live { epoch } = phase {
        if advertised != epoch {
            return Err(PrepareRejection::EpochMismatch {
                advertised,
                expected: epoch,
            });
        }
    }
    let validated =
        awili_seam::validate_roots(connected, host, policy).map_err(PrepareRejection::Roots)?;
    let catalog = project_advertised(&validated, connected).map_err(PrepareRejection::Catalog)?;
    Ok(Candidate {
        origin: connected.origin().clone(),
        generation: connected.generation(),
        phase,
        epoch: advertised,
        catalog,
        validated,
        prepared_at_revision: revision,
    })
}

impl WorkMcpState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Production construction (unit 5 ruling 46): mints exactly ONE
    /// non-`Clone` brand bound to the exact `Arc` — the only path into the
    /// floor owner's registration. A `new()` state can never register.
    pub fn new_registered() -> (std::sync::Arc<WorkMcpState>, RegistrationBrand) {
        let state = std::sync::Arc::new(Self::default());
        let brand = RegistrationBrand {
            state: state.clone(),
        };
        (state, brand)
    }

    pub(crate) fn ever_retired(&self) -> bool {
        self.retired.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn mark_retired(&self) {
        self.retired
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// The committed live entry's complete floor set for `origin` (unit 5
    /// rulings 22/32): every committed descriptor's risk floor, under this
    /// state's own lock. Crate-private — reachable from outside only
    /// through `crate::floor_owner::accepted_floors`. A retired state
    /// mints nothing.
    pub(crate) fn committed_floors(
        &self,
        origin: &awili_tools::ToolOrigin,
    ) -> Option<(ConnectionGeneration, Vec<(String, awili_engine::RiskClass)>)> {
        if self.ever_retired() {
            return None;
        }
        let st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let live = st.live.get(&origin.origin)?;
        let floors = live
            .catalog
            .tools()
            .iter()
            .map(|d| (d.name().to_string(), d.descriptor.risk_floor()))
            .collect();
        Some((live.generation, floors))
    }

    pub fn as_of_revision(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_of_revision
    }

    /// Record a drive-level degradation for the next turn_inputs surface.
    /// The bounded, capability-free diagnostic sink (unit 5 r9 §1,
    /// ruling 51): append one line; nothing is returned, no entry access.
    pub fn note_diagnostic(&self, line: String) {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if st.diagnostics.len() < 256 {
            st.diagnostics.push(line);
        }
    }

    /// Drain recorded fail-closed diagnostics (impossible cells, stale
    /// candidates) for surfacing.
    pub fn take_diagnostics(&self) -> Vec<String> {
        std::mem::take(
            &mut self
                .inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .diagnostics,
        )
    }

    /// The total reconciliation table (design r9 §5.3, T1–T18 plus the
    /// revision guard). Under ONE lock: compare the observed fact against
    /// pending/live/roots, atomically retire whatever differs, promote a
    /// matching pending on its Ready fact, and report what the caller
    /// must do next.
    pub fn reconcile(
        &self,
        origin: &ToolOrigin,
        observed: &OriginStatus,
        revision: u64,
    ) -> ReconcileOutcome {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // The revision guard: a snapshot OLDER than one already acted on
        // must never tear down state a newer read installed (row 43).
        if revision < st.as_of_revision {
            return ReconcileOutcome::Nothing;
        }
        st.as_of_revision = revision;
        let key = origin.origin.clone();

        match observed {
            // T1/T2.
            OriginStatus::Disconnected => {
                st.retire_any(&key, origin);
                ReconcileOutcome::Connect
            }
            // T3–T5.
            OriginStatus::Connecting { generation } => {
                match st.held_generation(&key) {
                    Some(h) if h == *generation => {
                        // T5 — impossible under within-generation
                        // monotonicity (commits postdate Awaiting). Fail
                        // closed: trust the pump's fact, retire, note.
                        st.diagnostics.push(format!(
                            "impossible cell: Connecting({generation}) with a held entry \
                             for the same generation on {key:?}; retired"
                        ));
                        st.retire_any(&key, origin);
                    }
                    Some(_) => st.retire_any(&key, origin), // T4
                    None => {}                              // T3
                }
                ReconcileOutcome::Nothing
            }
            // T6–T10.
            OriginStatus::AwaitingAcceptance { generation, .. } => {
                match (st.pending.get(&key), st.live.get(&key)) {
                    (Some(p), _) if p.generation == *generation => {
                        if p.published {
                            ReconcileOutcome::Nothing // T7
                        } else {
                            // T8 — the retryable publication.
                            ReconcileOutcome::Publish(PublishRequest {
                                origin: origin.clone(),
                                generation: *generation,
                                epoch: p.epoch,
                            })
                        }
                    }
                    (_, Some(l)) if l.generation == *generation => {
                        // T9 — impossible (status cannot regress from
                        // Ready within a generation). Fail closed.
                        st.diagnostics.push(format!(
                            "impossible cell: AwaitingAcceptance({generation}) with a live \
                             entry for the same generation on {key:?}; retired"
                        ));
                        st.retire_any(&key, origin);
                        ReconcileOutcome::Prepare {
                            generation: *generation,
                            phase: CommitPhase::Pending,
                        }
                    }
                    (None, None) => ReconcileOutcome::Prepare {
                        generation: *generation,
                        phase: CommitPhase::Pending,
                    }, // T6
                    _ => {
                        st.retire_any(&key, origin); // T10
                        ReconcileOutcome::Prepare {
                            generation: *generation,
                            phase: CommitPhase::Pending,
                        }
                    }
                }
            }
            // T11–T16.
            OriginStatus::Ready {
                generation,
                accepted_epoch,
                ..
            } => {
                let promote = matches!(
                    st.pending.get(&key),
                    Some(p) if p.generation == *generation && p.epoch == *accepted_epoch
                );
                if promote {
                    // T11 — promotion, under this same lock, regardless of
                    // the published flag: Ready proves SOME publication
                    // landed (multi-worker case).
                    let p = st.pending.remove(&key).expect("matched above");
                    st.live.insert(
                        key,
                        LiveEntry {
                            generation: p.generation,
                            epoch: p.epoch,
                            catalog: p.catalog,
                        },
                    );
                    return ReconcileOutcome::Nothing;
                }
                match (st.pending.get(&key), st.live.get(&key)) {
                    (Some(p), _) if p.generation == *generation => {
                        // T12 — pending epoch differs from Ready.
                        st.retire_any(&key, origin);
                        ReconcileOutcome::Prepare {
                            generation: *generation,
                            phase: CommitPhase::Live {
                                epoch: *accepted_epoch,
                            },
                        }
                    }
                    (_, Some(l)) if l.generation == *generation => {
                        if l.epoch == *accepted_epoch {
                            ReconcileOutcome::Nothing // T13
                        } else {
                            // T14 — believed unreachable in 4b (no
                            // mid-generation re-acceptance is wired);
                            // handled anyway, fail closed.
                            st.retire_any(&key, origin);
                            ReconcileOutcome::Prepare {
                                generation: *generation,
                                phase: CommitPhase::Live {
                                    epoch: *accepted_epoch,
                                },
                            }
                        }
                    }
                    (None, None) => ReconcileOutcome::Prepare {
                        generation: *generation,
                        phase: CommitPhase::Live {
                            epoch: *accepted_epoch,
                        },
                    }, // T15
                    _ => {
                        st.retire_any(&key, origin); // T16
                        ReconcileOutcome::Prepare {
                            generation: *generation,
                            phase: CommitPhase::Live {
                                epoch: *accepted_epoch,
                            },
                        }
                    }
                }
            }
            // T17/T18.
            OriginStatus::Dead { .. } => {
                st.retire_any(&key, origin);
                ReconcileOutcome::Connect
            }
        }
    }

    /// Phase 2 — INSIDE the mutex. Consumes the candidate; a stale one is
    /// refused, never installed (rows 35/40/44).
    pub fn commit(&self, candidate: Candidate) -> CommitOutcome {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if st.as_of_revision != candidate.prepared_at_revision {
            let note = format!(
                "stale candidate for {:?} {} refused: prepared at revision {}, state at {}",
                candidate.origin.origin,
                candidate.generation,
                candidate.prepared_at_revision,
                st.as_of_revision
            );
            st.diagnostics.push(note);
            return CommitOutcome::Refused {
                reason: CommitRefusal::RevisionChanged,
            };
        }
        let key = candidate.origin.origin.clone();
        if st.pending.contains_key(&key) || st.live.contains_key(&key) {
            return CommitOutcome::Refused {
                reason: CommitRefusal::Occupied,
            };
        }
        // The three-way epoch identity for a Live commit (design r9
        // §5.2): the phase's e, the candidate's own epoch, and the
        // validated record's accepted epoch — defense in depth over
        // prepare's own refusal, because the fields could only disagree
        // through a bug in this module.
        if let CommitPhase::Live { epoch } = candidate.phase {
            if candidate.epoch != epoch || candidate.validated.record().epoch() != epoch {
                return CommitOutcome::Refused {
                    reason: CommitRefusal::EpochMismatch,
                };
            }
        }
        // Install: entry AND roots under the one lock — the two host
        // records are never observable disagreeing (row 32/36 atomicity).
        st.roots.accept(candidate.validated);
        match candidate.phase {
            CommitPhase::Pending => {
                let request = PublishRequest {
                    origin: candidate.origin.clone(),
                    generation: candidate.generation,
                    epoch: candidate.epoch,
                };
                st.pending.insert(
                    key,
                    PendingEntry {
                        generation: candidate.generation,
                        epoch: candidate.epoch,
                        catalog: candidate.catalog,
                        published: false,
                    },
                );
                CommitOutcome::Committed {
                    publish: Some(request),
                }
            }
            CommitPhase::Live { epoch } => {
                st.live.insert(
                    key,
                    LiveEntry {
                        generation: candidate.generation,
                        epoch,
                        catalog: candidate.catalog,
                    },
                );
                CommitOutcome::Committed { publish: None }
            }
        }
    }

    /// Mark `pending(origin, generation)` published. Under the lock;
    /// idempotent; a no-op if the entry was retired or promoted in the
    /// interim.
    pub fn note_published(&self, request: &PublishRequest) {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = st.pending.get_mut(&request.origin.origin) {
            if p.generation == request.generation {
                p.published = true;
            }
        }
    }

    /// Both of `run_turn`'s MCP parameters from one locked read, owned —
    /// no guard spans the turn (design r9 §5.4). Only LIVE catalogs
    /// contribute tools: pending authority is not yet dispatchable.
    /// RC2-GEMMA2 (R7-speed): the servers come in name order, every turn and
    /// every worker. The tool list is part of the prompt the model server
    /// caches; in map order it changed from one worker to the next, and
    /// the server read the whole prompt again.
    pub fn turn_inputs(&self) -> TurnMcpInputs {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut extra_tools = Vec::new();
        let mut degradations = Vec::new();
        let mut live: Vec<(&String, &LiveEntry)> = st.live.iter().collect();
        live.sort_by(|a, b| a.0.cmp(b.0));
        for (key, entry) in live {
            extra_tools.extend(entry.catalog.tools().iter().cloned());
            degradations.push(format!(
                "{key}: live ({}, epoch {}, {:?}), {} tools",
                entry.generation,
                entry.epoch,
                entry.catalog.attestation(),
                entry.catalog.tools().len(),
            ));
            for (name, problem) in entry.catalog.problems() {
                degradations.push(format!("{key}: tool {name:?} dropped: {problem:?}"));
            }
        }
        let mut pending: Vec<(&String, &PendingEntry)> = st.pending.iter().collect();
        pending.sort_by(|a, b| a.0.cmp(b.0));
        for (key, entry) in pending {
            degradations.push(format!(
                "{key}: pending acceptance ({}, epoch {}, {:?}, {} tools, published: {})",
                entry.generation,
                entry.epoch,
                entry.catalog.attestation(),
                entry.catalog.tools().len(),
                entry.published
            ));
            for (name, problem) in entry.catalog.problems() {
                degradations.push(format!("{key}: tool {name:?} dropped: {problem:?}"));
            }
        }
        degradations.push(format!(
            "reconciled: {} live, {} pending",
            st.live.len(),
            st.pending.len()
        ));
        degradations.append(&mut st.diagnostics);
        TurnMcpInputs {
            extra_tools,
            roots: st.roots.clone(),
            degradations,
        }
    }
}

impl WorkAuthorityState {
    fn held_generation(&self, key: &str) -> Option<ConnectionGeneration> {
        self.pending
            .get(key)
            .map(|p| p.generation)
            .or_else(|| self.live.get(key).map(|l| l.generation))
    }

    /// Retire whatever is held for the origin — pending, live, and the
    /// roots record — in this one locked section, so the host records are
    /// never observable disagreeing.
    fn retire_any(&mut self, key: &str, origin: &ToolOrigin) {
        let retired = self
            .pending
            .remove(key)
            .map(|p| p.generation)
            .or_else(|| self.live.remove(key).map(|l| l.generation));
        if let Some(generation) = retired {
            self.roots.invalidate(origin, generation);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use awili_engine::RiskClass;
    use awili_seam::{validate_roots, HostRootProposal, RootsPolicy};
    use awili_tools::attest::{
        LaunchKind, LaunchToken, SetupRecordFacts, UnverifiedReason, VerifiedLaunch,
    };
    use awili_tools::runtime::{AdvertisedTool, McpEvent, McpRuntimeHandle};

    struct Allow;
    impl RootsPolicy for Allow {
        fn accepts(
            &self,
            _v: &VerifiedLaunch,
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

    fn tool(name: &str, risk: RiskClass) -> AdvertisedTool {
        AdvertisedTool {
            name: name.into(),
            arg_schema_canonical: r#"{"type":"object","properties":{}}"#.into(),
            risk,
            description: String::new(),
        }
    }

    fn bad_schema_tool(name: &str, schema: &str) -> AdvertisedTool {
        AdvertisedTool {
            name: name.into(),
            arg_schema_canonical: schema.into(),
            risk: RiskClass::ReadOnly,
            description: String::new(),
        }
    }

    /// A LEGITIMATELY emitted observation. `gen_skip` advances the fixture
    /// allocator so tests can hold observations of DIFFERENT generations.
    pub(crate) fn observation_with(
        origin: &str,
        gen_skip: u64,
        tools: Vec<AdvertisedTool>,
        dropped: Vec<String>,
        epoch: Option<u64>,
    ) -> ConnectedLaunch {
        let mut alloc = awili_engine::generation::GenerationAllocator::new();
        for _ in 0..gen_skip {
            let _ = alloc.next();
        }
        let record = SetupRecordFacts {
            origin: awili_engine::ToolOrigin::new(origin),
            setup_id: awili_tools::attest::SetupIdentityId::parse("setup-1").unwrap(),
            pinned_requirement: "cdhash H\"00\"".into(),
            expected_protocol_name: "srv".into(),
            launch_kind: LaunchKind::Direct,
            program: "/usr/local/lib/awili/srv".into(),
        };
        let token = LaunchToken::mint_verified(&record, alloc.next());
        emit_observation(token, tools, dropped, epoch)
    }

    pub(crate) fn unverified_observation(
        origin: &str,
        tools: Vec<AdvertisedTool>,
        epoch: Option<u64>,
    ) -> ConnectedLaunch {
        let mut alloc = awili_engine::generation::GenerationAllocator::new();
        let token = LaunchToken::mint_unverified(
            awili_engine::ToolOrigin::new(origin),
            alloc.next(),
            UnverifiedReason::NoSetupRecord,
        );
        emit_observation(token, tools, Vec::new(), epoch)
    }

    fn emit_observation(
        token: LaunchToken,
        tools: Vec<AdvertisedTool>,
        dropped: Vec<String>,
        epoch: Option<u64>,
    ) -> ConnectedLaunch {
        let (sink, stream) = awili_tools::runtime::event_channel::<String>(4);
        let (ctx, _crx) = async_channel::bounded(1);
        let handle = McpRuntimeHandle::from_parts(ctx, stream, std::thread::spawn(|| {}));
        sink.emit_server_connected_blocking(token, "srv".into(), tools, dropped, epoch)
            .expect("emit");
        drop(sink);
        match handle.try_recv_event() {
            Some(McpEvent::ServerConnected(c)) => c,
            other => panic!("expected the observation, got {other:?}"),
        }
    }

    fn validated_for(c: &ConnectedLaunch) -> ValidatedRoots {
        validate_roots(
            c,
            &HostRootProposal {
                origin: c.origin().clone(),
                proposed_roots: vec!["/work".into()],
            },
            &Allow,
        )
        .expect("validates")
    }

    /// Gate row 18a.
    #[test]
    fn projection_preserves_the_declared_risk_class_through_side_effect() {
        for risk in [
            RiskClass::ReadOnly,
            RiskClass::Mutating,
            RiskClass::Irreversible,
            RiskClass::Privileged,
        ] {
            let c = observation_with("mcp:t", 0, vec![tool("a", risk)], vec![], Some(1));
            let catalog = project_advertised(&validated_for(&c), &c).expect("projects");
            assert_eq!(catalog.tools().len(), 1);
            assert_eq!(
                catalog.tools()[0].descriptor.risk_floor(),
                risk,
                "side_effect {:?} must floor back to {risk:?}",
                catalog.tools()[0].descriptor.side_effect
            );
        }
    }

    /// Gate row 18b — including a duplicate hidden in the capture-dropped
    /// names, which is exactly why dropped NAMES exist.
    #[test]
    fn duplicate_advertised_name_rejects_the_whole_generation_catalog() {
        let c = observation_with(
            "mcp:t",
            0,
            vec![
                tool("a", RiskClass::ReadOnly),
                tool("a", RiskClass::Mutating),
            ],
            vec![],
            Some(1),
        );
        let err = project_advertised(&validated_for(&c), &c).unwrap_err();
        assert!(matches!(err, CatalogRejection::DuplicateName { ref name } if name == "a"));

        let c = observation_with(
            "mcp:t",
            0,
            vec![tool("a", RiskClass::ReadOnly)],
            vec!["a".into()],
            Some(1),
        );
        let err = project_advertised(&validated_for(&c), &c).unwrap_err();
        assert!(matches!(err, CatalogRejection::DuplicateName { ref name } if name == "a"));
    }

    /// Gate row 18c.
    #[test]
    fn unparsable_or_non_object_schema_drops_only_that_tool() {
        let c = observation_with(
            "mcp:t",
            0,
            vec![
                tool("good", RiskClass::ReadOnly),
                bad_schema_tool("broken", "not json"),
                bad_schema_tool("array", "[1,2]"),
            ],
            vec![],
            Some(1),
        );
        let catalog = project_advertised(&validated_for(&c), &c).expect("projects the good tool");
        assert_eq!(catalog.tools().len(), 1);
        assert_eq!(catalog.tools()[0].name(), "good");
        assert_eq!(catalog.problems().len(), 2);
        assert!(matches!(
            catalog.problems()[0],
            (ref n, ProjectionProblem::SchemaUnparsable { .. }) if n == "broken"
        ));
        assert!(matches!(
            catalog.problems()[1],
            (ref n, ProjectionProblem::SchemaNotAnObject) if n == "array"
        ));
    }

    /// Gate row 18d.
    #[test]
    fn nothing_projected_from_a_non_empty_advertisement_leaves_the_origin_unavailable() {
        let c = observation_with(
            "mcp:t",
            0,
            vec![bad_schema_tool("only", "not json")],
            vec![],
            Some(1),
        );
        let err = project_advertised(&validated_for(&c), &c).unwrap_err();
        assert_eq!(err, CatalogRejection::NothingProjected);
    }

    /// Gate row 28.
    #[test]
    fn projection_rejects_an_advertisement_from_a_different_generation() {
        let c1 = observation_with(
            "mcp:t",
            0,
            vec![tool("a", RiskClass::ReadOnly)],
            vec![],
            Some(1),
        );
        let c2 = observation_with(
            "mcp:t",
            1,
            vec![tool("a", RiskClass::ReadOnly)],
            vec![],
            Some(1),
        );
        assert_ne!(c1.generation(), c2.generation());
        let err = project_advertised(&validated_for(&c1), &c2).unwrap_err();
        assert!(
            matches!(err, CatalogRejection::GenerationMismatch { .. }),
            "{err:?}"
        );
    }

    /// Gate row 29.
    #[test]
    fn capture_dropping_every_tool_leaves_the_origin_unavailable() {
        let c = observation_with("mcp:t", 0, vec![], vec!["a".into(), "b".into()], Some(1));
        let err = project_advertised(&validated_for(&c), &c).unwrap_err();
        assert_eq!(err, CatalogRejection::NothingProjected);
    }

    /// Projection does not re-adjudicate class: an admitted unverified
    /// observation projects identically, with the class riding as a fact.
    #[test]
    fn projection_carries_the_class_as_a_fact_without_adjudicating_it() {
        let c = unverified_observation("mcp:t", vec![tool("a", RiskClass::ReadOnly)], Some(1));
        let catalog = project_advertised(&validated_for(&c), &c).expect("projects");
        assert_eq!(catalog.attestation(), AttestationClass::Unverified);
        assert!(catalog.setup_id().is_none());
    }

    // ----- WorkMcpState rows (unit 4b commit 4c; design r9 §5.3/§6.3) -----

    fn allow() -> Allow {
        Allow
    }

    fn host_for(c: &ConnectedLaunch) -> HostRootProposal {
        HostRootProposal {
            origin: c.origin().clone(),
            proposed_roots: vec!["/work".into()],
        }
    }

    fn ready_tool_obs(gen_skip: u64, epoch: u64) -> ConnectedLaunch {
        observation_with(
            "mcp:t",
            gen_skip,
            vec![tool("a", RiskClass::ReadOnly)],
            vec![],
            Some(epoch),
        )
    }

    /// Drive one origin to a committed PENDING entry; returns (state,
    /// generation, publish request).
    fn committed_pending(epoch: u64) -> (WorkMcpState, ConnectionGeneration, PublishRequest) {
        let state = WorkMcpState::new();
        let c = ready_tool_obs(0, epoch);
        let g = c.generation();
        let outcome = state.reconcile(
            c.origin(),
            &OriginStatus::AwaitingAcceptance {
                generation: g,
                attestation: awili_tools::attest::AttestationClass::Verified,
                claimed_server: "srv".into(),
                tool_count: 1,
                dropped_count: 0,
                roots_epoch: Some(epoch),
            },
            1,
        );
        let ReconcileOutcome::Prepare {
            generation,
            phase: CommitPhase::Pending,
        } = outcome
        else {
            panic!("expected Prepare Pending, got {outcome:?}");
        };
        assert_eq!(generation, g);
        let cand = prepare(&c, &host_for(&c), &allow(), CommitPhase::Pending, 1).expect("prepares");
        let CommitOutcome::Committed { publish: Some(req) } = state.commit(cand) else {
            panic!("expected a committed pending with a publish request");
        };
        (state, g, req)
    }

    /// Gate row 20.
    #[test]
    fn reconciliation_removes_the_host_catalog_for_a_dead_generation() {
        let (state, g, req) = committed_pending(1);
        state.note_published(&req);
        let origin = awili_engine::ToolOrigin::new("mcp:t");
        let outcome = state.reconcile(
            &origin,
            &OriginStatus::Dead {
                generation: g,
                cause: awili_tools::runtime::DeathCause::TransportFailed,
                detail: "died".into(),
                attestation: None,
            },
            2,
        );
        assert!(matches!(outcome, ReconcileOutcome::Connect), "{outcome:?}");
        let inputs = state.turn_inputs();
        assert!(inputs.extra_tools.is_empty(), "catalog retired");
        assert!(
            inputs.roots.accepted(&origin).is_none(),
            "roots invalidated"
        );
    }

    /// Gate row 30.
    #[test]
    fn pending_catalog_becomes_live_only_on_the_matching_ready_fact() {
        let (state, g, req) = committed_pending(4);
        state.note_published(&req);
        let origin = awili_engine::ToolOrigin::new("mcp:t");
        // A Ready fact for a DIFFERENT generation does not promote.
        let mut alloc = awili_engine::generation::GenerationAllocator::new();
        let _ = alloc.next();
        let g2 = alloc.next();
        assert_ne!(g, g2);
        let outcome = state.reconcile(
            &origin,
            &OriginStatus::Ready {
                generation: g2,
                accepted_epoch: 4,
                attestation: awili_tools::attest::AttestationClass::Verified,
            },
            2,
        );
        assert!(
            matches!(
                outcome,
                ReconcileOutcome::Prepare {
                    phase: CommitPhase::Live { epoch: 4 },
                    ..
                }
            ),
            "{outcome:?}"
        );
        assert!(
            state.turn_inputs().extra_tools.is_empty(),
            "nothing promoted, the mismatched pending was retired"
        );

        // Rebuild pending and promote on the MATCHING fact.
        let (state, g, req) = committed_pending(4);
        state.note_published(&req);
        let outcome = state.reconcile(
            &origin,
            &OriginStatus::Ready {
                generation: g,
                accepted_epoch: 4,
                attestation: awili_tools::attest::AttestationClass::Verified,
            },
            2,
        );
        assert!(matches!(outcome, ReconcileOutcome::Nothing), "{outcome:?}");
        assert_eq!(state.turn_inputs().extra_tools.len(), 1, "promoted to live");
    }

    /// Gate row 32.
    #[test]
    fn dead_invalidates_live_catalog_and_roots_under_the_same_lock() {
        let (state, g, req) = committed_pending(2);
        state.note_published(&req);
        let origin = awili_engine::ToolOrigin::new("mcp:t");
        let _ = state.reconcile(
            &origin,
            &OriginStatus::Ready {
                generation: g,
                accepted_epoch: 2,
                attestation: awili_tools::attest::AttestationClass::Verified,
            },
            2,
        );
        assert_eq!(state.turn_inputs().extra_tools.len(), 1);
        assert!(state.turn_inputs().roots.accepted(&origin).is_some());

        let _ = state.reconcile(
            &origin,
            &OriginStatus::Dead {
                generation: g,
                cause: awili_tools::runtime::DeathCause::TransportFailed,
                detail: "died".into(),
                attestation: None,
            },
            3,
        );
        let inputs = state.turn_inputs();
        assert!(inputs.extra_tools.is_empty(), "live catalog gone");
        assert!(
            inputs.roots.accepted(&origin).is_none(),
            "roots gone in the SAME reconcile — one lock, no observable disagreement"
        );
    }

    /// Gate row 35.
    #[test]
    fn a_candidate_prepared_against_a_retired_generation_is_refused_at_commit() {
        let state = WorkMcpState::new();
        let c = ready_tool_obs(0, 1);
        let g = c.generation();
        let origin = awili_engine::ToolOrigin::new("mcp:t");
        let _ = state.reconcile(
            &origin,
            &OriginStatus::AwaitingAcceptance {
                generation: g,
                attestation: awili_tools::attest::AttestationClass::Verified,
                claimed_server: "srv".into(),
                tool_count: 1,
                dropped_count: 0,
                roots_epoch: Some(1),
            },
            1,
        );
        let cand = prepare(&c, &host_for(&c), &allow(), CommitPhase::Pending, 1).expect("prepares");
        // The generation retires (and the revision advances) between
        // prepare and commit.
        let _ = state.reconcile(
            &origin,
            &OriginStatus::Dead {
                generation: g,
                cause: awili_tools::runtime::DeathCause::TransportFailed,
                detail: "died".into(),
                attestation: None,
            },
            2,
        );
        let outcome = state.commit(cand);
        assert!(
            matches!(
                outcome,
                CommitOutcome::Refused {
                    reason: CommitRefusal::RevisionChanged
                }
            ),
            "{outcome:?}"
        );
    }

    /// Gate row 36.
    #[test]
    fn reconcile_retires_the_held_generation_atomically_with_roots_invalidation() {
        let (state, g, req) = committed_pending(1);
        state.note_published(&req);
        let origin = awili_engine::ToolOrigin::new("mcp:t");
        assert!(state.turn_inputs().roots.accepted(&origin).is_some());
        // A Connecting fact for a NEWER generation retires the held one.
        let mut alloc = awili_engine::generation::GenerationAllocator::new();
        let _ = alloc.next();
        let g2 = alloc.next();
        assert_ne!(g, g2);
        let outcome = state.reconcile(&origin, &OriginStatus::Connecting { generation: g2 }, 2);
        assert!(matches!(outcome, ReconcileOutcome::Nothing), "{outcome:?}");
        let inputs = state.turn_inputs();
        assert!(inputs.extra_tools.is_empty());
        assert!(
            inputs.roots.accepted(&origin).is_none(),
            "entry and roots retired in ONE locked section"
        );
    }

    /// Gate row 40.
    #[test]
    fn a_refused_commit_yields_no_publish_request() {
        let (state, _g, _req) = committed_pending(1);
        // A second candidate for the same origin: Occupied — and the
        // refusal carries NO request; nothing to publish, nothing to roll
        // back.
        let c = ready_tool_obs(0, 1);
        let cand = prepare(&c, &host_for(&c), &allow(), CommitPhase::Pending, 1).expect("prepares");
        match state.commit(cand) {
            CommitOutcome::Refused { reason } => {
                assert_eq!(reason, CommitRefusal::Occupied);
            }
            CommitOutcome::Committed { .. } => panic!("occupied origin must refuse"),
        }
    }

    /// Gate row 41.
    #[test]
    fn a_refused_publication_is_retried_from_reconcile_until_published() {
        // Commit succeeds; the caller's publication FAILS (scripted: it
        // simply never calls note_published). T8 re-drives it.
        let (state, g, first_req) = committed_pending(3);
        let origin = awili_engine::ToolOrigin::new("mcp:t");
        let awaiting = OriginStatus::AwaitingAcceptance {
            generation: g,
            attestation: awili_tools::attest::AttestationClass::Verified,
            claimed_server: "srv".into(),
            tool_count: 1,
            dropped_count: 0,
            roots_epoch: Some(3),
        };
        let outcome = state.reconcile(&origin, &awaiting, 2);
        let ReconcileOutcome::Publish(req) = outcome else {
            panic!("unpublished pending must re-drive publication, got {outcome:?}");
        };
        assert_eq!(req, first_req, "the SAME state-minted request");
        // The retry SUCCEEDS this time.
        state.note_published(&req);
        let outcome = state.reconcile(&origin, &awaiting, 3);
        assert!(matches!(outcome, ReconcileOutcome::Nothing), "{outcome:?}");
    }

    /// Gate row 42.
    #[test]
    fn promotion_refuses_a_pending_epoch_that_differs_from_ready() {
        let (state, g, req) = committed_pending(5);
        state.note_published(&req);
        let origin = awili_engine::ToolOrigin::new("mcp:t");
        let outcome = state.reconcile(
            &origin,
            &OriginStatus::Ready {
                generation: g,
                accepted_epoch: 7,
                attestation: awili_tools::attest::AttestationClass::Verified,
            },
            2,
        );
        assert!(
            matches!(
                outcome,
                ReconcileOutcome::Prepare {
                    phase: CommitPhase::Live { epoch: 7 },
                    ..
                }
            ),
            "a mismatched pending must retire and re-prepare, got {outcome:?}"
        );
        assert!(
            state.turn_inputs().extra_tools.is_empty(),
            "never a half-promoted entry"
        );
    }

    /// Gate row 43.
    #[test]
    fn a_stale_revision_snapshot_reconciles_to_nothing_and_retires_nothing() {
        let (state, g, req) = committed_pending(1);
        state.note_published(&req);
        let origin = awili_engine::ToolOrigin::new("mcp:t");
        // Advance to revision 5 with the matching Ready (promotes).
        let _ = state.reconcile(
            &origin,
            &OriginStatus::Ready {
                generation: g,
                accepted_epoch: 1,
                attestation: awili_tools::attest::AttestationClass::Verified,
            },
            5,
        );
        assert_eq!(state.turn_inputs().extra_tools.len(), 1);
        // A STALE snapshot (revision 3) claiming the origin Dead must
        // mutate NOTHING.
        let outcome = state.reconcile(
            &origin,
            &OriginStatus::Dead {
                generation: g,
                cause: awili_tools::runtime::DeathCause::TransportFailed,
                detail: "stale ghost".into(),
                attestation: None,
            },
            3,
        );
        assert!(matches!(outcome, ReconcileOutcome::Nothing), "{outcome:?}");
        assert_eq!(
            state.turn_inputs().extra_tools.len(),
            1,
            "a stale read must never tear down state a newer read installed"
        );
        assert_eq!(state.as_of_revision(), 5, "the fence did not regress");
    }

    /// Gate row 44 — white-box (same-module privilege): the fields are
    /// private, so a disagreeing candidate can only exist through a bug in
    /// this module; commit refuses it anyway (defense in depth over
    /// prepare's own EpochMismatch refusal, which is also asserted).
    #[test]
    fn a_live_candidate_with_a_mismatched_epoch_is_refused_at_commit() {
        // prepare-level: advertised 3 cannot satisfy Live{5}.
        let c = ready_tool_obs(0, 3);
        let err = prepare(
            &c,
            &host_for(&c),
            &allow(),
            CommitPhase::Live { epoch: 5 },
            1,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            PrepareRejection::EpochMismatch {
                advertised: 3,
                expected: 5
            }
        ));

        // commit-level: a doctored candidate (constructible only here).
        let state = WorkMcpState::new();
        let c = ready_tool_obs(0, 5);
        let good = prepare(
            &c,
            &host_for(&c),
            &allow(),
            CommitPhase::Live { epoch: 5 },
            0,
        )
        .expect("prepares");
        let doctored = Candidate {
            origin: good.origin.clone(),
            generation: good.generation,
            phase: CommitPhase::Live { epoch: 9 },
            epoch: good.epoch,
            catalog: good.catalog,
            validated: good.validated,
            prepared_at_revision: good.prepared_at_revision,
        };
        let outcome = state.commit(doctored);
        assert!(
            matches!(
                outcome,
                CommitOutcome::Refused {
                    reason: CommitRefusal::EpochMismatch
                }
            ),
            "{outcome:?}"
        );
    }

    /// Gate row 45 — white-box: construct T5 (Connecting(g) with a held
    /// entry for g), which within-generation monotonicity makes
    /// impossible; reconcile retires it, records a diagnostic, and the
    /// drive proceeds from the current fact.
    #[test]
    fn an_impossible_status_and_held_combination_retires_and_fails_closed() {
        let (state, g, req) = committed_pending(1);
        state.note_published(&req);
        let origin = awili_engine::ToolOrigin::new("mcp:t");
        let _ = state.take_diagnostics();
        let outcome = state.reconcile(&origin, &OriginStatus::Connecting { generation: g }, 2);
        assert!(matches!(outcome, ReconcileOutcome::Nothing), "{outcome:?}");
        // turn_inputs DRAINS diagnostics into its degradation surface, so
        // the diagnostic assertion reads that surface.
        let inputs = state.turn_inputs();
        assert!(inputs.extra_tools.is_empty());
        assert!(
            inputs.roots.accepted(&origin).is_none(),
            "retired, fail closed"
        );
        assert!(
            inputs
                .degradations
                .iter()
                .any(|d| d.contains("impossible cell")),
            "the impossible cell is recorded, never silent: {:?}",
            inputs.degradations
        );
    }

    /// RC2-GEMMA2 (R7-speed): the tools of several live servers come in
    /// server-name order whatever order the servers went live in, so the
    /// tool list — part of the prompt the model server caches — is the same
    /// bytes for every worker.
    #[test]
    fn rc2gemma2_live_tools_come_in_server_name_order() {
        let names = [
            "mcp:q", "mcp:c", "mcp:x", "mcp:a", "mcp:m", "mcp:z", "mcp:f", "mcp:k",
        ];
        let state = WorkMcpState::new();
        for (i, origin) in names.iter().enumerate() {
            let c = observation_with(
                origin,
                0,
                vec![tool(&format!("t{i}"), RiskClass::ReadOnly)],
                vec![],
                Some(1),
            );
            let g = c.generation();
            let outcome = state.reconcile(
                c.origin(),
                &OriginStatus::AwaitingAcceptance {
                    generation: g,
                    attestation: awili_tools::attest::AttestationClass::Verified,
                    claimed_server: "srv".into(),
                    tool_count: 1,
                    dropped_count: 0,
                    roots_epoch: Some(1),
                },
                2 * i as u64 + 1,
            );
            assert!(
                matches!(outcome, ReconcileOutcome::Prepare { .. }),
                "{i} {origin}: {outcome:?}"
            );
            let cand = prepare(
                &c,
                &host_for(&c),
                &allow(),
                CommitPhase::Pending,
                2 * i as u64 + 1,
            )
            .expect("prepares");
            let CommitOutcome::Committed { publish: Some(req) } = state.commit(cand) else {
                panic!("expected a committed pending entry");
            };
            state.note_published(&req);
            let _ = state.reconcile(
                c.origin(),
                &OriginStatus::Ready {
                    generation: g,
                    accepted_epoch: 1,
                    attestation: awili_tools::attest::AttestationClass::Verified,
                },
                2 * i as u64 + 2,
            );
        }
        let order: Vec<String> = state
            .turn_inputs()
            .extra_tools
            .iter()
            .map(|d| d.identity.origin.origin.clone())
            .collect();
        let mut sorted: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        sorted.sort();
        assert_eq!(order, sorted);
    }
}
