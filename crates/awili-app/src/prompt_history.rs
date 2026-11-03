//! Up-arrow prompt history (M2g / F12).
//!
//! A session-wide, pure walk state for the main composer: sent prompts
//! (all modes, capped at 50, consecutive duplicates collapsed) plus the
//! active conversation's user messages as a tail. The `AwiliApp` owns one
//! value; the composer only receives the recalled string via
//! [`crate::Composer::set_recalled`].
//!
//! The conversation part is re-read on every `prev`/`next` step, so a
//! relaunch that restores the convo from disk is covered without a
//! separate persistence layer.

use awili_core::{ConversationState, Role};

/// RC2-UX (R2-14): what a Retry row shows in the transcript — the owner's
/// question sent again, said once, not drawn a second time.
pub(crate) const RETRY_NOTE: &str = "\u{21BB} You asked again.";

/// RC2-UX (R2-14): the test lookup key of a Retry note.
pub(crate) const RETRY_NOTE_SELECTOR: &str = "rc2ux-retry-note";

/// Maximum number of sent prompts kept in the session walk.
const SENT_CAP: usize = 50;

/// Pure walk state for Up/Down prompt recall.
///
/// `sent` is oldest→newest, capped at [`SENT_CAP`], consecutive duplicates
/// collapsed. `cursor` is an index into the newest-first candidate list
/// returned by [`Self::candidates`]; `None` means "not walking". `list`
/// is the candidate list captured at the most recent `prev` call, so
/// `next` can step forward without re-deriving the conversation tail.
#[derive(Debug, Default)]
pub(crate) struct PromptHistory {
    sent: Vec<String>,
    cursor: Option<usize>,
    list: Vec<String>,
}

impl PromptHistory {
    /// A fresh, empty walk state.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record a sent prompt. Trims; ignores blank input; ends any walk
    /// (the user typed a fresh prompt, so the recalled state is stale).
    pub(crate) fn record(&mut self, text: &str) {
        let t = text.trim().to_string();
        if t.is_empty() {
            // Still end the walk: the user pressed Enter on a blank draft.
            self.cursor = None;
            self.list.clear();
            return;
        }
        if self.sent.last().map(|s| s.as_str()) != Some(t.as_str()) {
            self.sent.push(t);
        }
        if self.sent.len() > SENT_CAP {
            self.sent.remove(0);
        }
        self.cursor = None;
        self.list.clear();
    }

    /// Newest-first, de-duplicated candidate list: sent (newest first)
    /// then `conversation` (newest first), skipping blanks and consecutive
    /// duplicates. The conversation tail is re-derived on every call so a
    /// restored convo is picked up without extra bookkeeping.
    pub(crate) fn candidates(&self, conversation: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut last: Option<&str> = None;
        // Sent, newest first.
        for s in self.sent.iter().rev() {
            if s.is_empty() {
                continue;
            }
            if last != Some(s.as_str()) {
                out.push(s.clone());
                last = Some(s.as_str());
            }
        }
        // Conversation user messages, newest first.
        for c in conversation.iter().rev() {
            if c.trim().is_empty() {
                continue;
            }
            if last == Some(c.as_str()) {
                continue;
            }
            out.push(c.clone());
            last = Some(c.as_str());
        }
        out
    }

    /// Start the walk (cursor `None` → 0 over `candidates(conversation)`)
    /// or step older. `None` when there is nothing older to recall.
    pub(crate) fn prev(&mut self, conversation: &[String]) -> Option<String> {
        let list = self.candidates(conversation);
        if self.cursor.is_none() {
            if list.is_empty() {
                return None;
            }
            self.list = list;
            self.cursor = Some(0);
            return Some(self.list[0].clone());
        }
        let cur = self.cursor?;
        if cur + 1 < self.list.len() {
            self.cursor = Some(cur + 1);
            Some(self.list[cur + 1].clone())
        } else {
            None
        }
    }

    /// Step newer. Past the newest entry returns `Some("")` and ends the
    /// walk (the draft clears back to empty). `None` when not walking.
    pub(crate) fn next(&mut self) -> Option<String> {
        let cur = self.cursor?;
        if cur == 0 {
            self.cursor = None;
            self.list.clear();
            Some(String::new())
        } else {
            self.cursor = Some(cur - 1);
            Some(self.list[cur - 1].clone())
        }
    }

    /// End the walk and forget the candidate snapshot. Called when the
    /// user edits the draft, the mode changes, or the convo changes.
    pub(crate) fn reset(&mut self) {
        self.cursor = None;
        self.list.clear();
    }

    /// `true` while a walk is in progress (the draft currently equals a
    /// recalled entry and Up/Down should keep recalling instead of moving the
    /// caret).
    #[allow(dead_code)]
    pub(crate) fn is_walking(&self) -> bool {
        self.cursor.is_some()
    }
}

