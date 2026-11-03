//! Context-window assembly — Slice 21.
//!
//! The promise: **nothing leaves the prompt silently.** When a conversation
//! outgrows a model's context window, Awili trims it *deterministically* and
//! *visibly* rather than letting the provider (or a hidden summarizer) drop
//! turns behind the user's back. This module is the pure, UI-free core of that
//! promise — every decision about what the model will and won't see lives in
//! [`assemble_context`], a function with no I/O, no clock, and no provider
//! dependency, so it can be exhaustively unit-tested.
//!
//! ## The rules (from the slice-21 spec)
//!
//! 1. **Pinned content is never trimmed.** The bound prompt template (the
//!    system message) and the most recent turn — which carries the latest user
//!    message — are always sent, even if that overflows the budget.
//! 2. **No automatic summarization.** Summaries exist only as an explicit user
//!    action; when one exists it is an ordinary [`Turn`] with [`TurnKind::Summary`]
//!    that *supersedes* the turns it replaces (see [`Turn::superseded_by`]).
//! 3. **Deterministic trim.** Turns are added newest-first until the next turn
//!    would overflow the budget; everything older than the overflow point is
//!    excluded. A turn is included whole or not at all — never split mid-turn.
//! 4. **Visibility.** The result reports exactly which turns were included and
//!    which were excluded, plus a token estimate, so the UI can render the
//!    meter and the "N earlier turns not sent" divider.

/// Per-message token overhead, on top of the text estimate. Real chat APIs
/// spend a few tokens per message on role/delimiter framing; 4 is the
/// conventional rule-of-thumb and keeps the estimate on the safe (slightly
/// high) side so we trim *before* the provider has to.
pub const MESSAGE_OVERHEAD_TOKENS: usize = 4;

/// Context window used when a model exposes no `contextWindow` metadata. A
/// deliberately conservative floor — small enough that an unknown model still
/// gets sane trimming rather than an overflow.
pub const DEFAULT_CONTEXT_WINDOW: usize = 32_768;

/// Output headroom reserved inside the window so the model has room to *reply*.
/// `usable = window - this`.
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 4_096;

/// Estimate the tokens in a span of text.
///
/// Heuristic, deliberately tokenizer-free (no model-specific dependency): ~4
/// characters per token, rounded up. This is an *estimate* — it is allowed to
/// be wrong by a little, and is biased slightly high (ceil) so we trim early
/// rather than overflow. Counts Unicode scalar values, not bytes, so multibyte
/// text isn't over-counted.
///
/// This is the *text-only* estimate, which is what the context meter shows.
/// The spec's per-message framing overhead ([`MESSAGE_OVERHEAD_TOKENS`]) is a
/// property of *sending* a message, not of the text itself, so it is applied
/// separately in [`message_tokens`] during assembly — keeping this function a
/// clean, composable primitive.
pub fn estimate_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    chars.div_ceil(4)
}

/// The token cost of sending one message: its text estimate plus the
/// per-message framing overhead.
fn message_tokens(text: &str) -> usize {
    estimate_tokens(text) + MESSAGE_OVERHEAD_TOKENS
}

/// Resolve a model's context window from optional metadata, falling back to
/// [`DEFAULT_CONTEXT_WINDOW`] when the gateway reported none. Kept free of any
/// `ModelChoice` dependency so this crate stays UI/provider-agnostic — the app
/// layer maps its picker selection to `Option<usize>`.
pub fn context_window_for(metadata_window: Option<usize>) -> usize {
    metadata_window.unwrap_or(DEFAULT_CONTEXT_WINDOW)
}

/// The token budget actually available for *input*, once output headroom is
/// reserved. Saturating: a tiny or misreported window yields 0, never a panic
/// or underflow.
pub fn usable_budget(context_window: usize, max_output_tokens: usize) -> usize {
    context_window.saturating_sub(max_output_tokens)
}

/// Whether a turn is ordinary conversation or a user-made summary standing in
/// for older turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnKind {
    /// A normal user/assistant exchange.
    Normal,
    /// A summary message that replaces the turns it supersedes.
    Summary,
}

/// One message inside a [`Turn`]. Mirrors the core [`crate::conversation::Role`]
/// so callers can build turns straight from the conversation log.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnMessage {
    pub role: crate::conversation::Role,
    pub text: String,
}

impl TurnMessage {
    pub fn new(role: crate::conversation::Role, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
        }
    }
}

