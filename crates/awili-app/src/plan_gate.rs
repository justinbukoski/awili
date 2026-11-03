//! U4e1 — the plan gate (spec D9–D11): an Ask turn on Claude or Codex runs
//! as a read-only planning step. When that step ends cleanly with the
//! [`awili_code::PLAN_READY_MARKER`] line, the marker is stripped from the
//! reply and a plan card opens: "Make the changes" or "Don't change anything".
//!
//! [`PlanGates`] is the pure core: which planning steps are armed, which plans
//! wait for a decision, and the ONE approved follow-up. [`next_turn`] decides
//! each agent turn's `Autonomy` over it. The `impl AwiliApp` block owns the
//! transcript side: the finish hook ([`crate::AwiliApp::plan_gate_after_work_turn`]),
//! the shared resolution path ([`crate::AwiliApp::resolve_plan_gate`], called
//! at the top of `resolve_work_approval`) and the card.
//!
//! Approve submits [`awili_code::PLAN_APPROVED_TEXT`] through the normal
//! `submit_work_run_in` (user row, instructions preflight, ledger, run_seq,
//! session resume) as ONE full-access turn. Deny pushes
//! [`awili_code::PLAN_DECLINED_TEXT`] and runs nothing. A new message or a
//! Stop clears the conversation's plan.

use std::collections::{HashMap, HashSet};

use awili_code::{Autonomy, CodeEngine};
use awili_work::ApprovalMode;

use crate::ask_mode::{autonomy_for, effective, AskKey, AskSupport};
use crate::theme;
use crate::work::{pill_button, WorkExecutor};
use awili_gateway::run_events::{ApprovalDecision, RunStatus};
use gpui::{div, prelude::*, Context, FontWeight, SharedString};

/// The executor a plan came from (D11): Approve runs only on the same one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlanGate {
    pub engine: CodeEngine,
    pub model: Option<String>,
}

/// The plan gates of every conversation (U4e1, pure).
#[derive(Clone, Debug, Default)]
pub(crate) struct PlanGates {
    /// Planning steps in flight: dispatched as `AskFirst`, not yet settled.
    armed: HashMap<AskKey, PlanGate>,
    /// Plans waiting for Approve or Deny.
    open: HashMap<AskKey, PlanGate>,
    /// The conversation whose NEXT agent turn is the approved follow-up.
    execute_next: Option<AskKey>,
    /// U4e1-FIX2: the folder each Work planning step ran in (set at dispatch).
    /// U4e1-FIX3: `None` = the step ran with no folder (still a binding).
    folders: HashMap<AskKey, Option<String>>,
    /// R1g: conversations whose open plan was set aside (a new message or a
    /// Stop) before Approve or Deny; their next card says so.
    replaced: HashSet<AskKey>,
    /// RC1-N: open plans whose reply ends with questions for the owner;
    /// their card says so and offers answering first.
    questions: HashSet<AskKey>,
}

impl PlanGates {
    /// A planning step was dispatched for `key`. It replaces any earlier
    /// arming and any plan still open for `key` (the new step supersedes it).
    pub(crate) fn arm(&mut self, key: AskKey, engine: CodeEngine, model: Option<String>) {
        if self.open.remove(&key).is_some() {
            self.replaced.insert(key.clone());
        }
        self.folders.remove(&key);
        self.armed.insert(key, PlanGate { engine, model });
    }

    /// A turn for `key` finished with `final_text`. Always strips the marker
    /// and returns the clean text. Opens a plan (returns `true`) only when
    /// `key` was armed, the turn ended `ok`, and the marker was present. The
    /// arming is consumed either way.
    pub(crate) fn settle(&mut self, key: &AskKey, final_text: &str, ok: bool) -> (String, bool) {
        let (clean, found) = awili_code::take_plan_marker(final_text);
        match self.armed.remove(key) {
            Some(gate) if ok && found => {
                self.open.insert(key.clone(), gate);
                if plan_ends_with_questions(&clean) {
                    self.questions.insert(key.clone());
                } else {
                    self.questions.remove(key);
                }
                (clean, true)
            }
            _ => (clean, false),
        }
    }

    /// RC1-N: whether `key`'s open plan ends with questions for the owner.
    pub(crate) fn has_questions(&self, key: &AskKey) -> bool {
        self.open.contains_key(key) && self.questions.contains(key)
    }

    /// The plan waiting for a decision in `key`, if any.
    pub(crate) fn open_for(&self, key: &AskKey) -> Option<&PlanGate> {
        self.open.get(key)
    }

    /// Approve: removes the plan and releases exactly one follow-up turn.
    pub(crate) fn approve(&mut self, key: &AskKey) -> Option<PlanGate> {
        let gate = self.open.remove(key)?;
        self.replaced.remove(key);
        self.questions.remove(key);
        self.execute_next = Some(key.clone());
        Some(gate)
    }

    /// Deny: removes the plan; nothing is released.
    pub(crate) fn deny(&mut self, key: &AskKey) -> bool {
        self.replaced.remove(key);
        self.questions.remove(key);
        self.open.remove(key).is_some()
    }

    /// A new message or a Stop: drops `key`'s plan and arming. It never
    /// touches `execute_next` (the approved follow-up submits through the
    /// same path that calls this).
    pub(crate) fn clear(&mut self, key: &AskKey) {
        if self.open.remove(key).is_some() {
            self.replaced.insert(key.clone());
        }
        self.questions.remove(key);
        self.armed.remove(key);
        self.folders.remove(key);
    }

    /// `true` once, when `key`'s next turn is the approved follow-up.
    pub(crate) fn take_execute(&mut self, key: &AskKey) -> bool {
        if self.execute_next.as_ref() == Some(key) {
            self.execute_next = None;
            true
        } else {
            false
        }
    }

    /// Record the folder `key`'s planning step runs in (U4e1-FIX2). `None`
    /// records "no folder" — itself a binding (U4e1-FIX3).
    pub(crate) fn bind_folder(&mut self, key: &AskKey, folder: Option<String>) {
        self.folders.insert(key.clone(), folder);
    }

    /// The folder binding of `key`'s plan: `None` when nothing was recorded,
    /// `Some(None)` when the planning step ran with no folder.
    pub(crate) fn folder_for(&self, key: &AskKey) -> Option<Option<&str>> {
        self.folders.get(key).map(Option::as_deref)
    }

    /// R1g: whether `key`'s open plan replaced an earlier plan that was never
    /// approved or declined.
    pub(crate) fn replaced_earlier(&self, key: &AskKey) -> bool {
        self.replaced.contains(key)
    }

    /// Whether a planning step is in flight for `key` (tests).
    #[cfg(test)]
    pub(crate) fn is_armed(&self, key: &AskKey) -> bool {
        self.armed.contains_key(key)
    }
}

/// The `Autonomy` of the next agent turn for `key` (U4e1, pure): the approved
/// follow-up runs `Autonomous` once. Any other turn replaces the open plan,
/// resolves the mode against the engine's support, and arms the gate when it
/// is an enforced planning step.
pub(crate) fn next_turn(
    gates: &mut PlanGates,
    key: AskKey,
    support: AskSupport,
    mode: ApprovalMode,
    engine: CodeEngine,
    model: Option<&str>,
) -> Autonomy {
    if gates.take_execute(&key) {
        return Autonomy::Autonomous;
    }
    gates.clear(&key);
    let autonomy = autonomy_for(effective(mode, support));
    if may_gate(autonomy, engine) {
        gates.arm(key, engine, model.map(str::to_string));
    }
    autonomy
}

/// The plan card's title (D10).
pub(crate) const PLAN_CARD_TITLE: &str = "Ready to make changes";
/// The plan card's body (D10).
pub(crate) const PLAN_CARD_BODY: &str = "The plan is in the reply above. Nothing has changed yet.";
/// The Approve pill (D10).
pub(crate) const PLAN_APPROVE_LABEL: &str = "Make the changes";
/// The Deny pill (D10).
pub(crate) const PLAN_DENY_LABEL: &str = "Don't change anything";
/// The toast after Deny put the request back into the composer (R1c).
pub(crate) const PLAN_DENIED_TOAST: &str =
    "Nothing was changed. Your request is back in the box \u{2014} edit it or press Enter to ask again.";
/// RC1-N: the toast after an in-app Deny (the reply line says what was not
/// done, and which files the turn had changed before it).
pub(crate) const DENY_STOPPED_TOAST: &str =
    "Not done. Your request is back in the box \u{2014} edit it or press Enter to ask again.";
/// The test lookup keys of the Deny pills (R1c).
pub(crate) const PLAN_DENY_SELECTOR: &str = "plan-deny";
pub(crate) const CODE_PLAN_DENY_SELECTOR: &str = "code-plan-deny";
/// R1g: the card's note when this plan replaced one that was never decided.
pub(crate) const PLAN_REPLACED_NOTE: &str =
    "This plan is for your newest message. The earlier plan was set aside and nothing from it was done.";
/// R1g: the test lookup key of the Work plan card (shown above the composer).
pub(crate) const WORK_PLAN_CARD_SELECTOR: &str = "r1g-work-plan-card";
/// R1g: the test lookup key of the card's replaced-plan note.
pub(crate) const PLAN_REPLACED_NOTE_SELECTOR: &str = "r1g-plan-replaced-note";
/// R1g: the test lookup key of the composer's input row (`composer_bar`).
pub(crate) const COMPOSER_ROW_SELECTOR: &str = "r1g-composer-row";

/// RC2-RUN (R3-14): the plan card's title while the plan cannot run here
/// (the conversation moved to another folder, or another helper is picked).
pub(crate) const PLAN_BLOCKED_TITLE: &str = "This plan can't run here";
/// RC2-RUN (R3-14): the test lookup key of the blocked card's words.
pub(crate) const PLAN_BLOCKED_SELECTOR: &str = "rc2-plan-blocked";

