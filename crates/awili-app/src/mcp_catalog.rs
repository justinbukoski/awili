//! Unit 4b app-side composition over the moved catalog state (unit 5 r9
//! §1): the facade-composing drive and the attestation-composing install
//! grant input stay HERE; the state and every pure projection live in
//! `awili_engine_worker::mcp_catalog` and are consumed as owned values.

// The moved vocabulary, re-exported so app modules keep one import path.
// (Bin-crate re-exports lint as unused when a name is only consumed by
// SOME build targets; the surface is deliberate.)
#[allow(unused_imports)]
pub use awili_engine_worker::mcp_catalog::{
    prepare, project_advertised, AcceptedCatalog, Candidate, CatalogRejection, CommitOutcome,
    CommitPhase, CommitRefusal, PrepareRejection, ProjectionProblem, PublishRequest,
    ReconcileOutcome, TurnMcpInputs, WorkMcpState,
};

use awili_engine_worker::mcp_coordinator::OriginStatus;
use awili_tools::runtime::ConnectedLaunch;
use awili_tools::ToolOrigin;

// ============================================================================
// The Install-grant boundary (design r9 §2.6, rulings E and G)
// ============================================================================

use awili_attest::InstallAuthorityView;
use awili_engine_worker::mcp_coordinator::McpFacade;
use awili_tools::attest::VerifiedLaunch;

/// Why a grant input was refused. Every variant is a refusal of PERMANENT
/// authority — nothing here affects per-call dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantRefusal {
    /// The launch is unverified — `bind_verified` refused (clause 1).
    NotVerified,
    /// The coordinator's CURRENT state does not show this observation's
    /// generation Ready (clause 1, ruling E): retired, dead, superseded,
    /// or never accepted.
    NotLive,
    /// The CURRENT installed record disagrees with the token's setup
    /// identity or pinned requirement (clause 2, row 38).
    ReplacedSetupRecord { field: &'static str },
    /// The record's protocol name, the server's claimed name, and the
    /// reserved name are not all identical (clause 3).
    ProtocolNameMismatch,
    /// The authority view, the observation, and the reserved origin are
    /// not all identical (clause 4).
    WrongOrigin,
    /// The effective configuration does not authorize Install (clause 4,
    /// ruling G, row 39).
    NotInstallAuthorized,
}

/// The reserved origin (the only origin that can ever seed an Install
/// grant): `"mcp:" + INSTALL_TRUSTED_SERVER`.
fn reserved_origin() -> String {
    format!("mcp:{}", awili_config::INSTALL_TRUSTED_SERVER)
}

/// Unit-5 grant seed input (design r9 §2.6). Reads CURRENT liveness from
/// the facade INSIDE the boundary — there is no liveness parameter for a
/// caller to retain or replay (ruling E): the facade read linearizes on
/// the coordinator's own state lock, so "current" means the pump's state
/// at THIS call, not at any earlier observation.
///
/// What the query does and does not promise: a generation that dies AFTER
/// the read but before the host writes its grant record is not detected
/// here — no non-transactional check can promise otherwise. That residue
/// is harmless by the same generation fence that makes an old `Arc`
/// inert: the grant record is scoped to setup identity and pinned
/// requirement, not to a generation, and EVERY launch of EVERY generation
/// re-attests against the installed record at spawn. What ruling E
/// required — and what the query delivers — is that the CALLER can never
/// substitute retained state for the coordinator's current answer.
pub fn install_grant_input<'a, T: Send + 'static>(
    authority: &InstallAuthorityView<'_>,
    facade: &McpFacade<T>,
    connected: &'a ConnectedLaunch,
) -> Result<&'a VerifiedLaunch, GrantRefusal> {
    // Clause 4 first (identity scope): the view, the observation, and the
    // reserved origin all name the same thing, and the config authorizes
    // Install (the lib.rs:354-355 name-and-trust conjunction, preserved
    // by InstallAuthorityView::current).
    if authority.origin() != connected.origin() {
        return Err(GrantRefusal::WrongOrigin);
    }
    if connected.origin().origin != reserved_origin() {
        return Err(GrantRefusal::WrongOrigin);
    }
    if !authority.install_authorized() {
        return Err(GrantRefusal::NotInstallAuthorized);
    }

    // Clause 1: the verified class...
    let verified = match connected.bind() {
        awili_tools::attest::BoundLaunch::Verified(v) => v,
        awili_tools::attest::BoundLaunch::Unverified(_) => return Err(GrantRefusal::NotVerified),
    };
    // ...and CURRENT liveness, queried at the grant linearization point —
    // whatever the caller retained, the answer is the pump's now.
    match facade.live_generation(connected.origin()) {
        Some((generation, _epoch)) if generation == connected.generation() => {}
        _ => return Err(GrantRefusal::NotLive),
    }

    // Clause 2: the CURRENT installed record. (With content-derived setup
    // identities every field change flips the id, so the pinned-
    // requirement comparison is defense in depth — asserted anyway.)
    if verified.setup_id() != &authority.record().setup_id {
        return Err(GrantRefusal::ReplacedSetupRecord { field: "setup_id" });
    }
    if verified.pinned_requirement() != authority.record().pinned_requirement {
        return Err(GrantRefusal::ReplacedSetupRecord {
            field: "pinned_requirement",
        });
    }

    // Clause 3: the protocol-name triple.
    if verified.expected_protocol_name() != connected.claimed_server()
        || connected.claimed_server() != awili_config::INSTALL_TRUSTED_SERVER
    {
        return Err(GrantRefusal::ProtocolNameMismatch);
    }

    Ok(verified)
}