/// A whole conversational turn — the unit of inclusion/exclusion. Normally a
/// user message plus the assistant reply, but a streaming turn (user only) or a
/// [`TurnKind::Summary`] turn are equally valid. A turn is included in the
/// prompt whole or not at all.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    /// Stable identifier (the app uses the user message's `ts`). Reported back
    /// in [`AssembledContext`] so the UI can map decisions to transcript rows.
    pub id: u64,
    pub kind: TurnKind,
    /// When `Some(summary_id)`, this turn has been replaced by a summary and is
    /// skipped during assembly — it is not sent, because its summary stands in
    /// for it. RC2-SVC7 (F0454): only a present `Summary` turn stands in; when
    /// that summary is itself trimmed the turn is listed as excluded. The
    /// transcript still shows it, dimmed.
    pub superseded_by: Option<u64>,
    pub messages: Vec<TurnMessage>,
}

impl Turn {
    /// A normal, live (non-superseded) turn.
    pub fn normal(id: u64, messages: Vec<TurnMessage>) -> Self {
        Self {
            id,
            kind: TurnKind::Normal,
            superseded_by: None,
            messages,
        }
    }

    /// A summary turn standing in for older turns.
    pub fn summary(id: u64, messages: Vec<TurnMessage>) -> Self {
        Self {
            id,
            kind: TurnKind::Summary,
            superseded_by: None,
            messages,
        }
    }

    /// Total token cost of sending this turn.
    fn token_cost(&self) -> usize {
        self.messages.iter().map(|m| message_tokens(&m.text)).sum()
    }
}

/// The role a [`PromptMessage`] is sent under. Adds `System` (the template) on
/// top of the conversational User/Assistant roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptRole {
    System,
    User,
    Assistant,
}

/// One message in the assembled prompt, in the order the provider receives it.
#[derive(Clone, Debug, PartialEq)]
pub struct PromptMessage {
    pub role: PromptRole,
    pub text: String,
}

/// The outcome of [`assemble_context`]: the exact messages the model will
/// receive, plus the bookkeeping the UI needs to be honest about what it cut.
#[derive(Clone, Debug, PartialEq)]
pub struct AssembledContext {
    /// Messages in send order: the system template first (when non-empty), then
    /// each included turn's messages oldest-first.
    pub messages: Vec<PromptMessage>,
    /// Turn ids that made it into the prompt, chronological (oldest first).
    pub included_turn_ids: Vec<u64>,
    /// Turn ids left out of the prompt, chronological: every live turn trimmed
    /// *for budget*, plus (RC2-SVC7, F0454) every superseded turn whose
    /// summary did not make it into the prompt. A superseded turn whose
    /// summary IS in the prompt is represented by it and not listed — so
    /// every turn is accounted for exactly once.
    pub excluded_turn_ids: Vec<u64>,
    /// Estimated token total of `messages` (template + included turns).
    pub est_tokens: usize,
}

