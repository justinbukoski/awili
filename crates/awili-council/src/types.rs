//! The council's state: members, rounds, and each member's turn in a round.

/// One seat on a council.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Stable per council ("m1", "m2", …).
    pub id: String,
    /// The route as persisted (`agent:claude:sonnet`, `in-app`, `provider:fleet`, …).
    pub route: String,
    /// The name shown ("Claude Sonnet").
    pub label: String,
    /// Column order.
    pub position: u32,
    /// Taken off the council; its earlier answers stay.
    pub removed: bool,
}

impl Member {
    pub fn new(id: &str, route: &str, label: &str, position: u32) -> Self {
        Self {
            id: id.to_string(),
            route: route.to_string(),
            label: label.to_string(),
            position,
            removed: false,
        }
    }
}

/// What opened a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundKind {
    /// The owner sent a message.
    Ask,
    /// "Let them answer each other": each member reads the others' answers.
    Respond,
    /// "Ask one to sum up": one member writes a combined answer.
    SumUp,
}

impl RoundKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RoundKind::Ask => "ask",
            RoundKind::Respond => "respond",
            RoundKind::SumUp => "sum_up",
        }
    }

    /// Unknown tokens read as [`RoundKind::Ask`].
    pub fn from_token(s: &str) -> Self {
        match s {
            "respond" => RoundKind::Respond,
            "sum_up" => RoundKind::SumUp,
            _ => RoundKind::Ask,
        }
    }
}

/// How one member's turn went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnStatus {
    /// Still answering.
    Running,
    Answered,
    /// The owner stopped it; its partial text is kept.
    Stopped,
    /// It did not answer. `plain` is the sentence the card shows, `detail`
    /// the raw error for Details; `needs_owner` marks the one case shown in
    /// red: he must act outside the council (sign in, add a key).
    Failed {
        plain: String,
        detail: String,
        needs_owner: bool,
    },
}

impl TurnStatus {
    /// The persisted token (`council_turns.status`).
    pub fn as_str(&self) -> &'static str {
        match self {
            TurnStatus::Running => "running",
            TurnStatus::Answered => "answered",
            TurnStatus::Stopped => "stopped",
            TurnStatus::Failed { .. } => "failed",
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, TurnStatus::Running)
    }
}

/// The line an "answer each other" reply opens with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stance {
    /// "Same answer".
    Same,
    /// "Changed my answer".
    Changed,
    /// "Changed partly".
    Partly,
}

impl Stance {
    /// The persisted token (`council_turns.stance`).
    pub fn as_str(self) -> &'static str {
        match self {
            Stance::Same => "same",
            Stance::Changed => "changed",
            Stance::Partly => "partly",
        }
    }

    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "same" => Some(Stance::Same),
            "changed" => Some(Stance::Changed),
            "partly" => Some(Stance::Partly),
            _ => None,
        }
    }

    /// The tag on the card.
    ///
    /// RC2-R613 (usability RC2 round 5, Minor 14: a card tagged "Same answer"
    /// said "I disagree with Gemma on one thing", which read as a
    /// contradiction): the tag says the member kept ITS OWN answer, not that
    /// it agrees with the others. (The members still write "Same answer",
    /// "Changed my answer" or "Changed partly"; [`parse_stance`] reads that.)
    pub fn label(self) -> &'static str {
        match self {
            Stance::Same => "Kept its answer",
            Stance::Changed => "Changed its answer",
            Stance::Partly => "Changed its answer partly",
        }
    }
}

/// One member's turn in one round. Its name and route are frozen at send
/// time, so a later rename or removal never rewrites who said what.
#[derive(Clone, Debug, PartialEq)]
pub struct SeatTurn {
    /// The `council_turns` row id.
    pub id: String,
    pub member: String,
    pub label: String,
    pub route: String,
    /// The answer as written (stance line included; see [`stance_and_body`]).
    pub text: String,
    pub status: TurnStatus,
    pub stance: Option<Stance>,
    /// Words in / out (tokens), when the model reported them.
    pub usage: Option<(u64, u64)>,
    pub first_token_ms: Option<u64>,
    pub elapsed_ms: Option<u64>,
    /// `None` = included in a subscription, or free.
    pub cost_usd: Option<f64>,
    /// Unix seconds the turn started.
    pub started_ts: i64,
}

