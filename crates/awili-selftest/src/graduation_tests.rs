//! P8a required proof for the graduation ledger (offline, temp dirs only).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{DateTime, Utc};

use super::*;

static TEMP_SEQ: AtomicUsize = AtomicUsize::new(0);

fn temp_ledger(tag: &str) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "awili-graduation-{}-{tag}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir.join("ledger.jsonl")
}

pub(super) fn at(text: &str) -> DateTime<Utc> {
    parse_rfc3339(text).expect("test timestamp parses")
}

pub(super) fn entry(recorded_at: &str, outcome: Outcome, frontier_tokens: u64) -> LedgerEntry {
    LedgerEntry {
        v: LEDGER_VERSION,
        recorded_at: recorded_at.to_owned(),
        task_id: "task-1".to_owned(),
        executor: "dsh".to_owned(),
        model: "qwen3.8-27b".to_owned(),
        outcome,
        frontier_tokens,
        local_tokens: 5000,
        notes: None,
    }
}

pub(super) fn success(recorded_at: &str, frontier_tokens: u64) -> LedgerEntry {
    entry(recorded_at, Outcome::Success, frontier_tokens)
}

pub(super) const POLICY_TOML: &str = r#"
version = 1
[comparator]
frozen_at = "2026-09-11"
source = "cowork-2026-09-11"
frontier_tokens = 100000
[policy]
target_ratio = 0.20
window_days = 7
"#;

pub(super) fn policy() -> GraduationPolicy {
    GraduationPolicy::from_toml_str(POLICY_TOML).expect("policy parses")
}

/// Seven successes on 2026-09-17..23, one per day, all at `tokens`.
pub(super) fn full_week(tokens: u64) -> Vec<LedgerEntry> {
    (17..=23)
        .map(|day| success(&format!("2026-09-{day:02}T12:00:00Z"), tokens))
        .collect()
}

#[test]
fn append_creates_file_and_appends_lines() {
    let path = temp_ledger("append");
    Ledger::append(&path, &success("2026-09-23T10:00:00Z", 1000)).expect("first append");
    let first = std::fs::read_to_string(&path).expect("read");
    Ledger::append(&path, &success("2026-09-23T11:00:00Z", 2000)).expect("second append");
    let text = std::fs::read_to_string(&path).expect("read");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(format!("{}\n", lines[0]), first, "first line unchanged");
    assert!(lines[1].contains("\"frontier_tokens\":2000"));
}

#[test]
fn append_never_truncates_existing_content() {
    let path = temp_ledger("truncate");
    let existing = "{\"v\":1,\"recorded_at\":\"2026-09-22T00:00:00Z\",\"task_id\":\"t\",\"executor\":\"e\",\"model\":\"m\",\"outcome\":\"Success\",\"frontier_tokens\":1,\"local_tokens\":0}\n";
    std::fs::write(&path, existing).expect("seed");
    Ledger::append(&path, &success("2026-09-23T10:00:00Z", 1000)).expect("append");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.starts_with(existing), "existing bytes preserved");
    assert_eq!(text.lines().count(), 2);
}

#[test]
fn fallback_without_reason_is_rejected() {
    let path = temp_ledger("fallback");
    let bad = entry(
        "2026-09-23T10:00:00Z",
        Outcome::Fallback {
            reason: "  ".to_owned(),
        },
        0,
    );
    let error = Ledger::append(&path, &bad).expect_err("rejected");
    assert!(error.to_string().contains("Fallback"), "{error}");
    assert!(!path.exists(), "nothing written");
}

#[test]
fn failed_without_reason_is_rejected() {
    let path = temp_ledger("failed");
    let bad = entry(
        "2026-09-23T10:00:00Z",
        Outcome::Failed {
            reason: String::new(),
        },
        0,
    );
    let error = Ledger::append(&path, &bad).expect_err("rejected");
    assert!(error.to_string().contains("Failed"), "{error}");
}

#[test]
fn empty_task_id_is_rejected() {
    let path = temp_ledger("task-id");
    let mut bad = success("2026-09-23T10:00:00Z", 1);
    bad.task_id = " ".to_owned();
    let error = Ledger::append(&path, &bad).expect_err("rejected");
    assert!(error.to_string().contains("task_id"), "{error}");
}

#[test]
fn bad_recorded_at_is_rejected() {
    let path = temp_ledger("recorded-at");
    let bad = success("yesterday", 1);
    let error = Ledger::append(&path, &bad).expect_err("rejected");
    assert!(error.to_string().contains("recorded_at"), "{error}");
}

