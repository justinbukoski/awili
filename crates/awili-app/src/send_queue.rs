//! RC3-S5: messages typed while a Work/Chat turn runs.
//!
//! Enter on a busy composer used to be refused (the draft stayed in the box).
//! Now it joins a visible queue under the composer. The queue belongs to its
//! conversation: it drains in order, one turn at a time, into the
//! conversation it was typed in, and a failed or stopped turn pauses it
//! instead of firing the rest. This module is the pure state; the app glue
//! (enqueue, drain, verbs, rendering) is in `send_queue_app.rs`.
use crate::work::WorkExecutor;
use awili_work::ApprovalMode;
use std::collections::HashMap;
use std::path::PathBuf;

/// Most messages one conversation may hold; past this Enter keeps the draft.
pub(crate) const MAX_QUEUED: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueState {
    Queued,
    Sent,
    Cancelled,
}

impl QueueState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            QueueState::Queued => "Queued",
            QueueState::Sent => "Sent",
            QueueState::Cancelled => "Cancelled",
        }
    }
}

/// What the owner had chosen when the message was queued. The message is sent
/// with these, not with whatever the composer shows when its turn comes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueueSnapshot {
    /// The text as sent: the typed message with its attached files in front.
    pub(crate) body: String,
    pub(crate) target: WorkExecutor,
    pub(crate) folder: Option<PathBuf>,
    pub(crate) mode: ApprovalMode,
    /// Files taken off the tray for this message (put back by Edit).
    pub(crate) attachment_ids: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueueItem {
    pub(crate) id: u64,
    /// What the owner typed (shown in the list, put back by Edit).
    pub(crate) text: String,
    pub(crate) snap: QueueSnapshot,
    pub(crate) state: QueueState,
}

/// Why a queue stopped draining.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PauseReason {
    Failed,
    Stopped,
    /// The next message could not start; the sentence says why.
    Refused(String),
}

impl PauseReason {
    pub(crate) fn label(&self) -> String {
        match self {
            PauseReason::Failed => "Paused — the last turn failed.".to_string(),
            PauseReason::Stopped => "Paused — the turn was stopped.".to_string(),
            PauseReason::Refused(why) => format!("Paused — {why}"),
        }
    }
}

#[derive(Default, Debug)]
pub(crate) struct ConvQueue {
    items: Vec<QueueItem>,
    paused: Option<PauseReason>,
}

#[derive(Default, Debug)]
pub(crate) struct SendQueues {
    next_id: u64,
    by_conv: HashMap<String, ConvQueue>,
    /// The conversation whose turn was last seen running; its end is the
    /// moment the queue drains (or pauses).
    pub(crate) watching: Option<String>,
    /// "Send now" is stopping this conversation's turn: that Stop drains.
    pub(crate) send_now_for: Option<String>,
}

impl SendQueues {
    /// Add `text` to `conv`'s queue; `None` when the queue is full.
    pub(crate) fn enqueue(&mut self, conv: &str, text: String, snap: QueueSnapshot) -> Option<u64> {
        let q = self.by_conv.entry(conv.to_string()).or_default();
        if q.items
            .iter()
            .filter(|i| i.state == QueueState::Queued)
            .count()
            >= MAX_QUEUED
        {
            return None;
        }
        // Finished rows only stay for the turn they belong to.
        if !q.items.iter().any(|i| i.state == QueueState::Queued) {
            q.items.clear();
        }
        self.next_id += 1;
        q.items.push(QueueItem {
            id: self.next_id,
            text,
            snap,
            state: QueueState::Queued,
        });
        Some(self.next_id)
    }

    pub(crate) fn items(&self, conv: &str) -> &[QueueItem] {
        self.by_conv.get(conv).map_or(&[], |q| q.items.as_slice())
    }

    pub(crate) fn queued_count(&self, conv: &str) -> usize {
        self.items(conv)
            .iter()
            .filter(|i| i.state == QueueState::Queued)
            .count()
    }

    pub(crate) fn paused(&self, conv: &str) -> Option<&PauseReason> {
        self.by_conv.get(conv).and_then(|q| q.paused.as_ref())
    }

    pub(crate) fn pause(&mut self, conv: &str, why: PauseReason) {
        if let Some(q) = self.by_conv.get_mut(conv) {
            q.paused = Some(why);
        }
    }

    pub(crate) fn resume(&mut self, conv: &str) {
        if let Some(q) = self.by_conv.get_mut(conv) {
            q.paused = None;
        }
    }

    /// Mark a still-queued item cancelled.
    pub(crate) fn cancel(&mut self, conv: &str, id: u64) -> bool {
        self.queued_mut(conv, id).is_some_and(|item| {
            item.state = QueueState::Cancelled;
            true
        })
    }

    /// Remove a still-queued item and hand back its text (Edit).
    pub(crate) fn take_for_edit(&mut self, conv: &str, id: u64) -> Option<QueueItem> {
        let q = self.by_conv.get_mut(conv)?;
        let at = q
            .items
            .iter()
            .position(|i| i.id == id && i.state == QueueState::Queued)?;
        Some(q.items.remove(at))
    }

    /// Move a still-queued item to the front, un-pause, and remember that the
    /// Stop that follows is meant to send it.
    pub(crate) fn send_now(&mut self, conv: &str, id: u64) -> bool {
        let Some(q) = self.by_conv.get_mut(conv) else {
            return false;
        };
        let Some(at) = q
            .items
            .iter()
            .position(|i| i.id == id && i.state == QueueState::Queued)
        else {
            return false;
        };
        let item = q.items.remove(at);
        let first_queued = q
            .items
            .iter()
            .position(|i| i.state == QueueState::Queued)
            .unwrap_or(q.items.len());
        q.items.insert(first_queued, item);
        q.paused = None;
        self.send_now_for = Some(conv.to_string());
        true
    }

