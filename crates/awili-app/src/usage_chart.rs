//! Command Center usage bar chart (plan unit 21, Phase E).
//!
//! Unit 20 built the gateway-side decoders and the pure condenser
//! [`awili_gateway::rest::roll_up_usage`], which turns a raw
//! `/api/analytics/usage` response into a [`UsageRollup`] — a per-day in/out
//! token series plus top-N models/skills and copied-through totals. This module
//! is the GPUI surface that *renders* that rollup: a stacked per-day bar chart
//! (input on the bottom, output stacked above), a legend, a compact top-models
//! list, and a totals footer.
//!
//! The chart geometry is factored into pure, `#[test]`-covered helpers
//! ([`build_usage_chart`], [`nice_axis_max`], [`fmt_tokens`]) exactly like the
//! unit-19 subagent helpers — the rollup is the single source of truth and the
//! layout math is verifiable without a running gateway. The render function is a
//! thin, side-effect-free consumer of that geometry.
//!
//! The Restart-Gateway / Update-Hermes action buttons and their
//! `/api/actions/{name}/status` polling are **[LIVE]** — they need a live server
//! to be meaningful and are intentionally left for Owner to wire/verify; this
//! module ships only the code-verifiable usage visualization.
//!
//! Carries a module-level `dead_code` allow (like `terminal_select`) until the
//! remainder of unit 21 — the Command Center view that hosts this chart
//! alongside the sessions/system panels and the [LIVE] action buttons — lands
//! and calls [`usage_chart`]. The geometry helpers are exercised by the tests
//! below regardless.
#![allow(dead_code)]

use awili_gateway::rest::UsageRollup;
use gpui::{div, prelude::*};

use crate::chart_palette;
use crate::theme;
use crate::ui::{text, Card};

/// Max drawn height of a bar column, in pixels. The tallest day (input+output)
/// maps to the chart's `axis_max`, and each segment's height is its token count
/// as a fraction of `axis_max` times this.
const CHART_H: f32 = 96.0;
/// Width of a single day's bar column.
const BAR_W: f32 = 14.0;

/// Geometry for one day's stacked bar: the input segment sits on the bottom and
/// the output segment stacks directly above it. `in_frac`/`out_frac` are each
/// the segment's token count as a fraction of the chart's `axis_max`, clamped to
/// `[0.0, 1.0]`, so `in_frac + out_frac <= 1.0` for the tallest day (it equals 1
/// only when that day *is* the axis max).
#[derive(Clone, Debug, PartialEq)]
pub struct BarGeom {
    pub day: String,
    pub in_frac: f32,
    pub out_frac: f32,
    pub in_tokens: i64,
    pub out_tokens: i64,
}

/// The full drawable model for the usage chart, derived purely from a
/// [`UsageRollup`]. `axis_max` is the nice-rounded ceiling the bars are scaled
/// against; `axis_label` is its human-formatted form for the y-axis cap.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageChartModel {
    pub bars: Vec<BarGeom>,
    pub axis_max: i64,
    pub axis_label: String,
    /// Sum of input tokens across the drawn days.
    pub total_in: i64,
    /// Sum of output tokens across the drawn days.
    pub total_out: i64,
}

/// Round `raw` up to the nearest "nice" axis maximum — one of {1, 2, 5} times a
/// power of ten (…, 500, 1000, 2000, 5000, 10000, …). A zero or negative `raw`
/// yields `1`, so callers can divide by `axis_max` without a guard (every
/// segment fraction is then `0`). This keeps the y-axis cap a readable round
/// number rather than the exact busiest day.
pub fn nice_axis_max(raw: i64) -> i64 {
    if raw <= 0 {
        return 1;
    }
    // Largest power of ten that does not exceed `raw`. `checked_mul` keeps this
    // total for the full `i64` range — an astronomically large token count
    // saturates the axis at `i64::MAX` rather than overflowing/panicking.
    let mut mag = 1i64;
    while mag <= raw / 10 {
        match mag.checked_mul(10) {
            Some(v) => mag = v,
            None => return i64::MAX,
        }
    }
    for m in [1i64, 2, 5, 10] {
        let cand = mag.saturating_mul(m);
        if cand >= raw {
            return cand;
        }
    }
    i64::MAX
}

