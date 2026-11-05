//! Automatic provider fallback (slice 15 §2): when a turn's provider stream
//! errors with a capacity-class failure, the turn retries on the next
//! `model_choices()` entry (wrapping), skipping models of the same provider
//! kind when the error is quota-level — an exhausted Anthropic account is
//! exhausted for every Anthropic model. Pure decision logic, no GPUI: the
//! stream loop in `start_turn` asks [`FallbackPlan::on_error`] what to do and
//! performs the side effects itself, so a mock provider can drive this in a
//! unit test.
//!
//! P9 (D.18): Chat no longer streams over the `[[providers]]` HTTP path (it is
//! pinned to an agent executor), so this fallback plan is no longer driven by
//! any production code — it is retained for its unit-test suite and left in
//! place per the spec ("do not delete provider code in this slice").
#![allow(dead_code)]

use awili_config::{ModelChoice, ProviderKind};
use awili_providers::{classify_error, ProviderErrorKind};

/// What the turn loop should do after a `ProviderEvent::Error`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FallbackDecision {
    /// Retry the same turn with `model_choices()[next_idx]`.
    Retry { next_idx: usize },
    /// Retryable error, but every candidate has been tried or banned —
    /// surface "all providers failed" and stop.
    Exhausted,
    /// Not a capacity error — surface as-is, no retry (pre-slice-15 behavior).
    NoRetry,
}

/// One turn's fallback bookkeeping: which choice indices were already burned
/// and which provider kinds a quota error banned outright. A fresh plan is
/// built per turn — quota bans must not outlive the turn (the account may be
/// topped up; the next turn should try the user's selection again).
pub struct FallbackPlan {
    tried: Vec<usize>,
    banned_kinds: Vec<ProviderKind>,
}

impl FallbackPlan {
    /// `start_idx` is the choice the turn began with — already burned.
    pub fn new(start_idx: usize) -> Self {
        Self {
            tried: vec![start_idx],
            banned_kinds: Vec::new(),
        }
    }

    /// Classify `err` from `choices[current]` and pick the next candidate:
    /// the first untried index after `current` (wrapping) whose provider kind
    /// is not quota-banned.
    pub fn on_error(
        &mut self,
        choices: &[ModelChoice],
        current: usize,
        err: &str,
    ) -> FallbackDecision {
        let kind = classify_error(err);
        if kind == ProviderErrorKind::Other {
            return FallbackDecision::NoRetry;
        }
        if kind == ProviderErrorKind::Quota {
            if let Some(c) = choices.get(current) {
                if !self.banned_kinds.contains(&c.kind) {
                    self.banned_kinds.push(c.kind);
                }
            }
        }
        let len = choices.len();
        for off in 1..len.max(1) {
            let cand = (current + off) % len;
            if self.tried.contains(&cand) || self.banned_kinds.contains(&choices[cand].kind) {
                continue;
            }
            self.tried.push(cand);
            return FallbackDecision::Retry { next_idx: cand };
        }
        FallbackDecision::Exhausted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_providers::{ChatProvider, ChatRequest, ProviderEvent};
    use futures::stream::BoxStream;
    use futures::StreamExt;

    fn choice(provider: &str, kind: ProviderKind, model: &str) -> ModelChoice {
        ModelChoice {
            provider: provider.to_string(),
            kind,
            base_url: "http://example.invalid".to_string(),
            model: model.to_string(),
            api_key: None,
        }
    }

    /// A canned provider: replays a fixed event script, no network. Stands in
    /// for [`build_provider`]'s real impls in the retry test below.
    struct MockProvider {
        script: Vec<ProviderEvent>,
    }

    impl ChatProvider for MockProvider {
        fn stream(&self, _req: ChatRequest) -> BoxStream<'static, ProviderEvent> {
            futures::stream::iter(self.script.clone()).boxed()
        }
    }

