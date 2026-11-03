//! P8a required proof, part 2: policy parsing, the typed-literal threshold
//! and the window arithmetic (split from `graduation_tests.rs` to keep both
//! files under the 400-line rule).

use super::graduation_tests::{at, entry, full_week, policy, success, POLICY_TOML};
use super::*;

#[test]
fn policy_parses_defaults_and_rejects_bad_fields() {
    let good = policy();
    assert_eq!(good.comparator.frontier_tokens, 100000);
    assert_eq!(good.policy.window_days, 7);
    for (replace_from, replace_to, field) in [
        ("target_ratio = 0.20", "target_ratio = 0.0", "target_ratio"),
        ("target_ratio = 0.20", "target_ratio = 1.5", "target_ratio"),
        ("window_days = 7", "window_days = 0", "window_days"),
        (
            "frontier_tokens = 100000",
            "frontier_tokens = 0",
            "frontier_tokens",
        ),
        ("version = 1", "version = 2", "version"),
    ] {
        let text = POLICY_TOML.replacen(replace_from, replace_to, 1);
        let error = GraduationPolicy::from_toml_str(&text).expect_err(field);
        assert!(error.to_string().contains(field), "{field}: {error}");
    }
}

#[test]
fn summary_median_even_and_odd_counts() {
    let now = at("2026-09-23T23:00:00Z");
    let odd = vec![
        success("2026-09-23T01:00:00Z", 30),
        success("2026-09-23T02:00:00Z", 10),
        success("2026-09-23T03:00:00Z", 20),
    ];
    assert_eq!(
        summarize(&odd, &policy(), now).median_success_frontier_tokens,
        Some(20)
    );
    let even = vec![
        success("2026-09-23T01:00:00Z", 10),
        success("2026-09-23T02:00:00Z", 21),
        success("2026-09-23T03:00:00Z", 40),
        success("2026-09-23T04:00:00Z", 100),
    ];
    // (21 + 40) / 2 = 30.5, rounded down.
    assert_eq!(
        summarize(&even, &policy(), now).median_success_frontier_tokens,
        Some(30)
    );
}

#[test]
fn summary_ratio_and_pass_threshold() {
    let now = at("2026-09-23T23:00:00Z");
    let exact = summarize(&full_week(20000), &policy(), now);
    assert!(exact.coverage_complete);
    assert_eq!(exact.ratio, Some(0.2));
    assert!(exact.passes, "exactly at target passes");
    let over = summarize(&full_week(20001), &policy(), now);
    assert!(!over.passes, "one token over fails");
    assert_eq!(over.attempted, 7);
    assert_eq!(over.successes, 7);
}

#[test]
fn summary_threshold_is_exact_at_large_comparators() {
    // Codex review F1: 10^18 comparator, median one token over 20% must fail.
    let text = POLICY_TOML.replacen(
        "frontier_tokens = 100000",
        "frontier_tokens = 1000000000000000000",
        1,
    );
    let big = GraduationPolicy::from_toml_str(&text).expect("policy parses");
    let now = at("2026-09-23T23:00:00Z");
    assert!(summarize(&full_week(200_000_000_000_000_000), &big, now).passes);
    assert!(!summarize(&full_week(200_000_000_000_000_001), &big, now).passes);
}

#[test]
fn summary_threshold_honours_every_target_ratio_digit() {
    // Codex review round 2: a target with seven decimals must not be rounded.
    let text = POLICY_TOML
        .replacen("target_ratio = 0.20", "target_ratio = 0.2000006", 1)
        .replacen("frontier_tokens = 100000", "frontier_tokens = 10000000", 1);
    let fine = GraduationPolicy::from_toml_str(&text).expect("policy parses");
    let now = at("2026-09-23T23:00:00Z");
    assert!(summarize(&full_week(2_000_006), &fine, now).passes);
    assert!(!summarize(&full_week(2_000_008), &fine, now).passes);
    // target_ratio = 1.0 (the validation ceiling) passes at the comparator
    // exactly and fails one over.
    let text = POLICY_TOML.replacen("target_ratio = 0.20", "target_ratio = 1.0", 1);
    let ceiling = GraduationPolicy::from_toml_str(&text).expect("policy parses");
    assert!(summarize(&full_week(100000), &ceiling, now).passes);
    assert!(!summarize(&full_week(100001), &ceiling, now).passes);
}

#[test]
fn summary_threshold_uses_the_typed_literal_not_the_float() {
    // Codex review round 3: 0.199999999999999999 parses to the f64 0.2; the
    // typed decimal must still govern, so 2e17 of 1e18 is one token OVER.
    let text = POLICY_TOML
        .replacen(
            "target_ratio = 0.20",
            "target_ratio = 0.199999999999999999",
            1,
        )
        .replacen(
            "frontier_tokens = 100000",
            "frontier_tokens = 1000000000000000000",
            1,
        );
    let strict = GraduationPolicy::from_toml_str(&text).expect("policy parses");
    assert_eq!(strict.policy.target_ratio_literal, "0.199999999999999999");
    let now = at("2026-09-23T23:00:00Z");
    let over = summarize(&full_week(200_000_000_000_000_000), &strict, now);
    assert!(!over.passes);
    // The rendering shows the exact figures the verdict used (round 7).
    let rendered = render_summary(&over);
    assert!(
        rendered.contains("| Target ratio | 0.199999999999999999 |"),
        "{rendered}"
    );
    assert!(
        rendered.contains("| Ratio | 200000000000000000 / 1000000000000000000 = 0.200000 |"),
        "{rendered}"
    );
    assert!(summarize(&full_week(199_999_999_999_999_999), &strict, now).passes);
    // A trailing comment after the literal is fine; an exponent form is not.
    let commented = POLICY_TOML.replacen("target_ratio = 0.20", "target_ratio = 0.20 # fifth", 1);
    assert_eq!(
        GraduationPolicy::from_toml_str(&commented)
            .expect("parses")
            .policy
            .target_ratio_literal,
        "0.20"
    );
    let exponent = POLICY_TOML.replacen("target_ratio = 0.20", "target_ratio = 2e-1", 1);
    let error = GraduationPolicy::from_toml_str(&exponent).expect_err("rejected");
    assert!(
        error.to_string().contains("plain decimal literal"),
        "{error}"
    );
}

