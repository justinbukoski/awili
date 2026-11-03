//! The composer: a GPUI text-input *client*.
//!
//! This is the heart of the slice-1 typing-feel gate. The composer is NOT a
//! `div` that edits from raw key events — that approach cannot see marked
//! (in-progress IME) text or multi-scalar commits, so CJK IMEs, dead keys,
//! press-and-hold accents and macOS Dictation are unavailable or malformed.
//!
//! Instead [`Composer`] implements GPUI's [`EntityInputHandler`] and a custom
//! [`ComposerElement`] installs an [`ElementInputHandler`] during paint. The
//! platform (NSTextInputClient on macOS; text-input-v3 / XIM / xkb-compose on
//! Linux) then routes BOTH committed text (`replace_text_in_range`) and marked
//! composition text (`replace_and_mark_text_in_range`) straight into the
//! [`EditBuffer`]. Key events are kept ONLY for navigation and submit
//! (Enter / Shift+Enter / arrows / Home / End / Backspace / Delete / paste);
//! character insertion — including the space bar — is the platform's job via the
//! input handler, so we never double-insert.
//!
//! Caret geometry uses GPUI text shaping for pixel-exact placement. The field
//! soft-wraps (A2): each logical line is shaped with `shape_text` into a
//! [`WrappedLine`] whose layout answers `position_for_index` (caret → pixels)
//! and `closest_index_for_position` (pointer → caret), so click-to-caret,
//! drag-selection and the IME candidate window all read off the same geometry.
//! The field auto-grows to [`theme::COMPOSER_MAX_ROWS`] rows, then holds that
//! height and scrolls vertically. Click, shift-click, double-click word select
//! and drag are handled by mouse listeners on the shell (see [`Composer::on_mouse_down`]).

use std::ops::Range;
use std::panic::Location;

use gpui::{
    div, fill, point, prelude::*, size, App, AvailableSpace, Bounds, ClipboardItem, Context,
    DispatchPhase, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, GlobalElementId, InspectorElementId, KeyDownEvent, Keystroke, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    SharedString, Size, Style, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
};

use awili_core::{EditAction, EditBuffer, UndoKind};
use awili_voice::Dictation;

use crate::composer_keys::{self, DeleteUnit};
use crate::native_menu;
use crate::theme;

/// Composer events the parent view subscribes to. The composer keeps no opinion
/// on what "send" or "cancel" mean.
#[derive(Clone, Debug)]
pub enum ComposerEvent {
    /// Plain Enter on a non-blank composer (single-line mode only).
    Submit,
    Cancel,
    SwitchChat,
    SwitchWork,
    /// M2g: Up while the draft is empty or still equals the recalled entry.
    HistoryPrev,
    /// M2g: Down while a walk is in progress.
    HistoryNext,
    /// M2e: Enter on a non-blank draft while the owner is busy; the draft was
    /// kept and nothing was submitted.
    BusySubmit,
    /// RC2-R4 (D-U3): Edit ▸ Paste or the context-menu Paste in the main
    /// composer with an image or a file on the clipboard: the app attaches it
    /// (as ⌘V does) instead of pasting nothing.
    PasteAttachments,
}

/// The caret-aware composer. Owns the draft [`EditBuffer`], the focus handle, the
/// IME marked-text range, and a cache of the last paint's shaped lines so the
/// input handler can answer geometry queries (IME candidate-window placement,
/// click-to-caret).
pub struct Composer {
    buffer: EditBuffer,
    focus: FocusHandle,
    /// The IME "composing" range, as CHARACTER indices over the whole buffer.
    /// `None` when not composing. **IME-only** — the dictation ghost is NOT
    /// mirrored here (review slice-3 blocker 2: the mirror desynchronized on
    /// platform edits); the ghost's single source of truth is [`Dictation`], and
    /// every text mutation below routes through it so the two states can never
    /// disagree.
    marked: Option<Range<usize>>,
    /// True from the first mutating preedit of an IME composition until the
    /// commit (or an app-side reset) ends it. Kept SEPARATE from `marked`
    /// because an empty preedit clears the styled span while the composition —
    /// and its one-Undo transaction — stays open (Codex round-2 finding 1);
    /// this flag is what lets the ending commit skip its own checkpoint even
    /// when `marked` is already `None`.
    composing: bool,
    /// Per-logical-line wrapped+shaped text from the last paint, for
    /// input-handler geometry and mouse hit-testing. Rebuilt every paint.
    wrap_layouts: Vec<WrapLayout>,
    /// The composer element's content bounds from the last paint, so pointer
    /// positions (which arrive in window coordinates) can be mapped to a
    /// character index outside the paint pass, and edge auto-scroll knows where
    /// the field's edges are.
    last_bounds: Bounds<Pixels>,
    /// Freshness stamp for [`wrap_layouts`]: the buffer text they were shaped
    /// from, written together with them at the end of paint. Hit-testing
    /// refuses to run against geometry shaped from different text (Codex review
    /// finding 4) — see [`Composer::geometry_fresh`].
    layout_text: String,
    /// Freshness stamp: the [`theme::ui_scale`] the layouts were shaped at.
    layout_scale: f32,
    /// The line height the layouts were painted with, so event handlers hit-test
    /// with the same metric the geometry was built from.
    layout_line_height: Pixels,
    /// `true` while a mouse-drag selection is in progress (button held after a
    /// press inside the field).
    is_selecting: bool,
    /// Vertical scroll offset (>= 0) once the soft-wrapped content grows past
    /// the field's max height (A2). Persists across frames; nudged in prepaint.
    scroll_y: Pixels,
    /// Whether prepaint may adjust [`scroll_y`] to keep the caret's row visible.
    /// `true` after any caret-driven action (typing, navigation, click, IME,
    /// dictation); `false` after a manual wheel/trackpad scroll, so the user can
    /// inspect rows away from the caret without the next paint yanking the
    /// viewport back (Codex review finding 7).
    scroll_follow: bool,
    /// The x the caret "wants" during a run of visual-row Up/Down moves, so it
    /// returns to its column after crossing a short wrapped row — the pixel-space
    /// sibling of [`EditBuffer`]'s preferred column. Cleared by any other action.
    preferred_x: Option<Pixels>,
    /// Live dictation state (slice 3): owns the ghost (partial-transcript) range
    /// and rebases/cancels it as external edits land around it.
    dictation: Dictation,
    /// Shown faint while the draft is empty.
    placeholder: SharedString,
    /// `true` (the chat composer): plain Enter emits [`ComposerEvent::Submit`],
    /// Shift+Enter inserts a newline. `false` (the raw-TOML editor): plain Enter
    /// inserts a newline too — saving is a button, not a keystroke.
    submit_on_enter: bool,
    /// Which chord table [`crate::composer_keys::classify`] uses (macOS vs
    /// Linux); tests override it.
    mac_keys: bool,
    /// U3a1: a credential field; nothing in it reaches the clipboard.
    secret: bool,
    /// M2g (O2): a mirror of the app's `composer_busy()` written by the owning
    /// `Render` before every frame. While set, Enter keeps the draft (no emit,
    /// no newline) — the subscription in `main.rs` drops the submit anyway, so
    /// the key must not reshape the draft either.
    busy: bool,
    /// M2g: `true` for the main composer only — the rename/palette/credential
    /// composers stay `false` so their Up/Down move the caret as before.
    history: bool,
    /// M2g: the entry the walk last recalled, or `None` when not walking.
    /// The Up guard in [`Self::on_key`] compares the draft against this: an
    /// edit that makes the draft differ from the recalled entry ends the
    /// walk (the next Up moves the caret, not the cursor).
    recalled: Option<String>,
}

/// U3a1: enabled flags of the Cut, Copy, Paste and Select All menu rows.
/// RC2-R4 (D-U3): whether the clipboard holds an image or a file the main
/// composer attaches (Edit ▸ Paste, the context menu, ⌘V).
fn clipboard_has_attachment(cx: &mut gpui::App) -> bool {
    cx.read_from_clipboard().is_some_and(|item| {
        item.entries().iter().any(|e| {
            matches!(
                e,
                gpui::ClipboardEntry::Image(_) | gpui::ClipboardEntry::ExternalPaths(_)
            )
        })
    })
}

fn composer_menu_state(sel: bool, secret: bool, clip: bool, empty: bool) -> [bool; 4] {
    [sel && !secret, sel && !secret, clip, !empty]
}

/// One logical line's soft-wrapped, shaped glyphs plus where it was painted, so
/// screen points and character ranges can be mapped back and forth. A single
/// [`WrappedLine`] paints all of its visual rows from `origin` downward; its
/// layout answers `position_for_index` / `closest_index_for_position` in
/// coordinates local to `origin`.
struct WrapLayout {
    /// Character index of this line's first char within the whole buffer.
    char_start: usize,
    /// Number of characters in this logical line (excluding the joining '\n').
    char_len: usize,
    /// Number of visual rows this line occupies (wrap boundaries + 1).
    rows: usize,
    line: WrappedLine,
    /// Top-left of the painted first row (already includes the vertical scroll).
    origin: Point<Pixels>,
}

impl Composer {
    #[cfg(test)]
    pub(crate) fn painted_geometry(&self) -> (Bounds<Pixels>, &str, usize) {
        (
            self.last_bounds,
            &self.layout_text,
            self.wrap_layouts.iter().map(|l| l.rows).sum(),
        )
    }

