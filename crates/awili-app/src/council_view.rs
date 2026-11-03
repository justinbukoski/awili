//! RC2-CNCL5: the Council page — one message to several models at once,
//! their answers side by side (Council plan §3).
//!
//! This slice puts Council on the left rail after Code, gives it its own
//! conversation kind and sidebar list, and opens a new council with the last
//! group used (so the first thing the owner does is type). The member bar,
//! rounds and answers arrive in the following slices.

use std::collections::HashMap;

use gpui::{div, prelude::*, AnyElement, Context, ScrollHandle, SharedString, Window};

use awili_core::ConvKind;

use crate::{theme, AwiliApp};

/// `app_state`: the members of the council used last, as JSON
/// `[[route, label], …]` — a new council starts with them.
pub(crate) const LAST_GROUP_KEY: &str = "council_last_group";

/// RC2-CNCL11: `app_state`: one council's settings, JSON `{"auto_rounds": n}`.
pub(crate) fn settings_key(conv: &str) -> String {
    format!("council_settings:{conv}")
}

/// The most automatic "answer each other" rounds a council can be set to.
pub(crate) const MAX_AUTO_ROUNDS: u8 = 3;

/// A council's saved settings !92 its automatic rounds (garbled: 0).
pub(crate) fn auto_rounds_from_json(raw: &str) -> u8 {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.get("auto_rounds")?.as_u64())
        .map(|n| n.min(u64::from(MAX_AUTO_ROUNDS)) as u8)
        .unwrap_or(0)
}

/// RC2-CNCLF4: `app_state`: the messages waiting their turn in one council,
/// JSON `["…", …]`.
pub(crate) fn queue_key(conv: &str) -> String {
    format!("council_queue:{conv}")
}

/// RC2-R3: `app_state`: what the owner did with one council's plans, JSON
/// `{"<turn id>": "declined" | "approved"}`.
pub(crate) fn plans_key(conv: &str) -> String {
    format!("council_plans:{conv}")
}

/// RC2-R3: [`plans_key`]'s JSON for `outcomes`.
pub(crate) fn plan_outcomes_json(
    outcomes: &std::collections::BTreeMap<String, awili_council::PlanOutcome>,
) -> String {
    let map: serde_json::Map<String, serde_json::Value> = outcomes
        .iter()
        .map(|(id, o)| (id.clone(), serde_json::Value::from(o.as_str())))
        .collect();
    serde_json::Value::Object(map).to_string()
}

/// RC2-R3: [`plans_key`]'s JSON read back (anything unreadable is left out).
pub(crate) fn plan_outcomes_from_json(
    raw: &str,
) -> std::collections::BTreeMap<String, awili_council::PlanOutcome> {
    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(id, v)| Some((id, awili_council::PlanOutcome::from_token(v.as_str()?)?)))
        .collect()
}

/// RC2-CNCLF4: a card's foot when Awili closed while it answered.
pub(crate) const CUT_FOOT: &str = "cut off when Awili closed";

/// RC2-CNCLF4: the card of a member cut off before its first word.
pub(crate) const CUT_BEFORE_WORDS: &str = "Awili closed before it wrote anything.";

/// RC2-CNCLF4: the line under a council whose waiting messages were given
/// back after a relaunch.
pub(crate) fn queue_back_note(n: usize) -> String {
    if n == 1 {
        "Your message waiting for that round wasn\u{2019}t sent before Awili closed. It\u{2019}s \
         back in the box."
            .to_string()
    } else {
        format!(
            "Your {n} messages waiting for that round weren\u{2019}t sent before Awili closed. \
             They\u{2019}re back in the box."
        )
    }
}

/// RC2-CNCL7: `app_state`: the saved groups, JSON `[{"name", "members": [[route, label], …]}]`.
pub(crate) const GROUPS_KEY: &str = "council_groups";

/// RC2-CNCL7: a group saved in the picker ("Usual four").
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedGroup {
    pub(crate) name: String,
    pub(crate) members: Vec<(String, String)>,
}

/// `[[route, label], …]` for `app_state`.
pub(crate) fn group_to_json(group: &[(String, String)]) -> String {
    let rows: Vec<[&str; 2]> = group
        .iter()
        .map(|(r, l)| [r.as_str(), l.as_str()])
        .collect();
    serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string())
}

/// The message box's hint on the Council page.
pub(crate) const COMPOSER_PLACEHOLDER: &str = "Ask the council\u{2026} (@ a name to ask just one)";

/// The empty state's `debug_selector`.
pub(crate) const EMPTY_STATE_SELECTOR: &str = "council-empty";
pub(crate) const EMPTY_INTRO_SELECTOR: &str = "council-empty-intro";
pub(crate) const EMPTY_GOES_TO_SELECTOR: &str = "council-empty-goes-to";
pub(crate) const DELETE_SELECTOR: &str = "council-delete";
pub(crate) const DELETE_CONFIRM_SELECTOR: &str = "council-delete-confirm";
pub(crate) const DELETE_CANCEL_SELECTOR: &str = "council-delete-cancel";

/// The member bar's chip `i` (`debug_selector`).
pub(crate) fn seat_selector(i: usize) -> String {
    format!("council-seat-{i}")
}

/// Dynamic test lookup belongs to the caller; shared kit selectors remain static.
fn track_control(selector: String, button: crate::ui::IconButton) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .debug_selector(move || selector.clone())
        .child(button)
        .into_any_element()
}

/// Preserve Council's stable selectors around the shared KIT button face.
fn council_button(selector: String, button: crate::ui::Button) -> AnyElement {
    div()
        .flex_none()
        .debug_selector(move || selector.clone())
        .child(button)
        .into_any_element()
}

/// Pending gates stay visible even when an answer body is collapsed.
pub(crate) fn track_signal(
    page: &CouncilPage,
    t: &awili_council::SeatTurn,
) -> (theme::Tone, String) {
    let needs_owner = page.approvals.contains_key(&t.id) || page.plans.contains_key(&t.id);
    let status = if needs_owner {
        "Needs you".to_string()
    } else {
        t.stance
            .map(|s| s.label().to_string())
            .unwrap_or_else(|| match &t.status {
                awili_council::TurnStatus::Running => "Answering".into(),
                awili_council::TurnStatus::Answered => "Answered".into(),
                awili_council::TurnStatus::Stopped => "Stopped".into(),
                awili_council::TurnStatus::Failed {
                    needs_owner: true, ..
                } => "Needs you".into(),
                awili_council::TurnStatus::Failed { .. } => "Could not answer".into(),
            })
    };
    let tone = if needs_owner {
        theme::Tone::ActNow
    } else {
        match &t.status {
            awili_council::TurnStatus::Failed {
                needs_owner: true, ..
            } => theme::Tone::ActNow,
            awili_council::TurnStatus::Failed { .. } => theme::Tone::Attention,
            _ => theme::Tone::Neutral,
        }
    };
    (tone, status)
}

/// One council on screen: its members and rounds (the pure
/// [`awili_council::Council`]) plus what only the page needs.
pub(crate) struct CouncilPage {
    pub(crate) council: awili_council::Council,
    pub(crate) scroll: ScrollHandle,
    pub(crate) focus_member: Option<String>,
    pub(crate) hidden_members: std::collections::HashSet<String>,
    /// RC2-CNCL6: each member answering right now, by turn id.
    pub(crate) runs: HashMap<String, crate::council_run::SeatRun>,
    /// RC2-CNCL6: one plain line about the last send (a refusal, who was
    /// left out), shown above the message box.
    pub(crate) note: Option<String>,
    /// RC2-CNCL7: the picker sheet, open, with the routes ticked so far (in
    /// order).
    pub(crate) picker: Option<Vec<String>>,
    /// RC2-CNCL8: cards whose Details are open (turn ids).
    pub(crate) details_open: std::collections::HashSet<String>,
    /// RC2-CNCL8: earlier rounds the owner opened again.
    pub(crate) expanded: std::collections::HashSet<u32>,
    /// RC2-CNCL8: what each turn was sent (for Details), by turn id.
    pub(crate) sent: HashMap<String, SentFacts>,
    /// RC2-CNCL9: messages typed while the council was answering, in order.
    pub(crate) queued: std::collections::VecDeque<String>,
    /// RC2-CNCL9: each round's memory-gate outcome (a retry or a continue
    /// uses the same one).
    pub(crate) gates: HashMap<u32, Result<Option<String>, String>>,
    /// RC2-CNCL9: the conversation row (ts) of each turn's answer, so a
    /// continued or retried answer replaces its row.
    pub(crate) answer_ts: HashMap<String, u64>,
    /// RC2-CNCL11: after each message, let the members answer each other
    /// this many times on their own (0–3; 0 = only on a tap).
    pub(crate) auto_rounds: u8,
    /// RC2-CNCL13: the "who sums up?" choice is showing.
    pub(crate) sum_up_open: bool,
    /// RC2-CNCLF1: plans waiting for the owner's OK, by turn id (the plan
    /// text). Like Work's plan card, a plan lasts while Awili runs.
    pub(crate) plans: HashMap<String, String>,
    /// RC2-CNCLF2: steps waiting for the owner's OK, by turn id (the in-app
    /// engine's approval card).
    pub(crate) approvals: HashMap<String, crate::work::ApprovalCard>,
    /// RC2-CNCLF4: turns cut off by a quit (still answering when Awili
    /// closed), by turn id.
    pub(crate) cut: std::collections::HashSet<String>,
    /// RC2-CNCLG1: members whose cards show them answering while the memory
    /// lookup for their owner message is still out (Stop takes them back).
    pub(crate) waiting: Vec<crate::council_run::Waiting>,
    /// RC2-CNCLG3: owner-message rounds whose memory lookup is out right
    /// now (a later caller waits on it rather than asking again).
    pub(crate) gating: std::collections::HashSet<u32>,
    /// RC2-R2F5: the tools each turn's member used, in order, by turn id
    /// (its card's "Used:" line and Details; saved with the turn).
    pub(crate) tools_used: HashMap<String, Vec<String>>,
    /// RC2-R2F11: the once-a-second repaint runs while a round answers (the
    /// header's time moves with no new words).
    pub(crate) ticking: bool,
    /// RC2-R3: what the owner did with each member's plan, by turn id
    /// (kept with the council; every later round, the sum-up too, is told).
    pub(crate) plan_outcomes: std::collections::BTreeMap<String, awili_council::PlanOutcome>,
    /// RC2-R3: what a rerun carries into its next start, by turn id.
    pub(crate) carry: HashMap<String, crate::council_run::Carry>,
}

