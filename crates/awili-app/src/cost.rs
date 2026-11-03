//! The cost ticker's state (slice-8 spec §3–4): session and today accumulators,
//! priced per usage event so cycling models mid-session stays accurate.
//!
//! Pure — no GPUI, no clock (timestamps come in), no IO — so the wiring the
//! spec's §6 app tests demand ("a Usage event arrives → counters move, the chip
//! says `$`") is provable right here without a window.

use awili_core::pricing::{cost_usd, format_usd, price_for};
use awili_persist::{utc_day_start, UsageByModel};
use awili_providers::Usage;

/// Running token/cost accumulators behind the status-bar chip.
pub struct CostTicker {
    /// Tokens consumed this SESSION (spec §3) — reset on a new conversation.
    pub session_input_tokens: u32,
    pub session_output_tokens: u32,
    /// Session cost, accumulated per event at that event's model price (a
    /// post-hoc `price × token-sums` would mis-price a model switch).
    session_usd: f64,
    /// Cost across all of today's events (UTC), seeded from the database at
    /// launch and advanced per event thereafter.
    today_usd: f64,
    /// The UTC day-start `today_usd` belongs to; an event landing in a new day
    /// rolls the daily total over to zero before accumulating, and the chip
    /// re-checks it on every read (review SF2 — midnight must not need an event).
    today_day_start: i64,
    /// Session generation (review SF1): bumped on every reset. A turn captures
    /// the generation it started under; a late `Usage` event from a turn that
    /// began before a `reset_session` must not credit the new session.
    generation: u64,
}

impl CostTicker {
    /// Build the ticker at launch: price each of today's persisted per-model
    /// rollups (read synchronously while the app still owned the `Store`) and
    /// start the session at zero.
    pub fn new(now_ts: i64, today: &[UsageByModel]) -> Self {
        let today_usd = today
            .iter()
            .map(|u| {
                cost_usd(
                    price_for(&u.provider, &u.model),
                    u.input_tokens,
                    u.output_tokens,
                )
            })
            .sum();
        Self {
            session_input_tokens: 0,
            session_output_tokens: 0,
            session_usd: 0.0,
            today_usd,
            today_day_start: utc_day_start(now_ts),
            generation: 0,
        }
    }

    /// The current session generation. A turn captures this when it starts and
    /// hands it back with each `Usage` event, so an event from a turn that
    /// predates a session reset is kept out of the new session (review SF1).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A `ProviderEvent::Usage` arrived: accumulate tokens and dollars into the
    /// session and today's totals. `gen` is the session generation the turn
    /// started under — a stale generation still counts toward today (the spend
    /// is real) but not toward the session it no longer belongs to (review
    /// SF1). Returns this event's cost in USD (what the memory-curation gate
    /// checks against zero).
    ///
    /// P9 (D.18): the only production caller (`on_usage`, the Chat HTTP stream
    /// loop) no longer runs — Chat is pinned to an agent executor. Retained per
    /// the spec; exercised by its unit tests.
    #[allow(dead_code)]
    pub fn note_usage(
        &mut self,
        provider: &str,
        model: &str,
        usage: Usage,
        now_ts: i64,
        gen: u64,
    ) -> f64 {
        let cost = cost_usd(
            price_for(provider, model),
            usage.input_tokens,
            usage.output_tokens,
        );
        if gen == self.generation {
            self.session_input_tokens =
                self.session_input_tokens.saturating_add(usage.input_tokens);
            self.session_output_tokens = self
                .session_output_tokens
                .saturating_add(usage.output_tokens);
            self.session_usd += cost;
        }
        self.add_today(cost, now_ts);
        cost
    }

    /// A background provider call's usage — e.g. turn distillation (slice 10
    /// review SF3) — is real spend that must reach today's total, but never
    /// the session ticker: the session number tracks the conversation the
    /// user is paying attention to. Returns the event's cost in USD.
    pub fn note_background_usage(
        &mut self,
        provider: &str,
        model: &str,
        usage: Usage,
        now_ts: i64,
    ) -> f64 {
        let cost = cost_usd(
            price_for(provider, model),
            usage.input_tokens,
            usage.output_tokens,
        );
        self.add_today(cost, now_ts);
        cost
    }

