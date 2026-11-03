// Unit 4 item 6 — the executor seam's binding tests. Included at the CRATE
// ROOT of `awili-engine-worker` (item 5's precedent) so each test's bare name
// IS its full path: `cargo test <name> -- --exact` addresses exactly one.
//
// They live in THIS crate, not in awili-seam, because the binding cases have to
// traverse the production path — a real `McpCoordinator` over scripted runtime
// channels, the real `McpFacade<IntentTicket>`, the real `McpExecutor` adapter,
// and the seam's real `run_turn`/`dispatch_mcp`. A fake implementing
// `McpExecutor` proves the seam talks to itself; it cannot prove the
// coordinator traced a stale roots update or that a refused submit returned its
// ticket. Plain comments because included items admit no inner `//!` docs.

// Imports are deliberately thin: this file is INCLUDED at the crate root, so
// it shares lib.rs's and mcp_coordinator_test.rs's imports (`Arc`, `Duration`,
// `Instant`, `AgentEvent`, `ExecutionLog`, `RiskClass`, the seam's `run_turn`
// and friends, and the runtime vocabulary). Re-importing any of them is a
// duplicate-definition error, not a style choice.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Mutex;

use awili_engine::events::ToolResultRecord;
use awili_engine_log::{IntentTicket, MemLog};
use awili_seam::{bring_up_roots, HostRootProposal, McpDispatchFailure, RootsPolicy};

use crate::mcp_coordinator::McpFacade;

// ---------------------------------------------------------------------------
// The scripted runtime: a real coordinator whose runtime side is this test
// ---------------------------------------------------------------------------

type Cmds = async_channel::Receiver<McpCommand<IntentTicket>>;
type Events = awili_tools::runtime::McpEventSink<IntentTicket>;

// A real McpCoordinator over channels the TEST plays the runtime on (r2's
// "scripted command/event channels through the coordinator"). Returns the
// coordinator, its production facade, the command receiver, and the event
// sender.
//
// DROP ORDER MATTERS: the coordinator's `Drop` orders shutdown and then waits
// for the event stream to close, which only happens once the test's event
// SENDER is gone. Bind it first — `let (_coord, facade, cmds, events)` — and
// let scope end drop them in reverse. An explicit `drop(coord)` while the
// channels are still alive deadlocks the shutdown.
fn scripted_runtime(
    command_capacity: usize,
) -> (
    McpCoordinator<IntentTicket>,
    McpFacade<IntentTicket>,
    Cmds,
    Events,
) {
    let (ctx, crx) = async_channel::bounded::<McpCommand<IntentTicket>>(command_capacity);
    let (etx, erx) = awili_tools::runtime::event_channel::<IntentTicket>(16);
    let join = std::thread::spawn(|| {});
    let handle = McpRuntimeHandle::from_parts(ctx, erx, join);
    let coord = McpCoordinator::new(handle);
    let facade = coord.facade();
    (coord, facade, crx, etx)
}

// What the scripted runtime observed for one CallTool submission.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SeenCall {
    intent_id: String,
    origin: String,
    tool: String,
    // The seam-minted awili_call_id. A terminal MUST echo this one: the
    // reducer keys tool results by it, and the SSE round's "call_p" is the
    // PROVIDER id, which is a different namespace.
    awili_call_id: String,
    root_epoch: u64,
    submission_id: SubmissionId,
}

// Block for the next CallTool the seam submits, returning what the runtime
// saw plus the ticket it now owns (by move, exactly as the runtime owns it).
fn take_call(cmds: &Cmds) -> (SeenCall, IntentTicket) {
    loop {
        match cmds.recv_blocking().expect("runtime command channel closed") {
            McpCommand::CallTool {
                ticket,
                intent_id,
                submission_id,
                origin,
                call,
                generation: _,
                root_epoch,
            } => {
                return (
                    SeenCall {
                        intent_id,
                        origin: origin.origin,
                        tool: call.name,
                        awili_call_id: call.awili_call_id.0,
                        root_epoch,
                        submission_id,
                    },
                    ticket,
                )
            }
            // Roots publications and cancels are drained by the cases that
            // care; skip anything else while waiting for the call.
            _ => continue,
        }
    }
}

// ---------------------------------------------------------------------------
// The seam turn harness (mirrors awili-seam's own suite, kept minimal)
// ---------------------------------------------------------------------------

struct Script {
    bodies: VecDeque<String>,
}

impl Transport for Script {
    fn stream(&mut self, _request_body: &str) -> Result<String, SeamError> {
        self.bodies
            .pop_front()
            .ok_or_else(|| SeamError::Transport("script exhausted".into()))
    }
}

fn chunk(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

fn sse_tool_round(id: &str, name: &str, args: &str) -> String {
    let args_esc = serde_json::to_string(args).unwrap();
    let mut out = chunk(&format!(
        r#"{{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{{"index":0,"delta":{{"role":"assistant","content":"","tool_calls":[{{"id":"{id}","index":0,"type":"function","function":{{"name":"{name}","arguments":{args_esc}}}}}]}},"finish_reason":null}}]}}"#
    ));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"tool_calls"}]}"#,
    ));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":10,"total_tokens":20}}"#,
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

fn sse_text_final(text: &str) -> String {
    let text_esc = serde_json::to_string(text).unwrap();
    let mut out = chunk(&format!(
        r#"{{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{{"index":0,"delta":{{"role":"assistant","content":{text_esc}}},"finish_reason":null}}]}}"#
    ));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"stop"}]}"#,
    ));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

// An mcp:* tool is risk-classified privileged, so the turn raises a dialog.
// This host approves once and can be flipped to "cancelled" by the scripted
// runtime — which is what makes cancellation-during-a-blocked-call
// deterministic instead of timing-dependent.
struct SeamHost {
    cancel: Arc<AtomicBool>,
    events: Vec<AgentEvent>,
    /// RC1-O-FIX7: the calls the seam said were dispatching.
    dispatching: Vec<String>,
}

impl TurnHost for SeamHost {
    fn approve(&mut self, _call_id: &str, _prompt: &str, _risk: RiskClass) -> ApprovalOutcome {
        ApprovalOutcome::Verdict(UserVerdict::Approve {
            scope: "once".into(),
        })
    }
    fn observe(&mut self, event: &AgentEvent) -> Result<(), awili_seam::ObserverGone> {
        self.events.push(event.clone());
        Ok(())
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(AtomicOrdering::SeqCst)
    }
    fn tool_dispatching(&mut self, call_id: &str, _floor: RiskClass) {
        self.dispatching.push(call_id.to_string());
    }
}

fn mcp_tool() -> awili_tools::ToolDef {
    awili_tools::ToolDef::from_parts(
        awili_tools::ToolOrigin::new("mcp:probe"),
        "probe.ping",
        awili_tools::CanonicalDescriptor {
            schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            side_effect: "read_only".into(),
            required_capabilities: vec![],
            executor: "mcp:probe".into(),
        },
    )
}

struct SeamRun {
    result: Result<awili_seam::TurnReport, SeamError>,
    records: Vec<awili_engine_log::schema::LogRecordV1>,
}

// Drive ONE real turn through the seam against `executors`/`roots`, calling
// `probe.ping` (or nothing, when `tool_round` is false).
fn run_seam_turn(
    executors: &Executors,
    roots: &RootsAcceptance,
    cancel: Arc<AtomicBool>,
    tool_round: bool,
) -> (SeamRun, Vec<AgentEvent>) {
    let (run, events, _) = run_seam_turn_traced(executors, roots, cancel, tool_round);
    (run, events)
}

/// RC1-O-FIX7: [`run_seam_turn`], also returning the calls the seam said were
/// dispatching (`TurnHost::tool_dispatching`).
fn run_seam_turn_traced(
    executors: &Executors,
    roots: &RootsAcceptance,
    cancel: Arc<AtomicBool>,
    tool_round: bool,
) -> (SeamRun, Vec<AgentEvent>, Vec<String>) {
    let host = authority().expect("worker authority");
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-item6").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let bodies = if tool_round {
        vec![
            sse_tool_round("call_p", "probe.ping", "{}"),
            sse_text_final("done"),
        ]
    } else {
        vec![sse_text_final("done")]
    };
    let mut transport = Script {
        bodies: bodies.into(),
    };
    let mut host_io = SeamHost {
        cancel,
        events: Vec::new(),
        dispatching: Vec::new(),
    };
    let extra = mcp_tool();
    let mut history = ContextHistory::new();
    let result = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "ping the probe",
        &mut host_io,
        &mut log,
        &mut grants,
        std::slice::from_ref(&extra),
        executors,
        &awili_engine_authz::NoInstallAuthority,
        roots,
        awili_seam::DEFAULT_WINDOW_TOKENS,
        "turn-item6-0",
        &mut history,
    );
    let (records, _) = log.read_records().unwrap();
    (
        SeamRun { result, records },
        host_io.events,
        host_io.dispatching,
    )
}

