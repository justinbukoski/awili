//! The sidebar's pure state machine (slice-7 spec §2/§4): which conversation is
//! selected, whether the panel / archived section are open, and the inline
//! rename lifecycle. No GPUI here — selection and rename commit/cancel are the
//! invariants the spec asks to unit-test, so they live where a test can drive
//! them without a window.

use std::collections::HashMap;
use std::path::Path;

use awili_core::{ConvKind, ConversationList, ConversationMeta};

/// Sidebar UI state. The conversation *data* stays in [`ConversationList`];
/// this is only the view-state layered on top.
#[derive(Clone, Debug)]
pub struct SidebarState {
    /// The whole panel collapsed to a slim rail.
    pub collapsed: bool,
    /// The "Archived" section expanded.
    pub archived_open: bool,
    /// Id of the conversation the main view shows. Private (RC1-I2-FIX3):
    /// read it with [`SidebarState::active_id`]; only
    /// [`crate::AwiliApp::set_active_conversation`] changes it.
    active_id: String,
    /// Id of the row being renamed inline, if any.
    pub renaming: Option<String>,
    /// Hover-owned actions change at render, never between prepaint and paint.
    pub hovered_id: Option<String>,
    /// Last Chat conversation that was active — restored when re-entering Chat
    /// mode so Chat and Work each keep their own place.
    pub last_chat_id: Option<String>,
    /// Last Work conversation that was active — restored when re-entering Work
    /// mode.
    pub last_work_id: Option<String>,
    /// RC2-CNCL5: last Council that was active — restored when re-entering
    /// the Council page.
    pub last_council_id: Option<String>,
}

impl SidebarState {
    pub fn new(active_id: String) -> Self {
        Self {
            collapsed: false,
            archived_open: false,
            last_chat_id: Some(active_id.clone()),
            last_work_id: None,
            last_council_id: None,
            active_id,
            renaming: None,
            hovered_id: None,
        }
    }

    /// The id of the conversation the main view shows.
    pub fn active_id(&self) -> &str {
        &self.active_id
    }

    /// Switch the active conversation. Selecting a row abandons any rename in
    /// progress (the editor disappears with the row it sat in). Returns whether
    /// the selection actually changed. Private (RC1-I2-FIX3): the app changes
    /// the active conversation only through
    /// [`crate::AwiliApp::set_active_conversation`].
    fn select(&mut self, id: &str) -> bool {
        self.renaming = None;
        if self.active_id == id {
            return false;
        }
        self.active_id = id.to_string();
        true
    }

    pub fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
    }

    pub fn toggle_archived(&mut self) {
        self.archived_open = !self.archived_open;
    }

    /// Start renaming a row (double-click). Replaces any rename in progress.
    pub fn begin_rename(&mut self, id: &str) {
        self.renaming = Some(id.to_string());
    }

    /// Abandon the rename (Escape) — nothing changes.
    pub fn cancel_rename(&mut self) {
        self.renaming = None;
    }

    /// Commit the inline rename with the editor's text. A blank title is a
    /// cancel, not a commit (the list rejects it and the old title stands).
    /// Returns the renamed `(id, trimmed_title)` when the rename applied, so
    /// the caller can persist it. The rename state is cleared either way.
    pub fn commit_rename(
        &mut self,
        list: &mut ConversationList,
        title: &str,
    ) -> Option<(String, String)> {
        let id = self.renaming.take()?;
        if list.rename(&id, title) {
            let committed = list.get(&id)?.title.clone();
            Some((id, committed))
        } else {
            None
        }
    }
}

/// RC1-I2-FIX3: the ONE way to change the conversation the main view shows.
/// The sidebar's `active_id` and `select` are private to this module, so the
/// compiler holds every path — sidebar, history, mode switch, new
/// conversation, Code sidebar, Code history, Code tab — to this setter, and
/// what must happen when the active conversation changes hangs off it.
impl crate::AwiliApp {
    /// Make `id` the active conversation; returns whether it changed. Leaving
    /// the in-app worker's conversation ends its session here (an idle worker
    /// is retired at once; a running run is never stopped — it finishes, and
    /// its worker is retired then).
    pub(crate) fn set_active_conversation(&mut self, id: &str) -> bool {
        let changed = self.sidebar.select(id);
        if changed {
            self.cancel_composer_navigation();
        }
        self.leave_worker_conversation();
        // RC2-CNCL5: a council opens with its members seated.
        self.ensure_council_page(id);
        changed
    }
}

