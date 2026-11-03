//! `subagent.*` reducer — the live subagent / parallel-work tree (plan unit 18).
//!
//! Ports Hermes Desktop's `toProgress` / `upsertSubagent` / `buildSubagentTree`
//! reducer (hermes-desktop-review §05) onto Awili's typed gateway events. It folds
//! the typed [`GatewayEventKind::Subagent`] pushes (see [`crate::events`]) into a
//! [`SubagentProgress`] store which the Work panel's agents view (plan unit 19)
//! renders as a parent-nested tree with delegation grouping.
//!
//! **Pure and deterministic.** Wall-clock arrival time is passed in as `now_ms`
//! rather than read from a global clock, so the fold reproduces exactly from
//! recorded `subagent.*` frames — the property that makes it fixture-testable and
//! an ideal coder unit. A `None` field on a later event never clobbers a value an
//! earlier event established (`overlay` semantics); file-touch lists union across
//! events; the activity `stream` is capped so a long-lived agent can't grow it
//! without bound.

use crate::events::{GatewayEventKind, SubagentPhase};
use std::collections::HashSet;

/// Siblings whose starts fall within this window (and share a `task_count`) are
/// one parallel delegation batch. Mirrors Cowork's 5-second grouping heuristic.
pub const DELEGATION_WINDOW_MS: u64 = 5_000;

/// Upper bound on retained activity-feed entries per agent. The UI shows only the
/// last handful; the store keeps a bounded tail so a chatty agent's `stream`
/// cannot grow unbounded over a long run.
pub const MAX_STREAM: usize = 50;

/// The kind of a subagent activity-feed entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// A `subagent.progress` line.
    Progress,
    /// A `subagent.complete` summary.
    Summary,
    /// A `subagent.thinking` line.
    Thinking,
    /// A `subagent.tool` line.
    Tool,
}

/// One entry in a subagent's live activity feed.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamEntry {
    /// Arrival time (`now_ms`) of the event that produced this entry.
    pub at: u64,
    /// Which lifecycle phase produced it.
    pub kind: StreamKind,
    /// The human-readable line.
    pub text: String,
    /// Whether the producing event reported a failed/errored status.
    pub is_error: bool,
}

/// The reduced live state of one subagent. Mirrors Cowork's `SubagentProgress`.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentProgress {
    /// Stable subagent id (the fold key).
    pub id: String,
    /// The spawning agent's id, when this is a delegated child.
    pub parent_id: Option<String>,
    /// The task the agent was spawned to do.
    pub goal: Option<String>,
    /// The child agent's own session id, when it has one.
    pub session_id: Option<String>,
    /// The model the agent runs on.
    pub model: Option<String>,
    /// Lifecycle status: `pending` before it starts, `running` while working,
    /// `complete` when done — or the backend's explicit status (e.g. `failed`).
    pub status: String,
    /// The delegation batch size this agent belongs to.
    pub task_count: Option<u64>,
    /// This agent's slot in its delegation batch.
    pub task_index: Option<u64>,
    /// Arrival time of the first event that mentioned this agent.
    pub started_at: u64,
    /// Arrival time of the most recent event that mentioned it.
    pub updated_at: u64,
    /// `(updated_at - started_at)` in seconds — the elapsed timer.
    pub duration_seconds: Option<f64>,
    /// Accumulated cost, parsed from the raw `costUsd` string/number.
    pub cost_usd: Option<f64>,
    /// Input token count, when reported.
    pub input_tokens: Option<u64>,
    /// Output token count, when reported.
    pub output_tokens: Option<u64>,
    /// Running tool-call count, when reported.
    pub tool_count: Option<u64>,
    /// Files the agent has read (union across events).
    pub files_read: Vec<String>,
    /// Files the agent has written (union across events).
    pub files_written: Vec<String>,
    /// Bounded activity feed (oldest dropped past [`MAX_STREAM`]).
    pub stream: Vec<StreamEntry>,
    /// The agent's final summary, once complete.
    pub summary: Option<String>,
    /// The tool the agent is running right now, if any.
    pub current_tool: Option<String>,
}

/// A node in the parent-nested subagent tree.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentNode {
    /// This node's reduced state.
    pub progress: SubagentProgress,
    /// Children spawned by this agent, in store (insertion) order.
    pub children: Vec<SubagentNode>,
}

