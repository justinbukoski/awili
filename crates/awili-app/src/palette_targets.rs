//! The ⌘K palette's PATH mode (M2i, F06): when the query starts with "/" or
//! "~", the palette lists FOLDERS instead of conversations — so the working
//! folder can be set by TYPING a path (with completion) rather than only
//! through the native Open panel. The row math is kept pure (`folder_rows`)
//! with the filesystem behind two closures, so the ordering, de-duplication,
//! hidden-directory and cap rules are unit-testable without touching the disk.

/// Cap on child-directory rows offered for the last path segment.
pub(crate) const CHILD_CAP: usize = 8;

/// Query is a path (starts with "/" or "~").
pub(crate) fn path_mode(query: &str) -> bool {
    query.starts_with('/') || query.starts_with('~')
}

/// The child DIRECTORY names of `dir` (bare names, no trailing slash), sorted.
/// Empty on error. The app passes this to `folder_rows`; tests pass a closure.
/// M2i-FIX1: NOT truncated here. `folder_rows` filters by the typed prefix and
/// the hidden rule FIRST and only then caps at `CHILD_CAP`, so a matching
/// folder past the 500th name is still offered.
pub(crate) fn list_dir_names(dir: &str) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                is_dir.then(|| e.file_name().to_string_lossy().to_string())
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// "~"-abbreviated display: "/Users/j/x" → "~/x"; home itself → "~".
/// A path outside home is shown as-is.
pub(crate) fn display_path(path: &str, home: &str) -> String {
    if home.is_empty() {
        return path.to_string();
    }
    match path.strip_prefix(home) {
        Some(rest) => {
            if rest.is_empty() {
                "~".to_string()
            } else if rest.starts_with('/') {
                format!("~{}", rest)
            } else {
                path.to_string()
            }
        }
        None => path.to_string(),
    }
}

fn expand(query: &str, home: &str) -> String {
    if let Some(rest) = query.strip_prefix("~/") {
        let base = if home.is_empty() {
            String::from("~")
        } else {
            home.to_string()
        };
        format!("{base}/{rest}")
    } else if query.starts_with('~') {
        home.to_string()
    } else {
        query.to_string()
    }
}