/// After archiving `archived_id`, the conversation the view should fall back to
/// when the archived one was active: the most recently updated remaining active
/// conversation in the same page family. Council stays separate from the
/// Workshop's Chat, Work and Code history. `None` creates one on the current page.
pub fn next_active_after_archive(
    list: &ConversationList,
    archived_id: &str,
    kind: ConvKind,
) -> Option<String> {
    list.active()
        .iter()
        .filter(|c| (c.kind == ConvKind::Council) == (kind == ConvKind::Council))
        .map(|c| c.id.clone())
        .find(|id| id != archived_id)
}

// ============================================================================
// Workspace groups — group conversations by cwd for the sidebar
// ============================================================================

/// A group of conversations sharing the same workspace (cwd).
#[derive(Clone, Debug)]
pub struct WorkspaceGroup {
    /// Display label (disambiguated directory name).
    pub label: String,
    /// Full path.
    pub path: String,
    /// Conversation IDs in this group, sorted by updated_ts descending.
    pub session_ids: Vec<String>,
}

/// Group active conversations by their `cwd` field. Conversations without a cwd
/// go into a "General" group. Groups are sorted by most-recent activity.
/// Labels are disambiguated when multiple groups share the same directory name.
///
/// When `profile` is `Some`, only conversations matching that profile (or with
/// no profile set) are included. When `show_all` is true, all profiles show.
#[allow(dead_code)] // the profile-scoped sidebar ships in Phase 4
pub fn workspace_groups(list: &ConversationList) -> Vec<WorkspaceGroup> {
    workspace_groups_scoped(list, None, true)
}

pub fn workspace_groups_scoped(
    list: &ConversationList,
    profile: Option<&str>,
    show_all: bool,
) -> Vec<WorkspaceGroup> {
    let all = list.active();
    let convs: Vec<&ConversationMeta> = if show_all {
        all
    } else if let Some(p) = profile {
        all.into_iter()
            .filter(|c| c.profile.as_deref() == Some(p) || c.profile.is_none())
            .collect()
    } else {
        all
    };

    workspace_groups_from(&convs, list)
}

/// R1f: the test lookup key of the active (unarchived) sidebar row.
pub(crate) const ACTIVE_ROW_SELECTOR: &str = "r1f-active-row";
/// R1f: the test lookup key prefix of a workspace group heading (the label
/// follows it).
pub(crate) const GROUP_SELECTOR: &str = "r1f-group-";

/// R1f: the sidebar's workspace sections for one conversation kind. Only
/// groups holding at least one conversation of `kind` are returned (a group
/// heading is never drawn with no rows under it), each group lists only its
/// `kind` conversations (newest first), and the groups are ordered by their
/// newest conversation of `kind` — so the conversation that just ran is at the
/// top of the list. "General" is not pinned first here; on a tie the
/// [`workspace_groups_scoped`] order stands.
pub fn kind_sections(
    list: &ConversationList,
    profile: Option<&str>,
    show_all: bool,
    kind: ConvKind,
) -> Vec<WorkspaceGroup> {
    let mut groups = workspace_groups_scoped(list, profile, show_all);
    for group in &mut groups {
        group
            .session_ids
            .retain(|id| list.get(id).is_some_and(|c| c.kind == kind));
    }
    groups.retain(|group| !group.session_ids.is_empty());
    let newest = |group: &WorkspaceGroup| {
        group
            .session_ids
            .first()
            .and_then(|id| list.get(id))
            .map_or(0, |c| c.updated_ts)
    };
    groups.sort_by_key(|group| std::cmp::Reverse(newest(group)));
    groups
}

/// Workshop history spans the three legacy conversation kinds; Council keeps
/// its own page. Profile and workspace ordering follow the existing browser.
pub(crate) fn workshop_sections(
    list: &ConversationList,
    profile: Option<&str>,
    show_all: bool,
    active: &str,
) -> Vec<WorkspaceGroup> {
    let mut groups = workspace_groups_scoped(list, profile, show_all);
    for group in &mut groups {
        group
            .session_ids
            .retain(|id| list.get(id).is_some_and(|c| c.kind != ConvKind::Council));
    }
    groups.retain(|group| !group.session_ids.is_empty());
    groups.sort_by_key(|group| {
        std::cmp::Reverse((
            group
                .session_ids
                .first()
                .and_then(|id| list.get(id))
                .map_or(0, |c| c.updated_ts),
            group.session_ids.iter().any(|id| id == active),
        ))
    });
    groups
}

