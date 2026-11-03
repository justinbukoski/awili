//! Unit 5: the process-long catalog floor owner (design r7-r9, rulings
//! 29-33, 38-40, 45-46).
//!
//! The fourth bundle member — [`InstallSeedAuthority`] — is moved HERE at
//! issuance by [`crate::authority`] and never leaves: no public API
//! returns, borrows, or exposes it (ruling 32). The one public
//! capability-bearing operation is [`accepted_floors`], which takes only
//! an origin, locks the owner, consults the CURRENT registered concrete
//! [`WorkMcpState`] (never a trait object, callback, or adapter — ruling
//! 39 made a publicly implementable source an alternate mint site), reads
//! its committed live entry, and mints internally.
//!
//! Registration is non-replayable (ruling 40/46): construction through
//! [`WorkMcpState::new_registered`] mints exactly ONE non-`Clone`
//! [`RegistrationBrand`] bound to that `Arc`; `register` consumes it under
//! the owner's lock, refuses a state that was EVER retired, and atomically
//! retires the predecessor. Worker respawn re-registers a fresh state; the
//! capability itself never moves (rulings 30-31: custody is the owner's
//! lifetime, not any worker's).

use std::sync::{Arc, Mutex};

use awili_engine_log::authz::{AcceptedFloors, InstallSeedAuthority};
use awili_tools::ToolOrigin;

use crate::mcp_catalog::{RegistrationBrand, WorkMcpState};

struct CatalogFloorOwner {
    /// Present from the process's one successful `issue_bundle` on; a
    /// failed, builtin-only, or dropped composition cannot strand it
    /// (r6 verdict rev 1) because it is owner-held, never worker-held.
    auth: Option<InstallSeedAuthority>,
    current: Option<Arc<WorkMcpState>>,
}

static OWNER: Mutex<CatalogFloorOwner> = Mutex::new(CatalogFloorOwner {
    auth: None,
    current: None,
});

/// Called ONCE by [`crate::authority`] with the fourth bundle member —
/// crate-private: the raw authority crosses no public API (ruling 32).
pub(crate) fn adopt_authority(auth: InstallSeedAuthority) {
    let mut owner = OWNER.lock().unwrap_or_else(|e| e.into_inner());
    debug_assert!(owner.auth.is_none(), "issue_bundle succeeds once");
    owner.auth = Some(auth);
}

/// Why a registration was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterRefusal {
    /// The state was already retired by a successor's registration — a
    /// retired catalog can never re-enter (ruling 40's non-replayability;
    /// the brand consumption already prevents the same-brand replay, this
    /// arm pins the belt-and-braces case of a brand raced against its own
    /// retirement).
    EverRetired,
}

/// Register a freshly constructed catalog state as THE current committed
/// catalog, atomically retiring the predecessor (relative to
/// [`accepted_floors`]: both run under the owner's one lock). Consumes the
/// brand — a state registers exactly once, ever.
pub fn register_catalog(brand: RegistrationBrand) -> Result<(), RegisterRefusal> {
    let state = brand.into_state();
    let mut owner = OWNER.lock().unwrap_or_else(|e| e.into_inner());
    if state.ever_retired() {
        return Err(RegisterRefusal::EverRetired);
    }
    if let Some(prev) = owner.current.take() {
        prev.mark_retired();
    }
    owner.current = Some(state);
    Ok(())
}

