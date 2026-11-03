//! rc.3 S2: what went into one reply's prompt, part by part.
//!
//! At assembly time Awili records each part it put in front of the model
//! (its instructions, each rules file, the memory block, the conversation so
//! far, files you attached, your message) with its size. Awili's own sizes
//! are estimates (characters / 4) and are shown as "about". For a helper that
//! runs its own CLI (Claude, Codex, GLM), whatever the provider counted
//! beyond Awili's parts is the CLI's own instructions and tools.
//!
//! Saved like the reply footer: `work_contexts:<conv_id>` in `app_state`,
//! keyed by [`crate::run_footer::footer_key`] of the reply, so it survives a
//! relaunch.

use gpui::SharedString;

/// What kind of thing a part is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PartCategory {
    Instructions,
    Rules,
    Memory,
    History,
    Attachments,
    Message,
    Tools,
    Provider,
}

/// One part of the prompt.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ContextPart {
    pub(crate) label: String,
    pub(crate) category: PartCategory,
    pub(crate) chars: usize,
    /// About `chars / 4` for Awili's parts; the provider's own count for the
    /// CLI remainder.
    pub(crate) tokens: u32,
    /// True when `tokens` is Awili's estimate.
    pub(crate) estimated: bool,
}

impl ContextPart {
    /// Awili's own part, sized by characters (tokens = chars / 4, rounded up).
    pub(crate) fn estimate(label: impl Into<String>, category: PartCategory, chars: usize) -> Self {
        Self {
            label: label.into(),
            category,
            chars,
            tokens: u32::try_from(chars.div_ceil(4)).unwrap_or(u32::MAX),
            estimated: true,
        }
    }
}

/// All the parts of one turn's prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ContextBreakdown {
    pub(crate) parts: Vec<ContextPart>,
    /// For a CLI helper: its name ("Claude"), so the remainder can be named.
    #[serde(default)]
    pub(crate) cli: Option<String>,
    /// The model's context window, when known.
    #[serde(default)]
    pub(crate) limit: Option<u32>,
    /// True when `limit` is the provider's own report for this turn, false when
    /// it is the table's fallback (shown as "about").
    #[serde(default)]
    pub(crate) limit_reported: bool,
    /// True when the provider's input figure is summed across several steps.
    #[serde(default)]
    pub(crate) summed_steps: bool,
    /// Input tokens the provider reported for the turn, when it did.
    #[serde(default)]
    pub(crate) provider_input: Option<u32>,
}

impl ContextBreakdown {
    pub(crate) fn push(&mut self, part: ContextPart) {
        if part.chars > 0 || part.tokens > 0 {
            self.parts.push(part);
        }
    }

    /// Awili's own estimated tokens (no provider remainder).
    pub(crate) fn awili_tokens(&self) -> u32 {
        self.parts
            .iter()
            .filter(|p| p.category != PartCategory::Provider)
            .fold(0u32, |a, p| a.saturating_add(p.tokens))
    }

    /// Close the turn: record the provider's input count and, for a CLI,
    /// add the remainder part (provider input minus Awili's parts, never
    /// below zero).
    pub(crate) fn settle(
        &mut self,
        provider_input: Option<u32>,
        window: Option<(u32, bool)>,
        summed_steps: bool,
    ) {
        self.limit = window.map(|w| w.0);
        self.limit_reported = window.is_some_and(|w| w.1);
        self.summed_steps = summed_steps;
        self.provider_input = provider_input;
        self.parts.retain(|p| p.category != PartCategory::Provider);
        if let (Some(cli), Some(input)) = (self.cli.clone(), provider_input) {
            let rest = input.saturating_sub(self.awili_tokens());
            if rest > 0 {
                self.parts.push(ContextPart {
                    label: if summed_steps {
                        format!("{cli}'s own instructions and tools, plus tool steps")
                    } else {
                        format!("{cli}'s own instructions and tools")
                    },
                    category: PartCategory::Provider,
                    chars: 0,
                    tokens: rest,
                    estimated: false,
                });
            }
        }
    }

    /// The total the share bars are measured against: the provider's count
    /// when it reported one and it covers the parts, else the parts' sum.
    pub(crate) fn total_tokens(&self) -> u32 {
        let sum = self
            .parts
            .iter()
            .fold(0u32, |a, p| a.saturating_add(p.tokens));
        self.provider_input.map_or(sum, |p| p.max(sum))
    }

