//! Conversation *metadata* — the sidebar's model (slice-7 spec §1).
//!
//! [`ConversationState`](crate::ConversationState) stays what it was: one
//! conversation's ordered message log. This module adds the layer above it: a
//! list of conversations, each with an id, a title, timestamps, and a SOFT
//! archived flag. Pure (no persistence, no UI) so the CRUD / auto-title /
//! archive invariants unit-test in isolation.
//!
//! ## The soft-archive invariant
//!
//! There is deliberately NO way to remove a [`ConversationMeta`] from a
//! [`ConversationList`] — no `delete`, no `remove`, no `retain`. Archive sets a
//! flag; unarchive clears it. The slice-7 spec is explicit that archived
//! conversations are hidden, never destroyed.

/// The title a conversation is born with, before the first user message
/// auto-titles it.
pub const DEFAULT_TITLE: &str = "New conversation";

/// Auto-titles truncate to roughly this many characters, at a word boundary.
pub const AUTO_TITLE_MAX_CHARS: usize = 40;

/// Which surface a conversation belongs to. Chat threads and Work (agentic) runs
/// are distinct conversations that never share a transcript — the discriminant
/// that keeps Work output from bleeding into the active chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ConvKind {
    /// A plain conversation with the model.
    #[default]
    Chat,
    /// An agentic Work run (terminal, touched files, cwd-scoped).
    Work,
    /// A Code-tab conversation: one coding-agent CLI session (U1).
    Code,
    /// RC2-CNCL1: a Council: one message goes to several models at once and
    /// their answers come back side by side.
    Council,
}

impl ConvKind {
    /// The persisted/serialized token for this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            ConvKind::Chat => "chat",
            ConvKind::Work => "work",
            ConvKind::Code => "code",
            ConvKind::Council => "council",
        }
    }

    /// Parse a persisted token; anything unrecognized (incl. legacy NULL rows
    /// read as empty) falls back to [`ConvKind::Chat`].
    pub fn from_token(s: &str) -> Self {
        match s {
            "work" => ConvKind::Work,
            "code" => ConvKind::Code,
            "council" => ConvKind::Council,
            _ => ConvKind::Chat,
        }
    }
}

/// One conversation's sidebar metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationMeta {
    /// UUID (v4, hyphenated lowercase). Stable across renames and archive.
    pub id: String,
    pub title: String,
    /// Unix seconds the conversation was created.
    pub created_ts: i64,
    /// Unix seconds of the last message activity (drives sidebar ordering).
    pub updated_ts: i64,
    /// SOFT archive flag — hidden from the default sidebar view, never deleted.
    pub archived: bool,
    /// `true` once the user renamed it manually; auto-titling then never
    /// overwrites it (slice-7 spec §1).
    pub title_is_custom: bool,
    /// Working directory for this conversation (workspace grouping key).
    pub cwd: Option<String>,
    /// Profile this conversation belongs to (multi-context).
    pub profile: Option<String>,
    /// Which surface owns this conversation — Chat or Work. Defaults to Chat for
    /// every conversation created or restored before the kind column existed.
    pub kind: ConvKind,
}