/// Assemble the prompt that will actually be sent, trimming deterministically
/// to fit `usable_tokens`.
///
/// Algorithm:
/// 1. Drop superseded turns (a summary already stands in for them).
/// 2. Always include the system `template` and the newest live turn — these are
///    pinned and counted but never trimmed, even if together they overflow.
/// 3. Walk the remaining turns newest→oldest, adding each whole turn while it
///    fits. The first turn that would overflow, and every turn older than it,
///    is excluded (oldest-first, contiguous).
///
/// Pure: no I/O, no clock, deterministic for a given input.
pub fn assemble_context(template: &str, turns: &[Turn], usable_tokens: usize) -> AssembledContext {
    // (1) Live turns only, preserving chronological order. RC2-SVC7 (F0454):
    // a turn is skipped only when a real summary stands in for it — its
    // `superseded_by` names ANOTHER turn present here that is a
    // `TurnKind::Summary`. A turn pointing at a missing or non-summary turn
    // stays live: nothing else carries its content.
    let by_id: std::collections::HashMap<u64, &Turn> = turns.iter().map(|t| (t.id, t)).collect();
    let stand_in = |t: &Turn| -> Option<u64> {
        t.superseded_by
            .filter(|s| *s != t.id && by_id.get(s).is_some_and(|o| o.kind == TurnKind::Summary))
    };
    let live: Vec<&Turn> = turns.iter().filter(|t| stand_in(t).is_none()).collect();
    // The turns left out: a live turn not in `included`, or a superseded turn
    // whose chain of stand-ins does not reach an included turn.
    let left_out = |included: &[u64]| -> Vec<u64> {
        turns
            .iter()
            .filter(|t| {
                let mut cur = *t;
                for _ in 0..=turns.len() {
                    match stand_in(cur) {
                        None => return !included.contains(&cur.id),
                        Some(s) => cur = by_id[&s],
                    }
                }
                true // a stand-in cycle reaches nothing that is sent
            })
            .map(|t| t.id)
            .collect()
    };

    let template_tokens = if template.is_empty() {
        0
    } else {
        message_tokens(template)
    };

    // No conversation yet: just the template (if any).
    if live.is_empty() {
        let mut messages = Vec::new();
        if !template.is_empty() {
            messages.push(PromptMessage {
                role: PromptRole::System,
                text: template.to_string(),
            });
        }
        return AssembledContext {
            messages,
            included_turn_ids: Vec::new(),
            excluded_turn_ids: left_out(&[]),
            est_tokens: template_tokens,
        };
    }

    // (2) Pin the newest live turn (carries the latest user message).
    let newest_idx = live.len() - 1;
    let mut running = template_tokens + live[newest_idx].token_cost();
    // Track inclusion by index into `live`; newest is always in.
    let mut included_from = newest_idx;

    // (3) Add older turns newest→oldest until one would overflow; stop there.
    for i in (0..newest_idx).rev() {
        let cost = live[i].token_cost();
        if running + cost <= usable_tokens {
            running += cost;
            included_from = i;
        } else {
            break;
        }
    }

    // `live[included_from..]` is included; `live[..included_from]` is excluded
    // for budget. Both are already chronological.
    let included_turn_ids: Vec<u64> = live[included_from..].iter().map(|t| t.id).collect();
    let excluded_turn_ids = left_out(&included_turn_ids);

    let mut messages = Vec::new();
    if !template.is_empty() {
        messages.push(PromptMessage {
            role: PromptRole::System,
            text: template.to_string(),
        });
    }
    for turn in &live[included_from..] {
        for m in &turn.messages {
            messages.push(PromptMessage {
                role: prompt_role(m.role),
                text: m.text.clone(),
            });
        }
    }

    AssembledContext {
        messages,
        included_turn_ids,
        excluded_turn_ids,
        est_tokens: running,
    }
}

