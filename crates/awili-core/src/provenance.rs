//! Provenance — the record of *what actually ran and why* for one assistant
//! turn (slice-5 spec §C). The brief's recurring promise: "Model badge shows what
//! actually ran and why." This is the data behind that badge and behind the
//! persisted-turn audit trail.
//!
//! `awili-core` is dependency-free on purpose, so this struct stores plain owned
//! values (strings, numbers) rather than referencing the `awili-ccd` /
//! `awili-providers` types. The app constructs a [`Provenance`] from those
//! crates' richer types via the small constructors here.

/// Whether the turn ran on the LAN (Local) or went to a cloud provider (Cloud).
/// `Unknown` is for a helper whose inference location Awili cannot see (it
/// only knows the configuration it requested): never claimed either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locality {
    Local,
    Cloud,
    Unknown,
}

impl Locality {
    pub fn as_str(self) -> &'static str {
        match self {
            Locality::Local => "local",
            Locality::Cloud => "cloud",
            Locality::Unknown => "location unknown",
        }
    }
}

/// Token accounting carried into provenance (mirrors `awili_providers::Usage`,
/// duplicated here to keep `awili-core` dependency-free).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Cached input is a subset of input_tokens; None means unreported.
    pub cached_input_tokens: Option<u32>,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// The ccd job a turn dispatched, if any — id + a human state label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcdRef {
    pub id: String,
    pub state: String,
}

/// Everything the provenance badge shows for one assistant turn.
#[derive(Clone, Debug, PartialEq)]
pub struct Provenance {
    /// The route the user picked: "private" | "balanced" | "frontier".
    pub route: String,
    /// The provider name that actually served the turn.
    pub provider: String,
    /// The concrete model that ran.
    pub model: String,
    /// Local or cloud.
    pub locality: Locality,
    /// Short human reason ("local fleet", "cloud fallback", …).
    pub why: String,
    /// Token usage, when the provider reported it.
    pub usage: Option<Usage>,
    /// Estimated cost in USD for cloud turns; `None` for local (free) turns.
    pub est_cost_usd: Option<f64>,
    /// The ccd job this turn dispatched, if it escalated heavy work.
    pub ccd: Option<CcdRef>,
}

impl Provenance {
    /// A minimal provenance for a plain chat turn (no ccd, no cost yet).
    pub fn chat(
        route: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        locality: Locality,
        why: impl Into<String>,
    ) -> Self {
        Self {
            route: route.into(),
            provider: provider.into(),
            model: model.into(),
            locality,
            why: why.into(),
            usage: None,
            est_cost_usd: None,
            ccd: None,
        }
    }

    /// Attach token usage (and, for cloud turns, a cost estimate via `rate`).
    pub fn with_usage(mut self, usage: Usage, rate: Option<&CostRate>) -> Self {
        if let (Locality::Cloud, Some(rate)) = (self.locality, rate) {
            self.est_cost_usd = Some(rate.estimate(usage));
        }
        self.usage = Some(usage);
        self
    }

    /// Attach a dispatched ccd job reference.
    pub fn with_ccd(mut self, id: impl Into<String>, state: impl Into<String>) -> Self {
        self.ccd = Some(CcdRef {
            id: id.into(),
            state: state.into(),
        });
        self
    }

    /// A compact one-line badge string for the UI / read-aloud:
    /// `frontier · anthropic/claude-opus-4-8 · cloud · $0.0123`.
    pub fn badge(&self) -> String {
        let mut s = format!(
            "{} · {}/{} · {}",
            self.route,
            self.provider,
            self.model,
            self.locality.as_str()
        );
        if let Some(c) = self.est_cost_usd {
            s.push_str(&format!(" · ${c:.4}"));
        }
        if let Some(job) = &self.ccd {
            s.push_str(&format!(" · ccd:{} {}", job.id, job.state));
        }
        s
    }
}

/// Per-million-token pricing for a cloud model, used to estimate a turn's cost.
/// Local turns have no rate (they are free).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CostRate {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

impl CostRate {
    pub const fn new(input_per_mtok: f64, output_per_mtok: f64) -> Self {
        Self {
            input_per_mtok,
            output_per_mtok,
        }
    }