struct AllowRoots;
impl RootsPolicy for AllowRoots {
    fn accepts(
        &self,
        _v: &awili_tools::attest::VerifiedLaunch,
        _c: &awili_tools::runtime::ConnectedLaunch,
        _h: &HostRootProposal,
    ) -> Result<(), String> {
        Ok(())
    }
}

struct DenyRoots;
impl RootsPolicy for DenyRoots {
    fn accepts(
        &self,
        _v: &awili_tools::attest::VerifiedLaunch,
        _c: &awili_tools::runtime::ConnectedLaunch,
        _h: &HostRootProposal,
    ) -> Result<(), String> {
        Err("host policy refuses this root set".into())
    }
}

/// A LEGITIMATELY emitted probe observation (unit 4b): the record expects
/// "probe-server"; `claimed` is what the server said at handshake — an
/// impostor claim trips the identity check exactly as the old proposal
/// fixtures did.
fn probe_observation(
    epoch: Option<u64>,
    claimed: &str,
) -> awili_tools::runtime::ConnectedLaunch {
    probe_observation_advertising(epoch, claimed, Vec::new())
}

/// RC2-ENGB4: [`probe_observation`] with the tools the server advertised.
fn probe_observation_advertising(
    epoch: Option<u64>,
    claimed: &str,
    tools: Vec<awili_tools::runtime::AdvertisedTool>,
) -> awili_tools::runtime::ConnectedLaunch {
    use awili_tools::attest::{LaunchKind, LaunchToken, SetupIdentityId, SetupRecordFacts};
    let record = SetupRecordFacts {
        origin: awili_engine::ToolOrigin::new("mcp:probe"),
        setup_id: SetupIdentityId::parse("setup-probe").unwrap(),
        pinned_requirement: "cdhash H\"00\"".into(),
        expected_protocol_name: "probe-server".into(),
        launch_kind: LaunchKind::Direct,
        program: "/usr/local/lib/awili/probe".into(),
    };
    let mut alloc = awili_engine::generation::GenerationAllocator::new();
    let token = LaunchToken::mint_verified(&record, alloc.next());
    let (sink, stream) = awili_tools::runtime::event_channel::<IntentTicket>(4);
    let (ctx, _crx) = async_channel::bounded(1);
    let handle =
        awili_tools::runtime::McpRuntimeHandle::from_parts(ctx, stream, std::thread::spawn(|| {}));
    sink.emit_server_connected_blocking(token, claimed.into(), tools, Vec::new(), epoch)
        .expect("emit");
    // Close the stream BEFORE the handle drops: the handle's drop
    // drains events with a blocking recv, which only ends when the
    // sink is gone — and the sink, declared first, would otherwise
    // outlive the handle.
    drop(sink);
    match handle.try_recv_event() {
        Some(awili_tools::runtime::McpEvent::ServerConnected(c)) => c,
        other => panic!("expected the observation, got {other:?}"),
    }
}

fn probe_host() -> HostRootProposal {
    HostRootProposal {
        origin: awili_engine::ToolOrigin::new("mcp:probe"),
        proposed_roots: vec!["/work".into()],
    }
}

// Accept `epoch` through the REAL publication path: bring_up_roots ->
// McpExecutor::publish_roots -> facade -> coordinator -> the scripted runtime,
// which must actually receive the UpdateAcceptedRoots command.
fn accept_epoch(facade: &McpFacade<IntentTicket>, roots: &mut RootsAcceptance, epoch: u64) {
    bring_up_roots(
        facade,
        roots,
        &probe_observation(Some(epoch), "probe-server"),
        &probe_host(),
        &AllowRoots,
    )
    .expect("scripted bring-up accepts");
}

fn finalize_count(records: &[awili_engine_log::schema::LogRecordV1]) -> usize {
    records
        .iter()
        .filter(|r| {
            matches!(
                r,
                awili_engine_log::schema::LogRecordV1::DispatchFinalized { .. }
            )
        })
        .count()
}

fn logged_intent_ids(records: &[awili_engine_log::schema::LogRecordV1]) -> Vec<String> {
    records
        .iter()
        .filter_map(|r| match r {
            awili_engine_log::schema::LogRecordV1::Intent { intent_id, .. } => {
                Some(intent_id.clone())
            }
            _ => None,
        })
        .collect()
}

fn statuses(
    records: &[awili_engine_log::schema::LogRecordV1],
) -> Vec<awili_engine_log::replay::IntentStatus> {
    awili_engine_log::replay::classify(records)
        .iter()
        .flat_map(|t| t.intents.iter().map(|i| i.status.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// ITEM 6 §1 — the builtin leg is unchanged
// ---------------------------------------------------------------------------

// The builtin filesystem leg routes exactly as it did before item 6, and an
// AVAILABLE MCP executor is never consulted for a builtin origin. Proven
// against the production coordinator: nothing reaches the scripted runtime.
#[test]
fn builtin_fs_routes_unchanged() {
    let (_coord, facade, cmds, _events) = scripted_runtime(4);
    let host = authority().expect("worker authority");
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-builtin").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut transport = Script {
        bodies: vec![
            sse_tool_round("call_w", "fs.write", r#"{"path":"notes.md","contents":"hello"}"#),
            sse_text_final("done"),
        ]
        .into(),
    };
    let mut host_io = SeamHost {
        cancel: Arc::new(AtomicBool::new(false)),
        events: Vec::new(),
        dispatching: Vec::new(),
    };
    let mut history = ContextHistory::new();
    let report = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "write my notes",
        &mut host_io,
        &mut log,
        &mut grants,
        &[],
        &Executors::mcp(facade.clone()),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        awili_seam::DEFAULT_WINDOW_TOKENS,
        "turn-builtin-0",
        &mut history,
    )
    .expect("builtin turn completes");

    assert_eq!(
        std::fs::read_to_string(root.path().join("notes.md")).unwrap(),
        "hello",
        "the builtin leg still does the real work"
    );
    assert_eq!(report.final_text, "done");
    let (records, _) = log.read_records().unwrap();
    assert_eq!(finalize_count(&records), 1);
    assert!(statuses(&records)
        .iter()
        .all(|s| matches!(s, awili_engine_log::replay::IntentStatus::Clean { .. })));
    assert!(
        cmds.try_recv().is_err(),
        "a builtin origin must never reach the MCP runtime"
    );
}

// ---------------------------------------------------------------------------
// ITEM 6 §2 — the ACCEPTED epoch, never the advertised proposal
// ---------------------------------------------------------------------------

// The server advertises 9 in a proposal the host REFUSES; the accepted record
// still says 7, and 7 is what the runtime actually receives on the wire.
#[test]
fn mcp_dispatch_uses_accepted_epoch_not_advertised_epoch() {
    let (_coord, facade, cmds, events) = scripted_runtime(4);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 7);

    let refused = bring_up_roots(
        &facade,
        &mut roots,
        &probe_observation(Some(9), "probe-server"),
        &probe_host(),
        &DenyRoots,
    );
    assert!(refused.is_err(), "a policy-refused proposal is not accepted");
    assert_eq!(
        roots.accepted(&awili_engine::ToolOrigin::new("mcp:probe")).map(|a| a.1),
        Some(7)
    );

    // The runtime side: answer the call with a Definite terminal.
    let player = std::thread::spawn(move || {
        // The accepted publication crossed first.
        let published = cmds.recv_blocking().expect("publication");
        let (seen, ticket) = take_call(&cmds);
        let intent = seen.intent_id.clone();
        events
            .send_blocking(McpEvent::CallFinished {
                ticket,
                intent_id: intent,
                disposition: CallDisposition::Completed,
                result: ToolResultRecord {
                    awili_call_id: awili_engine::AwiliCallId(seen.awili_call_id.clone()),
                    content: "pong".into(),
                    is_error: false,
                },
            })
            .expect("emit terminal");
        (published, seen)
    });

    let (run, _events) = run_seam_turn(
        &Executors::mcp(facade.clone()),
        &roots,
        Arc::new(AtomicBool::new(false)),
        true,
    );
    run.result.expect("turn completes");
    let (published, seen) = player.join().unwrap();

    assert!(
        matches!(published, McpCommand::UpdateAcceptedRoots { epoch: 7, .. }),
        "only the VALIDATED epoch is published: {published:?}"
    );
    assert_eq!(seen.origin, "mcp:probe");
    assert_eq!(seen.tool, "probe.ping");
    assert_eq!(
        seen.root_epoch, 7,
        "the ACCEPTED epoch crosses the wire, never the advertised 9"
    );
    assert_eq!(finalize_count(&run.records), 1);
}

// ---------------------------------------------------------------------------
// ITEM 6 §3 — refused roots leave the origin unavailable
// ---------------------------------------------------------------------------

// Malformed, identity-mismatched and policy-refused proposals each leave the
// origin with NO accepted record; nothing is published to the runtime, and a
// call to that origin is a finalized refusal that never dispatches and never
// falls back to the builtin leg.
#[test]
fn rejected_or_malformed_roots_leave_origin_unavailable() {
    let (_coord, facade, cmds, _events) = scripted_runtime(4);
    let origin = awili_engine::ToolOrigin::new("mcp:probe");
    let mut roots = RootsAcceptance::default();

    for (label, obs, policy) in [
        (
            "malformed",
            probe_observation(None, "probe-server"),
            &AllowRoots as &dyn RootsPolicy,
        ),
        ("identity", probe_observation(Some(3), "impostor"), &AllowRoots),
        ("policy", probe_observation(Some(3), "probe-server"), &DenyRoots),
    ] {
        assert!(
            bring_up_roots(&facade, &mut roots, &obs, &probe_host(), policy).is_err(),
            "{label}: must be refused"
        );
        assert_eq!(
            roots.accepted(&origin).map(|a| a.1),
            None,
            "{label}: the origin stays unavailable"
        );
    }
    assert!(
        cmds.try_recv().is_err(),
        "a refused proposal must never publish to the runtime"
    );

    let (run, events) = run_seam_turn(
        &Executors::mcp(facade.clone()),
        &roots,
        Arc::new(AtomicBool::new(false)),
        true,
    );
    run.result.expect("the turn continues past a carried refusal");
    assert!(
        cmds.try_recv().is_err(),
        "an unavailable origin must never dispatch"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolResultIngested(rec) if rec.is_error && rec.content.contains("unavailable")
        )),
        "the model is told the origin is unavailable"
    );
    // Unit 4b (row 21): the live-bind refusal happens BEFORE the durable
    // intent — the refused call mints no ticket and writes NO record, so
    // there is nothing to finalize. The refusal is still carried to the
    // model (asserted above); Definiteness is structural: nothing entered
    // the runtime.
    assert_eq!(
        finalize_count(&run.records),
        0,
        "a bind refusal writes nothing durable"
    );
}

