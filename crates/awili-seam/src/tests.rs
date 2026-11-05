//! C2-u5 offline gate: the deterministic seam suite. The LIVE acceptance
//! run (real model, real edit) is the slice's second gate, recorded in
//! docs/reviews/c2-u5-design-sketch-2026-08-13.md before merge.

use super::*;
use awili_engine_log::replay::IntentStatus;
use awili_engine_log::schema::LogRecordV1;
use awili_engine_log::{test_bundle, MemLog};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

// ONE process bootstrap owns the singleton authority bundle (design P2;
// unit 4 r7: the `test-authority` fixture performed this test process's
// sole issuance). Tests take the HostAuthority through a mutex —
// authorize/resolve take &self, but serializing keeps grant-store
// interactions independent. The fixture hands out a shared reference, so
// owning the pieces is possession CLONED from it (feature-gated `Clone`),
// never a second issuance.
fn host() -> &'static Mutex<HostAuthority> {
    static HOST: OnceLock<Mutex<HostAuthority>> = OnceLock::new();
    HOST.get_or_init(|| {
        let bundle = test_bundle();
        Mutex::new(HostAuthority {
            authorizer: Authorizer::new(bundle.0.clone()),
            approval_cap: bundle.1.clone(),
            registry: bundle.2.clone(),
        })
    })
}

// -- unit 4 item 3: the immediate pre-dispatch identity check ---------------

fn descriptor() -> awili_tools::CanonicalDescriptor {
    awili_tools::CanonicalDescriptor {
        schema: serde_json::json!({"type": "object"}),
        side_effect: "read_only".into(),
        required_capabilities: vec![],
        executor: "mcp:test".into(),
    }
}

fn def_of(origin: &str, name: &str) -> awili_tools::ToolDef {
    awili_tools::ToolDef::from_parts(awili_tools::ToolOrigin::new(origin), name, descriptor())
}

fn bound_identity(origin: &str, name: &str) -> awili_engine::ToolIdentity {
    awili_engine::ToolIdentity::new(
        awili_engine::ToolOrigin::new(origin),
        name,
        awili_engine::DescriptorDigest([0u8; 32]),
    )
}

/// Unit 4 verdict r7 item 3: the registry's CURRENT resolution must EQUAL
/// the evidence-bound identity at the moment of dispatch. A re-registration
/// (here: a second origin now owning the bare name after the first
/// vanished) is a HARD error before any dispatch — never a silent switch
/// of implementation.
#[test]
fn identity_mismatch_fails_before_dispatch() {
    let mut registry = ToolRegistry::with_validator(AllowAllValidator);
    registry
        .register(def_of("mcp:server-2", "probe.ping"))
        .unwrap();
    // Evidence bound mcp:server-1's resolution; the registry now resolves
    // the same bare name to mcp:server-2.
    let bound = bound_identity("mcp:server-1", "probe.ping");
    match verify_bound_identity(&registry, "probe.ping", &bound) {
        Err(IdentityCheckError::Mismatch {
            name,
            bound,
            current,
        }) => {
            assert_eq!(name, "probe.ping");
            assert_eq!(bound, "mcp:server-1");
            assert_eq!(current, "mcp:server-2");
        }
        other => panic!("an origin switch must be a hard Mismatch, got {other:?}"),
    }
    // The same identity still passes: equality is the rule, not origin
    // string comparison against a constant.
    let current = registry.lookup_unique("probe.ping").unwrap();
    assert!(verify_bound_identity(&registry, "probe.ping", &current.identity).is_ok());
}

/// Unit 4 verdict r7 item 3: an UNAVAILABLE bound origin fails closed.
/// The registry here offers ONLY builtin:fs under the bound tool's name —
/// exactly the fallback a vanished mcp server must never trigger. Both
/// shapes are hard errors before dispatch: a same-name builtin
/// (mismatch — no builtin fallback) and no resolution at all
/// (unavailable).
#[test]
fn unavailable_origin_fails_without_builtin_fallback() {
    // Shape 1: builtin:fs now owns the bare name the mcp origin bound.
    let mut registry = ToolRegistry::with_validator(AllowAllValidator);
    fs_builtins::install(&mut registry).unwrap();
    let bound = bound_identity("mcp:server-1", "fs.read");
    match verify_bound_identity(&registry, "fs.read", &bound) {
        Err(IdentityCheckError::Mismatch { bound, current, .. }) => {
            assert_eq!(bound, "mcp:server-1");
            assert_eq!(current, "builtin:fs");
        }
        other => panic!("a vanished mcp origin must NOT fall back to builtin:fs, got {other:?}"),
    }

    // Shape 2: nothing resolves the name at all (origin removed).
    let empty = ToolRegistry::with_validator(AllowAllValidator);
    match verify_bound_identity(&empty, "fs.read", &bound) {
        Err(IdentityCheckError::OriginUnavailable { name, .. }) => {
            assert_eq!(name, "fs.read");
        }
        other => panic!("an unresolvable name must be OriginUnavailable, got {other:?}"),
    }

    // And the genuine builtin binding still dispatches: only the check's
    // equality rule is in play, not a blanket origin prohibition.
    let mut only_builtin = ToolRegistry::with_validator(AllowAllValidator);
    fs_builtins::install(&mut only_builtin).unwrap();
    let def = only_builtin.lookup_unique("fs.read").unwrap();
    assert!(verify_bound_identity(&only_builtin, "fs.read", &def.identity).is_ok());
}

// -- scripted transport ------------------------------------------------------

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

/// One assistant round emitting the given tool calls (Ollama shape: the
/// whole call in one fragment), then finish_reason tool_calls + usage.
fn sse_tool_round(calls: &[(&str, &str, &str)]) -> String {
    let mut out = String::new();
    for (i, (id, name, args)) in calls.iter().enumerate() {
        let args_esc = serde_json::to_string(args).unwrap();
        out.push_str(&chunk(&format!(
            r#"{{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{{"index":0,"delta":{{"role":"assistant","content":"","tool_calls":[{{"id":"{id}","index":{i},"type":"function","function":{{"name":"{name}","arguments":{args_esc}}}}}]}},"finish_reason":null}}]}}"#
        )));
    }
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"tool_calls"}]}"#,
    ));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":10,"total_tokens":20}}"#,
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

/// A plain final round: text deltas then finish_reason stop + usage.
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

/// A TurnHost that never approves anything and observes nothing — for the
/// unit tests that drive `step`/`feed_deltas` directly.
fn silent_host() -> impl TurnHost {
    |prompt: &str, _r: RiskClass| -> UserVerdict {
        panic!("no approval expected in this unit test: {prompt}")
    }
}

fn approve_once() -> impl FnMut(&str, RiskClass) -> UserVerdict {
    |_prompt: &str, _risk: RiskClass| UserVerdict::Approve {
        scope: "once".into(),
    }
}

fn no_dialog_expected() -> impl FnMut(&str, RiskClass) -> UserVerdict {
    |prompt: &str, _| panic!("no approval dialog expected, got: {prompt}")
}

struct Run {
    result: Result<TurnReport, SeamError>,
    records: Vec<LogRecordV1>,
}

/// Big enough that no scripted test compacts unless it asks to (the
/// compaction tests build their own small-window run).
fn awili_seam_window_for_tests() -> u64 {
    crate::DEFAULT_WINDOW_TOKENS
}

fn run(root: &tempfile::TempDir, bodies: Vec<String>, approver: &mut dyn TurnHost) -> Run {
    run_with_extras(root, bodies, approver, &[])
}

fn run_with_extras(
    root: &tempfile::TempDir,
    bodies: Vec<String>,
    approver: &mut dyn TurnHost,
    extras: &[awili_tools::ToolDef],
) -> Run {
    run_composed(
        root,
        bodies,
        approver,
        extras,
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
    )
}

/// `run_with_extras` with the composition's executor seam supplied — the
/// unit 4 item 6 entry point. Everything else is identical, so a test that
/// passes `builtin_only()` + empty roots is running the pre-item-6 path.
fn run_composed(
    root: &tempfile::TempDir,
    bodies: Vec<String>,
    approver: &mut dyn TurnHost,
    extras: &[awili_tools::ToolDef],
    executors: &Executors,
    install_authority: &dyn awili_engine_authz::InstallAuthorityQuery,
    roots: &RootsAcceptance,
) -> Run {
    let composition = Composition {
        executors,
        install_authority,
        roots,
    };
    run_in(
        root,
        bodies,
        approver,
        extras,
        &composition,
        awili_seam_window_for_tests(),
    )
}

/// RC2-ENGB4: the composition a scripted turn runs in.
struct Composition<'a> {
    executors: &'a Executors,
    install_authority: &'a dyn awili_engine_authz::InstallAuthorityQuery,
    roots: &'a RootsAcceptance,
}

/// RC2-ENGB4: [`run_with_extras`] under a given context window.
fn run_windowed(
    root: &tempfile::TempDir,
    bodies: Vec<String>,
    approver: &mut dyn TurnHost,
    extras: &[awili_tools::ToolDef],
    window_tokens: u64,
) -> Run {
    let executors = Executors::builtin_only();
    let roots = RootsAcceptance::default();
    let composition = Composition {
        executors: &executors,
        install_authority: &awili_engine_authz::NoInstallAuthority,
        roots: &roots,
    };
    run_in(root, bodies, approver, extras, &composition, window_tokens)
}

fn run_in(
    root: &tempfile::TempDir,
    bodies: Vec<String>,
    approver: &mut dyn TurnHost,
    extras: &[awili_tools::ToolDef],
    composition: &Composition<'_>,
    window_tokens: u64,
) -> Run {
    let Composition {
        executors,
        install_authority,
        roots,
    } = *composition;
    let host = host().lock().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut transport = Script {
        bodies: bodies.into(),
    };
    let mut history = ContextHistory::new();
    let result = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "edit my notes",
        approver,
        &mut log,
        &mut grants,
        extras,
        executors,
        install_authority,
        roots,
        window_tokens,
        "turn-seam-0",
        &mut history,
    );
    let (records, _) = log.read_records().unwrap();
    Run { result, records }
}

fn statuses(records: &[LogRecordV1]) -> Vec<IntentStatus> {
    classify(records)
        .iter()
        .flat_map(|t| t.intents.iter().map(|i| i.status.clone()))
        .collect()
}

fn assert_all_clean(records: &[LogRecordV1], expect_intents: usize) {
    let st = statuses(records);
    assert_eq!(st.len(), expect_intents, "intent count: {st:?}");
    for s in &st {
        assert!(
            matches!(s, IntentStatus::Clean { .. }),
            "acceptance: replay must be Clean, got {s:?}"
        );
        assert!(!matches!(s, IntentStatus::AuthzInvalid { .. }));
        assert!(!matches!(s, IntentStatus::Uncertain));
    }
}

// ---------------------------------------------------------------------------

/// The acceptance shape, offline: write → (continuation) edit+read →
/// (continuation) final text. Mutating calls go through Ask/approve-once.
#[test]
fn scripted_three_round_seam_is_clean() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[(
            "call_w",
            "fs.write",
            r#"{"path":"notes.md","contents":"hello awili"}"#,
        )]),
        sse_tool_round(&[
            (
                "call_e",
                "fs.edit",
                r#"{"path":"notes.md","find":"awili","replace":"world"}"#,
            ),
            ("call_r", "fs.read", r#"{"path":"notes.md"}"#),
        ]),
        sse_text_final("done"),
    ];
    let mut approver = approve_once();
    let r = run(&root, bodies, &mut approver);
    let report = r.result.expect("turn completes");

    // The REAL file edit happened, inside the root.
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes.md")).unwrap(),
        "hello world"
    );
    assert_eq!(report.final_text, "done");
    assert_eq!(report.rounds, 3);

    // Replay: three intents, all Clean ok=true — zero AuthzInvalid,
    // zero Uncertain (the acceptance sentence's log half).
    assert_all_clean(&r.records, 3);
    let ok_finals = r
        .records
        .iter()
        .filter(|rec| matches!(rec, LogRecordV1::DispatchFinalized { ok: true, .. }))
        .count();
    assert_eq!(ok_finals, 3);

    // The read result the model saw is the post-edit content.
    assert!(report
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolResultIngested(rec) if rec.content == "hello world")));
}

/// The continuation request is the canonical Chat Completions shape the
/// conformance tool-result-continuation scenario pins (design blocker 1).
#[test]
fn continuation_request_has_the_canonical_shape() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[("call_w", "fs.write", r#"{"path":"a.txt","contents":"x"}"#)]),
        sse_text_final("ok"),
    ];
    let mut approver = approve_once();
    let r = run(&root, bodies, &mut approver);
    let report = r.result.unwrap();
    assert_eq!(report.requests.len(), 2);

    let cont: serde_json::Value = serde_json::from_str(&report.requests[1]).unwrap();

    // Fixture pin (design blocker 1 / round-1 P1): the continuation's
    // top-level keys and message STRUCTURE match the committed capture
    // 04-tool-continuation.request.json exactly (values differ: model,
    // tools advertised, call names/args — structure may not).
    let fixture: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../awili-engine/fixtures/chat-completions/hostc-qwen3-8b/04-tool-continuation.request.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let keys =
        |v: &serde_json::Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    // RC2-GEMMA6 (a8): the continuation also carries `max_tokens` (the
    // capture has none; without it a server applies its own default).
    let mut cont_keys = keys(&cont);
    cont_keys.retain(|k| k != "max_tokens");
    assert_eq!(cont_keys, keys(&fixture), "top-level request keys");
    let fix_msgs = fixture["messages"].as_array().unwrap();
    let msgs = cont["messages"].as_array().unwrap();
    assert_eq!(
        msgs.len(),
        fix_msgs.len(),
        "user, assistant tool_calls, tool result"
    );
    for (m, f) in msgs.iter().zip(fix_msgs) {
        assert_eq!(keys(m), keys(f), "message keys for role {}", f["role"]);
        assert_eq!(m["role"], f["role"]);
    }
    assert_eq!(
        keys(&msgs[1]["tool_calls"][0]),
        keys(&fix_msgs[1]["tool_calls"][0])
    );
    assert_eq!(
        keys(&msgs[1]["tool_calls"][0]["function"]),
        keys(&fix_msgs[1]["tool_calls"][0]["function"])
    );

    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(
        msgs[1]["content"], "",
        "fixture shape: empty string, not null"
    );
    let tc = &msgs[1]["tool_calls"][0];
    assert_eq!(tc["id"], "call_w");
    assert_eq!(tc["type"], "function");
    assert_eq!(tc["function"]["name"], "fs_write");
    assert_eq!(
        tc["function"]["arguments"],
        r#"{"path":"a.txt","contents":"x"}"#
    );
    assert_eq!(msgs[2]["role"], "tool");
    assert_eq!(msgs[2]["tool_call_id"], "call_w");
    assert!(msgs[2]["content"].as_str().unwrap().contains("wrote"));
    // Tools are re-advertised on the continuation.
    assert_eq!(cont["tools"].as_array().unwrap().len(), 5);
    // The initial request MAY carry tool_choice; the continuation must NOT
    // (the fixture has none). RC2-GEMMA6: every round carries max_tokens.
    assert!(cont.get("tool_choice").is_none());
    assert_eq!(cont["max_tokens"], 2048);
}

/// Round-1 P0: a batch with any unresolvable member is refused WHOLE —
/// zero dialogs, zero intents, zero filesystem effects; every member
/// gets one synthetic error result and the turn continues.
#[test]
fn mixed_batch_with_unknown_member_touches_nothing() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[
            ("call_w", "fs.write", r#"{"path":"a.txt","contents":"x"}"#),
            ("call_u", "get_weather", r#"{"city":"Paris"}"#),
        ]),
        sse_text_final("refused"),
    ];
    let mut approver = no_dialog_expected();
    let r = run(&root, bodies, &mut approver);
    let report = r.result.expect("turn completes");
    assert!(!root.path().join("a.txt").exists(), "no filesystem effect");
    assert_all_clean(&r.records, 0);
    let errs = report
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolResultIngested(rec) if rec.is_error))
        .count();
    assert_eq!(errs, 2, "one synthetic error per batch member");
}

/// Same, with an AMBIGUOUS member (extra origin also provides fs.read).
#[test]
fn mixed_batch_with_ambiguous_member_touches_nothing() {
    use awili_tools::{CanonicalDescriptor, ToolDef, ToolOrigin};
    let imposter = ToolDef::from_parts(
        ToolOrigin::new("mcp:weather"),
        "fs.read",
        CanonicalDescriptor {
            schema: serde_json::json!({"type":"object"}),
            side_effect: "read_only".into(),
            required_capabilities: vec![],
            executor: "mcp".into(),
        },
    );
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[
            ("call_w", "fs.write", r#"{"path":"b.txt","contents":"x"}"#),
            ("call_a", "fs.read", r#"{"path":"b.txt"}"#),
        ]),
        sse_text_final("refused"),
    ];
    let mut approver = no_dialog_expected();
    let r = run_with_extras(&root, bodies, &mut approver, &[imposter]);
    r.result.expect("turn completes");
    assert!(!root.path().join("b.txt").exists(), "no filesystem effect");
    assert_all_clean(&r.records, 0);
}

/// Denial: durable AuthzDenied FIRST, then the model is told; the turn
/// completes; replay is Clean with ZERO intents (a denial never mints
/// one).
#[test]
fn denied_write_is_durable_and_the_turn_continues() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[("call_w", "fs.write", r#"{"path":"a.txt","contents":"x"}"#)]),
        sse_text_final("understood"),
    ];
    let mut deny = |_p: &str, _r: RiskClass| UserVerdict::Deny {
        reason: "not today".into(),
    };
    let r = run(&root, bodies, &mut deny);
    let report = r.result.expect("turn completes after denial");
    assert_eq!(report.final_text, "understood");
    assert!(!root.path().join("a.txt").exists(), "nothing written");
    assert_all_clean(&r.records, 0);
    assert!(
        r.records
            .iter()
            .any(|rec| matches!(rec, LogRecordV1::AuthzDenied { .. })),
        "denial must be durable"
    );
    assert!(report
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolResultIngested(rec) if rec.is_error)));
}

/// `..` traversal is a C1 SCREENING denial (design P2 correction): no
/// dialog, no intent, durable AuthzDenied, error result, turn completes.
#[test]
fn traversal_is_denied_by_c1_screening() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[("call_t", "fs.read", r#"{"path":"../secret.txt"}"#)]),
        sse_text_final("blocked"),
    ];
    let mut approver = no_dialog_expected();
    let r = run(&root, bodies, &mut approver);
    let report = r.result.expect("turn completes");
    assert_all_clean(&r.records, 0);
    assert!(r
        .records
        .iter()
        .any(|rec| matches!(rec, LogRecordV1::AuthzDenied { .. })));
    assert!(report
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolResultIngested(rec) if rec.is_error)));
}

