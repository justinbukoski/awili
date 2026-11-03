//! The conversation log the composer appends to — a plain ordered list of
//! messages. No network, no provider, no persistence in this slice: `send` just
//! appends the user's text and a canned echo reply (the typing-feel slice only
//! needs the list to grow as you send).

/// Who authored a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// What kind of content this message carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    /// A regular user or assistant message.
    Normal,
    /// A summary message produced by the summarizer (v9).
    Summary,
    /// RC2-RUNF: a user row Awili wrote for a click on the plan card ("Make
    /// the changes" / "Don't change anything"). It is sent and kept like a
    /// normal user row; only its look differs (a note about the click, not a
    /// bubble). A message the owner types is never this kind, whatever its
    /// words.
    PlanClick,
    /// RC2-UX (R2-14): the user row a Retry click writes — the owner's last
    /// question sent again. Sent and kept like a normal user row; it is drawn
    /// as a short note ("You asked again."), not as a second copy of the
    /// question.
    Retry,
    /// RC2-CNCL1: one council member's answer (the member, round and status
    /// live in `council_turns`). Kept and searched like any assistant row.
    CouncilAnswer,
}

impl MessageKind {
    /// The wire string this kind is persisted as (matches the `kind` column on
    /// `StoredMessage`: `"normal"` / `"summary"` / `"plan_click"`).
    pub fn as_persist_str(self) -> &'static str {
        match self {
            MessageKind::Normal => "normal",
            MessageKind::Summary => "summary",
            MessageKind::PlanClick => "plan_click",
            MessageKind::Retry => "retry",
            MessageKind::CouncilAnswer => "council_answer",
        }
    }

    /// Parse a persisted `kind` string. Anything other than `"summary"` or
    /// `"plan_click"` — including legacy pre-v9 rows that predate the column,
    /// and plan-click rows written before RC2-RUNF (they carry no mark and
    /// stay ordinary rows) — reads as [`MessageKind::Normal`].
    pub fn from_persist_str(s: &str) -> MessageKind {
        match s {
            "summary" => MessageKind::Summary,
            "plan_click" => MessageKind::PlanClick,
            "retry" => MessageKind::Retry,
            "council_answer" => MessageKind::CouncilAnswer,
            _ => MessageKind::Normal,
        }
    }
}

/// One message in the log.
///
/// `Eq` is intentionally NOT derived: `provenance` may carry an `f64` cost
/// estimate, so the log is `PartialEq` only.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub role: Role,
    pub text: String,
    /// What kind of content this message carries (v9). Default for regular
    /// pushes is [`MessageKind::Normal`]; the summarizer sets it to
    /// [`MessageKind::Summary`] when it inserts a summary message.
    pub kind: MessageKind,
    /// Monotonic logical timestamp (insertion order). Not a wall clock — the
    /// slice has no time source — just enough to order/key rows.
    pub ts: u64,
    /// What actually ran for this turn (slice-5). `None` for user turns and for
    /// turns produced before provenance tracking.
    pub provenance: Option<crate::provenance::Provenance>,
    /// If this message was superseded by a summary, the `ts` of the summary
    /// message that replaced it (v9). `None` means the message is still live.
    pub superseded_by: Option<u64>,
}

/// The ordered message log.
#[derive(Clone, Debug, Default)]
pub struct ConversationState {
    messages: Vec<Message>,
    next_ts: u64,
}

impl ConversationState {
    /// An empty conversation.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a conversation from a list of `(role, text)` pairs — used for the
    /// static starter messages the window shows on first paint. Timestamps are
    /// assigned in order.
    pub fn seeded(seed: impl IntoIterator<Item = (Role, String)>) -> Self {
        let mut state = Self::new();
        for (role, text) in seed {
            state.push(role, text);
        }
        state
    }