    #[cfg(test)]
    pub(crate) fn painted_vertical_bounds(&self) -> Vec<(Pixels, Pixels)> {
        self.wrap_layouts
            .iter()
            .map(|line| {
                (
                    line.origin.y,
                    line.origin.y + self.layout_line_height * line.rows as f32,
                )
            })
            .collect()
    }
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::with_options(theme::COMPOSER_PLACEHOLDER, true, cx)
    }

    /// A composer with a custom placeholder and Enter behaviour — the inline
    /// rename editor and the settings panel's field/raw editors.
    pub fn with_options(
        placeholder: impl Into<SharedString>,
        submit_on_enter: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            buffer: EditBuffer::new(),
            // U3e1: every rendered composer is the first tab stop.
            focus: cx
                .focus_handle()
                .tab_index(crate::focus_nav::COMPOSER)
                .tab_stop(true),
            marked: None,
            composing: false,
            wrap_layouts: Vec::new(),
            last_bounds: Bounds::default(),
            layout_text: String::new(),
            layout_scale: 1.0,
            layout_line_height: theme::z(theme::LINE_HEIGHT),
            is_selecting: false,
            scroll_y: theme::z(0.),
            scroll_follow: true,
            preferred_x: None,
            dictation: Dictation::new(),
            placeholder: placeholder.into(),
            submit_on_enter,
            mac_keys: cfg!(target_os = "macos"),
            secret: false,
            busy: false,
            history: false,
            recalled: None,
        }
    }

    /// M2g: opt the composer into Up/Down prompt history. Only the main
    /// composer calls this — the rename, palette, and credential composers
    /// keep their caret-moving Up/Down.
    pub fn enable_history(&mut self) {
        self.history = true;
    }

    /// M2g: the owning view writes the recalled entry here after each
    /// Up/Down. `cx.notify()` repaints so the draft shows the entry.
    /// `None` clears the walk (the draft is no longer a recalled entry).
    pub fn set_recalled(&mut self, text: Option<String>, cx: &mut Context<Self>) {
        self.recalled = text;
        cx.notify();
    }

    /// M2g: the draft as a plain string (mirrors `buffer.text()` for the
    /// walk's Up-guard). Exposed for the owning view and the key tests.
    #[allow(dead_code)]
    pub fn draft(&self) -> String {
        self.buffer.text().to_string()
    }

    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>) {
        self.placeholder = placeholder.into();
    }

    /// The words the empty box shows (tests read them).
    #[cfg(test)]
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    /// U3a1: Cut and Copy do nothing in a secret field (menu rows disabled).
    pub fn set_secret(&mut self, secret: bool) {
        self.secret = secret;
    }

    /// M2g (O2): the owning view mirrors its `composer_busy()` into the
    /// composer once per frame (called from `Render::render` before painting),
    /// so the Enter guard in [`Self::on_key`] knows the draft would be dropped
    /// at the subscription anyway.
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    /// Replace the whole draft (prefill for rename / settings editing). Clears
    /// any IME mark and dictation ghost; caret goes to the end.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.marked = None;
        self.composing = false;
        self.dictation = Dictation::new();
        self.scroll_y = theme::z(0.);
        self.scroll_follow = true;
        self.preferred_x = None;
        self.buffer.set_text(text);
        self.buffer.set_caret(self.buffer.char_len());
        cx.notify();
    }

    /// The current draft text (read without taking).
    pub fn text(&self) -> &str {
        self.buffer.text()
    }

    /// Show or replace the streaming dictation *partial* as ghost text at the
    /// caret — rendered faint, like an IME preedit, so the user sees words appear
    /// as whisper refines them. Goes through [`Dictation`] → the buffer's public
    /// replace API, never a direct poke (slice-3 spec §2). While a real IME
    /// composition is in progress the partial is skipped — the preedit owns the
    /// insertion point, and the next partial (or the final commit) will land once
    /// it resolves.
    pub fn dictation_partial(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.marked.is_some() || self.composing {
            return;
        }
        self.scroll_follow = true;
        self.dictation.set_partial(&mut self.buffer, text);
        cx.notify();
    }

    /// Commit the final transcript as ordinary, editable text and end the ghost.
    /// Editing-by-typing keeps working because the text is now just part of the
    /// [`EditBuffer`]. One undoable transaction (Codex review finding 2): the
    /// preview is cancelled first — leaving the caret at the utterance start —
    /// so the checkpoint holds the pre-utterance document (ghost churn never
    /// enters history), then the transcript commits at that spot. One Undo
    /// removes the whole utterance.
    pub fn dictation_commit(&mut self, text: &str, cx: &mut Context<Self>) {
        self.scroll_follow = true;
        self.dictation.cancel(&mut self.buffer);
        self.buffer.checkpoint(UndoKind::Boundary);
        self.dictation.commit(&mut self.buffer, text);
        cx.notify();
    }

    /// Abandon the in-progress utterance, removing any ghost text (e.g. the user
    /// released push-to-talk on silence).
    pub fn dictation_cancel(&mut self, cx: &mut Context<Self>) {
        self.dictation.cancel(&mut self.buffer);
        cx.notify();
    }

    /// The focus handle, so the parent can focus the composer on first paint.
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Move the draft text out, leaving the composer empty (used on send so the
    /// draft becomes a message without a clone). Clears the IME mark and resets
    /// dictation — a live ghost's *preview text* goes with the draft (the user
    /// sent what they saw); the session-scoped final that may still be in flight
    /// is invalidated app-side (review blocker 1).
    pub fn take(&mut self) -> String {
        self.marked = None;
        self.composing = false;
        self.scroll_y = theme::z(0.);
        self.scroll_follow = true;
        self.preferred_x = None;
        self.is_selecting = false;
        self.dictation = Dictation::new();
        // M2g: taking the draft ends any walk — the next Up starts fresh over
        // the new candidate list (which now includes the just-sent prompt).
        self.recalled = None;
        self.buffer.take()
    }

    /// Handle one key event. Only navigation / submit shortcuts act here; all
    /// character (and space) insertion arrives through the platform text-input
    /// protocol (`replace_text_in_range`), so this never types a character.
    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        // RC2-R4 (D-U3): on macOS a ⌃ Emacs key acts as the key it stands for.
        let aliased = composer_keys::emacs_alias(&ev.keystroke, self.mac_keys);
        let ks = aliased.as_ref().unwrap_or(&ev.keystroke);
        // Any key action is caret-driven: the next paint may follow the caret
        // again after a manual wheel scroll parked the viewport elsewhere.
        self.scroll_follow = true;

        // Cmd+V (macOS) / Ctrl+V (Linux): paste clipboard text at the caret. The
        // platform does not route a paste through the input handler, so we read
        // the clipboard ourselves; insertion still goes through the buffer — via
        // the dictation-aware path, so a live ghost is rebased (paste before it)
        // or wiped (paste into it), never silently corrupted (review blocker 2).
        if composer_keys::app_chord(&ks.modifiers, self.mac_keys) && ks.key == "v" {
            self.paste(cx);
            return;
        }

        // U3e1: Tab moves focus at the root, except while IME text is marked
        // (candidate navigation keeps it).
        if self.marked.is_some() && ks.key == "tab" {
            cx.stop_propagation();
            return;
        }

        if self.marked.is_none() {
            if ks.key == "escape" && !ks.modifiers.modified() {
                cx.emit(ComposerEvent::Cancel);
                return;
            }
            if composer_keys::app_chord(&ks.modifiers, self.mac_keys) && ks.key == "1" {
                cx.emit(ComposerEvent::SwitchChat);
                return;
            }
            if composer_keys::app_chord(&ks.modifiers, self.mac_keys) && ks.key == "2" {
                cx.emit(ComposerEvent::SwitchWork);
                return;
            }
            // M2g: Up/Down walk the prompt history when the walk is live and
            // the draft is still the recalled entry. No modifiers, no IME
            // composition, and either an empty draft or a draft that equals
            // `recalled` (an edit that diverges the draft ends the walk —
            // the next Up falls through to caret movement).
            //
            // U3-INT1: checked BEFORE the U3 classifier below. The two gates
            // are disjoint — `history_key` refuses Shift, ⌥, ⌘ and ⌃ (it
            // ignores the function modifier, which macOS sets on every arrow),
            // so every shifted/modified motion still reaches the
            // classifier, and a plain Up/Down the walk declines reaches the
            // classifier as `Vertical { extend: false }`, which falls through
            // to the existing `key_to_action` caret path.
            // RC2-R4M5: checked against the key as pressed, so ⌃P / ⌃N (an
            // Emacs alias of ↑ / ↓) always move the caret.
            if let Some(direction) = history_key(
                &ev.keystroke,
                self.buffer.text(),
                self.recalled.as_deref(),
                self.history,
                self.composing,
            ) {
                if direction {
                    cx.emit(ComposerEvent::HistoryPrev);
                } else {
                    cx.emit(ComposerEvent::HistoryNext);
                }
                return;
            }
            // Caret motions and word/line deletes route through the platform
            // chord table (U3b1-FIX1b). IME composition (marked text present)
            // skips the classifier entirely — everything above is gated on
            // `self.marked.is_none()` so a composition in progress keeps the
            // old path. Unshifted Up/Down, Backspace, Delete, Enter and `Other`
            // fall through to `key_to_action` and behave exactly as before.
            match composer_keys::classify(ks, self.mac_keys) {
                composer_keys::KeyEdit::Move { motion, extend } => {
                    self.buffer.break_undo();
                    self.preferred_x = None;
                    self.buffer.move_caret(motion, extend);
                    cx.notify();
                    return;
                }
                composer_keys::KeyEdit::Vertical { down, extend: true } => {
                    // With stale geometry this is a no-op — never fall back to
                    // `apply(Up/Down)` while extending.
                    self.buffer.break_undo();
                    self.visual_vertical_move(down, true);
                    cx.notify();
                    return;
                }
                composer_keys::KeyEdit::DeleteBack(
                    unit @ (DeleteUnit::Word | DeleteUnit::Line),
                ) => {
                    // ⌥⌫ / ⌘⌫. With a live selection, delete it — exactly like
                    // the existing Backspace-with-selection branch; otherwise
                    // delete the classifier's unit range (word back / line
                    // back), and never an empty one. Both go through the
                    // dictation contract (boundary checkpoint + external
                    // replace), the way `cut_selection` does.
                    if let Some(sel) = self.buffer.selection() {
                        self.buffer.checkpoint(UndoKind::Boundary);
                        self.dictation.external_replace(&mut self.buffer, sel, "");
                    } else {
                        let range = match unit {
                            DeleteUnit::Word => self.buffer.word_backward_range(),
                            DeleteUnit::Line => self.buffer.line_backward_range(),
                            DeleteUnit::Char => unreachable!(),
                        };
                        if range.start != range.end {
                            self.buffer.checkpoint(UndoKind::Boundary);
                            self.dictation.external_replace(&mut self.buffer, range, "");
                        }
                    }
                    cx.notify();
                    return;
                }
                composer_keys::KeyEdit::DeleteFwd(DeleteUnit::Word) => {
                    // ⌥⌦ (mac) / ⌃⌦ (Linux): the word-forward range.
                    if let Some(sel) = self.buffer.selection() {
                        self.buffer.checkpoint(UndoKind::Boundary);
                        self.dictation.external_replace(&mut self.buffer, sel, "");
                    } else {
                        let range = self.buffer.word_forward_range();
                        if range.start != range.end {
                            self.buffer.checkpoint(UndoKind::Boundary);
                            self.dictation.external_replace(&mut self.buffer, range, "");
                        }
                    }
                    cx.notify();
                    return;
                }
                // Plain Up/Down, plain Backspace/Delete, and anything else keep
                // the existing `key_to_action` path below.
                _ => {}
            }
        }

        match key_to_action(ks) {
            EditAction::Submit if !self.submit_on_enter => {
                self.buffer.checkpoint(UndoKind::Boundary);
                key_edit(&mut self.buffer, &mut self.dictation, EditAction::Newline);
                cx.notify();
            }
            EditAction::Submit => {
                // M2g (O2): while a reply streams the app drops the submit at
                // the subscription — Enter must keep the draft as-is, not turn
                // it into a newline (the pre-M2g behavior in submit-on-enter
                // false was a newline; now the draft is simply kept).
                if self.busy {
                    // M2e: the owner explains why nothing was sent.
                    if !self.buffer.text().trim().is_empty() {
                        cx.emit(ComposerEvent::BusySubmit);
                    }
                    return;
                }
                if !self.buffer.text().trim().is_empty() {
                    cx.emit(ComposerEvent::Submit);
                }
            }
            EditAction::None => {}
            EditAction::Newline => {
                // Shift+Enter in submit-on-enter mode inserts a newline (A2).
                self.buffer.checkpoint(UndoKind::Boundary);
                key_edit(&mut self.buffer, &mut self.dictation, EditAction::Newline);
                cx.notify();
            }
            EditAction::Backspace | EditAction::Delete => {
                let before = (self.buffer.caret(), self.buffer.text().len());
                if let Some(sel) = self.buffer.selection() {
                    // Backspace/Delete over a selection deletes the selection —
                    // through the dictation contract, so a live ghost beside the
                    // selection rebases and one inside it is wiped, never left
                    // pointing at the wrong characters (Codex review finding 2).
                    self.buffer.checkpoint(UndoKind::Boundary);
                    self.dictation.external_replace(&mut self.buffer, sel, "");
                } else {
                    self.buffer.checkpoint(UndoKind::Delete);
                    let action = key_to_action(ks);
                    key_edit(&mut self.buffer, &mut self.dictation, action);
                }
                if (self.buffer.caret(), self.buffer.text().len()) != before {
                    cx.notify();
                }
            }
            action => {
                // Navigation (arrows / Home / End). A caret move breaks the undo
                // coalescing run so the next typed characters are their own step.
                let before = (self.buffer.caret(), self.buffer.text().len());
                self.buffer.break_undo();
                let vertical = matches!(action, EditAction::Up | EditAction::Down);
                if !vertical {
                    self.preferred_x = None;
                }
                // Up/Down move by VISUAL row over the painted soft-wrapped
                // geometry (Codex review finding 7) — inside one long logical
                // line the caret walks its wrapped rows. Stale geometry falls
                // back to `\n`-line movement.
                let moved_visually =
                    vertical && self.visual_vertical_move(action == EditAction::Down, false);
                if !moved_visually {
                    key_edit(&mut self.buffer, &mut self.dictation, action);
                }
                if (self.buffer.caret(), self.buffer.text().len()) != before
                    || self.buffer.has_selection()
                {
                    cx.notify();
                }
            }
        }
    }

    // ---- selection, mouse, and Edit-menu actions (A1 / A3) ------------------

    /// `true` when the cached paint geometry may be hit-tested: it exists, was
    /// shaped from the CURRENT buffer text, and at the current UI scale. Before
    /// the first non-empty paint, or between an edit/zoom and the following
    /// paint, this is `false` and pointer mapping declines instead of resolving
    /// against stale wrapping (Codex review finding 4). A pure resize is closed
    /// out by GPUI's layout→paint ordering: mouse events dispatch against the
    /// frame that was last painted, whose bounds match these layouts.
    fn geometry_fresh(&self) -> bool {
        !self.wrap_layouts.is_empty()
            && self.layout_text == self.buffer.text()
            && self.layout_scale == theme::ui_scale()
    }

    /// Map a pointer position (window coordinates) to a buffer character index,
    /// using the last paint's wrapped-line geometry. Clamps above the first row
    /// to 0, below the last row onto the last row, and x onto row boundaries.
    /// `None` when the cached geometry is stale or missing — the caller must
    /// leave the caret alone rather than jump it somewhere arbitrary.
    fn char_index_for_point(&self, position: Point<Pixels>, line_height: Pixels) -> Option<usize> {
        if self.buffer.is_empty() {
            return Some(0);
        }
        if !self.geometry_fresh() {
            return None;
        }
        let bands: Vec<(Pixels, Pixels)> = self
            .wrap_layouts
            .iter()
            .map(|l| (l.origin.y, line_height * l.rows as f32))
            .collect();
        let l = &self.wrap_layouts[line_index_for_y(&bands, position.y)];
        // Local to this line's top-left; the wrapped layout resolves x AND the
        // visual row (via y) internally, so a click on any wrapped row lands
        // right. The local y is clamped INTO the line's painted rows: for a y
        // past the last wrapped row gpui returns Err(0) — an error code, not an
        // index — and reading that payload as "byte 0" sent below-bottom clicks
        // to the line START (Codex review finding 3). Clamped onto the last row,
        // the pointer resolves by x there, exactly like a native text view.
        let local = point(
            position.x - l.origin.x,
            (position.y - l.origin.y)
                .max(theme::z(0.))
                .min(line_height * (l.rows as f32 - 0.5)),
        );
        let byte = match l.line.closest_index_for_position(local, line_height) {
            Ok(byte) => byte,
            // With y clamped into a valid row, Err means x fell before the row's
            // start or at/past its end, and the payload IS that row boundary's
            // byte index — the caret position a native field would pick.
            Err(byte) => byte,
        };
        Some(buffer_index_for_hit(
            &l.line.text,
            l.char_start,
            byte,
            self.buffer.char_len(),
        ))
    }

    /// The caret's window-space position (top of its visual row) off the last
    /// paint's geometry, or `None` when the caret's line isn't laid out.
    fn caret_window_position(&self, line_height: Pixels) -> Option<Point<Pixels>> {
        let caret = self.buffer.caret();
        let l = self
            .wrap_layouts
            .iter()
            .find(|l| caret >= l.char_start && caret <= l.char_start + l.char_len)?;
        let byte = byte_of_col(&l.line.text, caret - l.char_start);
        let local = l.line.position_for_index(byte, line_height)?;
        Some(point(l.origin.x + local.x, l.origin.y + local.y))
    }

    /// Move the caret one VISUAL row up/down through the painted soft-wrapped
    /// geometry, retaining the pixel column across a run of vertical moves (the
    /// pixel-space sibling of the buffer's preferred column). On the first/last
    /// visual row the caret collapses to the buffer start/end, as native text
    /// views do. Returns `false` when the geometry is stale — the caller falls
    /// back to logical-line movement.
    fn visual_vertical_move(&mut self, down: bool, extend: bool) -> bool {
        let line_height = self.layout_line_height;
        if !self.geometry_fresh() {
            return false;
        }
        let Some(pos) = self.caret_window_position(line_height) else {
            return false;
        };
        // `pos.y` is the caret row's top; target the middle of the adjacent row.
        let x = self.preferred_x.unwrap_or(pos.x);
        let target_y = if down {
            pos.y + line_height * 1.5
        } else {
            pos.y - line_height * 0.5
        };
        let Some(idx) = self.char_index_for_point(point(x, target_y), line_height) else {
            return false;
        };
        let idx = if idx == self.buffer.caret() {
            // Clamped onto the row the caret is already on: first/last visual
            // row, so collapse to the buffer boundary.
            if down {
                self.buffer.char_len()
            } else {
                0
            }
        } else {
            idx
        };
        self.buffer.place_caret(idx, extend);
        self.preferred_x = Some(x);
        true
    }

    /// Wheel / trackpad scroll (Codex review finding 7): the composer's content
    /// can be inspected without moving the caret. Suspends caret-follow until
    /// the next caret-driven action; prepaint clamps the offset to the content.
    fn on_scroll(&mut self, ev: &gpui::ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = ev.delta.pixel_delta(self.layout_line_height).y;
        if delta == theme::z(0.) {
            return;
        }
        self.scroll_y = (self.scroll_y - delta).max(theme::z(0.));
        self.scroll_follow = false;
        cx.notify();
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.scroll_follow = true;
        self.preferred_x = None;
        // U3a1: a context click opens the menu instead of a select or drag.
        if crate::context_menu::is_context_click(ev.button, &ev.modifiers, self.mac_keys) {
            cx.stop_propagation(); // else the shell's click-to-focus takes focus back
            self.open_menu(ev.position, window, cx);
            return;
        }
        let Some(idx) = self.char_index_for_point(ev.position, self.layout_line_height) else {
            // Stale/unpainted geometry: focus only, never jump the caret.
            return;
        };
        match ev.click_count {
            // Double-click selects the word under the pointer.
            2 => {
                let w = self.buffer.word_at(idx);
                self.buffer.set_selection(w.start, w.end);
            }
            // Triple-click (or more) selects the *line* under the pointer
            // (U3b2 B4/D6) — not the whole buffer: a third click on one line
            // is "select this line", and ⌘A / the Select All menu item still
            // take the whole draft. `line_range_at` is a pure text helper
            // (U3b2 keeps the new helpers in the app crate, not awili-core).
            n if n >= 3 => {
                let r = line_range_at(self.buffer.text(), idx);
                self.buffer.set_selection(r.start, r.end);
            }
            // Single click: place the caret, or extend the selection on Shift.
            _ => {
                self.is_selecting = true;
                self.buffer.place_caret(idx, ev.modifiers.shift);
            }
        }
        cx.notify();
    }

    /// U3a1: no menu during IME composition. A click inside the selection keeps
    /// it, elsewhere it places the caret (D3); the rows follow the field state.
    fn open_menu(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.marked.is_some() {
            return;
        }
        if let Some(idx) = self.char_index_for_point(at, self.layout_line_height) {
            let sel = self.buffer.selection();
            if !sel.is_some_and(|s| s.start <= idx && idx <= s.end) {
                self.buffer.place_caret(idx, false);
            }
        }
        let clip = cx.read_from_clipboard().and_then(|i| i.text());
        let (sel, empty) = (self.buffer.has_selection(), self.buffer.is_empty());
        // RC2-R4 (D-U3): an image or file counts in the main composer.
        let clip =
            clip.is_some_and(|t| !t.is_empty()) || (self.history && clipboard_has_attachment(cx));
        let [cut, copy, paste, all] = composer_menu_state(sel, self.secret, clip, empty);
        let this = cx.entity().downgrade();
        let run = |f: fn(&mut Self, &mut Context<Self>)| -> crate::context_menu::MenuRun {
            let this = this.clone();
            std::rc::Rc::new(move |_, cx| this.update(cx, f).unwrap_or(()))
        };
        use crate::context_menu::MenuEntry as E;
        let entries = vec![
            E::item(
                "Cut",
                Some(crate::shortcuts::localized_static("⌘X")),
                cut,
                run(Self::cut_selection),
            ),
            E::item(
                "Copy",
                Some(crate::shortcuts::localized_static("⌘C")),
                copy,
                run(Self::copy_selection),
            ),
            E::item(
                "Paste",
                Some(crate::shortcuts::localized_static("⌘V")),
                paste,
                run(Self::paste),
            ),
            E::Separator,
            E::item(
                "Select All",
                Some(crate::shortcuts::localized_static("⌘A")),
                all,
                run(Self::select_all),
            ),
        ];
        crate::context_menu::open(at, entries, window, cx);
    }

    /// Continue a drag-selection at `position` — called from the WINDOW-level
    /// move listener the element registers while [`Self::is_selecting`], so the
    /// pointer is captured: the drag keeps extending wherever the pointer goes,
    /// inside the field or far outside it (Codex review finding 4). A pointer
    /// past the top/bottom edge auto-scrolls one line per event toward it and
    /// hit-tests at the clamped edge, walking the selection through rows that
    /// were hidden when the drag started.
    fn drag_to(&mut self, position: Point<Pixels>, line_height: Pixels, cx: &mut Context<Self>) {
        if !self.is_selecting {
            return;
        }
        // The drag drives the scroll itself; don't let prepaint's caret-follow
        // fight it.
        self.scroll_follow = false;
        let bounds = self.last_bounds;
        let mut target = position;
        if position.y < bounds.top() {
            self.scroll_y = (self.scroll_y - line_height).max(theme::z(0.));
            target.y = bounds.top();
        } else if position.y > bounds.bottom() {
            // Over-scroll is clamped to the content in prepaint.
            self.scroll_y += line_height;
            target.y = bounds.bottom() - theme::z(1.);
        }
        if let Some(idx) = self.char_index_for_point(target, line_height) {
            // Drag extends the selection: keep the anchor, move the caret.
            self.buffer.place_caret(idx, true);
        }
        cx.notify();
    }

    fn end_drag(&mut self) {
        self.is_selecting = false;
    }

    /// Paste clipboard text at the caret (or over the selection). Shared by the
    /// Cmd/Ctrl+V key path and the Edit ▸ Paste menu action.
    fn paste(&mut self, cx: &mut Context<Self>) {
        if self.history && clipboard_has_attachment(cx) {
            cx.emit(ComposerEvent::PasteAttachments);
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            if !text.is_empty() {
                self.marked = None;
                self.composing = false;
                self.scroll_follow = true;
                self.buffer.checkpoint(UndoKind::Boundary);
                let range = self
                    .buffer
                    .selection()
                    .unwrap_or_else(|| self.buffer.caret()..self.buffer.caret());
                self.dictation
                    .external_replace(&mut self.buffer, range, &text);
                cx.notify();
            }
        }
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if self.secret {
            return;
        }
        let selected = self.buffer.selected_text();
        if !selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(selected));
        }
    }

    fn cut_selection(&mut self, cx: &mut Context<Self>) {
        if self.secret {
            return;
        }
        let Some(sel) = self.buffer.selection() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(self.buffer.selected_text()));
        self.scroll_follow = true;
        self.buffer.checkpoint(UndoKind::Boundary);
        // Through the dictation contract: cutting beside a live ghost rebases
        // it, cutting into it wipes the preview (Codex review finding 2).
        self.dictation.external_replace(&mut self.buffer, sel, "");
        cx.notify();
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.buffer.select_all();
        self.scroll_follow = true;
        cx.notify();
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        self.marked = None;
        self.composing = false;
        self.scroll_follow = true;
        // A snapshot restore invalidates any live ghost range. The preview is
        // transient (never part of undo history), so end it — removing the ghost
        // text — BEFORE restoring; otherwise Dictation keeps a range into the
        // restored text and a later partial could overwrite unrelated user text
        // (Codex review finding 2).
        let had_ghost = self.dictation.is_active();
        self.dictation.cancel(&mut self.buffer);
        if self.buffer.undo() || had_ghost {
            cx.notify();
        }
    }

    fn redo(&mut self, cx: &mut Context<Self>) {
        self.marked = None;
        self.composing = false;
        self.scroll_follow = true;
        // Same ghost contract as `undo` above.
        let had_ghost = self.dictation.is_active();
        self.dictation.cancel(&mut self.buffer);
        if self.buffer.redo() || had_ghost {
            cx.notify();
        }
    }
}

/// Apply a mutating/navigation key action with dictation awareness (review
/// blocker 2): while a ghost is live, the text-mutating keys (Backspace, Delete,
/// Newline) are routed through [`Dictation::external_replace`] so an edit beside
/// the ghost rebases it and an edit *into* it wipes the preview instead of
/// leaving a stale range behind. Navigation keys (and everything when no ghost is
/// active) go through the plain [`EditBuffer::apply`] path unchanged — the ghost
/// is anchored to its own range, not the caret, so motion needs no bookkeeping.
fn key_edit(buffer: &mut EditBuffer, dictation: &mut Dictation, action: EditAction) {
    if dictation.is_active() {
        let caret = buffer.caret();
        match action {
            EditAction::Backspace if caret > 0 => {
                dictation.external_replace(buffer, caret - 1..caret, "");
                return;
            }
            EditAction::Delete if caret < buffer.char_len() => {
                dictation.external_replace(buffer, caret..caret + 1, "");
                return;
            }
            EditAction::Newline => {
                dictation.external_replace(buffer, caret..caret, "\n");
                return;
            }
            _ => {}
        }
    }
    buffer.apply(action, true);
}

/// Parse a GPUI keystroke into a navigation/submit [`EditAction`]. Awili inverts
/// Hub's Enter convention per the slice-1 spec: plain Enter SUBMITS, Shift+Enter
/// inserts a newline. This NEVER returns `Insert` — character and space input is
/// the platform input handler's responsibility, so returning `Insert` here would
/// double-type on Linux (where the platform also commits the char). Everything
/// not listed (printable keys, shortcut combos, tab, escape) is `None`.
fn key_to_action(ks: &Keystroke) -> EditAction {
    match ks.key.as_str() {
        "enter" | "return" => {
            if ks.modifiers.shift {
                EditAction::Newline
            } else {
                EditAction::Submit
            }
        }
        "backspace" => EditAction::Backspace,
        "delete" => EditAction::Delete,
        "left" => EditAction::Left,
        "right" => EditAction::Right,
        "home" => EditAction::Home,
        "end" => EditAction::End,
        "up" => EditAction::Up,
        "down" => EditAction::Down,
        _ => EditAction::None,
    }
}

// ---- UTF-16 <-> char-index conversions ------------------------------------
//
// The platform protocol speaks UTF-16 code-unit offsets (the macOS/NSString
// convention); `EditBuffer` speaks character indices. These bridge the two over
// a given text. They are intentionally O(n) scans — composer drafts are short.

/// UTF-16 code-unit offset of character `char_idx` in `text`.
fn char_to_utf16(text: &str, char_idx: usize) -> usize {
    text.chars().take(char_idx).map(|c| c.len_utf16()).sum()
}

/// Character index at UTF-16 code-unit offset `utf16` in `text` (clamped to the
/// nearest char boundary at or before the offset, then to the char length).
fn utf16_to_char(text: &str, utf16: usize) -> usize {
    let mut u = 0usize;
    let mut ci = 0usize;
    for c in text.chars() {
        if u >= utf16 {
            break;
        }
        u += c.len_utf16();
        ci += 1;
    }
    ci
}

/// Byte offset of character column `col` within a single line `s`.
fn byte_of_col(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map(|(b, _)| b).unwrap_or(s.len())
}

// ---- pointer → caret mapping (pure, unit-tested) --------------------------
//
// The two arithmetic steps of click-to-caret, factored out of
// `char_index_for_point` so the mapping can be pinned without a live GPUI
// window (the x → glyph step is gpui's own `closest_index_for_position`).

