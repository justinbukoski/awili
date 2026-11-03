//! U3c: one selection model shared by the messages of a conversation scope, so
//! a drag can cross messages, double/triple-click pick a word or paragraph, and
//! Select All covers the conversation. Offsets are byte offsets into each
//! message's `TextCollector` source, on char boundaries.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{App, ClipboardItem, Global, SharedString};

use crate::context_menu::{MenuEntry, MenuRun};
use crate::selectable_message::selected_text;

/// A byte in message `msg`. Ordered by (msg, byte), so a backwards drag
/// normalizes to the same range as a forwards one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub msg: usize,
    pub byte: usize,
}

#[derive(Default)]
pub struct TranscriptSelection {
    anchor: Option<Pos>,
    head: Pos,
    dragging: bool,
    /// The messages rendered in the last painted frame.
    sources: BTreeMap<usize, SharedString>,
    /// The messages registered so far in the frame being drawn.
    frame: BTreeMap<usize, SharedString>,
    /// `true` once this frame's registrations were published by `commit`.
    published: bool,
}

impl TranscriptSelection {
    /// Prepaint: `msg` is rendered this frame with `source`. The first call
    /// after a `commit` starts a new frame.
    pub fn register(&mut self, msg: usize, source: &SharedString) {
        if std::mem::take(&mut self.published) {
            self.frame.clear();
        }
        self.frame.insert(msg, source.clone());
    }

    /// Paint: publish this frame's messages (the first call per frame wins),
    /// so a message no longer rendered (a shortened conversation) leaves
    /// Copy and Select All. A selected message whose source changed or
    /// disappeared clears the selection (its byte offsets are stale); new or
    /// changed messages outside the selection keep it.
    pub fn commit(&mut self) {
        if std::mem::replace(&mut self.published, true) {
            return;
        }
        let old = std::mem::replace(&mut self.sources, std::mem::take(&mut self.frame));
        if let Some((start, end)) = self.ends() {
            if (start.msg..=end.msg).any(|m| old.get(&m) != self.sources.get(&m)) {
                self.clear();
            }
        }
    }

    /// The selection's (start, end) in document order, `None` when empty.
    fn ends(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor.filter(|a| *a != self.head)?;
        Some((anchor.min(self.head), anchor.max(self.head)))
    }

    /// A mouse-down: 1 click places a caret, 2 select the word, 3+ the
    /// paragraph; `extend` (shift-click) keeps the anchor and moves the head.
    pub fn begin(&mut self, at: Pos, click_count: usize, extend: bool) {
        let text = self.sources.get(&at.msg).map_or("", |s| s.as_ref());
        let byte = at.byte.min(text.len());
        let range = match click_count {
            0 | 1 => byte..byte,
            2 => word_range(text, byte),
            _ => paragraph_range(text, byte),
        };
        if !(extend && self.anchor.is_some()) {
            self.anchor = Some(Pos {
                msg: at.msg,
                byte: range.start,
            });
        }
        self.head = Pos {
            msg: at.msg,
            byte: range.end,
        };
        self.dragging = true;
    }

    /// A drag over a rendered message moves the head there.
    pub fn drag_to(&mut self, at: Pos) {
        if let Some(text) = self.sources.get(&at.msg).filter(|_| self.dragging) {
            let byte = at.byte.min(text.len());
            self.head = Pos { msg: at.msg, byte };
        }
    }

    /// A drag past the conversation: above (`above`) the first rendered
    /// message the head goes to its start, below the last to its end. Any
    /// other message ignores it, so the gaps between bubbles change nothing.
    pub fn drag_beyond(&mut self, msg: usize, above: bool) {
        let end = match above {
            true => self.sources.first_key_value(),
            false => self.sources.last_key_value(),
        };
        if let Some((&m, source)) = end.filter(|(&m, _)| m == msg) {
            let byte = if above { 0 } else { source.len() };
            self.drag_to(Pos { msg: m, byte });
        }
    }

    pub fn end_drag(&mut self) {
        self.dragging = false;
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.dragging = false;
    }