// ---------------------------------------------------------------------------
// ITEM 6 §4 — reconnect revalidation
// ---------------------------------------------------------------------------

// A reconnect proposal is fully revalidated: a higher advertised epoch is not
// authority. Only a proposal that passes identity AND policy moves the accepted
// epoch, and only that one publishes.
#[test]
fn reconnect_requires_revalidation() {
    let (_coord, facade, cmds, _events) = scripted_runtime(4);
    let origin = awili_engine::ToolOrigin::new("mcp:probe");
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 3);

    assert!(bring_up_roots(
        &facade,
        &mut roots,
        &probe_observation(Some(9), "probe-server"),
        &probe_host(),
        &DenyRoots
    )
    .is_err());
    assert_eq!(roots.accepted(&origin).map(|a| a.1), Some(3));

    assert!(bring_up_roots(
        &facade,
        &mut roots,
        &probe_observation(Some(9), "someone-else"),
        &probe_host(),
        &AllowRoots
    )
    .is_err());
    assert_eq!(roots.accepted(&origin).map(|a| a.1), Some(3));

    accept_epoch(&facade, &mut roots, 9);
    assert_eq!(roots.accepted(&origin).map(|a| a.1), Some(9));

    // Exactly two publications reached the runtime: the initial bring-up and
    // the fully revalidated reconnect.
    let mut published = Vec::new();
    while let Ok(cmd) = cmds.try_recv() {
        if let McpCommand::UpdateAcceptedRoots { origin, epoch, .. } = cmd {
            published.push((origin.origin, epoch));
        }
    }
    assert_eq!(
        published,
        vec![("mcp:probe".to_string(), 3), ("mcp:probe".to_string(), 9)]
    );
}

// ---------------------------------------------------------------------------
// ITEM 6 §5 — a stale roots update is ignored AND traced
// ---------------------------------------------------------------------------

// The real trace: the runtime answers a stale publication with
// `RootsUpdateIgnored`, the SOLE coordinator pump ingests it, and it is visible
// in `LifecycleSnapshot::ignored_roots_updates` with its origin and both
// epochs. The local record keeps the newer authority, so the stale epoch never
// becomes dispatchable.
#[test]
fn stale_roots_update_is_ignored_and_traced() {
    let (_coord, facade, cmds, events) = scripted_runtime(4);
    let origin = awili_engine::ToolOrigin::new("mcp:probe");
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 5);

    // A valid-but-stale proposal: publication is attempted (that is what makes
    // it traceable at all), and the runtime refuses it as non-monotonic.
    accept_epoch(&facade, &mut roots, 2);
    assert_eq!(
        roots.accepted(&origin).map(|a| a.1),
        Some(5),
        "a stale update never lowers the accepted epoch"
    );

    let mut published = Vec::new();
    while let Ok(cmd) = cmds.try_recv() {
        if let McpCommand::UpdateAcceptedRoots { origin, epoch, .. } = cmd {
            published.push((origin.origin, epoch));
        }
    }
    assert_eq!(
        published,
        vec![("mcp:probe".to_string(), 5), ("mcp:probe".to_string(), 2)],
        "the stale publish is attempted, not silently dropped"
    );

    events
        .send_blocking(McpEvent::RootsUpdateIgnored {
            origin: awili_engine::ToolOrigin::new("mcp:probe"),
            stale_epoch: 2,
            current_epoch: 5,
        })
        .expect("emit the runtime's refusal");

    // The pump ingests continuously; wait bounded for the trace to land.
    let deadline = Instant::now() + Duration::from_secs(5);
    let ignored = loop {
        let snap = facade.lifecycle();
        if !snap.ignored_roots_updates.is_empty() {
            break snap.ignored_roots_updates;
        }
        assert!(
            Instant::now() < deadline,
            "the coordinator pump never traced RootsUpdateIgnored"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(ignored.len(), 1);
    assert_eq!(ignored[0].origin, "mcp:probe");
    assert_eq!(ignored[0].stale_epoch, 2);
    assert_eq!(ignored[0].current_epoch, 5);
}

// ---------------------------------------------------------------------------
// ITEM 6 §6 — ticket conservation through the PRODUCTION adapter
// ---------------------------------------------------------------------------

// A Definite terminal: the exact ticket the seam submitted comes back through
// the coordinator, finalizes the intent once, and replays Clean.
#[test]
fn mcp_terminal_returns_the_submitted_ticket_and_finalizes_once() {
    let (_coord, facade, cmds, events) = scripted_runtime(4);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 4);

    let player = std::thread::spawn(move || {
        let _publication = cmds.recv_blocking().expect("publication");
        let (seen, ticket) = take_call(&cmds);
        // The runtime returns the SAME ticket it received, by move.
        assert_eq!(ticket.intent_id(), seen.intent_id);
        events
            .send_blocking(McpEvent::CallFinished {
                ticket,
                intent_id: seen.intent_id.clone(),
                disposition: CallDisposition::Completed,
                result: ToolResultRecord {
                    awili_call_id: awili_engine::AwiliCallId(seen.awili_call_id.clone()),
                    content: "pong".into(),
                    is_error: false,
                },
            })
            .expect("emit terminal");
        seen
    });

    let (run, events_seen) = run_seam_turn(
        &Executors::mcp(facade.clone()),
        &roots,
        Arc::new(AtomicBool::new(false)),
        true,
    );
    run.result.expect("turn completes");
    let seen = player.join().unwrap();

    assert_eq!(
        logged_intent_ids(&run.records),
        vec![seen.intent_id],
        "the dispatched intent is the one B2 minted"
    );
    assert_eq!(finalize_count(&run.records), 1);
    assert!(events_seen
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolResultIngested(r) if r.content == "pong")));
}