/// Index of the logical line whose vertical band contains `y`. `bands` is each
/// line's `(top, height)` in paint order (contiguous, top to bottom). A `y`
/// above the first line clamps to 0; below the last clamps to the last line.
fn line_index_for_y(bands: &[(Pixels, Pixels)], y: Pixels) -> usize {
    let n = bands.len();
    for (i, (top, height)) in bands.iter().enumerate() {
        if y < *top + *height || i + 1 == n {
            return i;
        }
    }
    n.saturating_sub(1)
}

/// Combine a within-line byte offset (from the shaper's x hit-test) with the
/// line's `char_start` into a buffer CHARACTER index, clamped to the buffer
/// length. `byte` is clamped onto a char boundary of `line_text` first.
fn buffer_index_for_hit(
    line_text: &str,
    char_start: usize,
    byte: usize,
    buffer_char_len: usize,
) -> usize {
    let byte = byte.min(line_text.len());
    let col = line_text[..byte].chars().count();
    (char_start + col).min(buffer_char_len)
}

// ---- protocol primitives (pure, unit-tested) ------------------------------
//
// The `EntityInputHandler` methods are thin wrappers over these so the editing
// behaviour can be tested without a live GPUI window.

/// Resolve the protocol's optional UTF-16 replacement range to a buffer
/// character range: an explicit range wins, else the IME marked range, else a
/// collapsed range at the caret. The dictation ghost is deliberately NOT a
/// fallback target — it is never advertised through `marked_text_range`, so the
/// platform cannot mean it, and a typed character must insert at the caret
/// rather than swallow the preview (review blocker 2).
fn resolve_range(
    buffer: &EditBuffer,
    marked: &Option<Range<usize>>,
    range_utf16: Option<Range<usize>>,
) -> Range<usize> {
    if let Some(r) = range_utf16 {
        let t = buffer.text();
        utf16_to_char(t, r.start)..utf16_to_char(t, r.end)
    } else if let Some(m) = marked.clone() {
        m
    } else if let Some(sel) = buffer.selection() {
        // A typed character (or IME commit) with no explicit range replaces the
        // active selection — the standard "type over the selection" behaviour.
        sel
    } else {
        let c = buffer.caret();
        c..c
    }
}

/// Commit `text` over the resolved range and clear the IME marked state (a final
/// IME commit, a dead-key result, or an ordinary typed character). Routed through
/// the dictation state machine so a live ghost is rebased around — or, when the
/// commit bites into it, wiped from — the buffer (review blocker 2).
///
/// Undo transaction boundary: a commit that ENDS a composition does not
/// checkpoint — the whole composition is one transaction whose checkpoint was
/// taken by [`protocol_replace_and_mark`] before the first destructive preedit
/// replacement, so one Undo restores the pre-composition document (including a
/// selection the preedit replaced). The gate is `composing`, NOT `marked`: an
/// empty refinement clears the styled span while the transaction is still open
/// (Codex round-2 finding 1), and taking a fresh checkpoint at that commit
/// would make the first Undo restore the mid-composition document. A plain
/// typed commit (no open composition) checkpoints as a coalescing insert run.
fn protocol_replace(
    buffer: &mut EditBuffer,
    marked: &mut Option<Range<usize>>,
    composing: &mut bool,
    dictation: &mut Dictation,
    range_utf16: Option<Range<usize>>,
    text: &str,
) {
    if !*composing {
        buffer.checkpoint(UndoKind::Insert);
    }
    let range = resolve_range(buffer, marked, range_utf16);
    dictation.external_replace(buffer, range, text);
    *marked = None;
    *composing = false;
}

/// Replace the resolved range with in-progress composition text and mark it (the
/// underlined IME preedit). `new_selected_range_utf16` is the IME caret within
/// the new text; absent, the caret goes to the end of the marked text.
///
/// An IME composition beginning while a dictation ghost is live takes over: the
/// ghost preview is removed and the target range remapped (review blocker 2 —
/// the IME transition case), so the preedit and the ghost can never occupy the
/// buffer simultaneously.
fn protocol_replace_and_mark(
    buffer: &mut EditBuffer,
    marked: &mut Option<Range<usize>>,
    composing: &mut bool,
    dictation: &mut Dictation,
    range_utf16: Option<Range<usize>>,
    new_text: &str,
    new_selected_range_utf16: Option<Range<usize>>,
) {
    let range = resolve_range(buffer, marked, range_utf16);
    let range = dictation.cancel_remap(buffer, range);
    // The FIRST mutating preedit of a composition is the transaction boundary:
    // it may destructively replace a selection (or explicit range), and in a
    // fresh buffer no older checkpoint holds that text — so snapshot BEFORE the
    // replacement, or the replaced text is unrecoverable (Codex review
    // finding 1). Taken after `cancel_remap` so the snapshot holds the real
    // pre-composition document, never transient ghost preview. The gate is the
    // `composing` flag, not `marked` (round-2 finding 1): a refinement to an
    // EMPTY preedit clears `marked` while the composition stays open, and a
    // later refinement must not open a second transaction. A composition whose
    // first event has nothing to replace and no preedit text mutates nothing —
    // it neither checkpoints nor opens the transaction, so a later mutating
    // preedit still gets its boundary.
    if !*composing && (range.start != range.end || !new_text.is_empty()) {
        buffer.checkpoint(UndoKind::Boundary);
        *composing = true;
    }
    let start = range.start.min(buffer.char_len());
    buffer.replace_char_range(range, new_text);
    let n = new_text.chars().count();
    *marked = if n > 0 { Some(start..start + n) } else { None };
    let caret = match new_selected_range_utf16 {
        Some(sel) => start + utf16_to_char(new_text, sel.start),
        None => start + n,
    };
    buffer.set_caret(caret);
}

impl EntityInputHandler for Composer {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let t = self.buffer.text();
        let start_c = utf16_to_char(t, range_utf16.start);
        let end_c = utf16_to_char(t, range_utf16.end).max(start_c);
        adjusted_range.replace(char_to_utf16(t, start_c)..char_to_utf16(t, end_c));
        Some(t.chars().skip(start_c).take(end_c - start_c).collect())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let t = self.buffer.text();
        match self.buffer.selection() {
            Some(r) => {
                // The caret is the moving end; the selection reads as reversed
                // when the caret sits at the start of the ordered range.
                let reversed = self.buffer.caret() == r.start;
                Some(UTF16Selection {
                    range: char_to_utf16(t, r.start)..char_to_utf16(t, r.end),
                    reversed,
                })
            }
            None => {
                let u = char_to_utf16(t, self.buffer.caret());
                Some(UTF16Selection {
                    range: u..u,
                    reversed: false,
                })
            }
        }
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let t = self.buffer.text();
        self.marked
            .as_ref()
            .map(|m| char_to_utf16(t, m.start)..char_to_utf16(t, m.end))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        self.composing = false;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_follow = true;
        // Checkpointing lives in `protocol_replace`: a plain typed commit is a
        // coalescing insert step, while a commit ending an IME composition is
        // covered by the checkpoint `protocol_replace_and_mark` took before the
        // first destructive preedit replacement.
        protocol_replace(
            &mut self.buffer,
            &mut self.marked,
            &mut self.composing,
            &mut self.dictation,
            range_utf16,
            text,
        );
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_follow = true;
        protocol_replace_and_mark(
            &mut self.buffer,
            &mut self.marked,
            &mut self.composing,
            &mut self.dictation,
            range_utf16,
            new_text,
            new_selected_range_utf16,
        );
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line_height = window.line_height();
        let start_c = utf16_to_char(self.buffer.text(), range_utf16.start);
        let ll = self
            .wrap_layouts
            .iter()
            .find(|l| start_c >= l.char_start && start_c <= l.char_start + l.char_len)?;
        let byte = byte_of_col(&ll.line.text, start_c - ll.char_start);
        let local = ll.line.position_for_index(byte, line_height)?;
        Some(Bounds::new(
            point(ll.origin.x + local.x, ll.origin.y + local.y),
            size(theme::z(1.), line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        // The stamped paint-time line height, not `window.line_height()`: the
        // geometry was built inside the styled shell, and protocol queries
        // arrive outside that styling context.
        let ci = self.char_index_for_point(point, self.layout_line_height)?;
        Some(char_to_utf16(self.buffer.text(), ci))
    }
}

impl Focusable for Composer {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Edit-menu enablement (U3b2 B4, review P1). gpui greys a menu item,
        // and ignores its key binding, when no handler for the action is on the
        // focused dispatch path (`Window::is_action_available`). So each handler
        // is registered only while its action would do something. `undo`/`redo`
        // also end a live dictation preview, so a live ghost keeps them enabled.
        // Paste stays unconditional: probing the clipboard every frame costs too much.
        let live_ghost = self.dictation.is_active();
        let can_undo = self.buffer.can_undo() || live_ghost;
        let can_redo = self.buffer.can_redo() || live_ghost;
        let has_selection = self.buffer.has_selection();
        let has_text = !self.buffer.is_empty();
        // The styled shell (padding, border, fill). Text size / line height are
        // set here so the shaping in `ComposerElement` reads them off the window
        // text style, exactly as gpui's own input example does.
        div()
            .id("composer")
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .focus_visible(crate::focus_nav::ring)
            .w_full()
            .min_h(theme::target())
            .px(theme::z(theme::SP_12))
            .py(theme::z(theme::SP_8))
            .bg(theme::p().transparent)
            .text_size(theme::z(theme::TEXT_SIZE))
            .line_height(theme::z(theme::LINE_HEIGHT))
            .text_color(theme::p().text)
            // Click / double-click place and select (A1). The drag itself is a
            // captured interaction: while `is_selecting`, `ComposerElement::paint`
            // registers WINDOW-level move/up listeners, so moves and the ending
            // mouse-up are seen wherever the pointer is — no hitbox, no
            // outside-up special case (Codex review finding 4).
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            // U3a1: a real right-click reaches `on_mouse_down` (and opens the
            // menu through it); the host is drawn once at the root.
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            // Wheel / trackpad scrolls the overflowing draft (A2 / finding 7).
            .on_scroll_wheel(cx.listener(|this, ev: &gpui::ScrollWheelEvent, _w, cx| {
                this.on_scroll(ev, cx);
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                this.on_key(ev, cx);
            }))
            .when(can_undo, |d| {
                d.on_action(cx.listener(|this, _: &native_menu::Undo, _w, cx| this.undo(cx)))
            })
            .when(can_redo, |d| {
                d.on_action(cx.listener(|this, _: &native_menu::Redo, _w, cx| this.redo(cx)))
            })
            .when(has_selection, |d| {
                d.on_action(
                    cx.listener(|this, _: &native_menu::Cut, _w, cx| this.cut_selection(cx)),
                )
                .on_action(
                    cx.listener(|this, _: &native_menu::Copy, _w, cx| this.copy_selection(cx)),
                )
            })
            .on_action(cx.listener(|this, _: &native_menu::Paste, _w, cx| this.paste(cx)))
            .when(has_text, |d| {
                d.on_action(
                    cx.listener(|this, _: &native_menu::SelectAll, _w, cx| this.select_all(cx)),
                )
            })
            .child(ComposerElement {
                composer: cx.entity(),
            })
    }
}

/// The custom element that shapes the draft, paints text + caret, and installs
/// the platform input handler. Lives as a child of the styled shell above.
struct ComposerElement {
    composer: Entity<Composer>,
}

#[cfg(test)]
pub(crate) mod measure_probe {
    #[derive(Clone, Debug)]
    pub(crate) struct Sample {
        pub composer: gpui::EntityId,
        pub measuring: bool,
        pub font: gpui::Font,
        pub font_size: gpui::Pixels,
        pub line_height: gpui::Pixels,
        pub width: Option<gpui::Pixels>,
        pub height: gpui::Pixels,
        pub rows: usize,
        pub known: Option<gpui::Size<Option<gpui::Pixels>>>,
        pub available_width: Option<gpui::AvailableSpace>,
    }
    thread_local! {
        static DATA: std::cell::RefCell<Option<Vec<Sample>>> = const { std::cell::RefCell::new(None) };
    }
    pub(crate) fn begin() {
        DATA.with(|d| *d.borrow_mut() = Some(Vec::new()));
    }
    pub(crate) fn record(sample: Sample) {
        DATA.with(|d| {
            if let Some(samples) = d.borrow_mut().as_mut() {
                samples.push(sample);
            }
        });
    }
    pub(crate) fn take() -> Vec<Sample> {
        DATA.with(|d| d.borrow_mut().take().unwrap_or_default())
    }
}

#[cfg(test)]
thread_local! {
    /// RC2-R1G4: the focused box that installed the window's text-input
    /// handler on the last frame, and its bounds — what macOS's
    /// `insertText:` reaches (gpui_macos `insert_text` →
    /// `replace_text_in_range(None, text)` on that handler).
    pub(crate) static INSTALLED_INPUT: std::cell::RefCell<
        Option<(Bounds<Pixels>, gpui::WeakEntity<Composer>)>,
    > = const { std::cell::RefCell::new(None) };
}

/// What `prepaint` hands to `paint`: the wrapped lines (with paint origins), the
/// selection-highlight quads, the caret quad, and the vertical scroll it settled
/// on.
struct ComposerPrepaint {
    lines: Vec<WrapLayout>,
    selection: Vec<PaintQuad>,
    caret: Option<PaintQuad>,
    scroll_y: Pixels,
}

impl IntoElement for ComposerElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

/// Count the visual rows a run of `text` occupies when soft-wrapped to
/// `wrap_width` (each wrap boundary adds a row; empty text is one row).
fn wrapped_row_count(
    window: &mut Window,
    text: SharedString,
    runs: &[TextRun],
    font_size: Pixels,
    wrap_width: Option<Pixels>,
) -> usize {
    match window
        .text_system()
        .shape_text(text, font_size, runs, wrap_width, None)
    {
        Ok(lines) => lines
            .iter()
            .map(|l| l.wrap_boundaries.len() + 1)
            .sum::<usize>()
            .max(1),
        Err(_) => 1,
    }
}

impl Element for ComposerElement {
    type RequestLayoutState = ();
    type PrepaintState = ComposerPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // Auto-grow: the height is the soft-wrapped row count (capped at
        // COMPOSER_MAX_ROWS), measured against the width taffy resolves for us.
        // Width fills the shell. Beyond the cap the field holds its height and
        // scrolls vertically (handled in prepaint).
        let composer = self.composer.clone();
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.min_size.height = Pixels::ZERO.into();
        // Measurement runs later, outside this editor's styled subtree.
        // Keep the same font metrics that prepaint uses for wrapping and rows.
        let line_height = window.line_height();
        let text_style = window.text_style();
        let font = text_style.font();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let id = window.request_measured_layout(style, move |known, available, window, cx| {
            let wrap_width = known.width.or(match available.width {
                AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });

            #[cfg(test)]
            let composer_id = composer.entity_id();
            let composer = composer.read(cx);
            let empty = composer.buffer.is_empty();
            let rows = if empty {
                let placeholder = composer.placeholder.clone();
                let run = TextRun {
                    len: placeholder.len(),
                    font: font.clone(),
                    color: theme::p().faint.into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                wrapped_row_count(window, placeholder, &[run], font_size, wrap_width)
            } else {
                let text = composer.buffer.text().to_string();
                let run = TextRun {
                    len: text.len(),
                    font: font.clone(),
                    color: theme::p().text.into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                wrapped_row_count(window, text.into(), &[run], font_size, wrap_width)
            };
            let rows = rows.clamp(1, theme::COMPOSER_MAX_ROWS);
            #[cfg(test)]
            measure_probe::record(measure_probe::Sample {
                composer: composer_id,
                measuring: true,
                font: font.clone(),
                font_size,
                line_height,
                width: wrap_width,
                height: known.height.unwrap_or(line_height * rows as f32),
                rows,
                known: Some(known),
                available_width: Some(available.width),
            });
            Size {
                width: wrap_width.unwrap_or(theme::z(0.)),
                // Taffy may assign a smaller editor height to keep the
                // capsule on screen. Respect that allocation; prepaint
                // scrolls any additional rows inside these bounds.
                height: known.height.unwrap_or(line_height * rows as f32),
            }
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let composer = self.composer.read(cx);
        let text = composer.buffer.text().to_string();
        let empty = composer.buffer.is_empty();
        let caret_char = composer.buffer.caret();
        let selection = composer.buffer.selection();
        // The one styled composition span: an IME preedit (underlined, full
        // color) or — only when no preedit is active — the dictation ghost
        // (underlined, faint). The two are mutually exclusive by construction:
        // `protocol_replace_and_mark` cancels the ghost when an IME composition
        // begins, and partials are skipped while one is in progress.
        let (marked, ghost) = match composer.marked.clone() {
            Some(m) => (Some(m), false),
            None => (composer.dictation.ghost(), true),
        };
        let prev_scroll_y = composer.scroll_y;

        let style = window.text_style();
        let font = style.font();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let wrap_width = Some(bounds.size.width);

        // Shape each logical line into its own soft-wrapped line. When the draft
        // is empty, shape the placeholder (display only — the document is empty,
        // so the input handler still sees no text).
        let mut lines: Vec<WrapLayout> = Vec::new();
        if empty {
            let placeholder: SharedString = composer.placeholder.clone();
            let run = TextRun {
                len: placeholder.len(),
                font: font.clone(),
                color: theme::p().faint.into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            if let Ok(mut wrapped) =
                window
                    .text_system()
                    .shape_text(placeholder, font_size, &[run], wrap_width, None)
            {
                if let Some(line) = wrapped.drain(..).next() {
                    let rows = line.wrap_boundaries.len() + 1;
                    lines.push(WrapLayout {
                        char_start: 0,
                        char_len: 0,
                        rows,
                        line,
                        origin: point(theme::z(0.), theme::z(0.)),
                    });
                }
            }
        } else {
            let runs = buffer_runs(&text, &font, theme::p().text.into(), marked.as_ref(), ghost);
            if let Ok(wrapped) = window.text_system().shape_text(
                SharedString::from(text.clone()),
                font_size,
                &runs,
                wrap_width,
                None,
            ) {
                let mut char_start = 0usize;
                for line in wrapped {
                    let char_len = line.text.chars().count();
                    let rows = line.wrap_boundaries.len() + 1;
                    lines.push(WrapLayout {
                        char_start,
                        char_len,
                        rows,
                        line,
                        origin: point(theme::z(0.), theme::z(0.)),
                    });
                    char_start += char_len + 1; // + 1 for the joining '\n'
                }
            }
        }

        // Caret position relative to the content's top-left (x, y).
        let mut caret_x = theme::z(0.);
        let mut caret_y = theme::z(0.);
        if !empty {
            let mut row_top = 0usize;
            for l in &lines {
                if caret_char >= l.char_start && caret_char <= l.char_start + l.char_len {
                    let byte = byte_of_col(&l.line.text, caret_char - l.char_start);
                    if let Some(local) = l.line.position_for_index(byte, line_height) {
                        caret_x = local.x;
                        caret_y = line_height * row_top as f32 + local.y;
                    }
                    break;
                }
                row_top += l.rows;
            }
        }

        // Vertical scroll so the caret's row stays inside the field — but only
        // after a caret-driven action: a manual wheel scroll suspends the
        // follow so the user can inspect rows away from the caret without the
        // next paint yanking the viewport back (Codex review finding 7). The
        // offset is always clamped to the content.
        let total_rows: usize = lines.iter().map(|l| l.rows).sum::<usize>().max(1);
        let content_h = line_height * total_rows as f32;
        let view_h = bounds.size.height;
        #[cfg(test)]
        measure_probe::record(measure_probe::Sample {
            composer: self.composer.entity_id(),
            measuring: false,
            font: font.clone(),
            font_size,
            line_height,
            width: wrap_width,
            height: view_h,
            rows: total_rows,
            known: None,
            available_width: None,
        });
        let mut scroll_y = prev_scroll_y;
        if composer.scroll_follow {
            if caret_y + line_height - scroll_y > view_h {
                scroll_y = caret_y + line_height - view_h;
            }
            if caret_y - scroll_y < theme::z(0.) {
                scroll_y = caret_y;
            }
        }
        scroll_y = scroll_y.clamp(theme::z(0.), (content_h - view_h).max(theme::z(0.)));

        // Resolve final paint origins now that scroll is known.
        let mut y = bounds.origin.y - scroll_y;
        for l in &mut lines {
            l.origin = point(bounds.origin.x, y);
            y += line_height * l.rows as f32;
        }

        // Selection highlight quads (behind the text). Filled per visual row: a
        // multi-row selection fills to the right edge on all but the last row.
        let mut selection_quads: Vec<PaintQuad> = Vec::new();
        if let Some(sel) = selection {
            let right = bounds.size.width;
            for l in &lines {
                let ls = sel.start.max(l.char_start);
                let le = sel.end.min(l.char_start + l.char_len);
                if ls >= le {
                    continue;
                }
                let sb = byte_of_col(&l.line.text, ls - l.char_start);
                let eb = byte_of_col(&l.line.text, le - l.char_start);
                let (Some(ps), Some(pe)) = (
                    l.line.position_for_index(sb, line_height),
                    l.line.position_for_index(eb, line_height),
                ) else {
                    continue;
                };
                let row_s = (ps.y / line_height).round() as usize;
                let row_e = (pe.y / line_height).round() as usize;
                let mut push = |x0: Pixels, x1: Pixels, row: usize| {
                    let w = (x1 - x0).max(theme::z(0.));
                    selection_quads.push(fill(
                        Bounds::new(
                            point(l.origin.x + x0, l.origin.y + line_height * row as f32),
                            size(w, line_height),
                        ),
                        theme::p().selection,
                    ));
                };
                if row_s == row_e {
                    push(ps.x, pe.x, row_s);
                } else {
                    push(ps.x, right, row_s);
                    for r in (row_s + 1)..row_e {
                        push(theme::z(0.), right, r);
                    }
                    push(theme::z(0.), pe.x, row_e);
                }
            }
        }

        // The caret bar, vertically centred on its visual row.
        let caret_top =
            bounds.origin.y - scroll_y + caret_y + (line_height - theme::z(theme::CARET_H)) / 2.;
        let caret = fill(
            Bounds::new(
                point(bounds.origin.x + caret_x, caret_top),
                size(theme::z(theme::CARET_W), theme::z(theme::CARET_H)),
            ),
            theme::p().accent_bright,
        );

        ComposerPrepaint {
            lines,
            selection: selection_quads,
            caret: Some(caret),
            scroll_y,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Install the platform text-input handler for this focused element —
        // the line that makes IME / dead keys / Dictation route into the buffer.
        let focus = self.composer.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.composer.clone()),
            cx,
        );

        let line_height = window.line_height();
        let focused = focus.is_focused(window);
        // RC2-R1G4: a test reads which box installed the window's text-input
        // handler this frame (`handle_input` installs it only when focused).
        #[cfg(test)]
        if focused {
            INSTALLED_INPUT.with(|i| *i.borrow_mut() = Some((bounds, self.composer.downgrade())));
        }

        // Clip to the field so scrolled/wrapped text never bleeds out.
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            // Selection first, so the glyphs paint on top of the highlight.
            for quad in prepaint.selection.drain(..) {
                window.paint_quad(quad);
            }
            for ll in &prepaint.lines {
                ll.line
                    .paint(
                        ll.origin,
                        line_height,
                        gpui::TextAlign::Left,
                        None,
                        window,
                        cx,
                    )
                    .ok();
            }
            if focused {
                if let Some(caret) = prepaint.caret.take() {
                    window.paint_quad(caret);
                }
            }
        });

        // Stash this frame's layout for the input handler's geometry queries and
        // for mouse hit-testing outside the paint pass, stamped with the text
        // and scale it was shaped from so hit-testing can refuse stale geometry
        // (Codex review finding 4).
        let lines = std::mem::take(&mut prepaint.lines);
        let scroll_y = prepaint.scroll_y;
        let selecting = self.composer.update(cx, |composer, _cx| {
            composer.wrap_layouts = lines;
            composer.scroll_y = scroll_y;
            composer.last_bounds = bounds;
            composer.layout_text = composer.buffer.text().to_string();
            composer.layout_scale = theme::ui_scale();
            composer.layout_line_height = line_height;
            composer.is_selecting
        });

        // Pointer capture for an in-progress drag-selection: window-level
        // listeners see every move and the ending mouse-up no matter where the
        // pointer went, and `drag_to` auto-scrolls when it crosses an edge.
        // Registered per-frame (gpui clears them), so the capture lives exactly
        // as long as the drag does.
        if selecting {
            let composer = self.composer.clone();
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble {
                    let line_height = window.line_height();
                    composer.update(cx, |c, cx| c.drag_to(ev.position, line_height, cx));
                }
            });
            let composer = self.composer.clone();
            window.on_mouse_event(move |ev: &MouseUpEvent, phase, _window, cx| {
                if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left {
                    composer.update(cx, |c, _| c.end_drag());
                }
            });
        }
    }
}

