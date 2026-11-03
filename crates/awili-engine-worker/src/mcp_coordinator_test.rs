// Unit 4 item 5 — the scripted-channel coordinator tests. Included at the
// CRATE ROOT of `awili-engine-worker` so each test's bare name is its full
// path (`cargo test <name> -- --exact`); plain comments because included
// items admit no inner `//!` docs. `Duration`, `Arc`, and `Ordering` come
// from lib.rs's own crate-root imports — re-importing them here would be
// a duplicate.

use std::time::Instant;

use awili_tools::runtime::{
    CallDisposition, McpCommand, McpEvent, McpRuntimeHandle, RefusalKind, SubmissionId,
};
use awili_tools::ToolOrigin;

use crate::mcp_coordinator::{
    CancelSubmitFailure, CoordinatorDiagnostic, McpCoordinator, SubmissionOutcome,
};

/// A deliberately non-`Clone` ticket: every conservation proof below relies
/// on outcomes and refused commands coming back by MOVE — nothing copyable
/// exists on the path.
#[derive(Debug, PartialEq)]
struct Ticket(&'static str);

const FAR: Duration = Duration::from_secs(10);
const WAIT: Duration = Duration::from_secs(5);

fn origin() -> ToolOrigin {
    ToolOrigin::new("mcp:scripted")
}

fn tcall(id: &str) -> awili_engine::ToolCall {
    awili_engine::ToolCall {
        awili_call_id: awili_engine::AwiliCallId(id.to_string()),
        provider_call_id: None,
        name: "echo".into(),
        arguments: "{}".into(),
    }
}

fn result(id: &str, content: &str) -> awili_engine::events::ToolResultRecord {
    awili_engine::events::ToolResultRecord {
        awili_call_id: awili_engine::AwiliCallId(id.to_string()),
        content: content.to_string(),
        is_error: false,
    }
}

/// A coordinator over channels the TEST plays the runtime on: the test
/// holds the command receiver and the event sender, so every event and
/// every enqueue/refusal is scripted (r2 scope: coordinator tests run
/// against scripted command/event channels).
fn scripted(
    command_capacity: usize,
    event_capacity: usize,
) -> (
    McpCoordinator<Ticket>,
    crate::mcp_coordinator::McpFacade<Ticket>,
    async_channel::Receiver<McpCommand<Ticket>>,
    awili_tools::runtime::McpEventSink<Ticket>,
) {
    let (ctx, crx) = async_channel::bounded::<McpCommand<Ticket>>(command_capacity);
    let (etx, erx) = awili_tools::runtime::event_channel::<Ticket>(event_capacity);
    // Dummy "runtime thread" — the real one is the test itself.
    let join = std::thread::spawn(|| {});
    let handle = McpRuntimeHandle::from_parts(ctx, erx, join);
    let coord = McpCoordinator::new(handle);
    let facade = coord.facade();
    (coord, facade, crx, etx)
}

#[allow(clippy::result_large_err)] // mirrors submit_call
fn submit(
    facade: &crate::mcp_coordinator::McpFacade<Ticket>,
    ticket: &'static str,
    intent: &str,
    deadline: Instant,
) -> Result<
    crate::mcp_coordinator::CallWaiter<Ticket>,
    crate::mcp_coordinator::CallSubmitFailure<Ticket>,
> {
    facade.submit_call(
        deadline,
        Ticket(ticket),
        intent.to_string(),
        origin(),
        tcall(&format!("c-{ticket}")),
        awili_engine::generation::GenerationAllocator::new().next(),
        1,
    )
}

/// r5 Delta B′: a finally-refused submission unregisters synchronously
/// (the command never entered the runtime — no rejection can arrive for
/// it) and the COMPLETE command — non-`Clone` ticket included — returns to
/// the caller by move. Both refusal shapes: deadline exhaustion with the
/// channel pinned Full, and Closed.
#[test]
fn refused_submission_unregisters_synchronously_and_returns_the_ticket() {
    // DeadlineExhausted: capacity-1 command channel, occupied by a prior
    // publish that the scripted runtime never drains.
    let (coord, facade, crx, etx) = scripted(1, 8);
    facade
        .publish_accepted_roots(Instant::now() + FAR, origin(), awili_engine::generation::GenerationAllocator::new().next(), 1)
        .expect("publish occupies the slot");
    let failure = submit(&facade, "t1", "intent-refused", Instant::now() + FAR.min(Duration::from_millis(60)))
        .expect_err("deadline must be refused");
    assert_eq!(failure.kind, RefusalKind::DeadlineExhausted);
    match failure.command {
        McpCommand::CallTool {
            ticket,
            intent_id,
            submission_id,
            root_epoch,
            ..
        } => {
            assert_eq!(ticket, Ticket("t1"));
            assert_eq!(intent_id, "intent-refused");
            assert_eq!(submission_id, SubmissionId(0), "sole submitter allocates from zero");
            assert_eq!(root_epoch, 1);
        }
        other => panic!("refused command changed shape: {other:?}"),
    }
    // Synchronous cleanup: no waiter leak, no diagnostic, nothing
    // quarantined — the command never entered the runtime.
    assert_eq!(facade.live_submission_waiters(), 0, "no waiter leak");
    assert!(facade.diagnostics().is_empty(), "no rejection expected");
    assert!(facade.take_quarantine().is_empty());
    drop(crx);
    drop(etx);
    drop(facade);
    drop(coord);

    // Closed: the command channel is closed outright.
    let (coord, facade, crx, etx) = scripted(1, 8);
    crx.close();
    let failure = submit(&facade, "t2", "intent-closed", Instant::now() + FAR)
        .expect_err("closed channel must be refused");
    assert_eq!(failure.kind, RefusalKind::Closed);
    match failure.command {
        McpCommand::CallTool { ticket, .. } => assert_eq!(ticket, Ticket("t2")),
        other => panic!("refused command changed shape: {other:?}"),
    }
    assert_eq!(facade.live_submission_waiters(), 0, "no waiter leak");
    drop(etx);
    drop(facade);
    drop(coord);
}

/// r5 retirement rule + r2 stream closure: with a primary AND a secondary
/// pending, closure wakes EVERY live submission waiter — both — with the
/// typed StreamClosed failure, exactly one outcome each.
#[test]
fn stream_closure_wakes_every_live_submission_waiter() {
    let (coord, facade, _crx, etx) = scripted(8, 8);
    let w1 = submit(&facade, "t1", "intent-sc", Instant::now() + FAR).expect("primary enqueues");
    assert!(w1.is_primary());
    let w2 = submit(&facade, "t2", "intent-sc", Instant::now() + FAR).expect("secondary enqueues");
    assert!(!w2.is_primary());
    assert_eq!(facade.live_submission_waiters(), 2);

    // The scripted runtime dies: the event stream closes.
    drop(etx);
    match w1.wait_deadline(Instant::now() + WAIT) {
        Some(SubmissionOutcome::StreamClosed) => {}
        other => panic!("primary must be woken typed-failed, got {other:?}"),
    }
    match w2.wait_deadline(Instant::now() + WAIT) {
        Some(SubmissionOutcome::StreamClosed) => {}
        other => panic!("secondary must be woken typed-failed, got {other:?}"),
    }
    assert_eq!(facade.live_submission_waiters(), 0);
    assert!(facade.diagnostics().contains(&CoordinatorDiagnostic::StreamClosedWithLiveWaiters { count: 2 }));
    assert!(facade.take_quarantine().is_empty(), "no synthetic terminals, nothing retained");
    drop(facade);
    drop(coord);
}

/// r5: a secondary submission is a coordinator invariant violation in
/// normal flow — the hard diagnostic fires — yet the machinery still
/// handles it honestly: its `SubmissionRejected` routes to ITS waiter with
/// ITS ticket, and the primary's terminal path is untouched.
#[test]
fn secondary_submission_is_a_hard_diagnostic() {
    let (coord, facade, crx, etx) = scripted(8, 8);
    let w1 = submit(&facade, "t1", "intent-sec", Instant::now() + FAR).expect("primary enqueues");
    let w2 = submit(&facade, "t2", "intent-sec", Instant::now() + FAR).expect("secondary enqueues");

    // The diagnostic fired at the secondary's ENQUEUE — one enqueue
    // outcome, decided coordinator-side without any ack.
    assert!(facade.diagnostics().contains(&CoordinatorDiagnostic::SecondarySubmission {
        intent_id: "intent-sec".to_string(),
        submission_id: SubmissionId(1),
    }));

    // The scripted runtime saw two calls for one intent and rejected the
    // second, correlated by its own submission id.
    etx.send_blocking(McpEvent::SubmissionRejected {
        ticket: Ticket("t2"),
        intent_id: "intent-sec".to_string(),
        submission_id: SubmissionId(1),
        reason: "duplicate in-flight intent_id".into(),
    })
    .unwrap();
    match w2.wait_deadline(Instant::now() + WAIT) {
        Some(SubmissionOutcome::Rejected { ticket, reason }) => {
            assert_eq!(ticket, Ticket("t2"), "the duplicate's ticket returns to its own waiter");
            assert!(reason.contains("duplicate"));
        }
        other => panic!("secondary must receive its rejection, got {other:?}"),
    }

    // The primary is untouched by the rejection: it still gets the intent's
    // single terminal, ticket conserved.
    etx.send_blocking(McpEvent::CallFinished {
        ticket: Ticket("t1"),
        intent_id: "intent-sec".to_string(),
        disposition: CallDisposition::Completed,
        result: result("c-t1", "real"),
    })
    .unwrap();
    match w1.wait_deadline(Instant::now() + WAIT) {
        Some(SubmissionOutcome::Terminal { ticket, disposition, .. }) => {
            assert_eq!(ticket, Ticket("t1"));
            assert_eq!(disposition, CallDisposition::Completed);
        }
        other => panic!("primary must receive its terminal, got {other:?}"),
    }
    assert_eq!(facade.live_submission_waiters(), 0);
    assert!(facade.take_quarantine().is_empty());
    drop(crx);
    drop(etx);
    drop(facade);
    drop(coord);
}

/// r3 Delta 3, order 1 of the boundary race: the coordinator commits
/// cancel-submission FAILURE first (deadline exhausted against a pinned
/// -Full channel), waking the waiter with the typed, ticket-free failure;
/// the real terminal that arrives afterwards is QUARANTINED — retained
/// with its ticket, never honored, nothing finalized.
#[test]
fn cancel_failure_committed_first_quarantines_the_late_terminal() {
    let (coord, facade, _crx, etx) = scripted(1, 8);
    let w1 = submit(&facade, "t1", "intent-cf", Instant::now() + FAR).expect("primary enqueues");
    // The capacity-1 command channel still holds the CallTool: any cancel
    // now retries against Full until its deadline.
    let refused = facade
        .submit_cancel(Instant::now() + Duration::from_millis(80), "intent-cf".into())
        .expect_err("cancel must be refused");
    // Ticketless by construction: the type HAS no ticket field, and it is
    // not the call-refusal type — distinguishable from call refusal.
    assert_eq!(refused, CancelSubmitFailure { kind: RefusalKind::DeadlineExhausted });

    // The waiter was woken by the committed failure — proof the commit
    // happened BEFORE the terminal is sent.
    match w1.wait_deadline(Instant::now() + WAIT) {
        Some(SubmissionOutcome::CancelSubmitRefused { kind }) => {
            assert_eq!(kind, RefusalKind::DeadlineExhausted);
        }
        other => panic!("waiter must see the typed cancel failure, got {other:?}"),
    }

    // The late real terminal: quarantined, not honored.
    etx.send_blocking(McpEvent::CallFinished {
        ticket: Ticket("t1"),
        intent_id: "intent-cf".to_string(),
        disposition: CallDisposition::InFlightUnresolved,
        result: result("c-t1", "outcome unknown"),
    })
    .unwrap();
    assert!(
        w1.wait_deadline(Instant::now() + Duration::from_millis(200)).is_none(),
        "the late terminal must NOT reach the waiter"
    );
    let quarantine = facade.take_quarantine();
    assert_eq!(quarantine.len(), 1, "the late terminal is retained, never dropped");
    assert_eq!(quarantine[0].ticket, Ticket("t1"));
    assert_eq!(quarantine[0].disposition, Some(CallDisposition::InFlightUnresolved));
    assert_eq!(quarantine[0].intent_id, "intent-cf");
    assert!(facade.diagnostics().contains(&CoordinatorDiagnostic::LateTerminal {
        intent_id: "intent-cf".to_string(),
    }));
    drop(etx);
    drop(facade);
    drop(coord);
}

/// r3 Delta 3, order 2 of the boundary race: the pump commits the REAL
/// terminal first (the waiter received it), and the cancel-failure commit
/// that follows is absorbed — the terminal wins, the waiter is never
/// revised, nothing is quarantined.
#[test]
fn terminal_committed_first_beats_the_cancel_failure() {
    let (coord, facade, crx, etx) = scripted(1, 8);
    let w1 = submit(&facade, "t1", "intent-tf", Instant::now() + FAR).expect("primary enqueues");
    // Free the capacity-1 slot so the failure lever is available later.
    let consumed = crx.recv_blocking().expect("the CallTool");
    assert!(matches!(consumed, McpCommand::CallTool { .. }));

    // The real terminal commits FIRST — the waiter receiving it is the
    // proof of commit before the cancel refusal is even attempted.
    etx.send_blocking(McpEvent::CallFinished {
        ticket: Ticket("t1"),
        intent_id: "intent-tf".to_string(),
        disposition: CallDisposition::Completed,
        result: result("c-t1", "real"),
    })
    .unwrap();
    match w1.wait_deadline(Instant::now() + WAIT) {
        Some(SubmissionOutcome::Terminal { ticket, disposition, result }) => {
            assert_eq!(ticket, Ticket("t1"));
            assert_eq!(disposition, CallDisposition::Completed);
            assert_eq!(result.content, "real");
        }
        other => panic!("the real terminal must win, got {other:?}"),
    }

    // Now pin the command channel Full and let the cancel fail.
    facade
        .publish_accepted_roots(Instant::now() + FAR, origin(), awili_engine::generation::GenerationAllocator::new().next(), 9)
        .expect("publish pins the slot");
    let refused = facade
        .submit_cancel(Instant::now() + Duration::from_millis(80), "intent-tf".into())
        .expect_err("cancel must be refused");
    assert_eq!(refused, CancelSubmitFailure { kind: RefusalKind::DeadlineExhausted });

    // The committed terminal is final: no revised outcome, no quarantine,
    // and the absorbed refusal is a visible diagnostic.
    assert!(
        w1.wait_deadline(Instant::now() + Duration::from_millis(200)).is_none(),
        "a refused cancel must not revise a committed terminal"
    );
    assert!(facade.take_quarantine().is_empty());
    assert!(facade.diagnostics().contains(&CoordinatorDiagnostic::CancelRefusalAfterTerminal {
        intent_id: "intent-tf".to_string(),
        kind: RefusalKind::DeadlineExhausted,
    }));
    drop(crx);
    drop(etx);
    drop(facade);
    drop(coord);
}

/// Item-5 C5 review, finding 1 — the fast-terminal-during-submit gap,
/// FORCED. The submit thread registers provisional ownership under the
/// register-before-submit lock and only then enters `submit_until`; the
/// old code recorded `primary = Some(..)` only after the send returned, so
/// a `CallFinished` landing in that gap found `primary == None`: the pump
/// delivered the ticket, retired no submission entry, and left the submit
/// thread to misfile its own ACCEPTED submission as a secondary (false
/// hard diagnostic plus a leaked submission entry).
///
/// Determinism (no timing bet): the capacity-1 command channel is pinned
/// Full by a publish the scripted runtime refuses to drain, so after
/// registering, the submit thread is parked in `submit_until`'s Full retry
/// and provably cannot have completed its send.
/// `live_submission_waiters() == 1` positively observes that the
/// registration lock section — ownership included — has run. The terminal
/// is committed in exactly that window, before the command exists.
#[test]
fn fast_terminal_during_submit_finds_the_provisional_primary() {
    let (coord, facade, crx, etx) = scripted(1, 8);
    facade
        .publish_accepted_roots(Instant::now() + FAR, origin(), awili_engine::generation::GenerationAllocator::new().next(), 1)
        .expect("publish pins the command slot");
    assert_eq!(crx.len(), 1, "the runtime command channel is pinned Full");

    // The submit thread: registers provisionally, then spins on Full.
    let submitter = {
        let facade = facade.clone();
        #[allow(clippy::result_large_err)] // mirrors submit_call
        std::thread::spawn(move || {
            facade.submit_call(
                Instant::now() + FAR,
                Ticket("t1"),
                "intent-fast".to_string(),
                origin(),
                tcall("c1"),
                awili_engine::generation::GenerationAllocator::new().next(),
                1,
            )
        })
    };
    let fence = Instant::now() + WAIT;
    while facade.live_submission_waiters() != 1 {
        assert!(Instant::now() < fence, "the submission never registered");
        std::thread::sleep(Duration::from_millis(5));
    }

    // The gap itself: registered (primary recorded), not yet enqueued,
    // the submit thread still inside submit_until.
    etx.send_blocking(McpEvent::CallFinished {
        ticket: Ticket("t1"),
        intent_id: "intent-fast".to_string(),
        disposition: CallDisposition::Completed,
        result: result("c1", "real"),
    })
    .unwrap();
    // FENCE (bug-independent): the pump ingests events strictly in order,
    // so a lifecycle fact it has visibly ingested proves the terminal WAS
    // committed — while the submit thread is still parked.
    etx.send_blocking(McpEvent::ServerDied {
        origin: origin(),
        generation: awili_engine::generation::GenerationAllocator::new().next(),
        cause: awili_tools::runtime::DeathCause::TransportFailed,
        selected_class: None,
        detail: "fence: the pump is past the terminal".into(),
    })
    .unwrap();
    let fence = Instant::now() + WAIT;
    loop {
        let ingested = matches!(
            facade.lifecycle().origins.get("mcp:scripted"),
            Some(crate::mcp_coordinator::OriginStatus::Dead { .. })
        );
        if ingested {
            break;
        }
        assert!(Instant::now() < fence, "the pump never ingested the fence");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Free the slot: the command enters the runtime only NOW — after its
    // terminal was already committed — and the reply reports the role the
    // registration recorded, never a post-submit classification.
    let freed = crx.recv_blocking().expect("the pinned publish");
    assert!(matches!(freed, McpCommand::UpdateAcceptedRoots { .. }));
    let w1 = submitter
        .join()
        .expect("submitter thread")
        .expect("primary enqueues");
    assert!(w1.is_primary(), "the first submission stays the primary");
    match w1.wait_deadline(Instant::now() + WAIT) {
        Some(SubmissionOutcome::Terminal {
            ticket,
            disposition,
            result,
        }) => {
            assert_eq!(ticket, Ticket("t1"));
            assert_eq!(disposition, CallDisposition::Completed);
            assert_eq!(result.content, "real");
        }
        other => panic!("the fast terminal must reach the primary, got {other:?}"),
    }
    // The primary RECEIVING the terminal is the proof the pump committed
    // it — so the assertions below are ordered after that commit.
    match crx.recv_blocking().expect("the call command") {
        McpCommand::CallTool {
            ticket,
            submission_id,
            ..
        } => {
            assert_eq!(ticket, Ticket("t1"));
            assert_eq!(submission_id, SubmissionId(0));
        }
        other => panic!("expected the CallTool, got {other:?}"),
    }
    assert_eq!(
        facade.live_submission_waiters(),
        0,
        "the terminal retired the primary's own submission entry"
    );
    assert!(
        facade.diagnostics().is_empty(),
        "an accepted first submission must never be misfiled as a secondary"
    );
    assert!(facade.take_quarantine().is_empty());
    drop(crx);
    drop(etx);
    drop(facade);
    drop(coord);
}

/// Item-5 C5 review, finding 1, the analogous race — a duplicate
/// rejection arriving while the submit thread is still inside
/// `submit_until`. The old code added the submission id to `secondaries`
/// only after the send returned, so a rejection that landed first removed
/// the submission entry and left the submit thread to add stale secondary
/// state afterwards. Provisional ownership records the secondary role
/// before the command can enter the runtime, so the rejection and the
/// bookkeeping can no longer race — same determinism lever as above.
#[test]
fn fast_rejection_during_submit_retires_the_provisional_secondary() {
    let (coord, facade, crx, etx) = scripted(1, 8);
    // The primary first: enqueued into the free slot, then drained so the
    // slot can be pinned again for the duplicate.
    let w1 = submit(&facade, "t1", "intent-fst", Instant::now() + FAR).expect("primary enqueues");
    assert!(w1.is_primary());
    assert!(matches!(
        crx.recv_blocking().expect("the primary command"),
        McpCommand::CallTool { .. }
    ));
    facade
        .publish_accepted_roots(Instant::now() + FAR, origin(), awili_engine::generation::GenerationAllocator::new().next(), 1)
        .expect("publish pins the command slot");
    assert_eq!(crx.len(), 1, "the runtime command channel is pinned Full");

    let submitter = {
        let facade = facade.clone();
        #[allow(clippy::result_large_err)] // mirrors submit_call
        std::thread::spawn(move || submit(&facade, "t2", "intent-fst", Instant::now() + FAR))
    };
    // Fence: BOTH submissions are registered, the duplicate provisionally
    // as a secondary.
    let fence = Instant::now() + WAIT;
    while facade.live_submission_waiters() != 2 {
        assert!(Instant::now() < fence, "the duplicate never registered");
        std::thread::sleep(Duration::from_millis(5));
    }

    // The runtime's correlated rejection lands inside the submit window.
    etx.send_blocking(McpEvent::SubmissionRejected {
        ticket: Ticket("t2"),
        intent_id: "intent-fst".to_string(),
        submission_id: SubmissionId(1),
        reason: "duplicate in-flight intent_id; refused — never dispatched.".into(),
    })
    .unwrap();
    // FENCE (bug-independent): same ordered-ingest proof — by the time the
    // pump has ingested this lifecycle fact, the rejection is committed
    // and the duplicate is still un-enqueued.
    etx.send_blocking(McpEvent::ServerDied {
        origin: origin(),
        generation: awili_engine::generation::GenerationAllocator::new().next(),
        cause: awili_tools::runtime::DeathCause::TransportFailed,
        selected_class: None,
        detail: "fence: the pump is past the rejection".into(),
    })
    .unwrap();
    let fence = Instant::now() + WAIT;
    loop {
        let ingested = matches!(
            facade.lifecycle().origins.get("mcp:scripted"),
            Some(crate::mcp_coordinator::OriginStatus::Dead { .. })
        );
        if ingested {
            break;
        }
        assert!(Instant::now() < fence, "the pump never ingested the fence");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Free the slot: the duplicate enters the runtime only now, after its
    // rejection was already delivered to its own waiter.
    let freed = crx.recv_blocking().expect("the pinned publish");
    assert!(matches!(freed, McpCommand::UpdateAcceptedRoots { .. }));
    let w2 = submitter
        .join()
        .expect("submitter thread")
        .expect("duplicate still enqueues honestly");
    assert!(!w2.is_primary(), "a live primary admits no second primary");
    match w2.wait_deadline(Instant::now() + WAIT) {
        Some(SubmissionOutcome::Rejected { ticket, reason }) => {
            assert_eq!(ticket, Ticket("t2"), "the duplicate's ticket returns to its own waiter");
            assert!(reason.contains("duplicate"), "the in-flight admission rule must fire: {reason}");
        }
        other => panic!("duplicate must receive its rejection, got {other:?}"),
    }

    // The primary is untouched by any of it, and still gets the intent's
    // single terminal afterwards — one entry, one diagnostic, both tickets
    // conserved on their own waiters.
    assert!(
        w1.wait_deadline(Instant::now() + Duration::from_millis(200)).is_none(),
        "the rejection must not have touched the primary's waiter"
    );
    assert_eq!(facade.live_submission_waiters(), 1, "only the primary remains live");
    assert_eq!(
        facade.diagnostics(),
        vec![CoordinatorDiagnostic::SecondarySubmission {
            intent_id: "intent-fst".to_string(),
            submission_id: SubmissionId(1),
        }],
        "exactly the secondary diagnostic — no other violation"
    );
    assert!(facade.take_quarantine().is_empty());
    etx.send_blocking(McpEvent::CallFinished {
        ticket: Ticket("t1"),
        intent_id: "intent-fst".to_string(),
        disposition: CallDisposition::Completed,
        result: result("c-t1", "real"),
    })
    .unwrap();
    match w1.wait_deadline(Instant::now() + WAIT) {
        Some(SubmissionOutcome::Terminal { ticket, disposition, .. }) => {
            assert_eq!(ticket, Ticket("t1"));
            assert_eq!(disposition, CallDisposition::Completed);
        }
        other => panic!("primary must receive its terminal, got {other:?}"),
    }
    assert_eq!(facade.live_submission_waiters(), 0);
    drop(crx);
    drop(etx);
    drop(facade);
    drop(coord);
}

/// Item-5 C5 review, finding 2 — quarantine is LOSSLESS past the old
/// record cap. The former pop_front-at-cap ring destroyed the oldest
/// retained ticket at record 257, contradicting r2's binding
/// never-dropped rule. Now every ticket-bearing record stays owned by the
/// quarantine until the owner drains it; the count past the operational
/// bound is surfaced as a diagnostic instead. 300 unknown terminals — each
/// carrying a non-`Clone` ticket — must all still be there.
#[test]
fn quarantine_is_lossless_past_the_operational_bound() {
    let (coord, facade, _crx, etx) = scripted(8, 8);
    const COUNT: usize = 300;
    // 300 = 44 past the old 256 cap: the ring would have destroyed t0..t43.
    let mut minted: Vec<&'static str> = Vec::with_capacity(COUNT);
    for i in 0..COUNT {
        let ticket: &'static str = Box::leak(format!("t{i}").into_boxed_str());
        minted.push(ticket);
        etx.send_blocking(McpEvent::CallFinished {
            ticket: Ticket(ticket),
            intent_id: format!("intent-q{i}"),
            disposition: CallDisposition::NeverDispatched,
            result: result(&format!("c{i}"), "no live waiter owns this terminal"),
        })
        .unwrap();
    }
    let fence = Instant::now() + WAIT;
    while facade.quarantine_retained() != COUNT {
        assert!(
            Instant::now() < fence,
            "terminals not fully quarantined: {}",
            facade.quarantine_retained()
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let drained = facade.take_quarantine();
    assert_eq!(drained.len(), COUNT, "every ticket retained — none evicted at the cap");
    let mut retained: Vec<&'static str> = drained.iter().map(|r| r.ticket.0).collect();
    let mut expected = minted;
    retained.sort_unstable();
    expected.sort_unstable();
    assert_eq!(retained, expected, "every conserved ticket is still owned by quarantine");
    assert_eq!(facade.quarantine_retained(), 0, "draining is what removes records — nothing else");

    // The bound is surfaced, not silently exceeded — and the DIAGNOSTIC
    // ring stays bounded (it carries no ticket), unlike the quarantine.
    assert!(facade.diagnostics().contains(
        &CoordinatorDiagnostic::QuarantineOverOperationalBound { retained: 257 }
    ));
    assert!(
        facade.diagnostics().len() <= 256,
        "diagnostics stay bounded; only ticket-bearing retention is unbounded"
    );
    drop(etx);
    drop(facade);
    drop(coord);
}

/// Item-5 C5 review, finding 3 — coordinator teardown with the runtime
/// command channel pinned FULL. A best-effort `submit(Shutdown)` is
/// refused by backpressure; if that were the only teardown signal, the
/// pump would block on the event stream forever (the handle's sender
/// stays open, so the runtime waits for a next command that never comes)
/// and `McpCoordinator::drop` would never return. The teardown signal is
/// the one backpressure cannot mask: command-sender closure.
#[test]
fn teardown_completes_with_a_full_runtime_command_channel() {
    let (coord, facade, crx, etx) = scripted(1, 8);
    facade
        .publish_accepted_roots(Instant::now() + FAR, origin(), awili_engine::generation::GenerationAllocator::new().next(), 1)
        .expect("publish pins the command slot");
    assert_eq!(crx.len(), 1, "command channel pinned Full; nothing drains it");

    // The scripted "runtime": refuses to drain the queue, and closes the
    // event stream only once it POSITIVELY observes the shutdown signal.
    let runtime = std::thread::spawn(move || {
        let fence = Instant::now() + WAIT;
        while !crx.is_closed() {
            assert!(Instant::now() < fence, "coordinator never signalled shutdown");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            crx.len(),
            1,
            "closure must not have needed the full queue drained"
        );
        drop(crx);
        drop(etx); // stream closure is what unblocks the pump
    });

    // Teardown under a watchdog: the maskable-signal-only code hangs here
    // forever, which the timeout converts into a loud failure.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(facade);
        drop(coord);
        let _ = done_tx.send(());
    });
    match done_rx.recv_timeout(Duration::from_secs(20)) {
        Ok(()) => {}
        Err(_) => panic!("coordinator teardown deadlocked on a full runtime command channel"),
    }
    runtime.join().expect("scripted runtime thread");
}

/// r6 Delta B″, coordinator-logic coverage on scripted channels: the
/// pump's commit ordering when a terminal and a later correlated
/// rejection for the same intent arrive in sequence — the terminal
/// retires ONLY the primary, the rejection reaches the secondary's own
/// waiter with its own ticket, both tickets conserved, nothing
/// quarantined.
///
/// The BINDING real-runtime proof of the full interleaving — both
/// submissions genuinely enqueued in the runtime's command channel
/// before the original's completion is processed, the real loop
/// emitting the real `CallFinished` before consuming the queued
/// duplicate, the real `pending ∪ recently_terminal` admission
/// producing the rejection — is
/// `terminal_before_duplicate_rejection_retires_only_the_primary` in
/// `awili-app` (real `spawn_mcp_runtime` + real coordinator, ordered by
/// the cfg(test) runtime gate). This scripted rig remains for the
/// coordinator-side logic alone: every event below is hand-delivered
/// over channels the test plays the runtime on, which is exactly what
/// the binding proof must not be.
#[test]
fn scripted_terminal_before_rejection_retires_only_the_primary() {
    let (coord, facade, crx, etx) = scripted(4, 8);
    let w1 = submit(&facade, "t1", "intent-qd", Instant::now() + FAR).expect("primary enqueues");
    assert!(w1.is_primary());
    let w2 = submit(&facade, "t2", "intent-qd", Instant::now() + FAR)
        .expect("duplicate still enqueues honestly");
    assert!(!w2.is_primary());
    // The r6 precondition, positively observed: BOTH submissions are
    // queued and NEITHER has been consumed.
    assert_eq!(
        crx.len(),
        2,
        "original and duplicate both queued, neither consumed"
    );

    // The runtime dispatches ONLY the original…
    match crx.recv_blocking().expect("the original") {
        McpCommand::CallTool {
            ticket,
            submission_id,
            ..
        } => {
            assert_eq!(ticket, Ticket("t1"));
            assert_eq!(submission_id, SubmissionId(0));
        }
        other => panic!("expected the original CallTool, got {other:?}"),
    }
    // …its completion is processed and its ONE CallFinished emitted —
    // while the duplicate is still sitting in the command queue.
    etx.send_blocking(McpEvent::CallFinished {
        ticket: Ticket("t1"),
        intent_id: "intent-qd".to_string(),
        disposition: CallDisposition::Completed,
        result: result("c-t1", "real"),
    })
    .unwrap();
    // The primary RECEIVING it is the proof the coordinator committed the
    // terminal before the queued duplicate is consumed.
    match w1.wait_deadline(Instant::now() + WAIT) {
        Some(SubmissionOutcome::Terminal {
            ticket,
            disposition,
            result,
        }) => {
            assert_eq!(ticket, Ticket("t1"));
            assert_eq!(disposition, CallDisposition::Completed);
            assert_eq!(result.content, "real");
        }
        other => panic!("primary must receive its terminal, got {other:?}"),
    }
    assert_eq!(
        facade.live_submission_waiters(),
        1,
        "the terminal retired ONLY the primary"
    );

    // NOW the runtime consumes the already-queued duplicate — with
    // `pending` empty and the intent in the recent-terminal tombstone,
    // item 4's admission rule produces the correlated rejection.
    match crx.recv_blocking().expect("the queued duplicate") {
        McpCommand::CallTool {
            ticket,
            submission_id,
            ..
        } => {
            assert_eq!(ticket, Ticket("t2"));
            assert_eq!(submission_id, SubmissionId(1));
        }
        other => panic!("expected the duplicate CallTool, got {other:?}"),
    }
    etx.send_blocking(McpEvent::SubmissionRejected {
        ticket: Ticket("t2"),
        intent_id: "intent-qd".to_string(),
        submission_id: SubmissionId(1),
        reason: "duplicate intent_id: already terminal (bounded recent-terminal \
                 history); refused — never dispatched."
            .into(),
    })
    .unwrap();
    match w2.wait_deadline(Instant::now() + WAIT) {
        Some(SubmissionOutcome::Rejected { ticket, reason }) => {
            assert_eq!(ticket, Ticket("t2"), "the duplicate's ticket returns to its own waiter");
            assert!(
                reason.contains("already terminal"),
                "the recently-terminal admission rule must fire: {reason}"
            );
        }
        other => panic!("duplicate must receive its rejection, got {other:?}"),
    }

    // The terminal retired ONLY the primary: the retired waiter receives
    // nothing further, both tickets were conserved on their own waiters,
    // and exactly the one hard diagnostic a forced secondary earns.
    assert!(
        w1.wait_deadline(Instant::now() + Duration::from_millis(200)).is_none(),
        "the retired primary must not receive the duplicate's rejection"
    );
    assert_eq!(facade.live_submission_waiters(), 0);
    assert!(facade.take_quarantine().is_empty());
    assert_eq!(
        facade.diagnostics(),
        vec![CoordinatorDiagnostic::SecondarySubmission {
            intent_id: "intent-qd".to_string(),
            submission_id: SubmissionId(1),
        }],
        "exactly the secondary diagnostic — no other violation"
    );
    drop(crx);
    drop(etx);
    drop(facade);
    drop(coord);
}

/// RC2-ENG4 (F0658): once the pump has woken every waiter at stream
/// closure, a later submission is refused as Closed — command and ticket
/// back by move — instead of registering a waiter nothing would ever wake.
#[test]
fn rc2eng4_a_submission_after_stream_closure_is_refused() {
    let (coord, facade, crx, etx) = scripted(8, 8);
    let first = submit(&facade, "t1", "intent-1", Instant::now() + FAR).expect("enqueued");
    drop(etx); // the event stream closes
    match first.wait_deadline(Instant::now() + WAIT) {
        Some(SubmissionOutcome::StreamClosed) => {}
        other => panic!("closure must wake the live waiter, got {other:?}"),
    }
    // The scripted runtime still takes commands (crx is alive), so before
    // RC2-ENG4 this enqueued and returned a waiter that never resolves.
    let failure = submit(&facade, "t2", "intent-2", Instant::now() + FAR)
        .expect_err("a submission after closure must be refused");
    assert_eq!(failure.kind, RefusalKind::Closed);
    match failure.command {
        McpCommand::CallTool {
            ticket, intent_id, ..
        } => {
            assert_eq!(ticket, Ticket("t2"));
            assert_eq!(intent_id, "intent-2");
        }
        other => panic!("refused command changed shape: {other:?}"),
    }
    assert_eq!(facade.live_submission_waiters(), 0, "no waiter left behind");
    drop(crx);
    drop(facade);
    drop(coord);
}

/// RC2-ENG4 (F0659): lifecycle events from several sinks can arrive out of
/// order; an older generation's event never overwrites a newer one, and a
/// late step of one generation never undoes its death.
#[test]
fn rc2eng4_a_stale_lifecycle_event_never_moves_an_origin_back() {
    use crate::mcp_coordinator::OriginStatus;
    let (coord, facade, crx, etx) = scripted(8, 16);
    let mut alloc = awili_engine::generation::GenerationAllocator::new();
    let g1 = alloc.next();
    let g2 = alloc.next();
    let died = |generation| McpEvent::ServerDied {
        origin: origin(),
        generation,
        cause: awili_tools::runtime::DeathCause::TransportFailed,
        selected_class: None,
        detail: "late".into(),
    };
    // A fence on another origin: the pump ingests in order, so once the
    // fence is visible every event sent before it was ingested.
    let fence = |n: u64| {
        let fence_origin = ToolOrigin::new(format!("mcp:fence-{n}"));
        etx.send_blocking(McpEvent::OriginDisconnected {
            origin: fence_origin.clone(),
        })
        .unwrap();
        let until = Instant::now() + WAIT;
        while !facade
            .lifecycle()
            .origins
            .contains_key(&fence_origin.origin)
        {
            assert!(Instant::now() < until, "the pump never ingested the fence");
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    etx.send_blocking(McpEvent::OriginConnecting {
        origin: origin(),
        generation: g2,
    })
    .unwrap();
    etx.send_blocking(died(g1)).unwrap();
    etx.send_blocking(McpEvent::OriginConnecting {
        origin: origin(),
        generation: g1,
    })
    .unwrap();
    etx.send_blocking(McpEvent::OriginDisconnected { origin: origin() })
        .unwrap();
    fence(1);
    assert_eq!(
        facade.lifecycle().origins.get("mcp:scripted"),
        Some(&OriginStatus::Connecting { generation: g2 }),
        "g1's late events never replace g2"
    );
    // Within g2: the death stands against a late "connecting".
    etx.send_blocking(died(g2)).unwrap();
    etx.send_blocking(McpEvent::OriginConnecting {
        origin: origin(),
        generation: g2,
    })
    .unwrap();
    fence(2);
    assert!(
        matches!(
            facade.lifecycle().origins.get("mcp:scripted"),
            Some(OriginStatus::Dead { generation, .. }) if *generation == g2
        ),
        "{:?}",
        facade.lifecycle().origins.get("mcp:scripted")
    );
    drop(etx);
    drop(crx);
    drop(facade);
    drop(coord);
}

/// RC2-ENGF4 (RC2-ENG slice review P3): roots acceptances for one ready
/// generation can arrive out of order; a late, LOWER epoch never moves the
/// reported epoch back (before RC2-ENGF4 the last one to arrive won). A
/// higher one still advances it.
#[test]
fn rc2engf4_a_late_roots_acceptance_never_lowers_the_epoch() {
    use crate::mcp_coordinator::OriginStatus;
    let (coord, facade, crx, etx) = scripted(8, 16);
    let record = awili_tools::attest::SetupRecordFacts {
        origin: origin(),
        setup_id: awili_tools::attest::SetupIdentityId::parse("setup-1").unwrap(),
        pinned_requirement: "cdhash H\"00\"".into(),
        expected_protocol_name: "srv".into(),
        launch_kind: awili_tools::attest::LaunchKind::Direct,
        program: "/usr/local/lib/awili/srv".into(),
    };
    let g = awili_engine::generation::GenerationAllocator::new().next();
    let token = awili_tools::attest::LaunchToken::mint_verified(&record, g);
    etx.emit_server_connected_blocking(token, "srv".into(), Vec::new(), Vec::new(), Some(1))
        .unwrap();
    let accepted = |epoch| McpEvent::RootsAccepted {
        origin: origin(),
        generation: g,
        epoch,
    };
    // The pump ingests in order: once a fence on another origin is
    // visible, every event sent before it was ingested.
    let settle = |n: u64| {
        let fence = ToolOrigin::new(format!("mcp:fence-{n}"));
        etx.send_blocking(McpEvent::OriginDisconnected {
            origin: fence.clone(),
        })
        .unwrap();
        let until = Instant::now() + WAIT;
        while !facade.lifecycle().origins.contains_key(&fence.origin) {
            assert!(Instant::now() < until, "the pump never ingested the fence");
            std::thread::sleep(Duration::from_millis(2));
        }
        match facade.lifecycle().origins.get("mcp:scripted") {
            Some(OriginStatus::Ready {
                generation,
                accepted_epoch,
                ..
            }) if *generation == g => *accepted_epoch,
            other => panic!("expected Ready for {g}, got {other:?}"),
        }
    };
    etx.send_blocking(accepted(2)).unwrap();
    etx.send_blocking(accepted(1)).unwrap();
    assert_eq!(settle(1), 2, "epoch 2, then a late 1: still 2");
    etx.send_blocking(accepted(3)).unwrap();
    assert_eq!(settle(2), 3, "a newer acceptance still advances it");
    drop(etx);
    drop(crx);
    drop(facade);
    drop(coord);
}
