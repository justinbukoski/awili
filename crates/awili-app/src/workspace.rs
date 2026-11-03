//! Workspace file tree — lazy, gitignore-aware, watchable.
//!
//! Replaces the eager `FileNode::build` with a lazy tree that only reads
//! children on expand, respects `.gitignore` via the `ignore` crate, and
//! watches for FS changes via `notify`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// A single entry in the file tree.
#[derive(Clone, Debug)]
pub struct FsEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub hidden: bool,
    pub ignored: bool,
    // Painted by the tree rows once the detail rail ships.
    #[allow(dead_code)]
    pub symlink: bool,
    #[allow(dead_code)]
    pub size: Option<u64>,
}

impl FsEntry {
    fn from_dir_entry(entry: &std::fs::DirEntry) -> Option<Self> {
        let name = entry.file_name().to_string_lossy().to_string();
        let ft = entry.file_type().ok()?;
        let meta = entry.metadata().ok();
        Some(Self {
            name,
            path: entry.path(),
            is_dir: ft.is_dir(),
            hidden: entry.file_name().to_string_lossy().starts_with('.'),
            ignored: false,
            symlink: ft.is_symlink(),
            size: meta.map(|m| m.len()),
        })
    }
}

/// The file tree state, held per workspace session.
pub struct FileTreeState {
    root: PathBuf,
    children: HashMap<PathBuf, Vec<FsEntry>>,
    expanded: HashSet<PathBuf>,
    gitignore: Option<Gitignore>,
    pub show_hidden: bool,
    pub show_ignored: bool,
    // Closing this channel requests shutdown without waiting for notify. Both
    // FSEvents startup and its destructor can block in macOS service calls.
    watcher_stop: Option<mpsc::Sender<()>>,
    fs_rx: Option<mpsc::Receiver<Vec<PathBuf>>>,
    pub generation: u64,
}

const ALWAYS_IGNORED: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    "build",
    ".next",
    ".cache",
    ".turbo",
    "vendor",
    ".DS_Store",
];

impl FileTreeState {
    pub fn new(root: PathBuf) -> Self {
        let mut state = Self::reader(root);
        state.start_watcher();
        state
    }

    /// An on-demand directory reader. Title menus refresh explicitly and must
    /// not keep an unconsumed filesystem-event queue after closing.
    pub fn reader(root: PathBuf) -> Self {
        let gitignore = Self::load_gitignore(&root);
        let mut state = Self {
            root: root.clone(),
            children: HashMap::new(),
            expanded: HashSet::new(),
            gitignore,
            show_hidden: false,
            show_ignored: false,
            watcher_stop: None,
            fs_rx: None,
            generation: 0,
        };
        state.ensure_loaded(&root);
        state
    }

    #[allow(dead_code)] // used by the tree header once it ships
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn root_entries(&self) -> &[FsEntry] {
        self.children
            .get(&self.root)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Read a directory already loaded by `children_of`, without doing I/O in
    /// render. The title menu owns a reader separate from the running job.
    pub fn loaded_entries(&self, dir: &Path) -> &[FsEntry] {
        self.children.get(dir).map(Vec::as_slice).unwrap_or(&[])
    }

    #[cfg(test)]
    pub(crate) fn is_watching(&self) -> bool {
        self.watcher_stop.is_some() || self.fs_rx.is_some()
    }

    pub fn children_of(&mut self, dir: &Path) -> &[FsEntry] {
        self.ensure_loaded(dir);
        self.children.get(dir).map(|v| v.as_slice()).unwrap_or(&[])
    }

    #[cfg(test)]
    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    #[cfg(test)] // Legacy hierarchical renderer retired; retained contract checks.
    pub fn toggle_expand(&mut self, path: &Path) {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
        } else {
            self.expanded.insert(path.to_path_buf());
            self.ensure_loaded(path);
        }
    }

    /// Poll for FS changes. Returns true if the tree was invalidated.
    pub fn poll_changes(&mut self) -> bool {
        let Some(rx) = &self.fs_rx else {
            return false;
        };
        let mut changed = false;
        let mut changed_paths = Vec::new();
        while let Ok(paths) = rx.try_recv() {
            changed_paths.extend(paths);
            changed = true;
        }
        for path in changed_paths {
            if let Some(parent) = path.parent() {
                self.children.remove(parent);
            }
            self.forget_subtree(&path);
        }
        if changed {
            let expanded: Vec<PathBuf> = self.expanded.iter().cloned().collect();
            for dir in &expanded {
                self.ensure_loaded(dir);
            }
            self.ensure_loaded(&self.root.clone());
            self.generation += 1;
        }
        changed
    }