#[cfg(unix)]
/// A C1-ALLOWED escape (symlink, invisible to textual screening) is the
/// u4 path: intent minted, C3 refuses, dispatch_finalized(ok=false),
/// replay Clean — the acceptance sentence's C3 half.
#[test]
fn symlink_escape_lands_finalized_false_and_replays_clean() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("s.txt"), b"secret").unwrap();
    let root = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path().join("s.txt"), root.path().join("link.txt")).unwrap();

    let bodies = vec![
        sse_tool_round(&[("call_s", "fs.read", r#"{"path":"link.txt"}"#)]),
        sse_text_final("refused"),
    ];
    let mut approver = no_dialog_expected(); // read_only auto-allows
    let r = run(&root, bodies, &mut approver);
    let report = r.result.expect("turn completes");

    let st = statuses(&r.records);
    assert_eq!(st.len(), 1);
    assert!(
        matches!(st[0], IntentStatus::Clean { ok: false }),
        "C3 refusal must replay Clean(ok=false), got {st:?}"
    );
    assert!(r
        .records
        .iter()
        .any(|rec| matches!(rec, LogRecordV1::DispatchFinalized { ok: false, .. })));
    assert!(report.events.iter().any(
        |e| matches!(e, AgentEvent::ToolResultIngested(rec) if rec.is_error
            && !rec.content.contains("secret"))
    ));
}

/// Unknown tool: synthetic error, no dialog, no intent (design blocker 3).
#[test]
fn unknown_tool_is_a_synthetic_error_without_intent() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[("call_u", "get_weather", r#"{"city":"Paris"}"#)]),
        sse_text_final("no such tool"),
    ];
    let mut approver = no_dialog_expected();
    let r = run(&root, bodies, &mut approver);
    let report = r.result.expect("turn completes");
    assert_all_clean(&r.records, 0);
    assert!(report.events.iter().any(
        |e| matches!(e, AgentEvent::ToolResultIngested(rec) if rec.is_error
            && rec.content.contains("unresolvable tool"))
    ));
}

/// Ambiguous bare name (an extra origin also provides fs.read):
/// lookup_unique refuses; synthetic error, no dialog, no intent.
#[test]
fn ambiguous_tool_name_is_refused_without_intent() {
    use awili_tools::{CanonicalDescriptor, ToolDef, ToolOrigin};
    let imposter = ToolDef::from_parts(
        ToolOrigin::new("mcp:weather"),
        "fs.read",
        CanonicalDescriptor {
            schema: serde_json::json!({"type":"object"}),
            side_effect: "read_only".into(),
            required_capabilities: vec![],
            executor: "mcp".into(),
        },
    );
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), b"x").unwrap();
    let bodies = vec![
        sse_tool_round(&[("call_a", "fs.read", r#"{"path":"a.txt"}"#)]),
        sse_text_final("ambiguous"),
    ];
    let mut approver = no_dialog_expected();
    let r = run_with_extras(&root, bodies, &mut approver, &[imposter]);
    let report = r.result.expect("turn completes");
    assert_all_clean(&r.records, 0);
    assert!(report
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolResultIngested(rec) if rec.is_error)));
}

/// Duplicate provider call ids: PROTOCOL failure — the turn terminates
/// audited with ZERO members dispatched (design blocker 4).
#[test]
fn duplicate_provider_ids_terminate_without_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![sse_tool_round(&[
        ("call_x", "fs.read", r#"{"path":"a.txt"}"#),
        ("call_x", "fs.read", r#"{"path":"b.txt"}"#),
    ])];
    let mut approver = no_dialog_expected();
    let r = run(&root, bodies, &mut approver);
    match r.result {
        Err(SeamError::TurnFailed { reason, .. }) => {
            assert!(reason.contains("duplicate provider call id"), "{reason}")
        }
        other => panic!("expected TurnFailed, got {other:?}"),
    }
    assert_all_clean(&r.records, 0); // zero intents — nothing dispatched
    assert!(
        r.records
            .iter()
            .any(|rec| matches!(rec, LogRecordV1::TurnClosed { .. })),
        "protocol failure still closes/audits the turn"
    );
}

/// Batch validation unit coverage: empty and missing-id (reducer can't
/// produce these today; the host boundary still rejects them).
#[test]
fn validate_batch_rejects_empty_and_missing_ids() {
    use awili_engine::events::AwiliCallId;
    assert!(validate_batch(&[]).is_err());
    let call = ToolCall {
        awili_call_id: AwiliCallId("c1".into()),
        provider_call_id: None,
        name: "fs.read".into(),
        arguments: "{}".into(),
    };
    let err = validate_batch(std::slice::from_ref(&call)).unwrap_err();
    assert!(err.contains("no provider call id"));
}

/// Storage honesty (design blocker 5, round-1 P0/P1): B2 failures at
/// each adopted boundary abort typed, the wrapper best-effort CLOSES the
/// turn (audited termination), and replay tells the truth — never a
/// synthesized success.
#[test]
fn storage_failure_matrix_aborts_honestly_and_closes() {
    // (marker, occurrence, expected statuses after the fault)
    enum Expect {
        NoIntents,
        NeverDispatched,
        Uncertain,
        OneCleanThenNothing,
    }
    let cases: &[(&str, usize, Expect)] = &[
        // pre/at-intent: the intent append itself fails → no intent.
        ("\"intent\"", 0, Expect::NoIntents),
        // post-intent / at-start: started append fails → NeverDispatched.
        ("\"dispatch_started\"", 0, Expect::NeverDispatched),
        // post-start / at-finalize: finalized append fails → Uncertain.
        ("\"dispatch_finalized\"", 0, Expect::Uncertain),
        // mid-continuation: round 2's intent fails → round 1 stays
        // Clean, round 2 never exists.
        ("\"intent\"", 1, Expect::OneCleanThenNothing),
    ];
    let bodies = || {
        vec![
            sse_tool_round(&[("call_w", "fs.write", r#"{"path":"n.md","contents":"x"}"#)]),
            sse_tool_round(&[(
                "call_e",
                "fs.edit",
                r#"{"path":"n.md","find":"x","replace":"y"}"#,
            )]),
            sse_text_final("ok"),
        ]
    };

    // Clean probe run: map each (marker, occurrence) to its append index.
    // Frame index == append index for this suite (each record is one
    // append; no torn writes; text coalescing flushes are records too).
    let probe_root = tempfile::tempdir().unwrap();
    let mut approver = approve_once();
    let probe = run(&probe_root, bodies(), &mut approver);
    probe.result.expect("probe run completes");
    let frames: Vec<String> = probe
        .records
        .iter()
        .map(|rec| serde_json::to_string(rec).unwrap())
        .collect();

    for (marker, occurrence, expect) in cases {
        let idx = frames
            .iter()
            .enumerate()
            .filter(|(_, f)| f.contains(marker))
            .map(|(i, _)| i)
            .nth(*occurrence)
            .unwrap_or_else(|| panic!("{marker}#{occurrence} not found in probe"));

        let host = host().lock().unwrap();
        let mut mem = MemLog::new();
        mem.fail_on_append = Some(idx as u64);
        let mut log = ExecutionLog::open(mem, "sess-seam-fault").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
        let mut transport = Script {
            bodies: bodies().into(),
        };
        let mut approver = approve_once();
        let root = tempfile::tempdir().unwrap();
        let result = run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "edit my notes",
            &mut approver,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            awili_seam_window_for_tests(),
            "turn-seam-0",
            &mut ContextHistory::new(),
        );
        drop(host);
        match result {
            Err(SeamError::Storage(_)) => {}
            other => panic!("{marker}#{occurrence}: expected Storage error, got {other:?}"),
        }
        let (records, _) = log.read_records().unwrap();
        // Audited termination (round-1 P0): the wrapper closed the turn.
        assert!(
            records
                .iter()
                .any(|rec| matches!(rec, LogRecordV1::TurnClosed { .. })),
            "{marker}#{occurrence}: turn must be closed on the failure path"
        );
        let st = statuses(&records);
        assert!(
            !st.iter()
                .any(|s| matches!(s, IntentStatus::Clean { ok: true })
                    && matches!(expect, Expect::NoIntents | Expect::NeverDispatched)),
            "{marker}#{occurrence}: no synthesized success"
        );
        match expect {
            Expect::NoIntents => assert!(st.is_empty(), "{marker}#{occurrence}: {st:?}"),
            Expect::NeverDispatched => {
                assert_eq!(st.len(), 1, "{marker}#{occurrence}: {st:?}");
                assert!(
                    matches!(st[0], IntentStatus::NeverDispatched { .. }),
                    "{st:?}"
                );
            }
            Expect::Uncertain => {
                assert_eq!(st.len(), 1, "{marker}#{occurrence}: {st:?}");
                assert!(matches!(st[0], IntentStatus::Uncertain), "{st:?}");
            }
            Expect::OneCleanThenNothing => {
                assert_eq!(st.len(), 1, "{marker}#{occurrence}: {st:?}");
                assert!(matches!(st[0], IntentStatus::Clean { ok: true }), "{st:?}");
            }
        }
    }
}

/// Determinism through the seam (PS-ENG-02): identical scripted inputs
/// produce identical event traces.
#[test]
fn scripted_turn_is_deterministic() {
    let bodies = || {
        vec![
            sse_tool_round(&[("call_w", "fs.write", r#"{"path":"d.md","contents":"same"}"#)]),
            sse_text_final("done"),
        ]
    };
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let mut ap_a = approve_once();
    let mut ap_b = approve_once();
    let a = run(&root_a, bodies(), &mut ap_a).result.unwrap();
    let b = run(&root_b, bodies(), &mut ap_b).result.unwrap();
    assert_eq!(a.events, b.events, "seam must be deterministic");
    assert_eq!(a.final_text, b.final_text);
}

/// Round-2 P1 pin: `step()` writes InputRejected to B2 FIRST, then
/// fails typed. Driven directly against a terminal state — the loop
/// never produces this, which is exactly why it needs a direct pin.
#[test]
fn input_rejected_is_recorded_then_failed() {
    use awili_engine::events::AwiliCallId;
    use awili_engine::reducer::Input;
    use awili_engine::state::TurnState;
    let mut log = ExecutionLog::open(MemLog::new(), "sess-rejected").unwrap();
    log.turn_opened("t-rej", "test", "m").unwrap();
    let mut state = TurnState::new("test", "m", "t-rej");
    let mut trace = Vec::new();
    // Drive to terminal: text, finish stop, stream end.
    for raw in [sse_text_final("hi")] {
        for delta in awili_engine::codec::chat_completions::decode_stream(&raw) {
            let _ = step(
                &mut state,
                Input::Delta(delta),
                &mut log,
                "t-rej",
                &mut trace,
                &mut silent_host(),
            )
            .unwrap();
        }
    }
    assert!(state.is_terminal());
    // A tool result after terminal: reducer audits InputRejected.
    let late = awili_engine::events::ToolResultRecord {
        awili_call_id: AwiliCallId("late".into()),
        content: "x".into(),
        is_error: false,
    };
    let err = step(
        &mut state,
        Input::ToolResult(late),
        &mut log,
        "t-rej",
        &mut trace,
        &mut silent_host(),
    )
    .unwrap_err();
    assert!(matches!(err, SeamError::Harness(_)), "{err:?}");
    // Recorded BEFORE the failure: the audit record exists in B2 …
    let (records, _) = log.read_records().unwrap();
    let json = records
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        json.contains("input_rejected") || json.contains("InputRejected"),
        "{json}"
    );
    // … and in the trace.
    assert!(trace
        .iter()
        .any(|e| matches!(e, AgentEvent::InputRejected { .. })));
}

/// Round-2 P1 pin: a second terminal action in one delta sequence is a
/// typed harness failure. `decode_stream` stops at the first
/// `data: [DONE]` (also pinned here), so the guard is exercised against
/// a handcrafted delta sequence via `feed_deltas` directly.
#[test]
fn second_terminal_action_is_a_harness_failure() {
    use awili_engine::wire::{FinishReason, WireDelta};
    // Pin the decode property that makes the guard defensive-only:
    let doubled = format!("{}{}", sse_text_final("a"), sse_text_final("b"));
    let deltas = awili_engine::codec::chat_completions::decode_stream(&doubled);
    let stream_ends = deltas
        .iter()
        .filter(|d| matches!(d, WireDelta::StreamEnd))
        .count();
    assert_eq!(stream_ends, 1, "decode stops at the first [DONE]");

    // And pin the guard itself against a handcrafted sequence: a second
    // StreamEnd after the terminal produces a second Done.
    let mut log = ExecutionLog::open(MemLog::new(), "sess-double").unwrap();
    log.turn_opened("t-dbl", "test", "m").unwrap();
    let mut state = awili_engine::state::TurnState::new("test", "m", "t-dbl");
    let mut trace = Vec::new();
    let seq = vec![
        WireDelta::Text("a".into()),
        WireDelta::Finish {
            choice: 0,
            reason: FinishReason::Stop,
        },
        WireDelta::StreamEnd,
        WireDelta::StreamEnd,
    ];
    let err = feed_deltas(
        &mut state,
        seq,
        &mut log,
        "t-dbl",
        &mut trace,
        &mut silent_host(),
    )
    .unwrap_err();
    match err {
        SeamError::Harness(msg) => assert!(msg.contains("two terminal actions"), "{msg}"),
        other => panic!("expected Harness error, got {other:?}"),
    }
}

/// Round-2 P1 pin: per-round assistant text. Both tool rounds carry
/// text; each continuation's assistant message carries ONLY its own
/// round's text (the cumulative-text regression would repeat round 1's
/// text in round 2's message).
#[test]
fn continuation_assistant_text_is_per_round() {
    fn sse_text_then_tool(text: &str, id: &str, name: &str, args: &str) -> String {
        let text_esc = serde_json::to_string(text).unwrap();
        let args_esc = serde_json::to_string(args).unwrap();
        let mut out = chunk(&format!(
            r#"{{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{{"index":0,"delta":{{"role":"assistant","content":{text_esc}}},"finish_reason":null}}]}}"#
        ));
        out.push_str(&chunk(&format!(
            r#"{{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{{"index":0,"delta":{{"role":"assistant","content":"","tool_calls":[{{"id":"{id}","index":0,"type":"function","function":{{"name":"{name}","arguments":{args_esc}}}}}]}},"finish_reason":null}}]}}"#
        )));
        out.push_str(&chunk(
            r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"tool_calls"}]}"#,
        ));
        out.push_str(&chunk(
            r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":10,"total_tokens":20}}"#,
        ));
        out.push_str("data: [DONE]\n\n");
        out
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), b"x").unwrap();
    let bodies = vec![
        sse_text_then_tool("ROUND-ONE", "call_1", "fs.read", r#"{"path":"a.txt"}"#),
        sse_text_then_tool("ROUND-TWO", "call_2", "fs.read", r#"{"path":"a.txt"}"#),
        sse_text_final(" done"),
    ];
    let mut approver = no_dialog_expected();
    let r = run(&root, bodies, &mut approver);
    let report = r.result.expect("turn completes");
    assert_eq!(report.requests.len(), 3);

    let cont2: serde_json::Value = serde_json::from_str(&report.requests[2]).unwrap();
    let msgs = cont2["messages"].as_array().unwrap();
    // user, assistant(r1), tool(r1), assistant(r2), tool(r2)
    assert_eq!(msgs.len(), 5);
    assert_eq!(msgs[1]["content"], "ROUND-ONE");
    assert_eq!(
        msgs[3]["content"], "ROUND-TWO",
        "round 2's assistant message must not repeat round 1's text"
    );
    assert_all_clean(&r.records, 2);
}

/// C4 P0-3: the continuation request's `messages` array is pinned by FULL
/// JSON equality — every key, every value, byte-for-byte through serde_json
/// equality — not just structure. The tool content is captured from the
/// executed builtin (its wording is fs.write's, not this test's concern);
/// everything else is literal.
#[test]
fn continuation_messages_pinned_by_full_json_equality() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[("call_w", "fs.write", r#"{"path":"a.txt","contents":"x"}"#)]),
        sse_text_final("ok"),
    ];
    let mut approver = approve_once();
    let r = run(&root, bodies, &mut approver);
    let report = r.result.unwrap();
    let cont: serde_json::Value = serde_json::from_str(&report.requests[1]).unwrap();
    let tool_content = cont["messages"][2]["content"].clone();
    assert!(
        tool_content.as_str().unwrap().contains("wrote"),
        "fs.write result content: {tool_content}"
    );
    let expected = serde_json::json!([
        {"role": "user", "content": "edit my notes"},
        {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_w",
                "type": "function",
                "function": {
                    "name": "fs_write",
                    "arguments": r#"{"path":"a.txt","contents":"x"}"#
                }
            }]
        },
        {"role": "tool", "tool_call_id": "call_w", "content": tool_content}
    ]);
    assert_eq!(cont["messages"], expected, "full-JSON pin (C4 P0-3)");
}

/// C4 P0-3: a tool call whose provider id is missing cannot be encoded into
/// a continuation — typed `Context` failure, never a silent `""` id. Driven
/// through the PUBLIC seam surface: a pre-seeded history whose exchange
/// carries a call with `provider_call_id: None`.
#[test]
fn missing_provider_call_id_is_a_typed_encode_failure() {
    let host = host().lock().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut transport = Script {
        bodies: VecDeque::new(),
    };
    let mut approver = approve_once();
    let mut history = ContextHistory::new();
    history.push_user("earlier question");
    history
        .push_exchange(
            "earlier answer",
            vec![awili_engine::events::ToolCall {
                awili_call_id: awili_engine::events::AwiliCallId("awili-1".into()),
                provider_call_id: None,
                name: "fs.read".into(),
                arguments: "{}".into(),
            }],
            vec![ToolResultRecord {
                awili_call_id: awili_engine::events::AwiliCallId("awili-1".into()),
                content: "r".into(),
                is_error: false,
            }],
        )
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let result = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "new question",
        &mut approver,
        &mut log,
        &mut grants,
        &[],
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        awili_seam_window_for_tests(),
        "turn-seam-0",
        &mut history,
    );
    drop(host);
    match result {
        Err(SeamError::Context(msg)) => {
            assert!(msg.contains("provider_call_id"), "{msg}");
        }
        other => panic!("expected Context error, got {other:?}"),
    }
}

/// C4 gate 9 (scripted half): a second turn over a history holding a large
/// first turn, under a window too small to fit both — the seam gets a
/// deterministic CompactionPlan, obtains the summary from the SAME scripted
/// endpoint, applies it immutably, and completes the turn with the summary
/// (not the raw old text) on the wire.
#[test]
fn second_turn_forces_compaction_and_completes_on_the_summary() {
    let host = host().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut history = ContextHistory::new();
    let long_text = "the first answer was long ".repeat(400); // ~10400 chars

    // Turn 1: plain long answer, generous window.
    {
        let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
        let mut transport = Script {
            bodies: vec![sse_text_final(&long_text)].into(),
        };
        let mut approver = no_dialog_expected();
        let report = run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "first question",
            &mut approver,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            crate::DEFAULT_WINDOW_TOKENS,
            "turn-seam-0",
            &mut history,
        )
        .expect("turn 1 completes");
        assert_eq!(report.requests.len(), 1);
    }

    // Compute a window that forces exactly one compaction cycle: current
    // total + reserved(2048) + margin(512) minus a shove.
    let tools_json: Vec<serde_json::Value> = awili_tools::builtins::defs()
        .iter()
        .map(|d| {
            awili_tools::openai_tool_project_described(&d.descriptor, d.name(), &d.description)
        })
        .collect();
    let probe = seam_profile(
        "test-model",
        crate::DEFAULT_WINDOW_TOKENS,
        &serde_json::Value::Array(tools_json.clone()).to_string(),
    );
    // The probe history includes what turn 2 will add (its user message) —
    // approximate with the current groups plus a small allowance; the exact
    // number only needs to land between "doesn't fit" and "fits after one
    // summary", which the long first turn guarantees by a wide margin.
    let AssembleOutcome::Fits(a) = assemble(&history, &probe, &seam_counter()).unwrap() else {
        panic!("probe must fit under the default window")
    };
    // budget = window - reserved(2048) - margin(512); set it just UNDER the
    // already-present tokens so turn 2 cannot fit without compacting.
    let window = a.tokens + 2048 + 512 - 16;

    // Turn 2: the script serves the SUMMARIZER response first, then the
    // actual turn-2 completion.
    let report2 = {
        let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
        let mut transport = Script {
            bodies: vec![
                sse_text_final("SUMMARY-OF-THE-FIRST-TURN"),
                sse_text_final("done2"),
            ]
            .into(),
        };
        let mut approver = no_dialog_expected();
        run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "second question needs headroom",
            &mut approver,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            window,
            "turn-seam-0",
            &mut history,
        )
        .expect("turn 2 completes after compaction")
    };
    drop(host);

    // Evidence: request 0 is the summarize call over canonical material;
    // request 1 is the real turn carrying the summary, NOT the raw old text.
    assert_eq!(report2.requests.len(), 2, "{:?}", report2.requests.len());
    assert!(
        report2.requests[0].contains("Summarize the following working context"),
        "first request must be the summarizer"
    );
    assert!(report2.requests[0].contains("the first answer was long"));
    let turn2_req: serde_json::Value = serde_json::from_str(&report2.requests[1]).unwrap();
    let msgs = serde_json::to_string(&turn2_req["messages"]).unwrap();
    assert!(
        msgs.contains("[context summary] SUMMARY-OF-THE-FIRST-TURN"),
        "summary on the wire: {msgs}"
    );
    assert!(
        !msgs.contains("the first answer was long"),
        "raw old text must be replaced by the summary"
    );
    // The history was compacted immutably: one Summary group present, the
    // old exchange gone, the new turn's groups appended after.
    assert!(history.groups().iter().any(
        |g| matches!(g, ContextGroup::Summary { text, .. } if text == "SUMMARY-OF-THE-FIRST-TURN")
    ));
}

/// C4 round-1 P1, amended by RC1-N-FIX2 (lead, whole-slice R2 P1): a FAILED
/// turn with NO completed tool call leaves the session history exactly as it
/// found it — no user message, no partial text, no applied compaction. A
/// turn that fails AFTER a tool call ran keeps that completed work (the
/// request and the exchange) so the next turn does not repeat it; only the
/// unfinished tail (the failed continuation) is dropped.
#[test]
fn failed_turn_restores_session_history() {
    for tool_ran in [false, true] {
        let host = host().lock().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut history = ContextHistory::new();
        history.push_user("prior turn");
        history
            .push_exchange("prior answer", vec![], vec![])
            .unwrap();
        let before = history.clone();
        // With `tool_ran`, the script serves ONE tool round then exhausts —
        // the continuation's transport call fails mid-turn, after the write
        // ran. Without it, the very first request fails.
        let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
        let bodies = if tool_ran {
            vec![sse_tool_round(&[(
                "call_w",
                "fs.write",
                r#"{"path":"a.txt","contents":"x"}"#,
            )])]
        } else {
            vec![]
        };
        let mut transport = Script {
            bodies: bodies.into(),
        };
        let mut approver = approve_once();
        let result = run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "doomed turn",
            &mut approver,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            awili_seam_window_for_tests(),
            "turn-seam-0",
            &mut history,
        );
        drop(host);
        assert!(matches!(result, Err(SeamError::Transport(_))), "{result:?}");
        if !tool_ran {
            assert_eq!(
                history, before,
                "failed turn contaminated the session history"
            );
            continue;
        }
        assert!(root.path().join("a.txt").exists(), "the write ran");
        let groups = history.groups();
        assert_eq!(groups.len(), before.groups().len() + 2, "{groups:?}");
        assert!(
            matches!(&groups[groups.len() - 2], ContextGroup::User { text, .. } if text == "doomed turn")
        );
        assert!(matches!(
            &groups[groups.len() - 1],
            ContextGroup::Exchange { calls, results, .. }
                if calls.len() == 1 && calls[0].awili_call_id.0 == results[0].awili_call_id.0
        ));
    }
}

/// C4 round-1 P1: a truncated summarizer stream is a typed failure and the
/// history keeps its raw groups — partial text never becomes canonical.
#[test]
fn truncated_summarizer_rejected_and_history_untouched() {
    let host = host().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut history = ContextHistory::new();
    let long_text = "the first answer was long ".repeat(400);
    {
        let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
        let mut transport = Script {
            bodies: vec![sse_text_final(&long_text)].into(),
        };
        let mut approver = no_dialog_expected();
        run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "first question",
            &mut approver,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            crate::DEFAULT_WINDOW_TOKENS,
            "turn-seam-0",
            &mut history,
        )
        .expect("turn 1 completes");
    }
    let before = history.clone();

    let tools_json: Vec<serde_json::Value> = awili_tools::builtins::defs()
        .iter()
        .map(|d| {
            awili_tools::openai_tool_project_described(&d.descriptor, d.name(), &d.description)
        })
        .collect();
    let probe = seam_profile(
        "test-model",
        crate::DEFAULT_WINDOW_TOKENS,
        &serde_json::Value::Array(tools_json).to_string(),
    );
    let AssembleOutcome::Fits(a) = assemble(&history, &probe, &seam_counter()).unwrap() else {
        panic!("probe must fit")
    };
    let window = a.tokens + 2048 + 512 - 16;

    // Truncated summarizer: text but NO [DONE] and no finish.
    let truncated = chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":"PARTIAL"},"finish_reason":null}]}"#,
    );
    let result = {
        let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
        let mut transport = Script {
            bodies: vec![truncated].into(),
        };
        let mut approver = no_dialog_expected();
        run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "second question",
            &mut approver,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            window,
            "turn-seam-0",
            &mut history,
        )
    };
    drop(host);
    match result {
        Err(SeamError::Context(msg)) => assert!(msg.contains("truncated"), "{msg}"),
        other => panic!("expected Context(truncated), got {other:?}"),
    }
    assert_eq!(
        history, before,
        "partial summary must not touch the history"
    );
    assert!(
        !history
            .groups()
            .iter()
            .any(|g| matches!(g, ContextGroup::Summary { .. })),
        "no summary group from a truncated stream"
    );
}

/// C4 round-2 P1: text after Finish(Stop) is a malformed stream — rejected,
/// and the history keeps its raw groups.
#[test]
fn post_finish_summarizer_text_rejected_and_history_untouched() {
    let host = host().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut history = ContextHistory::new();
    let long_text = "the first answer was long ".repeat(400);
    {
        let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
        let mut transport = Script {
            bodies: vec![sse_text_final(&long_text)].into(),
        };
        let mut approver = no_dialog_expected();
        run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "first question",
            &mut approver,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            crate::DEFAULT_WINDOW_TOKENS,
            "turn-seam-0",
            &mut history,
        )
        .expect("turn 1 completes");
    }
    let before = history.clone();
    let tools_json: Vec<serde_json::Value> = awili_tools::builtins::defs()
        .iter()
        .map(|d| {
            awili_tools::openai_tool_project_described(&d.descriptor, d.name(), &d.description)
        })
        .collect();
    let probe = seam_profile(
        "test-model",
        crate::DEFAULT_WINDOW_TOKENS,
        &serde_json::Value::Array(tools_json).to_string(),
    );
    let AssembleOutcome::Fits(a) = assemble(&history, &probe, &seam_counter()).unwrap() else {
        panic!("probe must fit")
    };
    let window = a.tokens + 2048 + 512 - 16;

    // good text → Finish(Stop) → POISON text → clean [DONE].
    let mut poisoned = chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":"good"},"finish_reason":null}]}"#,
    );
    poisoned.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"stop"}]}"#,
    ));
    poisoned.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":"poison"},"finish_reason":null}]}"#,
    ));
    poisoned.push_str("data: [DONE]\n\n");
    let result = {
        let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
        let mut transport = Script {
            bodies: vec![poisoned].into(),
        };
        let mut approver = no_dialog_expected();
        run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "second question",
            &mut approver,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            window,
            "turn-seam-0",
            &mut history,
        )
    };
    drop(host);
    match result {
        Err(SeamError::Context(msg)) => assert!(msg.contains("after Finish"), "{msg}"),
        other => panic!("expected Context(after Finish), got {other:?}"),
    }
    assert_eq!(history, before);
}

/// D2-u1 P0-4 / Codex gate 5: the observer sees an event only AFTER its
/// durable append succeeded. When the B2 write fails, the surface must have
/// been told NOTHING about that event — a UI can never show what the log
/// does not carry.
#[test]
fn b2_append_failure_emits_nothing_to_the_observer() {
    /// Records what the surface was told, and how many appends the log took.
    struct Recorder {
        seen: Vec<String>,
    }
    impl TurnHost for Recorder {
        fn approve(&mut self, _call_id: &str, prompt: &str, _r: RiskClass) -> ApprovalOutcome {
            panic!("no approval expected: {prompt}")
        }
        fn observe(&mut self, event: &AgentEvent) -> Result<(), ObserverGone> {
            self.seen.push(format!("{event:?}"));
            Ok(())
        }
    }

    let bodies = || vec![sse_text_final("hello")];

    // Probe: how many events does a clean run observe, and in what order?
    let host = host().lock().unwrap();
    let probe_root = tempfile::tempdir().unwrap();
    let mut probe_rec = Recorder { seen: Vec::new() };
    {
        let mut log = ExecutionLog::open(MemLog::new(), "sess-probe").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
        let mut transport = Script {
            bodies: bodies().into(),
        };
        run_turn(
            &host,
            &mut transport,
            "test-model",
            probe_root.path().to_str().unwrap(),
            "hi",
            &mut probe_rec,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            crate::DEFAULT_WINDOW_TOKENS,
            "turn-obs-probe",
            &mut ContextHistory::new(),
        )
        .expect("probe completes");
    }
    let clean_seen = probe_rec.seen.clone();
    assert!(
        clean_seen.len() >= 3,
        "probe should observe TurnStarted + text + terminal: {clean_seen:?}"
    );

    // Now fail the append that carries the SECOND observed event. The
    // observer must stop one short — it never sees the event whose durable
    // write failed.
    for fail_at in 1..clean_seen.len() {
        let mut mem = MemLog::new();
        // Frame 0 is the log header; event appends follow the turn_opened
        // record, so locate the failing append by matching the probe's
        // event ORDER rather than a raw index: fail the (fail_at+2)th append
        // (header + turn_opened precede the first event).
        mem.fail_on_append = Some((fail_at + 2) as u64);
        let mut log = ExecutionLog::open(mem, "sess-obs-fault").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
        let mut transport = Script {
            bodies: bodies().into(),
        };
        let mut rec = Recorder { seen: Vec::new() };
        let root = tempfile::tempdir().unwrap();
        let result = run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "hi",
            &mut rec,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            crate::DEFAULT_WINDOW_TOKENS,
            "turn-obs-fault",
            &mut ContextHistory::new(),
        );
        assert!(
            matches!(result, Err(SeamError::Storage(_))),
            "fail_at={fail_at}: expected a storage abort, got {result:?}"
        );
        assert!(
            rec.seen.len() < clean_seen.len(),
            "fail_at={fail_at}: the observer must not see every event of a run that aborted"
        );
        assert_eq!(
            rec.seen,
            clean_seen[..rec.seen.len()],
            "fail_at={fail_at}: observed events must be a PREFIX of the clean run — \
             durable append happens first, so nothing unlogged is ever emitted"
        );
    }
    drop(host);
}

/// D2-u1 round-1 P0: a cancel that lands between the entry check and the
/// policy-denial branch must NOT leave a durable denial behind. The race is
/// controlled: the host trips the flag from inside `cancelled()` itself, so
/// the first (entry) check passes and the pre-denial check fails.
#[test]
fn cancel_racing_a_policy_denial_records_nothing() {
    /// `cancelled()` answers false ONCE (the entry check), then true — a
    /// deterministic stand-in for a flag tripped by the UI in between.
    struct RacingHost {
        checks: std::cell::Cell<usize>,
    }
    impl TurnHost for RacingHost {
        fn approve(&mut self, _c: &str, prompt: &str, _r: RiskClass) -> ApprovalOutcome {
            panic!("a DENIED call must never open a dialog: {prompt}")
        }
        fn cancelled(&self) -> bool {
            let n = self.checks.get();
            self.checks.set(n + 1);
            n > 0
        }
    }

    let host = host().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::new(), "sess-deny-race").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
    // An absolute path outside the session root: screened to Deny by C1.
    let escape = r#"{"path":"/etc/hosts"}"#;
    let mut transport = Script {
        bodies: vec![sse_tool_round(&[("c1", "fs.read", escape)])].into(),
    };
    let mut racing = RacingHost {
        checks: std::cell::Cell::new(0),
    };
    let result = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "read outside the root",
        &mut racing,
        &mut log,
        &mut grants,
        &[],
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        crate::DEFAULT_WINDOW_TOKENS,
        "turn-deny-race",
        &mut ContextHistory::new(),
    );
    drop(host);
    assert!(
        matches!(result, Err(SeamError::Cancelled(StopCause::Cancelled))),
        "expected a cancel stop, got {result:?}"
    );
    let (records, _) = log.read_records().unwrap();
    let dump = records
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !dump.contains("denied") && !dump.contains("Denied"),
        "no denial may be durably recorded once the turn is cancelling:\n{dump}"
    );
    assert!(
        dump.contains("cancelled"),
        "the turn is audited as cancelled:\n{dump}"
    );
}

/// D2-u1 round-1 P1: each call in a batch gets its OWN approval id — the
/// seam hands `approve` the call's `awili_call_id`.
#[test]
fn each_call_in_a_batch_gets_its_own_approval_id() {
    struct IdCollector {
        ids: Vec<String>,
    }
    impl TurnHost for IdCollector {
        fn approve(&mut self, call_id: &str, _p: &str, _r: RiskClass) -> ApprovalOutcome {
            self.ids.push(call_id.to_string());
            ApprovalOutcome::Verdict(UserVerdict::Approve {
                scope: "once".into(),
            })
        }
    }

    let host = host().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::new(), "sess-two-approvals").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
    let mut transport = Script {
        bodies: vec![
            sse_tool_round(&[
                ("call_a", "fs.write", r#"{"path":"a.txt","contents":"a"}"#),
                ("call_b", "fs.write", r#"{"path":"b.txt","contents":"b"}"#),
            ]),
            sse_text_final("both written"),
        ]
        .into(),
    };
    let mut collector = IdCollector { ids: Vec::new() };
    run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "write two files",
        &mut collector,
        &mut log,
        &mut grants,
        &[],
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        crate::DEFAULT_WINDOW_TOKENS,
        "turn-two-approvals",
        &mut ContextHistory::new(),
    )
    .expect("turn completes");
    drop(host);
    assert_eq!(collector.ids.len(), 2, "two dialogs: {:?}", collector.ids);
    assert_ne!(
        collector.ids[0], collector.ids[1],
        "each call needs its own approval id: {:?}",
        collector.ids
    );
    assert!(root.path().join("a.txt").exists() && root.path().join("b.txt").exists());
}

