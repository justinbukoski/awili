//! Conversation full-text search state (slice 14) — the pure, GPUI-free core
//! behind the ⌘F panel, same pattern as the sidebar/settings state modules.
//!
//! [`SearchState`] owns the panel's open flag, the query, and keyboard focus
//! over the result rows. The result rows themselves live on the app (they are
//! re-queried as the user types); the helpers below assemble them:
//! [`title_hits`] surfaces conversations whose TITLE matches (spec §"Title
//! search too" — these rank above body matches), [`merge_hits`] combines them
//! with the FTS body hits without duplicates, and [`snippet_runs`] splits an
//! FTS5 `**bold**` snippet into renderable (text, bold) runs.

use awili_persist::SearchResult;

/// The ⌘F search panel's pure state: open flag, query, and which result row
/// keyboard focus is on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchState {
    pub open: bool,
    pub query: String,
    pub focused_idx: usize,
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the panel fresh: empty query, focus on the first row.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.focused_idx = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Record the new query and clamp the focused row to the new result count
    /// (results shrink as the query narrows; focus must never dangle).
    pub fn set_query(&mut self, q: &str, result_count: usize) {
        self.query = q.to_string();
        self.focused_idx = self.focused_idx.min(result_count.saturating_sub(1));
    }

    /// Move focus up one row, wrapping from the first row to the last.
    /// `result_count` of zero is a no-op.
    pub fn move_up(&mut self, result_count: usize) {
        if result_count > 0 {
            self.focused_idx = (self.focused_idx + result_count - 1) % result_count;
        }
    }

    /// Move focus down one row, wrapping from the last row to the first.
    pub fn move_down(&mut self, result_count: usize) {
        if result_count > 0 {
            self.focused_idx = (self.focused_idx + 1) % result_count;
        }
    }

    /// The conversation id of the focused result row, if any.
    pub fn focused_id<'a>(&self, results: &'a [SearchResult]) -> Option<&'a str> {
        results
            .get(self.focused_idx)
            .map(|r| r.conversation_id.as_str())
    }
}

/// Split an FTS5 snippet with `**bold**` markers into (text, is_bold) runs,
/// in order. Text outside markers is plain; an unbalanced trailing marker
/// bolds to the end of the string (never panics, never drops text).
pub fn snippet_runs(snippet: &str) -> Vec<(String, bool)> {
    let mut runs = Vec::new();
    for (i, piece) in snippet.split("**").enumerate() {
        if !piece.is_empty() {
            runs.push((piece.to_string(), i % 2 == 1));
        }
    }
    runs
}

/// Conversations whose TITLE matches `query` (case-insensitive substring), in
/// the order given — callers pass them most-recently-updated first, mirroring
/// the body-hit ranking. The hit's snippet is the title with the matched span
/// wrapped in the same `**bold**` markers FTS5 snippets use.
pub fn title_hits(query: &str, conversations: &[(String, String)]) -> Vec<SearchResult> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    conversations
        .iter()
        .filter_map(|(id, title)| {
            title_match_snippet(title, query).map(|snippet| SearchResult {
                conversation_id: id.clone(),
                snippet,
            })
        })
        .collect()
}

/// If `title` contains `query` case-insensitively, the title with the first
/// matched span wrapped in `**…**`; `None` otherwise. Comparison is per-char
/// `to_lowercase`, so multi-byte text never splits a character.
fn title_match_snippet(title: &str, query: &str) -> Option<String> {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return None;
    }
    let chars: Vec<(usize, char)> = title.char_indices().collect();
    for i in 0..chars.len() {
        let mut qi = 0;
        let mut j = i;
        while qi < q.len() && j < chars.len() {
            let mut ok = true;
            for c in chars[j].1.to_lowercase() {
                if qi < q.len() {
                    if c != q[qi] {
                        ok = false;
                        break;
                    }
                    qi += 1;
                }
            }
            if !ok {
                break;
            }
            j += 1;
        }
        if qi == q.len() {
            let start = chars[i].0;
            let end = chars.get(j).map(|&(b, _)| b).unwrap_or(title.len());
            return Some(format!(
                "{}**{}**{}",
                &title[..start],
                &title[start..end],
                &title[end..]
            ));
        }
    }
    None
}

/// Why the app's `highlight_term` is changing (slice 16 §3). The transitions
/// are trivial but live here, pure and testable: selecting a result lights the
/// term up in the opened conversation's bubbles; Escape (anywhere) or the
/// panel closing puts it out.
#[derive(Clone, Copy, Debug)]
pub enum HighlightEvent<'a> {
    /// A search result was selected — highlight the (trimmed) query.
    Select(&'a str),
    /// Escape, or the search panel closed: no more highlight.
    Clear,
}

/// The next value of `highlight_term` after `ev`. A blank query never
/// highlights (there is nothing to match).
pub fn next_highlight(ev: HighlightEvent) -> Option<String> {
    match ev {
        HighlightEvent::Select(query) => {
            let q = query.trim();
            (!q.is_empty()).then(|| q.to_string())
        }
        HighlightEvent::Clear => None,
    }
}