/// The text of the user row preceding the last row, when the last row is
/// an assistant row. `None` for an empty convo, a convo whose last row is
/// a user row, or a convo where the preceding row is not a user row.
pub(crate) fn retry_text(convo: &ConversationState) -> Option<String> {
    let msgs = convo.messages();
    if msgs.len() < 2 {
        return None;
    }
    let last = &msgs[msgs.len() - 1];
    if last.role != Role::Assistant {
        return None;
    }
    let prev = &msgs[msgs.len() - 2];
    if prev.role != Role::User {
        return None;
    }
    let t = prev.text.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convo_with(pairs: &[(Role, &str)]) -> ConversationState {
        let mut c = ConversationState::new();
        for (r, t) in pairs {
            c.push(*r, t.to_string());
        }
        c
    }

    /// M2g: the walk goes through `sent` (newest first) then the
    /// conversation's user messages, de-duplicated. Spec scenario: record
    /// "a", "b", then `prev(&["c", "a"])` gives "b", "a", "c", None; `next`
    /// gives "a", "b", `Some("")`, None.
    #[test]
    fn m2g_history_walks_sent_then_conversation() {
        let mut h = PromptHistory::new();
        h.record("a");
        h.record("b");
        let conv: Vec<String> = vec!["c".to_string(), "a".to_string()];
        // Sent (newest first): b, a. Conversation tail (newest first):
        // a, c. The second `a` is a consecutive dup of the first → skipped.
        // Walk: b, a, c.
        assert_eq!(
            h.candidates(&conv),
            vec!["b".to_string(), "a".to_string(), "c".to_string()]
        );
        assert_eq!(h.prev(&conv).as_deref(), Some("b"));
        assert_eq!(h.prev(&conv).as_deref(), Some("a"));
        assert_eq!(h.prev(&conv).as_deref(), Some("c"));
        assert_eq!(h.prev(&conv), None, "nothing older");
        // `next` steps back to the newest entry, then past it returns
        // `Some("")` and ends the walk.
        assert_eq!(h.next().as_deref(), Some("a"));
        assert_eq!(h.next().as_deref(), Some("b"));
        assert_eq!(h.next().as_deref(), Some(""));
        assert_eq!(h.next(), None, "walk ended");
    }

    /// M2g: `sent` is capped at 50 and consecutive duplicates collapse.
    #[test]
    fn m2g_history_caps_and_collapses() {
        let mut h = PromptHistory::new();
        for i in 0..60 {
            h.record(&format!("p{i}"));
        }
        assert_eq!(h.sent_len_for_test(), 50);
        // p0..p59: 60 records. Cap 50 → the 10 oldest are evicted.
        assert_eq!(h.sent_first_for_test().as_deref(), Some("p10"));
        assert_eq!(h.sent_last_for_test().as_deref(), Some("p59"));
        // Consecutive duplicates collapse: record the current last entry
        // again → no change (the list stays capped at 50, no evict).
        h.record("p59");
        assert_eq!(h.sent_len_for_test(), 50);
        assert_eq!(h.sent_first_for_test().as_deref(), Some("p10"));
        h.record("same");
        h.record("same");
        assert_eq!(h.sent_len_for_test(), 50);
        assert_eq!(h.sent_last_for_test().as_deref(), Some("same"));
        assert_eq!(h.sent_first_for_test().as_deref(), Some("p11"));
    }

    /// M2g: `record` ends any in-progress walk.
    #[test]
    fn m2g_record_ends_walk() {
        let mut h = PromptHistory::new();
        h.record("a");
        h.record("b");
        let conv: Vec<String> = vec![];
        assert_eq!(h.prev(&conv).as_deref(), Some("b"));
        h.record("typed");
        assert!(!h.is_walking());
        // A fresh `prev` restarts at the newest entry.
        assert_eq!(h.prev(&conv).as_deref(), Some("typed"));
    }

    /// M2g: `retry_text` needs a user row immediately before an assistant
    /// row. Empty, single-row, or user-last convos return `None`.
    #[test]
    fn m2g_retry_text_needs_user_then_assistant() {
        let empty = ConversationState::new();
        assert_eq!(retry_text(&empty), None);
        let user_only = convo_with(&[(Role::User, "hi")]);
        assert_eq!(retry_text(&user_only), None);
        let user_then_user = convo_with(&[(Role::User, "hi"), (Role::User, "more")]);
        assert_eq!(retry_text(&user_then_user), None);
        let user_then_assistant = convo_with(&[(Role::User, "hi"), (Role::Assistant, "hello")]);
        assert_eq!(retry_text(&user_then_assistant).as_deref(), Some("hi"));
        let three = convo_with(&[
            (Role::User, "hi"),
            (Role::Assistant, "hello"),
            (Role::User, "more"),
        ]);
        assert_eq!(retry_text(&three), None);
        // Trimming: a user row with surrounding whitespace is returned trimmed.
        let padded = convo_with(&[(Role::User, "  hi  "), (Role::Assistant, "x")]);
        assert_eq!(retry_text(&padded).as_deref(), Some("hi"));
    }

    // Test-only accessors so the cap test doesn't need a `pub` field.
    impl PromptHistory {
        fn sent_len_for_test(&self) -> usize {
            self.sent.len()
        }
        fn sent_first_for_test(&self) -> Option<String> {
            self.sent.first().cloned()
        }
        fn sent_last_for_test(&self) -> Option<String> {
            self.sent.last().cloned()
        }
    }
}