/// D2-u1 round-2 P0: a cancel that lands WHILE the model request is in
/// flight must stop the turn even when the response is a plain text
/// completion. The transport trips the flag from inside `stream`, exactly
/// modelling "the user cancelled while we were waiting on the model".
#[test]
fn cancel_during_the_request_stops_a_text_only_response() {
    struct FlagOnStream {
        cancelled: Arc<AtomicBool>,
        body: String,
    }
    impl Transport for FlagOnStream {
        fn stream(&mut self, _request_body: &str) -> Result<String, SeamError> {
            // The request "took a while"; the user hit Cancel meanwhile.
            self.cancelled.store(true, Ordering::SeqCst);
            Ok(self.body.clone())
        }
    }
    struct FlagHost {
        cancelled: Arc<AtomicBool>,
        observed: usize,
    }
    impl TurnHost for FlagHost {
        fn approve(&mut self, _c: &str, p: &str, _r: RiskClass) -> ApprovalOutcome {
            panic!("no dialog expected: {p}")
        }
        fn observe(&mut self, _e: &AgentEvent) -> Result<(), ObserverGone> {
            self.observed += 1;
            Ok(())
        }
        fn cancelled(&self) -> bool {
            self.cancelled.load(Ordering::SeqCst)
        }
    }

    let host = host().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let mut log = ExecutionLog::open(MemLog::new(), "sess-inflight-cancel").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
    let mut transport = FlagOnStream {
        cancelled: flag.clone(),
        body: sse_text_final("this answer arrived after the user cancelled"),
    };
    let mut flag_host = FlagHost {
        cancelled: flag.clone(),
        observed: 0,
    };
    let result = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "slow question",
        &mut flag_host,
        &mut log,
        &mut grants,
        &[],
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        crate::DEFAULT_WINDOW_TOKENS,
        "turn-inflight-cancel",
        &mut ContextHistory::new(),
    );
    drop(host);
    assert!(
        matches!(result, Err(SeamError::Cancelled(StopCause::Cancelled))),
        "an in-flight cancel must stop the turn, got {result:?}"
    );
    assert_eq!(
        flag_host.observed, 0,
        "the response must not be reduced or surfaced after the cancel"
    );
    let (records, _) = log.read_records().unwrap();
    let dump = records
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        dump.contains("cancelled"),
        "the turn is audited as cancelled:\n{dump}"
    );
    assert!(
        !dump.contains("TurnCompleted"),
        "no completed turn may be recorded:\n{dump}"
    );
}

// ============================================================================
// Tool projection — every REGISTERED tool must also be ADVERTISED
// ============================================================================

/// An `extra_tools` entry must reach the model, not just the registry.
///
/// Regression pin for a latent bug found 2026-08-13: `run_turn` registered
/// `extra_tools` (so they would execute if called) but built the wire `tools`
/// array from `fs_builtins::defs()` alone, so a non-builtin tool was INVISIBLE
/// to the model. Every caller passed `&[]`, so nothing exercised it.
///
/// This matters more than an ordinary omission because of how it fails: the
/// server connects, reports healthy, and the model simply never calls it —
/// which reads as a broken transport rather than a missing schema. The
/// mac-shell MCP slice depends entirely on this path.
#[test]
fn extra_tools_are_advertised_to_the_model_not_just_registered() {
    let host = host().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut history = ContextHistory::new();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-extra").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut transport = Script {
        bodies: vec![sse_text_final("ok")].into(),
    };
    let mut approver = no_dialog_expected();

    let extra = awili_tools::ToolDef::from_parts(
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
    );

    let report = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "hello",
        &mut approver,
        &mut log,
        &mut grants,
        std::slice::from_ref(&extra),
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        crate::DEFAULT_WINDOW_TOKENS,
        "turn-extra-0",
        &mut history,
    )
    .expect("turn completes");

    let body = report.requests.first().expect("one request was sent");
    assert!(
        body.contains("probe_ping"),
        "the extra tool must appear in the request the model sees:\n{body}"
    );
    // And the builtins are still advertised alongside it.
    assert!(
        body.contains("fs_read"),
        "projecting extras must not displace the builtins:\n{body}"
    );
}

// ============================================================================
// Unit 4 item 6 — the executor seam
//
// The binding item-6 tests do NOT live here. They are in
// `awili-engine-worker/src/executor_seam_test.rs`, included at THAT crate's
// root, for two reasons the c6 review made binding: their bare names must be
// their full paths (`cargo test <name> -- --exact`), and they must traverse
// the REAL coordinator and the production `McpExecutor` adapter rather than a
// fake that fabricates outcomes. A fake here could only prove the seam agrees
// with itself.
// ============================================================================

/// Gate row 21 (unit 4b, design r9 §4.3): a LIVE-BIND refusal — no
/// accepted roots record for the origin — happens BEFORE the durable
/// intent. The refused call mints no ticket and writes NO durable record:
/// nothing happened that replay could need, and the executor is provably
/// never reached.
#[test]
fn live_bind_refusal_before_intent_writes_no_durable_record_and_mints_no_ticket() {
    struct NeverDispatch;
    impl McpExecutor for NeverDispatch {
        fn dispatch_call(
            &self,
            _dispatch: McpDispatch,
        ) -> Result<Box<dyn McpCall + '_>, McpSubmitRefused> {
            panic!("a bind-refused call must never reach the executor")
        }
        fn publish_roots(
            &self,
            _origin: awili_engine::ToolOrigin,
            _generation: awili_engine::generation::ConnectionGeneration,
            _epoch: u64,
        ) -> Result<(), awili_tools::runtime::RefusalKind> {
            Ok(())
        }
    }

    let bundle = test_bundle();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-row21").unwrap();
    let call = ToolCall {
        awili_call_id: awili_engine::AwiliCallId("c21".into()),
        provider_call_id: None,
        name: "probe".into(),
        arguments: "{}".into(),
    };
    let evidence = bundle
        .0
        .policy_evidence(
            &bundle.2.mint(awili_engine::ToolIdentity::new(
                awili_engine::ToolOrigin::new("mcp:ghost"),
                "probe",
                awili_engine::DescriptorDigest([0u8; 32]),
            )),
            "turn-21",
            &call,
            RiskClass::ReadOnly,
            None,
        )
        .unwrap();

    let roots = RootsAcceptance::default(); // NO record: the bind refuses.
    let baseline = log.read_records().unwrap().0.len();
    let result = super::dispatch_mcp(
        &NeverDispatch,
        awili_engine::RiskClass::Privileged,
        &roots,
        Duration::from_secs(1),
        &mut log,
        "turn-21",
        &call,
        evidence,
        &mut silent_host(),
    )
    .expect("a bind refusal is a carried result, not a turn error");
    assert!(result.is_error);
    assert!(result.content.contains("unavailable"), "{}", result.content);

    let (records, _) = log.read_records().unwrap();
    assert_eq!(
        records.len(),
        baseline,
        "no intent, no dispatch_started, no finalize — NOTHING durable: {records:?}"
    );
}

// -- HttpTransport auth-header rule (mirrors awili-providers) ---------------

/// A non-empty engine key attaches `Authorization: Bearer <key>` (and the
/// Content-Type header stays as before). Builds the request without sending
/// — `.build()` performs no IO — so the rule is pinned offline. The key
/// value is an obvious synthetic, never a real credential.
#[test]
fn http_transport_bearer_attached_only_for_non_empty_key() {
    let t = HttpTransport::with_api_key(
        "http://127.0.0.1:9",
        Duration::from_secs(1),
        Some("sk-test-000".to_string()),
    );
    let built = t.post_request("{}").build().expect("request builds");
    assert_eq!(
        built
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_string()),
        Some("Bearer sk-test-000".to_string())
    );
    assert_eq!(
        built
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_string()),
        Some("application/json".to_string())
    );

    // No credential (the self-hosted default) ⇒ no Authorization header.
    let none = HttpTransport::new("http://127.0.0.1:9", Duration::from_secs(1));
    let built = none.post_request("{}").build().expect("request builds");
    assert_eq!(built.headers().get("authorization"), None);

    // An EMPTY string is treated as absent — never `Bearer ` with nothing.
    let empty = HttpTransport::with_api_key(
        "http://127.0.0.1:9",
        Duration::from_secs(1),
        Some(String::new()),
    );
    let built = empty.post_request("{}").build().expect("request builds");
    assert_eq!(built.headers().get("authorization"), None);
}

/// A key still written as `secret:<account>` was never resolved from the
/// keychain: it is never sent as a bearer, and the stream refuses it.
#[test]
fn http_transport_refuses_an_unresolved_secret_reference() {
    let mut t = HttpTransport::with_api_key(
        "http://127.0.0.1:9",
        Duration::from_secs(1),
        Some("secret:engine".to_string()),
    );
    let built = t.post_request("{}").build().expect("request builds");
    assert_eq!(built.headers().get("authorization"), None);
    let err = t.stream("{}").unwrap_err().to_string();
    assert!(err.contains("unresolved secret"), "{err}");
}

// ---------------------------------------------------------------------------
// U4b1: Ask-before-changes at the seam — the auto-approve hook and its
// floors. `U4Host` switches the dialog off (`auto`) and records every
// prompt that DID reach the dialog (`asked`).

/// U4b1 (D7): a host whose `auto_approve` reports the shared flag. Its
/// `approve` records the prompt and returns Approve-once — or Deny when
/// the `deny` flag is set.
struct U4Host {
    auto: bool,
    deny: bool,
    asked: Vec<String>,
    root: std::path::PathBuf,
}

impl TurnHost for U4Host {
    fn approve(&mut self, _id: &str, prompt: &str, _r: RiskClass) -> ApprovalOutcome {
        self.asked.push(prompt.to_string());
        // The dialog MUST block before the write: the target file does not
        // exist while the user is deciding.
        assert!(
            !self.root.join("a.txt").exists(),
            "a.txt must not exist while the dialog is up"
        );
        if self.deny {
            ApprovalOutcome::Verdict(UserVerdict::Deny {
                reason: "denied by the user".into(),
            })
        } else {
            ApprovalOutcome::Verdict(UserVerdict::Approve {
                scope: "once".into(),
            })
        }
    }
    fn auto_approve(&mut self, _id: &str, _prompt: &str, _r: RiskClass) -> bool {
        self.auto
    }
}

fn u4_write_bodies() -> Vec<String> {
    vec![
        sse_tool_round(&[("call_w", "fs.write", r#"{"path":"a.txt","contents":"x"}"#)]),
        sse_text_final("done"),
    ]
}

/// Ask mode: the dialog is called exactly once, the write happens only
/// AFTER the verdict, and the record is a plain (non-auto) approval.
#[test]
fn u4b1_ask_blocks_the_write_until_the_verdict() {
    let root = tempfile::tempdir().unwrap();
    let mut host = U4Host {
        auto: false,
        deny: false,
        asked: Vec::new(),
        root: root.path().to_path_buf(),
    };
    let r = run(&root, u4_write_bodies(), &mut host);
    r.result.expect("turn completes");
    assert_eq!(host.asked.len(), 1, "the dialog must run exactly once");
    assert!(
        !host.asked[0].contains("auto_mode"),
        "the dialog prompt names the call, not the auto signature"
    );
    assert!(root.path().join("a.txt").exists(), "approved write landed");
    assert!(
        r.records
            .iter()
            .filter(|rec| matches!(rec, LogRecordV1::ApprovalRecorded { .. }))
            .count()
            == 1,
        "one approval record"
    );
    assert!(
        r.records.iter().all(|rec| !matches!(
            rec,
            LogRecordV1::ApprovalRecorded {
                constraints, ..
            } if constraints.contains("auto_mode")
        )),
        "ask-mode approval must NOT carry the auto signature"
    );
    assert_all_clean(&r.records, 1);
}

/// Ask mode + deny: nothing is written, the denial is durable, the turn
/// completes.
#[test]
fn u4b1_ask_deny_leaves_the_file_untouched() {
    let root = tempfile::tempdir().unwrap();
    let mut host = U4Host {
        auto: false,
        deny: true,
        asked: Vec::new(),
        root: root.path().to_path_buf(),
    };
    let r = run(&root, u4_write_bodies(), &mut host);
    r.result.expect("turn completes after denial");
    assert_eq!(host.asked.len(), 1, "the dialog must run");
    assert!(!root.path().join("a.txt").exists(), "nothing written");
    assert!(
        r.records
            .iter()
            .any(|rec| matches!(rec, LogRecordV1::AuthzDenied { .. })),
        "denial must be durable"
    );
    assert_all_clean(&r.records, 0);
}

/// Auto mode: a non-floor write never reaches the dialog — the approval
/// record carries the auto signature and the file lands.
#[test]
fn u4b1_auto_writes_without_a_dialog() {
    let root = tempfile::tempdir().unwrap();
    let mut host = U4Host {
        auto: true,
        deny: false,
        asked: Vec::new(),
        root: root.path().to_path_buf(),
    };
    let r = run(&root, u4_write_bodies(), &mut host);
    r.result.expect("turn completes");
    assert!(host.asked.is_empty(), "auto mode: no dialog");
    assert!(
        root.path().join("a.txt").exists(),
        "auto-approved write landed"
    );
    let autos: Vec<_> = r
        .records
        .iter()
        .filter(|rec| {
            matches!(
                rec,
                LogRecordV1::ApprovalRecorded {
                    constraints, ..
                } if constraints.contains("auto_mode")
            )
        })
        .collect();
    assert_eq!(autos.len(), 1, "one auto-signed ApprovalRecorded");
    assert_all_clean(&r.records, 1);
}

/// Auto mode does NOT clear a secrets floor: `.env` still reaches the
/// dialog, and the prompt names the file.
#[test]
fn u4b1_auto_still_asks_for_a_secret_file() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[("call_w", "fs.write", r#"{"path":".env","contents":"x"}"#)]),
        sse_text_final("done"),
    ];
    let mut host = U4Host {
        auto: true,
        deny: false,
        asked: Vec::new(),
        root: root.path().to_path_buf(),
    };
    let r = run(&root, bodies, &mut host);
    r.result.expect("turn completes");
    assert_eq!(host.asked.len(), 1, "the floor forces the dialog");
    assert!(
        host.asked[0].contains(".env"),
        "the dialog prompt names the secret file"
    );
    assert!(
        r.records.iter().all(|rec| !matches!(
            rec,
            LogRecordV1::ApprovalRecorded {
                constraints, ..
            } if constraints.contains("auto_mode")
        )),
        "a floor approval is a real user dialog record"
    );
    assert_all_clean(&r.records, 1);
}

/// Auto mode does NOT clear the outside-folder read floor.
#[test]
fn u4b1_auto_still_asks_for_a_read_outside_the_folder() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[("call_r", "fs.read", r#"{"path":"/etc/hosts"}"#)]),
        sse_text_final("done"),
    ];
    let mut host = U4Host {
        auto: true,
        deny: false,
        asked: Vec::new(),
        root: root.path().to_path_buf(),
    };
    let r = run(&root, bodies, &mut host);
    r.result.expect("turn completes");
    assert_eq!(host.asked.len(), 1, "outside-folder read forces the dialog");
}

/// Closure hosts never auto-approve (the blanket impl's default is
/// `false`): an `approve_once()` closure behaves exactly as before — one
/// plain approval record.
#[test]
fn u4b1_closure_hosts_never_auto_approve() {
    let root = tempfile::tempdir().unwrap();
    let mut approver = approve_once();
    let r = run(&root, u4_write_bodies(), &mut approver);
    r.result.expect("turn completes");
    assert!(root.path().join("a.txt").exists());
    let approvals: Vec<_> = r
        .records
        .iter()
        .filter(|rec| matches!(rec, LogRecordV1::ApprovalRecorded { .. }))
        .collect();
    assert_eq!(approvals.len(), 1, "one approval record");
    assert!(
        approvals.iter().all(|rec| !matches!(
            rec,
            LogRecordV1::ApprovalRecorded {
                constraints, ..
            } if constraints.contains("auto_mode")
        )),
        "closure-host approval must not carry the auto signature"
    );
    assert_all_clean(&r.records, 1);
}

// ============================================================================
// RC1-F5: tool names on the OpenAI-compatible wire
// ============================================================================

/// Every function name in a request body (tools and prior tool_calls).
fn rc1f5_wire_names_in(body: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    let mut out = Vec::new();
    for t in v["tools"].as_array().unwrap() {
        out.push(t["function"]["name"].as_str().unwrap().to_string());
    }
    for m in v["messages"].as_array().unwrap() {
        for c in m["tool_calls"].as_array().into_iter().flatten() {
            out.push(c["function"]["name"].as_str().unwrap().to_string());
        }
    }
    out
}

fn rc1f5_is_wire_safe(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[test]
fn rc1f5_wire_names_are_safe_unique_and_reversible() {
    use crate::wire_names::{wire_safe, WireNames};
    assert_eq!(wire_safe("fs.read"), "fs_read");
    assert_eq!(wire_safe("a b/c-d_e"), "a_b_c-d_e");
    let names = [
        "fs.read",
        "fs.write",
        "probe.ping",
        "a.b",
        "a_b",
        "x y",
        "x_y",
    ];
    let map = WireNames::new(names);
    let mut seen = std::collections::BTreeSet::new();
    for n in names {
        let w = map.wire(n);
        assert!(rc1f5_is_wire_safe(&w), "{n} -> {w}");
        assert!(seen.insert(w.clone()), "{w} used twice");
        assert_eq!(map.canonical(&w), n, "{n} -> {w} -> back");
    }
    // An already-safe name keeps itself; the colliding one takes a suffix.
    assert_eq!(map.wire("a_b"), "a_b");
    assert_eq!(map.wire("a.b"), "a_b_2");
    assert_eq!(map.wire("x y"), "x_y_2");
    assert_eq!(map.wire("fs.read"), "fs_read");
    // Deterministic whatever the tool order.
    let mut rev = names;
    rev.reverse();
    assert_eq!(WireNames::new(rev), map);
    // A name the request never advertised comes back unchanged.
    assert_eq!(map.canonical("fs_delete"), "fs_delete");
    assert_eq!(map.canonical("fs.read"), "fs.read");
}

/// The live failure (Studio mlx_lm, Gemma 4): the model must be offered
/// wire-safe names, and its call `fs_write` must run Awili's `fs.write`,
/// with the canonical name in the approval prompt and in the log.
#[test]
fn rc1f5_requests_carry_wire_names_and_a_wire_call_dispatches_canonically() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[(
            "call_w",
            "fs_write",
            r#"{"path":"answer.txt","contents":"heliotrope"}"#,
        )]),
        sse_text_final("done"),
    ];
    let mut prompts: Vec<String> = Vec::new();
    let mut approver = |prompt: &str, _r: RiskClass| {
        prompts.push(prompt.to_string());
        UserVerdict::Approve {
            scope: "once".into(),
        }
    };
    let r = run(&root, bodies, &mut approver);
    let report = r.result.expect("turn completes");
    assert_eq!(
        std::fs::read_to_string(root.path().join("answer.txt")).unwrap(),
        "heliotrope",
        "the wire call ran fs.write"
    );
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].starts_with("fs.write "),
        "canonical in the prompt: {prompts:?}"
    );
    assert_eq!(report.requests.len(), 2);
    for body in &report.requests {
        let names = rc1f5_wire_names_in(body);
        assert!(!names.is_empty());
        for n in &names {
            assert!(rc1f5_is_wire_safe(n), "{n} in {body}");
        }
        assert!(names.contains(&"fs_read".to_string()), "{names:?}");
    }
    let cont: serde_json::Value = serde_json::from_str(&report.requests[1]).unwrap();
    assert_eq!(
        cont["messages"][1]["tool_calls"][0]["function"]["name"], "fs_write",
        "the echoed call is in wire form too"
    );
    let tools: Vec<String> = r
        .records
        .iter()
        .filter_map(|rec| match rec {
            LogRecordV1::Intent { tool, .. } => Some(tool.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(tools, ["fs.write"], "the log keeps the canonical name");
    assert_all_clean(&r.records, 1);
}

/// A wire name the request never offered is still an unknown tool: no
/// dialog, no intent, no file, and the turn goes on.
#[test]
fn rc1f5_an_unknown_wire_name_is_still_refused() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[("call_x", "fs_delete", r#"{"path":"a.txt"}"#)]),
        sse_text_final("ok"),
    ];
    std::fs::write(root.path().join("a.txt"), b"keep").unwrap();
    let mut approver = no_dialog_expected();
    let r = run(&root, bodies, &mut approver);
    r.result.expect("turn completes");
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "keep"
    );
    assert_eq!(
        statuses(&r.records).len(),
        0,
        "no intent for an unknown tool"
    );
}

/// An extra tool already named `fs_read` keeps that wire name; the builtin
/// `fs.read` is offered as `fs_read_2`, and a call to it reads the file.
#[test]
fn rc1f5_a_name_collision_is_disambiguated_deterministically() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("notes.txt"), b"heliotrope").unwrap();
    let extra = def_of("mcp:other", "fs_read");
    // RC1-R-FIX1: the builtin keeps `fs_read` (the in-app brief names it);
    // the other tool already called `fs_read` takes `fs_read_2`.
    let bodies = vec![
        sse_tool_round(&[("call_r", "fs_read", r#"{"path":"notes.txt"}"#)]),
        sse_text_final("ok"),
    ];
    let mut approver = no_dialog_expected();
    let r = run_with_extras(&root, bodies, &mut approver, std::slice::from_ref(&extra));
    let report = r.result.expect("turn completes");
    let names = rc1f5_wire_names_in(&report.requests[0]);
    assert!(names.contains(&"fs_read".to_string()), "{names:?}");
    assert!(names.contains(&"fs_read_2".to_string()), "{names:?}");
    let cont: serde_json::Value = serde_json::from_str(&report.requests[1]).unwrap();
    assert!(
        cont["messages"][2]["content"]
            .as_str()
            .unwrap()
            .contains("heliotrope"),
        "fs_read ran the builtin fs.read: {}",
        cont["messages"][2]
    );
    assert_all_clean(&r.records, 1);
}

#[test]
fn builtin_registry_alias_executes_when_mcp_has_same_name() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("notes.txt"), b"collision fixture").unwrap();
    let extra = def_of("mcp:other", "fs.read");
    let bodies = vec![
        sse_tool_round(&[("call_r", "fs_fs_read", r#"{"path":"notes.txt"}"#)]),
        sse_text_final("ok"),
    ];
    let mut approver = no_dialog_expected();
    let r = run_with_extras(&root, bodies, &mut approver, std::slice::from_ref(&extra));
    let report = r.result.expect("turn completes");
    let names = rc1f5_wire_names_in(&report.requests[0]);
    assert!(names.contains(&"fs_fs_read".to_string()), "{names:?}");
    let continuation: serde_json::Value = serde_json::from_str(&report.requests[1]).unwrap();
    assert!(continuation["messages"][2]["content"].as_str().unwrap().contains("collision fixture"));
    assert_all_clean(&r.records, 1);
}

/// RC1-F4b-FIX1: the HTTP transport posts to exactly `chat_completions_url`
/// (the string the app classifies the engine's locality from).
#[test]
fn rc1f4b_fix1_transport_posts_to_chat_completions_url() {
    for base in [
        "http://10.77.68.94:8844",
        "http://10.77.68.94:8844/",
        "http://evil.com\\@10.77.1.2:8844",
        "https://api.example.com/v1/",
    ] {
        let t = crate::HttpTransport::new(base, std::time::Duration::from_secs(1));
        assert_eq!(t.url, crate::chat_completions_url(base), "{base}");
        let req = t.post_request("{}").build().unwrap();
        assert_eq!(
            req.url().as_str(),
            reqwest::Url::parse(&crate::chat_completions_url(base))
                .unwrap()
                .as_str(),
            "{base}"
        );
    }
    assert_eq!(
        crate::chat_completions_url("http://h:1/"),
        "http://h:1/v1/chat/completions"
    );
}

// ============================================================================
// RC1-F-SLICE-FIX1b: the engine never follows a redirect to another host
// ============================================================================

/// A one-request-per-connection HTTP server on 127.0.0.1. `respond(n, path)`
/// gives the raw response for the n-th request; the counter is returned.
fn rc1f_slice_server(
    respond: impl Fn(usize, &str) -> String + Send + 'static,
) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap_or(0);
            let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            let n = seen.fetch_add(1, Ordering::SeqCst);
            let _ = stream.write_all(respond(n, &path).as_bytes());
        }
    });
    (port, hits)
}

