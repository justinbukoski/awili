//! Session identity duality — the `SessionIdMap` primitive.
//!
//! Hermes sessions carry a **dual identity**:
//!
//! - **Stored ID** (durable): the persistent identifier used by the session list,
//!   pins, routing, and resume. It lives as long as the session is stored.
//! - **Runtime ID** (ephemeral): the live per-connection handle that the gateway
//!   returns from `session.create` / `session.resume` and that turn verbs
//!   (`prompt_submit`, etc.) and incoming `event.session_id` use.
//!
//! Resuming the *same* stored id yields a *fresh* runtime id — the old runtime
//! handle becomes stale. This map keeps the two in sync and never collapses them.
//!
//! The map also tracks **lineage** (parent pointers for forked / branched sessions)
//! so that walking to the root ancestor is O(depth) and cycle-safe.

use std::collections::{HashMap, HashSet};

/// A bidirectional map between stored session IDs and runtime session IDs, plus
/// optional parent pointers for session lineage.
///
/// See the module-level documentation for the stored/runtime duality.
#[derive(Debug, Default, Clone)]
pub struct SessionIdMap {
    stored_to_runtime: HashMap<String, String>,
    runtime_to_stored: HashMap<String, String>,
    parent_pointers: HashMap<String, String>,
}

impl SessionIdMap {
    /// Create an empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) the runtime id for a stored id.
    ///
    /// # Stale-entry cleanup
    ///
    /// - If `stored_id` already had a *different* runtime id (i.e., a resume
    ///   replaced the old runtime), the OLD runtime→stored reverse entry is
    ///   removed so stale runtime ids never resolve.
    /// - If the *new* runtime id was previously linked to a different stored id,
    ///   that stale forward entry is dropped too.
    pub fn link(&mut self, stored_id: impl Into<String>, runtime_id: impl Into<String>) {
        let stored = stored_id.into();
        let runtime = runtime_id.into();

        // If stored already maps to a different runtime, remove the old reverse entry.
        if let Some(old_runtime) = self.stored_to_runtime.get(&stored) {
            if *old_runtime != runtime {
                self.runtime_to_stored.remove(old_runtime);
            }
        }

        // If runtime already maps to a different stored, remove the old forward entry.
        if let Some(old_stored) = self.runtime_to_stored.get(&runtime) {
            if *old_stored != stored {
                self.stored_to_runtime.remove(old_stored);
            }
        }

        self.stored_to_runtime
            .insert(stored.clone(), runtime.clone());
        self.runtime_to_stored.insert(runtime, stored);
    }

    /// Look up the runtime id for a stored id.
    pub fn runtime_id_by_stored(&self, stored_id: &str) -> Option<&str> {
        self.stored_to_runtime.get(stored_id).map(|s| s.as_str())
    }

    /// Look up the stored id for a runtime id.
    pub fn stored_id_by_runtime(&self, runtime_id: &str) -> Option<&str> {
        self.runtime_to_stored.get(runtime_id).map(|s| s.as_str())
    }

    /// Record a lineage parent pointer (for forked / branched sessions).
    ///
    /// A self-parent (`child_stored_id == parent_stored_id`) is silently ignored
    /// to prevent trivial cycles.
    pub fn set_parent(
        &mut self,
        child_stored_id: impl Into<String>,
        parent_stored_id: impl Into<String>,
    ) {
        let child = child_stored_id.into();
        let parent = parent_stored_id.into();
        if child == parent {
            return;
        }
        self.parent_pointers.insert(child, parent);
    }

    /// Walk parent pointers to the topmost ancestor and return it.
    ///
    /// If `stored_id` has no recorded parent, returns the id itself. If the id
    /// is entirely unknown with no parent, returns the input unchanged.
    ///
    /// # Cycle safety
    ///
    /// The walk tracks visited ids in a `HashSet`. If a parent chain loops back
    /// to an already-visited id, walking stops and returns the deepest distinct
    /// id reached — the walk is bounded by the number of recorded parents and
    /// can never infinite-loop.
    pub fn lineage_root<'a>(&'a self, stored_id: &'a str) -> &'a str {
        let mut visited = HashSet::<&str>::new();
        let mut current = stored_id;

        loop {
            visited.insert(current);
            match self.parent_pointers.get(current) {
                Some(parent) => {
                    if visited.contains(parent.as_str()) {
                        // Cycle detected — return the deepest distinct id reached
                        // (the id *before* the repeat).
                        return current;
                    }
                    current = parent.as_str();
                }
                None => return current,
            }
        }
    }