    /// A provider-reported dollar cost arrived directly on the wire — for
    /// backends like Hermes that send `cost_usd` rather than leaving Awili to
    /// price tokens (Awili has no Hermes pricing table, so token-pricing would
    /// read $0). Counts like [`note_usage`](Self::note_usage): toward the session
    /// only when the generation still matches, always toward today. No token
    /// counts ride along, so only dollars move.
    pub fn note_reported_cost(&mut self, cost: f64, now_ts: i64, gen: u64) {
        if cost <= 0.0 {
            return;
        }
        if gen == self.generation {
            self.session_usd += cost;
        }
        self.add_today(cost, now_ts);
    }

    /// Accumulate into today's total, rolling it over first if UTC midnight
    /// passed while the app ran.
    fn add_today(&mut self, cost: f64, now_ts: i64) {
        let day = utc_day_start(now_ts);
        if day != self.today_day_start {
            self.today_day_start = day;
            self.today_usd = 0.0;
        }
        self.today_usd += cost;
    }

    /// The running session cost (what the usage memory reports).
    ///
    /// P9 (D.18): only the (now-unreachable-from-Chat) usage path read this.
    /// Retained per the spec; exercised by its unit tests.
    #[allow(dead_code)]
    pub fn session_usd(&self) -> f64 {
        self.session_usd
    }

    /// New conversation → the session counter starts over (spec §4), and the
    /// generation advances so an in-flight turn from the old session cannot
    /// repopulate the fresh one (review SF1).
    pub fn reset_session(&mut self) {
        self.session_input_tokens = 0;
        self.session_output_tokens = 0;
        self.session_usd = 0.0;
        self.generation += 1;
    }

    /// Today's spend in USD (UTC day), re-checked against the clock on every
    /// read like [`Self::chip_label`]: the strip's popover shows it for a
    /// metered cloud model.
    pub fn today_usd(&self, now_ts: i64) -> f64 {
        if utc_day_start(now_ts) == self.today_day_start {
            self.today_usd
        } else {
            0.0
        }
    }