    /// Parts largest first (stable for equal sizes).
    pub(crate) fn sorted(&self) -> Vec<&ContextPart> {
        let mut v: Vec<&ContextPart> = self.parts.iter().collect();
        v.sort_by_key(|p| std::cmp::Reverse(p.tokens));
        v
    }

    /// "Total: 43.6K of 200K" or "Total: about 43.6K" when the window is unknown.
    pub(crate) fn total_line(&self) -> String {
        let total = self.total_tokens();
        let about = if self.provider_input.is_some() {
            ""
        } else {
            "about "
        };
        match self.limit {
            Some(l) if !self.summed_steps && total <= l => format!(
                "Total: {about}{}",
                awili_core::limits::used_of_limit(total, l, self.limit_reported)
            ),
            // Input summed across tool steps (or past the window) is not how full
            // the window was: say what it is, and the window beside it.
            Some(l) => format!(
                "Total: {about}{}{} \u{b7} window {}{}",
                awili_core::limits::compact(total),
                if self.summed_steps {
                    " across tool steps"
                } else {
                    ""
                },
                if self.limit_reported { "" } else { "about " },
                awili_core::limits::compact(l)
            ),
            None => format!("Total: {about}{}", awili_core::limits::compact(total)),
        }
    }
}

/// A part's size as shown: "about 1.2K tokens" (estimate) or "3.4K tokens".
pub(crate) fn part_size(p: &ContextPart) -> String {
    let n = awili_core::limits::compact(p.tokens);
    if p.estimated {
        format!("about {n} tokens")
    } else {
        format!("{n} tokens")
    }
}

/// The share of `total` a part takes, 0..=1.
pub(crate) fn share(tokens: u32, total: u32) -> f32 {
    if total == 0 {
        0.0
    } else {
        (tokens as f32 / total as f32).clamp(0.0, 1.0)
    }
}

/// The executor name the model-limits table and ledger use for a CLI engine.
pub(crate) fn engine_executor(engine: awili_code::CodeEngine) -> &'static str {
    match engine {
        awili_code::CodeEngine::Claude => "claude",
        awili_code::CodeEngine::Codex => "codex",
        awili_code::CodeEngine::Glm => "glm",
        awili_code::CodeEngine::Dsh => "deepseek",
    }
}

/// One part per rules file, as `instructions_block` would send it.
pub(crate) fn rules_file_parts(docs: &[crate::agent_launch::AgentsMd]) -> Vec<ContextPart> {
    let mut seen: Vec<&std::path::Path> = Vec::new();
    let mut out = Vec::new();
    for d in docs {
        if seen.contains(&d.path.as_path()) {
            continue;
        }
        seen.push(d.path.as_path());
        let name = d
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "rules".into());
        let place = d
            .path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let label = match place {
            Some(p) => format!("{name} in {p}"),
            None => name,
        };
        let chars = d.path.display().to_string().chars().count()
            + d.text.trim_end().chars().count()
            + "Instructions from :\n\n".len();
        out.push(ContextPart::estimate(label, PartCategory::Rules, chars));
    }
    out
}

impl crate::AwiliApp {
    /// The owner's submit: what the typed message and each attached file add.
    /// Lanes add their own parts when they assemble ([`Self::record_inapp_context`],
    /// [`Self::record_agent_context`]).
    pub(crate) fn note_submit_context(
        &mut self,
        _conv: &str,
        typed: &str,
        attachments: &[awili_persist::Attachment],
    ) {
        let mut draft = ContextBreakdown::default();
        draft.push(ContextPart::estimate(
            "Your message",
            PartCategory::Message,
            typed.chars().count(),
        ));
        for a in attachments {
            draft.push(ContextPart::estimate(
                format!("Attached file {}", a.filename),
                PartCategory::Attachments,
                awili_persist::format_attachment_for_message(a)
                    .chars()
                    .count(),
            ));
        }
        self.work.ctx_draft = Some(draft);
        self.work.ctx_pending = None;
        self.work.ctx_identity = None;
    }

