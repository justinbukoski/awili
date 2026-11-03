//! Inserting dictation into the composer through the EXISTING text-input path.
//!
//! Slice-3 spec §2 is emphatic: streaming partial transcripts render *ghost-style*
//! in the composer, and committed text goes in through the [`EditBuffer`] so
//! editing-by-typing still works mid-dictation. Dictation is modelled like an IME
//! composition — a marked span replaced in place as whisper refines it — but the
//! ghost range lives HERE and only here (review slice-3 blocker 2: the composer
//! used to mirror it into its IME `marked` field, and any platform edit that
//! cleared/moved `marked` desynchronized the two, letting the next partial delete
//! user-typed text).
//!
//! The composer therefore routes **every non-dictation text mutation** — typed
//! commits, IME transitions, paste, key edits — through
//! [`external_replace`](Dictation::external_replace), which keeps the ghost
//! consistent by construction: an edit *overlapping* the ghost wipes the preview
//! (it is only a preview; the final transcript still commits at the caret), and a
//! disjoint edit rebases the ghost range by the edit's length delta. Navigation
//! needs nothing: the ghost is anchored to its own range, not the caret.
//!
//! This module never pokes the buffer's private state. It drives the public
//! [`EditBuffer`] API (`replace_char_range`, `set_caret`) — the identical calls
//! the composer's IME protocol uses — which is the whole point of the "no direct
//! buffer pokes" test the spec calls for.

use std::ops::Range;

use awili_core::EditBuffer;

/// A live dictation session over a composer buffer. Tracks the char range of the
/// current ghost (partial) transcript so successive partials replace it in place,
/// and a commit can drop the mark leaving ordinary editable text behind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dictation {
    /// Char range of the ghost span in the buffer, mirrored into the composer's
    /// `marked` field so it renders as a preedit. `None` between utterances.
    ghost: Option<Range<usize>>,
}

impl Dictation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a ghost (partial) transcript is currently shown.
    pub fn is_active(&self) -> bool {
        self.ghost.is_some()
    }

    /// The current ghost span, if any (the composer mirrors this into `marked`).
    pub fn ghost(&self) -> Option<Range<usize>> {
        self.ghost.clone()
    }

    /// Show/replace the partial transcript as ghost text at the caret. The first
    /// partial of an utterance inserts at the caret; later partials replace the
    /// previous ghost span in place, so the preedit grows/shrinks as whisper
    /// refines it. The caret is left at the end of the ghost (where committed text
    /// will land), so a user can still arrow away and type elsewhere.
    pub fn set_partial(&mut self, buffer: &mut EditBuffer, text: &str) {
        let start = match &self.ghost {
            Some(g) => g.start.min(buffer.char_len()),
            None => buffer.caret(),
        };
        let end = self
            .ghost
            .as_ref()
            .map(|g| g.end.min(buffer.char_len()))
            .unwrap_or(start);
        buffer.replace_char_range(start..end, text);
        let n = text.chars().count();
        self.ghost = if n > 0 { Some(start..start + n) } else { None };
    }

    /// Commit the final transcript: replace the ghost span (or insert at the caret
    /// when no ghost is showing) with `text` as ordinary, no-longer-marked text,
    /// followed by a single trailing space so consecutive utterances don't run
    /// together. Clears the ghost. Returns the committed text's char range in the
    /// buffer (handy for tests / cursor logic).
    pub fn commit(&mut self, buffer: &mut EditBuffer, text: &str) -> Range<usize> {
        let start = match &self.ghost {
            Some(g) => g.start.min(buffer.char_len()),
            None => buffer.caret(),
        };
        let end = self
            .ghost
            .as_ref()
            .map(|g| g.end.min(buffer.char_len()))
            .unwrap_or(start);
        let committed = format!("{} ", text.trim());
        buffer.replace_char_range(start..end, &committed);
        self.ghost = None;
        start..start + committed.chars().count()
    }

    /// Abandon the in-progress utterance, removing any ghost text from the buffer
    /// (e.g. the user released push-to-talk on silence, or pressed Escape).
    pub fn cancel(&mut self, buffer: &mut EditBuffer) {
        if let Some(g) = self.ghost.take() {
            let g = g.start.min(buffer.char_len())..g.end.min(buffer.char_len());
            buffer.replace_char_range(g, "");
        }
    }

    /// Remove the ghost preview (if any) and remap `range` — char indices given
    /// against the *pre-removal* text — onto the post-removal text. Used when an
    /// IME composition begins mid-dictation: the preedit takes over and the
    /// preview must not linger as a second phantom span (review blocker 2).
    pub fn cancel_remap(&mut self, buffer: &mut EditBuffer, range: Range<usize>) -> Range<usize> {
        let Some(g) = self.ghost.take() else {
            return range;
        };
        let g = g.start.min(buffer.char_len())..g.end.min(buffer.char_len());
        buffer.replace_char_range(g.clone(), "");
        map_after_removal(range.start, &g)..map_after_removal(range.end, &g)
    }

    /// Apply a text edit that did **not** come from dictation — a typed/IME
    /// commit, a paste, a key edit — replacing char `range` with `text`, while
    /// keeping the ghost consistent (review blocker 2):
    ///
    /// * an edit **overlapping** the ghost first wipes the preview (it is only a
    ///   preview — the final transcript still commits at the caret later), then
    ///   applies the edit at the remapped range;
    /// * a **disjoint** edit applies as-is, and a ghost *after* the edit is
    ///   shifted by the edit's length delta so subsequent partials keep replacing
    ///   the right span.
    ///
    /// Returns the char range the edit was actually applied at (after any
    /// remapping), so callers (e.g. the IME mark path) can place their own spans.
    pub fn external_replace(
        &mut self,
        buffer: &mut EditBuffer,
        range: Range<usize>,
        text: &str,
    ) -> Range<usize> {
        let overlaps = self
            .ghost
            .as_ref()
            .map(|g| range.start < g.end && range.end > g.start)
            .unwrap_or(false);
        let range = if overlaps {
            self.cancel_remap(buffer, range)
        } else {
            range
        };
        buffer.replace_char_range(range.clone(), text);
        if let Some(g) = &mut self.ghost {
            // Disjoint by construction here. An edit entirely before the ghost
            // shifts it; one at/after the ghost end leaves it untouched.
            if range.end <= g.start {
                let removed = range.end - range.start;
                let inserted = text.chars().count();
                g.start = g.start - removed + inserted;
                g.end = g.end - removed + inserted;
            }
        }
        range
    }
}