/// Human-format a token count compactly: `950` → `"950"`, `1234` → `"1.2k"`,
/// `5000` → `"5k"`, `12_300` → `"12.3k"`, `1_500_000` → `"1.5M"`. One decimal
/// place, trailing `.0` trimmed. Negative inputs are formatted with a leading
/// `-` on the magnitude.
pub fn fmt_tokens(n: i64) -> String {
    if n < 0 {
        // `saturating_neg` keeps `i64::MIN` from overflowing on negation.
        return format!("-{}", fmt_tokens(n.saturating_neg()));
    }
    let one_dp = |v: f64, suffix: &str| -> String {
        // Round to one decimal, then drop a trailing ".0".
        let r = (v * 10.0).round() / 10.0;
        if (r - r.trunc()).abs() < f64::EPSILON {
            format!("{}{}", r.trunc() as i64, suffix)
        } else {
            format!("{r:.1}{suffix}")
        }
    };
    if n >= 1_000_000 {
        one_dp(n as f64 / 1_000_000.0, "M")
    } else if n >= 1_000 {
        one_dp(n as f64 / 1_000.0, "k")
    } else {
        n.to_string()
    }
}

/// Build the pure chart geometry from a rollup. The axis is scaled to the
/// busiest day's *stacked* (input+output) total so no bar ever overflows
/// `CHART_H`; each segment's fraction is its own tokens over `axis_max`.
pub fn build_usage_chart(rollup: &UsageRollup) -> UsageChartModel {
    let peak_stacked = rollup
        .per_day
        .iter()
        .map(|d| d.input_tokens.saturating_add(d.output_tokens))
        .max()
        .unwrap_or(0);
    let axis_max = nice_axis_max(peak_stacked);

    let mut total_in = 0i64;
    let mut total_out = 0i64;
    let bars = rollup
        .per_day
        .iter()
        .map(|d| {
            total_in = total_in.saturating_add(d.input_tokens);
            total_out = total_out.saturating_add(d.output_tokens);
            BarGeom {
                day: d.day.clone(),
                in_frac: (d.input_tokens.max(0) as f32 / axis_max as f32).clamp(0.0, 1.0),
                out_frac: (d.output_tokens.max(0) as f32 / axis_max as f32).clamp(0.0, 1.0),
                in_tokens: d.input_tokens,
                out_tokens: d.output_tokens,
            }
        })
        .collect();

    UsageChartModel {
        bars,
        axis_max,
        axis_label: fmt_tokens(axis_max),
        total_in,
        total_out,
    }
}