    /// Drain a provider stream exactly the way `start_turn`'s loop does:
    /// accumulate tokens, capture the last error, stop on `Done`.
    fn drain(provider: &dyn ChatProvider) -> (String, Option<String>) {
        let req = ChatRequest {
            model: "m".into(),
            system: None,
            messages: vec![],
        };
        let mut stream = provider.stream(req);
        futures::executor::block_on(async {
            let mut text = String::new();
            let mut error = None;
            while let Some(ev) = stream.next().await {
                match ev {
                    ProviderEvent::HostApproval(_)
                    | ProviderEvent::Activity(_)
                    | ProviderEvent::ToolStarted(_)
                    | ProviderEvent::ToolAttemptFailed => {}
                    ProviderEvent::Token(t) => text.push_str(&t),
                    ProviderEvent::Error(e) => error = Some(e),
                    ProviderEvent::Usage(_) => {}
                    ProviderEvent::ContextWindow(_) => {}
                    ProviderEvent::Done => break,
                }
            }
            (text, error)
        })
    }

    /// Spec §4: choices [A, B, C], A's stream errors Overloaded → the turn
    /// retries with B and streams its reply to completion.
    #[test]
    fn overloaded_error_retries_with_next_choice() {
        let choices = vec![
            choice("a", ProviderKind::Anthropic, "model-a"),
            choice("b", ProviderKind::Ollama, "model-b"),
            choice("c", ProviderKind::Hermes, "model-c"),
        ];
        let providers = [
            MockProvider {
                script: vec![
                    ProviderEvent::Error("overloaded_error (529)".into()),
                    ProviderEvent::Done,
                ],
            },
            MockProvider {
                script: vec![
                    ProviderEvent::Token("Hel".into()),
                    ProviderEvent::Token("lo".into()),
                    ProviderEvent::Done,
                ],
            },
            MockProvider {
                script: vec![ProviderEvent::Done],
            },
        ];

        let mut plan = FallbackPlan::new(0);
        let (text, error) = drain(&providers[0]);
        assert_eq!(text, "");
        let err = error.expect("A must error");
        let decision = plan.on_error(&choices, 0, &err);
        assert_eq!(
            decision,
            FallbackDecision::Retry { next_idx: 1 },
            "A's overload retries B"
        );

        let (text, error) = drain(&providers[1]);
        assert_eq!(error, None);
        assert_eq!(text, "Hello", "the retry streams B's reply");
    }

    /// Spec §4: [anthropic-1, anthropic-2, ollama-1] and anthropic-1 hits a
    /// quota error → the whole anthropic account is out, so the fallback
    /// skips anthropic-2 and lands on ollama-1.
    #[test]
    fn quota_error_skips_same_provider_kind() {
        let choices = vec![
            choice("anthropic", ProviderKind::Anthropic, "claude-opus-4-8"),
            choice("anthropic", ProviderKind::Anthropic, "claude-sonnet-4-6"),
            choice("ollama", ProviderKind::Ollama, "llama3.1:8b"),
        ];
        let mut plan = FallbackPlan::new(0);
        assert_eq!(
            plan.on_error(&choices, 0, "quota exceeded for this billing period"),
            FallbackDecision::Retry { next_idx: 2 },
        );
    }

    /// The complement of `quota_error_skips_same_provider_kind` and the
    /// under-tested half of the `lib.rs` contract ("Overloaded/RateLimit retry
    /// on the next model of any provider" — including a same-kind sibling).
    /// A transient capacity error is model-specific, not account-wide, so on
    /// [anthropic-1, anthropic-2, ollama] an overload of anthropic-1 must land
    /// on anthropic-2 (the sibling), NOT skip past all Anthropic to ollama the
    /// way quota does. Both existing transient tests
    /// (`overloaded_error_retries_with_next_choice`, `rate_limit_rotates_...`)
    /// use lists whose next entry is a *different* kind, so a regression that
    /// widened the `if kind == Quota` ban to also fire on Overloaded/RateLimit
    /// would pass every one of them while silently downgrading a momentary 529
    /// on the user's chosen provider straight to a local model. `next_idx == 1`
    /// (vs quota's `2` on the same list) is what catches that.
    #[test]
    fn transient_error_retries_same_provider_kind_sibling() {
        let choices = vec![
            choice("anthropic", ProviderKind::Anthropic, "claude-opus-4-8"),
            choice("anthropic", ProviderKind::Anthropic, "claude-sonnet-4-6"),
            choice("ollama", ProviderKind::Ollama, "llama3.1:8b"),
        ];
        // Overloaded (529): retries the Anthropic sibling, not the local model.
        let mut plan = FallbackPlan::new(0);
        assert_eq!(
            plan.on_error(&choices, 0, "overloaded_error (529)"),
            FallbackDecision::Retry { next_idx: 1 },
            "a transient overload retries the same-kind sibling (index 1), unlike quota",
        );
        // RateLimit (429) is the other transient class — same non-banning rule.
        let mut plan = FallbackPlan::new(0);
        assert_eq!(
            plan.on_error(&choices, 0, "rate_limit_error (429)"),
            FallbackDecision::Retry { next_idx: 1 },
            "a rate-limit likewise retries the same-kind sibling, not the local fallback",
        );
    }

