//! One conversation's live view state (`ConvView`) plus the small GPUI-free
//! helpers that read or mutate it: the Retry target, the discard-summary
//! realignment for old threads that hold a summary row, the stop/failure row
//! texts, and the provenance-tag strip. Every Workshop turn (Work or Code)
//! streams through its own owner (`work.rs`, `code_run.rs`); this view is only
//! the shared transcript those owners write into.

use gpui::ScrollHandle;

use awili_core::{ConversationState, MessageKind, Role};

use crate::MessageRow;

/// Identity remains unique while any callback retains it, including across
/// view removal/recreation. Pointer identity avoids counter wrap/reuse.
#[derive(Clone)]
pub(crate) struct TurnToken(std::sync::Arc<()>);

impl TurnToken {
    pub(crate) fn fresh() -> Self {
        Self(std::sync::Arc::new(()))
    }
    /// Whether `self` is the SAME identity as `other` (pointer equality).
    pub(crate) fn matches(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

/// One conversation's live view state: its message log, pre-rendered rows, and
/// scroll state. Kept for EVERY conversation (not just the active one) so
/// switching away never drops an in-flight run's output — it keeps appending
/// into its own `ConvView`.
pub(crate) struct ConvView {
    /// Transient reply presentation revision; never a durable message id or worker identity.
    pub(crate) reply_presentation_token: TurnToken,
    pub(crate) convo: ConversationState,
    /// Pre-rendered, stable per-message rows; index-aligned with `convo`.
    pub(crate) rows: Vec<MessageRow>,
    /// Row indices whose bubble holds an error notice rather than model output.
    /// These are shown to the user but never persisted nor replayed as assistant
    /// history (review SF4).
    pub(crate) errored: std::collections::HashSet<usize>,
    /// Per-conversation scroll position, preserved across switches (spec §2).
    pub(crate) scroll: ScrollHandle,
    /// RC1-F3: whether the transcript stays at its bottom (`transcript_keys::Tail`).
    pub(crate) tail: crate::transcript_keys::Tail,
}

impl ConvView {
    pub(crate) fn new() -> Self {
        Self::from_state(ConversationState::new())
    }

    pub(crate) fn from_state(convo: ConversationState) -> Self {
        let rows = convo.messages().iter().map(MessageRow::new).collect();
        Self {
            reply_presentation_token: TurnToken::fresh(),
            convo,
            rows,
            errored: std::collections::HashSet::new(),
            scroll: ScrollHandle::new(),
            tail: Default::default(),
        }
    }

    pub(crate) fn invalidate_reply_presentations(&mut self) {
        self.reply_presentation_token = TurnToken::fresh();
    }

    /// Cut the view back to its first `len` messages, the way a rewritten
    /// transcript tail looks to the reply presentations (tests only: no
    /// shipped path rewrites a transcript in place any more).
    #[cfg(test)]
    pub(crate) fn truncate_to(&mut self, len: usize) {
        self.invalidate_reply_presentations();
        self.convo.truncate_to_keep(len);
        self.rows.truncate(len);
        self.errored.retain(|&i| i < len);
    }

    /// Discard the summary at `summary_ts` (slice 21 §D4 "Discard summary"): the
    /// in-memory inverse of the summarize action. Restores the turns it
    /// superseded, drops the summary bubble, and keeps `rows`/`errored` aligned
    /// with `convo` — the single removed index shifts every later error-notice
    /// index down by one. Returns the restored turns' `ts`, or `None` if
    /// `summary_ts` is not a summary message (then nothing is touched). GPUI-free
    /// so the realignment stays unit-testable without a window.
    pub(crate) fn discard_summary(&mut self, summary_ts: u64) -> Option<Vec<u64>> {
        // The row the convo will drop — captured before the mutation so the
        // error-index shift below references the right position.
        let removed = self
            .convo
            .messages()
            .iter()
            .position(|m| m.ts == summary_ts && m.kind == MessageKind::Summary)?;
        let restored = self.convo.discard_summary(summary_ts)?;
        self.invalidate_reply_presentations();
        // One message left the log: rebuild the index-aligned rows from the now
        // canonical convo, and slide the error-notice indices to match.
        self.rows = self.convo.messages().iter().map(MessageRow::new).collect();
        self.errored = self
            .errored
            .iter()
            .filter(|&&i| i != removed)
            .map(|&i| if i > removed { i - 1 } else { i })
            .collect();
        Some(restored)
    }
}

/// The row regenerate may act on (slice 12 §2): the LAST message of the
/// conversation, when it is assistant-authored and some user turn precedes it
/// (a config-error notice with no prompt above it has nothing to re-run).
/// Everywhere else the button renders disabled.
pub(crate) fn regen_target(view: &ConvView) -> Option<usize> {
    let last = view.convo.len().checked_sub(1)?;
    let is_assistant = view.convo.messages()[last].role == Role::Assistant;
    (is_assistant && preceding_user_index(&view.convo, last).is_some()).then_some(last)
}

/// The user turn an assistant bubble replies to: the nearest user row above it.
pub(crate) fn preceding_user_index(convo: &ConversationState, idx: usize) -> Option<usize> {
    convo.messages()[..idx]
        .iter()
        .rposition(|m| m.role == Role::User)
}

/// What the stop button appends to a partial reply: in the stored message
/// body, so it persists across restarts for free. One stop line everywhere —
/// Work and Code say "(stopped)", a reply stopped before any text says
/// "(stopped before any text)".
pub(crate) const STOPPED_SUFFIX: &str = "\n\n*(stopped)*";

/// RC2-UX: the Work reply that a Stop ended before any text.
pub(crate) const STOPPED_EMPTY: &str = "*(stopped before any text)*";
/// RC2-UX: the Work reply that failed before any text.
pub(crate) const FAILED_EMPTY: &str = "*(failed before any text)*";
/// RC2-UX: the Work reply that ended with no text.
pub(crate) const NO_TEXT: &str = "*(no text)*";

/// The composer-bar action button's label: ⏹ Stop while a turn runs in the
/// active conversation, Send otherwise.
pub(crate) fn send_button_label(streaming: bool) -> &'static str {
    if streaming {
        "⏹ Stop"
    } else {
        "Send"
    }
}

/// Strip a trailing `[answered by <model>]` provenance tag (slice 15 §2.4)
/// from an assistant message before it is replayed to a provider (codex
/// slice-15 SF4): the tag is display/storage metadata, not something the
/// assistant said. Only the trailing tag line is stripped — a tag the model
/// itself wrote mid-reply is left alone. Stored text and the bubble keep it.
pub(crate) fn strip_answered_by_tag(text: &str) -> &str {
    if let Some(pos) = text.rfind("\n\n[answered by ") {
        let tag = &text[pos + 2..];
        if tag.ends_with(']') && !tag.contains('\n') {
            return &text[..pos];
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_core::{ConversationState, Role};

    /// Helper: seed a ConvView from (Role, &str) pairs, same as the
    /// `view_of` helper in main.rs tests.
    fn view_of(seed: &[(Role, &str)]) -> ConvView {
        ConvView::from_state(ConversationState::seeded(
            seed.iter().map(|(r, t)| (*r, t.to_string())),
        ))
    }

    #[test]
    fn discard_summary_realigns_rows_and_error_indices() {
        // Slice 21 §D4 "Discard summary": dropping the summary bubble must keep
        // `rows` index-aligned with `convo` and slide the error-notice indices
        // (which sit at LATER positions) down by the one removed row.
        let mut v = view_of(&[
            (Role::User, "q0"),      // ts 0
            (Role::Assistant, "a0"), // ts 1 — will be superseded
            (Role::User, "q1"),      // ts 2 — stays live
            (Role::Assistant, "a1"), // ts 3 — an errored bubble, after the summary
        ]);
        // Append the summary bubble (ts 4) at index 4, keeping rows aligned, and
        // mark the oldest turns superseded — the state the summarize action leaves.
        let summary_ts = v
            .convo
            .push_kind(Role::Assistant, "SUMMARY", MessageKind::Summary);
        v.rows.push(MessageRow::new(v.convo.message(4).unwrap()));
        assert!(v.convo.set_superseded(0, summary_ts));
        assert!(v.convo.set_superseded(1, summary_ts));
        // An error notice on a row BEFORE the removed summary (index 4) keeps its
        // index; the later sibling test covers the shift-down case.
        v.errored.insert(3);

        let restored = v.discard_summary(summary_ts).expect("summary discarded");
        assert_eq!(restored, vec![0, 1]);
        // rows and convo stay aligned; the summary row is gone.
        assert_eq!(v.rows.len(), v.convo.len());
        assert_eq!(v.convo.len(), 4);
        assert!(v.convo.messages().iter().all(|m| m.superseded_by.is_none()));
        assert!(v
            .convo
            .messages()
            .iter()
            .all(|m| m.kind == MessageKind::Normal));
        // The errored index (3) was before the removed summary (index 4), so it
        // is unchanged; it must still be present and < the new length.
        assert!(v.errored.contains(&3));
        assert!(v.errored.iter().all(|&i| i < v.rows.len()));
    }

    #[test]
    fn discard_summary_shifts_error_index_past_the_removed_row() {
        // The summary is NOT last: a later errored row's index must shift down.
        let mut v = view_of(&[(Role::User, "q0"), (Role::Assistant, "a0")]); // ts 0,1
        let summary_ts = v
            .convo
            .push_kind(Role::Assistant, "SUMMARY", MessageKind::Summary); // ts 2, idx 2
        v.rows.push(MessageRow::new(v.convo.message(2).unwrap()));
        let _later = v.convo.push(Role::User, "q1"); // ts 3, idx 3 — AFTER the summary
        v.rows.push(MessageRow::new(v.convo.message(3).unwrap()));
        assert!(v.convo.set_superseded(0, summary_ts));
        v.errored.insert(3); // the later turn carries an error notice

        v.discard_summary(summary_ts).expect("discarded");
        // Removing index 2 slides the errored row from 3 to 2.
        assert!(
            v.errored.contains(&2),
            "error index shifted down past the gap"
        );
        assert!(!v.errored.contains(&3));
        assert_eq!(v.rows.len(), 3);
    }

    #[test]
    fn discard_summary_non_summary_ts_is_none() {
        let mut v = view_of(&[(Role::User, "q0"), (Role::Assistant, "a0")]);
        // ts 0 is a normal turn; ts 99 does not exist.
        assert!(v.discard_summary(0).is_none());
        assert!(v.discard_summary(99).is_none());
        assert_eq!(v.rows.len(), 2, "nothing removed");
    }

    #[test]
    fn strip_answered_by_tag_only_strips_a_well_formed_trailing_tag() {
        // slice 15 §2.4 / codex slice-15 SF4: the `[answered by <model>]`
        // provenance tag is display/storage metadata and must be peeled off
        // before an assistant turn is replayed to a provider — but ONLY when it
        // is a genuine single-line tag pinned to the very end of the message.
        // Two guards keep model prose that merely resembles the marker intact;
        // each is exercised in isolation below so neither can silently drop.

        // Positive: a clean trailing tag is stripped, leaving the reply body.
        assert_eq!(
            strip_answered_by_tag("real answer\n\n[answered by claude-opus]"),
            "real answer",
        );

        // `ends_with(']')` guard: the marker is present but the message does not
        // END at the tag's close bracket, so it is the model's own prose, not a
        // provenance stamp — nothing is stripped. (Isolates the ends_with arm:
        // dropping it would strip this to "real answer", losing " and more".)
        assert_eq!(
            strip_answered_by_tag("real answer\n\n[answered by X] and more"),
            "real answer\n\n[answered by X] and more",
        );

        // `!tag.contains('\n')` guard: the candidate tag spans multiple lines, so
        // it is body text that happens to open with the marker and close with a
        // bracket, not a one-line trailing tag — left alone. (Isolates the
        // no-newline arm: dropping it would collapse this to "real answer".)
        assert_eq!(
            strip_answered_by_tag("real answer\n\n[answered by X]\nmore]"),
            "real answer\n\n[answered by X]\nmore]",
        );
    }

    /// RC2-UX (D-PUNCH S2-F16 stop wording): one stop line — "(stopped)"
    /// after a partial reply (Chat, Code), "(stopped before any text)" for a
    /// Work reply with none — and the in-app history still leaves the notes
    /// out, old words and new.
    #[test]
    fn rc2ux_one_stop_line() {
        assert_eq!(STOPPED_SUFFIX, "\n\n*(stopped)*");
        assert_eq!(STOPPED_EMPTY, "*(stopped before any text)*");
        for note in [
            STOPPED_EMPTY,
            FAILED_EMPTY,
            NO_TEXT,
            "*(turn stopped \u{2014} no text returned)*",
        ] {
            assert_eq!(crate::work::inapp_history::model_text(note), "", "{note}");
        }
    }
}