/// Render the Command Center usage bar chart from a rollup. Returns `None` when
/// there is no per-day data to draw (the caller omits the card entirely rather
/// than showing an empty frame).
pub fn usage_chart(rollup: &UsageRollup) -> Option<impl IntoElement> {
    if rollup.per_day.is_empty() {
        return None;
    }
    let model = build_usage_chart(rollup);

    // One column per day: a fixed-height frame with the two segments packed to
    // the bottom (justify_end) so bars grow upward. `min_h(theme::z(1.))` keeps a
    // hairline visible for a nonzero-but-tiny segment.
    let columns: Vec<gpui::AnyElement> = model
        .bars
        .iter()
        .map(|b| {
            // Input sits on the bottom; output stacks directly above. The two
            // segments (each ≥1px when nonzero) are clamped so their combined
            // height never exceeds the `CHART_H` frame — the axis already
            // guarantees `in_frac + out_frac <= 1.0`, and capping `out_h` at the
            // leftover space absorbs the hairline minimums.
            let in_h = if b.in_tokens > 0 {
                (b.in_frac * CHART_H).clamp(1.0, CHART_H)
            } else {
                0.0
            };
            let out_h = if b.out_tokens > 0 {
                (b.out_frac * CHART_H).max(1.0).min(CHART_H - in_h)
            } else {
                0.0
            };
            div()
                .flex()
                .flex_col()
                .justify_end()
                .items_center()
                .h(theme::z(CHART_H))
                .w(theme::z(BAR_W))
                // Output stacks on top … (content palette: chart data, not
                // UI chrome — the series no longer borrow the theme tokens).
                .child(
                    div()
                        .w(theme::z(BAR_W))
                        .h(theme::z(out_h))
                        .rounded(theme::z(2.))
                        .bg(chart_palette::OUTPUT),
                )
                // … the input segment sits on the bottom.
                .child(
                    div()
                        .w(theme::z(BAR_W))
                        .h(theme::z(in_h))
                        .rounded(theme::z(2.))
                        .bg(chart_palette::INPUT),
                )
                .into_any_element()
        })
        .collect();

    let x_axis = {
        let first = model
            .bars
            .first()
            .map(|b| b.day.clone())
            .unwrap_or_default();
        let last = model.bars.last().map(|b| b.day.clone()).unwrap_or_default();
        div()
            .flex()
            .flex_row()
            .justify_between()
            .child(text::caption(first).id("usage-chart-axis-first"))
            .child(text::caption(last).id("usage-chart-axis-last"))
    };

    let legend = div()
        .flex()
        .flex_row()
        .gap(theme::z(theme::SP_12))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(theme::z(theme::SP_4))
                .child(
                    div()
                        .size(theme::z(theme::DOT))
                        .rounded(theme::z(2.))
                        .bg(chart_palette::INPUT),
                )
                .child(
                    text::caption(format!("in {}", fmt_tokens(model.total_in)))
                        .id("usage-chart-legend-in"),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(theme::z(theme::SP_4))
                .child(
                    div()
                        .size(theme::z(theme::DOT))
                        .rounded(theme::z(2.))
                        .bg(chart_palette::OUTPUT),
                )
                .child(
                    text::caption(format!("out {}", fmt_tokens(model.total_out)))
                        .id("usage-chart-legend-out"),
                ),
        );

    // Compact top-models list beneath the chart (already server-ranked and
    // capped by `roll_up_usage`).
    let model_rows: Vec<gpui::AnyElement> = rollup
        .top_models
        .iter()
        .enumerate()
        .map(|(index, m)| {
            let name = m.model.clone().unwrap_or_else(|| "—".to_string());
            let toks = m.input_tokens.saturating_add(m.output_tokens);
            div()
                .flex()
                .flex_row()
                .justify_between()
                .child(text::label(name).id(format!("usage-chart-model-{index}-name")))
                .child(
                    text::caption(fmt_tokens(toks)).id(format!("usage-chart-model-{index}-tokens")),
                )
                .into_any_element()
        })
        .collect();

    let cost = rollup.totals.total_estimated_cost;
    let mut footer = format!(
        "{} in · {} out",
        fmt_tokens(model.total_in),
        fmt_tokens(model.total_out)
    );
    if cost > 0.0 {
        footer.push_str(&format!(" · ${cost:.2}"));
    }

    Some(
        Card::new("usage-chart").children([div()
            .flex()
            .flex_col()
            .gap(theme::z(theme::SP_8))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .child(
                        text::label("USAGE")
                            .strong()
                            .text_color_override(theme::p().accent_text),
                    )
                    .child(
                        text::caption(format!("max {}/day", model.axis_label))
                            .id("usage-chart-axis-max"),
                    ),
            )
            // The plotted columns.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_end()
                    .gap(theme::z(3.))
                    .h(theme::z(CHART_H))
                    .children(columns),
            )
            .child(x_axis)
            .child(legend)
            .when(!model_rows.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(theme::z(theme::SP_4))
                        .children(model_rows),
                )
            })
            .child(text::caption(footer).id("usage-chart-totals"))]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_gateway::rest::{DayInOut, ModelUsage, UsageRollup, UsageTotals};

    fn day(day: &str, i: i64, o: i64) -> DayInOut {
        DayInOut {
            day: day.to_string(),
            input_tokens: i,
            output_tokens: o,
        }
    }

    fn rollup(days: Vec<DayInOut>) -> UsageRollup {
        UsageRollup {
            per_day: days,
            top_models: vec![],
            top_skills: vec![],
            totals: UsageTotals::default(),
        }
    }

    #[test]
    fn nice_axis_rounds_up_to_1_2_5_decade() {
        assert_eq!(nice_axis_max(1), 1);
        assert_eq!(nice_axis_max(50), 50);
        assert_eq!(nice_axis_max(1234), 2000);
        assert_eq!(nice_axis_max(5000), 5000);
        assert_eq!(nice_axis_max(5001), 10000);
        assert_eq!(nice_axis_max(8000), 10000);
        assert_eq!(nice_axis_max(150), 200);
        assert_eq!(nice_axis_max(3), 5);
    }

    #[test]
    fn nice_axis_guards_zero_and_negative() {
        assert_eq!(nice_axis_max(0), 1);
        assert_eq!(nice_axis_max(-100), 1);
    }

    #[test]
    fn nice_axis_saturates_instead_of_overflowing() {
        // Must not panic (debug) or wrap negative (release) on extreme inputs.
        let a = nice_axis_max(i64::MAX);
        assert!(a >= i64::MAX / 2, "axis should saturate high, got {a}");
        // A value just under the top decade still resolves to a positive cap.
        assert!(nice_axis_max(9_000_000_000_000_000_000) > 0);
    }

    #[test]
    fn fmt_tokens_guards_i64_min() {
        // Must not overflow on negation; exact magnitude is irrelevant.
        let s = fmt_tokens(i64::MIN);
        assert!(s.starts_with('-'));
    }

    #[test]
    fn fmt_tokens_compacts() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(950), "950");
        assert_eq!(fmt_tokens(1234), "1.2k");
        assert_eq!(fmt_tokens(5000), "5k");
        assert_eq!(fmt_tokens(12_300), "12.3k");
        assert_eq!(fmt_tokens(1_500_000), "1.5M");
        assert_eq!(fmt_tokens(2_000_000), "2M");
        assert_eq!(fmt_tokens(-1234), "-1.2k");
    }

    #[test]
    fn fmt_tokens_switches_units_at_exactly_the_threshold_not_before() {
        // The unit thresholds are `>=`, so the boundary value itself flips:
        // 999 is still raw, but exactly 1_000 compacts to "1k" (not "1000"),
        // and 950_000 stays in "k" while exactly 1_000_000 becomes "1M" (not
        // "1000k"). Every case in `fmt_tokens_compacts` sits strictly above a
        // threshold (950/1234/5000/…), so a `>=`→`>` regression at either the
        // thousand or the million boundary would pass all of them while
        // mislabelling the usage chart's axis at a round thousand/million.
        assert_eq!(
            fmt_tokens(999),
            "999",
            "just below the k threshold stays raw"
        );
        assert_eq!(
            fmt_tokens(1_000),
            "1k",
            "exactly 1_000 is the first k, not '1000'"
        );
        assert_eq!(
            fmt_tokens(950_000),
            "950k",
            "just below the M threshold stays in k"
        );
        assert_eq!(
            fmt_tokens(1_000_000),
            "1M",
            "exactly 1_000_000 is the first M, not '1000k'"
        );
    }

    #[test]
    fn build_scales_bars_against_peak_stacked_day() {
        // Busiest stacked day is 3000+1000 = 4000 → axis 5000.
        let r = rollup(vec![day("d1", 1000, 500), day("d2", 3000, 1000)]);
        let m = build_usage_chart(&r);
        assert_eq!(m.axis_max, 5000);
        assert_eq!(m.axis_label, "5k");
        assert_eq!(m.bars.len(), 2);
        // d2 input fraction = 3000/5000 = 0.6.
        assert!((m.bars[1].in_frac - 0.6).abs() < 1e-6);
        assert!((m.bars[1].out_frac - 0.2).abs() < 1e-6);
        // Tallest day never overflows: in+out fractions <= 1.
        for b in &m.bars {
            assert!(b.in_frac + b.out_frac <= 1.0 + 1e-6);
            assert!((0.0..=1.0).contains(&b.in_frac));
            assert!((0.0..=1.0).contains(&b.out_frac));
        }
    }

    #[test]
    fn build_accumulates_totals() {
        let r = rollup(vec![day("d1", 1000, 500), day("d2", 3000, 1000)]);
        let m = build_usage_chart(&r);
        assert_eq!(m.total_in, 4000);
        assert_eq!(m.total_out, 1500);
    }

    #[test]
    fn build_handles_all_zero_days_without_div_by_zero() {
        let r = rollup(vec![day("d1", 0, 0), day("d2", 0, 0)]);
        let m = build_usage_chart(&r);
        assert_eq!(m.axis_max, 1);
        assert!(m.bars.iter().all(|b| b.in_frac == 0.0 && b.out_frac == 0.0));
        assert_eq!(m.total_in, 0);
        assert_eq!(m.total_out, 0);
    }

    #[test]
    fn build_preserves_server_day_order() {
        let r = rollup(vec![day("mon", 1, 0), day("tue", 2, 0), day("wed", 3, 0)]);
        let m = build_usage_chart(&r);
        let order: Vec<&str> = m.bars.iter().map(|b| b.day.as_str()).collect();
        assert_eq!(order, vec!["mon", "tue", "wed"]);
    }

    #[test]
    fn render_is_none_when_no_days() {
        let r = rollup(vec![]);
        assert!(usage_chart(&r).is_none());
    }

    #[test]
    fn render_is_some_with_data() {
        let mut r = rollup(vec![day("d1", 100, 50)]);
        r.top_models = vec![ModelUsage {
            model: Some("opus".to_string()),
            input_tokens: 100,
            output_tokens: 50,
            estimated_cost: 0.01,
            sessions: 1,
            api_calls: 2,
        }];
        assert!(usage_chart(&r).is_some());
    }
}
