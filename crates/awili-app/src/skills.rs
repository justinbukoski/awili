//! Skills + toolsets panel — read-only listing of installed capabilities.
//!
//! Fetches from `GET /api/skills` and `GET /api/tools/toolsets` on the Hermes
//! REST surface. The panel is a filterable list grouped by category.
//!
//! This module also owns the composer's `/`-command **skill palette** state
//! (the inline picker that opens when a draft starts with `/`). The palette is
//! the same shape as the ⌘K command palette ([`crate::palette`]) but over the
//! installed skill list rather than conversations/built-ins: a pure
//! open/focused-row state machine plus a trigger parser, kept free of GPUI so
//! the invariants (when it opens, wrap-around navigation, focus reset on query
//! change) are unit-testable without a window.

use awili_gateway::rest::{RestClient, Skill, Toolset};

use crate::palette::fuzzy_match;

/// The skills panel state.
#[derive(Clone, Debug)]
pub struct SkillsState {
    pub skills: Vec<Skill>,
    pub toolsets: Vec<Toolset>,
    pub filter: String,
    pub loading: bool,
    pub error: Option<String>,
    /// The expanded skill card (H2): its name, or `None` when all collapsed.
    pub selected: Option<String>,
    /// A toggle round-trip is in flight; the detail's button is disabled.
    pub toggling: bool,
}

impl SkillsState {
    pub fn new() -> Self {
        Self {
            selected: None,
            toggling: false,
            skills: Vec::new(),
            toolsets: Vec::new(),
            filter: String::new(),
            loading: false,
            error: None,
        }
    }

    /// Filtered skills matching the current query.
    pub fn filtered_skills(&self) -> Vec<&Skill> {
        if self.filter.is_empty() {
            return self.skills.iter().collect();
        }
        let q = self.filter.to_lowercase();
        self.skills
            .iter()
            .filter(|s| {
                s.name.to_lowercase().contains(&q)
                    || s.description
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&q)
            })
            .collect()
    }

    /// Filtered toolsets matching the current query.
    pub fn filtered_toolsets(&self) -> Vec<&Toolset> {
        if self.filter.is_empty() {
            return self.toolsets.iter().collect();
        }
        let q = self.filter.to_lowercase();
        self.toolsets
            .iter()
            .filter(|t| {
                t.name.to_lowercase().contains(&q)
                    || t.description
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&q)
            })
            .collect()
    }

    /// Load skills and toolsets from the gateway REST surface.
    #[allow(dead_code)] // wired by the skills panel (Phase 5)
    pub fn fetch(&mut self, client: &RestClient) {
        self.loading = true;
        self.error = None;
        match client.list_skills() {
            Ok(skills) => self.skills = skills,
            Err(e) => {
                self.error = Some(format!("Skills: {e}"));
                self.loading = false;
                return;
            }
        }
        match client.list_toolsets() {
            Ok(toolsets) => self.toolsets = toolsets,
            Err(e) => self.error = Some(format!("Toolsets: {e}")),
        }
        self.loading = false;
    }
}

/// The `/`-command skill palette's pure view-state — open flag, the live
/// query (mirrored from the draft, after the leading `/`), and which row is
/// focused within [`matching_skills`]'s list. Mirrors
/// [`crate::palette::PaletteState`]; the *data* stays in [`SkillsState`].
#[derive(Clone, Debug, Default)]
pub struct SkillPaletteState {
    /// The inline picker is showing.
    pub open: bool,
    /// Filter text — the draft with its leading `/` stripped.
    pub query: String,
    /// Index of the focused row within [`matching_skills`]'s list.
    pub focused_idx: usize,
}

impl SkillPaletteState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the query. A changed query rebuilds the row list, so focus snaps
    /// back to the top — which also keeps `focused_idx` in range as the list
    /// narrows (the [`crate::palette::PaletteState::set_query`] invariant).
    pub fn set_query(&mut self, q: &str) {
        if self.query != q {
            self.query = q.to_string();
            self.focused_idx = 0;
        }
    }

    /// Close and clear: a fresh open should never inherit stale focus/query.
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.focused_idx = 0;
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
}

/// The skill-palette query a composer draft triggers, or `None` when the draft
/// is not a `/`-command. The palette opens only when the WHOLE draft is a
/// single bare `/name` token: it must start with `/` and carry no whitespace
/// after the slash — the first space ends skill-name entry (the user is now
/// typing arguments) and closes the palette, exactly like Claude/Cowork's
/// `/`-commands. Returns the text after the slash (empty right after `/`, which
/// lists every skill).
pub fn skill_query(draft: &str) -> Option<&str> {
    let rest = draft.strip_prefix('/')?;
    if rest.chars().any(char::is_whitespace) {
        return None;
    }
    Some(rest)
}

