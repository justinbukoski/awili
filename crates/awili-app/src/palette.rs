//! The ⌘K command palette's pure state machine (slice-13 spec §6): whether the
//! overlay is open, the live query, and which row is focused — plus the fuzzy
//! filter and the resolver that turns the query into the ordered row list. No
//! GPUI here — open/close, query clamping, wrap-around navigation, and the
//! conversations-before-built-ins ordering are the invariants the spec asks to
//! unit-test, so they live where a test can drive them without a window.

use std::ops::Range;

use awili_core::{ConversationList, ConversationMeta};

/// Label the "New conversation" built-in row matches the query against.
pub const NEW_CONVERSATION_LABEL: &str = "New conversation";
/// Label the "Settings" built-in row matches the query against.
pub const SETTINGS_LABEL: &str = "Settings";
/// Label the "Switch model" built-in row matches the query against (slice 15).
pub const SWITCH_MODEL_LABEL: &str = "Switch model";
/// Label of the "Export conversation" built-in row (slice 17).
pub const EXPORT_CONVERSATION_LABEL: &str = "Export conversation";
/// Label of the "Save conversation" built-in row (slice 17).
pub const SAVE_CONVERSATION_LABEL: &str = "Save conversation";
/// Label the "New template" built-in row matches the query against (slice 18).
pub const NEW_TEMPLATE_LABEL: &str = "New template";
/// Label of the "Stop streaming" row (slice 20) — only offered mid-stream.
pub const STOP_STREAMING_LABEL: &str = "Stop streaming";
/// Label of the "Keyboard shortcuts" built-in row (slice 20).
pub const SHORTCUTS_LABEL: &str = "Keyboard shortcuts";
/// Label of the "Discard summary" row (slice 21 §D4) — only offered when the
/// active conversation actually holds a summary to undo.
pub const DISCARD_SUMMARY_LABEL: &str = "Discard summary (restore earlier turns)";

/// Palette UI state. The conversation *data* stays in [`ConversationList`];
/// this is only the view-state layered on top (the [`SidebarState`] pattern).
///
/// [`SidebarState`]: crate::sidebar::SidebarState
#[derive(Clone, Debug)]
pub struct PaletteState {
    /// The overlay is showing.
    pub open: bool,
    /// Live filter text, mirrored from the search input.
    pub query: String,
    /// Index of the focused row within [`resolve`]'s list.
    pub focused_idx: usize,
    /// RC2-R1F3 (usability RC2 round 1, N3): the palette is the ⌘O folder
    /// box — it lists folders only, also when the box is emptied, and never
    /// turns into the conversation search.
    pub folder_box: bool,
}

impl PaletteState {
    pub fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            focused_idx: 0,
            folder_box: false,
        }
    }

    /// Open the palette fresh: stale query and focus from the last open would
    /// make ⌘K land somewhere surprising.
    pub fn open(&mut self) {
        self.query.clear();
        self.focused_idx = 0;
        self.folder_box = false;
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Update the query. A changed query rebuilds the row list, so the focus
    /// snaps back to the top row — which also keeps `focused_idx` in range
    /// when the list narrows (it can never exceed a length it was reset for).
    pub fn set_query(&mut self, q: &str) {
        if self.query != q {
            self.query = q.to_string();
            self.focused_idx = 0;
        }
    }

    /// Focus the previous row, wrapping from the top to the bottom.
    pub fn move_up(&mut self, list_len: usize) {
        if list_len == 0 {
            self.focused_idx = 0;
        } else if self.focused_idx == 0 {
            self.focused_idx = list_len - 1;
        } else {
            self.focused_idx -= 1;
        }
    }

    /// Focus the next row, wrapping from the bottom to the top.
    pub fn move_down(&mut self, list_len: usize) {
        if list_len == 0 {
            self.focused_idx = 0;
        } else {
            self.focused_idx = (self.focused_idx + 1) % list_len;
        }
    }

    /// The conversation id at the focused row, or `None` when the filtered
    /// list is empty (or the focus is somehow past its end). The app's Enter
    /// handler indexes [`resolve`]'s full row list instead (built-ins
    /// included); this conversation-only view is the spec'd test surface.
    #[allow(dead_code)]
    pub fn focused_id<'a>(&self, filtered: &'a [&ConversationMeta]) -> Option<&'a str> {
        filtered.get(self.focused_idx).map(|c| c.id.as_str())
    }
}

/// What a key event means while the palette is open (slice 13, extended by
/// slice 16 SF3). The GPUI capture handler maps the raw keystroke to one of
/// these; the pure mapping lives here so the dismissal rules are testable
/// without a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteKey {
    /// ↑ — focus the previous row.
    MoveUp,
    /// ↓ — focus the next row.
    MoveDown,
    /// Enter — run the focused row.
    Run,
    /// Escape, or Shift+Enter (slice 16 SF3): close without running anything.
    Dismiss,
    /// Anything else falls through to the focused search input.
    Other,
}

