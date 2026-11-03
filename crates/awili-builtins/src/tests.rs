//! C2-u4 acceptance gate (design finding 7): table-driven refusal matrix
//! plus the happy path, each run through the REAL B2 seam — evidence →
//! intent → ticket → dispatch — asserting the plan's acceptance sentence:
//! an escape refused by C3 lands as `dispatch_finalized(ok=false)`, with
//! exactly one started, exactly one finalized, `is_error=true` on the
//! record, replay Clean (zero AuthzInvalid, zero Uncertain), no ok=true
//! record for the intent, and nothing outside the root touched.

use super::*;
use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};
use awili_engine_log::replay::{classify, IntentStatus};
use awili_engine_log::schema::LogRecordV1;
use awili_engine_log::{test_bundle, AuthzGate, MemLog, ResolvedTool, RiskClass};

/// The process's gate, straight from the shared fixture (unit 4 r7): the
/// fixture's own `OnceLock` owns the sole issuance, so this crate's former
/// process-global `GATE` cache — and the second-issuence failure it had to
/// tolerate — are gone.
fn gate() -> &'static AuthzGate {
    &test_bundle().0
}

/// The resolved token these tests' evidence binds (unit 4 item 3): minted
/// by the fixture's registry authority — the same authority-minted source
/// production code must use; the digest content is irrelevant to the C3
/// flows under test.
fn resolved(name: &str) -> ResolvedTool {
    test_bundle().2.mint(ToolIdentity::new(
        ToolOrigin::new("builtin:fs"),
        name,
        DescriptorDigest([0u8; 32]),
    ))
}

fn call(id: &str, name: &str, args: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId(id.into()),
        provider_call_id: None,
        name: name.into(),
        arguments: args.into(),
    }
}

struct SeamResult {
    record: awili_engine::events::ToolResultRecord,
    records: Vec<LogRecordV1>,
}

/// Drive one call through the full seam against a sandbox rooted at `root`.
fn seam(root: &tempfile::TempDir, risk: RiskClass, c: ToolCall, limits: Limits) -> SeamResult {
    seam_scoped(Some(root.path().to_str().unwrap()), risk, c, limits)
}

fn seam_scoped(
    scope_root: Option<&str>,
    risk: RiskClass,
    c: ToolCall,
    limits: Limits,
) -> SeamResult {
    seam_secrets(scope_root, risk, c, limits, SecretFiles::NONE)
}

/// RC2-R2G1: [`seam_scoped`] with a secret-file test and the files the
/// owner allowed.
fn seam_secrets(
    scope_root: Option<&str>,
    risk: RiskClass,
    c: ToolCall,
    limits: Limits,
    secrets: SecretFiles<'_>,
) -> SeamResult {
    let turn = "turn-u4";
    let mut log = ExecutionLog::open(MemLog::default(), "sess-u4").unwrap();
    log.turn_opened(turn, "test", "test-model").unwrap();
    let evidence = gate()
        .policy_evidence(&resolved(&c.name), turn, &c, risk, scope_root)
        .unwrap();
    let record = match run_builtin_fs(&mut log, turn, &c, evidence, limits, secrets).unwrap() {
        BuiltinRunOutcome::Completed(r) => r,
        other => panic!("expected Completed, got {other:?}"),
    };
    let (records, _) = log.read_records().unwrap();
    SeamResult { record, records }
}

/// The acceptance sentence, asserted in full for a refusal.
fn assert_failed_dispatch(r: &SeamResult) {
    assert!(r.record.is_error, "refusal must surface is_error=true");
    assert_seam_shape(r, false);
}

fn assert_ok_dispatch(r: &SeamResult) {
    assert!(
        !r.record.is_error,
        "expected success, got: {}",
        r.record.content
    );
    assert_seam_shape(r, true);
}

