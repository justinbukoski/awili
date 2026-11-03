//! Token pricing (slice-8 spec §2): USD-per-million-token rates for the models
//! Awili talks to, and the arithmetic turning a turn's token counts into dollars.
//!
//! Pure data + math — no clock, no IO — so the cost ticker's numbers are
//! unit-testable to the cent. The table is deliberately conservative: anything
//! it does not recognise prices at $0 (treat-as-free), so a new or self-hosted
//! model never shows an invented cost. Rates are the published Anthropic API
//! list prices as of the slice-8 spec (2026-06).

/// USD per million tokens, input and output priced separately.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TokenPrice {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

/// Free — local models, and the fallback for anything unrecognised.
const FREE: TokenPrice = TokenPrice {
    input_per_mtok: 0.0,
    output_per_mtok: 0.0,
};

/// The price for a provider·model pair (spec §2 match rules):
///
/// - any `ollama`/`local` provider → free
/// - `anthropic` + `claude-haiku*` → $0.80 / $4.00
/// - `anthropic` + `claude-sonnet*` → $3.00 / $15.00
/// - `anthropic` + `claude-opus*` → $15.00 / $75.00
/// - `anthropic` + `claude-fable*` → $3.00 / $15.00
/// - `deepseek` + `deepseek-flash*` → $0.30 / $1.20 (peak rates — see below)
/// - `deepseek` + `deepseek-v4-pro*` → $1.32 / $3.96 (peak rates — see below)
/// - anything else → free (unknown must not invent a cost)
///
/// Matching is case-insensitive on both fields; models match by prefix so a
/// dated id like `claude-haiku-4-5-20251001` prices the same as its family.
pub fn price_for(provider: &str, model: &str) -> TokenPrice {
    let provider = provider.to_ascii_lowercase();
    if provider == "deepseek" {
        // PEAK RATES, deliberately: DeepSeek halves them off-peak (peak =
        // 01:00-04:00 and 06:00-10:00 UTC, Mon-Fri), but `price_for` is pure
        // data + math with NO clock, and that property must hold. Pricing at
        // peak means the ledger may OVERSTATE spend off-peak and can never
        // understate it — matching this module's "deliberately conservative"
        // stance. The off-peak discount was not missed.
        let model = model.to_ascii_lowercase();
        if model.starts_with("deepseek-flash") {
            TokenPrice {
                input_per_mtok: 0.30,
                output_per_mtok: 1.20,
            }
        } else if model.starts_with("deepseek-v4-pro") {
            TokenPrice {
                input_per_mtok: 1.32,
                output_per_mtok: 3.96,
            }
        } else {
            // An unrecognised deepseek-* id must not invent a cost either.
            FREE
        }
    } else if provider != "anthropic" {
        // ollama/local — and any provider we cannot price (e.g. a self-hosted
        // Hermes box) — is free rather than guessed-at.
        FREE
    } else {
        let model = model.to_ascii_lowercase();
        if model.starts_with("claude-haiku") {
            TokenPrice {
                input_per_mtok: 0.80,
                output_per_mtok: 4.00,
            }
        } else if model.starts_with("claude-sonnet") || model.starts_with("claude-fable") {
            TokenPrice {
                input_per_mtok: 3.00,
                output_per_mtok: 15.00,
            }
        } else if model.starts_with("claude-opus") {
            TokenPrice {
                input_per_mtok: 15.00,
                output_per_mtok: 75.00,
            }
        } else {
            FREE
        }
    }
}

/// RC2-CNCLF6: the price of a provider·model pair only when it is KNOWN —
/// a priced DeepSeek or Anthropic model, or a local `ollama`/`local`
/// provider (free). Anything else is `None`: its cost is not reported,
/// never shown as free ([`price_for`] keeps pricing it at $0 for the
/// ledger, so nothing is invented there either).
pub fn known_price(provider: &str, model: &str) -> Option<TokenPrice> {
    let p = provider.to_ascii_lowercase();
    if p == "ollama" || p == "local" {
        return Some(FREE);
    }
    if p != "deepseek" && p != "anthropic" {
        return None;
    }
    let price = price_for(provider, model);
    (price != FREE).then_some(price)
}

/// The cost in USD of a turn that consumed `input`/`output` tokens at `price`.
pub fn cost_usd(price: TokenPrice, input: u32, output: u32) -> f64 {
    (f64::from(input) * price.input_per_mtok + f64::from(output) * price.output_per_mtok)
        / 1_000_000.0
}

