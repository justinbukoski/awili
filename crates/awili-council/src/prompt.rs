//! RC2-CNCL4: what each member is sent (Council plan §4).
//!
//! Every member gets one brief (the request's system text) and one user
//! message: the council so far as a transcript — the owner's messages by his
//! name, every earlier answer by its member's name, the member's own as
//! "You (Name)" — then the round's ask. Stopped answers are marked "(stopped
//! early)"; a member that didn't answer is one line. When the transcript is
//! over the member's window, the oldest rounds' answers go first; the owner's
//! messages always stay.

use crate::types::{Council, Member, RoundKind, TurnStatus};

/// Who the owner is and who this round goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanOptions {
    /// The owner's name as the members see it ("Owner").
    pub owner: String,
    /// `@`-named members (ask round). Empty = everyone.
    pub to: Vec<String>,
    /// The member asked to sum up (sum-up round).
    pub summarizer: Option<String>,
    /// RC2-R3: what the owner did with a member's plan, by its turn id (a
    /// declined plan stays declined in every later round, the sum-up too).
    pub plan_outcomes: Vec<(String, PlanOutcome)>,
}

impl PlanOptions {
    pub fn new(owner: &str) -> Self {
        Self {
            owner: owner.to_string(),
            to: Vec::new(),
            summarizer: None,
            plan_outcomes: Vec::new(),
        }
    }
}

/// RC2-R3: what the owner did with a member's plan (its plan card's "Make
/// the changes" or "Don't change anything") and, once he approved it, how
/// carrying it out went (RC2-R3M3: approving a plan is not doing it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanOutcome {
    /// Approved, and carried out: the follow-up answered without failing
    /// and its own steps show a change made (RC2-R3M13).
    Approved,
    Declined,
    /// RC2-R3M3: approved; the follow-up started but was not seen to end
    /// (still running, or Awili quit while it ran).
    Started,
    /// RC2-R3M3: approved, but the follow-up failed.
    Failed,
    /// RC2-R3M3: approved, but the owner stopped the follow-up.
    Stopped,
    /// RC2-R3M13 (RC2-R3 review P2 on 74e18e33): approved; the follow-up
    /// answered, but none of its own steps showed a change made (a reply
    /// such as "I couldn't edit the file" is words, not a change).
    Answered,
}

impl PlanOutcome {
    /// The token kept with the council.
    pub fn as_str(self) -> &'static str {
        match self {
            PlanOutcome::Approved => "approved",
            PlanOutcome::Declined => "declined",
            PlanOutcome::Started => "approved_started",
            PlanOutcome::Answered => "approved_answered",
            PlanOutcome::Failed => "approved_failed",
            PlanOutcome::Stopped => "approved_stopped",
        }
    }

    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "approved" => Some(PlanOutcome::Approved),
            "declined" => Some(PlanOutcome::Declined),
            "approved_started" => Some(PlanOutcome::Started),
            "approved_answered" => Some(PlanOutcome::Answered),
            "approved_failed" => Some(PlanOutcome::Failed),
            "approved_stopped" => Some(PlanOutcome::Stopped),
            _ => None,
        }
    }
}

/// One transcript line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// The owner's message.
    Owner(String),
    /// A member's answer. `own` = this prompt's member wrote it.
    Answer {
        name: String,
        own: bool,
        text: String,
        stopped: bool,
    },
    /// A member that did not answer that round.
    NoAnswer { name: String },
    /// RC2-R3: what the owner did with the plan in the answer above it.
    Plan {
        name: String,
        outcome: PlanOutcome,
        /// RC2-R3M8: the plan in short, once its answer above was left out
        /// to fit (so the line still says which change it was).
        gist: Option<String>,
    },
}

/// Everything one member is sent for one round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatPrompt {
    pub member: String,
    /// The brief (goes in the request's system slot).
    pub brief: String,
    /// The council so far: `(round, line)`, oldest first.
    pub transcript: Vec<(u32, Line)>,
    /// The round's ask (the owner's new message, or the respond / sum-up ask).
    pub ask: String,
    /// Older answers were left out to fit the member's window.
    pub older_left_out: bool,
}

/// A planned round: one prompt per member it goes to, and the members left
/// out of it with why (one plain line each).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoundPlan {
    pub seats: Vec<SeatPrompt>,
    pub left_out: Vec<String>,
}

/// The note added to a transcript whose older answers were cut.
pub const OLDER_LEFT_OUT: &str = "(Older answers from the council were left out to fit.)";

/// The respond-round ask (Council plan §4).
pub fn respond_ask(owner: &str, others: &[&str]) -> String {
    format!(
        "Here are the other members' answers to {owner}'s last message ({}). Start with one \
         line: Same answer, Changed my answer, or Changed partly. Then say where you agree or \
         disagree and why. Keep it short.",
        join_names(others)
    )
}