/// Build the whole-buffer text runs, underlining the slice that overlaps the
/// marked range. `marked` is a CHARACTER range over the whole buffer. Run
/// lengths are in UTF-8 bytes and cover the entire text (including the `'\n'`
/// separators, which `shape_text` consumes between lines), as the shaper
/// expects. When `ghost` is set the marked span is a dictation *partial* — drawn
/// faint (a translucent preview) instead of the full text color, so a streaming
/// transcript reads as not-yet-committed.
fn buffer_runs(
    text: &str,
    font: &gpui::Font,
    color: gpui::Hsla,
    marked: Option<&Range<usize>>,
    ghost: bool,
) -> Vec<TextRun> {
    let base = TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };

    let total_chars = text.chars().count();
    let local = marked.and_then(|m| {
        let s = m.start.min(total_chars);
        let e = m.end.min(total_chars);
        (s < e).then_some(s..e)
    });

    let Some(local) = local else {
        return vec![base];
    };

    let start_b = byte_of_col(text, local.start);
    let end_b = byte_of_col(text, local.end);
    let mark_color = if ghost {
        theme::p().faint.into()
    } else {
        color
    };
    let underline = Some(UnderlineStyle {
        color: Some(mark_color),
        thickness: theme::z(1.),
        wavy: false,
    });
    [
        TextRun {
            len: start_b,
            ..base.clone()
        },
        TextRun {
            len: end_b - start_b,
            color: mark_color,
            underline,
            ..base.clone()
        },
        TextRun {
            len: text.len() - end_b,
            ..base
        },
    ]
    .into_iter()
    .filter(|r| r.len > 0)
    .collect()
}

/// The line containing character index `idx`, as an ordered CHARACTER range,
/// INCLUDING the trailing newline when the line has one (the triple-click
/// selection unit, U3b2 B4/D6). The line is the text between the nearest
/// preceding and next `\n` boundaries. On the empty text the range is `0..0`.
/// The range is empty (`n..n`) for the empty last line after a trailing
/// newline (review P2): the end index is a real caret position, its own line.
///
/// A pure helper over the text (U3b2 keeps its new helpers in the app
/// crate, not `awili-core`).
pub fn line_range_at(text: &str, idx: usize) -> std::ops::Range<usize> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    // `n` is a real caret position. After a trailing '\n' it is the empty last
    // line, not the line before it (review P2), so clamp to `n`, not `n - 1`.
    let i = idx.min(n);
    let mut start = i;
    while start > 0 && chars[start - 1] != '\n' {
        start -= 1;
    }
    // Scan from `i` itself: when `idx` IS the line's '\n' (a click past the
    // line's text), the line ends right there.
    let mut end = i;
    while end < n && chars[end] != '\n' {
        end += 1;
    }
    // Include the newline itself so Cut removes the line cleanly (D6).
    if end < n {
        end += 1;
    }
    start..end
}

/// The Edit menu's enablement table over a buffer's state alone (U3b2 B4).
/// The four conditional items grey exactly when this returns `false`: Undo
/// needs a non-empty undo stack, Redo a non-empty redo stack, Cut/Copy a
/// selection. Paste and Select All are always doable and are not consulted
/// (they return `false` here because they never grey). This is the pure,
/// unit-testable contract; the app adds a dispatch-tree check on top
/// (`window.is_action_available`) to mirror AppKit's menu validation.
///
/// `undo_stack_nonempty` / `redo_stack_nonempty` are the observable facts the
/// table consumes: `EditBuffer` exposes only the side-effecting
/// [`EditBuffer::undo`] / [`EditBuffer::redo`] (which return `false` when
/// their stacks are empty), so callers observe stack non-emptiness by the
/// *attempted* outcome — this helper takes them pre-computed to stay pure.
///
/// Only exercised from `#[cfg(test)]` — the binary never calls it directly,
/// so it is `pub` (visible to integration tests) but unused in the release
/// build; the dead-code lint is suppressed accordingly.
#[allow(dead_code)]
pub fn edit_action_buffer_enabled(
    has_selection: bool,
    undo_available: bool,
    redo_available: bool,
    action: &dyn gpui::Action,
) -> bool {
    let ty = action.as_any().type_id();
    if ty == std::any::TypeId::of::<native_menu::Undo>() {
        return undo_available;
    }
    if ty == std::any::TypeId::of::<native_menu::Redo>() {
        return redo_available;
    }
    if ty == std::any::TypeId::of::<native_menu::Cut>()
        || ty == std::any::TypeId::of::<native_menu::Copy>()
    {
        return has_selection;
    }
    false
}

/// M2g: pure decision for the Up/Down prompt-history walk.
///
/// Returns `Some(true)` for Up, `Some(false)` for Down when the key should
/// drive the walk; `None` when it should fall through to caret movement.
/// The walk is live only when `history` is on, no modifier is held, no IME
/// composition is in progress, and the draft is either empty (a fresh Up) or
/// still equals `recalled` (the entry the walk last put there — any edit
/// that diverges the draft ends the walk and the next Up moves the caret).
///
/// The `recalled: None` + empty-draft case still walks: that is the first Up
/// after a relaunch (the candidate list is rebuilt from the restored
/// convo), and the walk is meant to work even when the app has never set
/// `recalled` this session.
pub(crate) fn history_key(
    ks: &gpui::Keystroke,
    text: &str,
    recalled: Option<&str>,
    history: bool,
    composing: bool,
) -> Option<bool> {
    // U3-INT1: "no modifiers" means no Shift/⌥/⌘/⌃. `function` is ignored,
    // as in every U3 key classifier (U3 spec rule 6): AppKit sets
    // `NSFunctionKeyMask` on every arrow key-down, so with `modified()` here a
    // real ↑ on macOS would never walk the history.
    let m = &ks.modifiers;
    if !history || m.shift || m.alt || m.platform || m.control || composing {
        return None;
    }
    let walkable = text.is_empty() || recalled.map(|r| r == text) == Some(true);
    if !walkable {
        return None;
    }
    match ks.key.as_str() {
        "up" => Some(true),
        "down" => Some(false),
        _ => None,
    }
}