/// RC2-CNCL8: what one member was sent, as Details shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SentFacts {
    /// Older answers were left out to fit its window.
    pub(crate) older_left_out: bool,
    /// The fingerprint of exactly what was sent.
    pub(crate) fingerprint: String,
    /// RC2-CNCL9: the prompt itself (a retry sends it again).
    pub(crate) prompt: awili_council::SeatPrompt,
    /// RC2-CNCLF2: what it could use, in plain words (Details).
    pub(crate) tools: String,
}

impl CouncilPage {
    /// A council seated with `group` (`(route, label)` pairs, in order).
    pub(crate) fn with_group(group: &[(String, String)]) -> Self {
        let mut council = awili_council::Council::default();
        for (route, label) in group {
            council.add_member(route, label);
        }
        Self {
            council,
            scroll: ScrollHandle::new(),
            focus_member: None,
            hidden_members: Default::default(),
            runs: HashMap::new(),
            note: None,
            picker: None,
            details_open: Default::default(),
            expanded: Default::default(),
            sent: HashMap::new(),
            queued: Default::default(),
            gates: HashMap::new(),
            answer_ts: HashMap::new(),
            auto_rounds: 0,
            sum_up_open: false,
            plans: HashMap::new(),
            approvals: HashMap::new(),
            cut: Default::default(),
            waiting: Vec::new(),
            gating: Default::default(),
            tools_used: HashMap::new(),
            ticking: false,
            plan_outcomes: Default::default(),
            carry: HashMap::new(),
        }
    }

    /// RC2-CNCLH1: how many members are answering right now — streaming, or
    /// still waiting for the memory lookup (their cards already show them
    /// answering, and Stop all stops them too).
    pub(crate) fn answering(&self) -> usize {
        self.runs.len() + self.waiting.len()
    }

    /// The present members' names, in column order.
    pub(crate) fn member_names(&self) -> Vec<String> {
        self.council
            .active_members()
            .iter()
            .map(|m| m.label.clone())
            .collect()
    }
}

/// The group a council starts with when none was used before: Claude
/// Sonnet and Codex (subscriptions) and the model Work uses on this Mac.
pub(crate) fn default_group(engine: &awili_config::EngineConfig) -> Vec<(String, String)> {
    ["agent:claude:sonnet", "agent:codex", "in-app"]
        .iter()
        .map(|r| (r.to_string(), route_label(r, engine, &[])))
        .collect()
}

/// A saved group (`[[route, label], …]`); a garbled or empty value is `None`.
pub(crate) fn group_from_json(raw: &str) -> Option<Vec<(String, String)>> {
    let rows: Vec<[String; 2]> = serde_json::from_str(raw).ok()?;
    let group: Vec<(String, String)> = rows
        .into_iter()
        .filter(|[r, l]| !r.trim().is_empty() && !l.trim().is_empty())
        .map(|[r, l]| (r, l))
        .collect();
    (!group.is_empty()).then_some(group)
}