/// The sum-up ask (Council plan §4).
///
/// RC2-R3 (usability RC2 round 3, Finding 8: the summary talked about
/// "Owner" and advised "proceed with the one-step action" the owner had
/// just declined): it speaks to the owner as "you", and a plan he declined
/// or approved in this council (a line in the transcript) is taken as he
/// decided it.
pub fn sum_up_ask(owner: &str) -> String {
    format!(
        "Write one combined answer for {owner}: where the council agrees, where it splits and \
         why, and what you would do. Speak to {owner} as \"you\" and never use his name. Where \
         he declined a member's plan above, nothing was changed because he chose not to: do not \
         recommend doing it anyway. Where he approved one, the line under it says whether it \
         was carried out: do not say it was done unless that line says so."
    )
}

fn join_names(names: &[&str]) -> String {
    match names.len() {
        0 => String::new(),
        1 => names[0].to_string(),
        n => format!("{} and {}", names[..n - 1].join(", "), names[n - 1]),
    }
}

impl SeatPrompt {
    /// The user message: transcript, then the ask.
    pub fn user_text(&self, owner: &str) -> String {
        let mut out = String::new();
        if !self.transcript.is_empty() {
            out.push_str("The council so far:\n\n");
            if self.older_left_out {
                out.push_str(OLDER_LEFT_OUT);
                out.push_str("\n\n");
            }
            for (_, line) in &self.transcript {
                match line {
                    Line::Owner(t) => out.push_str(&format!("{owner}: {t}\n\n")),
                    Line::Answer {
                        name,
                        own,
                        text,
                        stopped,
                    } => {
                        let who = if *own {
                            format!("You ({name})")
                        } else {
                            name.clone()
                        };
                        let mark = if *stopped { " (stopped early)" } else { "" };
                        out.push_str(&format!("{who}{mark}: {}\n\n", text.trim()));
                    }
                    Line::NoAnswer { name } => {
                        out.push_str(&format!("{name} didn't answer.\n\n"));
                    }
                    Line::Plan {
                        name,
                        outcome,
                        gist,
                    } => {
                        let said = match outcome {
                            PlanOutcome::Approved => format!(
                                "{owner} approved {name}\u{2019}s plan, and it was carried out."
                            ),
                            PlanOutcome::Declined => format!(
                                "{owner} declined {name}\u{2019}s plan: nothing was changed."
                            ),
                            PlanOutcome::Answered => format!(
                                "{owner} approved {name}\u{2019}s plan and it answered, but \
                                 whether the change was made is not confirmed."
                            ),
                            PlanOutcome::Started => format!(
                                "{owner} approved {name}\u{2019}s plan, but it did not finish: \
                                 it may not have been carried out."
                            ),
                            PlanOutcome::Failed => format!(
                                "{owner} approved {name}\u{2019}s plan, but carrying it out \
                                 failed: it was not done, or only partly."
                            ),
                            PlanOutcome::Stopped => format!(
                                "{owner} approved {name}\u{2019}s plan, but he stopped it before \
                                 it finished: it was not done, or only partly."
                            ),
                        };
                        // RC2-R3M8: which change it was, when its answer was
                        // left out to fit.
                        let what = gist
                            .as_ref()
                            .map(|g| format!(" The plan was: \u{201c}{g}\u{201d}"))
                            .unwrap_or_default();
                        out.push_str(&format!("({said}{what})\n\n"));
                    }
                }
            }
            out.push_str("---\n\n");
        }
        out.push_str(&self.ask);
        out
    }

    /// A rough size in tokens (four characters each).
    pub fn approx_tokens(&self, owner: &str) -> u32 {
        let chars = self.brief.chars().count() + self.user_text(owner).chars().count();
        (chars / 4) as u32 + 1
    }

    /// A short fingerprint of exactly what was sent (FNV-1a, hex), kept with
    /// the turn so Details can prove the rebuilt prompt is the one sent.
    pub fn fingerprint(&self, owner: &str) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.brief.bytes().chain(self.user_text(owner).bytes()) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{h:016x}")
    }
}

/// Shrink `p` to fit `window_tokens`: drop member answers from the oldest
/// rounds first; the owner's messages and the ask always stay (if they alone
/// are over, the prompt goes as it is — the member says so itself). RC2-R3:
/// what the owner did with a plan stays too.
pub fn trim_to_budget(p: SeatPrompt, window_tokens: u32, owner: &str) -> SeatPrompt {
    trim_while(p, |p| p.approx_tokens(owner) > window_tokens)
}