    /// Remove a stored id, its runtime reverse entry, and its parent pointer.
    ///
    /// Children that point at this stored id keep their (now-dangling) parent
    /// pointer; `lineage_root` still terminates because the cycle-safe walk
    /// will stop at the dangling id (no further parent exists for it).
    pub fn forget(&mut self, stored_id: &str) {
        // Drop the stored→runtime mapping and the reverse entry.
        if let Some(runtime_id) = self.stored_to_runtime.remove(stored_id) {
            self.runtime_to_stored.remove(&runtime_id);
        }
        // Drop the parent pointer for this id.
        self.parent_pointers.remove(stored_id);
    }

    /// Number of stored ids currently linked.
    pub fn len(&self) -> usize {
        self.stored_to_runtime.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.stored_to_runtime.is_empty()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- link round-trip ----

    #[test]
    fn link_and_query_both_directions() {
        let mut map = SessionIdMap::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);

        map.link("stored-1", "runtime-a");

        assert_eq!(map.runtime_id_by_stored("stored-1"), Some("runtime-a"));
        assert_eq!(map.stored_id_by_runtime("runtime-a"), Some("stored-1"));
        assert!(!map.is_empty());
        assert_eq!(map.len(), 1);
    }

    // ---- resume (re-link same stored to new runtime) drops old runtime ----

    #[test]
    fn resume_replaces_runtime_and_drops_old_reverse_entry() {
        let mut map = SessionIdMap::new();

        map.link("stored-1", "runtime-a");
        assert_eq!(map.stored_id_by_runtime("runtime-a"), Some("stored-1"));

        // Resume: same stored, fresh runtime.
        map.link("stored-1", "runtime-b");

        // New runtime resolves back to stored.
        assert_eq!(map.stored_id_by_runtime("runtime-b"), Some("stored-1"));
        // Old runtime is stale — must NOT resolve.
        assert!(map.stored_id_by_runtime("runtime-a").is_none());
        // Runtime-by-stored returns the new one.
        assert_eq!(map.runtime_id_by_stored("stored-1"), Some("runtime-b"));
        // Only one stored id linked.
        assert_eq!(map.len(), 1);
    }

    // ---- re-linking a runtime id away from an old stored id ----

    #[test]
    fn relink_runtime_to_different_stored_drops_old_forward_entry() {
        let mut map = SessionIdMap::new();

        map.link("stored-1", "runtime-a");
        map.link("stored-2", "runtime-a");

        // stored-1's entry was overwritten because runtime-a now points at stored-2.
        assert!(map.runtime_id_by_stored("stored-1").is_none());
        assert_eq!(map.stored_id_by_runtime("runtime-a"), Some("stored-2"));
        assert_eq!(map.runtime_id_by_stored("stored-2"), Some("runtime-a"));
        // Only one live forward entry remains.
        assert_eq!(map.len(), 1);
    }

    // ---- idempotent re-link of the identical pair leaves no leak ----

    #[test]
    fn link_idempotent_relink_keeps_entry() {
        let mut map = SessionIdMap::new();

        map.link("stored-1", "runtime-a");
        // Re-link the EXACT same pair. The end state must be unchanged: both
        // directions still resolve and len stays 1 (no duplicate/leak from
        // re-inserting an already-present pair).
        map.link("stored-1", "runtime-a");

        assert_eq!(map.runtime_id_by_stored("stored-1"), Some("runtime-a"));
        assert_eq!(map.stored_id_by_runtime("runtime-a"), Some("stored-1"));
        // No duplicate/leak: still exactly one linked stored id.
        assert_eq!(map.len(), 1);
    }

    // ---- both cleanup blocks fire in a SINGLE link call (resume + steal collide) ----

    #[test]
    fn link_simultaneous_resume_and_steal_stays_consistent() {
        let mut map = SessionIdMap::new();

        map.link("stored-1", "runtime-a");
        map.link("stored-2", "runtime-b");

        // Now link stored-1 -> runtime-b. This trips BOTH cleanup blocks at once:
        //   Block A: stored-1 already had runtime-a (!= runtime-b) -> drop reverse runtime-a.
        //   Block B: runtime-b already had stored-2 (!= stored-1)  -> drop forward stored-2.
        // The map must end fully consistent with no dangling half-entry on either side.
        map.link("stored-1", "runtime-b");

        // stored-1 <-> runtime-b is the only live link, both directions.
        assert_eq!(map.runtime_id_by_stored("stored-1"), Some("runtime-b"));
        assert_eq!(map.stored_id_by_runtime("runtime-b"), Some("stored-1"));

        // Block A worked: the old runtime-a reverse entry is gone.
        assert!(map.stored_id_by_runtime("runtime-a").is_none());
        // Block B worked: the stolen-from stored-2 forward entry is gone.
        assert!(map.runtime_id_by_stored("stored-2").is_none());

        // Collapsed to the single surviving link — len (the stored→runtime side)
        // reflects exactly one entry, no orphaned forward mapping left behind.
        assert_eq!(map.len(), 1);
    }

    // ---- lineage_root: no parent (returns self) ----