fn workspace_groups_from(
    convs: &[&ConversationMeta],
    list: &ConversationList,
) -> Vec<WorkspaceGroup> {
    let mut by_path: HashMap<String, Vec<&ConversationMeta>> = HashMap::new();

    for &c in convs {
        let key = c.cwd.clone().unwrap_or_default();
        by_path.entry(key).or_default().push(c);
    }

    // Sort each group's sessions by updated_ts descending.
    for sessions in by_path.values_mut() {
        sessions.sort_by_key(|b| std::cmp::Reverse(b.updated_ts));
    }

    let mut groups: Vec<WorkspaceGroup> = by_path
        .into_iter()
        .map(|(path, sessions)| {
            let label = if path.is_empty() {
                "General".to_string()
            } else {
                Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.clone())
            };
            let session_ids = sessions.iter().map(|c| c.id.clone()).collect();
            WorkspaceGroup {
                label,
                path,
                session_ids,
            }
        })
        .collect();

    // Disambiguate labels that collide.
    disambiguate_labels(&mut groups);

    // Sort groups: "General" first, then by most-recent session activity.
    groups.sort_by(|a, b| {
        let a_gen = a.path.is_empty();
        let b_gen = b.path.is_empty();
        b_gen.cmp(&a_gen).then_with(|| {
            let a_ts = list
                .get(a.session_ids.first().unwrap_or(&String::new()))
                .map(|c| c.updated_ts)
                .unwrap_or(0);
            let b_ts = list
                .get(b.session_ids.first().unwrap_or(&String::new()))
                .map(|c| c.updated_ts)
                .unwrap_or(0);
            b_ts.cmp(&a_ts)
        })
    });

    groups
}