/// Pure. `list_dirs(dir)` returns the child DIRECTORY names of `dir` (the app
/// passes `list_dir_names`, uncapped; tests pass a closure).
/// `is_dir(path)` reports whether an absolute path is an existing directory.
/// `saved` = saved project folders (Code mode) or empty.
///
/// Returns absolute paths (trimmed of any trailing "/") in order:
/// 1. saved projects whose folder starts with the typed text (sorted);
/// 2. the typed path itself, when it is an existing directory and not already
///    listed (and only when no offered row is its parent — a parent row's
///    child scan would re-list it);
/// 3. up to `CHILD_CAP` child directories of the typed path (or of its parent
///    while the last segment is still being typed) matching the last segment
///    case-insensitively by prefix; hidden children only when the segment
///    itself starts with ".".
///    De-duplicated throughout.
pub(crate) fn folder_rows(
    query: &str,
    home: &str,
    saved: &[String],
    list_dirs: impl Fn(&str) -> Vec<String>,
    is_dir: impl Fn(&str) -> bool,
) -> Vec<String> {
    let full = expand(query, home).trim_end_matches('/').to_string();
    let mut out: Vec<String> = Vec::new();

    let parent = full
        .rsplit_once('/')
        .map(|(p, _)| p.to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_default();
    let last = full.rsplit('/').next().unwrap_or("").to_string();
    let hide = !last.starts_with('.');

    // 1. Saved project folders whose folder starts with the typed text, FIRST
    // (M2i spec; M2i-FIX1 restored this order), sorted.
    let mut saved_rows: Vec<String> = saved
        .iter()
        .filter(|s| s.starts_with(&full))
        .cloned()
        .collect();
    saved_rows.sort();
    for s in saved_rows {
        if !out.contains(&s) {
            out.push(s);
        }
    }

    // 2. The typed path itself, when it is an existing directory and not
    // already offered as a saved row.
    if is_dir(&full) && !out.contains(&full) {
        out.push(full.clone());
    }

    // 3. Up to CHILD_CAP child directories of the typed path.
    //
    // "Complete" mode (query ends with "/" or is bare "~"): the typed path IS
    // the directory whose children are being listed — scan its children
    // directly and offer all of them (no prefix filter).
    //
    // "Partial" mode (last segment still being typed): scan the PARENT
    // directory and let the case-insensitive prefix filter pick up matching
    // siblings; hidden siblings only when the segment starts with ".".
    //
    // In BOTH modes the resulting rows are `format!("{scan_dir}/{child}")`
    // (where `scan_dir` is the dir actually being scanned): in complete mode
    // the scan dir is the typed path, in partial mode it is the typed path's
    // parent.
    let complete = query.ends_with('/') || (query.starts_with('~') && query.len() == 1);
    // In partial mode `last` is the dir's own name; the user is typing a
    // partial segment that will be appended to it. The prefix filter must
    // match the *partial segment* (the suffix of the typed query), not the
    // dir's name.
    let partial_segment = if complete {
        String::new()
    } else {
        full.strip_prefix(&format!("{parent}/"))
            .unwrap_or(last.as_str())
            .to_string()
    };
    let scan_dir = if complete {
        full.clone()
    } else {
        parent.clone()
    };
    if !scan_dir.is_empty() {
        let children: Vec<String> = list_dirs(&scan_dir)
            .into_iter()
            .filter(|c| {
                // In complete mode (trailing "/" or bare "~") the typed
                // path is the scan dir: offer every non-hidden child (a
                // bare listing — the user typed the dir, not a "."-prefixed
                // segment, so hidden entries stay hidden).
                if complete {
                    return !c.starts_with('.');
                }
                // Partial mode: hide hidden siblings unless the typed
                // segment itself starts with "."; then prefix-match.
                if hide && c.starts_with('.') {
                    return false;
                }
                // The prefix filter matches the typed segment's prefix.
                // In partial mode `partial_segment` is the suffix of the
                // typed query (the partial segment).
                c.to_lowercase()
                    .starts_with(partial_segment.to_lowercase().as_str())
            })
            .collect();
        let mut children = children;
        children.sort();
        for child in children.into_iter().take(CHILD_CAP) {
            let path = format!("{scan_dir}/{child}");
            if is_dir(&path) && !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out
}

/// RC2-UX (R2-15): the `app_state` key of the folders chosen lately (a JSON
/// list of paths, newest first).
pub(crate) const RECENT_FOLDERS_KEY: &str = "recent_folders";

/// RC2-UX (R2-15): how many recent folders ⌘O remembers.
pub(crate) const RECENT_CAP: usize = 8;

/// RC2-UX (R2-15): put `path` first in `recent` (once), keeping at most
/// [`RECENT_CAP`].
pub(crate) fn remember_recent(recent: &mut Vec<String>, path: &str) {
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return;
    }
    recent.retain(|p| p != path);
    recent.insert(0, path.to_string());
    recent.truncate(RECENT_CAP);
}

/// RC2-UX (R2-15, round 2 Minor 8): the folders ⌘O offers before any typed
/// path — the recent ones (newest first), then the saved projects (by
/// name). On the ⌘O prefill ("~/", or a bare "~" or "/") every one is
/// listed, wherever it lives; as the owner types, one stays while its path
/// starts with the typed path or its folder name contains the last typed
/// part (so "~/f2" still finds a saved project under /Projects). Only existing
/// folders, once each.
pub(crate) fn known_folder_rows(
    query: &str,
    home: &str,
    recent: &[String],
    saved: &[String],
    is_dir: impl Fn(&str) -> bool,
) -> Vec<String> {
    let full = expand(query, home).trim_end_matches('/').to_string();
    let prefill = matches!(query, "~" | "~/" | "/");
    let part = if query.ends_with('/') {
        String::new()
    } else {
        query.rsplit('/').next().unwrap_or("").to_lowercase()
    };
    let name_of = |p: &str| p.rsplit('/').next().unwrap_or("").to_lowercase();
    let keep = |p: &str| {
        prefill
            || p.starts_with(&full)
            || (!part.is_empty() && !part.starts_with('~') && name_of(p).contains(&part))
    };
    let mut saved_sorted: Vec<&String> = saved.iter().collect();
    saved_sorted.sort_by_key(|p| name_of(p));
    let mut out: Vec<String> = Vec::new();
    for p in recent.iter().chain(saved_sorted) {
        let p = p.trim_end_matches('/');
        if !p.is_empty() && keep(p) && is_dir(p) && !out.iter().any(|o| o == p) {
            out.push(p.to_string());
        }
    }
    out
}

/// RC2-R2F1 (usability RC2 round 2, F1 MAJOR: a full path typed into ⌘O and
/// Enter opened ".../rc2-round1/k3", the first suggestion, not the typed
/// ".../rc2-round2/k3"): when the owner has typed a path to an existing
/// folder, that folder is the first row — the one Enter opens. A suggestion
/// runs only when the owner moves to it with the arrow keys or clicks it.
/// The untouched prefill ("~/", "~", "/") keeps the recent folders first.
pub(crate) fn typed_folder_first(
    rows: &mut Vec<String>,
    query: &str,
    home: &str,
    is_dir: impl Fn(&str) -> bool,
) {
    if !path_mode(query) || matches!(query, "~" | "~/" | "/") {
        return;
    }
    let typed = expand(query, home).trim_end_matches('/').to_string();
    if typed.is_empty() || !is_dir(&typed) {
        return;
    }
    rows.retain(|r| r != &typed);
    rows.insert(0, typed);
}

/// M2h: the engines a "New Code tab: …" row can open, in row order.
pub(crate) const CODE_TAB_ENGINES: [awili_code::CodeEngine; 4] = [
    awili_code::CodeEngine::Claude,
    awili_code::CodeEngine::Codex,
    awili_code::CodeEngine::Glm,
    awili_code::CodeEngine::Dsh,
];

/// M2h (pure): the "Executor: {label}" rows for `query`, in the order of
/// `rows` (`(label, persist)` pairs). A row matches when the query is empty,
/// fuzzy-matches "Executor: {label}", or fuzzy-matches "executor".
pub(crate) fn executor_rows(
    query: &str,
    rows: &[(String, String)],
) -> Vec<crate::palette::PaletteCmd> {
    let q = query.trim();
    rows.iter()
        .filter(|(label, _)| {
            q.is_empty()
                || crate::palette::fuzzy_match(&format!("Executor: {label}"), q)
                || crate::palette::fuzzy_match("executor", q)
        })
        .map(|(label, persist)| crate::palette::PaletteCmd::UseExecutor {
            label: label.clone(),
            persist: persist.clone(),
        })
        .collect()
}

/// M2h (pure): the "New Code tab: {engine}" rows for `query`. A row matches
/// when the query is empty or fuzzy-matches "Executor: New Code tab:
/// {engine}", so "code tab", the engine name, and ⌘E's "executor …" all
/// find it.
pub(crate) fn code_tab_rows(query: &str) -> Vec<crate::palette::PaletteCmd> {
    let q = query.trim();
    CODE_TAB_ENGINES
        .into_iter()
        .filter(|e| {
            q.is_empty()
                || crate::palette::fuzzy_match(&format!("Executor: New Code tab: {}", e.label()), q)
        })
        .map(crate::palette::PaletteCmd::NewCodeTab)
        .collect()
}

impl crate::AwiliApp {
    /// RC2-UX (R2-15): remember a chosen folder for ⌘O (newest first) and
    /// save the list.
    pub(crate) fn remember_recent_folder(&mut self, path: &std::path::Path) {
        remember_recent(&mut self.work.recent_folders, &path.display().to_string());
        if let (Some(persist), Ok(json)) = (
            self.persist.as_ref(),
            serde_json::to_string(&self.work.recent_folders),
        ) {
            persist.set_state(RECENT_FOLDERS_KEY, json);
        }
    }

    /// M2h: the palette's executor rows for the current query: Work → one
    /// "Executor: …" row per picker row (L1d's sections, in order); Code →
    /// the "New Code tab: …" rows; other modes → none.
    pub(crate) fn executor_palette_cmds(&self) -> Vec<crate::palette::PaletteCmd> {
        let query = self.palette.query.as_str();
        match self.mode {
            crate::work::Mode::Work => {
                let rows: Vec<(String, String)> = crate::executor_labels::picker_sections(
                    &self.work.engine,
                    &self.work.extra_engines,
                    &self.work.models_cfg,
                    &self.work.build_targets,
                )
                .into_iter()
                .flat_map(|s| s.rows)
                .map(|(label, exec)| (label, exec.to_persist()))
                .collect();
                executor_rows(query, &rows)
            }
            crate::work::Mode::Code => code_tab_rows(query),
            _ => Vec::new(),
        }
    }

    /// M2h: run an "Executor: …" row. An unparsable `persist` does nothing.
    /// Otherwise the same user pick as the mouse picker
    /// (`pick_work_executor`: persist, close the picker, U4f's Ask note),
    /// after the toast "Executor: {chip label}" (a U4f no-Ask toast wins).
    pub(crate) fn use_executor(&mut self, persist: &str, cx: &mut gpui::Context<Self>) {
        let Some(exec) = crate::work::WorkExecutor::from_persist(persist) else {
            return;
        };
        let chip = crate::executor_labels::chip_label(
            &exec,
            &self.work.engine,
            &self.work.extra_engines,
            &self.work.build_targets,
        );
        self.show_toast(format!("Executor: {chip}"), cx);
        self.pick_work_executor(exec, cx);
    }
}

#[cfg(test)]
#[path = "palette_targets_tests.rs"]
mod tests;