// A DUPLICATE rejection: the duplicate's own ticket comes back through the
// coordinator and reaches the seam's typed failure INTACT — the identity is
// asserted at the seam boundary, and the intent is left unfinalized.
#[test]
fn mcp_rejection_returns_the_ticket_to_the_seam_failure() {
    let (_coord, facade, cmds, events) = scripted_runtime(4);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 4);

    let player = std::thread::spawn(move || {
        let _publication = cmds.recv_blocking().expect("publication");
        let (seen, ticket) = take_call(&cmds);
        events
            .send_blocking(McpEvent::SubmissionRejected {
                ticket,
                intent_id: seen.intent_id.clone(),
                submission_id: seen.submission_id,
                reason: "duplicate intent_id".into(),
            })
            .expect("emit rejection");
        seen
    });

    let (run, _events) = run_seam_turn(
        &Executors::mcp(facade.clone()),
        &roots,
        Arc::new(AtomicBool::new(false)),
        true,
    );
    let seen = player.join().unwrap();

    match run.result {
        Err(SeamError::McpDispatch(McpDispatchFailure::SubmissionRejected {
            ref ticket,
            ref reason,
        })) => {
            assert_eq!(
                ticket.intent_id(),
                seen.intent_id,
                "the REJECTED submission's own ticket reaches the seam failure"
            );
            assert!(reason.contains("duplicate"));
        }
        other => panic!("expected a ticket-bearing rejection failure, got {other:?}"),
    }
    assert_eq!(
        finalize_count(&run.records),
        0,
        "a rejection is not a terminal — the intent stays open for replay"
    );
}

// A FINAL submit refusal on the deadline: nothing was enqueued, so the
// complete command comes back and the seam's typed failure owns the ticket.
// The command channel is deliberately jammed so `submit_until` cannot enqueue.
#[test]
fn mcp_submit_refused_on_deadline_returns_the_ticket_to_the_seam_failure() {
    // Capacity 1, pre-filled: every later submit finds the runtime full.
    let (_coord, facade, cmds, _events) = scripted_runtime(1);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 4);
    // Drain the publication, then jam the channel with an unconsumed command.
    let _ = cmds.recv_blocking().expect("publication");
    facade
        .publish_accepted_roots(Instant::now() + Duration::from_secs(5), awili_engine::ToolOrigin::new("mcp:jam"), awili_engine::generation::GenerationAllocator::new().next(), 1)
        .expect("the jamming publication enqueues");

    let (run, _events) = run_seam_turn(
        &Executors::mcp(facade.clone()).with_submit_deadline(Duration::from_millis(150)),
        &roots,
        Arc::new(AtomicBool::new(false)),
        true,
    );
    match run.result {
        Err(SeamError::McpDispatch(McpDispatchFailure::SubmitRefused {
            kind: RefusalKind::DeadlineExhausted,
            ref ticket,
        })) => {
            assert_eq!(
                logged_intent_ids(&run.records),
                vec![ticket.intent_id().to_string()],
                "the refused submit returned the exact ticket B2 minted"
            );
        }
        other => panic!("expected a ticket-bearing deadline refusal, got {other:?}"),
    }
    assert_eq!(finalize_count(&run.records), 0);
    assert!(statuses(&run.records)
        .iter()
        .any(|s| matches!(s, awili_engine_log::replay::IntentStatus::Uncertain)));
}

// A FINAL submit refusal because the runtime CLOSED: same conservation, and
// the refusal kind is distinct.
#[test]
fn mcp_submit_refused_on_closed_returns_the_ticket_to_the_seam_failure() {
    let (_coord, facade, cmds, _events) = scripted_runtime(4);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 4);
    // The runtime is gone: its command receiver is dropped.
    drop(cmds);

    let (run, _events) = run_seam_turn(
        &Executors::mcp(facade.clone()).with_submit_deadline(Duration::from_millis(500)),
        &roots,
        Arc::new(AtomicBool::new(false)),
        true,
    );
    match run.result {
        Err(SeamError::McpDispatch(McpDispatchFailure::SubmitRefused {
            kind: RefusalKind::Closed,
            ref ticket,
        })) => {
            assert_eq!(
                logged_intent_ids(&run.records),
                vec![ticket.intent_id().to_string()],
                "a closed runtime returns the exact ticket too"
            );
        }
        other => panic!("expected a ticket-bearing closed refusal, got {other:?}"),
    }
    assert_eq!(finalize_count(&run.records), 0);
}

// ---------------------------------------------------------------------------
// ITEM 6 §7 — in-flight cancellation
// ---------------------------------------------------------------------------

// Cancellation DURING a blocked MCP call, made deterministic by the scripted
// runtime: it holds the call, trips the host's cancel flag, and only then
// answers. The seam must observe the stop while blocked, submit
// `Cancel { intent_id }` keyed by the B2 intent, drain the linearized winner,
// finalize the Definite terminal that won, and stop the turn with
// StopCause::Cancelled — mirroring the builtin path.
#[test]
fn mcp_cancellation_during_a_blocked_call_submits_cancel_and_stops_the_turn() {
    let (_coord, facade, cmds, events) = scripted_runtime(4);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 4);
    let cancel = Arc::new(AtomicBool::new(false));

    let player = {
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let _publication = cmds.recv_blocking().expect("publication");
            let (seen, ticket) = take_call(&cmds);
            // The call is now IN FLIGHT and the seam is polling for it. Trip
            // the stop; the seam must react while blocked here.
            cancel.store(true, AtomicOrdering::SeqCst);
            // The seam's Cancel must arrive, keyed by the B2 intent id.
            let cancelled = loop {
                match cmds.recv_blocking().expect("runtime channel") {
                    McpCommand::Cancel { intent_id } => break intent_id,
                    _ => continue,
                }
            };
            // The real terminal then wins (join-before-synthesis).
            events
                .send_blocking(McpEvent::CallFinished {
                    ticket,
                    intent_id: seen.intent_id.clone(),
                    disposition: CallDisposition::CancelledBeforeDispatch,
                    result: ToolResultRecord {
                        awili_call_id: awili_engine::AwiliCallId(seen.awili_call_id.clone()),
                        content: "cancelled before dispatch".into(),
                        is_error: true,
                    },
                })
                .expect("emit terminal");
            (seen, cancelled)
        })
    };

    let (run, _events) = run_seam_turn(
        &Executors::mcp(facade.clone()),
        &roots,
        Arc::clone(&cancel),
        true,
    );
    let (seen, cancelled_intent) = player.join().unwrap();

    assert_eq!(
        cancelled_intent, seen.intent_id,
        "Cancel is keyed by the B2 intent id, never a provider call id"
    );
    match run.result {
        Err(SeamError::Cancelled(StopCause::Cancelled)) => {}
        other => panic!("a cancelled turn must stop with StopCause::Cancelled, got {other:?}"),
    }
    // CancelledBeforeDispatch is DEFINITE, so the intent is finalized — the
    // durable record stays honest even though the turn stopped.
    assert_eq!(finalize_count(&run.records), 1);
    assert!(statuses(&run.records)
        .iter()
        .all(|s| !matches!(s, awili_engine_log::replay::IntentStatus::Uncertain)));
}