/// Skills matching `query`, in install order — the palette's rows. An empty
/// query lists every skill. The NAME is matched with the ⌘K palette's
/// subsequence matcher (so `/ws` finds `web-search`), but the DESCRIPTION only
/// by case-insensitive substring: subsequence-matching long descriptions
/// surfaces near-everything with no visible highlight, which reads as noise in
/// a `/`-command list. Substring keeps description matches meaningful.
pub fn matching_skills<'a>(skills: &'a [Skill], query: &str) -> Vec<&'a Skill> {
    let needle = query.to_lowercase();
    skills
        .iter()
        .filter(|s| {
            fuzzy_match(&s.name, query)
                || s.description
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&needle)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_skills() -> Vec<Skill> {
        vec![
            Skill {
                name: "web-search".to_string(),
                category: Some("research".to_string()),
                description: Some("Search the web".to_string()),
                enabled: true,
                version: Some("1.0".to_string()),
            },
            Skill {
                name: "file-reader".to_string(),
                category: None,
                description: Some("Read local files".to_string()),
                enabled: false,
                version: None,
            },
        ]
    }

    fn sample_toolsets() -> Vec<Toolset> {
        vec![Toolset {
            name: "default".to_string(),
            label: Some("Default".to_string()),
            description: Some("Built-in tools".to_string()),
            enabled: true,
            configured: true,
            tools: vec!["bash".to_string(), "read".to_string()],
        }]
    }

    #[test]
    fn filter_skills_by_name() {
        let mut state = SkillsState::new();
        state.skills = sample_skills();
        state.filter = "web".to_string();
        let filtered = state.filtered_skills();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "web-search");
    }

    #[test]
    fn filter_skills_by_description() {
        let mut state = SkillsState::new();
        state.skills = sample_skills();
        state.filter = "local".to_string();
        let filtered = state.filtered_skills();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "file-reader");
    }

    #[test]
    fn empty_filter_returns_all() {
        let mut state = SkillsState::new();
        state.skills = sample_skills();
        state.toolsets = sample_toolsets();
        assert_eq!(state.filtered_skills().len(), 2);
        assert_eq!(state.filtered_toolsets().len(), 1);
    }

    #[test]
    fn case_insensitive_filter() {
        let mut state = SkillsState::new();
        state.skills = sample_skills();
        state.filter = "WEB".to_string();
        assert_eq!(state.filtered_skills().len(), 1);
    }

    // ---- `/`-command trigger parser ------------------------------------------

    #[test]
    fn slash_alone_opens_with_empty_query() {
        assert_eq!(skill_query("/"), Some(""));
    }

    #[test]
    fn slash_name_yields_the_name_as_query() {
        assert_eq!(skill_query("/web"), Some("web"));
        assert_eq!(skill_query("/file-reader"), Some("file-reader"));
    }

    #[test]
    fn draft_not_starting_with_slash_does_not_trigger() {
        assert_eq!(skill_query(""), None);
        assert_eq!(skill_query("hello"), None);
        assert_eq!(
            skill_query(" /web"),
            None,
            "a leading space is not a command"
        );
    }

    #[test]
    fn a_space_after_the_name_closes_the_palette() {
        // The first space ends skill-name entry — the user is typing args now.
        assert_eq!(skill_query("/web "), None);
        assert_eq!(skill_query("/web search foo"), None);
        assert_eq!(skill_query("/web\n"), None, "a newline also closes it");
    }

    // ---- matching_skills (fuzzy name, substring description) -----------------

    #[test]
    fn empty_query_lists_every_skill_in_order() {
        let skills = sample_skills();
        let rows = matching_skills(&skills, "");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "web-search");
        assert_eq!(rows[1].name, "file-reader");
    }

    #[test]
    fn matching_filters_by_name_and_description() {
        let skills = sample_skills();
        assert_eq!(matching_skills(&skills, "web")[0].name, "web-search");
        // "local" only appears in file-reader's description ("Read local files").
        let by_desc = matching_skills(&skills, "local");
        assert_eq!(by_desc.len(), 1);
        assert_eq!(by_desc[0].name, "file-reader");
    }

    #[test]
    fn matching_is_subsequence_and_case_insensitive() {
        let skills = sample_skills();
        // "ws" is a non-adjacent subsequence of "web-search" (and "WS" too).
        assert_eq!(matching_skills(&skills, "ws")[0].name, "web-search");
        assert_eq!(matching_skills(&skills, "WS")[0].name, "web-search");
        assert!(matching_skills(&skills, "zzz").is_empty());
    }

    #[test]
    fn description_matches_by_substring_not_subsequence() {
        // "rdf" is a subsequence of "Read local files" (R..d..f) but NOT a
        // substring — the old subsequence-on-description rule matched it and
        // produced noisy, unhighlighted rows. Substring matching excludes it.
        let skills = sample_skills();
        assert!(matching_skills(&skills, "rdf").is_empty());
        // A genuine substring of the description still matches.
        assert_eq!(
            matching_skills(&skills, "local files")[0].name,
            "file-reader"
        );
    }

    // ---- SkillPaletteState machine -------------------------------------------

    #[test]
    fn set_query_resets_focus_only_on_change() {
        let mut s = SkillPaletteState::new();
        s.focused_idx = 2;
        s.set_query("web"); // changed → reset
        assert_eq!(s.focused_idx, 0);
        s.focused_idx = 1;
        s.set_query("web"); // unchanged → focus kept
        assert_eq!(s.focused_idx, 1);
    }

    #[test]
    fn navigation_wraps_both_ways() {
        let mut s = SkillPaletteState::new();
        s.move_down(3);
        assert_eq!(s.focused_idx, 1);
        s.move_up(3);
        assert_eq!(s.focused_idx, 0);
        s.move_up(3); // wrap to the bottom
        assert_eq!(s.focused_idx, 2);
        s.move_down(3); // wrap back to the top
        assert_eq!(s.focused_idx, 0);
    }

    #[test]
    fn navigation_is_safe_on_an_empty_list() {
        let mut s = SkillPaletteState::new();
        s.focused_idx = 0;
        s.move_down(0);
        s.move_up(0);
        assert_eq!(s.focused_idx, 0);
    }

    #[test]
    fn close_clears_query_and_focus() {
        let mut s = SkillPaletteState::new();
        s.open = true;
        s.set_query("web");
        s.focused_idx = 1;
        s.close();
        assert!(!s.open);
        assert_eq!(s.query, "");
        assert_eq!(s.focused_idx, 0);
    }
}