    #[test]
    fn lineage_root_no_parent_returns_self() {
        let mut map = SessionIdMap::new();
        map.link("stored-1", "runtime-a"); // link does not set a parent

        assert_eq!(map.lineage_root("stored-1"), "stored-1");
    }

    // ---- lineage_root: 2-level chain ----

    #[test]
    fn lineage_root_two_level_chain() {
        let mut map = SessionIdMap::new();

        // fork: child branched from parent
        map.set_parent("child", "parent");

        assert_eq!(map.lineage_root("child"), "parent");
        assert_eq!(map.lineage_root("parent"), "parent");
    }

    // ---- lineage_root: 3-level chain ----

    #[test]
    fn lineage_root_three_level_chain() {
        let mut map = SessionIdMap::new();

        map.set_parent("leaf", "branch");
        map.set_parent("branch", "trunk");
        map.set_parent("trunk", "root");

        assert_eq!(map.lineage_root("leaf"), "root");
        assert_eq!(map.lineage_root("branch"), "root");
        assert_eq!(map.lineage_root("trunk"), "root");
        assert_eq!(map.lineage_root("root"), "root");
    }

    // ---- self-parent is ignored ----

    #[test]
    fn self_parent_is_noop() {
        let mut map = SessionIdMap::new();

        map.set_parent("same", "same");

        // No parent pointer was recorded, so lineage_root returns self.
        assert_eq!(map.lineage_root("same"), "same");
        // No parent pointers exist.
        assert_eq!(map.lineage_root("other"), "other");
    }

    // ---- 2-node cycle terminates ----

    #[test]
    fn lineage_root_two_node_cycle_terminates() {
        let mut map = SessionIdMap::new();

        // Manually create A -> B -> A (a 2-node cycle).
        map.set_parent("a", "b");
        map.set_parent("b", "a");

        let root_a = map.lineage_root("a");
        let root_b = map.lineage_root("b");

        // The deepest distinct id reached before the repeat. For a -> b -> a,
        // walking from 'a': visited {a}, parent of a is b. b not visited, go to b.
        // visited {a, b}, parent of b is a. a IS visited → stop, return b.
        // So lineage_root("a") should return "b".
        assert_eq!(root_a, "b");
        // Walking from 'b': visited {b}, parent of b is a. a not visited, go to a.
        // visited {b, a}, parent of a is b. b IS visited → stop, return a.
        assert_eq!(root_b, "a");
    }

    // ---- forget removes all three entries ----

    #[test]
    fn forget_removes_stored_runtime_and_parent() {
        let mut map = SessionIdMap::new();

        map.link("stored-1", "runtime-a");
        map.set_parent("stored-1", "ancestor");

        assert_eq!(map.len(), 1);
        assert!(!map.is_empty());

        map.forget("stored-1");

        // Forward and reverse lookups are gone.
        assert!(map.runtime_id_by_stored("stored-1").is_none());
        assert!(map.stored_id_by_runtime("runtime-a").is_none());
        // len reflects the removal.
        assert_eq!(map.len(), 0);
        assert!(map.is_empty());

        // Parent pointer was removed — lineage_root falls through to self.
        assert_eq!(map.lineage_root("stored-1"), "stored-1");
    }

    // ---- lineage_root on an unknown id ----

    #[test]
    fn lineage_root_unknown_id_returns_input() {
        let mut map = SessionIdMap::new();
        map.set_parent("real-child", "real-parent");

        // An id that was never linked and has no parent pointer.
        assert_eq!(map.lineage_root("ghost-id"), "ghost-id");
        // A known child still resolves normally.
        assert_eq!(map.lineage_root("real-child"), "real-parent");
    }

    // ---- len / is_empty ----

    #[test]
    fn len_tracks_stored_ids() {
        let mut map = SessionIdMap::new();

        assert_eq!(map.len(), 0);
        assert!(map.is_empty());

        map.link("s1", "r1");
        assert_eq!(map.len(), 1);
        assert!(!map.is_empty());

        map.link("s2", "r2");
        assert_eq!(map.len(), 2);

        map.forget("s1");
        assert_eq!(map.len(), 1);

        map.forget("s2");
        assert_eq!(map.len(), 0);
        assert!(map.is_empty());
    }

    // ---- forget does not invalidate children' dangling parent walk ----

    #[test]
    fn forget_parent_dangling_child_still_terminates() {
        let mut map = SessionIdMap::new();

        map.set_parent("child", "parent");
        map.link("parent", "r-parent");

        // Normal: child -> parent.
        assert_eq!(map.lineage_root("child"), "parent");

        // Forget the parent.
        map.forget("parent");

        // Child's parent pointer is now dangling. lineage_root still terminates:
        // it walks child -> parent, finds no parent for parent, returns parent.
        assert_eq!(map.lineage_root("child"), "parent");

        // The parent id is gone from the map.
        assert!(map.runtime_id_by_stored("parent").is_none());
    }
}