fn rc1f_slice_redirect(to: &str) -> String {
    format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

fn rc1f_slice_ok(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Slice review P2 (ad1907ca): a home endpoint that answers 307 to another
/// host: the request fails with a plain error and the other host is never
/// contacted. A redirect on the SAME host is followed.
#[test]
fn rc1f_slice_fix1b_engine_never_follows_a_redirect_to_another_host() {
    let reply = sse_text_final("hi");
    let (cloud_port, cloud_hits) = rc1f_slice_server(move |_, _| rc1f_slice_ok("{}"));
    let (home_port, home_hits) = rc1f_slice_server(move |_, _| {
        rc1f_slice_redirect(&format!(
            "http://localhost:{cloud_port}/v1/chat/completions"
        ))
    });
    let mut t = HttpTransport::new(
        &format!("http://127.0.0.1:{home_port}"),
        std::time::Duration::from_secs(5),
    );
    let err = t
        .stream("{}")
        .expect_err("a redirect to another host fails");
    assert!(
        matches!(&err, SeamError::Transport(m) if m.contains("another host")),
        "{err:?}"
    );
    assert_eq!(home_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        cloud_hits.load(Ordering::SeqCst),
        0,
        "the other host is never contacted"
    );

    // Same host (127.0.0.1), another port and path: followed.
    let body = reply.clone();
    let (next_port, next_hits) = rc1f_slice_server(move |_, _| rc1f_slice_ok(&body));
    let (first_port, _) = rc1f_slice_server(move |_, _| {
        rc1f_slice_redirect(&format!("http://127.0.0.1:{next_port}/v2/chat/completions"))
    });
    let mut t = HttpTransport::new(
        &format!("http://127.0.0.1:{first_port}"),
        std::time::Duration::from_secs(5),
    );
    assert_eq!(t.stream("{}").expect("same-host redirect followed"), reply);
    assert_eq!(next_hits.load(Ordering::SeqCst), 1);
}

/// A same-host redirect loop stops at the bound with an error.
#[test]
fn rc1f_slice_fix1b_redirect_loop_is_bounded() {
    let (port, hits) = rc1f_slice_server(move |n, _| rc1f_slice_redirect(&format!("/loop/{n}")));
    let mut t = HttpTransport::new(
        &format!("http://127.0.0.1:{port}"),
        std::time::Duration::from_secs(5),
    );
    let err = t.stream("{}").expect_err("a redirect loop fails");
    assert!(
        matches!(&err, SeamError::Transport(m) if m.contains("too many redirects")),
        "{err:?}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), MAX_ENGINE_REDIRECTS + 1);
}

/// RC1-N (usability round 4, M3: Gemma's turn failed "context: cannot fit:
/// 37571 tokens needed after maximal compaction, budget 30208" when a tool
/// returned the owner's 63k-character memory): a tool result larger than the
/// window can still hold is cut to fit, ends with Awili's plain marker, and
/// the turn completes — for any tool and any model. The log keeps the full
/// result.
#[test]
fn rc1n_an_oversized_tool_result_is_cut_to_fit_and_the_turn_goes_on() {
    let host = host().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let big: String = (0..150_000)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    std::fs::write(root.path().join("big.txt"), &big).unwrap();
    let mut history = ContextHistory::new();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut transport = Script {
        bodies: vec![
            sse_tool_round(&[("call_r", "fs.read", r#"{"path":"big.txt"}"#)]),
            sse_text_final("read it"),
        ]
        .into(),
    };
    let mut approver = no_dialog_expected();
    let report = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "read big.txt",
        &mut approver,
        &mut log,
        &mut grants,
        &[],
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        32_768,
        "turn-seam-0",
        &mut history,
    )
    .expect("the turn completes with the result cut to fit");
    drop(host);
    assert_eq!(report.requests.len(), 2);
    let second: serde_json::Value = serde_json::from_str(&report.requests[1]).unwrap();
    let tool_msg = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the tool result went back to the model")["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        tool_msg.contains("[Cut by Awili to fit the model's context:"),
        "{}",
        &tool_msg[tool_msg.len().saturating_sub(200)..]
    );
    assert!(tool_msg.len() < big.len(), "cut");
    assert!(
        tool_msg.starts_with(&big[..1000]),
        "the start is kept verbatim"
    );
    // The whole request fits the window the profile allows.
    let counter = seam_counter();
    assert!(
        counter.count_text(&report.requests[1]) < 32_768,
        "the next request fits"
    );
    // The durable log kept the full result.
    let (records, _) = log.read_records().unwrap();
    assert!(records.iter().any(|r| matches!(
        r,
        LogRecordV1::DispatchFinalized { content, .. } if content.len() >= big.len()
    )));
}

/// RC1-N item 3: a host that asks the seam to stop after the owner's Deny.
struct DenyStopHost {
    asked: usize,
    stop: bool,
}

impl TurnHost for DenyStopHost {
    fn approve(&mut self, _id: &str, _prompt: &str, _r: RiskClass) -> ApprovalOutcome {
        self.asked += 1;
        ApprovalOutcome::Verdict(UserVerdict::Deny {
            reason: "denied by the user".into(),
        })
    }
    fn stop_after_denial(&self) -> bool {
        self.stop
    }
}

/// RC1-N item 3: the owner's Deny on the Work surface records the denial
/// durably, then ends the turn as `StopCause::Denied` — no further model
/// request, nothing written, the session history left as it was, and replay
/// still Clean with zero intents. A host that does not ask for the stop keeps
/// the old behaviour (the model is told and answers).
#[test]
fn rc1n_deny_stop_records_the_denial_and_ends_the_turn() {
    for stop in [true, false] {
        let host = host().lock().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
        let mut transport = Script {
            bodies: u4_write_bodies().into(),
        };
        let mut history = ContextHistory::new();
        let before = format!("{history:?}");
        let mut approver = DenyStopHost { asked: 0, stop };
        let result = run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "write a.txt",
            &mut approver,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            awili_seam_window_for_tests(),
            "turn-seam-0",
            &mut history,
        );
        drop(host);
        assert_eq!(approver.asked, 1, "the card was shown once");
        assert!(!root.path().join("a.txt").exists(), "nothing written");
        let (records, _) = log.read_records().unwrap();
        assert!(
            records
                .iter()
                .any(|rec| matches!(rec, LogRecordV1::AuthzDenied { .. })),
            "the denial is durable"
        );
        assert_all_clean(&records, 0);
        if stop {
            assert!(
                matches!(result, Err(SeamError::Cancelled(StopCause::Denied))),
                "{result:?}"
            );
            assert_eq!(
                transport.bodies.len(),
                1,
                "no second model request after the Deny"
            );
            assert_eq!(format!("{history:?}"), before, "history rolled back");
            assert!(records.iter().any(|rec| matches!(
                rec,
                LogRecordV1::TurnClosed { outcome, .. } if outcome == "cancelled"
            )));
        } else {
            result.expect("without the stop the model is told and the turn completes");
            assert!(transport.bodies.is_empty(), "the model answered the denial");
        }
    }
}

/// RC1-N (pre-review P2): the unresolved-tool batch is fitted too. Forty
/// calls to long unknown names give forty refusals that each list every
/// name (about 400,000 characters together); on a 32K window the turn still
/// goes on, and the next request fits.
#[test]
fn rc1n_an_unresolved_batch_is_fitted_too() {
    let host = host().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..40)
        .map(|i| format!("no_such_tool_{i:02}_{}", "x".repeat(220)))
        .collect();
    let ids: Vec<String> = (0..40).map(|i| format!("call_u{i}")).collect();
    let calls: Vec<(&str, &str, &str)> = ids
        .iter()
        .zip(&names)
        .map(|(id, n)| (id.as_str(), n.as_str(), "{}"))
        .collect();
    let mut history = ContextHistory::new();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut transport = Script {
        bodies: vec![sse_tool_round(&calls), sse_text_final("sorry")].into(),
    };
    let mut approver = no_dialog_expected();
    let report = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "do things",
        &mut approver,
        &mut log,
        &mut grants,
        &[],
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        32_768,
        "turn-seam-0",
        &mut history,
    )
    .expect("the turn goes on with the refusals cut to fit");
    drop(host);
    assert_eq!(report.requests.len(), 2);
    assert!(report.requests[1].contains("[Cut by Awili"));
    assert!(seam_counter().count_text(&report.requests[1]) < 32_768);
}

/// RC1-N (pre-review P1): a compaction summary longer than the room the
/// budget assumed for it is cut to that room, so the turn assembles after
/// one compaction cycle. The window here fits a summary of the assumed size
/// and not the 6,000-character one the summarizer returns.
#[test]
fn rc1n_a_long_summary_is_cut_to_its_room() {
    let host = host().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut history = ContextHistory::new();
    let long_text = "the first answer was long ".repeat(400);
    {
        let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
        let mut transport = Script {
            bodies: vec![sse_text_final(&long_text)].into(),
        };
        let mut approver = no_dialog_expected();
        run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "first question",
            &mut approver,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            crate::DEFAULT_WINDOW_TOKENS,
            "turn-seam-0",
            &mut history,
        )
        .expect("turn 1 completes");
    }
    let tools_json = serde_json::Value::Array(
        awili_tools::builtins::defs()
            .iter()
            .map(|d| {
                awili_tools::openai_tool_project_described(&d.descriptor, d.name(), &d.description)
            })
            .collect(),
    )
    .to_string();
    // The window that holds turn 2 with a summary of exactly the assumed
    // size (1,024 tokens: 3,072 characters) and no more.
    let ask = "second question needs headroom";
    let mut probe = history.clone();
    probe.push_user(ask);
    let big = seam_profile("test-model", crate::DEFAULT_WINDOW_TOKENS, &tools_json);
    let AssembleOutcome::Fits(full) = assemble(&probe, &big, &seam_counter()).unwrap() else {
        panic!("fits the default window")
    };
    let small = seam_profile("test-model", full.tokens + 2048 + 512 - 16, &tools_json);
    let AssembleOutcome::NeedsCompaction(plan) = assemble(&probe, &small, &seam_counter()).unwrap()
    else {
        panic!("the small window needs compaction")
    };
    let summarized =
        awili_engine::context::apply_summary(&probe, &plan.summarize_ids, "s".repeat(3_072))
            .unwrap();
    let AssembleOutcome::Fits(after) = assemble(&summarized, &big, &seam_counter()).unwrap() else {
        panic!("fits")
    };
    let window = after.tokens + 2048 + 512;
    let report = {
        let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
        let mut transport = Script {
            bodies: vec![sse_text_final(&"S".repeat(6_000)), sse_text_final("done2")].into(),
        };
        let mut approver = no_dialog_expected();
        run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            ask,
            &mut approver,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            window,
            "turn-seam-1",
            &mut history,
        )
        .expect("the long summary is cut and the turn completes")
    };
    drop(host);
    assert_eq!(report.requests.len(), 2);
    assert!(
        report.requests[1].contains("SSSS\u{2026}"),
        "the summary was cut"
    );
}

/// RC1-N: a transport answering each request from a script of results
/// (`Err` = the model server's error).
struct Answers {
    answers: VecDeque<Result<String, SeamError>>,
    seen: Vec<String>,
}

impl Transport for Answers {
    fn stream(&mut self, request_body: &str) -> Result<String, SeamError> {
        self.seen.push(request_body.to_string());
        self.answers
            .pop_front()
            .unwrap_or_else(|| Err(SeamError::Transport("script exhausted".into())))
    }
}

fn run_answers(
    root: &tempfile::TempDir,
    answers: Vec<Result<String, SeamError>>,
    log: &mut ExecutionLog<MemLog>,
    approver: &mut dyn TurnHost,
) -> (Result<TurnReport, SeamError>, Vec<String>) {
    let mut history = ContextHistory::new();
    run_answers_into(root, answers, log, approver, &mut history, "turn-seam-0")
}

/// [`run_answers`] on the caller's `history` (RC1-N-FIX2).
fn run_answers_into(
    root: &tempfile::TempDir,
    answers: Vec<Result<String, SeamError>>,
    log: &mut ExecutionLog<MemLog>,
    approver: &mut dyn TurnHost,
    history: &mut ContextHistory,
    turn_id: &str,
) -> (Result<TurnReport, SeamError>, Vec<String>) {
    let host = host().lock().unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut transport = Answers {
        answers: answers.into(),
        seen: Vec::new(),
    };
    let result = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "go",
        approver,
        log,
        &mut grants,
        &[],
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        32_768,
        turn_id,
        history,
    );
    (result, transport.seen)
}