fn title_word(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The plain name of a member route: "Claude Sonnet", "Codex", "DeepSeek
/// Flash", "GLM", "Hermes", or the in-app engine's own label ("Gemma 4 26B").
pub(crate) fn route_label(
    route: &str,
    engine: &awili_config::EngineConfig,
    extras: &[awili_config::ExtraEngineConfig],
) -> String {
    if route == "in-app" {
        return engine.display_label();
    }
    if let Some(id) = crate::council_run::in_app_extra_id(route) {
        return match extras.iter().find(|e| e.id == id) {
            Some(e) => e.to_engine_config().display_label(),
            None => format!("{id} (not set up)"),
        };
    }
    recorded_route_label(route)
}

/// The track header's second line: where the answer came from, then its status.
pub(crate) fn track_subtitle(route: &str, status: &str) -> String {
    format!("{} \u{00B7} {status}", recorded_route_label(route))
}

/// Decode only the route frozen into the turn; current settings are not history.
fn recorded_route_label(route: &str) -> String {
    if route == "in-app" || crate::council_run::in_app_extra_id(route).is_some() {
        return "In-app route".to_string();
    }
    if route == crate::agent_chat::FLEET_CHAT_ROUTE {
        return "Hermes".to_string();
    }
    // RC2-CNCL7: a `[[providers]]` model reads as its model name.
    if let Some((_, model)) = route.strip_prefix("model:").and_then(|r| r.split_once(':')) {
        return model.to_string();
    }
    let Some(rest) = route.strip_prefix("agent:") else {
        return route.to_string();
    };
    let (engine_s, model) = match rest.split_once(':') {
        Some((e, m)) if !m.is_empty() => (e, Some(m)),
        Some((e, _)) => (e, None),
        None => (rest, None),
    };
    match (engine_s, model) {
        ("claude", None) => "Claude".to_string(),
        ("claude", Some(m)) => format!("Claude {}", title_word(m)),
        ("codex", None) => "Codex".to_string(),
        ("codex", Some(m)) => format!("Codex {m}"),
        ("glm", None) => "GLM".to_string(),
        ("glm", Some(m)) => format!("GLM {m}"),
        ("dsh", None) => "DeepSeek".to_string(),
        ("dsh", Some(m)) => match m.strip_prefix("deepseek-") {
            Some(v) => format!(
                "DeepSeek {}",
                v.split('-').map(title_word).collect::<Vec<_>>().join(" ")
            ),
            None => format!("DeepSeek {m}"),
        },
        _ => route.to_string(),
    }
}

/// "Goes to Claude Sonnet, Codex and Gemma 4 26B" (the empty state's line).
pub(crate) fn goes_to_line(names: &[String]) -> String {
    match names.len() {
        0 => "Nobody is on this council yet. Add a model to start.".to_string(),
        1 => format!("Goes to {}", names[0]),
        n => format!("Goes to {} and {}", names[..n - 1].join(", "), names[n - 1]),
    }
}

impl AwiliApp {
    /// The group a new council starts with: the last one used, else the
    /// default.
    pub(crate) fn council_start_group(&mut self) -> Vec<(String, String)> {
        if self.council_last_group.is_none() {
            self.council_last_group = self
                .search_store
                .as_ref()
                .and_then(|s| s.get_app_state(LAST_GROUP_KEY).ok().flatten())
                .and_then(|raw| group_from_json(&raw));
        }
        self.council_last_group
            .clone()
            .unwrap_or_else(|| default_group(&self.work.engine))
    }

    /// Make sure a Council conversation has its page (members seated from
    /// the last group used). Other kinds are left alone.
    pub(crate) fn ensure_council_page(&mut self, id: &str) {
        if self.councils.contains_key(id)
            || self.conversations.get(id).map(|c| c.kind) != Some(ConvKind::Council)
        {
            return;
        }
        // RC2-CNCL15: a council kept in `awili.db` comes back as it was;
        // one never seated starts with the last group.
        let saved = self
            .search_store
            .as_ref()
            .and_then(|s| page_from_store(s, id));
        let restored = saved.is_some();
        let mut page = match saved {
            Some(p) => p,
            None => {
                let group = self.council_start_group();
                CouncilPage::with_group(&group)
            }
        };
        // RC2-CNCLF4: messages that were waiting their turn when Awili closed
        // go back in this council's box (nothing answers now to send them).
        let queued: Vec<String> = restored
            .then(|| {
                self.search_store
                    .as_ref()
                    .and_then(|s| s.get_app_state(&queue_key(id)).ok().flatten())
            })
            .flatten()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|t| !t.trim().is_empty())
            .collect();
        if !queued.is_empty() {
            self.drafts.hand_back(
                &crate::composer_drafts::conversation_key(id),
                &queued.join("\n\n"),
            );
            page.note = Some(queue_back_note(queued.len()));
            if let Some(p) = &self.persist {
                p.set_state(&queue_key(id), "[]".to_string());
            }
        }
        // RC2-R3: what the owner did with its plans (a relaunch keeps it).
        page.plan_outcomes = self
            .search_store
            .as_ref()
            .and_then(|s| s.get_app_state(&plans_key(id)).ok().flatten())
            .map(|raw| plan_outcomes_from_json(&raw))
            .unwrap_or_default();
        // RC2-CNCL11: this council's own setting.
        page.auto_rounds = self
            .search_store
            .as_ref()
            .and_then(|s| s.get_app_state(&settings_key(id)).ok().flatten())
            .map(|raw| auto_rounds_from_json(&raw))
            .unwrap_or(0);
        self.councils.insert(id.to_string(), page);
        // RC2-R2F10: ready its Codex members' first turn.
        self.council_prewarm(id);
    }

    /// The council on screen, if the active conversation is one.
    pub(crate) fn active_council(&self) -> Option<&CouncilPage> {
        self.councils.get(self.sidebar.active_id())
    }

    /// The member bar: the title, then one chip per member.
    fn council_bar(&self, page: Option<&CouncilPage>, cx: &mut Context<Self>) -> AnyElement {
        let title: SharedString = self
            .conversations
            .get(self.sidebar.active_id())
            .map(|c| c.title.clone())
            .filter(|t| !t.trim().is_empty() && t != awili_core::DEFAULT_TITLE)
            .unwrap_or_else(|| "New council".to_string())
            .into();
        let mut seats = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(theme::z(theme::SP_4));
        for (i, name) in page
            .map(CouncilPage::member_names)
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            seats = seats.child(
                div().debug_selector(move || seat_selector(i)).child(
                    crate::ui::Chip::new(
                        SharedString::from(format!("council-seat-chip-{i}")),
                        SharedString::from(name),
                    )
                    .dot(theme::Tone::Neutral),
                ),
            );
        }
        div()
            .flex_none()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(theme::z(theme::SP_12))
            .px(theme::z(theme::SP_20))
            .py(theme::z(theme::SP_8))
            .border_b_1()
            .border_color(theme::palette().line)
            .child(
                div()
                    .min_w_0()
                    .max_w_full()
                    .truncate()
                    .child(crate::ui::ui_text(title).strong()),
            )
            .child(seats)
            .child(self.council_add_remove_button(page, cx))
            .child(self.council_delete_control(cx))
            // RC2-CNCL12: the whole council's time and cost.
            .children(page.filter(|p| !p.council.rounds.is_empty()).map(|p| {
                div()
                    .debug_selector(|| TALLY_SELECTOR.to_string())
                    .ml_auto()
                    .text_size(theme::z(theme::type_::UI.size))
                    .text_color(theme::palette().text_dim)
                    .child({
                        let drawn = gpui::StyledText::new(SharedString::from(
                            crate::run_footer::council_tally(&p.council),
                        ));
                        #[cfg(test)]
                        DRAWN_TALLIES.with(|d| d.borrow_mut().push(drawn.layout().clone()));
                        drawn
                    })
            }))
            .into_any_element()
    }

    /// RC3-S1a: Delete for the council on screen, with the sidebar's inline
    /// "Delete?" confirm (the same `pending_delete` state, so the sidebar row
    /// and this page agree).
    fn council_delete_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let id = self.sidebar.active_id().to_string();
        if self.pending_delete.as_deref() != Some(id.as_str()) {
            let arm_id = id;
            return council_button(
                DELETE_SELECTOR.to_string(),
                crate::ui::Button::new("council-delete", "Delete")
                    .quiet()
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.pending_delete = Some(arm_id.clone());
                        cx.notify();
                    })),
            );
        }
        let (confirm_id, cancel_id) = (id.clone(), id);
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(theme::z(theme::SP_4))
            .child(crate::ui::text::caption("Delete?").text_color_override(theme::p().sev_critical))
            .child(track_control(
                DELETE_CONFIRM_SELECTOR.to_string(),
                crate::ui::IconButton::new("council-delete-yes", crate::icons::Icon::Check)
                    .quiet()
                    .pointer_only()
                    .allow_double_click()
                    .label("Confirm delete")
                    .tooltip("Confirm delete")
                    .on_click(cx.listener(move |this, _ev, window, cx| {
                        if this.pending_delete.as_deref() == Some(confirm_id.as_str()) {
                            this.delete_conversation(&confirm_id, window, cx);
                        }
                    })),
            ))
            .child(track_control(
                DELETE_CANCEL_SELECTOR.to_string(),
                crate::ui::IconButton::new("council-delete-no", crate::icons::Icon::X)
                    .quiet()
                    .pointer_only()
                    .allow_double_click()
                    .label("Keep council")
                    .tooltip("Keep council")
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        if this.pending_delete.as_deref() == Some(cancel_id.as_str()) {
                            this.pending_delete = None;
                            cx.notify();
                        }
                    })),
            ))
            .into_any_element()
    }

    /// Before the first message: what Council is, and who it goes to.
    fn council_empty_state(&self, page: Option<&CouncilPage>) -> AnyElement {
        let names = page.map(CouncilPage::member_names).unwrap_or_default();
        div()
            .debug_selector(|| EMPTY_STATE_SELECTOR.to_string())
            .flex()
            .flex_col()
            .gap(theme::z(theme::SP_8))
            .max_w(theme::z(theme::READING_MEASURE))
            .mx_auto()
            .mt(theme::z(theme::SP_56))
            .px(theme::z(theme::SP_20))
            .child(crate::ui::title("Council"))
            // RC3-S1a: each paragraph sits in its own full-width, non-shrinking
            // block so a wrapped paragraph can never be squeezed into the one
            // below it.
            .child(
                div()
                    .debug_selector(|| EMPTY_INTRO_SELECTOR.to_string())
                    .flex_none()
                    .w_full()
                    .min_w_0()
                    .child(
                        crate::ui::ui_text(
                            "Ask several models at once. They answer side by side, then, if you \
                 want, read and answer each other.",
                        )
                        .dim()
                        .wrap(),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| EMPTY_GOES_TO_SELECTOR.to_string())
                    .flex_none()
                    .w_full()
                    .min_w_0()
                    .child(
                        crate::ui::ui_text(SharedString::from(goes_to_line(&names)))
                            .dim()
                            .wrap(),
                    ),
            )
            .into_any_element()
    }

    /// RC2-CNCL8: one member's answer card. It leads with the answer; a
    /// streaming answer ends in a caret; a member that only shows its whole
    /// answer at the end says so; a member that didn't answer says what
    /// happened. "Details" (route, what it was sent, words, time to first
    /// word) stays closed until asked.
    pub(crate) fn council_toggle_track_focus(
        &mut self,
        owner: &str,
        member: &str,
        cx: &mut Context<Self>,
    ) {
        if self.mode != crate::work::Mode::Council || self.sidebar.active_id() != owner {
            return;
        }
        let Some(page) = self.councils.get_mut(owner) else {
            return;
        };
        if !page
            .council
            .rounds
            .iter()
            .any(|r| r.turns.iter().any(|t| t.member == member))
        {
            return;
        }
        page.focus_member = if page.focus_member.as_deref() == Some(member) {
            None
        } else {
            Some(member.into())
        };
        page.hidden_members.remove(member);
        cx.notify();
    }

    pub(crate) fn council_toggle_track_hidden(
        &mut self,
        owner: &str,
        member: &str,
        cx: &mut Context<Self>,
    ) {
        if self.mode != crate::work::Mode::Council || self.sidebar.active_id() != owner {
            return;
        }
        let Some(page) = self.councils.get_mut(owner) else {
            return;
        };
        if !page
            .council
            .rounds
            .iter()
            .any(|r| r.turns.iter().any(|t| t.member == member))
        {
            return;
        }
        if !page.hidden_members.remove(member) {
            page.hidden_members.insert(member.into());
        }
        if page.focus_member.as_deref() == Some(member) {
            page.focus_member = None;
        }
        cx.notify();
    }

    fn council_track_header(
        &self,
        page: &CouncilPage,
        r: &awili_council::Round,
        t: &awili_council::SeatTurn,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let owner = self.sidebar.active_id().to_string();
        let hidden = page.hidden_members.contains(&t.member);
        let focused = page.focus_member.as_deref() == Some(t.member.as_str());
        let (focus_owner, focus_member) = (owner.clone(), t.member.clone());
        let (hide_owner, hide_member) = (owner, t.member.clone());
        let (tone, status) = track_signal(page, t);
        let buttons = div()
            .w_full()
            .flex()
            .flex_wrap()
            .min_w_0()
            .gap(theme::z(theme::SP_4))
            .child(track_control(
                format!("council-track-focus-{}", t.id),
                crate::ui::IconButton::new(
                    SharedString::from(format!("track-focus-{}", t.id)),
                    crate::icons::Icon::Focus,
                )
                .quiet()
                .label(if focused {
                    "Unfocus this model"
                } else {
                    "Focus this model"
                })
                .tooltip(if focused {
                    "Show every model's answers"
                } else {
                    "Show this model's answers; all other models are still asked"
                })
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.council_toggle_track_focus(&focus_owner, &focus_member, cx)
                })),
            ))
            .child(track_control(
                format!("council-track-hide-{}", t.id),
                crate::ui::IconButton::new(
                    SharedString::from(format!("track-hide-{}", t.id)),
                    if hidden {
                        crate::icons::Icon::Eye
                    } else {
                        crate::icons::Icon::EyeOff
                    },
                )
                .quiet()
                .label(if hidden {
                    "Show this model"
                } else {
                    "Hide this model"
                })
                .tooltip(if hidden {
                    "Show this model's answers"
                } else {
                    "Collapse this model's answers; it is still asked"
                })
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.council_toggle_track_hidden(&hide_owner, &hide_member, cx)
                })),
            ))
            .children(
                t.status
                    .is_running()
                    .then(|| self.council_card_stop(r.number, t, cx)),
            );
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .child(
                crate::ui::Row::new(SharedString::from(format!("council-track-header-{}", t.id)))
                    .title(t.label.clone())
                    .subtitle(track_subtitle(&t.route, &status))
                    .leading(crate::ui::Dot::new(tone).label(status))
                    .pointer_only()
                    .into_any_element(),
            )
            // This definite width lets the control row wrap inside narrow tracks.
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .px(theme::z(theme::SP_12))
                    .pb(theme::z(theme::SP_8))
                    .child(buttons),
            )
            .into_any_element()
    }

    fn council_track_frame(
        &self,
        t: &awili_council::SeatTurn,
        round: u32,
        width: Option<f32>,
        header: AnyElement,
        body: Option<AnyElement>,
    ) -> AnyElement {
        let member = t.member.clone();
        div()
            .debug_selector(move || card_selector(round, &member))
            .min_w_0()
            .when_some(width, |d, w| d.w(gpui::px(w)).flex_none())
            .child(
                crate::ui::Card::new(SharedString::from(format!("council-track-{}", t.id)))
                    .flush()
                    .children(std::iter::once(header).chain(body)),
            )
            .into_any_element()
    }

    fn council_card(
        &self,
        page: &CouncilPage,
        r: &awili_council::Round,
        t: &awili_council::SeatTurn,
        width: Option<f32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let round = r.number;
        let member = t.member.clone();
        let running = t.status.is_running();
        let (_, body, foot) = card_parts(t, page.cut.contains(&t.id));
        let header = self.council_track_header(page, r, t, cx);
        let collapsed = page.hidden_members.contains(&t.member)
            || page
                .focus_member
                .as_ref()
                .is_some_and(|member| member != &t.member);
        if collapsed {
            return self.council_track_frame(t, round, width, header, None);
        }
        // RC2-R616: a member still waiting for its first word says for how
        // long (Gemma took 24-42 s on a first round).
        let wait = (running && body.is_empty()).then(|| card_wait_shown(t, unix_now()));
        let caret = running && !body.is_empty();
        let open = page.details_open.contains(&t.id);
        let turn_id = t.id.clone();
        let sel_member = member.clone();
        let details: Vec<AnyElement> = card_details_with_tools(page, r, t)
            .into_iter()
            .map(|(key, value)| {
                crate::ui::label(format!("{key}: {value}"))
                    .wrap()
                    .into_any_element()
            })
            .collect();
        let details_owner = self.sidebar.active_id().to_string();
        let details_lookup = details_selector(round, &sel_member);
        let body_selector = format!("council-track-body-{}", t.id);
        let content = div()
            .debug_selector(move || body_selector.clone())
            .flex()
            .flex_col()
            .min_w_0()
            .gap(theme::z(theme::SP_8))
            .px(theme::z(theme::SP_20))
            .pb(theme::z(theme::SP_20))
            // RC2-CNCL11: whether it kept its answer, as a small tag.
            .children(t.stance.map(|s| {
                let (round, m) = (round, sel_member.clone());
                let tone = if s == awili_council::Stance::Same {
                    theme::Tone::Neutral
                } else {
                    theme::Tone::Attention
                };
                div()
                    .debug_selector(move || stance_selector(round, &m))
                    .child(
                        crate::ui::Chip::new(
                            SharedString::from(format!("council-stance-{}", t.id)),
                            s.label(),
                        )
                        .tone(tone),
                    )
            }))
            .children(wait.map(|w| {
                let (round, m) = (round, sel_member.clone());
                div()
                    .debug_selector(move || wait_selector(round, &m))
                    .child(crate::ui::ui_text(SharedString::from(w)).dim().wrap())
            }))
            .when(!body.is_empty(), |d| {
                d.child(
                    div()
                        .text_color(theme::palette().text)
                        // RC2-R2F4 (round 2 F6): formatted, as Chat draws a
                        // reply — no raw "**" or link syntax.
                        .child(crate::markdown_ui::message_blocks(
                            &format!("council-{}", t.id),
                            0,
                            body.len(),
                            &card_body_plan(&body),
                            &crate::markdown_ui::assistant(),
                            None,
                        ))
                        .children(caret.then(|| {
                            let (round, m) = (round, sel_member.clone());
                            div()
                                .debug_selector(move || caret_selector(round, &m))
                                .text_color(theme::palette().text_dim)
                                .child("\u{258D}")
                        })),
                )
            })
            // RC2-R2F5: the tools it used, as one quiet line.
            .children(tools_used_line(page, &t.id).map(|line| {
                let (round, m) = (round, sel_member.clone());
                div()
                    .debug_selector(move || tools_used_selector(round, &m))
                    .child(crate::ui::ui_text(SharedString::from(line)).dim().wrap())
            }))
            // RC2-CNCLF2: a step waiting for the owner's OK.
            .children(
                page.approvals
                    .get(&t.id)
                    .map(|card| self.council_approval_box(round, t, card, cx)),
            )
            // RC2-CNCL12: time, words and cost (or "included" / "free").
            .children(crate::run_footer::council_footer(t).map(|f| {
                let (round, m) = (round, sel_member.clone());
                div()
                    .debug_selector(move || footer_selector(round, &m))
                    .child(crate::ui::ui_text(SharedString::from(f)).dim().wrap())
            }))
            .children(
                (!foot.is_empty()).then(|| {
                    div().child(crate::ui::ui_text(SharedString::from(foot)).dim().wrap())
                }),
            )
            .children(self.council_card_actions(page, r, t, cx))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .debug_selector(move || details_lookup.clone())
                    .child(
                        crate::ui::Section::new(
                            SharedString::from(format!("track-details-{}", t.id)),
                            "Details for nerds",
                        )
                        .nerd()
                        .open(open)
                        .body(details)
                        .on_toggle(cx.listener(move |app, _, _, cx| {
                            if app.mode == crate::work::Mode::Council
                                && app.sidebar.active_id() == details_owner
                            {
                                app.council_toggle_details(&turn_id, cx);
                            }
                        })),
                    ),
            )
            .into_any_element();
        self.council_track_frame(t, round, width, header, Some(content))
    }

    /// RC2-R2F4: a running card's Stop, in its header (the card's own
    /// buttons' look).
    fn council_card_stop(
        &self,
        round: u32,
        t: &awili_council::SeatTurn,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sel = stop_selector(round, &t.member);
        let turn_id = t.id.clone();
        let owner = self.sidebar.active_id().to_string();
        track_control(
            sel,
            crate::ui::IconButton::new(
                SharedString::from(format!("track-stop-{}", t.id)),
                crate::icons::Icon::Square,
            )
            .label("Stop this model")
            .ghost()
            .on_click(cx.listener(move |app, _, _, cx| {
                if app.mode == crate::work::Mode::Council && app.sidebar.active_id() == owner {
                    app.council_stop_member(&turn_id, cx);
                }
            })),
        )
    }

    /// RC2-CNCL9/10: a card's own buttons: Stop while it answers; Continue it
    /// (or Ask again) once stopped; Retry and Remove from council when it
    /// didn't answer — plus, only when the owner must act outside the
    /// council (signed out, a key missing), the one red action.
    fn council_card_actions(
        &self,
        page: &CouncilPage,
        r: &awili_council::Round,
        t: &awili_council::SeatTurn,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        use awili_council::TurnStatus;
        let (round, member) = (r.number, t.member.clone());
        let owner = self.sidebar.active_id().to_string();
        let button = |id: String, label: SharedString| {
            crate::ui::Button::new(SharedString::from(id), label).ghost()
        };
        let mut row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(theme::z(theme::SP_4));
        match &t.status {
            // RC2-R2F4: a running card's Stop is in its header.
            TurnStatus::Running => return None,
            TurnStatus::Stopped => {
                let label = if t.text.trim().is_empty() {
                    "Ask again"
                } else {
                    "Continue it"
                };
                let selector = continue_selector(round, &member);
                row = row.child(council_button(
                    selector.clone(),
                    button(selector, label.into()).on_click(cx.listener(
                        move |this, _ev, _w, cx| this.council_continue_member(round, &member, cx),
                    )),
                ));
            }
            TurnStatus::Failed {
                needs_owner,
                detail,
                ..
            } => {
                // S3: the category's plain title and the run id (the first 8
                // characters of the turn id, also in the log line).
                let classified = crate::run_error::classify(detail);
                let run_id = crate::run_error::run_id_of_token(&t.id);
                let id_selector = format!("council-run-id-{round}-{member}");
                row = row.child(
                    div()
                        .debug_selector({
                            let s = id_selector.clone();
                            move || s.clone()
                        })
                        .child(crate::ui::label(format!(
                            "{} \u{b7} Run id {run_id}",
                            classified.category.title()
                        ))),
                );
                let copy_id = format!("council-run-id-copy-{round}-{member}");
                row = row.child(council_button(
                    copy_id.clone(),
                    button(copy_id, "Copy run id".into()).on_click(cx.listener(
                        move |this, _ev, _w, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(run_id.clone()));
                            this.show_info_toast("Run id copied", cx);
                        },
                    )),
                ));
                if *needs_owner {
                    let route = t.route.clone();
                    let selector = signin_selector(round, &member);
                    row = row.child(
                        div()
                            .flex_none()
                            .debug_selector(move || selector.clone())
                            .child(
                                crate::ui::Chip::new(
                                    SharedString::from(format!(
                                        "council-signin-chip-{round}-{member}"
                                    )),
                                    SharedString::from(format!("Sign in to {}", t.label)),
                                )
                                .tone(theme::Tone::ActNow)
                                .on_click(cx.listener(
                                    move |this, _ev, _w, cx| {
                                        if this.mode == crate::work::Mode::Council
                                            && this.sidebar.active_id() == owner
                                        {
                                            this.show_sticky_toast(sign_in_help(&route), cx);
                                        }
                                    },
                                )),
                            )
                            .into_any_element(),
                    );
                }
                let m = member.clone();
                let retry = retry_selector(round, &member);
                let remove = remove_selector(round, &member);
                row = row
                    .child(council_button(
                        retry.clone(),
                        button(retry, "Retry".into()).on_click(cx.listener(
                            move |this, _ev, _w, cx| this.council_retry_member(round, &m, cx),
                        )),
                    ))
                    .child(council_button(
                        remove.clone(),
                        button(remove, "Remove from council".into()).on_click(cx.listener(
                            move |this, _ev, _w, cx| this.council_remove_member(&member, cx),
                        )),
                    ));
            }
            // RC2-CNCL14: an answer ends in action: Copy, or Continue in Work.
            TurnStatus::Answered => {
                // RC2-CNCLF1: a plan waiting for the owner's OK — Work's
                // plan card: "Make the changes" or "Don't change anything".
                if page.plans.contains_key(&t.id) {
                    let (ma, md) = (member.clone(), member.clone());
                    row = row
                        .child(council_button(
                            plan_approve_selector(round, &member),
                            crate::ui::Button::new(
                                SharedString::from(plan_approve_selector(round, &member)),
                                crate::plan_gate::PLAN_APPROVE_LABEL,
                            )
                            .primary()
                            .on_click(cx.listener(
                                move |this, _ev, _w, cx| this.council_plan_approve(round, &ma, cx),
                            )),
                        ))
                        .child(council_button(
                            plan_decline_selector(round, &member),
                            button(
                                plan_decline_selector(round, &member),
                                crate::plan_gate::PLAN_DENY_LABEL.into(),
                            )
                            .on_click(cx.listener(
                                move |this, _ev, _w, cx| this.council_plan_decline(round, &md, cx),
                            )),
                        ));
                }
                let text = t.body().to_string();
                let m = member.clone();
                let copy = copy_selector(round, &member);
                let work = continue_in_work_selector(round, &member);
                row = row
                    .child(council_button(
                        copy.clone(),
                        button(copy, "Copy".into()).on_click(cx.listener(
                            move |this, _ev, _w, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    text.clone(),
                                ));
                                this.show_info_toast("Copied.", cx);
                            },
                        )),
                    ))
                    .child(council_button(
                        work.clone(),
                        button(work, "Continue in Work".into()).on_click(cx.listener(
                            move |this, _ev, window, cx| {
                                this.council_continue_in_work(round, &m, window, cx)
                            },
                        )),
                    ));
            }
        }
        Some(row.into_any_element())
    }

    /// RC2-CNCLF2: Work's approval card on a member's card: what the step
    /// is, then Allow once, Deny, and the broader grants the step allows
    /// ("for this answer" — the member's tool engine lives for one answer —
    /// and "always" for steps that can be undone).
    fn council_approval_box(
        &self,
        round: u32,
        t: &awili_council::SeatTurn,
        card: &crate::work::ApprovalCard,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use awili_gateway::run_events::ApprovalDecision as D;
        let mut row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(theme::z(theme::SP_4));
        for (decision, label) in [
            (D::Approve, "Allow once"),
            (D::Deny, "Deny"),
            (D::Session, "Allow for this answer"),
            (D::Always, "Allow always"),
        ] {
            if !card.offers(decision, false) {
                continue;
            }
            let id = approval_selector(round, &t.member, decision.scope_str());
            let turn_id = t.id.clone();
            let primary = decision == D::Approve;
            let button =
                crate::ui::Button::new(SharedString::from(format!("council-approval-{id}")), label);
            let button = if primary {
                button.primary()
            } else {
                button.ghost()
            };
            row = row.child(council_button(
                id,
                button.on_click(cx.listener(move |this, _ev, _w, cx| {
                    this.council_answer_approval(&turn_id, decision, cx)
                })),
            ));
        }
        crate::ui::Card::new(SharedString::from(format!(
            "council-approval-card-{}",
            t.id
        )))
        .tone(theme::Tone::ActNow)
        .children([
            crate::ui::label(crate::work::approval_heading(card))
                .tone(theme::Tone::ActNow)
                .strong()
                .wrap()
                .into_any_element(),
            crate::ui::ui_text(SharedString::from(card.prompt.clone()))
                .wrap()
                .into_any_element(),
            row.into_any_element(),
        ])
        .into_any_element()
    }

    /// Open or close one card's Details.
    pub(crate) fn council_toggle_details(&mut self, turn_id: &str, cx: &mut Context<Self>) {
        let conv = self.sidebar.active_id().to_string();
        if let Some(page) = self.councils.get_mut(&conv) {
            if !page.details_open.remove(turn_id) {
                page.details_open.insert(turn_id.to_string());
            }
        }
        cx.notify();
    }

    /// Show or fold an earlier round's answers.
    pub(crate) fn council_toggle_round(&mut self, round: u32, cx: &mut Context<Self>) {
        let conv = self.sidebar.active_id().to_string();
        if let Some(page) = self.councils.get_mut(&conv) {
            if !page.expanded.remove(&round) {
                page.expanded.insert(round);
            }
        }
        cx.notify();
    }

    /// RC2-CNCL11: under the latest round: "Let them answer each other"
    /// (after a reply round: "Another round"), and how many rounds they
    /// answer each other on their own after each message.
    fn council_next_row(&self, page: &CouncilPage, cx: &mut Context<Self>) -> AnyElement {
        use awili_council::{RoundKind, TurnStatus};
        let last = page
            .council
            .rounds
            .iter()
            .rev()
            .find(|r| r.kind != RoundKind::SumUp);
        let answered = last.map_or(0, |r| {
            r.turns
                .iter()
                .filter(|t| t.status == TurnStatus::Answered)
                .count()
        });
        let after_reply = last.is_some_and(|r| r.kind == RoundKind::Respond);
        let mut row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .pt(theme::z(theme::SP_4));
        if answered >= 2 {
            row = row
                .child(council_button(
                    RESPOND_SELECTOR.to_string(),
                    crate::ui::Button::new(
                        "council-respond",
                        if after_reply {
                            "Another round"
                        } else {
                            "Let them answer each other"
                        },
                    )
                    .primary()
                    .on_click(cx.listener(|this, _ev, _w, cx| this.council_answer_each_other(cx))),
                ))
                .child(
                    div().child(
                        crate::ui::ui_text(SharedString::from(respond_hint(answered)))
                            .dim()
                            .wrap(),
                    ),
                );
        }
        // RC2-CNCL13: "Ask one to sum up", then who.
        if answered >= 1 {
            row = row.child(council_button(
                SUM_UP_SELECTOR.to_string(),
                crate::ui::Button::new("council-sumup", "Ask one to sum up")
                    .ghost()
                    .on_click(cx.listener(|this, _ev, _w, cx| this.council_toggle_sum_up(cx))),
            ));
        }
        if page.sum_up_open {
            let mut who = div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(theme::z(theme::SP_4))
                .child(crate::ui::ui_text("Who sums up?").dim());
            for m in page.council.active_members() {
                let id = m.id.clone();
                let selector = sum_up_member_selector(&m.id);
                who = who.child(council_button(
                    selector.clone(),
                    crate::ui::Button::new(
                        SharedString::from(selector),
                        SharedString::from(m.label.clone()),
                    )
                    .ghost()
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.council_sum_up(&id, cx))),
                ));
            }
            row = row.child(who);
        }
        let mut auto = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(theme::SP_4))
            .child(
                crate::ui::ui_text("Answer each other automatically:")
                    .dim()
                    .wrap(),
            );
        for n in 0..=MAX_AUTO_ROUNDS {
            let on = page.auto_rounds == n;
            auto = auto.child(council_button(
                auto_selector(n),
                crate::ui::Button::new(
                    SharedString::from(auto_selector(n)),
                    if n == 0 {
                        "Off".to_string()
                    } else {
                        n.to_string()
                    },
                )
                .selected(on)
                .on_click(
                    cx.listener(move |this, _ev, _w, cx| this.council_set_auto_rounds(n, cx)),
                ),
            ));
        }
        row.child(auto).into_any_element()
    }

    /// The width the Council page's centre has right now.
    pub(crate) fn council_width(&self) -> f32 {
        let side = f32::from(theme::z(theme::STAGE_LEFT));
        let w = if self.window_w > 0. {
            self.window_w
        } else {
            1280.
        };
        (w - side).max(0.)
    }

    /// RC2-CNCL6/8: every round: the owner's message, its header with a
    /// one-line summary, who was left out, and the cards — side by side on a
    /// wide window, stacked on a narrow one. When a newer round exists, the
    /// older rounds fold to their header line ("show" opens one).
    fn council_rounds(&self, page: &CouncilPage, cx: &mut Context<Self>) -> AnyElement {
        let mut col = div()
            .flex()
            .flex_col()
            .gap(theme::z(theme::SP_12))
            .px(theme::z(theme::SP_20))
            .py(theme::z(theme::SP_16));
        let last = page.council.rounds.last().map(|r| r.number);
        let width = self.council_width();
        for r in rounds_in_order(&page.council) {
            if let Some(text) = &r.owner_text {
                col = col.child(
                    div().flex().flex_row().justify_end().child(
                        div()
                            .max_w(theme::z(theme::READING_MEASURE))
                            .px(theme::z(theme::SP_12))
                            .py(theme::z(theme::SP_8))
                            .rounded(theme::z(theme::RADIUS_M))
                            .bg(theme::palette().contrast_soft)
                            .child(crate::ui::body(SharedString::from(text.clone())).wrap()),
                    ),
                );
            }
            let n = r.number;
            let folded = Some(n) != last && !page.expanded.contains(&n) && !r.is_running();
            let mut head = div()
                .debug_selector(move || round_selector(n))
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(theme::z(theme::SP_8))
                .text_size(theme::z(theme::type_::UI.size))
                .text_color(theme::palette().text_dim)
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::palette().text)
                        .child(SharedString::from(round_title(r))),
                );
            if !r.turns.is_empty() {
                // RC2-CNCL12: the round's time and cost.
                if !r.is_running() {
                    head = head.child(SharedString::from(crate::run_footer::council_round_tally(
                        r,
                    )));
                }
            }
            if Some(n) != last && !r.turns.is_empty() {
                head = head.child(council_button(
                    fold_selector(n),
                    crate::ui::Button::new(
                        SharedString::from(format!("council-fold-{n}")),
                        if folded { "show" } else { "fold" },
                    )
                    .quiet()
                    .on_click(
                        cx.listener(move |this, _ev, _w, cx| this.council_toggle_round(n, cx)),
                    ),
                ));
            }
            col = col.child(head);
            if !r.turns.is_empty() {
                col = col.child(
                    crate::ui::Row::new(format!("council-summary-{n}"))
                        .title("Summary")
                        .subtitle(awili_council::round_summary(r))
                        .wrap()
                        .pointer_only(),
                );
            }
            for line in &r.left_out {
                col = col.child(
                    crate::ui::ui_text(SharedString::from(line.clone()))
                        .dim()
                        .wrap(),
                );
            }
            if folded {
                continue;
            }
            let layout = card_layout(width, theme::ui_scale(), r.turns.len());
            let mut cards = match layout {
                CardLayout::Columns { .. } => div().flex().flex_row().flex_wrap(),
                CardLayout::Stacked => div().flex().flex_col(),
            }
            .gap(theme::z(CARD_GAP));
            let card_w = match layout {
                CardLayout::Columns { card_w, .. } => Some(card_w),
                CardLayout::Stacked => None,
            };
            for t in &r.turns {
                cards = cards.child(self.council_card(page, r, t, card_w, cx));
            }
            col = col.child(cards);
        }
        // RC2-CNCL11: what to do next, once nobody is answering.
        if !page.council.is_running() {
            col = col.child(self.council_next_row(page, cx));
        }
        col.into_any_element()
    }

    /// The Council page's centre: member bar, rounds, message box.
    pub(crate) fn council_center(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let page = self.active_council();
        let content = match page {
            Some(p) if !p.council.rounds.is_empty() => self.council_rounds(p, cx),
            _ => self.council_empty_state(page),
        };
        let compact_header = gpui::px(self.window_w) < theme::z(theme::COMPACT_W);
        let body = div()
            .id("council-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .when_some(page, |d, p| d.track_scroll(&p.scroll))
            .when(compact_header, |d| d.child(self.council_bar(page, cx)))
            .child(content);
        // RC2-CNCL9: while members answer: how many, Esc, and Stop all
        // (RC2-CNCLH1: those still waiting for the memory lookup count).
        let answering = page.map(|p| p.answering()).filter(|n| *n > 0).map(|n| {
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(theme::z(theme::SP_8))
                .px(theme::z(theme::SP_20))
                .py(theme::z(theme::SP_4))
                .debug_selector(answering_hint_selector)
                .child(
                    div().flex_1().child(
                        crate::ui::ui_text(SharedString::from(answering_hint(n)))
                            .dim()
                            .wrap(),
                    ),
                )
                .child(council_button(
                    STOP_ALL_SELECTOR.to_string(),
                    crate::ui::Button::new("council-stop-all", "Stop all")
                        .ghost()
                        .on_click(cx.listener(|this, _ev, _w, cx| {
                            this.council_stop_all(cx);
                        })),
                ))
        });
        let note = page.and_then(|p| p.note.clone()).map(|n| {
            div()
                .debug_selector(|| NOTE_SELECTOR.to_string())
                .flex_none()
                .px(theme::z(theme::SP_20))
                .py(theme::z(theme::SP_4))
                .child(crate::ui::ui_text(SharedString::from(n)).dim().wrap())
        });
        let sheet = self.council_picker_sheet(window, cx);
        div()
            .debug_selector(|| "awili-center".to_string())
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .when(!compact_header, |d| d.child(self.council_bar(page, cx)))
            .child(body)
            .children(answering)
            .children(note)
            .child(self.composer_bar(cx))
            .children(sheet)
            .into_any_element()
    }
}