/// A parallel delegation batch — sibling agents started close together sharing a
/// `task_count`. Rendered as one grouped workstream.
#[derive(Debug, Clone, PartialEq)]
pub struct DelegationGroup {
    /// The shared batch size (`None` when the siblings carried no `task_count`).
    pub task_count: Option<u64>,
    /// Member subagent ids, in start order.
    pub members: Vec<String>,
}

// ============================================================================
// reducer
// ============================================================================

/// Build a fresh [`SubagentProgress`] from a `subagent.*` event. Returns `None`
/// for a non-`Subagent` event or one with a missing/empty id (which cannot key
/// the fold). The activity `stream` starts empty — [`upsert_subagent`] appends to
/// it; call this directly only for the initial state of a known-new agent.
pub fn to_progress(kind: &GatewayEventKind, now_ms: u64) -> Option<SubagentProgress> {
    let GatewayEventKind::Subagent {
        phase,
        id,
        parent_id,
        goal,
        model,
        status,
        summary,
        cost_usd,
        session_id,
        task_count,
        task_index,
        input_tokens,
        output_tokens,
        tool_count,
        files_read,
        files_written,
        current_tool,
        ..
    } = kind
    else {
        return None;
    };
    let id = id.as_deref().filter(|s| !s.is_empty())?;
    Some(SubagentProgress {
        id: id.to_string(),
        parent_id: parent_id.clone(),
        goal: goal.clone(),
        session_id: session_id.clone(),
        model: model.clone(),
        status: status_for(*phase, status.as_deref()),
        task_count: *task_count,
        task_index: *task_index,
        started_at: now_ms,
        updated_at: now_ms,
        duration_seconds: Some(0.0),
        cost_usd: cost_usd.as_deref().and_then(parse_cost),
        input_tokens: *input_tokens,
        output_tokens: *output_tokens,
        tool_count: *tool_count,
        files_read: files_read.clone(),
        files_written: files_written.clone(),
        stream: Vec::new(),
        summary: summary.clone(),
        current_tool: current_tool.clone(),
    })
}

/// Fold one `subagent.*` event into the store: insert a new agent (keyed by id)
/// or merge into the existing one, then append an activity-feed entry. Returns
/// the index of the affected agent, or `None` if the event is not a usable
/// `Subagent` event (wrong variant or empty id) — in which case the store is
/// untouched.
///
/// Merge discipline: a present field overwrites; a `None` field is ignored
/// (`overlay`), so a terse mid-run event never erases the goal/model an earlier
/// event established. `status`/`updated_at`/`duration_seconds` always refresh.
pub fn upsert_subagent(
    store: &mut Vec<SubagentProgress>,
    kind: &GatewayEventKind,
    now_ms: u64,
) -> Option<usize> {
    let GatewayEventKind::Subagent {
        phase,
        id,
        parent_id,
        goal,
        model,
        name,
        status,
        text,
        summary,
        cost_usd,
        session_id,
        task_count,
        task_index,
        input_tokens,
        output_tokens,
        tool_count,
        files_read,
        files_written,
        current_tool,
        ..
    } = kind
    else {
        return None;
    };
    let id = id.as_deref().filter(|s| !s.is_empty())?;

    let idx = match store.iter().position(|a| a.id == id) {
        Some(i) => {
            let a = &mut store[i];
            a.updated_at = now_ms;
            // RC2-SVC4 (F0701): a late event that names no status never
            // reopens an agent that already finished — its status (and its
            // run time) stay as the terminal event left them. An explicit
            // status from upstream still wins.
            let explicit = status.as_deref().is_some_and(|s| !s.is_empty());
            if explicit || !is_terminal_status(&a.status) {
                a.duration_seconds = Some(now_ms.saturating_sub(a.started_at) as f64 / 1000.0);
                a.status = status_for(*phase, status.as_deref());
            }
            overlay(&mut a.parent_id, parent_id);
            overlay(&mut a.goal, goal);
            overlay(&mut a.session_id, session_id);
            overlay(&mut a.model, model);
            overlay(&mut a.summary, summary);
            overlay(&mut a.current_tool, current_tool);
            if task_count.is_some() {
                a.task_count = *task_count;
            }
            if task_index.is_some() {
                a.task_index = *task_index;
            }
            if input_tokens.is_some() {
                a.input_tokens = *input_tokens;
            }
            if output_tokens.is_some() {
                a.output_tokens = *output_tokens;
            }
            if tool_count.is_some() {
                a.tool_count = *tool_count;
            }
            if let Some(c) = cost_usd.as_deref().and_then(parse_cost) {
                a.cost_usd = Some(c);
            }
            union_paths(&mut a.files_read, files_read);
            union_paths(&mut a.files_written, files_written);
            i
        }
        None => {
            store.push(to_progress(kind, now_ms)?);
            store.len() - 1
        }
    };

    // A `subagent.tool` frame identifies the running tool in `name` when it omits
    // an explicit `currentTool` (the upstream Cowork shape). Surface it so both
    // the live row and the Tool activity entry show what the agent is doing.
    // `overlay` already applied any explicit `currentTool` above, so only fill
    // when still empty.
    if *phase == SubagentPhase::Tool && store[idx].current_tool.is_none() {
        if let Some(n) = name.as_deref().filter(|s| !s.is_empty()) {
            store[idx].current_tool = Some(n.to_string());
        }
    }

    // A completed agent is no longer running a tool — clear any stale current_tool
    // (overlay alone never clears it, since absent fields don't clobber).
    if *phase == SubagentPhase::Complete {
        store[idx].current_tool = None;
    }

    if let Some(entry) = stream_entry(
        *phase,
        text.as_deref(),
        summary.as_deref(),
        store[idx].current_tool.as_deref(),
        status.as_deref(),
        now_ms,
    ) {
        let stream = &mut store[idx].stream;
        stream.push(entry);
        if stream.len() > MAX_STREAM {
            let overflow = stream.len() - MAX_STREAM;
            stream.drain(0..overflow);
        }
    }
    Some(idx)
}