impl SeatTurn {
    /// A running turn for `m`.
    pub fn running(id: &str, m: &Member, started_ts: i64) -> Self {
        Self {
            id: id.to_string(),
            member: m.id.clone(),
            label: m.label.clone(),
            route: m.route.clone(),
            text: String::new(),
            status: TurnStatus::Running,
            stance: None,
            usage: None,
            first_token_ms: None,
            elapsed_ms: None,
            cost_usd: None,
            started_ts,
        }
    }

    /// The text without its stance line (what the card body shows).
    pub fn body(&self) -> &str {
        if self.stance.is_some() {
            stance_and_body(&self.text).1
        } else {
            &self.text
        }
    }
}

/// One round: the owner's message (or the button that opened it) and every
/// member's turn.
#[derive(Clone, Debug, PartialEq)]
pub struct Round {
    /// 1-based.
    pub number: u32,
    pub kind: RoundKind,
    /// The owner's message (`None` when a button opened the round).
    pub owner_text: Option<String>,
    /// That message's `ts` in the conversation.
    pub owner_ts: Option<i64>,
    pub turns: Vec<SeatTurn>,
    /// Members left out of this round, by name, with why (one line each).
    pub left_out: Vec<String>,
}

impl Round {
    pub fn new(number: u32, kind: RoundKind) -> Self {
        Self {
            number,
            kind,
            owner_text: None,
            owner_ts: None,
            turns: Vec::new(),
            left_out: Vec::new(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.turns.iter().any(|t| t.status.is_running())
    }

    pub fn turn(&self, member: &str) -> Option<&SeatTurn> {
        self.turns.iter().find(|t| t.member == member)
    }

    pub fn turn_mut(&mut self, member: &str) -> Option<&mut SeatTurn> {
        self.turns.iter_mut().find(|t| t.member == member)
    }
}

/// A whole council: who sits on it and every round so far.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Council {
    pub members: Vec<Member>,
    pub rounds: Vec<Round>,
}

impl Council {
    /// The members still on the council, in column order.
    pub fn active_members(&self) -> Vec<&Member> {
        let mut v: Vec<&Member> = self.members.iter().filter(|m| !m.removed).collect();
        v.sort_by_key(|m| m.position);
        v
    }

    pub fn member(&self, id: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.id == id)
    }