/// RC2-R53 (usability RC2 round 4, Major 1): shrink `p` the same way until
/// its user message ([`SeatPrompt::user_text`]) has at most `max_chars`
/// characters — the room a member on Awili's tool engine has once its
/// brief, tools, answer and step results are counted (the brief goes in the
/// system text, counted there).
pub fn trim_to_chars(p: SeatPrompt, max_chars: usize, owner: &str) -> SeatPrompt {
    trim_while(p, |p| p.user_text(owner).chars().count() > max_chars)
}

/// Drop member answers, oldest first, while `over(p)` holds (owner messages,
/// plan outcomes and the ask always stay).
fn trim_while(mut p: SeatPrompt, over: impl Fn(&SeatPrompt) -> bool) -> SeatPrompt {
    while over(&p) {
        let Some(pos) = p
            .transcript
            .iter()
            .position(|(_, l)| !matches!(l, Line::Owner(_) | Line::Plan { .. }))
        else {
            break;
        };
        let (_, gone) = p.transcript.remove(pos);
        // RC2-R3M8 (RC2-R34 review P2): a plan's answer left out keeps the
        // plan in short on its outcome line, so that line still says which
        // change was declined or approved.
        if let (Line::Answer { text, .. }, Some((_, Line::Plan { gist, .. }))) =
            (&gone, p.transcript.get_mut(pos))
        {
            if gist.is_none() {
                *gist = Some(plan_gist(text));
            }
        }
        p.older_left_out = true;
    }
    p
}

/// RC2-R3M8: the longest a plan in short runs, in characters.
pub const PLAN_GIST_CHARS: usize = 160;

/// RC2-R3M8: a plan in short: its words on one line, at most
/// [`PLAN_GIST_CHARS`] characters ("\u{2026}" when cut). RC2-R3M14 (RC2-R3
/// review P3 on 74e18e33: an introduction before the steps filled it): its
/// numbered steps ("1. …", "2) …"), when it has any; its first words only
/// when it has none.
fn plan_gist(text: &str) -> String {
    let steps: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| is_numbered_step(l))
        .collect();
    let source = if steps.is_empty() {
        text.to_string()
    } else {
        steps.join("\n")
    };
    let flat = source.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= PLAN_GIST_CHARS {
        return flat;
    }
    let cut: String = flat.chars().take(PLAN_GIST_CHARS - 1).collect();
    format!("{}\u{2026}", cut.trim_end())
}

/// RC2-R3M14: whether `line` is a numbered step: digits, then "." or ")",
/// then a space and words.
fn is_numbered_step(line: &str) -> bool {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    let rest = &line[digits..];
    digits > 0 && (rest.starts_with(". ") || rest.starts_with(") ")) && !rest[2..].trim().is_empty()
}

/// The transcript of `c` as `member` sees it (every finished turn).
fn transcript_for(
    c: &Council,
    member: &str,
    outcomes: &[(String, PlanOutcome)],
) -> Vec<(u32, Line)> {
    let mut out = Vec::new();
    for r in &c.rounds {
        if let Some(t) = &r.owner_text {
            out.push((r.number, Line::Owner(t.clone())));
        }
        for t in &r.turns {
            let line = match &t.status {
                TurnStatus::Running => continue,
                TurnStatus::Answered => Line::Answer {
                    name: t.label.clone(),
                    own: t.member == member,
                    text: t.text.clone(),
                    stopped: false,
                },
                TurnStatus::Stopped if !t.text.trim().is_empty() => Line::Answer {
                    name: t.label.clone(),
                    own: t.member == member,
                    text: t.text.clone(),
                    stopped: true,
                },
                // RC2-R3M3: a plan the owner approved whose follow-up failed
                // is still that member's answer, with the line saying it
                // failed under it.
                TurnStatus::Failed { .. }
                    if !t.text.trim().is_empty()
                        && outcomes
                            .iter()
                            .any(|(id, o)| *id == t.id && *o == PlanOutcome::Failed) =>
                {
                    Line::Answer {
                        name: t.label.clone(),
                        own: t.member == member,
                        text: t.text.clone(),
                        stopped: false,
                    }
                }
                TurnStatus::Stopped | TurnStatus::Failed { .. } => Line::NoAnswer {
                    name: t.label.clone(),
                },
            };
            let plan = matches!(line, Line::Answer { .. })
                .then(|| outcomes.iter().find(|(id, _)| *id == t.id))
                .flatten()
                .map(|(_, outcome)| Line::Plan {
                    name: t.label.clone(),
                    outcome: *outcome,
                    gist: None,
                });
            out.push((r.number, line));
            if let Some(plan) = plan {
                out.push((r.number, plan));
            }
        }
    }
    out
}