/// ONE reconciliation pass over the catalog origins (design r9 §5.4) —
/// the body of the worker's turn-open drive, extracted so the drive is
/// testable against a scripted coordinator (row 19). No retry loops: each
/// origin gets at most one action per pass, and the NEXT pass re-drives
/// whatever remained incomplete.
pub(crate) fn drive_reconcile_once<T: Send + 'static>(
    facade: &McpFacade<T>,
    state: &WorkMcpState,
    origins: &[ToolOrigin],
    workdir: &str,
    policy: &dyn awili_seam::RootsPolicy,
) {
    let deadline = || std::time::Instant::now() + std::time::Duration::from_secs(2);
    let snapshot = facade.lifecycle();
    for origin in origins {
        // §5.5: EVERY configured origin surfaces its current lifecycle
        // status each turn open — Disconnected, Connecting, Awaiting,
        // Ready, Dead alike (impl review r2 finding 3).
        state.note_diagnostic(match snapshot.origins.get(&origin.origin) {
            None => format!("{}: not yet announced", origin.origin),
            Some(OriginStatus::Disconnected) => format!("{}: disconnected", origin.origin),
            Some(OriginStatus::Connecting { generation }) => {
                format!("{}: connecting ({generation})", origin.origin)
            }
            Some(OriginStatus::AwaitingAcceptance {
                generation,
                attestation,
                tool_count,
                dropped_count,
                ..
            }) => format!(
                "{}: awaiting acceptance ({generation}, {attestation:?}, {tool_count} tools, \
                 {dropped_count} dropped)",
                origin.origin
            ),
            Some(OriginStatus::Ready {
                generation,
                accepted_epoch,
                attestation,
            }) => format!(
                "{}: ready ({generation}, epoch {accepted_epoch}, {attestation:?})",
                origin.origin
            ),
            Some(OriginStatus::Dead {
                generation,
                cause,
                detail,
                ..
            }) => format!(
                "{}: dead ({generation}, {cause:?}: {detail})",
                origin.origin
            ),
        });
        let Some(status) = snapshot.origins.get(&origin.origin) else {
            // Not yet announced (runtime still constructing): idempotent
            // connect, re-driven next turn open.
            let _ = facade.connect_origin(deadline(), origin.clone());
            continue;
        };
        match state.reconcile(origin, status, snapshot.revision) {
            ReconcileOutcome::Nothing => {}
            ReconcileOutcome::Connect => {
                let _ = facade.connect_origin(deadline(), origin.clone());
            }
            ReconcileOutcome::Publish(req) => {
                if facade
                    .publish_accepted_roots(
                        deadline(),
                        req.origin().clone(),
                        req.generation(),
                        req.epoch(),
                    )
                    .is_ok()
                {
                    state.note_published(&req);
                } else {
                    // Surfaced like the initial-publication refusal (impl
                    // review r3): the RETRY's refusal is never discarded.
                    state.note_diagnostic(format!(
                        "{}: publication retry refused; will retry next turn open",
                        origin.origin
                    ));
                }
            }
            ReconcileOutcome::Prepare { generation, phase } => {
                let Some(connected) = facade.connected(origin, generation) else {
                    continue; // observation gone; next pass re-drives
                };
                let host = awili_seam::HostRootProposal {
                    origin: origin.clone(),
                    proposed_roots: vec![workdir.to_string()],
                };
                match prepare(&connected, &host, policy, phase, snapshot.revision) {
                    Ok(cand) => match state.commit(cand) {
                        CommitOutcome::Committed { publish: Some(req) } => {
                            if facade
                                .publish_accepted_roots(
                                    deadline(),
                                    req.origin().clone(),
                                    req.generation(),
                                    req.epoch(),
                                )
                                .is_ok()
                            {
                                state.note_published(&req);
                            } else {
                                state.note_diagnostic(format!(
                                    "{}: publication refused; will retry next turn open",
                                    origin.origin
                                ));
                            }
                        }
                        CommitOutcome::Committed { publish: None } => {}
                        CommitOutcome::Refused { reason } => {
                            // Re-driven next pass — and SURFACED (impl
                            // review r2 finding 3), never silent.
                            state.note_diagnostic(format!(
                                "{}: commit refused ({reason:?}); re-driving next turn open",
                                origin.origin
                            ));
                        }
                    },
                    Err(rejection) => {
                        state.note_diagnostic(format!(
                            "{}: not prepared: {rejection:?}",
                            origin.origin
                        ));
                    }
                }
            }
        }
    }
}

