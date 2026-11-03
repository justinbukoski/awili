// Unit 4 item 4 — the standalone, bare-name-addressable duplicate
// admission test (the item-2 lesson). `cargo test
// terminal_processed_before_queued_duplicate_still_rejects_the_duplicate
// -- --exact` must RUN this test; an in-module `#[test]` would only be
// addressable under its full `mcp_runtime::tests::…` path, and this
// bin-only package has no lib target an integration test could link.
// main.rs therefore `include!`s this file at the crate root, where the
// test's full name IS its bare name. (Plain comments because included
// items admit no inner `//!` docs; `Duration`/`Arc` come from main.rs.)

use std::sync::atomic::Ordering::SeqCst;
use std::time::Instant;

use awili_tools::runtime::{
    CallDisposition, McpCommand, McpEvent, SubmissionId,
};
use awili_tools::ToolOrigin;

use crate::mcp_runtime::{spawn_mcp_runtime, LoopSendProbe};
use crate::mcp_runtime::tests::{
    call, next_event, publish, spec, submit, test_config, EVENT_WAIT,
};
use crate::mcp_transport::McpServerCatalog;

/// Unit 4 r6 Delta B″ — the r5 counterexample ordering, FORCED. The
/// original and a duplicate are both enqueued; the runtime processes
/// the original's completion and emits its single `CallFinished`
/// BEFORE consuming the queued duplicate. Under the old pending-only
/// admission rule the duplicate was admitted as a FRESH CALL (two
/// dispatches, two terminals); the strengthened rule rejects it via
/// the same bounded tombstone that absorbs late cancels.
///
/// Determinism (no select-race): the event channel is wedged so the
/// loop is parked mid-send at each arbitration point, and at every
/// post-unwedge select poll exactly ONE branch is ready:
/// 1. two stale publishes wedge the loop mid-`RootsUpdateIgnored`
///    send while the slow (500ms) original completes un-joined;
/// 2. draining the wedge lets the loop finish that send, then commit
///    to the ONLY ready branch — the join (the duplicate is not yet
///    submitted) — which parks it again inside the blocked
///    `CallFinished` send;
/// 3. the duplicate is enqueued NOW: the loop is committed to the
///    join arm and cannot consume commands, so it queues;
/// 4. draining again releases the terminal, and the loop's next poll
///    has exactly one ready branch — the queued duplicate — consumed
///    with `pending` empty and the intent in `recently_terminal`.
#[test]
fn terminal_processed_before_queued_duplicate_still_rejects_the_duplicate() {
    let probe = std::sync::Arc::new(LoopSendProbe::default());
    let catalog = McpServerCatalog::new().with_stdio(
        ToolOrigin::new("mcp:slow"),
        spec(&[("MCP_FAULT", "slow"), ("MCP_DELAY_MS", "500")]),
    );
    let mut config = test_config();
    config.command_capacity = 8;
    config.event_capacity = 1; // one event slot — the wedge lever
    config.send_probe = Some(std::sync::Arc::clone(&probe));
    let handle = spawn_mcp_runtime::<String>(catalog, config);
    let (wedge_g, _obs) = crate::mcp_runtime::tests::bring_up(&handle, "mcp:slow", 1);
    // Capacity-1 wedge: the acceptance's RootsAccepted marker must not
    // occupy the slot assigned to Ignored #1.
    crate::mcp_runtime::tests::drain_until_roots_accepted(&handle);

    // Original: enqueued, dispatched; its echo reply takes 500ms.
    submit(
        &handle,
        "t1",
        "mcp:slow",
        call("c1", "echo", r#"{"text":"real"}"#),
    );

    // Wedge: Ignored #1 fills the single event slot; Ignored #2 parks
    // the loop mid-send (probe-positively — `blocked_on_full`).
    publish(&handle, "mcp:slow", wedge_g, 0); // stale → Ignored #1
    publish(&handle, "mcp:slow", wedge_g, 0); // stale → Ignored #2 blocks
    let deadline = Instant::now() + Duration::from_secs(10);
    while probe.blocked_on_full.load(SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "loop never wedged: completed={}",
            probe.completed.load(SeqCst)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The original completes un-joined while the loop is parked.
    std::thread::sleep(Duration::from_millis(900));

    // Unwedge #1: the loop finishes the Ignored #2 send, then commits
    // to the only ready branch (the join — the duplicate is not yet
    // enqueued), processes the terminal, and parks inside the blocked
    // CallFinished send. FIRED-PROOF: `completed == 2` positively
    // observes the Ignored #2 send finishing; the settle sleep covers
    // the deschedule window between that increment and the loop's
    // select poll (no await point sits in between).
    let first = next_event(&handle, EVENT_WAIT);
    assert!(matches!(first, McpEvent::RootsUpdateIgnored { .. }));
    let deadline = Instant::now() + Duration::from_secs(10);
    while probe.completed.load(SeqCst) < 2 {
        assert!(Instant::now() < deadline, "Ignored #2 send never finished");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(200));

    // The duplicate: enqueued while the loop is committed to the join
    // arm (parked in the CallFinished send) — it QUEUES and cannot be
    // consumed until the terminal is emitted.
    let dup = McpCommand::CallTool {
        ticket: "t2".to_string(),
        intent_id: "intent-t1".to_string(),
        submission_id: SubmissionId(7),
        origin: ToolOrigin::new("mcp:slow"),
        call: call("c2", "echo", r#"{"text":"dup-never-runs"}"#),
        generation: wedge_g,
        root_epoch: 1,
    };
    handle.submit(dup).expect("duplicate enqueues");

    // Unwedge #2: the original's ONE CallFinished is emitted — only
    // after the duplicate was queued — then the loop consumes the
    // queued duplicate with `pending` empty.
    let second = next_event(&handle, EVENT_WAIT);
    assert!(matches!(second, McpEvent::RootsUpdateIgnored { .. }));
    let finished = next_event(&handle, EVENT_WAIT);
    let McpEvent::CallFinished {
        ticket,
        intent_id,
        disposition,
        result,
        ..
    } = finished
    else {
        panic!("expected the original's CallFinished, got {finished:?}");
    };
    assert_eq!(ticket, "t1");
    assert_eq!(intent_id, "intent-t1");
    assert_eq!(disposition, CallDisposition::Completed);
    assert_eq!(result.content, "real");

    // The queued duplicate: rejected via the recently-terminal
    // tombstone — its OWN ticket and SubmissionId, never a second
    // dispatch or terminal.
    let refused = next_event(&handle, EVENT_WAIT);
    let McpEvent::SubmissionRejected {
        ticket,
        intent_id,
        submission_id,
        reason,
    } = refused
    else {
        panic!("expected SubmissionRejected, got {refused:?}");
    };
    assert_eq!(ticket, "t2");
    assert_eq!(intent_id, "intent-t1");
    assert_eq!(submission_id, SubmissionId(7));
    assert!(
        reason.contains("already terminal"),
        "the recently-terminal admission rule must fire, not the in-flight one: {reason}"
    );

    // Exactly one dispatch: nothing else ever arrives — an admitted
    // duplicate would reuse the live session and surface a SECOND
    // CallFinished for t2 with its own content.
    let quiet_until = Instant::now() + Duration::from_millis(600);
    while Instant::now() < quiet_until {
        assert!(
            handle.try_recv_event().is_none(),
            "exactly one dispatch — no further events"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let drained = handle.shutdown_and_drain();
    assert!(
        drained.iter().all(|e| !matches!(
            e,
            McpEvent::CallFinished { .. }
                | McpEvent::SubmissionRejected { .. }
                | McpEvent::ServerConnected { .. }
        )),
        "both tickets already returned; nothing terminal in the drain: {drained:?}"
    );
}