    /// The whole rendered conversation: the start of the first message to
    /// the end of the last.
    pub fn select_all(&mut self) {
        let first = self.sources.first_key_value().map(|(&m, _)| m);
        let last = self.sources.last_key_value().map(|(&m, s)| (m, s.len()));
        if let (Some(first), Some((last, len))) = (first, last) {
            self.anchor = Some(Pos {
                msg: first,
                byte: 0,
            });
            self.head = Pos {
                msg: last,
                byte: len,
            };
            self.dragging = false;
        }
    }

    /// The local byte range to paint inside `msg`, or `None` when the
    /// selection does not cover a rendered `msg` (or covers zero bytes).
    pub fn range_in(&self, msg: usize) -> Option<Range<usize>> {
        let (start, end) = self.ends()?;
        let len = self.sources.get(&msg)?.len();
        let begin = if msg == start.msg { start.byte } else { 0 };
        let stop = if msg == end.msg { end.byte } else { len };
        (start.msg <= msg && msg <= end.msg && begin < stop).then_some(begin..stop)
    }

    /// The selected plain text of the rendered messages: each covered slice
    /// in msg order, joined with `"\n\n"`; `None` when nothing is selected.
    /// Slices go through `selectable_message::selected_text`, so a stale
    /// non-boundary offset yields nothing instead of a panic.
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.ends()?;
        let parts: Vec<&str> = (start.msg..=end.msg)
            .filter_map(|msg| {
                let source = self.sources.get(&msg)?;
                selected_text(source, self.range_in(msg)?)
            })
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// U3a2: whether `at` lies inside the selection (its ends included).
    pub fn covers(&self, at: Pos) -> bool {
        self.ends()
            .is_some_and(|(start, end)| start <= at && at <= end)
    }

    /// U3a2: the plain text of rendered message `msg` (D4).
    pub fn source(&self, msg: usize) -> Option<SharedString> {
        self.sources.get(&msg).cloned()
    }
}

/// U3a2: message `msg`'s menu. Copy runs the ⌘C code on the live selection;
/// Copy Message copies the message's plain text; Select All the conversation.
pub fn transcript_menu_entries(
    model: Rc<RefCell<TranscriptSelection>>,
    msg: usize,
) -> Vec<MenuEntry> {
    let (selected, source) = {
        let m = model.borrow();
        (
            m.selected_text().is_some(),
            m.source(msg).unwrap_or_default(),
        )
    };
    let live = model.clone();
    let copy: MenuRun = Rc::new(move |_, cx| {
        if let Some(text) = live.borrow().selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    });
    let text = source.to_string();
    let copy_message: MenuRun =
        Rc::new(move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone())));
    let select_all: MenuRun = Rc::new(move |window, _| {
        model.borrow_mut().select_all();
        window.refresh();
    });
    vec![
        MenuEntry::item(
            "Copy",
            Some(crate::shortcuts::localized_static("⌘C")),
            selected,
            copy,
        ),
        MenuEntry::item("Copy Message", None, !source.is_empty(), copy_message),
        MenuEntry::Separator,
        MenuEntry::item(
            "Select All",
            Some(crate::shortcuts::localized_static("⌘A")),
            true,
            select_all,
        ),
    ]
}

/// The word, whitespace run or punctuation run around `byte`, with the char
/// classes of `awili_core::edit`; at the end of the text, the last one.
pub fn word_range(text: &str, byte: usize) -> Range<usize> {
    let byte = floor_boundary(text, byte);
    let last = text[..byte]
        .char_indices()
        .next_back()
        .map_or(0, |(i, _)| i);
    let at = if byte == text.len() { last } else { byte };
    let Some(class) = text[at..].chars().next().map(char_class) else {
        return 0..0;
    };
    let same = |c: char| char_class(c) == class;
    let before = text[..at]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| same(c));
    let start = before.last().map_or(at, |(i, _)| i);
    let after = text[at..].char_indices().find(|&(_, c)| !same(c));
    start..after.map_or(text.len(), |(i, _)| at + i)
}