fn brief_for(owner: &str, me: &Member, all: &[&Member]) -> String {
    let others: Vec<&str> = all
        .iter()
        .filter(|m| m.id != me.id)
        .map(|m| m.label.as_str())
        .collect();
    let with = if others.is_empty() {
        String::new()
    } else {
        format!(": you, {}", join_names(&others))
    };
    format!(
        "You are {name}, one of {n} models on {owner}'s council{with}. {owner} sees all answers \
         side by side. Answer {owner} directly and in plain words. Earlier answers from the \
         others are shown with their names; you may agree or disagree with them by name.",
        name = me.label,
        n = all.len(),
    )
}

/// Builds one member's ask.
type AskFor<'a> = Box<dyn Fn(&Member) -> String + 'a>;

/// Who the round goes to and what each is sent. `owner_text` is the new
/// message for an ask round (ignored otherwise). The round is not added to
/// `c`; the caller opens it with the returned seats.
pub fn plan_round(
    c: &Council,
    kind: RoundKind,
    owner_text: Option<&str>,
    opts: &PlanOptions,
) -> RoundPlan {
    let owner = opts.owner.as_str();
    let active = c.active_members();
    let mut plan = RoundPlan::default();
    let (targets, ask_for): (Vec<&Member>, AskFor) = match kind {
        RoundKind::Ask => {
            let text = owner_text.unwrap_or("").to_string();
            let targets: Vec<&Member> = if opts.to.is_empty() {
                active.clone()
            } else {
                active
                    .iter()
                    .copied()
                    .filter(|m| opts.to.contains(&m.id))
                    .collect()
            };
            (targets, Box::new(move |_| text.clone()))
        }
        RoundKind::Respond => {
            let last = c
                .rounds
                .iter()
                .rev()
                .find(|r| r.kind != RoundKind::SumUp && !r.is_running());
            let answered = |m: &Member| {
                last.and_then(|r| r.turn(&m.id)).is_some_and(|t| {
                    t.status == TurnStatus::Answered
                        || (t.status == TurnStatus::Stopped && !t.text.trim().is_empty())
                })
            };
            let targets: Vec<&Member> = active.iter().copied().filter(|m| answered(m)).collect();
            if let Some(r) = last {
                for m in active.iter().filter(|m| !answered(m)) {
                    plan.left_out.push(format!(
                        "{} was left out of this round because it didn\u{2019}t answer round {}.",
                        m.label, r.number
                    ));
                }
            }
            if targets.len() < 2 {
                plan.left_out.push(
                    "Fewer than two members answered, so there is nothing to answer yet.".into(),
                );
                return plan;
            }
            let names: Vec<(String, String)> = targets
                .iter()
                .map(|m| (m.id.clone(), m.label.clone()))
                .collect();
            let owner_s = owner.to_string();
            (
                targets,
                Box::new(move |me: &Member| {
                    let others: Vec<&str> = names
                        .iter()
                        .filter(|(id, _)| *id != me.id)
                        .map(|(_, l)| l.as_str())
                        .collect();
                    respond_ask(&owner_s, &others)
                }),
            )
        }
        RoundKind::SumUp => {
            let targets: Vec<&Member> = active
                .iter()
                .copied()
                .filter(|m| opts.summarizer.as_deref() == Some(m.id.as_str()))
                .collect();
            let ask = sum_up_ask(owner);
            (targets, Box::new(move |_| ask.clone()))
        }
    };
    for m in &targets {
        plan.seats.push(SeatPrompt {
            member: m.id.clone(),
            brief: brief_for(owner, m, &active),
            transcript: transcript_for(c, &m.id, &opts.plan_outcomes),
            ask: ask_for(m),
            older_left_out: false,
        });
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Round, SeatTurn};

    /// Claude, Codex, Gemma, DeepSeek; round 1 asked and settled as given.
    fn council_after_round_one(outcomes: &[(&str, TurnStatus, &str)]) -> Council {
        let mut c = Council::default();
        for (route, label) in [
            ("agent:claude:sonnet", "Claude Sonnet"),
            ("agent:codex", "Codex"),
            ("in-app", "Gemma 4 26B"),
            ("agent:dsh:deepseek-flash", "DeepSeek Flash"),
        ] {
            c.add_member(route, label);
        }
        let mut r = Round::new(1, RoundKind::Ask);
        r.owner_text = Some("Gemma or Qwen on the Ultra?".into());
        for m in c.active_members() {
            r.turns
                .push(SeatTurn::running(&format!("t-{}", m.id), m, 0));
        }
        c.rounds.push(r);
        for (m, st, text) in outcomes {
            c.settle(1, m, st.clone(), text);
        }
        c
    }

    fn all_answered() -> Council {
        council_after_round_one(&[
            ("m1", TurnStatus::Answered, "Keep Gemma for now."),
            ("m2", TurnStatus::Answered, "Move coding to Qwen."),
            ("m3", TurnStatus::Answered, "Keep Gemma; run a trial."),
            ("m4", TurnStatus::Answered, "Count failed tool calls."),
        ])
    }

    fn timeout() -> TurnStatus {
        TurnStatus::Failed {
            plain: "Its service stopped responding after 30 seconds.".into(),
            detail: "read timeout".into(),
            needs_owner: false,
        }
    }

    /// RC2-CNCL4: one message goes to every member with the same council so
    /// far — every owner message and every answer, by name — and the same
    /// ask; only "You (Name)" marks a member's own answer.
    #[test]
    fn ask_round_sends_everyone_the_same_history() {
        let c = all_answered();
        let plan = plan_round(
            &c,
            RoundKind::Ask,
            Some("And for Code?"),
            &PlanOptions::new("Owner"),
        );
        assert_eq!(plan.seats.len(), 4);
        assert!(plan.left_out.is_empty());
        let texts: Vec<String> = plan.seats.iter().map(|s| s.user_text("Owner")).collect();
        for (s, t) in plan.seats.iter().zip(&texts) {
            assert_eq!(s.ask, "And for Code?");
            assert!(t.ends_with("And for Code?"), "{t}");
            assert!(t.contains("Owner: Gemma or Qwen on the Ultra?"), "{t}");
            for answer in [
                "Keep Gemma for now.",
                "Move coding to Qwen.",
                "Keep Gemma; run a trial.",
                "Count failed tool calls.",
            ] {
                assert!(t.contains(answer), "{t}");
            }
            assert!(s.brief.contains("one of 4 models on Owner's council"));
        }
        assert!(texts[1].contains("You (Codex): Move coding to Qwen."));
        assert!(texts[1].contains("Claude Sonnet: Keep Gemma for now."));
        assert!(texts[0].contains("You (Claude Sonnet): Keep Gemma for now."));
        // Strip the own-answer marks: the histories are then identical.
        let norm = |t: &str| {
            t.replace("You (Claude Sonnet)", "Claude Sonnet")
                .replace("You (Codex)", "Codex")
                .replace("You (Gemma 4 26B)", "Gemma 4 26B")
                .replace("You (DeepSeek Flash)", "DeepSeek Flash")
        };
        assert!(texts.iter().all(|t| norm(t) == norm(&texts[0])));
    }

    /// RC2-CNCL4: in "answer each other" each member reads the others'
    /// answers under their names, is asked to open with its stance, and is
    /// told who the others are.
    #[test]
    fn respond_round_shows_each_member_the_others_by_name() {
        let c = all_answered();
        let plan = plan_round(&c, RoundKind::Respond, None, &PlanOptions::new("Owner"));
        assert_eq!(plan.seats.len(), 4);
        let codex = plan.seats.iter().find(|s| s.member == "m2").unwrap();
        let t = codex.user_text("Owner");
        assert!(t.contains("Claude Sonnet: Keep Gemma for now."), "{t}");
        assert!(t.contains("Gemma 4 26B: Keep Gemma; run a trial."), "{t}");
        assert!(t.contains("You (Codex): Move coding to Qwen."), "{t}");
        assert!(codex
            .ask
            .contains("Claude Sonnet, Gemma 4 26B and DeepSeek Flash"));
        assert!(!codex.ask.contains("Codex"));
        assert!(codex
            .ask
            .contains("Same answer, Changed my answer, or Changed partly"));
    }

    /// RC2-CNCL4: a member that didn't answer is left out of the next
    /// "answer each other" round with one line saying why, and the others
    /// see one line that it didn't answer.
    #[test]
    fn failed_member_left_out_with_note() {
        let c = council_after_round_one(&[
            ("m1", TurnStatus::Answered, "Keep Gemma for now."),
            ("m2", TurnStatus::Answered, "Move coding to Qwen."),
            ("m3", TurnStatus::Answered, "Keep Gemma; run a trial."),
            ("m4", timeout(), ""),
        ]);
        let plan = plan_round(&c, RoundKind::Respond, None, &PlanOptions::new("Owner"));
        assert_eq!(
            plan.seats
                .iter()
                .map(|s| s.member.as_str())
                .collect::<Vec<_>>(),
            ["m1", "m2", "m3"]
        );
        assert_eq!(
            plan.left_out,
            ["DeepSeek Flash was left out of this round because it didn\u{2019}t answer round 1."]
        );
        let t = plan.seats[0].user_text("Owner");
        assert!(t.contains("DeepSeek Flash didn't answer."), "{t}");
        assert!(!plan.seats[0].ask.contains("DeepSeek"));
        // With fewer than two answers there is nothing to answer.
        let lone = council_after_round_one(&[
            ("m1", TurnStatus::Answered, "x"),
            ("m2", timeout(), ""),
            ("m3", timeout(), ""),
            ("m4", timeout(), ""),
        ]);
        let plan = plan_round(&lone, RoundKind::Respond, None, &PlanOptions::new("Owner"));
        assert!(plan.seats.is_empty());
        assert_eq!(plan.left_out.len(), 4);
    }

    /// RC2-CNCL4: over the window, the oldest answers go first and every
    /// owner message stays; the prompt says older answers were left out.
    #[test]
    fn budget_keeps_every_owner_message() {
        let mut c = all_answered();
        let long = "word ".repeat(400);
        for n in 2..=4 {
            let mut r = Round::new(n, RoundKind::Ask);
            r.owner_text = Some(format!("question {n}"));
            for m in c.active_members() {
                r.turns
                    .push(SeatTurn::running(&format!("t{n}-{}", m.id), m, 0));
            }
            c.rounds.push(r);
            for m in ["m1", "m2", "m3", "m4"] {
                c.settle(
                    n,
                    m,
                    TurnStatus::Answered,
                    &format!("{m} round {n}: {long}"),
                );
            }
        }
        let plan = plan_round(
            &c,
            RoundKind::Ask,
            Some("last one"),
            &PlanOptions::new("Owner"),
        );
        let full = plan.seats[0].clone();
        let before = full.approx_tokens("Owner");
        let trimmed = trim_to_budget(full, before / 3, "Owner");
        assert!(trimmed.older_left_out);
        assert!(trimmed.approx_tokens("Owner") <= before / 3);
        let t = trimmed.user_text("Owner");
        for q in [
            "Gemma or Qwen on the Ultra?",
            "question 2",
            "question 3",
            "question 4",
        ] {
            assert!(t.contains(&format!("Owner: {q}")), "{q} kept");
        }
        assert!(t.contains(OLDER_LEFT_OUT));
        assert!(
            !t.contains("Keep Gemma for now."),
            "oldest answers go first"
        );
        assert!(t.contains("m4 round 4"), "newest answers stay longest");
        assert!(t.ends_with("last one"));
        // A tiny window still keeps every owner message.
        let tiny = trim_to_budget(plan.seats[0].clone(), 1, "Owner");
        assert_eq!(
            tiny.transcript
                .iter()
                .filter(|(_, l)| matches!(l, Line::Owner(_)))
                .count(),
            4
        );
        assert!(tiny
            .transcript
            .iter()
            .all(|(_, l)| matches!(l, Line::Owner(_))));
    }

    /// RC2-CNCL4: an `@Name` message goes to that member only (the others
    /// see it in the next round's history), and sum-up goes to the one asked.
    #[test]
    fn mention_round_sends_to_one() {
        let c = all_answered();
        let mut opts = PlanOptions::new("Owner");
        opts.to = vec!["m2".into()];
        let plan = plan_round(&c, RoundKind::Ask, Some("@Codex which tasks first?"), &opts);
        assert_eq!(plan.seats.len(), 1);
        assert_eq!(plan.seats[0].member, "m2");
        let mut opts = PlanOptions::new("Owner");
        opts.summarizer = Some("m1".into());
        let plan = plan_round(&c, RoundKind::SumUp, None, &opts);
        assert_eq!(plan.seats.len(), 1);
        assert_eq!(plan.seats[0].member, "m1");
        assert!(plan.seats[0]
            .ask
            .starts_with("Write one combined answer for Owner"));
        // The fingerprint follows exactly what is sent.
        let a = plan.seats[0].fingerprint("Owner");
        let mut changed = plan.seats[0].clone();
        changed.ask.push('!');
        assert_ne!(a, changed.fingerprint("Owner"));
        assert_eq!(a.len(), 16);
    }

    /// RC2-R3 (usability RC2 round 3, Finding 8: Claude's summary spoke of
    /// "Owner" and advised "proceed with the one-step action" the owner had
    /// just declined): the sum-up ask speaks to the owner as "you", and the
    /// transcript every later round sees says, under a member's answer, that
    /// its plan was declined (nothing changed) or approved (carried out).
    /// Trimming to a window keeps those lines.
    #[test]
    fn rc2r3_a_sum_up_knows_each_plan_outcome_and_says_you() {
        let c = all_answered();
        let mut opts = PlanOptions::new("Owner");
        opts.summarizer = Some("m1".into());
        opts.plan_outcomes = vec![
            ("t-m1".into(), PlanOutcome::Declined),
            ("t-m2".into(), PlanOutcome::Approved),
        ];
        let plan = plan_round(&c, RoundKind::SumUp, None, &opts);
        assert_eq!(plan.seats.len(), 1);
        let seat = &plan.seats[0];
        let text = seat.user_text("Owner");
        assert!(
            text.contains(
                "You (Claude Sonnet): Keep Gemma for now.\n\n(Owner declined Claude Sonnet\u{2019}s \
                 plan: nothing was changed.)"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "Codex: Move coding to Qwen.\n\n(Owner approved Codex\u{2019}s plan, and it was \
                 carried out.)"
            ),
            "{text}"
        );
        assert!(
            !text.contains("Gemma 4 26B\u{2019}s plan"),
            "no outcome, no line: {text}"
        );
        for words in [
            "Speak to Owner as \"you\" and never use his name",
            "do not recommend doing it anyway",
        ] {
            assert!(seat.ask.contains(words), "{words:?} missing: {}", seat.ask);
        }
        // A tight window drops answers first; the outcome lines stay.
        let tight = trim_to_budget(seat.clone(), seat.approx_tokens("Owner") - 10, "Owner");
        assert!(tight.older_left_out);
        assert!(tight.transcript.iter().any(|(_, l)| matches!(
            l,
            Line::Plan {
                outcome: PlanOutcome::Declined,
                ..
            }
        )));
        assert_eq!(
            PlanOutcome::from_token("declined"),
            Some(PlanOutcome::Declined)
        );
        assert_eq!(
            PlanOutcome::from_token(PlanOutcome::Approved.as_str()),
            Some(PlanOutcome::Approved)
        );
        assert_eq!(PlanOutcome::from_token("x"), None);
    }

    /// RC2-R3M3 (RC2-R3 review P2 on 82deea7a): approving a plan is not
    /// carrying it out. Under each approved plan the later rounds are told how
    /// its follow-up went: carried out only when it answered without failing;
    /// failed, stopped, or not seen to finish otherwise \u{2014} a failed one keeps
    /// its plan in the transcript, with the line under it. The sum-up is told
    /// not to say a plan was done unless that line says so.
    #[test]
    fn rc2r3m3_an_approved_plan_says_how_carrying_it_out_went() {
        let c = council_after_round_one(&[
            ("m1", timeout(), "1. Write notes.txt."),
            ("m2", TurnStatus::Stopped, "1. Edit a.md."),
            ("m3", TurnStatus::Answered, "1. Add b.md."),
            ("m4", TurnStatus::Answered, "1. Count calls. Done."),
        ]);
        let label = |m: &str| c.round(1).unwrap().turn(m).unwrap().label.clone();
        let mut opts = PlanOptions::new("Owner");
        opts.summarizer = Some("m4".into());
        opts.plan_outcomes = vec![
            ("t-m1".into(), PlanOutcome::Failed),
            ("t-m2".into(), PlanOutcome::Stopped),
            ("t-m3".into(), PlanOutcome::Started),
            ("t-m4".into(), PlanOutcome::Approved),
        ];
        let plan = plan_round(&c, RoundKind::SumUp, None, &opts);
        let seat = &plan.seats[0];
        let text = seat.user_text("Owner");
        for want in [
            format!(
                "{}: 1. Write notes.txt.\n\n(Owner approved {}\u{2019}s plan, but carrying it \
                 out failed: it was not done, or only partly.)",
                label("m1"),
                label("m1")
            ),
            format!(
                "(Owner approved {}\u{2019}s plan, but he stopped it before it finished: it \
                 was not done, or only partly.)",
                label("m2")
            ),
            format!(
                "(Owner approved {}\u{2019}s plan, but it did not finish: it may not have \
                 been carried out.)",
                label("m3")
            ),
            format!(
                "(Owner approved {}\u{2019}s plan, and it was carried out.)",
                label("m4")
            ),
        ] {
            assert!(text.contains(&want), "{want:?} missing: {text}");
        }
        for m in ["m1", "m2", "m3"] {
            assert!(
                !text.contains(&format!(
                    "{}\u{2019}s plan, and it was carried out",
                    label(m)
                )),
                "{m}: {text}"
            );
        }
        assert!(
            seat.ask
                .contains("do not say it was done unless that line says so"),
            "{}",
            seat.ask
        );
        assert!(!seat
            .ask
            .contains("Where he approved one, it was carried out"));
        for o in [
            PlanOutcome::Approved,
            PlanOutcome::Declined,
            PlanOutcome::Started,
            PlanOutcome::Failed,
            PlanOutcome::Stopped,
        ] {
            assert_eq!(PlanOutcome::from_token(o.as_str()), Some(o));
        }
    }

    /// RC2-R3M8 (RC2-R34 review P2): trimming to a window can leave out the
    /// answer that held a plan; its outcome line then says the plan in
    /// short, so "declined Claude Sonnet's plan" still says which change.
    #[test]
    fn rc2r3m8_a_trimmed_plan_keeps_its_gist() {
        let long_plan = format!(
            "1. Write notes.txt with hi.\n2. {}",
            "Then check it and tidy the folder. ".repeat(12)
        );
        let c = council_after_round_one(&[
            ("m1", TurnStatus::Answered, long_plan.as_str()),
            ("m2", TurnStatus::Answered, "Move coding to Qwen."),
        ]);
        let mut opts = PlanOptions::new("Owner");
        opts.summarizer = Some("m2".into());
        opts.plan_outcomes = vec![("t-m1".into(), PlanOutcome::Declined)];
        let plan = plan_round(&c, RoundKind::SumUp, None, &opts);
        let seat = plan.seats[0].clone();
        let whole = seat.user_text("Owner");
        assert!(
            !whole.contains("The plan was"),
            "not when the answer is there"
        );
        let tight = trim_to_budget(seat.clone(), seat.approx_tokens("Owner") - 10, "Owner");
        assert!(tight.older_left_out);
        let text = tight.user_text("Owner");
        assert!(!text.contains("hi.\n2. Then"), "the answer went: {text}");
        let label = c.round(1).unwrap().turn("m1").unwrap().label.clone();
        let want = format!(
            "(Owner declined {label}\u{2019}s plan: nothing was changed. The plan was: \
             \u{201c}1. Write notes.txt with hi. 2. Then check it"
        );
        assert!(text.contains(&want), "{want:?} missing: {text}");
        let gist = tight
            .transcript
            .iter()
            .find_map(|(_, l)| match l {
                Line::Plan { gist, .. } => gist.clone(),
                _ => None,
            })
            .expect("a gist");
        assert!(gist.chars().count() <= PLAN_GIST_CHARS, "{gist}");
        assert!(gist.ends_with('\u{2026}'), "{gist}");
    }

    /// RC2-R3M13: a follow-up that answered with no change step of its own
    /// is told as answered, the change not confirmed \u{2014} never carried out.
    #[test]
    fn rc2r3m13_an_answered_follow_up_is_not_told_as_done() {
        let c = all_answered();
        let label = c.round(1).unwrap().turn("m1").unwrap().label.clone();
        let mut opts = PlanOptions::new("Owner");
        opts.summarizer = Some("m2".into());
        opts.plan_outcomes = vec![("t-m1".into(), PlanOutcome::Answered)];
        let text = plan_round(&c, RoundKind::SumUp, None, &opts).seats[0].user_text("Owner");
        let want = format!(
            "(Owner approved {label}\u{2019}s plan and it answered, but whether the change \
             was made is not confirmed.)"
        );
        assert!(text.contains(&want), "{want:?} missing: {text}");
        assert!(!text.contains("carried out.)"), "{text}");
        assert_eq!(
            PlanOutcome::from_token(PlanOutcome::Answered.as_str()),
            Some(PlanOutcome::Answered)
        );
    }

    /// RC2-R3M14 (RC2-R3 review P3 on 74e18e33): a plan with a long
    /// introduction before its numbered steps keeps its steps in short, not
    /// the introduction; a plan with no numbered steps keeps its first words.
    #[test]
    fn rc2r3m14_a_trimmed_plan_keeps_its_steps_not_its_introduction() {
        let intro = "Thanks for asking. I looked through the folder and read every file \
                     there carefully, and here is what I think we should do about it. ";
        let plan = format!(
            "{}\n\n1. Write notes.txt with hi.\n2) Delete old.txt.\n\nTell me if that suits.",
            intro.repeat(2)
        );
        let c = council_after_round_one(&[
            ("m1", TurnStatus::Answered, plan.as_str()),
            ("m2", TurnStatus::Answered, "Move coding to Qwen."),
        ]);
        let mut opts = PlanOptions::new("Owner");
        opts.summarizer = Some("m2".into());
        opts.plan_outcomes = vec![("t-m1".into(), PlanOutcome::Declined)];
        let seat = plan_round(&c, RoundKind::SumUp, None, &opts).seats[0].clone();
        let tight = trim_to_budget(seat.clone(), seat.approx_tokens("Owner") - 10, "Owner");
        let text = tight.user_text("Owner");
        assert!(
            text.contains(
                "nothing was changed. The plan was: \u{201c}1. Write notes.txt with hi. 2) \
                 Delete old.txt.\u{201d})"
            ),
            "{text}"
        );
        assert!(!text.contains("Thanks for asking"), "{text}");
        assert_eq!(plan_gist("Write notes.txt now."), "Write notes.txt now.");
        assert!(!is_numbered_step("2026 was a year."));
        assert!(!is_numbered_step("1."));
        assert!(is_numbered_step("12. Add b.md"));
    }
}