// A terminal that lands in the SAME poll slice as the host's stop must not mask
// it. The runtime here trips the cancel flag and immediately emits the terminal
// WITHOUT waiting to see a `Cancel` command — the c6r counterexample. The
// terminal keeps its durable disposition (Definite ⇒ finalized once), the
// result is NOT fed to the reducer, and the turn still stops Cancelled.
#[test]
fn mcp_terminal_in_the_same_slice_does_not_mask_a_requested_cancellation() {
    let (_coord, facade, cmds, events) = scripted_runtime(4);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 4);
    let cancel = Arc::new(AtomicBool::new(false));

    let player = {
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let _publication = cmds.recv_blocking().expect("publication");
            let (seen, ticket) = take_call(&cmds);
            // Stop FIRST, terminal immediately after, and never wait for a
            // Cancel: the seam must still see the stop for this iteration.
            cancel.store(true, AtomicOrdering::SeqCst);
            events
                .send_blocking(McpEvent::CallFinished {
                    ticket,
                    intent_id: seen.intent_id.clone(),
                    disposition: CallDisposition::Completed,
                    result: ToolResultRecord {
                        awili_call_id: awili_engine::AwiliCallId(seen.awili_call_id.clone()),
                        content: "pong".into(),
                        is_error: false,
                    },
                })
                .expect("emit terminal");
            seen
        })
    };

    let (run, events_seen) = run_seam_turn(
        &Executors::mcp(facade.clone()),
        &roots,
        Arc::clone(&cancel),
        true,
    );
    let _seen = player.join().unwrap();

    match run.result {
        Err(SeamError::Cancelled(StopCause::Cancelled)) => {}
        other => panic!("a terminal must not mask a requested stop, got {other:?}"),
    }
    // Durability first: the Definite terminal is still finalized exactly once.
    assert_eq!(finalize_count(&run.records), 1);
    assert!(run.records.iter().any(|r| matches!(
        r,
        awili_engine_log::schema::LogRecordV1::DispatchFinalized { ok: true, .. }
    )));
    // But the turn stopped, so the model never saw the result.
    assert!(
        !events_seen
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolResultIngested(r) if r.content == "pong")),
        "a stopped turn must not feed the result to the reducer"
    );
}

// A cancel whose SUBMISSION is refused: the coordinator commits the refusal at
// the waiter table and wakes the waiter with CancelSubmitRefused. The seam must
// surface that typed failure, never synthesize a terminal, and leave the intent
// Uncertain for replay.
#[test]
fn mcp_cancel_submit_refusal_wakes_the_seam_without_a_synthetic_terminal() {
    // Capacity 1: with one unconsumed command sitting in it, the runtime is
    // full and nothing further can enqueue.
    let (_coord, facade, cmds, _events) = scripted_runtime(1);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 4);
    let cancel = Arc::new(AtomicBool::new(false));

    let player = {
        let cancel = Arc::clone(&cancel);
        let facade = facade.clone();
        std::thread::spawn(move || {
            let _publication = cmds.recv_blocking().expect("publication");
            let (seen, ticket) = take_call(&cmds);
            // The call is in flight and the runtime channel is empty. JAM it
            // with a publication nobody will consume, so the seam's Cancel
            // meets real backpressure and exhausts its deadline.
            facade
                .publish_accepted_roots(Instant::now() + Duration::from_secs(5), awili_engine::ToolOrigin::new("mcp:jam"), awili_engine::generation::GenerationAllocator::new().next(), 1)
                .expect("the jamming publication enqueues");
            cancel.store(true, AtomicOrdering::SeqCst);
            // Hold the ticket: this call NEVER finishes, so the only thing
            // that can wake the seam is the coordinator's committed
            // cancel-submission failure.
            std::thread::sleep(Duration::from_secs(2));
            drop(ticket);
            seen
        })
    };

    let (run, _events) = run_seam_turn(
        &Executors::mcp(facade.clone()).with_submit_deadline(Duration::from_millis(150)),
        &roots,
        Arc::clone(&cancel),
        true,
    );
    let _seen = player.join().unwrap();

    match run.result {
        Err(SeamError::McpDispatch(McpDispatchFailure::CancelSubmitRefused { .. })) => {}
        other => panic!("expected a typed cancel-submit refusal, got {other:?}"),
    }
    assert_eq!(
        finalize_count(&run.records),
        0,
        "no synthetic terminal for a refused cancel"
    );
    assert!(statuses(&run.records)
        .iter()
        .any(|s| matches!(s, awili_engine_log::replay::IntentStatus::Uncertain)));
}

// The cancel rides the call's ORIGINAL absolute deadline, not a fresh window.
// The runtime jams the command channel, lets the original deadline ELAPSE, and
// only then trips the stop. `submit_until` must therefore refuse the cancel
// immediately (its bound is already spent) instead of fighting backpressure for
// another full submit_deadline — the bug this pins would have cost a cancelled
// turn a second complete window.
#[test]
fn cancel_rides_the_original_deadline_and_gets_no_fresh_retry_window() {
    // Long enough that a fresh window is unmistakable in the timing below.
    const SUBMIT_DEADLINE: Duration = Duration::from_secs(2);
    let (_coord, facade, cmds, _events) = scripted_runtime(1);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 4);
    let cancel = Arc::new(AtomicBool::new(false));
    let tripped_at: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    let player = {
        let cancel = Arc::clone(&cancel);
        let tripped_at = Arc::clone(&tripped_at);
        let facade = facade.clone();
        std::thread::spawn(move || {
            let _publication = cmds.recv_blocking().expect("publication");
            let (seen, ticket) = take_call(&cmds);
            // Jam the runtime so any later submission meets real backpressure.
            facade
                .publish_accepted_roots(Instant::now() + Duration::from_secs(5), awili_engine::ToolOrigin::new("mcp:jam"), awili_engine::generation::GenerationAllocator::new().next(), 1)
                .expect("the jamming publication enqueues");
            // Let the call's ORIGINAL deadline elapse before stopping.
            std::thread::sleep(SUBMIT_DEADLINE + Duration::from_millis(150));
            *tripped_at.lock().unwrap() = Some(Instant::now());
            cancel.store(true, AtomicOrdering::SeqCst);
            // The call never finishes: only the committed cancel-submission
            // failure can wake the seam.
            std::thread::sleep(Duration::from_secs(2));
            drop(ticket);
            seen
        })
    };

    let (run, _events) = run_seam_turn(
        &Executors::mcp(facade.clone()).with_submit_deadline(SUBMIT_DEADLINE),
        &roots,
        Arc::clone(&cancel),
        true,
    );
    let returned_at = Instant::now();
    let _seen = player.join().unwrap();

    match run.result {
        Err(SeamError::McpDispatch(McpDispatchFailure::CancelSubmitRefused {
            kind: RefusalKind::DeadlineExhausted,
        })) => {}
        other => panic!("expected an immediate deadline-exhausted cancel refusal, got {other:?}"),
    }
    let trip = tripped_at.lock().unwrap().expect("the stop was tripped");
    let waited = returned_at.saturating_duration_since(trip);
    assert!(
        waited < SUBMIT_DEADLINE,
        "the cancel got a FRESH retry window: {waited:?} elapsed after the stop, \
         but the call's original {SUBMIT_DEADLINE:?} bound was already spent"
    );
    assert_eq!(finalize_count(&run.records), 0);
}

// ---------------------------------------------------------------------------
// ITEM 6 §8 — routing has no silent fallback
// ---------------------------------------------------------------------------

// A non-builtin origin in a composition with no MCP executor is a typed
// harness error at DISPATCH — the mirror of item 3's no-fallback rule on the
// routing side. Registering and advertising such a tool stays legitimate.
#[test]
fn non_builtin_origin_without_an_executor_is_a_typed_harness_error() {
    let executors = Executors::builtin_only();
    let err = executors
        .route(&awili_engine::ToolOrigin::new("mcp:probe"))
        .expect_err("no MCP executor is configured");
    assert!(
        matches!(&err, SeamError::Harness(m) if m.contains("no executor for origin")),
        "{err:?}"
    );
    assert!(matches!(
        executors.route(&awili_engine::ToolOrigin::new("builtin:fs")),
        Ok(awili_seam::Executor::BuiltinFs)
    ));
}

// ---------------------------------------------------------------------------
// UNIT 5 §5 — the end-to-end install-grant proof and the live kill switch
// ---------------------------------------------------------------------------

// A host that treats ANY approval card as test failure when armed, and
// records + denies when not: the positive leg proves NO card is ever
// raised for an install-granted call; the kill-switch leg proves the card
// RETURNS the moment the query stops issuing witnesses (ruling 8's caveat
// holds trivially here — no other grant covers the call).
struct CardMeter {
    asked: Arc<AtomicBool>,
    panic_on_ask: bool,
    events: Vec<AgentEvent>,
}