    /// The draft's parts plus whatever the lane added to the message
    /// (fleet recipes, folder instructions, notes from earlier chats).
    fn draft_with_extra(&self, task_chars: usize, extra_label: &str) -> ContextBreakdown {
        let mut b = self.work.ctx_draft.clone().unwrap_or_default();
        let known: usize = b.parts.iter().map(|p| p.chars).sum();
        b.push(ContextPart::estimate(
            extra_label,
            PartCategory::Instructions,
            task_chars.saturating_sub(known),
        ));
        b
    }

    /// The in-app engine's turn: its system text split into memory, rules
    /// files and Awili's own instructions, the conversation so far, and the
    /// message.
    pub(crate) fn record_inapp_context(
        &mut self,
        conv: &str,
        system: Option<&str>,
        policy: Option<&str>,
        folder: Option<&std::path::Path>,
        task_chars: usize,
    ) {
        let mut b = self.draft_with_extra(task_chars, "Notes Awili added to your message");
        let policy_chars = policy.map_or(0, |p| p.chars().count());
        let docs = crate::work::inapp_docs(
            folder,
            self.work.agents.agents_md.as_deref(),
            crate::agent_launch::LaunchPaths::production()
                .map(|p| p.home)
                .as_deref(),
        );
        let rules = rules_file_parts(&docs);
        let rules_chars: usize = rules.iter().map(|p| p.chars).sum();
        let system_chars = system.map_or(0, |s| s.chars().count());
        b.push(ContextPart::estimate(
            "Memory and standing rules",
            PartCategory::Memory,
            policy_chars,
        ));
        for r in rules {
            b.push(r);
        }
        b.push(ContextPart::estimate(
            "Awili's instructions",
            PartCategory::Instructions,
            system_chars.saturating_sub(policy_chars + rules_chars),
        ));
        let history: usize = self
            .views
            .get(conv)
            .map(|v| {
                let m = v.convo.messages();
                m[..m.len().saturating_sub(2)]
                    .iter()
                    .filter(|m| m.superseded_by.is_none())
                    .map(|m| m.text.chars().count())
                    .sum()
            })
            .unwrap_or(0);
        b.push(ContextPart::estimate(
            "Conversation so far",
            PartCategory::History,
            history,
        ));
        self.work.ctx_identity = Some(("owned".into(), self.work.effective_engine().model));
        self.work.ctx_pending = Some(b);
    }

    /// A CLI helper's turn: Awili's brief, the pinned memory block, and the
    /// prompt (message, files, earlier conversation and notes).
    pub(crate) fn record_agent_context(
        &mut self,
        engine: awili_code::CodeEngine,
        model: Option<&str>,
        system: Option<&str>,
        pinned: Option<&str>,
        prompt: &str,
    ) {
        let mut b = self.draft_with_extra(0, "");
        let known: usize = b.parts.iter().map(|p| p.chars).sum();
        b.push(ContextPart::estimate(
            "Earlier conversation and notes",
            PartCategory::History,
            prompt.chars().count().saturating_sub(known),
        ));
        b.push(ContextPart::estimate(
            "Awili's instructions",
            PartCategory::Instructions,
            system
                .map_or(0, |s| s.chars().count())
                .saturating_sub(pinned.map_or(0, |s| s.chars().count())),
        ));
        b.push(ContextPart::estimate(
            "Memory and standing rules",
            PartCategory::Memory,
            pinned.map_or(0, |s| s.chars().count()),
        ));
        b.cli = Some(engine.label().to_string());
        self.work.ctx_identity = Some((
            engine_executor(engine).to_string(),
            model.unwrap_or_default().to_string(),
        ));
        self.work.ctx_pending = Some(b);
    }
}

// ---- saving -----------------------------------------------------------------

/// The `app_state` key prefix of a Work conversation's saved breakdowns.
pub(crate) const CONTEXTS_KEY_PREFIX: &str = "work_contexts:";

pub(crate) fn contexts_key(conv: &str) -> String {
    format!("{CONTEXTS_KEY_PREFIX}{conv}")
}

/// A conversation's saved breakdowns, by [`crate::run_footer::footer_key`].
pub(crate) type SavedContexts = std::collections::BTreeMap<String, ContextBreakdown>;