/// RC2-RUN (R3-14): the card's words for a refusal: the refusal toast's own
/// sentence, without its "Not run — " lead.
pub(crate) fn plan_blocked_card_text(refusal: &str) -> String {
    let Some(rest) = refusal.strip_prefix("Not run \u{2014} ") else {
        return refusal.to_string();
    };
    let mut chars = rest.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// RC2-RUN (round 4 Minor 10): the user rows Awili writes for a click on
/// the plan card ("Approved — go ahead…", "Declined the plan — …") are shown
/// as a note about the click, never as a bubble the owner seems to have
/// typed. `None` for any other text. RC2-RUNF (review P2): only a row marked
/// as a click ([`awili_core::MessageKind::PlanClick`]) is looked up here —
/// the same words typed by the owner stay a normal bubble.
pub(crate) fn plan_click_note(text: &str) -> Option<String> {
    if text == awili_code::PLAN_APPROVED_TEXT {
        Some(format!("You chose \u{201C}{PLAN_APPROVE_LABEL}\u{201D}."))
    } else if text == awili_code::PLAN_DECLINED_TEXT {
        Some(format!(
            "You chose \u{201C}{PLAN_DENY_LABEL}\u{201D} \u{2014} nothing was changed."
        ))
    } else {
        None
    }
}

/// RC2-RUN (round 4 Minor 10): the test lookup key of a click note.
pub(crate) const PLAN_CLICK_NOTE_SELECTOR: &str = "rc2-plan-click-note";

/// RC1-N: the plan card's title when the plan ends with questions.
pub(crate) const PLAN_QUESTIONS_TITLE: &str = "The plan ends with questions";
/// RC1-N: the plan card's body when the plan ends with questions.
pub(crate) const PLAN_QUESTIONS_BODY: &str =
    "Answer them in the box below first and the plan will \
be updated. Nothing has changed yet.";
/// RC1-N: the first pill of a plan that ends with questions — it puts the
/// cursor in the box.
pub(crate) const PLAN_ANSWER_LABEL: &str = "Answer the questions";
/// RC1-N: the test lookup key of the questions note.
pub(crate) const PLAN_QUESTIONS_SELECTOR: &str = "rc1n-plan-questions";
/// RC1-N: the test lookup keys of the Work card's answer and approve pills.
pub(crate) const PLAN_ANSWER_SELECTOR: &str = "rc1n-plan-answer";
pub(crate) const PLAN_APPROVE_SELECTOR: &str = "rc1n-plan-approve";

/// RC1-N (round 4 M1/M2): whether a plan's reply ends with questions for
/// the owner — its last paragraph asks something other than only whether to
/// start ("Two things to confirm: A) Shut down now, or B) at a set time?").
/// A `?` counts when it ends a sentence (followed by the end, a space, or
/// closing punctuation — never a URL's query), outside code (backticks), not
/// part of `??` and not a ternary operator (pre-review P2, passes 8 and 9).
pub(crate) fn plan_ends_with_questions(text: &str) -> bool {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let start = lines
        .iter()
        .rposition(|l| l.trim().is_empty())
        .map_or(0, |i| i + 1);
    let block = lines[start..].join("\n");
    let chars: Vec<char> = block.chars().collect();
    let mut sentence = String::new();
    let mut in_code = false;
    for (i, &c) in chars.iter().enumerate() {
        if c == '`' {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        sentence.push(c);
        let ends = |c: char| matches!(c, '.' | '!' | ':' | '\n');
        let doubled = |j: Option<usize>| j.and_then(|j| chars.get(j)) == Some(&'?');
        if c == '?' && (doubled(i.checked_sub(1)) || doubled(Some(i + 1))) {
            continue;
        }
        // Pre-review P2 (pass 9): a ternary operator (`cond ? a : b`), its
        // ` : ` later on the same line, is code, not a question.
        // Only the expression right after the `?` — up to the sentence's
        // end or a code span — is looked at (pre-review P2, pass 10), and
        // only an operator `?` (a space before it) can be a ternary: a
        // question's `?` follows its last word (pass 11).
        let operator = i > 0 && chars[i - 1] == ' ';
        if c == '?' && operator {
            let expr: String = chars[i + 1..]
                .iter()
                .take_while(|&&n| !matches!(n, '\n' | '.' | '!' | '?' | '`'))
                .collect();
            if expr.contains(" : ") {
                continue;
            }
        }
        if c == '?' {
            let next = chars.get(i + 1).copied();
            let boundary = next.is_none_or(|n| {
                n.is_whitespace()
                    || matches!(n, ')' | '*' | '_' | '"' | '\'' | '\u{201D}' | '\u{2019}')
            });
            if boundary && !go_ahead_question(&sentence) {
                return true;
            }
            sentence.clear();
        } else if ends(c) {
            sentence.clear();
        }
    }
    false
}

/// RC1-N: a question that only asks whether to start: a permission phrase
/// ("Shall I", "Would you like me to", or none) followed by nothing but a
/// go-ahead ("go ahead", "start now", "make the changes", "proceed with the
/// plan"). Anything that qualifies it — a time, a choice, a detail — makes
/// it an open question: "When should we start?", "Can I start Tuesday?",
/// "Should I proceed with port 8080?" (pre-review P2).
fn go_ahead_question(sentence: &str) -> bool {
    let q = sentence
        .trim()
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    let q = q.trim_end_matches(|c: char| !c.is_alphanumeric());
    let rest = [
        "would you like me to ",
        "do you want me to ",
        "are we ready to ",
        "shall we ",
        "should we ",
        "can we ",
        "may we ",
        "ready for me to ",
        "want me to ",
        "shall i ",
        "should i ",
        "can i ",
        "may i ",
        "ok to ",
        "okay to ",
        "ready to ",
    ]
    .iter()
    .find_map(|p| q.strip_prefix(p))
    .unwrap_or(q);
    let rest = rest.strip_suffix(" now").unwrap_or(rest);
    const GO: [&str; 15] = [
        "start",
        "begin",
        "proceed",
        "go ahead",
        "continue",
        "carry on",
        "make the changes",
        "make these changes",
        "apply the changes",
        "apply these changes",
        "apply them",
        "do it",
        "do this",
        "run it",
        "run the plan",
    ];
    // Pre-review P2: "proceed with" only with the plan itself, never with
    // a detail ("Should I proceed with port 8080?").
    const WITH_PLAN: [&str; 6] = [
        "the plan",
        "this plan",
        "the changes",
        "these changes",
        "the steps",
        "these steps",
    ];
    GO.contains(&rest)
        || ["proceed with ", "go ahead with ", "continue with "]
            .iter()
            .filter_map(|p| rest.strip_prefix(p))
            .any(|what| WITH_PLAN.contains(&what))
}

/// The request a plan answered (R1c): the newest user row that is not an
/// approval or decline record. `None` when there is none.
pub(crate) fn plan_request_text(messages: &[awili_core::Message]) -> Option<String> {
    messages
        .iter()
        .rev()
        .filter(|m| m.role == awili_core::Role::User)
        .map(|m| m.text.trim())
        .find(|t| {
            !t.is_empty()
                && *t != awili_code::PLAN_APPROVED_TEXT
                && *t != awili_code::PLAN_DECLINED_TEXT
        })
        .map(str::to_string)
}

/// What a vendor reply ends with after the marker is stripped. Superseded by
/// [`PlanGates::settle`]; kept only for the first U4e1 tests.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanGateDecision {
    /// The reply carries the plan-ready marker: hold the run and show the plan
    /// approval card (Approve / Discard).
    Hold {
        /// The reply with every marker line removed (what the transcript and
        /// the approval card show).
        clean: String,
    },
    /// No marker: finish the turn normally, marker or not.
    Finish,
}

/// Superseded by [`PlanGates::settle`]; kept only for the first U4e1 tests.
#[cfg(test)]
pub fn gate_reply(autonomy: Autonomy, _engine: CodeEngine, reply: &str) -> PlanGateDecision {
    if !autonomy.plan_first() {
        return PlanGateDecision::Finish;
    }
    let (clean, found) = awili_code::take_plan_marker(reply);
    if found {
        PlanGateDecision::Hold { clean }
    } else {
        PlanGateDecision::Finish
    }
}

/// Whether a vendor turn on `engine` may end in the plan gate: the engine
/// enforces the read-only planning step (Claude/Codex) — GLM and DeepSeek
/// cannot be asked (U4c), and an `Autonomous` turn never gates (D10: only the
/// Ask planning step holds for approval).
pub fn may_gate(autonomy: Autonomy, engine: CodeEngine) -> bool {
    autonomy.plan_first() && engine.enforces_ask()
}

/// Superseded by the D10 card copy ([`PLAN_CARD_TITLE`] and the labels);
/// kept only for the first U4e1 tests.
#[cfg(test)]
pub fn plan_card_text(clean_plan: &str) -> (String, String, &'static str, &'static str) {
    (
        "APPROVE PLAN".to_string(),
        clean_plan.to_string(),
        "Approve",
        "Discard",
    )
}

/// Superseded (a new message now replaces an open plan, D10); kept only for
/// the first U4e1 tests.
#[cfg(test)]
pub fn plan_submit_toast() -> String {
    "A plan is waiting for your decision — approve it to run, or discard it.".to_string()
}

impl crate::AwiliApp {
    /// The agent lane's finish hook, called right BEFORE `work_finish_stream`
    /// persists the reply: settles `conv_id`'s arming on the streamed text,
    /// writes the marker-free text back into the row, and opens the plan when
    /// the planning step ended cleanly with the marker.
    pub(crate) fn plan_gate_after_work_turn(&mut self, conv_id: &str, cx: &mut Context<Self>) {
        let key = AskKey::Work(conv_id.to_string());
        let ok =
            self.work.panel.error.is_none() && self.work.panel.status != Some(RunStatus::Stopped);
        let idx = self.work.stream_row_idx;
        let text = idx
            .and_then(|i| self.views.get(conv_id)?.convo.message(i))
            .map(|m| m.text.clone())
            .unwrap_or_default();
        let (clean, _opened) = self.work.plan_gates.settle(&key, &text, ok);
        if let (Some(i), true) = (idx, clean != text) {
            if let Some(view) = self.views.get_mut(conv_id) {
                view.convo.set_text(i, clean.as_str());
                if let Some(msg) = view.convo.message(i) {
                    view.rows[i].stream_update(msg);
                }
            }
        }
        cx.notify();
    }

    /// The Code-tab lane's finish hook (U4e2), called right AFTER the turn's
    /// text settles: settles the active tab's arming on the last row's text,
    /// and writes the marker-free text back when the marker was stripped.
    /// `finish_code_turn` refreshes the row right after, so the render is
    /// re-parsed there.
    pub(crate) fn plan_gate_after_code_turn(&mut self, tab_id: u64) {
        let key = AskKey::CodeTab(tab_id);
        let Some(session) = self.work.code_tabs.iter().find(|t| t.id == tab_id) else {
            return;
        };
        // The last row is the assistant reply the planning step wrote.
        let idx = session.rows.len().checked_sub(1);
        // `settle` needs `&mut self.work.plan_gates`, so take what we need from
        // the session before the borrow.
        let (ok, text) = idx
            .map(|i| {
                (
                    !session.errored.contains(&i),
                    session
                        .convo
                        .message(i)
                        .map(|m| m.text.clone())
                        .unwrap_or_default(),
                )
            })
            .unwrap_or((true, String::new()));
        // R1b: a stopped or failed reply may end inside the marker; that cut
        // start of it is dropped too (`code_turn_end` saves this text).
        let stopped = session
            .history
            .turn
            .as_ref()
            .is_some_and(|t| t.stop_requested);
        let (clean, _opened) = self.work.plan_gates.settle(&key, &text, ok);
        let clean = awili_code::hide_plan_marker(&clean, !ok || stopped).into_owned();
        if clean != text {
            if let Some(session) = self.work.code_tabs.iter_mut().find(|t| t.id == tab_id) {
                if let Some(i) = session.rows.len().checked_sub(1) {
                    session.convo.set_text(i, clean.as_str());
                }
            }
        }
    }

    /// A plan waits for a decision in the active Work conversation, its card
    /// is visible there, and no run is in flight (U3e1's ⌘⇧A/⌘⇧D predicate
    /// ORs this in).
    pub(crate) fn plan_gate_pending_here(&self) -> bool {
        self.mode == crate::work::Mode::Work
            && !self.work.running
            && self.work.panel_is_visible(self.sidebar.active_id())
            && self
                .work
                .plan_gates
                .open_for(&AskKey::Work(self.sidebar.active_id().to_string()))
                .is_some()
    }

    /// The top of `resolve_work_approval` (D11): `false` when no plan waits
    /// here (the approval card then gets the decision). Approve submits the
    /// ONE full-access follow-up through the normal path; Deny records the
    /// decline and runs nothing. Other decisions are ignored (no scopes).
    pub(crate) fn resolve_plan_gate(
        &mut self,
        decision: ApprovalDecision,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.plan_gate_pending_here() {
            return false;
        }
        let conv_id = self.sidebar.active_id().to_string();
        let key = AskKey::Work(conv_id.clone());
        match decision {
            ApprovalDecision::Approve => {
                let Some(gate) = self.work.plan_gates.open_for(&key).cloned() else {
                    return false;
                };
                // The approval belongs to the plan's immutable owner, not to
                // the mutable chooser. Temporarily feed that owner through the
                // existing submit seam; it snapshots the run immediately and
                // we restore the selected next target right afterwards.
                let planned_target = WorkExecutor::Agent {
                    engine: gate.engine,
                    model: gate.model.clone(),
                };
                let next_target = self
                    .work
                    .conversation_routes
                    .entry(conv_id.clone())
                    .or_insert_with(|| {
                        crate::conversation_route::ConversationRoute::new(planned_target.clone())
                    })
                    .next_target()
                    .clone();
                let previous_folder = self.work.folder.clone();
                let planned_folder = self
                    .work
                    .plan_gates
                    .folder_for(&key)
                    .flatten()
                    .map(std::path::PathBuf::from);
                self.work
                    .conversation_routes
                    .get_mut(&conv_id)
                    .expect("route was just created")
                    .set_next_target(planned_target);
                self.work.folder = planned_folder;
                self.work.plan_gates.approve(&key);
                // RC2-RUNF: the row is marked as the click, not typed words.
                self.submit_work_run_kind(
                    awili_code::PLAN_APPROVED_TEXT.to_string(),
                    conv_id.clone(),
                    awili_core::MessageKind::PlanClick,
                    cx,
                );
                self.work.folder = previous_folder;
                self.work
                    .conversation_routes
                    .get_mut(&conv_id)
                    .expect("route was just created")
                    .set_next_target(next_target);
                self.persist_conversation_route(&conv_id);
                // One-shot: a release the dispatch did not consume (a refused
                // or parked submission) is dropped, never carried to a later turn.
                self.work.plan_gates.take_execute(&key);
            }
            ApprovalDecision::Deny => {
                self.work.plan_gates.deny(&key);
                let request = self
                    .views
                    .get(&conv_id)
                    .and_then(|v| plan_request_text(v.convo.messages()));
                self.plan_push_user_row(&conv_id, awili_code::PLAN_DECLINED_TEXT);
                self.plan_restore_request(request, cx);
            }
            _ => {}
        }
        cx.notify();
        true
    }

    /// Why the active Work conversation cannot run its open plan now, or
    /// `None` when it can (RC2-RUN: one check for the Approve refusal and
    /// the card). The plan runs only on the helper that planned it (D11),
    /// and only in the folder it was made in (U4e1-FIX2: the follow-up
    /// resumes that folder's planning session; U4e1-FIX3: an exact match,
    /// "no folder" included).
    pub(crate) fn work_plan_refusal(&self, key: &AskKey) -> Option<String> {
        // A plan owns its original engine/model/cwd. The chooser is next-turn
        // state, so it must not force the owner to switch back before approval.
        let _ = self.work.plan_gates.open_for(key)?;
        None
    }

    /// Push a user row into `conv_id` (not the last run's conversation) and
    /// persist it — the Deny record, marked as the click (RC2-RUNF).
    fn plan_push_user_row(&mut self, conv_id: &str, text: &str) {
        let kind = awili_core::MessageKind::PlanClick;
        let mut ts = None;
        if let Some(view) = self.views.get_mut(conv_id) {
            let idx = view.rows.len();
            ts = Some(
                view.convo
                    .push_kind(awili_core::Role::User, text.to_string(), kind),
            );
            if let Some(msg) = view.convo.message(idx) {
                view.rows.push(crate::MessageRow::new(msg));
            }
            crate::transcript_keys::pin_to_bottom(&view.scroll);
        }
        if let Some(ts) = ts {
            self.persist_message_kind(
                conv_id,
                crate::role_str(awili_core::Role::User),
                text,
                ts,
                kind,
            );
        }
    }

    /// R1c (designer decision): after "Don't change anything" the request
    /// that produced the plan goes back into the composer, on top of anything
    /// already typed there (the `merged_draft` rule), and a toast says so:
    /// edit it or press Enter, no retyping. Nothing runs.
    pub(crate) fn plan_restore_request(&mut self, request: Option<String>, cx: &mut Context<Self>) {
        self.restore_request_with(request, PLAN_DENIED_TOAST, cx);
    }

    /// RC1-N: put `request` back into the composer (the `merged_draft` rule)
    /// and say so with `toast`. Shared by the plan card's Deny and the in-app
    /// approval card's Deny.
    pub(crate) fn restore_request_with(
        &mut self,
        request: Option<String>,
        toast: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(request) = request else {
            return;
        };
        self.composer.update(cx, |c, cx| {
            let merged = crate::executor_status::merged_draft(c.text(), &request);
            c.set_text(&merged, cx);
        });
        self.show_toast(toast, cx);
    }

    /// R1c: a Deny pill click leaves the cursor in the composer, on the
    /// request it put back.
    pub(crate) fn plan_focus_composer(&self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let composer = self.composer.read(cx).focus_handle().clone();
        window.focus(&composer, cx);
    }

    /// The Work plan card (D10/D11): shown while [`Self::plan_gate_pending_here`].
    /// Both pills go through `resolve_work_approval`, the shared path.
    pub(crate) fn work_plan_gate_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.plan_gate_pending_here() {
            return None;
        }
        let key = AskKey::Work(self.sidebar.active_id().to_string());
        let questions = self.work.plan_gates.has_questions(&key);
        let answer = questions.then(|| {
            pill_button(
                "plan-answer",
                PLAN_ANSWER_LABEL,
                theme::p().accent,
                theme::p().on_accent,
                true,
            )
            .debug_selector(|| PLAN_ANSWER_SELECTOR.to_string())
            .on_click(cx.listener(|this, _ev, window, cx| this.plan_focus_composer(window, cx)))
            .into_any_element()
        });
        // RC1-N: with open questions, answering is the first action and
        // "Make the changes" stays, as a plain pill.
        let approve = if questions {
            pill_button(
                "plan-approve",
                PLAN_APPROVE_LABEL,
                theme::p().chip_warn,
                theme::p().text,
                false,
            )
        } else {
            pill_button(
                "plan-approve",
                PLAN_APPROVE_LABEL,
                theme::p().accent,
                theme::p().on_accent,
                true,
            )
        }
        .debug_selector(|| PLAN_APPROVE_SELECTOR.to_string())
        .on_click(cx.listener(|this, _ev, _w, cx| {
            this.resolve_work_approval(ApprovalDecision::Approve, cx)
        }));
        let deny = pill_button(
            "plan-deny",
            PLAN_DENY_LABEL,
            theme::p().chip_warn,
            theme::p().text,
            false,
        )
        .debug_selector(|| PLAN_DENY_SELECTOR.to_string())
        .on_click(cx.listener(|this, _ev, window, cx| {
            this.resolve_work_approval(ApprovalDecision::Deny, cx);
            this.plan_focus_composer(window, cx);
        }));
        let replaced = self.work.plan_gates.replaced_earlier(&key);
        // RC2-RUN (R3-14): a plan that cannot run here says so on the card
        // and offers no live "Make the changes".
        let blocked = self
            .work_plan_refusal(&key)
            .map(|r| plan_blocked_card_text(&r));
        let approve = blocked.is_none().then(|| approve.into_any_element());
        Some(
            div()
                .debug_selector(|| WORK_PLAN_CARD_SELECTOR.to_string())
                .child(self.plan_gate_card_body(answer, approve, deny, replaced, blocked)),
        )
    }

    /// A plan waits for a decision in the ACTIVE Code tab, its card is visible
    /// there (U4e2, the Code-lane analogue of [`Self::plan_gate_pending_here`]).
    fn code_plan_gate_open(&self) -> bool {
        self.work
            .code_tabs
            .get(self.work.code_active)
            .is_some_and(|t| {
                self.work
                    .plan_gates
                    .open_for(&AskKey::CodeTab(t.id))
                    .is_some()
            })
    }

    /// The Code-tab plan card (U4e2): shown while the ACTIVE tab's gate is open
    /// ([`Self::code_plan_gate_open`]). The same copy and pills as the Work
    /// card (the shared [`Self::plan_gate_card_body`]); the pills route through
    /// [`Self::resolve_code_plan_gate`], and the element `id`s are
    /// `code-plan-approve` / `code-plan-deny`.
    pub(crate) fn code_plan_gate_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.code_plan_gate_open() {
            return None;
        }
        let questions = self
            .work
            .code_tabs
            .get(self.work.code_active)
            .is_some_and(|t| self.work.plan_gates.has_questions(&AskKey::CodeTab(t.id)));
        let answer = questions.then(|| {
            pill_button(
                "code-plan-answer",
                PLAN_ANSWER_LABEL,
                theme::p().accent,
                theme::p().on_accent,
                true,
            )
            .on_click(cx.listener(|this, _ev, window, cx| this.plan_focus_composer(window, cx)))
            .into_any_element()
        });
        let approve = if questions {
            pill_button(
                "code-plan-approve",
                PLAN_APPROVE_LABEL,
                theme::p().chip_warn,
                theme::p().text,
                false,
            )
        } else {
            pill_button(
                "code-plan-approve",
                PLAN_APPROVE_LABEL,
                theme::p().accent,
                theme::p().on_accent,
                true,
            )
        }
        .on_click(cx.listener(|this, _ev, _w, cx| this.resolve_code_plan_gate(true, cx)));
        let deny = pill_button(
            "code-plan-deny",
            PLAN_DENY_LABEL,
            theme::p().chip_warn,
            theme::p().text,
            false,
        )
        .debug_selector(|| CODE_PLAN_DENY_SELECTOR.to_string())
        .on_click(cx.listener(|this, _ev, window, cx| {
            this.resolve_code_plan_gate(false, cx);
            this.plan_focus_composer(window, cx);
        }));
        let replaced = self
            .work
            .code_tabs
            .get(self.work.code_active)
            .is_some_and(|t| {
                self.work
                    .plan_gates
                    .replaced_earlier(&AskKey::CodeTab(t.id))
            });
        // RC2-RUN (R3-14): the Code tab's card says so too.
        let blocked = self
            .work
            .code_tabs
            .get(self.work.code_active)
            .and_then(|t| self.code_plan_refusal(t.id, &AskKey::CodeTab(t.id)))
            .map(|r| plan_blocked_card_text(&r));
        let approve = blocked.is_none().then(|| approve.into_any_element());
        Some(self.plan_gate_card_body(answer, approve, deny, replaced, blocked))
    }

    /// Approve/Deny the active Code tab's open plan (U4e2): Approve records
    /// the decision and submits the ONE full-access follow-up through the
    /// normal Code path; Deny records the decline and pushes
    /// [`awili_code::PLAN_DECLINED_TEXT`] as a user row, running nothing.
    pub(crate) fn resolve_code_plan_gate(&mut self, approve: bool, cx: &mut Context<Self>) {
        let Some(tab_id) = self.work.code_tabs.get(self.work.code_active).map(|t| t.id) else {
            return;
        };
        let key = AskKey::CodeTab(tab_id);
        if self.work.plan_gates.open_for(&key).is_none() {
            return;
        }
        if approve {
            let Some(gate) = self.work.plan_gates.open_for(&key).cloned() else {
                return;
            };
            let conv_id = self
                .work
                .code_tabs
                .iter()
                .find(|tab| tab.id == tab_id)
                .and_then(|tab| tab.history.conv_id.clone());
            let tab_target = self
                .work
                .code_tabs
                .iter()
                .find(|tab| tab.id == tab_id)
                .map(|tab| tab.route.next_target().clone());
            let next_target = conv_id.as_deref().and_then(|id| {
                self.work
                    .conversation_routes
                    .get(id)
                    .map(|route| route.next_target().clone())
                    .or_else(|| tab_target.clone())
            });
            let planned_cwd = self
                .work
                .code_tabs
                .iter()
                .find(|tab| tab.id == tab_id)
                .and_then(|tab| tab.project_folder.clone());
            let previous_cwd = self.work.workshop_selected_folder.clone();
            if let Some(id) = conv_id.as_deref() {
                let fallback = tab_target.clone().unwrap_or_else(|| WorkExecutor::Agent {
                    engine: gate.engine,
                    model: gate.model.clone(),
                });
                self.work
                    .conversation_routes
                    .entry(id.to_string())
                    .or_insert_with(|| crate::conversation_route::ConversationRoute::new(fallback))
                    .set_next_target(WorkExecutor::Agent {
                        engine: gate.engine,
                        model: gate.model.clone(),
                    });
            }
            self.work.workshop_selected_folder = planned_cwd;
            self.work.plan_gates.approve(&key);
            self.submit_code_turn_kind(
                awili_code::PLAN_APPROVED_TEXT.to_string(),
                awili_core::MessageKind::PlanClick,
                cx,
            );
            self.work.workshop_selected_folder = previous_cwd;
            if let Some(id) = conv_id.as_deref() {
                if let Some(next_target) = next_target {
                    if let Some(route) = self.work.conversation_routes.get_mut(id) {
                        route.set_next_target(next_target);
                    }
                }
                self.persist_conversation_route(id);
            }
            // A successful accepted dispatch consumed this in its target-aware
            // autonomy calculation. If Code rejected before acceptance, drop
            // the release here so it cannot authorize an unrelated later Send.
            self.work.plan_gates.take_execute(&key);
        } else {
            self.work.plan_gates.deny(&key);
            let mut request = None;
            let mut ts = None;
            if let Some(session) = self.work.code_tabs.iter_mut().find(|t| t.id == tab_id) {
                request = plan_request_text(session.convo.messages());
                let idx = session.rows.len();
                ts = Some(session.convo.push_kind(
                    awili_core::Role::User,
                    awili_code::PLAN_DECLINED_TEXT,
                    awili_core::MessageKind::PlanClick,
                ));
                if let Some(msg) = session.convo.message(idx) {
                    session.rows.push(crate::MessageRow::new(msg));
                }
                session.scroll.scroll_to_bottom();
            }
            // RC2-FIXA3: the Deny row is saved in the tab's conversation.
            if let Some(ts) = ts {
                self.code_record_row(
                    tab_id,
                    awili_core::Role::User,
                    awili_code::PLAN_DECLINED_TEXT,
                    awili_core::MessageKind::PlanClick,
                    ts as i64,
                );
            }
            self.plan_restore_request(request, cx);
            cx.notify();
        }
    }

    /// Why the active Code tab cannot run its open plan now (U4e2-FIX1), or
    /// `None` when it can: the tab must still be on the engine and model that
    /// planned, and in the project folder the planning session belongs to
    /// (`CodeSession::project_folder`, set when the planning turn was sent;
    /// `submit_code_turn` would drop the planning session in any other folder).
    fn code_plan_refusal(&self, tab_id: u64, key: &AskKey) -> Option<String> {
        let _ = (tab_id, key);
        None
    }

    /// The shared plan-card body (D10 copy): title, body, and a pill row.
    /// Used by both the Work and the Code-tab cards, which supply their own
    /// approve/deny buttons (each lane routes its pills through its own
    /// resolution path, and the element `id`s name the lane). R1g:
    /// `replaced` adds the note that an earlier plan was set aside.
    fn plan_gate_card_body(
        &self,
        answer: Option<gpui::AnyElement>,
        approve: Option<gpui::AnyElement>,
        deny: impl IntoElement,
        replaced: bool,
        blocked: Option<String>,
    ) -> impl IntoElement {
        // RC1-N: a plan that ends with questions says so first.
        let questions = answer.is_some();
        // RC2-RUN (R3-14): a plan that cannot run here says why instead.
        let is_blocked = blocked.is_some();
        let (title, body) = match blocked {
            Some(why) => (PLAN_BLOCKED_TITLE, why),
            None if questions => (PLAN_QUESTIONS_TITLE, PLAN_QUESTIONS_BODY.to_string()),
            None => (PLAN_CARD_TITLE, PLAN_CARD_BODY.to_string()),
        };
        div()
            .flex()
            .flex_col()
            .gap(theme::z(8.))
            .p(theme::z(12.))
            .rounded(theme::z(theme::RADIUS))
            .bg(theme::p().raised)
            .border_1()
            .border_color(theme::p().chip_warn)
            .child(
                div()
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::p().chip_warn)
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .when(questions && !is_blocked, |d| {
                        d.debug_selector(|| PLAN_QUESTIONS_SELECTOR.to_string())
                    })
                    .when(is_blocked, |d| {
                        d.debug_selector(|| PLAN_BLOCKED_SELECTOR.to_string())
                    })
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .text_color(theme::p().text)
                    .child(SharedString::from(body)),
            )
            .when(replaced, |card| {
                card.child(
                    div()
                        .debug_selector(|| PLAN_REPLACED_NOTE_SELECTOR.to_string())
                        .text_size(theme::z(theme::LABEL_SIZE))
                        .text_color(theme::p().muted)
                        .child(SharedString::from(PLAN_REPLACED_NOTE)),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(theme::z(8.))
                    .children(answer)
                    .children(approve)
                    .child(deny),
            )
    }
}