/// A fresh v4 UUID for a new conversation.
pub fn new_conversation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Derive a title from the first user message: first line, whitespace
/// collapsed, truncated to ~[`AUTO_TITLE_MAX_CHARS`] at a word boundary with an
/// ellipsis. Whitespace-only input yields [`DEFAULT_TITLE`] (a blank sidebar
/// row would be unclickable).
pub fn auto_title(text: &str) -> String {
    // Attachment composition puts inline images before the owner's words.
    // Skip their encoded payload without decoding pixels or allocating a
    // megabyte title; storage and Copy retain the complete original message.
    let mut text = text;
    let mut image_only = false;
    loop {
        let trimmed = text.trim_start();
        if !trimmed.starts_with("![") {
            break;
        }
        let Some((label, uri)) = trimmed.split_once("](") else {
            break;
        };
        if label.contains('\n') || !uri.starts_with("data:image/") {
            break;
        }
        image_only = true;
        let Some(end) = uri.find(')') else {
            text = "";
            break;
        };
        text = &uri[end + 1..];
    }
    let collapsed: Vec<&str> = text
        .split_whitespace()
        .take(AUTO_TITLE_MAX_CHARS + 1)
        .collect();
    if collapsed.is_empty() {
        return if image_only {
            "Image attachment".to_string()
        } else {
            DEFAULT_TITLE.to_string()
        };
    }
    let joined = collapsed.join(" ");
    if joined.chars().count() <= AUTO_TITLE_MAX_CHARS {
        return joined;
    }
    // Too long: keep whole words while they fit, then ellipsis. A single word
    // longer than the budget is cut mid-word — better a hard cut than nothing.
    let mut out = String::new();
    for word in joined.split(' ') {
        let candidate_len =
            out.chars().count() + word.chars().count() + usize::from(!out.is_empty());
        if candidate_len > AUTO_TITLE_MAX_CHARS {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        out = joined.chars().take(AUTO_TITLE_MAX_CHARS).collect();
    }
    out.push('…');
    out
}

/// The ordered set of conversations the sidebar renders.
#[derive(Clone, Debug, Default)]
pub struct ConversationList {
    items: Vec<ConversationMeta>,
}

impl ConversationList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild from persisted metadata (restore-on-launch).
    pub fn from_items(items: Vec<ConversationMeta>) -> Self {
        Self { items }
    }

    /// Create a Chat conversation with a fresh UUID and [`DEFAULT_TITLE`],
    /// returning a clone of its metadata.
    pub fn create(&mut self, now_ts: i64) -> ConversationMeta {
        self.create_kind(now_ts, ConvKind::Chat)
    }

    /// Create a conversation of the given [`ConvKind`], returning a clone of its
    /// metadata.
    pub fn create_kind(&mut self, now_ts: i64, kind: ConvKind) -> ConversationMeta {
        let meta = ConversationMeta {
            id: new_conversation_id(),
            title: DEFAULT_TITLE.to_string(),
            created_ts: now_ts,
            updated_ts: now_ts,
            archived: false,
            title_is_custom: false,
            cwd: None,
            profile: None,
            kind,
        };
        self.items.push(meta.clone());
        meta
    }

    pub fn get(&self, id: &str) -> Option<&ConversationMeta> {
        self.items.iter().find(|c| c.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut ConversationMeta> {
        self.items.iter_mut().find(|c| c.id == id)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Every conversation, archived included (persistence sync, tests).
    pub fn all(&self) -> &[ConversationMeta] {
        &self.items
    }

    /// Unarchived conversations, most recently updated first (the sidebar's
    /// default view).
    pub fn active(&self) -> Vec<&ConversationMeta> {
        self.sorted(|c| !c.archived)
    }

    /// Archived conversations, most recently updated first (the collapsible
    /// "Archived" section).
    pub fn archived(&self) -> Vec<&ConversationMeta> {
        self.sorted(|c| c.archived)
    }

    fn sorted(&self, keep: impl Fn(&ConversationMeta) -> bool) -> Vec<&ConversationMeta> {
        let mut v: Vec<&ConversationMeta> = self.items.iter().filter(|c| keep(c)).collect();
        v.sort_by(|a, b| {
            b.updated_ts
                .cmp(&a.updated_ts)
                .then(b.created_ts.cmp(&a.created_ts))
        });
        v
    }

    /// Manual rename. A non-blank title is recorded and marks the title custom,
    /// so auto-titling never overwrites it. A blank title is rejected (returns
    /// `false`) — the row must stay clickable.
    pub fn rename(&mut self, id: &str, title: &str) -> bool {
        let title = title.trim();
        if title.is_empty() {
            return false;
        }
        match self.get_mut(id) {
            Some(c) => {
                c.title = title.to_string();
                c.title_is_custom = true;
                true
            }
            None => false,
        }
    }

    /// Note message activity: bump `updated_ts`, and on the FIRST user message
    /// of a never-renamed conversation derive the title from the text.
    /// Returns the new title when auto-titling fired (the caller persists it).
    pub fn note_user_message(&mut self, id: &str, text: &str, now_ts: i64) -> Option<String> {
        let c = self.get_mut(id)?;
        c.updated_ts = now_ts;
        if !c.title_is_custom && c.title == DEFAULT_TITLE {
            c.title = auto_title(text);
            return Some(c.title.clone());
        }
        None
    }

    /// Bump `updated_ts` (assistant reply landed, etc.).
    pub fn touch(&mut self, id: &str, now_ts: i64) {
        if let Some(c) = self.get_mut(id) {
            c.updated_ts = now_ts;
        }
    }

    /// SOFT archive: hide from the default view. The metadata (and its
    /// messages, wherever they live) are untouched. Returns `false` for an
    /// unknown id.
    /// HARD-remove a conversation from the list. `true` when it existed.
    /// The soft path is [`archive`](Self::archive); this one backs the
    /// explicit, confirmed delete (user-test B2).
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|c| c.id != id);
        self.items.len() != before
    }

    pub fn archive(&mut self, id: &str) -> bool {
        match self.get_mut(id) {
            Some(c) => {
                c.archived = true;
                true
            }
            None => false,
        }
    }

    /// Restore an archived conversation to the default view.
    pub fn unarchive(&mut self, id: &str) -> bool {
        match self.get_mut(id) {
            Some(c) => {
                c.archived = false;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ConvKind serialization ----------------------------------------------

    #[test]
    fn convkind_token_roundtrips_and_falls_back_to_chat() {
        // as_str is the ONLY writer of the persisted `kind` column
        // (main.rs:905, doc.rs:313); from_token the ONLY reader (main.rs:878).
        assert_eq!(ConvKind::Chat.as_str(), "chat");
        assert_eq!(ConvKind::Work.as_str(), "work");

        // Exact-token parse: only "work" maps to Work.
        assert_eq!(ConvKind::from_token("work"), ConvKind::Work);
        assert_eq!(ConvKind::from_token("chat"), ConvKind::Chat);

        // Round-trip both variants through the persisted token.
        assert_eq!(
            ConvKind::from_token(ConvKind::Chat.as_str()),
            ConvKind::Chat
        );
        assert_eq!(
            ConvKind::from_token(ConvKind::Work.as_str()),
            ConvKind::Work
        );

        // Everything unrecognized falls back to Chat. The persist layer restores
        // NULL `kind` as "chat" via COALESCE, so the empty string never reaches
        // here in practice — but pinning it as Chat keeps the parser hardened:
        // if this fell back to Work, any stray/empty token would resurrect a
        // plain chat as a Work run.
        assert_eq!(ConvKind::from_token(""), ConvKind::Chat);
        assert_eq!(ConvKind::from_token("Work"), ConvKind::Chat);
        assert_eq!(ConvKind::from_token("garbage"), ConvKind::Chat);
    }

    #[test]
    fn u1b_code_kind_round_trips() {
        // U1b: the Code-tab conversation kind serializes as "code" and parses
        // back; every kind round-trips through its persisted token, and a
        // capitalized token is unrecognized (falls back to Chat).
        assert_eq!(ConvKind::Code.as_str(), "code");
        assert_eq!(ConvKind::from_token("code"), ConvKind::Code);
        for kind in [ConvKind::Chat, ConvKind::Work, ConvKind::Code] {
            assert_eq!(ConvKind::from_token(kind.as_str()), kind);
        }
        assert_eq!(ConvKind::from_token("Code"), ConvKind::Chat);
    }

    // ---- CRUD -----------------------------------------------------------------

    #[test]
    fn create_assigns_unique_uuid_and_default_title() {
        let mut list = ConversationList::new();
        let a = list.create(100);
        let b = list.create(200);
        assert_ne!(a.id, b.id);
        assert_eq!(a.title, DEFAULT_TITLE);
        assert!(!a.archived);
        assert!(!a.title_is_custom);
        assert_eq!(a.created_ts, 100);
        assert_eq!(a.updated_ts, 100);
        assert_eq!(list.len(), 2);
        // The id is a hyphenated v4 UUID.
        assert_eq!(a.id.len(), 36);
        assert_eq!(a.id.matches('-').count(), 4);
    }

    #[test]
    fn create_kind_sets_requested_variant_on_returned_and_stored_meta() {
        // `kind` is the Chat/Work surface split (persisted via ConvKind::as_str).
        // create() defaults to Chat; create_kind must honor the requested variant
        // on BOTH the returned clone and the row it pushes. Every other create
        // test goes through create() (always Chat) and none asserts `.kind`, so
        // hardcoding `kind: ConvKind::Chat` in create_kind would survive the suite.
        let mut list = ConversationList::new();
        let chat = list.create(100); // Chat via the create() default.
        assert_eq!(chat.kind, ConvKind::Chat);

        let work = list.create_kind(200, ConvKind::Work);
        // Returned clone carries the requested kind...
        assert_eq!(work.kind, ConvKind::Work);
        // ...and so does the stored row (push happens after the clone).
        assert_eq!(list.get(&work.id).unwrap().kind, ConvKind::Work);
        // The earlier Chat conversation is untouched.
        assert_eq!(list.get(&chat.id).unwrap().kind, ConvKind::Chat);
    }

    #[test]
    fn get_finds_by_id() {
        let mut list = ConversationList::new();
        let a = list.create(1);
        assert_eq!(list.get(&a.id).unwrap().id, a.id);
        assert!(list.get("nope").is_none());
    }

    #[test]
    fn rename_sets_custom_flag_and_rejects_blank() {
        let mut list = ConversationList::new();
        let a = list.create(1);
        assert!(list.rename(&a.id, "  Project notes  "));
        let c = list.get(&a.id).unwrap();
        assert_eq!(c.title, "Project notes"); // trimmed
        assert!(c.title_is_custom);
        // Blank rename is rejected and changes nothing.
        assert!(!list.rename(&a.id, "   "));
        assert_eq!(list.get(&a.id).unwrap().title, "Project notes");
        // Unknown id is a no-op `false`.
        assert!(!list.rename("nope", "x"));
    }

    #[test]
    fn active_sorts_by_updated_desc() {
        let mut list = ConversationList::new();
        let a = list.create(10);
        let b = list.create(20);
        let c = list.create(30);
        list.touch(&a.id, 99); // oldest created, newest activity
        let order: Vec<&str> = list.active().iter().map(|c| c.id.as_str()).collect();
        assert_eq!(order, vec![a.id.as_str(), c.id.as_str(), b.id.as_str()]);
    }

    #[test]
    fn active_tiebreaks_equal_updated_by_created_desc() {
        // When two conversations share an updated_ts, the more recently CREATED
        // one sorts first. Every other ordering test uses DISTINCT updated_ts
        // values, so the secondary created_ts key — and its DESC direction — is
        // otherwise unpinned: dropping the `.then(created_ts)` clause, or
        // reversing it to ascending, survives the whole suite.
        let mut list = ConversationList::new();
        let a = list.create(10); // created earlier
        let b = list.create(20); // created later
        list.touch(&a.id, 50);
        list.touch(&b.id, 50); // both now tie on updated_ts = 50
        let order: Vec<&str> = list.active().iter().map(|c| c.id.as_str()).collect();
        // Tie broken by created_ts DESC → later-created `b` before earlier `a`.
        assert_eq!(order, vec![b.id.as_str(), a.id.as_str()]);
    }

    // ---- auto-title -------------------------------------------------------------

    #[test]
    fn auto_title_passes_short_text_through() {
        assert_eq!(auto_title("How do I tune wgpu?"), "How do I tune wgpu?");
    }

    #[test]
    fn auto_title_collapses_whitespace_and_newlines() {
        assert_eq!(auto_title("  hello\n  world\t again "), "hello world again");
    }

    #[test]
    fn auto_title_truncates_at_word_boundary_with_ellipsis() {
        let t = auto_title("please explain the borrow checker rules for mutable references");
        assert!(t.ends_with('…'), "{t:?}");
        let body = t.trim_end_matches('…');
        assert!(body.chars().count() <= AUTO_TITLE_MAX_CHARS);
        // Ends on a whole word, not mid-word.
        assert!(!body.ends_with(' '));
        assert!("please explain the borrow checker rules for mutable references".contains(body));
    }

    #[test]
    fn auto_title_at_exactly_max_chars_passes_through_without_ellipsis() {
        // The `<=` in the fits-clean check (`joined.chars().count() <=
        // AUTO_TITLE_MAX_CHARS`) is the boundary between "return the title as-is"
        // and "truncate + append an ellipsis". Every other title test sits well
        // UNDER the cap (short text) or well OVER it (the truncation / hard-cut
        // cases), so a message that collapses to EXACTLY AUTO_TITLE_MAX_CHARS is
        // otherwise unpinned: tightening the guard to `<` spills an exact-fit
        // title into the truncation path, appending a spurious ellipsis to a
        // title that fit perfectly — and the whole suite still passes. Pin the
        // exact-fit case so tightening fails here and only here.
        let text = format!("{} y", "x".repeat(AUTO_TITLE_MAX_CHARS - 2));
        // Sanity: the collapsed form is exactly the cap, and carries a word
        // break so the truncation loop (were it wrongly taken) would run.
        assert_eq!(text.chars().count(), AUTO_TITLE_MAX_CHARS);
        let t = auto_title(&text);
        assert_eq!(t, text, "an exact-fit title must pass through unchanged");
        assert!(
            !t.ends_with('…'),
            "an exact-fit title must carry no ellipsis: {t:?}"
        );
        assert_eq!(t.chars().count(), AUTO_TITLE_MAX_CHARS);
    }

    #[test]
    fn auto_title_hard_cuts_a_single_giant_word() {
        let word = "x".repeat(100);
        let t = auto_title(&word);
        assert!(t.ends_with('…'));
        assert_eq!(t.chars().count(), AUTO_TITLE_MAX_CHARS + 1);
    }

    #[test]
    fn auto_title_handles_multibyte_at_the_boundary() {
        let text = "こんにちは ".repeat(20); // 5-char words, multibyte
        let t = auto_title(&text);
        assert!(t.ends_with('…'));
        assert!(t.trim_end_matches('…').chars().count() <= AUTO_TITLE_MAX_CHARS);
    }

    #[test]
    fn auto_title_of_blank_is_default() {
        assert_eq!(auto_title("   \n\t"), DEFAULT_TITLE);
    }

    #[test]
    fn first_user_message_titles_an_untouched_conversation() {
        let mut list = ConversationList::new();
        let a = list.create(1);
        let fired = list.note_user_message(&a.id, "Plan the garden beds", 5);
        assert_eq!(fired.as_deref(), Some("Plan the garden beds"));
        let c = list.get(&a.id).unwrap();
        assert_eq!(c.title, "Plan the garden beds");
        assert_eq!(c.updated_ts, 5);
        assert!(!c.title_is_custom, "auto-title is not a manual rename");
        // The SECOND user message must not retitle.
        assert!(list.note_user_message(&a.id, "different text", 6).is_none());
        assert_eq!(list.get(&a.id).unwrap().title, "Plan the garden beds");
    }

    #[test]
    fn manual_rename_blocks_auto_title() {
        let mut list = ConversationList::new();
        let a = list.create(1);
        list.rename(&a.id, "My title");
        assert!(list.note_user_message(&a.id, "first message", 2).is_none());
        assert_eq!(list.get(&a.id).unwrap().title, "My title");
        // updated_ts still bumps — it's activity either way.
        assert_eq!(list.get(&a.id).unwrap().updated_ts, 2);
    }

    #[test]
    fn manual_rename_to_the_default_string_still_blocks_auto_title() {
        // The auto-title guard is `!c.title_is_custom && c.title == DEFAULT_TITLE`.
        // The `c.title == DEFAULT_TITLE` half is well pinned (every fresh
        // conversation auto-titles on its first message), but the
        // `!c.title_is_custom` half has independent necessity ONLY when a user
        // deliberately renames to the exact default string: that sets
        // `title_is_custom = true` while `title` stays == DEFAULT_TITLE, so the
        // title check alone can't tell the manual rename apart from a fresh row.
        // No other test renames to DEFAULT_TITLE, so dropping the
        // `!c.title_is_custom` clause survives the whole suite — yet it would
        // silently retitle a conversation the user explicitly named
        // "New conversation". Pin it: a manual rename blocks auto-title even when
        // the chosen title happens to equal the default.
        let mut list = ConversationList::new();
        let a = list.create(1);
        assert!(list.rename(&a.id, DEFAULT_TITLE)); // custom rename, title == DEFAULT
        assert!(list.get(&a.id).unwrap().title_is_custom);
        assert_eq!(list.get(&a.id).unwrap().title, DEFAULT_TITLE);
        // First user message must NOT auto-title over the deliberate rename.
        assert!(list
            .note_user_message(&a.id, "some brand new topic", 2)
            .is_none());
        assert_eq!(list.get(&a.id).unwrap().title, DEFAULT_TITLE);
        // Activity still bumps updated_ts.
        assert_eq!(list.get(&a.id).unwrap().updated_ts, 2);
    }

    // ---- archive / unarchive invariants ----------------------------------------

    #[test]
    fn archive_is_soft_and_reversible() {
        let mut list = ConversationList::new();
        let a = list.create(1);
        list.rename(&a.id, "keep me");
        assert!(list.archive(&a.id));

        // Hidden from the default view, present in the archived section.
        assert!(list.active().is_empty());
        assert_eq!(list.archived().len(), 1);
        // NOTHING was destroyed: same id, same title, same timestamps.
        let c = list.get(&a.id).unwrap();
        assert_eq!(c.title, "keep me");
        assert_eq!(c.created_ts, 1);
        assert_eq!(list.len(), 1);

        // Unarchive restores it to the default view, data intact.
        assert!(list.unarchive(&a.id));
        assert_eq!(list.active().len(), 1);
        assert!(list.archived().is_empty());
        assert_eq!(list.get(&a.id).unwrap().title, "keep me");
    }

    #[test]
    fn archive_unknown_id_is_a_noop() {
        let mut list = ConversationList::new();
        list.create(1);
        assert!(!list.archive("nope"));
        assert!(!list.unarchive("nope"));
        assert_eq!(list.active().len(), 1);
    }

    #[test]
    fn archive_then_activity_then_unarchive_keeps_everything() {
        // Renames and touches while archived still apply — archived is a view
        // filter, not a freeze, and certainly not a delete.
        let mut list = ConversationList::new();
        let a = list.create(1);
        list.archive(&a.id);
        assert!(list.rename(&a.id, "renamed while archived"));
        list.touch(&a.id, 50);
        list.unarchive(&a.id);
        let c = list.get(&a.id).unwrap();
        assert_eq!(c.title, "renamed while archived");
        assert_eq!(c.updated_ts, 50);
    }

    // ---- RC2-CNCL1: Council conversations ------------------------------------

    /// RC2-CNCL1: a Council conversation is its own kind and keeps it
    /// through the persisted token.
    #[test]
    fn council_kind_round_trips() {
        assert_eq!(ConvKind::Council.as_str(), "council");
        assert_eq!(ConvKind::from_token("council"), ConvKind::Council);
        assert_eq!(
            ConvKind::from_token(ConvKind::Council.as_str()),
            ConvKind::Council
        );
        let mut list = ConversationList::new();
        let meta = list.create_kind(10, ConvKind::Council);
        assert_eq!(list.get(&meta.id).map(|c| c.kind), Some(ConvKind::Council));
    }

    /// RC2-CNCL1: a kind this build does not know still reads as Chat (an
    /// older build opening a newer database degrades the same way).
    #[test]
    fn unknown_kind_still_reads_as_chat() {
        for raw in ["", "councils", "COUNCIL", "debate", "future-kind"] {
            assert_eq!(ConvKind::from_token(raw), ConvKind::Chat, "{raw:?}");
        }
    }
}

#[cfg(test)]
mod image_title_regressions {
    use super::auto_title;
    #[test]
    fn screenshot_title_uses_the_question_without_encoded_payload() {
        let image = format!(
            "![pasted-image.png](data:image/png;base64,{})",
            "A".repeat(1024 * 1024)
        );
        assert_eq!(
            auto_title(&format!("{image}\n\nWhat is shown here?")),
            "What is shown here?"
        );
        assert_eq!(
            auto_title(&format!("{image}{image}Question after two images")),
            "Question after two images"
        );
        assert_eq!(auto_title(&image), "Image attachment");
        assert_eq!(
            auto_title("![broken](data:image/png;base64,unterminated"),
            "Image attachment"
        );
        assert_eq!(
            auto_title("Explain this data:image/png example"),
            "Explain this data:image/png example"
        );
    }
}