impl TurnHost for CardMeter {
    fn approve(&mut self, _call_id: &str, prompt: &str, _risk: RiskClass) -> ApprovalOutcome {
        if self.panic_on_ask {
            panic!("an install-granted call must never raise a card: {prompt}");
        }
        self.asked.store(true, AtomicOrdering::SeqCst);
        ApprovalOutcome::Verdict(UserVerdict::Deny {
            reason: "test denies".into(),
        })
    }
    fn observe(&mut self, event: &AgentEvent) -> Result<(), awili_seam::ObserverGone> {
        self.events.push(event.clone());
        Ok(())
    }
    fn cancelled(&self) -> bool {
        false
    }
}

// The scripted per-call authority: issues a witness for the identity the
// grant was seeded under, until the kill switch flips — then None, exactly
// as the app implementation refuses when trust leaves Install (unit 5 §4;
// tests C/D of the design's plan collapse to this None-arm because every
// refusal reason converges on "no witness this call").
struct ScriptedInstallAuthority {
    facts: awili_tools::attest::SetupRecordFacts,
    enabled: Arc<AtomicBool>,
}

impl awili_engine_authz::InstallAuthorityQuery for ScriptedInstallAuthority {
    fn with_witness(
        &self,
        origin: &awili_tools::ToolOrigin,
        f: awili_engine_authz::InstallWitnessCallback<'_>,
    ) -> awili_engine_authz::Decision {
        if !self.enabled.load(AtomicOrdering::SeqCst) || origin != &self.facts.origin {
            return f(None);
        }
        let generation = awili_engine::generation::process().next();
        let token = awili_tools::attest::LaunchToken::mint_verified(&self.facts, generation);
        match token.bind_scoped(&self.facts.origin, generation) {
            Ok(awili_tools::attest::BoundLaunch::Verified(v)) => {
                v.with_install_witness(|w| f(Some(w)))
            }
            _ => f(None),
        }
    }
}

fn mac_tool() -> awili_tools::ToolDef {
    awili_tools::ToolDef::from_parts(
        awili_tools::ToolOrigin::new("mcp:mac-shell"),
        // The COMMITTED catalog's tool name (commit_live advertises "run"):
        // install grants are per (origin, tool).
        "run",
        awili_tools::CanonicalDescriptor {
            schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            side_effect: "privileged".into(),
            required_capabilities: vec![],
            executor: "mcp:mac-shell".into(),
        },
    )
}

// Bring mcp:mac-shell to accepted roots on the scripted runtime, sharing
// the committed catalog's identity facts.
fn mac_observation(
    facts: &awili_tools::attest::SetupRecordFacts,
    epoch: Option<u64>,
) -> awili_tools::runtime::ConnectedLaunch {
    let token = awili_tools::attest::LaunchToken::mint_verified(
        facts,
        awili_engine::generation::process().next(),
    );
    let (sink, stream) = awili_tools::runtime::event_channel::<IntentTicket>(4);
    let (ctx, _crx) = async_channel::bounded(1);
    let handle =
        awili_tools::runtime::McpRuntimeHandle::from_parts(ctx, stream, std::thread::spawn(|| {}));
    sink.emit_server_connected_blocking(token, "srv".into(), Vec::new(), Vec::new(), epoch)
        .expect("emit");
    drop(sink);
    match handle.try_recv_event() {
        Some(awili_tools::runtime::McpEvent::ServerConnected(c)) => c,
        other => panic!("expected the observation, got {other:?}"),
    }
}

// The end-to-end proof (design r9 slice s4): a privileged mac-shell call is
// authorized by the SEEDED install grant — no card, evidence basis "grant"
// naming the transition's approval_ref, dispatch completes — and flipping
// the authority off brings the card back on the very next turn.
#[test]
fn an_install_grant_authorizes_without_a_card_and_the_kill_switch_restores_it() {
    let _g = crate::floor_owner::tests::owner_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let host = authority().expect("worker authority");

    // The committed catalog: register a state and commit mcp:mac-shell
    // live (the floor owner is the ONLY floors source).
    let (state, brand) = crate::mcp_catalog::WorkMcpState::new_registered();
    crate::floor_owner::register_catalog(brand).expect("register");
    let origin = awili_tools::ToolOrigin::new("mcp:mac-shell");
    let connected = crate::floor_owner::tests::commit_live(&state, "mcp:mac-shell", 7);

    // Seed the install grant from the committed floors + the verified
    // launch — through the REAL candidate path.
    let floors = crate::floor_owner::accepted_floors(&origin).expect("committed floors");
    let awili_tools::attest::BoundLaunch::Verified(v) = connected.bind() else {
        panic!("verified");
    };
    let candidate =
        awili_engine_log::authz::InstallSeedCandidate::mint(v, floors).expect("candidate");
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let outcome = grants
        .record_install_grants(&host.approval_cap, candidate)
        .unwrap();
    assert_eq!(outcome.seeded, vec!["run"], "the committed tool seeds");
    let (grant_records, _) = grants.read_records().unwrap();
    let seeded_ref = grant_records
        .iter()
        .find_map(|r| match r {
            awili_engine_log::schema::LogRecordV1::InstallTransition { approval_ref, .. } => {
                Some(approval_ref.clone())
            }
            _ => None,
        })
        .expect("transition present");

    // The identity facts the witness must carry = the committed record's.
    let facts = awili_tools::attest::SetupRecordFacts {
        origin: origin.clone(),
        setup_id: awili_tools::attest::SetupIdentityId::parse("setup-1").unwrap(),
        pinned_requirement: "cdhash H\"00\"".into(),
        expected_protocol_name: "srv".into(),
        launch_kind: awili_tools::attest::LaunchKind::Direct,
        program: "/usr/local/lib/awili/srv".into(),
    };
    let enabled = Arc::new(AtomicBool::new(true));
    let query = ScriptedInstallAuthority {
        facts: facts.clone(),
        enabled: enabled.clone(),
    };

    // Scripted runtime with accepted roots for mcp:mac-shell.
    let (_coord, facade, cmds, events) = scripted_runtime(4);
    let mut roots = RootsAcceptance::default();
    bring_up_roots(
        &facade,
        &mut roots,
        &mac_observation(&facts, Some(7)),
        &HostRootProposal {
            origin: origin.clone(),
            proposed_roots: vec!["/work".into()],
        },
        &AllowRoots,
    )
    .expect("bring-up accepts");

    // Runtime side: answer the ONE expected call with a success terminal.
    let player = std::thread::spawn(move || {
        let _publication = cmds.recv_blocking().expect("publication");
        let (seen, ticket) = take_call(&cmds);
        events
            .send_blocking(McpEvent::CallFinished {
                ticket,
                intent_id: seen.intent_id.clone(),
                disposition: CallDisposition::Completed,
                result: ToolResultRecord {
                    awili_call_id: awili_engine::AwiliCallId(seen.awili_call_id.clone()),
                    content: "ran".into(),
                    is_error: false,
                },
            })
            .expect("emit terminal");
        seen
    });

    // Turn 1: the card would PANIC the host — it must never come.
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-u5").unwrap();
    let mut transport = Script {
        bodies: vec![
            sse_tool_round("call_m", "run", "{}"),
            sse_text_final("done"),
        ]
        .into(),
    };
    let mut host_io = CardMeter {
        asked: Arc::new(AtomicBool::new(false)),
        panic_on_ask: true,
        events: Vec::new(),
    };
    let extra = mac_tool();
    let mut history = ContextHistory::new();
    run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "run it",
        &mut host_io,
        &mut log,
        &mut grants,
        std::slice::from_ref(&extra),
        &Executors::mcp(facade.clone()),
        &query,
        &roots,
        awili_seam::DEFAULT_WINDOW_TOKENS,
        "turn-u5-0",
        &mut history,
    )
    .expect("turn completes without a card");
    player.join().unwrap();
    let (records, _) = log.read_records().unwrap();
    let (basis, aref) = records
        .iter()
        .find_map(|r| match r {
            awili_engine_log::schema::LogRecordV1::Intent {
                decision_basis,
                approval_ref,
                ..
            } => Some((decision_basis.clone(), approval_ref.clone())),
            _ => None,
        })
        .expect("intent logged");
    assert_eq!(basis, "grant", "authorized by the stored install grant");
    assert_eq!(aref, seeded_ref, "evidence names the seeded transition");

    // Turn 2: the kill switch — no witness, the card RETURNS.
    enabled.store(false, AtomicOrdering::SeqCst);
    let mut log2 = ExecutionLog::open(MemLog::default(), "sess-u5b").unwrap();
    let mut transport2 = Script {
        bodies: vec![
            sse_tool_round("call_m2", "run", "{}"),
            sse_text_final("done"),
        ]
        .into(),
    };
    let asked = Arc::new(AtomicBool::new(false));
    let mut host_io2 = CardMeter {
        asked: asked.clone(),
        panic_on_ask: false,
        events: Vec::new(),
    };
    let mut history2 = ContextHistory::new();
    run_turn(
        &host,
        &mut transport2,
        "test-model",
        root.path().to_str().unwrap(),
        "run it again",
        &mut host_io2,
        &mut log2,
        &mut grants,
        std::slice::from_ref(&extra),
        &Executors::mcp(facade.clone()),
        &query,
        &roots,
        awili_seam::DEFAULT_WINDOW_TOKENS,
        "turn-u5-1",
        &mut history2,
    )
    .expect("denied turn still completes");
    assert!(
        asked.load(AtomicOrdering::SeqCst),
        "trust off => the card returns on the very next call"
    );
}

