//! Acceptance 6 (P0-selftest.md): `Matrix::from_reports` with two reports
//! (fable 4/4, haiku 2/4) renders a markdown table with 2 columns and
//! per-row outcomes; snapshot-compared to a fixture string.

use awili_selftest::evaluate::{reduce, Outcome};
use awili_selftest::manifest::ExecutorKind;
use awili_selftest::matrix::Matrix;
use awili_selftest::report::{ProbeResult, RunReport};

fn probe_result(id: &str, outcome: Outcome) -> ProbeResult {
    ProbeResult {
        probe_id: id.to_owned(),
        executor: ExecutorKind::Claude,
        model: "scripted".to_owned(),
        outcome,
        duration_ms: 1,
        tokens_in: None,
        tokens_out: None,
        evidence: String::new(),
        tool_origin: None,
        started_at: "2026-09-11T00:00:00Z".to_owned(),
    }
}

fn run_report(model: &str, outcomes: &[(&str, Outcome)]) -> RunReport {
    let results: Vec<ProbeResult> = outcomes
        .iter()
        .map(|(id, outcome)| probe_result(id, outcome.clone()))
        .collect();
    RunReport {
        manifest_version: 1,
        executor: ExecutorKind::Claude,
        model: model.to_owned(),
        summary: reduce(&results),
        results,
    }
}

#[test]
fn matrix_renders_two_column_snapshot() {
    let fable = run_report(
        "fable",
        &[
            ("alpha", Outcome::Pass),
            ("beta", Outcome::Pass),
            ("gamma", Outcome::Pass),
            ("delta", Outcome::Pass),
        ],
    );
    let haiku = run_report(
        "haiku",
        &[
            ("alpha", Outcome::Pass),
            (
                "beta",
                Outcome::Fail {
                    reason: "missing: fleet".into(),
                },
            ),
            ("gamma", Outcome::Pass),
            (
                "delta",
                Outcome::Fail {
                    reason: "declared-missing".into(),
                },
            ),
        ],
    );

    let matrix = Matrix::from_reports(&[fable, haiku]);
    assert_eq!(matrix.columns.len(), 2);
    assert_eq!(
        matrix.rows,
        vec![
            "alpha".to_owned(),
            "beta".to_owned(),
            "gamma".to_owned(),
            "delta".to_owned()
        ]
    );

    let expected = "\
| Probe | claude/fable | claude/haiku |
|---|---|---|
| alpha | PASS | PASS |
| beta | PASS | FAIL missing: fleet |
| gamma | PASS | PASS |
| delta | PASS | FAIL declared-missing |
";
    assert_eq!(matrix.to_markdown(), expected);
}

#[test]
fn matrix_fills_missing_probe_as_skipped() {
    let full = run_report("fable", &[("alpha", Outcome::Pass)]);
    let partial = run_report(
        "haiku",
        &[("alpha", Outcome::Pass), ("beta", Outcome::Pass)],
    );
    let matrix = Matrix::from_reports(&[full, partial]);
    let cell = &matrix.cells[1][0];
    assert_eq!(
        cell,
        &Outcome::Skipped {
            reason: "no-result".to_owned()
        }
    );
    assert!(matrix.to_markdown().contains("SKIPPED no-result"));
}