/// "Round 2 · answering each other", "Summary", "Round 1".
pub(crate) fn round_title(r: &awili_council::Round) -> String {
    match r.kind {
        awili_council::RoundKind::Ask => format!("Round {}", r.number),
        awili_council::RoundKind::Respond => {
            format!("Round {} \u{00B7} answering each other", r.number)
        }
        // RC2-CNCL13: the summary names who wrote it.
        awili_council::RoundKind::SumUp => match r.turns.first() {
            Some(t) => format!("Summary by {}", t.label),
            None => "Summary".to_string(),
        },
    }
}

/// A member's card in a round (`debug_selector`).
pub(crate) fn card_selector(round: u32, member: &str) -> String {
    format!("council-card-{round}-{member}")
}

/// A round's header (`debug_selector`).
pub(crate) fn round_selector(round: u32) -> String {
    format!("council-round-{round}")
}

/// The note line above the message box (`debug_selector`).
pub(crate) const NOTE_SELECTOR: &str = "council-note";

/// RC2-CNCL8: the gap between cards (points).
pub(crate) const CARD_GAP: f32 = 12.;
/// The narrowest a card in a column gets (points).
pub(crate) const CARD_MIN_W: f32 = 230.;
/// At most this many cards in one row; more wrap to the next.
pub(crate) const CARDS_PER_ROW: usize = 4;