    /// RC2-APP (F0303): a changed path invalidates its whole cached subtree
    /// (a renamed or deleted folder's children must not come back from the
    /// cache), and a folder that is no longer a folder is no longer
    /// expanded (re-reading it would show an empty folder that is gone).
    fn forget_subtree(&mut self, path: &Path) {
        self.children.retain(|dir, _| !dir.starts_with(path));
        if !path.is_dir() {
            self.expanded.retain(|dir| !dir.starts_with(path));
        }
    }

    /// R1h: re-read the root and every expanded folder from disk now. The
    /// end of a run must not wait for the watcher, whose events can arrive
    /// late or not at all.
    pub fn reload(&mut self) {
        self.children.clear();
        let expanded: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        for dir in &expanded {
            self.ensure_loaded(dir);
        }
        self.ensure_loaded(&self.root.clone());
        self.generation += 1;
    }

    #[allow(dead_code)] // used by the tree footer once it ships
    pub fn visible_count(&self) -> usize {
        self.root_entries()
            .iter()
            .filter(|e| self.is_visible(e))
            .count()
    }

    pub fn is_visible(&self, entry: &FsEntry) -> bool {
        if entry.hidden && !self.show_hidden {
            return false;
        }
        if entry.ignored && !self.show_ignored {
            return false;
        }
        true
    }

    fn ensure_loaded(&mut self, dir: &Path) {
        if self.children.contains_key(dir) {
            return;
        }
        let entries = self.read_dir(dir);
        self.children.insert(dir.to_path_buf(), entries);
    }

    fn read_dir(&self, dir: &Path) -> Vec<FsEntry> {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut entries: Vec<FsEntry> = rd
            .flatten()
            .filter_map(|e| {
                let mut entry = FsEntry::from_dir_entry(&e)?;
                if entry.is_dir && ALWAYS_IGNORED.contains(&entry.name.as_str()) {
                    entry.ignored = true;
                }
                if !entry.ignored {
                    if let Some(gi) = &self.gitignore {
                        let matched = gi.matched_path_or_any_parents(&entry.path, entry.is_dir);
                        if matched.is_ignore() {
                            entry.ignored = true;
                        }
                    }
                }
                Some(entry)
            })
            .collect();
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        entries
    }

    fn load_gitignore(root: &Path) -> Option<Gitignore> {
        let gi_path = root.join(".gitignore");
        if !gi_path.exists() {
            return None;
        }
        let mut builder = GitignoreBuilder::new(root);
        builder.add(&gi_path);
        let mut dir = root.parent();
        while let Some(parent) = dir {
            let parent_gi = parent.join(".gitignore");
            if parent_gi.exists() {
                builder.add(&parent_gi);
            }
            if parent.join(".git").exists() {
                break;
            }
            dir = parent.parent();
        }
        builder.build().ok()
    }

    fn start_watcher(&mut self) {
        #[cfg(test)]
        if let Some(start) = WATCHER_START_OVERRIDE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|(root, _)| root == &self.root) {
                slot.take().map(|(_, start)| start)
            } else {
                None
            }
        }) {
            if let Some((stop, rx)) = start_owned_watcher(move |tx| {
                start(tx);
                Some(())
            }) {
                self.watcher_stop = Some(stop);
                self.fs_rx = Some(rx);
            }
            return;
        }

        let root = self.root.clone();
        if let Some((stop, rx)) = start_owned_watcher(move |tx| {
            let mut watcher = RecommendedWatcher::new(
                move |res: Result<notify::Event, notify::Error>| {
                    if let Ok(event) = res {
                        if !event.paths.is_empty() {
                            let _ = tx.send(event.paths);
                        }
                    }
                },
                notify::Config::default(),
            )
            .ok()?;
            watcher.watch(&root, RecursiveMode::Recursive).ok()?;
            Some(watcher)
        }) {
            self.watcher_stop = Some(stop);
            self.fs_rx = Some(rx);
        }
    }
}

