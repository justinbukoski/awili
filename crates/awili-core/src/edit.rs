//! Caret-aware, char-indexed, UTF-8-safe text editing.
//!
//! Adapted from Hub `crates/hub-app/src/ui/peers.rs` (`apply_edit` / `byte_of_char`
//! / `line_start` / `line_end` / `vertical_move`) and the `caret_line_col` /
//! `split_at_col` render helpers from `mailui.rs`. The original Hub code threaded a
//! `&mut String` + `&mut usize` caret through free functions; here they are folded
//! into an [`EditBuffer`] struct so the caret can never drift out of sync with the
//! text and so the UI layer holds a single value.
//!
//! Invariant: `caret` is a CHARACTER index in `0..=text.chars().count()` — NEVER a
//! byte index. Char indices are mapped to byte offsets via [`byte_of_char`] before
//! any `String` mutation, so a multi-byte char (emoji, CJK) is never split.

use std::ops::Range;

/// The pure editing intent, decoupled from any key-event plumbing so the buffer
/// mutation is unit-testable directly. The UI layer (awili-app) maps GPUI
/// keystrokes onto these. `Submit` is the single-line Enter; `Newline` the
/// multiline one; `None` a recognised-but-inert key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditAction {
    Insert(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    Newline,
    Submit,
    None,
}

/// Byte offset of character `char_idx` in `s`, or `s.len()` when the index is at
/// (or past) the end. Always lands on a UTF-8 char boundary.
fn byte_of_char(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// Char index of the start of the line containing `caret` — just after the
/// previous `'\n'`, or 0. `caret` is a char index.
fn line_start(chars: &[char], caret: usize) -> usize {
    let mut i = caret.min(chars.len());
    while i > 0 && chars[i - 1] != '\n' {
        i -= 1;
    }
    i
}

/// Char index of the end of the line containing `caret` — at the next `'\n'`, or
/// the buffer length. `caret` is a char index.
fn line_end(chars: &[char], caret: usize) -> usize {
    let mut i = caret.min(chars.len());
    while i < chars.len() && chars[i] != '\n' {
        i += 1;
    }
    i
}

/// Move the caret one line up (`dir < 0`) or down, landing on `want_col` (the
/// caller's preferred column), clamped to the target line's length. Returns
/// `caret` unchanged when there is no line in that direction. All indices are
/// char indices. The preferred column — rather than the current, already-clamped
/// column — is what native editors retain across a run of Up/Down presses so the
/// caret returns to its original column after crossing a short line.
fn vertical_move(chars: &[char], caret: usize, dir: i32, want_col: usize) -> usize {
    let caret = caret.min(chars.len());
    let cur_start = line_start(chars, caret);
    if dir < 0 {
        if cur_start == 0 {
            return caret; // already on the first line
        }
        let prev_nl = cur_start - 1; // the '\n' ending the previous line
        let prev_start = line_start(chars, prev_nl);
        let prev_len = prev_nl - prev_start;
        prev_start + want_col.min(prev_len)
    } else {
        let cur_end = line_end(chars, caret);
        if cur_end >= chars.len() {
            return caret; // already on the last line
        }
        let next_start = cur_end + 1;
        let next_len = line_end(chars, next_start) - next_start;
        next_start + want_col.min(next_len)
    }
}

/// One coalescing class for the undo stack. Consecutive edits of the same
/// coalescing class fold into a single undo step (a run of typed characters is
/// one Undo, not one-per-keystroke); anything else — a caret move, a paste, a
/// switch from typing to deleting — starts a fresh step. [`UndoKind::Boundary`]
/// never coalesces, so a caller can force a break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoKind {
    /// A run of inserted text (typing, IME/paste-free character commits).
    Insert,
    /// A run of deletions (Backspace / forward-Delete).
    Delete,
    /// An edit that must stand alone in history (paste, cut, newline).
    Boundary,
}

/// A point-in-time snapshot of the editable state, for undo/redo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct UndoState {
    text: String,
    caret: usize,
    anchor: Option<usize>,
}

/// A caret-aware text buffer. The caret is a CHARACTER index into the text
/// (`0..=chars().count()`); every mutation keeps it in range. UTF-8 correct:
/// never splits a multi-byte char.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditBuffer {
    text: String,
    /// Caret position as a CHARACTER index (`0..=char_len`).
    caret: usize,
    /// The fixed end of an active selection, as a CHARACTER index. The moving
    /// end is always [`caret`]. `None`, or equal to `caret`, means no selection
    /// (a collapsed caret). Every text mutation and every explicit caret
    /// placement clears it; only the selection API (mouse drag, Select All,
    /// double-click word select, shift-click) sets it.
    anchor: Option<usize>,
    /// The column the caret "wants" to be at, retained across a run of
    /// consecutive vertical (`Up`/`Down`) moves so the caret returns to its
    /// original column after passing through a shorter line. `None` until the
    /// first vertical move of a run; any non-vertical edit/motion resets it.
    preferred_col: Option<usize>,
    /// Undo history: pre-edit snapshots, oldest first. Pushed by
    /// [`EditBuffer::checkpoint`] at user-edit boundaries (never on IME preedit
    /// or dictation churn, which don't checkpoint).
    undo_stack: Vec<UndoState>,
    /// Redo history: states popped by [`EditBuffer::undo`], cleared by any fresh
    /// checkpoint.
    redo_stack: Vec<UndoState>,
    /// The coalescing class of the last checkpointed edit, so a run of the same
    /// class folds into one undo step. Reset by caret placement / undo / redo.
    coalescing: Option<UndoKind>,
}