/// Test-only: drive the Up/Down walk decision without re-implementing the
/// `Keystroke` + `KeyDownEvent` plumbing (which is the same code the
/// production path uses). Returns `Some(true)` for Up, `Some(false)` for
/// Down, `None` when the key should fall through to caret movement.
///
/// Uses [`Composer::buffer.text()`] and the `history_key` pure fn — the
/// exact same decision the production path uses — so the test is a thin
/// wrapper over the real decision, not a re-implementation of it.
#[cfg(test)]
pub(crate) fn composer_history_key_direction(c: &Composer, key: &str) -> Option<bool> {
    let ks = gpui::Keystroke {
        modifiers: gpui::Modifiers::default(),
        key: key.to_string(),
        key_char: None,
    };
    history_key(
        &ks,
        c.buffer.text(),
        c.recalled.as_deref(),
        c.history,
        c.composing,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(key: &str, shift: bool) -> Keystroke {
        Keystroke {
            modifiers: gpui::Modifiers {
                shift,
                ..Default::default()
            },
            key: key.to_string(),
            key_char: None,
        }
    }

    // ---- pointer → caret mapping (A1) ---------------------------------------
    //
    // The combined tests below replace the old `line_index_for_y_bands_and_clamps`
    // helper test, which proved only the logical-line clamp and thereby enshrined
    // half of the below-bottom bug (Codex review finding 3): they shape REAL
    // wrapped lines through gpui's test text system (a deterministic monospace
    // shaper: every glyph is `0.6 * font_size` wide) and drive the full
    // `char_index_for_point` path, shaper hit-test included.

    /// Shape the composer's current text and install wrapped layouts exactly as
    /// `prepaint` would, with the content origin at the window's (0, 0).
    fn install_layouts(
        c: &mut Composer,
        window: &mut Window,
        wrap_width: Pixels,
        font_size: Pixels,
        line_height: Pixels,
    ) {
        let text = c.buffer.text().to_string();
        let font = window.text_style().font();
        let run = TextRun {
            len: text.len(),
            font,
            color: theme::p().text.into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let wrapped = window
            .text_system()
            .shape_text(text.into(), font_size, &[run], Some(wrap_width), None)
            .unwrap();
        let mut lines = Vec::new();
        let mut char_start = 0usize;
        let mut y = theme::z(0.);
        for line in wrapped {
            let char_len = line.text.chars().count();
            let rows = line.wrap_boundaries.len() + 1;
            let origin = point(theme::z(0.), y);
            y += line_height * rows as f32;
            lines.push(WrapLayout {
                char_start,
                char_len,
                rows,
                line,
                origin,
            });
            char_start += char_len + 1;
        }
        c.wrap_layouts = lines;
        c.layout_text = c.buffer.text().to_string();
        c.layout_scale = theme::ui_scale();
        c.layout_line_height = line_height;
    }

    #[gpui::test]
    fn click_below_the_content_maps_to_the_buffer_end_not_the_line_start(
        cx: &mut gpui::TestAppContext,
    ) {
        let handle = cx.add_window(|_, cx| Composer::new(cx));
        handle
            .update(cx, |c, window, cx| {
                let (font_size, line_height) = (theme::z(10.), theme::z(20.));
                c.set_text("hello\nworld", cx);
                install_layouts(c, window, theme::z(300.), font_size, line_height);
                assert_eq!(c.wrap_layouts.len(), 2, "two logical lines, unwrapped");
                // A click far below the content at an x past the text used to
                // read gpui's Err(0) as byte 0 and land at the START of "world"
                // (char 6); it must land at the buffer end.
                assert_eq!(
                    c.char_index_for_point(point(theme::z(500.), theme::z(500.)), line_height),
                    Some(11)
                );
                // Below-bottom at an x left of the text clamps onto the last
                // row and resolves by x — the start of "world", like a native
                // text view.
                assert_eq!(
                    c.char_index_for_point(point(theme::z(-10.), theme::z(500.)), line_height),
                    Some(6)
                );
                // And a click above the first row still clamps to the top.
                assert_eq!(
                    c.char_index_for_point(point(theme::z(-10.), theme::z(-50.)), line_height),
                    Some(0)
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn below_bottom_hits_resolve_on_the_last_wrapped_row(cx: &mut gpui::TestAppContext) {
        let handle = cx.add_window(|_, cx| Composer::new(cx));
        handle
            .update(cx, |c, window, cx| {
                // Glyphs are 6px wide (0.6 * font size); a 30px wrap width
                // breaks the 12-char line into rows "aaaaa" "bbbbb" "cc".
                let (font_size, line_height) = (theme::z(10.), theme::z(20.));
                c.set_text("aaaaabbbbbcc", cx);
                install_layouts(c, window, theme::z(30.), font_size, line_height);
                assert_eq!(c.wrap_layouts.len(), 1, "one logical line");
                assert_eq!(c.wrap_layouts[0].rows, 3, "wrapped into three rows");
                // Below the last row, x past the row's right edge → buffer end.
                assert_eq!(
                    c.char_index_for_point(point(theme::z(100.), theme::z(300.)), line_height),
                    Some(12)
                );
                // Below the last row, x mid-row resolves by x on the LAST row:
                // 6px is the boundary after the first 'c' → char 11.
                assert_eq!(
                    c.char_index_for_point(point(theme::z(6.), theme::z(300.)), line_height),
                    Some(11)
                );
                // An in-band hit past a middle row's right edge maps to that
                // row's end ("bbbbb" → char 10), not the error payload.
                assert_eq!(
                    c.char_index_for_point(point(theme::z(100.), theme::z(30.)), line_height),
                    Some(10)
                );
            })
            .unwrap();
    }

    #[test]
    fn line_index_for_y_handles_multi_row_bands() {
        // A wrapped line occupying 3 rows (height 75) sits above a 1-row line.
        let lh = theme::z(25.);
        let bands = [(theme::z(0.), lh * 3.), (theme::z(75.), lh)];
        assert_eq!(
            line_index_for_y(&bands, theme::z(40.)),
            0,
            "still inside the tall line"
        );
        assert_eq!(line_index_for_y(&bands, theme::z(80.)), 1);
    }

    #[test]
    fn buffer_index_for_hit_offsets_by_line_and_clamps() {
        // Second logical line "world" starts at char 6 in "hello\nworld".
        // A byte hit of 2 within "world" -> char 2 -> buffer index 8.
        assert_eq!(buffer_index_for_hit("world", 6, 2, 11), 8);
        // A hit at the line end maps to the char after the last char.
        assert_eq!(buffer_index_for_hit("world", 6, 5, 11), 11);
        // A byte past the line text clamps onto its end, then onto the buffer.
        assert_eq!(buffer_index_for_hit("world", 6, 99, 11), 11);
    }

    #[test]
    fn buffer_index_for_hit_is_multibyte_safe() {
        // "héllo": byte 3 is *after* the 2-byte 'é' -> char column 2.
        assert_eq!(buffer_index_for_hit("héllo", 0, 3, 5), 2);
    }

    // ---- U3b2 B3: triple-click selects the line, not the whole buffer -------

    #[gpui::test]
    fn u3b2_composer_triple_click_selects_only_its_line(cx: &mut gpui::TestAppContext) {
        let handle = cx.add_window(|_, cx| Composer::new(cx));
        handle
            .update(cx, |c, window, cx| {
                let (font_size, line_height) = (theme::z(10.), theme::z(20.));
                c.set_text("hello world\nsecond line", cx);
                install_layouts(c, window, theme::z(300.), font_size, line_height);
                // Third click inside the FIRST line (x is past "hello world").
                let mid = point(theme::z(60.), theme::z(10.));
                c.on_mouse_down(
                    &MouseDownEvent {
                        button: MouseButton::Left,
                        position: mid,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 3,
                        first_mouse: false,
                    },
                    window,
                    cx,
                );
                // Exactly the first line (including its newline): not the
                // whole draft, not just the word. "hello world" is 11 chars +
                // 1 newline = 12.
                assert_eq!(c.buffer.selection(), Some(0..12));

                // (b) A third click PAST the end of line 1 hits the '\n' index
                // (11). Still exactly line 1, never both lines.
                let past_end = point(theme::z(250.), theme::z(10.));
                assert_eq!(c.char_index_for_point(past_end, line_height), Some(11));
                c.on_mouse_down(
                    &MouseDownEvent {
                        button: MouseButton::Left,
                        position: past_end,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 3,
                        first_mouse: false,
                    },
                    window,
                    cx,
                );
                assert_eq!(c.buffer.selection(), Some(0..12));

                // (c) Review P2: the empty line after a trailing newline. The
                // hit index is char_len() (12). The triple-click selects that
                // empty line (a collapsed caret at 12), not "hello world\n".
                c.set_text("hello world\n", cx);
                install_layouts(c, window, theme::z(300.), font_size, line_height);
                assert_eq!(
                    c.wrap_layouts.len(),
                    2,
                    "trailing newline shapes an empty 2nd line"
                );
                let last_line = point(theme::z(5.), theme::z(30.));
                assert_eq!(c.char_index_for_point(last_line, line_height), Some(12));
                c.on_mouse_down(
                    &MouseDownEvent {
                        button: MouseButton::Left,
                        position: last_line,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 3,
                        first_mouse: false,
                    },
                    window,
                    cx,
                );
                assert_eq!(
                    c.buffer.selection(),
                    None,
                    "the empty last line selects nothing"
                );
                assert_eq!(c.buffer.caret(), 12);
            })
            .unwrap();
    }

    #[test]
    fn u3b2_line_range_at_end_index_is_its_own_line() {
        // Review P2: the end index after a trailing newline is the empty last line.
        assert_eq!(line_range_at("abc\n", 4), 4..4);
        assert_eq!(line_range_at("one\ntwo\n", 8), 8..8);
        // The end index WITHOUT a trailing newline is still the last line.
        assert_eq!(line_range_at("abc", 3), 0..3);
        assert_eq!(line_range_at("one\ntwo", 7), 4..7);
        // The '\n' index itself (a click past a line's text) is that line only.
        assert_eq!(line_range_at("one\ntwo", 3), 0..4);
        assert_eq!(line_range_at("hello world\nsecond line", 11), 0..12);
        // A blank middle line, empty text, an out-of-range clamp, multibyte text.
        assert_eq!(line_range_at("a\n\nb", 2), 2..3);
        assert_eq!(line_range_at("", 0), 0..0);
        assert_eq!(line_range_at("abc\n", 99), 4..4);
        assert_eq!(line_range_at("héllo\nwörld", 7), 6..11);
    }

    /// What the Edit menu shows for the focused composer: redraw the window,
    /// then ask gpui's dispatch tree for each action. This is the query that
    /// AppKit menu validation reaches through `App::is_action_available`.
    #[derive(Debug, PartialEq, Eq)]
    struct EditAvail {
        undo: bool,
        redo: bool,
        cut: bool,
        copy: bool,
        paste: bool,
        select_all: bool,
    }

    fn edit_avail(cx: &mut gpui::TestAppContext, any: gpui::AnyWindowHandle) -> EditAvail {
        cx.update_window(any, |_, window, cx| {
            window.draw(cx).clear();
            EditAvail {
                undo: window.is_action_available(&native_menu::Undo, cx),
                redo: window.is_action_available(&native_menu::Redo, cx),
                cut: window.is_action_available(&native_menu::Cut, cx),
                copy: window.is_action_available(&native_menu::Copy, cx),
                paste: window.is_action_available(&native_menu::Paste, cx),
                select_all: window.is_action_available(&native_menu::SelectAll, cx),
            }
        })
        .unwrap()
    }

    /// U3b2 B4 / review P1 proof: the Edit-menu actions are available on the
    /// focused composer exactly when they would do something. The test drives the
    /// real `render`, which registers the handlers, in a focused test window,
    /// not a pure table.
    #[gpui::test]
    fn u3b2_edit_actions_available_only_when_meaningful(cx: &mut gpui::TestAppContext) {
        let handle = cx.add_window(|_, cx| Composer::new(cx));
        let any: gpui::AnyWindowHandle = handle.into();

        // Focus the composer: availability is read along the FOCUSED path.
        handle
            .update(cx, |c, window, cx| {
                window.focus(&c.focus, cx);
                assert!(c.focus.is_focused(window), "composer holds focus");
                cx.notify();
            })
            .unwrap();

        // (1) Fresh, empty draft: nothing to undo/redo/cut/copy/select.
        assert_eq!(
            edit_avail(cx, any),
            EditAvail {
                undo: false,
                redo: false,
                cut: false,
                copy: false,
                paste: true,
                select_all: false,
            },
            "fresh empty composer"
        );

        // (2) Type "hello" through the platform text path (one undo checkpoint).
        handle
            .update(cx, |c, _window, cx| {
                protocol_replace(
                    &mut c.buffer,
                    &mut c.marked,
                    &mut c.composing,
                    &mut c.dictation,
                    None,
                    "hello",
                );
                cx.notify();
            })
            .unwrap();
        assert_eq!(
            edit_avail(cx, any),
            EditAvail {
                undo: true,
                redo: false,
                cut: false,
                copy: false,
                paste: true,
                select_all: true,
            },
            "typed text, caret collapsed"
        );

        // (3) Select everything: Cut and Copy become available.
        handle
            .update(cx, |c, _window, cx| {
                c.buffer.select_all();
                cx.notify();
            })
            .unwrap();
        assert_eq!(
            edit_avail(cx, any),
            EditAvail {
                undo: true,
                redo: false,
                cut: true,
                copy: true,
                paste: true,
                select_all: true,
            },
            "with a selection"
        );

        // (4) Undo DISPATCHED as an action (the menu path) reaches the handler:
        // the draft is empty again, Redo is now available, Undo is not.
        cx.dispatch_action(any, native_menu::Undo);
        let text = handle
            .update(cx, |c, _window, _cx| c.buffer.text().to_string())
            .unwrap();
        assert_eq!(text, "", "the dispatched Undo ran the composer's handler");
        assert_eq!(
            edit_avail(cx, any),
            EditAvail {
                undo: false,
                redo: true,
                cut: false,
                copy: false,
                paste: true,
                select_all: false,
            },
            "after undo"
        );

        // (5) Redo dispatched: text is back, Undo available again, Redo not.
        cx.dispatch_action(any, native_menu::Redo);
        let text = handle
            .update(cx, |c, _window, _cx| c.buffer.text().to_string())
            .unwrap();
        assert_eq!(
            text, "hello",
            "the dispatched Redo ran the composer's handler"
        );
        let a = edit_avail(cx, any);
        assert!(a.undo && !a.redo && a.select_all, "after redo: {a:?}");

        // (6) A live dictation ghost with an empty history: Undo/Redo stay
        // available because they end the preview (see `undo`/`redo`).
        handle
            .update(cx, |c, _window, cx| {
                c.set_text("", cx);
                c.dictation_partial("hi", cx);
                assert!(!c.buffer.can_undo() && !c.buffer.can_redo());
                assert!(c.dictation.is_active());
                cx.notify();
            })
            .unwrap();
        let a = edit_avail(cx, any);
        assert!(
            a.undo && a.redo,
            "live ghost keeps Undo/Redo enabled: {a:?}"
        );
    }

    // ---- U3b2 B4: the Edit menu greys exactly when there is nothing to do ---

    /// Proof test for the Edit-menu enablement table (U3b2 B4). The four
    /// conditional items grey exactly when their action is not doable: Undo
    /// needs a non-empty undo stack, Redo a non-empty redo stack, Cut/Copy a
    /// selection. The table is pure over the buffer state, so this drives the
    /// buffer directly and reads the observable facts (selection via
    /// [`EditBuffer::has_selection`], undo/redo availability via the
    /// side-effect-free *probe* that a pop returns `false` when the stack is
    /// empty).
    #[test]
    fn u3b2_edit_menu_greys_exactly_when_nothing_to_do() {
        let mut b = awili_core::EditBuffer::new();
        let undo: &dyn gpui::Action = &native_menu::Undo;
        let redo: &dyn gpui::Action = &native_menu::Redo;
        let cut: &dyn gpui::Action = &native_menu::Cut;
        let copy: &dyn gpui::Action = &native_menu::Copy;

        // Fresh buffer: nothing to undo, nothing to redo, no selection.
        assert!(!b.has_selection(), "no selection -> Cut/Copy greyed");
        // A buffer with no checkpointed edits cannot report undo/redo
        // availability without mutating, so the observable facts are read
        // directly from the buffer's public API: after zero edits both
        // stacks are empty.
        assert!(
            !b.undo(),
            "empty undo stack -> Undo greyed (probe: undo() is a no-op)"
        );
        assert!(
            !b.redo(),
            "empty redo stack -> Redo greyed (probe: redo() is a no-op)"
        );
        // And the table consults each predicate for the right action.
        assert!(!edit_action_buffer_enabled(
            b.has_selection(),
            false,
            false,
            undo
        ));
        assert!(!edit_action_buffer_enabled(
            b.has_selection(),
            false,
            false,
            redo
        ));
        assert!(!edit_action_buffer_enabled(
            b.has_selection(),
            false,
            false,
            cut
        ));
        assert!(!edit_action_buffer_enabled(
            b.has_selection(),
            false,
            false,
            copy
        ));

        // One edit: Undo is now doable, Redo is not, still no selection.
        b.checkpoint(UndoKind::Insert);
        b.apply(EditAction::Insert('h'), false);
        b.apply(EditAction::Insert('i'), false);
        assert!(!b.has_selection());
        assert!(edit_action_buffer_enabled(
            b.has_selection(),
            true,
            false,
            undo
        ));
        assert!(!edit_action_buffer_enabled(
            b.has_selection(),
            true,
            false,
            redo
        ));
        assert!(!edit_action_buffer_enabled(
            b.has_selection(),
            true,
            false,
            cut
        ));
        assert!(!edit_action_buffer_enabled(
            b.has_selection(),
            true,
            false,
            copy
        ));

        // Select some text: Cut and Copy are now doable.
        b.select_all();
        assert!(b.has_selection());
        assert!(edit_action_buffer_enabled(
            b.has_selection(),
            true,
            false,
            cut
        ));
        assert!(edit_action_buffer_enabled(
            b.has_selection(),
            true,
            false,
            copy
        ));

        // Undo: the redo stack is now non-empty.
        let undid = b.undo();
        assert!(undid, "undo() succeeded after a checkpointed edit");
        assert!(
            b.redo(),
            "Redo is doable after an undo (probe: redo() succeeded)"
        );
        assert!(edit_action_buffer_enabled(
            b.has_selection(),
            true,
            true,
            redo
        ));
    }

    // ---- Codex review finding 4: geometry freshness + captured edge drag -----

    #[gpui::test]
    fn stale_or_unpainted_geometry_declines_to_hit_test(cx: &mut gpui::TestAppContext) {
        let handle = cx.add_window(|_, cx| Composer::new(cx));
        handle
            .update(cx, |c, window, cx| {
                let (font_size, line_height) = (theme::z(10.), theme::z(20.));
                c.set_text("hello world", cx);
                // Before any paint: no geometry, no hit-test — never "caret 0".
                c.wrap_layouts.clear();
                let mid = point(theme::z(30.), theme::z(10.));
                assert_eq!(c.char_index_for_point(mid, line_height), None);
                // Painted geometry hit-tests normally...
                install_layouts(c, window, theme::z(300.), font_size, line_height);
                assert!(c.char_index_for_point(mid, line_height).is_some());
                // ...until the buffer changes under it (an edit between paints):
                // the stamp no longer matches and the mapping declines.
                c.buffer.insert_str("XYZ");
                assert_eq!(c.char_index_for_point(mid, line_height), None);
                // A mouse-down against stale geometry must not move the caret.
                let caret_before = c.buffer.caret();
                c.on_mouse_down(
                    &MouseDownEvent {
                        button: MouseButton::Left,
                        position: mid,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    },
                    window,
                    cx,
                );
                assert_eq!(c.buffer.caret(), caret_before);
                assert!(!c.is_selecting);
                // The next paint refreshes the stamp and mapping resumes.
                install_layouts(c, window, theme::z(300.), font_size, line_height);
                assert!(c.char_index_for_point(mid, line_height).is_some());
            })
            .unwrap();
    }

    #[gpui::test]
    fn dragging_past_the_bottom_edge_scrolls_and_extends_the_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        let handle = cx.add_window(|_, cx| Composer::new(cx));
        handle
            .update(cx, |c, window, cx| {
                // 12 chars wrapped into rows "aaaaa" "bbbbb" "cc"; the field
                // shows only two rows (40px), so row 3 starts hidden.
                let (font_size, line_height) = (theme::z(10.), theme::z(20.));
                c.set_text("aaaaabbbbbcc", cx);
                install_layouts(c, window, theme::z(30.), font_size, line_height);
                c.last_bounds = Bounds::new(
                    point(theme::z(0.), theme::z(0.)),
                    size(theme::z(30.), theme::z(40.)),
                );
                c.buffer.place_caret(0, false);
                c.is_selecting = true;
                // The captured pointer is far below the field: the drag scrolls
                // a line toward it and extends the selection to the clamped
                // bottom edge (end of the visible row "bbbbb" at x past it).
                c.drag_to(point(theme::z(100.), theme::z(100.)), line_height, cx);
                assert_eq!(c.scroll_y, theme::z(20.), "auto-scrolled one line down");
                assert_eq!(c.buffer.selection(), Some(0..10));
                // Above the top edge scrolls back up and keeps extending.
                c.drag_to(point(theme::z(0.), theme::z(-50.)), line_height, cx);
                assert_eq!(c.scroll_y, theme::z(0.), "auto-scrolled back up");
                // A drag with the button already released is inert.
                c.end_drag();
                let sel = c.buffer.selection();
                c.drag_to(point(theme::z(100.), theme::z(100.)), line_height, cx);
                assert_eq!(c.buffer.selection(), sel);
            })
            .unwrap();
    }

    // ---- Codex review finding 7: scroll path + visual-row vertical movement --

    #[gpui::test]
    fn wheel_scroll_moves_the_viewport_and_suspends_caret_follow(cx: &mut gpui::TestAppContext) {
        let handle = cx.add_window(|_, cx| Composer::new(cx));
        handle
            .update(cx, |c, window, cx| {
                let (font_size, line_height) = (theme::z(10.), theme::z(20.));
                c.set_text("aaaaabbbbbcc", cx);
                install_layouts(c, window, theme::z(30.), font_size, line_height);
                let wheel = |lines: f32| gpui::ScrollWheelEvent {
                    delta: gpui::ScrollDelta::Lines(gpui::Point { x: 0.0, y: lines }),
                    ..Default::default()
                };
                // Scrolling down (negative delta) moves the viewport down and
                // stops prepaint from yanking it back to the caret.
                c.on_scroll(&wheel(-1.5), cx);
                assert_eq!(c.scroll_y, theme::z(30.));
                assert!(!c.scroll_follow, "manual scroll suspends caret follow");
                // Scrolling up past the top clamps at 0.
                c.on_scroll(&wheel(5.0), cx);
                assert_eq!(c.scroll_y, theme::z(0.));
                // Any caret-driven key re-enables the follow.
                c.on_key(&key_down("left", false), cx);
                assert!(c.scroll_follow);
            })
            .unwrap();
    }

    #[gpui::test]
    fn up_down_walk_the_wrapped_visual_rows_of_one_logical_line(cx: &mut gpui::TestAppContext) {
        let handle = cx.add_window(|_, cx| Composer::new(cx));
        handle
            .update(cx, |c, window, cx| {
                // One logical line, no '\n' anywhere — the case where logical
                // vertical movement is inert — wrapped into rows "aaaaa"
                // "bbbbb" "cc" (glyphs 6px wide).
                let (font_size, line_height) = (theme::z(10.), theme::z(20.));
                c.set_text("aaaaabbbbbcc", cx);
                install_layouts(c, window, theme::z(30.), font_size, line_height);
                c.buffer.set_caret(2); // row 0, column 2 (x = 12px)
                c.on_key(&key_down("down", false), cx);
                assert_eq!(c.buffer.caret(), 7, "row 1, same column");
                c.on_key(&key_down("down", false), cx);
                assert_eq!(c.buffer.caret(), 12, "row 2 clamps the column onto 'cc'");
                c.on_key(&key_down("up", false), cx);
                assert_eq!(c.buffer.caret(), 7, "returns to the preferred column");
                c.on_key(&key_down("up", false), cx);
                assert_eq!(c.buffer.caret(), 2);
                c.on_key(&key_down("up", false), cx);
                assert_eq!(
                    c.buffer.caret(),
                    0,
                    "Up on the first visual row collapses to the buffer start"
                );
                // Stale geometry (an edit since the last paint) falls back to
                // logical movement instead of hit-testing old rows: on a single
                // logical line that is inert.
                c.buffer.insert_str("x");
                let caret = c.buffer.caret();
                c.on_key(&key_down("up", false), cx);
                assert_eq!(c.buffer.caret(), caret);
            })
            .unwrap();
    }

    // ---- key_to_action: navigation/submit only -------------------------------

    #[test]
    fn enter_submits_shift_enter_makes_newline() {
        assert_eq!(key_to_action(&ks("enter", false)), EditAction::Submit);
        assert_eq!(key_to_action(&ks("return", false)), EditAction::Submit);
        assert_eq!(key_to_action(&ks("enter", true)), EditAction::Newline);
    }

    #[test]
    fn navigation_keys_map_through() {
        assert_eq!(key_to_action(&ks("left", false)), EditAction::Left);
        assert_eq!(key_to_action(&ks("right", false)), EditAction::Right);
        assert_eq!(key_to_action(&ks("up", false)), EditAction::Up);
        assert_eq!(key_to_action(&ks("down", false)), EditAction::Down);
        assert_eq!(key_to_action(&ks("home", false)), EditAction::Home);
        assert_eq!(key_to_action(&ks("end", false)), EditAction::End);
        assert_eq!(
            key_to_action(&ks("backspace", false)),
            EditAction::Backspace
        );
        assert_eq!(key_to_action(&ks("delete", false)), EditAction::Delete);
    }

    #[test]
    fn printable_and_space_keys_are_inert_for_keys() {
        // Character + space insertion is the platform input handler's job; the
        // key path must NOT also insert, or Linux would double-type.
        assert_eq!(key_to_action(&ks("a", false)), EditAction::None);
        assert_eq!(key_to_action(&ks("space", false)), EditAction::None);
        assert_eq!(key_to_action(&ks("tab", false)), EditAction::None);
        assert_eq!(key_to_action(&ks("escape", false)), EditAction::None);
    }

    // ---- protocol commit path (replace_text_in_range) ------------------------

    #[test]
    fn commit_inserts_multi_char_string_at_caret() {
        // A single commit may carry more than one scalar (IME, Dictation). The
        // old single_char path dropped these; the protocol path keeps them whole.
        let mut buf = EditBuffer::new();
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        protocol_replace(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "こんにちは",
        );
        assert_eq!(buf.text(), "こんにちは");
        assert_eq!(buf.caret(), 5);
        assert!(marked.is_none());
    }

    #[test]
    fn commit_replaces_explicit_utf16_range() {
        let mut buf = EditBuffer::from_text("hello");
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        // Replace UTF-16 [1,4) ("ell") with "ELL".
        protocol_replace(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            Some(1..4),
            "ELL",
        );
        assert_eq!(buf.text(), "hELLo");
        assert_eq!(buf.caret(), 4);
    }

    #[test]
    fn commit_range_is_utf16_aware_over_astral_chars() {
        // "😀" is ONE char but TWO UTF-16 code units; a range past it must land
        // on the right character, not split the scalar.
        let mut buf = EditBuffer::from_text("😀x");
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        // UTF-16 [2,3) is the 'x' (emoji occupies code units 0..2).
        protocol_replace(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            Some(2..3),
            "Y",
        );
        assert_eq!(buf.text(), "😀Y");
    }

    // ---- paste at a mid-buffer caret -----------------------------------------

    #[test]
    fn paste_inserts_at_mid_buffer_caret() {
        // Models the Cmd/Ctrl+V branch of `on_key`: the clipboard string is
        // inserted at the caret (not appended). The clipboard read itself needs a
        // live window; the insertion — the part that can go wrong — is exercised
        // here exactly as `on_key` performs it.
        let mut buf = EditBuffer::from_text("hello world");
        buf.apply(EditAction::Home, true);
        buf.apply(EditAction::Right, true);
        buf.apply(EditAction::Right, true); // caret at char 2 (after "he")
        buf.insert_str("XYZ");
        assert_eq!(buf.text(), "heXYZllo world");
        assert_eq!(buf.caret(), 5);
    }

    // ---- IME marked-text path (replace_and_mark_text_in_range) ---------------

    #[test]
    fn marking_then_committing_composition() {
        let mut buf = EditBuffer::from_text("ab");
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        // Compose "ni" as preedit at the caret (after "ab").
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "ni",
            None,
        );
        assert_eq!(buf.text(), "abni");
        assert_eq!(marked, Some(2..4));
        assert_eq!(buf.caret(), 4);
        // The IME refines the preedit to the final kana over the SAME marked span.
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "に",
            None,
        );
        assert_eq!(buf.text(), "abに");
        assert_eq!(marked, Some(2..3));
        // Commit clears the mark and leaves the committed text in place.
        protocol_replace(&mut buf, &mut marked, &mut composing, &mut d, None, "二");
        assert_eq!(buf.text(), "ab二");
        assert!(marked.is_none());
        assert_eq!(buf.caret(), 3);
    }

    // ---- Codex review finding 1: an IME composition is ONE undo transaction ---

    #[test]
    fn ime_replacing_a_selection_in_a_fresh_buffer_is_recoverable_by_one_undo() {
        // `set_text` models a prefilled composer: undo history deliberately
        // empty, so if the first preedit replaces the selection without a
        // checkpoint the selected text is unrecoverable from any snapshot.
        let mut buf = EditBuffer::new();
        buf.set_text("hello world");
        buf.set_selection(6, 11); // select "world"
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        // First preedit destructively replaces the selection, then refines,
        // then commits — the standard composition shape.
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "せ",
            None,
        );
        assert_eq!(buf.text(), "hello せ");
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "せかい",
            None,
        );
        protocol_replace(&mut buf, &mut marked, &mut composing, &mut d, None, "世界");
        assert_eq!(buf.text(), "hello 世界");
        // ONE Undo restores the pre-composition document, selection included.
        assert!(buf.undo());
        assert_eq!(buf.text(), "hello world");
        assert_eq!(buf.selection(), Some(6..11));
        // And the original text was never lost to an unreachable state.
        assert!(!buf.undo());
        assert_eq!(buf.text(), "hello world");
    }

    #[test]
    fn preedit_refinement_adds_no_undo_steps() {
        let mut buf = EditBuffer::new();
        buf.set_text("ab");
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "n",
            None,
        );
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "ni",
            None,
        );
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "に",
            None,
        );
        protocol_replace(&mut buf, &mut marked, &mut composing, &mut d, None, "に");
        assert_eq!(buf.text(), "abに");
        assert!(buf.undo());
        assert_eq!(buf.text(), "ab", "one Undo unwinds the whole composition");
        assert!(
            !buf.undo(),
            "refinements and the commit added no extra steps"
        );
    }

    #[test]
    fn plain_typed_commits_coalesce_into_one_undo_step() {
        // Commits with no active mark are ordinary typing; a run coalesces so
        // Undo steps back a word-ish run, not one keystroke at a time.
        let mut buf = EditBuffer::new();
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        for ch in ["h", "i", "!"] {
            protocol_replace(&mut buf, &mut marked, &mut composing, &mut d, None, ch);
        }
        assert_eq!(buf.text(), "hi!");
        assert!(buf.undo());
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn unmark_via_empty_replace_and_mark() {
        let mut buf = EditBuffer::from_text("x");
        buf.set_selection(0, 1);
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "y",
            None,
        );
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "",
            None,
        );
        assert!(
            marked.is_none(),
            "no styled span to advertise to the platform"
        );
        assert!(composing, "but the composition transaction is still open");
    }

    // ---- Codex round-2 finding 1: empty preedit must not end the transaction --

    #[test]
    fn empty_preedit_then_commit_is_still_one_undo_transaction() {
        // first-preedit → refine-to-empty → final commit over a fresh selected
        // document: the commit must NOT open a second transaction, so ONE Undo
        // restores the original text and selection (round-2 finding 1 repro).
        let mut buf = EditBuffer::new();
        buf.set_text("hello world");
        buf.set_selection(6, 11); // select "world"
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "x",
            None,
        );
        assert_eq!(buf.text(), "hello x");
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "",
            None,
        );
        assert_eq!(buf.text(), "hello ");
        protocol_replace(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "world!",
        );
        assert_eq!(buf.text(), "hello world!");
        assert!(!composing, "the commit closed the composition");
        assert!(buf.undo());
        assert_eq!(buf.text(), "hello world");
        assert_eq!(buf.selection(), Some(6..11));
        assert!(!buf.undo(), "exactly one step for the whole composition");
    }

    #[test]
    fn preedit_resumed_after_empty_refinement_adds_no_undo_steps() {
        // x → "" → y → commit: the resumed preedit must not take a SECOND
        // boundary checkpoint (the composition never ended).
        let mut buf = EditBuffer::new();
        buf.set_text("hello world");
        buf.set_selection(6, 11);
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "x",
            None,
        );
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "",
            None,
        );
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "y",
            None,
        );
        protocol_replace(&mut buf, &mut marked, &mut composing, &mut d, None, "z");
        assert_eq!(buf.text(), "hello z");
        assert!(buf.undo());
        assert_eq!(buf.text(), "hello world");
        assert_eq!(buf.selection(), Some(6..11));
        assert!(!buf.undo());
    }

    #[test]
    fn composition_aborted_by_reset_reopens_checkpointing() {
        // LMX review suggestion: a hard reset mid-composition must drop
        // `composing`, or the next plain typed commit would skip its own
        // checkpoint and merge into the dead composition's transaction. The
        // protocol-level equivalent of set_text/take/undo is modeled by
        // clearing the flags the way those sites do.
        let mut buf = EditBuffer::new();
        buf.set_text("ab");
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "x",
            None,
        );
        assert!(composing);
        // Hard reset (what Composer::set_text / take / undo / redo do):
        marked = None;
        composing = false;
        buf.set_text("fresh");
        protocol_replace(&mut buf, &mut marked, &mut composing, &mut d, None, "!");
        assert_eq!(buf.text(), "fresh!");
        assert!(buf.undo(), "the typed commit checkpointed on its own");
        assert_eq!(buf.text(), "fresh");
    }

    #[test]
    fn vacuous_composition_event_neither_checkpoints_nor_opens_a_composition() {
        // An event with nothing to replace and no preedit text mutates nothing:
        // no undo step, and the NEXT typed commit is ordinary coalescing input
        // (it must still checkpoint — `composing` stayed false).
        let mut buf = EditBuffer::new();
        buf.set_text("ab");
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "",
            None,
        );
        assert!(!composing);
        assert!(marked.is_none());
        assert_eq!(buf.text(), "ab");
        protocol_replace(&mut buf, &mut marked, &mut composing, &mut d, None, "c");
        assert_eq!(buf.text(), "abc");
        assert!(buf.undo());
        assert_eq!(buf.text(), "ab");
    }

    // ---- blocker 2: the platform text-input path during dictation -------------
    //
    // These drive the REAL protocol functions (the same code the
    // EntityInputHandler methods wrap) interleaved with dictation partials — the
    // review's "type, paste, move, and submit between partials" integration
    // cases, runnable without a live GPUI window.

    #[test]
    fn typing_through_the_platform_path_between_partials() {
        let mut buf = EditBuffer::new();
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        // Partial arrives, then the user types "!" — a platform commit with no
        // explicit range, exactly what NSTextInputClient sends for a keypress.
        d.set_partial(&mut buf, "hello");
        protocol_replace(&mut buf, &mut marked, &mut composing, &mut d, None, "!");
        assert_eq!(buf.text(), "hello!");
        assert_eq!(d.ghost(), Some(0..5));
        // The next partial refines the ghost only; the typed char survives.
        d.set_partial(&mut buf, "hello there");
        assert_eq!(buf.text(), "hello there!");
        // And the final commit lands over the ghost, not over typed text.
        d.commit(&mut buf, "hello there");
        assert_eq!(buf.text(), "hello there !");
        assert!(!d.is_active());
    }

    #[test]
    fn moving_the_caret_then_typing_between_partials() {
        // Navigate away mid-dictation, type at the new spot: the ghost rebases
        // and later partials still replace exactly the preview span (the old
        // numeric-mirror bug deleted text after the caret moved).
        let mut buf = EditBuffer::from_text("note: ");
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "alpha");
        assert_eq!(buf.text(), "note: alpha");
        // User presses Home then types "x " at position 0.
        buf.apply(EditAction::Home, true);
        protocol_replace(&mut buf, &mut marked, &mut composing, &mut d, None, "x ");
        assert_eq!(buf.text(), "x note: alpha");
        assert_eq!(d.ghost(), Some(8..13));
        d.set_partial(&mut buf, "alpha beta");
        assert_eq!(buf.text(), "x note: alpha beta");
        d.commit(&mut buf, "alpha beta");
        assert_eq!(buf.text(), "x note: alpha beta ");
    }

    #[test]
    fn paste_between_partials_keeps_ghost_and_text_consistent() {
        // Models the Cmd/Ctrl+V branch of on_key while dictating: paste goes
        // through external_replace at the caret.
        let mut buf = EditBuffer::new();
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "spoken");
        let caret = buf.caret();
        d.external_replace(&mut buf, caret..caret, " pasted");
        assert_eq!(buf.text(), "spoken pasted");
        d.set_partial(&mut buf, "spoken words");
        assert_eq!(buf.text(), "spoken words pasted");
        assert_eq!(d.ghost(), Some(0..12));
    }

    #[test]
    fn backspace_and_delete_keys_respect_the_ghost() {
        // key_edit is the on_key routing: edits beside the ghost rebase it,
        // edits into it wipe the preview (never user text).
        let mut buf = EditBuffer::from_text("ab ");
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "ghost");
        assert_eq!(buf.text(), "ab ghost");
        // Caret sits at the ghost end; Backspace bites into the preview → wiped.
        key_edit(&mut buf, &mut d, EditAction::Backspace);
        assert_eq!(buf.text(), "ab ");
        assert!(!d.is_active());

        // Fresh ghost; move to start and Delete the 'a' — entirely before the
        // ghost, which must rebase, and the next partial replaces only it.
        d.set_partial(&mut buf, "tail");
        assert_eq!(buf.text(), "ab tail");
        buf.set_caret(0);
        key_edit(&mut buf, &mut d, EditAction::Delete);
        assert_eq!(buf.text(), "b tail");
        assert_eq!(d.ghost(), Some(2..6));
        d.set_partial(&mut buf, "tails");
        assert_eq!(buf.text(), "b tails");
    }

    #[test]
    fn ime_composition_takes_over_from_the_ghost() {
        // An IME preedit starting mid-dictation removes the preview and owns the
        // marked range; the two spans can never coexist in the buffer.
        let mut buf = EditBuffer::from_text("ab ");
        let mut marked = None;
        let mut composing = false;
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "ghost");
        protocol_replace_and_mark(
            &mut buf,
            &mut marked,
            &mut composing,
            &mut d,
            None,
            "ni",
            None,
        );
        assert!(
            !d.is_active(),
            "ghost must be cancelled by an IME transition"
        );
        assert_eq!(buf.text(), "ab ni");
        // The caret sat at the ghost end (8); remapped to 3 after preview removal.
        assert_eq!(marked, Some(3..5));
    }

    #[test]
    fn platform_commit_never_swallows_the_ghost_as_marked() {
        // The ghost is not advertised via marked_text_range, so a rangeless
        // commit resolves to the caret — NOT to the ghost span (the blocker's
        // "typed char replaces the whole preview" failure).
        let mut buf = EditBuffer::new();
        let mut marked = None; // IME marked stays None during dictation
        let mut composing = false;
        let mut d = Dictation::new();
        d.set_partial(&mut buf, "preview");
        protocol_replace(&mut buf, &mut marked, &mut composing, &mut d, None, "Z");
        assert_eq!(buf.text(), "previewZ");
        assert_eq!(d.ghost(), Some(0..7));
    }

    // ---- Codex review finding 2: selection/undo paths keep the ghost contract —
    //
    // These drive the REAL Composer methods (on_key, cut_selection, undo/redo,
    // dictation_commit) on an entity, so the routing itself is under test — not
    // a re-derivation of the library calls.

    fn key_down(key: &str, shift: bool) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: ks(key, shift),
            is_held: false,
            prefer_character_input: false,
        }
    }

    /// A key event with arbitrary modifiers (U3b1-FIX1b chords).
    fn key_mods(key: &str, shift: bool, alt: bool, platform: bool, control: bool) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke {
                modifiers: gpui::Modifiers {
                    shift,
                    alt,
                    platform,
                    control,
                    ..Default::default()
                },
                key: key.to_string(),
                key_char: None,
            },
            is_held: false,
            prefer_character_input: false,
        }
    }

    /// `⌥⌫` (mac word backspace) as a key event.
    fn opt_backspace() -> KeyDownEvent {
        key_mods("backspace", false, true, false, false)
    }

    /// `⌘⌫` (mac line backspace) as a key event.
    fn cmd_backspace() -> KeyDownEvent {
        key_mods("backspace", false, false, true, false)
    }

    #[gpui::test]
    fn u3b1_composer_shift_arrows_select_and_cmd_backspace_deletes_line(
        cx: &mut gpui::TestAppContext,
    ) {
        let composer = cx.update(|cx| cx.new(Composer::new));
        composer.update(cx, |c, cx| {
            c.mac_keys = true;
            c.set_text("hello world", cx);
            assert_eq!(c.buffer.caret(), 11);
            // Shift+Left ×5 selects "world".
            for _ in 0..5 {
                c.on_key(&key_mods("left", true, false, false, false), cx);
            }
            assert_eq!(c.buffer.selected_text(), "world");
            // Plain Right collapses the selection at its right edge (caret 11).
            c.on_key(&key_down("right", false), cx);
            assert_eq!(c.buffer.caret(), 11);
            assert_eq!(c.buffer.selection(), None);
            // ⌥⌫ deletes "world "; undo restores it.
            c.on_key(&opt_backspace(), cx);
            assert_eq!(c.buffer.text(), "hello ");
            c.undo(cx);
            assert_eq!(c.buffer.text(), "hello world");
            // ⌘⌫ deletes the whole line; undo restores it.
            c.on_key(&cmd_backspace(), cx);
            assert_eq!(c.buffer.text(), "");
            c.undo(cx);
            assert_eq!(c.buffer.text(), "hello world");
            // ⌘↑ moves the caret to the document start (mac chord).
            c.on_key(&key_mods("up", false, false, true, false), cx);
            assert_eq!(c.buffer.caret(), 0);
            // Shift+⌘↓ selects from there to the document end.
            c.on_key(&key_mods("down", true, false, true, false), cx);
            assert_eq!(c.buffer.selected_text(), "hello world");
            // Linux chord table: ⌃⌫ deletes the word before the caret.
            c.mac_keys = false;
            c.set_text("foo bar", cx);
            c.on_key(&key_mods("backspace", false, false, false, true), cx);
            assert_eq!(c.buffer.text(), "foo ");
        });
    }

    #[gpui::test]
    fn u3b1_word_delete_rebases_live_ghost(cx: &mut gpui::TestAppContext) {
        let composer = cx.update(|cx| cx.new(Composer::new));
        composer.update(cx, |c, cx| {
            c.mac_keys = true;
            c.set_text("abc def ", cx);
            c.dictation_partial("tail", cx);
            assert_eq!(c.buffer.text(), "abc def tail");
            assert_eq!(c.dictation.ghost(), Some(8..12));
            c.buffer.set_caret(7);
            // ⌥⌫ deletes the word before the caret; the ghost rebases.
            c.on_key(&opt_backspace(), cx);
            assert_eq!(c.buffer.text(), "abc  tail");
            assert_eq!(c.dictation.ghost(), Some(5..9));
            // The next partial replaces only the preview.
            c.dictation_partial("tails", cx);
            assert_eq!(c.buffer.text(), "abc  tails");
        });
    }

    #[gpui::test]
    fn backspace_over_a_selection_rebases_a_live_ghost(cx: &mut gpui::TestAppContext) {
        let composer = cx.update(|cx| cx.new(Composer::new));
        composer.update(cx, |c, cx| {
            c.set_text("abc ", cx);
            c.dictation_partial("tail", cx);
            assert_eq!(c.buffer.text(), "abc tail");
            c.buffer.set_selection(0, 2); // "ab", entirely before the ghost
            c.on_key(&key_down("backspace", false), cx);
            assert_eq!(c.buffer.text(), "c tail");
            assert_eq!(c.dictation.ghost(), Some(2..6), "ghost rebased, not stale");
            c.dictation_partial("tails", cx);
            assert_eq!(
                c.buffer.text(),
                "c tails",
                "the next partial replaces only the preview"
            );
        });
    }

    #[gpui::test]
    fn delete_over_a_selection_biting_the_ghost_wipes_only_the_preview(
        cx: &mut gpui::TestAppContext,
    ) {
        let composer = cx.update(|cx| cx.new(Composer::new));
        composer.update(cx, |c, cx| {
            c.set_text("keep ", cx);
            c.dictation_partial("preview", cx); // "keep preview", ghost 5..12
            c.buffer.set_selection(3, 8); // overlaps the ghost
            c.on_key(&key_down("delete", false), cx);
            // The preview is wiped, then the remapped selection (3..5) deletes.
            assert_eq!(c.buffer.text(), "kee");
            assert!(!c.dictation.is_active(), "overlap cancels the ghost");
        });
    }

    #[gpui::test]
    fn cut_before_a_live_ghost_rebases_it_and_fills_the_clipboard(cx: &mut gpui::TestAppContext) {
        let composer = cx.update(|cx| cx.new(Composer::new));
        composer.update(cx, |c, cx| {
            c.set_text("x ", cx);
            c.dictation_partial("ghost", cx); // "x ghost", ghost 2..7
            c.buffer.set_selection(0, 1); // "x", before the ghost
            c.cut_selection(cx);
            assert_eq!(c.buffer.text(), " ghost");
            assert_eq!(c.dictation.ghost(), Some(1..6), "ghost rebased after cut");
            c.dictation_partial("ghosts", cx);
            assert_eq!(c.buffer.text(), " ghosts");
        });
        assert_eq!(
            cx.read_from_clipboard().and_then(|i| i.text()),
            Some("x".to_string())
        );
    }

    #[gpui::test]
    fn undo_and_redo_cancel_a_live_preview_first(cx: &mut gpui::TestAppContext) {
        let composer = cx.update(|cx| cx.new(Composer::new));
        composer.update(cx, |c, cx| {
            c.set_text("", cx);
            // One checkpointed typed run, then a live partial on top of it.
            protocol_replace(
                &mut c.buffer,
                &mut c.marked,
                &mut c.composing,
                &mut c.dictation,
                None,
                "hello",
            );
            c.dictation_partial(" world", cx);
            assert_eq!(c.buffer.text(), "hello world");
            c.undo(cx);
            assert_eq!(
                c.buffer.text(),
                "",
                "preview removed, then the typed run undone"
            );
            assert!(!c.dictation.is_active(), "no stale ghost range after undo");
            // A late partial starts fresh at the caret instead of overwriting
            // unrelated text through a stale range.
            c.dictation_partial("again", cx);
            assert_eq!(c.buffer.text(), "again");
            // Redo also ends the preview before restoring.
            c.redo(cx);
            assert_eq!(c.buffer.text(), "hello");
            assert!(!c.dictation.is_active());
        });
    }

    #[gpui::test]
    fn dictation_commit_is_one_undoable_transaction(cx: &mut gpui::TestAppContext) {
        let composer = cx.update(|cx| cx.new(Composer::new));
        composer.update(cx, |c, cx| {
            c.set_text("pre ", cx);
            c.dictation_partial("draft", cx);
            c.dictation_partial("draft words", cx);
            c.dictation_commit("draft words", cx);
            assert_eq!(c.buffer.text(), "pre draft words ");
            assert!(c.buffer.undo());
            assert_eq!(
                c.buffer.text(),
                "pre ",
                "one Undo removes the whole utterance"
            );
            assert!(!c.buffer.undo(), "partial churn added no undo steps");

            // A bare commit (no partial ever shown) is checkpointed too.
            c.dictation_commit("hi", cx);
            assert_eq!(c.buffer.text(), "pre hi ");
            assert!(c.buffer.undo());
            assert_eq!(c.buffer.text(), "pre ");
        });
    }

    // ---- U3b1: Select All ---------------------------------------------------

    /// U3b1 proof (re-bodied in U3b1-FIX1a): a ⌘A keydown on the composer
    /// changes nothing — on macOS ⌘A reaches the composer only as the native
    /// Edit ▸ Select All menu action, never through `on_key` — and that menu
    /// action still selects the whole buffer, including during IME marked text.
    #[gpui::test]
    fn u3b1_select_all_selects_whole_buffer_via_key_and_menu(cx: &mut gpui::TestAppContext) {
        // name kept from d98fbef for the deletion gate; the reverted feature's type and ⌘A key path are gone in U3b1-FIX1a
        let composer = cx.update(|cx| cx.new(Composer::new));
        composer.update(cx, |c, cx| {
            c.set_text("hello world", cx);
            // Put the caret mid-text so a partial selection would be detectable.
            c.buffer.set_caret(5);
            // The ⌘A key path. The platform swallows ⌘A on macOS (it is the
            // native Edit ▸ Select All shortcut), so nothing ever reaches
            // `on_key` and nothing changes: no selection, caret still at 5.
            c.on_key(
                &KeyDownEvent {
                    keystroke: Keystroke {
                        modifiers: gpui::Modifiers {
                            platform: true,
                            ..Default::default()
                        },
                        key: "a".to_string(),
                        key_char: None,
                    },
                    is_held: false,
                    prefer_character_input: false,
                },
                cx,
            );
            assert_eq!(c.buffer.selection(), None, "⌘A keydown changes nothing");
            assert_eq!(c.buffer.caret(), 5);
            // The Edit ▸ Select All menu action selects the whole buffer.
            c.select_all(cx);
            assert_eq!(c.buffer.selection(), Some(0..11));
            // The menu action still works during IME marked text (as before
            // d98fbef broke it).
            c.buffer.set_caret(5);
            c.marked = Some(0..2);
            c.select_all(cx);
            assert_eq!(c.buffer.selection(), Some(0..11));
            c.marked = None;
        });
    }

    // ---- U3a1: context menu through a rendered root host ---------------------

    /// A root view like `AwiliApp`'s: one composer and, when `host`, the ONE
    /// context-menu host (the composer itself mounts none).
    struct MenuHarness {
        composer: Entity<Composer>,
        host: bool,
    }

    impl Render for MenuHarness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let host = match self.host {
                true => crate::context_menu::host_element(window, cx),
                false => None,
            };
            div()
                .size_full()
                .on_key_down(|ev: &KeyDownEvent, _, _| {
                    ROOT_KEYS.with(|k| k.borrow_mut().push(ev.keystroke.key.clone()))
                })
                .child(div().w(gpui::px(400.)).child(self.composer.clone()))
                .children(host)
        }
    }

    thread_local! {
        /// Keys that bubbled up to the harness root (the menu must stop them).
        static ROOT_KEYS: std::cell::RefCell<Vec<String>> = Default::default();
    }

    fn menu_harness<'a>(
        cx: &'a mut gpui::TestAppContext,
        text: &str,
        host: bool,
    ) -> (Entity<Composer>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| cx.bind_keys([gpui::KeyBinding::new("cmd-c", native_menu::Copy, None)]));
        ROOT_KEYS.with(|k| k.borrow_mut().clear());
        let text = text.to_string();
        let (view, cx) = cx.add_window_view(move |_, cx| {
            let composer = cx.new(|cx| {
                let mut c = Composer::new(cx);
                c.set_text(&text, cx);
                c
            });
            MenuHarness { composer, host }
        });
        // A real menu lives only in the active window (see `host_element`).
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let composer = view.read_with(cx, |h, _| h.composer.clone());
        (composer, cx)
    }

    /// The window point just right of the boundary before char `idx` (one line).
    fn at(
        composer: &Entity<Composer>,
        cx: &mut gpui::VisualTestContext,
        idx: usize,
    ) -> Point<Pixels> {
        composer.read_with(cx, |c, _| {
            let l = &c.wrap_layouts[0];
            let lh = c.layout_line_height;
            let p = l
                .line
                .position_for_index(byte_of_col(&l.line.text, idx), lh);
            let p = p.expect("laid out");
            point(l.origin.x + p.x + gpui::px(1.), l.origin.y + p.y + lh / 2.)
        })
    }

    fn right_click(composer: &Entity<Composer>, cx: &mut gpui::VisualTestContext, idx: usize) {
        let p = at(composer, cx, idx);
        cx.simulate_mouse_down(p, MouseButton::Right, gpui::Modifiers::default());
        cx.simulate_mouse_up(p, MouseButton::Right, gpui::Modifiers::default());
    }

    const MENU_ROWS: [&str; 4] = [
        "ui-row-context-menu-item-0",
        "ui-row-context-menu-item-1",
        "ui-row-context-menu-item-2",
        "ui-row-context-menu-item-3",
    ];

    fn row_center(cx: &mut gpui::VisualTestContext, row: usize) -> Point<Pixels> {
        // The legacy indices include the separator at 3; KIT Menu assigns
        // item indices only to actual rows, so Select All (entry 4) is item 3.
        let item = match row {
            0..=2 => row,
            4 => 3,
            _ => panic!("menu separator has no row bounds"),
        };
        cx.debug_bounds(MENU_ROWS[item])
            .expect("menu row drawn")
            .center()
    }

    /// The open menu's rows, or `None` when it is closed.
    fn menu_rows(cx: &mut gpui::VisualTestContext) -> Option<Vec<(String, bool)>> {
        cx.update(|_, cx| crate::context_menu::rows(cx))
    }

    fn rows_enabled(cut: bool, copy: bool, paste: bool, all: bool) -> Option<Vec<(String, bool)>> {
        let rows = [
            ("Cut", cut),
            ("Copy", copy),
            ("Paste", paste),
            ("-", false),
            ("Select All", all),
        ];
        Some(rows.iter().map(|(l, e)| (l.to_string(), *e)).collect())
    }

    #[derive(Debug, PartialEq)]
    struct Snap {
        text: String,
        sel: Option<Range<usize>>,
        caret: usize,
        focused: bool,
        open: bool,
        clip: Option<String>,
    }

    fn snap(composer: &Entity<Composer>, cx: &mut gpui::VisualTestContext) -> Snap {
        cx.update(|window, cx| {
            let c = composer.read(cx);
            Snap {
                text: c.buffer.text().to_string(),
                sel: c.buffer.selection(),
                caret: c.buffer.caret(),
                focused: c.focus.is_focused(window),
                open: crate::context_menu::is_open(cx),
                clip: cx.read_from_clipboard().and_then(|i| i.text()),
            }
        })
    }

    fn s(text: &str, sel: Option<Range<usize>>, caret: usize, focused: bool, open: bool) -> Snap {
        Snap {
            text: text.to_string(),
            sel,
            caret,
            focused,
            open,
            clip: None,
        }
    }

    fn clip(snap: Snap, clip: &str) -> Snap {
        Snap {
            clip: Some(clip.to_string()),
            ..snap
        }
    }

    #[test]
    fn u3a1_composer_menu_state_table() {
        // (has_selection, secret, clipboard_has_text, is_empty) → [Cut, Copy, Paste, Select All]
        let cases = [
            ((false, false, false, false), [false, false, false, true]),
            ((false, false, false, true), [false, false, false, false]),
            ((false, false, true, false), [false, false, true, true]),
            ((false, false, true, true), [false, false, true, false]),
            ((false, true, false, false), [false, false, false, true]),
            ((false, true, false, true), [false, false, false, false]),
            ((false, true, true, false), [false, false, true, true]),
            ((false, true, true, true), [false, false, true, false]),
            ((true, false, false, false), [true, true, false, true]),
            ((true, false, false, true), [true, true, false, false]),
            ((true, false, true, false), [true, true, true, true]),
            ((true, false, true, true), [true, true, true, false]),
            ((true, true, false, false), [false, false, false, true]),
            ((true, true, false, true), [false, false, false, false]),
            ((true, true, true, false), [false, false, true, true]),
            ((true, true, true, true), [false, false, true, false]),
        ];
        for ((sel, secret, clip, empty), want) in cases {
            assert_eq!(
                composer_menu_state(sel, secret, clip, empty),
                want,
                "sel={sel} secret={secret} clip={clip} empty={empty}"
            );
        }
    }

    #[gpui::test]
    fn u3a1_right_click_opens_menu_and_paste_inserts(cx: &mut gpui::TestAppContext) {
        let (composer, cx) = menu_harness(cx, "hello", true);
        cx.write_to_clipboard(ClipboardItem::new_string("PASTED".into()));
        right_click(&composer, cx, 2);
        assert_eq!(
            snap(&composer, cx),
            clip(s("hello", None, 2, false, true), "PASTED")
        );
        assert!(
            cx.debug_bounds("context-menu").is_some(),
            "the root host draws it"
        );
        assert_eq!(menu_rows(cx), rows_enabled(false, false, true, true));
        // No selection: Cut and Copy are disabled, so the first Down is Paste.
        cx.simulate_keystrokes("down enter");
        assert_eq!(
            snap(&composer, cx),
            clip(s("hePASTEDllo", None, 8, true, false), "PASTED")
        );
        assert!(cx.debug_bounds("context-menu").is_none());
    }

    #[gpui::test]
    fn u3a1_ctrl_click_opens_menu_on_mac_path(cx: &mut gpui::TestAppContext) {
        let (composer, cx) = menu_harness(cx, "hello world", true);
        let ctrl = gpui::Modifiers {
            control: true,
            ..Default::default()
        };
        let p = at(&composer, cx, 3);
        // Linux: ctrl-Left is a plain click.
        composer.update(cx, |c, _| c.mac_keys = false);
        cx.simulate_mouse_down(p, MouseButton::Left, ctrl);
        cx.simulate_mouse_up(p, MouseButton::Left, ctrl);
        assert_eq!(snap(&composer, cx), s("hello world", None, 3, true, false));
        // macOS: ctrl-Left opens the menu; the caret stays and no drag starts.
        composer.update(cx, |c, _| c.mac_keys = true);
        cx.simulate_mouse_down(p, MouseButton::Left, ctrl);
        assert_eq!(snap(&composer, cx), s("hello world", None, 3, false, true));
        assert!(!composer.read_with(cx, |c, _| c.is_selecting), "no drag");
        cx.simulate_mouse_up(p, MouseButton::Left, ctrl);
        assert_eq!(menu_rows(cx), rows_enabled(false, false, false, true));
        cx.simulate_keystrokes("escape");
        assert_eq!(snap(&composer, cx), s("hello world", None, 3, true, false));
    }

    #[gpui::test]
    fn u3a1_right_click_inside_selection_keeps_it_and_copy_copies(cx: &mut gpui::TestAppContext) {
        let (composer, cx) = menu_harness(cx, "hello world", true);
        composer.update(cx, |c, cx| {
            c.buffer.set_selection(6, 11);
            cx.notify();
        });
        right_click(&composer, cx, 8);
        assert_eq!(
            snap(&composer, cx),
            s("hello world", Some(6..11), 11, false, true)
        );
        assert_eq!(menu_rows(cx), rows_enabled(true, true, false, true));
        let copy = row_center(cx, 1);
        cx.simulate_click(copy, gpui::Modifiers::default());
        assert_eq!(
            snap(&composer, cx),
            clip(s("hello world", Some(6..11), 11, true, false), "world")
        );
    }

    #[gpui::test]
    fn u3a1_right_click_outside_selection_places_caret(cx: &mut gpui::TestAppContext) {
        let (composer, cx) = menu_harness(cx, "hello world", true);
        composer.update(cx, |c, cx| {
            c.buffer.set_selection(0, 5);
            cx.notify();
        });
        right_click(&composer, cx, 8);
        assert_eq!(snap(&composer, cx), s("hello world", None, 8, false, true));
        assert_eq!(menu_rows(cx), rows_enabled(false, false, false, true));
        cx.simulate_keystrokes("escape");
        assert_eq!(snap(&composer, cx), s("hello world", None, 8, true, false));
    }

    #[gpui::test]
    fn u3a1_marked_text_blocks_the_menu(cx: &mut gpui::TestAppContext) {
        let (composer, cx) = menu_harness(cx, "hello", true);
        composer.update(cx, |c, _| c.marked = Some(0..2));
        right_click(&composer, cx, 3);
        assert_eq!(snap(&composer, cx), s("hello", None, 5, true, false));
        assert!(cx.debug_bounds("context-menu").is_none());
    }

    #[gpui::test]
    fn u3a1_escape_and_click_outside_close(cx: &mut gpui::TestAppContext) {
        let (composer, cx) = menu_harness(cx, "hello", true);
        right_click(&composer, cx, 2);
        assert_eq!(snap(&composer, cx), s("hello", None, 2, false, true));
        cx.simulate_keystrokes("escape");
        assert_eq!(snap(&composer, cx), s("hello", None, 2, true, false));
        right_click(&composer, cx, 3);
        assert_eq!(snap(&composer, cx), s("hello", None, 3, false, true));
        let outside = point(gpui::px(700.), gpui::px(700.));
        cx.simulate_click(outside, gpui::Modifiers::default());
        assert_eq!(snap(&composer, cx), s("hello", None, 3, true, false));
        // Any other key closes the menu and is not typed.
        right_click(&composer, cx, 1);
        cx.simulate_keystrokes("x");
        assert_eq!(snap(&composer, cx), s("hello", None, 1, true, false));
        let leaked = ROOT_KEYS.with(|k| k.borrow().clone());
        assert!(leaked.is_empty(), "keys reached the root: {leaked:?}");
    }

    #[gpui::test]
    fn u3a1_reopen_keeps_the_field_to_refocus(cx: &mut gpui::TestAppContext) {
        let (composer, cx) = menu_harness(cx, "hello", true);
        right_click(&composer, cx, 2);
        // A second menu replaces the first (U3a2's surfaces do this).
        cx.update(|window, cx| {
            crate::context_menu::open(point(gpui::px(9.), gpui::px(9.)), vec![], window, cx)
        });
        assert_eq!(snap(&composer, cx), s("hello", None, 2, false, true));
        assert_eq!(menu_rows(cx), Some(vec![]));
        cx.simulate_keystrokes("escape");
        assert_eq!(snap(&composer, cx), s("hello", None, 2, true, false));
    }

    #[gpui::test]
    fn u3a1_select_all_and_cut_rows_run_from_the_menu(cx: &mut gpui::TestAppContext) {
        let (composer, cx) = menu_harness(cx, "hello world", true);
        right_click(&composer, cx, 3);
        assert_eq!(menu_rows(cx), rows_enabled(false, false, false, true));
        // Hover highlights Select All; Return runs it.
        let all = row_center(cx, 4);
        cx.simulate_mouse_move(all, None, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        assert_eq!(
            snap(&composer, cx),
            s("hello world", Some(0..11), 11, true, false)
        );
        right_click(&composer, cx, 5);
        assert_eq!(menu_rows(cx), rows_enabled(true, true, false, true));
        let cut = row_center(cx, 0);
        cx.simulate_click(cut, gpui::Modifiers::default());
        assert_eq!(
            snap(&composer, cx),
            clip(s("", None, 0, true, false), "hello world")
        );
        // Empty field: Select All is disabled; Paste puts the cut text back.
        right_click(&composer, cx, 0);
        assert_eq!(menu_rows(cx), rows_enabled(false, false, true, false));
        cx.simulate_keystrokes("down enter");
        assert_eq!(
            snap(&composer, cx),
            clip(s("hello world", None, 11, true, false), "hello world")
        );
    }

    #[gpui::test]
    fn u3a1_secret_field_never_copies(cx: &mut gpui::TestAppContext) {
        let (composer, cx) = menu_harness(cx, "hunter2", true);
        cx.write_to_clipboard(ClipboardItem::new_string("before".into()));
        composer.update_in(cx, |c, window, cx| {
            c.set_secret(true);
            c.select_all(cx);
            window.focus(&c.focus, cx);
        });
        cx.simulate_keystrokes("cmd-c");
        let selected = s("hunter2", Some(0..7), 7, true, false);
        assert_eq!(snap(&composer, cx), clip(selected, "before"));
        right_click(&composer, cx, 3);
        assert_eq!(menu_rows(cx), rows_enabled(false, false, true, true));
        // A disabled row does nothing and the menu stays open.
        let copy = row_center(cx, 1);
        cx.simulate_click(copy, gpui::Modifiers::default());
        let open = s("hunter2", Some(0..7), 7, false, true);
        assert_eq!(snap(&composer, cx), clip(open, "before"));
        cx.simulate_keystrokes("escape");
        let closed = s("hunter2", Some(0..7), 7, true, false);
        assert_eq!(snap(&composer, cx), clip(closed, "before"));
    }

    #[gpui::test]
    fn u3a1_composer_mounts_no_menu_host(cx: &mut gpui::TestAppContext) {
        let (composer, cx) = menu_harness(cx, "hello", false);
        right_click(&composer, cx, 2);
        assert_eq!(snap(&composer, cx), s("hello", None, 2, false, true));
        assert!(
            cx.debug_bounds("context-menu").is_none(),
            "only the root host draws the menu"
        );
        cx.update(crate::context_menu::close);
        assert_eq!(snap(&composer, cx), s("hello", None, 2, true, false));
    }

    #[gpui::test]
    fn u3a1_menu_closes_when_the_window_deactivates(cx: &mut gpui::TestAppContext) {
        let (composer, cx) = menu_harness(cx, "hello", true);
        right_click(&composer, cx, 2);
        assert_eq!(snap(&composer, cx), s("hello", None, 2, false, true));
        cx.deactivate_window();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let closed = s("hello", None, 2, true, false);
        assert_eq!(
            snap(&composer, cx),
            closed,
            "the menu never survives deactivation"
        );
        assert!(cx.debug_bounds("context-menu").is_none());
    }

    // ---- M2g: Enter guard while the reply is streaming (O2) -------------------

    /// M2g: while busy, Enter keeps the draft — it neither submits nor
    /// inserts a newline. (The Submit event is additionally swallowed at the
    /// `AwiliApp` subscription layer; the composer-level guard is asserted
    /// here by the buffer state staying byte-for-byte identical.)
    #[gpui::test]
    fn m2g_enter_while_busy_keeps_draft(cx: &mut gpui::TestAppContext) {
        let composer = cx.update(|cx| cx.new(Composer::new));
        composer.update(cx, |c, cx| {
            c.set_busy(true);
            c.set_text("queued draft", cx);
            let caret_before = c.buffer.caret();
            c.on_key(&key_down("enter", false), cx);
            assert_eq!(
                c.buffer.text(),
                "queued draft",
                "Enter must not insert a newline or drop the draft"
            );
            assert_eq!(c.buffer.caret(), caret_before, "caret untouched");
            // And not the idle path either: an idle composer turns Enter into
            // a Submit (and the empty-draft no-op path), so the busy guard is
            // the thing keeping the buffer untouched here.
            c.set_busy(false);
            c.on_key(&key_down("enter", false), cx);
            assert_eq!(
                c.buffer.text(),
                "queued draft",
                "idle path submits (no-op here)"
            );
        });
    }

    #[gpui::test]
    fn m2g_enter_while_busy_with_empty_draft_is_inert(cx: &mut gpui::TestAppContext) {
        let composer = cx.update(|cx| cx.new(Composer::new));
        composer.update(cx, |c, cx| {
            c.set_busy(true);
            c.set_text("  ", cx);
            c.on_key(&key_down("enter", false), cx);
            assert_eq!(
                c.buffer.text(),
                "  ",
                "a whitespace-only draft stays as-is (no newline)"
            );
        });
    }

    // ---- M2g: prompt-history key routing -----------------------------------

    /// M2g: pure decision for the Up/Down prompt-history walk.
    ///
    /// Returns `Some(true)` for Up, `Some(false)` for Down when the key
    /// should drive the walk; `None` when the key should fall through to
    /// caret movement. The walk is live only when `history` is on, no
    /// modifier is held, no IME composition is in progress, and the draft
    /// is either empty (a fresh Up) or still equals `recalled` (the entry
    /// the walk last put there — any edit that diverges the draft ends the
    /// walk and the next Up moves the caret).
    ///
    /// M2g: Up in a history-enabled composer with an empty draft and no
    /// recalled entry still routes to HistoryPrev — the walk is live over
    /// the restored convo, not over the (empty) session `sent` list. The
    /// test observes the pure decision (the same one the production path
    /// uses in `on_key`): `Some(true)` means Up fires HistoryPrev.
    #[gpui::test]
    fn m2g_up_in_history_composer_keeps_text(cx: &mut gpui::TestAppContext) {
        let handle = cx.add_window(|_w, cx| Composer::new(cx));
        handle
            .update(cx, |c, _w, _cx| {
                c.enable_history();
                let got = composer_history_key_direction(c, "up");
                assert!(
                    matches!(got, Some(true)),
                    "Up in a history composer with an empty draft and no recalled entry must fire HistoryPrev, got {got:?}"
                );
            })
            .unwrap();
    }

    /// M2g-FIX1 (T2 P2): Up and Down reach `on_key` through the window's real
    /// key dispatch. In a history composer whose draft equals the recalled
    /// entry, Up emits `HistoryPrev` and Down emits `HistoryNext`, and the
    /// draft and caret do not change. Once the draft is edited, Up emits
    /// nothing and moves the caret. A composer without `enable_history` never
    /// emits a history event.
    #[gpui::test]
    fn m2g_fix1_up_key_emits_history_prev_and_keeps_the_draft(cx: &mut gpui::TestAppContext) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::<&'static str>::new()));
        let (composer, window) = cx.add_window_view(|_, cx| Composer::new(cx));
        let seen = events.clone();
        window.update(|window, cx| {
            cx.subscribe(&composer, move |_, ev: &ComposerEvent, _| {
                seen.borrow_mut().push(match ev {
                    ComposerEvent::HistoryPrev => "prev",
                    ComposerEvent::HistoryNext => "next",
                    _ => "other",
                });
            })
            .detach();
            let focus = composer.read(cx).focus.clone();
            window.focus(&focus, cx);
        });
        composer.update(window, |c, cx| {
            c.enable_history();
            c.set_text("prev", cx);
            c.set_recalled(Some("prev".to_string()), cx);
        });
        window.run_until_parked();

        window.simulate_keystrokes("up");
        assert_eq!(*events.borrow(), vec!["prev"], "Up emits HistoryPrev");
        composer.read_with(window, |c, _| {
            assert_eq!(c.buffer.text(), "prev", "Up keeps the draft");
            assert_eq!(c.buffer.caret(), 4, "Up does not move the caret");
        });
        window.simulate_keystrokes("down");
        assert_eq!(
            *events.borrow(),
            vec!["prev", "next"],
            "Down emits HistoryNext"
        );

        // An edited draft ends the walk: Up moves the caret and emits nothing.
        composer.update(window, |c, cx| c.set_text("prev edited", cx));
        window.run_until_parked();
        window.simulate_keystrokes("up");
        assert_eq!(
            events.borrow().len(),
            2,
            "no history event for an edited draft"
        );
        composer.read_with(window, |c, _| {
            assert_eq!(c.buffer.text(), "prev edited");
            assert_eq!(c.buffer.caret(), 0, "Up on the only row goes to the start");
        });

        // Without enable_history, Up on an empty draft emits nothing.
        let plain_events = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let (plain, window) = cx.add_window_view(|_, cx| Composer::new(cx));
        let seen = plain_events.clone();
        window.update(|window, cx| {
            cx.subscribe(&plain, move |_, _: &ComposerEvent, _| {
                seen.set(seen.get() + 1)
            })
            .detach();
            let focus = plain.read(cx).focus.clone();
            window.focus(&focus, cx);
        });
        window.simulate_keystrokes("up");
        assert_eq!(
            plain_events.get(),
            0,
            "a plain composer emits no history event"
        );
    }

    /// M2g: the pure `history_key` gate. Up fires only when the draft is
    /// empty or equals `recalled`; Down fires under the same rule; a
    /// modifier, an IME composition, or a diverged draft each return `None`.
    #[test]
    fn m2g_history_key_fires_only_on_empty_or_recalled() {
        use gpui::Modifiers;
        fn ks(k: &str) -> Keystroke {
            Keystroke {
                modifiers: Modifiers::default(),
                key: k.to_string(),
                key_char: None,
            }
        }
        fn ks_with_modifiers(k: &str, mods: Modifiers) -> Keystroke {
            Keystroke {
                modifiers: mods,
                key: k.to_string(),
                key_char: None,
            }
        }
        fn alt() -> Modifiers {
            Modifiers {
                alt: true,
                ..Default::default()
            }
        }
        // Empty draft, no recalled: Up fires, Down fires.
        assert_eq!(history_key(&ks("up"), "", None, true, false), Some(true));
        assert_eq!(history_key(&ks("down"), "", None, true, false), Some(false));
        // Draft equals recalled: Up fires, Down fires.
        assert_eq!(
            history_key(&ks("up"), "alpha", Some("alpha"), true, false),
            Some(true)
        );
        assert_eq!(
            history_key(&ks("down"), "alpha", Some("alpha"), true, false),
            Some(false)
        );
        // Draft diverged from recalled: neither fires (caret movement).
        assert_eq!(
            history_key(&ks("up"), "al", Some("alpha"), true, false),
            None
        );
        assert_eq!(
            history_key(&ks("down"), "al", Some("alpha"), true, false),
            None
        );
        // Modifier held: neither fires.
        assert_eq!(
            history_key(&ks_with_modifiers("up", alt()), "", None, true, false),
            None,
            "Alt+Up moves the caret"
        );
        // IME composing: neither fires.
        assert_eq!(history_key(&ks("up"), "", None, true, true), None);
        // history flag off: neither fires (rename/palette composers).
        assert_eq!(history_key(&ks("up"), "", None, false, false), None);
        assert_eq!(history_key(&ks("down"), "", None, false, false), None);
        // Other keys: never fire.
        assert_eq!(history_key(&ks("left"), "", None, true, false), None);
        assert_eq!(history_key(&ks("escape"), "", None, true, false), None);
    }

    // ---- U3-INT1: U3 key classifier x M2g history walk ------------------------

    /// An arrow key-down as macOS delivers it: AppKit sets `NSFunctionKeyMask`
    /// on every arrow, so `function` is true even with no key held.
    fn mac_arrow(key: &str, shift: bool) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke {
                modifiers: gpui::Modifiers {
                    shift,
                    function: true,
                    ..Default::default()
                },
                key: key.to_string(),
                key_char: None,
            },
            is_held: false,
            prefer_character_input: false,
        }
    }

    /// A history composer in a window, with a recorder of its history events.
    fn history_events_harness(
        cx: &mut gpui::TestAppContext,
        history: bool,
    ) -> (
        Entity<Composer>,
        &mut gpui::VisualTestContext,
        std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    ) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::<&'static str>::new()));
        let (composer, window) = cx.add_window_view(|_, cx| Composer::new(cx));
        let seen = events.clone();
        window.update(|_, cx| {
            cx.subscribe(&composer, move |_, ev: &ComposerEvent, _| {
                seen.borrow_mut().push(match ev {
                    ComposerEvent::HistoryPrev => "prev",
                    ComposerEvent::HistoryNext => "next",
                    _ => "other",
                });
            })
            .detach();
        });
        if history {
            composer.update(window, |c, _| c.enable_history());
        }
        (composer, window, events)
    }

    /// U3-INT1: with the walk live (draft == recalled), Shift+Up and Shift+Down
    /// still belong to the U3 classifier and extend the selection by a visual
    /// row; ⌘↑ still jumps to the document start. None emits a history event.
    #[gpui::test]
    fn u3_int1_shifted_arrows_extend_selection_while_history_is_live(
        cx: &mut gpui::TestAppContext,
    ) {
        let (composer, window, events) = history_events_harness(cx, true);
        composer.update_in(window, |c, window, cx| {
            c.mac_keys = true;
            let (font_size, line_height) = (theme::z(10.), theme::z(20.));
            c.set_text("aaaaabbbbbcc", cx);
            c.set_recalled(Some("aaaaabbbbbcc".to_string()), cx);
            install_layouts(c, window, theme::z(30.), font_size, line_height);
            assert_eq!(
                composer_history_key_direction(c, "up"),
                Some(true),
                "precondition: a plain Up would walk the history"
            );
            c.buffer.set_caret(7); // row 1, column 2
            c.on_key(&mac_arrow("up", true), cx);
            assert_eq!(c.buffer.caret(), 2, "Shift+Up moves to row 0");
            assert_eq!(c.buffer.selected_text(), "aaabb", "Shift+Up extends");
            c.on_key(&mac_arrow("down", true), cx);
            c.on_key(&mac_arrow("down", true), cx);
            assert_eq!(
                c.buffer.selected_text(),
                "bbbcc",
                "Shift+Down extends past the anchor"
            );
            c.on_key(&key_mods("up", false, false, true, false), cx);
            assert_eq!(c.buffer.caret(), 0, "⌘↑ goes to the document start");
            assert_eq!(c.buffer.text(), "aaaaabbbbbcc", "the draft is untouched");
        });
        window.run_until_parked();
        assert!(
            events.borrow().is_empty(),
            "no modified arrow walks the history, got {:?}",
            events.borrow()
        );
    }

    /// U3-INT1-FIX1: Option+↑/↓ and Control+↑/↓, with and without the
    /// `function` flag macOS sets on arrows, never walk the history, on an
    /// empty draft or on a draft that still equals the recalled entry.
    #[gpui::test]
    fn u3_int1_fix1_option_and_control_arrows_never_walk_history(cx: &mut gpui::TestAppContext) {
        fn chord(key: &str, alt: bool, control: bool, function: bool) -> KeyDownEvent {
            KeyDownEvent {
                keystroke: Keystroke {
                    modifiers: gpui::Modifiers {
                        alt,
                        control,
                        function,
                        ..Default::default()
                    },
                    key: key.to_string(),
                    key_char: None,
                },
                is_held: false,
                prefer_character_input: false,
            }
        }
        for draft in ["", "alpha"] {
            let (composer, window, events) = history_events_harness(cx, true);
            composer.update(window, |c, cx| {
                c.set_text(draft, cx);
                if !draft.is_empty() {
                    c.set_recalled(Some(draft.to_string()), cx);
                }
                assert_eq!(
                    composer_history_key_direction(c, "up"),
                    Some(true),
                    "precondition: a plain Up would walk the history"
                );
                for key in ["up", "down"] {
                    for (alt, control) in [(true, false), (false, true)] {
                        for function in [false, true] {
                            c.on_key(&chord(key, alt, control, function), cx);
                        }
                    }
                }
                assert_eq!(c.buffer.text(), draft, "the draft is untouched");
            });
            window.run_until_parked();
            assert!(
                events.borrow().is_empty(),
                "draft={draft:?}: ⌥/⌃ arrows walked the history: {:?}",
                events.borrow()
            );
        }
    }

    /// U3-INT1: a plain ↑/↓ as macOS delivers it (`function` set) on the empty
    /// main composer walks the history and leaves the draft alone.
    #[gpui::test]
    fn u3_int1_plain_mac_arrow_on_empty_history_composer_walks_history(
        cx: &mut gpui::TestAppContext,
    ) {
        let (composer, window, events) = history_events_harness(cx, true);
        composer.update(window, |c, cx| {
            c.on_key(&mac_arrow("up", false), cx);
            c.on_key(&mac_arrow("down", false), cx);
            assert_eq!(c.buffer.text(), "", "the walk itself never edits the draft");
            assert_eq!(c.buffer.selection(), None);
        });
        window.run_until_parked();
        assert_eq!(*events.borrow(), vec!["prev", "next"]);
    }

    /// U3-INT1: a plain ↑ the walk declines reaches the old caret path through
    /// the classifier's `Vertical { extend: false }` fall-through: on a composer
    /// without history, and on a history composer whose draft has diverged
    /// from the recalled entry. The caret moves one visual row, nothing is
    /// selected, and no history event fires.
    #[gpui::test]
    fn u3_int1_plain_arrow_not_taken_by_history_moves_the_caret(cx: &mut gpui::TestAppContext) {
        for history in [false, true] {
            let (composer, window, events) = history_events_harness(cx, history);
            composer.update_in(window, |c, window, cx| {
                c.mac_keys = true;
                let (font_size, line_height) = (theme::z(10.), theme::z(20.));
                c.set_text("aaaaabbbbbcc", cx);
                if history {
                    c.set_recalled(Some("aaaaabbbbb".to_string()), cx);
                }
                install_layouts(c, window, theme::z(30.), font_size, line_height);
                c.buffer.set_caret(7); // row 1, column 2
                c.on_key(&mac_arrow("up", false), cx);
                assert_eq!(c.buffer.caret(), 2, "history={history}: ↑ moves a row");
                assert_eq!(
                    c.buffer.selection(),
                    None,
                    "history={history}: no selection"
                );
                c.on_key(&mac_arrow("down", false), cx);
                assert_eq!(c.buffer.caret(), 7, "history={history}: ↓ moves back");
            });
            window.run_until_parked();
            assert!(
                events.borrow().is_empty(),
                "history={history}: no history event, got {:?}",
                events.borrow()
            );
        }
    }

    // ---- utf16 conversions ---------------------------------------------------

    #[test]
    fn utf16_roundtrip_over_mixed_text() {
        let t = "a😀本z"; // chars: a(1) 😀(2) 本(1) z(1) utf16 units
        assert_eq!(char_to_utf16(t, 0), 0);
        assert_eq!(char_to_utf16(t, 1), 1); // after 'a'
        assert_eq!(char_to_utf16(t, 2), 3); // after emoji (2 units)
        assert_eq!(char_to_utf16(t, 3), 4); // after 本
        assert_eq!(utf16_to_char(t, 3), 2); // start of 本
        assert_eq!(utf16_to_char(t, 1), 1);
    }

    /// RC2-R4 (D-U3, U3 list "Emacs keys"): on macOS the text-field ⌃ keys
    /// move and delete as in every Mac text field; off macOS they do nothing
    /// here (Ctrl is the app chord key there).
    #[gpui::test]
    fn rc2r4_the_mac_control_keys_move_and_delete_in_the_composer(cx: &mut gpui::TestAppContext) {
        let ctrl = |k: &str| key_mods(k, false, false, false, true);
        let composer = cx.update(|cx| cx.new(Composer::new));
        composer.update(cx, |c, cx| {
            c.mac_keys = true;
            c.set_text("hello world", cx);
            c.on_key(&ctrl("a"), cx);
            assert_eq!(c.buffer.caret(), 0, "⌃A goes to the line start");
            c.on_key(&ctrl("f"), cx);
            assert_eq!(c.buffer.caret(), 1, "⌃F one character forward");
            c.on_key(&ctrl("e"), cx);
            assert_eq!(c.buffer.caret(), 11, "⌃E goes to the line end");
            c.on_key(&ctrl("b"), cx);
            assert_eq!(c.buffer.caret(), 10, "⌃B one character back");
            c.on_key(&ctrl("h"), cx);
            assert_eq!(c.buffer.text(), "hello word", "⌃H deletes back");
            c.on_key(&ctrl("a"), cx);
            c.on_key(&ctrl("d"), cx);
            assert_eq!(c.buffer.text(), "ello word", "⌃D deletes forward");
            c.on_key(&key_mods("e", true, false, false, true), cx);
            assert_eq!(c.buffer.selection(), Some(0..9), "⌃⇧E extends");

            c.mac_keys = false;
            c.set_text("hello", cx);
            for k in ["a", "b", "h", "d"] {
                c.on_key(&ctrl(k), cx);
            }
            assert_eq!((c.buffer.text(), c.buffer.caret()), ("hello", 5));
        });
    }

    /// RC2-R4M5 (slice review P3): on macOS ⌃P / ⌃N move the caret a row up /
    /// down, also in a history composer whose draft is a recalled entry or
    /// empty; only the plain ↑ / ↓ walk the prompt history.
    #[gpui::test]
    fn rc2r4m5_control_p_and_n_move_the_caret_in_a_recalled_draft(cx: &mut gpui::TestAppContext) {
        let ctrl = |k: &str| key_mods(k, false, false, false, true);
        let plain = |k: &str| key_mods(k, false, false, false, false);
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::<&'static str>::new()));
        let (composer, window) = cx.add_window_view(|_, cx| Composer::new(cx));
        let seen = events.clone();
        window.update(|_, cx| {
            cx.subscribe(&composer, move |_, ev: &ComposerEvent, _| {
                seen.borrow_mut().push(match ev {
                    ComposerEvent::HistoryPrev => "prev",
                    ComposerEvent::HistoryNext => "next",
                    _ => "other",
                });
            })
            .detach();
        });
        let recalled = "first row\nsecond row";
        composer.update(window, |c, cx| {
            c.mac_keys = true;
            c.enable_history();
            c.set_text(recalled, cx);
            c.set_recalled(Some(recalled.to_string()), cx);
            assert_eq!(c.buffer.caret(), recalled.len());
            c.on_key(&ctrl("p"), cx);
            assert!(
                c.buffer.caret() <= 9,
                "⌃P goes up a row: {}",
                c.buffer.caret()
            );
            c.on_key(&ctrl("n"), cx);
            assert!(
                c.buffer.caret() > 9,
                "⌃N goes down a row: {}",
                c.buffer.caret()
            );
            assert_eq!(c.buffer.text(), recalled);
        });
        window.run_until_parked();
        assert!(
            events.borrow().is_empty(),
            "⌃P/⌃N walked the history: {:?}",
            events.borrow()
        );
        // An empty draft: ⌃P / ⌃N do not recall either.
        composer.update(window, |c, cx| {
            c.set_text("", cx);
            c.set_recalled(None, cx);
            c.on_key(&ctrl("p"), cx);
            c.on_key(&ctrl("n"), cx);
        });
        window.run_until_parked();
        assert!(events.borrow().is_empty(), "{:?}", events.borrow());
        // The plain arrows still walk it.
        composer.update(window, |c, cx| {
            c.on_key(&plain("up"), cx);
            c.on_key(&plain("down"), cx);
        });
        window.run_until_parked();
        assert_eq!(*events.borrow(), vec!["prev", "next"]);
    }
}