/// Format a dollar amount for the cost chip: `$` + at most 4 significant
/// figures, trailing decimal zeros stripped (review slice-8 SF4 — the spec's
/// 2–4 sig-fig contract: `$1.5`, never `$1.50`; `$123.5`, never `$123.46`).
/// Examples: `$0.0031`, `$0.0124`, `$1.5`, `$2`, `$12.35`.
pub fn format_usd(usd: f64) -> String {
    if usd <= 0.0 {
        return "$0".to_string();
    }
    // Round to the decimal place that yields 4 significant figures (0 decimals
    // once the amount reaches four digits of dollars), then strip the trailing
    // zeros the fixed-width format leaves behind.
    let magnitude = usd.log10().floor() as i32;
    let decimals = (3 - magnitude).clamp(0, 12) as usize;
    let mut s = format!("{usd:.decimals$}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    format!("${s}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_families_price_by_prefix() {
        assert_eq!(
            price_for("anthropic", "claude-haiku-4-5-20251001"),
            TokenPrice {
                input_per_mtok: 0.80,
                output_per_mtok: 4.00
            }
        );
        assert_eq!(
            price_for("anthropic", "claude-sonnet-4-6"),
            TokenPrice {
                input_per_mtok: 3.00,
                output_per_mtok: 15.00
            }
        );
        assert_eq!(
            price_for("anthropic", "claude-opus-4-8"),
            TokenPrice {
                input_per_mtok: 15.00,
                output_per_mtok: 75.00
            }
        );
        // fable prices as sonnet (spec §2).
        assert_eq!(
            price_for("anthropic", "claude-fable-5"),
            TokenPrice {
                input_per_mtok: 3.00,
                output_per_mtok: 15.00
            }
        );
        // Case-insensitive on both fields.
        assert_eq!(
            price_for("Anthropic", "Claude-Opus-4-8").output_per_mtok,
            75.00
        );
    }

    #[test]
    fn local_and_unknown_are_free() {
        assert_eq!(price_for("ollama", "llama3.1:8b"), FREE);
        assert_eq!(price_for("local", "anything"), FREE);
        assert_eq!(price_for("hermes", "Hermes-4-70B"), FREE);
        // An unrecognised Anthropic model must not invent a cost either.
        assert_eq!(price_for("anthropic", "claude-newthing-9"), FREE);
    }

    #[test]
    fn deepseek_prices_at_peak_rates_by_prefix() {
        assert_eq!(
            price_for("deepseek", "deepseek-flash"),
            TokenPrice {
                input_per_mtok: 0.30,
                output_per_mtok: 1.20
            }
        );
        assert_eq!(
            price_for("deepseek", "deepseek-v4-pro"),
            TokenPrice {
                input_per_mtok: 1.32,
                output_per_mtok: 3.96
            }
        );
        // Case-insensitive on both fields, like every other family.
        assert_eq!(
            price_for("DeepSeek", "DeepSeek-Flash").output_per_mtok,
            1.20
        );
        assert_eq!(
            price_for("DEEPSEEK", "DEEPSEEK-V4-PRO").input_per_mtok,
            1.32
        );
        // An unrecognised deepseek-* id must not invent a cost.
        assert_eq!(price_for("deepseek", "deepseek-v4-flash"), FREE);
        assert_eq!(price_for("deepseek", "deepseek-newthing"), FREE);
        // The rates are the PEAK (full) prices — a clockless overstatement,
        // never an understatement (see the arm's doc comment).
        let flash = price_for("deepseek", "deepseek-flash");
        assert_eq!(cost_usd(flash, 1_000_000, 1_000_000), 1.50);
    }

    #[test]
    fn cost_usd_arithmetic() {
        let sonnet = price_for("anthropic", "claude-sonnet-4-6");
        // 1000 in @ $3/M + 500 out @ $15/M = $0.003 + $0.0075 = $0.0105
        let cost = cost_usd(sonnet, 1_000, 500);
        assert!((cost - 0.0105).abs() < 1e-12, "got {cost}");
        // A free model costs exactly zero, whatever the volume.
        assert_eq!(cost_usd(FREE, u32::MAX, u32::MAX), 0.0);
        // A million of each is exactly the per-mtok rates summed.
        assert_eq!(cost_usd(sonnet, 1_000_000, 1_000_000), 18.0);
    }

    #[test]
    fn format_usd_significant_figures() {
        assert_eq!(format_usd(0.0031), "$0.0031");
        assert_eq!(format_usd(0.0124), "$0.0124");
        assert_eq!(format_usd(0.010500), "$0.0105");
        assert_eq!(format_usd(0.0), "$0");
        // Trailing decimal zeros are stripped (SF4): never $1.50 or $2.00.
        assert_eq!(format_usd(1.5), "$1.5");
        assert_eq!(format_usd(2.0), "$2");
        assert_eq!(format_usd(0.1), "$0.1");
        // Capped at 4 significant figures (SF4): never $123.46.
        assert_eq!(format_usd(12.3456), "$12.35");
        assert_eq!(format_usd(123.456), "$123.5");
        assert_eq!(format_usd(1234.56), "$1235");
        // Sub-cent costs keep enough places to be visible at all.
        assert_eq!(format_usd(0.000123), "$0.000123");
    }

    #[test]
    fn format_usd_rounding_carry_crosses_a_decade() {
        // `decimals` is derived from the magnitude of the PRE-rounded amount, but
        // the fixed-width `{:.decimals$}` format ROUNDS (it does not truncate).
        // When an amount sits just under a power of ten, that rounding carries it
        // up across the decade boundary — so the rendered figure is a clean
        // round number one order of magnitude above where its magnitude placed
        // it. Every other format test rounds *within* its decade ($12.3456 →
        // $12.35, $123.456 → $123.5, $1234.56 → $1235), so this carry path was
        // wholly unexercised. It pins two things at once: that format_usd rounds
        // rather than truncates (a truncating impl would yield $9.999 / $99.99 /
        // $0.9999), and that the trailing-zero strip then collapses the carried
        // "10.000"/"100.00"/"1.0000" to a bare integer.
        assert_eq!(format_usd(9.9999), "$10");
        assert_eq!(format_usd(99.999), "$100");
        assert_eq!(format_usd(0.99999), "$1");
        // The carry also fires from a genuine sub-unit amount that rounds up to a
        // whole dollar — the chip reads "$1", never "$0.9999" or "$1.0000".
        assert_eq!(format_usd(0.999951), "$1");
    }

    #[test]
    fn format_usd_large_amounts_render_as_plain_dollars_without_panic() {
        // For any amount >= $10,000 the magnitude is >= 4, so `3 - magnitude`
        // is NEGATIVE. The `.clamp(0, ..)` LOWER bound is the only thing that
        // keeps that from becoming a huge `usize` (via `-1 as usize`) and
        // panicking the format string on the cost-chip render path. Every other
        // test tops out at $1234.56 (magnitude 3, where `3 - 3 == 0` so the
        // lower clamp is a no-op), so this is the first case that actually
        // exercises it. Five- and six-figure costs must render as plain integer
        // dollars — no decimals, no panic.
        assert_eq!(format_usd(10_000.0), "$10000");
        assert_eq!(format_usd(12_345.0), "$12345");
        assert_eq!(format_usd(99_999.0), "$99999");
        assert_eq!(format_usd(123_456.0), "$123456");
        // A fractional five-figure amount rounds to whole dollars (0 decimals).
        assert_eq!(format_usd(12_345.6), "$12346");
    }

    /// RC2-CNCLF6: only a known rate is a price; a local provider is free;
    /// anything else is not known (never "free").
    #[test]
    fn rc2cnclf6_only_known_rates_are_prices() {
        assert_eq!(
            known_price("deepseek", "deepseek-flash"),
            Some(price_for("deepseek", "deepseek-flash"))
        );
        assert!(known_price("anthropic", "claude-sonnet-4-5").is_some());
        assert_eq!(known_price("ollama", "qwen3"), Some(FREE));
        assert_eq!(known_price("local", "anything"), Some(FREE));
        assert_eq!(known_price("deepseek", "deepseek-mystery-9"), None);
        assert_eq!(known_price("openrouter", "some-model"), None);
        assert_eq!(known_price("anthropic", "claude-unknown"), None);
        // The ledger's own pricing is unchanged: unknown is $0 there.
        assert_eq!(price_for("openrouter", "some-model"), FREE);
    }
}
