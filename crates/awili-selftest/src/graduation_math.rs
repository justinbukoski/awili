//! P8a: the exact arithmetic behind a graduation verdict (median and the
//! target-ratio comparison). Kept apart from `graduation.rs` so each file
//! stays under the 400-line rule; unit-tested through `graduation_tests.rs`.
//! Also home to the span-based capture of the `target_ratio` literal, the
//! text that arithmetic runs on.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

/// `median / comparator <= target`, decided exactly against the decimal the
/// policy author wrote (Codex review rounds 1-5). `target_literal` is the
/// TOML text (`0.20`, `0.199999999999999999`, ...), read as
/// `numerator / 10^scale` and cross-multiplied in `u128`: no float on either
/// side, so neither a one-token miss at a `10^18` comparator nor any digit of
/// the target can round into a pass. The parser bounds `scale` by
/// [`MAX_RATIO_SCALE`], so the products cannot overflow for a validated
/// policy; a literal that is not a plain decimal fails closed.
pub(crate) fn within_target(median: u64, comparator: u64, target_literal: &str) -> bool {
    let Some((numerator, scale)) = decimal_fraction(target_literal) else {
        return false;
    };
    let Some(lhs) = 10u128
        .checked_pow(scale)
        .and_then(|p| u128::from(median).checked_mul(p))
    else {
        return false;
    };
    let Some(rhs) = numerator.checked_mul(u128::from(comparator)) else {
        return false;
    };
    lhs <= rhs
}

/// A plain decimal literal as `(numerator, scale)` with
/// `value == numerator / 10^scale`. `None` for anything but digits with at
/// most one point (no sign, no exponent, no empty side).
pub(crate) fn decimal_fraction(text: &str) -> Option<(u128, u32)> {
    let (int_part, frac_part) = text.split_once('.').unwrap_or((text, ""));
    if int_part.is_empty()
        || !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let numerator = format!("{int_part}{frac_part}").parse::<u128>().ok()?;
    let scale = u32::try_from(frac_part.len()).ok()?;
    Some((numerator, scale))
}

/// Median of `values` (sorted in place); even count = floor of the mean of
/// the two middle values. `None` when empty.
pub(crate) fn median_floor(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 1 {
        values[mid]
    } else {
        // Sum in u128 so two u64::MAX values cannot overflow.
        ((u128::from(values[mid - 1]) + u128::from(values[mid])) / 2) as u64
    })
}

/// Locates `policy.target_ratio` in the TOML source through the
/// deserializer's span (so only the `[policy]` table's value counts) and
/// returns its literal text: the source of truth for the verdict.
#[derive(Deserialize)]
struct SpannedPolicyFile {
    policy: SpannedRules,
}

/// See [`SpannedPolicyFile`].
#[derive(Deserialize)]
struct SpannedRules {
    target_ratio: toml::Spanned<f64>,
}

/// Most fractional digits a `target_ratio` literal may carry. With `scale`
/// bounded here, `median * 10^scale` and `numerator * comparator` both stay
/// below `u128::MAX` for any `u64` inputs (`1.8e19 * 1e18 = 1.8e37`), so the
/// verdict never has to fall back or overflow (Codex round 5).
pub(crate) const MAX_RATIO_SCALE: u32 = 18;

/// The `target_ratio` literal exactly as written in `[policy]`: digits with
/// at most one point and at most [`MAX_RATIO_SCALE`] fractional digits.
/// Exponent forms, signs and underscores are rejected so the text and the
/// verdict arithmetic agree.
pub(crate) fn target_ratio_literal(text: &str) -> Result<String> {
    let spanned: SpannedPolicyFile =
        toml::from_str(text).context("locate graduation policy `policy.target_ratio`")?;
    let span = spanned.policy.target_ratio.span();
    let literal = text
        .get(span)
        .ok_or_else(|| anyhow!("graduation policy `policy.target_ratio` span is out of range"))?
        .trim()
        .to_string();
    let Some((_, scale)) = decimal_fraction(&literal) else {
        bail!(
            "graduation policy `policy.target_ratio` must be a plain decimal literal (digits and one optional point), got `{literal}`"
        );
    };
    if scale > MAX_RATIO_SCALE {
        bail!(
            "graduation policy `policy.target_ratio` may carry at most {MAX_RATIO_SCALE} fractional digits, got {scale}"
        );
    }
    Ok(literal)
}