    /// A rate-limit error rotates to ANY other provider's model — and a later
    /// error there keeps rotating without revisiting a burned choice.
    #[test]
    fn rate_limit_rotates_and_never_revisits() {
        let choices = vec![
            choice("a", ProviderKind::Anthropic, "m-a"),
            choice("b", ProviderKind::Ollama, "m-b"),
            choice("c", ProviderKind::Hermes, "m-c"),
        ];
        let mut plan = FallbackPlan::new(0);
        assert_eq!(
            plan.on_error(&choices, 0, "rate_limit_error (429)"),
            FallbackDecision::Retry { next_idx: 1 },
        );
        assert_eq!(
            plan.on_error(&choices, 1, "overloaded"),
            FallbackDecision::Retry { next_idx: 2 },
        );
        // Every choice burned: the wrap must not loop back to 0.
        assert_eq!(
            plan.on_error(&choices, 2, "overloaded"),
            FallbackDecision::Exhausted
        );
    }

    /// The user's selection need not be index 0: starting mid-list, the
    /// fallback wraps past the end to an earlier untried choice. Here the
    /// turn begins on anthropic-1 (index 1) and hits quota → Anthropic is
    /// banned, so anthropic-2 (index 2) is skipped and the wrap lands on
    /// ollama-0 (index 0). Guards the `(current + off) % len` wrap together
    /// with the quota ban skipping a later same-kind entry.
    #[test]
    fn quota_wraps_past_end_to_earlier_choice() {
        let choices = vec![
            choice("ollama", ProviderKind::Ollama, "llama3.1:8b"),
            choice("anthropic", ProviderKind::Anthropic, "claude-opus-4-8"),
            choice("anthropic", ProviderKind::Anthropic, "claude-sonnet-4-6"),
        ];
        let mut plan = FallbackPlan::new(1);
        assert_eq!(
            plan.on_error(&choices, 1, "quota exceeded for this billing period"),
            FallbackDecision::Retry { next_idx: 0 },
            "quota bans Anthropic (skips index 2) and wraps to ollama at index 0",
        );
    }