    /// The chip text — `$0.0031 session · $0.0124 today` — or `None` when both
    /// are zero (an all-local day shows no chip at all, spec §4). Takes the
    /// render-time clock: the stored day is re-checked on every read so a
    /// quiet app crossing UTC midnight shows `$0 today` without waiting for
    /// another usage event (review SF2).
    pub fn chip_label(&self, now_ts: i64) -> Option<String> {
        let today_usd = if utc_day_start(now_ts) == self.today_day_start {
            self.today_usd
        } else {
            0.0
        };
        if self.session_usd <= 0.0 && today_usd <= 0.0 {
            return None;
        }
        Some(format!(
            "{} session · {} today",
            format_usd(self.session_usd),
            format_usd(today_usd)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOON: i64 = 1_750_000_000; // some UTC mid-day instant

    fn fresh() -> CostTicker {
        CostTicker::new(NOON, &[])
    }

    #[test]
    fn reported_cost_counts_session_and_today_with_stale_gen_rule() {
        let mut t = fresh();
        let gen = t.generation();
        t.note_reported_cost(0.05, NOON, gen);
        assert!((t.session_usd() - 0.05).abs() < 1e-12);
        assert_eq!(
            t.chip_label(NOON).as_deref(),
            Some("$0.05 session · $0.05 today")
        );

        // A stale generation (the user started a new session) still reaches
        // today but must not move the current session — same rule as note_usage.
        t.reset_session();
        t.note_reported_cost(0.03, NOON, gen);
        assert!(
            (t.session_usd() - 0.0).abs() < 1e-12,
            "stale gen must not hit session"
        );
        assert_eq!(
            t.chip_label(NOON).as_deref(),
            Some("$0 session · $0.08 today")
        );

        // Zero/negative reported costs are ignored.
        t.note_reported_cost(0.0, NOON, t.generation());
        assert_eq!(
            t.chip_label(NOON).as_deref(),
            Some("$0 session · $0.08 today")
        );
    }

    #[test]
    fn usage_event_updates_session_counters_and_chip_shows_dollars() {
        // The spec §6 integration shape: a (mock) ProviderEvent::Usage payload
        // arrives → session counters move and the chip string carries a `$`.
        let mut t = fresh();
        assert_eq!(t.chip_label(NOON), None, "all-zero ticker shows no chip");

        let cost = t.note_usage(
            "anthropic",
            "claude-sonnet-4-6",
            Usage {
                cached_input_tokens: None,
                input_tokens: 1_000,
                output_tokens: 500,
            },
            NOON,
            t.generation(),
        );
        assert_eq!(t.session_input_tokens, 1_000);
        assert_eq!(t.session_output_tokens, 500);
        assert!((cost - 0.0105).abs() < 1e-12);

        let chip = t
            .chip_label(NOON)
            .expect("paid usage must surface the chip");
        assert!(chip.contains('$'), "chip must show dollars: {chip}");
        assert_eq!(chip, "$0.0105 session · $0.0105 today");
    }

    #[test]
    fn free_local_usage_counts_tokens_but_keeps_the_chip_hidden() {
        let mut t = fresh();
        let cost = t.note_usage(
            "ollama",
            "llama3.1:8b",
            Usage {
                cached_input_tokens: None,
                input_tokens: 9_999,
                output_tokens: 9_999,
            },
            NOON,
            t.generation(),
        );
        assert_eq!(cost, 0.0);
        assert_eq!(t.session_input_tokens, 9_999, "tokens still count");
        assert_eq!(
            t.chip_label(NOON),
            None,
            "an all-local session shows nothing (spec §4)"
        );
    }

    #[test]
    fn session_resets_but_today_persists_across_new_conversation() {
        let mut t = fresh();
        t.note_usage(
            "anthropic",
            "claude-haiku-4-5",
            Usage {
                cached_input_tokens: None,
                input_tokens: 10_000,
                output_tokens: 1_000,
            },
            NOON,
            t.generation(),
        );
        t.reset_session();
        assert_eq!(t.session_input_tokens, 0);
        assert_eq!(t.session_usd(), 0.0);
        // Today's spend is history, not session state — the chip survives on it.
        let chip = t
            .chip_label(NOON)
            .expect("today's total keeps the chip alive");
        assert!(chip.starts_with("$0 session · "), "chip was {chip}");
    }

    #[test]
    fn late_usage_from_a_pre_reset_turn_stays_out_of_the_new_session() {
        // Review SF1: conversation A is still streaming when the user creates
        // conversation B (reset_session). A's terminal Usage event then arrives
        // carrying the generation it STARTED under — it must count toward
        // today (the spend is real) but not toward B's fresh session.
        let mut t = fresh();
        let old_gen = t.generation();
        t.reset_session();
        let cost = t.note_usage(
            "anthropic",
            "claude-sonnet-4-6",
            Usage {
                cached_input_tokens: None,
                input_tokens: 1_000,
                output_tokens: 500,
            },
            NOON,
            old_gen,
        );
        assert!(
            (cost - 0.0105).abs() < 1e-12,
            "the event still prices normally"
        );
        assert_eq!(
            t.session_input_tokens, 0,
            "stale event must not credit the new session"
        );
        assert_eq!(t.session_usd(), 0.0);
        let chip = t
            .chip_label(NOON)
            .expect("today's total keeps the chip alive");
        assert_eq!(chip, "$0 session · $0.0105 today");

        // A turn of the CURRENT generation still accumulates normally.
        t.note_usage(
            "anthropic",
            "claude-sonnet-4-6",
            Usage {
                cached_input_tokens: None,
                input_tokens: 1_000,
                output_tokens: 500,
            },
            NOON,
            t.generation(),
        );
        assert_eq!(t.session_input_tokens, 1_000);
    }

    #[test]
    fn background_usage_reaches_today_but_never_the_session() {
        // Slice 10 review SF3: a distillation call's spend is real — it must
        // show in "today" — but the session ticker tracks only the
        // conversation the user is watching.
        let mut t = fresh();
        let cost = t.note_background_usage(
            "anthropic",
            "claude-sonnet-4-6",
            Usage {
                cached_input_tokens: None,
                input_tokens: 1_000,
                output_tokens: 500,
            },
            NOON,
        );
        assert!((cost - 0.0105).abs() < 1e-12, "the call prices normally");
        assert_eq!(t.session_input_tokens, 0, "session tokens untouched");
        assert_eq!(t.session_output_tokens, 0);
        assert_eq!(t.session_usd(), 0.0, "session dollars untouched");
        let chip = t
            .chip_label(NOON)
            .expect("background spend keeps the chip honest");
        assert_eq!(chip, "$0 session · $0.0105 today");

        // Midnight rollover applies to background spend like any other.
        let tomorrow = utc_day_start(NOON) + 86_400 + 60;
        t.note_background_usage(
            "anthropic",
            "claude-sonnet-4-6",
            Usage {
                cached_input_tokens: None,
                input_tokens: 1_000,
                output_tokens: 500,
            },
            tomorrow,
        );
        assert_eq!(
            t.chip_label(tomorrow).unwrap(),
            "$0 session · $0.0105 today"
        );
    }

    #[test]
    fn today_seeds_from_per_model_rollups_at_launch() {
        // Earlier-today usage from a previous run, mixed models — each priced
        // at its own rate: haiku 10k/1k = $0.012; opus 1k/100 = $0.0225.
        let today = vec![
            UsageByModel {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
                input_tokens: 10_000,
                output_tokens: 1_000,
            },
            UsageByModel {
                provider: "anthropic".into(),
                model: "claude-opus-4-8".into(),
                input_tokens: 1_000,
                output_tokens: 100,
            },
            // Local rows seed nothing.
            UsageByModel {
                provider: "ollama".into(),
                model: "llama3.1:8b".into(),
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
            },
        ];
        let t = CostTicker::new(NOON, &today);
        let chip = t.chip_label(NOON).unwrap();
        assert_eq!(chip, "$0 session · $0.0345 today");
    }

    #[test]
    fn utc_midnight_rolls_today_over() {
        let mut t = fresh();
        t.note_usage(
            "anthropic",
            "claude-opus-4-8",
            Usage {
                cached_input_tokens: None,
                input_tokens: 1_000_000,
                output_tokens: 0,
            },
            NOON,
            t.generation(),
        );
        // The next event lands after UTC midnight: today restarts from zero,
        // the session keeps accumulating.
        let tomorrow = utc_day_start(NOON) + 86_400 + 60;
        t.note_usage(
            "anthropic",
            "claude-haiku-4-5",
            Usage {
                cached_input_tokens: None,
                input_tokens: 1_000_000,
                output_tokens: 0,
            },
            tomorrow,
            t.generation(),
        );
        let chip = t.chip_label(tomorrow).unwrap();
        // session: $15 (opus) + $0.80 (haiku); today: just the haiku event.
        assert_eq!(chip, "$15.8 session · $0.8 today");
    }

    #[test]
    fn chip_rolls_today_at_midnight_without_a_usage_event() {
        // Review SF2: the app sits open across 00:00 UTC with NO further
        // provider event. The chip must stop calling yesterday's spend
        // "today" purely from the render-time clock.
        let mut t = fresh();
        t.note_usage(
            "anthropic",
            "claude-sonnet-4-6",
            Usage {
                cached_input_tokens: None,
                input_tokens: 1_000,
                output_tokens: 500,
            },
            NOON,
            t.generation(),
        );
        assert_eq!(
            t.chip_label(NOON).unwrap(),
            "$0.0105 session · $0.0105 today"
        );

        // One render after midnight, no event in between: today reads $0, the
        // session (which spans midnight) is untouched.
        let after_midnight = utc_day_start(NOON) + 86_400 + 1;
        assert_eq!(
            t.chip_label(after_midnight).unwrap(),
            "$0.0105 session · $0 today"
        );
    }

    #[test]
    fn chip_hides_after_midnight_when_only_today_had_spend() {
        // Yesterday's launch-seeded spend with a zero session: after midnight
        // (still no event) both displayed totals are zero → no chip at all.
        let today = vec![UsageByModel {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            input_tokens: 10_000,
            output_tokens: 1_000,
        }];
        let t = CostTicker::new(NOON, &today);
        assert!(
            t.chip_label(NOON).is_some(),
            "seeded spend shows while it is still today"
        );
        let after_midnight = utc_day_start(NOON) + 86_400 + 1;
        assert_eq!(
            t.chip_label(after_midnight),
            None,
            "yesterday's spend is not 'today'"
        );
    }

    #[test]
    fn token_counters_saturate_instead_of_wrapping() {
        let mut t = fresh();
        t.note_usage(
            "ollama",
            "x",
            Usage {
                cached_input_tokens: None,
                input_tokens: u32::MAX,
                output_tokens: u32::MAX,
            },
            NOON,
            t.generation(),
        );
        t.note_usage(
            "ollama",
            "x",
            Usage {
                cached_input_tokens: None,
                input_tokens: 1,
                output_tokens: 1,
            },
            NOON,
            t.generation(),
        );
        assert_eq!(t.session_input_tokens, u32::MAX);
        assert_eq!(t.session_output_tokens, u32::MAX);
    }
}