/// The production roots policy for user-declared `[mcp.servers]` (unit 4b
/// commit 5). The server exists because the USER configured it; the host
/// proposes exactly the bound workdir; per-call risk approval still gates
/// every action. So: a verified launch's root set is accepted as proposed,
/// and an unverified launch (no setup record — every server except the
/// installed `mac-shell`) is ADMITTED — full-strength validation, Ready,
/// dispatch — while remaining permanently unable to satisfy the
/// Install-grant boundary (design r9 §4.2: two properties, independent).
#[derive(Debug, Clone, Copy)]
pub struct DeclaredServerPolicy;

impl awili_seam::RootsPolicy for DeclaredServerPolicy {
    fn accepts(
        &self,
        _verified: &VerifiedLaunch,
        _connected: &ConnectedLaunch,
        _host: &awili_seam::HostRootProposal,
    ) -> Result<(), String> {
        Ok(())
    }
    fn admit_unverified(
        &self,
        _unverified: &awili_tools::attest::UnverifiedLaunch,
        _connected: &ConnectedLaunch,
    ) -> bool {
        true
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use awili_engine::generation::ConnectionGeneration;
    use awili_engine::RiskClass;
    use awili_tools::runtime::AdvertisedTool;

    /// Shared with the moved state tests in
    /// `awili_engine_worker::mcp_catalog` (duplicated: cfg(test) helpers
    /// do not cross crates).
    use awili_seam::{HostRootProposal, RootsPolicy};

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
        fn admit_unverified(
            &self,
            _u: &awili_tools::attest::UnverifiedLaunch,
            _c: &ConnectedLaunch,
        ) -> bool {
            true
        }
    }

    fn allow() -> Allow {
        Allow
    }

    use awili_tools::attest::{LaunchKind, LaunchToken, SetupRecordFacts};
    use awili_tools::runtime::{McpEvent, McpRuntimeHandle};

    /// Duplicated from the moved state tests in
    /// `awili_engine_worker::mcp_catalog` (cfg(test) helpers do not cross
    /// crates): a legitimately emitted observation, and a committed-but-
    /// unpublished pending entry.
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

    fn tool(name: &str, risk: RiskClass) -> AdvertisedTool {
        AdvertisedTool {
            name: name.into(),
            arg_schema_canonical: r#"{"type":"object","properties":{}}"#.into(),
            risk,
            description: String::new(),
        }
    }

    // ----- Grant boundary + worker-B rows (unit 4b commit 4d) -----

    use awili_attest::setup_record::{load_setup_records, SetupRecords};
    use awili_attest::InstallAuthorityView;
    use awili_config::{McpServerConfig, McpTrust};
    use awili_engine_worker::mcp_coordinator::{McpCoordinator, McpFacade};
    use std::io::Write as _;
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(5);