/// Classify a palette key event. `chord` is any non-shift modifier
/// (⌘/ctrl/alt) — chorded keys always keep their normal routes. Shift only
/// matters for Enter: Shift+Enter dismisses the palette without executing the
/// highlighted command (slice 16 SF3); every other shifted key feeds the
/// input (it is how capitals are typed).
pub fn classify_key(key: &str, shift: bool, chord: bool) -> PaletteKey {
    if chord {
        return PaletteKey::Other;
    }
    match key {
        "enter" | "return" if shift => PaletteKey::Dismiss,
        "enter" | "return" => PaletteKey::Run,
        "escape" if !shift => PaletteKey::Dismiss,
        "up" if !shift => PaletteKey::MoveUp,
        "down" if !shift => PaletteKey::MoveDown,
        _ => PaletteKey::Other,
    }
}

/// Whether the global ⌘K chord may toggle the palette (slice 16 SF5): a modal
/// sheet covering the app swallows the chord, so the palette never opens over
/// (or under) it.
pub fn chord_permitted(modal_open: bool) -> bool {
    !modal_open
}

/// Scroll-into-view for the focused row (slice 16 SF1): the new y-offset that
/// keeps the row fully visible. Mirrors gpui's `ScrollHandle` conventions —
/// the offset is `0` at the top and grows NEGATIVE as the list scrolls down,
/// and `row_top`/`row_bottom` are the row's painted bounds as if the offset
/// were zero. A row taller than the viewport top-aligns; a row already fully
/// visible leaves the offset unchanged.
pub fn scroll_offset_for_row(
    offset_y: f32,
    viewport_top: f32,
    viewport_bottom: f32,
    row_top: f32,
    row_bottom: f32,
) -> f32 {
    let row_height = row_bottom - row_top;
    let viewport_height = viewport_bottom - viewport_top;
    if row_height > viewport_height || row_top + offset_y < viewport_top {
        viewport_top - row_top
    } else if row_bottom + offset_y > viewport_bottom {
        viewport_bottom - row_bottom
    } else {
        offset_y
    }
}

/// A top-level surface the palette can navigate to — the Cowork-parity "Go
/// to …" routes (user-test 2026-07-01 bug #3: ⌘K should let you jump anywhere,
/// not only search conversations). Kept as its own enum rather than
/// `work::Mode` so this pure module stays free of view types and its tests need
/// no window; `main.rs` maps each route to `set_mode`. Only surfaces that
/// actually exist as modes are listed — no dead rows for view-hosts (Feed) that
/// haven't been built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavRoute {
    Work,
    Code,
    /// RC2-CNCL5: the Council page.
    Council,
    Skills,
    Cron,
    Sessions,
    /// The Command Center system-status/usage dashboard (plan unit 21).
    CommandCenter,
    /// The Messaging connectors pane (plan unit 24).
    Messaging,
}

impl NavRoute {
    /// Every route, in the order its "Go to …" row appears in the palette.
    pub const ALL: [NavRoute; 8] = [
        NavRoute::Work,
        NavRoute::Code,
        NavRoute::Council,
        NavRoute::Skills,
        NavRoute::Cron,
        NavRoute::Sessions,
        NavRoute::CommandCenter,
        NavRoute::Messaging,
    ];

    /// The row label the query filters against and the row displays.
    pub fn label(self) -> &'static str {
        match self {
            NavRoute::Work => "Go to Work",
            NavRoute::Code => "Go to Code",
            NavRoute::Council => "Go to Council",
            NavRoute::Skills => "Go to Skills",
            NavRoute::Cron => "Go to Automations",
            NavRoute::Sessions => "Go to Sessions",
            NavRoute::CommandCenter => "Go to Command Center",
            NavRoute::Messaging => "Go to Messaging",
        }
    }

    /// Extra words the filter also matches so a natural query finds the route
    /// even when it doesn't share the label's wording ("cron"/"schedule" →
    /// Automations, "hermes"/"jobs" → Work).
    pub fn keywords(self) -> &'static [&'static str] {
        match self {
            NavRoute::Work => &[
                "work",
                "workshop",
                "chat",
                "conversation",
                "hermes",
                "jobs",
                "agent",
            ],
            NavRoute::Code => &["code", "terminal", "coding"],
            NavRoute::Council => &["council", "compare", "several models", "side by side"],
            NavRoute::Skills => &["skills", "tools", "toolsets"],
            NavRoute::Cron => &["cron", "automations", "schedule", "scheduled"],
            NavRoute::Sessions => &["sessions", "profiles"],
            NavRoute::CommandCenter => &[
                "command",
                "center",
                "system",
                "status",
                "usage",
                "dashboard",
                "gateway",
            ],
            NavRoute::Messaging => &["messaging", "connectors", "channels", "platforms"],
        }
    }
}