    /// Dollar estimate for a usage tally at this rate.
    pub fn estimate(&self, usage: Usage) -> f64 {
        (usage.input_tokens as f64 / 1_000_000.0) * self.input_per_mtok
            + (usage.output_tokens as f64 / 1_000_000.0) * self.output_per_mtok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_turn_has_no_cost_even_with_usage() {
        let p = Provenance::chat(
            "private",
            "ollama",
            "llama3.1:8b",
            Locality::Local,
            "local fleet",
        )
        .with_usage(
            Usage {
                cached_input_tokens: None,
                input_tokens: 1000,
                output_tokens: 2000,
            },
            None,
        );
        assert_eq!(p.est_cost_usd, None);
        assert!(p
            .badge()
            .starts_with("private · ollama/llama3.1:8b · local"));
    }

    #[test]
    fn cloud_turn_estimates_cost() {
        // Opus-class rate: $15/Mtok in, $75/Mtok out (illustrative).
        let rate = CostRate::new(15.0, 75.0);
        let p = Provenance::chat(
            "frontier",
            "anthropic",
            "claude-opus-4-8",
            Locality::Cloud,
            "frontier cloud",
        )
        .with_usage(
            Usage {
                cached_input_tokens: None,
                input_tokens: 1_000,
                output_tokens: 1_000,
            },
            Some(&rate),
        );
        // 1k in * 15/M + 1k out * 75/M = 0.015 + 0.075 = 0.09.
        let c = p.est_cost_usd.unwrap();
        assert!((c - 0.09).abs() < 1e-9, "got {c}");
        assert!(p.badge().contains("$0.0900"));
    }

    #[test]
    fn local_turn_with_rate_supplied_stays_free() {
        // The money-path guarantee behind the "what actually ran and why" badge:
        // a LOCAL (LAN/fleet) turn is free, full stop — even if a cloud CostRate
        // is handed to `with_usage` (e.g. a shared code path that always passes
        // the configured rate, or a future refactor that forwards one). The
        // `Locality::Cloud` guard, not the caller passing `None`, is what keeps a
        // local turn from showing an invented cost. Every other test either runs
        // Cloud or passes `None` for a Local turn, so dropping that guard (matching
        // only `Some(rate)`) survives the whole suite — this is the sole test that
        // pins it.
        let rate = CostRate::new(15.0, 75.0);
        let p = Provenance::chat(
            "private",
            "ollama",
            "llama3.1:8b",
            Locality::Local,
            "local fleet",
        )
        .with_usage(
            Usage {
                cached_input_tokens: None,
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
            },
            Some(&rate),
        );
        // A rate that would price a cloud turn at $90 must not attach any cost here.
        assert_eq!(
            p.est_cost_usd, None,
            "a local turn must be free even when a rate is supplied"
        );
        // Usage is still recorded (the tokens are real); only the cost is suppressed.
        assert_eq!(
            p.usage,
            Some(Usage {
                cached_input_tokens: None,
                input_tokens: 1_000_000,
                output_tokens: 1_000_000
            })
        );
        // The badge shows locality but no cost segment.
        let badge = p.badge();
        assert!(
            badge.starts_with("private · ollama/llama3.1:8b · local"),
            "got {badge}"
        );
        assert!(
            !badge.contains('$'),
            "local badge must carry no cost segment: {badge}"
        );
    }

    #[test]
    fn estimate_applies_input_rate_to_input_and_output_rate_to_output() {
        // The money-path core: `estimate` must multiply the INPUT token count by
        // the INPUT per-Mtok rate and the OUTPUT count by the OUTPUT rate. Every
        // other cost test uses symmetric token counts (1000 in / 1000 out), so
        // swapping the two rate multiplications in `estimate` yields the identical
        // sum and survives the whole suite. Asymmetric counts with asymmetric rates
        // are the only thing that pins the input↔output rate assignment.
        let rate = CostRate::new(15.0, 75.0);
        let usage = Usage {
            cached_input_tokens: None,
            input_tokens: 2_000,
            output_tokens: 1_000,
        };
        // Correct: 2k in * 15/M + 1k out * 75/M = 0.030 + 0.075 = 0.105.
        // Rate-swapped: 2k * 75/M + 1k * 15/M = 0.150 + 0.015 = 0.165 (would fail).
        let c = rate.estimate(usage);
        assert!((c - 0.105).abs() < 1e-9, "got {c}");
    }

    #[test]
    fn cloud_without_rate_has_no_cost() {
        let p = Provenance::chat(
            "frontier",
            "anthropic",
            "claude-opus-4-8",
            Locality::Cloud,
            "frontier",
        )
        .with_usage(
            Usage {
                cached_input_tokens: None,
                input_tokens: 10,
                output_tokens: 10,
            },
            None,
        );
        assert_eq!(p.est_cost_usd, None);
    }

    #[test]
    fn ccd_ref_shows_in_badge() {
        let p = Provenance::chat(
            "balanced",
            "ollama",
            "qwen2.5:14b",
            Locality::Local,
            "local",
        )
        .with_ccd("ccd-7f3a9c", "running");
        assert!(p.badge().contains("ccd:ccd-7f3a9c running"));
    }

    #[test]
    fn badge_orders_cost_before_ccd_when_both_present() {
        // A cloud turn that also escalated to a ccd job carries BOTH trailing
        // segments. `badge()` appends the cost segment first, then the ccd
        // segment, so the guaranteed shape is:
        //   route · provider/model · locality · $cost · ccd:id state
        // Every other test exercises at most one trailing segment, so swapping
        // the two `if let` blocks in `badge()` survives the whole suite — this
        // is the sole test pinning that cost comes before ccd when both exist.
        let rate = CostRate::new(15.0, 75.0);
        let p = Provenance::chat(
            "frontier",
            "anthropic",
            "claude-opus-4-8",
            Locality::Cloud,
            "frontier cloud",
        )
        .with_usage(
            Usage {
                cached_input_tokens: None,
                input_tokens: 1_000,
                output_tokens: 1_000,
            },
            Some(&rate),
        )
        .with_ccd("ccd-42", "verified");
        let badge = p.badge();
        // Both segments are present…
        assert!(badge.contains("$0.0900"), "cost segment missing: {badge}");
        assert!(
            badge.contains("ccd:ccd-42 verified"),
            "ccd segment missing: {badge}"
        );
        // …and the cost segment precedes the ccd segment.
        let cost_at = badge.find("$0.0900").unwrap();
        let ccd_at = badge.find("ccd:ccd-42").unwrap();
        assert!(
            cost_at < ccd_at,
            "cost must precede ccd in the badge: {badge}"
        );
        // Full pinned shape, end to end.
        assert_eq!(
            badge,
            "frontier · anthropic/claude-opus-4-8 · cloud · $0.0900 · ccd:ccd-42 verified"
        );
    }
}