/// Split `text` into `(run, is_match)` spans around every case-insensitive
/// occurrence of `term` (slice 16 §3) — the per-message body highlighting the
/// bubbles render with an accent fill behind the matching runs. Comparison is
/// per-char `to_lowercase` (the [`title_hits`] approach), so multi-byte text
/// never splits a character. A blank term yields one plain span.
pub fn highlight_spans(text: &str, term: &str) -> Vec<(String, bool)> {
    let t: Vec<char> = term.trim().chars().flat_map(char::to_lowercase).collect();
    if text.is_empty() {
        return Vec::new();
    }
    if t.is_empty() {
        return vec![(text.to_string(), false)];
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut plain_start = 0;
    let mut i = 0;
    while i < chars.len() {
        match match_at(&chars, i, &t, text.len()) {
            Some(end) => {
                let start = chars[i].0;
                if start > plain_start {
                    out.push((text[plain_start..start].to_string(), false));
                }
                out.push((text[start..end].to_string(), true));
                plain_start = end;
                // Resume after the match — occurrences never overlap.
                while i < chars.len() && chars[i].0 < end {
                    i += 1;
                }
            }
            None => i += 1,
        }
    }
    if plain_start < text.len() {
        out.push((text[plain_start..].to_string(), false));
    }
    out
}

/// If `term` matches `chars[i..]` case-insensitively, the byte offset just
/// past the matched text; `None` otherwise.
fn match_at(chars: &[(usize, char)], i: usize, term: &[char], text_len: usize) -> Option<usize> {
    let mut ti = 0;
    let mut j = i;
    while ti < term.len() {
        let &(_, c) = chars.get(j)?;
        for lc in c.to_lowercase() {
            if ti >= term.len() || lc != term[ti] {
                return None;
            }
            ti += 1;
        }
        j += 1;
    }
    Some(chars.get(j).map(|&(b, _)| b).unwrap_or(text_len))
}

/// Combine title matches and FTS body hits into the final result list: title
/// matches first (spec §"Title search too"), then body hits for conversations
/// not already listed, truncated to `limit`.
pub fn merge_hits(
    title: Vec<SearchResult>,
    body: Vec<SearchResult>,
    limit: usize,
) -> Vec<SearchResult> {
    let mut out = title;
    for hit in body {
        if !out.iter().any(|t| t.conversation_id == hit.conversation_id) {
            out.push(hit);
        }
    }
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: &str, snippet: &str) -> SearchResult {
        SearchResult {
            conversation_id: id.to_string(),
            snippet: snippet.to_string(),
        }
    }

    #[test]
    fn open_resets_state() {
        let mut s = SearchState::new();
        s.query = "stale".into();
        s.focused_idx = 4;
        s.open();
        assert!(s.open);
        assert_eq!(s.query, "");
        assert_eq!(s.focused_idx, 0);
        s.close();
        assert!(!s.open);
    }

    #[test]
    fn set_query_clamps_idx() {
        let mut s = SearchState::new();
        s.focused_idx = 5;
        s.set_query("ne", 2);
        assert_eq!(s.query, "ne");
        assert_eq!(s.focused_idx, 1, "clamped to the last valid row");
        s.set_query("new", 0);
        assert_eq!(s.focused_idx, 0, "no results clamps to zero");
    }

    #[test]
    fn move_down_wraps() {
        let mut s = SearchState::new();
        s.focused_idx = 2;
        s.move_down(3);
        assert_eq!(s.focused_idx, 0, "last row wraps to first");
        s.move_down(3);
        assert_eq!(s.focused_idx, 1);
    }

    #[test]
    fn move_up_wraps() {
        let mut s = SearchState::new();
        s.move_up(3);
        assert_eq!(s.focused_idx, 2, "first row wraps to last");
        s.move_up(3);
        assert_eq!(s.focused_idx, 1);
    }

    #[test]
    fn move_on_empty_results_is_noop() {
        let mut s = SearchState::new();
        s.move_down(0);
        s.move_up(0);
        assert_eq!(
            s.focused_idx, 0,
            "no rows: focus stays put, no division by zero"
        );
    }

    #[test]
    fn focused_id_on_empty_results() {
        let s = SearchState::new();
        assert_eq!(s.focused_id(&[]), None);
    }

    #[test]
    fn focused_id_returns_the_focused_row() {
        let mut s = SearchState::new();
        let results = [hit("a", ""), hit("b", "")];
        assert_eq!(s.focused_id(&results), Some("a"));
        s.move_down(results.len());
        assert_eq!(s.focused_id(&results), Some("b"));
    }

    #[test]
    fn snippet_runs_alternate_bold() {
        assert_eq!(
            snippet_runs("…the **quick** brown **fox**"),
            vec![
                ("…the ".to_string(), false),
                ("quick".to_string(), true),
                (" brown ".to_string(), false),
                ("fox".to_string(), true),
            ]
        );
    }

    #[test]
    fn snippet_runs_handle_plain_and_unbalanced_text() {
        assert_eq!(
            snippet_runs("no markers"),
            vec![("no markers".to_string(), false)]
        );
        // An unbalanced marker bolds the tail rather than dropping it.
        assert_eq!(
            snippet_runs("odd **tail"),
            vec![("odd ".to_string(), false), ("tail".to_string(), true)]
        );
        assert!(snippet_runs("").is_empty());
    }

    #[test]
    fn title_hits_match_case_insensitively_and_bold_the_span() {
        let convs = vec![
            ("c1".to_string(), "Garden Plan".to_string()),
            ("c2".to_string(), "Taxes".to_string()),
        ];
        let hits = title_hits("garden", &convs);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conversation_id, "c1");
        assert_eq!(hits[0].snippet, "**Garden** Plan");
    }

    /// A match in the MIDDLE of the title keeps the un-bolded prefix and suffix
    /// intact — the snippet is `prefix **match** suffix`, not just the bolded
    /// span. The other title-hit test bolds a start-anchored match (empty
    /// prefix), so the `&title[..start]` prefix slice is only pinned here.
    #[test]
    fn title_hits_bold_a_mid_title_match_keeping_prefix() {
        let convs = vec![("c1".to_string(), "My Garden Plan".to_string())];
        let hits = title_hits("garden", &convs);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conversation_id, "c1");
        assert_eq!(hits[0].snippet, "My **Garden** Plan");
    }

    #[test]
    fn title_hits_keep_input_order_and_ignore_blank_queries() {
        let convs = vec![
            ("newer".to_string(), "plan A".to_string()),
            ("older".to_string(), "plan B".to_string()),
        ];
        let hits = title_hits("plan", &convs);
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].conversation_id, "newer",
            "caller's recency order is preserved"
        );
        assert!(title_hits("  ", &convs).is_empty());
    }

    #[test]
    fn merge_hits_puts_title_matches_first_and_dedupes() {
        let title = vec![hit("t1", "**Plan**")];
        let body = vec![
            hit("t1", "body snippet for the same conv"),
            hit("b1", "…a **plan**…"),
        ];
        let merged = merge_hits(title, body, 10);
        assert_eq!(merged.len(), 2, "t1 appears once");
        assert_eq!(merged[0].conversation_id, "t1");
        assert_eq!(
            merged[0].snippet, "**Plan**",
            "the title hit wins for a dual match"
        );
        assert_eq!(merged[1].conversation_id, "b1");
    }

    #[test]
    fn merge_hits_respects_limit() {
        let title = vec![hit("t1", ""), hit("t2", "")];
        let body = vec![hit("b1", ""), hit("b2", "")];
        assert_eq!(merge_hits(title, body, 3).len(), 3);
    }

    // ---- slice 16 §3: per-message highlight ------------------------------------

    /// Selecting a search result sets the highlight term to the query.
    #[test]
    fn highlight_term_set_on_select() {
        let mut s = SearchState::new();
        s.open();
        s.set_query("garden plan", 1);
        let term = next_highlight(HighlightEvent::Select(&s.query));
        assert_eq!(term.as_deref(), Some("garden plan"));
        // A padded query highlights its trimmed text; a blank one nothing.
        assert_eq!(
            next_highlight(HighlightEvent::Select("  fox ")).as_deref(),
            Some("fox")
        );
        assert_eq!(next_highlight(HighlightEvent::Select("   ")), None);
    }

    /// Escape in (or after) the search panel clears the highlight.
    #[test]
    fn highlight_term_cleared_on_escape() {
        let mut s = SearchState::new();
        s.open();
        s.set_query("fox", 1);
        let mut term = next_highlight(HighlightEvent::Select(&s.query));
        assert!(term.is_some());
        // Escape: the panel closes and the highlight clears.
        s.close();
        term = next_highlight(HighlightEvent::Clear);
        assert_eq!(term, None);
        assert!(!s.open);
    }

    /// The spec's worked example: "Hello world" highlighted for "world" is
    /// exactly two spans — "Hello " plain, "world" highlighted.
    #[test]
    fn highlight_spans_split_message_text() {
        assert_eq!(
            highlight_spans("Hello world", "world"),
            vec![("Hello ".to_string(), false), ("world".to_string(), true)]
        );
    }

    /// Matching is case-insensitive, finds every occurrence, and a term that
    /// never occurs leaves one plain span.
    #[test]
    fn highlight_spans_case_insensitive_and_repeated() {
        assert_eq!(
            highlight_spans("Fox, fox, FOX.", "fox"),
            vec![
                ("Fox".to_string(), true),
                (", ".to_string(), false),
                ("fox".to_string(), true),
                (", ".to_string(), false),
                ("FOX".to_string(), true),
                (".".to_string(), false),
            ]
        );
        assert_eq!(
            highlight_spans("no match here", "zebra"),
            vec![("no match here".to_string(), false)]
        );
        assert_eq!(
            highlight_spans("anything", ""),
            vec![("anything".to_string(), false)]
        );
        assert!(highlight_spans("", "fox").is_empty());
    }
}
