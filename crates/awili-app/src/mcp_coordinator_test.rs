// Unit 4 item 5 — the REAL-runtime coordinator tests: the coordinator from
// `awili-engine-worker` driving `spawn_mcp_runtime` and the
// awili-mcp-testserver gate, so every admission decision and terminal is
// produced by the actual runtime, never synthesized (r6: the coordinator
// test exercises the runtime-guaranteed event sequence). Included at the
// CRATE ROOT so each test's bare name is its full path — `cargo test
// duplicate_rejection_before_terminal_leaves_the_primary_live -- --exact`
// (the item-2/item-4 lesson; a bin-only package has no lib target). Plain
// comments because included items admit no inner `//!` docs; `Duration`,
// `Instant`, `CallDisposition`, `ToolOrigin`, `spec`, `call`,
// `test_config`, `EVENT_WAIT`, `spawn_mcp_runtime`, and `McpServerCatalog`
// come from main.rs and the sibling mcp_dup_admission_test.rs include —
// the shared crate-root scope.

use awili_engine_worker::mcp_coordinator::SubmissionOutcome;

use crate::mcp_runtime::LoopGate;

/// Unit 4b coordinator-level bring-up: connect the origin, watch the pump's
/// ingested lifecycle to learn the machine's own generation, publish the
/// generation-bound acceptance, and wait for Ready — everything through the
/// REAL facade and the REAL pump, nothing scripted.
fn coord_bring_up(
    facade: &awili_engine_worker::mcp_coordinator::McpFacade<String>,
    origin: &str,
    epoch: u64,
) -> awili_engine::generation::ConnectionGeneration {
    use awili_engine_worker::mcp_coordinator::OriginStatus;
    let far = Instant::now() + Duration::from_secs(10);
    facade
        .connect_origin(far, ToolOrigin::new(origin))
        .expect("connect origin");
    let deadline = Instant::now() + EVENT_WAIT;
    let generation = loop {
        if let Some(OriginStatus::AwaitingAcceptance { generation, .. }) =
            facade.lifecycle().origins.get(origin)
        {
            break *generation;
        }
        assert!(
            Instant::now() < deadline,
            "origin {origin:?} never reached AwaitingAcceptance"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    facade
        .publish_accepted_roots(far, ToolOrigin::new(origin), generation, epoch)
        .expect("publish accepted roots");
    let deadline = Instant::now() + EVENT_WAIT;
    loop {
        if matches!(
            facade.lifecycle().origins.get(origin),
            Some(OriginStatus::Ready { .. })
        ) {
            break;
        }
        assert!(Instant::now() < deadline, "origin {origin:?} never reached Ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    generation
}

/// `terminal_before_duplicate_rejection_retires_only_the_primary` — THE
/// BINDING r6 Delta B″ proof, against the REAL runtime through the REAL
/// coordinator: every admission decision, terminal, and rejection below
/// is produced by `spawn_mcp_runtime`'s actual loop against the real
/// testserver and consumed by the coordinator's actual pump. Nothing is
/// scripted, synthesized, or hand-dequeued.
///
/// The forced ordering, step by step. The `cfg(test)` `LoopGate` is the
/// deterministic lever at the real-runtime boundary (the f3a386d
/// discipline — a normal runtime carries no gate; this config opts in):
///
/// (a) BOTH submissions enqueued before the original's completion is
///     processed: the runtime consumes and dispatches the original and
///     immediately parks on the gate holding command consumption AND
///     completion processing; `gate.engaged()` positively observes that
///     park (no await point sits between the arm and it), and the
///     duplicate's `Ok` waiter positively observes it entering the
///     runtime's command channel — where it sits unconsumable, because
///     the loop cannot reach its command arm. Held by construction, not
///     by timing: even if the server's reply had already landed, the
///     completion waits unprocessed in the JoinSet.
///
/// (b) terminal BEFORE the queued duplicate: the single release lets
///     completions flow while command intake stays held; the loop
///     processes the original's real completion and emits its ONE
///     `CallFinished`, and only that emission lifts the hold. The pump
///     receiving the terminal and committing it to the primary's waiter
///     — observed below — is therefore also the observation that the
///     emission preceded the duplicate's consumption.
///
/// (c) the runtime's OWN admission rule then rejects the duplicate:
///     consumed with `pending` empty and the intent in the bounded
///     recently-terminal set, the real `pending ∪ recently_terminal`
///     code emits `SubmissionRejected` with the real reason string
///     (never synthesized here) — the reason itself is the runtime's
///     testimony that the terminal was committed before consumption.
///
/// (d) the coordinator retires ONLY the primary on the terminal; the
///     later rejection reaches the secondary's own waiter with its own
///     ticket; both tickets conserved, nothing quarantined.
#[test]
fn terminal_before_duplicate_rejection_retires_only_the_primary() {
    let gate = std::sync::Arc::new(LoopGate::new());
    // The slow gate server: the original's real echo reply takes 500ms
    // of real server time — the completion genuinely races the test's
    // bookkeeping, which is exactly what the gate holds off.
    let catalog = McpServerCatalog::new().with_stdio(
        ToolOrigin::new("mcp:slow"),
        spec(&[("MCP_FAULT", "slow"), ("MCP_DELAY_MS", "500")]),
    );
    let mut config = test_config();
    config.gate = Some(std::sync::Arc::clone(&gate));
    let handle = spawn_mcp_runtime::<String>(catalog, config);
    let coord = awili_engine_worker::mcp_coordinator::McpCoordinator::new(handle);
    let facade = coord.facade();
    let far = Instant::now() + Duration::from_secs(10);
    let g_slow = coord_bring_up(&facade, "mcp:slow", 1);

    // The ORIGINAL: through the real coordinator into the real runtime.
    let w1 = facade
        .submit_call(
            far,
            "t1".to_string(),
            "intent-bnd".to_string(),
            ToolOrigin::new("mcp:slow"),
            call("c1", "echo", r#"{"text":"real"}"#),
            g_slow, 1,
        )
        .expect("primary enqueues");
    assert!(w1.is_primary());
    // (a), first half — FIRED-POSITIVE: the gate observed the runtime
    // CONSUME and dispatch the original; from that instant until the
    // release below, the loop is parked — no command consumed, no
    // completion processed.
    let deadline = Instant::now() + EVENT_WAIT;
    while !gate.engaged() {
        assert!(
            Instant::now() < deadline,
            "the runtime never consumed the original CallTool"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // (a), second half — the DUPLICATE, enqueued while the loop is
    // parked: the `Ok` waiter is the positive observation that the
    // coordinator's submit thread placed it in the runtime's bounded
    // command channel, and the FIFO channel plus the parked loop make
    // it strictly the SECOND call the runtime will consume.
    let w2 = facade
        .submit_call(
            far,
            "t2".to_string(),
            "intent-bnd".to_string(),
            ToolOrigin::new("mcp:slow"),
            call("c2", "echo", r#"{"text":"dup-never-runs"}"#),
            g_slow, 1,
        )
        .expect("duplicate still enqueues honestly");
    assert!(!w2.is_primary());

    // The single release: completion processing flows; command intake
    // stays held until the original's `CallFinished` has been emitted.
    gate.release();

    // (b) — the original's ONE terminal reaches the PRIMARY's waiter:
    // the pump committed it, which is the waiter-side observation that
    // the runtime emitted the terminal before consuming the duplicate
    // (command intake was held until the emission itself).
    match w1.wait_deadline(Instant::now() + EVENT_WAIT) {
        Some(SubmissionOutcome::Terminal {
            ticket,
            disposition,
            result,
        }) => {
            assert_eq!(ticket, "t1");
            assert_eq!(disposition, CallDisposition::Completed);
            assert_eq!(result.content, "real");
        }
        other => panic!("primary must receive its terminal, got {other:?}"),
    }

    // (c) — the queued duplicate, consumed only now (post-terminal):
    // the REAL admission rule's own rejection reaches the SECONDARY's
    // own waiter with its OWN ticket, and the reason string is the
    // recently-terminal branch's, never synthesized here.
    match w2.wait_deadline(Instant::now() + EVENT_WAIT) {
        Some(SubmissionOutcome::Rejected { ticket, reason }) => {
            assert_eq!(ticket, "t2", "the duplicate's ticket returns to its own waiter");
            assert!(
                reason.contains("already terminal"),
                "the recently-terminal admission rule must fire: {reason}"
            );
        }
        other => panic!("duplicate must receive its rejection, got {other:?}"),
    }

    // (d) — the terminal retired ONLY the primary: the retired waiter
    // receives nothing further, both tickets were conserved on their
    // own waiters, and exactly the one hard diagnostic a forced
    // secondary earns.
    assert!(
        w1.wait_deadline(Instant::now() + Duration::from_millis(200)).is_none(),
        "the retired primary must not receive the duplicate's rejection"
    );
    assert_eq!(facade.live_submission_waiters(), 0);
    assert!(facade.take_quarantine().is_empty());
    assert_eq!(
        facade.diagnostics(),
        vec![awili_engine_worker::mcp_coordinator::CoordinatorDiagnostic::SecondarySubmission {
            intent_id: "intent-bnd".to_string(),
            submission_id: awili_tools::runtime::SubmissionId(1),
        }],
        "exactly the secondary diagnostic — no other violation"
    );
    drop(facade);
    drop(coord);
}

/// `submission_against_a_retired_intent_still_reaches_its_own_waiter` —
/// the already-retired-intent case over the REAL runtime and item 4's
/// strengthened admission rule: the original's terminal has already landed
/// and retired the primary; the duplicate — submitted AFTER the terminal,
/// so the runtime's `pending ∪ recently_terminal` admission rejects it
/// through the bounded tombstone — still reaches ITS OWN waiter with ITS
/// OWN ticket. Both tickets conserved; nothing synthesized.
///
/// The BINDING r6 queued-duplicate interleaving (both submissions enqueued
/// FIRST, the original's `CallFinished` emitted before the queued
/// duplicate is consumed) is forced against this same real runtime and
/// coordinator by `terminal_before_duplicate_rejection_retires_only_the_primary`
/// above, via the cfg(test) runtime gate. This test keeps the
/// complementary already-retired case.
#[test]
fn submission_against_a_retired_intent_still_reaches_its_own_waiter() {
    let catalog =
        McpServerCatalog::new().with_stdio(ToolOrigin::new("mcp:tbr"), spec(&[]));
    let handle = spawn_mcp_runtime::<String>(catalog, test_config());
    let coord = awili_engine_worker::mcp_coordinator::McpCoordinator::new(handle);
    let facade = coord.facade();
    let far = Instant::now() + Duration::from_secs(10);
    facade
.connect_origin(far, ToolOrigin::new("mcp:tbr")).map(|_| ()).expect("connect");
    let g_tbr = coord_bring_up(&facade, "mcp:tbr", 1);

    // The primary: enqueued, dispatched, completed by the real server.
    let w1 = facade
        .submit_call(
            far,
            "t1".to_string(),
            "intent-tbr".to_string(),
            ToolOrigin::new("mcp:tbr"),
            call("c1", "echo", r#"{"text":"real"}"#),
            g_tbr, 1,
        )
        .expect("primary enqueues");
    assert!(w1.is_primary());
    match w1.wait_deadline(Instant::now() + EVENT_WAIT) {
        Some(SubmissionOutcome::Terminal {
            ticket,
            disposition,
            result,
        }) => {
            assert_eq!(ticket, "t1");
            assert_eq!(disposition, CallDisposition::Completed);
            assert_eq!(result.content, "real");
        }
        other => panic!("primary must receive its terminal, got {other:?}"),
    }

    // The duplicate, AFTER the terminal was committed coordinator-side.
    // The real runtime consults its bounded recently-terminal history —
    // item 4's rule — and refuses the submission without a dispatch.
    let w2 = facade
        .submit_call(
            far,
            "t2".to_string(),
            "intent-tbr".to_string(),
            ToolOrigin::new("mcp:tbr"),
            call("c2", "echo", r#"{"text":"dup-never-runs"}"#),
            g_tbr, 1,
        )
        .expect("duplicate still enqueues honestly");
    assert!(!w2.is_primary(), "a retired intent admits no new primary");
    match w2.wait_deadline(Instant::now() + EVENT_WAIT) {
        Some(SubmissionOutcome::Rejected { ticket, reason }) => {
            assert_eq!(ticket, "t2", "the duplicate's own ticket returns to its own waiter");
            assert!(
                reason.contains("already terminal"),
                "the recently-terminal admission rule must fire: {reason}"
            );
        }
        other => panic!("duplicate must receive its rejection, got {other:?}"),
    }

    // The terminal retired ONLY the primary: the rejection never reached
    // the retired waiter, and both tickets were conserved on their own
    // paths — nothing quarantined, exactly the one hard diagnostic a
    // forced secondary earns.
    assert!(
        w1.wait_deadline(Instant::now() + Duration::from_millis(200)).is_none(),
        "the retired primary must not receive the duplicate's rejection"
    );
    assert!(facade.take_quarantine().is_empty());
    let diagnostics = facade.diagnostics();
    assert_eq!(
        diagnostics,
        vec![awili_engine_worker::mcp_coordinator::CoordinatorDiagnostic::SecondarySubmission {
            intent_id: "intent-tbr".to_string(),
            submission_id: awili_tools::runtime::SubmissionId(1),
        }],
        "exactly the secondary diagnostic — no other violation"
    );
    drop(facade);
    drop(coord);
}

/// `duplicate_rejection_before_terminal_leaves_the_primary_live` — the
/// opposite order over the real runtime: the duplicate is rejected while
/// the original is still pending (`pending` admission), and the rejection
/// neither overwrites nor retires the primary's waiter — the original
/// still gets its single terminal afterwards.
#[test]
fn duplicate_rejection_before_terminal_leaves_the_primary_live() {
    let catalog = McpServerCatalog::new().with_stdio(
        ToolOrigin::new("mcp:hang"),
        spec(&[("MCP_FAULT", "hang")]),
    );
    let mut config = test_config();
    config.call_timeout = Duration::from_secs(60);
    config.shutdown_grace = Duration::from_millis(300);
    let handle = spawn_mcp_runtime::<String>(catalog, config);
    let coord = awili_engine_worker::mcp_coordinator::McpCoordinator::new(handle);
    let facade = coord.facade();
    let far = Instant::now() + Duration::from_secs(10);
    facade
.connect_origin(far, ToolOrigin::new("mcp:hang")).map(|_| ()).expect("connect");
    let g_hang = coord_bring_up(&facade, "mcp:hang", 1);

    let w1 = facade
        .submit_call(
            far,
            "t1".to_string(),
            "intent-dbl".to_string(),
            ToolOrigin::new("mcp:hang"),
            call("c1", "echo", r#"{"text":"x"}"#),
            g_hang, 1,
        )
        .expect("primary enqueues");
    // FIRED-PROOF the hang: no terminal within the quiet window.
    assert!(
        w1.wait_deadline(Instant::now() + Duration::from_millis(600)).is_none(),
        "first call completed — the hang fault did not fire"
    );

    // The duplicate, in-flight: the real runtime's pending-intent
    // admission rejects it without a dispatch.
    let w2 = facade
        .submit_call(
            far,
            "t2".to_string(),
            "intent-dbl".to_string(),
            ToolOrigin::new("mcp:hang"),
            call("c2", "echo", r#"{"text":"x"}"#),
            g_hang, 1,
        )
        .expect("duplicate still enqueues honestly");
    match w2.wait_deadline(Instant::now() + EVENT_WAIT) {
        Some(SubmissionOutcome::Rejected { ticket, reason }) => {
            assert_eq!(ticket, "t2");
            assert!(
                reason.contains("duplicate"),
                "the in-flight admission rule must fire: {reason}"
            );
        }
        other => panic!("duplicate must receive its rejection, got {other:?}"),
    }

    // The primary is LIVE and untouched by the rejection.
    assert!(
        w1.wait_deadline(Instant::now() + Duration::from_millis(200)).is_none(),
        "the rejection must not have touched the primary's waiter"
    );

    // Cancel resolves the hanging original: its ONE terminal, Uncertain —
    // the call was in flight when cancelled.
    facade
        .submit_cancel(far, "intent-dbl".to_string())
        .expect("cancel enqueues");
    match w1.wait_deadline(Instant::now() + EVENT_WAIT) {
        Some(SubmissionOutcome::Terminal { ticket, disposition, .. }) => {
            assert_eq!(ticket, "t1");
            assert_eq!(disposition, CallDisposition::InFlightUnresolved);
        }
        other => panic!("primary must receive its terminal, got {other:?}"),
    }
    assert!(facade.take_quarantine().is_empty());
    drop(facade);
    drop(coord);
}

/// Lifecycle ingestion runs CONTINUOUSLY on the pump — while a call
/// blocks in flight: `ServerConnected` (registry view + surfaced
/// tools_dropped problem), `RootsUpdateIgnored` (traced + recorded), and
/// `ServerDied` are all ingested while the primary waiter is still
/// pending, and the blocked call still resolves afterwards (r2
/// acceptance 9's continuously-handled lifecycle).
#[test]
fn lifecycle_ingestion_continues_while_a_call_blocks() {
    let catalog = McpServerCatalog::new()
        .with_stdio(
            ToolOrigin::new("mcp:hang"),
            spec(&[("MCP_FAULT", "hang")]),
        )
        .with_stdio(
            ToolOrigin::new("mcp:crash"),
            spec(&[("MCP_FAULT", "crash_on_call")]),
        );
    let mut config = test_config();
    config.call_timeout = Duration::from_secs(60);
    config.shutdown_grace = Duration::from_millis(300);
    let handle = spawn_mcp_runtime::<String>(catalog, config);
    let coord = awili_engine_worker::mcp_coordinator::McpCoordinator::new(handle);
    let facade = coord.facade();
    let far = Instant::now() + Duration::from_secs(10);
    let hang = ToolOrigin::new("mcp:hang");
    let crash = ToolOrigin::new("mcp:crash");
    let g_hang = coord_bring_up(&facade, "mcp:hang", 5);
    let g_crash = coord_bring_up(&facade, "mcp:crash", 5);

    // The blocked call: hangs in flight on mcp:hang.
    let w1 = facade
        .submit_call(
            far,
            "t1".to_string(),
            "intent-lc".to_string(),
            hang.clone(),
            call("c1", "echo", r#"{"text":"x"}"#),
            g_hang, 5,
        )
        .expect("primary enqueues");

    // A concurrent call on mcp:crash dies mid-flight: the pump must ingest
    // its ServerDied while the first call blocks.
    let w2 = facade
        .submit_call(
            far,
            "t2".to_string(),
            "intent-lc2".to_string(),
            crash.clone(),
            call("c2", "echo", r#"{"text":"x"}"#),
            g_crash, 5,
        )
        .expect("crash call enqueues");
    match w2.wait_deadline(Instant::now() + EVENT_WAIT) {
        Some(SubmissionOutcome::Terminal {
            ticket,
            disposition,
            ..
        }) => {
            assert_eq!(ticket, "t2");
            assert_eq!(disposition, CallDisposition::InFlightUnresolved);
        }
        other => panic!("crashed call must receive its terminal, got {other:?}"),
    }

    // A STALE roots publish while the first call still blocks: the runtime
    // refuses it loudly; the pump ingests + traces the refusal.
    facade
        .publish_accepted_roots(far, hang.clone(), awili_engine::generation::GenerationAllocator::new().next(), 3)
        .expect("stale publish still enqueues");

    // All three lifecycle facts land in the coordinator's ingested view
    // WHILE the primary is still blocked.
    let deadline = Instant::now() + EVENT_WAIT;
    loop {
        let lifecycle = facade.lifecycle();
        let hang_connected = matches!(
            lifecycle.origins.get("mcp:hang"),
            Some(awili_engine_worker::mcp_coordinator::OriginStatus::Ready { .. })
        );
        let crash_died = matches!(
            lifecycle.origins.get("mcp:crash"),
            Some(awili_engine_worker::mcp_coordinator::OriginStatus::Dead { .. })
        );
        let ignored = lifecycle
            .ignored_roots_updates
            .iter()
            .any(|u| u.origin == "mcp:hang" && u.stale_epoch == 3 && u.current_epoch == 5);
        if hang_connected && crash_died && ignored {
            break;
        }
        assert!(Instant::now() < deadline, "lifecycle not fully ingested: {lifecycle:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        w1.wait_deadline(Instant::now() + Duration::from_millis(200)).is_none(),
        "the lifecycle facts must be ingested while the call still blocks"
    );

    // And the blocked call still resolves normally afterwards.
    facade
        .submit_cancel(far, "intent-lc".to_string())
        .expect("cancel enqueues");
    match w1.wait_deadline(Instant::now() + EVENT_WAIT) {
        Some(SubmissionOutcome::Terminal { ticket, disposition, .. }) => {
            assert_eq!(ticket, "t1");
            assert_eq!(disposition, CallDisposition::InFlightUnresolved);
        }
        other => panic!("blocked call must receive its terminal, got {other:?}"),
    }
    assert!(facade.take_quarantine().is_empty());
    drop(facade);
    drop(coord);
}