impl EditBuffer {
    /// An empty buffer with the caret at 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// A buffer seeded with `text`, caret pinned to the end.
    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let caret = text.chars().count();
        Self {
            text,
            caret,
            ..Default::default()
        }
    }

    /// The current text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The caret position, as a CHARACTER index.
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// Number of characters (NOT bytes) in the buffer.
    pub fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    /// `true` when the buffer holds no text.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replace the whole buffer and pin the caret to the end. Treated as a fresh
    /// document: the selection is cleared and the undo history is discarded (a
    /// prefill — rename, settings raw/field editing — is not something the user
    /// should be able to Undo back to empty).
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.caret = self.char_len();
        self.anchor = None;
        self.preferred_col = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.coalescing = None;
    }

    /// Move the caret to `char_idx` (clamped into range). Resets the retained
    /// preferred column and clears any selection — an explicit caret placement
    /// starts a fresh column and a collapsed caret. Also breaks undo coalescing
    /// so the next typed run is its own undo step (native behaviour).
    pub fn set_caret(&mut self, char_idx: usize) {
        self.caret = char_idx.min(self.char_len());
        self.anchor = None;
        self.preferred_col = None;
        self.coalescing = None;
    }

    /// Replace the CHARACTER range `range` with `s`, leaving the caret just past
    /// the inserted text. UTF-8 correct: the range ends are mapped to byte
    /// offsets on char boundaries, so a multi-byte char is never split. The range
    /// is clamped into `0..=char_len` (and `start <= end`). This is the primitive
    /// the platform text-input protocol drives: an IME commit, a dead-key result,
    /// or a marked-text replacement is `replace_char_range` over the affected
    /// span.
    pub fn replace_char_range(&mut self, range: Range<usize>, s: &str) {
        let n = self.char_len();
        let start = range.start.min(n);
        let end = range.end.min(n).max(start);
        let start_b = byte_of_char(&self.text, start);
        let end_b = byte_of_char(&self.text, end);
        self.text.replace_range(start_b..end_b, s);
        self.caret = start + s.chars().count();
        self.anchor = None;
        self.preferred_col = None;
    }

    /// Empty the buffer and reset the caret to 0. Discards the undo history —
    /// this is a programmatic reset, not an editing action.
    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
        self.anchor = None;
        self.preferred_col = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.coalescing = None;
    }

    /// Take the text out, leaving the buffer empty (caret reset to 0). Useful on
    /// send: move the draft into a message without a clone. The emptied draft is
    /// a fresh document, so the undo history goes with it.
    pub fn take(&mut self) -> String {
        self.caret = 0;
        self.anchor = None;
        self.preferred_col = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.coalescing = None;
        std::mem::take(&mut self.text)
    }

    /// Clamp the caret into `0..=char_len` (callers may have shrunk the text).
    fn clamp_caret(&mut self) {
        let n = self.char_len();
        if self.caret > n {
            self.caret = n;
        }
    }

    /// Insert a string at the caret (e.g. a clipboard paste), advancing the caret
    /// past it. UTF-8 correct and multibyte-safe.
    pub fn insert_str(&mut self, s: &str) {
        self.clamp_caret();
        let at = byte_of_char(&self.text, self.caret);
        self.text.insert_str(at, s);
        self.caret += s.chars().count();
        self.anchor = None;
        self.preferred_col = None;
    }

    /// Apply one [`EditAction`] at the caret. Returns `true` only for `Submit`
    /// (single-line Enter); every editing action returns `false`. `multiline`
    /// decides whether vertical motion and `Newline` do anything.
    pub fn apply(&mut self, action: EditAction, multiline: bool) -> bool {
        self.clamp_caret();
        // Any key action collapses an active selection: the selection API is
        // driven only by the mouse / Select All, and a motion or edit key ends
        // it (native behaviour). Selection-replacing edits are handled a layer
        // up (the composer deletes the selection first), so by the time an
        // action reaches here the selection is spent.
        self.anchor = None;
        // The preferred column survives only a run of consecutive vertical
        // moves; every other action restarts it from the new caret column.
        if !matches!(action, EditAction::Up | EditAction::Down) {
            self.preferred_col = None;
        }
        let char_len = self.char_len();
        match action {
            EditAction::Insert(ch) => {
                let at = byte_of_char(&self.text, self.caret);
                self.text.insert(at, ch);
                self.caret += 1;
            }
            EditAction::Backspace => {
                if self.caret > 0 {
                    let at = byte_of_char(&self.text, self.caret - 1);
                    self.text.remove(at);
                    self.caret -= 1;
                }
            }
            EditAction::Delete => {
                if self.caret < char_len {
                    let at = byte_of_char(&self.text, self.caret);
                    self.text.remove(at);
                }
            }
            EditAction::Left => self.caret = self.caret.saturating_sub(1),
            EditAction::Right => self.caret = (self.caret + 1).min(char_len),
            EditAction::Home => {
                let chars: Vec<char> = self.text.chars().collect();
                self.caret = line_start(&chars, self.caret);
            }
            EditAction::End => {
                let chars: Vec<char> = self.text.chars().collect();
                self.caret = line_end(&chars, self.caret);
            }
            EditAction::Up => {
                if multiline {
                    let chars: Vec<char> = self.text.chars().collect();
                    let want = self
                        .preferred_col
                        .unwrap_or_else(|| self.caret - line_start(&chars, self.caret));
                    self.preferred_col = Some(want);
                    self.caret = vertical_move(&chars, self.caret, -1, want);
                }
            }
            EditAction::Down => {
                if multiline {
                    let chars: Vec<char> = self.text.chars().collect();
                    let want = self
                        .preferred_col
                        .unwrap_or_else(|| self.caret - line_start(&chars, self.caret));
                    self.preferred_col = Some(want);
                    self.caret = vertical_move(&chars, self.caret, 1, want);
                }
            }
            EditAction::Newline => {
                if multiline {
                    let at = byte_of_char(&self.text, self.caret);
                    self.text.insert(at, '\n');
                    self.caret += 1;
                }
            }
            EditAction::Submit => return true,
            EditAction::None => {}
        }
        false
    }

    /// Resolve the caret (a char index over the whole buffer) to its
    /// `(line, column)` in the `'\n'`-split view — so a UI can draw a caret bar
    /// mid-text, not just after the last line. Both indices are clamped; the line
    /// list is `[""]` for an empty buffer, giving `(0, 0)`.
    pub fn caret_line_col(&self) -> (usize, usize) {
        let lines: Vec<&str> = if self.text.is_empty() {
            vec![""]
        } else {
            self.text.split('\n').collect()
        };
        let caret = self.caret.min(self.char_len());
        let mut acc = 0; // char index at the start of the current line
        for (i, line) in lines.iter().enumerate() {
            let len = line.chars().count();
            // The caret belongs to this line when it falls within the line's
            // chars OR exactly at its end (before the joining '\n').
            if caret <= acc + len {
                return (i, caret - acc);
            }
            acc += len + 1; // + 1 for the '\n' joining to the next line
        }
        let last = lines.len() - 1;
        (last, lines[last].chars().count())
    }

    // ---- selection ----------------------------------------------------------
    //
    // A selection is the (unordered) span between [`anchor`] and [`caret`]. The
    // caret is always the moving end; the anchor is the fixed end. `None` anchor
    // — or an anchor equal to the caret — is a collapsed caret (no selection).

    /// The current selection as an ordered CHARACTER range, or `None` when the
    /// caret is collapsed. Never empty when `Some`.
    pub fn selection(&self) -> Option<Range<usize>> {
        let a = self.anchor?;
        let n = self.char_len();
        let a = a.min(n);
        let c = self.caret.min(n);
        if a == c {
            None
        } else {
            Some(a.min(c)..a.max(c))
        }
    }

    /// `true` when a non-empty selection is active.
    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    /// The selected text, or `""` when nothing is selected. Allocates.
    pub fn selected_text(&self) -> String {
        match self.selection() {
            Some(r) => self
                .text
                .chars()
                .skip(r.start)
                .take(r.end - r.start)
                .collect(),
            None => String::new(),
        }
    }

    /// Collapse any selection to the caret (drop the anchor). Cheap no-op when
    /// there is none.
    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// Select the whole buffer, caret at the end. A no-op on an empty buffer
    /// (nothing to select). Breaks undo coalescing.
    pub fn select_all(&mut self) {
        let n = self.char_len();
        if n == 0 {
            self.anchor = None;
            self.caret = 0;
        } else {
            self.anchor = Some(0);
            self.caret = n;
        }
        self.preferred_col = None;
        self.coalescing = None;
    }

    /// Set an explicit selection `anchor..caret` (either order), both clamped.
    /// The moving end (caret) is `caret`. Used by mouse drag, double-click word
    /// select and shift-click. Breaks undo coalescing.
    pub fn set_selection(&mut self, anchor: usize, caret: usize) {
        let n = self.char_len();
        self.anchor = Some(anchor.min(n));
        self.caret = caret.min(n);
        self.preferred_col = None;
        self.coalescing = None;
    }

    /// Place (or extend) the caret from a pointer hit — the primitive behind a
    /// click, shift-click, and drag. With `extend`, keep the existing anchor
    /// (seeding it from the old caret if there was none) and move the caret to
    /// `idx`, growing/shrinking the selection. Without `extend`, drop the anchor
    /// and move the collapsed caret to `idx`. Breaks undo coalescing.
    pub fn place_caret(&mut self, idx: usize, extend: bool) {
        let idx = idx.min(self.char_len());
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.caret.min(self.char_len()));
            }
        } else {
            self.anchor = None;
        }
        self.caret = idx;
        self.preferred_col = None;
        self.coalescing = None;
    }

    /// The word (or run of whitespace) containing character index `idx`, as an
    /// ordered range — the double-click selection unit. A "word" is a maximal
    /// run of alphanumeric/`_` characters; if `idx` sits on a non-word, non-space
    /// character the single character is returned, and a run of spaces selects
    /// the whole run. At the very end of the buffer the last word is returned.
    pub fn word_at(&self, idx: usize) -> Range<usize> {
        let chars: Vec<char> = self.text.chars().collect();
        let n = chars.len();
        if n == 0 {
            return 0..0;
        }
        // Clamp onto a real character: at the end, look back at the last one.
        let i = idx.min(n - 1);
        let class = char_class(chars[i]);
        let mut start = i;
        while start > 0 && char_class(chars[start - 1]) == class {
            start -= 1;
        }
        let mut end = i + 1;
        while end < n && char_class(chars[end]) == class {
            end += 1;
        }
        start..end
    }

    /// The destination CHARACTER index of motion `m` starting from the current
    /// caret. Pure: never mutates the buffer.
    pub fn motion_target(&self, m: Motion) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let n = chars.len();
        let caret = self.caret.min(n);
        match m {
            Motion::CharLeft => caret.saturating_sub(1),
            Motion::CharRight => (caret + 1).min(n),
            Motion::WordLeft => {
                let mut i = caret;
                while i > 0 && char_class(chars[i - 1]) != 0 {
                    i -= 1;
                }
                while i > 0 && char_class(chars[i - 1]) == 0 {
                    i -= 1;
                }
                i
            }
            Motion::WordRight => {
                let mut i = caret;
                while i < n && char_class(chars[i]) != 0 {
                    i += 1;
                }
                while i < n && char_class(chars[i]) == 0 {
                    i += 1;
                }
                i
            }
            Motion::LineStart => line_start(&chars, caret),
            Motion::LineEnd => line_end(&chars, caret),
            Motion::DocStart => 0,
            Motion::DocEnd => n,
        }
    }

    /// Move the caret by motion `m`. With `extend`, the existing anchor is
    /// seeded from the old caret and kept (growing the selection); without it,
    /// an active selection collapses to the edge the motion started from
    /// (macOS collapses to the edge pressed toward: `CharLeft` to `start`,
    /// `CharRight` to `end`) and any other motion jumps the collapsed caret.
    pub fn move_caret(&mut self, m: Motion, extend: bool) {
        if extend {
            self.place_caret(self.motion_target(m), true);
            return;
        }
        if let Some(r) = self.selection() {
            if m == Motion::CharLeft {
                self.set_caret(r.start);
                return;
            }
            if m == Motion::CharRight {
                self.set_caret(r.end);
                return;
            }
        }
        self.set_caret(self.motion_target(m));
    }

    /// The range `⌥⌫` deletes: from the `WordLeft` target to the caret.
    pub fn word_backward_range(&self) -> Range<usize> {
        self.motion_target(Motion::WordLeft)..self.caret
    }

    /// The range `⌥⌦` deletes: from the caret to the `WordRight` target.
    pub fn word_forward_range(&self) -> Range<usize> {
        self.caret..self.motion_target(Motion::WordRight)
    }

    /// The range `⌘⌫` deletes: from the line start to the caret — or, at a line
    /// start with text before it, one character left (deleting the `'\n'` that
    /// joins this line with the previous one).
    pub fn line_backward_range(&self) -> Range<usize> {
        let ls = self.motion_target(Motion::LineStart);
        if ls == self.caret && self.caret > 0 {
            (self.caret - 1)..self.caret
        } else {
            ls..self.caret
        }
    }

    /// The CHARACTER range of the logical line containing `idx` (clamped to
    /// the buffer length), inclusive of the trailing `'\n'` when present —
    /// the triple-click selection unit.
    pub fn line_range_at(&self, idx: usize) -> Range<usize> {
        let chars: Vec<char> = self.text.chars().collect();
        let n = chars.len();
        let idx = idx.min(n);
        let end = line_end(&chars, idx);
        let end = if end < n && chars[end] == '\n' {
            end + 1
        } else {
            end
        };
        line_start(&chars, idx)..end
    }

    /// `true` when [`EditBuffer::undo`] would restore an earlier state.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// `true` when [`EditBuffer::redo`] would restore a later state.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Delete the current selection (if any), leaving the caret where the
    /// selection started. Returns `true` when something was deleted. The
    /// selection-replacing insert path (typing over a selection) and Backspace/
    /// Delete-with-a-selection both route through here.
    pub fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some(r) => {
                self.replace_char_range(r, "");
                true
            }
            None => false,
        }
    }

    // ---- undo / redo --------------------------------------------------------

    /// Record the current state as an undo checkpoint *before* an edit of the
    /// given coalescing class. A run of same-class edits (typing, or a run of
    /// deletes) collapses into one step; a class change, a caret placement, or
    /// an [`UndoKind::Boundary`] edit starts a fresh step. Any checkpoint clears
    /// the redo stack. Callers checkpoint only at genuine user-edit boundaries —
    /// IME preedit refinement and dictation churn deliberately do not, so Undo
    /// steps back over words the user typed, not over transient composition.
    pub fn checkpoint(&mut self, kind: UndoKind) {
        let coalesce = kind != UndoKind::Boundary
            && self.coalescing == Some(kind)
            && !self.undo_stack.is_empty();
        self.coalescing = Some(kind);
        self.redo_stack.clear();
        if coalesce {
            return;
        }
        self.undo_stack.push(self.snapshot());
    }

    /// Force the next edit to start a fresh undo step (used when the caret is
    /// repositioned by a pointer or a navigation key mid-typing).
    pub fn break_undo(&mut self) {
        self.coalescing = None;
    }

    /// Undo the last checkpointed edit-run, restoring text, caret and selection.
    /// Returns `false` (and does nothing) when there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push(self.snapshot());
        self.restore(prev);
        self.coalescing = None;
        true
    }

    /// Redo the last undone edit-run. Returns `false` when the redo stack is
    /// empty.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(self.snapshot());
        self.restore(next);
        self.coalescing = None;
        true
    }

    fn snapshot(&self) -> UndoState {
        UndoState {
            text: self.text.clone(),
            caret: self.caret,
            anchor: self.anchor,
        }
    }

    fn restore(&mut self, state: UndoState) {
        self.text = state.text;
        self.caret = state.caret.min(self.char_len());
        self.anchor = state.anchor;
        self.preferred_col = None;
    }
}