#[test]
fn notes_are_redacted_before_write() {
    let path = temp_ledger("redact");
    let mut noted = success("2026-09-23T10:00:00Z", 1);
    noted.notes = Some("used token=sk-live-ABC123 for outline".to_owned());
    Ledger::append(&path, &noted).expect("append");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(!text.contains("sk-live-ABC123"), "secret leaked: {text}");
    assert!(text.contains("[REDACTED]"));
}

#[test]
fn read_missing_file_is_empty() {
    let path = temp_ledger("missing");
    assert!(Ledger::read(&path).expect("read").is_empty());
}

#[test]
fn read_fails_closed_on_malformed_line_with_line_number() {
    let path = temp_ledger("malformed");
    Ledger::append(&path, &success("2026-09-23T10:00:00Z", 1)).expect("append");
    Ledger::append(&path, &success("2026-09-23T11:00:00Z", 2)).expect("append");
    let mut text = std::fs::read_to_string(&path).expect("read");
    text.push_str("{not json\n");
    std::fs::write(&path, text).expect("corrupt");
    let error = Ledger::read(&path).expect_err("fails closed");
    assert!(error.to_string().contains("line 3"), "{error}");
}

#[test]
fn read_fails_closed_on_entry_violating_door_rules() {
    let path = temp_ledger("door-on-read");
    Ledger::append(&path, &success("2026-09-23T10:00:00Z", 1)).expect("append");
    let mut text = std::fs::read_to_string(&path).expect("read");
    text.push_str(
        "{\"v\":1,\"recorded_at\":\"2026-09-23T11:00:00Z\",\"task_id\":\"\",\"executor\":\"e\",\"model\":\"m\",\"outcome\":\"Success\",\"frontier_tokens\":1,\"local_tokens\":0}\n",
    );
    std::fs::write(&path, text).expect("corrupt");
    let error = Ledger::read(&path).expect_err("fails closed");
    let full = format!("{error:#}");
    assert!(full.contains("line 2"), "{full}");
    assert!(full.contains("task_id"), "{full}");
}

#[test]
fn recorded_at_is_normalized_to_utc_before_write() {
    let path = temp_ledger("utc");
    Ledger::append(&path, &success("2026-09-23T10:00:00+02:00", 1)).expect("append");
    let entries = Ledger::read(&path).expect("read");
    assert_eq!(entries[0].recorded_at, "2026-09-23T08:00:00Z");
}

#[test]
fn bare_credentials_in_notes_are_redacted() {
    // Codex review round 6: an unlabelled key must not reach the file either.
    let path = temp_ledger("bare-token");
    for secret in [
        "sk-live-ABC123XYZ789",
        "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
        "AKIAIOSFODNN7EXAMPLE",
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0",
    ] {
        let mut noted = success("2026-09-23T10:00:00Z", 1);
        noted.notes = Some(format!("retried after {secret} expired"));
        Ledger::append(&path, &noted).expect("append");
    }
    let text = std::fs::read_to_string(&path).expect("read");
    for fragment in [
        "ABC123XYZ789",
        "16C7e42F292c",
        "AKIAIOSFODNN7",
        "eyJhbGciOiJ",
    ] {
        assert!(!text.contains(fragment), "secret leaked: {fragment}");
    }
    assert!(text.contains("retried after [REDACTED] expired"), "{text}");
    // A human reason stays readable.
    assert_eq!(
        redact_notes("local verify failed on clippy, retried on codex"),
        "local verify failed on clippy, retried on codex"
    );
}

#[test]
fn m1b_stopped_round_trips_as_bare_string() {
    assert_eq!(
        serde_json::to_string(&Outcome::Stopped).expect("serialize"),
        "\"Stopped\""
    );
    let path = temp_ledger("m1b-stopped");
    let e = entry("2026-09-23T10:00:00Z", Outcome::Stopped, 0);
    Ledger::append(&path, &e).expect("append");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.contains("\"outcome\":\"Stopped\""), "{text}");
    let read = Ledger::read(&path).expect("read back");
    assert_eq!(read, vec![e], "the Stopped entry reads back equal");
}

#[test]
fn m1b_legacy_failed_stop_line_still_reads() {
    let path = temp_ledger("m1b-legacy-stop");
    let line = r#"{"v":1,"recorded_at":"2026-09-29T14:29:23Z","task_id":"work:c:t","executor":"gateway","model":"default","outcome":{"Failed":{"reason":"stopped by user"}},"frontier_tokens":0,"local_tokens":0}"#;
    std::fs::write(&path, format!("{line}\n")).expect("write");
    let read = Ledger::read(&path).expect("read");
    assert_eq!(read.len(), 1, "one line");
    assert_eq!(
        read[0].outcome,
        Outcome::Failed {
            reason: "stopped by user".to_owned()
        },
        "legacy stop lines read as Failed, unmigrated"
    );
}