// Ship-fix P0-5 (runtime half): a REQUIRED origin missing at turn open
// fails the TURN — no transport call, no silent builtin-only run — and
// the next prompt gets a fresh chance.
#[test]
fn a_missing_required_origin_fails_the_turn_without_running_it() {
    let (_coord, facade, _cmds, _events) = scripted_runtime(4);
    let host = authority().expect("worker authority");
    // ANY transport call is test failure — the gate must fire first.
    struct PanicTransport;
    impl awili_seam::Transport for PanicTransport {
        fn stream(&mut self, _request_body: &str) -> Result<String, SeamError> {
            panic!("a failed-required turn must never reach the transport");
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = crate::AwiliEngineConfig::new(
        "http://unused",
        "test-model",
        dir.path().join("b2.jsonl"),
        dir.path().join("grants.jsonl"),
    );
    let worker = crate::AwiliEngineWorker::with_transport(cfg, host, Box::new(PanicTransport))
        .unwrap()
        .with_mcp(crate::WorkerMcp {
            executors: Executors::mcp(facade),
            turn_open: Box::new(|| {
                (
                    Vec::new(),
                    RootsAcceptance::default(),
                    Vec::new(),
                    None,
                    vec!["mcp:mac-shell".into()],
                )
            }),
            install_authority: std::sync::Arc::new(awili_engine_authz::NoInstallAuthority),
            coordinator: {
                let (ctx2, _c) = async_channel::bounded(1);
                let (_s2, e2) = awili_tools::runtime::event_channel::<IntentTicket>(1);
                McpCoordinator::new(awili_tools::runtime::McpRuntimeHandle::from_parts(
                    ctx2,
                    e2,
                    std::thread::spawn(|| {}),
                ))
            },
        });
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (ev_tx, events) = async_channel::unbounded();
    let _handle = awili_work::spawn_worker(
        Box::new(worker),
        WorkerContext {
            workdir: dir.path().display().to_string(),
            cmd_rx,
            cancel: Arc::new(AtomicBool::new(false)),
            events: ev_tx,
        },
    );
    cmd_tx
        .send(WorkCmd::Prompt {
            text: "do work".into(),
            system: None,
        })
        .unwrap();
    let mut failed_msg = None;
    loop {
        match events.recv_blocking().expect("worker alive") {
            WorkEvent::TurnCompleted { status, output, .. } => {
                assert_eq!(status, awili_work::run_events::RunStatus::Failed);
                failed_msg = output;
            }
            WorkEvent::TurnComplete => break,
            WorkEvent::Failed(m) => panic!("worker-fatal is wrong here: {m}"),
            _ => {}
        }
    }
    drop(cmd_tx);
    let msg = failed_msg.expect("failed turn carries the reason");
    assert!(msg.contains("mcp:mac-shell"), "{msg}");
    // (transport panics on any call — reaching here proves it never ran)
}

// RC1-N-FIX2/FIX3: a batch of a builtin write, then an MCP call whose
// Completed terminal arrives with the owner's Stop. The seam finalizes the MCP
// call but does not feed its result to the stopped turn; the kept history
// still holds it (every finalized call is placed in a kept turn's history),
// so nothing is left for recovery.
#[test]
fn rc1n_fix2_an_mcp_result_that_arrives_with_a_stop_is_recovered() {
    let (_coord, facade, cmds, events) = scripted_runtime(4);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 4);
    let cancel = Arc::new(AtomicBool::new(false));
    let player = {
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let _publication = cmds.recv_blocking().expect("publication");
            let (seen, ticket) = take_call(&cmds);
            cancel.store(true, AtomicOrdering::SeqCst);
            events
                .send_blocking(McpEvent::CallFinished {
                    ticket,
                    intent_id: seen.intent_id.clone(),
                    disposition: CallDisposition::Completed,
                    result: ToolResultRecord {
                        awili_call_id: awili_engine::AwiliCallId(seen.awili_call_id.clone()),
                        content: "PONG-AFTER-STOP".into(),
                        is_error: false,
                    },
                })
                .expect("emit terminal");
        })
    };
    let host = authority().expect("worker authority");
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-rc1nf2").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let call = |i: usize, id: &str, name: &str, args: &str| {
        let args_esc = serde_json::to_string(args).unwrap();
        chunk(&format!(
            r#"{{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{{"index":0,"delta":{{"role":"assistant","content":"","tool_calls":[{{"id":"{id}","index":{i},"type":"function","function":{{"name":"{name}","arguments":{args_esc}}}}}]}},"finish_reason":null}}]}}"#
        ))
    };
    let mut round = call(0, "call_w", "fs.write", r#"{"path":"kept.txt","contents":"x"}"#);
    round.push_str(&call(1, "call_p", "probe.ping", "{}"));
    round.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"tool_calls"}]}"#,
    ));
    round.push_str("data: [DONE]\n\n");
    let mut transport = Script {
        bodies: vec![round, sse_text_final("done")].into(),
    };
    let mut host_io = SeamHost {
        cancel: Arc::clone(&cancel),
        events: Vec::new(),
        dispatching: Vec::new(),
    };
    let extra = mcp_tool();
    let mut history = ContextHistory::new();
    let result = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "write, then ping",
        &mut host_io,
        &mut log,
        &mut grants,
        std::slice::from_ref(&extra),
        &Executors::mcp(facade.clone()),
        &awili_engine_authz::NoInstallAuthority,
        &roots,
        awili_seam::DEFAULT_WINDOW_TOKENS,
        "turn-rc1nf2-0",
        &mut history,
    );
    player.join().unwrap();
    assert!(
        matches!(result, Err(SeamError::Cancelled(StopCause::Cancelled))),
        "{result:?}"
    );
    assert!(root.path().join("kept.txt").exists(), "the write ran");
    let (records, _) = log.read_records().unwrap();
    assert_eq!(finalize_count(&records), 2, "both calls finalized");
    // RC1-N-FIX3: the kept history holds the write AND the ping (every call
    // the turn finalized is placed in it, with its finalized result) and is
    // stamped with the turn; the stopped turn's reducer never saw the ping.
    assert_eq!(history.log_turn(), Some("turn-rc1nf2-0"));
    let held = serde_json::to_string(&history).unwrap();
    assert!(
        held.contains("kept.txt") && held.contains("probe.ping") && held.contains("PONG-AFTER-STOP"),
        "{held}"
    );
    // So the next worker's recovery finds nothing missing: no note, ever.
    assert_eq!(awili_seam::recovered_work_note(&records, &history), None);
    let mut next = history.clone();
    assert!(!awili_seam::recover_completed_work(&records, &mut next));
    // Without that history (the stopped worker died before it was kept),
    // recovery lists both steps from the log.
    let note = awili_seam::recovered_work_note(&records, &ContextHistory::new()).expect("a note");
    assert!(
        note.starts_with(awili_seam::RECOVERED_WORK_LABEL)
            && note.contains("kept.txt")
            && note.contains("PONG-AFTER-STOP"),
        "{note}"
    );
}