/// Exactly one started, exactly one finalized with the expected ok, no
/// contrary finalization, replay Clean with zero AuthzInvalid/Uncertain.
fn assert_seam_shape(r: &SeamResult, expect_ok: bool) {
    let started = r
        .records
        .iter()
        .filter(|rec| matches!(rec, LogRecordV1::DispatchStarted { .. }))
        .count();
    let finals: Vec<bool> = r
        .records
        .iter()
        .filter_map(|rec| match rec {
            LogRecordV1::DispatchFinalized { ok, .. } => Some(*ok),
            _ => None,
        })
        .collect();
    assert_eq!(started, 1, "exactly one dispatch_started");
    assert_eq!(finals.len(), 1, "exactly one dispatch_finalized");
    assert_eq!(finals[0], expect_ok, "dispatch_finalized ok flag");

    let turns = classify(&r.records);
    let statuses: Vec<&IntentStatus> = turns
        .iter()
        .flat_map(|t| t.intents.iter().map(|i| &i.status))
        .collect();
    assert!(!statuses.is_empty(), "replay must see the intent");
    for s in statuses {
        match s {
            IntentStatus::Clean { ok } => assert_eq!(*ok, expect_ok, "replay Clean flag"),
            other => panic!("replay must classify Clean, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Refusal matrix
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn refusal_matrix_lands_as_failed_dispatch() {
    // Outside world: a secret file the sandbox must never reach, and a
    // sibling dir the escapes aim at.
    let outside = tempfile::tempdir().unwrap();
    let secret_path = outside.path().join("secret.txt");
    std::fs::write(&secret_path, b"outside-secret").unwrap();

    // RC2-SVC8 (F0330): the root sits INSIDE `outside`, so the relative
    // escapes (`../secret.txt`, `../pwned.txt`, `..` for grep) aim at the
    // very secret and directory the assertions below check — not at a
    // sibling of an unrelated temp dir.
    let root_dir = outside.path().join("root");
    std::fs::create_dir(&root_dir).unwrap();
    std::fs::write(root_dir.join("inside.txt"), b"inside").unwrap();
    assert_eq!(
        root_dir
            .join("..")
            .join("secret.txt")
            .canonicalize()
            .unwrap(),
        secret_path.canonicalize().unwrap(),
        "`../secret.txt` from the root must be the real secret"
    );

    // Symlink LEAF pointing outside, planted inside the root.
    std::os::unix::fs::symlink(&secret_path, root_dir.join("leaf-link")).unwrap();
    // Symlink PARENT: a dir symlink pointing outside.
    std::os::unix::fs::symlink(outside.path(), root_dir.join("dir-link")).unwrap();

    let cases: Vec<(&str, &str, String)> = vec![
        ("traversal", "fs.read", r#"{"path":"../secret.txt"}"#.into()),
        (
            "absolute",
            "fs.read",
            format!(r#"{{"path":"{}"}}"#, secret_path.display()),
        ),
        ("symlink-leaf", "fs.read", r#"{"path":"leaf-link"}"#.into()),
        (
            "symlink-parent",
            "fs.read",
            r#"{"path":"dir-link/secret.txt"}"#.into(),
        ),
        (
            "traversal-write",
            "fs.write",
            r#"{"path":"../pwned.txt","contents":"x"}"#.into(),
        ),
        (
            "traversal-edit",
            "fs.edit",
            r#"{"path":"../secret.txt","find":"outside","replace":"x"}"#.into(),
        ),
        (
            "traversal-glob",
            "fs.glob",
            r#"{"pattern":"../*.txt"}"#.into(),
        ),
        (
            "traversal-grep",
            "fs.grep",
            r#"{"needle":"secret","under":".."}"#.into(),
        ),
        (
            "schema-invalid-type",
            "fs.write",
            r#"{"path":3,"contents":"c"}"#.into(),
        ),
        (
            "schema-invalid-extra",
            "fs.read",
            r#"{"path":"inside.txt","mode":"raw"}"#.into(),
        ),
        (
            "schema-invalid-missing",
            "fs.edit",
            r#"{"path":"inside.txt","find":"a"}"#.into(),
        ),
        (
            "unknown-tool",
            "fs.chmod",
            r#"{"path":"inside.txt"}"#.into(),
        ),
    ];

    for (label, name, args) in cases {
        let c = call(&format!("c-{label}"), name, &args);
        let r = seam_scoped(
            Some(root_dir.to_str().unwrap()),
            RiskClass::Mutating,
            c,
            Limits::default(),
        );
        assert_failed_dispatch(&r);
        // Nothing escaped: the secret is intact and unrevealed, no file
        // appeared outside the root.
        assert_eq!(
            std::fs::read(&secret_path).unwrap(),
            b"outside-secret",
            "{label}"
        );
        assert!(
            !r.record.content.contains("outside-secret"),
            "{label}: leaked content"
        );
        assert!(!outside.path().join("pwned.txt").exists(), "{label}");
    }
}

#[test]
fn missing_scope_root_is_a_failed_dispatch() {
    let c = call("c-noscope", "fs.read", r#"{"path":"a.txt"}"#);
    let r = seam_scoped(None, RiskClass::ReadOnly, c, Limits::default());
    assert_failed_dispatch(&r);
}

#[test]
fn identical_twin_calls_each_finalize_their_own_intent() {
    // Round-2 P1: two DISTINCT calls with identical name and arguments
    // (different awili_call_id) — the case a fingerprint gate cannot
    // distinguish. The structural seam gives each its own intent, its
    // own dispatch pair, and a record bound to ITS call id.
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), b"twin").unwrap();
    let turn = "turn-u4";
    let mut log = ExecutionLog::open(MemLog::default(), "sess-u4-twins").unwrap();
    log.turn_opened(turn, "test", "test-model").unwrap();

    for id in ["c-twin-1", "c-twin-2"] {
        let c = call(id, "fs.read", r#"{"path":"a.txt"}"#);
        let ev = gate()
            .policy_evidence(
                &resolved("fs.read"),
                turn,
                &c,
                RiskClass::ReadOnly,
                Some(root.path().to_str().unwrap()),
            )
            .unwrap();
        let rec = match run_builtin_fs(&mut log, turn, &c, ev, Limits::default(), SecretFiles::NONE)
            .unwrap()
        {
            BuiltinRunOutcome::Completed(r) => r,
            other => panic!("{other:?}"),
        };
        assert!(!rec.is_error);
        assert_eq!(rec.awili_call_id.0, id, "record bound to ITS call");
    }
    let (records, _) = log.read_records().unwrap();
    let started = records
        .iter()
        .filter(|r| matches!(r, LogRecordV1::DispatchStarted { .. }))
        .count();
    let finalized = records
        .iter()
        .filter(|r| matches!(r, LogRecordV1::DispatchFinalized { ok: true, .. }))
        .count();
    assert_eq!((started, finalized), (2, 2), "one dispatch pair per twin");
    let turns = classify(&records);
    let clean = turns
        .iter()
        .flat_map(|t| t.intents.iter())
        .filter(|i| matches!(i.status, IntentStatus::Clean { ok: true }))
        .count();
    assert_eq!(clean, 2, "both intents replay Clean independently");
}

#[test]
fn evidence_for_another_call_is_refused_before_anything_is_written() {
    // Evidence minted for c1, presented with c2 through the PUBLIC seam:
    // B2's intent() is the write-time rejection point (EvidenceMismatch)
    // — the run errors with NOTHING recorded, so there is no intent to
    // finalize wrongly. prepare's own carried refusal (the authorizes
    // check inside sandbox_for_evidence) remains as defense in depth.
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), b"a").unwrap();
    let turn = "turn-u4";
    let c1 = call("c-legit", "fs.read", r#"{"path":"a.txt"}"#);
    let c2 = call("c-other", "fs.read", r#"{"path":"a.txt"}"#);

    let mut log = ExecutionLog::open(MemLog::default(), "sess-u4-mismatch").unwrap();
    log.turn_opened(turn, "test", "test-model").unwrap();

    // Defense in depth, observable at the prepare layer:
    let ev_probe = gate()
        .policy_evidence(
            &resolved("fs.read"),
            turn,
            &c1,
            RiskClass::ReadOnly,
            Some(root.path().to_str().unwrap()),
        )
        .unwrap();
    let prepared = PreparedBuiltinFs::prepare(&ev_probe, turn, &c2);
    assert!(prepared.is_refused());
    drop(prepared);
    drop(ev_probe);

    // The public seam refuses at intent(): Err, nothing recorded.
    let ev1 = gate()
        .policy_evidence(
            &resolved("fs.read"),
            turn,
            &c1,
            RiskClass::ReadOnly,
            Some(root.path().to_str().unwrap()),
        )
        .unwrap();
    assert!(run_builtin_fs(
        &mut log,
        turn,
        &c2,
        ev1,
        Limits::default(),
        SecretFiles::NONE
    )
    .is_err());
    let (records, _) = log.read_records().unwrap();
    assert!(!records.iter().any(|r| matches!(
        r,
        LogRecordV1::Intent { .. }
            | LogRecordV1::DispatchStarted { .. }
            | LogRecordV1::DispatchFinalized { .. }
    )));

    // And the RIGHT pairing still works afterwards.
    let ev1b = gate()
        .policy_evidence(
            &resolved("fs.read"),
            turn,
            &c1,
            RiskClass::ReadOnly,
            Some(root.path().to_str().unwrap()),
        )
        .unwrap();
    match run_builtin_fs(
        &mut log,
        turn,
        &c1,
        ev1b,
        Limits::default(),
        SecretFiles::NONE,
    )
    .unwrap()
    {
        BuiltinRunOutcome::Completed(r) => assert!(!r.is_error),
        other => panic!("{other:?}"),
    }
}

#[test]
fn over_limit_read_refuses_whole_never_truncates() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("big.txt"), vec![b'x'; 4096]).unwrap();
    let limits = Limits {
        max_file_bytes: 1024,
        ..Limits::default()
    };
    let c = call("c-big", "fs.read", r#"{"path":"big.txt"}"#);
    let r = seam(&root, RiskClass::ReadOnly, c, limits);
    assert_failed_dispatch(&r);
    assert!(!r.record.content.contains("xxxx"), "no partial content");
}

#[cfg(unix)]
#[test]
fn grep_tree_containment_refusal_fails_the_dispatch() {
    // A symlink pointing OUTSIDE planted in the grepped tree: plain grep
    // would report it as a skip inside an ok=true result — the bounded
    // builtin path must fail the whole dispatch (design finding 5).
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("s.txt"), b"needle here").unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("ok.txt"), b"needle inside").unwrap();
    std::os::unix::fs::symlink(outside.path().join("s.txt"), root.path().join("link.txt")).unwrap();

    let c = call("c-grepsym", "fs.grep", r#"{"needle":"needle","under":""}"#);
    let r = seam(&root, RiskClass::ReadOnly, c, Limits::default());
    assert_failed_dispatch(&r);
}

#[test]
fn glob_entry_budget_refuses_whole() {
    let root = tempfile::tempdir().unwrap();
    for i in 0..20 {
        std::fs::write(root.path().join(format!("f{i}.txt")), b"x").unwrap();
    }
    let limits = Limits {
        max_entries: 10,
        ..Limits::default()
    };
    let c = call("c-globbudget", "fs.glob", r#"{"pattern":"*.txt"}"#);
    let r = seam(&root, RiskClass::ReadOnly, c, limits);
    assert_failed_dispatch(&r);
}

#[test]
fn grep_entry_budget_refuses_whole() {
    let root = tempfile::tempdir().unwrap();
    for i in 0..20 {
        std::fs::write(root.path().join(format!("f{i}.txt")), b"needle").unwrap();
    }
    let limits = Limits {
        max_entries: 10,
        ..Limits::default()
    };
    let c = call(
        "c-grepbudget",
        "fs.grep",
        r#"{"needle":"needle","under":""}"#,
    );
    let r = seam(&root, RiskClass::ReadOnly, c, limits);
    assert_failed_dispatch(&r);
}

#[test]
fn grep_match_cap_refuses_whole() {
    let root = tempfile::tempdir().unwrap();
    let many = "needle
"
    .repeat(50);
    std::fs::write(root.path().join("many.txt"), many).unwrap();
    let limits = Limits {
        max_matches: 10,
        ..Limits::default()
    };
    let c = call("c-grepcap", "fs.grep", r#"{"needle":"needle","under":""}"#);
    let r = seam(&root, RiskClass::ReadOnly, c, limits);
    assert_failed_dispatch(&r);
}

#[test]
fn grep_result_byte_cap_refuses_whole() {
    let root = tempfile::tempdir().unwrap();
    let long_line = format!(
        "needle {}
",
        "x".repeat(500)
    );
    std::fs::write(root.path().join("long.txt"), long_line.repeat(5)).unwrap();
    let limits = Limits {
        max_result_bytes: 256,
        ..Limits::default()
    };
    let c = call(
        "c-grepbytes",
        "fs.grep",
        r#"{"needle":"needle","under":""}"#,
    );
    let r = seam(&root, RiskClass::ReadOnly, c, limits);
    assert_failed_dispatch(&r);
}

#[cfg(unix)]
#[test]
fn grep_skip_records_are_charged_against_the_byte_cap() {
    // Round-2 P1: unreadable files produce (rel, reason) skip records —
    // those bytes count against max_result_bytes too, refusing whole
    // rather than allocating past the cap.
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let mut locked = Vec::new();
    for i in 0..8 {
        let name = format!("very-long-locked-file-name-{i:04}.txt");
        let path = root.path().join(&name);
        std::fs::write(&path, b"needle hidden").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        locked.push(path);
    }
    // Root can read anything; skip the scenario in that case.
    if std::fs::read(&locked[0]).is_ok() {
        return;
    }
    let limits = Limits {
        max_result_bytes: 64,
        ..Limits::default()
    };
    let c = call(
        "c-skipbytes",
        "fs.grep",
        r#"{"needle":"needle","under":""}"#,
    );
    let r = seam(&root, RiskClass::ReadOnly, c, limits);
    assert_failed_dispatch(&r);
}

#[test]
fn edit_amplification_refuses_whole_without_writing() {
    // Bounded input, large replacement: the result must be refused
    // DURING construction, and the file must be untouched.
    let root = tempfile::tempdir().unwrap();
    let original = "a".repeat(512);
    std::fs::write(root.path().join("amp.txt"), &original).unwrap();
    let limits = Limits {
        max_file_bytes: 1024,
        ..Limits::default()
    };
    let big = "B".repeat(64);
    let args = format!(r#"{{"path":"amp.txt","find":"a","replace":"{big}"}}"#);
    let c = call("c-amp", "fs.edit", &args);
    let r = seam(&root, RiskClass::Mutating, c, limits);
    assert_failed_dispatch(&r);
    assert_eq!(
        std::fs::read_to_string(root.path().join("amp.txt")).unwrap(),
        original,
        "refused edit must leave the file untouched"
    );
}

#[test]
fn glob_result_byte_cap_refuses_whole() {
    let root = tempfile::tempdir().unwrap();
    for i in 0..10 {
        std::fs::write(root.path().join(format!("long-name-{i:04}.txt")), b"x").unwrap();
    }
    let limits = Limits {
        max_result_bytes: 32,
        ..Limits::default()
    };
    let c = call("c-globbytes", "fs.glob", r#"{"pattern":"*.txt"}"#);
    let r = seam(&root, RiskClass::ReadOnly, c, limits);
    assert_failed_dispatch(&r);
}

#[test]
fn non_utf8_read_is_a_failed_dispatch() {
    // STRICT UTF-8 (round-1 P1): lossy conversion would hand the model
    // content that differs from the file while claiming success.
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("bin.dat"), [0xff, 0xfe, 0x00, 0x41]).unwrap();
    let c = call("c-bin", "fs.read", r#"{"path":"bin.dat"}"#);
    let r = seam(&root, RiskClass::ReadOnly, c, Limits::default());
    assert_failed_dispatch(&r);
    assert!(r.record.content.contains("not valid UTF-8"));
}

#[cfg(unix)]
#[test]
fn grep_ordinary_skip_stays_ok_and_reported() {
    // An unreadable file (permissions) is a tree condition, not a
    // containment refusal: the search succeeds and REPORTS the skip.
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("ok.txt"), b"needle inside").unwrap();
    let locked = root.path().join("locked.txt");
    std::fs::write(&locked, b"needle hidden").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    // Root can read anything; skip the scenario in that case.
    if std::fs::read(&locked).is_ok() {
        return;
    }
    let c = call("c-grepskip", "fs.grep", r#"{"needle":"needle","under":""}"#);
    let r = seam(&root, RiskClass::ReadOnly, c, Limits::default());
    assert_ok_dispatch(&r);
    let v: serde_json::Value = serde_json::from_str(&r.record.content).unwrap();
    assert_eq!(v["matches"][0]["rel"], "ok.txt");
    assert_eq!(v["skipped"][0][0], "locked.txt");
}

// ---------------------------------------------------------------------------
// Malformed JSON is pinned at the INTENT layer (design finding 7): it
// never becomes a ticket, so it can never reach dispatch at all.
// ---------------------------------------------------------------------------

#[test]
fn malformed_json_never_reaches_dispatch() {
    // Pinned even EARLIER than expected: C1 refuses to mint evidence for
    // malformed arguments at all (normalized-call hashing requires valid
    // JSON), so no evidence, no intent, no ticket, no dispatch. And B2's
    // own intent path keeps its Malformed → NotDispatchable behavior
    // (covered by its kill-point suite). The builtin parse layer is total
    // on malformed input as defense in depth (awili-tools parse tests).
    let c = call("c-malformed", "fs.read", "{nope");
    assert!(gate()
        .policy_evidence(
            &resolved("fs.read"),
            "turn-u4",
            &c,
            RiskClass::ReadOnly,
            None
        )
        .is_err());
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[test]
fn happy_path_full_seam_write_edit_read_glob_grep() {
    let root = tempfile::tempdir().unwrap();

    let w = seam(
        &root,
        RiskClass::Mutating,
        call(
            "c-w",
            "fs.write",
            r#"{"path":"notes.md","contents":"hello awili"}"#,
        ),
        Limits::default(),
    );
    assert_ok_dispatch(&w);

    let e = seam(
        &root,
        RiskClass::Mutating,
        call(
            "c-e",
            "fs.edit",
            r#"{"path":"notes.md","find":"awili","replace":"world"}"#,
        ),
        Limits::default(),
    );
    assert_ok_dispatch(&e);
    assert!(e.record.content.contains("1 replacement"));

    let r = seam(
        &root,
        RiskClass::ReadOnly,
        call("c-r", "fs.read", r#"{"path":"notes.md"}"#),
        Limits::default(),
    );
    assert_ok_dispatch(&r);
    assert_eq!(r.record.content, "hello world");

    let g = seam(
        &root,
        RiskClass::ReadOnly,
        call("c-g", "fs.glob", r#"{"pattern":"*.md"}"#),
        Limits::default(),
    );
    assert_ok_dispatch(&g);
    let v: serde_json::Value = serde_json::from_str(&g.record.content).unwrap();
    assert_eq!(v["paths"], serde_json::json!(["notes.md"]));

    let gr = seam(
        &root,
        RiskClass::ReadOnly,
        call("c-gr", "fs.grep", r#"{"needle":"world","under":""}"#),
        Limits::default(),
    );
    assert_ok_dispatch(&gr);
    let v: serde_json::Value = serde_json::from_str(&gr.record.content).unwrap();
    assert_eq!(v["matches"][0]["rel"], "notes.md");
    assert_eq!(v["skipped"], serde_json::json!([]));
}

#[test]
fn edit_stays_inside_and_reports_zero_on_no_match() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), b"abc").unwrap();
    let r = seam(
        &root,
        RiskClass::Mutating,
        call(
            "c-e0",
            "fs.edit",
            r#"{"path":"a.txt","find":"zzz","replace":"y"}"#,
        ),
        Limits::default(),
    );
    assert_ok_dispatch(&r);
    assert!(r.record.content.contains("0 replacement"));
    assert_eq!(std::fs::read(root.path().join("a.txt")).unwrap(), b"abc");
}

/// RC2-R2G1 (RC2-R2F review P1): a search of an ordinary folder found a
/// secret file beneath it and showed it: `fs.grep` its text, `fs.glob` its
/// name. A secret file the owner did not allow for the call is left out
/// (said, not named, never read); one he allowed is shown.
/// `secret_files_reached` names what a search would show before it runs.
#[test]
fn rc2r2g1_a_search_shows_a_secret_file_only_when_allowed() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("app")).unwrap();
    std::fs::write(root.path().join("app/notes.txt"), b"key = plain\n").unwrap();
    std::fs::write(root.path().join("app/.env"), b"key = hunter2\n").unwrap();
    fn dot_env(p: &str) -> bool {
        p.rsplit('/').next() == Some(".env")
    }
    let dir = root.path().to_str().unwrap();
    let scope = Some(dir);
    let glob = || call("c-g", "fs.glob", r#"{"pattern":"app/*"}"#);
    let grep = || call("c-gr", "fs.grep", r#"{"needle":"key","under":"app"}"#);
    // RC2-R2K: named by full path (the sandbox's own spelling of the root).
    let reached = secret_files_reached(dir, &glob(), dot_env);
    assert_eq!(reached.len(), 1, "{reached:?}");
    assert!(
        reached[0].starts_with('/') && reached[0].ends_with("/app/.env"),
        "{reached:?}"
    );
    assert_eq!(secret_files_reached(dir, &grep(), dot_env), reached);
    let none = SecretFiles {
        is_secret: dot_env,
        allowed: &[],
    };
    let g = seam_secrets(scope, RiskClass::ReadOnly, glob(), Limits::default(), none);
    assert_ok_dispatch(&g);
    let v: serde_json::Value = serde_json::from_str(&g.record.content).unwrap();
    assert_eq!(v["paths"], serde_json::json!(["app/notes.txt"]));
    assert!(v["left_out"].as_str().unwrap().starts_with("1 secret file"));
    assert!(!g.record.content.contains(".env"), "{}", g.record.content);
    let gr = seam_secrets(scope, RiskClass::ReadOnly, grep(), Limits::default(), none);
    assert_ok_dispatch(&gr);
    assert!(
        !gr.record.content.contains("hunter2"),
        "{}",
        gr.record.content
    );
    assert!(!gr.record.content.contains(".env"), "{}", gr.record.content);
    let v: serde_json::Value = serde_json::from_str(&gr.record.content).unwrap();
    assert_eq!(v["matches"].as_array().unwrap().len(), 1);
    assert!(v["left_out"].as_str().unwrap().starts_with("1 secret file"));

    let allowed = reached.clone();
    let ok = SecretFiles {
        is_secret: dot_env,
        allowed: &allowed,
    };
    let g = seam_secrets(scope, RiskClass::ReadOnly, glob(), Limits::default(), ok);
    let v: serde_json::Value = serde_json::from_str(&g.record.content).unwrap();
    assert_eq!(v["paths"], serde_json::json!(["app/.env", "app/notes.txt"]));
    assert!(v.get("left_out").is_none());
    let gr = seam_secrets(scope, RiskClass::ReadOnly, grep(), Limits::default(), ok);
    assert!(
        gr.record.content.contains("hunter2"),
        "{}",
        gr.record.content
    );
}

/// RC2-R2K (RC2-R2F review P1 on 55d863a3: with the Work folder a secret
/// folder, a relative search saw `config`, not `<folder>/.ssh/config`, and
/// showed it with no card). Files are judged by their full paths: a search
/// run from within `.ssh` reaches its files as secret, shows none of them
/// unless allowed, and shows them once allowed.
#[test]
fn rc2r2k_a_search_from_within_a_secret_folder_is_caught() {
    let home = tempfile::tempdir().unwrap();
    let ssh = home.path().join(".ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    std::fs::write(ssh.join("config"), b"Host hostb key\n").unwrap();
    fn in_ssh(p: &str) -> bool {
        p.contains("/.ssh")
    }
    let dir = ssh.to_str().unwrap();
    let scope = Some(dir);
    let glob = || call("c-k", "fs.glob", r#"{"pattern":"*"}"#);
    let grep = || call("c-kr", "fs.grep", r#"{"needle":"key","under":""}"#);
    let reached = secret_files_reached(dir, &glob(), in_ssh);
    assert_eq!(reached.len(), 1, "{reached:?}");
    assert!(reached[0].ends_with("/.ssh/config"), "{reached:?}");
    assert_eq!(secret_files_reached(dir, &grep(), in_ssh), reached);
    let none = SecretFiles {
        is_secret: in_ssh,
        allowed: &[],
    };
    let g = seam_secrets(scope, RiskClass::ReadOnly, glob(), Limits::default(), none);
    assert_ok_dispatch(&g);
    let v: serde_json::Value = serde_json::from_str(&g.record.content).unwrap();
    assert_eq!(v["paths"], serde_json::json!([]), "{}", g.record.content);
    assert!(v["left_out"].as_str().unwrap().starts_with("1 secret file"));
    let gr = seam_secrets(scope, RiskClass::ReadOnly, grep(), Limits::default(), none);
    assert_ok_dispatch(&gr);
    assert!(
        !gr.record.content.contains("hostb"),
        "{}",
        gr.record.content
    );
    let ok = SecretFiles {
        is_secret: in_ssh,
        allowed: &reached,
    };
    let gr = seam_secrets(scope, RiskClass::ReadOnly, grep(), Limits::default(), ok);
    assert!(gr.record.content.contains("hostb"), "{}", gr.record.content);
}

/// RC2-R3 (usability RC2 round 3, Major 1): a read that fails because no
/// such file is in the folder, or because the name is a folder, tells the
/// model why in plain words \u{2014} the folder it looked in and how to list it
/// \u{2014} never an "io: \u{2026} (os error 2)" that sends it searching the disk.
#[test]
fn rc2r3_a_failed_read_says_why_in_plain_words() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("notes")).unwrap();
    let folder = root.path().to_str().unwrap().to_string();

    let c = call("c-missing", "fs.read", r#"{"path":"alpha.txt"}"#);
    let r = seam(&root, RiskClass::ReadOnly, c, Limits::default());
    assert_failed_dispatch(&r);
    let told = &r.record.content;
    for words in [
        "No file \"alpha.txt\" in the folder",
        folder.as_str(),
        "fs_glob (pattern \"*\")",
    ] {
        assert!(told.contains(words), "{words:?} missing from: {told}");
    }
    assert!(!told.contains("os error"), "{told}");
    assert!(!told.starts_with("io:"), "{told}");

    let c = call("c-dir", "fs.read", r#"{"path":"notes"}"#);
    let r = seam(&root, RiskClass::ReadOnly, c, Limits::default());
    assert_failed_dispatch(&r);
    let told = &r.record.content;
    assert!(told.contains("is not a file"), "{told}");
    assert!(told.contains("fs_glob (pattern \"notes/*\")"), "{told}");
}

/// RC2-R3M7 (RC2-R32 review P2): a long file name (or a long Work folder)
/// once pushed the fs_glob instruction past the 300-byte cap, so it was cut
/// off. The names are cut instead (from the front, "\u{2026}" and their end):
/// the whole instruction is always there.
#[test]
fn rc2r3m7_a_long_name_never_cuts_the_instruction() {
    let root = tempfile::tempdir().unwrap();
    let long = "n".repeat(220);
    std::fs::create_dir_all(root.path().join(&long)).unwrap();

    let missing = format!("{}.txt", "x".repeat(240));
    let c = call(
        "c-missing",
        "fs.read",
        &serde_json::json!({ "path": missing }).to_string(),
    );
    let r = seam(&root, RiskClass::ReadOnly, c, Limits::default());
    assert_failed_dispatch(&r);
    let told = &r.record.content;
    assert!(told.len() <= 300, "{} bytes: {told}", told.len());
    assert!(told.starts_with("No file \"\u{2026}"), "{told}");
    assert!(
        told.ends_with(
            "List the folder with fs_glob (pattern \"*\") to see the names in it; the file is \
             not searched for anywhere else."
        ),
        "{told}"
    );
    assert!(told.contains("xxx.txt\""), "the name's end is kept: {told}");

    let c = call(
        "c-dir",
        "fs.read",
        &serde_json::json!({ "path": long }).to_string(),
    );
    let r = seam(&root, RiskClass::ReadOnly, c, Limits::default());
    assert_failed_dispatch(&r);
    let told = &r.record.content;
    assert!(told.len() <= 300, "{} bytes: {told}", told.len());
    assert!(told.contains("is not a file"), "{told}");
    assert!(
        told.ends_with("fs_glob (pattern: its path followed by \"/*\")."),
        "{told}"
    );
}