#[test]
fn m1b_summary_counts_stops_apart_from_failures() {
    let policy = policy();
    let mut entries = full_week(100);
    entries.push(entry("2026-09-23T13:00:00Z", Outcome::Stopped, 0));
    entries.push(entry(
        "2026-09-23T14:00:00Z",
        Outcome::Failed {
            reason: "x".to_owned(),
        },
        0,
    ));
    let plain = summarize(&full_week(100), &policy, at("2026-09-23T23:59:00Z"));
    let full = summarize(&entries, &policy, at("2026-09-23T23:59:00Z"));
    assert_eq!(full.stopped, 1);
    assert_eq!(full.failures, 1);
    assert_eq!(full.attempted, 9);
    assert_eq!(full.successes, 7);
    assert_eq!(full.fallbacks, 0);
    assert_eq!(
        full.median_success_frontier_tokens, plain.median_success_frontier_tokens,
        "stops and failures do not move the success median"
    );
    let rendered = render_summary(&full);
    assert!(rendered.contains("| Stopped | 1 |"), "{rendered}");
    assert!(rendered.contains("| Failures | 1 |"), "{rendered}");
}

#[test]
fn m1b_fix1_legacy_summary_without_stopped_deserializes() {
    // A summary written before M1b has no `stopped` key and must still
    // deserialize, with the field defaulting.
    let legacy = r#"{"window_start":"2026-09-17","window_days":7,"days_with_entries":7,"attempted":7,"successes":7,"failures":0,"fallbacks":0,"median_success_frontier_tokens":100,"ratio":0.001,"target_ratio":0.2,"target_ratio_literal":"0.20","comparator_frontier_tokens":100000,"coverage_complete":true,"passes":true}"#;
    let parsed: Summary = serde_json::from_str(legacy).expect("legacy summary deserializes");
    assert_eq!(parsed.stopped, u64::default(), "stopped defaults");

    // A current summary with stopped > 0 round trips unchanged.
    let mut current = parsed;
    current.stopped = 3;
    let reparsed: Summary =
        serde_json::from_str(&serde_json::to_string(&current).expect("serialize"))
            .expect("round trip");
    assert_eq!(reparsed, current, "stopped round trips");
}

/// S7d (F0824): Failed and Fallback reasons are free text and get the same
/// redaction as `notes` before they reach the file. The human part stays.
#[test]
fn s7d_outcome_reasons_are_redacted_on_append() {
    let path = temp_ledger("s7d-reasons");
    let failed = entry(
        "2026-09-23T10:00:00Z",
        Outcome::Failed {
            reason: "HTTP 401 from gateway: token=tok-AAAA1111 Authorization: Bearer abcdef123456"
                .to_owned(),
        },
        0,
    );
    let fallback = entry(
        "2026-09-23T11:00:00Z",
        Outcome::Fallback {
            reason: "after local failed: {\"api_key\": \"sk live 9999\"} ghp_ZZZZ1111yyyy2222"
                .to_owned(),
        },
        0,
    );
    Ledger::append(&path, &failed).expect("append failed");
    Ledger::append(&path, &fallback).expect("append fallback");
    let text = std::fs::read_to_string(&path).expect("read");
    for secret in [
        "tok-AAAA1111",
        "abcdef123456",
        "sk live 9999",
        "ZZZZ1111yyyy2222",
    ] {
        assert!(
            !text.contains(secret),
            "secret leaked into the ledger: {secret}\n{text}"
        );
    }
    assert!(text.contains("HTTP 401 from gateway"), "{text}");
    assert!(text.contains("after local failed"), "{text}");
}

/// S7d (F0828): `Ledger::read` returns the checked, redacted entry, so a
/// line written before this fix (or by hand) never hands a secret back.
#[test]
fn s7d_read_redacts_lines_written_before_the_fix() {
    let path = temp_ledger("s7d-legacy");
    let line = r#"{"v":1,"recorded_at":"2026-09-23T10:00:00Z","task_id":"t","executor":"e","model":"m","outcome":{"Failed":{"reason":"engine said Bearer legacySECRET42"}},"frontier_tokens":0,"local_tokens":0,"notes":"secret=hunter2"}"#;
    std::fs::write(&path, format!("{line}\n")).expect("write");
    let read = Ledger::read(&path).expect("read");
    assert_eq!(read.len(), 1);
    let Outcome::Failed { reason } = &read[0].outcome else {
        panic!("outcome kind kept: {:?}", read[0].outcome);
    };
    assert!(!reason.contains("legacySECRET42"), "{reason}");
    assert!(reason.starts_with("engine said"), "{reason}");
    let notes = read[0].notes.as_deref().unwrap_or("");
    assert!(!notes.contains("hunter2"), "{notes}");
}