    /// Seed a conversation from `(role, text, kind)` triples — used when
    /// restoring persisted messages so a [`MessageKind::Summary`] survives a
    /// reload instead of silently coming back as `Normal`. Timestamps are
    /// assigned in order, exactly like [`seeded`](Self::seeded).
    pub fn seeded_kinded(seed: impl IntoIterator<Item = (Role, String, MessageKind)>) -> Self {
        let mut state = Self::new();
        for (role, text, kind) in seed {
            state.push_kind(role, text, kind);
        }
        state
    }

    /// The messages, oldest first.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// The next timestamp a [`push`](Self::push) will assign.
    pub fn next_ts(&self) -> u64 {
        self.next_ts
    }

    /// Drop everything from position `keep` onward, keeping the survivors'
    /// `ts`, `kind`, `provenance`, and `superseded_by` intact (safe
    /// regeneration's in-memory half: the stale reply disappears, but the
    /// surviving prefix keeps its full metadata — a text-only reseed would
    /// lose summary kinds, provenance, and supersession links).
    ///
    /// `next_ts` is set to one past the survivors' maximum `ts`, so the next
    /// push never collides with a durable timestamp the survivors still carry.
    /// (The `ts == position` invariant is re-established by the app layer
    /// re-deriving it through the `seeded` restore path when needed — the
    /// durable store addresses rows by `ts`, never by position.)
    ///
    /// RC2-APP (F0114): a survivor whose summary was cut off (the summary sat
    /// past `keep`) is live again — never hidden by a summary that no longer
    /// exists (the same rule a restore applies to a dangling link).
    pub fn truncate_to_keep(&mut self, keep: usize) {
        if keep < self.messages.len() {
            self.messages.truncate(keep);
        }
        let present: std::collections::HashSet<u64> = self.messages.iter().map(|m| m.ts).collect();
        for m in &mut self.messages {
            if m.superseded_by.is_some_and(|s| !present.contains(&s)) {
                m.superseded_by = None;
            }
        }
        let max = self.messages.iter().map(|m| m.ts).max().unwrap_or(0);
        self.next_ts = max + 1;
    }

    /// Number of messages in the log.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// `true` when no messages have been sent.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Append a message with the next timestamp, returning its `ts`. Default
    /// kind is [`MessageKind::Normal`].
    pub fn push(&mut self, role: Role, text: impl Into<String>) -> u64 {
        self.push_kind(role, text, MessageKind::Normal)
    }

    /// Append a message with the next timestamp and an explicit [`MessageKind`],
    /// returning its `ts`.
    pub fn push_kind(&mut self, role: Role, text: impl Into<String>, kind: MessageKind) -> u64 {
        let ts = self.next_ts;
        self.next_ts += 1;
        self.messages.push(Message {
            role,
            text: text.into(),
            ts,
            kind,
            provenance: None,
            superseded_by: None,
        });
        ts
    }

    /// Send the composer's text: append it as a `User` message, then append a
    /// canned `Assistant` echo. Slice 1's offline behaviour, kept for its tests;
    /// slice 2's app instead pushes the user turn and streams a real reply into an
    /// empty assistant message via [`append_text`](Self::append_text).
    /// Whitespace-only input is rejected (returns `false`, nothing appended).
    pub fn send(&mut self, text: impl Into<String>) -> bool {
        let text = text.into();
        if text.trim().is_empty() {
            return false;
        }
        self.push(Role::User, text.clone());
        self.push(Role::Assistant, canned_reply(&text));
        true
    }

    /// Borrow the message at `index`, if it exists.
    pub fn message(&self, index: usize) -> Option<&Message> {
        self.messages.get(index)
    }

    /// Append `chunk` to the text of the message at `index` (the streaming-token
    /// path: the reply bubble grows as the provider emits tokens). No-op if the
    /// index is out of range. Returns whether a message was updated.
    pub fn append_text(&mut self, index: usize, chunk: &str) -> bool {
        match self.messages.get_mut(index) {
            Some(msg) => {
                msg.text.push_str(chunk);
                true
            }
            None => false,
        }
    }