/// Parse saved breakdowns; anything unreadable is an empty set.
pub(crate) fn parse_saved(raw: &str) -> SavedContexts {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Put saved breakdowns back on a restored conversation's replies (rows line
/// up with the stored messages as in `apply_saved_footers`).
pub(crate) fn apply_saved(
    view: &mut crate::conv_view::ConvView,
    msgs: &[awili_persist::StoredMessage],
    saved: &SavedContexts,
) {
    if saved.is_empty() {
        return;
    }
    let kept = msgs
        .iter()
        .filter(|m| crate::role_from_str(&m.role).is_some());
    for (row, m) in view.rows.iter_mut().zip(kept) {
        if row.role != crate::Role::Assistant {
            continue;
        }
        let ts = u64::try_from(m.ts).unwrap_or_default();
        if let Some(c) = saved.get(&crate::run_footer::footer_key(ts, &m.text)) {
            row.context = Some(c.clone());
        }
    }
}

impl crate::AwiliApp {
    /// A Work turn is finishing: close the pending breakdown with the
    /// provider's reported input, attach it to the reply at `idx` and save it.
    pub(crate) fn attach_work_context(&mut self, conv: &str, idx: usize, ts: u64, text: &str) {
        let Some(mut pending) = self.work.ctx_pending.take() else {
            return;
        };
        let (_, model) = self.work.ctx_identity.take().unwrap_or_default();
        // The provider's own window for this turn, else the table's fallback.
        let window = awili_core::limits::window_for_turn(self.work.panel.context_window, &model);
        // A provider that summed several steps (tool rounds) reports one input
        // figure for all of them.
        let summed = self.work.panel.usage_frames > 1
            || self
                .views
                .get(conv)
                .and_then(|v| v.rows.get(idx))
                .is_some_and(|r| !r.activity.is_empty());
        pending.settle(
            self.work.panel.usage.map(|u| u.input_tokens),
            window,
            summed,
        );
        if pending.parts.is_empty() {
            return;
        }
        let Some(row) = self.views.get_mut(conv).and_then(|v| v.rows.get_mut(idx)) else {
            return;
        };
        row.context = Some(pending.clone());
        let saved = self.work_contexts.entry(conv.to_string()).or_default();
        let prefix = format!("{ts}:");
        saved.retain(|k, _| !k.starts_with(&prefix));
        saved.insert(crate::run_footer::footer_key(ts, text), pending);
        if let (Some(p), Ok(json)) = (&self.persist, serde_json::to_string(saved)) {
            p.set_state(&contexts_key(conv), json);
        }
    }
}

/// How full the model's window was, in tokens, or `None` when the run's figures
/// cannot say. A run that folded several usage reports (or used tool steps)
/// reports input summed across its requests, which is not what any one request
/// held: with several reports the LAST one stands in; a single summed report
/// over tool steps says nothing about the window.
pub(crate) fn window_fill(
    usage: Option<awili_providers::Usage>,
    frames: u32,
    last_input: Option<u32>,
    has_steps: bool,
) -> Option<u32> {
    let usage = usage?;
    if frames > 1 {
        last_input
    } else if has_steps {
        None
    } else {
        Some(usage.input_tokens)
    }
}

/// "43.6K of 200K context" for the run footer. Never more used than the window.
pub(crate) fn footer_context_part(
    model: &str,
    used: Option<u32>,
    reported: Option<u32>,
) -> Option<SharedString> {
    let used = used?;
    let (limit, is_reported) = awili_core::limits::window_for_turn(reported, model)?;
    if used > limit {
        return None;
    }
    Some(
        format!(
            "{} context",
            awili_core::limits::used_of_limit(used, limit, is_reported)
        )
        .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_breakdown_survives_a_relaunch_through_app_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s2.db");
        let store = awili_persist::Store::open(&path).unwrap();
        let mut b = cli_turn();
        b.settle(Some(5_000), Some((200_000, true)), false);
        let mut saved = SavedContexts::new();
        saved.insert(crate::run_footer::footer_key(9, "the reply"), b.clone());
        store
            .set_app_state(&contexts_key("c1"), &serde_json::to_string(&saved).unwrap())
            .unwrap();
        drop(store);
        let reopened = awili_persist::Store::open(&path).unwrap();
        let raw = reopened
            .get_app_state(&contexts_key("c1"))
            .unwrap()
            .unwrap();
        let msgs = vec![
            stored(1, "user", "q", 8),
            stored(2, "assistant", "the reply", 9),
        ];
        let mut view = crate::conv_view::ConvView::from_state(crate::restore_conversation(&msgs));
        apply_saved(&mut view, &msgs, &parse_saved(&raw));
        assert!(view.rows[0].context.is_none());
        assert_eq!(view.rows[1].context.as_ref(), Some(&b));
    }

    #[test]
    fn council_trim_prefers_the_reported_window_else_the_fallback() {
        let engine = awili_config::EngineConfig::default();
        let mut reported = std::collections::HashMap::new();
        // Nothing reported yet: a conservative trim, never a hard-coded window.
        assert_eq!(
            crate::council_run::window_tokens_reported("agent:claude", &engine, &[], &reported),
            100_000
        );
        reported.insert("agent:claude".to_string(), 1_000_000);
        assert_eq!(
            crate::council_run::window_tokens_reported("agent:claude", &engine, &[], &reported),
            900_000
        );
        assert_eq!(
            crate::council_run::window_tokens_reported("agent:codex", &engine, &[], &reported),
            100_000
        );
        assert_eq!(
            crate::council_run::window_tokens_reported("provider:fleet", &engine, &[], &reported),
            32_000
        );
    }

    fn stored(id: i64, role: &str, text: &str, ts: i64) -> awili_persist::StoredMessage {
        awili_persist::StoredMessage {
            id,
            role: role.into(),
            text: text.into(),
            ts,
            kind: "normal".into(),
            superseded_by: None,
        }
    }

    fn cli_turn() -> ContextBreakdown {
        let mut b = ContextBreakdown {
            cli: Some("Claude".into()),
            ..Default::default()
        };
        b.push(ContextPart::estimate(
            "Your message",
            PartCategory::Message,
            400,
        ));
        b.push(ContextPart::estimate(
            "Awili's instructions",
            PartCategory::Instructions,
            4_000,
        ));
        b
    }

    #[test]
    fn estimate_is_chars_over_four_rounded_up() {
        assert_eq!(
            ContextPart::estimate("x", PartCategory::Message, 400).tokens,
            100
        );
        assert_eq!(
            ContextPart::estimate("x", PartCategory::Message, 401).tokens,
            101
        );
        assert!(ContextPart::estimate("x", PartCategory::Message, 4).estimated);
    }

    #[test]
    fn cli_remainder_is_provider_input_minus_awili_parts() {
        let mut b = cli_turn();
        assert_eq!(b.awili_tokens(), 1_100);
        b.settle(Some(13_100), Some((200_000, true)), false);
        let rest = b.parts.last().unwrap();
        assert_eq!(rest.label, "Claude's own instructions and tools");
        assert_eq!(rest.tokens, 12_000);
        assert!(!rest.estimated);
        assert_eq!(b.total_tokens(), 13_100);
        assert_eq!(b.total_line(), "Total: 13.1K of 200K");
        // Settling again does not stack a second remainder.
        b.settle(Some(13_100), Some((200_000, true)), false);
        assert_eq!(b.parts.len(), 3);
    }

    #[test]
    fn remainder_floors_at_zero_and_total_is_never_below_the_parts() {
        let mut b = cli_turn();
        b.settle(Some(500), None, false);
        assert!(b.parts.iter().all(|p| p.category != PartCategory::Provider));
        assert_eq!(b.total_tokens(), 1_100);
        assert_eq!(b.total_line(), "Total: 1.1K");
    }

    #[test]
    fn no_provider_count_means_no_remainder_and_an_about_total() {
        let mut b = cli_turn();
        b.settle(None, Some((200_000, true)), false);
        assert_eq!(b.parts.len(), 2);
        assert_eq!(b.total_line(), "Total: about 1.1K of 200K");
    }

    #[test]
    fn in_app_turn_has_no_remainder() {
        let mut b = ContextBreakdown::default();
        b.push(ContextPart::estimate(
            "Your message",
            PartCategory::Message,
            40,
        ));
        b.settle(Some(9_000), None, false);
        assert_eq!(b.parts.len(), 1);
    }

    #[test]
    fn sorted_is_largest_first_and_share_is_clamped() {
        let b = cli_turn();
        assert_eq!(b.sorted()[0].label, "Awili's instructions");
        assert_eq!(share(50, 100), 0.5);
        assert_eq!(share(5, 0), 0.0);
        assert_eq!(share(500, 100), 1.0);
    }

    #[test]
    fn saved_json_round_trips() {
        let mut b = cli_turn();
        b.settle(Some(5_000), Some((200_000, true)), false);
        let mut saved = SavedContexts::new();
        saved.insert(crate::run_footer::footer_key(7, "reply"), b.clone());
        let json = serde_json::to_string(&saved).unwrap();
        let back = parse_saved(&json);
        assert_eq!(
            back.get(&crate::run_footer::footer_key(7, "reply")),
            Some(&b)
        );
        assert!(parse_saved("not json").is_empty());
    }

    #[test]
    fn footer_context_part_prefers_the_reported_window_and_marks_the_fallback() {
        let u = awili_providers::Usage {
            cached_input_tokens: None,
            input_tokens: 43_600,
            output_tokens: 10,
        };
        // Reported (Claude haiku: 1M) beats the table and is not marked "about".
        assert_eq!(
            footer_context_part("claude-haiku-4-5", Some(u.input_tokens), Some(1_000_000)).unwrap(),
            "43.6K of 1M context"
        );
        // Nothing reported: the table's value, marked "about".
        assert_eq!(
            footer_context_part("glm-4.6", Some(u.input_tokens), None).unwrap(),
            "43.6K of about 200K context"
        );
        assert!(footer_context_part("my-local-model", Some(u.input_tokens), None).is_none());
        assert!(footer_context_part("glm-4.6", None, None).is_none());
    }

    #[test]
    fn a_tool_heavy_run_never_reads_more_used_than_the_window() {
        let u = |n| awili_providers::Usage {
            cached_input_tokens: None,
            input_tokens: n,
            output_tokens: 1,
        };
        // One summed report over tool steps says nothing about the window.
        assert_eq!(window_fill(Some(u(700_000)), 1, None, true), None);
        // Several reports: the last one stands in for the window's fill.
        assert_eq!(
            window_fill(Some(u(700_000)), 4, Some(180_000), true),
            Some(180_000)
        );
        // A plain single-request run uses its own input.
        assert_eq!(window_fill(Some(u(43_600)), 1, None, false), Some(43_600));
        assert_eq!(window_fill(None, 1, None, false), None);
        // Whatever the figures, used is never shown above the window.
        assert!(footer_context_part("m", Some(700_000), Some(200_000)).is_none());
        assert_eq!(
            footer_context_part("m", Some(200_000), Some(200_000)).unwrap(),
            "200K of 200K context"
        );
    }

    #[test]
    fn summed_tool_step_input_is_never_set_against_the_window_in_inspect() {
        let mut b = cli_turn();
        b.settle(Some(700_000), Some((200_000, true)), true);
        assert_eq!(
            b.total_line(),
            "Total: 700K across tool steps \u{b7} window 200K"
        );
        let mut over = cli_turn();
        over.settle(Some(700_000), Some((200_000, false)), false);
        assert_eq!(over.total_line(), "Total: 700K \u{b7} window about 200K");
    }

    #[test]
    fn cli_remainder_says_when_tool_steps_were_summed() {
        let mut b = cli_turn();
        b.settle(Some(13_100), Some((475_000, true)), true);
        assert_eq!(
            b.parts.last().unwrap().label,
            "Claude's own instructions and tools, plus tool steps"
        );
        assert_eq!(
            b.total_line(),
            "Total: 13.1K across tool steps \u{b7} window 475K"
        );
        let mut f = cli_turn();
        f.settle(Some(13_100), Some((200_000, false)), false);
        assert_eq!(f.total_line(), "Total: 13.1K of about 200K");
    }

    #[test]
    fn rules_files_are_one_part_each_and_deduped() {
        let doc = |p: &str, t: &str| crate::agent_launch::AgentsMd {
            path: std::path::PathBuf::from(p),
            text: t.into(),
        };
        let parts = rules_file_parts(&[
            doc("/a/proj/AGENTS.md", &"x".repeat(100)),
            doc("/a/proj/AGENTS.md", "dup"),
            doc("/home/u/AGENTS.md", "hi"),
        ]);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].label, "AGENTS.md in proj");
        assert_eq!(parts[0].category, PartCategory::Rules);
    }
}