/// How a round's cards sit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CardLayout {
    /// Side by side, `per_row` to a row, each `card_w` pixels wide.
    Columns { per_row: usize, card_w: f32 },
    /// One under the other, full width.
    Stacked,
}

/// Side by side when at least two cards of [`CARD_MIN_W`] fit in the
/// centre's `width` (pixels, at zoom `scale`); stacked otherwise.
pub(crate) fn card_layout(width: f32, scale: f32, cards: usize) -> CardLayout {
    let pad = 40. * scale;
    let gap = CARD_GAP * scale;
    let avail = (width - pad).max(0.);
    let narrow = width + theme::STAGE_LEFT * scale < theme::COMPACT_W * scale;
    if narrow && cards >= 2 && avail >= theme::COUNCIL_TRACK_MIN_W * scale.max(1.) * 2. + gap {
        return CardLayout::Columns {
            per_row: 2,
            card_w: (avail - gap) / 2.,
        };
    }
    let fit = ((avail + gap) / (CARD_MIN_W * scale + gap)).floor() as usize;
    if cards < 2 || fit < 2 {
        return CardLayout::Stacked;
    }
    let per_row = cards.min(fit).min(CARDS_PER_ROW);
    let card_w = (avail - gap * (per_row as f32 - 1.)) / per_row as f32;
    CardLayout::Columns { per_row, card_w }
}