    /// Replace the text of the message at `index` (used to swap a streamed reply
    /// for an error notice, or to finalise it). Returns whether it was updated.
    pub fn set_text(&mut self, index: usize, text: impl Into<String>) -> bool {
        match self.messages.get_mut(index) {
            Some(msg) => {
                msg.text = text.into();
                true
            }
            None => false,
        }
    }

    /// Attach provenance to the message at `index` (the app calls this once a
    /// turn's route/model/usage is known). Returns whether it was updated.
    pub fn set_provenance(&mut self, index: usize, prov: crate::provenance::Provenance) -> bool {
        match self.messages.get_mut(index) {
            Some(msg) => {
                msg.provenance = Some(prov);
                true
            }
            None => false,
        }
    }

    /// Mark the message with timestamp `ts` as superseded by the summary at
    /// `summary_ts`. Returns `true` if the message was found and updated.
    ///
    /// RC2-APP (F0466): refused (`false`, nothing changed) unless `summary_ts`
    /// is ANOTHER message of this conversation of kind
    /// [`MessageKind::Summary`] — a missing, ordinary or self target would
    /// hide a live message with nothing standing in for it.
    pub fn set_superseded(&mut self, ts: u64, summary_ts: u64) -> bool {
        let is_summary = self
            .messages
            .iter()
            .any(|m| m.ts == summary_ts && m.kind == MessageKind::Summary);
        if ts == summary_ts || !is_summary {
            return false;
        }
        match self.messages.iter_mut().find(|m| m.ts == ts) {
            Some(msg) => {
                msg.superseded_by = Some(summary_ts);
                true
            }
            None => false,
        }
    }

    /// Messages that have NOT been superseded, in order — i.e. the live
    /// conversation visible to the user or the provider.
    pub fn live_messages(&self) -> Vec<&Message> {
        self.messages
            .iter()
            .filter(|m| m.superseded_by.is_none())
            .collect()
    }

    /// Reverse a "Summarize earlier" action (slice 21 §D4 "Discard summary"):
    /// clear the `superseded_by` mark from every turn the summary at `summary_ts`
    /// replaced (restoring them to live), then drop the summary message itself.
    /// Returns the restored turns' `ts`, oldest-first, or `None` if no message
    /// with that `ts` is a [`MessageKind::Summary`] (then nothing is touched).
    /// The exact inverse of the [`set_superseded`](Self::set_superseded) /
    /// `push_kind(Summary)` pair the summarize action applies.
    ///
    /// Removing one message would leave a hole in the `ts` sequence; because a
    /// restore re-derives `ts` *positionally* (`0..len`, see
    /// [`seeded`](Self::seeded)) and every durable supersede write addresses rows
    /// by `ts`, that hole would desync memory from disk on the next launch. So
    /// the gap is closed: every later message (and any link pointing past the
    /// hole — another summary that sat after this one) shifts down by one, and
    /// `next_ts` follows, keeping `ts` a gapless `0..len`.
    pub fn discard_summary(&mut self, summary_ts: u64) -> Option<Vec<u64>> {
        // The target must really be a summary message, not just any ts.
        let pos = self
            .messages
            .iter()
            .position(|m| m.ts == summary_ts && m.kind == MessageKind::Summary)?;
        // Restore the turns this summary replaced (they sort before it, so their
        // ts are below `summary_ts` and are not shifted below).
        let restored: Vec<u64> = self
            .messages
            .iter_mut()
            .filter(|m| m.superseded_by == Some(summary_ts))
            .map(|m| {
                m.superseded_by = None;
                m.ts
            })
            .collect();
        self.messages.remove(pos);
        // Close the ts gap left by the removed summary.
        for m in &mut self.messages {
            if m.ts > summary_ts {
                m.ts -= 1;
            }
            if let Some(s) = m.superseded_by {
                if s > summary_ts {
                    m.superseded_by = Some(s - 1);
                }
            }
        }
        self.next_ts -= 1;
        Some(restored)
    }
}

