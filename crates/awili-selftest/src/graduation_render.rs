//! P8a: the Markdown rendering of a graduation [`Summary`].

use crate::graduation::Summary;

/// Render a summary as a small Markdown block with one table.
pub fn render_summary(summary: &Summary) -> String {
    let median = summary
        .median_success_frontier_tokens
        .map_or_else(|| "n/a".to_owned(), |m| m.to_string());
    // The verdict is decided on exact integers (median / comparator against
    // the typed literal); render those, not a rounded float, so a NOT YET at
    // `0.199999999999999999` is legible (Codex round 7).
    let ratio = match summary.median_success_frontier_tokens {
        Some(m) => format!(
            "{m} / {} = {:.6}",
            summary.comparator_frontier_tokens,
            m as f64 / summary.comparator_frontier_tokens as f64
        ),
        None => "n/a".to_owned(),
    };
    let verdict = if summary.passes { "PASS" } else { "NOT YET" };
    let mut out = String::new();
    out.push_str("## Graduation summary\n\n");
    out.push_str("| Measure | Value |\n|---|---|\n");
    out.push_str(&format!(
        "| Window start (UTC) | {} |\n",
        summary.window_start
    ));
    out.push_str(&format!("| Attempted | {} |\n", summary.attempted));
    out.push_str(&format!("| Successes | {} |\n", summary.successes));
    out.push_str(&format!("| Failures | {} |\n", summary.failures));
    out.push_str(&format!("| Fallbacks | {} |\n", summary.fallbacks));
    out.push_str(&format!("| Stopped | {} |\n", summary.stopped));
    out.push_str(&format!(
        "| Days covered / window | {} / {} |\n",
        summary.days_with_entries, summary.window_days
    ));
    out.push_str(&format!("| Median success frontier tokens | {median} |\n"));
    out.push_str(&format!("| Ratio | {ratio} |\n"));
    out.push_str(&format!(
        "| Target ratio | {} |\n",
        summary.target_ratio_literal
    ));
    out.push_str(&format!("| Verdict | {verdict} |\n"));
    out
}