/// Assemble the parent-nested tree from the flat store. Roots are agents with no
/// `parent_id`, plus orphans whose `parent_id` names an agent not in the store
/// (so a lost parent never hides its children). Insertion order is preserved at
/// every level. Every agent appears **exactly once**: a `parent_id` cycle both
/// terminates (the `visited` guard) and cannot hide its members — any agent not
/// reachable from a natural root is promoted to a root in a second pass, so the
/// live tree never silently drops an agent.
pub fn build_subagent_tree(store: &[SubagentProgress]) -> Vec<SubagentNode> {
    let ids: HashSet<&str> = store.iter().map(|a| a.id.as_str()).collect();
    let mut visited: HashSet<String> = HashSet::new();
    let mut roots: Vec<SubagentNode> = Vec::new();

    // Natural roots: no parent, or a parent absent from the store.
    for a in store.iter().filter(|a| match &a.parent_id {
        None => true,
        Some(p) => !ids.contains(p.as_str()),
    }) {
        roots.push(build_node(a, store, &mut visited));
    }
    // Cycle rescue: any agent still unvisited has an ancestor chain that never
    // reaches a root (a `parent_id` cycle). Promote the first-seen to a root so
    // no agent is hidden; the `visited` guard nests the rest of the cycle under it.
    // Re-scan the store each step: promoting one agent can visit others, so the
    // set shrinks as we go (and `build_node` skips already-visited agents).
    loop {
        let next = store.iter().find(|a| !visited.contains(a.id.as_str()));
        match next {
            Some(a) => roots.push(build_node(a, store, &mut visited)),
            None => break,
        }
    }
    roots
}

fn build_node(
    agent: &SubagentProgress,
    store: &[SubagentProgress],
    visited: &mut HashSet<String>,
) -> SubagentNode {
    visited.insert(agent.id.clone());
    // Collect child refs first (immutable borrow) so the recursive call can take
    // `visited` mutably; `!visited` also breaks any parent_id cycle.
    let child_refs: Vec<&SubagentProgress> = store
        .iter()
        .filter(|c| {
            c.parent_id.as_deref() == Some(agent.id.as_str()) && !visited.contains(c.id.as_str())
        })
        .collect();
    let children = child_refs
        .into_iter()
        .map(|c| build_node(c, store, visited))
        .collect();
    SubagentNode {
        progress: agent.clone(),
        children,
    }
}