/// A caret motion, decoupled from any key-event plumbing. The UI layer
/// (awili-app) maps keystrokes such as arrows, ⌥←/⌥→ and ⌘←/⌘→ onto these.
/// Word motions cross punctuation and whitespace, then cross the adjacent
/// word — the macOS option-arrow behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// One character left.
    CharLeft,
    /// One character right.
    CharRight,
    /// One word left (over the trailing non-word run, then the word).
    WordLeft,
    /// One word right (over the leading non-word run, then the word).
    WordRight,
    /// Start of the current logical line.
    LineStart,
    /// End of the current logical line.
    LineEnd,
    /// Start of the document.
    DocStart,
    /// End of the document.
    DocEnd,
}

/// The character class used for word selection: word characters
/// (alphanumeric + `_`) cohere into words, whitespace into runs, and every
/// other character is its own class so punctuation selects a single glyph.
fn char_class(c: char) -> u8 {
    if c.is_alphanumeric() || c == '_' {
        0
    } else if c.is_whitespace() {
        1
    } else {
        2
    }
}

/// Split `line` into the text before / at-and-after a caret column (a char
/// index). UTF-8 correct — the split lands on a char boundary. A free helper so
/// the UI can render a caret bar between the two halves of the caret line.
pub fn split_at_col(line: &str, col: usize) -> (String, String) {
    let at = byte_of_char(line, col);
    (line[..at].to_string(), line[at..].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type a run of chars through `Insert`, returning the buffer.
    fn typed(s: &str) -> EditBuffer {
        let mut b = EditBuffer::new();
        for ch in s.chars() {
            assert!(!b.apply(EditAction::Insert(ch), true));
        }
        b
    }

    #[test]
    fn insert_advances_caret_per_char() {
        let b = typed("hello");
        assert_eq!(b.text(), "hello");
        assert_eq!(b.caret(), 5);
        assert_eq!(b.char_len(), 5);
    }

    #[test]
    fn backspace_removes_char_before_caret() {
        let mut b = typed("hello");
        assert!(!b.apply(EditAction::Backspace, true));
        assert_eq!(b.text(), "hell");
        assert_eq!(b.caret(), 4);
    }

    #[test]
    fn backspace_at_start_is_a_noop() {
        let mut b = EditBuffer::from_text("hi");
        b.apply(EditAction::Home, false);
        assert_eq!(b.caret(), 0);
        assert!(!b.apply(EditAction::Backspace, false));
        assert_eq!(b.text(), "hi");
        assert_eq!(b.caret(), 0);
    }

    #[test]
    fn delete_removes_char_at_caret() {
        let mut b = EditBuffer::from_text("hello");
        b.apply(EditAction::Home, false);
        assert!(!b.apply(EditAction::Delete, false));
        assert_eq!(b.text(), "ello");
        assert_eq!(b.caret(), 0);
    }

    #[test]
    fn left_right_clamp_at_bounds() {
        let mut b = EditBuffer::from_text("ab");
        // caret at 2 (end); Right is a no-op.
        assert!(!b.apply(EditAction::Right, false));
        assert_eq!(b.caret(), 2);
        b.apply(EditAction::Left, false);
        b.apply(EditAction::Left, false);
        b.apply(EditAction::Left, false); // saturating at 0
        assert_eq!(b.caret(), 0);
    }

    #[test]
    fn insert_at_caret_in_middle() {
        let mut b = EditBuffer::from_text("ac");
        b.apply(EditAction::Left, false); // caret between a and c
        assert_eq!(b.caret(), 1);
        b.apply(EditAction::Insert('b'), false);
        assert_eq!(b.text(), "abc");
        assert_eq!(b.caret(), 2);
    }

    // ---- multibyte: emoji ------------------------------------------------

    #[test]
    fn emoji_inserts_and_caret_counts_chars_not_bytes() {
        // "😀" is 4 UTF-8 bytes but ONE char.
        let mut b = typed("a😀b");
        assert_eq!(b.text(), "a😀b");
        assert_eq!(b.char_len(), 3);
        assert_eq!(b.caret(), 3);
        // Byte length proves these are multi-byte chars.
        assert_eq!(b.text().len(), 6);
        // Backspace removes the whole 'b', then the whole emoji — never a byte.
        b.apply(EditAction::Backspace, false);
        assert_eq!(b.text(), "a😀");
        b.apply(EditAction::Backspace, false);
        assert_eq!(b.text(), "a");
        assert_eq!(b.caret(), 1);
    }

    #[test]
    fn caret_moves_over_emoji_by_whole_char() {
        let mut b = EditBuffer::from_text("😀😀");
        assert_eq!(b.caret(), 2);
        b.apply(EditAction::Left, false);
        assert_eq!(b.caret(), 1);
        // Insert between the two emoji — must not split either.
        b.apply(EditAction::Insert('x'), false);
        assert_eq!(b.text(), "😀x😀");
        assert_eq!(b.caret(), 2);
    }

    #[test]
    fn delete_emoji_at_caret() {
        let mut b = EditBuffer::from_text("😀tail");
        b.apply(EditAction::Home, false);
        b.apply(EditAction::Delete, false);
        assert_eq!(b.text(), "tail");
        assert_eq!(b.caret(), 0);
    }

    // ---- multibyte: CJK --------------------------------------------------

    #[test]
    fn cjk_inserts_and_edits_by_whole_char() {
        // Each of these is 3 UTF-8 bytes, one char.
        let mut b = typed("日本語");
        assert_eq!(b.char_len(), 3);
        assert_eq!(b.text().len(), 9);
        assert_eq!(b.caret(), 3);
        b.apply(EditAction::Left, false); // between 本 and 語
        b.apply(EditAction::Insert('!'), false);
        assert_eq!(b.text(), "日本!語");
        assert_eq!(b.caret(), 3);
    }

    #[test]
    fn cjk_backspace_removes_whole_char() {
        let mut b = EditBuffer::from_text("中文");
        b.apply(EditAction::Backspace, false);
        assert_eq!(b.text(), "中");
        assert_eq!(b.caret(), 1);
    }

    #[test]
    fn paste_inserts_multibyte_string_at_caret() {
        let mut b = EditBuffer::from_text("ab");
        b.apply(EditAction::Left, false); // caret at 1
        b.insert_str("日😀");
        assert_eq!(b.text(), "a日😀b");
        assert_eq!(b.caret(), 3); // advanced by 2 chars
    }

    // ---- multiline / newline / vertical motion ---------------------------

    #[test]
    fn newline_only_in_multiline_mode() {
        let mut b = EditBuffer::from_text("ab");
        // single-line: Newline is inert
        assert!(!b.apply(EditAction::Newline, false));
        assert_eq!(b.text(), "ab");
        // multiline: inserts '\n'
        assert!(!b.apply(EditAction::Newline, true));
        assert_eq!(b.text(), "ab\n");
        assert_eq!(b.caret(), 3);
    }

    #[test]
    fn submit_returns_true_only_for_submit_action() {
        let mut b = EditBuffer::from_text("hi");
        assert!(b.apply(EditAction::Submit, false));
        assert!(!b.apply(EditAction::Insert('x'), false));
    }

    #[test]
    fn home_and_end_respect_line_boundaries() {
        let mut b = EditBuffer::from_text("foo\nbar");
        // caret at 7 (end of "bar")
        b.apply(EditAction::Home, true);
        assert_eq!(b.caret(), 4); // start of "bar"
        b.apply(EditAction::End, true);
        assert_eq!(b.caret(), 7);
    }

    #[test]
    fn vertical_move_keeps_column_and_clamps() {
        // line 0 "hello" (5), line 1 "hi" (2)
        let mut b = EditBuffer::from_text("hello\nhi");
        // caret at 8 (end of "hi"); col 2
        b.apply(EditAction::Up, true);
        // up to line 0 keeping col 2 -> char index 2
        assert_eq!(b.caret(), 2);
        b.apply(EditAction::End, true); // col 5, char index 5
        b.apply(EditAction::Down, true);
        // down to "hi" clamps col 5 -> 2 -> char index 5+1+2 = 8
        assert_eq!(b.caret(), 8);
    }

    #[test]
    fn vertical_move_retains_preferred_column_across_short_line() {
        // Long / short / long: from column 5 on line 0, Down lands on the short
        // line "x" (clamped to column 1), and the NEXT Down must return to column
        // 5 on the bottom line — not stay at column 1. This is the regression the
        // old `vertical_move` (deriving the column from the clamped caret each
        // time) failed.
        let mut b = EditBuffer::from_text("12345\nx\n12345");
        b.apply(EditAction::Home, true); // line 2, col 0
        b.apply(EditAction::Up, true); // line 1 "x", col 0
        b.apply(EditAction::Up, true); // line 0, col 0
        b.apply(EditAction::End, true); // line 0, col 5 -> char index 5
        assert_eq!(b.caret(), 5);
        b.apply(EditAction::Down, true); // line 1 "x": col clamps 5 -> 1, index 7
        assert_eq!(b.caret(), 7);
        b.apply(EditAction::Down, true); // line 2: preferred col 5 restored -> 13
        assert_eq!(b.caret(), 13);
        assert_eq!(b.caret_line_col(), (2, 5));
    }

    #[test]
    fn horizontal_motion_resets_preferred_column() {
        // After a vertical move parks the preferred column, a horizontal motion
        // must clear it so the next Down uses the new column, not the stale one.
        let mut b = EditBuffer::from_text("12345\nx\n12345");
        b.apply(EditAction::Up, true); // from end (line 2 col 5) -> line 1 col 1
        b.apply(EditAction::Up, true); // line 0 col 1 (preferred 5 clamps... )
                                       // Park at a fresh column with Home, then a single Down should track col 0.
        b.apply(EditAction::Home, true); // line 0 col 0, preferred reset
        b.apply(EditAction::Down, true); // line 1 col 0
        assert_eq!(b.caret_line_col(), (1, 0));
        b.apply(EditAction::Down, true); // line 2 col 0
        assert_eq!(b.caret_line_col(), (2, 0));
    }

    #[test]
    fn replace_char_range_edits_and_places_caret() {
        let mut b = EditBuffer::from_text("hello");
        // Replace "ell" (chars 1..4) with "ELLO".
        b.replace_char_range(1..4, "ELLO");
        assert_eq!(b.text(), "hELLOo");
        assert_eq!(b.caret(), 5); // just past the inserted text
                                  // Empty replacement = deletion.
        b.replace_char_range(0..1, "");
        assert_eq!(b.text(), "ELLOo");
        assert_eq!(b.caret(), 0);
    }

    #[test]
    fn replace_char_range_is_multibyte_safe_and_clamps() {
        let mut b = EditBuffer::from_text("日本語");
        // Replace the middle char (1..2) with a 2-char string.
        b.replace_char_range(1..2, "ab");
        assert_eq!(b.text(), "日ab語");
        assert_eq!(b.caret(), 3);
        // Out-of-range end clamps to the buffer end.
        b.replace_char_range(3..99, "!");
        assert_eq!(b.text(), "日ab!");
        assert_eq!(b.caret(), 4);
    }

    #[test]
    fn replace_char_range_clamps_inverted_range_to_empty() {
        // An inverted range (start > end) is documented to clamp so `start <= end`
        // via `end.min(n).max(start)`, collapsing the span to empty AT `start` — a
        // pure insertion, never a panic. Every other replace_char_range test passes
        // a well-ordered range (1..4, 0..1, 1..2, 3..99), so the `.max(start)`
        // clamp is unpinned: removing it leaves `end` below `start`, and the byte
        // mapping then yields `end_b < start_b` so `String::replace_range` PANICS —
        // yet the whole suite survives. Pin the inverted case here.
        let mut b = EditBuffer::from_text("hello");
        #[allow(clippy::reversed_empty_ranges)] // the inverted range IS the case under test
        b.replace_char_range(4..1, "X"); // start clamps to 4, end.max(4) = 4 -> empty
        assert_eq!(b.text(), "hellXo"); // pure insert at char 4, no panic
        assert_eq!(b.caret(), 5); // start(4) + 1 inserted char
    }

    #[test]
    fn set_caret_clamps_and_resets_preferred_column() {
        let mut b = EditBuffer::from_text("ab\ncd");
        b.set_caret(99);
        assert_eq!(b.caret(), 5);
        b.set_caret(1);
        assert_eq!(b.caret(), 1);
    }

    #[test]
    fn vertical_move_over_multibyte_lines() {
        // line 0 "日本" (2 chars), line 1 "x" (1 char)
        let mut b = EditBuffer::from_text("日本\nx");
        // caret at end (4); Up -> col 1 clamps to line0 len 2 -> col 1
        b.apply(EditAction::Up, true);
        assert_eq!(b.caret(), 1);
        assert_eq!(b.text(), "日本\nx");
    }

    #[test]
    fn up_down_inert_in_single_line_mode() {
        let mut b = EditBuffer::from_text("a\nb");
        let before = b.caret();
        b.apply(EditAction::Up, false);
        b.apply(EditAction::Down, false);
        assert_eq!(b.caret(), before);
    }

    #[test]
    fn down_on_the_last_line_is_inert() {
        // Pressing Down while the caret is already on the LAST line must leave the
        // caret untouched — `vertical_move`'s `if cur_end >= chars.len()` guard.
        // Every other vertical test only ever LANDS on the last line from the line
        // above (so the guard is evaluated against the starting line, never the
        // last line itself); none presses Down again once there. That leaves the
        // guard unpinned: an off-by-one mutation (`>=` → `>`) or its removal makes
        // the code compute `next_start = chars.len() + 1` and then
        // `next_len = line_end(..) - next_start`, a usize underflow that PANICS in
        // debug — yet the whole suite survives. Pin it here.
        let mut b = EditBuffer::from_text("hello\nhi");
        assert_eq!(b.caret(), 8); // end of "hi", the last line
        assert!(!b.apply(EditAction::Down, true));
        assert_eq!(
            b.caret(),
            8,
            "Down on the last line must not move the caret"
        );
    }

    #[test]
    fn up_on_the_first_line_is_inert() {
        // The mirror of the above: pressing Up while already on the FIRST line must
        // leave the caret untouched — `vertical_move`'s `if cur_start == 0` guard.
        // No test presses Up once the caret has reached line 0 (they only land
        // there from line 1), so the guard is unpinned: mutating `== 0` or removing
        // it makes the code compute `prev_nl = cur_start - 1` at `cur_start == 0`, a
        // usize underflow that PANICS in debug, yet the suite survives.
        let mut b = EditBuffer::from_text("hi\nthere");
        b.apply(EditAction::Up, true); // land on line 0 (from the last line)
        assert_eq!(b.caret(), 2); // end-clamped column on "hi"
        assert!(!b.apply(EditAction::Up, true));
        assert_eq!(b.caret(), 2, "Up on the first line must not move the caret");
    }

    // ---- caret_line_col / split_at_col -----------------------------------

    #[test]
    fn caret_line_col_maps_multiline_caret() {
        let mut b = EditBuffer::from_text("foo\nbarbaz");
        // caret at end -> line 1, col 6
        assert_eq!(b.caret_line_col(), (1, 6));
        b.apply(EditAction::Home, true); // start of line 1
        assert_eq!(b.caret_line_col(), (1, 0));
        b.apply(EditAction::Up, true); // line 0 col 0
        assert_eq!(b.caret_line_col(), (0, 0));
    }

    #[test]
    fn caret_line_col_resolves_caret_at_end_of_non_final_line() {
        // The caret sitting exactly at the end of a non-final line (on the joining
        // '\n', char index == that line's end) must resolve to THAT line's end,
        // not spill onto the next line. This pins the `caret <= acc + len`
        // boundary: every other caret_line_col test places the caret either at a
        // line START, mid-line, or at the buffer's very end (which the trailing
        // fallback also handles) — none at an interior line's end. Tightening the
        // boundary to `caret < acc + len` makes this caret resolve to the next
        // line (here underflowing `caret - acc`), so this test fails that mutation
        // while passing real code.
        let mut b = EditBuffer::from_text("ab\ncd");
        b.apply(EditAction::Up, true); // from line 1 col 2 up to line 0 -> char index 2
        assert_eq!(b.caret(), 2); // end of "ab", on the '\n'
        assert_eq!(b.caret_line_col(), (0, 2)); // line 0 col 2, NOT (1, _)
    }

    #[test]
    fn caret_line_col_empty_buffer() {
        let b = EditBuffer::new();
        assert_eq!(b.caret_line_col(), (0, 0));
    }

    #[test]
    fn split_at_col_is_utf8_safe() {
        let (a, z) = split_at_col("日本語", 1);
        assert_eq!(a, "日");
        assert_eq!(z, "本語");
        // col past the end pins everything before.
        let (a, z) = split_at_col("hi", 9);
        assert_eq!(a, "hi");
        assert_eq!(z, "");
    }

    #[test]
    fn take_empties_and_resets_caret() {
        let mut b = EditBuffer::from_text("draft");
        assert_eq!(b.take(), "draft");
        assert!(b.is_empty());
        assert_eq!(b.caret(), 0);
    }

    // ---- selection -------------------------------------------------------

    #[test]
    fn place_caret_click_and_shift_click_extend() {
        let mut b = EditBuffer::from_text("abcdef"); // caret at 6
                                                     // Plain click between c and d (char index 3).
        b.place_caret(3, false);
        assert_eq!(b.caret(), 3);
        assert!(!b.has_selection());
        // Shift-click at 5 extends a selection from the old caret (3) to 5.
        b.place_caret(5, true);
        assert_eq!(b.caret(), 5);
        assert_eq!(b.selection(), Some(3..5));
        assert_eq!(b.selected_text(), "de");
        // Shift-click back before the anchor flips the ordered range.
        b.place_caret(1, true);
        assert_eq!(b.selection(), Some(1..3));
        assert_eq!(b.selected_text(), "bc");
    }

    #[test]
    fn drag_selection_via_set_selection() {
        let mut b = EditBuffer::from_text("hello world");
        b.set_selection(0, 5); // drag from 0 to 5
        assert_eq!(b.selection(), Some(0..5));
        assert_eq!(b.selected_text(), "hello");
        // Reversed drag reports the same ordered range.
        b.set_selection(11, 6);
        assert_eq!(b.selection(), Some(6..11));
        assert_eq!(b.selected_text(), "world");
    }

    #[test]
    fn select_all_and_delete_selection() {
        let mut b = EditBuffer::from_text("pick me");
        b.select_all();
        assert_eq!(b.selection(), Some(0..7));
        assert!(b.delete_selection());
        assert_eq!(b.text(), "");
        assert_eq!(b.caret(), 0);
        assert!(!b.has_selection());
        // Select All on an empty buffer selects nothing.
        b.select_all();
        assert!(!b.has_selection());
    }

    #[test]
    fn any_key_action_collapses_a_selection() {
        let mut b = EditBuffer::from_text("abcdef");
        b.set_selection(1, 4);
        assert!(b.has_selection());
        // A motion key ends the selection (caret keeps moving from its end).
        b.apply(EditAction::Right, false);
        assert!(!b.has_selection());
    }

    #[test]
    fn word_at_selects_words_spaces_and_punctuation() {
        let b = EditBuffer::from_text("foo, bar_baz qux");
        // Inside "foo" -> the word, not the comma.
        assert_eq!(b.word_at(1), 0..3);
        // "bar_baz" is one word (underscore is a word char).
        assert_eq!(b.word_at(6), 5..12);
        // The comma at index 3 is its own single-char class.
        assert_eq!(b.word_at(3), 3..4);
        // The space at index 4 selects just that run of one space.
        assert_eq!(b.word_at(4), 4..5);
        // Past the end clamps back onto the last word.
        assert_eq!(b.word_at(99), 13..16);
    }

    #[test]
    fn word_at_multibyte_is_char_indexed() {
        // "héllo wörld" — accented letters are word chars, one char each.
        let b = EditBuffer::from_text("héllo wörld");
        assert_eq!(b.word_at(0), 0..5);
        assert_eq!(b.word_at(7), 6..11);
    }

    // ---- undo / redo -----------------------------------------------------

    #[test]
    fn undo_redo_reverts_and_reapplies_a_typing_run() {
        let mut b = EditBuffer::new();
        // A run of typed characters coalesces into ONE undo step.
        for ch in "hello".chars() {
            b.checkpoint(UndoKind::Insert);
            b.apply(EditAction::Insert(ch), false);
        }
        assert_eq!(b.text(), "hello");
        assert!(b.undo());
        assert_eq!(b.text(), "", "one Undo reverts the whole typed run");
        assert!(b.redo());
        assert_eq!(b.text(), "hello");
        // Nothing left to redo.
        assert!(!b.redo());
    }

    #[test]
    fn caret_move_breaks_the_undo_coalescing_run() {
        let mut b = EditBuffer::new();
        b.checkpoint(UndoKind::Insert);
        b.insert_str("foo");
        // A caret placement breaks coalescing, so the next typing is a new step.
        b.set_caret(0);
        b.checkpoint(UndoKind::Insert);
        b.insert_str("X");
        assert_eq!(b.text(), "Xfoo");
        assert!(b.undo());
        assert_eq!(b.text(), "foo", "first Undo drops only the second run");
        assert!(b.undo());
        assert_eq!(b.text(), "", "second Undo drops the first run");
        assert!(!b.undo());
    }

    #[test]
    fn a_fresh_checkpoint_clears_the_redo_stack() {
        let mut b = EditBuffer::new();
        b.checkpoint(UndoKind::Insert);
        b.insert_str("a");
        b.undo();
        assert_eq!(b.text(), "");
        // A new edit after an undo must discard the redo history.
        b.checkpoint(UndoKind::Boundary);
        b.insert_str("z");
        assert!(!b.redo(), "redo is dead once a new edit lands");
        assert_eq!(b.text(), "z");
    }

    #[test]
    fn boundary_edits_never_coalesce() {
        let mut b = EditBuffer::from_text("");
        b.checkpoint(UndoKind::Boundary);
        b.insert_str("aaa");
        b.checkpoint(UndoKind::Boundary);
        b.insert_str("bbb");
        assert_eq!(b.text(), "aaabbb");
        assert!(b.undo());
        assert_eq!(b.text(), "aaa", "each boundary edit is its own step");
    }

    #[test]
    fn set_text_discards_undo_history() {
        let mut b = EditBuffer::new();
        b.checkpoint(UndoKind::Insert);
        b.insert_str("typed");
        b.set_text("prefilled");
        // A prefill is a fresh document — Undo must not walk back into it.
        assert!(!b.undo());
        assert_eq!(b.text(), "prefilled");
    }

    // ---- U3b1: caret motion model ------------------------------------------

    #[test]
    fn u3b1_word_motion_skips_punctuation_and_spaces() {
        // "foo, bar_baz  qux" — 17 chars; indices: foo=0-2 ,=3 space=4
        // bar_baz=5-11 spaces=12-13 qux=14-16.
        let b = EditBuffer::from_text("foo, bar_baz  qux");
        assert_eq!(b.motion_target(Motion::WordLeft), 14);
        let mut b = b;
        b.set_caret(14);
        assert_eq!(b.motion_target(Motion::WordLeft), 5);
        b.set_caret(5);
        assert_eq!(b.motion_target(Motion::WordLeft), 0);
        b.set_caret(0);
        assert_eq!(b.motion_target(Motion::WordLeft), 0);
        b.set_caret(0);
        assert_eq!(b.motion_target(Motion::WordRight), 3);
        b.set_caret(3);
        assert_eq!(b.motion_target(Motion::WordRight), 12);
        b.set_caret(12);
        assert_eq!(b.motion_target(Motion::WordRight), 17);
        b.set_caret(17);
        assert_eq!(b.motion_target(Motion::WordRight), 17);
        // From mid-word the WordRight target is the end of that word.
        b.set_caret(7);
        assert_eq!(b.motion_target(Motion::WordRight), 12);
        // Multibyte text is char-indexed.
        let mut b = EditBuffer::from_text("héllo wörld");
        assert_eq!(b.motion_target(Motion::WordLeft), 6);
        b.set_caret(0);
        assert_eq!(b.motion_target(Motion::WordRight), 5);
        b.set_caret(5);
        assert_eq!(b.motion_target(Motion::WordRight), 11);
    }

    #[test]
    fn u3b1_shift_motion_extends_and_plain_motion_collapses() {
        let mut b = EditBuffer::new();
        b.insert_str("abcdef");
        b.set_caret(1);
        for _ in 0..3 {
            b.move_caret(Motion::CharRight, true);
        }
        assert_eq!(b.selection(), Some(1..4));
        // A plain (non-extend) CharLeft collapses to the selection's start.
        b.move_caret(Motion::CharLeft, false);
        assert_eq!(b.caret(), 1);
        assert_eq!(b.selection(), None);
        // Extend again to 1..4, then a plain CharRight collapses to the end.
        for _ in 0..3 {
            b.move_caret(Motion::CharRight, true);
        }
        assert_eq!(b.selection(), Some(1..4));
        b.move_caret(Motion::CharRight, false);
        assert_eq!(b.caret(), 4);
        assert_eq!(b.selection(), None);
        // A backward selection collapses to its end on a plain CharRight.
        b.set_caret(4);
        b.move_caret(Motion::CharLeft, true);
        b.move_caret(Motion::CharLeft, true);
        assert_eq!(b.selection(), Some(2..4));
        b.move_caret(Motion::CharRight, false);
        assert_eq!(b.caret(), 4);
        assert_eq!(b.selection(), None);
        // A plain CharLeft with no selection moves exactly one character.
        b.set_caret(6);
        b.move_caret(Motion::CharLeft, false);
        assert_eq!(b.caret(), 5);
    }

    #[test]
    fn u3b1_line_and_doc_targets() {
        let mut b = EditBuffer::from_text("one\ntwo\nthree");
        b.set_caret(5);
        assert_eq!(b.motion_target(Motion::LineStart), 4);
        assert_eq!(b.motion_target(Motion::LineEnd), 7);
        assert_eq!(b.motion_target(Motion::DocStart), 0);
        assert_eq!(b.motion_target(Motion::DocEnd), 13);
        // Extend variants, each from a fresh collapsed caret at 5.
        let mut b = EditBuffer::from_text("one\ntwo\nthree");
        b.set_caret(5);
        b.move_caret(Motion::LineEnd, true);
        assert_eq!(b.selection(), Some(5..7));
        let mut b = EditBuffer::from_text("one\ntwo\nthree");
        b.set_caret(5);
        b.move_caret(Motion::LineStart, true);
        assert_eq!(b.selection(), Some(4..5));
        let mut b = EditBuffer::from_text("one\ntwo\nthree");
        b.set_caret(5);
        b.move_caret(Motion::DocStart, true);
        assert_eq!(b.selection(), Some(0..5));
        let mut b = EditBuffer::from_text("one\ntwo\nthree");
        b.set_caret(5);
        b.move_caret(Motion::DocEnd, true);
        assert_eq!(b.selection(), Some(5..13));
    }

    #[test]
    fn u3b1_delete_ranges() {
        let mut b = EditBuffer::from_text("foo bar");
        b.set_caret(7);
        assert_eq!(b.word_backward_range(), 4..7);
        b.set_caret(4);
        assert_eq!(b.word_forward_range(), 4..7);
        b.set_caret(3);
        assert_eq!(b.word_forward_range(), 3..7);
        let mut b = EditBuffer::from_text("one\ntwo");
        b.set_caret(6);
        assert_eq!(b.line_backward_range(), 4..6);
        b.set_caret(4);
        assert_eq!(
            b.line_backward_range(),
            3..4,
            "at line start it joins the lines"
        );
        b.set_caret(0);
        assert_eq!(b.line_backward_range(), 0..0);
        let b = EditBuffer::from_text("");
        assert_eq!(b.word_backward_range(), 0..0);
        assert_eq!(b.word_forward_range(), 0..0);
        assert_eq!(b.line_backward_range(), 0..0);
    }

    #[test]
    fn u3b1_line_range_at_includes_trailing_newline() {
        let b = EditBuffer::from_text("one\ntwo\nthree");
        assert_eq!(b.line_range_at(5), 4..8);
        assert_eq!(b.line_range_at(0), 0..4);
        assert_eq!(b.line_range_at(10), 8..13);
    }

    #[test]
    fn u3b1_can_undo_redo_track_stacks() {
        let mut b = EditBuffer::new();
        assert!(!b.can_undo());
        assert!(!b.can_redo());
        b.checkpoint(UndoKind::Insert);
        b.insert_str("x");
        assert!(b.can_undo());
        assert!(!b.can_redo());
        assert!(b.undo());
        assert!(b.can_redo());
        assert!(!b.can_undo());
        assert!(b.redo());
        assert!(b.can_undo());
        assert!(!b.can_redo());
    }

    // ---- U3b1: shared undo/redo coalescing type ----------------------------

    #[test]
    fn u3b1_undo_kind_maps_onto_undo_redo_kind() {
        // name kept from d98fbef for the deletion gate; the reverted feature's type and ⌘A key path are gone in U3b1-FIX1a
        // Each UndoKind coalesces exactly like it used to: same-class runs
        // fold into one undo step, Boundary never coalesces.
        for kind in [UndoKind::Insert, UndoKind::Delete, UndoKind::Boundary] {
            let mut b = EditBuffer::new();
            b.checkpoint(kind);
            b.insert_str("a");
            b.checkpoint(kind);
            b.insert_str("b");
            assert_eq!(b.text(), "ab");
            assert!(b.undo());
            if matches!(kind, UndoKind::Insert | UndoKind::Delete) {
                // The two same-class rounds coalesced: one undo empties.
                assert_eq!(b.text(), "");
            } else {
                // Boundary never coalesces: one undo leaves the first round.
                assert_eq!(b.text(), "a");
            }
        }
    }

    #[test]
    fn u3b1_checkpoint_accepts_the_shared_kind_for_both_sides() {
        // name kept from d98fbef for the deletion gate; the reverted feature's type and ⌘A key path are gone in U3b1-FIX1a
        let mut b = EditBuffer::new();
        b.checkpoint(UndoKind::Insert);
        b.insert_str("ab");
        b.checkpoint(UndoKind::Insert);
        b.insert_str("c");
        assert_eq!(b.text(), "abc");
        // Same-class checkpoints coalesced into ONE step.
        assert!(b.undo());
        assert_eq!(b.text(), "");
        // The redo side mirrors the same single step.
        assert!(b.redo());
        assert_eq!(b.text(), "abc");
        assert!(!b.can_redo(), "redo side mirrors the single coalesced step");
    }
}