// RC1-O-FIX7 (whole-slice review P2): the seam tells the host a call is
// dispatching only once it really started — a call to an origin without
// accepted roots is refused first and never reported; an accepted one is
// reported once.
#[test]
fn rc1o_fix7_only_a_started_mcp_call_is_reported_as_dispatching() {
    // Refused: no accepted roots record for the origin.
    let (_coord, facade, cmds, _events) = scripted_runtime(4);
    let roots = RootsAcceptance::default();
    let (run, _events, dispatching) = run_seam_turn_traced(
        &Executors::mcp(facade.clone()),
        &roots,
        Arc::new(AtomicBool::new(false)),
        true,
    );
    run.result.expect("the turn continues past the refusal");
    assert!(cmds.try_recv().is_err(), "never dispatched");
    assert!(dispatching.is_empty(), "{dispatching:?}");

    // Accepted: reported once.
    let (_coord, facade, cmds, events) = scripted_runtime(4);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 7);
    let player = std::thread::spawn(move || {
        let _published = cmds.recv_blocking().expect("publication");
        let (seen, ticket) = take_call(&cmds);
        events
            .send_blocking(McpEvent::CallFinished {
                ticket,
                intent_id: seen.intent_id.clone(),
                disposition: CallDisposition::Completed,
                result: ToolResultRecord {
                    awili_call_id: awili_engine::AwiliCallId(seen.awili_call_id.clone()),
                    content: "pong".into(),
                    is_error: false,
                },
            })
            .expect("emit terminal");
    });
    let (run, _events, dispatching) = run_seam_turn_traced(
        &Executors::mcp(facade.clone()),
        &roots,
        Arc::new(AtomicBool::new(false)),
        true,
    );
    run.result.expect("turn completes");
    player.join().unwrap();
    assert_eq!(dispatching.len(), 1, "{dispatching:?}");
}

// RC1-O-FIX8 (whole-slice review of FIX7, P2): a submission the runtime
// refuses never entered it, so it is never reported as dispatching.
#[test]
fn rc1o_fix8_a_refused_mcp_submission_is_not_reported_as_dispatching() {
    let (_coord, facade, cmds, _events) = scripted_runtime(1);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 4);
    let _ = cmds.recv_blocking().expect("publication");
    facade
        .publish_accepted_roots(
            Instant::now() + Duration::from_secs(5),
            awili_engine::ToolOrigin::new("mcp:jam"),
            awili_engine::generation::GenerationAllocator::new().next(),
            1,
        )
        .expect("the jamming publication enqueues");
    let (run, _events, dispatching) = run_seam_turn_traced(
        &Executors::mcp(facade.clone()).with_submit_deadline(Duration::from_millis(150)),
        &roots,
        Arc::new(AtomicBool::new(false)),
        true,
    );
    assert!(
        matches!(
            run.result,
            Err(SeamError::McpDispatch(McpDispatchFailure::SubmitRefused { .. }))
        ),
        "the submit was refused"
    );
    assert!(dispatching.is_empty(), "{dispatching:?}");
}

// RC2-ENGB4 (S-R2, RC1-R slice review P3): a server's own description goes
// the production way into the request the model gets — captured
// (`bounded_description`, control characters removed), advertised in the
// observation, projected by the host catalog (`project_advertised`) and
// emitted by the seam's `run_turn` — instead of a hand-described `ToolDef`.
#[test]
fn rc2engb4_an_advertised_description_reaches_the_request_through_the_catalog() {
    let advertised = awili_tools::runtime::AdvertisedTool {
        name: "probe.ping".into(),
        arg_schema_canonical: r#"{"properties":{},"type":"object"}"#.into(),
        risk: RiskClass::ReadOnly,
        description: awili_tools::runtime::bounded_description(
            "Pings the\u{7} probe: PROBE-SAYS-HELLO",
        ),
    };
    let observed = probe_observation_advertising(Some(1), "probe-server", vec![advertised]);
    let validated = awili_seam::validate_roots(&observed, &probe_host(), &AllowRoots)
        .expect("the probe's roots validate");
    let catalog =
        crate::mcp_catalog::project_advertised(&validated, &observed).expect("projects");
    assert_eq!(catalog.tools().len(), 1);

    let host = authority().expect("worker authority");
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-engb4").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut transport = Script {
        bodies: vec![sse_text_final("done")].into(),
    };
    let mut host_io = SeamHost {
        cancel: Arc::new(AtomicBool::new(false)),
        events: Vec::new(),
        dispatching: Vec::new(),
    };
    let mut history = ContextHistory::new();
    let report = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "what can the probe do?",
        &mut host_io,
        &mut log,
        &mut grants,
        catalog.tools(),
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        awili_seam::DEFAULT_WINDOW_TOKENS,
        "turn-engb4-0",
        &mut history,
    )
    .expect("the turn completes");
    let req: serde_json::Value = serde_json::from_str(&report.requests[0]).unwrap();
    let tools = req["tools"].as_array().expect("tools advertised");
    let probe = tools
        .iter()
        .find(|t| t["function"]["name"] == "probe_ping")
        .unwrap_or_else(|| panic!("the probe is advertised: {tools:?}"));
    assert_eq!(
        probe["function"]["description"],
        "Pings the probe: PROBE-SAYS-HELLO"
    );
}

/// RC2-R51 (usability RC2 round 4, Major 3: three of the owner's servers
/// each offer `run_command`, so Gemma's call to it was "batch refused:
/// unresolvable tool(s)"): two servers' tools of the same name are each
/// advertised by a qualified name, and a call to one reaches that server,
/// asked for its tool by its own name.
#[test]
fn rc2r51_a_name_two_servers_share_reaches_the_one_called() {
    let tool = |origin: &str| {
        awili_tools::ToolDef::from_parts(
            awili_tools::ToolOrigin::new(origin),
            "run_command",
            awili_tools::CanonicalDescriptor {
                schema: serde_json::json!({"type": "object", "properties": {}}),
                side_effect: "read_only".into(),
                required_capabilities: vec![],
                executor: origin.into(),
            },
        )
    };
    let (_coord, facade, cmds, events) = scripted_runtime(4);
    let mut roots = RootsAcceptance::default();
    accept_epoch(&facade, &mut roots, 7);
    // A call that never reaches the server fails the test (it never hangs).
    let (seen_tx, seen_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _published = cmds.recv_blocking().expect("publication");
        let (seen, ticket) = take_call(&cmds);
        events
            .send_blocking(McpEvent::CallFinished {
                ticket,
                intent_id: seen.intent_id.clone(),
                disposition: CallDisposition::Completed,
                result: ToolResultRecord {
                    awili_call_id: awili_engine::AwiliCallId(seen.awili_call_id.clone()),
                    content: "ran".into(),
                    is_error: false,
                },
            })
            .expect("emit terminal");
        let _ = seen_tx.send(seen);
    });
    let host = authority().expect("worker authority");
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-r51").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut transport = Script {
        bodies: vec![
            sse_tool_round("call_p", "probe_run_command", "{}"),
            sse_text_final("done"),
        ]
        .into(),
    };
    let mut host_io = SeamHost {
        cancel: Arc::new(AtomicBool::new(false)),
        events: Vec::new(),
        dispatching: Vec::new(),
    };
    let mut history = ContextHistory::new();
    let report = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "run it",
        &mut host_io,
        &mut log,
        &mut grants,
        &[tool("mcp:other"), tool("mcp:probe")],
        &Executors::mcp(facade.clone()),
        &awili_engine_authz::NoInstallAuthority,
        &roots,
        awili_seam::DEFAULT_WINDOW_TOKENS,
        "turn-r51-0",
        &mut history,
    )
    .expect("the turn completes");
    let seen = seen_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the call reached the server");
    assert_eq!(seen.origin, "mcp:probe");
    assert_eq!(seen.tool, "run_command", "the server's own name");
    assert_eq!(report.final_text, "done");
    let req: serde_json::Value = serde_json::from_str(&report.requests[0]).unwrap();
    let names: Vec<&str> = req["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(names.contains(&"probe_run_command"), "{names:?}");
    assert!(names.contains(&"other_run_command"), "{names:?}");
    assert!(!names.contains(&"run_command"), "{names:?}");
}