/// Group a parent's direct children into parallel delegation batches: siblings
/// sharing a `task_count` whose `started_at` fall within [`DELEGATION_WINDOW_MS`]
/// of the batch's earliest member. Siblings started far apart (or with a
/// different `task_count`) form separate groups; a lone sibling is a one-member
/// group. Members are ordered by start time (id-tiebroken for determinism).
pub fn delegation_groups(siblings: &[SubagentProgress]) -> Vec<DelegationGroup> {
    let mut ordered: Vec<&SubagentProgress> = siblings.iter().collect();
    ordered.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    // (anchor start time, group) — anchor is the earliest member's start.
    let mut groups: Vec<(u64, DelegationGroup)> = Vec::new();
    for a in ordered {
        let slot = groups.iter_mut().find(|(anchor, g)| {
            g.task_count == a.task_count
                && a.started_at.saturating_sub(*anchor) <= DELEGATION_WINDOW_MS
        });
        match slot {
            Some((_, g)) => g.members.push(a.id.clone()),
            None => groups.push((
                a.started_at,
                DelegationGroup {
                    task_count: a.task_count,
                    members: vec![a.id.clone()],
                },
            )),
        }
    }
    groups.into_iter().map(|(_, g)| g).collect()
}

// ============================================================================
// helpers
// ============================================================================