    /// The next message to send, in arrival order.
    pub(crate) fn next_queued(&self, conv: &str) -> Option<QueueItem> {
        self.items(conv)
            .iter()
            .find(|i| i.state == QueueState::Queued)
            .cloned()
    }

    pub(crate) fn mark_sent(&mut self, conv: &str, id: u64) {
        if let Some(item) = self
            .by_conv
            .get_mut(conv)
            .and_then(|q| q.items.iter_mut().find(|i| i.id == id))
        {
            item.state = QueueState::Sent;
        }
    }

    /// Re-take the choices of every still-queued message (after the owner
    /// resolved a pause about them).
    pub(crate) fn retarget(
        &mut self,
        conv: &str,
        target: &WorkExecutor,
        folder: &Option<PathBuf>,
        mode: ApprovalMode,
    ) {
        if let Some(q) = self.by_conv.get_mut(conv) {
            for item in q.items.iter_mut().filter(|i| i.state == QueueState::Queued) {
                item.snap.target = target.clone();
                item.snap.folder = folder.clone();
                item.snap.mode = mode;
            }
        }
    }

    /// Drop the rows that are done (Sent, Cancelled) once their turn ended.
    pub(crate) fn prune_finished(&mut self, conv: &str) {
        if let Some(q) = self.by_conv.get_mut(conv) {
            q.items.retain(|i| i.state == QueueState::Queued);
            if q.items.is_empty() {
                self.by_conv.remove(conv);
            }
        }
    }

    /// The conversation is gone: its queue goes with it.
    pub(crate) fn forget(&mut self, conv: &str) {
        self.by_conv.remove(conv);
        if self.watching.as_deref() == Some(conv) {
            self.watching = None;
        }
        if self.send_now_for.as_deref() == Some(conv) {
            self.send_now_for = None;
        }
    }

    fn queued_mut(&mut self, conv: &str, id: u64) -> Option<&mut QueueItem> {
        self.by_conv
            .get_mut(conv)?
            .items
            .iter_mut()
            .find(|i| i.id == id && i.state == QueueState::Queued)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(text: &str) -> QueueSnapshot {
        QueueSnapshot {
            body: text.to_string(),
            target: WorkExecutor::InApp,
            folder: None,
            mode: ApprovalMode::default(),
            attachment_ids: Vec::new(),
        }
    }

    fn texts(q: &SendQueues, conv: &str) -> Vec<(String, QueueState)> {
        q.items(conv)
            .iter()
            .map(|i| (i.text.clone(), i.state))
            .collect()
    }

    #[test]
    fn s5_drains_in_arrival_order() {
        let mut q = SendQueues::default();
        q.enqueue("a", "one".into(), snap("x"));
        q.enqueue("a", "two".into(), snap("x"));
        let first = q.next_queued("a").unwrap();
        let id = first.id;
        assert_eq!(first.text, "one");
        q.mark_sent("a", id);
        assert_eq!(q.next_queued("a").unwrap().text, "two");
    }

    #[test]
    fn s5_queues_are_per_conversation() {
        let mut q = SendQueues::default();
        q.enqueue("a", "for a".into(), snap("x"));
        assert!(q.next_queued("b").is_none());
        assert_eq!(q.queued_count("a"), 1);
        q.forget("a");
        assert!(q.items("a").is_empty());
    }

    #[test]
    fn s5_cancel_and_edit_only_touch_queued_items() {
        let mut q = SendQueues::default();
        let a = q.enqueue("c", "a".into(), snap("x")).unwrap();
        let b = q.enqueue("c", "b".into(), snap("x")).unwrap();
        assert!(q.cancel("c", a));
        assert!(!q.cancel("c", a), "already cancelled");
        assert_eq!(
            q.take_for_edit("c", b).map(|i| i.text).as_deref(),
            Some("b")
        );
        assert!(q.take_for_edit("c", b).is_none());
        assert_eq!(texts(&q, "c"), vec![("a".into(), QueueState::Cancelled)]);
    }

    #[test]
    fn s5_send_now_goes_first_and_clears_the_pause() {
        let mut q = SendQueues::default();
        q.enqueue("c", "one".into(), snap("x"));
        let two = q.enqueue("c", "two".into(), snap("x")).unwrap();
        q.pause("c", PauseReason::Failed);
        assert!(q.send_now("c", two));
        assert!(q.paused("c").is_none());
        assert_eq!(q.next_queued("c").unwrap().text, "two");
        assert_eq!(q.send_now_for.as_deref(), Some("c"));
    }

    #[test]
    fn s5_finished_rows_leave_with_their_turn() {
        let mut q = SendQueues::default();
        let a = q.enqueue("c", "a".into(), snap("x")).unwrap();
        q.enqueue("c", "b".into(), snap("x"));
        q.mark_sent("c", a);
        q.prune_finished("c");
        assert_eq!(texts(&q, "c"), vec![("b".into(), QueueState::Queued)]);
    }

    #[test]
    fn s5_queue_is_capped() {
        let mut q = SendQueues::default();
        for n in 0..MAX_QUEUED {
            assert!(q.enqueue("c", format!("{n}"), snap("x")).is_some());
        }
        assert!(q.enqueue("c", "one too many".into(), snap("x")).is_none());
    }
}