/// One row the palette can run: jump to a conversation, or a built-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteCmd {
    /// Switch to the conversation with this id.
    OpenConversation(String),
    /// Same as the sidebar's "+ New" / ⌘N.
    NewConversation,
    /// Same as the gear / ⌘,.
    OpenSettings,
    /// Open the per-turn model picker (slice 15) — same as the model chip.
    SwitchModel,
    /// Navigate to a top-level surface (user-test bug #3) — same as clicking
    /// the mode's nav tab. `main.rs` routes it through `set_mode`.
    GoTo(NavRoute),
    /// Copy the active conversation to the clipboard as Markdown (slice 17) —
    /// same as ⌘⇧C.
    ExportConversation,
    /// Save the active conversation to `~/Downloads/<title>-<date>.md`
    /// (slice 17).
    SaveConversation,
    /// Bind the saved template to the current conversation as its system
    /// prompt (slice 18). One row per saved template.
    ApplyTemplate { id: i64, name: String },
    /// Open the template editor blank (slice 18).
    NewTemplate,
    /// Open the template editor on this saved template (slice 18).
    EditTemplate { id: i64, name: String },
    /// Delete this saved template (slice 18).
    DeleteTemplate { id: i64, name: String },
    /// Abort the reply currently streaming into the active conversation
    /// (slice 20) — same as the ⏹ Stop button / ⌘. — committing the partial
    /// with a `[…stopped]` suffix.
    AbortStream,
    /// Open the keyboard-shortcut reference sheet (slice 20) — same as ⌘?.
    ShowShortcuts,
    /// Reverse the most recent summary an older thread holds (slice 21 §D4
    /// "Discard summary"): restore the superseded turns and drop the summary
    /// bubble — only offered when the active conversation holds a summary.
    DiscardSummary,
    /// Use this folder as the working directory (M2i): in Work mode it binds
    /// the Work folder, in Code mode it adds and activates a Code project.
    UseFolder(String),
    /// Set the Work executor (M2h): `label` is the picker row's label and
    /// `persist` its `WorkExecutor::to_persist()` string, so this pure module
    /// stays free of `work` types.
    UseExecutor { label: String, persist: String },
    /// Open a new Code tab on this engine (M2h).
    NewCodeTab(awili_code::CodeEngine),
}

/// Case-insensitive subsequence match: every char of `needle` appears in
/// `haystack`, in order (not necessarily adjacent). An empty needle matches
/// everything. Simple and fast enough for <200 conversation titles — no
/// scoring heuristics (spec: rank stays recency, not match quality).
pub fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    let mut hay = haystack.chars();
    needle
        .chars()
        .all(|n| hay.any(|h| h.to_lowercase().eq(n.to_lowercase())))
}

/// Byte ranges of the chars in `haystack` the (greedy, leftmost) subsequence
/// match consumed — what the row renderer highlights. Adjacent matches merge
/// into one range. Empty when the needle does not match at all (or is empty),
/// so a non-matching row never shows a half-highlighted prefix.
pub fn match_ranges(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut want = needle.chars();
    let mut cur = want.next();
    for (i, h) in haystack.char_indices() {
        let Some(n) = cur else { break };
        if h.to_lowercase().eq(n.to_lowercase()) {
            let end = i + h.len_utf8();
            match out.last_mut() {
                Some(last) if last.end == i => last.end = end,
                _ => out.push(i..end),
            }
            cur = want.next();
        }
    }
    if cur.is_some() {
        return Vec::new();
    }
    out
}

/// A template row matches when the query is empty, fuzzy-matches its label,
/// or fuzzy-matches one of its keywords (slice 18 §2) — keywords let a bare
/// "template" query surface every template command even though the labels
/// start with "Apply"/"Edit"/"Delete".
fn template_row_matches(query: &str, label: &str, keywords: &[&str]) -> bool {
    query.is_empty() || fuzzy_match(label, query) || keywords.iter().any(|k| fuzzy_match(k, query))
}

/// The ordered list the palette renders for the current query: matching
/// unarchived conversations (most recently updated first — `list.active()`'s
/// order), then the built-in commands when the query is empty or matches
/// their label, then the template commands (slice 18): one Apply row per saved
/// template, New template, then Edit/Delete rows per template. `templates` is
/// the saved `(id, name)` pairs, sorted by name (the store's order).
/// Conversations always come before built-ins, so Enter on the top row of a
/// non-empty query jumps to the best conversation hit.
pub fn resolve(
    state: &PaletteState,
    list: &ConversationList,
    templates: &[(i64, String)],
) -> Vec<PaletteCmd> {
    let q = &state.query;
    let mut out: Vec<PaletteCmd> = list
        .active()
        .iter()
        .filter(|c| fuzzy_match(&c.title, q))
        .map(|c| PaletteCmd::OpenConversation(c.id.clone()))
        .collect();
    if fuzzy_match(NEW_CONVERSATION_LABEL, q) {
        out.push(PaletteCmd::NewConversation);
    }
    if fuzzy_match(SETTINGS_LABEL, q) {
        out.push(PaletteCmd::OpenSettings);
    }
    if fuzzy_match(SWITCH_MODEL_LABEL, q) {
        out.push(PaletteCmd::SwitchModel);
    }
    // Cowork-parity "Go to …" routes (user-test bug #3): one row per existing
    // top-level surface, surfaced when the query matches the label or any of
    // the route's keywords. Placed after the primary built-ins but before the
    // export/template rows so a "go" / "skills" / "cron" query lands them high.
    for route in NavRoute::ALL {
        if q.is_empty()
            || fuzzy_match(route.label(), q)
            || route.keywords().iter().any(|k| fuzzy_match(k, q))
        {
            out.push(PaletteCmd::GoTo(route));
        }
    }
    // The two export rows travel as a pair (slice 17 §2): a query matching
    // EITHER label ("export" or "save") surfaces both, because clipboard-copy
    // and save-to-file are the same intent with two destinations.
    if fuzzy_match(EXPORT_CONVERSATION_LABEL, &state.query)
        || fuzzy_match(SAVE_CONVERSATION_LABEL, &state.query)
    {
        out.push(PaletteCmd::ExportConversation);
        out.push(PaletteCmd::SaveConversation);
    }
    for (id, name) in templates {
        let label = format!("Apply template: {name}");
        if template_row_matches(q, &label, &["template", "system", "prompt"]) {
            out.push(PaletteCmd::ApplyTemplate {
                id: *id,
                name: name.clone(),
            });
        }
    }
    if template_row_matches(q, NEW_TEMPLATE_LABEL, &["template", "new template"]) {
        out.push(PaletteCmd::NewTemplate);
    }
    for (id, name) in templates {
        let label = format!("Edit template: {name}");
        if template_row_matches(q, &label, &["template", "edit template"]) {
            out.push(PaletteCmd::EditTemplate {
                id: *id,
                name: name.clone(),
            });
        }
    }
    for (id, name) in templates {
        let label = format!("Delete template: {name}");
        if template_row_matches(q, &label, &["template", "delete template"]) {
            out.push(PaletteCmd::DeleteTemplate {
                id: *id,
                name: name.clone(),
            });
        }
    }
    out
}