    /// Quota bans accumulate across retries within a turn: two sequential quota
    /// errors from *different* provider kinds must both stay banned, so a later
    /// rotation skips a same-kind sibling of an EARLIER banned provider — not
    /// just the current one. This is the real multi-cloud-exhaustion money-path:
    /// [anthropic-1, openai, anthropic-2, ollama], the user's Anthropic pick
    /// quota-exhausts (→ openai), then openai also quota-exhausts; the fallback
    /// must skip anthropic-2 (Anthropic was banned in the FIRST call, and the
    /// whole account is still out) and land on the local ollama.
    ///
    /// Every other quota test fires a single `on_error`, and the only multi-call
    /// test (`rate_limit_rotates_and_never_revisits`) uses transient errors that
    /// never populate `banned_kinds` — so cross-call ban persistence is otherwise
    /// unpinned. A regression that scoped the ban to only the current call (e.g.
    /// clearing `banned_kinds` per `on_error`, or banning only `choices[current]`
    /// for that one rotation) would land call 2 on anthropic-2 (index 2) and
    /// still pass all seven existing tests. `next_idx == 3` is what catches it.
    #[test]
    fn quota_bans_accumulate_across_retries_within_a_turn() {
        let choices = vec![
            choice("anthropic", ProviderKind::Anthropic, "claude-opus-4-8"),
            choice("openai", ProviderKind::OpenAi, "gpt-5"),
            choice("anthropic", ProviderKind::Anthropic, "claude-sonnet-4-6"),
            choice("ollama", ProviderKind::Ollama, "llama3.1:8b"),
        ];
        let mut plan = FallbackPlan::new(0);
        // Call 1: Anthropic quota-exhausts → ban Anthropic, rotate to openai (1).
        assert_eq!(
            plan.on_error(&choices, 0, "quota exceeded for this billing period"),
            FallbackDecision::Retry { next_idx: 1 },
            "first quota bans Anthropic and falls to the other cloud (openai)",
        );
        // Call 2: openai also quota-exhausts → ban OpenAi; the Anthropic ban from
        // call 1 must still suppress anthropic-2 (index 2), landing on ollama (3).
        assert_eq!(
            plan.on_error(&choices, 1, "credit balance is too low"),
            FallbackDecision::Retry { next_idx: 3 },
            "the earlier Anthropic ban persists, so index 2 is skipped for the local model",
        );
    }

    /// The money-path give-up: an all-Anthropic config whose account
    /// quota-exhausts must land on [`FallbackDecision::Exhausted`] (→ main.rs's
    /// "all providers failed" stop), NOT keep hammering sibling Anthropic models
    /// that share the same dead account. Quota bans the kind, so every remaining
    /// candidate is skipped and the loop falls through to `Exhausted`.
    ///
    /// This is the ONLY test where a *quota ban* — not merely a burned/`tried`
    /// list — is what forces `Exhausted`: every other `Exhausted` case
    /// (`rate_limit_rotates_and_never_revisits`, `single_choice_is_exhausted...`)
    /// uses transient errors that never populate `banned_kinds`. So a plausible
    /// "last resort: if the ban-aware loop found nothing, retry the first untried
    /// choice anyway" regression would return `Retry { next_idx: 1 }` here (a
    /// doomed request to a sibling on the exhausted account) while passing all
    /// eight existing tests — the quota tests with a different-kind entry
    /// (`quota_error_skips_same_provider_kind`, `quota_wraps_past_end_...`) find
    /// that entry before any last-resort branch is reached, so they never
    /// exercise it. Asserting `Exhausted` on an all-same-kind list is what catches it.
    #[test]
    fn quota_on_all_same_kind_config_is_exhausted_not_sibling_retry() {
        let choices = vec![
            choice("anthropic", ProviderKind::Anthropic, "claude-opus-4-8"),
            choice("anthropic", ProviderKind::Anthropic, "claude-sonnet-4-6"),
            choice("anthropic", ProviderKind::Anthropic, "claude-haiku-4-5"),
        ];
        let mut plan = FallbackPlan::new(0);
        assert_eq!(
            plan.on_error(&choices, 0, "credit balance is too low"),
            FallbackDecision::Exhausted,
            "an exhausted Anthropic account gives up cleanly instead of retrying its own siblings",
        );
    }

    /// A single configured choice has nowhere to fall back to.
    #[test]
    fn single_choice_is_exhausted_immediately() {
        let choices = vec![choice("a", ProviderKind::Anthropic, "m-a")];
        let mut plan = FallbackPlan::new(0);
        assert_eq!(
            plan.on_error(&choices, 0, "overloaded"),
            FallbackDecision::Exhausted
        );
    }

    /// A non-capacity error (bad key, network) keeps the pre-slice-15
    /// behavior: surface it, no retry.
    #[test]
    fn other_errors_do_not_retry() {
        let choices = vec![
            choice("a", ProviderKind::Anthropic, "m-a"),
            choice("b", ProviderKind::Ollama, "m-b"),
        ];
        let mut plan = FallbackPlan::new(0);
        assert_eq!(
            plan.on_error(
                &choices,
                0,
                "no Anthropic API key (set AWILI_ANTHROPIC_KEY)"
            ),
            FallbackDecision::NoRetry,
        );
    }
}