/// Walk up path segments to disambiguate colliding labels.
fn disambiguate_labels(groups: &mut [WorkspaceGroup]) {
    let mut seen: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, g) in groups.iter().enumerate() {
        seen.entry(g.label.clone()).or_default().push(i);
    }

    for indices in seen.values() {
        if indices.len() <= 1 {
            continue;
        }
        // Grow labels by adding parent segments until unique.
        let mut depth = 1;
        loop {
            depth += 1;
            for &i in indices {
                let path = &groups[i].path;
                let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
                let start = components.len().saturating_sub(depth);
                groups[i].label = components[start..].join("/");
            }
            // Check if all labels are now unique.
            let labels: Vec<&str> = indices.iter().map(|&i| groups[i].label.as_str()).collect();
            let unique: std::collections::HashSet<&str> = labels.iter().copied().collect();
            if unique.len() == labels.len() || depth > 10 {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list_with(n: usize) -> (ConversationList, Vec<String>) {
        let mut list = ConversationList::new();
        let ids = (0..n)
            .map(|i| list.create((i as i64 + 1) * 10).id)
            .collect();
        (list, ids)
    }

    // ---- selection --------------------------------------------------------

    #[test]
    fn select_changes_active_and_reports_it() {
        let (_, ids) = list_with(2);
        let mut s = SidebarState::new(ids[0].clone());
        assert!(s.select(&ids[1]));
        assert_eq!(s.active_id, ids[1]);
        // Re-selecting the same row is a no-op.
        assert!(!s.select(&ids[1]));
    }

    #[test]
    fn select_abandons_an_in_progress_rename() {
        let (_, ids) = list_with(2);
        let mut s = SidebarState::new(ids[0].clone());
        s.begin_rename(&ids[0]);
        s.select(&ids[1]);
        assert!(
            s.renaming.is_none(),
            "switching rows must not leave a stale editor"
        );
    }

    // ---- rename commit / cancel --------------------------------------------

    #[test]
    fn commit_rename_applies_trimmed_title_and_clears_state() {
        let (mut list, ids) = list_with(1);
        let mut s = SidebarState::new(ids[0].clone());
        s.begin_rename(&ids[0]);
        let committed = s.commit_rename(&mut list, "  Garden plan  ");
        assert_eq!(committed, Some((ids[0].clone(), "Garden plan".to_string())));
        assert!(s.renaming.is_none());
        let c = list.get(&ids[0]).unwrap();
        assert_eq!(c.title, "Garden plan");
        assert!(c.title_is_custom, "an inline rename is a manual rename");
    }

    #[test]
    fn blank_commit_is_a_cancel() {
        let (mut list, ids) = list_with(1);
        let before = list.get(&ids[0]).unwrap().title.clone();
        let mut s = SidebarState::new(ids[0].clone());
        s.begin_rename(&ids[0]);
        assert_eq!(s.commit_rename(&mut list, "   "), None);
        assert!(s.renaming.is_none());
        assert_eq!(
            list.get(&ids[0]).unwrap().title,
            before,
            "blank must not erase the title"
        );
    }

    #[test]
    fn cancel_rename_leaves_the_title_untouched() {
        let (mut list, ids) = list_with(1);
        let mut s = SidebarState::new(ids[0].clone());
        s.begin_rename(&ids[0]);
        s.cancel_rename();
        assert!(s.renaming.is_none());
        assert_eq!(
            s.commit_rename(&mut list, "late"),
            None,
            "no rename in progress anymore"
        );
        assert_eq!(list.get(&ids[0]).unwrap().title, awili_core::DEFAULT_TITLE);
    }

    #[test]
    fn commit_without_begin_is_a_noop() {
        let (mut list, ids) = list_with(1);
        let mut s = SidebarState::new(ids[0].clone());
        assert_eq!(s.commit_rename(&mut list, "title"), None);
    }

    // ---- archive fallback ---------------------------------------------------

    #[test]
    fn archive_fallback_picks_most_recent_remaining() {
        let (mut list, ids) = list_with(3);
        list.touch(&ids[0], 99); // oldest row, newest activity
        list.archive(&ids[2]);
        assert_eq!(
            next_active_after_archive(&list, &ids[2], ConvKind::Chat),
            Some(ids[0].clone())
        );
    }

    #[test]
    fn archive_fallback_is_none_when_everything_is_archived() {
        let (mut list, ids) = list_with(1);
        list.archive(&ids[0]);
        assert_eq!(
            next_active_after_archive(&list, &ids[0], ConvKind::Chat),
            None
        );
    }

    #[test]
    fn archive_fallback_keeps_council_separate_from_shared_workshop_history() {
        let mut list = ConversationList::new();
        let chat = list.create_kind(10, ConvKind::Chat).id;
        let work = list.create_kind(20, ConvKind::Work).id;
        let council = list.create_kind(30, ConvKind::Council).id;
        list.archive(&council);
        assert_eq!(
            next_active_after_archive(&list, &council, ConvKind::Council),
            None
        );
        list.unarchive(&council);
        list.archive(&work);
        assert_eq!(
            next_active_after_archive(&list, &work, ConvKind::Work),
            Some(chat)
        );
    }

    // ---- workspace groups ---------------------------------------------------

    fn list_with_cwds(cwds: &[Option<&str>]) -> ConversationList {
        let mut list = ConversationList::new();
        for (i, cwd) in cwds.iter().enumerate() {
            let meta = list.create((i as i64 + 1) * 10);
            if let Some(path) = cwd {
                // Update the cwd on the conversation in the list.
                if let Some(c) = list.get_mut(&meta.id) {
                    c.cwd = Some(path.to_string());
                }
            }
        }
        list
    }

    #[test]
    fn groups_by_cwd() {
        let list = list_with_cwds(&[
            Some("/home/user/project-a"),
            Some("/home/user/project-b"),
            Some("/home/user/project-a"),
            None,
        ]);
        let groups = workspace_groups(&list);
        assert!(groups.iter().any(|g| g.label == "General"));
        assert!(groups.iter().any(|g| g.label == "project-a"));
        assert!(groups.iter().any(|g| g.label == "project-b"));
        let proj_a = groups.iter().find(|g| g.label == "project-a").unwrap();
        assert_eq!(proj_a.session_ids.len(), 2);
    }

    #[test]
    fn general_group_first() {
        let list = list_with_cwds(&[Some("/a"), None]);
        let groups = workspace_groups(&list);
        assert_eq!(groups[0].label, "General");
    }

    #[test]
    fn disambiguate_colliding_labels() {
        let list = list_with_cwds(&[
            Some("/a/hermes-agent/apps/desktop"),
            Some("/b/hermes-agent/apps/desktop"),
        ]);
        let groups = workspace_groups(&list);
        // Both would be "desktop" without disambiguation.
        assert_ne!(groups[0].label, groups[1].label);
        // Both should contain "desktop".
        for g in &groups {
            assert!(
                g.label.contains("desktop"),
                "label should contain 'desktop': {}",
                g.label
            );
        }
    }

    #[test]
    fn no_cwd_conversations_all_in_general() {
        let list = list_with_cwds(&[None, None, None]);
        let groups = workspace_groups(&list);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "General");
        assert_eq!(groups[0].session_ids.len(), 3);
    }

    // ---- profile scoping ---------------------------------------------------

    #[test]
    fn profile_scoping_filters_conversations() {
        let mut list = ConversationList::new();
        let a = list.create(10);
        let b = list.create(20);
        let c = list.create(30);
        list.get_mut(&a.id).unwrap().profile = Some("work".to_string());
        list.get_mut(&b.id).unwrap().profile = Some("personal".to_string());
        // c has no profile — should appear in any scoped view.

        let work_groups = workspace_groups_scoped(&list, Some("work"), false);
        let work_ids: Vec<_> = work_groups.iter().flat_map(|g| &g.session_ids).collect();
        assert!(work_ids.contains(&&a.id));
        assert!(!work_ids.contains(&&b.id));
        assert!(work_ids.contains(&&c.id)); // no-profile conv shows everywhere

        let all_groups = workspace_groups_scoped(&list, Some("work"), true);
        let all_ids: Vec<_> = all_groups.iter().flat_map(|g| &g.session_ids).collect();
        assert_eq!(all_ids.len(), 3);
    }

    // ---- R1f: kind sections ---------------------------------------------------

    #[test]
    fn r1f_kind_sections_newest_group_first_and_never_empty() {
        // General (older Work), b1 (newest Work), chat-only (Chat only).
        let mut list = list_with_cwds(&[None, Some("/w/b1"), Some("/c/chat-only")]);
        let ids: Vec<String> = list.all().iter().map(|c| c.id.clone()).collect();
        list.get_mut(&ids[0]).unwrap().kind = ConvKind::Work;
        list.get_mut(&ids[1]).unwrap().kind = ConvKind::Work;
        let work = kind_sections(&list, None, true, ConvKind::Work);
        let labels: Vec<&str> = work.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(
            labels,
            ["b1", "General"],
            "newest group first, no empty group"
        );
        assert_eq!(work[0].session_ids, [ids[1].clone()]);
        let chat = kind_sections(&list, None, true, ConvKind::Chat);
        assert_eq!(chat.len(), 1);
        assert_eq!(chat[0].label, "chat-only");
    }
}

/// Calendar labels follow the user's timezone, not UTC midnight or a fixed
/// 24-hour subtraction (local calendar days can span a daylight-saving change).
pub(crate) fn conversation_time_group<Tz: chrono::TimeZone>(
    updated_ts: i64,
    now: &chrono::DateTime<Tz>,
) -> &'static str {
    let Some(updated) = chrono::DateTime::from_timestamp(updated_ts, 0) else {
        return "earlier";
    };
    let date = updated.with_timezone(&now.timezone()).date_naive();
    let today = now.date_naive();
    if date >= today {
        "today"
    } else if Some(date) == today.pred_opt() {
        "yesterday"
    } else {
        "earlier"
    }
}

#[cfg(test)]
mod calendar_label_tests {
    use super::conversation_time_group;
    use chrono::DateTime;

    #[test]
    fn utc_midnight_does_not_move_same_local_day_to_yesterday() {
        let now = DateTime::parse_from_rfc3339("2026-09-12T17:05:00-07:00").unwrap();
        let updated = DateTime::parse_from_rfc3339("2026-09-12T16:02:04-07:00").unwrap();
        assert_eq!(conversation_time_group(updated.timestamp(), &now), "today");
    }

    #[test]
    fn local_midnight_and_older_dates_use_calendar_boundaries() {
        let now = DateTime::parse_from_rfc3339("2026-09-13T00:05:00+09:00").unwrap();
        for (value, expected) in [
            ("2026-09-13T00:00:00+09:00", "today"),
            ("2026-09-12T23:59:00+09:00", "yesterday"),
            ("2026-09-11T23:59:00+09:00", "earlier"),
        ] {
            let updated = DateTime::parse_from_rfc3339(value).unwrap();
            assert_eq!(conversation_time_group(updated.timestamp(), &now), expected);
        }
        assert_eq!(conversation_time_group(i64::MAX, &now), "earlier");
    }
}