/// Derive a status string from the phase, letting an explicit backend `status`
/// (e.g. `failed`) win over the phase-implied default.
fn status_for(phase: SubagentPhase, explicit: Option<&str>) -> String {
    if let Some(s) = explicit {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    match phase {
        SubagentPhase::SpawnRequested => "pending",
        SubagentPhase::Complete => "complete",
        _ => "running",
    }
    .to_string()
}

/// RC2-SVC4 (F0701): the statuses after which an agent is finished.
fn is_terminal_status(status: &str) -> bool {
    [
        "complete",
        "completed",
        "failed",
        "error",
        "cancelled",
        "canceled",
        "interrupted",
    ]
    .iter()
    .any(|t| status.eq_ignore_ascii_case(t))
}

/// Overwrite `dst` only when `src` is present — a `None` never clobbers.
fn overlay(dst: &mut Option<String>, src: &Option<String>) {
    if src.is_some() {
        *dst = src.clone();
    }
}

/// Append any new paths from `src` to `dst`, preserving order and skipping dups.
fn union_paths(dst: &mut Vec<String>, src: &[String]) {
    for p in src {
        if !dst.iter().any(|e| e == p) {
            dst.push(p.clone());
        }
    }
}

/// Parse the raw `costUsd` string (a number is stringified upstream) into `f64`.
fn parse_cost(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

/// Build the activity-feed entry a phase contributes, or `None` when the event
/// carries no display text (e.g. a bare `spawn_requested`/`start`).
fn stream_entry(
    phase: SubagentPhase,
    text: Option<&str>,
    summary: Option<&str>,
    current_tool: Option<&str>,
    status: Option<&str>,
    now_ms: u64,
) -> Option<StreamEntry> {
    let is_error = matches!(status, Some(s) if s.eq_ignore_ascii_case("failed") || s.eq_ignore_ascii_case("error"));
    let (kind, content) = match phase {
        SubagentPhase::Thinking => (StreamKind::Thinking, text),
        SubagentPhase::Tool => (StreamKind::Tool, text.or(current_tool)),
        SubagentPhase::Progress | SubagentPhase::Other => (StreamKind::Progress, text),
        SubagentPhase::Complete => (StreamKind::Summary, summary.or(text)),
        SubagentPhase::SpawnRequested | SubagentPhase::Start => return None,
    };
    let content = content?;
    if content.is_empty() {
        return None;
    }
    Some(StreamEntry {
        at: now_ms,
        kind,
        text: content.to_string(),
        is_error,
    })
}

// ============================================================================
// tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A `subagent.*` event with all fields defaulted; tests set what they need.
    fn sub(phase: SubagentPhase, id: &str) -> GatewayEventKind {
        GatewayEventKind::Subagent {
            phase,
            id: Some(id.to_string()),
            parent_id: None,
            goal: None,
            model: None,
            name: None,
            status: None,
            text: None,
            summary: None,
            cost_usd: None,
            session_id: None,
            task_count: None,
            task_index: None,
            input_tokens: None,
            output_tokens: None,
            tool_count: None,
            files_read: vec![],
            files_written: vec![],
            current_tool: None,
        }
    }

    #[test]
    fn creates_then_merges_and_accumulates_stream() {
        let mut store = Vec::new();

        // spawn — establishes goal/model, no stream entry (bare start).
        let mut e = sub(SubagentPhase::Start, "a1");
        if let GatewayEventKind::Subagent { goal, model, .. } = &mut e {
            *goal = Some("do the thing".into());
            *model = Some("opus".into());
        }
        assert_eq!(upsert_subagent(&mut store, &e, 1_000), Some(0));
        assert_eq!(store.len(), 1);
        assert_eq!(store[0].status, "running");
        assert_eq!(store[0].goal.as_deref(), Some("do the thing"));
        assert!(store[0].stream.is_empty());

        // progress — appends a Progress entry, refreshes duration.
        let mut p = sub(SubagentPhase::Progress, "a1");
        if let GatewayEventKind::Subagent { text, .. } = &mut p {
            *text = Some("reading files".into());
        }
        upsert_subagent(&mut store, &p, 3_500);
        assert_eq!(store.len(), 1, "same id merges, not appends");
        assert_eq!(store[0].updated_at, 3_500);
        assert_eq!(store[0].duration_seconds, Some(2.5));
        assert_eq!(store[0].stream.len(), 1);
        assert_eq!(store[0].stream[0].kind, StreamKind::Progress);

        // complete — Summary entry + cost parse + status.
        let mut c = sub(SubagentPhase::Complete, "a1");
        if let GatewayEventKind::Subagent {
            summary, cost_usd, ..
        } = &mut c
        {
            *summary = Some("all done".into());
            *cost_usd = Some("0.42".into());
        }
        upsert_subagent(&mut store, &c, 5_000);
        assert_eq!(store[0].status, "complete");
        assert_eq!(store[0].summary.as_deref(), Some("all done"));
        assert_eq!(store[0].cost_usd, Some(0.42));
        assert_eq!(store[0].stream.last().unwrap().kind, StreamKind::Summary);
    }

    #[test]
    fn none_never_clobbers_established_fields() {
        let mut store = Vec::new();
        let mut e = sub(SubagentPhase::Start, "a1");
        if let GatewayEventKind::Subagent { goal, .. } = &mut e {
            *goal = Some("keep me".into());
        }
        upsert_subagent(&mut store, &e, 0);
        // a terse progress event with goal:None must not erase the goal.
        upsert_subagent(&mut store, &sub(SubagentPhase::Progress, "a1"), 100);
        assert_eq!(store[0].goal.as_deref(), Some("keep me"));
    }

    #[test]
    fn files_union_across_events_dedups() {
        let mut store = Vec::new();
        let mut e1 = sub(SubagentPhase::Tool, "a1");
        if let GatewayEventKind::Subagent { files_read, .. } = &mut e1 {
            *files_read = vec!["a.rs".into(), "b.rs".into()];
        }
        upsert_subagent(&mut store, &e1, 0);
        let mut e2 = sub(SubagentPhase::Tool, "a1");
        if let GatewayEventKind::Subagent { files_read, .. } = &mut e2 {
            *files_read = vec!["b.rs".into(), "c.rs".into()];
        }
        upsert_subagent(&mut store, &e2, 10);
        assert_eq!(store[0].files_read, vec!["a.rs", "b.rs", "c.rs"]);
    }

    /// RC2-SVC4 (F0701): a late status-less event (a stray progress or tool
    /// frame) after the agent finished does not reopen it, and its run time
    /// stops at the finish; an explicit status still applies.
    #[test]
    fn rc2svc4_a_late_event_does_not_reopen_a_finished_agent() {
        let mut store = Vec::new();
        upsert_subagent(&mut store, &sub(SubagentPhase::Start, "a1"), 0);
        upsert_subagent(&mut store, &sub(SubagentPhase::Complete, "a1"), 2_000);
        assert_eq!(store[0].status, "complete");
        upsert_subagent(&mut store, &sub(SubagentPhase::Progress, "a1"), 9_000);
        assert_eq!(store[0].status, "complete", "a late frame reopened it");
        assert_eq!(store[0].duration_seconds, Some(2.0));

        let mut failed = sub(SubagentPhase::Complete, "a2");
        if let GatewayEventKind::Subagent { status, .. } = &mut failed {
            *status = Some("failed".into());
        }
        upsert_subagent(&mut store, &failed, 0);
        upsert_subagent(&mut store, &sub(SubagentPhase::Complete, "a2"), 10);
        assert_eq!(
            store[1].status, "failed",
            "failed is not turned into complete"
        );

        let mut retry = sub(SubagentPhase::Progress, "a2");
        if let GatewayEventKind::Subagent { status, .. } = &mut retry {
            *status = Some("running".into());
        }
        upsert_subagent(&mut store, &retry, 20);
        assert_eq!(store[1].status, "running", "an explicit status still wins");
    }

    #[test]
    fn explicit_failed_status_wins_and_flags_stream_error() {
        let mut store = Vec::new();
        let mut e = sub(SubagentPhase::Complete, "a1");
        if let GatewayEventKind::Subagent {
            status, summary, ..
        } = &mut e
        {
            *status = Some("failed".into());
            *summary = Some("boom".into());
        }
        upsert_subagent(&mut store, &e, 0);
        assert_eq!(store[0].status, "failed");
        assert!(store[0].stream.last().unwrap().is_error);
    }

    #[test]
    fn stream_is_capped_at_max() {
        let mut store = Vec::new();
        for i in 0..(MAX_STREAM + 10) {
            let mut e = sub(SubagentPhase::Progress, "a1");
            if let GatewayEventKind::Subagent { text, .. } = &mut e {
                *text = Some(format!("step {i}"));
            }
            upsert_subagent(&mut store, &e, i as u64);
        }
        assert_eq!(store[0].stream.len(), MAX_STREAM);
        // oldest dropped: first retained entry is step 10.
        assert_eq!(store[0].stream[0].text, "step 10");
    }

    #[test]
    fn non_subagent_event_and_empty_id_ignored() {
        let mut store = Vec::new();
        assert_eq!(
            upsert_subagent(&mut store, &GatewayEventKind::GatewayReady, 0),
            None
        );
        let empty = sub(SubagentPhase::Start, "");
        assert_eq!(upsert_subagent(&mut store, &empty, 0), None);
        assert!(store.is_empty());
    }

    #[test]
    fn tree_nests_by_parent_and_orphans_are_roots() {
        let mut store = Vec::new();
        // root -> child -> grandchild; plus an orphan whose parent is absent.
        upsert_subagent(&mut store, &sub(SubagentPhase::Start, "root"), 0);
        let child = child_of("child", "root");
        upsert_subagent(&mut store, &child, 1);
        let gc = child_of("gc", "child");
        upsert_subagent(&mut store, &gc, 2);
        let orphan = child_of("orphan", "ghost");
        upsert_subagent(&mut store, &orphan, 3);

        let tree = build_subagent_tree(&store);
        // roots: "root" and "orphan" (its parent "ghost" isn't in the store).
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].progress.id, "root");
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].progress.id, "child");
        assert_eq!(tree[0].children[0].children[0].progress.id, "gc");
        assert_eq!(tree[1].progress.id, "orphan");
        assert!(tree[1].children.is_empty());
    }

    #[test]
    fn tree_includes_every_agent_even_in_a_cycle() {
        // a -> b -> a (parent cycle). Build must terminate AND hide no agent: the
        // first-seen is promoted to a root, the other nests under it, and each id
        // appears exactly once across the tree.
        let mut store = vec![
            progress_with_parent("a", Some("b")),
            progress_with_parent("b", Some("a")),
        ];
        store[0].started_at = 0;
        store[1].started_at = 1;
        let tree = build_subagent_tree(&store);
        assert_eq!(tree.len(), 1, "one promoted root, no hang");
        let mut ids = Vec::new();
        collect_ids(&tree, &mut ids);
        ids.sort();
        assert_eq!(ids, vec!["a", "b"], "every agent appears exactly once");
    }

    #[test]
    fn current_tool_clears_on_complete() {
        let mut store = Vec::new();
        let mut t = sub(SubagentPhase::Tool, "a1");
        if let GatewayEventKind::Subagent { current_tool, .. } = &mut t {
            *current_tool = Some("Edit".into());
        }
        upsert_subagent(&mut store, &t, 0);
        assert_eq!(store[0].current_tool.as_deref(), Some("Edit"));
        // completing clears the running tool even though the event omits it.
        upsert_subagent(&mut store, &sub(SubagentPhase::Complete, "a1"), 10);
        assert_eq!(store[0].current_tool, None);
    }

    #[test]
    fn tool_name_supplies_current_tool_and_stream() {
        // Upstream `subagent.tool` frames carry the tool identity in `name`, not
        // `currentTool`. The reducer must surface it so the live row and the Tool
        // activity entry still show what's running.
        let mut store = Vec::new();
        let mut t = sub(SubagentPhase::Tool, "a1");
        if let GatewayEventKind::Subagent { name, .. } = &mut t {
            *name = Some("Bash".into());
        }
        upsert_subagent(&mut store, &t, 0);
        assert_eq!(store[0].current_tool.as_deref(), Some("Bash"));
        assert_eq!(
            store[0].stream.last().map(|e| e.text.as_str()),
            Some("Bash")
        );

        // An explicit `currentTool` still wins over `name` when both are present.
        let mut store2 = Vec::new();
        let mut t2 = sub(SubagentPhase::Tool, "a2");
        if let GatewayEventKind::Subagent {
            name, current_tool, ..
        } = &mut t2
        {
            *name = Some("Bash".into());
            *current_tool = Some("Edit".into());
        }
        upsert_subagent(&mut store2, &t2, 0);
        assert_eq!(store2[0].current_tool.as_deref(), Some("Edit"));
    }

    #[test]
    fn delegation_groups_by_taskcount_and_window() {
        // three siblings, task_count=3, started within 5s -> one group.
        let mut a = progress_with_parent("a", Some("p"));
        let mut b = progress_with_parent("b", Some("p"));
        let mut c = progress_with_parent("c", Some("p"));
        for (s, t) in [(&mut a, 0u64), (&mut b, 1_000), (&mut c, 2_000)] {
            s.started_at = t;
            s.task_count = Some(3);
        }
        // a fourth sibling far later (>5s) -> separate group even with same count.
        let mut d = progress_with_parent("d", Some("p"));
        d.started_at = 20_000;
        d.task_count = Some(3);

        let groups = delegation_groups(&[a, b, c, d]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].members, vec!["a", "b", "c"]);
        assert_eq!(groups[0].task_count, Some(3));
        assert_eq!(groups[1].members, vec!["d"]);
    }

    #[test]
    fn delegation_separates_distinct_task_counts_in_window() {
        let mut a = progress_with_parent("a", Some("p"));
        a.started_at = 0;
        a.task_count = Some(2);
        let mut b = progress_with_parent("b", Some("p"));
        b.started_at = 500;
        b.task_count = Some(5);
        let groups = delegation_groups(&[a, b]);
        assert_eq!(groups.len(), 2, "different task_count -> different groups");
    }

    #[test]
    fn delegation_window_is_anchored_to_earliest_not_chained() {
        // Three siblings at 0/4000/8000ms, same task_count, each within
        // DELEGATION_WINDOW_MS (5s) of the *previous* one but NOT all within 5s
        // of the earliest. The window is anchored to the batch's earliest member
        // (docstring), so c@8000 (8000 > anchor 0 + 5000) starts a fresh group:
        // {a,b} and {c}. A member-chained window would instead chain all three
        // into one group (c is 4000ms after b) — this test pins anchor-relative
        // and would fail under that refactor. The existing window test uses an
        // 18s gap that passes under *both* interpretations, so it can't.
        let mut a = progress_with_parent("a", Some("p"));
        let mut b = progress_with_parent("b", Some("p"));
        let mut c = progress_with_parent("c", Some("p"));
        for (s, t) in [(&mut a, 0u64), (&mut b, 4_000), (&mut c, 8_000)] {
            s.started_at = t;
            s.task_count = Some(3);
        }
        let groups = delegation_groups(&[a, b, c]);
        assert_eq!(
            groups.len(),
            2,
            "anchor-relative window, not member-chained"
        );
        assert_eq!(groups[0].members, vec!["a", "b"]);
        assert_eq!(groups[1].members, vec!["c"]);
    }

    // --- test builders ------------------------------------------------------

    fn collect_ids<'a>(nodes: &'a [SubagentNode], out: &mut Vec<&'a str>) {
        for n in nodes {
            out.push(n.progress.id.as_str());
            collect_ids(&n.children, out);
        }
    }

    fn child_of(id: &str, parent: &str) -> GatewayEventKind {
        let mut e = sub(SubagentPhase::Start, id);
        if let GatewayEventKind::Subagent { parent_id, .. } = &mut e {
            *parent_id = Some(parent.to_string());
        }
        e
    }

    fn progress_with_parent(id: &str, parent: Option<&str>) -> SubagentProgress {
        SubagentProgress {
            id: id.to_string(),
            parent_id: parent.map(|s| s.to_string()),
            goal: None,
            session_id: None,
            model: None,
            status: "running".into(),
            task_count: None,
            task_index: None,
            started_at: 0,
            updated_at: 0,
            duration_seconds: Some(0.0),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            tool_count: None,
            files_read: vec![],
            files_written: vec![],
            stream: vec![],
            summary: None,
            current_tool: None,
        }
    }
}