/// A card's dot colour, the words it shows and its foot (`cut`: Awili closed
/// while it answered, RC2-CNCLF4).
///
/// RC2-R62 (usability RC2 round 5, Minor 2: `[[awili:plan-ready]]` showed on
/// its own line while a Claude card streamed): the plan marker is never
/// shown, whole or half-written at the end, while a member answers or once
/// it stopped. (A settled answer is already clean: `take_plan_marker`.)
pub(crate) fn card_parts(t: &awili_council::SeatTurn, cut: bool) -> (gpui::Rgba, String, String) {
    use awili_council::TurnStatus;
    let shown = || awili_code::hide_plan_marker(&t.text, true).into_owned();
    let empty = shown().trim().is_empty();
    match &t.status {
        TurnStatus::Running => (
            theme::tone_color(theme::Tone::Neutral),
            shown(),
            String::new(),
        ),
        TurnStatus::Answered => (
            theme::palette().text_dim,
            t.body().to_string(),
            String::new(),
        ),
        // RC2-CNCLF4: cut off when Awili closed.
        TurnStatus::Stopped if cut && empty => (
            theme::palette().text_dim,
            CUT_BEFORE_WORDS.to_string(),
            CUT_FOOT.into(),
        ),
        TurnStatus::Stopped if cut => (theme::palette().text_dim, shown(), CUT_FOOT.into()),
        TurnStatus::Stopped if empty => (
            theme::palette().text_dim,
            "Stopped before it wrote anything.".to_string(),
            "stopped".into(),
        ),
        TurnStatus::Stopped => (theme::palette().text_dim, shown(), "stopped".into()),
        TurnStatus::Failed { plain, .. } => (card_tone(t), plain.clone(), String::new()),
    }
}