#[cfg(test)]
#[path = "plan_gate_r1g_tests.rs"]
mod r1g_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use awili_code::{PLAN_APPROVED_TEXT, PLAN_DECLINED_TEXT, PLAN_READY_MARKER};

    /// D10: an Ask plan step whose reply ends with the marker holds the run and
    /// strips the marker from the stored reply; a marker-free planning reply
    /// finishes normally.
    #[test]
    fn u4e1_plan_marker_holds_the_turn() {
        let reply = format!("1. edit main.rs\n2. add a test\n{PLAN_READY_MARKER}");
        let d = gate_reply(Autonomy::AskFirst, CodeEngine::Claude, &reply);
        let PlanGateDecision::Hold { clean } = d else {
            panic!("the marker must hold the turn")
        };
        assert!(
            !clean.contains(PLAN_READY_MARKER),
            "the marker is stripped: {clean}"
        );
        assert!(
            clean.contains("1. edit main.rs"),
            "the plan survives: {clean}"
        );

        // A planning reply without the marker (nothing to change) just
        // finishes — no card, no hold.
        assert_eq!(
            gate_reply(Autonomy::AskFirst, CodeEngine::Claude, "No changes needed."),
            PlanGateDecision::Finish
        );
    }

    /// D10: only the Ask planning step gates; an Autonomous turn with the same
    /// marker text finishes normally, and non-enforcing engines never gate
    /// regardless of posture.
    #[test]
    fn u4e1_autonomous_turn_is_not_gated() {
        let reply = format!("will make the change {PLAN_READY_MARKER}");
        for engine in [
            CodeEngine::Claude,
            CodeEngine::Codex,
            CodeEngine::Glm,
            CodeEngine::Dsh,
        ] {
            assert_eq!(
                gate_reply(Autonomy::Autonomous, engine, &reply),
                PlanGateDecision::Finish,
                "Autonomous turns never gate ({engine:?})"
            );
            assert!(
                !may_gate(Autonomy::Autonomous, engine),
                "may_gate must refuse Autonomous ({engine:?})"
            );
        }
        // Ask + non-enforcing engine (GLM/DeepSeek): the gate never arms —
        // Awili cannot offer a card it cannot enforce.
        for engine in [CodeEngine::Glm, CodeEngine::Dsh] {
            assert!(
                !may_gate(Autonomy::AskFirst, engine),
                "may_gate must refuse non-enforcing engines ({engine:?})"
            );
        }
        // Ask + enforcing engine: the gate arms.
        for engine in [CodeEngine::Claude, CodeEngine::Codex] {
            assert!(
                may_gate(Autonomy::AskFirst, engine),
                "may_gate must arm for Ask + {engine:?}"
            );
        }
    }

    /// D10: the follow-up texts are the exact awili-code constants — Approve
    /// pushes the go-ahead row, Discard pushes the declined row — and the
    /// pending-plan composer submit is rejected with a reason, not silently
    /// dropped.
    #[test]
    fn u4e1_approve_releases_the_follow_up_turn() {
        assert_eq!(
            PLAN_APPROVED_TEXT,
            "Approved — go ahead and make the changes you proposed."
        );
        assert_eq!(
            PLAN_DECLINED_TEXT,
            "Declined the plan — nothing was changed."
        );

        let (heading, body, approve, discard) = plan_card_text("1. edit main.rs\n2. add a test");
        assert_eq!(heading, "APPROVE PLAN");
        assert!(
            body.contains("1. edit main.rs"),
            "the card shows the clean plan: {body:?}"
        );
        assert_eq!((approve, discard), ("Approve", "Discard"));

        let toast = plan_submit_toast();
        assert!(
            !toast.is_empty() && toast.contains("approve it to run") && toast.contains("discard"),
            "the toast names both exits: {toast:?}"
        );

        // The gate only arms for an enforcing agent lane: a Gateway or
        // LocalBuild run with the same marker never holds (their cards and
        // lanes are D7's, not D10's).
        let reply = format!("plan {PLAN_READY_MARKER}");
        assert_eq!(
            gate_reply(Autonomy::AskFirst, CodeEngine::Claude, &reply),
            PlanGateDecision::Hold {
                clean: "plan".to_string()
            }
        );
        // The gate only arms for enforcing agent lanes: pin the shape so the
        // boundary cannot silently widen.
        assert!(CodeEngine::Claude.enforces_ask());
        assert!(!CodeEngine::Glm.enforces_ask());
    }

    // ---- U4e1-FIX1: the gate state (pure) ------------------------------------

    fn work(id: &str) -> AskKey {
        AskKey::Work(id.to_string())
    }

    fn plan_text() -> String {
        format!("1. edit main.rs\n2. add a test\n{PLAN_READY_MARKER}")
    }

    /// A clean planning step with the marker opens `key`'s plan (Claude).
    fn open_plan(gates: &mut PlanGates, key: &AskKey) {
        gates.arm(key.clone(), CodeEngine::Claude, None);
        let (_, opened) = gates.settle(key, &plan_text(), true);
        assert!(opened, "setup: the plan must open");
    }

    fn ask_turn(gates: &mut PlanGates, key: &AskKey) -> Autonomy {
        next_turn(
            gates,
            key.clone(),
            AskSupport::PlanFirst,
            ApprovalMode::AskBeforeChanges,
            CodeEngine::Claude,
            None,
        )
    }

    #[test]
    fn u4e1_no_card_without_arming() {
        let mut gates = PlanGates::default();
        let key = work("a");
        let (clean, opened) = gates.settle(&key, &plan_text(), true);
        assert!(!opened, "an unarmed turn never opens a card");
        assert!(!clean.contains(PLAN_READY_MARKER), "{clean}");
        assert!(clean.contains("2. add a test"), "{clean}");
        assert!(gates.open_for(&key).is_none());
    }

    #[test]
    fn u4e1_card_opens_only_for_a_clean_planning_step() {
        let key = work("a");

        let mut gates = PlanGates::default();
        gates.arm(key.clone(), CodeEngine::Codex, Some("m1".into()));
        let (clean, opened) = gates.settle(&key, &plan_text(), true);
        assert!(opened);
        assert!(!clean.contains(PLAN_READY_MARKER));
        assert_eq!(
            gates.open_for(&key),
            Some(&PlanGate {
                engine: CodeEngine::Codex,
                model: Some("m1".into())
            })
        );

        let mut gates = PlanGates::default();
        gates.arm(key.clone(), CodeEngine::Claude, None);
        let (clean, opened) = gates.settle(&key, &plan_text(), false);
        assert!(!opened, "an errored or stopped step opens no card");
        assert!(
            !clean.contains(PLAN_READY_MARKER),
            "still stripped: {clean}"
        );
        assert!(gates.open_for(&key).is_none());
        assert!(!gates.is_armed(&key), "the arming is consumed");

        let mut gates = PlanGates::default();
        gates.arm(key.clone(), CodeEngine::Claude, None);
        let (clean, opened) = gates.settle(&key, "Nothing to change.", true);
        assert!(!opened, "no marker, no card");
        assert_eq!(clean, "Nothing to change.");
        assert!(!gates.is_armed(&key));
    }

    #[test]
    fn u4e1_approve_releases_exactly_one_full_access_turn() {
        let mut gates = PlanGates::default();
        let key = work("a");
        assert_eq!(ask_turn(&mut gates, &key), Autonomy::AskFirst);
        assert!(gates.is_armed(&key), "an Ask turn arms the gate");
        let (_, opened) = gates.settle(&key, &plan_text(), true);
        assert!(opened);

        assert!(gates.approve(&key).is_some());
        assert!(gates.open_for(&key).is_none(), "the card is gone");
        assert!(
            gates.approve(&key).is_none(),
            "a second approve releases nothing"
        );
        assert_eq!(
            ask_turn(&mut gates, &key),
            Autonomy::Autonomous,
            "the approved follow-up runs with full access"
        );
        assert!(
            !gates.is_armed(&key),
            "the follow-up is not a planning step"
        );
        assert_eq!(
            ask_turn(&mut gates, &key),
            Autonomy::AskFirst,
            "the turn after the follow-up asks again"
        );
        assert!(!gates.take_execute(&key));
    }

    #[test]
    fn u4e1_clear_does_not_eat_the_approved_follow_up() {
        let mut gates = PlanGates::default();
        let key = work("a");
        open_plan(&mut gates, &key);
        gates.approve(&key);
        gates.clear(&key); // submit_work_run_in's hook runs before the dispatch
        assert!(gates.take_execute(&key), "the release survives the clear");
        assert!(!gates.take_execute(&key), "and is taken once");
    }

    #[test]
    fn u4e1_deny_closes_without_a_follow_up() {
        let mut gates = PlanGates::default();
        let key = work("a");
        open_plan(&mut gates, &key);
        assert!(gates.deny(&key));
        assert!(gates.open_for(&key).is_none());
        assert!(!gates.take_execute(&key));
        assert_eq!(ask_turn(&mut gates, &key), Autonomy::AskFirst);
        assert!(!gates.deny(&key), "nothing left to deny");
    }

    #[test]
    fn u4e1_new_arming_replaces_the_old_plan() {
        let mut gates = PlanGates::default();
        let key = work("a");
        open_plan(&mut gates, &key);
        gates.arm(key.clone(), CodeEngine::Codex, None);
        assert!(
            gates.open_for(&key).is_none(),
            "a new planning step drops the old plan"
        );
        let (_, opened) = gates.settle(&key, &plan_text(), true);
        assert!(opened);
        assert_eq!(
            gates.open_for(&key).map(|g| g.engine),
            Some(CodeEngine::Codex)
        );

        // A new Ask turn (next_turn) also replaces an open plan, and a plan in
        // another conversation is untouched.
        let other = work("b");
        open_plan(&mut gates, &other);
        assert_eq!(ask_turn(&mut gates, &key), Autonomy::AskFirst);
        assert!(gates.open_for(&key).is_none());
        assert!(gates.open_for(&other).is_some());
    }

    /// Source scan: the plan is resolved at the TOP of `resolve_work_approval`
    /// (before its Gateway-only return), so U3e1's ⌘⇧A/⌘⇧D reach it; the card
    /// pills route through `resolve_work_approval`, not a private path.
    #[test]
    fn u4e1_resolution_path_is_the_approval_path() {
        let src = include_str!("work.rs");
        let start = src
            .find("pub fn resolve_work_approval(")
            .expect("resolve_work_approval exists");
        let body = &src[start..];
        let hook = body
            .find("self.resolve_plan_gate(decision, cx)")
            .expect("the plan hook is in resolve_work_approval");
        let gateway = body
            .find("WorkExecutor::Gateway")
            .expect("the Gateway check is in resolve_work_approval");
        assert!(
            hook < gateway,
            "the plan hook must come before the Gateway check"
        );

        let me = include_str!("plan_gate.rs");
        let card = &me[me.find("fn work_plan_gate_card(").unwrap()..];
        let card = &card[..card.find("\n    }\n").unwrap()];
        assert_eq!(
            card.matches("this.resolve_work_approval(ApprovalDecision::")
                .count(),
            2,
            "both pills use the shared path"
        );
        let old = concat!("fn plan_", "approve(");
        assert!(!src.contains(old), "the private approve path is gone");

        // The agent lane's run end settles the plan right before the finish
        // persists the reply. (N1-INT1: after the N1 merge this lives in
        // `work_agent_run_done_in`, which the run's task calls with the
        // submit-time `conv_id`; the behavioural proof is
        // `n1_int1_agent_run_end_after_switch_settles_and_notifies_the_submit_conversation`.)
        let hook = concat!(
            "self.plan_gate_after_work_turn(conv_id, cx);\n",
            "        self.work_finish_stream(cx);"
        );
        assert_eq!(src.matches(hook).count(), 1, "the agent run-end hook");
    }

    // ---- U4e1-FIX1: the app wiring (isolated HOME, real AwiliApp) ------------

    use crate::AwiliApp;

    const CHILD: &str = "AWILI_U4E1_TEST_HOME";
    const CONV: &str = "u4e1-conv";

    /// Run `test` in a subprocess with a private HOME (`AwiliApp::new` reads
    /// config and opens persistence). `None` in the parent once it passed.
    fn isolated(test: &str) -> Option<std::path::PathBuf> {
        if let Some(home) = std::env::var_os(CHILD) {
            return Some(home.into());
        }
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, home.path())
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "isolated test failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        None
    }

    /// A Work conversation on Claude in a real folder. The Claude binary is a
    /// path that does not exist, so no real CLI can ever run.
    fn plan_app(app: &mut AwiliApp, home: &std::path::Path) {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.work
            .graduation
            .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.mode = crate::work::Mode::Work;
        app.work.executor = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        };
        app.work.agents.claude_bin = Some(home.join("no-such-claude").display().to_string());
        let folder = home.join("proj");
        std::fs::create_dir_all(&folder).unwrap();
        app.work.folder = Some(folder);
    }

    /// Choose Claude as the Work thread's next-turn target the way the
    /// chooser does: a run follows the conversation's route, not the ambient
    /// executor field `plan_app` also sets.
    fn choose_claude(app: &mut AwiliApp, cx: &mut gpui::Context<AwiliApp>) {
        assert!(app.set_composer_target(
            &crate::workshop_composer::ModelOwner::Work(CONV.into()),
            WorkExecutor::Agent {
                engine: CodeEngine::Claude,
                model: None,
            },
            cx,
        ));
    }

    fn texts(app: &AwiliApp) -> Vec<(awili_core::Role, String)> {
        app.views[CONV]
            .convo
            .messages()
            .iter()
            .map(|m| (m.role, m.text.clone()))
            .collect()
    }

    /// P1 + P2: the card's Approve (through `resolve_work_approval`, with a
    /// non-Gateway executor) submits ONE full-access turn through the normal
    /// submission path, and cannot be pressed twice.
    #[gpui::test]
    fn u4e1_card_approve_submits_one_full_access_turn(cx: &mut gpui::TestAppContext) {
        let Some(home) =
            isolated("plan_gate::tests::u4e1_card_approve_submits_one_full_access_turn")
        else {
            return;
        };
        // The dispatched follow-up's CLI thread (a missing binary) may report.
        cx.executor().allow_parking();
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            let key = work(CONV);
            open_plan(&mut app.work.plan_gates, &key);
            assert!(app.plan_gate_pending_here());

            // Selecting another helper affects the next ordinary turn only;
            // this approval still owns the planner's captured helper.
            app.work.executor = WorkExecutor::Agent {
                engine: CodeEngine::Codex,
                model: None,
            };
            let seq0 = app.work.run_seq;
            app.resolve_work_approval(ApprovalDecision::Approve, cx);
            assert_eq!(app.work.run_seq, seq0 + 1, "exactly one turn dispatched");
            assert!(app.work.running);
            assert!(app.work.graduation.is_armed(), "the normal ledger path ran");
            assert_eq!(
                texts(app),
                [
                    (awili_core::Role::User, PLAN_APPROVED_TEXT.to_string()),
                    (awili_core::Role::Assistant, String::new()),
                ],
                "the approval is a normal user turn"
            );
            assert!(!app.plan_gate_pending_here(), "the card is gone");
            assert!(
                !app.work.plan_gates.is_armed(&key),
                "the follow-up is not a planning step (full access)"
            );
            assert!(
                !app.work.plan_gates.take_execute(&key),
                "the release was used"
            );

            app.cancel_work_run(cx);
            let seq1 = app.work.run_seq;
            app.resolve_work_approval(ApprovalDecision::Approve, cx);
            assert_eq!(app.work.run_seq, seq1, "a second Approve runs nothing");
            assert_eq!(texts(app).len(), 2);
            assert_eq!(
                app.agent_autonomy_for_turn(CONV, CodeEngine::Claude, None),
                Autonomy::AskFirst,
                "the next turn asks again"
            );

            // A refused submission (no folder) never keeps the release for a
            // later turn.
            open_plan(&mut app.work.plan_gates, &key);
            app.work.folder = None;
            app.resolve_work_approval(ApprovalDecision::Approve, cx);
            assert!(!app.plan_gate_pending_here());
            assert_eq!(
                app.agent_autonomy_for_turn(CONV, CodeEngine::Claude, None),
                Autonomy::AskFirst,
                "an unconsumed approval is dropped"
            );
        });
    }

    /// P1: a Stop and a new message each clear the conversation's plan, so a
    /// stopped or replaced plan can never be approved later.
    #[gpui::test]
    fn u4e1_stop_and_new_message_clear_the_plan(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::u4e1_stop_and_new_message_clear_the_plan")
        else {
            return;
        };
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            let key = work(CONV);

            open_plan(&mut app.work.plan_gates, &key);
            app.cancel_work_run(cx);
            assert!(!app.plan_gate_pending_here(), "Stop clears the plan");
            let seq = app.work.run_seq;
            app.resolve_work_approval(ApprovalDecision::Approve, cx);
            assert_eq!(app.work.run_seq, seq, "a stopped plan cannot be approved");

            // A Stop during the planning step: its late marker opens nothing.
            app.work
                .plan_gates
                .arm(key.clone(), CodeEngine::Claude, None);
            app.cancel_work_run(cx);
            let (_, opened) = app.work.plan_gates.settle(&key, &plan_text(), true);
            assert!(!opened, "a stopped planning step never opens a card");

            // A new message replaces the plan (no folder: nothing is spawned).
            open_plan(&mut app.work.plan_gates, &key);
            app.work.folder = None;
            app.submit_work_run("something else".into(), cx);
            assert!(
                !app.plan_gate_pending_here(),
                "a new message clears the plan"
            );
            assert!(app.work.plan_gates.open_for(&key).is_none());
        });
    }

    /// FIX1d (D10): a new message clears the open plan even when the submit
    /// preflight refuses it — the old card is never approvable.
    #[gpui::test]
    fn u4e1_refused_new_message_still_clears_the_plan(cx: &mut gpui::TestAppContext) {
        let Some(home) =
            isolated("plan_gate::tests::u4e1_refused_new_message_still_clears_the_plan")
        else {
            return;
        };
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            let key = work(CONV);
            open_plan(&mut app.work.plan_gates, &key);
            // A preflight refusal (RC1-M: a Work folder that is gone — the S1
            // CLAUDE.md refusal is gone with CLAUDE.md).
            let folder = app.work.folder.clone().unwrap();
            app.work.folder = Some(folder.join("gone"));
            let seq = app.work.run_seq;

            app.submit_work_run("something else".into(), cx);
            assert!(app.work.panel.error.is_some(), "the preflight refused it");
            assert_eq!(app.work.run_seq, seq, "nothing was dispatched");
            assert!(texts(app).is_empty(), "no row was pushed");
            assert!(
                !app.plan_gate_pending_here(),
                "the refused message cleared the plan"
            );
            assert!(app.work.plan_gates.open_for(&key).is_none());

            app.resolve_work_approval(ApprovalDecision::Approve, cx);
            assert_eq!(app.work.run_seq, seq, "the old plan cannot be approved");
            assert!(texts(app).is_empty());
        });
    }

    /// Deny through the shared path: the declined row, no run, no release.
    #[gpui::test]
    fn u4e1_card_deny_records_and_runs_nothing(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::u4e1_card_deny_records_and_runs_nothing")
        else {
            return;
        };
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            let key = work(CONV);
            open_plan(&mut app.work.plan_gates, &key);
            let seq = app.work.run_seq;
            app.resolve_work_approval(ApprovalDecision::Deny, cx);
            assert_eq!(app.work.run_seq, seq, "Deny runs nothing");
            assert!(!app.work.running);
            assert!(!app.plan_gate_pending_here());
            assert_eq!(
                texts(app),
                [(awili_core::Role::User, PLAN_DECLINED_TEXT.to_string())]
            );
            assert!(!app.work.plan_gates.take_execute(&key));
            assert_eq!(
                app.agent_autonomy_for_turn(CONV, CodeEngine::Claude, None),
                Autonomy::AskFirst
            );
        });
    }

    #[cfg(not(unix))]
    #[allow(unused_variables, dead_code)]
    fn fake_claude(home: &std::path::Path) -> String {
        unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
    }
    #[cfg(unix)]
    /// A fake `claude` that records each run's argv (`argv1`, `argv2`, …) and
    /// answers the first run with a plan (session `s-plan`), later runs with
    /// "Done.".
    fn fake_claude(home: &std::path::Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let script = r#"#!/bin/sh
n=$(cat "DIR/count" 2>/dev/null || echo 0)
n=$((n+1))
echo "$n" > "DIR/count"
cat > "DIR/stdin$n"
: > "DIR/argv$n"
for a in "$@"; do printf '%s\n' "$a" >> "DIR/argv$n"; done
cat "DIR/stdin$n" >> "DIR/argv$n"
printf '%s\n' '{"type":"system","subtype":"init","session_id":"s-plan"}'
if [ "$n" = 1 ]; then
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"1. edit main.rs\n2. add a test\n[[awili:plan-ready]]"}]}}'
else
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}'
fi
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"ok","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
        let dir = home.join("fake");
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("claude");
        std::fs::write(&bin, script.replace("DIR", &dir.display().to_string())).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        bin.display().to_string()
    }

    fn argv(home: &std::path::Path, n: u32) -> Vec<String> {
        std::fs::read_to_string(home.join("fake").join(format!("argv{n}")))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Drive the app until its Work run has finished (the fake CLI runs on
    /// its own thread; `allow_parking` lets its events in).
    fn wait_idle(cx: &mut gpui::TestAppContext, entity: &gpui::Entity<AwiliApp>) {
        let deadline = std::time::Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
        loop {
            cx.run_until_parked();
            if !entity.read_with(cx, |app, _| app.work.running) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the agent turn never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// End to end over a fake Claude: an Ask turn runs as the read-only
    /// planning step; its finish strips the marker BEFORE the reply is
    /// persisted and opens the card; Approve submits one full-access turn that
    /// resumes the planning step's own session.
    #[gpui::test]
    fn u4e1_planning_step_then_approval_end_to_end(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::u4e1_planning_step_then_approval_end_to_end")
        else {
            return;
        };
        let store = awili_persist::Store::open_default().unwrap();
        store.create_conversation(CONV, "plan", 0).unwrap();
        cx.executor().allow_parking();
        let bin = fake_claude(&home);
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            choose_claude(app, cx);
            app.work.agents.claude_bin = Some(bin);
            app.submit_work_run("fix it".into(), cx);
            assert!(app.work.running);
            assert!(
                app.work.plan_gates.is_armed(&work(CONV)),
                "Ask arms the gate"
            );
        });
        wait_idle(cx, &entity);
        let plan = entity.update(cx, |app, cx| {
            assert!(app.plan_gate_pending_here(), "the plan card opens");
            let plan = texts(app)[1].1.clone();
            assert!(!plan.contains(PLAN_READY_MARKER), "{plan}");
            assert!(plan.contains("2. add a test"), "{plan}");
            app.resolve_work_approval(ApprovalDecision::Approve, cx);
            assert!(app.work.running, "the follow-up is running");
            plan
        });
        wait_idle(cx, &entity);
        entity.update(cx, |app, _| {
            assert!(!app.plan_gate_pending_here());
            assert_eq!(
                texts(app),
                [
                    (awili_core::Role::User, "fix it".to_string()),
                    (awili_core::Role::Assistant, plan.clone()),
                    (awili_core::Role::User, PLAN_APPROVED_TEXT.to_string()),
                    (awili_core::Role::Assistant, "Done.".to_string()),
                ]
            );
        });

        let first = argv(&home, 1);
        let pm = first.iter().position(|a| a == "--permission-mode");
        assert_eq!(
            pm.and_then(|i| first.get(i + 1)).map(String::as_str),
            Some("plan"),
            "the first turn is the read-only planning step: {first:?}"
        );
        let second = argv(&home, 2);
        assert!(
            !second.iter().any(|a| a == "--permission-mode"),
            "the approved turn has full access: {second:?}"
        );
        let resume = second.iter().position(|a| a == "--resume");
        assert_eq!(
            resume.and_then(|i| second.get(i + 1)).map(String::as_str),
            Some("s-plan"),
            "the approved turn resumes the planning session: {second:?}"
        );
        assert!(argv(&home, 3).is_empty(), "exactly two CLI runs");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let saved: Vec<String> = store
                .messages(CONV)
                .unwrap()
                .into_iter()
                .filter(|m| m.role == "assistant")
                .map(|m| m.text)
                .collect();
            if saved.len() == 2 {
                assert_eq!(
                    saved,
                    [plan.clone(), "Done.".to_string()],
                    "persisted replies"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "both replies must be persisted: {saved:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// N1-INT1 (N1 × U4): the production agent-run end
    /// (`work_agent_run_done_in`, called by `run_work_via_agent`'s task with
    /// the submit-time conversation) after the user switched conversations.
    /// A planning step submitted in A finishes while B is on screen: A's plan
    /// card opens (B gets none), A's reply is clean, and the run-end
    /// notification targets A.
    #[gpui::test]
    fn n1_int1_agent_run_end_after_switch_settles_and_notifies_the_submit_conversation(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::notifications::{route_id, Permission, Target};
        use crate::notify_platform::{BackendSignal, Recorded};
        const OTHER: &str = "n1-int1-other-conv";
        let Some(home) = isolated(
            "plan_gate::tests::n1_int1_agent_run_end_after_switch_settles_and_notifies_the_submit_conversation",
        ) else {
            return;
        };
        let store = awili_persist::Store::open_default().unwrap();
        store.create_conversation(CONV, "plan", 0).unwrap();
        cx.executor().allow_parking();
        let bin = fake_claude(&home);
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            choose_claude(app, cx);
            app.on_notify_signal(BackendSignal::Permission(Permission::Authorized), cx);
            app.notifier.observe_window(
                false,
                std::time::Instant::now() - std::time::Duration::from_secs(30),
            );
            app.work.agents.claude_bin = Some(bin);
            app.submit_work_run("fix it".into(), cx);
            assert!(app.work.running);
            assert!(app.work.plan_gates.is_armed(&work(CONV)));
            // A run long enough to notify (N1's minimum run time; the N1
            // agent-run test backdates its start the same way).
            app.work.run_started =
                Some(std::time::Instant::now() - std::time::Duration::from_secs(20));
            // The user moves to conversation B while A's run is live.
            app.views.insert(
                OTHER.into(),
                crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
            );
            app.set_active_conversation(OTHER);
        });
        wait_idle(cx, &entity);
        entity.update(cx, |app, _cx| {
            assert!(
                app.work.plan_gates.open_for(&work(CONV)).is_some(),
                "A's plan card opens"
            );
            assert!(
                app.work.plan_gates.open_for(&work(OTHER)).is_none(),
                "B gets no plan card"
            );
            assert!(!app.plan_gate_pending_here(), "no card on B's screen");
            let reply = texts(app)[1].1.clone();
            assert!(
                !reply.contains(PLAN_READY_MARKER),
                "A's reply is clean: {reply}"
            );
            assert!(reply.contains("2. add a test"), "{reply}");
            let routes: Vec<String> = app
                .notifier
                .test_log()
                .into_iter()
                .filter_map(|r| match r {
                    Recorded::Native(n) => Some(n.id),
                    _ => None,
                })
                .collect();
            assert!(!routes.is_empty(), "the run end posts a notification");
            assert!(
                routes
                    .iter()
                    .all(|id| *id == route_id(&Target::Work(CONV.into()))),
                "every notification targets A: {routes:?}"
            );
            app.set_active_conversation(CONV);
            assert!(app.plan_gate_pending_here(), "back in A, its card shows");
        });
    }

    // ---- U4e2: the plan gate on a Code tab --------------------------------

    /// A pure `PlanGates` for a Work conversation keyed by `CONV` (U4e2 tests).
    fn work_key() -> AskKey {
        AskKey::Work(CONV.to_string())
    }

    /// A pure `PlanGates` for Code tab 1 (U4e2 tests).
    fn code_key_1() -> AskKey {
        AskKey::CodeTab(1)
    }

    /// P1 (U4e2): a gate on `CodeTab(1)` does not show for `Work("1")`, and
    /// vice versa — the two lanes key off different `AskKey`s and never cross.
    #[test]
    fn u4e2_code_and_work_gates_are_independent() {
        let mut g = PlanGates::default();
        g.open.insert(
            code_key_1(),
            PlanGate {
                engine: CodeEngine::Claude,
                model: None,
            },
        );
        // The Code lane sees it; the Work lane does not.
        assert!(g.open_for(&code_key_1()).is_some(), "the Code gate is open");
        assert!(
            g.open_for(&work_key()).is_none(),
            "the Work lane is untouched"
        );
        g.clear(&code_key_1());
        assert!(g.open_for(&code_key_1()).is_none());
        assert!(g.open_for(&work_key()).is_none());

        // And the reverse: a Work gate never reaches the Code lane.
        let mut g = PlanGates::default();
        g.open.insert(
            work_key(),
            PlanGate {
                engine: CodeEngine::Claude,
                model: None,
            },
        );
        assert!(g.open_for(&work_key()).is_some(), "the Work gate is open");
        assert!(
            g.open_for(&code_key_1()).is_none(),
            "the Code lane is untouched"
        );
        g.clear(&work_key());
        assert!(g.open_for(&work_key()).is_none());
        assert!(g.open_for(&code_key_1()).is_none());
    }

    /// P2 (U4e2): a new Code message (a non-follow-up turn) replaces an open
    /// plan — the gate is cleared and the tab is re-armed as a planning step.
    /// Exercises the pure `next_turn` (the helper both `*_autonomy_for_turn`
    /// fns call) over a Code-tab key: a message typed while a plan is still
    /// awaiting Approve is NOT an approved follow-up, so `take_execute` is
    /// false and `next_turn` clears the open plan before resolving.
    #[test]
    fn u4e2_new_code_message_replaces_the_plan() {
        let mut gates = PlanGates::default();
        let key = code_key_1();
        let support = AskSupport::PlanFirst;
        let mode = awili_work::ApprovalMode::AskBeforeChanges;
        let engine = CodeEngine::Claude;

        // First planning turn: arms the gate and runs AskFirst.
        let a1 = next_turn(&mut gates, key.clone(), support, mode, engine, None);
        assert_eq!(a1, Autonomy::AskFirst);
        assert!(gates.is_armed(&key), "the planning step is armed");

        // The planning step settles with a marker: the gate opens.
        let plan = format!("1. edit main.rs\n2. add a test\n{PLAN_READY_MARKER}");
        let (_clean, opened) = gates.settle(&key, &plan, true);
        assert!(opened, "the marker opens the plan card");
        assert!(gates.open_for(&key).is_some(), "the gate is open");
        assert!(!gates.is_armed(&key), "settling disarms the planning step");

        // No approve yet: the user types a NEW message. It is not the approved
        // follow-up, so `take_execute` is false, `next_turn` clears the open
        // plan and re-arms the tab as a fresh planning step.
        assert!(!gates.take_execute(&key), "no follow-up was released");
        let a2 = next_turn(&mut gates, key.clone(), support, mode, engine, None);
        assert_eq!(
            a2,
            Autonomy::AskFirst,
            "a new message runs the planning step again"
        );
        assert!(gates.open_for(&key).is_none(), "the old plan was replaced");
        assert!(gates.is_armed(&key), "the new planning step is armed");
    }

    /// P3 (U4e2): after an approve, the next turn runs `Autonomous` exactly
    /// once, then asks again.
    #[test]
    fn u4e2_follow_up_runs_full_access_once() {
        let mut gates = PlanGates::default();
        let key = code_key_1();
        let support = AskSupport::PlanFirst;
        let mode = awili_work::ApprovalMode::AskBeforeChanges;
        let engine = CodeEngine::Claude;

        // Arm and open a plan.
        let _a1 = next_turn(&mut gates, key.clone(), support, mode, engine, None);
        let plan = format!("1. edit main.rs\n{PLAN_READY_MARKER}");
        let (_c, opened) = gates.settle(&key, &plan, true);
        assert!(opened);

        // Approve: the NEXT turn is released as full access.
        gates.approve(&key);
        assert!(gates.open_for(&key).is_none(), "approve closes the plan");
        let a2 = next_turn(&mut gates, key.clone(), support, mode, engine, None);
        assert_eq!(a2, Autonomy::Autonomous, "the follow-up runs full access");
        assert!(
            !gates.is_armed(&key),
            "the follow-up is not re-armed as a planning step"
        );

        // The release is consumed: the next turn asks again.
        let a3 = next_turn(&mut gates, key.clone(), support, mode, engine, None);
        assert_eq!(
            a3,
            Autonomy::AskFirst,
            "the turn after the follow-up asks again"
        );
        assert!(gates.is_armed(&key), "the next planning step is armed");
    }

    // ---- U4e2-FIX1: the Code-tab plan, end to end -------------------------

    /// Drive the app until the active Code tab's turn has finished.
    fn wait_code_idle(cx: &mut gpui::TestAppContext, entity: &gpui::Entity<AwiliApp>) {
        let deadline = std::time::Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
        loop {
            cx.run_until_parked();
            let streaming = entity.read_with(cx, |app, _| {
                app.work
                    .code_tabs
                    .get(app.work.code_active)
                    .is_some_and(|t| t.streaming)
            });
            if !streaming {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the Code turn never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn code_texts(app: &AwiliApp) -> Vec<(awili_core::Role, String)> {
        app.work.code_tabs[app.work.code_active]
            .convo
            .messages()
            .iter()
            .map(|m| (m.role, m.text.clone()))
            .collect()
    }

    /// Review P1+P1+P2 (U4e2): over a fake Claude, an Ask Code turn shows the
    /// plan card with the marker stripped; Approve after a model change or a
    /// chooser/model and project-folder changes are next-turn state: Approve
    /// still runs ONE full-access turn on the plan's captured owner and keeps
    /// the selected next target intact.
    #[gpui::test]
    fn u4e2_code_plan_approval_keeps_next_target_but_runs_captured_owner(
        cx: &mut gpui::TestAppContext,
    ) {
        let Some(home) = isolated(
            "plan_gate::tests::u4e2_code_plan_approval_keeps_next_target_but_runs_captured_owner",
        ) else {
            return;
        };
        cx.executor().allow_parking();
        let bin = fake_claude(&home);
        let (proj_a, proj_b) = (home.join("proj-a"), home.join("proj-b"));
        std::fs::create_dir_all(&proj_a).unwrap();
        std::fs::create_dir_all(&proj_b).unwrap();
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.fleet_pack = None;
            app.work.agents.claude_bin = Some(bin);
            // Every other harness the chooser can name is a path that does
            // not exist, so nothing but the faked Claude can ever run.
            app.work.agents.codex_bin = Some(home.join("no-such-codex").display().to_string());
            app.work.agents.dsh_bin = Some(home.join("no-such-dsh").display().to_string());
            app.work.projects = vec![
                awili_persist::StoredProject {
                    name: "a".into(),
                    folder: proj_a.display().to_string(),
                    id: 0,
                    instructions: String::new(),
                    created_ts: 0,
                },
                awili_persist::StoredProject {
                    name: "b".into(),
                    folder: proj_b.display().to_string(),
                    id: 0,
                    instructions: String::new(),
                    created_ts: 0,
                },
            ];
            app.work.code_active_project = 0;
            app.mode = crate::work::Mode::Code;
            app.work.workshop_selected_folder = Some(proj_a.display().to_string());
            app.ensure_code_tab(cx);
            app.test_choose_native_target(CodeEngine::Claude, None, cx);
            app.attach_bytes("plan.txt", "text/plain", b"CODE-ATTACHMENT-MARKER", cx);
            app.submit_code_turn("fix it".into(), cx);
            assert!(app.work.code_tabs[app.work.code_active].streaming);
        });
        wait_code_idle(cx, &entity);
        entity.update(cx, |app, cx| {
            assert!(app.code_plan_gate_open(), "the plan card shows");
            let plan = code_texts(app)[1].1.clone();
            assert!(!plan.contains(PLAN_READY_MARKER), "{plan}");
            assert!(plan.contains("2. add a test"), "{plan}");
            let active = app.work.code_active;
            let conv = app.work.code_tabs[active].history.conv_id.clone().unwrap();
            // Chosen through the chooser's own setter, on the shown tab.
            let tab_id = app.work.code_tabs[active].id;
            assert!(app.set_composer_target(
                &crate::workshop_composer::ModelOwner::Native(tab_id),
                WorkExecutor::Agent {
                    engine: CodeEngine::Dsh,
                    model: Some("next-model".into()),
                },
                cx,
            ));
            // A different folder is likewise a next-turn change. The approval
            // must keep it selected but run the plan in proj-a.
            app.work.workshop_selected_folder = Some(proj_b.display().to_string());
            app.resolve_code_plan_gate(true, cx);
            assert!(!app.code_plan_gate_open(), "the card is gone");
            assert!(app.work.code_tabs[active].streaming, "the follow-up runs");
            assert_eq!(
                app.work.conversation_routes[&conv].next_target(),
                &WorkExecutor::Agent {
                    engine: CodeEngine::Dsh,
                    model: Some("next-model".into()),
                },
                "approval did not overwrite the selected next target"
            );
        });
        wait_code_idle(cx, &entity);
        entity.update(cx, |app, _| {
            assert!(!app.code_plan_gate_open());
            let texts = code_texts(app);
            assert_eq!(texts.len(), 4, "{texts:?}");
            assert_eq!(
                texts[2],
                (awili_core::Role::User, PLAN_APPROVED_TEXT.to_string())
            );
            assert_eq!(texts[3], (awili_core::Role::Assistant, "Done.".to_string()));
        });
        let first = argv(&home, 1);
        assert!(
            first.iter().any(|arg| arg.contains("CODE-ATTACHMENT-MARKER")),
            "the provider request contains the exact attachment-enriched current request: {first:?}"
        );
        let pm = first.iter().position(|a| a == "--permission-mode");
        assert_eq!(
            pm.and_then(|i| first.get(i + 1)).map(String::as_str),
            Some("plan"),
            "the first Code turn is the planning step: {first:?}"
        );
        let second = argv(&home, 2);
        assert!(
            !second.iter().any(|a| a == "--permission-mode"),
            "the approved turn has full access: {second:?}"
        );
        let resume = second.iter().position(|a| a == "--resume");
        assert_eq!(
            resume.and_then(|i| second.get(i + 1)).map(String::as_str),
            Some("s-plan"),
            "the approved turn resumes the planning session: {second:?}"
        );
        assert!(argv(&home, 3).is_empty(), "exactly two CLI runs");
    }

    // ---- U4e1-FIX2: the Work plan is bound to its folder ---------------------

    /// A Work plan remains owned by its planning folder even after the chooser
    /// moves the next ordinary turn to another folder.
    #[gpui::test]
    fn u4e1_fix2_work_plan_approval_checks_the_folder(cx: &mut gpui::TestAppContext) {
        let Some(home) =
            isolated("plan_gate::tests::u4e1_fix2_work_plan_approval_checks_the_folder")
        else {
            return;
        };
        let store = awili_persist::Store::open_default().unwrap();
        store.create_conversation(CONV, "plan", 0).unwrap();
        cx.executor().allow_parking();
        let bin = fake_claude(&home);
        let other = home.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let entity = cx.new(AwiliApp::new);
        let planned = entity.update(cx, |app, cx| {
            plan_app(app, &home);
            choose_claude(app, cx);
            app.work.agents.claude_bin = Some(bin);
            app.submit_work_run("fix it".into(), cx);
            app.work.folder.clone().unwrap()
        });
        wait_idle(cx, &entity);
        entity.update(cx, |app, cx| {
            assert!(app.plan_gate_pending_here(), "the plan card opens");
            let seq = app.work.run_seq;

            // The current folder changes, but approval uses the captured one.
            app.work.folder = Some(other.clone());
            app.resolve_work_approval(ApprovalDecision::Approve, cx);
            assert!(!app.plan_gate_pending_here());
            assert_eq!(app.work.run_seq, seq + 1, "exactly one turn dispatched");
            assert_eq!(
                app.work
                    .active_work_route_turn
                    .as_ref()
                    .map(|turn| turn.cwd.clone()),
                Some(planned.display().to_string())
            );
        });
        wait_idle(cx, &entity);
        assert!(argv(&home, 3).is_empty(), "exactly two CLI runs");
        let second = argv(&home, 2);
        assert!(
            !second.iter().any(|a| a == "--permission-mode"),
            "the approved turn has full access: {second:?}"
        );
        let resume = second.iter().position(|a| a == "--resume");
        assert_eq!(
            resume.and_then(|i| second.get(i + 1)).map(String::as_str),
            Some("s-plan"),
            "the approved turn resumes the planning session: {second:?}"
        );
    }

    /// Integration review P1: after the user opens another conversation, the
    /// Work chip shows and changes the ACTIVE conversation's choice (the one
    /// the next submission uses), not the previous run's `work.conv_id` — the
    /// displayed mode, the saved choice and the dispatched mode agree.
    #[gpui::test]
    fn u4e1_fix2_work_chip_follows_the_active_conversation(cx: &mut gpui::TestAppContext) {
        use crate::ask_mode::{chip_text, effective, work_support};
        use crate::ask_mode_ui::{choose, AskMenu, MenuStep};
        let Some(home) =
            isolated("plan_gate::tests::u4e1_fix2_work_chip_follows_the_active_conversation")
        else {
            return;
        };
        let store = awili_persist::Store::open_default().unwrap();
        store.create_conversation(CONV, "chip", 0).unwrap();
        cx.executor().allow_parking();
        let bin = fake_claude(&home);
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            choose_claude(app, cx);
            app.work.agents.claude_bin = Some(bin);
            // The previous run was in another conversation; CONV is open now.
            let previous = AskKey::Work("previous-conv".to_string());
            app.work.conv_id = Some("previous-conv".to_string());
            let support = work_support(&app.work.executor, app.work.engine.kind);
            let shown = |app: &AwiliApp| {
                let key = app.work_chip_key();
                chip_text(effective(app.work.ask_modes.mode_for(&key), support)).label
            };
            assert_eq!(
                app.work_chip_key(),
                work(CONV),
                "the chip is the open conversation's"
            );
            assert_eq!(shown(app), "Plan first");

            // A NEXT pick of a helper that cannot ask says so, and keeps an
            // open plan; picking Claude again restores the chip.
            open_plan(&mut app.work.plan_gates, &work(CONV));
            app.work.agents.dsh_bin = Some(home.join("no-such-dsh").display().to_string());
            app.pick_work_executor(
                WorkExecutor::Agent {
                    engine: CodeEngine::Dsh,
                    model: None,
                },
                cx,
            );
            assert!(app.work.plan_gates.open_for(&work(CONV)).is_some());
            assert_eq!(
                app.toast.visible().map(|m| m.to_string()),
                Some(crate::ask_mode::no_ask_toast(
                    crate::ask_mode::code_support(CodeEngine::Dsh),
                    &WorkExecutor::Agent {
                        engine: CodeEngine::Dsh,
                        model: None,
                    }
                    .label(),
                )),
                "the support notice for a helper that cannot ask"
            );
            app.pick_work_executor(
                WorkExecutor::Agent {
                    engine: CodeEngine::Claude,
                    model: None,
                },
                cx,
            );
            assert_eq!(shown(app), "Plan first");

            // The chip's menu: Auto, confirmed.
            app.work.ask_menu = AskMenu::Open(app.work_chip_key());
            let MenuStep::Confirm(k) = choose(&app.work.ask_menu, ApprovalMode::Auto, None) else {
                panic!("Auto asks for a confirmation")
            };
            app.work.ask_menu = AskMenu::ConfirmAuto(k);
            let MenuStep::SetMode(k, mode) = choose(&app.work.ask_menu, ApprovalMode::Auto, None)
            else {
                panic!("the confirmation applies Auto")
            };
            app.set_ask_mode(k, mode, cx);
            assert_eq!(shown(app), "Auto", "the chip shows the new choice");
            assert_eq!(app.work.ask_modes.mode_for(&work(CONV)), ApprovalMode::Auto);
            assert_eq!(
                app.work.ask_modes.mode_for(&previous),
                ApprovalMode::AskBeforeChanges,
                "the previous conversation's choice is untouched"
            );

            // Picking another helper is a NEXT-turn choice: it keeps the
            // OPEN conversation's plan, and the plan's captured owner (Claude)
            // is who runs when it is approved.
            open_plan(&mut app.work.plan_gates, &work(CONV));
            // The plan came from CONV's own planning run, so the Work panel's
            // owner is the open conversation again. (Left on "previous-conv",
            // the card is not shown here, `plan_gate_pending_here` is false
            // and Approve is deliberately ignored: zero turns.)
            app.work.conv_id = Some(CONV.to_string());
            assert!(
                app.plan_gate_pending_here(),
                "setup: the card is actionable"
            );
            app.work.agents.codex_bin = Some(home.join("no-such-codex").display().to_string());
            app.pick_work_executor(
                WorkExecutor::Agent {
                    engine: CodeEngine::Codex,
                    model: None,
                },
                cx,
            );
            assert!(
                app.work.plan_gates.open_for(&work(CONV)).is_some(),
                "a NEXT-target pick keeps the open plan"
            );
            assert_eq!(
                app.composer_target_for(&crate::workshop_composer::ModelOwner::Work(CONV.into())),
                Some(WorkExecutor::Agent {
                    engine: CodeEngine::Codex,
                    model: None,
                }),
                "the pick is the conversation's next target"
            );

            // Approving runs the plan's captured owner with the chip's choice
            // (Auto), not the newly picked next target.
            let seq = app.work.run_seq;
            app.resolve_work_approval(ApprovalDecision::Approve, cx);
            assert_eq!(app.work.run_seq, seq + 1, "exactly one turn dispatched");
            assert!(app.work.running);
        });
        wait_idle(cx, &entity);
        let first = argv(&home, 1);
        assert!(
            first.iter().any(|a| a == "--dangerously-skip-permissions")
                && !first.iter().any(|a| a == "--permission-mode"),
            "the approved turn ran as Claude with the chip's Auto: {first:?}"
        );
        assert!(argv(&home, 2).is_empty(), "only the captured owner ran");
        // The chip reads the same conversation the source dispatches from.
        let src = include_str!("ask_mode_ui.rs");
        let chip = &src[src.find("pub(crate) fn work_ask_mode_chip(").unwrap()..];
        let chip = &chip[..chip.find("\n    }\n").unwrap()];
        assert!(chip.contains("self.work_chip_key()"), "{chip}");
        assert!(!chip.contains("work.conv_id"), "{chip}");
    }

    // ---- U4e1-FIX3: "no folder" is a folder binding too ----------------------

    /// Slice re-review P1: a Work plan whose planning step ran with NO folder
    /// cannot be approved after a folder is chosen, and a plan made in a
    /// folder cannot be approved with no folder; the card stays, a toast says
    /// why, and nothing runs. Back on the planning folder (or none), Approve
    /// goes through.
    #[gpui::test]
    fn u4e1_fix3_no_folder_plan_needs_no_folder(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::u4e1_fix3_no_folder_plan_needs_no_folder")
        else {
            return;
        };
        // The approved follow-up's CLI thread (a missing binary) may report.
        cx.executor().allow_parking();
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            let key = work(CONV);
            let chosen = app.work.folder.clone().unwrap();

            // Planned with no bound folder, then a folder is chosen: approval
            // runs the captured home-based owner and leaves the new folder for
            // the next ordinary turn.
            app.work.folder = None;
            assert_eq!(
                app.agent_autonomy_for_turn(CONV, CodeEngine::Claude, None),
                Autonomy::AskFirst
            );
            let (_, opened) = app.work.plan_gates.settle(&key, &plan_text(), true);
            assert!(opened);
            assert_eq!(app.work.plan_gates.folder_for(&key), Some(None));
            app.work.folder = Some(chosen.clone());
            let seq = app.work.run_seq;
            app.resolve_work_approval(ApprovalDecision::Approve, cx);
            assert!(!app.plan_gate_pending_here(), "the card is gone");
            assert_eq!(app.work.run_seq, seq + 1, "exactly one turn dispatched");
            app.cancel_work_run(cx);
        });
    }

    // ---- approval preserves the captured folder ------------------------

    /// An approval after a folder change dispatches on the captured folder;
    /// it is no longer a path-disclosing refusal card.
    #[gpui::test]
    fn r1d_work_plan_folder_refusal_is_path_free_and_stays(cx: &mut gpui::TestAppContext) {
        let Some(home) =
            isolated("plan_gate::tests::r1d_work_plan_folder_refusal_is_path_free_and_stays")
        else {
            return;
        };
        // The approved follow-up's CLI thread (a missing binary) reports back.
        cx.executor().allow_parking();
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            let key = work(CONV);
            let planned = app.work.folder.clone().unwrap();
            assert_eq!(
                app.agent_autonomy_for_turn(CONV, CodeEngine::Claude, None),
                Autonomy::AskFirst
            );
            let (_, opened) = app.work.plan_gates.settle(&key, &plan_text(), true);
            assert!(opened);
            let other = home.join("elsewhere");
            std::fs::create_dir_all(&other).unwrap();
            app.work.folder = Some(other);
            let seq = app.work.run_seq;
            app.resolve_work_approval(ApprovalDecision::Approve, cx);
            assert!(!app.plan_gate_pending_here(), "the card was consumed");
            assert_eq!(app.work.run_seq, seq + 1, "the captured owner ran");
            // The turn is opened synchronously; read its folder before the
            // CLI thread can report back and close it.
            assert_eq!(
                app.work
                    .active_work_route_turn
                    .as_ref()
                    .map(|turn| turn.cwd.clone()),
                Some(planned.display().to_string())
            );
        });
        // The CLI thread reports back on its own schedule: pump until it has.
        for _ in 0..400 {
            cx.executor()
                .advance_clock(std::time::Duration::from_secs(10));
            cx.run_until_parked();
            if entity.read_with(cx, |app, _| {
                texts(app)
                    .iter()
                    .any(|(_, text)| text.contains("could not start `claude`"))
            }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        entity.update(cx, |app, _cx| {
            assert!(
                texts(app)
                    .iter()
                    .any(|(_, text)| text.contains("could not start `claude`")),
                "the missing CLI reports back on the captured turn"
            );
        });
    }

    // ---- R1b: the plan-ready marker is never shown nor saved ---------------

    /// The request typed before the plan (R1b/R1c).
    const REQUEST: &str = "Rename every .txt to .md";

    /// A plan whose marker arrives split over tokens, as a CLI streams it.
    const PLAN_CHUNKS: [&str; 7] = [
        "Plan:\n1. rename the files\n",
        "[",
        "[awi",
        "li:pla",
        "n-re",
        "ady]",
        "]",
    ];

    /// The plan text once the marker is gone.
    const PLAN_CLEAN: &str = "Plan:\n1. rename the files";

    /// Push a row into `CONV` without persisting it; returns its index.
    fn push_row(app: &mut AwiliApp, role: awili_core::Role, text: &str) -> usize {
        let view = app.views.get_mut(CONV).unwrap();
        let idx = view.rows.len();
        view.convo.push(role, text.to_string());
        view.rows
            .push(crate::MessageRow::new(view.convo.message(idx).unwrap()));
        idx
    }

    /// An armed Work planning step for [`REQUEST`], streaming into a fresh
    /// assistant row of `CONV` (what `submit_work_run_in` sets up). Returns
    /// that row.
    fn start_work_plan_stream(app: &mut AwiliApp) -> usize {
        push_row(app, awili_core::Role::User, REQUEST);
        let idx = push_row(app, awili_core::Role::Assistant, "");
        assert_eq!(
            app.agent_autonomy_for_turn(CONV, CodeEngine::Claude, None),
            Autonomy::AskFirst,
            "setup: the planning step is armed"
        );
        app.work.conv_id = Some(CONV.to_string());
        app.work.stream_row_idx = Some(idx);
        app.work.running = true;
        idx
    }

    /// What the Work row at `idx` displays (and copies).
    fn work_shown(app: &AwiliApp, idx: usize) -> String {
        app.views[CONV].rows[idx].raw.to_string()
    }

    /// The assistant replies saved for `CONV` (waits for the persistence
    /// worker until `n` are there).
    fn saved_replies(store: &awili_persist::Store, n: usize) -> Vec<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let saved: Vec<String> = store
                .messages(CONV)
                .unwrap()
                .into_iter()
                .filter(|m| m.role == "assistant")
                .map(|m| m.text)
                .collect();
            if saved.len() >= n {
                return saved;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the reply was never saved: {saved:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// R1b (findings Major 2): a Work planning step whose marker arrives
    /// split over tokens never shows any part of it, at any token; the raw
    /// text stays in the message, so the finish still opens the plan card and
    /// saves the reply without the marker.
    #[gpui::test]
    fn r1b_work_stream_never_shows_the_marker(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::r1b_work_stream_never_shows_the_marker")
        else {
            return;
        };
        let store = awili_persist::Store::open_default().unwrap();
        store.create_conversation(CONV, "plan", 0).unwrap();
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            let idx = start_work_plan_stream(app);
            for (i, chunk) in PLAN_CHUNKS.iter().enumerate() {
                app.work_stream_token(chunk, cx);
                let shown = work_shown(app, idx);
                assert!(!shown.contains('['), "token {i}: {shown:?}");
            }
            assert!(
                texts(app)[idx].1.ends_with(PLAN_READY_MARKER),
                "the message keeps the raw text for the settle"
            );
            // The agent lane's finish closure (work.rs): settle, then finish.
            app.work.running = false;
            app.work.panel.status = Some(RunStatus::Completed);
            app.plan_gate_after_work_turn(CONV, cx);
            app.work_finish_stream(cx);
            assert!(app.plan_gate_pending_here(), "the plan card opens");
            assert_eq!(texts(app)[idx].1, PLAN_CLEAN);
            assert_eq!(work_shown(app, idx), PLAN_CLEAN);
        });
        assert_eq!(saved_replies(&store, 1), [PLAN_CLEAN.to_string()]);
    }

    /// R1b: a Work planning step stopped while the marker is half streamed
    /// shows and saves no part of it, and opens no card.
    #[gpui::test]
    fn r1b_work_stop_mid_marker_saves_no_marker(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::r1b_work_stop_mid_marker_saves_no_marker")
        else {
            return;
        };
        let store = awili_persist::Store::open_default().unwrap();
        store.create_conversation(CONV, "plan", 0).unwrap();
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            let idx = start_work_plan_stream(app);
            for chunk in [PLAN_CHUNKS[0], "[[awili:pla"] {
                app.work_stream_token(chunk, cx);
                assert!(!work_shown(app, idx).contains('['));
            }
            let (abort_tx, _abort_rx) = std::sync::mpsc::channel();
            app.work.agent_abort_tx = Some(abort_tx);
            app.cancel_work_run(cx);
            assert!(!app.plan_gate_pending_here(), "a Stop opens no card");
            let text = texts(app)[idx].1.clone();
            assert!(!text.contains("[["), "{text:?}");
            assert!(text.starts_with(PLAN_CLEAN), "{text:?}");
            assert_eq!(work_shown(app, idx), text);
        });
        let saved = saved_replies(&store, 1);
        assert!(!saved[0].contains("[["), "saved: {saved:?}");
    }

    /// R1b: hiding a start of the marker is for the stream only. A reply that
    /// finishes cleanly with its own brackets shows them in full.
    #[gpui::test]
    fn r1b_finished_reply_keeps_its_own_brackets(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::r1b_finished_reply_keeps_its_own_brackets")
        else {
            return;
        };
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            let idx = start_work_plan_stream(app);
            app.work_stream_token("Wiki links start with ", cx);
            app.work_stream_token("[[", cx);
            assert_eq!(work_shown(app, idx), "Wiki links start with ");
            app.work.running = false;
            app.work.panel.status = Some(RunStatus::Completed);
            app.plan_gate_after_work_turn(CONV, cx);
            app.work_finish_stream(cx);
            assert!(!app.plan_gate_pending_here(), "no marker, no card");
            assert_eq!(work_shown(app, idx), "Wiki links start with [[");
        });
    }

    /// A Claude Code tab with an armed planning step for [`REQUEST`],
    /// streaming into its assistant row (what `submit_code_turn` sets up,
    /// without a CLI). Returns the tab id; the assistant row is 1.
    fn start_code_plan_stream(
        app: &mut AwiliApp,
        home: &std::path::Path,
        cx: &mut Context<AwiliApp>,
    ) -> u64 {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.work.executor = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        };
        let proj = home.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        app.work.projects = vec![awili_persist::StoredProject {
            name: "proj".into(),
            folder: proj.display().to_string(),
            id: 0,
            instructions: String::new(),
            created_ts: 0,
        }];
        app.work.code_active_project = 0;
        app.mode = crate::work::Mode::Code;
        app.ensure_code_tab(cx);
        assert_eq!(
            app.code_autonomy_for_turn(),
            Autonomy::AskFirst,
            "setup: the planning step is armed"
        );
        let active = app.work.code_active;
        let tab = &mut app.work.code_tabs[active];
        tab.project_folder = Some(proj.display().to_string());
        for (role, text) in [
            (awili_core::Role::User, REQUEST),
            (awili_core::Role::Assistant, ""),
        ] {
            let i = tab.rows.len();
            tab.convo.push(role, text.to_string());
            tab.rows
                .push(crate::MessageRow::new(tab.convo.message(i).unwrap()));
        }
        tab.streaming = true;
        let tab_id = tab.id;
        app.code_turn_begin(tab_id, REQUEST, 1);
        tab_id
    }

    /// What the active Code tab's row 1 displays, its message text, and the
    /// reply mirrored into its saved conversation.
    fn code_reply(app: &AwiliApp) -> (String, String, Option<String>) {
        let tab = &app.work.code_tabs[app.work.code_active];
        let saved = tab.history.conv_id.as_ref().and_then(|id| {
            let msgs = app.views.get(id)?.convo.messages();
            msgs.iter()
                .rev()
                .find(|m| m.role == awili_core::Role::Assistant)
                .map(|m| m.text.clone())
        });
        (
            tab.rows[1].raw.to_string(),
            tab.convo.message(1).unwrap().text.clone(),
            saved,
        )
    }

    /// R1b: the Code-tab lane. A split marker never shows at any token, the
    /// finish opens the Code plan card, and the saved reply has no marker.
    #[gpui::test]
    fn r1b_code_stream_never_shows_the_marker(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::r1b_code_stream_never_shows_the_marker")
        else {
            return;
        };
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            let tab_id = start_code_plan_stream(app, &home, cx);
            for (i, chunk) in PLAN_CHUNKS.iter().enumerate() {
                app.code_on_token(tab_id, 1, chunk, cx);
                let (shown, _, _) = code_reply(app);
                assert!(!shown.contains('['), "token {i}: {shown:?}");
            }
            assert!(code_reply(app).1.ends_with(PLAN_READY_MARKER));
            app.finish_code_turn(tab_id, cx);
            assert!(app.code_plan_gate_open(), "the Code plan card opens");
            assert_eq!(
                code_reply(app),
                (
                    PLAN_CLEAN.to_string(),
                    PLAN_CLEAN.to_string(),
                    Some(PLAN_CLEAN.to_string())
                )
            );
        });
    }

    /// R1b: a Code planning step stopped mid-marker shows and saves no part
    /// of it (the stopped suffix is kept), and opens no card.
    #[gpui::test]
    fn r1b_code_stop_mid_marker_saves_no_marker(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::r1b_code_stop_mid_marker_saves_no_marker")
        else {
            return;
        };
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            let tab_id = start_code_plan_stream(app, &home, cx);
            for chunk in [PLAN_CHUNKS[0], "[[awili:pla"] {
                app.code_on_token(tab_id, 1, chunk, cx);
                assert!(!code_reply(app).0.contains('['));
            }
            let (abort_tx, _abort_rx) = std::sync::mpsc::channel();
            let active = app.work.code_active;
            app.work.code_tabs[active].stream_abort_tx = Some(abort_tx);
            app.cancel_code_turn(cx);
            // The killed CLI closes the stream; the provider loop then finishes.
            app.finish_code_turn(tab_id, cx);
            assert!(!app.code_plan_gate_open(), "a Stop opens no card");
            let (shown, text, saved) = code_reply(app);
            let saved = saved.expect("the stopped reply is saved");
            for t in [&shown, &text, &saved] {
                assert!(!t.contains("[[awili"), "{t:?}");
                assert!(t.starts_with(PLAN_CLEAN), "{t:?}");
            }
            assert!(saved.ends_with(crate::conv_view::STOPPED_SUFFIX));
        });
    }

    // ---- R1c: Deny puts the request back into the composer -----------------

    /// The composer's text.
    fn composer_text(app: &AwiliApp, cx: &gpui::App) -> String {
        app.composer.read(cx).text().to_string()
    }

    /// R1c (findings F1, most tiring step): Deny on a Work plan puts the
    /// request that produced it back into the composer with a toast, and
    /// runs nothing. Text typed in the meantime stays, under the request.
    #[gpui::test]
    fn r1c_work_deny_puts_the_request_back(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::r1c_work_deny_puts_the_request_back") else {
            return;
        };
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            plan_app(app, &home);
            push_row(app, awili_core::Role::User, REQUEST);
            push_row(app, awili_core::Role::Assistant, PLAN_CLEAN);
            let key = work(CONV);
            open_plan(&mut app.work.plan_gates, &key);
            assert!(composer_text(app, cx).is_empty());
            let seq = app.work.run_seq;

            app.resolve_work_approval(ApprovalDecision::Deny, cx);
            assert_eq!(
                composer_text(app, cx),
                REQUEST,
                "the request is back in the box"
            );
            assert_eq!(
                app.toast.msg.as_ref().map(|m| m.to_string()),
                Some(PLAN_DENIED_TOAST.to_string())
            );
            assert_eq!(app.work.run_seq, seq, "Deny runs nothing");
            assert!(!app.work.running);
            assert!(!app.plan_gate_pending_here(), "the card is gone");
            assert_eq!(
                texts(app),
                [
                    (awili_core::Role::User, REQUEST.to_string()),
                    (awili_core::Role::Assistant, PLAN_CLEAN.to_string()),
                    (awili_core::Role::User, PLAN_DECLINED_TEXT.to_string()),
                ],
                "the decline record is unchanged"
            );

            // Another plan, with new text typed meanwhile: the request goes on
            // top and the typed text stays (the `merged_draft` rule).
            push_row(app, awili_core::Role::Assistant, PLAN_CLEAN);
            open_plan(&mut app.work.plan_gates, &key);
            app.composer
                .update(cx, |c, cx| c.set_text("and the .log files", cx));
            app.resolve_work_approval(ApprovalDecision::Deny, cx);
            assert_eq!(
                composer_text(app, cx),
                format!("{REQUEST}\n\nand the .log files")
            );
            assert_eq!(app.work.run_seq, seq, "Deny runs nothing");
        });
    }

    /// R1c: the same on a Code tab, after a real streamed planning step
    /// opened the Code plan card.
    #[gpui::test]
    fn r1c_code_deny_puts_the_request_back(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::r1c_code_deny_puts_the_request_back") else {
            return;
        };
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            let tab_id = start_code_plan_stream(app, &home, cx);
            for chunk in PLAN_CHUNKS {
                app.code_on_token(tab_id, 1, chunk, cx);
            }
            app.finish_code_turn(tab_id, cx);
            assert!(app.code_plan_gate_open(), "setup: the Code plan card shows");
            assert!(composer_text(app, cx).is_empty());

            app.resolve_code_plan_gate(false, cx);
            assert_eq!(
                composer_text(app, cx),
                REQUEST,
                "the request is back in the box"
            );
            assert_eq!(
                app.toast.msg.as_ref().map(|m| m.to_string()),
                Some(PLAN_DENIED_TOAST.to_string())
            );
            assert!(!app.code_plan_gate_open(), "the card is gone");
            assert!(
                !app.work.code_tabs[app.work.code_active].streaming,
                "Deny runs nothing"
            );
            assert_eq!(
                code_texts(app),
                [
                    (awili_core::Role::User, REQUEST.to_string()),
                    (awili_core::Role::Assistant, PLAN_CLEAN.to_string()),
                    (awili_core::Role::User, PLAN_DECLINED_TEXT.to_string()),
                ]
            );
        });
    }

    /// RC2-RUNF (review P2): on a Code tab the rows the plan card's clicks
    /// write are marked as clicks (on the tab and in its saved
    /// conversation); the same sentence typed by the owner is not.
    #[gpui::test]
    fn rc2runf_code_plan_clicks_are_marked_and_typed_words_are_not(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated(
            "plan_gate::tests::rc2runf_code_plan_clicks_are_marked_and_typed_words_are_not",
        ) else {
            return;
        };
        cx.executor().allow_parking();
        let click = awili_core::MessageKind::PlanClick;
        let normal = awili_core::MessageKind::Normal;
        // The newest user row on the tab and in its saved conversation.
        fn last_user(
            app: &AwiliApp,
        ) -> (
            String,
            awili_core::MessageKind,
            bool,
            awili_core::MessageKind,
        ) {
            let tab = &app.work.code_tabs[app.work.code_active];
            let i = tab
                .convo
                .messages()
                .iter()
                .rposition(|m| m.role == awili_core::Role::User)
                .unwrap();
            let saved = tab.history.conv_id.as_ref().unwrap();
            let mirror = app.views[saved]
                .convo
                .messages()
                .iter()
                .rev()
                .find(|m| m.role == awili_core::Role::User)
                .unwrap()
                .kind;
            let m = &tab.convo.messages()[i];
            (m.text.clone(), m.kind, tab.rows[i].plan_click, mirror)
        }
        let entity = cx.new(AwiliApp::new);
        entity.update(cx, |app, cx| {
            app.work.agents.claude_bin = Some(home.join("no-such-claude").display().to_string());
            let tab_id = start_code_plan_stream(app, &home, cx);
            for chunk in PLAN_CHUNKS {
                app.code_on_token(tab_id, 1, chunk, cx);
            }
            app.finish_code_turn(tab_id, cx);
            assert!(app.code_plan_gate_open(), "setup: the Code plan card shows");
            app.resolve_code_plan_gate(false, cx);
            let tab = &app.work.code_tabs[app.work.code_active];
            let last = tab.convo.messages().last().unwrap();
            assert_eq!((last.text.as_str(), last.kind), (PLAN_DECLINED_TEXT, click));
            assert!(tab.rows.last().unwrap().plan_click, "the Deny click");

            // The owner types the approval sentence himself.
            app.submit_code_turn(PLAN_APPROVED_TEXT.to_string(), cx);
            assert_eq!(
                last_user(app),
                (PLAN_APPROVED_TEXT.to_string(), normal, false, normal),
                "typed words are an ordinary message"
            );
        });
        wait_code_idle(cx, &entity);
        entity.update(cx, |app, cx| {
            let tab_id = app.work.code_tabs[app.work.code_active].id;
            open_plan(&mut app.work.plan_gates, &AskKey::CodeTab(tab_id));
            app.resolve_code_plan_gate(true, cx);
            assert_eq!(
                last_user(app),
                (PLAN_APPROVED_TEXT.to_string(), click, true, click),
                "the Approve click is marked, and saved marked"
            );
        });
        wait_code_idle(cx, &entity);
    }

    /// RC2-FIXA3 (RC2-RUNAPP review P2): the Code plan card's Deny row is
    /// saved in the tab's conversation, marked as the click, and comes back
    /// so after a relaunch (the reopened store), after the plan it declined.
    #[gpui::test]
    fn rc2fixa_a_code_deny_is_kept_after_a_relaunch(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::rc2fixa_a_code_deny_is_kept_after_a_relaunch")
        else {
            return;
        };
        cx.executor().allow_parking();
        let db = home.join("fixa3.db");
        let (persist, worker) =
            crate::persistence::Persistence::spawn_worker(awili_persist::Store::open(&db).unwrap());
        let entity = cx.new(AwiliApp::new);
        let conv = entity.update(cx, |app, cx| {
            app.persist = Some(persist);
            app.work.agents.claude_bin = Some(home.join("no-such-claude").display().to_string());
            let tab_id = start_code_plan_stream(app, &home, cx);
            for chunk in PLAN_CHUNKS {
                app.code_on_token(tab_id, 1, chunk, cx);
            }
            app.finish_code_turn(tab_id, cx);
            assert!(app.code_plan_gate_open(), "setup: the Code plan card shows");
            app.resolve_code_plan_gate(false, cx);
            let conv = app.work.code_tabs[app.work.code_active]
                .history
                .conv_id
                .clone()
                .expect("the tab has a saved conversation");
            let last = app.views[&conv].convo.messages().last().unwrap().clone();
            assert_eq!(
                (last.text.as_str(), last.kind),
                (PLAN_DECLINED_TEXT, awili_core::MessageKind::PlanClick),
                "the saved conversation has the Deny row"
            );
            drop(app.persist.take());
            conv
        });
        worker.join().unwrap();
        let rows = awili_persist::Store::open(&db)
            .unwrap()
            .messages(&conv)
            .unwrap();
        let stored: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|m| (m.role.as_str(), m.text.as_str(), m.kind.as_str()))
            .collect();
        assert_eq!(
            stored,
            [
                ("user", REQUEST, "normal"),
                ("assistant", PLAN_CLEAN, "normal"),
                ("user", PLAN_DECLINED_TEXT, "plan_click"),
            ]
        );
        let back = crate::restore_conversation(&rows);
        assert_eq!(
            back.messages().last().map(|m| m.kind),
            Some(awili_core::MessageKind::PlanClick),
            "after a relaunch the row is the click again"
        );
    }

    /// R1c: clicking the rendered Work "Don't change anything" pill puts the
    /// request back and leaves the cursor in the composer.
    #[gpui::test]
    fn r1c_work_deny_click_focuses_the_composer(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::r1c_work_deny_click_focuses_the_composer")
        else {
            return;
        };
        let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
        view.update(window, |app, cx| {
            plan_app(app, &home);
            push_row(app, awili_core::Role::User, REQUEST);
            push_row(app, awili_core::Role::Assistant, PLAN_CLEAN);
            open_plan(&mut app.work.plan_gates, &work(CONV));
            cx.notify();
        });
        window.run_until_parked();
        let deny = window
            .debug_bounds(PLAN_DENY_SELECTOR)
            .expect("the Deny pill is on screen");
        window.update(|window, _cx| window.blur());
        window.simulate_click(deny.center(), gpui::Modifiers::none());
        window.run_until_parked();
        view.update_in(window, |app, window, cx| {
            assert_eq!(composer_text(app, cx), REQUEST);
            assert!(!app.plan_gate_pending_here(), "the click denied the plan");
            assert!(
                app.composer.read(cx).focus_handle().is_focused(window),
                "the cursor is in the composer"
            );
        });
    }

    /// R1c: the same click on the Code tab's card.
    #[gpui::test]
    fn r1c_code_deny_click_focuses_the_composer(cx: &mut gpui::TestAppContext) {
        let Some(home) = isolated("plan_gate::tests::r1c_code_deny_click_focuses_the_composer")
        else {
            return;
        };
        let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
        view.update(window, |app, cx| {
            let tab_id = start_code_plan_stream(app, &home, cx);
            for chunk in PLAN_CHUNKS {
                app.code_on_token(tab_id, 1, chunk, cx);
            }
            app.finish_code_turn(tab_id, cx);
            assert!(app.code_plan_gate_open());
            cx.notify();
        });
        window.run_until_parked();
        let deny = window
            .debug_bounds(CODE_PLAN_DENY_SELECTOR)
            .expect("the Code Deny pill is on screen");
        window.update(|window, _cx| window.blur());
        window.simulate_click(deny.center(), gpui::Modifiers::none());
        window.run_until_parked();
        view.update_in(window, |app, window, cx| {
            assert_eq!(composer_text(app, cx), REQUEST);
            assert!(!app.code_plan_gate_open(), "the click denied the plan");
            assert!(
                app.composer.read(cx).focus_handle().is_focused(window),
                "the cursor is in the composer"
            );
        });
    }
}