    /// Seat a new member at the end; returns its id ("m1", "m2", …, never
    /// reused within a council).
    pub fn add_member(&mut self, route: &str, label: &str) -> String {
        let n = self
            .members
            .iter()
            .filter_map(|m| m.id.strip_prefix('m')?.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let position = self
            .members
            .iter()
            .map(|m| m.position + 1)
            .max()
            .unwrap_or(0);
        let id = format!("m{n}");
        self.members.push(Member::new(&id, route, label, position));
        id
    }

    /// Take a member off the council (its answers stay).
    pub fn remove_member(&mut self, id: &str) -> bool {
        match self.members.iter_mut().find(|m| m.id == id && !m.removed) {
            Some(m) => {
                m.removed = true;
                true
            }
            None => false,
        }
    }

    /// The number the next round gets.
    pub fn next_round_number(&self) -> u32 {
        self.rounds.last().map(|r| r.number + 1).unwrap_or(1)
    }

    pub fn round(&self, number: u32) -> Option<&Round> {
        self.rounds.iter().find(|r| r.number == number)
    }

    pub fn round_mut(&mut self, number: u32) -> Option<&mut Round> {
        self.rounds.iter_mut().find(|r| r.number == number)
    }

    /// Whether any member is answering right now.
    pub fn is_running(&self) -> bool {
        self.rounds.iter().any(Round::is_running)
    }

    /// Settle one member's turn: its final text and status. In an "answer
    /// each other" round the reply's opening line becomes its stance tag; a
    /// reply that ignores the format is kept whole, just untagged.
    pub fn settle(&mut self, round: u32, member: &str, status: TurnStatus, text: &str) -> bool {
        let Some(r) = self.round_mut(round) else {
            return false;
        };
        let kind = r.kind;
        let Some(t) = r.turn_mut(member) else {
            return false;
        };
        t.text = text.to_string();
        t.stance = if kind == RoundKind::Respond && !matches!(status, TurnStatus::Failed { .. }) {
            parse_stance(text)
        } else {
            None
        };
        t.status = status;
        true
    }
}

/// The stance an "answer each other" reply opens with, read from its first
/// non-blank line only ("Same answer", "Changed my answer", "Changed partly",
/// with or without bold, quotes or a trailing colon or full stop). Anything
/// else — including the words appearing later in the reply — is `None`.
pub fn parse_stance(reply: &str) -> Option<Stance> {
    stance_and_body(reply).0
}

/// The stance (if the first line is one) and the rest of the reply.
pub fn stance_and_body(reply: &str) -> (Option<Stance>, &str) {
    let trimmed = reply.trim_start();
    let (first, rest) = match trimmed.find('\n') {
        Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
        None => (trimmed, ""),
    };
    match line_stance(first) {
        Some(s) => (Some(s), rest.trim_start()),
        // RC2-R510 (usability RC2 round 4, Minor: Gemma's "Same answer. I
        // agree with Codex's recommendation …" got no tag): the stance as
        // the first sentence of a longer first line; the rest of the line
        // is the answer.
        None => {
            let cut = [". ", ": ", ".** ", ":** ", " \u{2014} ", " - ", "! "]
                .iter()
                .filter_map(|sep| first.find(sep).map(|i| (i, i + sep.len())))
                .min();
            match cut.and_then(|(end, next)| line_stance(&first[..end]).map(|s| (s, next))) {
                Some((s, next)) => (Some(s), trimmed[next..].trim_start()),
                None => (None, reply),
            }
        }
    }
}

/// The stance a whole line says, if it is one.
fn line_stance(first: &str) -> Option<Stance> {
    let norm: String = first
        .chars()
        .filter(|c| !matches!(c, '*' | '_' | '"' | '\u{201C}' | '\u{201D}' | '#' | '`'))
        .collect::<String>()
        .trim()
        .trim_end_matches(['.', ':', '!', ';', ','])
        .trim()
        .to_lowercase();
    let norm = norm
        .trim_start_matches("stance")
        .trim_start_matches([':', ' ']);
    if norm == "same answer" || norm == "same" {
        Some(Stance::Same)
    } else if norm == "changed partly" || norm == "changed in part" || norm == "partly changed" {
        Some(Stance::Partly)
    } else if norm == "changed my answer" || norm == "changed" {
        Some(Stance::Changed)
    } else {
        None
    }
}

/// The one line on a round's header: who is still answering, who answered,
/// who changed their answer and who didn't answer — names where it matters.
///
/// - while running: "3 answering · 1 done";
/// - an ask or sum-up round: "All 4 answered", or "3 answered · DeepSeek
///   Flash didn't";
/// - an "answer each other" round: "2 kept their answer · Codex changed its
///   answer in part".
pub fn round_summary(r: &Round) -> String {
    let running = r.turns.iter().filter(|t| t.status.is_running()).count();
    if running > 0 {
        return format!(
            "{running} answering \u{00B7} {} done",
            r.turns.len() - running
        );
    }
    let mut parts: Vec<String> = Vec::new();
    let failed: Vec<&SeatTurn> = r
        .turns
        .iter()
        .filter(|t| matches!(t.status, TurnStatus::Failed { .. }))
        .collect();
    let stopped: Vec<&SeatTurn> = r
        .turns
        .iter()
        .filter(|t| t.status == TurnStatus::Stopped)
        .collect();
    let answered: Vec<&SeatTurn> = r
        .turns
        .iter()
        .filter(|t| t.status == TurnStatus::Answered)
        .collect();
    if r.kind == RoundKind::Respond {
        let same: Vec<&&SeatTurn> = answered
            .iter()
            .filter(|t| t.stance == Some(Stance::Same))
            .collect();
        match same.len() {
            0 => {}
            1 => parts.push(format!("{} kept its answer", same[0].label)),
            n => parts.push(format!("{n} kept their answer")),
        }
        for t in &answered {
            match t.stance {
                Some(Stance::Changed) => parts.push(format!("{} changed its answer", t.label)),
                Some(Stance::Partly) => {
                    parts.push(format!("{} changed its answer in part", t.label))
                }
                Some(Stance::Same) => {}
                None => parts.push(format!("{} replied", t.label)),
            }
        }
        for t in &failed {
            parts.push(format!("{} didn\u{2019}t answer", t.label));
        }
    } else {
        if failed.is_empty() && stopped.is_empty() {
            return match answered.len() {
                0 => "Nobody answered".to_string(),
                1 => format!("{} answered", answered[0].label),
                n => format!("All {n} answered"),
            };
        }
        if !answered.is_empty() {
            parts.push(format!("{} answered", answered.len()));
        }
        for t in &failed {
            parts.push(format!("{} didn\u{2019}t", t.label));
        }
    }
    for t in &stopped {
        parts.push(format!("{} was stopped", t.label));
    }
    parts.join(" \u{00B7} ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn council(labels: &[&str]) -> Council {
        let mut c = Council::default();
        for l in labels {
            c.add_member("agent:x", l);
        }
        c
    }

    fn open(c: &mut Council, kind: RoundKind) -> u32 {
        let n = c.next_round_number();
        let mut r = Round::new(n, kind);
        for m in c.active_members() {
            r.turns
                .push(SeatTurn::running(&format!("t{n}{}", m.id), m, 0));
        }
        c.rounds.push(r);
        n
    }

    /// RC2-CNCL3: the stance comes from the first line only, in the forms
    /// models actually write it; the same words later in a reply do not
    /// count, and the card body leaves the line out.
    #[test]
    fn stance_reads_first_line_only() {
        assert_eq!(
            parse_stance("Same answer\nClaude is right."),
            Some(Stance::Same)
        );
        assert_eq!(
            parse_stance("**Changed partly.**\n\nI'd still put Qwen on Code."),
            Some(Stance::Partly)
        );
        assert_eq!(
            parse_stance("  Changed my answer:\nYes."),
            Some(Stance::Changed)
        );
        assert_eq!(parse_stance("Stance: same answer"), Some(Stance::Same));
        assert_eq!(
            parse_stance("I agree with Codex.\nSame answer as before."),
            None,
            "only the first line counts"
        );
        assert_eq!(parse_stance("Same answer as Claude, mostly."), None);
        let (s, body) = stance_and_body("Changed partly\nKeep Gemma for now.");
        assert_eq!(s, Some(Stance::Partly));
        assert_eq!(body, "Keep Gemma for now.");
    }

    /// RC2-R510 (usability RC2 round 4, Minor: Gemma's card began "Same
    /// answer. I agree with Codex's recommendation …" and had no tag): the
    /// stance as the first sentence of a longer first line is read, and the
    /// rest of the line is the answer; words after the phrase with no stop
    /// still are not a stance.
    /// RC2-R613 (usability RC2 round 5, Minor 14): a Claude card tagged
    /// "Same answer" said "I disagree with Gemma on one thing". The tag says
    /// the member kept its own answer, which disagreeing with another does
    /// not contradict; the members' own stance lines are still read.
    #[test]
    fn rc2r613_a_stance_tag_says_whose_answer_was_kept() {
        assert_eq!(Stance::Same.label(), "Kept its answer");
        assert_eq!(Stance::Changed.label(), "Changed its answer");
        assert_eq!(Stance::Partly.label(), "Changed its answer partly");
        let (s, body) = stance_and_body("Same answer\nI disagree with Gemma on one thing: scrubs.");
        assert_eq!(s, Some(Stance::Same));
        assert_eq!(body, "I disagree with Gemma on one thing: scrubs.");
        for s in [Stance::Same, Stance::Changed, Stance::Partly] {
            assert!(!s.label().contains("Same answer"), "{s:?}");
            assert_eq!(Stance::from_token(s.as_str()), Some(s));
        }
    }

    #[test]
    fn rc2r510_a_stance_sentence_opening_a_line_is_read() {
        let (s, body) =
            stance_and_body("Same answer. I agree with Codex's recommendation of ZFS.\nMore.");
        assert_eq!(s, Some(Stance::Same));
        assert_eq!(body, "I agree with Codex's recommendation of ZFS.\nMore.");
        assert_eq!(
            parse_stance("**Changed partly:** keep backups and scrubs."),
            Some(Stance::Partly)
        );
        assert_eq!(parse_stance("Same answer as Claude, mostly."), None);
        assert_eq!(parse_stance("Same old story. Nothing new."), None);
    }

    /// RC2-CNCL3: a reply that ignores the format is still kept whole — only
    /// the tag is missing — and an ask round never tags.
    #[test]
    fn untagged_reply_is_kept() {
        let mut c = council(&["Claude Sonnet", "Codex"]);
        let r1 = open(&mut c, RoundKind::Ask);
        assert!(c.settle(r1, "m1", TurnStatus::Answered, "Same answer\nKeep Gemma."));
        assert_eq!(c.round(r1).unwrap().turn("m1").unwrap().stance, None);
        let r2 = open(&mut c, RoundKind::Respond);
        let reply = "I think Codex has a point about tool steps.\n\nKeep Gemma for now.";
        assert!(c.settle(r2, "m1", TurnStatus::Answered, reply));
        let t = c.round(r2).unwrap().turn("m1").unwrap();
        assert_eq!(t.status, TurnStatus::Answered);
        assert_eq!(t.stance, None);
        assert_eq!(t.text, reply);
        assert_eq!(t.body(), reply);
        assert!(c.settle(r2, "m2", TurnStatus::Answered, "Changed my answer\nFair."));
        let t = c.round(r2).unwrap().turn("m2").unwrap();
        assert_eq!(t.stance, Some(Stance::Changed));
        assert_eq!(t.body(), "Fair.");
    }

    /// RC2-CNCL3: the round header names who changed their answer and who
    /// didn't answer, and counts the rest.
    #[test]
    fn round_summary_names_who_changed() {
        let mut c = council(&["Claude Sonnet", "Codex", "Gemma 4 26B", "DeepSeek Flash"]);
        let r1 = open(&mut c, RoundKind::Ask);
        assert_eq!(
            round_summary(c.round(r1).unwrap()),
            "4 answering \u{00B7} 0 done"
        );
        c.settle(r1, "m3", TurnStatus::Answered, "Keep Gemma.");
        assert_eq!(
            round_summary(c.round(r1).unwrap()),
            "3 answering \u{00B7} 1 done"
        );
        c.settle(r1, "m1", TurnStatus::Answered, "a");
        c.settle(r1, "m2", TurnStatus::Answered, "b");
        let fail = TurnStatus::Failed {
            plain: "Its service stopped responding after 30 seconds.".into(),
            detail: "read timeout".into(),
            needs_owner: false,
        };
        c.settle(r1, "m4", fail, "");
        assert_eq!(
            round_summary(c.round(r1).unwrap()),
            "3 answered \u{00B7} DeepSeek Flash didn\u{2019}t"
        );
        let r2 = open(&mut c, RoundKind::Respond);
        c.round_mut(r2).unwrap().turns.retain(|t| t.member != "m4");
        c.settle(r2, "m1", TurnStatus::Answered, "Same answer\nok");
        c.settle(r2, "m2", TurnStatus::Answered, "Changed partly\nok");
        c.settle(r2, "m3", TurnStatus::Answered, "Same answer\nok");
        assert_eq!(
            round_summary(c.round(r2).unwrap()),
            "2 kept their answer \u{00B7} Codex changed its answer in part"
        );
        let r3 = open(&mut c, RoundKind::Ask);
        for m in ["m1", "m2", "m3", "m4"] {
            c.settle(r3, m, TurnStatus::Answered, "x");
        }
        assert_eq!(round_summary(c.round(r3).unwrap()), "All 4 answered");
    }

    /// Member ids are never reused and removal keeps the seat's history.
    #[test]
    fn members_keep_their_ids() {
        let mut c = council(&["A", "B"]);
        assert!(c.remove_member("m2"));
        assert!(!c.remove_member("m2"));
        assert_eq!(c.add_member("in-app", "C"), "m3");
        assert_eq!(
            c.active_members()
                .iter()
                .map(|m| m.label.as_str())
                .collect::<Vec<_>>(),
            ["A", "C"]
        );
        assert_eq!(c.member("m2").map(|m| m.removed), Some(true));
    }
}