/// The ONE public capability-bearing operation (rulings 32/45): derive the
/// complete floor set for `origin` from the CURRENT registered state's
/// committed live entry — every committed descriptor's risk floor, the
/// whole set, no caller input besides the origin. `None`: no authority
/// issued yet, nothing registered, or the origin is not live.
pub fn accepted_floors(origin: &ToolOrigin) -> Option<AcceptedFloors> {
    let owner = OWNER.lock().unwrap_or_else(|e| e.into_inner());
    let auth = owner.auth.as_ref()?;
    let state = owner.current.as_ref()?;
    let (generation, floors) = state.committed_floors(origin)?;
    Some(auth.mint_floors(origin.clone(), generation, floors))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mcp_catalog::{prepare, CommitOutcome, CommitPhase, ReconcileOutcome, WorkMcpState};
    use crate::mcp_coordinator::OriginStatus;
    use awili_engine::RiskClass;
    use awili_seam::{HostRootProposal, RootsPolicy};
    use awili_tools::runtime::ConnectedLaunch;

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

    /// Serializes every test that mutates the process-global OWNER (the
    /// e2e install test in executor_seam_test.rs shares it).
    pub(crate) fn owner_mutex() -> &'static std::sync::Mutex<()> {
        static M: std::sync::Mutex<()> = std::sync::Mutex::new(());
        &M
    }

    /// Drive one origin all the way to a committed LIVE entry on `state`,
    /// minting a PROCESS-unique generation (the respawn-fence property
    /// under test rides on it).
    pub(crate) fn commit_live(state: &WorkMcpState, origin: &str, epoch: u64) -> ConnectedLaunch {
        let record = awili_tools::attest::SetupRecordFacts {
            origin: awili_engine::ToolOrigin::new(origin),
            setup_id: awili_tools::attest::SetupIdentityId::parse("setup-1").unwrap(),
            pinned_requirement: "cdhash H\"00\"".into(),
            expected_protocol_name: "srv".into(),
            launch_kind: awili_tools::attest::LaunchKind::Direct,
            program: "/usr/local/lib/awili/srv".into(),
        };
        let token = awili_tools::attest::LaunchToken::mint_verified(
            &record,
            awili_engine::generation::process().next(),
        );
        let (sink, stream) = awili_tools::runtime::event_channel::<String>(4);
        let (ctx, _crx) = async_channel::bounded(1);
        let handle = awili_tools::runtime::McpRuntimeHandle::from_parts(
            ctx,
            stream,
            std::thread::spawn(|| {}),
        );
        sink.emit_server_connected_blocking(
            token,
            "srv".into(),
            vec![awili_tools::runtime::AdvertisedTool {
                name: "run".into(),
                arg_schema_canonical: r#"{"type":"object","properties":{}}"#.into(),
                risk: RiskClass::Privileged,
                description: String::new(),
            }],
            vec![],
            Some(epoch),
        )
        .expect("emit");
        drop(sink);
        let Some(awili_tools::runtime::McpEvent::ServerConnected(c)) = handle.try_recv_event()
        else {
            panic!("expected the observation");
        };
        let g = c.generation();
        let origin_t = c.origin().clone();
        let host = HostRootProposal {
            origin: origin_t.clone(),
            proposed_roots: vec!["/w".into()],
        };
        // Pending → published → Ready promotes to Live.
        let out = state.reconcile(
            &origin_t,
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
        assert!(matches!(out, ReconcileOutcome::Prepare { .. }), "{out:?}");
        let cand = prepare(&c, &host, &Allow, CommitPhase::Pending, 1).expect("prepare");
        let CommitOutcome::Committed { publish: Some(req) } = state.commit(cand) else {
            panic!("pending commit");
        };
        state.note_published(&req);
        // A published pending matching the Ready fact PROMOTES to live
        // inside reconcile (T11) — the outcome is Nothing.
        let out = state.reconcile(
            &origin_t,
            &OriginStatus::Ready {
                generation: g,
                accepted_epoch: epoch,
                attestation: awili_tools::attest::AttestationClass::Verified,
            },
            2,
        );
        assert!(matches!(out, ReconcileOutcome::Nothing), "{out:?}");
        assert!(
            state.committed_floors(&origin_t).is_some(),
            "live entry committed"
        );
        c
    }

    /// The full custody/respawn contract (r8 §2 / rulings 45-46): A mints
    /// from its committed catalog; B's registration retires A atomically;
    /// A's retained handle mints nothing afterwards; a `new()` state has
    /// no brand and can never register.
    #[test]
    fn respawn_retires_the_predecessor_and_a_retained_handle_mints_nothing() {
        let _g = owner_mutex().lock().unwrap_or_else(|e| e.into_inner());
        let _ = crate::authority().expect("bundle issues once in this process");
        let (a, brand_a) = WorkMcpState::new_registered();
        register_catalog(brand_a).expect("A registers");
        let ca = commit_live(&a, "mcp:owner-test", 1);
        let origin = ca.origin().clone();
        let floors_a = accepted_floors(&origin).expect("A is current and live");
        assert_eq!(floors_a.generation(), ca.generation());
        assert_eq!(
            floors_a.floors().collect::<Vec<_>>(),
            vec![("run", RiskClass::Privileged)]
        );

        // Worker B replaces A (respawn): atomic retirement.
        let (b, brand_b) = WorkMcpState::new_registered();
        register_catalog(brand_b).expect("B registers");
        let cb = commit_live(&b, "mcp:owner-test", 1);
        let floors_b = accepted_floors(&origin).expect("B is current and live");
        assert_eq!(floors_b.generation(), cb.generation());
        assert_ne!(
            floors_a.generation(),
            floors_b.generation(),
            "process-unique generations fence the respawn"
        );

        // A is retired: its retained handle mints nothing, directly or
        // through the owner.
        assert!(a.ever_retired());
        assert!(
            a.committed_floors(&origin).is_none(),
            "retired mints nothing"
        );

        // A brandless state cannot register — no path exists (compile-level:
        // RegistrationBrand has no public constructor; this pins the
        // runtime half).
        let _plain = WorkMcpState::new();
        // (no register_catalog call possible without a brand)

        // Racing/successor sanity: B remains current after A's retirement.
        assert!(accepted_floors(&origin).is_some());
    }
}