#[test]
fn target_ratio_literal_is_taken_from_the_policy_table_only() {
    // Codex review round 4: a decoy `target_ratio` in another table must not
    // be the literal the verdict uses, and the exponent form in [policy] is
    // rejected even when a decoy would parse to the same float.
    let decoy = format!("{POLICY_TOML}\n[other]\ntarget_ratio = 0.20\n");
    let p = GraduationPolicy::from_toml_str(&decoy).expect("parses");
    assert_eq!(p.policy.target_ratio_literal, "0.20");
    let strict = decoy.replacen(
        "target_ratio = 0.20\nwindow_days",
        "target_ratio = 0.199999999999999999\nwindow_days",
        1,
    );
    assert!(
        strict.contains("0.199999999999999999"),
        "fixture edited the [policy] value"
    );
    let p = GraduationPolicy::from_toml_str(&strict).expect("parses");
    assert_eq!(p.policy.target_ratio_literal, "0.199999999999999999");
    let exponent = decoy.replacen(
        "target_ratio = 0.20\nwindow_days",
        "target_ratio = 0.199999999999999999e0\nwindow_days",
        1,
    );
    let error = GraduationPolicy::from_toml_str(&exponent).expect_err("rejected");
    assert!(
        error.to_string().contains("plain decimal literal"),
        "{error}"
    );
    // The range rule is decided on the literal: 1.0000000000000001 rounds to
    // the f64 1.0 but is over 1 as typed.
    let over = POLICY_TOML.replacen(
        "target_ratio = 0.20",
        "target_ratio = 1.0000000000000001",
        1,
    );
    let error = GraduationPolicy::from_toml_str(&over).expect_err("rejected");
    assert!(error.to_string().contains("0 < ratio <= 1"), "{error}");
    let zero = POLICY_TOML.replacen("target_ratio = 0.20", "target_ratio = 0.000", 1);
    assert!(GraduationPolicy::from_toml_str(&zero).is_err());
}

#[test]
fn target_ratio_precision_is_bounded_so_the_verdict_never_overflows() {
    // Codex review round 5: 22 fractional digits at a 1e18 comparator made
    // both cross products overflow and turned an exact-at-target week into a
    // false NOT YET. The parser now caps the literal at 18 fractional digits;
    // at that cap the arithmetic is exact for every u64 input.
    let too_fine = POLICY_TOML.replacen(
        "target_ratio = 0.20",
        "target_ratio = 0.2000000000000000000000",
        1,
    );
    let error = GraduationPolicy::from_toml_str(&too_fine).expect_err("rejected");
    assert!(
        error.to_string().contains("at most 18 fractional digits"),
        "{error}"
    );
    let at_cap = POLICY_TOML
        .replacen(
            "target_ratio = 0.20",
            "target_ratio = 0.200000000000000000",
            1,
        )
        .replacen(
            "frontier_tokens = 100000",
            "frontier_tokens = 9223372036854775807",
            1,
        );
    let policy = GraduationPolicy::from_toml_str(&at_cap).expect("18 digits parse");
    let now = at("2026-09-23T23:00:00Z");
    // TOML integers are i64, so the largest comparator is i64::MAX; 20% of it
    // floors to 1844674407370955161: at target passes, one over fails.
    assert!(summarize(&full_week(1_844_674_407_370_955_161), &policy, now).passes);
    assert!(!summarize(&full_week(1_844_674_407_370_955_162), &policy, now).passes);
}

#[test]
fn summary_requires_every_window_day() {
    let now = at("2026-09-23T23:00:00Z");
    let mut six = full_week(1000);
    six.remove(3);
    let summary = summarize(&six, &policy(), now);
    assert_eq!(summary.days_with_entries, 6);
    assert!(!summary.coverage_complete);
    assert!(!summary.passes);
    let rendered = render_summary(&summary);
    assert!(rendered.contains("| Days covered / window | 6 / 7 |"));
    assert!(rendered.contains("NOT YET"));
}

#[test]
fn summary_window_excludes_older_entries() {
    let now = at("2026-09-23T23:00:00Z");
    let mut entries = full_week(1000);
    entries.push(success("2026-09-16T23:59:59Z", 999_999));
    entries.push(entry(
        "2026-09-10T00:00:00Z",
        Outcome::Fallback {
            reason: "old".to_owned(),
        },
        0,
    ));
    let summary = summarize(&entries, &policy(), now);
    assert_eq!(summary.window_start, "2026-09-17");
    assert_eq!(summary.attempted, 7);
    assert_eq!(summary.fallbacks, 0);
    assert_eq!(summary.median_success_frontier_tokens, Some(1000));
    assert!(summary.passes);
    assert!(render_summary(&summary).contains("| Verdict | PASS |"));
}