/// The backend never crosses into the UI owner, including on startup failure.
/// A stalled OS registration cannot be interrupted through notify; once it
/// returns, a disconnected owner skips the wait and drops it on this worker.
fn start_owned_watcher<W: 'static>(
    start: impl FnOnce(mpsc::Sender<Vec<PathBuf>>) -> Option<W> + Send + 'static,
) -> Option<(mpsc::Sender<()>, mpsc::Receiver<Vec<PathBuf>>)> {
    let (stop, stop_rx) = mpsc::channel();
    let (events, events_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("awili-workspace-watch".into())
        .spawn(move || {
            if !matches!(stop_rx.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                return;
            }
            if let Some(watcher) = start(events) {
                // Already disconnected during startup? recv returns immediately.
                let _ = stop_rx.recv();
                drop(watcher);
            }
        })
        .ok()?;
    Some((stop, events_rx))
}

#[cfg(test)]
type WatcherStart = Box<dyn FnOnce(mpsc::Sender<Vec<PathBuf>>) + Send>;
#[cfg(test)]
thread_local! {
    static WATCHER_START_OVERRIDE: std::cell::RefCell<Option<(PathBuf, WatcherStart)>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(crate) fn override_watcher_start(root: PathBuf, start: WatcherStart) {
    WATCHER_START_OVERRIDE.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some((root, start));
    });
}

// ============================================================================
// Backward compat: snapshot visible top-level names for job refresh checks
// ============================================================================

use crate::work::FileNode;

pub fn file_tree_from_workspace(tree: &mut FileTreeState) -> Vec<FileNode> {
    let root_entries: Vec<FsEntry> = tree
        .root_entries()
        .iter()
        .filter(|e| tree.is_visible(e))
        .cloned()
        .collect();
    build_nodes(&root_entries)
}