    fn scripted_coordinator() -> (
        McpCoordinator<String>,
        McpFacade<String>,
        awili_tools::runtime::McpEventSink<String>,
    ) {
        let (sink, stream) = awili_tools::runtime::event_channel::<String>(16);
        let (ctx, _crx) = async_channel::bounded::<awili_tools::runtime::McpCommand<String>>(4);
        let handle = awili_tools::runtime::McpRuntimeHandle::from_parts(
            ctx,
            stream,
            std::thread::spawn(|| {}),
        );
        let coord = McpCoordinator::new(handle);
        let facade = coord.facade();
        (coord, facade, sink)
    }

    fn write_setup(program: &str, requirement: &str) -> (tempfile::TempDir, SetupRecords) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("attested-servers.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            r#"
[[server]]
origin        = "mcp:mac-shell"
program       = "{program}"
launch_kind   = "direct"
requirement   = '{requirement}'
protocol_name = "mac-shell"
trust         = "install"
"#
        )
        .unwrap();
        let records = load_setup_records(&path);
        assert!(records.problems().is_empty(), "{:?}", records.problems());
        (dir, records)
    }

    fn install_config(name: &str, trust: McpTrust) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: "/usr/local/lib/awili/mac-shell".to_string(),
            args: Vec::new(),
            env: Default::default(),
            trust,
            modes: Vec::new(),
            enabled: true,
        }
    }

    /// Emit one generation's ServerConnected + RootsAccepted through the
    /// sink and wait until the pump shows Ready; returns the generation.
    fn drive_ready(
        sink: &awili_tools::runtime::McpEventSink<String>,
        facade: &McpFacade<String>,
        records: &SetupRecords,
        gen_skip: u64,
        epoch: u64,
    ) -> ConnectionGeneration {
        let origin = awili_engine::ToolOrigin::new("mcp:mac-shell");
        let rec = records.record(&origin).expect("record");
        let mut alloc = awili_engine::generation::GenerationAllocator::new();
        for _ in 0..gen_skip {
            let _ = alloc.next();
        }
        let g = alloc.next();
        let token = awili_tools::attest::LaunchToken::mint_verified(rec.facts(), g);
        sink.emit_server_connected_blocking(
            token,
            "mac-shell".into(),
            vec![tool("run", RiskClass::Privileged)],
            Vec::new(),
            Some(epoch),
        )
        .expect("emit connected");
        sink.send_blocking(awili_tools::runtime::McpEvent::RootsAccepted {
            origin: origin.clone(),
            generation: g,
            epoch,
        })
        .expect("emit accepted");
        let deadline = Instant::now() + WAIT;
        loop {
            if facade.live_generation(&origin) == Some((g, epoch)) {
                return g;
            }
            assert!(Instant::now() < deadline, "pump never reached Ready({g})");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn kill_generation(
        sink: &awili_tools::runtime::McpEventSink<String>,
        facade: &McpFacade<String>,
        g: ConnectionGeneration,
    ) {
        let origin = awili_engine::ToolOrigin::new("mcp:mac-shell");
        sink.send_blocking(awili_tools::runtime::McpEvent::ServerDied {
            origin: origin.clone(),
            generation: g,
            cause: awili_tools::runtime::DeathCause::TransportFailed,
            selected_class: Some(awili_tools::attest::AttestationClass::Verified),
            detail: "scripted death".into(),
        })
        .expect("emit died");
        let deadline = Instant::now() + WAIT;
        while facade.live_generation(&origin).is_some() {
            assert!(Instant::now() < deadline, "Ready never dropped");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Gate row 33 — including the r6-verdict retained-pair sequence:
    /// obtain the observation and observe Ready(g); retire g and advance
    /// to Ready(g+1); the retained observation cannot seed a grant,
    /// because the boundary's OWN facade query returns g+1 and the
    /// function has no liveness parameter to replay.
    #[test]
    fn a_stale_genuine_token_cannot_self_bind_into_a_current_grant() {
        let (_coord, facade, sink) = scripted_coordinator();
        let (_dir, records) = write_setup(
            "/usr/local/lib/awili/mac-shell",
            "cdhash H\"4dc1e6f6ab3f835b521f1e9da8fa5ef2a484f7d9\"",
        );
        let origin = awili_engine::ToolOrigin::new("mcp:mac-shell");
        let cfg = install_config("mac-shell", McpTrust::Install);
        let rec = records.record(&origin).expect("record");
        let view = InstallAuthorityView::current(rec, &cfg).expect("view");

        let g1 = drive_ready(&sink, &facade, &records, 0, 1);
        let retained = facade.connected(&origin, g1).expect("the observation");
        // CONTROL: while Ready(g1), the grant input succeeds.
        assert!(
            install_grant_input(&view, &facade, &retained).is_ok(),
            "control: live + verified + matching record must grant"
        );

        // Retire g1; bring up g2. The caller retains the g1 pair.
        kill_generation(&sink, &facade, g1);
        let g2 = drive_ready(&sink, &facade, &records, 1, 1);
        assert_ne!(g1, g2);

        let refused = install_grant_input(&view, &facade, &retained).unwrap_err();
        assert_eq!(refused, GrantRefusal::NotLive);
        drop(sink);
    }

    /// Gate row 34.
    #[test]
    fn a_worker_created_after_ready_gets_the_current_tools_and_only_its_own_roots() {
        let (_coord, facade, sink) = scripted_coordinator();
        let (_dir, records) = write_setup(
            "/usr/local/lib/awili/mac-shell",
            "cdhash H\"4dc1e6f6ab3f835b521f1e9da8fa5ef2a484f7d9\"",
        );
        let origin = awili_engine::ToolOrigin::new("mcp:mac-shell");
        let epoch = 3;
        let g = drive_ready(&sink, &facade, &records, 0, epoch);
        let observation = facade.connected(&origin, g).expect("observation");

        // Worker A: pending → published → promoted on the Ready fact.
        let a = WorkMcpState::new();
        let awaiting = OriginStatus::AwaitingAcceptance {
            generation: g,
            attestation: awili_tools::attest::AttestationClass::Verified,
            claimed_server: "mac-shell".into(),
            tool_count: 1,
            dropped_count: 0,
            roots_epoch: Some(epoch),
        };
        let ReconcileOutcome::Prepare {
            phase: CommitPhase::Pending,
            ..
        } = a.reconcile(&origin, &awaiting, 1)
        else {
            panic!("A prepares pending");
        };
        let host_a = HostRootProposal {
            origin: origin.clone(),
            proposed_roots: vec!["/work-a".into()],
        };
        let cand = prepare(&observation, &host_a, &allow(), CommitPhase::Pending, 1).unwrap();
        let CommitOutcome::Committed { publish: Some(req) } = a.commit(cand) else {
            panic!("A commits pending");
        };
        a.note_published(&req);
        let ready = OriginStatus::Ready {
            generation: g,
            accepted_epoch: epoch,
            attestation: awili_tools::attest::AttestationClass::Verified,
        };
        assert!(matches!(
            a.reconcile(&origin, &ready, 2),
            ReconcileOutcome::Nothing
        ));

        // Worker B, created while the origin is ALREADY Ready: the same
        // code path, entered at the Live phase, against B's OWN workdir.
        let b = WorkMcpState::new();
        let ReconcileOutcome::Prepare {
            generation,
            phase: CommitPhase::Live { epoch: e },
        } = b.reconcile(&origin, &ready, 2)
        else {
            panic!("B prepares live");
        };
        assert_eq!((generation, e), (g, epoch));
        let b_observation = facade
            .connected(&origin, generation)
            .expect("the SAME observation");
        let host_b = HostRootProposal {
            origin: origin.clone(),
            proposed_roots: vec!["/work-b".into()],
        };
        let cand = prepare(
            &b_observation,
            &host_b,
            &allow(),
            CommitPhase::Live { epoch: e },
            2,
        )
        .unwrap();
        assert!(matches!(
            b.commit(cand),
            CommitOutcome::Committed { publish: None }
        ));

        // B holds the CURRENT tools and ONLY its own roots.
        let inputs = b.turn_inputs();
        assert_eq!(inputs.extra_tools.len(), 1);
        assert_eq!(inputs.extra_tools[0].name(), "run");
        let record = inputs.roots.record(&origin).expect("B's record");
        assert_eq!(record.roots(), ["/work-b".to_string()]);
        // A's roots are untouched and different — host bookkeeping,
        // per-worker (ruling D).
        let a_record_roots = a
            .turn_inputs()
            .roots
            .record(&origin)
            .expect("A's record")
            .roots()
            .to_vec();
        assert_eq!(a_record_roots, ["/work-a".to_string()]);
        drop(sink);
    }

    /// Gate row 31.
    #[test]
    fn ready_tools_reach_the_per_turn_registry_as_extra_tools() {
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
        let inputs = state.turn_inputs();
        assert_eq!(inputs.extra_tools.len(), 1, "the live catalog's tools");
        assert_eq!(inputs.extra_tools[0].name(), "a");
        assert_eq!(
            inputs.extra_tools[0].identity.origin.origin, "mcp:t",
            "run_turn's extra_tools carry the MCP origin identity"
        );
    }

    /// Gate row 38 — both clause-2 comparisons. (Setup identities are
    /// CONTENT-DERIVED, so any replaced field flips the id and the
    /// pinned-requirement comparison is defense in depth; both replaced
    /// records must refuse regardless.)
    #[test]
    fn a_replaced_setup_record_refuses_the_grant_input() {
        let (_coord, facade, sink) = scripted_coordinator();
        let original_req = "cdhash H\"4dc1e6f6ab3f835b521f1e9da8fa5ef2a484f7d9\"";
        let (_d1, records) = write_setup("/usr/local/lib/awili/mac-shell", original_req);
        let origin = awili_engine::ToolOrigin::new("mcp:mac-shell");
        let cfg = install_config("mac-shell", McpTrust::Install);
        let g = drive_ready(&sink, &facade, &records, 0, 1);
        let observation = facade.connected(&origin, g).expect("observation");

        // The installed record is REPLACED (new program) after the launch.
        let (_d2, replaced) = write_setup("/tmp/replaced-binary", original_req);
        let rec = replaced.record(&origin).expect("replaced record");
        let view = InstallAuthorityView::current(rec, &cfg).expect("view");
        let refused = install_grant_input(&view, &facade, &observation).unwrap_err();
        assert!(
            matches!(refused, GrantRefusal::ReplacedSetupRecord { .. }),
            "{refused:?}"
        );

        // The installed record is replaced with a NEW REQUIREMENT.
        let (_d3, replaced) = write_setup(
            "/usr/local/lib/awili/mac-shell",
            "cdhash H\"ffffffffffffffffffffffffffffffffffffffff\"",
        );
        let rec = replaced.record(&origin).expect("replaced record");
        let view = InstallAuthorityView::current(rec, &cfg).expect("view");
        let refused = install_grant_input(&view, &facade, &observation).unwrap_err();
        assert!(
            matches!(refused, GrantRefusal::ReplacedSetupRecord { .. }),
            "{refused:?}"
        );
        drop(sink);
    }

    /// Gate row 39 — complete configs, never a bare trust (ruling G):
    /// Prompt refuses at the boundary; a programmatically built
    /// `{name: "other", trust: Install}` refuses LOUDLY at the view's one
    /// constructor, an explicit malformed-authority refusal.
    #[test]
    fn non_install_effective_trust_refuses_the_grant_input() {
        let (_coord, facade, sink) = scripted_coordinator();
        let (_dir, records) = write_setup(
            "/usr/local/lib/awili/mac-shell",
            "cdhash H\"4dc1e6f6ab3f835b521f1e9da8fa5ef2a484f7d9\"",
        );
        let origin = awili_engine::ToolOrigin::new("mcp:mac-shell");
        let rec = records.record(&origin).expect("record");
        let g = drive_ready(&sink, &facade, &records, 0, 1);
        let observation = facade.connected(&origin, g).expect("observation");

        // trust = Prompt: never seeds a grant even while live + verified.
        let prompt = install_config("mac-shell", McpTrust::Prompt);
        let view = InstallAuthorityView::current(rec, &prompt).expect("view");
        assert_eq!(
            install_grant_input(&view, &facade, &observation).unwrap_err(),
            GrantRefusal::NotInstallAuthorized
        );

        // A programmatically malformed Install config refuses at
        // CONSTRUCTION — an explicit refusal, never a silent downgrade.
        let malformed = install_config("other", McpTrust::Install);
        let err = InstallAuthorityView::current(rec, &malformed).unwrap_err();
        assert!(
            matches!(
                err,
                awili_attest::AuthorityRefusal::MalformedInstallTrust { ref name } if name == "other"
            ),
            "{err:?}"
        );
        drop(sink);
    }
}
