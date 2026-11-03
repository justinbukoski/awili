//! Acceptance 2-5 and 7-8 (P0-selftest.md): `evaluate`, `reduce`, evidence
//! clamping, and redaction.

use awili_selftest::evaluate::{evaluate, reduce, Outcome};
use awili_selftest::manifest::{ExecutorKind, Expect};
use awili_selftest::redact::redact;
use awili_selftest::report::{clamp_evidence, ProbeResult, EVIDENCE_LIMIT};

fn probe_result(id: &str, outcome: Outcome) -> ProbeResult {
    ProbeResult {
        probe_id: id.to_owned(),
        executor: ExecutorKind::Local,
        model: "fixture".to_owned(),
        outcome,
        duration_ms: 1,
        tokens_in: None,
        tokens_out: None,
        evidence: String::new(),
        tool_origin: None,
        started_at: "2026-09-11T00:00:00Z".to_owned(),
    }
}

#[test]
fn contains_all_passes_when_all_present() {
    let expect = Expect::ContainsAll(vec!["alpha".to_owned(), "beta".to_owned()]);
    assert_eq!(evaluate(&expect, "we have ALPHA and beta"), Outcome::Pass);
}

#[test]
fn contains_all_fails_naming_the_first_missing_string() {
    let expect = Expect::ContainsAll(vec![
        "alpha".to_owned(),
        "beta".to_owned(),
        "gamma".to_owned(),
    ]);
    assert_eq!(
        evaluate(&expect, "only alpha here"),
        Outcome::Fail {
            reason: "missing: beta".to_owned()
        }
    );
}

#[test]
fn regex_passes_on_the_spec_uuid() {
    let expect = Expect::Regex(r"[0-9a-f]{8}-[0-9a-f]{4}".to_owned());
    assert_eq!(
        evaluate(&expect, "2b4994af-6177-401e-9226-6548abad5fd3"),
        Outcome::Pass
    );
}

#[test]
fn sentinel_present_is_declared_missing() {
    let expect = Expect::Sentinel("NO OUTLINE ACCESS".to_owned());
    assert_eq!(
        evaluate(&expect, "I cannot do that. NO OUTLINE ACCESS"),
        Outcome::Fail {
            reason: "declared-missing".to_owned()
        }
    );
}

#[test]
fn any_of_passes_on_first_branch() {
    let expect = Expect::AnyOf(vec![
        Expect::ContainsAll(vec!["READY".to_owned(), "READY".to_owned()]),
        Expect::Sentinel("NO SUBAGENTS".to_owned()),
    ]);
    assert_eq!(
        evaluate(&expect, "two sub-agents: READY and READY"),
        Outcome::Pass
    );
}

#[test]
fn any_of_reports_declared_missing_when_sentinel_fires() {
    let expect = Expect::AnyOf(vec![
        Expect::ContainsAll(vec!["READY".to_owned(), "READY".to_owned()]),
        Expect::Sentinel("NO SUBAGENTS".to_owned()),
    ]);
    assert_eq!(
        evaluate(&expect, "NO SUBAGENTS available in this session"),
        Outcome::Fail {
            reason: "declared-missing".to_owned()
        }
    );
}

#[test]
fn reduce_counts_outcomes_and_pass_rate() {
    let results = vec![
        probe_result("p1", Outcome::Pass),
        probe_result("p2", Outcome::Pass),
        probe_result("p3", Outcome::Pass),
        probe_result("p4", Outcome::Fail { reason: "x".into() }),
        probe_result(
            "p5",
            Outcome::Error {
                message: "timeout".into(),
            },
        ),
    ];
    let summary = reduce(&results);
    assert_eq!(summary.pass, 3);
    assert_eq!(summary.fail, 1);
    assert_eq!(summary.error, 1);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.total, 5);
    assert_eq!(summary.pass_rate, 0.6);
}

#[test]
fn evidence_longer_than_2_kib_is_truncated_with_suffix() {
    let big = "y".repeat(EVIDENCE_LIMIT + 100);
    let clamped = clamp_evidence(&big);
    assert!(
        clamped.ends_with("…[truncated]"),
        "truncated evidence must end with the suffix"
    );
    assert!(clamped.len() <= EVIDENCE_LIMIT);
    // ASCII input: clamped to exactly the budget.
    assert_eq!(clamped.len(), EVIDENCE_LIMIT);
}

#[test]
fn evidence_within_limit_is_untouched() {
    assert_eq!(clamp_evidence("short evidence"), "short evidence");
}

#[test]
fn redact_bearer_header() {
    assert_eq!(
        redact("Authorization: Bearer abc123"),
        "Authorization: [REDACTED]"
    );
}

#[test]
fn redact_key_value_pairs() {
    assert_eq!(redact("api_key=secret123"), "api_key: [REDACTED]");
    assert_eq!(redact("token: abc123 rest"), "token: [REDACTED] rest");
}
