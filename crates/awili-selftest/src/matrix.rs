//! Cross-executor result matrix (P0a).
//!
//! Rows are probes, columns are executor/model pairs, cells are
//! [`Outcome`]s. [`Matrix::to_markdown`] renders the PASS/FAIL table shown
//! in `docs/daily-driver/01-findings-fable-2026-09-11.md`.

use crate::evaluate::Outcome;
use crate::manifest::Executor;
use crate::report::RunReport;

/// Rows = probes, columns = executor/model, cells = outcome.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Matrix {
    /// Probe ids, in first-seen order across the reports.
    pub rows: Vec<String>,
    /// One column per report.
    pub columns: Vec<Executor>,
    /// `cells[row][col]` — outcome for that probe in that report.
    pub cells: Vec<Vec<Outcome>>,
}

impl Matrix {
    /// Build a matrix from run reports; each report becomes one column.
    ///
    /// A probe absent from a report is filled with
    /// `Skipped { reason: "no-result" }`.
    pub fn from_reports(reports: &[RunReport]) -> Matrix {
        let mut rows: Vec<String> = Vec::new();
        for report in reports {
            for result in &report.results {
                if !rows.contains(&result.probe_id) {
                    rows.push(result.probe_id.clone());
                }
            }
        }
        let columns: Vec<Executor> = reports
            .iter()
            .map(|report| Executor {
                kind: report.executor,
                model: report.model.clone(),
            })
            .collect();
        let cells = rows
            .iter()
            .map(|row| {
                reports
                    .iter()
                    .map(|report| {
                        report
                            .results
                            .iter()
                            .find(|result| result.probe_id == *row)
                            .map(|result| result.outcome.clone())
                            .unwrap_or_else(|| Outcome::Skipped {
                                reason: "no-result".to_owned(),
                            })
                    })
                    .collect()
            })
            .collect();
        Matrix {
            rows,
            columns,
            cells,
        }
    }

    /// Render as a markdown table: header `| Probe | kind/model | ... |`,
    /// one row per probe, cells as `PASS` / `FAIL <reason>` /
    /// `ERROR <message>` / `SKIPPED <reason>`.
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("| Probe |");
        for column in &self.columns {
            out.push_str(&format!(" {} |", column_header(column)));
        }
        out.push('\n');

        let mut separator = String::from("|---|");
        for _ in &self.columns {
            separator.push_str("---|");
        }
        out.push_str(&separator);
        out.push('\n');

        for (row, probe_id) in self.rows.iter().enumerate() {
            out.push_str(&format!("| {probe_id} |"));
            for cell in &self.cells[row] {
                out.push_str(&format!(" {} |", cell_to_markdown(cell)));
            }
            out.push('\n');
        }
        out
    }
}

/// Column header for one executor/model pair.
fn column_header(column: &Executor) -> String {
    format!("{}/{}", column.kind.as_str(), column.model)
}

/// Cell rendering for one outcome.
fn cell_to_markdown(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Pass => "PASS".to_owned(),
        Outcome::Fail { reason } => format!("FAIL {reason}"),
        Outcome::Error { message } => format!("ERROR {message}"),
        Outcome::Skipped { reason } => format!("SKIPPED {reason}"),
    }
}