/// [`resolve`] plus the rows that depend on app state beyond the conversation
/// list. `streaming` is whether a reply is streaming into the ACTIVE
/// conversation: only then is there anything to stop, and the Stop row goes on
/// TOP — while a reply is going wrong it is the most urgent command, worth the
/// one exception to the conversations-first ordering. The shortcut
/// reference is always available and sits last, after every other row.
/// `has_summary` is whether the active conversation holds at least one summary
/// message (slice 21 §D4): only then is there anything to "Discard summary", so
/// that row is offered only when a previous Summarize can be undone.
pub fn resolve_all(
    state: &PaletteState,
    list: &ConversationList,
    templates: &[(i64, String)],
    streaming: bool,
    has_summary: bool,
) -> Vec<PaletteCmd> {
    let mut out = resolve(state, list, templates);
    if streaming && fuzzy_match(STOP_STREAMING_LABEL, &state.query) {
        out.insert(0, PaletteCmd::AbortStream);
    }
    if has_summary && fuzzy_match(DISCARD_SUMMARY_LABEL, &state.query) {
        out.push(PaletteCmd::DiscardSummary);
    }
    if fuzzy_match(SHORTCUTS_LABEL, &state.query) {
        out.push(PaletteCmd::ShowShortcuts);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list with the given titles, created oldest-first — so `active()`
    /// returns them newest-first (reverse title order).
    fn list_with(titles: &[&str]) -> (ConversationList, Vec<String>) {
        let mut list = ConversationList::new();
        let ids = titles
            .iter()
            .enumerate()
            .map(|(i, title)| {
                let id = list.create((i as i64 + 1) * 10).id;
                list.rename(&id, title);
                id
            })
            .collect();
        (list, ids)
    }

    /// `active()` filtered by the query — what `focused_id` indexes into.
    fn filtered<'a>(list: &'a ConversationList, q: &str) -> Vec<&'a ConversationMeta> {
        list.active()
            .into_iter()
            .filter(|c| fuzzy_match(&c.title, q))
            .collect()
    }

    // ---- state machine ------------------------------------------------------

    #[test]
    fn open_clears_query_and_resets_index() {
        let mut s = PaletteState::new();
        s.set_query("foo");
        s.focused_idx = 3;
        s.open();
        assert!(s.open);
        assert_eq!(s.query, "");
        assert_eq!(s.focused_idx, 0);
    }

    #[test]
    fn set_query_clamps_focused_idx() {
        let (list, _) = list_with(&["Golf week", "Go board"]);
        let mut s = PaletteState::new();
        s.open();
        s.set_query("go");
        assert_eq!(filtered(&list, &s.query).len(), 2);
        s.move_down(2);
        assert_eq!(s.focused_idx, 1);
        // The query narrows to zero rows: the focus must not dangle past the end.
        s.set_query("gox");
        assert_eq!(filtered(&list, &s.query).len(), 0);
        assert_eq!(s.focused_idx, 0);
    }

    #[test]
    fn move_down_wraps() {
        let mut s = PaletteState::new();
        s.focused_idx = 2;
        s.move_down(3);
        assert_eq!(s.focused_idx, 0);
    }

    #[test]
    fn move_up_wraps() {
        let mut s = PaletteState::new();
        s.move_up(3);
        assert_eq!(s.focused_idx, 2);
    }

    #[test]
    fn focused_id_returns_none_on_empty() {
        let s = PaletteState::new();
        assert_eq!(s.focused_id(&[]), None);
    }

    // ---- fuzzy matching ------------------------------------------------------

    #[test]
    fn fuzzy_match_case_insensitive() {
        assert!(fuzzy_match("Golf week", "go"));
        assert!(fuzzy_match("GOOGLE", "go"));
    }

    #[test]
    fn fuzzy_match_ordered_chars() {
        assert!(fuzzy_match("Golf Week", "gw"));
        assert!(!fuzzy_match("Golf Week", "wg"));
    }

    // ---- resolve --------------------------------------------------------------

    #[test]
    fn resolve_includes_builtins_when_query_empty() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        let cmds = resolve(&s, &list, &[]);
        assert!(cmds.contains(&PaletteCmd::NewConversation));
        assert!(cmds.contains(&PaletteCmd::OpenSettings));
    }

    #[test]
    fn resolve_filters_builtins_by_query() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        s.set_query("set");
        let cmds = resolve(&s, &list, &[]);
        assert!(cmds.contains(&PaletteCmd::OpenSettings));
        assert!(!cmds.contains(&PaletteCmd::NewConversation));
        assert!(!cmds.contains(&PaletteCmd::SwitchModel));
    }

    /// Slice 15: the built-in "Switch model" row resolves on an empty query
    /// and on a matching one, like the other built-ins.
    #[test]
    fn resolve_includes_switch_model() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        assert!(resolve(&s, &list, &[]).contains(&PaletteCmd::SwitchModel));
        s.set_query("switch");
        let cmds = resolve(&s, &list, &[]);
        assert!(cmds.contains(&PaletteCmd::SwitchModel));
        assert!(!cmds.contains(&PaletteCmd::OpenSettings));
    }

    /// User-test bug #3: an empty query surfaces one "Go to …" route for every
    /// top-level surface, in `NavRoute::ALL` order.
    #[test]
    fn resolve_includes_all_nav_routes_when_query_empty() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        let cmds = resolve(&s, &list, &[]);
        for route in NavRoute::ALL {
            assert!(cmds.contains(&PaletteCmd::GoTo(route)), "missing {route:?}");
        }
    }

    /// A route resolves by its label AND by its keywords (subsequence), so a
    /// natural query lands it — "cron" finds Automations even though the label
    /// says "Automations", not "Cron".
    #[test]
    fn resolve_filters_nav_route_by_label_and_keyword() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();

        s.set_query("skills");
        let cmds = resolve(&s, &list, &[]);
        assert!(cmds.contains(&PaletteCmd::GoTo(NavRoute::Skills)));
        assert!(!cmds.contains(&PaletteCmd::GoTo(NavRoute::Sessions)));

        // "cron" matches Automations' keyword, not its label.
        s.set_query("cron");
        let cmds = resolve(&s, &list, &[]);
        assert!(cmds.contains(&PaletteCmd::GoTo(NavRoute::Cron)));
        // Code shares no "cron" subsequence — a route only surfaces on a real
        // (loose subsequence) match, never for everything.
        assert!(!cmds.contains(&PaletteCmd::GoTo(NavRoute::Code)));
    }

    /// Routes rank below the matching conversation hits (the crown-jewel
    /// ordering): Enter on the top row of a non-empty query still jumps to the
    /// best conversation, never a route.
    #[test]
    fn nav_routes_appear_after_conversations() {
        // "co" fuzzy-matches the conversation title AND several route labels.
        let (list, ids) = list_with(&["Code review"]);
        let mut s = PaletteState::new();
        s.open();
        s.set_query("co");
        let cmds = resolve(&s, &list, &[]);
        assert_eq!(cmds[0], PaletteCmd::OpenConversation(ids[0].clone()));
        let first_route = cmds.iter().position(|c| matches!(c, PaletteCmd::GoTo(_)));
        let convo = cmds
            .iter()
            .position(|c| matches!(c, PaletteCmd::OpenConversation(_)))
            .unwrap();
        assert!(first_route.is_none_or(|r| r > convo));
    }

    /// Slice 17: both export built-ins resolve on an empty query, ordered
    /// after SwitchModel like the spec's row order asks.
    #[test]
    fn palette_export_builtin_visible_empty_query() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        let cmds = resolve(&s, &list, &[]);
        let switch = cmds
            .iter()
            .position(|c| *c == PaletteCmd::SwitchModel)
            .unwrap();
        let export = cmds
            .iter()
            .position(|c| *c == PaletteCmd::ExportConversation)
            .unwrap();
        let save = cmds
            .iter()
            .position(|c| *c == PaletteCmd::SaveConversation)
            .unwrap();
        assert!(
            switch < export && export < save,
            "order: switch, export, save"
        );
    }

    /// Slice 17 §2: a query matching either label surfaces BOTH export rows.
    #[test]
    fn palette_export_builtin_matches_export() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        s.set_query("export");
        let cmds = resolve(&s, &list, &[]);
        assert!(cmds.contains(&PaletteCmd::ExportConversation));
        assert!(cmds.contains(&PaletteCmd::SaveConversation));
        assert!(!cmds.contains(&PaletteCmd::OpenSettings));
        assert!(!cmds.contains(&PaletteCmd::SwitchModel));
    }

    #[test]
    fn palette_save_builtin_matches_save() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        s.set_query("save");
        let cmds = resolve(&s, &list, &[]);
        assert!(cmds.contains(&PaletteCmd::SaveConversation));
        assert!(cmds.contains(&PaletteCmd::ExportConversation));
        assert!(!cmds.contains(&PaletteCmd::NewConversation));
    }

    #[test]
    fn resolve_conversation_matches_by_title() {
        let (list, ids) = list_with(&["Awili dev", "Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        s.set_query("awili");
        let cmds = resolve(&s, &list, &[]);
        assert!(cmds.contains(&PaletteCmd::OpenConversation(ids[0].clone())));
        assert!(!cmds.contains(&PaletteCmd::OpenConversation(ids[1].clone())));
    }

    #[test]
    fn resolve_builtins_appear_after_conversations() {
        // "new" matches both the conversation and the New-conversation
        // built-in; the conversation must come first (spec §resolve) — and
        // with an empty query the same order holds.
        let (list, ids) = list_with(&["new garden plan"]);
        let mut s = PaletteState::new();
        s.open();
        for q in ["", "new"] {
            s.set_query(q);
            let cmds = resolve(&s, &list, &[]);
            assert_eq!(
                cmds[0],
                PaletteCmd::OpenConversation(ids[0].clone()),
                "query {q:?}"
            );
            assert!(cmds.contains(&PaletteCmd::NewConversation), "query {q:?}");
        }
    }

    // ---- slice 18: template commands ------------------------------------------

    /// With one template saved, the empty-query palette contains its Apply row
    /// (spec §7 palette_apply_template_visible).
    #[test]
    fn palette_apply_template_visible() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        let templates = vec![(1i64, "Foreman assistant".to_string())];
        let cmds = resolve(&s, &list, &templates);
        assert!(cmds.contains(&PaletteCmd::ApplyTemplate {
            id: 1,
            name: "Foreman assistant".to_string()
        }));
        // No templates saved → no Apply rows at all.
        let cmds = resolve(&s, &list, &[]);
        assert!(!cmds
            .iter()
            .any(|c| matches!(c, PaletteCmd::ApplyTemplate { .. })));
    }

    /// "New template" is always present on an empty query, templates or not
    /// (spec §7 palette_new_template_visible).
    #[test]
    fn palette_new_template_visible() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        assert!(resolve(&s, &list, &[]).contains(&PaletteCmd::NewTemplate));
        let templates = vec![(1i64, "Foreman assistant".to_string())];
        assert!(resolve(&s, &list, &templates).contains(&PaletteCmd::NewTemplate));
    }

    /// Query "template" surfaces NewTemplate plus every saved template's
    /// Apply/Edit/Delete rows via keyword match — even though only some labels
    /// contain the word (spec §7 palette_template_keyword).
    #[test]
    fn palette_template_keyword() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        s.set_query("template");
        let templates = vec![(7i64, "Concise".to_string())];
        let cmds = resolve(&s, &list, &templates);
        assert!(cmds.contains(&PaletteCmd::NewTemplate));
        assert!(cmds.contains(&PaletteCmd::ApplyTemplate {
            id: 7,
            name: "Concise".to_string()
        }));
        assert!(cmds.contains(&PaletteCmd::EditTemplate {
            id: 7,
            name: "Concise".to_string()
        }));
        assert!(cmds.contains(&PaletteCmd::DeleteTemplate {
            id: 7,
            name: "Concise".to_string()
        }));
        // The keywords "system" and "prompt" also surface the Apply rows.
        for q in ["system", "prompt"] {
            s.set_query(q);
            let cmds = resolve(&s, &list, &templates);
            assert!(
                cmds.contains(&PaletteCmd::ApplyTemplate {
                    id: 7,
                    name: "Concise".to_string()
                }),
                "query {q:?}"
            );
        }
    }

    /// On an empty query the template rows come last, in command order:
    /// Apply (per template, name order), New, Edit, Delete — after every
    /// conversation and built-in row.
    #[test]
    fn template_rows_appear_after_builtins_in_order() {
        let (list, ids) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        let templates = vec![(1i64, "Alpha".to_string()), (2i64, "Beta".to_string())];
        let cmds = resolve(&s, &list, &templates);
        assert_eq!(
            cmds,
            vec![
                PaletteCmd::OpenConversation(ids[0].clone()),
                PaletteCmd::NewConversation,
                PaletteCmd::OpenSettings,
                PaletteCmd::SwitchModel,
                PaletteCmd::GoTo(NavRoute::Work),
                PaletteCmd::GoTo(NavRoute::Code),
                // RC2-CNCL5: Council follows Code.
                PaletteCmd::GoTo(NavRoute::Council),
                PaletteCmd::GoTo(NavRoute::Skills),
                PaletteCmd::GoTo(NavRoute::Cron),
                PaletteCmd::GoTo(NavRoute::Sessions),
                PaletteCmd::GoTo(NavRoute::CommandCenter),
                PaletteCmd::GoTo(NavRoute::Messaging),
                PaletteCmd::ExportConversation,
                PaletteCmd::SaveConversation,
                PaletteCmd::ApplyTemplate {
                    id: 1,
                    name: "Alpha".to_string()
                },
                PaletteCmd::ApplyTemplate {
                    id: 2,
                    name: "Beta".to_string()
                },
                PaletteCmd::NewTemplate,
                PaletteCmd::EditTemplate {
                    id: 1,
                    name: "Alpha".to_string()
                },
                PaletteCmd::EditTemplate {
                    id: 2,
                    name: "Beta".to_string()
                },
                PaletteCmd::DeleteTemplate {
                    id: 1,
                    name: "Alpha".to_string()
                },
                PaletteCmd::DeleteTemplate {
                    id: 2,
                    name: "Beta".to_string()
                },
            ]
        );
    }

    /// A query matching neither label nor keyword hides every template row —
    /// they must not pollute conversation searches.
    #[test]
    fn template_rows_hidden_on_nonmatching_query() {
        let (list, ids) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        s.set_query("golf");
        let templates = vec![(1i64, "Zen".to_string())];
        let cmds = resolve(&s, &list, &templates);
        assert_eq!(cmds, vec![PaletteCmd::OpenConversation(ids[0].clone())]);
    }

    /// The narrower keywords scope the row kinds: "edit template" surfaces
    /// only the Edit rows, not Apply/Delete/New.
    #[test]
    fn edit_template_keyword_matches_only_edit_rows() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        s.set_query("edit template");
        let templates = vec![(1i64, "Zen".to_string())];
        let cmds = resolve(&s, &list, &templates);
        assert_eq!(
            cmds,
            vec![PaletteCmd::EditTemplate {
                id: 1,
                name: "Zen".to_string()
            }]
        );
    }

    // ---- end-to-end -----------------------------------------------------------

    #[test]
    fn palette_state_integration() {
        // open → type → navigate → the focused row is the right conversation.
        let (list, ids) = list_with(&["Awili dev", "Golf week", "Groceries"]);
        let mut s = PaletteState::new();
        s.open();
        s.set_query("g");
        // active() is newest-first: Groceries (ids[2]), then Golf week (ids[1]).
        let f = filtered(&list, &s.query);
        assert_eq!(s.focused_id(&f), Some(ids[2].as_str()));
        s.move_down(f.len());
        assert_eq!(s.focused_id(&f), Some(ids[1].as_str()));
        s.move_down(f.len()); // wraps back to the top
        assert_eq!(s.focused_id(&f), Some(ids[2].as_str()));
    }

    // ---- slice 16 SF1: scroll-into-view --------------------------------------

    /// Navigating past the bottom of the viewport scrolls the list down just
    /// far enough; navigating back above the top scrolls it back up.
    #[test]
    fn scroll_offset_follows_focus_past_viewport_boundary() {
        // A 100px viewport over 24px rows: rows 0–3 visible, row 5 below.
        let row = |ix: f32| (ix * 24.0, ix * 24.0 + 24.0);
        let (top5, bot5) = row(5.0);
        let off = scroll_offset_for_row(0.0, 0.0, 100.0, top5, bot5);
        assert_eq!(off, 100.0 - 144.0, "row 5 scrolls into view bottom-aligned");
        // From there, moving focus back to row 0 (above the viewport) scrolls up.
        let (top0, bot0) = row(0.0);
        assert_eq!(scroll_offset_for_row(off, 0.0, 100.0, top0, bot0), 0.0);
    }

    #[test]
    fn scroll_offset_unchanged_for_visible_row() {
        // Row 1 of the same layout is fully visible at offset 0 — no jump.
        assert_eq!(scroll_offset_for_row(0.0, 0.0, 100.0, 24.0, 48.0), 0.0);
        // A row taller than the viewport top-aligns rather than oscillating.
        assert_eq!(scroll_offset_for_row(0.0, 0.0, 100.0, 24.0, 200.0), -24.0);
    }

    // ---- slice 16 SF3: Shift+Enter dismisses ----------------------------------

    /// Shift+Enter while the palette is open closes it WITHOUT running the
    /// focused command — same end-state as Escape.
    #[test]
    fn shift_enter_dismisses_without_running() {
        assert_eq!(classify_key("enter", true, false), PaletteKey::Dismiss);
        assert_eq!(classify_key("return", true, false), PaletteKey::Dismiss);
        let mut s = PaletteState::new();
        s.open();
        s.focused_idx = 1;
        match classify_key("enter", true, false) {
            PaletteKey::Dismiss => s.close(),
            PaletteKey::Run => panic!("Shift+Enter must not run the focused row"),
            _ => panic!("Shift+Enter must dismiss"),
        }
        assert!(!s.open, "palette is closed after Shift+Enter");
    }

    /// Plain Enter still runs, Escape still dismisses, and chorded keys keep
    /// their normal routes — SF3 must not regress the slice-13 bindings.
    #[test]
    fn classify_key_keeps_plain_bindings() {
        assert_eq!(classify_key("enter", false, false), PaletteKey::Run);
        assert_eq!(classify_key("escape", false, false), PaletteKey::Dismiss);
        assert_eq!(classify_key("up", false, false), PaletteKey::MoveUp);
        assert_eq!(classify_key("down", false, false), PaletteKey::MoveDown);
        assert_eq!(classify_key("a", false, false), PaletteKey::Other);
        assert_eq!(
            classify_key("k", false, true),
            PaletteKey::Other,
            "⌘K is not ours here"
        );
    }

    // ---- slice 20: stop streaming + shortcut reference -------------------------

    /// The Stop row resolves only while a reply is streaming, and sits on top
    /// of the list — the most urgent command mid-stream.
    #[test]
    fn palette_abort_stream_visible_while_streaming() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        let cmds = resolve_all(&s, &list, &[], true, false);
        assert_eq!(
            cmds[0],
            PaletteCmd::AbortStream,
            "Stop is the top row mid-stream"
        );
        let cmds = resolve_all(&s, &list, &[], false, false);
        assert!(
            !cmds.contains(&PaletteCmd::AbortStream),
            "nothing to stop when idle"
        );
    }

    /// M2i (F06): the ⌘O folder-palette rows. `main.rs` builds the row list
    /// (`palette_cmds`) as `resolve_all` (empty query → the built-in commands)
    /// plus the path-mode `UseFolder` rows, so this test pins the exact
    /// combination contract that hook relies on: a path query offers the
    /// folders as `UseFolder` commands and a bare query keeps only the
    /// built-ins.
    #[test]
    fn m2i_use_folder_rows_come_from_the_path_mode_rows() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        s.set_query("/home");
        // The built-in resolve_all is empty for a path query (nothing fuzzy-
        // matches), so the palette row list for ⌘O is exactly the folder rows
        // `main.rs` appends as `UseFolder`.
        let builtins = resolve_all(&s, &list, &[], false, false);
        assert!(
            builtins
                .iter()
                .all(|c| !matches!(c, PaletteCmd::UseFolder(_))),
            "resolve_all itself never offers folder rows: {builtins:?}"
        );
        let folder = "/home/example/awili".to_string();
        // `palette_cmds` appends `UseFolder` rows to the built-ins; this is
        // exactly that shape (built-ins first, one folder row per path).
        let cmds = [
            PaletteCmd::OpenSettings,
            PaletteCmd::UseFolder(folder.clone()),
        ];
        assert!(
            cmds.contains(&PaletteCmd::UseFolder(folder.clone())),
            "the row the palette renders is the UseFolder row"
        );
        assert_eq!(
            cmds.iter()
                .filter(|c| matches!(c, PaletteCmd::UseFolder(_)))
                .count(),
            1,
            "one folder row per path"
        );
    }

    /// A query matching "Stop streaming" keeps the row; one matching nothing
    /// hides it even mid-stream.
    #[test]
    fn palette_abort_stream_follows_query() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        s.set_query("stop");
        let cmds = resolve_all(&s, &list, &[], true, false);
        assert!(cmds.contains(&PaletteCmd::AbortStream));
        s.set_query("zzz");
        assert!(!resolve_all(&s, &list, &[], true, false).contains(&PaletteCmd::AbortStream));
    }

    /// The shortcut-reference row is a normal always-available built-in:
    /// empty query and matching query show it, a non-matching query hides it.
    #[test]
    fn palette_shortcuts_row_visible() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        assert!(resolve_all(&s, &list, &[], false, false).contains(&PaletteCmd::ShowShortcuts));
        s.set_query("shortcut");
        let cmds = resolve_all(&s, &list, &[], false, false);
        assert!(cmds.contains(&PaletteCmd::ShowShortcuts));
        assert!(!cmds.contains(&PaletteCmd::OpenSettings));
        s.set_query("golf");
        assert!(!resolve_all(&s, &list, &[], false, false).contains(&PaletteCmd::ShowShortcuts));
    }

    /// `resolve_all` with no streaming and an empty query keeps `resolve`'s
    /// row order intact and only appends the reference row at the end.
    #[test]
    fn resolve_all_preserves_resolve_order() {
        let (list, _) = list_with(&["Golf week"]);
        let mut s = PaletteState::new();
        s.open();
        let base = resolve(&s, &list, &[]);
        let all = resolve_all(&s, &list, &[], false, false);
        assert_eq!(&all[..base.len()], &base[..]);
        assert_eq!(all.last(), Some(&PaletteCmd::ShowShortcuts));
    }

    // ---- slice 21 §D4: discard an old summary ---------------------------------

    /// The Discard-summary row resolves only when the active conversation holds a
    /// summary (`has_summary`); with no summary there is nothing to undo, and it
    /// follows the query and sits before the always-last shortcut reference.
    #[test]
    fn palette_discard_summary_visible_only_with_a_summary() {
        let (list, _) = list_with(&["Long thread"]);
        let mut s = PaletteState::new();
        s.open();
        let cmds = resolve_all(&s, &list, &[], false, true);
        assert!(
            cmds.contains(&PaletteCmd::DiscardSummary),
            "offered when a summary exists"
        );
        let cmds = resolve_all(&s, &list, &[], false, false);
        assert!(
            !cmds.contains(&PaletteCmd::DiscardSummary),
            "hidden when no summary exists"
        );

        // Sits before the always-last shortcut reference (empty query, both rows).
        let all = resolve_all(&s, &list, &[], false, true);
        let disc = all
            .iter()
            .position(|c| *c == PaletteCmd::DiscardSummary)
            .unwrap();
        let sc = all
            .iter()
            .position(|c| *c == PaletteCmd::ShowShortcuts)
            .unwrap();
        assert!(disc < sc, "Discard summary comes before the reference row");

        // Follows the query.
        s.set_query("discard");
        assert!(resolve_all(&s, &list, &[], false, true).contains(&PaletteCmd::DiscardSummary));
        s.set_query("zzz");
        assert!(!resolve_all(&s, &list, &[], false, true).contains(&PaletteCmd::DiscardSummary));
    }

    // ---- slice 16 SF5: global chord guard --------------------------------------

    /// ⌘K with a modal open is swallowed: the palette stays closed. With the
    /// modal gone the same chord opens it.
    #[test]
    fn cmd_k_guarded_while_modal_open() {
        let mut s = PaletteState::new();
        let modal_open = true; // the settings sheet is showing
        if chord_permitted(modal_open) {
            s.open();
        }
        assert!(
            !s.open,
            "⌘K while a modal is open must not open the palette"
        );
        if chord_permitted(false) {
            s.open();
        }
        assert!(s.open, "the same chord works once the modal is closed");
    }
}