/// The line around `byte`, without its newline (D6: Copy yields clean text).
pub fn paragraph_range(text: &str, byte: usize) -> Range<usize> {
    let byte = floor_boundary(text, byte);
    let start = text[..byte].rfind('\n').map_or(0, |i| i + 1);
    start..text[byte..].find('\n').map_or(text.len(), |i| byte + i)
}

fn char_class(c: char) -> u8 {
    match c {
        c if c.is_alphanumeric() || c == '_' => 0,
        c if c.is_whitespace() => 1,
        _ => 2,
    }
}

/// The largest char boundary of `text` at or below `byte`.
fn floor_boundary(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    (0..=byte)
        .rev()
        .find(|&b| text.is_char_boundary(b))
        .unwrap_or(0)
}

/// One model per conversation scope; the 8 most recently used are kept (an
/// abandoned conversation drops its selection).
#[derive(Default)]
struct ScopeRegistry(Vec<(SharedString, Rc<RefCell<TranscriptSelection>>)>);

impl Global for ScopeRegistry {}

/// The shared model of `scope`, created on first use. Called from
/// `SelectableMessage::prepaint`, the only place with both a scope and an `App`.
pub fn shared(scope: &str, cx: &mut App) -> Rc<RefCell<TranscriptSelection>> {
    let scopes = &mut cx.default_global::<ScopeRegistry>().0;
    let entry = match scopes.iter().position(|(s, _)| s.as_ref() == scope) {
        Some(ix) => scopes.remove(ix),
        None => (scope.to_string().into(), Rc::default()),
    };
    let model = entry.1.clone();
    scopes.insert(0, entry);
    scopes.truncate(8);
    model
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `u3c_word_and_paragraph_ranges` (proof test): ASCII, punctuation,
    /// whitespace runs, multibyte, end of text.
    #[test]
    fn u3c_word_and_paragraph_ranges() {
        // Words.
        let text = "foo, bar_baz  qux";
        assert_eq!(word_range(text, 0), 0..3); // "foo"
        assert_eq!(word_range(text, 2), 0..3);
        assert_eq!(word_range(text, 3), 3..4); // "," single punctuation
        assert_eq!(word_range(text, 4), 4..5); // single space " "
        assert_eq!(word_range(text, 5), 5..12); // "bar_baz"
        assert_eq!(word_range(text, 11), 5..12);
        assert_eq!(word_range(text, 12), 12..14); // "  " run
        assert_eq!(word_range(text, 15), 14..17); // "qux"
        assert_eq!(word_range(text, 16), 14..17);
        // End of text lands on the last word.
        assert_eq!(word_range(text, text.len()), 14..17);

        // Whitespace run of two spaces.
        assert_eq!(word_range(text, 13), 12..14);

        // Multibyte: word chars that are not ASCII letters.
        // "naïve café" = n(0) a(1) ï(2-3) v(4) e(5) ' '(6) c(7) a(8) f(9) é(10-11) ' '(12)
        let mb = "naïve café";
        assert_eq!(word_range(mb, 0), 0..6); // "naïve" (ï is 2 bytes)
        assert_eq!(word_range(mb, 3), 0..6); // byte 3 inside ñ, snaps to 4 → "naïve"
        assert_eq!(word_range(mb, 6), 6..7); // " "
        assert_eq!(word_range(mb, 7), 7..12); // "café" (é is 2 bytes)
        assert_eq!(word_range(mb, 8), 7..12); // "café"
        assert_eq!(word_range(mb, mb.len()), 7..12); // end of text lands on "café"

        // Empty text never panics.
        assert_eq!(word_range("", 0), 0..0);

        // Paragraphs.
        let para = "one\ntwo\nthree";
        assert_eq!(paragraph_range(para, 0), 0..3); // "one"
        assert_eq!(paragraph_range(para, 4), 4..7); // "two"
        assert_eq!(paragraph_range(para, 6), 4..7);
        assert_eq!(paragraph_range(para, 7), 4..7); // byte 7 is '\n', snaps to para "two"
        assert_eq!(paragraph_range(para, para.len()), 8..13); // end of text → last para
                                                              // Multibyte paragraph: byte offsets stay on boundaries.
                                                              // "héllo\nwörld": h(0) é(1-2) l(3) l(4) o(5) \n(6) w(7) ö(8-9) r(10) l(11) d(12)
        let mbp = "héllo\nwörld";
        assert_eq!(paragraph_range(mbp, 0), 0..6);
        assert_eq!(paragraph_range(mbp, 1), 0..6); // byte 1 inside é, snaps to 1 → "héllo"
        assert_eq!(paragraph_range(mbp, 7), 7..13);
    }

    /// One frame: every message rendered last frame again, `msg` with `text`.
    fn reg(model: &mut TranscriptSelection, msg: usize, text: &str) {
        let mut frame = model.sources.clone();
        frame.insert(msg, text.into());
        frame.iter().for_each(|(m, s)| model.register(*m, s));
        model.commit();
    }

    /// One frame with exactly `texts` (messages 0..n) rendered.
    fn frame(model: &mut TranscriptSelection, texts: &[&str]) {
        for (msg, text) in texts.iter().enumerate() {
            model.register(msg, &(*text).into());
        }
        model.commit();
    }

    #[test]
    fn u3c_commit_drops_messages_no_longer_rendered() {
        let mut model = TranscriptSelection::default();
        frame(&mut model, &["one", "two", "three"]);
        model.select_all();
        assert_eq!(
            model.selected_text().as_deref(),
            Some("one\n\ntwo\n\nthree")
        );
        // The conversation is shortened: message 2 is no longer rendered. It
        // was selected, so the selection clears; Select All covers 0..=1.
        frame(&mut model, &["one", "two"]);
        assert_eq!(model.selected_text(), None);
        model.select_all();
        assert_eq!(model.selected_text().as_deref(), Some("one\n\ntwo"));
        // A new message outside the selection keeps it (streaming a reply).
        model.begin(Pos { msg: 0, byte: 0 }, 2, false);
        model.end_drag();
        frame(&mut model, &["one", "two", "streaming"]);
        assert_eq!(model.selected_text().as_deref(), Some("one"));
        // A second commit in the same frame (another message's paint) is a no-op.
        model.commit();
        assert_eq!(model.range_in(2), None);
        assert_eq!(model.sources.len(), 3);
    }

    #[test]
    fn u3c_drag_beyond_clamps_only_at_the_conversation_ends() {
        let mut model = TranscriptSelection::default();
        frame(&mut model, &["alpha", "beta", "gamma"]);
        model.begin(Pos { msg: 1, byte: 2 }, 1, false);
        model.drag_beyond(1, true); // above a middle message: a gap, no change
        model.drag_beyond(0, false); // below a non-last message: no change
        assert_eq!(model.selected_text(), None);
        model.drag_beyond(0, true); // above the first: its start
        assert_eq!(model.selected_text().as_deref(), Some("alpha\n\nbe"));
        model.drag_beyond(2, false); // below the last: its end
        assert_eq!(model.selected_text().as_deref(), Some("ta\n\ngamma"));
    }

    /// `u3c_cross_message_range_and_text` (proof test).
    #[test]
    fn u3c_cross_message_range_and_text() {
        let mut model = TranscriptSelection::default();
        reg(&mut model, 0, "alpha"); // len 5
        reg(&mut model, 1, "beta gamma"); // len 10
        reg(&mut model, 2, "delta"); // len 5

        // Forward drag: begin at msg 0 byte 3, drag to msg 2 byte 4.
        model.begin(Pos { msg: 0, byte: 3 }, 1, false);
        model.drag_to(Pos { msg: 2, byte: 4 });
        model.end_drag();
        assert_eq!(model.range_in(0), Some(3..5));
        assert_eq!(model.range_in(1), Some(0..10));
        assert_eq!(model.range_in(2), Some(0..4));
        assert_eq!(
            model.selected_text(),
            Some("ha\n\nbeta gamma\n\ndelt".to_string())
        );

        // Backwards drag: same result, anchor and head swapped.
        let mut back = TranscriptSelection::default();
        reg(&mut back, 0, "alpha");
        reg(&mut back, 1, "beta gamma");
        reg(&mut back, 2, "delta");
        back.begin(Pos { msg: 2, byte: 4 }, 1, false);
        back.drag_to(Pos { msg: 0, byte: 3 });
        back.end_drag();
        assert_eq!(back.range_in(0), Some(3..5));
        assert_eq!(back.range_in(1), Some(0..10));
        assert_eq!(back.range_in(2), Some(0..4));
        assert_eq!(
            back.selected_text(),
            Some("ha\n\nbeta gamma\n\ndelt".to_string())
        );
    }

    /// `u3c_select_all_covers_every_registered_message` (proof test).
    #[test]
    fn u3c_select_all_covers_every_registered_message() {
        let mut model = TranscriptSelection::default();
        reg(&mut model, 0, "one");
        reg(&mut model, 1, "two");
        reg(&mut model, 2, "three");
        model.begin(Pos { msg: 1, byte: 1 }, 1, false);
        model.select_all();
        assert_eq!(model.range_in(0), Some(0..3));
        assert_eq!(model.range_in(1), Some(0..3));
        assert_eq!(model.range_in(2), Some(0..5));
        assert_eq!(
            model.selected_text(),
            Some("one\n\ntwo\n\nthree".to_string())
        );
    }

    /// `u3c_changed_source_inside_selection_clears` (proof test).
    #[test]
    fn u3c_changed_source_inside_selection_clears() {
        let mut model = TranscriptSelection::default();
        reg(&mut model, 0, "alpha");
        reg(&mut model, 1, "beta");
        model.begin(Pos { msg: 0, byte: 1 }, 1, false);
        model.drag_to(Pos { msg: 1, byte: 2 });
        model.end_drag();
        assert!(model.selected_text().is_some());

        // Streaming extends message 1's source while the selection covers it.
        reg(&mut model, 1, "beta gamma");
        assert_eq!(model.selected_text(), None);
        assert_eq!(model.range_in(0), None);
        assert_eq!(model.range_in(1), None);
        assert!(!model.is_dragging());

        // Re-registering the unchanged source of a message outside the
        // (now-cleared) selection is a no-op; an identical source also does
        // not clear.
        reg(&mut model, 1, "beta gamma");
        model.begin(Pos { msg: 0, byte: 0 }, 1, false);
        model.drag_to(Pos { msg: 1, byte: 3 });
        model.end_drag();
        reg(&mut model, 0, "alpha"); // unchanged
        assert_eq!(model.selected_text(), Some("alpha\n\nbet".to_string()));
    }

    /// `u3c_double_click_selects_word_triple_selects_paragraph` (proof test).
    #[test]
    fn u3c_double_click_selects_word_triple_selects_paragraph() {
        // "one line\nsecond word here":
        // o(0) n(1) e(2) ' '(3) l(4) i(5) n(6) e(7) \n(8) s(9) e(10) c(11) o(12) n(13) d(14) ' '(15) w(16) o(17) r(18) d(19) ' '(20) h(21) e(22) r(23) e(24)
        let mut model = TranscriptSelection::default();
        reg(&mut model, 0, "one line\nsecond word here");

        // Double-click on "second" (byte 10, inside "second").
        model.begin(Pos { msg: 0, byte: 10 }, 2, false);
        model.end_drag();
        assert_eq!(model.selected_text(), Some("second".to_string()));
        assert_eq!(model.range_in(0), Some(9..15));

        // Triple-click on the same paragraph (byte 10, inside "second").
        let mut triple = TranscriptSelection::default();
        reg(&mut triple, 0, "one line\nsecond word here");
        triple.begin(Pos { msg: 0, byte: 10 }, 3, false);
        triple.end_drag();
        assert_eq!(triple.selected_text(), Some("second word here".to_string()));
        assert_eq!(triple.range_in(0), Some(9..25));

        // A second paragraph: triple-click selects it without its newline.
        // "first para\nsecond para": f(0)..a(9) \n(10) s(11)..a(21)
        let mut para = TranscriptSelection::default();
        reg(&mut para, 0, "first para\nsecond para");
        para.begin(Pos { msg: 0, byte: 5 }, 3, false); // inside "first para"
        para.end_drag();
        assert_eq!(para.selected_text(), Some("first para".to_string()));
        assert_eq!(para.range_in(0), Some(0..10));

        // Triple-click on the second paragraph selects only that paragraph.
        let mut para2 = TranscriptSelection::default();
        reg(&mut para2, 0, "first para\nsecond para");
        para2.begin(Pos { msg: 0, byte: 15 }, 3, false); // inside "second para"
        para2.end_drag();
        assert_eq!(para2.selected_text(), Some("second para".to_string()));
        assert_eq!(para2.range_in(0), Some(11..22));

        // Extend (shift-click) keeps the anchor and moves the head.
        let mut ext = TranscriptSelection::default();
        reg(&mut ext, 0, "hello world");
        ext.begin(Pos { msg: 0, byte: 0 }, 1, false); // caret at 0
        ext.begin(Pos { msg: 0, byte: 7 }, 1, true); // shift-click at 7 → head = 7
        ext.end_drag();
        assert_eq!(ext.selected_text(), Some("hello w".to_string()));
    }

    /// A byte inside a multibyte character snaps to a valid boundary.
    #[test]
    fn u3c_word_range_snaps_non_boundary_bytes() {
        // "naïve café": ï is bytes 2..4, so byte 3 is inside it.
        // nearest_boundary(3) → 2 (char 'ï'), which is inside the word "naïve" (0..6).
        let text = "naïve café";
        assert_eq!(word_range(text, 3), 0..6);
        // Byte 11 is inside é (bytes 10..12), nearest_boundary(11) → 10 (char 'é').
        // 'é' is a word char, so the whole word "café" (7..12) is selected.
        assert_eq!(word_range(text, 11), 7..12);
        // paragraph_range also snaps cleanly: no newline, whole text.
        assert_eq!(paragraph_range(text, 3), 0..text.len());
    }

    fn enabled(entries: &[MenuEntry]) -> Vec<(String, bool)> {
        let row = |e: &MenuEntry| match e {
            MenuEntry::Item { label, enabled, .. } => (label.to_string(), *enabled),
            MenuEntry::Separator => ("-".to_string(), false),
        };
        entries.iter().map(row).collect()
    }

    fn rows(copy: bool, message: bool) -> Vec<(String, bool)> {
        let rows = [
            ("Copy", copy),
            ("Copy Message", message),
            ("-", false),
            ("Select All", true),
        ];
        rows.iter().map(|(l, e)| (l.to_string(), *e)).collect()
    }

    #[test]
    fn u3a2_menu_entries_enablement() {
        let model = Rc::new(RefCell::new(TranscriptSelection::default()));
        frame(&mut model.borrow_mut(), &["alpha", ""]);
        // No selection: Copy is disabled, Copy Message is enabled.
        let entries = transcript_menu_entries(model.clone(), 0);
        assert_eq!(enabled(&entries), rows(false, true));
        // An empty message has nothing to copy.
        assert_eq!(
            enabled(&transcript_menu_entries(model.clone(), 1)),
            rows(false, false)
        );
        // A selection enables Copy.
        model.borrow_mut().begin(Pos { msg: 0, byte: 1 }, 2, false);
        assert_eq!(
            enabled(&transcript_menu_entries(model.clone(), 0)),
            rows(true, true)
        );
        // `covers` includes both ends and nothing outside them.
        let m = model.borrow();
        assert!(m.covers(Pos { msg: 0, byte: 0 }) && m.covers(Pos { msg: 0, byte: 5 }));
        assert!(!m.covers(Pos { msg: 1, byte: 0 }));
        assert_eq!(m.source(0).as_deref(), Some("alpha"));
        assert_eq!(m.source(7), None);
    }
}