/// RC1-N (pre-review P1, "any model"): when the model server refuses a
/// request as longer than its context although Awili's count said it fit,
/// the newest results shrink by half and the request goes again, while they
/// can still shrink; the turn completes. Once only markers are left, the
/// refusal is the turn's failure.
#[test]
fn rc1n_a_refused_request_is_resent_with_smaller_results() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("big.txt"), "y".repeat(40_000)).unwrap();
    let read = || {
        Ok(sse_tool_round(&[(
            "call_r",
            "fs.read",
            r#"{"path":"big.txt"}"#,
        )]))
    };
    let refused = || {
        Err(SeamError::Transport(
            "HTTP 400: the request exceeds the available context size".into(),
        ))
    };
    let tool_len = |body: &str| -> usize {
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap().len())
            .unwrap()
    };
    // Accepted only after four reductions.
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut answers = vec![read()];
    answers.extend((0..4).map(|_| refused()));
    answers.push(Ok(sse_text_final("read it")));
    let (result, seen) = run_answers(&root, answers, &mut log, &mut no_dialog_expected());
    result.expect("the turn completes once the results are small enough");
    assert_eq!(seen.len(), 6);
    let lens: Vec<usize> = seen[1..].iter().map(|b| tool_len(b)).collect();
    assert!(lens.windows(2).all(|w| w[1] < w[0]), "{lens:?}");
    assert!(seen[5].contains("[Cut by Awili"));
    // Refused at every size: the resends stop when only the marker is left
    // and the refusal is the turn's error.
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut answers = vec![read()];
    answers.extend((0..40).map(|_| refused()));
    let (result, seen) = run_answers(&root, answers, &mut log, &mut no_dialog_expected());
    assert!(
        matches!(&result, Err(SeamError::Transport(m)) if context_overflow_text(m)),
        "{result:?}"
    );
    assert!(seen.len() < 15, "{}", seen.len());
    let last: serde_json::Value = serde_json::from_str(seen.last().unwrap()).unwrap();
    let tool = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(tool.contains("[Cut by Awili"), "the marker stays: {tool:?}");
    // Pre-review P1: the refusal as an HTTP error BODY (decoded to a
    // provider error) is resent the same way.
    let body_refusal = || {
        Ok(r#"{"error":{"message":"This model's maximum context length is 32768 tokens","type":"invalid_request_error"}}"#.to_string())
    };
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let (result, seen) = run_answers(
        &root,
        vec![read(), body_refusal(), Ok(sse_text_final("read it"))],
        &mut log,
        &mut no_dialog_expected(),
    );
    result.expect("the turn completes after the body refusal");
    assert_eq!(seen.len(), 3);
    assert!(tool_len(&seen[2]) < tool_len(&seen[1]));
    // Pre-review P1 (pass 7): Awili's own engine wording from a server.
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let (result, seen) = run_answers(
        &root,
        vec![
            read(),
            Ok(r#"{"error":{"message":"context: cannot fit: 37571 tokens needed after maximal compaction, budget 30208"}}"#.to_string()),
            Ok(sse_text_final("read it")),
        ],
        &mut log,
        &mut no_dialog_expected(),
    );
    result.expect("the turn completes after the cannot-fit refusal");
    assert_eq!(seen.len(), 3);
    // Pre-review P1 (pass 5): two batches; the server accepts only once the
    // EARLIER result is down to almost nothing too — more than twelve
    // resends, the newest results first, then the earlier ones.
    std::fs::write(root.path().join("first.txt"), "q".repeat(40_000)).unwrap();
    std::fs::write(root.path().join("second.txt"), "w".repeat(40_000)).unwrap();
    struct Picky {
        script: VecDeque<String>,
        seen: Vec<String>,
    }
    impl Transport for Picky {
        fn stream(&mut self, body: &str) -> Result<String, SeamError> {
            self.seen.push(body.to_string());
            if body.contains(&"q".repeat(20)) && self.seen.len() > 2 {
                return Err(SeamError::Transport(
                    "HTTP 400: the request exceeds the available context size".into(),
                ));
            }
            self.script
                .pop_front()
                .ok_or_else(|| SeamError::Transport("script exhausted".into()))
        }
    }
    let host = host().lock().unwrap();
    let mut picky = Picky {
        script: vec![
            sse_tool_round(&[("call_1", "fs.read", r#"{"path":"first.txt"}"#)]),
            sse_tool_round(&[("call_2", "fs.read", r#"{"path":"second.txt"}"#)]),
            sse_text_final("both read"),
        ]
        .into(),
        seen: Vec::new(),
    };
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut history = ContextHistory::new();
    let result = run_turn(
        &host,
        &mut picky,
        "test-model",
        root.path().to_str().unwrap(),
        "go",
        &mut no_dialog_expected(),
        &mut log,
        &mut grants,
        &[],
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        32_768,
        "turn-seam-0",
        &mut history,
    );
    drop(host);
    result.expect("the turn completes once both results are small");
    assert!(picky.seen.len() > 14, "{} requests", picky.seen.len());
    let last = picky.seen.last().unwrap();
    assert!(!last.contains(&"q".repeat(20)) && last.contains("[Cut by Awili"));
    // Any other server error is not retried.
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let (result, seen) = run_answers(
        &root,
        vec![read(), Err(SeamError::Transport("HTTP 500".into()))],
        &mut log,
        &mut no_dialog_expected(),
    );
    assert!(result.is_err());
    assert_eq!(seen.len(), 2);
}

/// RC1-N (pre-review P1): a batch whose own calls leave no room for their
/// results is refused before anything runs — the file is not written and
/// nothing is dispatched.
#[test]
fn rc1n_a_batch_with_no_room_for_its_results_never_runs() {
    let root = tempfile::tempdir().unwrap();
    let args =
        serde_json::json!({ "path": "huge.txt", "contents": "z".repeat(120_000) }).to_string();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let (result, _) = run_answers(
        &root,
        vec![Ok(sse_tool_round(&[("call_w", "fs.write", &args)]))],
        &mut log,
        &mut no_dialog_expected(),
    );
    assert!(
        matches!(&result, Err(SeamError::Context(m)) if m.contains("cannot fit")),
        "{result:?}"
    );
    assert!(!root.path().join("huge.txt").exists());
    let (records, _) = log.read_records().unwrap();
    assert!(!records
        .iter()
        .any(|r| matches!(r, LogRecordV1::DispatchStarted { .. })));
}

/// RC1-N (pre-review P2): a Deny is reported done only with its turn
/// durably closed — a failed `turn_closed` write is the turn's error.
#[test]
fn rc1n_a_deny_without_its_closing_record_is_an_error() {
    let root = tempfile::tempdir().unwrap();
    let write = || {
        Ok(sse_tool_round(&[(
            "call_w",
            "fs.write",
            r#"{"path":"a.txt","contents":"x"}"#,
        )]))
    };
    // How many records a denied turn writes: the last is `turn_closed`.
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let (result, _) = run_answers(
        &root,
        vec![write()],
        &mut log,
        &mut DenyStopHost {
            asked: 0,
            stop: true,
        },
    );
    assert!(matches!(
        result,
        Err(SeamError::Cancelled(StopCause::Denied))
    ));
    let (records, _) = log.read_records().unwrap();
    assert!(matches!(
        records.last(),
        Some(LogRecordV1::TurnClosed { .. })
    ));
    let n = records.len() as u64;
    let mut storage = MemLog::default();
    storage.fail_on_append = Some(n - 1);
    let mut log = ExecutionLog::open(storage, "sess-seam").unwrap();
    let (result, _) = run_answers(
        &root,
        vec![write()],
        &mut log,
        &mut DenyStopHost {
            asked: 0,
            stop: true,
        },
    );
    assert!(matches!(result, Err(SeamError::Storage(_))), "{result:?}");
}

/// RC1-N-FIX1: approves the first card, then answers the owner's Deny that
/// ends the turn.
struct ApproveThenDenyStop {
    asked: usize,
}

impl TurnHost for ApproveThenDenyStop {
    fn approve(&mut self, _id: &str, _prompt: &str, _r: RiskClass) -> ApprovalOutcome {
        self.asked += 1;
        if self.asked == 1 {
            ApprovalOutcome::Verdict(UserVerdict::Approve {
                scope: "once".into(),
            })
        } else {
            ApprovalOutcome::Verdict(UserVerdict::Deny {
                reason: "denied by the user".into(),
            })
        }
    }
    fn stop_after_denial(&self) -> bool {
        self.asked >= 2
    }
}

/// RC1-N-FIX1 (whole-slice review P1): a Deny after an approved call that
/// already ran keeps that work in the model's history — in the same batch
/// (the denied call and the calls after it are dropped) and across rounds —
/// so the next turn sees it and does not repeat it. Nothing completed: the
/// history is restored (`rc1n_deny_stop_records_the_denial_and_ends_the_turn`).
#[test]
fn rc1n_fix1_a_deny_keeps_the_turns_completed_work() {
    let write = |id: &str, path: &str| {
        (
            id.to_string(),
            serde_json::json!({ "path": path, "contents": "x" }).to_string(),
        )
    };
    for same_batch in [true, false] {
        let host = host().lock().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
        let (a, b, c) = (
            write("call_a", "a.txt"),
            write("call_b", "b.txt"),
            write("call_c", "c.txt"),
        );
        let bodies: Vec<String> = if same_batch {
            vec![sse_tool_round(&[
                (a.0.as_str(), "fs.write", a.1.as_str()),
                (b.0.as_str(), "fs.write", b.1.as_str()),
                (c.0.as_str(), "fs.write", c.1.as_str()),
            ])]
        } else {
            vec![
                sse_tool_round(&[(a.0.as_str(), "fs.write", a.1.as_str())]),
                sse_tool_round(&[(b.0.as_str(), "fs.write", b.1.as_str())]),
            ]
        };
        let mut transport = Script {
            bodies: bodies.into(),
        };
        let mut history = ContextHistory::new();
        let mut approver = ApproveThenDenyStop { asked: 0 };
        let result = run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "write a, b and c",
            &mut approver,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            awili_seam_window_for_tests(),
            "turn-seam-0",
            &mut history,
        );
        assert!(
            matches!(result, Err(SeamError::Cancelled(StopCause::Denied))),
            "{result:?}"
        );
        assert!(root.path().join("a.txt").exists(), "the approved write ran");
        assert!(!root.path().join("b.txt").exists() && !root.path().join("c.txt").exists());
        let kept: Vec<String> = history
            .groups()
            .iter()
            .filter_map(|g| match g {
                ContextGroup::Exchange { calls, .. } if !calls.is_empty() => Some(
                    calls
                        .iter()
                        .map(|c| c.awili_call_id.0.clone())
                        .collect::<Vec<_>>()
                        .join(","),
                ),
                _ => None,
            })
            .collect();
        assert_eq!(kept.len(), 1, "same_batch={same_batch}: {kept:?}");
        assert!(
            history.groups().iter().any(
                |g| matches!(g, ContextGroup::User { text, .. } if text == "write a, b and c")
            ),
            "the request stays with the work it led to"
        );
        // The next turn is shown the completed write, and only it.
        let mut next = Script {
            bodies: vec![sse_text_final("ok")].into(),
        };
        let report = run_turn(
            &host,
            &mut next,
            "test-model",
            root.path().to_str().unwrap(),
            "and now?",
            &mut no_dialog_expected(),
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            awili_seam_window_for_tests(),
            "turn-seam-1",
            &mut history,
        )
        .expect("the next turn runs");
        drop(host);
        let body = &report.requests[0];
        assert!(
            body.contains("a.txt"),
            "the completed write is in the history"
        );
        assert!(!body.contains("b.txt") && !body.contains("c.txt"), "{body}");
    }
}

/// RC1-N-FIX2: a host for the turn-ending paths. Every card is approved
/// once, except that the `shutdown_on`-th card answers Shutdown; with
/// `stop_after_first`, the flag trips (a Stop) as soon as the first tool
/// result is ingested.
struct EndingHost {
    cards: usize,
    shutdown_on: Option<usize>,
    stop_after_first: bool,
    stopped: bool,
}

impl TurnHost for EndingHost {
    fn approve(&mut self, _id: &str, _prompt: &str, _r: RiskClass) -> ApprovalOutcome {
        self.cards += 1;
        if self.shutdown_on == Some(self.cards) {
            return ApprovalOutcome::Shutdown;
        }
        ApprovalOutcome::Verdict(UserVerdict::Approve {
            scope: "once".into(),
        })
    }
    fn observe(&mut self, event: &AgentEvent) -> Result<(), ObserverGone> {
        if self.stop_after_first && matches!(event, AgentEvent::ToolResultIngested(_)) {
            self.stopped = true;
        }
        Ok(())
    }
    fn cancelled(&self) -> bool {
        self.stopped
    }
}

/// RC1-N-FIX2 (whole-slice R2 P1): EVERY path that ends an in-app turn after
/// a completed tool call keeps that work in the model's history and drops
/// only the unfinished tail — one case per path:
/// - Stop in the middle of a batch (the cancel flag trips after the first
///   call ran; the second never runs);
/// - Shutdown at the second card of a batch;
/// - overflow: the model server refuses the continuation as too long until
///   nothing can shrink (`context_overflow_text`);
/// - provider error body (a `TurnFailed`, not an overflow);
/// - stream/transport error (HTTP 500 on the continuation);
/// - storage error (the log write after the first dispatch fails).
///
/// The Deny path is `rc1n_fix1_a_deny_keeps_the_turns_completed_work`; the
/// crash path is the worker's `rc1n_fix2_a_new_worker_recovers_*`; a turn
/// that fails before any tool ran is restored
/// (`failed_turn_restores_session_history`).
#[test]
fn rc1n_fix2_every_ending_keeps_the_turns_completed_work() {
    let w = |id: &str, path: &str| {
        (
            id.to_string(),
            serde_json::json!({ "path": path, "contents": "x" }).to_string(),
        )
    };
    let (a, b) = (w("call_a", "a.txt"), w("call_b", "b.txt"));
    let one = || Ok(sse_tool_round(&[(a.0.as_str(), "fs.write", a.1.as_str())]));
    let two = || {
        Ok(sse_tool_round(&[
            (a.0.as_str(), "fs.write", a.1.as_str()),
            (b.0.as_str(), "fs.write", b.1.as_str()),
        ]))
    };
    let refused = || {
        Err(SeamError::Transport(
            "HTTP 400: the request exceeds the available context size".into(),
        ))
    };
    struct Case {
        name: &'static str,
        answers: Vec<Result<String, SeamError>>,
        host: EndingHost,
        fail_log_after_records: Option<u64>,
        expect: fn(&Result<TurnReport, SeamError>) -> bool,
    }
    let ending = |shutdown_on: Option<usize>, stop_after_first: bool| EndingHost {
        cards: 0,
        shutdown_on,
        stop_after_first,
        stopped: false,
    };
    let cases = vec![
        Case {
            name: "Stop mid-batch",
            answers: vec![two()],
            host: ending(None, true),
            fail_log_after_records: None,
            expect: |r| matches!(r, Err(SeamError::Cancelled(StopCause::Cancelled))),
        },
        Case {
            name: "Shutdown at the second card",
            answers: vec![two()],
            host: ending(Some(2), false),
            fail_log_after_records: None,
            expect: |r| matches!(r, Err(SeamError::Cancelled(StopCause::Shutdown))),
        },
        Case {
            name: "overflow (refused at every size)",
            answers: {
                let mut v = vec![one()];
                v.extend((0..40).map(|_| refused()));
                v
            },
            host: ending(None, false),
            fail_log_after_records: None,
            expect: |r| matches!(r, Err(SeamError::Transport(m)) if context_overflow_text(m)),
        },
        Case {
            name: "provider error body",
            answers: vec![
                one(),
                Ok(
                    r#"{"error":{"message":"upstream model crashed","type":"server_error"}}"#
                        .into(),
                ),
            ],
            host: ending(None, false),
            fail_log_after_records: None,
            expect: |r| r.is_err(),
        },
        Case {
            name: "stream/transport error",
            answers: vec![one(), Err(SeamError::Transport("HTTP 500".into()))],
            host: ending(None, false),
            fail_log_after_records: None,
            expect: |r| matches!(r, Err(SeamError::Transport(m)) if m == "HTTP 500"),
        },
        Case {
            name: "storage error after the first dispatch",
            answers: vec![two()],
            host: ending(None, false),
            fail_log_after_records: Some(0),
            expect: |r| matches!(r, Err(SeamError::Storage(_))),
        },
    ];
    for mut case in cases {
        let root = tempfile::tempdir().unwrap();
        let mut storage = MemLog::default();
        if case.fail_log_after_records.is_some() {
            // Count the records the same turn writes up to and including the
            // first call's `dispatch_finalized`, then fail the next append.
            let mut probe_log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
            let (_r, _seen) = run_answers(
                &root,
                vec![two()],
                &mut probe_log,
                &mut EndingHost {
                    cards: 0,
                    shutdown_on: None,
                    stop_after_first: false,
                    stopped: false,
                },
            );
            let (records, _) = probe_log.read_records().unwrap();
            let first_final = records
                .iter()
                .position(|r| matches!(r, LogRecordV1::DispatchFinalized { .. }))
                .expect("a dispatch was finalized") as u64;
            let _ = std::fs::remove_file(root.path().join("a.txt"));
            let _ = std::fs::remove_file(root.path().join("b.txt"));
            storage.fail_on_append = Some(first_final + 1);
        }
        let host_lock = host().lock().unwrap();
        let mut log = ExecutionLog::open(storage, "sess-seam").unwrap();
        let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
        let mut transport = Answers {
            answers: std::mem::take(&mut case.answers).into(),
            seen: Vec::new(),
        };
        let mut history = ContextHistory::new();
        let result = run_turn(
            &host_lock,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "write a and b",
            &mut case.host,
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            32_768,
            "turn-seam-0",
            &mut history,
        );
        drop(host_lock);
        assert!((case.expect)(&result), "{}: {result:?}", case.name);
        assert!(root.path().join("a.txt").exists(), "{}: a ran", case.name);
        assert!(
            !root.path().join("b.txt").exists(),
            "{}: b never ran",
            case.name
        );
        let kept: Vec<Vec<String>> = history
            .groups()
            .iter()
            .filter_map(|g| match g {
                ContextGroup::Exchange { calls, results, .. } if !calls.is_empty() => {
                    assert_eq!(calls.len(), results.len(), "{}", case.name);
                    Some(
                        calls
                            .iter()
                            .map(|c| c.provider_call_id.clone().unwrap_or_default())
                            .collect(),
                    )
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            kept,
            vec![vec!["call_a".to_string()]],
            "{}: {kept:?}",
            case.name
        );
        assert!(
            history
                .groups()
                .iter()
                .any(|g| matches!(g, ContextGroup::User { text, .. } if text == "write a and b")),
            "{}: the request stays with its work",
            case.name
        );
    }
}

/// RC1-N-FIX2 (pre-review P1/P2), the crash path's note: a call whose
/// dispatch was finalized but whose `tool_result` event never reached the
/// log (the append failed, or the app died in between) still counts as
/// completed; a call the kept history already holds is left out call by
/// call (a batch with one held and one new call lists only the new one);
/// a turn that completed normally gives no note.
#[test]
fn rc1n_fix2_the_recovered_note_counts_finalized_calls_and_skips_held_ones() {
    let w = |id: &str, path: &str| {
        (
            id.to_string(),
            serde_json::json!({ "path": path, "contents": format!("BODY-{path}") }).to_string(),
        )
    };
    let (a, b) = (w("call_a", "a.txt"), w("call_b", "b.txt"));
    let two = || {
        Ok(sse_tool_round(&[
            (a.0.as_str(), "fs.write", a.1.as_str()),
            (b.0.as_str(), "fs.write", b.1.as_str()),
        ]))
    };
    let ending = || EndingHost {
        cards: 0,
        shutdown_on: None,
        stop_after_first: false,
        stopped: false,
    };
    // 1. Find the record that carries call a's `tool_result` event.
    let root = tempfile::tempdir().unwrap();
    let mut probe = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let _ = run_answers(&root, vec![two()], &mut probe, &mut ending());
    let (records, _) = probe.read_records().unwrap();
    let result_at = records
        .iter()
        .position(|r| {
            matches!(
                r,
                LogRecordV1::EventAppended {
                    event: awili_engine_log::schema::CanonicalEventV1::ToolResult { .. },
                    ..
                }
            )
        })
        .expect("a result event") as u64;
    assert!(records[..result_at as usize]
        .iter()
        .any(|r| matches!(r, LogRecordV1::DispatchFinalized { .. })));
    // 2. The same turn, the result event's append failing.
    let root = tempfile::tempdir().unwrap();
    let mut storage = MemLog::default();
    storage.fail_on_append = Some(result_at);
    let mut log = ExecutionLog::open(storage, "sess-seam").unwrap();
    let (result, _) = run_answers(&root, vec![two()], &mut log, &mut ending());
    assert!(matches!(result, Err(SeamError::Storage(_))), "{result:?}");
    let (records, _) = log.read_records().unwrap();
    assert!(!records.iter().any(|r| matches!(
        r,
        LogRecordV1::EventAppended {
            event: awili_engine_log::schema::CanonicalEventV1::ToolResult { .. },
            ..
        }
    )));
    let note = recovered_work_note(&records, &ContextHistory::new()).expect("a note");
    assert!(note.starts_with(RECOVERED_WORK_LABEL), "{note}");
    assert!(note.contains("a.txt") && !note.contains("b.txt"), "{note}");
    // 3. Both calls completed; the kept history holds only a: only b listed.
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut both = ending();
    both.stop_after_first = false;
    let (_r, _) = run_answers(
        &root,
        vec![two(), Err(SeamError::Transport("HTTP 500".into()))],
        &mut log,
        &mut both,
    );
    let (records, _) = log.read_records().unwrap();
    let ids: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::EventAppended {
                event:
                    awili_engine_log::schema::CanonicalEventV1::ToolCallReady {
                        awili_call_id,
                        name,
                        arguments,
                        provider_call_id,
                    },
                ..
            } => Some(format!(
                "{awili_call_id}|{name}|{arguments}|{}",
                provider_call_id.clone().unwrap_or_default()
            )),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    let first: Vec<&str> = ids[0].split('|').collect();
    let mut kept = ContextHistory::new();
    kept.push_user("write a and b");
    kept.push_exchange(
        "",
        vec![ToolCall {
            awili_call_id: awili_engine::AwiliCallId(first[0].into()),
            provider_call_id: Some(first[3].into()),
            name: first[1].into(),
            arguments: first[2].into(),
        }],
        vec![ToolResultRecord {
            awili_call_id: awili_engine::AwiliCallId(first[0].into()),
            content: "ok".into(),
            is_error: false,
        }],
    )
    .unwrap();
    let note = recovered_work_note(&records, &kept).expect("b is new");
    assert!(note.contains("b.txt") && !note.contains("a.txt"), "{note}");
    assert!(recovered_work_note(&records, &ContextHistory::new())
        .is_some_and(|n| n.contains("a.txt") && n.contains("b.txt")));
    // 4. A normally completed turn: no note.
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut marked = ContextHistory::new();
    let (r, _) = run_answers_into(
        &root,
        vec![two(), Ok(sse_text_final("done"))],
        &mut log,
        &mut ending(),
        &mut marked,
        "turn-seam-0",
    );
    r.expect("completes");
    let (records, _) = log.read_records().unwrap();
    // The history that turn produced (marked by the seam), or one kept
    // before the mark existed: nothing to recover.
    assert_eq!(marked.log_turn(), Some("turn-seam-0"));
    assert_eq!(recovered_work_note(&records, &marked), None);
    assert_eq!(recovered_work_note(&records, &unmarked(&marked)), None);
    // No history kept at all: its steps are recovered (they ran).
    assert!(recovered_work_note(&records, &ContextHistory::new())
        .is_some_and(|n| n.contains("a.txt") && n.contains("b.txt")));
}

/// RC1-N-FIX2 (pre-review pass 2): two interrupted turns in a row — the
/// first crashed after its write, the next (on a new worker, before that
/// recovery was kept) also ended after its own write — are both recovered:
/// the note lists every completed call after the kept history's log-turn
/// mark (the completed turn 0), oldest first.
#[test]
fn rc1n_fix2_successive_interrupted_turns_are_all_recovered() {
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let write = |id: &str, path: &str| {
        Ok(sse_tool_round(&[(
            id,
            "fs.write",
            &serde_json::json!({ "path": path, "contents": "x" }).to_string(),
        )]))
    };
    let mut turn = |answers: Vec<Result<String, SeamError>>, turn_id: &str, ask: &str| {
        let host = host().lock().unwrap();
        let mut transport = Answers {
            answers: answers.into(),
            seen: Vec::new(),
        };
        let mut history = ContextHistory::new();
        let r = run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            ask,
            &mut approve_once(),
            &mut log,
            &mut grants,
            &[],
            &Executors::builtin_only(),
            &awili_engine_authz::NoInstallAuthority,
            &RootsAcceptance::default(),
            32_768,
            turn_id,
            &mut history,
        );
        assert!(r.is_ok() == (turn_id == "turn-seam-0"), "{turn_id}: {r:?}");
        history
    };
    // A normally completed turn first; its kept history bounds the recovery.
    let kept = turn(
        vec![write("call_0", "zero.txt"), Ok(sse_text_final("done"))],
        "turn-seam-0",
        "write zero",
    );
    turn(
        vec![
            write("call_1", "one.txt"),
            Err(SeamError::Transport("HTTP 500".into())),
        ],
        "turn-seam-1",
        "write one",
    );
    turn(
        vec![
            write("call_2", "two.txt"),
            Err(SeamError::Transport("HTTP 500".into())),
        ],
        "turn-seam-2",
        "write two",
    );
    let (records, _) = log.read_records().unwrap();
    assert_eq!(kept.log_turn(), Some("turn-seam-0"), "the seam marks it");
    let note = recovered_work_note(&records, &kept).expect("a note");
    let (one, two) = (note.find("one.txt"), note.find("two.txt"));
    assert!(one.is_some() && two.is_some() && one < two, "{note}");
    assert!(
        !note.contains("zero.txt"),
        "the completed turn bounds it: {note}"
    );
}

/// RC1-N-FIX2 (pre-review pass 3): a call the seam answered without running
/// anything — here a refused batch (an unknown tool) — is not "completed
/// work": an interrupted turn holding only that gives no note, and beside a
/// call that did run only that one is listed.
#[test]
fn rc1n_fix2_the_recovered_note_lists_only_calls_that_ran() {
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let refused = Ok(sse_tool_round(&[(
        "call_x",
        "no_such_tool",
        r#"{"path":"refused.txt"}"#,
    )]));
    let (r, _) = run_answers(
        &root,
        vec![refused, Err(SeamError::Transport("HTTP 500".into()))],
        &mut log,
        &mut approve_once(),
    );
    assert!(r.is_err());
    let (records, _) = log.read_records().unwrap();
    assert!(
        records.iter().any(|r| matches!(
            r,
            LogRecordV1::EventAppended {
                event: awili_engine_log::schema::CanonicalEventV1::ToolResult { .. },
                ..
            }
        )),
        "the refusal was logged as a result"
    );
    assert_eq!(recovered_work_note(&records, &ContextHistory::new()), None);
}

/// RC1-N-FIX2 (pre-review pass 4, P1 and the duplicate-note P2): a turn that
/// completed later — on a worker that recovered an earlier interrupted turn,
/// then died before its history was kept — never hides that earlier turn's
/// steps; and a history that already holds a recovery note (the mark moved
/// with it) never gets the same steps in a second note.
#[test]
fn rc1n_fix2_a_later_completed_turn_never_hides_earlier_recovered_work() {
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let write = |id: &str, path: &str| {
        Ok(sse_tool_round(&[(
            id,
            "fs.write",
            &serde_json::json!({ "path": path, "contents": "x" }).to_string(),
        )]))
    };
    let mut turn =
        |answers: Vec<Result<String, SeamError>>, turn_id: &str, history: &mut ContextHistory| {
            let host = host().lock().unwrap();
            let mut transport = Answers {
                answers: answers.into(),
                seen: Vec::new(),
            };
            let ok = run_turn(
                &host,
                &mut transport,
                "test-model",
                root.path().to_str().unwrap(),
                turn_id,
                &mut approve_once(),
                &mut log,
                &mut grants,
                &[],
                &Executors::builtin_only(),
                &awili_engine_authz::NoInstallAuthority,
                &RootsAcceptance::default(),
                32_768,
                turn_id,
                history,
            )
            .is_ok();
            (ok, log.read_records().unwrap().0)
        };
    let notes = |h: &ContextHistory| {
        h.groups()
            .iter()
            .filter(|g| matches!(g, ContextGroup::User { text, .. } if text.starts_with(RECOVERED_WORK_LABEL)))
            .count()
    };
    // Turn 0 completes; its history is kept.
    let mut kept = ContextHistory::new();
    assert!(
        turn(
            vec![write("call_0", "zero.txt"), Ok(sse_text_final("done"))],
            "turn-seam-0",
            &mut kept,
        )
        .0
    );
    // Turn 1 writes one.txt, then the app dies (its history is never kept).
    let mut lost = kept.clone();
    let mut died = vec![write("call_1", "one.txt")];
    died.push(Err(SeamError::Transport("HTTP 500".into())));
    let (ok, records) = turn(died, "turn-seam-1", &mut lost);
    assert!(!ok);
    // Worker 2 starts from the kept history and recovers turn 1 ...
    let mut w2 = kept.clone();
    assert!(recover_completed_work(&records, &mut w2));
    assert_eq!(w2.log_turn(), Some("turn-seam-1"));
    // ... recovering again from that history adds nothing (no second note).
    let mut again = w2.clone();
    assert!(!recover_completed_work(&records, &mut again));
    assert_eq!(notes(&again), 1);
    // ... then turn 2 completes on it, and the app dies before keeping it.
    let (ok, records) = turn(
        vec![write("call_2", "two.txt"), Ok(sse_text_final("done"))],
        "turn-seam-2",
        &mut w2,
    );
    assert!(ok);
    // Worker 3 starts from the last KEPT history (turn 0): turn 2 completed
    // normally, yet turn 1's step is still recovered, with turn 2's.
    let note = recovered_work_note(&records, &kept).expect("a note");
    assert!(
        note.contains("one.txt") && note.contains("two.txt") && !note.contains("zero.txt"),
        "{note}"
    );
    // Had turn 2's history been kept (it holds the note), nothing repeats.
    let mut from_w2 = w2.clone();
    assert!(!recover_completed_work(&records, &mut from_w2));
    assert_eq!(notes(&from_w2), 1);
    // A later interrupted turn on that history: only its own step is new.
    let mut w4 = w2.clone();
    let mut died = vec![write("call_3", "three.txt")];
    died.push(Err(SeamError::Transport("HTTP 500".into())));
    let (ok, records) = turn(died, "turn-seam-3", &mut w4);
    assert!(!ok);
    let note = recovered_work_note(&records, &w2).expect("turn 3");
    assert!(
        note.contains("three.txt") && !note.contains("one.txt") && !note.contains("two.txt"),
        "{note}"
    );
    // Its kept history (the seam kept the completed write) holds it.
    assert_eq!(w4.log_turn(), Some("turn-seam-3"));
    assert_eq!(recovered_work_note(&records, &w4), None);
}

/// RC1-N-FIX2 (pre-review pass 4, P2): a step finalized as an error — a
/// failed tool, or an MCP call refused before dispatch (the seam finalizes
/// every such disposition as an error; the log schema is frozen) — is never
/// listed as a step that ran: it gets its own label saying it may not have.
#[test]
fn rc1n_fix2_a_step_that_ended_in_an_error_is_not_said_to_have_run() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("there.txt"), "THERE").unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let (r, _) = run_answers(
        &root,
        vec![
            Ok(sse_tool_round(&[
                ("call_ok", "fs.read", r#"{"path":"there.txt"}"#),
                ("call_bad", "fs.read", r#"{"path":"missing.txt"}"#),
            ])),
            Err(SeamError::Transport("HTTP 500".into())),
        ],
        &mut log,
        &mut approve_once(),
    );
    assert!(r.is_err());
    let (records, _) = log.read_records().unwrap();
    let bad_finalized_as_error = records
        .iter()
        .any(|r| matches!(r, LogRecordV1::DispatchFinalized { ok: false, .. }));
    assert!(
        bad_finalized_as_error,
        "the failed read is finalized as an error"
    );
    let check = |records: &[LogRecordV1], err_text: &str| {
        let note = recovered_work_note(records, &ContextHistory::new()).expect("a note");
        let (ran, failed) = note.split_once(RECOVERED_FAILED_LABEL).expect("two parts");
        assert!(ran.starts_with(RECOVERED_WORK_LABEL), "{note}");
        assert!(
            ran.contains("there.txt") && !ran.contains("missing.txt"),
            "{note}"
        );
        assert!(
            failed.contains("missing.txt") && failed.contains(err_text),
            "{note}"
        );
    };
    check(&records, "error:");
    // The same record as the MCP leg writes for a call refused before
    // dispatch (NeverDispatched): finalized, not OK, "never ran".
    let refused: Vec<LogRecordV1> = records
        .iter()
        .cloned()
        .map(|r| match r {
            LogRecordV1::DispatchFinalized {
                intent_id,
                ok: false,
                ..
            } => LogRecordV1::DispatchFinalized {
                intent_id,
                ok: false,
                content: "shutdown before dispatch; call never ran.".into(),
            },
            other => other,
        })
        .collect();
    check(&refused, "never ran");
    // Only failed steps: no "they ran" label at all.
    let only_failed: Vec<LogRecordV1> = refused
        .iter()
        .filter(|r| !matches!(r, LogRecordV1::DispatchFinalized { ok: true, .. }))
        .cloned()
        .collect();
    let note = recovered_work_note(&only_failed, &ContextHistory::new()).expect("a note");
    assert!(
        note.starts_with(RECOVERED_FAILED_LABEL) && !note.contains(RECOVERED_WORK_LABEL),
        "{note}"
    );
}

/// RC1-N-FIX2 (pre-review pass 5): recovered steps keep the log's order — a
/// step that ran, one that failed, then one that ran again are shown in that
/// order, each run of one kind under its own label.
#[test]
fn rc1n_fix2_recovered_steps_keep_their_log_order() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("first.txt"), "FIRST").unwrap();
    std::fs::write(root.path().join("third.txt"), "THIRD").unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let (r, _) = run_answers(
        &root,
        vec![
            Ok(sse_tool_round(&[
                ("call_1", "fs.read", r#"{"path":"first.txt"}"#),
                ("call_2", "fs.read", r#"{"path":"second-missing.txt"}"#),
                ("call_3", "fs.read", r#"{"path":"third.txt"}"#),
            ])),
            Err(SeamError::Transport("HTTP 500".into())),
        ],
        &mut log,
        &mut approve_once(),
    );
    assert!(r.is_err());
    let (records, _) = log.read_records().unwrap();
    let note = recovered_work_note(&records, &ContextHistory::new()).expect("a note");
    let at = |s: &str| {
        note.find(s)
            .unwrap_or_else(|| panic!("{s} missing: {note}"))
    };
    let (first, second, third) = (at("first.txt"), at("second-missing.txt"), at("third.txt"));
    assert!(first < second && second < third, "{note}");
    let failed_label = at(RECOVERED_FAILED_LABEL);
    let second_ran_label = note.rfind(RECOVERED_WORK_LABEL).unwrap();
    assert!(note.starts_with(RECOVERED_WORK_LABEL), "{note}");
    assert!(
        first < failed_label && failed_label < second && second < second_ran_label,
        "{note}"
    );
    assert!(second_ran_label < third, "{note}");
}

/// RC1-N-FIX2: `history` as saved before the log-turn mark existed.
fn unmarked(history: &ContextHistory) -> ContextHistory {
    let mut v = serde_json::to_value(history).unwrap();
    v.as_object_mut().unwrap().remove("log_checkpoint");
    serde_json::from_value(v).unwrap()
}

/// RC1-N-FIX2 (pre-review pass 6): a history saved before the mark existed
/// (an upgrade) covers only what it holds. A turn that completed after it —
/// its write ran, then the app exited before keeping the new history — is
/// still recovered: the log's newest completed turn never counts as covered.
#[test]
fn rc1n_fix2_an_unmarked_saved_history_never_hides_a_later_completed_turn() {
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let write = |id: &str, path: &str| {
        Ok(sse_tool_round(&[(
            id,
            "fs.write",
            &serde_json::json!({ "path": path, "contents": "x" }).to_string(),
        )]))
    };
    let mut saved = ContextHistory::new();
    let (r, _) = run_answers_into(
        &root,
        vec![write("call_0", "zero.txt"), Ok(sse_text_final("done"))],
        &mut log,
        &mut approve_once(),
        &mut saved,
        "turn-seam-0",
    );
    r.expect("turn 0 completes");
    let saved = unmarked(&saved);
    assert_eq!(saved.log_turn(), None, "saved before the mark existed");
    // After the upgrade: turn 1 completes its write, then the app exits
    // before keeping the new history.
    let mut lost = saved.clone();
    let (r, _) = run_answers_into(
        &root,
        vec![write("call_1", "one.txt"), Ok(sse_text_final("done"))],
        &mut log,
        &mut approve_once(),
        &mut lost,
        "turn-seam-1",
    );
    r.expect("turn 1 completes");
    let (records, _) = log.read_records().unwrap();
    assert!(records.iter().any(|r| matches!(r,
        LogRecordV1::TurnClosed { outcome, .. } if outcome == "completed")));
    let note = recovered_work_note(&records, &saved).expect("turn 1 recovered");
    assert!(
        note.contains("one.txt") && !note.contains("zero.txt"),
        "{note}"
    );
    // A saved history that holds nothing covers nothing: everything logged.
    let note = recovered_work_note(&records, &unmarked(&ContextHistory::new())).expect("all");
    assert!(
        note.contains("zero.txt") && note.contains("one.txt"),
        "{note}"
    );
}

/// RC1-N-FIX2 (pre-review pass 6): the log-turn mark is what covers turns
/// whose calls compaction folded into a summary (no exchange holds them any
/// more) — a kept, marked, compacted history recovers nothing.
#[test]
fn rc1n_fix2_the_mark_covers_compacted_turns() {
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut kept = ContextHistory::new();
    let (r, _) = run_answers_into(
        &root,
        vec![
            Ok(sse_tool_round(&[(
                "call_0",
                "fs.write",
                r#"{"path":"zero.txt","contents":"x"}"#,
            )])),
            Ok(sse_text_final("done")),
        ],
        &mut log,
        &mut approve_once(),
        &mut kept,
        "turn-seam-0",
    );
    r.expect("turn 0 completes");
    let (r, _) = run_answers_into(
        &root,
        vec![Ok(sse_text_final("nothing to do"))],
        &mut log,
        &mut approve_once(),
        &mut kept,
        "turn-seam-1",
    );
    r.expect("turn 1 completes");
    // Compaction folds turn 0 (its user message and exchanges) into a summary.
    let last_user = kept
        .groups()
        .iter()
        .rposition(|g| matches!(g, ContextGroup::User { .. }))
        .unwrap();
    let ids: Vec<_> = kept.groups()[..last_user].iter().map(|g| g.id()).collect();
    let compacted = apply_summary(&kept, &ids, "earlier: wrote zero.txt").unwrap();
    assert!(!compacted
        .groups()
        .iter()
        .any(|g| matches!(g, ContextGroup::Exchange { calls, .. } if !calls.is_empty())));
    assert_eq!(compacted.log_turn(), Some("turn-seam-1"));
    let (records, _) = log.read_records().unwrap();
    assert_eq!(recovered_work_note(&records, &compacted), None);
}

/// RC1-N-FIX3 (lane review P2 on 8397ca6): a refused batch runs nothing (no
/// dispatch is finalized), so a turn that then fails keeps nothing — its
/// history is restored exactly, the refused exchange dropped. Beside a call
/// that did run, the turn is kept.
#[test]
fn rc1n_fix3_a_refused_batch_then_a_failure_keeps_nothing() {
    let refused = || {
        Ok(sse_tool_round(&[(
            "call_x",
            "no_such_tool",
            r#"{"path":"refused.txt"}"#,
        )]))
    };
    let mut before = ContextHistory::new();
    before.push_user("prior turn");
    before
        .push_exchange("prior answer", vec![], vec![])
        .unwrap();
    // Refused, then a provider error: restored.
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut history = before.clone();
    let (r, _) = run_answers_into(
        &root,
        vec![
            refused(),
            Ok(r#"{"error":{"message":"upstream model crashed","type":"server_error"}}"#.into()),
        ],
        &mut log,
        &mut approve_once(),
        &mut history,
        "turn-seam-0",
    );
    assert!(r.is_err(), "{r:?}");
    let (records, _) = log.read_records().unwrap();
    assert!(finalized_calls(&records).is_empty(), "nothing ran");
    assert_eq!(history, before, "a refused batch is not completed work");
    // A write that ran, then the refused batch, then a failure: kept.
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut history = before.clone();
    let (r, _) = run_answers_into(
        &root,
        vec![
            Ok(sse_tool_round(&[(
                "call_w",
                "fs.write",
                r#"{"path":"ran.txt","contents":"x"}"#,
            )])),
            refused(),
            Err(SeamError::Transport("HTTP 500".into())),
        ],
        &mut log,
        &mut approve_once(),
        &mut history,
        "turn-seam-0",
    );
    assert!(r.is_err(), "{r:?}");
    assert!(root.path().join("ran.txt").exists());
    assert_eq!(history.log_turn(), Some("turn-seam-0"));
    assert!(serde_json::to_string(&history).unwrap().contains("ran.txt"));
}

/// RC1-N-FIX3 (lane review P1 on 8397ca6): a history saved before the stamp
/// existed says nothing about which turns it holds, so recovery lists every
/// finalized call it lacks. The two-turn sequence: turn 0 wrote early.txt
/// and then failed (its history never kept); turn 1 started from the older
/// saved history, wrote late.txt and completed, and its history was saved
/// unstamped. The next worker lists early.txt (late.txt is held).
#[test]
fn rc1n_fix3_an_unstamped_history_lists_an_earlier_failed_turns_write() {
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let write = |id: &str, path: &str| {
        Ok(sse_tool_round(&[(
            id,
            "fs.write",
            &serde_json::json!({ "path": path, "contents": "x" }).to_string(),
        )]))
    };
    let mut lost = ContextHistory::new();
    let (r, _) = run_answers_into(
        &root,
        vec![
            write("call_e", "early.txt"),
            Err(SeamError::Transport("HTTP 500".into())),
        ],
        &mut log,
        &mut approve_once(),
        &mut lost,
        "turn-seam-0",
    );
    assert!(r.is_err());
    let mut saved = ContextHistory::new();
    let (r, _) = run_answers_into(
        &root,
        vec![write("call_l", "late.txt"), Ok(sse_text_final("done"))],
        &mut log,
        &mut approve_once(),
        &mut saved,
        "turn-seam-1",
    );
    r.expect("turn 1 completes");
    let saved = unmarked(&saved);
    let (records, _) = log.read_records().unwrap();
    let note = recovered_work_note(&records, &saved).expect("early.txt is listed");
    assert!(
        note.contains("early.txt") && !note.contains("late.txt"),
        "{note}"
    );
    // Stamped by recovery, the next restart lists nothing again.
    let mut next = saved.clone();
    assert!(recover_completed_work(&records, &mut next));
    assert_eq!(next.log_turn(), Some("turn-seam-1"));
    assert_eq!(recovered_work_note(&records, &next), None);
}

/// RC1-N-FIX3 (pre-review P1): a history kept by RC1-N-FIX2's code carries a
/// `log_turn` stamp without that code having placed every finalized call
/// (an MCP result that arrived with a Stop was left out). Its stamp is not
/// exact, so it is ignored on load: the history counts as unstamped and
/// recovery lists the finalized call it lacks.
#[test]
fn rc1n_fix3_a_fix2_stamped_history_is_read_as_unstamped() {
    let root = tempfile::tempdir().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-seam").unwrap();
    let mut ran = ContextHistory::new();
    let (r, _) = run_answers_into(
        &root,
        vec![
            Ok(sse_tool_round(&[
                (
                    "call_a",
                    "fs.write",
                    r#"{"path":"held.txt","contents":"x"}"#,
                ),
                (
                    "call_b",
                    "fs.write",
                    r#"{"path":"omitted.txt","contents":"x"}"#,
                ),
            ])),
            Err(SeamError::Transport("HTTP 500".into())),
        ],
        &mut log,
        &mut approve_once(),
        &mut ran,
        "turn-seam-0",
    );
    assert!(r.is_err());
    let (records, _) = log.read_records().unwrap();
    assert_eq!(finalized_calls(&records).len(), 2);
    // What FIX2 could have kept: the turn stamped, its exchange holding only
    // the first call.
    let mut v = serde_json::to_value(&ran).unwrap();
    let groups = v["groups"].as_array_mut().unwrap();
    let ex = groups
        .iter_mut()
        .find(|g| g.get("Exchange").is_some())
        .unwrap();
    ex["Exchange"]["calls"].as_array_mut().unwrap().truncate(1);
    ex["Exchange"]["results"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    let obj = v.as_object_mut().unwrap();
    obj.remove("log_checkpoint");
    obj.insert("log_turn".into(), serde_json::json!("turn-seam-0"));
    let fix2: ContextHistory = serde_json::from_value(v).unwrap();
    assert_eq!(fix2.log_turn(), None, "a FIX2 stamp is not trusted");
    let note = recovered_work_note(&records, &fix2).expect("the omitted call");
    assert!(
        note.contains("omitted.txt") && !note.contains("held.txt"),
        "{note}"
    );
    // The same history stamped by this code would be exact: nothing listed.
    assert_eq!(ran.log_turn(), Some("turn-seam-0"));
    assert_eq!(recovered_work_note(&records, &ran), None);
}

/// RC1-O: every resolved call's registry floor reaches the host before it
/// is authorized, and an absolute path inside the session root is the
/// relative path that follows it (round 5 J: "path escapes the session root:
/// /…/rep1/a.txt" while the model was told its tools work in that folder).
#[test]
fn rc1o_floors_reach_the_host_and_a_full_path_inside_the_root_works() {
    struct Floors(Vec<(String, RiskClass)>, usize);
    impl TurnHost for Floors {
        fn approve(&mut self, _call_id: &str, _prompt: &str, _risk: RiskClass) -> ApprovalOutcome {
            // RC1-O-FIX5: the second write is denied — it never dispatches.
            self.1 += 1;
            ApprovalOutcome::Verdict(if self.1 == 2 {
                UserVerdict::Deny {
                    reason: "no".into(),
                }
            } else {
                UserVerdict::Approve {
                    scope: "once".into(),
                }
            })
        }
        fn tool_dispatching(&mut self, call_id: &str, floor: RiskClass) {
            self.0.push((call_id.to_string(), floor));
        }
    }
    let root = tempfile::tempdir().unwrap();
    let full = root.path().join("a.txt");
    let write = serde_json::json!({"path": full.to_str().unwrap(), "content": "one"}).to_string();
    let bodies = vec![
        sse_tool_round(&[("call_w", "fs.write", write.as_str())]),
        sse_tool_round(&[("call_r", "fs.read", r#"{"path":"a.txt"}"#)]),
        sse_tool_round(&[("call_d", "fs.write", r#"{"path":"b.txt","contents":"x"}"#)]),
        sse_text_final("done"),
    ];
    let mut host = Floors(Vec::new(), 0);
    let r = run(&root, bodies, &mut host);
    let report = r.result.expect("turn completes");
    // Pre-review: each floor is keyed by that call's own awili call id.
    let ready: Vec<(String, String)> = report
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallReady(c) => Some((c.awili_call_id.0.clone(), c.name.clone())),
            _ => None,
        })
        .collect();
    // RC1-O-FIX5: only the calls that ran are told — the denied third call
    // (a write) never dispatched.
    assert_eq!(ready.len(), 3);
    let ids: Vec<&str> = host.0.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        ready[..2]
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(ready[0].1, "fs.write");
    assert_eq!(ready[1].1, "fs.read");
    assert_eq!(ready[2].1, "fs.write");
    assert_eq!(std::fs::read_to_string(&full).unwrap(), "one");
    assert!(!root.path().join("b.txt").exists());
    // One floor per call that ran, in call order, keyed by its awili call id.
    let floors: Vec<RiskClass> = host.0.iter().map(|(_, f)| *f).collect();
    assert_eq!(floors, vec![RiskClass::Mutating, RiskClass::ReadOnly]);
}

/// RC1-R: the request the model gets carries each tool's description — the
/// builtin fs tools say they work in the Work folder (round 6: Gemma chose
/// another server's `write_file` for a file in the folder).
#[test]
fn rc1r_the_request_describes_the_tools() {
    let root = tempfile::tempdir().unwrap();
    let mut approver = no_dialog_expected();
    let r = run(&root, vec![sse_text_final("ok")], &mut approver);
    let report = r.result.unwrap();
    let req: serde_json::Value = serde_json::from_str(&report.requests[0]).unwrap();
    let tools = req["tools"].as_array().expect("tools advertised");
    let desc = |name: &str| {
        tools
            .iter()
            .find(|t| t["function"]["name"] == name)
            .map(|t| {
                t["function"]["description"]
                    .as_str()
                    .unwrap_or("")
                    .to_string()
            })
            .unwrap_or_default()
    };
    // The wire names are the provider-safe spellings (fs.write -> fs_write).
    assert!(desc("fs_write").contains("Work folder"), "{tools:?}");
    assert!(desc("fs_glob").contains("list"), "{tools:?}");
}

/// RC1-R-FIX1 (slice review P2): the engine's file tools keep their plain
/// wire names, which the in-app brief names; another tool already called
/// `fs_glob` takes `fs_glob_2`.
#[test]
fn rc1r_fix1_builtin_wire_names_are_reserved() {
    let map = crate::wire_names::WireNames::new([
        "fs_glob", "fs.glob", "fs_write", "fs.write", "x_y", "x.y",
    ]);
    assert_eq!(map.wire("fs.glob"), "fs_glob");
    assert_eq!(map.wire("fs_glob"), "fs_glob_2");
    assert_eq!(map.wire("fs.write"), "fs_write");
    assert_eq!(map.wire("fs_write"), "fs_write_2");
    // Everything else as before.
    assert_eq!(map.wire("x_y"), "x_y");
    assert_eq!(map.wire("x.y"), "x_y_2");
}

/// RC1-R-FIX1 (slice review P1): many described tools share one budget for
/// the window; every tool keeps a (shortened) description.
#[test]
fn rc1r_fix1_descriptions_share_one_budget() {
    let long = "d".repeat(512);
    let many: Vec<&str> = (0..256).map(|_| long.as_str()).collect();
    let fitted = crate::fit_descriptions(&many, 32_768);
    assert_eq!(fitted.len(), 256);
    let total: usize = fitted.iter().map(|d| d.chars().count()).sum();
    assert!(total <= crate::description_budget_chars(32_768), "{total}");
    assert!(fitted
        .iter()
        .all(|d| !d.is_empty() && d.ends_with('\u{2026}')));
    // A few short ones go whole.
    let few = crate::fit_descriptions(&["a", "bb"], 32_768);
    assert_eq!(few, ["a", "bb"]);
    // A share too small to say anything sends none.
    assert!(crate::fit_descriptions(&many, 2_000)
        .iter()
        .all(|d| d.is_empty()));
}

/// RC1-R-FIX1 (slice review P3): an extra (MCP) tool's own description
/// reaches the request the model gets.
#[test]
fn rc1r_fix1_an_mcp_description_reaches_the_request() {
    let root = tempfile::tempdir().unwrap();
    let extra = def_of("mcp:probe", "probe.ping").with_description("PROBE-SAYS-HELLO");
    let mut approver = no_dialog_expected();
    let r = run_with_extras(&root, vec![sse_text_final("ok")], &mut approver, &[extra]);
    let report = r.result.unwrap();
    let req: serde_json::Value = serde_json::from_str(&report.requests[0]).unwrap();
    let tools = req["tools"].as_array().expect("tools advertised");
    assert!(
        tools
            .iter()
            .any(|t| t["function"]["description"] == "PROBE-SAYS-HELLO"),
        "{tools:?}"
    );
}

/// RC2-ENGB4 (S-R2, RC1-R slice review P3): the budget holds on the request
/// the model gets, not only in `fit_descriptions`: 256 described MCP tools
/// under a 32K window are all advertised, each with a shortened description,
/// and their descriptions together stay within the window's share; the
/// engine's own file tools keep their whole descriptions.
#[test]
fn rc2engb4_the_emitted_request_keeps_descriptions_in_the_budget() {
    const WINDOW: u64 = 32_768;
    let root = tempfile::tempdir().unwrap();
    let long = "d".repeat(512);
    let extras: Vec<awili_tools::ToolDef> = (0..256)
        .map(|i| def_of("mcp:many", &format!("tool_{i:03}")).with_description(long.clone()))
        .collect();
    let mut approver = no_dialog_expected();
    let r = run_windowed(
        &root,
        vec![sse_text_final("ok")],
        &mut approver,
        &extras,
        WINDOW,
    );
    let report = r.result.unwrap();
    let req: serde_json::Value = serde_json::from_str(&report.requests[0]).unwrap();
    let tools = req["tools"].as_array().expect("tools advertised");
    let described = |t: &serde_json::Value| {
        t["function"]["description"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    let extra: Vec<String> = tools
        .iter()
        .filter(|t| {
            t["function"]["name"]
                .as_str()
                .is_some_and(|n| n.starts_with("tool_"))
        })
        .map(described)
        .collect();
    assert_eq!(extra.len(), 256, "every tool is still advertised");
    let total: usize = extra.iter().map(|d| d.chars().count()).sum();
    assert!(total <= crate::description_budget_chars(WINDOW), "{total}");
    assert!(
        extra
            .iter()
            .all(|d| !d.is_empty() && d.ends_with('\u{2026}')),
        "each keeps a shortened description"
    );
    let builtin_write = tools
        .iter()
        .find(|t| t["function"]["name"] == "fs_write")
        .map(described)
        .unwrap_or_default();
    assert!(builtin_write.contains("Work folder"), "{builtin_write}");
}

/// RC2-ENG5 (S-R1): advertising `fs.read`, `fs_read` and `fs_read_2`, the
/// tool already named `fs_read_2` keeps its name; `fs_read` (its name taken
/// by the builtin) takes the next free one. Before, `fs_read` took
/// `fs_read_2` and the real `fs_read_2` became `fs_read_2_2`.
#[test]
fn rc2eng5_a_safe_name_keeps_itself_when_a_builtin_takes_another() {
    let map = crate::wire_names::WireNames::new(["fs.read", "fs_read", "fs_read_2"]);
    assert_eq!(map.wire("fs.read"), "fs_read");
    assert_eq!(map.wire("fs_read_2"), "fs_read_2");
    assert_eq!(map.wire("fs_read"), "fs_read_3");
    // Reversible: every wire name maps back to its own tool.
    for name in ["fs.read", "fs_read", "fs_read_2"] {
        assert_eq!(map.canonical(&map.wire(name)), name);
    }
}

#[cfg(unix)]
/// RC2-ENG5 (T26): the acceptance runner's `--expect` file stays inside
/// `--dir`: a plain name inside is read; `..`, an absolute path and a link
/// that leads outside are refused.
#[test]
fn rc2eng5_the_expect_file_stays_inside_the_run_folder() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("run");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/out.txt"), "ok").unwrap();
    std::fs::write(base.path().join("secret.txt"), "secret").unwrap();
    std::os::unix::fs::symlink(base.path().join("secret.txt"), dir.join("link.txt")).unwrap();
    let inside = crate::expect_file_in_dir(&dir, "sub/out.txt").unwrap();
    assert_eq!(inside, "ok");
    for bad in [
        "../secret.txt",
        "sub/../../secret.txt",
        base.path().join("secret.txt").to_str().unwrap(),
        "link.txt",
        "",
    ] {
        assert!(
            crate::expect_file_in_dir(&dir, bad).is_err(),
            "{bad:?} must be refused"
        );
    }
}

#[cfg(unix)]
/// RC2-ENGG1 (RC2-ENGALL slice review P2): the `--expect` file is read
/// through descriptors anchored to `--dir`. A folder inside the run folder
/// swapped for a link to somewhere else while the check runs is refused;
/// the file behind the link is never read. Before, the check approved the
/// path and the runner opened it again later by name, so the swap won.
#[test]
fn rc2engg1_a_folder_swapped_for_a_link_during_the_check_is_never_read() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("run");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/out.txt"), "ok").unwrap();
    let elsewhere = base.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("out.txt"), "secret").unwrap();

    // Untouched, the file inside is read.
    assert_eq!(
        crate::expect_file_in_dir(&dir, "sub/out.txt").as_deref(),
        Ok("ok")
    );
    // Swapped while the check runs: refused, and never the outside file.
    let got = crate::read_expect_file(&dir, "sub/out.txt", || {
        std::fs::rename(dir.join("sub"), dir.join("sub-moved")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("sub")).unwrap();
    });
    assert!(
        got.is_err(),
        "a folder swapped for a link must be refused, got {got:?}"
    );
}

// ---- RC2-R3: usability RC2 round 3 ----------------------------------------

/// RC2-R3: a Work turn's host (Ask mode: nothing is approved without a
/// card), whose folder is the owner's Work folder. Any card fails the test.
struct WorkFolderHost;

impl TurnHost for WorkFolderHost {
    fn approve(&mut self, _call_id: &str, prompt: &str, _risk: RiskClass) -> ApprovalOutcome {
        panic!("no card expected for a read in the Work folder, got: {prompt}")
    }
    fn relative_paths_mean_the_folder(&self) -> bool {
        true
    }
}

/// The owner's shell server's `read_file`: it says nothing about what it
/// does, so the registry holds it at "privileged" (a card in Ask mode).
fn owners_read_file() -> awili_tools::ToolDef {
    awili_tools::ToolDef::from_parts(
        awili_tools::ToolOrigin::new("mcp:mac-shell"),
        "read_file",
        awili_tools::CanonicalDescriptor {
            schema: serde_json::json!({"type": "object"}),
            side_effect: "privileged".into(),
            required_capabilities: vec![],
            executor: "mcp:mac-shell".into(),
        },
    )
}

/// RC2-R3 (usability RC2 round 3, Major 1 and Minor 10): in Work, Ask mode,
/// "what is in alpha.txt?" \u{2014} Gemma called the owner's `read_file` with
/// "alpha.txt": a card "this runs with full rights", "read_file FAILED",
/// then `find / -name alpha.txt`. Now that call is answered at once with no
/// card and nothing run, in plain words naming the engine's own `fs_read`
/// and the file's full path (and no whole-disk search); the relative
/// `fs_read` that follows reads the file in the Work folder, also with no
/// card.
#[test]
fn rc2r3_a_relative_read_in_the_work_folder_needs_no_card() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("alpha.txt"), "one\n").unwrap();
    let bodies = vec![
        sse_tool_round(&[("call_m", "read_file", r#"{"path": "alpha.txt"}"#)]),
        sse_tool_round(&[("call_r", "fs.read", r#"{"path": "alpha.txt"}"#)]),
        sse_text_final("It says: one"),
    ];
    let mut host = WorkFolderHost;
    let r = run_with_extras(&root, bodies, &mut host, &[owners_read_file()]);
    let report = r.result.expect("turn completes");
    assert_eq!(report.final_text, "It says: one");
    let results: Vec<&awili_engine::events::ToolResultRecord> = report
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolResultIngested(rec) => Some(rec),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2, "{results:#?}");
    let full = format!("{}/alpha.txt", root.path().display());
    let told = &results[0].content;
    assert!(results[0].is_error, "{told}");
    for words in [
        "Not run",
        "fs_read (path \"alpha.txt\")",
        full.as_str(),
        "Do not search the whole disk",
    ] {
        assert!(told.contains(words), "{words:?} missing from: {told}");
    }
    assert!(!results[1].is_error, "{:?}", results[1].content);
    assert_eq!(results[1].content, "one\n");
    // The owner's tool never ran: no intent for it.
    assert_all_clean(&r.records, 1);
}

/// RC2-R3: which arguments count as a relative path, and what the model is
/// told.
#[test]
fn rc2r3_only_a_relative_path_argument_is_redirected() {
    for (args, want) in [
        (r#"{"path": "alpha.txt"}"#, Some("alpha.txt")),
        (r#"{"file_path": "notes/a.md"}"#, Some("notes/a.md")),
        (r#"{"cwd": ".", "command": "ls"}"#, Some(".")),
        (r#"{"path": "/Projects/w1/alpha.txt"}"#, None),
        (r#"{"path": "~/alpha.txt"}"#, None),
        (r#"{"url": "https://x.y/z"}"#, None),
        (r#"{"path": "https://x.y/z"}"#, None),
        (r#"{"command": "cat alpha.txt"}"#, None),
        (r#"{"path": ""}"#, None),
        ("not json", None),
    ] {
        assert_eq!(relative_path_arg(args).as_deref(), want, "{args}");
    }
    // Only a local file or shell tool: a path in a Gitea repository or an
    // Outline document is not a file on this Mac.
    assert_eq!(
        local_relative_path("read_file", r#"{"path": "alpha.txt"}"#).as_deref(),
        Some("alpha.txt")
    );
    assert_eq!(
        local_relative_path("run_command", r#"{"command": "ls", "cwd": "."}"#).as_deref(),
        Some(".")
    );
    assert_eq!(
        local_relative_path(
            "get_file_content",
            r#"{"owner": "owner", "repo": "awili", "filePath": "README.md"}"#
        ),
        None
    );
    assert_eq!(
        local_relative_path("create_file", r#"{"path": "docs/a.md"}"#),
        None
    );
    let told = relative_path_redirect("read_file", "./alpha.txt", "/Projects/w1/");
    assert!(told.contains("/Projects/w1/alpha.txt"), "{told}");
    assert!(!told.contains("find /"), "{told}");
}

/// RC2-R3 (usability RC2 round 3, Minor 6): after the owner's Deny the
/// model is told plainly that he declined it \u{2014} never words a model reads
/// as a missing permission ("user denied the request" read as "I don't have
/// the permissions required").
#[test]
fn rc2r3_after_deny_the_model_is_told_the_owner_declined() {
    let root = tempfile::tempdir().unwrap();
    let bodies = vec![
        sse_tool_round(&[(
            "call_w",
            "fs.write",
            r#"{"path":"notes.txt","contents":"hi"}"#,
        )]),
        sse_text_final("Not done: you declined it."),
    ];
    let mut deny = |_p: &str, _r: RiskClass| UserVerdict::Deny {
        reason: "denied by the user".into(),
    };
    let r = run(&root, bodies, &mut deny);
    let report = r.result.expect("turn completes after the denial");
    assert!(!root.path().join("notes.txt").exists(), "nothing written");
    let told = report
        .events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolResultIngested(rec) if rec.is_error => Some(rec.content.clone()),
            _ => None,
        })
        .expect("the model is told");
    assert_eq!(told, OWNER_DECLINED);
    for words in ["he pressed Deny", "his choice", "declined it"] {
        assert!(told.contains(words), "{words:?} missing from: {told}");
    }
    assert!(!told.contains("user denied the request"), "{told}");
}

/// RC2-R3: a remote tool given a relative path (a file in a Gitea
/// repository) is not redirected in the Work folder: it asks as before.
#[test]
fn rc2r3_a_remote_tools_relative_path_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let gitea = awili_tools::ToolDef::from_parts(
        awili_tools::ToolOrigin::new("mcp:gitea"),
        "get_file_content",
        awili_tools::CanonicalDescriptor {
            schema: serde_json::json!({"type": "object"}),
            side_effect: "privileged".into(),
            required_capabilities: vec![],
            executor: "mcp:gitea".into(),
        },
    );
    struct Asked(Vec<String>);
    impl TurnHost for Asked {
        fn approve(&mut self, _id: &str, prompt: &str, _r: RiskClass) -> ApprovalOutcome {
            self.0.push(prompt.to_string());
            ApprovalOutcome::Verdict(UserVerdict::Deny {
                reason: "denied by the user".into(),
            })
        }
        fn relative_paths_mean_the_folder(&self) -> bool {
            true
        }
    }
    let bodies = vec![
        sse_tool_round(&[(
            "call_g",
            "get_file_content",
            r#"{"owner": "owner", "repo": "awili", "filePath": "README.md"}"#,
        )]),
        sse_text_final("ok"),
    ];
    let mut host = Asked(Vec::new());
    let r = run_with_extras(&root, bodies, &mut host, &[gitea]);
    let report = r.result.expect("turn completes");
    assert_eq!(host.0.len(), 1, "it asked, as before");
    assert!(report.events.iter().all(|e| !matches!(
        e,
        AgentEvent::ToolResultIngested(rec) if rec.content.contains("Not run")
    )));
}

/// RC2-R3M1 (RC2-R3 review P2): a tool called `read_file` from a server that
/// does not work on this Mac (a shell server on another machine, a
/// repository server) keeps its own relative path in the Work folder: it is
/// not answered with the Work folder's path, it asks as before. The owner's
/// own `mac-shell` and the filesystem server are the only local ones.
#[test]
fn rc2r3m1_a_remote_read_file_is_not_redirected() {
    let root = tempfile::tempdir().unwrap();
    let remote = awili_tools::ToolDef::from_parts(
        awili_tools::ToolOrigin::new("mcp:hostb-shell"),
        "read_file",
        awili_tools::CanonicalDescriptor {
            schema: serde_json::json!({"type": "object"}),
            side_effect: "privileged".into(),
            required_capabilities: vec![],
            executor: "mcp:hostb-shell".into(),
        },
    );
    struct Asked(Vec<String>);
    impl TurnHost for Asked {
        fn approve(&mut self, _id: &str, prompt: &str, _r: RiskClass) -> ApprovalOutcome {
            self.0.push(prompt.to_string());
            ApprovalOutcome::Verdict(UserVerdict::Deny {
                reason: "denied by the user".into(),
            })
        }
        fn relative_paths_mean_the_folder(&self) -> bool {
            true
        }
    }
    let bodies = vec![
        sse_tool_round(&[("call_x", "read_file", r#"{"path": "docs/README.md"}"#)]),
        sse_text_final("ok"),
    ];
    let mut host = Asked(Vec::new());
    let r = run_with_extras(&root, bodies, &mut host, &[remote]);
    let report = r.result.expect("turn completes");
    assert_eq!(host.0.len(), 1, "the remote read_file asked, as before");
    assert!(report.events.iter().all(|e| !matches!(
        e,
        AgentEvent::ToolResultIngested(rec) if rec.content.contains("Not run")
    )));
    assert!(is_local_file_origin("mcp:mac-shell"));
    assert!(is_local_file_origin("mcp:filesystem"));
    assert!(!is_local_file_origin("mcp:hostb-shell"));
    assert!(!is_local_file_origin("mcp:gitea"));
    assert!(!is_local_file_origin(
        awili_engine_authz::policy::BUILTIN_FS_ORIGIN
    ));
}

/// The owner's shell server's tool `name` (privileged: a card in Ask mode).
fn owners_tool(name: &str) -> awili_tools::ToolDef {
    awili_tools::ToolDef::from_parts(
        awili_tools::ToolOrigin::new("mcp:mac-shell"),
        name,
        awili_tools::CanonicalDescriptor {
            schema: serde_json::json!({"type": "object"}),
            side_effect: "privileged".into(),
            required_capabilities: vec![],
            executor: "mcp:mac-shell".into(),
        },
    )
}

/// RC2-R3M5 (RC2-R31 review P2): the filesystem server's
/// `read_multiple_files` takes a list (`paths`) and `move_file` takes
/// `source` and `destination`; given relative paths in the Work folder (Ask
/// mode) they are answered at once like `read_file` \u{2014} no card, nothing
/// run \u{2014} with the full path.
#[test]
fn rc2r3m5_a_list_of_paths_and_a_move_are_redirected_too() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("alpha.txt"), "one\n").unwrap();
    let bodies = vec![
        sse_tool_round(&[(
            "call_m",
            "read_multiple_files",
            r#"{"paths": ["/etc/hosts", "alpha.txt"]}"#,
        )]),
        sse_tool_round(&[(
            "call_v",
            "move_file",
            r#"{"source": "alpha.txt", "destination": "/tmp/beta.txt"}"#,
        )]),
        sse_text_final("done"),
    ];
    let mut host = WorkFolderHost;
    let r = run_with_extras(
        &root,
        bodies,
        &mut host,
        &[owners_tool("read_multiple_files"), owners_tool("move_file")],
    );
    let report = r.result.expect("turn completes");
    let results: Vec<&awili_engine::events::ToolResultRecord> = report
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolResultIngested(rec) => Some(rec),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2, "{results:#?}");
    let full = format!("{}/alpha.txt", root.path().display());
    for rec in &results {
        assert!(rec.is_error, "{}", rec.content);
        assert!(rec.content.starts_with("Not run"), "{}", rec.content);
        assert!(rec.content.contains(&full), "{}", rec.content);
    }
    assert!(root.path().join("alpha.txt").exists(), "nothing moved");
    assert_all_clean(&r.records, 0);
    assert_eq!(
        relative_path_arg(r#"{"paths": ["/a", "~/b"]}"#),
        None,
        "full paths only: not redirected"
    );
    assert_eq!(
        relative_path_arg(r#"{"source": "/a", "destination": "b/c.txt"}"#).as_deref(),
        Some("b/c.txt")
    );
}

/// RC2-R3M6 (RC2-R31 review P3): a file name may start or end with a space;
/// the redirect keeps the path exactly as given, so the full path it names
/// is that file's, not another one's.
#[test]
fn rc2r3m6_a_path_keeps_its_spaces() {
    assert_eq!(
        relative_path_arg(r#"{"path": " alpha.txt"}"#).as_deref(),
        Some(" alpha.txt")
    );
    assert_eq!(
        relative_path_arg(r#"{"paths": ["notes/a b.md "]}"#).as_deref(),
        Some("notes/a b.md ")
    );
    assert_eq!(relative_path_arg(r#"{"path": "   "}"#), None);
    let told = relative_path_redirect("read_file", " alpha.txt", "/Projects/w1");
    assert!(told.contains("/Projects/w1/ alpha.txt."), "{told}");
    assert!(told.contains("fs_read (path \" alpha.txt\")"), "{told}");
}

/// RC2-R51 (usability RC2 round 4, Major 3: Gemma's write outside the Work
/// folder came back "denied: path … outside bound folder …" and it told the
/// owner its "write_file tool is restricted to … the bound folder"): the
/// refusal reaches the model in plain, correct words, and nothing is
/// written.
#[test]
fn rc2r51_a_refused_path_is_told_in_plain_words() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("x.txt");
    let writer = awili_tools::ToolDef::from_parts(
        awili_tools::ToolOrigin::new("mcp:mac-shell"),
        "write_file",
        awili_tools::CanonicalDescriptor {
            schema: serde_json::json!({"type": "object"}),
            side_effect: "write".into(),
            required_capabilities: vec![],
            executor: "mcp:mac-shell".into(),
        },
    );
    let args = format!(r#"{{"path":"{}","contents":"hi"}}"#, target.display());
    let bodies = vec![
        sse_tool_round(&[("call_w", "write_file", &args)]),
        sse_text_final("not done"),
    ];
    let mut approver = no_dialog_expected();
    let r = run_with_extras(&root, bodies, &mut approver, &[writer]);
    let report = r.result.expect("turn completes");
    let told = report
        .events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolResultIngested(rec) if rec.is_error => Some(rec.content.clone()),
            _ => None,
        })
        .expect("the refusal reached the model");
    let folder = root.path().display().to_string();
    assert!(told.starts_with("Not done: "), "{told}");
    assert!(
        told.contains(&format!("is outside this conversation's folder, {folder}")),
        "{told}"
    );
    for jargon in ["denied:", "bound folder", "unresolvable"] {
        assert!(!told.contains(jargon), "{told}");
    }
    assert!(!target.exists(), "nothing was written");
}

/// RC2-R5M1 (slice review befb867d, P1): mac-shell and lightcraft each offer
/// `run_command`, and a third server offers a tool literally named
/// `mac-shell.run_command`. The request advertises every tool under its own
/// name, and a call by mac-shell's advertised name reaches mac-shell's tool
/// (its own risk, a dialog), not the third server's.
#[test]
fn rc2r5m1_a_call_by_an_advertised_name_reaches_its_own_server() {
    let root = tempfile::tempdir().unwrap();
    let tool = |origin: &str, name: &str, side_effect: &str| {
        awili_tools::ToolDef::from_parts(
            awili_tools::ToolOrigin::new(origin),
            name,
            awili_tools::CanonicalDescriptor {
                schema: serde_json::json!({"type": "object"}),
                side_effect: side_effect.into(),
                required_capabilities: vec![],
                executor: origin.into(),
            },
        )
    };
    let extras = [
        tool("mcp:mac-shell", "run_command", "dangerous"),
        tool("mcp:lightcraft", "run_command", "write"),
        tool("mcp:third", "mac-shell.run_command", "read_only"),
    ];
    let bodies = vec![
        sse_tool_round(&[("call_s", "mac-shell_run_command-2", r#"{"command":"ls"}"#)]),
        sse_text_final("not done"),
    ];
    let mut dialogs: Vec<RiskClass> = Vec::new();
    let r = {
        let mut approver = |_p: &str, r: RiskClass| {
            dialogs.push(r);
            UserVerdict::Deny {
                reason: "test".into(),
            }
        };
        run_with_extras(&root, bodies, &mut approver, &extras)
    };
    let report = r.result.expect("turn completes");
    let body: serde_json::Value =
        serde_json::from_str(report.requests.first().expect("one request")).unwrap();
    let names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    let distinct: BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        names.len(),
        "no two tools share a name: {names:?}"
    );
    for n in [
        "mac-shell_run_command-2",
        "lightcraft_run_command",
        "mac-shell_run_command",
    ] {
        assert!(names.contains(&n), "{n} advertised: {names:?}");
    }
    assert_eq!(
        dialogs,
        vec![RiskClass::Irreversible],
        "the call reached mac-shell's dangerous tool, not the third server's read-only one"
    );
}
/// RC2-R51: a tool name the model has none of is told plainly (and still as
/// an unresolvable tool name).
#[test]
fn rc2r51_an_unknown_tool_name_is_told_plainly() {
    let told = crate::unresolved_text(&["run_command".to_string()]);
    assert!(told.starts_with("Not done: no tool here is named \u{201C}run_command\u{201D}"));
    assert!(told.contains("unresolvable tool"), "{told}");
    assert!(!told.contains("batch refused"), "{told}");
}

/// RC2-R52 (usability RC2 round 4, Majors 1 and 2: with all of the owner's
/// MCP servers up — 337 tools — every in-app request failed "cannot fit:
/// 40742 tokens needed …, budget 30208" before it was sent, though the
/// model server counted it at about 27,000 of its 32,768): a large tool set
/// and a long brief fit the 32K window, every tool and every parameter
/// still advertised, the tool definitions within their half of the input
/// budget, and byte-identical in every request of the turn (the model
/// server's prompt cache holds).
#[test]
fn rc2r52_the_owners_whole_tool_set_fits_the_window() {
    const WINDOW: u64 = 32_768;
    let root = tempfile::tempdir().unwrap();
    let words = |n: usize| "word ".repeat(n / 5);
    let extras: Vec<awili_tools::ToolDef> = (0..337)
        .map(|i| {
            let props: serde_json::Map<String, serde_json::Value> = ["path", "mode"]
                .iter()
                .map(|p| {
                    (
                        p.to_string(),
                        serde_json::json!({
                            "type": "string",
                            "title": "A title no model reads",
                            "description": words(120),
                        }),
                    )
                })
                .collect();
            awili_tools::ToolDef::from_parts(
                awili_tools::ToolOrigin::new(format!("mcp:server{}", i % 5)),
                format!("tool_{i:03}"),
                awili_tools::CanonicalDescriptor {
                    schema: serde_json::json!({
                        "type": "object",
                        "properties": props,
                        "required": ["path"],
                    }),
                    side_effect: "read_only".into(),
                    required_capabilities: vec![],
                    executor: format!("mcp:server{}", i % 5),
                },
            )
            .with_description(words(400))
        })
        .collect();
    let host = host().lock().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-r52").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut transport = Script {
        bodies: vec![
            sse_tool_round(&[("call_g", "fs.glob", r#"{"pattern":"*"}"#)]),
            sse_text_final("ok"),
        ]
        .into(),
    };
    let mut history = ContextHistory::new();
    // The owner's brief: his rules and AGENTS.md, about 22,000 characters.
    history.push_pinned(words(22_000));
    let mut approver = no_dialog_expected();
    let report = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "what is in alpha.txt?",
        &mut approver,
        &mut log,
        &mut grants,
        &extras,
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        WINDOW,
        "turn-r52-0",
        &mut history,
    )
    .expect("the request fits: no \"cannot fit\"");
    assert_eq!(report.final_text, "ok");
    assert_eq!(report.requests.len(), 2);
    let tools_of = |i: usize| {
        let req: serde_json::Value = serde_json::from_str(&report.requests[i]).unwrap();
        req["tools"].clone()
    };
    let tools = tools_of(0);
    assert_eq!(tools, tools_of(1), "the same definitions in every request");
    let list = tools.as_array().unwrap();
    assert_eq!(list.len(), 5 + 337, "every tool is advertised");
    for i in 0..337 {
        let t = list
            .iter()
            .find(|t| t["function"]["name"] == format!("tool_{i:03}"))
            .unwrap_or_else(|| panic!("tool_{i:03} advertised"));
        let params = &t["function"]["parameters"];
        for p in ["path", "mode"] {
            assert_eq!(params["properties"][p]["type"], "string");
        }
        assert_eq!(params["required"][0], "path");
        assert!(params["properties"]["mode"].get("title").is_none());
    }
    // The brief counts 7,334 tokens (22,000 characters), plus its framing.
    let cap = crate::tool_block_cap_tokens(WINDOW, 7_342) as usize * 4;
    assert!(
        tools.to_string().len() <= cap,
        "{} > {cap}",
        tools.to_string().len()
    );
}

/// RC2-R52: a tool set that fits is sent exactly as the servers described
/// it; the engine's own file tools are never shortened.
#[test]
fn rc2r52_a_small_tool_set_is_left_whole() {
    let tools = vec![
        serde_json::json!({"type": "function", "function": {"name": "fs_read", "description": "B".repeat(9_000), "parameters": {"type": "object", "title": "kept"}}}),
        serde_json::json!({"type": "function", "function": {"name": "x", "description": "d", "parameters": {"type": "object", "title": "kept"}}}),
    ];
    assert_eq!(crate::fit_tool_block(tools.clone(), 1, 32_768, 0), tools);
    let tight = crate::fit_tool_block(tools.clone(), 1, 4_096 + 2_560, 0);
    assert_eq!(tight[0], tools[0], "the builtin stays whole");
    // A parameter NAMED like a schema note or check keeps its place; the
    // notes and checks themselves go.
    let named = vec![
        serde_json::json!({"type": "function", "function": {"name": "export", "description": "D".repeat(20_000), "parameters": {"type": "object", "properties": {
        "format": {"type": "string", "enum": ["png", "jpg"], "default": "png", "title": "Format"},
        "default": {"type": "boolean"}
    }, "required": ["format"]}}}),
    ];
    let fit = crate::fit_tool_block(named, 0, 4_096 + 2_560, 0);
    let props = &fit[0]["function"]["parameters"]["properties"];
    assert_eq!(props["format"]["enum"], serde_json::json!(["png", "jpg"]));
    assert!(props["format"].get("default").is_none() && props["format"].get("title").is_none());
    assert_eq!(props["default"]["type"], "boolean");
    assert_eq!(fit[0]["function"]["parameters"]["required"][0], "format");
}

struct ImmediateCatalogCall(Option<McpDispatchOutcome>);

impl McpCall for ImmediateCatalogCall {
    fn poll_outcome(&mut self, _deadline: Instant) -> Option<McpDispatchOutcome> {
        self.0.take()
    }

    fn cancel(&mut self, _deadline: Instant) -> Result<(), awili_tools::runtime::RefusalKind> {
        Ok(())
    }
}

#[derive(Clone, Default)]
struct CatalogDispatchProbe {
    calls: Arc<Mutex<Vec<(String, String, String)>>>,
}

impl McpExecutor for CatalogDispatchProbe {
    fn dispatch_call(
        &self,
        dispatch: McpDispatch,
    ) -> Result<Box<dyn McpCall + '_>, McpSubmitRefused> {
        self.calls.lock().unwrap().push((
            dispatch.origin.origin.clone(),
            dispatch.call.name.clone(),
            dispatch.call.arguments.clone(),
        ));
        let result = ToolResultRecord {
            awili_call_id: dispatch.call.awili_call_id.clone(),
            content: "fixture MCP result".into(),
            is_error: false,
        };
        Ok(Box::new(ImmediateCatalogCall(Some(
            McpDispatchOutcome::Terminal {
                ticket: dispatch.ticket,
                disposition: awili_tools::runtime::CallDisposition::Completed,
                result,
            },
        ))))
    }

    fn publish_roots(
        &self,
        _origin: awili_engine::ToolOrigin,
        _generation: awili_engine::generation::ConnectionGeneration,
        _epoch: u64,
    ) -> Result<(), awili_tools::runtime::RefusalKind> {
        Ok(())
    }
}

struct AllowCatalogRoots;

impl RootsPolicy for AllowCatalogRoots {
    fn accepts(
        &self,
        _verified: &awili_tools::attest::VerifiedLaunch,
        _connected: &awili_tools::runtime::ConnectedLaunch,
        _host: &HostRootProposal,
    ) -> Result<(), String> {
        Ok(())
    }
}

fn catalog_test_roots(executor: &CatalogDispatchProbe) -> RootsAcceptance {
    catalog_test_roots_for(executor, "mcp:fixture")
}

fn catalog_test_roots_for(executor: &CatalogDispatchProbe, origin: &str) -> RootsAcceptance {
    use awili_tools::attest::{LaunchKind, LaunchToken, SetupIdentityId, SetupRecordFacts};
    use awili_tools::runtime::{ConnectedLaunch, McpEvent, McpRuntimeHandle};

    let facts = SetupRecordFacts {
        origin: awili_engine::ToolOrigin::new(origin),
        setup_id: SetupIdentityId::parse("catalog-fixture").unwrap(),
        pinned_requirement: "cdhash H\"00\"".into(),
        expected_protocol_name: "fixture-server".into(),
        launch_kind: LaunchKind::Direct,
        program: "/tmp/fixture-server".into(),
    };
    let mut generations = awili_engine::generation::GenerationAllocator::new();
    let token = LaunchToken::mint_verified(&facts, generations.next());
    let (sink, stream) = awili_tools::runtime::event_channel::<String>(4);
    let (commands, _receiver) = async_channel::bounded(1);
    let handle = McpRuntimeHandle::from_parts(commands, stream, std::thread::spawn(|| {}));
    sink.emit_server_connected_blocking(
        token,
        "fixture-server".into(),
        Vec::new(),
        Vec::new(),
        Some(1),
    )
    .unwrap();
    drop(sink);
    let connected: ConnectedLaunch = match handle.try_recv_event() {
        Some(McpEvent::ServerConnected(connected)) => connected,
        other => panic!("expected verified fixture connection, got {other:?}"),
    };
    drop(handle);
    let mut roots = RootsAcceptance::default();
    bring_up_roots(
        executor,
        &mut roots,
        &connected,
        &HostRootProposal {
            origin: awili_engine::ToolOrigin::new(origin),
            proposed_roots: vec!["/tmp".into()],
        },
        &AllowCatalogRoots,
    )
    .unwrap();
    roots
}

fn catalog_fixture_tools() -> (awili_tools::ToolDef, awili_tools::ToolDef) {
    let small = awili_tools::ToolDef::from_parts(
        awili_engine::ToolOrigin::new("mcp:fixture"),
        "read_fixture",
        awili_tools::CanonicalDescriptor {
            schema: serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
            side_effect: "read_only".into(),
            required_capabilities: vec![],
            executor: "mcp:fixture".into(),
        },
    )
    .with_description("Read the synthetic fixture.");
    // Property names are intentionally large: the existing fit path keeps
    // property names and types intact, so this remains irreducible and
    // reliably forces catalog mode in a small synthetic context window.
    let mut properties = serde_json::Map::new();
    for index in 0..1_500 {
        properties.insert(
            format!("property_{index:04}_{}", "x".repeat(64)),
            serde_json::json!({"type":"string"}),
        );
    }
    let large = awili_tools::ToolDef::from_parts(
        awili_engine::ToolOrigin::new("mcp:fixture"),
        "large_fixture",
        awili_tools::CanonicalDescriptor {
            schema: serde_json::json!({"type":"object","properties":properties}),
            side_effect: "read_only".into(),
            required_capabilities: vec![],
            executor: "mcp:fixture".into(),
        },
    )
    .with_description("Large schema fixture.");
    (small, large)
}

#[test]
fn catalog_search_projects_returned_schema_and_dispatches_without_explicit_select() {
    let root = tempfile::tempdir().unwrap();
    let fake = CatalogDispatchProbe::default();
    let roots = catalog_test_roots(&fake);
    let executors = Executors::mcp(fake.clone());
    let (small, large) = catalog_fixture_tools();
    let query = serde_json::json!({"query":"read_fixture"}).to_string();
    let bodies = vec![
        sse_tool_round(&[("find", "tools_find", &query)]),
        sse_tool_round(&[("read", "read_fixture", "{}")]),
        sse_text_final("actual fixture result"),
    ];
    let composition = Composition {
        executors: &executors,
        install_authority: &awili_engine_authz::NoInstallAuthority,
        roots: &roots,
    };
    let mut approver = no_dialog_expected();
    let run = run_in(
        &root,
        bodies,
        &mut approver,
        &[small.clone(), large],
        &composition,
        8_192,
    );
    let report = run.result.expect("search should prepare its returned tool");
    assert_eq!(
        fake.calls.lock().unwrap().as_slice(),
        &[("mcp:fixture".into(), "read_fixture".into(), "{}".into())]
    );
    let first: serde_json::Value = serde_json::from_str(&report.requests[0]).unwrap();
    let next: serde_json::Value = serde_json::from_str(&report.requests[1]).unwrap();
    assert!(!first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["function"]["name"] == "read_fixture"));
    let projected = next["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == "read_fixture")
        .unwrap();
    assert_eq!(projected["function"]["parameters"], small.descriptor.schema);
    assert!(!next["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["function"]["name"] == "large_fixture"));
    assert_all_clean(&run.records, 2);
}

#[test]
fn catalog_search_retains_prior_tools_and_empty_browsing_does_not_clear_them() {
    let root = tempfile::tempdir().unwrap();
    let fake = CatalogDispatchProbe::default();
    let roots = catalog_test_roots(&fake);
    let executors = Executors::mcp(fake.clone());
    let (small, large) = catalog_fixture_tools();
    let other = awili_tools::ToolDef::from_parts(
        awili_engine::ToolOrigin::new("mcp:fixture"),
        "other_fixture",
        small.descriptor.clone(),
    )
    .with_description("Another synthetic read tool.");
    let first_query = serde_json::json!({"query":"read_fixture"}).to_string();
    let second_query = serde_json::json!({"query":"other_fixture"}).to_string();
    let browse = serde_json::json!({"query":"","limit":1}).to_string();
    let bodies = vec![
        sse_tool_round(&[("find-first", "tools_find", &first_query)]),
        sse_tool_round(&[("find-second", "tools_find", &second_query)]),
        sse_tool_round(&[("browse", "tools_find", &browse)]),
        sse_tool_round(&[
            ("read-first", "read_fixture", "{}"),
            ("read-second", "other_fixture", "{}"),
        ]),
        sse_text_final("both discovered tools ran"),
    ];
    let composition = Composition {
        executors: &executors,
        install_authority: &awili_engine_authz::NoInstallAuthority,
        roots: &roots,
    };
    let mut approver = no_dialog_expected();
    let run = run_in(
        &root,
        bodies,
        &mut approver,
        &[small, large, other],
        &composition,
        8_192,
    );
    let report = run
        .result
        .expect("later searches must retain previously discovered schemas");
    for index in [2, 3] {
        let request: serde_json::Value = serde_json::from_str(&report.requests[index]).unwrap();
        for name in ["read_fixture", "other_fixture"] {
            assert!(request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == name));
        }
    }
    assert_eq!(fake.calls.lock().unwrap().len(), 2);
    assert_all_clean(&run.records, 5);
}

#[test]
fn catalog_search_union_over_twenty_preserves_the_previous_projection() {
    let root = tempfile::tempdir().unwrap();
    let fake = CatalogDispatchProbe::default();
    let roots = catalog_test_roots(&fake);
    let executors = Executors::mcp(fake.clone());
    let (small, large) = catalog_fixture_tools();
    let mut extras = vec![small.clone(), large];
    for index in 0..20 {
        extras.push(
            awili_tools::ToolDef::from_parts(
                awili_engine::ToolOrigin::new("mcp:fixture"),
                format!("cap_fixture_{index}"),
                small.descriptor.clone(),
            )
            .with_description("A bounded secondary tool."),
        );
    }
    let first_query = serde_json::json!({"query":"read_fixture"}).to_string();
    let cap_query = serde_json::json!({"query":"cap_fixture","limit":20}).to_string();
    let bodies = vec![
        sse_tool_round(&[("find-first", "tools_find", &first_query)]),
        sse_tool_round(&[("find-page", "tools_find", &cap_query)]),
        sse_tool_round(&[("read", "read_fixture", "{}")]),
        sse_text_final("the original tool is still callable"),
    ];
    let composition = Composition {
        executors: &executors,
        install_authority: &awili_engine_authz::NoInstallAuthority,
        roots: &roots,
    };
    let mut approver = no_dialog_expected();
    let run = run_in(&root, bodies, &mut approver, &extras, &composition, 16_384);
    let report = run
        .result
        .expect("search must not grow the active set above twenty");
    let request: serde_json::Value = serde_json::from_str(&report.requests[2]).unwrap();
    let tools = request["tools"].as_array().unwrap();
    assert!(tools
        .iter()
        .any(|t| t["function"]["name"] == "read_fixture"));
    assert!(!tools.iter().any(|t| t["function"]["name"]
        .as_str()
        .is_some_and(|n| n.starts_with("cap_fixture_"))));
    assert!(report.events.iter().any(|event| matches!(event,
        AgentEvent::ToolResultIngested(record) if !record.is_error && serde_json::from_str::<serde_json::Value>(&record.content).is_ok_and(|v| v["selection_required"] == true)
    )));
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
    assert_all_clean(&run.records, 3);
}

#[test]
fn catalog_search_and_selected_tool_in_one_batch_dispatches_neither() {
    let root = tempfile::tempdir().unwrap();
    let fake = CatalogDispatchProbe::default();
    let roots = catalog_test_roots(&fake);
    let executors = Executors::mcp(fake.clone());
    let (small, large) = catalog_fixture_tools();
    let query = serde_json::json!({"query":"read_fixture"}).to_string();
    let bodies = vec![
        sse_tool_round(&[("find-first", "tools_find", &query)]),
        sse_tool_round(&[
            ("find-again", "tools_find", &query),
            ("read", "read_fixture", "{}"),
        ]),
        sse_text_final("the mixed batch did not run"),
    ];
    let composition = Composition {
        executors: &executors,
        install_authority: &awili_engine_authz::NoInstallAuthority,
        roots: &roots,
    };
    let mut approver = no_dialog_expected();
    let run = run_in(
        &root,
        bodies,
        &mut approver,
        &[small, large],
        &composition,
        8_192,
    );
    let report = run
        .result
        .expect("mixed catalog batch is a recoverable protocol error");
    assert!(fake.calls.lock().unwrap().is_empty());
    let rejected = report.events.iter().filter(|event| matches!(event,
        AgentEvent::ToolResultIngested(record) if record.is_error && record.content.contains("must be called alone")
    )).count();
    assert_eq!(rejected, 2);
    assert_all_clean(&run.records, 1);
}

#[test]
fn oversized_catalog_search_preserves_previous_projection_and_remains_metadata_only() {
    let root = tempfile::tempdir().unwrap();
    let fake = CatalogDispatchProbe::default();
    let roots = catalog_test_roots(&fake);
    let executors = Executors::mcp(fake.clone());
    let (small, large) = catalog_fixture_tools();
    let small_query = serde_json::json!({"query":"read_fixture"}).to_string();
    let large_query = serde_json::json!({"query":"large_fixture"}).to_string();
    let bodies = vec![
        sse_tool_round(&[("find-small", "tools_find", &small_query)]),
        sse_tool_round(&[("find-large", "tools_find", &large_query)]),
        sse_tool_round(&[("read", "read_fixture", "{}")]),
        sse_text_final("read the still-active small fixture"),
    ];
    let composition = Composition {
        executors: &executors,
        install_authority: &awili_engine_authz::NoInstallAuthority,
        roots: &roots,
    };
    let mut approver = no_dialog_expected();
    let run = run_in(
        &root,
        bodies,
        &mut approver,
        &[small, large],
        &composition,
        8_192,
    );
    let report = run
        .result
        .expect("an oversized search should preserve usable selection");
    let next: serde_json::Value = serde_json::from_str(&report.requests[2]).unwrap();
    let tools = next["tools"].as_array().unwrap();
    assert!(tools
        .iter()
        .any(|t| t["function"]["name"] == "read_fixture"));
    assert!(!tools
        .iter()
        .any(|t| t["function"]["name"] == "large_fixture"));
    assert!(report.events.iter().any(|event| matches!(event,
        AgentEvent::ToolResultIngested(record) if !record.is_error && serde_json::from_str::<serde_json::Value>(&record.content).is_ok_and(|v| v["selection_required"] == true)
    )));
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
    assert_all_clean(&run.records, 3);
}

#[test]
fn deferred_catalog_selection_is_transactional_and_dispatches_original_mcp_tool() {
    let root = tempfile::tempdir().unwrap();
    let fake = CatalogDispatchProbe::default();
    let roots = catalog_test_roots(&fake);
    let executors = Executors::mcp(fake.clone());
    let (small, large) = catalog_fixture_tools();
    let small_alias = "read_fixture";
    let large_alias = "large_fixture";
    let query =
        serde_json::json!({"query":"read_fixture","select":[],"limit":10,"offset":0}).to_string();
    let select_small =
        serde_json::json!({"query":"","select":[small_alias],"limit":10,"offset":0}).to_string();
    let select_large =
        serde_json::json!({"query":"","select":[large_alias],"limit":10,"offset":0}).to_string();
    let bodies = vec![
        sse_tool_round(&[("find-1", "tools_find", &query)]),
        sse_tool_round(&[("find-2", "tools_find", &select_small)]),
        sse_tool_round(&[("find-3", "tools_find", &select_large)]),
        sse_tool_round(&[("find-4", "tools_find", &select_small)]),
        sse_tool_round(&[("read-5", "read_fixture", "{}")]),
        sse_text_final("done"),
    ];
    let composition = Composition {
        executors: &executors,
        install_authority: &awili_engine_authz::NoInstallAuthority,
        roots: &roots,
    };
    let mut approver = no_dialog_expected();
    let run = run_in(
        &root,
        bodies,
        &mut approver,
        &[small.clone(), large.clone()],
        &composition,
        8_192,
    );
    let report = run
        .result
        .expect("catalog search and selection should complete");
    assert_eq!(report.final_text, "done");
    assert_eq!(report.requests.len(), 6);
    assert_all_clean(&run.records, 5);
    assert_eq!(
        fake.calls.lock().unwrap().as_slice(),
        &[("mcp:fixture".into(), "read_fixture".into(), "{}".into())]
    );
    let tools_at = |index: usize| -> Vec<serde_json::Value> {
        let request: serde_json::Value = serde_json::from_str(&report.requests[index]).unwrap();
        request["tools"].as_array().unwrap().clone()
    };
    let first = tools_at(0);
    assert!(first
        .iter()
        .any(|tool| tool["function"]["name"] == "tools_find"));
    assert!(!first
        .iter()
        .any(|tool| tool["function"]["name"] == small_alias));
    assert!(!first
        .iter()
        .any(|tool| tool["function"]["name"] == large_alias));
    for index in [2, 3, 4] {
        let tools = tools_at(index);
        assert!(tools
            .iter()
            .any(|tool| tool["function"]["name"] == small_alias));
        assert!(!tools
            .iter()
            .any(|tool| tool["function"]["name"] == large_alias));
        let schema = &tools
            .iter()
            .find(|tool| tool["function"]["name"] == small_alias)
            .unwrap()["function"]["parameters"];
        assert_eq!(schema, &small.descriptor.schema);
    }
    // A rejected replacement retains the previously accepted small schema
    // in the immediately following request; the same exact small alias can
    // then be selected again as an explicit retry.
    let result = report
        .events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolResultIngested(record)
                if record.content.contains("selection_too_large") =>
            {
                Some(record)
            }
            _ => None,
        })
        .expect("oversized selection gets a recoverable result");
    assert!(result.is_error);
    assert!(result.content.contains("selection_too_large"));
    let start_count = run
        .records
        .iter()
        .filter(|r| matches!(r, LogRecordV1::DispatchStarted { .. }))
        .count();
    let finalized_count = run
        .records
        .iter()
        .filter(|r| matches!(r, LogRecordV1::DispatchFinalized { .. }))
        .count();
    assert_eq!(
        (start_count, finalized_count),
        (5, 5),
        "four catalog calls and one selected MCP call use normal B2 lifecycle"
    );
}

#[test]
fn catalog_unknown_alias_guides_selection_and_selected_tool_dispatches() {
    let root = tempfile::tempdir().unwrap();
    let fake = CatalogDispatchProbe::default();
    let roots = catalog_test_roots(&fake);
    let executors = Executors::mcp(fake.clone());
    let (small, large) = catalog_fixture_tools();
    // The model first sends a wire-style alias where the catalog requires its
    // canonical alias; the bounded query returns the exact canonical choice.
    let initial = r#"{"select":["read_fixture"]}"#;
    let unknown = r#"{"query":"MCP.FIXTURE","select":["mcp__fixture__read_fixture"],"offset":4000000000}"#;
    // Selection takes priority over query. Omitted limit/offset use defaults.
    let select = r#"{"query":"read_fixture","select":["read_fixture"]}"#;
    let bodies = vec![
        sse_tool_round(&[("find-0", "tools_find", initial)]),
        sse_tool_round(&[("find-1", "tools_find", unknown)]),
        sse_tool_round(&[("find-2", "tools_find", select)]),
        sse_tool_round(&[("read-3", "read_fixture", "{}")]),
        sse_text_final("done"),
    ];
    let composition = Composition {
        executors: &executors,
        install_authority: &awili_engine_authz::NoInstallAuthority,
        roots: &roots,
    };
    let mut approver = no_dialog_expected();
    let run = run_in(
        &root,
        bodies,
        &mut approver,
        &[small.clone(), large],
        &composition,
        8_192,
    );
    let report = run
        .result
        .expect("unknown alias recovery and selected dispatch complete");
    assert_eq!(report.final_text, "done");
    assert_eq!(report.requests.len(), 5);
    let after_initial_selection: serde_json::Value =
        serde_json::from_str(&report.requests[1]).unwrap();
    assert!(after_initial_selection["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["function"]["name"] == "read_fixture"));
    let rejected = report
        .events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolResultIngested(record)
                if record.content.contains("unknown exact alias") =>
            {
                Some(record)
            }
            _ => None,
        })
        .expect("unknown alias returns a specific recoverable result");
    assert!(rejected.is_error);
    let rejected_json: serde_json::Value = serde_json::from_str(&rejected.content).unwrap();
    let matches = rejected_json["matches"].as_array().unwrap();
    assert!(matches.iter().any(|item| item["alias"] == "read_fixture"));
    assert!(matches
        .iter()
        .all(|item| item["source_hint"] == "mcp:fixture"));
    assert_eq!(
        fake.calls.lock().unwrap().as_slice(),
        &[("mcp:fixture".into(), "read_fixture".into(), "{}".into())]
    );
    let after_unknown: serde_json::Value = serde_json::from_str(&report.requests[2]).unwrap();
    let unknown_tools = after_unknown["tools"].as_array().unwrap();
    assert!(unknown_tools
        .iter()
        .any(|tool| tool["function"]["name"] == "tools_find"));
    let still_selected = unknown_tools
        .iter()
        .find(|tool| tool["function"]["name"] == "read_fixture")
        .unwrap();
    assert_eq!(still_selected["function"]["parameters"], small.descriptor.schema);
    let after_select: serde_json::Value = serde_json::from_str(&report.requests[3]).unwrap();
    let tools = after_select["tools"].as_array().unwrap();
    let selected = tools
        .iter()
        .find(|tool| tool["function"]["name"] == "read_fixture")
        .unwrap();
    assert_eq!(selected["function"]["parameters"], small.descriptor.schema);
    assert_all_clean(&run.records, 4);
}

#[test]
fn oversized_catalog_keeps_answer_only_request_and_complete_registry_searchable() {
    let root = tempfile::tempdir().unwrap();
    let (small, large) = catalog_fixture_tools();
    let bodies = vec![sse_text_final("The answer is 43.")];
    let mut approver = no_dialog_expected();
    let run = run_windowed(&root, bodies, &mut approver, &[small, large], 8_192);
    let report = run.result.expect("fallback can answer without a tool call");
    assert_eq!(report.final_text, "The answer is 43.");
    assert_eq!(report.requests.len(), 1);
    assert_all_clean(&run.records, 0);
    let request: serde_json::Value = serde_json::from_str(&report.requests[0]).unwrap();
    let tools = request["tools"].as_array().unwrap();
    assert_eq!(
        tools.len(),
        6,
        "base file tools plus the single catalog builtin"
    );
    assert!(tools
        .iter()
        .any(|tool| tool["function"]["name"] == "tools_find"));
    assert!(!tools
        .iter()
        .any(|tool| tool["function"]["name"] == "read_fixture"));
    assert!(!tools
        .iter()
        .any(|tool| tool["function"]["name"] == "large_fixture"));
}

#[test]
fn catalog_selection_of_base_fs_alias_is_not_appended_twice() {
    let root = tempfile::tempdir().unwrap();
    let fake = CatalogDispatchProbe::default();
    let roots = catalog_test_roots(&fake);
    let executors = Executors::mcp(fake.clone());
    let (small, large) = catalog_fixture_tools();
    std::fs::write(root.path().join("fixture.txt"), "fixture contents").unwrap();
    let mut registry = tool_registry(&[small.clone(), large.clone()]).unwrap();
    awili_tools::catalog::install(&mut registry).unwrap();
    let aliases = registry.aliases();
    let all_aliases: Vec<_> = aliases.values().map(String::as_str).collect();
    let wire = wire_names::WireNames::new(all_aliases);
    let fs_alias = aliases[&(
        awili_engine_authz::policy::BUILTIN_FS_ORIGIN.into(),
        "fs.read".into(),
    )]
        .clone();
    let fs_wire = wire.wire(&fs_alias);
    let catalog_alias = aliases[&("builtin:catalog".into(), "tools.find".into())].clone();
    let catalog_wire = wire.wire(&catalog_alias);
    let small_alias = aliases[&("mcp:fixture".into(), "read_fixture".into())].clone();
    let select_fs =
        serde_json::json!({"query":"","select":[fs_alias],"limit":10,"offset":0}).to_string();
    let select_mixed =
        serde_json::json!({"query":"","select":[fs_alias, small_alias],"limit":10,"offset":0})
            .to_string();
    let read_args = serde_json::json!({"path":root.path().join("fixture.txt")}).to_string();
    let bodies = vec![
        sse_tool_round(&[("select-fs", &catalog_wire, &select_fs)]),
        sse_tool_round(&[("select-mixed", &catalog_wire, &select_mixed)]),
        sse_tool_round(&[("read-fs", &fs_wire, &read_args)]),
        sse_text_final("read completed"),
    ];
    let composition = Composition {
        executors: &executors,
        install_authority: &awili_engine_authz::NoInstallAuthority,
        roots: &roots,
    };
    let mut approver = no_dialog_expected();
    let run = run_in(
        &root,
        bodies,
        &mut approver,
        &[small, large],
        &composition,
        8_192,
    );
    let report = run.result.expect("selecting a base tool is a valid no-op");
    assert_eq!(report.final_text, "read completed");
    assert_all_clean(&run.records, 3);
    let fs_only_request: serde_json::Value = serde_json::from_str(&report.requests[1]).unwrap();
    let fs_only_names: Vec<_> = fs_only_request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| {
            tool.pointer("/function/name")
                .and_then(serde_json::Value::as_str)
        })
        .collect();
    assert_eq!(
        fs_only_names
            .iter()
            .filter(|name| **name == fs_wire.as_str())
            .count(),
        1
    );
    assert!(fs_only_names.iter().any(|name| *name == catalog_wire));
    assert!(!fs_only_names.contains(&"read_fixture"));
    let mixed_request: serde_json::Value = serde_json::from_str(&report.requests[2]).unwrap();
    let mixed_names: Vec<_> = mixed_request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| {
            tool.pointer("/function/name")
                .and_then(serde_json::Value::as_str)
        })
        .collect();
    assert_eq!(
        mixed_names
            .iter()
            .filter(|name| **name == fs_wire.as_str())
            .count(),
        1
    );
    assert!(mixed_names.iter().any(|name| *name == small_alias));
    assert!(mixed_names.iter().any(|name| *name == catalog_wire));
    assert!(report.events.iter().any(|event| matches!(event, AgentEvent::ToolResultIngested(record) if record.content.contains("fixture contents"))));
}

#[test]
fn catalog_metadata_path_query_uses_readonly_c1_and_b2_without_io() {
    let root = tempfile::tempdir().unwrap();
    let fake = CatalogDispatchProbe::default();
    let roots = catalog_test_roots(&fake);
    let executors = Executors::mcp(fake.clone());
    let (small, large) = catalog_fixture_tools();
    let args = serde_json::json!({
        "query":"read /tmp/private/.env ../outside",
        "select":[],
        "limit":5,
        "offset":0
    })
    .to_string();
    let bodies = vec![
        sse_tool_round(&[("find-path", "tools_find", &args)]),
        sse_text_final("No matching tool metadata."),
    ];
    let composition = Composition {
        executors: &executors,
        install_authority: &awili_engine_authz::NoInstallAuthority,
        roots: &roots,
    };
    let mut approver = no_dialog_expected();
    let run = run_in(
        &root,
        bodies,
        &mut approver,
        &[small, large],
        &composition,
        8_192,
    );
    let report = run
        .result
        .expect("path-shaped metadata query is a normal read-only call");
    assert_eq!(report.final_text, "No matching tool metadata.");
    assert!(
        fake.calls.lock().unwrap().is_empty(),
        "catalog never invokes MCP"
    );
    assert_all_clean(&run.records, 1);
    assert_eq!(
        run.records
            .iter()
            .filter(|r| matches!(r, LogRecordV1::DispatchStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        run.records
            .iter()
            .filter(|r| matches!(r, LogRecordV1::DispatchFinalized { .. }))
            .count(),
        1
    );
    assert!(!run
        .records
        .iter()
        .any(|r| matches!(r, LogRecordV1::ApprovalRecorded { .. })));
}

#[test]
fn selected_mutating_catalog_tool_still_asks_and_denial_dispatches_nothing() {
    assert_catalog_mutation_denied(serde_json::json!({"select":["write_fixture"]}).to_string());
}

#[test]
fn discovered_mutating_catalog_tool_still_asks_and_denial_dispatches_nothing() {
    assert_catalog_mutation_denied(serde_json::json!({"query":"write_fixture"}).to_string());
}

fn assert_catalog_mutation_denied(catalog_args: String) {
    let make_writer = || {
        awili_tools::ToolDef::from_parts(
            awili_engine::ToolOrigin::new("mcp:fixture"),
            "write_fixture",
            awili_tools::CanonicalDescriptor {
                schema: serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
                side_effect: "mutating".into(),
                required_capabilities: vec![],
                executor: "mcp:fixture".into(),
            },
        )
        .with_description("Write the synthetic fixture.")
    };
    let root = tempfile::tempdir().unwrap();
    let fake = CatalogDispatchProbe::default();
    let roots = catalog_test_roots(&fake);
    let executors = Executors::mcp(fake.clone());
    let writer = make_writer();
    let (small, large) = catalog_fixture_tools();
    let extras = [small, large, writer];
    let call_args = "{}";
    let bodies = vec![
        sse_tool_round(&[("select-write", "tools_find", &catalog_args)]),
        sse_tool_round(&[("write-call", "write_fixture", call_args)]),
        sse_text_final("denied write did not run"),
    ];
    let composition = Composition {
        executors: &executors,
        install_authority: &awili_engine_authz::NoInstallAuthority,
        roots: &roots,
    };
    let mut deny = |_prompt: &str, risk: RiskClass| {
        assert_eq!(risk, RiskClass::Mutating);
        UserVerdict::Deny {
            reason: "fixture denial".into(),
        }
    };
    let run = run_in(&root, bodies, &mut deny, &extras, &composition, 8_192);
    let report = run
        .result
        .expect("denied selected mutation returns its refusal");
    assert_eq!(report.final_text, "denied write did not run");
    assert!(
        fake.calls.lock().unwrap().is_empty(),
        "denied MCP mutation never dispatches"
    );
    assert!(run
        .records
        .iter()
        .any(|record| matches!(record, LogRecordV1::AuthzDenied { .. })));
    assert_eq!(
        run.records
            .iter()
            .filter(|record| matches!(record, LogRecordV1::DispatchStarted { .. }))
            .count(),
        1,
        "only the catalog selection starts a dispatch"
    );
    assert_all_clean(&run.records, 1);
}

#[derive(Default)]
struct CaptureTransport {
    requests: Vec<String>,
    response: Option<String>,
}

impl Transport for CaptureTransport {
    fn stream(&mut self, request_body: &str) -> Result<String, SeamError> {
        self.requests.push(request_body.to_string());
        self.response
            .take()
            .ok_or_else(|| SeamError::Transport("capture transport exhausted".into()))
    }
}

#[test]
fn default_local_shell_can_be_searched_and_selected_again_without_duplicates() {
    let root = tempfile::tempdir().unwrap();
    let (small, large) = catalog_fixture_tools();
    let local = def_of("mcp:mac-shell", "run_command");
    let extras = [small, large, local];
    let bodies = vec![
        sse_tool_round(&[("find", "tools_find", r#"{"query":"run_command"}"#)]),
        sse_tool_round(&[("select", "tools_find", r#"{"select":["run_command"]}"#)]),
        sse_text_final("ready")];
    let mut approver = no_dialog_expected();
    let run = run_windowed(&root, bodies, &mut approver, &extras, 8_192);
    let report = run.result.expect("search and replacement selection keep one schema");
    assert_eq!(report.requests.len(), 3);
    for body in &report.requests {
        let request: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(request["tools"].as_array().unwrap().iter()
            .filter(|t| t["function"]["name"] == "run_command").count(), 1);
    }
    assert_all_clean(&run.records, 2);
}

#[test]
fn default_local_shell_still_requires_approval_and_deny_dispatches_nothing() {
    let root = tempfile::tempdir().unwrap();
    let fake = CatalogDispatchProbe::default();
    let roots = catalog_test_roots_for(&fake, "mcp:mac-shell");
    let executors = Executors::mcp(fake.clone());
    let shell = awili_tools::ToolDef::from_parts(
        awili_engine::ToolOrigin::new("mcp:mac-shell"), "run_command",
        awili_tools::CanonicalDescriptor {
            schema: serde_json::json!({"type":"object", "properties":{"command":{"type":"string"}}, "required":["command"]}),
            side_effect: "privileged".into(), required_capabilities: vec![],
            executor: "mcp:mac-shell".into(),
        });
    let (small, large) = catalog_fixture_tools();
    let extras = [small, large, shell];
    let bodies = vec![sse_tool_round(&[("shell", "run_command", r#"{"command":"printf denied"}"#)]),
        sse_text_final("owner declined; nothing ran")];
    let composition = Composition { executors: &executors,
        install_authority: &awili_engine_authz::NoInstallAuthority, roots: &roots };
    let mut asked = 0;
    let mut deny = |_prompt: &str, risk: RiskClass| {
        assert_eq!(risk, RiskClass::Privileged); asked += 1;
        UserVerdict::Deny { reason: "fixture denial".into() }
    };
    let run = run_in(&root, bodies, &mut deny, &extras, &composition, 8_192);
    let report = run.result.unwrap();
    assert_eq!(asked, 1);
    assert!(fake.calls.lock().unwrap().is_empty());
    let first: serde_json::Value = serde_json::from_str(&report.requests[0]).unwrap();
    assert!(first["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "run_command"));
    assert!(run.records.iter().any(|r| matches!(r, LogRecordV1::AuthzDenied { .. })));
    assert!(!run.records.iter().any(|r| matches!(r, LogRecordV1::DispatchStarted { .. })));
    assert_all_clean(&run.records, 0);
}

#[test]
fn warm_prompt_cache_uses_the_same_fallback_tool_projection_as_first_turn() {
    let root = tempfile::tempdir().unwrap();
    let (small, large) = catalog_fixture_tools();
    let local = def_of("mcp:mac-shell", "run_command");
    let extras = [small, large, local];
    let mut warm_transport = CaptureTransport {
        requests: Vec::new(),
        response: Some(sse_text_final("ok")),
    };
    let mut host_io = silent_host();
    assert!(warm_prompt_cache(
        &mut warm_transport,
        "test-model",
        &extras,
        8_192,
        &ContextHistory::new(),
        &mut host_io,
    )
    .unwrap());
    let warm: serde_json::Value = serde_json::from_str(&warm_transport.requests[0]).unwrap();

    let mut approver = no_dialog_expected();
    let regular = run_windowed(
        &root,
        vec![sse_text_final("ok")],
        &mut approver,
        &extras,
        8_192,
    );
    let report = regular.result.unwrap();
    let first: serde_json::Value = serde_json::from_str(&report.requests[0]).unwrap();
    assert_eq!(warm["tools"], first["tools"]);
}

#[test]
fn inactive_tool_in_mixed_fallback_batch_prevents_fs_write_and_approval() {
    let root = tempfile::tempdir().unwrap();
    let (small, large) = catalog_fixture_tools();
    let write_args =
        serde_json::json!({"path":"must-not-exist.txt","content":"changed"}).to_string();
    let bodies = vec![
        sse_tool_round(&[
            ("inactive-1", "large_fixture", "{}"),
            ("write-2", "fs_write", &write_args),
        ]),
        sse_text_final("no tools ran"),
    ];
    let mut approver = no_dialog_expected();
    let run = run_windowed(&root, bodies, &mut approver, &[small, large], 8_192);
    let report = run
        .result
        .expect("inactive mixed batch returns tool errors");
    assert_eq!(report.final_text, "no tools ran");
    assert!(!root.path().join("must-not-exist.txt").exists());
    let rejected: Vec<_> = report
        .events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolResultIngested(record) => Some(record),
            _ => None,
        })
        .collect();
    assert_eq!(rejected.len(), 2);
    assert!(rejected.iter().all(|record| {
        record.is_error
            && record.content.contains("no batch member ran")
            && record.content.contains("select containing exact returned aliases")
    }));
    assert_all_clean(&run.records, 0);
}

// -- S6: a Revoke takes effect at the point of use ----------------------------

/// Calls `hook(n)` as the n-th request goes out (0-based), then serves the
/// next scripted body.
struct HookScript<'a> {
    bodies: VecDeque<String>,
    n: usize,
    hook: Box<dyn FnMut(usize) + 'a>,
}

impl Transport for HookScript<'_> {
    fn stream(&mut self, _request_body: &str) -> Result<String, SeamError> {
        (self.hook)(self.n);
        self.n += 1;
        self.bodies
            .pop_front()
            .ok_or_else(|| SeamError::Transport("script exhausted".into()))
    }
}

/// A folder grant exists before the turn. The owner revokes it between two
/// rounds of ONE turn: the round-1 write is covered (no dialog); the
/// round-2 write, same path, asks again.
#[test]
fn s6_a_revoke_between_rounds_stops_the_next_matching_action() {
    use awili_engine_log::grant_service::{GrantStoreService, ServiceHandle};
    let root = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let svc = GrantStoreService::init(dir.path().join("grants.log"), None).unwrap();
    let mut store = ServiceHandle(svc);
    let host = host().lock().unwrap();
    let write = r#"{"path":"n.txt","contents":"x"}"#;
    let run_one = |bodies: Vec<String>,
                   hook: Box<dyn FnMut(usize) + '_>,
                   store: &mut ServiceHandle,
                   verdict: &str,
                   turn: &str,
                   asks: &mut usize| {
        let mut log = ExecutionLog::open(MemLog::default(), format!("sess-s6-{turn}")).unwrap();
        let mut transport = HookScript {
            bodies: bodies.into(),
            n: 0,
            hook,
        };
        let mut history = ContextHistory::new();
        let executors = Executors::builtin_only();
        let roots = RootsAcceptance::default();
        let mut approver = |_p: &str, _r: RiskClass| {
            *asks += 1;
            UserVerdict::Approve {
                scope: verdict.into(),
            }
        };
        run_turn(
            &host,
            &mut transport,
            "test-model",
            root.path().to_str().unwrap(),
            "write",
            &mut approver,
            &mut log,
            store,
            &[],
            &executors,
            &awili_engine_authz::NoInstallAuthority,
            &roots,
            1_000_000,
            turn,
            &mut history,
        )
        .expect("turn completes");
    };
    // Turn A: approve the write for this folder.
    let mut asks_a = 0;
    run_one(
        vec![
            sse_tool_round(&[("a1", "fs.write", write)]),
            sse_text_final("ok"),
        ],
        Box::new(|_| {}),
        &mut store,
        "folder",
        "turn-s6-a",
        &mut asks_a,
    );
    assert_eq!(asks_a, 1);
    assert_eq!(svc.list_remembered().unwrap().len(), 1);
    // Turn B: write in round 1, revoke, write again in round 2.
    let mut asks_b = 0;
    run_one(
        vec![
            sse_tool_round(&[("b1", "fs.write", write)]),
            sse_tool_round(&[("b2", "fs.write", write)]),
            sse_text_final("ok"),
        ],
        Box::new(|n| {
            if n == 1 {
                let g = svc.list_remembered().unwrap();
                assert!(svc.revoke_grant(&g[0]).unwrap());
            }
        }),
        &mut store,
        "once",
        "turn-s6-b",
        &mut asks_b,
    );
    assert_eq!(
        asks_b, 1,
        "round 1 rode the grant; after Revoke, round 2 asked again"
    );
}

#[test]
fn reasoning_only_output_limit_is_a_recoverable_failure() {
    let root = tempfile::tempdir().unwrap();
    let raw = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":{\"prompt_tokens\":9468,\"completion_tokens\":2048}}\n\n",
        "data: [DONE]\n\n",
    );
    let r = run(&root, vec![raw.into()], &mut no_dialog_expected());
    assert_eq!(r.records.iter().filter(|r| matches!(r, LogRecordV1::TurnClosed { outcome, .. } if outcome == "failed")).count(), 1);
    match r.result {
        Err(SeamError::TurnFailed { reason, report }) => {
            assert!(reason.contains("output-token limit"));
            assert!(reason.contains("reasoning budget"));
            assert!(report.final_text.is_empty());
            assert_eq!(report.requests.len(), 1, "a budget failure must not automatically retry");
            assert!(report.events.iter().any(|e| matches!(e, AgentEvent::UsageReported(u) if u.completion_tokens == 2048)));
        }
        other => panic!("expected recoverable output-limit failure, got {other:?}"),
    }
}

#[test]
fn visible_answer_at_output_limit_is_preserved() {
    let root = tempfile::tempdir().unwrap();
    let raw = sse_text_final("A visible answer").replace("\"stop\"", "\"length\"");
    let r = run(&root, vec![raw], &mut no_dialog_expected());
    assert_eq!(r.result.unwrap().final_text, "A visible answer");
}

struct BudgetScript {
    script: Script,
    budget: u64,
    sent: Arc<std::sync::atomic::AtomicUsize>,
}
impl Transport for BudgetScript {
    fn max_output_tokens(&self) -> u64 { self.budget }
    fn stream(&mut self, body: &str) -> Result<String, SeamError> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        self.script.stream(body)
    }
}

fn run_budgeted(root: &tempfile::TempDir, budget: u64, window: u64, bodies: Vec<String>) -> (Result<TurnReport, SeamError>, usize) {
    let host = host().lock().unwrap();
    let mut log = ExecutionLog::open(MemLog::default(), "budget-session").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut transport = BudgetScript { script: Script { bodies: bodies.into() }, budget, sent: sent.clone() };
    let result = run_turn(&host, &mut transport, "test-model", root.path().to_str().unwrap(), "read the note", &mut no_dialog_expected(), &mut log, &mut grants, &[], &Executors::builtin_only(), &awili_engine_authz::NoInstallAuthority, &RootsAcceptance::default(), window, "budget-turn", &mut ContextHistory::new());
    (result, sent.load(Ordering::SeqCst))
}

#[test]
fn configured_output_budget_reaches_initial_and_tool_continuation_requests() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("note.txt"), "read only fixture").unwrap();
    let (result, sent) = run_budgeted(&root, 16384, 131072, vec![sse_tool_round(&[("read", "fs.read", r#"{"path":"note.txt"}"#)]), sse_text_final("done")]);
    let report = result.unwrap();
    assert_eq!(sent, 2);
    assert_eq!(report.final_text, "done");
    for body in report.requests { assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["max_tokens"], 16384); }
}

#[test]
fn configured_output_reservation_prevents_an_overfull_request_from_being_sent() {
    let root = tempfile::tempdir().unwrap();
    let (result, sent) = run_budgeted(&root, 9487, 10000, vec![sse_text_final("must not be requested")]);
    assert!(matches!(result, Err(SeamError::Context(_))));
    assert_eq!(sent, 0);
    assert_eq!(input_budget_tokens(projection_window_tokens(10000, 9487)), 1);
}

#[test]
fn bounded_stream_observes_cancel_between_unterminated_chunks() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Reader<'a>(&'a AtomicBool);
    impl std::io::Read for Reader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.0.store(true, Ordering::SeqCst);
            buf[0] = b'x';
            Ok(1)
        }
    }
    let cancelled = AtomicBool::new(false);
    let result = super::bounded_stream_body(Reader(&cancelled), true, &mut |_| true, || cancelled.load(Ordering::SeqCst));
    assert!(matches!(result, Err(super::SeamError::Cancelled(super::StopCause::Cancelled))));
}