/// Map a char position through the removal of range `g`: positions before the
/// removed span are unchanged, ones after slide left by its length, ones inside
/// collapse to its start.
fn map_after_removal(p: usize, g: &Range<usize>) -> usize {
    if p <= g.start {
        p
    } else if p >= g.end {
        p - (g.end - g.start)
    } else {
        g.start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_partial_inserts_ghost_at_caret() {
        let mut buf = EditBuffer::new();
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "hello");
        assert_eq!(buf.text(), "hello");
        assert_eq!(d.ghost(), Some(0..5));
        assert!(d.is_active());
        // Caret sits at the end of the ghost (where a commit will land).
        assert_eq!(buf.caret(), 5);
    }

    #[test]
    fn successive_partials_replace_in_place() {
        let mut buf = EditBuffer::new();
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "hel");
        d.set_partial(&mut buf, "hello wo");
        d.set_partial(&mut buf, "hello world");
        // Only the latest partial is present — earlier ghosts were replaced, never
        // accumulated.
        assert_eq!(buf.text(), "hello world");
        assert_eq!(d.ghost(), Some(0..11));
    }

    #[test]
    fn commit_drops_mark_and_appends_trailing_space() {
        let mut buf = EditBuffer::new();
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "hello world");
        let range = d.commit(&mut buf, "hello world");
        assert_eq!(buf.text(), "hello world ");
        assert_eq!(range, 0..12);
        assert!(!d.is_active());
        assert_eq!(d.ghost(), None);
    }

    #[test]
    fn dictation_preserves_text_typed_before_it() {
        // Pre-existing typed text must survive: the ghost inserts at the caret, not
        // over the document. This is the "editing-by-typing still works" guarantee.
        let mut buf = EditBuffer::from_text("Note: ");
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "buy milk");
        assert_eq!(buf.text(), "Note: buy milk");
        d.commit(&mut buf, "buy milk");
        assert_eq!(buf.text(), "Note: buy milk ");
    }

    #[test]
    fn ghost_in_the_middle_of_existing_text() {
        // Caret placed mid-buffer; dictation inserts there and commits there,
        // leaving the trailing text intact (proves we go through replace_char_range
        // at the caret, not append).
        let mut buf = EditBuffer::from_text("a c");
        buf.set_caret(2); // between "a " and "c"
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "b");
        assert_eq!(buf.text(), "a bc");
        d.commit(&mut buf, "b");
        assert_eq!(buf.text(), "a b c");
    }

    #[test]
    fn cancel_removes_ghost_only() {
        let mut buf = EditBuffer::from_text("keep ");
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "draft words");
        assert_eq!(buf.text(), "keep draft words");
        d.cancel(&mut buf);
        assert_eq!(buf.text(), "keep ");
        assert!(!d.is_active());
    }

    #[test]
    fn commit_without_partial_inserts_at_caret() {
        // A short utterance may produce a final transcript with no intermediate
        // partial (push-to-talk released quickly). Commit still inserts at caret.
        let mut buf = EditBuffer::from_text("x");
        let mut d = Dictation::new();
        d.commit(&mut buf, "hi");
        assert_eq!(buf.text(), "xhi ");
    }

    #[test]
    fn empty_partial_clears_ghost() {
        let mut buf = EditBuffer::from_text("pre");
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "draft");
        d.set_partial(&mut buf, ""); // whisper produced nothing this frame
        assert_eq!(buf.text(), "pre");
        assert!(!d.is_active());
    }

    // ---- blocker 2: external edits while a ghost is live ----------------------

    #[test]
    fn typing_at_caret_after_ghost_survives_the_next_partial() {
        // The user types while dictating, at the caret (which sits at the ghost
        // end). The typed char must land after the preview, survive the next
        // partial AND the commit — the spec's "editing-by-typing still works
        // mid-dictation".
        let mut buf = EditBuffer::new();
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "hello");
        let caret = buf.caret();
        d.external_replace(&mut buf, caret..caret, "!");
        assert_eq!(buf.text(), "hello!");
        assert_eq!(d.ghost(), Some(0..5)); // ghost untouched
                                           // The refined partial replaces only the ghost, never the typed char.
        d.set_partial(&mut buf, "hello world");
        assert_eq!(buf.text(), "hello world!");
        assert_eq!(d.ghost(), Some(0..11));
        d.commit(&mut buf, "hello world");
        assert_eq!(buf.text(), "hello world !");
    }

    #[test]
    fn typing_before_the_ghost_rebases_it() {
        // Caret moved before the ghost; typed text shifts the ghost right, and
        // the next partial still replaces exactly the preview span.
        let mut buf = EditBuffer::from_text("x ");
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "draft");
        assert_eq!(d.ghost(), Some(2..7));
        d.external_replace(&mut buf, 0..0, "AB"); // type at the very start
        assert_eq!(buf.text(), "ABx draft");
        assert_eq!(d.ghost(), Some(4..9));
        d.set_partial(&mut buf, "draft two");
        assert_eq!(buf.text(), "ABx draft two");
        assert_eq!(d.ghost(), Some(4..13));
    }

    #[test]
    fn deleting_before_the_ghost_rebases_it() {
        let mut buf = EditBuffer::from_text("abc ");
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "ghost");
        assert_eq!(d.ghost(), Some(4..9));
        // Backspace-style deletion of "c" (range 2..3), entirely before the ghost.
        d.external_replace(&mut buf, 2..3, "");
        assert_eq!(buf.text(), "ab ghost");
        assert_eq!(d.ghost(), Some(3..8));
        d.commit(&mut buf, "ghost");
        assert_eq!(buf.text(), "ab ghost ");
    }

    #[test]
    fn edit_overlapping_the_ghost_wipes_only_the_preview() {
        // An edit into the preview (e.g. backspace into it) removes the ghost —
        // never user text — and the edit applies at the remapped position.
        let mut buf = EditBuffer::from_text("keep ");
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "preview");
        assert_eq!(buf.text(), "keep preview");
        // Backspace at the ghost end: deletes "into" the preview.
        let caret = buf.caret();
        let applied = d.external_replace(&mut buf, caret - 1..caret, "");
        assert_eq!(buf.text(), "keep ");
        assert!(!d.is_active());
        assert_eq!(applied, 5..5); // collapsed inside the removed span
                                   // A later partial (same session still running) starts a fresh ghost at
                                   // the caret rather than resurrecting the old span.
        d.set_partial(&mut buf, "again");
        assert_eq!(buf.text(), "keep again");
    }

    #[test]
    fn paste_after_ghost_is_untouched_paste_inside_cancels_preview() {
        let mut buf = EditBuffer::new();
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "alpha");
        // Paste at the caret (ghost end): ghost intact.
        let caret = buf.caret();
        d.external_replace(&mut buf, caret..caret, " pasted");
        assert_eq!(buf.text(), "alpha pasted");
        assert_eq!(d.ghost(), Some(0..5));
        // Paste over a range that bites into the ghost: preview wiped, paste in.
        d.external_replace(&mut buf, 3..8, "PASTE");
        assert_eq!(buf.text(), "PASTEsted");
        assert!(!d.is_active());
    }

    #[test]
    fn ime_transition_cancels_preview_and_remaps() {
        // An IME composition starting mid-dictation takes over: cancel_remap
        // removes the preview and remaps the IME's target range.
        let mut buf = EditBuffer::from_text("ab ");
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "ghost");
        assert_eq!(buf.text(), "ab ghost");
        // IME wants to compose at the (post-ghost) caret 8..8.
        let r = d.cancel_remap(&mut buf, 8..8);
        assert_eq!(buf.text(), "ab ");
        assert_eq!(r, 3..3);
        assert!(!d.is_active());
    }

    #[test]
    fn external_edit_with_no_ghost_is_a_plain_replace() {
        let mut buf = EditBuffer::from_text("hello");
        let mut d = Dictation::new();
        let r = d.external_replace(&mut buf, 1..4, "ELL");
        assert_eq!(buf.text(), "hELLo");
        assert_eq!(r, 1..4);
        assert!(!d.is_active());
    }

    #[test]
    fn multibyte_ghost_is_char_indexed() {
        let mut buf = EditBuffer::from_text("日本");
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "語");
        assert_eq!(buf.text(), "日本語");
        assert_eq!(d.ghost(), Some(2..3)); // char indices, not bytes
        d.commit(&mut buf, "語");
        assert_eq!(buf.text(), "日本語 ");
    }
}