/// RC2-R616: [`card_wait_line`] with the time the member has waited, once
/// it is [`crate::work::ALIVE_AFTER`] or more (`now`: Unix seconds).
pub(crate) fn card_wait_shown(t: &awili_council::SeatTurn, now: i64) -> String {
    let line = card_wait_line(t);
    let waited = now.saturating_sub(t.started_ts);
    if waited >= crate::work::ALIVE_AFTER.as_secs() as i64 {
        format!(
            "{line} \u{b7} {}",
            crate::executor_status::fmt_silence(std::time::Duration::from_secs(waited as u64))
        )
    } else {
        line
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// What a card says before a member's first word: a member that only
/// shows its whole answer at the end (GLM, DeepSeek) says so.
pub(crate) fn card_wait_line(t: &awili_council::SeatTurn) -> String {
    let buffers = match crate::work::WorkExecutor::from_persist(&t.route) {
        Some(crate::work::WorkExecutor::Agent { engine, .. }) => engine.buffers_whole_output(),
        _ => false,
    };
    if buffers {
        format!(
            "Working. {} shows its whole answer when it is done.",
            t.label
        )
    } else {
        "Answering\u{2026}".to_string()
    }
}

fn secs(ms: u64) -> String {
    format!("{:.1} s", ms as f64 / 1000.)
}

/// RC2-R2F4 (usability RC2 round 2, F6: raw "**ZFS**" and "[docs](…)" on
/// Codex and Gemma cards): what a card draws for an answer — its markdown
/// read as Chat reads a reply.
pub(crate) fn card_body_plan(text: &str) -> Vec<crate::markdown_plan::PlanBlock> {
    crate::markdown_plan::plan_blocks(
        &crate::markdown::parse_or_plain(text),
        &crate::markdown_ui::assistant(),
    )
}

/// A card's Details, in order: where it ran, what it was sent, words in and
/// out, time to the first word, and what went wrong when it didn't answer.
pub(crate) fn card_details(
    r: &awili_council::Round,
    t: &awili_council::SeatTurn,
    sent: Option<&SentFacts>,
) -> Vec<(&'static str, String)> {
    use awili_council::{RoundKind, TurnStatus};
    let mut sent_line = match r.kind {
        RoundKind::Ask if r.number == 1 => "your message".to_string(),
        RoundKind::Ask => "your message and this council\u{2019}s earlier answers".to_string(),
        RoundKind::Respond => "your last message and the other members\u{2019} answers".to_string(),
        RoundKind::SumUp => "the whole council, to sum up".to_string(),
    };
    if sent.is_some_and(|s| s.older_left_out) {
        sent_line.push_str(" (older answers left out to fit)");
    }
    let mut out = vec![
        ("Route", t.route.clone()),
        ("It was sent", sent_line),
        // RC2-CNCLF1: what it can use, in plain words.
        (
            "Tools",
            sent.map(|s| s.tools.clone())
                .unwrap_or_else(|| crate::council_exec::tools_line(&t.route, None)),
        ),
    ];
    out.push((
        "Words in / out",
        match t.usage {
            Some((i, o)) => format!("{i} / {o}"),
            None => "not reported".to_string(),
        },
    ));
    out.push((
        "First word after",
        t.first_token_ms
            .map(secs)
            .unwrap_or_else(|| "\u{2014}".into()),
    ));
    if let Some(ms) = t.elapsed_ms {
        out.push(("Time", secs(ms)));
    }
    if let TurnStatus::Failed { detail, .. } = &t.status {
        out.push(("What happened", detail.clone()));
    }
    if let Some(s) = sent {
        out.push(("Fingerprint", s.fingerprint.clone()));
    }
    out
}

/// RC2-R2F5: how many tools the quiet "Used:" line names before "+N more".
const TOOLS_SHOWN: usize = 3;

/// RC2-R2F5 (usability RC2 round 2, F8: no card said which tool a member
/// used): the quiet line under an answer — "Used: Read notes.txt · ls -la"
/// — or `None` when it used none.
pub(crate) fn tools_used_line(page: &CouncilPage, turn_id: &str) -> Option<String> {
    let used = page.tools_used.get(turn_id).filter(|u| !u.is_empty())?;
    let mut line = format!(
        "Used: {}",
        used.iter()
            .take(TOOLS_SHOWN)
            .map(|u| awili_code::plain_step(u))
            .collect::<Vec<_>>()
            .join(" \u{00B7} ")
    );
    if used.len() > TOOLS_SHOWN {
        line.push_str(&format!(" \u{00B7} +{} more", used.len() - TOOLS_SHOWN));
    }
    Some(line)
}

/// RC2-R2F5: a card's Details, with every tool it used.
pub(crate) fn card_details_with_tools(
    page: &CouncilPage,
    r: &awili_council::Round,
    t: &awili_council::SeatTurn,
) -> Vec<(&'static str, String)> {
    let mut out = card_details(r, t, page.sent.get(&t.id));
    if let Some(used) = page.tools_used.get(&t.id).filter(|u| !u.is_empty()) {
        let at = out
            .iter()
            .position(|(k, _)| *k == "Tools")
            .map_or(out.len(), |i| i + 1);
        out.insert(
            at,
            (
                "Tools used",
                used.iter()
                    .map(|u| awili_code::plain_step(u))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        );
    }
    out
}

#[cfg(test)]
thread_local! {
    /// RC2-R2G9: the layout of every council header tally drawn on this
    /// thread, in draw order (a test clears it, then reads what a frame
    /// drew).
    pub(crate) static DRAWN_TALLIES: std::cell::RefCell<Vec<gpui::TextLayout>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// RC2-R2G9: the header tallies drawn since [`DRAWN_TALLIES`] was cleared
/// (one never laid out drew nothing and is left out).
#[cfg(test)]
pub(crate) fn drawn_tallies() -> Vec<String> {
    DRAWN_TALLIES.with(|d| {
        d.borrow()
            .iter()
            .filter_map(|layout| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| layout.text())).ok()
            })
            .collect()
    })
}

/// RC2-R2F5: a card's "Used:" line (`debug_selector`).
pub(crate) fn tools_used_selector(round: u32, member: &str) -> String {
    format!("council-tools-used-{round}-{member}")
}

/// A card's waiting line (`debug_selector`).
pub(crate) fn wait_selector(round: u32, member: &str) -> String {
    format!("council-wait-{round}-{member}")
}

/// A streaming card's caret (`debug_selector`).
pub(crate) fn caret_selector(round: u32, member: &str) -> String {
    format!("council-caret-{round}-{member}")
}

/// A card's Details toggle (`debug_selector`).
pub(crate) fn details_selector(round: u32, member: &str) -> String {
    format!("council-details-{round}-{member}")
}

/// RC2-CNCL9: a card's Stop (`debug_selector`).
pub(crate) fn stop_selector(round: u32, member: &str) -> String {
    format!("council-stop-{round}-{member}")
}

/// RC2-CNCL9: a stopped card's Continue it (`debug_selector`).
pub(crate) fn continue_selector(round: u32, member: &str) -> String {
    format!("council-continue-{round}-{member}")
}

/// RC2-CNCL9: the "N models answering" line (`debug_selector`).
pub(crate) fn answering_hint_selector() -> String {
    "council-answering".to_string()
}

/// RC2-CNCL10: a failed card's Retry (`debug_selector`).
pub(crate) fn retry_selector(round: u32, member: &str) -> String {
    format!("council-retry-{round}-{member}")
}

/// RC2-CNCL10: a failed card's Remove from council (`debug_selector`).
pub(crate) fn remove_selector(round: u32, member: &str) -> String {
    format!("council-remove-{round}-{member}")
}

/// RC2-CNCL10: the red "Sign in to …" (`debug_selector`).
pub(crate) fn signin_selector(round: u32, member: &str) -> String {
    format!("council-signin-{round}-{member}")
}

/// RC2-CNCL10: the palette role for a member that didn't answer: violet
/// (worth a look), red only when the owner must act outside the council.
pub(crate) fn card_tone(t: &awili_council::SeatTurn) -> gpui::Rgba {
    match t.status {
        awili_council::TurnStatus::Failed {
            needs_owner: true, ..
        } => theme::palette().council_act,
        _ => theme::palette().council_look,
    }
}

/// RC2-CNCL10: how to sign a member in again, in plain words.
pub(crate) fn sign_in_help(route: &str) -> String {
    if let Some(crate::work::WorkExecutor::Agent { engine, .. }) =
        crate::work::WorkExecutor::from_persist(route)
    {
        if let Some(i) = crate::harness_setup::RECIPES
            .iter()
            .position(|(e, _, _)| *e == engine)
        {
            return crate::harness_setup::GUIDES[i].0.to_string();
        }
    }
    "Check its address and key in Settings, then press Retry.".to_string()
}

/// RC2-CNCL11: a reply's stance tag (`debug_selector`).
pub(crate) fn stance_selector(round: u32, member: &str) -> String {
    format!("council-stance-{round}-{member}")
}

/// RC2-CNCL11: "Let them answer each other" (`debug_selector`).
pub(crate) const RESPOND_SELECTOR: &str = "council-respond";

/// RC2-CNCL11: an "answer each other automatically" choice (`debug_selector`).
pub(crate) fn auto_selector(n: u8) -> String {
    format!("council-auto-{n}")
}

/// RC2-CNCL11: the hint beside "Let them answer each other".
pub(crate) fn respond_hint(answered: usize) -> String {
    if answered == 2 {
        "Each of the 2 reads the other\u{2019}s answer and replies.".to_string()
    } else {
        format!("Each of the {answered} reads the others\u{2019} answers and replies.")
    }
}

/// RC2-CNCL12: a card's footer (`debug_selector`).
pub(crate) fn footer_selector(round: u32, member: &str) -> String {
    format!("council-footer-{round}-{member}")
}

/// RC2-CNCL12: the council's tally in the member bar (`debug_selector`).
pub(crate) const TALLY_SELECTOR: &str = "council-tally";

/// RC2-CNCL13: "Ask one to sum up" (`debug_selector`).
pub(crate) const SUM_UP_SELECTOR: &str = "council-sumup";

/// RC2-CNCL13: one member in "Who sums up?" (`debug_selector`).
pub(crate) fn sum_up_member_selector(member: &str) -> String {
    format!("council-sumup-{member}")
}

/// RC2-CNCL14: an answer's Copy (`debug_selector`).
pub(crate) fn copy_selector(round: u32, member: &str) -> String {
    format!("council-copy-{round}-{member}")
}

/// RC2-CNCL14: an answer's Continue in Work (`debug_selector`).
pub(crate) fn continue_in_work_selector(round: u32, member: &str) -> String {
    format!("council-work-{round}-{member}")
}

/// RC2-CNCLF2: one choice on a member's approval card (`debug_selector`;
/// `scope` is the choice's scope word: once, deny, session, always).
pub(crate) fn approval_selector(round: u32, member: &str, scope: &str) -> String {
    format!("council-approve-{round}-{member}-{scope}")
}

/// RC2-CNCLF1: a plan's "Make the changes" (`debug_selector`).
pub(crate) fn plan_approve_selector(round: u32, member: &str) -> String {
    format!("council-plan-ok-{round}-{member}")
}

/// RC2-CNCLF1: a plan's "Don't change anything" (`debug_selector`).
pub(crate) fn plan_decline_selector(round: u32, member: &str) -> String {
    format!("council-plan-no-{round}-{member}")
}

/// RC2-CNCL9: the Stop all button (`debug_selector`).
pub(crate) const STOP_ALL_SELECTOR: &str = "council-stop-all";

/// RC2-CNCL9: the hint while members answer.
pub(crate) fn answering_hint(n: usize) -> String {
    let who = if n == 1 {
        "1 model answering".to_string()
    } else {
        format!("{n} models answering")
    };
    format!("{who} \u{00B7} Esc stops all \u{00B7} You can type your next message now")
}

/// An earlier round's show/fold link (`debug_selector`).
pub(crate) fn fold_selector(round: u32) -> String {
    format!("council-fold-{round}")
}

/// RC2-R3 (usability RC2 round 3, Major 3: the summary card sat at the top
/// of the page, above Round 1, while it was written, when done and after a
/// relaunch; was RC2-CNCL13's "a summary just written leads the page"): the
/// order the page shows a council's rounds in \u{2014} by number, so a summary
/// comes right after the round it summed up, as it is kept.
pub(crate) fn rounds_in_order(c: &awili_council::Council) -> Vec<&awili_council::Round> {
    let mut ordered: Vec<&awili_council::Round> = c.rounds.iter().collect();
    ordered.sort_by_key(|r| r.number);
    ordered
}

// ---- RC2-CNCL15: a council comes back after a relaunch -----------------------------

/// Rebuild a council from what `awili.db` kept: its seats (removed ones
/// marked), every round and turn — status, stance, plain and raw error,
/// time, words, cost — with each answer's text and each owner message.
pub(crate) fn page_from_rows(
    members: &[awili_persist::CouncilMemberRow],
    turns: &[awili_persist::CouncilTurnRow],
    msgs: &[awili_persist::StoredMessage],
) -> CouncilPage {
    use awili_council::{Member, Round, RoundKind, SeatTurn, Stance, TurnStatus};
    let mut page = CouncilPage::with_group(&[]);
    for m in members {
        page.council.members.push(Member {
            id: m.member_id.clone(),
            route: m.route.clone(),
            label: m.label.clone(),
            position: u32::try_from(m.position).unwrap_or(0),
            removed: m.removed_ts.is_some(),
        });
    }
    let mut by_round: std::collections::BTreeMap<i64, Vec<&awili_persist::CouncilTurnRow>> =
        Default::default();
    for t in turns {
        by_round.entry(t.round).or_default().push(t);
    }
    // A round's turns come back in column order (the order the members
    // were asked and read each other in), not in the order they were saved.
    let position = |member: &str| {
        members
            .iter()
            .find(|m| m.member_id == member)
            .map_or(i64::MAX, |m| m.position)
    };
    for (n, mut rows) in by_round {
        rows.sort_by_key(|t| (position(&t.member_id), t.started_ts));
        let mut r = Round::new(
            u32::try_from(n).unwrap_or(0),
            RoundKind::from_token(&rows[0].round_kind),
        );
        r.owner_ts = rows.iter().find_map(|t| t.owner_ts);
        r.owner_text = r.owner_ts.and_then(|ts| {
            msgs.iter()
                .find(|m| m.ts == ts && m.role == "user")
                .map(|m| m.text.clone())
        });
        for t in rows {
            // RC2-CNCLH3: a round the memory lookup refused: its one line
            // (no member's card).
            if t.status == crate::council_run::REFUSED {
                r.left_out.extend(t.error_plain.clone());
                continue;
            }
            let answer = t.message_id.and_then(|id| msgs.iter().find(|m| m.id == id));
            let status = match t.status.as_str() {
                "answered" => TurnStatus::Answered,
                "failed" => TurnStatus::Failed {
                    plain: t.error_plain.clone().unwrap_or_default(),
                    detail: t.error_detail.clone().unwrap_or_default(),
                    needs_owner: t.needs_owner,
                },
                // RC2-CNCLF4: a turn still answering when Awili closed
                // reads as stopped, cut off by the quit (Retry, Continue).
                _ => {
                    if t.status == crate::council_run::RUNNING {
                        page.cut.insert(t.id.clone());
                    }
                    TurnStatus::Stopped
                }
            };
            if let Some(a) = answer {
                page.answer_ts.insert(t.id.clone(), a.ts.max(0) as u64);
            }
            // RC2-CNCLF3: exactly what it was sent, for Retry and Continue.
            if let (Some(system), Some(text)) = (&t.sent_system, &t.sent_text) {
                page.sent
                    .insert(t.id.clone(), sent_from_saved(t, system, text));
            }
            // RC2-R2F5: the tools it used.
            let used: Vec<String> = t
                .tools_used
                .as_deref()
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect();
            if !used.is_empty() {
                page.tools_used.insert(t.id.clone(), used);
            }
            r.turns.push(SeatTurn {
                id: t.id.clone(),
                member: t.member_id.clone(),
                label: t.label.clone(),
                route: t.route.clone(),
                text: answer.map(|a| a.text.clone()).unwrap_or_default(),
                status,
                stance: t.stance.as_deref().and_then(Stance::from_token),
                usage: t
                    .input_tokens
                    .zip(t.output_tokens)
                    .map(|(i, o)| (i.max(0) as u64, o.max(0) as u64)),
                first_token_ms: t.first_token_ms.map(|v| v.max(0) as u64),
                elapsed_ms: t.elapsed_ms.map(|v| v.max(0) as u64),
                cost_usd: t.cost_usd,
                started_ts: t.started_ts,
            });
        }
        page.council.rounds.push(r);
    }
    page
}

/// RC2-CNCLF3: what a member was sent, from its saved turn: the system text
/// and message exactly as they went (a prompt with no transcript renders its
/// ask as is, so the fingerprint is the one taken at the send), and the
/// fingerprint saved then.
pub(crate) fn sent_from_saved(
    t: &awili_persist::CouncilTurnRow,
    system: &str,
    text: &str,
) -> SentFacts {
    SentFacts {
        older_left_out: text.contains(awili_council::OLDER_LEFT_OUT),
        fingerprint: t.sent_prompt_hash.clone().unwrap_or_default(),
        prompt: awili_council::SeatPrompt {
            member: t.member_id.clone(),
            brief: system.to_string(),
            transcript: Vec::new(),
            ask: text.to_string(),
            older_left_out: false,
        },
        tools: crate::council_exec::tools_line(&t.route, None),
    }
}

/// A saved council, read through the read-only connection; `None` when it
/// never seated anyone (it is then seated with the last group).
pub(crate) fn page_from_store(store: &awili_persist::Store, id: &str) -> Option<CouncilPage> {
    let members = store.council_members(id).ok()?;
    if members.is_empty() {
        return None;
    }
    let turns = store.council_turns(id).ok()?;
    let msgs = store.messages(id).ok()?;
    Some(page_from_rows(&members, &turns, &msgs))
}

/// A council's line under its title in the sidebar: "Claude, Codex, Gemma
/// · 2 rounds".
pub(crate) fn sidebar_line(page: &CouncilPage) -> String {
    let names: Vec<String> = page
        .council
        .active_members()
        .iter()
        .map(|m| m.label.split_whitespace().next().unwrap_or("").to_string())
        .collect();
    let who = if names.len() > 4 {
        format!("{} models", names.len())
    } else {
        names.join(", ")
    };
    match page.council.rounds.len() {
        0 => who,
        1 => format!("{who} \u{00B7} 1 round"),
        n => format!("{who} \u{00B7} {n} rounds"),
    }
}

/// Councils on screen this session, by conversation id.
pub(crate) type CouncilPages = HashMap<String, CouncilPage>;

#[cfg(test)]
mod tests {
    use super::*;

    /// RC2-R62 (usability RC2 round 5, Minor 2): a streaming Claude card
    /// showed `[[awili:plan-ready]]` on its own line. A card never shows the
    /// marker: whole, half-written at the end, or after a Stop.
    #[test]
    fn rc2r62_a_card_never_shows_the_plan_marker() {
        let m = awili_council::Member::new("m1", "agent:claude:sonnet", "Claude Sonnet", 0);
        let mut t = awili_council::SeatTurn::running("t1", &m, 0);
        t.text = "1. Overwrite notes.txt so it contains hi.\n[[awili:plan-ready]]".into();
        let (_, body, _) = card_parts(&t, false);
        assert!(!body.contains("[[awili"), "{body}");
        assert!(body.starts_with("1. Overwrite notes.txt"), "{body}");
        t.text = "1. Overwrite notes.txt.\n[[awili:plan-re".into();
        let (_, body, _) = card_parts(&t, false);
        assert!(!body.contains("[[awili"), "{body}");
        t.text = "[[awili:plan-ready]]".into();
        let (_, body, _) = card_parts(&t, false);
        assert!(body.trim().is_empty(), "nothing to show yet: {body:?}");
        t.text = "1. Overwrite notes.txt.\n[[awili:plan-ready]]".into();
        t.status = awili_council::TurnStatus::Stopped;
        for cut in [false, true] {
            let (_, body, _) = card_parts(&t, cut);
            assert!(!body.contains("[[awili"), "{body}");
        }
        // Ordinary text is shown as written.
        t.status = awili_council::TurnStatus::Running;
        t.text = "See [[wiki links]] and a [b] c".into();
        assert_eq!(card_parts(&t, false).1, "See [[wiki links]] and a [b] c");
    }

    fn engine() -> awili_config::EngineConfig {
        awili_config::EngineConfig {
            label: "Gemma 4 26B".into(),
            ..Default::default()
        }
    }

    /// Every route the picker offers reads as a plain name.
    #[test]
    fn routes_read_as_plain_names() {
        let e = engine();
        for (route, want) in [
            ("agent:claude:sonnet", "Claude Sonnet"),
            ("agent:claude:haiku", "Claude Haiku"),
            ("agent:claude", "Claude"),
            ("agent:codex", "Codex"),
            ("agent:glm", "GLM"),
            ("agent:dsh:deepseek-flash", "DeepSeek Flash"),
            ("provider:fleet", "Hermes"),
            ("in-app", "Gemma 4 26B"),
        ] {
            assert_eq!(route_label(route, &e, &[]), want, "{route}");
        }
        assert_eq!(
            group_from_json(r#"[["agent:codex","Codex"],["in-app","Gemma 4 26B"]]"#),
            Some(vec![
                ("agent:codex".to_string(), "Codex".to_string()),
                ("in-app".to_string(), "Gemma 4 26B".to_string())
            ])
        );
        assert_eq!(default_group(&e).len(), 3);
        assert_eq!(group_from_json("not json"), None);
        assert_eq!(group_from_json("[]"), None);
        assert_eq!(
            goes_to_line(&["Codex".into(), "Gemma 4 26B".into()]),
            "Goes to Codex and Gemma 4 26B"
        );
    }
}