fn prompt_role(role: crate::conversation::Role) -> PromptRole {
    match role {
        crate::conversation::Role::User => PromptRole::User,
        crate::conversation::Role::Assistant => PromptRole::Assistant,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::Role;

    fn user_turn(id: u64, text: &str) -> Turn {
        Turn::normal(id, vec![TurnMessage::new(Role::User, text)])
    }

    fn pair_turn(id: u64, user: &str, assistant: &str) -> Turn {
        Turn::normal(
            id,
            vec![
                TurnMessage::new(Role::User, user),
                TurnMessage::new(Role::Assistant, assistant),
            ],
        )
    }

    #[test]
    fn estimate_tokens_monotonic() {
        // Non-decreasing as text grows, and ceil (never under-counts a partial).
        let mut last = 0;
        for n in 0..200 {
            let s = "x".repeat(n);
            let t = estimate_tokens(&s);
            assert!(t >= last, "tokens dropped at len {n}: {t} < {last}");
            last = t;
        }
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abc"), 1); // 3 chars -> ceil(3/4) = 1
        assert_eq!(estimate_tokens("abcd"), 1); // 4 chars -> 1
        assert_eq!(estimate_tokens("abcde"), 2); // 5 chars -> ceil(5/4) = 2
                                                 // Multibyte counts scalar values, not bytes.
        assert_eq!(estimate_tokens("😀😀😀😀"), 1);
    }

    #[test]
    fn usable_budget_reserves_headroom() {
        assert_eq!(usable_budget(128_000, 4_096), 123_904);
        assert!(usable_budget(128_000, DEFAULT_MAX_OUTPUT_TOKENS) < 128_000);
        // Never underflows when headroom exceeds the window.
        assert_eq!(usable_budget(1_000, 4_096), 0);
    }

    #[test]
    fn fallback_window_when_no_metadata() {
        assert_eq!(context_window_for(None), DEFAULT_CONTEXT_WINDOW);
        assert_eq!(context_window_for(Some(200_000)), 200_000);
    }

    #[test]
    fn assemble_pins_template_and_last_user() {
        // Zero budget must still send the template and the latest user message.
        let turns = vec![
            pair_turn(1, "old question", "old answer"),
            user_turn(2, "the live question"),
        ];
        let ctx = assemble_context("SYSTEM RULES", &turns, 0);

        assert_eq!(ctx.messages[0].role, PromptRole::System);
        assert_eq!(ctx.messages[0].text, "SYSTEM RULES");
        // Latest user message is present despite the 0 budget...
        assert!(ctx.messages.iter().any(|m| m.text == "the live question"));
        assert_eq!(ctx.included_turn_ids, vec![2]);
        // ...and the older turn was excluded (visibly, not silently).
        assert_eq!(ctx.excluded_turn_ids, vec![1]);
    }

    #[test]
    fn assemble_excludes_oldest_first() {
        // Five same-size turns; a budget that fits the template + ~3 turns.
        let turns: Vec<Turn> = (1..=5).map(|i| user_turn(i, "0123456789012345")).collect();
        // Each turn: ceil(16/4)=4 + overhead 4 = 8 tokens. Template "sys" = 1+4 = 5.
        // Budget for template + 3 turns = 5 + 24 = 29; deny a 4th.
        let ctx = assemble_context("sys", &turns, 29);

        assert_eq!(ctx.included_turn_ids, vec![3, 4, 5]); // newest three
        assert_eq!(ctx.excluded_turn_ids, vec![1, 2]); // oldest two, contiguous
        assert_eq!(ctx.est_tokens, 5 + 24);
    }

    #[test]
    fn assemble_never_splits_turn() {
        // A budget that could fit ONE message of a two-message turn but not the
        // whole turn must exclude the entire turn — no half-turns.
        let turns = vec![
            pair_turn(1, "AAAAAAAA", "BBBBBBBB"), // 2 msgs, 8 tokens each = 16
            user_turn(2, "live"),                 // ceil(4/4)=1 + 4 = 5
        ];
        // Template none. Pinned newest = 5. Give room for one extra message (8)
        // but not the whole 16-token turn 1.
        let ctx = assemble_context("", &turns, 5 + 8);

        assert_eq!(ctx.included_turn_ids, vec![2]);
        assert_eq!(ctx.excluded_turn_ids, vec![1]);
        // Neither half of turn 1 leaked in.
        assert!(!ctx.messages.iter().any(|m| m.text == "AAAAAAAA"));
        assert!(!ctx.messages.iter().any(|m| m.text == "BBBBBBBB"));
    }

    #[test]
    fn assemble_includes_summary_skips_superseded() {
        // t1, t2 superseded by a summary turn (id 99); t3 live and newest.
        let mut t1 = pair_turn(1, "ancient one", "reply one");
        let mut t2 = pair_turn(2, "ancient two", "reply two");
        t1.superseded_by = Some(99);
        t2.superseded_by = Some(99);
        let summary = Turn::summary(
            99,
            vec![TurnMessage::new(Role::Assistant, "SUMMARY of 1-2")],
        );
        let t3 = user_turn(3, "current question");
        let turns = vec![t1, t2, summary, t3];

        let ctx = assemble_context("sys", &turns, 10_000);

        // Summary stands in; superseded originals are gone from the prompt.
        assert!(ctx.messages.iter().any(|m| m.text == "SUMMARY of 1-2"));
        assert!(!ctx.messages.iter().any(|m| m.text == "ancient one"));
        assert!(!ctx.messages.iter().any(|m| m.text == "ancient two"));
        // The summary and the live turn are the included ids; superseded turns
        // appear in NEITHER list (represented by the summary, not "excluded").
        assert_eq!(ctx.included_turn_ids, vec![99, 3]);
        assert!(ctx.excluded_turn_ids.is_empty());
    }

    /// RC2-SVC7 (F0454): a superseded turn never vanishes from both lists.
    /// When its summary is trimmed for budget it is listed as excluded; when
    /// its summary is missing (or is not a summary) it stays live and is sent.
    #[test]
    fn rc2svc7_superseded_turns_are_always_accounted_for() {
        // (a) The summary itself is trimmed: t1 and t2 are excluded with it.
        let mut t1 = pair_turn(1, "ancient one", "reply one");
        let mut t2 = pair_turn(2, "ancient two", "reply two");
        t1.superseded_by = Some(99);
        t2.superseded_by = Some(99);
        let summary = Turn::summary(
            99,
            vec![TurnMessage::new(Role::Assistant, "S".repeat(4_000))],
        );
        let t3 = user_turn(3, "current question");
        let turns = vec![t1.clone(), t2.clone(), summary, t3.clone()];
        let ctx = assemble_context("", &turns, 50);
        assert_eq!(ctx.included_turn_ids, vec![3]);
        assert_eq!(ctx.excluded_turn_ids, vec![1, 2, 99]);

        // (b) The summary is missing: nothing stands in, so t1 is sent.
        let mut orphan = pair_turn(1, "orphaned", "still here");
        orphan.superseded_by = Some(77);
        let ctx = assemble_context("", &[orphan, t3.clone()], 10_000);
        assert_eq!(ctx.included_turn_ids, vec![1, 3]);
        assert!(ctx.messages.iter().any(|m| m.text == "orphaned"));

        // (c) A non-summary target does not stand in either.
        let mut pointed = pair_turn(1, "pointed", "at a normal turn");
        pointed.superseded_by = Some(3);
        let ctx = assemble_context("", &[pointed, t3], 10_000);
        assert_eq!(ctx.included_turn_ids, vec![1, 3]);
    }

    #[test]
    fn assemble_empty_conversation_is_just_template() {
        let ctx = assemble_context("only system", &[], 1000);
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.messages[0].role, PromptRole::System);
        assert!(ctx.included_turn_ids.is_empty());
        assert!(ctx.excluded_turn_ids.is_empty());
        assert_eq!(ctx.est_tokens, message_tokens("only system"));
    }

    #[test]
    fn assemble_all_fit_includes_everything() {
        let turns: Vec<Turn> = (1..=4).map(|i| user_turn(i, "short")).collect();
        let ctx = assemble_context("sys", &turns, 10_000);
        assert_eq!(ctx.included_turn_ids, vec![1, 2, 3, 4]);
        assert!(ctx.excluded_turn_ids.is_empty());
    }

    #[test]
    fn assemble_emits_messages_in_chronological_send_order() {
        // AssembledContext send-order contract (this module's doc): the system
        // template first, then each included turn's messages oldest-first, and
        // within a turn user-before-assistant. Every OTHER test checks only
        // `included_turn_ids` (built from a SEPARATE, un-reversed iterator) or
        // probes message text with order-independent `any()`. So reversing the
        // turn-emission loop (`live[included_from..].rev()`) would feed the model
        // the conversation BACKWARDS — and reversing a turn's own messages would
        // put the assistant reply before its user prompt — while the whole suite
        // still passed. Pin the exact (role, text) sequence so either reversal
        // fails here and only here.
        let turns = vec![
            pair_turn(1, "first user", "first assistant"),
            pair_turn(2, "second user", "second assistant"),
        ];
        let ctx = assemble_context("SYS", &turns, 10_000);

        let seq: Vec<(PromptRole, &str)> = ctx
            .messages
            .iter()
            .map(|m| (m.role, m.text.as_str()))
            .collect();
        assert_eq!(
            seq,
            vec![
                (PromptRole::System, "SYS"),
                (PromptRole::User, "first user"),
                (PromptRole::Assistant, "first assistant"),
                (PromptRole::User, "second user"),
                (PromptRole::Assistant, "second assistant"),
            ],
        );
    }

    #[test]
    fn assemble_overflow_reports_true_over_budget_tokens() {
        // Visibility promise (slice-21): when the pinned template + newest turn
        // overflow the budget, est_tokens reports the REAL total — it is NOT
        // capped to `usable_tokens`. This over-budget number is what drives the
        // >100% red context meter; silently clamping it to the budget would hide
        // the overflow, the exact "content dropped behind your back" behavior
        // this module exists to prevent. The all-fit est_tokens tests never
        // exercise the running > budget path, so this contract stands alone.
        let turns = vec![
            pair_turn(1, "older question", "older answer"), // excluded by budget
            user_turn(2, "the pinned live question"),       // pinned, overflows
        ];
        // Budget 0: nothing "fits", but the template and newest turn are pinned
        // and must still be counted at their true cost.
        let ctx = assemble_context("SYS", &turns, 0);

        let expected = message_tokens("SYS") + message_tokens("the pinned live question");
        assert_eq!(ctx.est_tokens, expected);
        // The whole point: the reported total EXCEEDS the (zero) budget, visibly.
        assert!(
            ctx.est_tokens > 0,
            "over-budget total must not be capped to the budget"
        );
        // The excluded older turn contributed nothing to the count.
        assert_eq!(ctx.included_turn_ids, vec![2]);
        assert_eq!(ctx.excluded_turn_ids, vec![1]);
    }
}