fn build_nodes(entries: &[FsEntry]) -> Vec<FileNode> {
    entries
        .iter()
        .map(|entry| FileNode {
            name: entry.name.clone(),
            is_dir: entry.is_dir,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    struct BlockingDrop {
        entered: mpsc::Sender<std::thread::ThreadId>,
        release: mpsc::Receiver<()>,
        finished: mpsc::Sender<()>,
    }
    impl Drop for BlockingDrop {
        fn drop(&mut self) {
            self.entered.send(std::thread::current().id()).unwrap();
            self.release.recv_timeout(Duration::from_secs(5)).unwrap();
            self.finished.send(()).unwrap();
        }
    }

    #[test]
    fn watcher_owner_drop_never_waits_for_backend_destructor() {
        let ui = std::thread::current().id();
        let (ready_tx, ready) = mpsc::channel();
        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let (finished_tx, finished) = mpsc::channel();
        let (stop, events) = start_owned_watcher(move |_| {
            ready_tx.send(std::thread::current().id()).unwrap();
            Some(BlockingDrop {
                entered: entered_tx,
                release,
                finished: finished_tx,
            })
        })
        .unwrap();
        let worker = ready.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_ne!(worker, ui);
        let before = Instant::now();
        drop(stop);
        drop(events);
        assert!(before.elapsed() < Duration::from_secs(1));
        assert_eq!(
            entered.recv_timeout(Duration::from_secs(5)).unwrap(),
            worker
        );
        assert!(
            finished.try_recv().is_err(),
            "cleanup remains blocked off the UI"
        );
        release_tx.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn watcher_cancel_during_startup_cleans_up_after_start_returns() {
        let (started_tx, started) = mpsc::channel();
        let (start_release_tx, start_release) = mpsc::channel();
        let (entered_tx, entered) = mpsc::channel();
        let (drop_release_tx, drop_release) = mpsc::channel();
        let (finished_tx, finished) = mpsc::channel();
        let before = Instant::now();
        let (stop, events) = start_owned_watcher(move |_| {
            started_tx.send(std::thread::current().id()).unwrap();
            start_release.recv_timeout(Duration::from_secs(5)).unwrap();
            Some(BlockingDrop {
                entered: entered_tx,
                release: drop_release,
                finished: finished_tx,
            })
        })
        .unwrap();
        assert!(before.elapsed() < Duration::from_secs(1));
        let worker = started.recv_timeout(Duration::from_secs(5)).unwrap();
        let before = Instant::now();
        drop(stop);
        drop(events);
        assert!(before.elapsed() < Duration::from_secs(1));
        assert!(entered.try_recv().is_err(), "startup is still stalled");
        start_release_tx.send(()).unwrap();
        assert_eq!(
            entered.recv_timeout(Duration::from_secs(5)).unwrap(),
            worker
        );
        drop_release_tx.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn replacing_tree_disconnects_old_events_and_only_refreshes_new_root() {
        let old_dir = tempfile::tempdir().unwrap();
        let new_dir = tempfile::tempdir().unwrap();
        let (old_tx, old_events) = mpsc::channel();
        override_watcher_start(
            old_dir.path().into(),
            Box::new(move |events| {
                old_tx.send(events).unwrap();
            }),
        );
        let old = FileTreeState::new(old_dir.path().into());
        let old_events = old_events.recv_timeout(Duration::from_secs(5)).unwrap();
        let (new_tx, new_events) = mpsc::channel();
        override_watcher_start(
            new_dir.path().into(),
            Box::new(move |events| {
                new_tx.send(events).unwrap();
            }),
        );
        let mut new = FileTreeState::new(new_dir.path().into());
        let new_events = new_events.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(old);
        assert!(old_events
            .send(vec![old_dir.path().join("late.txt")])
            .is_err());
        assert!(!new.poll_changes());
        fs::write(new_dir.path().join("fresh.txt"), "new").unwrap();
        new_events
            .send(vec![new_dir.path().join("fresh.txt")])
            .unwrap();
        assert!(new.poll_changes());
        assert_eq!(new.generation, 1);
        assert!(new.root_entries().iter().any(|e| e.name == "fresh.txt"));
        assert!(new
            .root_entries()
            .iter()
            .all(|e| e.path.starts_with(new_dir.path())));
    }

    fn make_tree(dir: &Path) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        fs::write(dir.join(".hidden"), "").unwrap();
        fs::create_dir_all(dir.join("node_modules/foo")).unwrap();
        fs::write(dir.join("node_modules/foo/bar.js"), "").unwrap();
        fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
        fs::write(dir.join("debug.log"), "").unwrap();
    }

    #[test]
    fn lazy_load_root() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let tree = FileTreeState::new(tmp.path().to_path_buf());
        assert!(!tree.root_entries().is_empty());
        let src = tmp.path().join("src");
        assert!(!tree.children.contains_key(&src));
    }

    #[test]
    fn expand_loads_children() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let mut tree = FileTreeState::new(tmp.path().to_path_buf());
        let src = tmp.path().join("src");
        tree.toggle_expand(&src);
        assert!(tree.is_expanded(&src));
        let children = tree.children_of(&src);
        assert!(children.iter().any(|e| e.name == "main.rs"));
    }

    #[test]
    fn hidden_filtered_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let tree = FileTreeState::new(tmp.path().to_path_buf());
        let hidden: Vec<_> = tree.root_entries().iter().filter(|e| e.hidden).collect();
        assert!(!hidden.is_empty());
        for h in &hidden {
            assert!(!tree.is_visible(h));
        }
    }

    #[test]
    fn node_modules_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let tree = FileTreeState::new(tmp.path().to_path_buf());
        let nm = tree
            .root_entries()
            .iter()
            .find(|e| e.name == "node_modules");
        assert!(nm.is_some());
        assert!(nm.unwrap().ignored);
        assert!(!tree.is_visible(nm.unwrap()));
    }

    #[test]
    fn gitignore_filters_log_files() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let tree = FileTreeState::new(tmp.path().to_path_buf());
        let log = tree.root_entries().iter().find(|e| e.name == "debug.log");
        assert!(log.is_some());
        assert!(log.unwrap().ignored);
    }

    #[test]
    fn toggle_expand_collapses() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let mut tree = FileTreeState::new(tmp.path().to_path_buf());
        let src = tmp.path().join("src");
        tree.toggle_expand(&src);
        assert!(tree.is_expanded(&src));
        tree.toggle_expand(&src);
        assert!(!tree.is_expanded(&src));
    }

    #[test]
    fn read_dir_sorts_directories_first_then_case_insensitive_alpha() {
        // The file-tree sidebar depends on read_dir's ordering: directories
        // grouped before files, each group sorted case-insensitively. Names are
        // chosen so a single assertion pins BOTH contracts and fails under the
        // two plausible refactors — flipping the is_dir comparison (files first)
        // and dropping the .to_lowercase() (ASCII-case-sensitive order).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // A dir whose name sorts alphabetically AFTER two files, so its
        // dirs-first placement can't be a mere alphabetical accident.
        fs::create_dir_all(root.join("zeta_dir")).unwrap();
        fs::create_dir_all(root.join("Alpha_dir")).unwrap();
        fs::write(root.join("aardvark.txt"), "").unwrap();
        fs::write(root.join("Banana.txt"), "").unwrap();
        fs::write(root.join("apple.txt"), "").unwrap();
        let tree = FileTreeState::new(root.to_path_buf());
        let order: Vec<&str> = tree
            .root_entries()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        // Dirs first (Alpha_dir, zeta_dir — case-insensitive), then files
        // (aardvark, apple, Banana — case-insensitive, NOT ASCII where the
        // capital 'B' of Banana would sort ahead of the lowercase file names).
        assert_eq!(
            order,
            vec![
                "Alpha_dir",
                "zeta_dir",
                "aardvark.txt",
                "apple.txt",
                "Banana.txt"
            ]
        );
    }

    #[test]
    fn show_hidden_and_show_ignored_flags_reveal_filtered_entries() {
        // is_visible gates on two independent toggles: `show_hidden` reveals
        // dotfiles and `show_ignored` reveals gitignore/ALWAYS_IGNORED entries.
        // Every other test leaves BOTH flags at their `false` default, so the
        // reveal path — the "show hidden files" / "show ignored" sidebar
        // toggles — is wholly unpinned: dropping either `&& !self.show_*`
        // conjunct (making the entry permanently invisible regardless of the
        // flag) survives the whole suite. Chosen entries isolate each conjunct:
        // `.hidden` is a dotfile but NOT ignored (gitignore is only `*.log`),
        // `node_modules` is ignored but NOT a dotfile, so show_hidden reveals
        // one without the other.
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let mut tree = FileTreeState::new(tmp.path().to_path_buf());

        let hidden = tree
            .root_entries()
            .iter()
            .find(|e| e.name == ".hidden")
            .unwrap()
            .clone();
        let ignored = tree
            .root_entries()
            .iter()
            .find(|e| e.name == "node_modules")
            .unwrap()
            .clone();

        // Default: both flags false -> both filtered out.
        assert!(!tree.is_visible(&hidden));
        assert!(!tree.is_visible(&ignored));

        // show_hidden reveals the dotfile but leaves the ignored dir hidden.
        tree.show_hidden = true;
        assert!(tree.is_visible(&hidden));
        assert!(!tree.is_visible(&ignored));

        // show_ignored reveals the ignored dir too.
        tree.show_ignored = true;
        assert!(tree.is_visible(&ignored));
    }

    #[test]
    fn backward_compat_file_nodes() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let mut tree = FileTreeState::new(tmp.path().to_path_buf());
        let nodes = file_tree_from_workspace(&mut tree);
        assert!(nodes.iter().any(|n| n.name == "src"));
        assert!(nodes.iter().any(|n| n.name == "Cargo.toml"));
        assert!(!nodes.iter().any(|n| n.name == "node_modules"));
        assert!(!nodes.iter().any(|n| n.name == ".hidden"));
    }

    /// RC2-APP (F0303): a deleted folder takes its cached subtree and its
    /// expanded entries with it — nothing of it comes back from the cache.
    #[test]
    fn rc2app_a_deleted_folder_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (a, b, c) = (root.join("a"), root.join("a/b"), root.join("a/b/c"));
        fs::create_dir_all(&c).unwrap();
        fs::write(c.join("x.txt"), "x").unwrap();
        let mut state = FileTreeState::new(root.clone());
        for dir in [&a, &b, &c] {
            state.toggle_expand(dir);
        }
        assert!(state.children.contains_key(&c));
        // Drive the change through the app's own path, not the OS watcher.
        let (tx, rx) = mpsc::channel();
        state.watcher_stop = None;
        state.fs_rx = Some(rx);
        fs::remove_dir_all(&b).unwrap();
        tx.send(vec![b.clone()]).unwrap();
        assert!(state.poll_changes());
        for gone in [&b, &c] {
            assert!(!state.children.contains_key(gone), "{gone:?} cached");
            assert!(!state.is_expanded(gone), "{gone:?} expanded");
        }
        assert!(state.is_expanded(&a), "the parent stays open");
        assert!(
            state.children_of(&a).is_empty(),
            "and shows the folder gone"
        );
    }
}