/// Slice 1's stand-in for a model reply: a fixed echo. Retained for `send`'s
/// offline tests; the slice-2 app uses a real provider instead.
fn canned_reply(user_text: &str) -> String {
    let trimmed = user_text.trim();
    format!("echo: {trimmed}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_conversation_is_empty() {
        let c = ConversationState::new();
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
        assert_eq!(c.messages(), &[]);
    }

    #[test]
    fn push_assigns_monotonic_timestamps() {
        let mut c = ConversationState::new();
        let a = c.push(Role::User, "first");
        let b = c.push(Role::Assistant, "second");
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert_eq!(c.len(), 2);
        assert_eq!(c.messages()[0].role, Role::User);
        assert_eq!(c.messages()[0].text, "first");
        assert_eq!(c.messages()[1].ts, 1);
    }

    #[test]
    fn send_appends_user_then_echo() {
        let mut c = ConversationState::new();
        assert!(c.send("hello"));
        assert_eq!(c.len(), 2);
        assert_eq!(c.messages()[0].role, Role::User);
        assert_eq!(c.messages()[0].text, "hello");
        assert_eq!(c.messages()[1].role, Role::Assistant);
        assert_eq!(c.messages()[1].text, "echo: hello");
    }

    #[test]
    fn send_rejects_blank_and_whitespace() {
        let mut c = ConversationState::new();
        assert!(!c.send(""));
        assert!(!c.send("   \n\t "));
        assert!(c.is_empty());
    }

    #[test]
    fn send_trims_echo_but_keeps_user_text_verbatim() {
        let mut c = ConversationState::new();
        assert!(c.send("  spaced  "));
        // User text is kept exactly as typed...
        assert_eq!(c.messages()[0].text, "  spaced  ");
        // ...while the echo trims for a tidy reply.
        assert_eq!(c.messages()[1].text, "echo: spaced");
    }

    #[test]
    fn send_handles_multibyte_text() {
        let mut c = ConversationState::new();
        assert!(c.send("こんにちは 😀"));
        assert_eq!(c.messages()[0].text, "こんにちは 😀");
        assert_eq!(c.messages()[1].text, "echo: こんにちは 😀");
    }

    #[test]
    fn seeded_preserves_order_and_timestamps() {
        let c = ConversationState::seeded([
            (Role::Assistant, "Welcome to Awili.".to_string()),
            (Role::User, "Hi.".to_string()),
        ]);
        assert_eq!(c.len(), 2);
        assert_eq!(c.messages()[0].text, "Welcome to Awili.");
        assert_eq!(c.messages()[0].ts, 0);
        assert_eq!(c.messages()[1].ts, 1);
    }

    #[test]
    fn append_text_grows_streaming_reply() {
        let mut c = ConversationState::new();
        c.push(Role::User, "hi");
        let idx = c.len();
        c.push(Role::Assistant, "");
        assert!(c.append_text(idx, "Hel"));
        assert!(c.append_text(idx, "lo"));
        assert_eq!(c.message(idx).unwrap().text, "Hello");
        // Out-of-range is a no-op, not a panic.
        assert!(!c.append_text(99, "x"));
    }

    #[test]
    fn set_text_replaces_message() {
        let mut c = ConversationState::new();
        c.push(Role::Assistant, "partial");
        assert!(c.set_text(0, "⚠ error"));
        assert_eq!(c.message(0).unwrap().text, "⚠ error");
        assert!(!c.set_text(5, "x"));
    }

    #[test]
    fn set_provenance_attaches_to_target_row_and_reports_missing() {
        // `set_provenance` is the documented attach primitive (slice5-handoff:
        // `convo.set_provenance(row_idx, prov)`) and the ONLY writer of
        // `Message.provenance` — which `conv_view` reads to render the badge — yet
        // the whole method was uncovered, so mutating its bool return, dropping the
        // `Some(prov)` write, or mistargeting the row all survived the suite. Pin
        // the full contract: it targets the row at `index`, leaves siblings
        // untouched, and returns `false` (attaching nothing) for an out-of-range
        // index.
        let mut c = ConversationState::new();
        c.push(Role::User, "q"); // index 0 — must stay None
        c.push(Role::Assistant, "a"); // index 1 — the target
        let prov = crate::provenance::Provenance::chat(
            "frontier",
            "anthropic",
            "claude-opus-4-8",
            crate::provenance::Locality::Cloud,
            "cloud fallback",
        );

        assert!(c.set_provenance(1, prov.clone()));
        // The target row carries exactly the attached provenance...
        assert_eq!(c.message(1).unwrap().provenance, Some(prov.clone()));
        // ...and the sibling is untouched (pins get_mut(index), not a fixed row).
        assert!(c.message(0).unwrap().provenance.is_none());

        // Out of range attaches nothing and reports false (no panic).
        assert!(!c.set_provenance(9, prov));
        assert!(c.message(0).unwrap().provenance.is_none());
    }

    #[test]
    fn timestamps_continue_after_seeding() {
        let mut c = ConversationState::seeded([(Role::Assistant, "seed".to_string())]);
        let ts = c.push(Role::User, "next");
        assert_eq!(ts, 1);
    }

    // ─── v9: superseded_by ────────────────────────────────────────────────

    #[test]
    fn new_message_superseded_by_is_none() {
        let mut c = ConversationState::new();
        let ts = c.push(Role::User, "hello");
        assert!(c.message(ts as usize).unwrap().superseded_by.is_none());
    }

    #[test]
    fn set_superseded_marks_message() {
        let mut c = ConversationState::new();
        let a = c.push(Role::User, "alpha");
        // RC2-APP (F0466): the target is a real summary.
        let b = c.push_kind(Role::Assistant, "beta", MessageKind::Summary);

        // Supersede message a (ts=0) by summary b (ts=1).
        assert!(c.set_superseded(a, b));
        assert_eq!(c.message(0).unwrap().superseded_by, Some(1));
        // Message b is untouched.
        assert!(c.message(1).unwrap().superseded_by.is_none());
    }

    #[test]
    fn set_superseded_missing_ts_returns_false() {
        let mut c = ConversationState::new();
        c.push(Role::User, "hello");
        // Ts=99 doesn't exist.
        assert!(!c.set_superseded(99, 0));
    }

    /// RC2-SVC7 (F0466): a missing or self target is refused and hides
    /// nothing; a real summary target still supersedes.
    #[test]
    fn rc2svc7_set_superseded_refuses_missing_and_self_targets() {
        let mut c = ConversationState::new();
        let a = c.push(Role::User, "alpha");
        assert!(!c.set_superseded(a, 99), "missing target");
        assert!(!c.set_superseded(a, a), "self target");
        assert_eq!(c.live_messages().len(), 1, "alpha stays live");
        let s = c.push_kind(Role::Assistant, "summary", MessageKind::Summary);
        assert!(c.set_superseded(a, s));
        assert_eq!(c.live_messages().len(), 1, "only the summary is live");
    }

    #[test]
    fn live_messages_excludes_superseded() {
        let mut c = ConversationState::new();
        let a = c.push(Role::User, "alpha");
        let _b = c.push(Role::Assistant, "beta");
        let c_ts = c.push(Role::User, "gamma");
        // RC2-APP (F0466): the target is a real summary.
        let d = c.push_kind(Role::Assistant, "delta", MessageKind::Summary);

        // Supersede alpha (ts=0) and gamma (ts=2).
        assert!(c.set_superseded(a, d));
        assert!(c.set_superseded(c_ts, d));

        let live = c.live_messages();
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].ts, 1); // beta — still live
        assert_eq!(live[1].ts, 3); // delta — still live
    }

    #[test]
    fn live_messages_all_live_when_none_superseded() {
        let mut c = ConversationState::new();
        c.push(Role::User, "a");
        c.push(Role::Assistant, "b");
        let live = c.live_messages();
        assert_eq!(live.len(), 2);
    }

    #[test]
    fn live_messages_empty_when_all_superseded() {
        let mut c = ConversationState::new();
        // RC2-APP (F0466): only a summary stands in for a message, and never
        // for itself; two summaries standing in for each other hide all.
        let a = c.push(Role::User, "a");
        let s1 = c.push_kind(Role::Assistant, "s1", MessageKind::Summary);
        let s2 = c.push_kind(Role::Assistant, "s2", MessageKind::Summary);
        assert!(c.set_superseded(a, s1));
        assert!(!c.set_superseded(s1, s1), "never by itself");
        assert!(c.set_superseded(s1, s2));
        assert!(c.set_superseded(s2, s1));
        assert!(c.live_messages().is_empty());
    }

    // ─── v9: discard_summary (the inverse of summarize-earlier) ─────────────

    #[test]
    fn discard_summary_restores_turns_and_drops_the_summary() {
        let mut c = ConversationState::new();
        let a = c.push(Role::User, "alpha"); // ts 0
        let b = c.push(Role::Assistant, "beta"); // ts 1
        let _live = c.push(Role::User, "gamma"); // ts 2 — stays live
        let summary = c.push_kind(Role::Assistant, "SUMMARY", MessageKind::Summary); // ts 3
        assert!(c.set_superseded(a, summary));
        assert!(c.set_superseded(b, summary));

        let restored = c.discard_summary(summary).expect("summary exists");
        // Both superseded turns come back, oldest-first.
        assert_eq!(restored, vec![0, 1]);
        // The summary bubble is gone; the three real turns remain, all live.
        assert_eq!(c.len(), 3);
        assert!(c.messages().iter().all(|m| m.superseded_by.is_none()));
        assert!(c.messages().iter().all(|m| m.kind == MessageKind::Normal));
        assert_eq!(c.live_messages().len(), 3);
    }

    #[test]
    fn discard_summary_missing_ts_is_none_and_mutates_nothing() {
        let mut c = ConversationState::new();
        let a = c.push(Role::User, "alpha");
        let summary = c.push_kind(Role::Assistant, "SUMMARY", MessageKind::Summary);
        assert!(c.set_superseded(a, summary));

        // ts 99 names no message at all.
        assert!(c.discard_summary(99).is_none());
        // ts 0 is a Normal message, not a summary — refused, nothing restored.
        assert!(c.discard_summary(0).is_none());
        assert_eq!(c.len(), 2);
        assert_eq!(c.message(0).unwrap().superseded_by, Some(summary));
    }

    #[test]
    fn discard_summary_closes_the_gap_and_remaps_a_later_summarys_link() {
        // Codex's restart scenario: a summary sits in the MIDDLE (the user kept
        // chatting after summarizing), and a later summary supersedes a turn.
        // Discarding the middle summary must close the ts hole so `ts` stays a
        // gapless 0..len — and the later summary (and its link) must shift to
        // match, or a restart would desync memory from disk.
        let mut c = ConversationState::new();
        let a = c.push(Role::User, "alpha"); // ts 0 — superseded by s1
        let b = c.push(Role::User, "beta"); // ts 1 — superseded by s2
        let s1 = c.push_kind(Role::Assistant, "S1", MessageKind::Summary); // ts 2
        let _c = c.push(Role::User, "gamma"); // ts 3 — chatted after s1
        let s2 = c.push_kind(Role::Assistant, "S2", MessageKind::Summary); // ts 4
        assert!(c.set_superseded(a, s1));
        assert!(c.set_superseded(b, s2));

        // Discarding s1 (ts 2) restores only alpha; everything above ts 2 slides
        // down one: gamma 3→2, s2 4→3, and beta's link 4→3.
        let restored = c.discard_summary(s1).expect("s1 exists");
        assert_eq!(restored, vec![0]);
        // ts is a gapless 0..len with no hole at the old summary position.
        let tss: Vec<u64> = c.messages().iter().map(|m| m.ts).collect();
        assert_eq!(tss, vec![0, 1, 2, 3]);
        assert_eq!(
            c.messages()
                .iter()
                .find(|m| m.ts == 0)
                .unwrap()
                .superseded_by,
            None
        );
        // beta's link followed s2 down to its new ts (3).
        assert_eq!(
            c.messages()
                .iter()
                .find(|m| m.ts == 1)
                .unwrap()
                .superseded_by,
            Some(3)
        );
        // The surviving summary is now at ts 3; s1 is gone.
        assert!(c
            .messages()
            .iter()
            .any(|m| m.ts == 3 && m.kind == MessageKind::Summary));
        assert!(
            c.messages()
                .iter()
                .filter(|m| m.kind == MessageKind::Summary)
                .count()
                == 1
        );
        // The next push continues the gapless sequence.
        assert_eq!(c.push(Role::User, "next"), 4);
        let _ = (s2, b);
    }

    // ─── MessageKind ──────────────────────────────────────────────────────

    #[test]
    fn push_default_kind_is_normal() {
        let mut c = ConversationState::new();
        c.push(Role::User, "hi");
        assert_eq!(c.messages()[0].kind, MessageKind::Normal);
    }

    #[test]
    fn push_kind_sets_summary() {
        let mut c = ConversationState::new();
        c.push_kind(Role::Assistant, "summary text", MessageKind::Summary);
        assert_eq!(c.messages()[0].kind, MessageKind::Summary);
        assert_eq!(c.messages()[0].text, "summary text");
        assert_eq!(c.messages()[0].role, Role::Assistant);
    }

    #[test]
    fn message_kind_persist_str_roundtrips() {
        assert_eq!(MessageKind::Normal.as_persist_str(), "normal");
        assert_eq!(MessageKind::Summary.as_persist_str(), "summary");
        assert_eq!(
            MessageKind::from_persist_str("summary"),
            MessageKind::Summary
        );
        assert_eq!(MessageKind::from_persist_str("normal"), MessageKind::Normal);
        // Legacy / unknown rows fall back to Normal.
        assert_eq!(MessageKind::from_persist_str(""), MessageKind::Normal);
        assert_eq!(MessageKind::from_persist_str("bogus"), MessageKind::Normal);
    }

    /// RC2-RUNF: a plan-card click keeps its own mark through storage; the
    /// same words without the mark are an ordinary row.
    #[test]
    fn rc2runf_a_plan_click_keeps_its_mark() {
        assert_eq!(MessageKind::PlanClick.as_persist_str(), "plan_click");
        assert_eq!(
            MessageKind::from_persist_str("plan_click"),
            MessageKind::PlanClick
        );
        let c = ConversationState::seeded_kinded([
            (Role::User, "Approved".to_string(), MessageKind::PlanClick),
            (Role::User, "Approved".to_string(), MessageKind::Normal),
        ]);
        assert_eq!(c.messages()[0].kind, MessageKind::PlanClick);
        assert_eq!(c.messages()[1].kind, MessageKind::Normal);
    }

    /// RC2-UX (R2-14): a Retry row keeps its mark through storage.
    #[test]
    fn rc2ux_a_retry_keeps_its_mark() {
        assert_eq!(MessageKind::Retry.as_persist_str(), "retry");
        assert_eq!(MessageKind::from_persist_str("retry"), MessageKind::Retry);
        let c = ConversationState::seeded_kinded([
            (Role::User, "q".to_string(), MessageKind::Normal),
            (Role::User, "q".to_string(), MessageKind::Retry),
        ]);
        assert_eq!(c.messages()[1].kind, MessageKind::Retry);
    }

    #[test]
    fn truncate_to_keep_preserves_metadata_and_avoids_ts_collision() {
        let mut c = ConversationState::new();
        let q = c.push(Role::User, "q1");
        let a = c.push_kind(Role::Assistant, "old summary", MessageKind::Summary);
        let _ = c.push(Role::User, "q2");
        let b = c.push(Role::Assistant, "a2");
        c.set_superseded(q, a);
        c.set_superseded(b, b + 1); // a dangling link past the truncation point

        // Drop the last two messages: the summary and q2 survive.
        c.truncate_to_keep(2);
        assert_eq!(c.len(), 2);
        assert_eq!(c.messages()[0].ts, 0);
        assert_eq!(c.messages()[0].text, "q1");
        assert_eq!(c.messages()[1].ts, 1);
        assert_eq!(c.messages()[1].kind, MessageKind::Summary);
        assert_eq!(c.messages()[1].superseded_by, None);
        assert_eq!(c.messages()[0].superseded_by, Some(1));

        // The next push continues past the survivors' max ts — no collision
        // with a durable timestamp the surviving rows still address.
        let fresh = c.push(Role::Assistant, "fresh");
        assert!(fresh > 1);
        assert_eq!(c.len(), 3);

        // Truncating to the full length (or beyond) is a no-op.
        c.truncate_to_keep(99);
        assert_eq!(c.len(), 3);
    }

    /// RC2-APP (F0466): an ordinary message, a missing one, or the message
    /// itself never stands in for a message; a summary does.
    #[test]
    fn rc2app_only_a_summary_can_supersede() {
        let mut c = ConversationState::new();
        let a = c.push(Role::User, "alpha");
        let b = c.push(Role::Assistant, "beta");
        assert!(!c.set_superseded(a, b), "an ordinary message");
        assert!(!c.set_superseded(a, 99), "a missing message");
        assert!(!c.set_superseded(a, a), "the message itself");
        assert_eq!(c.live_messages().len(), 2, "nothing is hidden");
        let s = c.push_kind(Role::Assistant, "summary", MessageKind::Summary);
        assert!(!c.set_superseded(s, s), "a summary never hides itself");
        assert!(c.set_superseded(a, s));
        assert_eq!(c.message(0).unwrap().superseded_by, Some(s));
    }

    /// RC2-APP (F0114): cutting the log before a summary brings back the
    /// turns it stood in for; a summary that survives keeps its turns hidden.
    #[test]
    fn rc2app_truncation_drops_links_to_a_summary_that_is_gone() {
        let mut c = ConversationState::new();
        let q = c.push(Role::User, "q1");
        let a = c.push(Role::Assistant, "a1");
        let s = c.push_kind(Role::Assistant, "summary", MessageKind::Summary);
        let q2 = c.push(Role::User, "q2");
        assert!(c.set_superseded(q, s) && c.set_superseded(a, s));
        c.truncate_to_keep(4);
        assert_eq!(c.live_messages().len(), 2, "the summary still stands in");
        assert_eq!(c.messages()[3].ts, q2);
        c.truncate_to_keep(2);
        assert!(c.messages().iter().all(|m| m.superseded_by.is_none()));
        assert_eq!(c.live_messages().len(), 2, "q1 and a1 are live again");
    }

    #[test]
    fn seeded_kinded_preserves_kind() {
        let c = ConversationState::seeded_kinded([
            (Role::User, "q".to_string(), MessageKind::Normal),
            (Role::Assistant, "sum".to_string(), MessageKind::Summary),
        ]);
        assert_eq!(c.messages()[0].kind, MessageKind::Normal);
        assert_eq!(c.messages()[1].kind, MessageKind::Summary);
        // Timestamps still assigned in order, like `seeded`.
        assert_eq!(c.messages()[0].ts, 0);
        assert_eq!(c.messages()[1].ts, 1);
    }

    /// RC2-CNCL1: a council member's answer is its own row kind and keeps it
    /// through the persisted string; everything unknown is still Normal.
    #[test]
    fn council_answer_kind_round_trips() {
        assert_eq!(
            MessageKind::CouncilAnswer.as_persist_str(),
            "council_answer"
        );
        assert_eq!(
            MessageKind::from_persist_str("council_answer"),
            MessageKind::CouncilAnswer
        );
        assert_eq!(
            MessageKind::from_persist_str("council_answers"),
            MessageKind::Normal
        );
        let mut c = ConversationState::new();
        c.push(Role::User, "which model?");
        c.push_kind(Role::Assistant, "Gemma.", MessageKind::CouncilAnswer);
        assert_eq!(c.messages()[1].kind, MessageKind::CouncilAnswer);
    }
}
