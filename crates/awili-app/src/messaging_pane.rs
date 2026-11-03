//! Messaging-connectors pane — pure list / detail / status state machine
//! (plan unit 25, Phase E, review-06 §Messaging Connectors).
//!
//! Unit 24 ([`awili_gateway::rest`]) built the PURE decoder half of the Cowork
//! messaging surface: [`MessagingPlatforms`] (the `/api/messaging/platforms`
//! envelope), the per-connector [`MessagingPlatform`], its
//! [`MessagingHomeChannel`] and the [`MessagingEnvVar`] rows, plus the
//! `RestClient` `messaging_platforms()` / `update_messaging()` /
//! `test_messaging()` calls. This module is the client-side *view* half: a pure
//! keyboard/selection state machine over that decoded snapshot — the list, the
//! filter, the detail selection, and the connector-health classification the
//! pane renders — designed to be re-fed by the plan's **6-second status poll**
//! without losing the user's place.
//!
//! ## Selection survives the poll (the crown-jewel behaviour)
//!
//! The pane refreshes every [`POLL_INTERVAL_MS`] by replacing the whole
//! platform list with a fresh [`MessagingPlatforms`] snapshot. If selection
//! were tracked by list *index*, a reorder or an added/removed connector
//! between polls would silently jump the detail pane to a different platform.
//! So selection is tracked by connector **id** ([`MessagingPaneState::selected_id`]):
//! [`MessagingPaneState::apply_snapshot`] keeps the same connector selected
//! across a refresh, and only falls back to the first visible row when the
//! selected connector genuinely disappears (or nothing was selected yet).
//!
//! ## What is code-verifiable vs. Owner-gated
//!
//! Everything here is `#[test]`-covered and window-free: health
//! classification, the word-AND filter (reusing unit-28's
//! [`every_word_matches`]), wrap-around navigation with stale-index recovery,
//! selection-by-id refresh, and the header roll-up. What is **NOT** here — and
//! is intentionally deferred — is the GPUI render (Owner's eyes) and, per the
//! plan, the **[LIVE]** `test` action, which needs a live platform to exercise
//! (`RestClient::test_messaging`). The pane exposes [`MessagingPaneState::test_target`]
//! so the eventual wiring knows *which* connector a "Test" button acts on, but
//! the call itself is left to the live-integration half. Until that lands the
//! module carries a `dead_code` allow exactly like [`crate::usage_chart`],
//! [`crate::command_catalog`] and [`crate::command_palette`]; the engine tests
//! exercise it regardless.
#![allow(dead_code)]

use awili_gateway::rest::{MessagingEnvVar, MessagingPlatform, MessagingPlatforms};

use crate::command_catalog::every_word_matches;
use crate::messaging_controls::{
    ChangePlan, ConnectorAction, ConnectorDraft, DraftValidation, EnvEdit,
};
use crate::messaging_selftest::SelfTestLog;

/// How often the pane re-fetches `/api/messaging/platforms` (plan unit 25:
/// "6s status poll"). The poll cadence lives here so the eventual GPUI host and
/// the tests agree on one number; the pure engine itself is time-free.
pub const POLL_INTERVAL_MS: u64 = 6_000;

/// The rendered health of a single connector, derived purely from a
/// [`MessagingPlatform`]'s flags. Drives the status dot/label in the list and
/// detail header.
///
/// Precedence (first match wins) — most actionable state surfaces first:
/// 1. an error is reported → [`ConnectorHealth::Error`]
/// 2. not configured (missing creds) → [`ConnectorHealth::NeedsSetup`]
/// 3. configured but the user turned it off → [`ConnectorHealth::Disabled`]
/// 4. enabled + configured + gateway running → [`ConnectorHealth::Running`]
/// 5. enabled + configured, gateway not running → [`ConnectorHealth::Ready`]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorHealth {
    /// The connector reported an error (`error_code` / `error_message` set).
    Error,
    /// Missing required configuration — can't run until set up.
    NeedsSetup,
    /// Configured but switched off by the user.
    Disabled,
    /// Enabled, configured, and the gateway process is live.
    Running,
    /// Enabled and configured, but the gateway process is not running yet.
    Ready,
}

impl ConnectorHealth {
    /// Classify a connector from its decoded flags. Pure; see the precedence in
    /// the type docs.
    pub fn of(p: &MessagingPlatform) -> ConnectorHealth {
        if platform_has_error(p) {
            ConnectorHealth::Error
        } else if !p.configured {
            ConnectorHealth::NeedsSetup
        } else if !p.enabled {
            ConnectorHealth::Disabled
        } else if p.gateway_running {
            ConnectorHealth::Running
        } else {
            ConnectorHealth::Ready
        }
    }

    /// Short human label for the status chip.
    pub fn label(self) -> &'static str {
        match self {
            ConnectorHealth::Error => "Error",
            ConnectorHealth::NeedsSetup => "Needs setup",
            ConnectorHealth::Disabled => "Disabled",
            ConnectorHealth::Running => "Running",
            ConnectorHealth::Ready => "Ready",
        }
    }

    /// A single glyph for the status dot (render-agnostic; the GPUI host maps
    /// it to a coloured indicator).
    pub fn glyph(self) -> &'static str {
        match self {
            ConnectorHealth::Error => "✗",
            ConnectorHealth::NeedsSetup => "○",
            ConnectorHealth::Disabled => "·",
            ConnectorHealth::Running => "●",
            ConnectorHealth::Ready => "◉",
        }
    }
}

/// True when the connector is reporting an error: a non-empty `error_code` or a
/// non-empty `error_message`. (Both are `Option<String>` in the decoder; the
/// live dashboard sometimes sends an empty string rather than `null`, so empty
/// is treated as "no error".)
fn platform_has_error(p: &MessagingPlatform) -> bool {
    opt_nonempty(&p.error_code) || opt_nonempty(&p.error_message)
}

fn opt_nonempty(s: &Option<String>) -> bool {
    s.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false)
}

/// A compact list-row view of a connector — what the left-hand list renders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorRow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub health: ConnectorHealth,
}

impl ConnectorRow {
    fn from_platform(p: &MessagingPlatform) -> ConnectorRow {
        ConnectorRow {
            id: p.id.clone(),
            name: p.name.clone(),
            description: p.description.clone(),
            health: ConnectorHealth::of(p),
        }
    }
}

/// Header roll-up counts, one per health bucket, for the pane title bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HealthSummary {
    pub total: usize,
    pub running: usize,
    pub ready: usize,
    pub needs_setup: usize,
    pub disabled: usize,
    pub error: usize,
}

/// One env-var row for the detail pane, with a display value that never leaks a
/// secret (the server already redacts; this only decides the placeholder).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvVarView {
    pub key: String,
    pub required: bool,
    pub is_set: bool,
    pub is_password: bool,
    pub advanced: bool,
    pub description: String,
    /// What to show in the value cell: the server-redacted value if set, else a
    /// `(not set)` placeholder.
    pub display_value: String,
}

impl EnvVarView {
    /// Build the masked display view for one decoded env var. Public so the
    /// controls engine's tests ([`crate::messaging_controls`]) can pin their
    /// redaction guard against the exact strings this pane renders.
    pub fn from_env(v: &MessagingEnvVar) -> EnvVarView {
        let display_value = if v.is_set {
            match v.redacted_value.as_deref() {
                Some(r) if !r.is_empty() => r.to_string(),
                // Set but the server sent no redacted preview: mask it.
                _ => "••••••••".to_string(),
            }
        } else {
            "(not set)".to_string()
        };
        EnvVarView {
            key: v.key.clone(),
            required: v.required,
            is_set: v.is_set,
            is_password: v.is_password,
            advanced: v.advanced,
            description: v.description.clone(),
            display_value,
        }
    }
}

/// The pure list/detail/status state for the messaging pane.
///
/// Fed a fresh [`MessagingPlatforms`] every [`POLL_INTERVAL_MS`] via
/// [`apply_snapshot`](Self::apply_snapshot); driven by keyboard via
/// [`set_query`](Self::set_query) / [`select_next`](Self::select_next) /
/// [`select_prev`](Self::select_prev). Selection is keyed by connector id so it
/// survives poll refreshes (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct MessagingPaneState {
    platforms: Vec<MessagingPlatform>,
    /// `.env` file path the connectors read (envelope metadata).
    env_path: String,
    /// The command that starts the gateway (envelope metadata).
    gateway_start_command: String,
    /// Current filter text; word-AND substring over id/name/description.
    query: String,
    /// The selected connector's id, or `None` if nothing is selected.
    selected_id: Option<String>,
}

impl MessagingPaneState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The `.env` path reported by the last snapshot.
    pub fn env_path(&self) -> &str {
        &self.env_path
    }

    /// The gateway start command reported by the last snapshot.
    pub fn gateway_start_command(&self) -> &str {
        &self.gateway_start_command
    }

    /// The current filter text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Ingest a fresh poll snapshot. Preserves the current selection *by id*
    /// when that connector still exists; otherwise (or when nothing was
    /// selected) selects the first visible row under the active filter.
    pub fn apply_snapshot(&mut self, snap: MessagingPlatforms) {
        self.platforms = snap.platforms;
        self.env_path = snap.env_path;
        self.gateway_start_command = snap.gateway_start_command;
        self.reconcile_selection();
    }

    /// Update the filter text and keep selection valid: if the selected row is
    /// filtered out, drop to the first row that remains visible.
    pub fn set_query(&mut self, q: impl Into<String>) {
        self.query = q.into();
        self.reconcile_selection();
    }

    /// Append to the filter text (a printable keystroke while the pane has
    /// keyboard focus). Selection is re-reconciled through [`set_query`] so a
    /// keystroke that narrows the current row out of view moves to the first
    /// still-visible connector — same guarantee as a programmatic `set_query`.
    pub fn push_query(&mut self, s: &str) {
        let mut q = self.query.clone();
        q.push_str(s);
        self.set_query(q);
    }

    /// Delete the last character of the filter text (Backspace/Delete). No-op on
    /// an empty query. Widening the filter can only reveal rows, so the current
    /// selection always survives — but reconcile anyway for symmetry.
    pub fn backspace(&mut self) {
        if self.query.is_empty() {
            return;
        }
        let mut q = self.query.clone();
        q.pop();
        self.set_query(q);
    }

    /// Explicitly select a connector by id (e.g. a mouse click). No-op if the
    /// id isn't among the current platforms.
    pub fn select_id(&mut self, id: &str) {
        if self.platforms.iter().any(|p| p.id == id) {
            self.selected_id = Some(id.to_string());
        }
    }

    /// The id of the currently selected connector, if any.
    pub fn selected_id(&self) -> Option<&str> {
        self.selected_id.as_deref()
    }

    /// The connector rows visible under the active filter, in snapshot order.
    pub fn visible_rows(&self) -> Vec<ConnectorRow> {
        self.visible_platforms()
            .map(ConnectorRow::from_platform)
            .collect()
    }

    /// Index of the selected connector within [`visible_rows`](Self::visible_rows),
    /// if any is selected and visible.
    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected_id.as_deref()?;
        self.visible_platforms().position(|p| p.id == id)
    }

    /// The currently selected connector (full detail), if any.
    pub fn selected_platform(&self) -> Option<&MessagingPlatform> {
        let id = self.selected_id.as_deref()?;
        self.platforms.iter().find(|p| p.id == id)
    }

    /// The health of the selected connector, if any.
    pub fn selected_health(&self) -> Option<ConnectorHealth> {
        self.selected_platform().map(ConnectorHealth::of)
    }

    /// The env-var rows of the selected connector, masked for display.
    pub fn selected_env_vars(&self) -> Vec<EnvVarView> {
        self.selected_platform()
            .map(|p| p.env_vars.iter().map(EnvVarView::from_env).collect())
            .unwrap_or_default()
    }

    /// The connector a "Test" action would target: the selected connector's id,
    /// but only when it is actually testable (configured — a not-yet-configured
    /// connector has nothing to test). This is the [LIVE] hand-off point; the
    /// call itself (`RestClient::test_messaging`) is deferred.
    pub fn test_target(&self) -> Option<&str> {
        self.selected_platform()
            .filter(|p| p.configured)
            .map(|p| p.id.as_str())
    }

    /// Move selection to the next visible row, wrapping around. Recovers when
    /// the current selection is stale/out of view by starting at the top.
    pub fn select_next(&mut self) {
        self.step_selection(1);
    }

    /// Move selection to the previous visible row, wrapping around. Recovers
    /// when the current selection is stale/out of view by starting at the top.
    pub fn select_prev(&mut self) {
        self.step_selection(-1);
    }

    /// Header roll-up of connector health across the *visible* rows.
    pub fn health_summary(&self) -> HealthSummary {
        let mut s = HealthSummary::default();
        for p in self.visible_platforms() {
            s.total += 1;
            match ConnectorHealth::of(p) {
                ConnectorHealth::Running => s.running += 1,
                ConnectorHealth::Ready => s.ready += 1,
                ConnectorHealth::NeedsSetup => s.needs_setup += 1,
                ConnectorHealth::Disabled => s.disabled += 1,
                ConnectorHealth::Error => s.error += 1,
            }
        }
        s
    }

    // --- internals ---------------------------------------------------------

    /// Platforms passing the current filter, in snapshot order.
    fn visible_platforms(&self) -> impl Iterator<Item = &MessagingPlatform> {
        let q = self.query.clone();
        self.platforms.iter().filter(move |p| {
            if q.trim().is_empty() {
                return true;
            }
            let hay = format!("{} {} {}", p.id, p.name, p.description);
            every_word_matches(&hay, &q)
        })
    }

    /// Ensure `selected_id` points at a currently-visible row, else default to
    /// the first visible row (or `None` if none are visible).
    fn reconcile_selection(&mut self) {
        let still_visible = self
            .selected_id
            .as_deref()
            .map(|id| self.visible_platforms().any(|p| p.id == id))
            .unwrap_or(false);
        if !still_visible {
            let first = self.visible_platforms().next().map(|p| p.id.clone());
            self.selected_id = first;
        }
    }

    /// Shared wrap-around step for [`select_next`]/[`select_prev`] with
    /// stale-selection recovery. `dir` is +1 or -1.
    fn step_selection(&mut self, dir: isize) {
        let ids: Vec<&str> = self.visible_platforms().map(|p| p.id.as_str()).collect();
        let len = ids.len();
        if len == 0 {
            self.selected_id = None;
            return;
        }
        // Current position, or a recovery anchor when selection is stale.
        let cur = self
            .selected_id
            .as_deref()
            .and_then(|id| ids.iter().position(|x| *x == id));
        let next = match cur {
            // Stale/no selection: land on the first row regardless of direction.
            None => 0,
            Some(i) => {
                let len_i = len as isize;
                (((i as isize + dir) % len_i + len_i) % len_i) as usize
            }
        };
        self.selected_id = Some(ids[next].to_string());
    }
}

/// What a keystroke means while the hosted Messaging pane owns keyboard input.
/// The pane has no embedded text field (like the Cowork command palette), so
/// the capture-phase handler classifies every key through [`messaging_key`] and
/// drives the pure [`MessagingPaneState`] with the result — keeping the
/// key→action decision window-free and unit-tested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessagingKey {
    /// Append this string to the filter query (a printable char, or a space).
    Append(String),
    /// Delete the last query character (Backspace / Delete).
    Backspace,
    /// Move the detail selection to the next visible connector (Down).
    SelectNext,
    /// Move the detail selection to the previous visible connector (Up).
    SelectPrev,
    /// Not something the pane consumes (a chord, an unhandled named key) — the
    /// dispatcher lets it fall through so global chords (⌘K/⌘N/…) still fire.
    Ignore,
}

/// Classify a keystroke for the hosted Messaging pane. Mirrors the Cowork
/// palette's [`crate::command_palette::query_key`], with arrow keys added for
/// list navigation. Any modifier chord is [`MessagingKey::Ignore`] so it falls
/// through to the global chord handler (the pane never steals ⌘K/⌘N/⌘,).
pub fn messaging_key(key: &str, chord: bool) -> MessagingKey {
    if chord {
        return MessagingKey::Ignore;
    }
    match key {
        "down" => MessagingKey::SelectNext,
        "up" => MessagingKey::SelectPrev,
        "backspace" | "delete" => MessagingKey::Backspace,
        "space" => MessagingKey::Append(" ".to_string()),
        k if k.chars().count() == 1 => MessagingKey::Append(k.to_string()),
        _ => MessagingKey::Ignore,
    }
}

/// A one-line health roll-up for the pane header, e.g.
/// `"5 connectors · 2 running · 1 needs setup · 1 error"`. Only the non-zero
/// buckets after the total are shown (a clean header when everything is nominal),
/// and `connector`/`connectors` agree in number.
pub fn summary_line(s: &HealthSummary) -> String {
    let mut parts: Vec<String> = Vec::new();
    let noun = if s.total == 1 {
        "connector"
    } else {
        "connectors"
    };
    parts.push(format!("{} {}", s.total, noun));
    if s.running > 0 {
        parts.push(format!("{} running", s.running));
    }
    if s.ready > 0 {
        parts.push(format!("{} ready", s.ready));
    }
    if s.needs_setup > 0 {
        parts.push(format!("{} needs setup", s.needs_setup));
    }
    if s.disabled > 0 {
        parts.push(format!("{} disabled", s.disabled));
    }
    if s.error > 0 {
        parts.push(format!("{} error", s.error));
    }
    parts.join(" \u{00b7} ")
}

/// The live store backing the hosted Messaging pane: the pure pane state plus
/// the fetch lifecycle (loading / error / first-load flag) and the controls
/// lifecycle (draft / save / credential-edit), mirroring
/// [`crate::command_center::CommandCenterData`]. Populated by
/// `AwiliApp::refresh_messaging` on view-open.
///
/// The controls half holds a [`ConnectorDraft`] for the currently-selected
/// connector. See [`ConnectorDraft`] for the secret-safety invariant: the draft
/// starts empty, records only keys the user explicitly edits, and rejects any
/// value that looks like a redaction placeholder — so the displayed mask can
/// never be round-tripped back as a real credential.
///
/// A save awaiting the user's confirmation: the names-only [`ChangePlan`] shown
/// in the dialog, paired with the exact [`ConnectorAction`]s captured when the
/// dialog opened. Confirm applies *these* actions — never a re-derivation from
/// the live draft — so what the user approved is exactly what is written, even
/// if the draft is edited underneath the open dialog. Only [`plan`](Self::plan)
/// is ever surfaced to the UI ([`confirm_plan`](MessagingData::confirm_plan));
/// the captured actions carry values and stay internal.
#[derive(Clone, Debug)]
pub struct PendingSave {
    /// The connector the confirmation belongs to — pinned at open time so a
    /// stale Confirm can never target a different connector.
    connector_id: String,
    /// The draft generation the confirmation was opened against; the save
    /// completion matches it before touching state (stale completions never
    /// clear a newer draft).
    generation: u64,
    /// Names-only summary rendered in the confirmation dialog.
    plan: ChangePlan,
    /// The exact actions to apply on Confirm, snapshotted at open time.
    actions: Vec<ConnectorAction>,
    /// The connector's display name at open time, for the dialog header.
    /// `None` when the connector had already left the snapshot by the time the
    /// dialog opened (the header then falls back to the id).
    name: Option<String>,
}

/// The identity + captured actions returned by
/// [`MessagingData::confirm_save`]: the connector the confirmed actions belong
/// to and the draft generation they were snapshotted from, paired with the
/// actions themselves. The host uses THIS identity — never a re-read of the
/// live draft — for self-test invalidation and for the save-completion call,
/// so a completion can only ever land on the draft the user confirmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveConfirmation {
    /// The connector the confirmed actions were captured for.
    pub connector_id: String,
    /// The draft generation the actions were snapshotted from.
    pub generation: u64,
    /// The exact actions to apply, in save order (credential writes before
    /// the config toggle).
    pub actions: Vec<ConnectorAction>,
}

#[derive(Clone, Debug, Default)]
pub struct MessagingData {
    /// The pure list/detail/selection state (fed via `apply_snapshot`).
    pub state: MessagingPaneState,
    /// A fetch is in flight (drives the loading affordance, guards re-entry).
    pub loading: bool,
    /// The last fetch's error, if the platforms call failed.
    pub error: Option<String>,
    /// Whether at least one snapshot has been applied — distinguishes the
    /// "not fetched yet" placeholder from a genuinely empty connector list.
    pub loaded: bool,

    // --- controls lifecycle -----------------------------------------------
    /// Pending unsaved edits for the currently-selected connector. `None` means
    /// no draft is open (the detail view is read-only).
    pub draft: Option<ConnectorDraft>,
    /// A save is in flight (drives the saving affordance, guards re-entry).
    pub saving: bool,
    /// A save confirmation is awaiting the user's decision. `Some(_)` means the
    /// confirmation dialog is open; the draft is left untouched until the user
    /// confirms. This gate sits between clicking Save and applying, and —
    /// alongside [`saving`](Self::saving) — guards against double-fire/duplicate
    /// saves. See [`PendingSave`]: the dialog shows only its names-only
    /// [`ChangePlan`], and Confirm applies the actions captured *here* at open
    /// time, not a re-derivation from the live draft.
    pub confirm: Option<PendingSave>,
    /// The last save's error message, if any. Cleared on next save attempt.
    pub save_error: Option<String>,
    /// Whether the last save succeeded. Set true on success, cleared on the
    /// next user edit or save attempt. The renderer shows a brief "Saved" chip.
    pub save_success: bool,
    /// Which credential key the user is currently editing inline, or `None`
    /// if no inline edit is active. The renderer shows a text input for this
    /// key; typing commits on Enter and cancels on Escape.
    pub editing_key: Option<String>,
    /// Secret-safe, generation-aware self-test outcomes keyed by connector id.
    pub self_tests: SelfTestLog,
    /// Monotonic draft generation: bumped whenever a NEW connector draft is
    /// opened (a different connector than the current one, or from no draft at
    /// all). Reopening the same connector keeps its generation, so the user's
    /// pending edits survive poll refreshes. The number is captured into every
    /// open confirmation and returned with the confirmed actions; a save
    /// completion only clears the draft if its generation still matches, so a
    /// stale completion (from an earlier draft) can never wipe a newer one.
    draft_generation: u64,
}

/// The draft mutations the host may perform. RVS2B-FIX1: the ONLY way host
/// handlers may mutate the draft is through
/// [`MessagingData::edit_draft_for`], which refuses the mutation unless the
/// open draft and the selection agree on the connector's identity — so a
/// click on one connector's controls can never touch another connector's
/// draft (or an in-flight save's draft).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftEdit<'a> {
    /// Toggle the draft's enable/disable state.
    ToggleEnabled,
    /// Begin editing the named credential key inline.
    BeginCredential(&'a str),
    /// Commit a typed value to the currently-edited credential key.
    CommitCredential(&'a str),
    /// Stage an explicit clear/unset for the named key.
    ClearEnv(&'a str),
    /// Drop the pending edit for the named key.
    RevertEnv(&'a str),
    /// Open the save confirmation dialog.
    RequestSave,
}

impl MessagingData {
    /// Open a draft for the selected platform, seeding its enable/disable
    /// baseline from the snapshot. Returns `false` when `p` is `None` (nothing
    /// to draft against). Reopening the same id preserves existing edits.
    ///
    /// S2(b) confirmation identity: while a save is in flight, a *different*
    /// connector may be selected but not drafted — the draft stays read-only
    /// until the in-flight save settles. While a save confirmation is open,
    /// switching to a different connector atomically cancels the confirmation
    /// and the old draft before opening the new one (the old connector's plan
    /// must never be confirmable against the new selection).
    pub fn open_draft(&mut self, p: Option<&MessagingPlatform>) -> bool {
        let Some(platform) = p else { return false };
        // If we already have a draft for this connector, keep it (the user's
        // pending edits survive across poll refreshes).
        let already = self
            .draft
            .as_ref()
            .map(|d| d.id() == platform.id)
            .unwrap_or(false);
        if already {
            return true;
        }
        // While a save is in flight, do not open a different draft: selection
        // is allowed, but the detail stays read-only until the save completes.
        if self.saving {
            return false;
        }
        // A switch while a confirmation is open: cancel the confirmation and
        // drop the old draft atomically, then open the new one.
        self.confirm = None;
        self.draft = Some(ConnectorDraft::for_platform(platform));
        self.draft_generation += 1;
        self.editing_key = None;
        true
    }

    /// Close the draft and drop all pending edits.
    ///
    /// S2(b): the generation is monotonic and is NOT reset here — closing a
    /// draft consumes the old generation so the next newly opened connector
    /// draft always gets a strictly newer one. A reset would let a stale
    /// completion (captured before the close) accidentally match a fresh
    /// draft that happens to reopen the same connector.
    pub fn close_draft(&mut self) {
        self.draft = None;
        self.saving = false;
        self.save_error = None;
        self.save_success = false;
        self.editing_key = None;
        self.confirm = None;
    }

    /// The pending enabled state from the draft, or the platform's snapshot
    /// value when no draft is open.
    ///
    /// RVS2B-FIX1: when a draft is open for a DIFFERENT connector than `p`, the
    /// detail shows the snapshot value of `p` — never another connector's
    /// (possibly in-flight) draft.
    pub fn draft_enabled(&self, p: Option<&MessagingPlatform>) -> bool {
        let draft = self.draft.as_ref();
        if let (Some(d), Some(p)) = (draft, p) {
            if d.id() != p.id {
                return p.enabled;
            }
        }
        draft
            .map(|d| d.enabled())
            .or_else(|| p.map(|p| p.enabled))
            .unwrap_or(false)
    }

    /// True when the draft has pending changes that would be saved.
    pub fn draft_is_dirty(&self) -> bool {
        self.draft.as_ref().map(|d| d.is_dirty()).unwrap_or(false)
    }

    /// The platform whose credential *schema* the open draft edits, looked up by
    /// the draft's own connector id (not `selected_id`, which the caller may not
    /// have set). `None` when no draft is open or its connector left the
    /// snapshot. This is the source of the required-var schema for
    /// [`draft_validation`](Self::draft_validation).
    fn draft_platform(&self) -> Option<&MessagingPlatform> {
        let id = self.draft.as_ref()?.id();
        self.state.platforms.iter().find(|p| p.id == id)
    }

    /// Validate the open draft against its connector's required-credential
    /// schema (see [`ConnectorDraft::validate`]). `None` when no draft is open.
    /// The renderer uses this to explain *why* a Save is refused (which required
    /// credentials are still missing). When the draft's connector is not in the
    /// current snapshot the schema is treated as empty, so validation cannot
    /// spuriously block on vars it can't see. That fallback is a deliberate
    /// best-effort trade-off: if a poll drops the connector while a gapped draft
    /// stays open the gate reverts to permissive (a vanished connector has no
    /// schema to violate) rather than blocking on stale required-var knowledge.
    /// Pinned by `messaging_data_vanished_connector_reverts_to_permissive`.
    pub fn draft_validation(&self) -> Option<DraftValidation> {
        let d = self.draft.as_ref()?;
        let empty: &[MessagingEnvVar] = &[];
        let vars = self
            .draft_platform()
            .map(|p| p.env_vars.as_slice())
            .unwrap_or(empty);
        Some(d.validate(vars))
    }

    /// True when the draft is dirty, no save is in flight, AND the pending state
    /// is savable — a draft that would *enable* a connector while a required
    /// credential is still unset is refused ([`DraftValidation::can_save`]),
    /// because such a connector is doomed to fail its self-test. A connector
    /// left *disabled* with gaps is a legitimate partial save and stays enabled.
    pub fn save_enabled(&self) -> bool {
        self.draft_is_dirty()
            && !self.saving
            && self
                .draft_validation()
                .map(|v| v.can_save())
                .unwrap_or(true)
    }

    /// Toggle the draft's enable/disable state. No-op when no draft is open.
    pub fn toggle_draft_enabled(&mut self) {
        if let Some(d) = self.draft.as_mut() {
            d.toggle_enabled();
            self.save_success = false;
        }
    }

    /// Begin editing a credential key: set `editing_key` so the renderer shows
    /// an inline text input for this key. No-op if the key doesn't match a
    /// known env var, or if another edit is already in progress.
    pub fn begin_credential_edit(&mut self, key: &str) -> bool {
        if self.draft.is_none() || self.editing_key.is_some() {
            return false;
        }
        self.editing_key = Some(key.to_string());
        self.save_success = false;
        true
    }

    /// Commit the typed value to the draft for the currently-edited key. The
    /// value is checked by [`ConnectorDraft::set_env`] — a redaction placeholder
    /// or empty value is rejected and the edit stays open. On success the edit
    /// is closed. Returns whether the value was accepted.
    pub fn commit_credential_edit(&mut self, value: &str) -> bool {
        let Some(d) = self.draft.as_mut() else {
            return false;
        };
        let Some(key) = self.editing_key.take() else {
            return false;
        };
        let accepted = d.set_env(&key, value.to_string());
        if accepted {
            self.save_success = false;
        } else {
            // Value rejected (empty/placeholder) — keep the edit open so the
            // user can try again.
            self.editing_key = Some(key);
        }
        accepted
    }

    /// Cancel the inline credential edit without saving.
    pub fn cancel_credential_edit(&mut self) {
        self.editing_key = None;
    }

    /// Stage an explicit clear/unset for `key` in the draft. Delegates to
    /// [`ConnectorDraft::clear_env`]; no-op when no draft is open.
    pub fn clear_env(&mut self, key: &str) {
        if let Some(d) = self.draft.as_mut() {
            d.clear_env(key);
            self.save_success = false;
        }
    }

    /// Drop any pending edit for `key`, returning it to the untouched state.
    /// Delegates to [`ConnectorDraft::revert_env`]; no-op when no draft is open.
    pub fn revert_env(&mut self, key: &str) {
        if let Some(d) = self.draft.as_mut() {
            d.revert_env(key);
            self.save_success = false;
        }
    }

    /// The pending edit for a credential key in the draft, or `None` if the
    /// user hasn't touched it. Mirrors [`ConnectorDraft::env_edit`] but returns
    /// `None` when no draft is open.
    pub fn draft_env_edit(&self, key: &str) -> Option<&EnvEdit> {
        self.draft.as_ref().and_then(|d| d.env_edit(key))
    }

    /// The pending edit for a credential key, but only when the open draft
    /// belongs to `connector_id` (RVS2B-FIX1): the renderer must never read
    /// one connector's draft edits while displaying another connector's
    /// detail. `None` when no draft is open or it belongs to someone else.
    pub fn draft_env_edit_for(&self, connector_id: &str, key: &str) -> Option<&EnvEdit> {
        if self.draft_id() != Some(connector_id) {
            return None;
        }
        self.draft.as_ref().and_then(|d| d.env_edit(key))
    }

    /// Lower the draft into actions for the host to apply. Returns an empty
    /// vec when no draft is open or the draft is clean. The actions are
    /// ordered: credential writes first, then config toggle.
    pub fn draft_actions(&self) -> Vec<crate::messaging_controls::ConnectorAction> {
        self.draft.as_ref().map(|d| d.actions()).unwrap_or_default()
    }

    /// Connector id owned by the current draft, if one is open.
    pub fn draft_id(&self) -> Option<&str> {
        self.draft.as_ref().map(|d| d.id())
    }

    /// True when the open draft belongs to `connector_id` AND the selection
    /// points at the same connector — the identity gate the guarded edit
    /// entry point ([`edit_draft_for`](Self::edit_draft_for)) enforces.
    pub fn draft_matches(&self, connector_id: &str) -> bool {
        self.draft_id() == Some(connector_id) && self.state.selected_id() == Some(connector_id)
    }

    /// The guarded draft edit entry point (RVS2B-FIX1): the only way host
    /// handlers may mutate the draft. Returns `false` and changes NO state
    /// when the draft and the selection do not both identify
    /// `connector_id` — so a click on a different connector's controls can
    /// never touch another connector's draft (or an in-flight save's draft).
    /// Otherwise delegates to the existing edit method: the env/clear/revert
    /// and toggle paths report success, the credential and save paths report
    /// their own gate.
    pub fn edit_draft_for(&mut self, connector_id: &str, edit: DraftEdit<'_>) -> bool {
        if !self.draft_matches(connector_id) {
            return false;
        }
        match edit {
            DraftEdit::ToggleEnabled => {
                self.toggle_draft_enabled();
                true
            }
            DraftEdit::BeginCredential(key) => self.begin_credential_edit(key),
            DraftEdit::CommitCredential(value) => self.commit_credential_edit(value),
            DraftEdit::ClearEnv(key) => {
                self.clear_env(key);
                true
            }
            DraftEdit::RevertEnv(key) => {
                self.revert_env(key);
                true
            }
            DraftEdit::RequestSave => self.request_save_confirmation(),
        }
    }

    /// Reconcile the draft and the confirmation with the current selection
    /// (RVS2B-FIX1): the single atomic state transition every selection
    /// change goes through. With a selected platform, a stale confirmation
    /// (for a DIFFERENT connector) is cancelled and the selected
    /// connector's draft is opened — or, while a save is in flight, the
    /// in-flight draft is kept and the detail goes read-only. With no
    /// selection, the confirmation is simply cancelled.
    pub fn sync_draft_to_selection(&mut self) {
        let selected_id = self.state.selected_id().map(str::to_owned);
        let Some(selected_id) = selected_id else {
            self.confirm = None;
            return;
        };
        // Cancel a confirmation that belongs to a DIFFERENT connector; the
        // open draft's own confirmation is untouched (open_draft below no-ops
        // for a matching id).
        if self
            .confirm
            .as_ref()
            .is_some_and(|c| c.connector_id != selected_id)
        {
            self.confirm = None;
        }
        // Reuse the draft when it already belongs to the selection (no
        // generation bump, no re-seed); otherwise open a fresh one.
        if self.draft_id().is_none_or(|id| id != selected_id) {
            let selected = self.state.selected_platform().cloned();
            if let Some(p) = selected {
                self.open_draft(Some(&p));
            }
        }
    }

    /// Select a connector by id AND atomically sync the draft/confirmation to
    /// it (RVS2B-FIX1). This is the single state transition every selection
    /// change (row click, snapshot reconciliation) goes through, so a stale
    /// confirmation for another connector can never survive a selection
    /// change.
    pub fn select_id(&mut self, id: &str) {
        self.state.select_id(id);
        self.sync_draft_to_selection();
    }

    /// Keyboard entry point (RVS2-FIX5): move to the next row through the
    /// single selection transition. Cancels any open save confirmation if the
    /// selection changes, then syncs the draft to the new selection.
    pub fn select_next(&mut self) {
        let before = self.state.selected_id().map(str::to_owned);
        self.state.select_next();
        if self.state.selected_id() != before.as_deref() {
            self.confirm = None;
        }
        self.sync_draft_to_selection();
    }

    /// Keyboard entry point (RVS2-FIX5): move to the previous row through the
    /// single selection transition. Cancels any open save confirmation if the
    /// selection changes, then syncs the draft to the new selection.
    pub fn select_prev(&mut self) {
        let before = self.state.selected_id().map(str::to_owned);
        self.state.select_prev();
        if self.state.selected_id() != before.as_deref() {
            self.confirm = None;
        }
        self.sync_draft_to_selection();
    }

    /// Keyboard entry point (RVS2-FIX5): append filter text through the single
    /// selection transition. Cancels any open save confirmation if the
    /// selection changes (e.g. the filter hides the selected row), then syncs.
    pub fn push_query(&mut self, s: &str) {
        let before = self.state.selected_id().map(str::to_owned);
        self.state.push_query(s);
        if self.state.selected_id() != before.as_deref() {
            self.confirm = None;
        }
        self.sync_draft_to_selection();
    }

    /// Keyboard entry point (RVS2-FIX5): delete filter text through the single
    /// selection transition. Cancels any open save confirmation if the
    /// selection changes, then syncs.
    pub fn backspace(&mut self) {
        let before = self.state.selected_id().map(str::to_owned);
        self.state.backspace();
        if self.state.selected_id() != before.as_deref() {
            self.confirm = None;
        }
        self.sync_draft_to_selection();
    }

    /// Reset the save lifecycle for a new save attempt.
    pub fn begin_save(&mut self) {
        self.saving = true;
        self.save_error = None;
        self.save_success = false;
    }

    /// Record a save success. Identity-gated (S2(b)): the completion carries
    /// the `{connector_id, generation}` returned by
    /// [`confirm_save`](Self::confirm_save), and the success state is applied
    /// only when the live draft still matches that identity. A stale
    /// completion (the draft was closed or a newer one opened in the
    /// meantime) is dropped — it never clears a newer draft or flips the save
    /// flags. Returns whether the completion matched.
    pub fn finish_save_success(&mut self, completion: &SaveConfirmation) -> bool {
        if !self.identity_matches(completion) {
            return false;
        }
        self.saving = false;
        self.save_error = None;
        self.save_success = true;
        self.draft = None;
        self.editing_key = None;
        self.confirm = None;
        true
    }

    /// Record a save failure. Identity-gated like
    /// [`finish_save_success`](Self::finish_save_success): a stale completion
    /// is dropped (it may still refresh the list, but it must not clear or
    /// re-flag a newer draft). When the identity matches, the draft stays open
    /// so the user can retry. Returns whether the completion matched.
    pub fn finish_save_error(&mut self, completion: &SaveConfirmation, err: String) -> bool {
        if !self.identity_matches(completion) {
            return false;
        }
        self.saving = false;
        self.save_error = Some(err);
        self.save_success = false;
        true
    }

    /// Whether the given completion still identifies the live draft: the draft
    /// is open, it is the same connector, and its generation is unchanged.
    fn identity_matches(&self, completion: &SaveConfirmation) -> bool {
        let Some(d) = self.draft.as_ref() else {
            return false;
        };
        d.id() == completion.connector_id && self.draft_generation == completion.generation
    }

    /// True while the save confirmation dialog is open awaiting Confirm/Cancel.
    pub fn is_confirming(&self) -> bool {
        self.confirm.is_some()
    }

    /// The secret-safe [`ChangePlan`] being confirmed, for the dialog renderer.
    /// `None` when no confirmation is open. Names only — never a plaintext value.
    pub fn confirm_plan(&self) -> Option<&ChangePlan> {
        self.confirm.as_ref().map(|c| &c.plan)
    }

    /// The connector name for the confirmation dialog header. `None` when no
    /// confirmation is open or the connector left the snapshot at open time.
    pub fn confirm_name(&self) -> Option<&str> {
        self.confirm.as_ref().and_then(|c| c.name.as_deref())
    }

    /// Open the save confirmation: snapshot the draft's secret-safe
    /// [`ChangePlan`] (for the dialog) *and* the exact actions to apply (for
    /// Confirm), leaving the draft untouched. Refused — returns `false` with no
    /// state change — unless the draft is actually savable
    /// ([`save_enabled`](Self::save_enabled)) and neither a save nor a
    /// confirmation is already in flight. That last clause is the double-fire
    /// guard: a second click while the dialog is up is a no-op. Capturing the
    /// actions here (rather than re-deriving them at Confirm time) is what makes
    /// "Confirm" apply exactly the plan the user was shown, even if the draft is
    /// edited while the dialog is open.
    pub fn request_save_confirmation(&mut self) -> bool {
        if self.saving || self.confirm.is_some() || !self.save_enabled() {
            return false;
        }
        let Some(draft) = self.draft.as_ref() else {
            return false;
        };
        let plan = draft.change_plan();
        let actions = draft.actions();
        // A clean draft has nothing to confirm; save_enabled already implies
        // dirty, but keep the invariant explicit so an empty plan/action set can
        // never surface an empty dialog.
        if plan.is_empty() || actions.is_empty() {
            return false;
        }
        // S2b: the dialog needs the connector's name for its header. Look it up
        // by the draft's id; a connector a poll dropped degrades to the bare id
        // (never blocks the confirm gate — that is the save_enabled() job).
        let name = self
            .state
            .platforms
            .iter()
            .find(|p| p.id == draft.id())
            .map(|p| p.name.clone());
        self.confirm = Some(PendingSave {
            connector_id: draft.id().to_string(),
            generation: self.draft_generation,
            plan,
            actions,
            name,
        });
        true
    }

    /// Cancel an open save confirmation. The draft and all its pending edits are
    /// left exactly as they were — Cancel is strictly non-destructive.
    pub fn cancel_save_confirmation(&mut self) {
        self.confirm = None;
    }

    /// Confirm the pending save: close the dialog, enter the in-flight save
    /// state, and return a [`SaveConfirmation`] carrying the connector id, the
    /// draft generation, and the actions snapshotted when the dialog opened
    /// (never a re-derivation from the live draft). Returns `None` with no
    /// state change when no confirmation is open, a save is already running, or
    /// the live draft no longer matches the confirmation's identity — the
    /// guard against a double Confirm firing two saves. RVS2B-FIX1: it also
    /// refuses when the selection points at ANOTHER connector — covering the
    /// keyboard/snapshot paths that change the selection without
    /// [`sync_draft_to_selection`](Self::sync_draft_to_selection) (a `None`
    /// selection is still allowed; see the test-only `savable_draft()`
    /// fixture). The host must use the
    /// returned identity (never a re-read of `draft_id()`) for self-test
    /// invalidation and the save-completion call.
    pub fn confirm_save(&mut self) -> Option<SaveConfirmation> {
        if self.saving {
            return None;
        }
        let pending = self.confirm.take()?;
        if pending.actions.is_empty() {
            return None;
        }
        // Identity gate: a switch/selection change while the dialog was open
        // would have already cancelled it (see `open_draft`), but check
        // explicitly so a confirm can never target a stale draft.
        let draft = self.draft.as_ref()?;
        if draft.id() != pending.connector_id || self.draft_generation != pending.generation {
            return None;
        }
        // RVS2B-FIX1: the selection must not point at ANOTHER connector.
        // (`None` is allowed — the test-only `savable_draft()` fixture has no
        // snapshot; a real selection change goes through
        // `sync_draft_to_selection`, which cancels the confirmation anyway.)
        if let Some(selected) = self.state.selected_id() {
            if selected != pending.connector_id {
                return None;
            }
        }
        self.begin_save();
        Some(SaveConfirmation {
            connector_id: pending.connector_id,
            generation: pending.generation,
            actions: pending.actions,
        })
    }
}

/// Test-only fixture: a minimal [`MessagingPlatform`] with the given id/name and
/// default (disabled, unconfigured) state. Exposed so other modules' tests (e.g.
/// `work::tests`' `connector_save_flow_uses_exact_plan_actions`) can build
/// fixtures without re-spelling the struct literal.
#[cfg(test)]
pub(crate) fn tests_pub_platform(id: &str, name: &str) -> MessagingPlatform {
    MessagingPlatform {
        id: id.to_string(),
        name: name.to_string(),
        description: String::new(),
        docs_url: String::new(),
        enabled: false,
        configured: false,
        gateway_running: false,
        state: String::new(),
        error_code: None,
        error_message: None,
        updated_at: None,
        home_channel: None,
        env_vars: Vec::new(),
    }
}

/// Test-only fixture: a [`MessagingPlatforms`] snapshot envelope. See
/// [`tests_pub_platform`](Self::tests_pub_platform).
#[cfg(test)]
pub(crate) fn tests_pub_snap(platforms: Vec<MessagingPlatform>) -> MessagingPlatforms {
    MessagingPlatforms {
        env_path: "/home/x/.env".to_string(),
        gateway_start_command: "hermes gateway".to_string(),
        platforms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_gateway::rest::MessagingPlatform;

    fn platform(id: &str, name: &str) -> MessagingPlatform {
        MessagingPlatform {
            id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            docs_url: String::new(),
            enabled: false,
            configured: false,
            gateway_running: false,
            state: String::new(),
            error_code: None,
            error_message: None,
            updated_at: None,
            home_channel: None,
            env_vars: Vec::new(),
        }
    }

    fn snap(platforms: Vec<MessagingPlatform>) -> MessagingPlatforms {
        MessagingPlatforms {
            env_path: "/home/x/.env".to_string(),
            gateway_start_command: "hermes gateway".to_string(),
            platforms,
        }
    }

    // --- health classification -------------------------------------------

    #[test]
    fn health_precedence_error_wins_over_everything() {
        let mut p = platform("tg", "Telegram");
        p.enabled = true;
        p.configured = true;
        p.gateway_running = true;
        p.error_code = Some("AUTH".to_string());
        assert_eq!(ConnectorHealth::of(&p), ConnectorHealth::Error);
    }

    #[test]
    fn health_empty_error_string_is_not_an_error() {
        let mut p = platform("tg", "Telegram");
        p.configured = true;
        p.enabled = true;
        p.gateway_running = true;
        p.error_code = Some("   ".to_string());
        p.error_message = Some(String::new());
        assert_eq!(ConnectorHealth::of(&p), ConnectorHealth::Running);
    }

    #[test]
    fn health_not_configured_is_needs_setup() {
        let mut p = platform("d", "Discord");
        p.enabled = true; // enabled but no creds
        assert_eq!(ConnectorHealth::of(&p), ConnectorHealth::NeedsSetup);
    }

    #[test]
    fn health_configured_but_off_is_disabled() {
        let mut p = platform("d", "Discord");
        p.configured = true;
        p.enabled = false;
        assert_eq!(ConnectorHealth::of(&p), ConnectorHealth::Disabled);
    }

    #[test]
    fn health_unconfigured_and_disabled_prefers_needs_setup() {
        // A connector that is BOTH unconfigured AND switched off — the default
        // state of a freshly-listed connector — must read "Needs setup" (the
        // actionable state), not "Disabled". No other test exercises
        // `!configured && !enabled` together, so this pins the
        // NeedsSetup-before-Disabled precedence: swapping the two branches would
        // silently mislabel an untouched connector as Disabled on the pane.
        let p = platform("s", "Slack"); // configured=false, enabled=false, no error
        assert_eq!(ConnectorHealth::of(&p), ConnectorHealth::NeedsSetup);
    }

    #[test]
    fn health_running_and_ready() {
        let mut p = platform("e", "Email");
        p.configured = true;
        p.enabled = true;
        p.gateway_running = true;
        assert_eq!(ConnectorHealth::of(&p), ConnectorHealth::Running);
        p.gateway_running = false;
        assert_eq!(ConnectorHealth::of(&p), ConnectorHealth::Ready);
    }

    // --- snapshot + default selection ------------------------------------

    #[test]
    fn apply_snapshot_selects_first_when_nothing_selected() {
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![platform("a", "A"), platform("b", "B")]));
        assert_eq!(s.selected_platform().map(|p| p.id.as_str()), Some("a"));
        assert_eq!(s.env_path(), "/home/x/.env");
        assert_eq!(s.gateway_start_command(), "hermes gateway");
        assert_eq!(s.selected_index(), Some(0));
    }

    #[test]
    fn empty_snapshot_leaves_no_selection() {
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![]));
        assert!(s.selected_platform().is_none());
        assert_eq!(s.selected_index(), None);
        assert_eq!(s.visible_rows().len(), 0);
    }

    // --- the crown jewel: selection survives the poll --------------------

    #[test]
    fn selection_survives_reordering_poll_by_id() {
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![
            platform("a", "A"),
            platform("b", "B"),
            platform("c", "C"),
        ]));
        s.select_next(); // now on "b"
        assert_eq!(
            s.selected_platform().map(|p| p.id.clone()),
            Some("b".to_string())
        );

        // Poll returns the same connectors reordered + one new one prepended.
        s.apply_snapshot(snap(vec![
            platform("z", "Z"),
            platform("c", "C"),
            platform("b", "B"),
            platform("a", "A"),
        ]));
        // Still on "b", not on whatever now sits at the old index.
        assert_eq!(
            s.selected_platform().map(|p| p.id.clone()),
            Some("b".to_string())
        );
        assert_eq!(s.selected_index(), Some(2));
    }

    #[test]
    fn selection_falls_back_when_selected_connector_disappears() {
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![platform("a", "A"), platform("b", "B")]));
        s.select_id("b");
        assert_eq!(
            s.selected_platform().map(|p| p.id.clone()),
            Some("b".to_string())
        );

        // "b" removed by the next poll → fall back to first visible.
        s.apply_snapshot(snap(vec![platform("a", "A"), platform("c", "C")]));
        assert_eq!(
            s.selected_platform().map(|p| p.id.clone()),
            Some("a".to_string())
        );
    }

    // --- filter ----------------------------------------------------------

    #[test]
    fn filter_is_word_and_substring_over_id_name_description() {
        let mut a = platform("telegram", "Telegram");
        a.description = "chat bot".to_string();
        let mut b = platform("discord", "Discord");
        b.description = "voice chat".to_string();
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![a, b]));

        s.set_query("chat tele");
        let rows = s.visible_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "telegram");
    }

    #[test]
    fn filtering_out_selection_moves_to_first_visible() {
        let mut a = platform("telegram", "Telegram");
        let mut b = platform("discord", "Discord");
        a.description = "one".to_string();
        b.description = "two".to_string();
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![a, b]));
        s.select_id("discord");
        s.set_query("telegram");
        assert_eq!(
            s.selected_platform().map(|p| p.id.clone()),
            Some("telegram".to_string())
        );
    }

    #[test]
    fn empty_query_shows_all() {
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![platform("a", "A"), platform("b", "B")]));
        s.set_query("   ");
        assert_eq!(s.visible_rows().len(), 2);
    }

    // --- navigation ------------------------------------------------------

    #[test]
    fn select_next_prev_wraps_around() {
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![
            platform("a", "A"),
            platform("b", "B"),
            platform("c", "C"),
        ]));
        assert_eq!(s.selected_index(), Some(0));
        s.select_prev(); // wrap to last
        assert_eq!(s.selected_index(), Some(2));
        s.select_next(); // wrap to first
        assert_eq!(s.selected_index(), Some(0));
        s.select_next();
        assert_eq!(s.selected_index(), Some(1));
    }

    #[test]
    fn navigation_recovers_from_stale_selection() {
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![platform("a", "A"), platform("b", "B")]));
        // Force a stale selection id not present in the list, then navigate.
        s.selected_id = Some("gone".to_string());
        s.select_next();
        // Recovery lands on the first visible row, not a panic/no-op.
        assert_eq!(s.selected_index(), Some(0));

        s.selected_id = Some("gone".to_string());
        s.select_prev();
        assert_eq!(s.selected_index(), Some(0));
    }

    #[test]
    fn navigation_on_empty_list_is_a_noop() {
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![]));
        s.select_next();
        s.select_prev();
        assert_eq!(s.selected_index(), None);
        assert!(s.selected_platform().is_none());
    }

    // --- detail + env vars -----------------------------------------------

    #[test]
    fn env_var_view_masks_and_placeholders() {
        let mut p = platform("e", "Email");
        p.env_vars = vec![
            MessagingEnvVar {
                key: "SMTP_HOST".to_string(),
                required: true,
                is_set: true,
                redacted_value: Some("smtp.example.com".to_string()),
                description: "host".to_string(),
                prompt: String::new(),
                help: String::new(),
                url: None,
                is_password: false,
                advanced: false,
            },
            MessagingEnvVar {
                key: "SMTP_PASS".to_string(),
                required: true,
                is_set: true,
                redacted_value: None, // set but no preview → masked
                description: "password".to_string(),
                prompt: String::new(),
                help: String::new(),
                url: None,
                is_password: true,
                advanced: false,
            },
            MessagingEnvVar {
                key: "SMTP_OPT".to_string(),
                required: false,
                is_set: false,
                redacted_value: None,
                description: "optional".to_string(),
                prompt: String::new(),
                help: String::new(),
                url: None,
                is_password: false,
                advanced: true,
            },
        ];
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![p]));
        let vars = s.selected_env_vars();
        assert_eq!(vars.len(), 3);
        assert_eq!(vars[0].display_value, "smtp.example.com");
        assert_eq!(vars[1].display_value, "••••••••");
        assert_eq!(vars[2].display_value, "(not set)");
        assert!(vars[1].is_password);
        assert!(vars[2].advanced);
    }

    #[test]
    fn selected_env_vars_empty_when_no_selection() {
        let s = MessagingPaneState::new();
        assert!(s.selected_env_vars().is_empty());
        assert_eq!(s.selected_health(), None);
    }

    // --- test action target (the [LIVE] hand-off) ------------------------

    #[test]
    fn test_target_only_when_configured() {
        let mut unconfigured = platform("a", "A"); // configured=false
        unconfigured.enabled = true;
        let mut configured = platform("b", "B");
        configured.configured = true;
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![unconfigured, configured]));

        s.select_id("a");
        assert_eq!(s.test_target(), None); // not configured → nothing to test

        s.select_id("b");
        assert_eq!(s.test_target(), Some("b"));
    }

    // --- header roll-up ---------------------------------------------------

    #[test]
    fn health_summary_counts_visible_by_bucket() {
        let mut running = platform("r", "R");
        running.configured = true;
        running.enabled = true;
        running.gateway_running = true;
        let mut ready = platform("y", "Y");
        ready.configured = true;
        ready.enabled = true;
        let mut needs = platform("n", "N"); // configured=false
        needs.enabled = true;
        let mut disabled = platform("d", "D");
        disabled.configured = true;
        let mut err = platform("e", "E");
        err.error_message = Some("boom".to_string());

        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![running, ready, needs, disabled, err]));
        let sum = s.health_summary();
        assert_eq!(sum.total, 5);
        assert_eq!(sum.running, 1);
        assert_eq!(sum.ready, 1);
        assert_eq!(sum.needs_setup, 1);
        assert_eq!(sum.disabled, 1);
        assert_eq!(sum.error, 1);
    }

    #[test]
    fn health_summary_respects_filter() {
        let mut running = platform("keep", "Keep");
        running.configured = true;
        running.enabled = true;
        running.gateway_running = true;
        let mut other = platform("drop", "Drop");
        other.configured = true;
        let mut s = MessagingPaneState::new();
        s.apply_snapshot(snap(vec![running, other]));
        s.set_query("keep");
        let sum = s.health_summary();
        assert_eq!(sum.total, 1);
        assert_eq!(sum.running, 1);
        assert_eq!(sum.disabled, 0);
    }

    #[test]
    fn summary_line_only_lists_nonzero_buckets_and_agrees_in_number() {
        // Everything nominal: just the total, singular noun, no bucket clutter.
        let one = HealthSummary {
            total: 1,
            running: 1,
            ..Default::default()
        };
        assert_eq!(summary_line(&one), "1 connector \u{00b7} 1 running");

        // Mixed: only the non-zero buckets appear, in fixed precedence order,
        // and the total noun is plural. `ready`/`disabled` are zero here, so they
        // must be absent (the guard that makes a clean header when nominal).
        let mixed = HealthSummary {
            total: 5,
            running: 2,
            ready: 0,
            needs_setup: 1,
            disabled: 0,
            error: 1,
        };
        assert_eq!(
            summary_line(&mixed),
            "5 connectors \u{00b7} 2 running \u{00b7} 1 needs setup \u{00b7} 1 error"
        );

        // No connectors at all: total only, plural noun.
        assert_eq!(summary_line(&HealthSummary::default()), "0 connectors");
    }

    #[test]
    fn summary_line_lists_all_nonzero_buckets_in_fixed_precedence_order() {
        // Every bucket non-zero with DISTINCT counts pins the COMPLETE header
        // precedence — running · ready · needs setup · disabled · error. The
        // mixed-case test above zeroes `ready`/`disabled`, so their position
        // relative to their neighbours is unconstrained there: swapping two
        // adjacent bucket blocks in `summary_line` would survive it. Distinct
        // counts make any block reordering observable in the rendered numbers.
        let all = HealthSummary {
            total: 15, // each connector is in exactly one bucket: 1+2+3+4+5
            running: 1,
            ready: 2,
            needs_setup: 3,
            disabled: 4,
            error: 5,
        };
        assert_eq!(
            summary_line(&all),
            "15 connectors \u{00b7} 1 running \u{00b7} 2 ready \u{00b7} 3 needs setup \u{00b7} 4 disabled \u{00b7} 5 error"
        );
    }

    // --- keyboard mapping (hosted-pane capture handler) ------------------

    #[test]
    fn messaging_key_maps_printables_nav_and_edits() {
        // Printable characters (and the named "space") append to the filter.
        assert_eq!(
            messaging_key("a", false),
            MessagingKey::Append("a".to_string())
        );
        assert_eq!(
            messaging_key("space", false),
            MessagingKey::Append(" ".to_string())
        );
        // Backspace/Delete delete; arrows navigate the list.
        assert_eq!(messaging_key("backspace", false), MessagingKey::Backspace);
        assert_eq!(messaging_key("delete", false), MessagingKey::Backspace);
        assert_eq!(messaging_key("down", false), MessagingKey::SelectNext);
        assert_eq!(messaging_key("up", false), MessagingKey::SelectPrev);
        // Multi-char named keys the pane does not consume fall through.
        assert_eq!(messaging_key("enter", false), MessagingKey::Ignore);
        assert_eq!(messaging_key("escape", false), MessagingKey::Ignore);
    }

    #[test]
    fn messaging_key_ignores_all_chords() {
        // Any modifier chord is Ignore so the dispatcher lets ⌘K/⌘N/… reach the
        // global handler — the pane must never swallow a real navigation chord.
        // Even keys that are meaningful unshifted (a printable, an arrow) yield
        // Ignore under a chord.
        assert_eq!(messaging_key("a", true), MessagingKey::Ignore);
        assert_eq!(messaging_key("down", true), MessagingKey::Ignore);
        assert_eq!(messaging_key("backspace", true), MessagingKey::Ignore);
    }

    #[test]
    fn push_and_backspace_edit_the_filter_and_keep_selection_valid() {
        let mut st = MessagingPaneState::default();
        st.apply_snapshot(snap(vec![
            platform("telegram", "Telegram"),
            platform("discord", "Discord"),
        ]));
        // Nothing selected? apply_snapshot defaults to the first visible row.
        assert_eq!(
            st.selected_platform().map(|p| p.id.as_str()),
            Some("telegram")
        );

        // Typing "disc" narrows to Discord; the previously-selected Telegram is
        // filtered out, so selection moves to the first still-visible row.
        for c in ["d", "i", "s", "c"] {
            st.push_query(c);
        }
        assert_eq!(st.query(), "disc");
        assert_eq!(st.visible_rows().len(), 1);
        assert_eq!(
            st.selected_platform().map(|p| p.id.as_str()),
            Some("discord")
        );

        // Backspacing widens the filter back out; both rows are visible again and
        // the Discord selection survives (widening only reveals rows).
        for _ in 0..4 {
            st.backspace();
        }
        assert_eq!(st.query(), "");
        assert_eq!(st.visible_rows().len(), 2);
        assert_eq!(
            st.selected_platform().map(|p| p.id.as_str()),
            Some("discord")
        );
        // Backspace on an empty query is a harmless no-op.
        st.backspace();
        assert_eq!(st.query(), "");
    }

    // --- MessagingData draft lifecycle ------------------------------------

    #[test]
    fn messaging_data_open_draft_mirrors_selected_platform() {
        let p = platform("tg", "Telegram");
        let mut d = MessagingData::default();
        assert!(d.draft.is_none());

        // Open a draft for the platform — baseline mirrors snapshot.
        assert!(d.open_draft(Some(&p)));
        let draft = d.draft.as_ref().expect("draft opened");
        assert_eq!(draft.id(), "tg");
        assert_eq!(draft.enabled(), p.enabled);
        assert!(!draft.is_dirty());
    }

    #[test]
    fn messaging_data_open_draft_reuses_existing_for_same_id() {
        let p = platform("tg", "Telegram");
        let mut d = MessagingData::default();
        d.open_draft(Some(&p));
        let id_before = d.draft.as_ref().map(|d| d.id().to_string());

        // Re-opening the same platform keeps the existing draft.
        assert!(d.open_draft(Some(&p)));
        assert_eq!(d.draft.as_ref().map(|d| d.id().to_string()), id_before);
    }

    #[test]
    fn messaging_data_open_draft_requires_a_platform() {
        let mut d = MessagingData::default();
        assert!(!d.open_draft(None));
        assert!(d.draft.is_none());
    }

    #[test]
    fn messaging_data_close_draft_clears_everything() {
        let p = platform("tg", "Telegram");
        let mut d = MessagingData::default();
        d.open_draft(Some(&p));
        d.saving = true;
        d.save_error = Some("test".to_string());
        d.save_success = true;
        d.editing_key = Some("K".to_string());

        d.close_draft();
        assert!(d.draft.is_none());
        assert!(!d.saving);
        assert!(d.save_error.is_none());
        assert!(!d.save_success);
        assert!(d.editing_key.is_none());
    }

    #[test]
    fn messaging_data_draft_enabled_falls_back_to_platform() {
        let p = platform("tg", "Telegram"); // enabled=false by default
        let mut d = MessagingData::default();
        // No draft open: falls back to platform value.
        assert!(!d.draft_enabled(Some(&p)));
        // With draft open: uses draft value.
        d.open_draft(Some(&p));
        assert!(!d.draft_enabled(Some(&p)));
        d.toggle_draft_enabled();
        assert!(d.draft_enabled(Some(&p)));
    }

    #[test]
    fn messaging_data_toggle_draft_enabled_and_dirty() {
        let p = platform("tg", "Telegram");
        let mut d = MessagingData::default();
        d.open_draft(Some(&p));
        assert!(!d.draft_is_dirty());

        d.toggle_draft_enabled();
        assert!(d.draft_is_dirty());
        assert!(d.save_enabled());

        // Toggle back to baseline → clean.
        d.toggle_draft_enabled();
        assert!(!d.draft_is_dirty());
    }

    #[test]
    fn messaging_data_credential_edit_isolation() {
        let mut p = platform("e", "Email");
        p.env_vars = vec![
            MessagingEnvVar {
                key: "SMTP_HOST".to_string(),
                required: true,
                is_set: true,
                redacted_value: Some("smtp.example.com".to_string()),
                description: "host".to_string(),
                prompt: String::new(),
                help: String::new(),
                url: None,
                is_password: false,
                advanced: false,
            },
            MessagingEnvVar {
                key: "SMTP_PASS".to_string(),
                required: true,
                is_set: true,
                redacted_value: None,
                description: "password".to_string(),
                prompt: String::new(),
                help: String::new(),
                url: None,
                is_password: true,
                advanced: false,
            },
        ];
        let mut d = MessagingData::default();
        d.open_draft(Some(&p));

        // Begin editing SMTP_HOST.
        assert!(d.begin_credential_edit("SMTP_HOST"));
        assert_eq!(d.editing_key.as_deref(), Some("SMTP_HOST"));
        // Can't begin a second edit while one is active.
        assert!(!d.begin_credential_edit("SMTP_PASS"));
        assert_eq!(d.editing_key.as_deref(), Some("SMTP_HOST"));

        // Commit a valid value — edit closes.
        assert!(d.commit_credential_edit("new-host.com"));
        assert!(d.editing_key.is_none());
        assert_eq!(
            d.draft_env_edit("SMTP_HOST"),
            Some(&EnvEdit::Set("new-host.com".to_string()))
        );
        assert!(d.draft_is_dirty());

        // Commit with a placeholder is rejected and keeps the edit open.
        d.begin_credential_edit("SMTP_PASS");
        assert_eq!(d.editing_key.as_deref(), Some("SMTP_PASS"));
        assert!(!d.commit_credential_edit("••••••••")); // rejected
        assert_eq!(d.editing_key.as_deref(), Some("SMTP_PASS"));

        // Cancel clears editing_key without changing the draft.
        d.cancel_credential_edit();
        assert!(d.editing_key.is_none());
        assert_eq!(d.draft_env_edit("SMTP_PASS"), None);
    }

    #[test]
    fn messaging_data_credential_edit_no_draft_is_noop() {
        let mut d = MessagingData::default();
        assert!(!d.begin_credential_edit("K"));
        assert!(!d.commit_credential_edit("v"));
        assert!(d.editing_key.is_none());
    }

    #[test]
    fn messaging_data_save_lifecycle() {
        let p = platform("tg", "Telegram");
        let mut d = MessagingData::default();
        d.open_draft(Some(&p));
        d.toggle_draft_enabled();

        d.begin_save();
        assert!(d.saving);
        assert!(d.save_error.is_none());
        assert!(!d.save_success);
        assert!(!d.save_enabled()); // saving guards re-entry

        // Success: clears draft, shows success. The completion identity is the
        // live draft's own {connector_id, generation}, so it matches.
        let done = SaveConfirmation {
            connector_id: "tg".to_string(),
            generation: d.draft_generation,
            actions: Vec::new(),
        };
        assert!(d.finish_save_success(&done));
        assert!(!d.saving);
        assert!(!d.draft_is_dirty());
        assert!(d.draft.is_none());
        assert!(d.save_success);

        // Open draft again, save error: keeps draft open.
        d.open_draft(Some(&p));
        d.toggle_draft_enabled();
        d.begin_save();
        let failed = SaveConfirmation {
            connector_id: "tg".to_string(),
            generation: d.draft_generation,
            actions: Vec::new(),
        };
        assert!(d.finish_save_error(&failed, "network error".to_string()));
        assert!(!d.saving);
        assert_eq!(d.save_error.as_deref(), Some("network error"));
        assert!(!d.save_success);
        assert!(d.draft_is_dirty()); // draft still open, user can retry
    }

    // A required credential var, `is_set` controlled by the caller.
    fn req_var(key: &str, is_set: bool) -> MessagingEnvVar {
        MessagingEnvVar {
            key: key.to_string(),
            required: true,
            is_set,
            redacted_value: if is_set { Some("x".to_string()) } else { None },
            description: String::new(),
            prompt: String::new(),
            help: String::new(),
            url: None,
            is_password: true,
            advanced: false,
        }
    }

    #[test]
    fn messaging_data_save_blocked_when_enabling_with_missing_required() {
        let mut p = platform("tg", "Telegram");
        p.env_vars = vec![req_var("TELEGRAM_BOT_TOKEN", false)];
        let mut d = MessagingData::default();
        // The connector must be in the snapshot so its required-var schema is
        // visible to validation.
        d.state.apply_snapshot(snap(vec![p.clone()]));
        d.open_draft(Some(&p));

        // Enabling a connector whose required credential is still unset.
        d.toggle_draft_enabled();
        assert!(d.draft_is_dirty(), "toggle should dirty the draft");

        // Save must be refused: the connector would be doomed to fail its test.
        assert!(
            !d.save_enabled(),
            "save must be blocked with a missing required credential"
        );
        let v = d.draft_validation().expect("draft is open");
        assert!(v.blocks_enable());
        assert_eq!(v.missing_required(), ["TELEGRAM_BOT_TOKEN"]);
    }

    #[test]
    fn messaging_data_save_allowed_once_required_credential_supplied() {
        let mut p = platform("tg", "Telegram");
        p.env_vars = vec![req_var("TELEGRAM_BOT_TOKEN", false)];
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![p.clone()]));
        d.open_draft(Some(&p));
        d.toggle_draft_enabled();
        assert!(!d.save_enabled());

        // Supplying the required credential clears the block.
        d.begin_credential_edit("TELEGRAM_BOT_TOKEN");
        assert!(d.commit_credential_edit("123:abc"));
        assert!(
            d.save_enabled(),
            "save allowed once the required credential is set"
        );
        assert!(d.draft_validation().expect("open").can_save());
    }

    #[test]
    fn messaging_data_disabled_draft_with_gap_is_savable() {
        // A connector left *disabled* may be saved even with a required
        // credential still unset — that is a legitimate partial save.
        let mut p = platform("tg", "Telegram");
        p.enabled = true;
        p.env_vars = vec![req_var("TELEGRAM_BOT_TOKEN", false)];
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![p.clone()]));
        d.open_draft(Some(&p));

        // Toggle from enabled → disabled: dirty, and savable despite the gap.
        d.toggle_draft_enabled();
        assert!(d.draft_is_dirty());
        assert!(
            d.save_enabled(),
            "disabling a connector with gaps is a valid save"
        );
        let v = d.draft_validation().expect("open");
        assert!(!v.blocks_enable());
        assert!(v.can_save());
    }

    #[test]
    fn messaging_data_validation_none_when_no_draft() {
        let d = MessagingData::default();
        assert!(d.draft_validation().is_none());
    }

    #[test]
    fn messaging_data_vanished_connector_reverts_to_permissive() {
        // Pins the deliberate best-effort fallback: while a gapped, would-enable
        // draft is open, if a poll drops the connector from the snapshot the gate
        // reverts to permissive (no schema to violate) rather than blocking on
        // stale required-var knowledge.
        let mut p = platform("tg", "Telegram");
        p.env_vars = vec![req_var("TELEGRAM_BOT_TOKEN", false)];
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![p.clone()]));
        d.open_draft(Some(&p));
        d.toggle_draft_enabled();
        assert!(
            !d.save_enabled(),
            "blocked while the connector's schema is visible"
        );

        // A later poll no longer lists this connector.
        d.state
            .apply_snapshot(snap(vec![platform("other", "Other")]));
        assert!(d.draft.is_some(), "draft survives the poll");
        assert!(
            d.save_enabled(),
            "gate reverts to permissive for a connector no longer in the snapshot"
        );
        assert!(d.draft_validation().expect("open").can_save());
    }

    #[test]
    fn messaging_data_draft_actions_empty_when_no_draft() {
        let d = MessagingData::default();
        assert!(d.draft_actions().is_empty());
    }

    #[test]
    fn messaging_data_draft_actions_lower_dirty_draft() {
        let p = platform("tg", "Telegram");
        let mut d = MessagingData::default();
        d.open_draft(Some(&p));
        d.toggle_draft_enabled();
        let acts = d.draft_actions();
        assert_eq!(acts.len(), 1);
        // The single action is UpdateConfig (no env edits).
        match &acts[0] {
            crate::messaging_controls::ConnectorAction::UpdateConfig { id, .. } => {
                assert_eq!(id, "tg");
            }
            _ => panic!("expected UpdateConfig"),
        }
    }

    // --- save-confirmation gate -------------------------------------------

    /// Open a dirty, savable draft (enable toggle only) ready to confirm.
    fn savable_draft() -> MessagingData {
        let p = platform("tg", "Telegram");
        let mut d = MessagingData::default();
        d.open_draft(Some(&p));
        d.toggle_draft_enabled();
        assert!(d.save_enabled(), "fixture must be savable");
        d
    }

    #[test]
    fn request_save_confirmation_opens_dialog_without_touching_draft() {
        let mut d = savable_draft();
        assert!(!d.is_confirming());
        assert!(
            d.request_save_confirmation(),
            "savable draft opens the dialog"
        );
        assert!(d.is_confirming());
        // The draft is untouched: still dirty, not yet saving.
        assert!(d.draft_is_dirty());
        assert!(!d.saving);
        // The plan is the draft's secret-safe change plan (enable toggle).
        let plan = d.confirm_plan().expect("plan present while confirming");
        assert!(!plan.is_empty());
        assert_eq!(plan.change_count(), 1);
    }

    #[test]
    fn request_save_confirmation_refused_when_not_savable() {
        // Enabling a connector with a missing required credential is not savable,
        // so it must never open a confirmation either.
        let mut p = platform("tg", "Telegram");
        p.env_vars = vec![req_var("TELEGRAM_BOT_TOKEN", false)];
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![p.clone()]));
        d.open_draft(Some(&p));
        d.toggle_draft_enabled();
        assert!(!d.save_enabled());
        assert!(
            !d.request_save_confirmation(),
            "must refuse an unsavable draft"
        );
        assert!(!d.is_confirming());
    }

    #[test]
    fn save_confirmation_captures_name_at_open_time() {
        let p = platform("tg", "Telegram");
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![p.clone()]));
        d.open_draft(Some(&p));
        d.toggle_draft_enabled();
        assert!(d.request_save_confirmation());
        // The header name is the connector's snapshot name at open time.
        assert_eq!(d.confirm_name(), Some("Telegram"));

        // A poll refresh (same connector, unchanged) does not leak a re-read:
        // the captured name stands until the confirmation closes.
        d.state.apply_snapshot(snap(vec![p.clone()]));
        assert_eq!(d.confirm_name(), Some("Telegram"));
        d.cancel_save_confirmation();
        assert_eq!(d.confirm_name(), None);
    }

    #[test]
    fn save_confirmation_name_falls_back_when_connector_vanishes_at_open() {
        // A connector absent from the snapshot at open time: the gate still
        // opens (no schema visible) and the name degrades to None — the header
        // falls back to the bare headline.
        let p = platform("tg", "Telegram");
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![p.clone()]));
        d.open_draft(Some(&p));
        d.toggle_draft_enabled();
        // The poll drops the connector before the dialog opens.
        d.state
            .apply_snapshot(snap(vec![platform("other", "Other")]));
        assert!(d.request_save_confirmation());
        assert_eq!(d.confirm_name(), None);
    }

    #[test]
    fn request_save_confirmation_is_a_noop_second_time_double_fire_guard() {
        let mut d = savable_draft();
        assert!(d.request_save_confirmation());
        // A second click while the dialog is already up is a no-op — the plan
        // must not be recomputed or the dialog re-opened.
        assert!(
            !d.request_save_confirmation(),
            "double-fire while confirming is refused"
        );
        assert!(d.is_confirming());
    }

    #[test]
    fn cancel_save_confirmation_leaves_draft_untouched() {
        let mut d = savable_draft();
        d.request_save_confirmation();
        d.cancel_save_confirmation();
        assert!(!d.is_confirming());
        assert!(d.confirm_plan().is_none());
        // Cancel is non-destructive: the draft and its dirty edits survive.
        assert!(d.draft_is_dirty(), "Cancel must not drop the draft");
        assert!(!d.saving);
        assert!(
            d.save_enabled(),
            "the draft can still be saved after Cancel"
        );
    }

    #[test]
    fn confirm_save_enters_in_flight_and_returns_actions() {
        let mut d = savable_draft();
        d.request_save_confirmation();
        let Some(conf) = d.confirm_save() else {
            panic!("confirm must return the identity + actions");
        };
        assert_eq!(
            conf.actions.len(),
            1,
            "the enable toggle lowers to one action"
        );
        // S2(b): the confirmation carries the connector identity and the
        // draft's generation at open time.
        assert_eq!(conf.connector_id, "tg");
        assert!(
            conf.generation > 0,
            "a newly opened draft has a non-zero generation"
        );
        // Confirming closes the dialog and enters the in-flight save state.
        assert!(!d.is_confirming());
        assert!(d.saving, "confirm_save begins the save");
        assert!(d.save_error.is_none());
    }

    #[test]
    fn confirm_save_without_open_confirmation_is_a_noop() {
        let mut d = savable_draft();
        // No request_save_confirmation() first.
        assert!(
            d.confirm_save().is_none(),
            "confirm with no dialog open yields no actions"
        );
        assert!(!d.saving, "and must not enter the save state");
    }

    #[test]
    fn confirm_save_twice_only_fires_once_double_fire_guard() {
        let mut d = savable_draft();
        d.request_save_confirmation();
        let first = d.confirm_save();
        assert_eq!(first.as_ref().map(|c| c.actions.len()), Some(1));
        // A second Confirm (e.g. a double click landing after the first) must
        // not fire a second save: the dialog is closed and a save is in flight.
        assert!(d.confirm_save().is_none(), "second confirm must be a no-op");
        assert!(d.saving);
    }

    #[test]
    fn confirm_applies_the_snapshot_not_edits_made_after_opening() {
        // The dialog shows a plan captured at open time. If the user edits the
        // draft while it is open, Confirm must apply what they were shown — the
        // snapshot — not the mutated draft state.
        let mut p = platform("tg", "Telegram");
        p.env_vars = vec![req_var("TELEGRAM_BOT_TOKEN", true)];
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![p.clone()]));
        d.open_draft(Some(&p));
        d.toggle_draft_enabled();
        assert!(d.request_save_confirmation());
        let shown = d.confirm_plan().expect("plan shown").clone();
        assert_eq!(
            shown.change_count(),
            1,
            "only the enable toggle at open time"
        );

        // User keeps typing underneath the open dialog: stage a credential edit.
        d.begin_credential_edit("TELEGRAM_BOT_TOKEN");
        assert!(d.commit_credential_edit("999:zzz"));

        // Confirm applies exactly the snapshot (one action: the toggle), not the
        // now-two-change live draft.
        let conf = d.confirm_save().expect("confirm applies the snapshot");
        assert_eq!(
            conf.actions.len(),
            1,
            "Confirm applies the snapshot, not later edits"
        );
        match &conf.actions[0] {
            ConnectorAction::UpdateConfig { id, .. } => assert_eq!(id, "tg"),
            other => panic!("expected the snapshotted UpdateConfig, got {other:?}"),
        }
    }

    #[test]
    fn finish_save_success_clears_any_confirmation() {
        let mut d = savable_draft();
        d.request_save_confirmation();
        let conf = d.confirm_save().expect("confirm returns the identity");
        assert!(
            d.finish_save_success(&conf),
            "a matching completion applies the success"
        );
        assert!(!d.is_confirming());
        assert!(d.confirm_plan().is_none());
        assert!(d.save_success);
    }

    #[test]
    fn close_draft_clears_pending_confirmation() {
        let mut d = savable_draft();
        d.request_save_confirmation();
        assert!(d.is_confirming());
        d.close_draft();
        assert!(
            !d.is_confirming(),
            "closing the draft drops the confirmation"
        );
        assert!(d.draft.is_none());
    }

    // --- S2(b): confirmation identity (generation-gated) --------------------

    /// Fixture: a connector with one required credential so a draft can stage
    /// a SetEnv action, in the snapshot.
    fn env_platform(id: &str, name: &str) -> MessagingPlatform {
        let mut p = platform(id, name);
        p.configured = true;
        p.enabled = true;
        p.env_vars = vec![req_var("TG_BOT_TOKEN", true)];
        p
    }

    /// Fixture: a connector with two required credentials (one unset, one
    /// set) so a draft can stage BOTH a SetEnv and a ClearEnv action.
    fn two_cred_platform(
        id: &str,
        name: &str,
        set_key: &str,
        clear_key: &str,
    ) -> MessagingPlatform {
        let mut p = platform(id, name);
        p.enabled = true;
        p.configured = true;
        p.env_vars = vec![req_var(set_key, false), req_var(clear_key, true)];
        p
    }

    /// S2(b) required proof: switching the selection while a confirmation is
    /// open atomically cancels the confirmation and the old draft, then opens
    /// the new connector's draft. Confirming afterwards emits NEITHER the old
    /// connector's actions nor the new one's (no new confirmation exists).
    #[test]
    fn connector_switch_before_confirm_cancels_old_plan() {
        let a = env_platform("tg", "Telegram");
        let b = env_platform("wh", "WhatsApp");
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![a.clone(), b.clone()]));
        d.state.select_id("tg");
        d.open_draft(Some(&a));
        d.toggle_draft_enabled();
        assert!(d.request_save_confirmation());
        let plan = d.confirm_plan().expect("A's plan while confirming");
        let plan_changes = plan.change_count();

        // Switch the selection to B (row click / arrow keys): the draft is
        // auto-opened for the new connector on the next render.
        d.state.select_id("wh");
        d.open_draft(Some(&b));

        assert!(
            !d.is_confirming(),
            "A's confirmation is cancelled on switch"
        );
        assert_eq!(d.draft_id(), Some("wh"), "B's draft is open");
        // A's plan and its edits are gone.
        assert!(d.confirm_plan().is_none());
        assert_eq!(plan_changes, 1);

        // No new confirmation exists for B, so Confirm emits no actions —
        // not A's, not B's.
        assert!(
            d.confirm_save().is_none(),
            "Confirm with no open confirmation dispatches nothing"
        );
        assert!(!d.saving);
        // B's fresh draft is intact and savable once dirtied.
        assert_eq!(d.draft_id(), Some("wh"));
        d.toggle_draft_enabled();
        assert!(d.draft_is_dirty(), "B's fresh draft accepts edits");
        assert!(
            d.request_save_confirmation(),
            "B's own confirmation opens cleanly after the switch"
        );
    }

    /// RVS2B-FIX1 required proof: `MessagingData::select_id` is the ONE state
    /// transition every selection change goes through (row click, snapshot
    /// reconciliation) — and it atomically cancels a stale open confirmation
    /// for another connector, closing the draft and opening the new
    /// connector's. No confirmable state remains.
    #[test]
    fn selection_change_cancels_open_confirmation_atomically() {
        let a = env_platform("tg", "Telegram");
        let b = env_platform("wh", "WhatsApp");
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![a.clone(), b.clone()]));
        d.state.select_id("tg");
        d.open_draft(Some(&a));
        d.toggle_draft_enabled();
        assert!(d.request_save_confirmation(), "A's confirmation opens");
        let gen_a = d.draft_generation;

        // The ONLY row-click path: one call, no render, no `open_draft`.
        d.select_id("wh");

        assert!(!d.is_confirming(), "the stale confirmation is cancelled");
        assert_eq!(
            d.draft_id(),
            Some("wh"),
            "the selection's draft is open in the same transition"
        );
        assert!(
            d.draft_generation > gen_a,
            "switching connectors opens B's draft at a strictly newer generation"
        );
        assert!(
            !d.draft_is_dirty(),
            "A's edits did not leak into B's fresh draft"
        );
        assert!(
            d.confirm_plan().is_none(),
            "no plan is surfaced for the stale confirmation"
        );
        assert!(d.confirm_save().is_none(), "Confirm dispatches nothing");
        assert!(!d.saving);

        // RVS2B-FIX3: return to A WITHOUT closing B's draft first — the
        // round trip happens with B's fresh draft still open. The atomic
        // transition cancels the OLD A confirmation (it already is gone) and
        // replaces B's draft with a fresh A one at a newer generation; only a
        // FRESHLY created A confirmation can dispatch.
        let gen_b = d.draft_generation;
        d.select_id("tg");
        assert_eq!(d.draft_id(), Some("tg"), "reselecting A reopens A's draft");
        assert!(
            !d.draft_is_dirty(),
            "the old A draft's edits did not survive the round trip"
        );
        assert!(
            d.draft_generation > gen_b,
            "the round trip opens A's draft at a strictly newer generation"
        );
        // The OLD A confirmation (staged before the switch) was cancelled by
        // the selection change and is gone for good: no stale confirm state
        // survives the round trip.
        assert!(
            !d.is_confirming(),
            "the old A confirmation is not resurrected"
        );
        d.toggle_draft_enabled();
        assert!(
            d.request_save_confirmation(),
            "a fresh A confirmation opens after reselecting A"
        );
        let fresh = d
            .confirm_save()
            .expect("only the freshly created A confirmation dispatches");
        assert_eq!(fresh.connector_id, "tg");
        assert_eq!(
            fresh.actions,
            vec![ConnectorAction::UpdateConfig {
                id: "tg".to_string(),
                body: serde_json::json!({ "enabled": false }),
            }],
            "the fresh confirmation carries only the re-staged toggle"
        );
    }

    /// RVS2B-FIX1 required proof: `confirm_save` refuses when the selection
    /// moved away WITHOUT going through `sync_draft_to_selection` — the
    /// keyboard path (`select_next` on the pane state) and snapshot
    /// reconciliation that change the selection without the sync. The
    /// confirmation stays cancelled and nothing is saved.
    #[test]
    fn confirm_rejected_when_selection_moved_without_sync() {
        let a = env_platform("tg", "Telegram");
        let b = env_platform("wh", "WhatsApp");
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![a.clone(), b.clone()]));
        d.state.select_id("tg");
        d.open_draft(Some(&a));
        d.toggle_draft_enabled();
        assert!(d.request_save_confirmation(), "A's confirmation opens");

        // The keyboard path: moves the selection, never syncs the draft.
        d.state.select_next();
        assert_eq!(
            d.state.selected_id(),
            Some("wh"),
            "the keyboard path moved the selection to B"
        );

        assert!(
            d.confirm_save().is_none(),
            "a Confirm while B is selected must not dispatch A's actions"
        );
        assert!(
            d.confirm_save().is_none(),
            "the confirmation stayed cancelled — no retry possible"
        );
        assert!(!d.saving);
        assert_eq!(d.draft_id(), Some("tg"), "A's draft is untouched");

        // RVS2B-FIX2: reselecting A (through the atomic transition) lets ONLY
        // a FRESH A confirmation dispatch — the old A confirmation from before
        // the switch was cancelled by it and is refused (above and by every
        // retry): the only dispatchable confirmation is the one opened now.
        d.close_draft();
        d.select_id("tg");
        d.toggle_draft_enabled();
        assert!(
            d.request_save_confirmation(),
            "the fresh A confirmation opens after reselecting A"
        );
        let fresh = d
            .confirm_save()
            .expect("only the fresh A confirmation dispatches");
        assert_eq!(fresh.connector_id, "tg");
        assert_eq!(
            fresh.actions,
            vec![ConnectorAction::UpdateConfig {
                id: "tg".to_string(),
                body: serde_json::json!({ "enabled": false }),
            }],
            "the fresh confirmation carries only the re-staged toggle"
        );
    }

    /// RVS2B-FIX2 required proof (strengthened from the FIX1 in-flight
    /// proof): A's in-flight save stages the EXACT three-action set — SetEnv,
    /// ClearEnv and UpdateConfig over DISTINCT keys/values — and B's snapshot
    /// value differs from A's draft, so an implementation that leaks A's
    /// draft into B's detail would flip the rendered enabled state and fail.
    /// For BOTH stale control identities (B, the selection, and A, the
    /// in-flight draft), EVERY edit variant is refused; A's draft is
    /// byte-for-byte unchanged; and once A's save settles, B becomes
    /// editable.
    #[test]
    fn in_flight_save_makes_other_selection_read_only() {
        // RVS2B-FIX3: TWO credential fixtures. A carries TWO env keys that are
        // BOTH present in its snapshot; B carries its own different key. A's
        // staged ClearEnv targets `A_SECRET` — a key that IS present in A's
        // fixture — and the proof asserts that it is cleared ONLY for A.
        let a = two_set_platform("tg", "Telegram", "A_TOKEN", "A_SECRET");
        let mut b = two_set_platform("wh", "WhatsApp", "WH_API_KEY", "WH_CLIENT_ID");
        b.enabled = true; // snapshot says B is enabled (distinct from A's draft)
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![a.clone(), b.clone()]));
        d.state.select_id("tg");
        d.open_draft(Some(&a));

        // Stage A's EXACT three-action set through the guarded entry point:
        // set A_TOKEN, clear A_SECRET (a key that is PRESENT in A's fixture),
        // and flip A's config toggle.
        assert!(
            d.edit_draft_for("tg", DraftEdit::BeginCredential("A_TOKEN")),
            "begin A's credential edit"
        );
        assert!(
            d.edit_draft_for("tg", DraftEdit::CommitCredential("123:abc")),
            "commit A's credential value"
        );
        assert!(
            d.edit_draft_for("tg", DraftEdit::ClearEnv("A_SECRET")),
            "stage A's explicit clear of a key present in A's fixture"
        );
        assert!(
            d.edit_draft_for("tg", DraftEdit::ToggleEnabled),
            "stage A's config toggle"
        );
        assert!(
            d.edit_draft_for("tg", DraftEdit::RequestSave),
            "A's save confirmation opens"
        );
        let conf = d.confirm_save().expect("A's confirmation");
        assert!(d.saving);
        // The exact ordered plan: ClearEnv (A_SECRET), SetEnv (A_TOKEN), then
        // the config toggle — env edits sorted by key, config last. Every key
        // and value is A's: the plan is never B's key or B's id.
        assert_eq!(
            conf.actions,
            vec![
                ConnectorAction::ClearEnv {
                    key: "A_SECRET".to_string(),
                },
                ConnectorAction::SetEnv {
                    key: "A_TOKEN".to_string(),
                    value: "123:abc".to_string(),
                },
                ConnectorAction::UpdateConfig {
                    id: "tg".to_string(),
                    body: serde_json::json!({ "enabled": false }),
                },
            ],
            "A's in-flight actions are the exact staged three-action set"
        );
        // The staged clear is scoped to A's connector: nowhere in the plan is
        // B's key cleared, and B's own key is not touched at all.
        assert!(
            conf.actions.iter().all(
                |act| !matches!(act, ConnectorAction::ClearEnv { key } if key == "WH_API_KEY")
            ),
            "the staged clear is scoped to A — B's key is never cleared"
        );
        // B's snapshot says ENABLED while A's draft has disabled A — the
        // snapshot value DIFFERS from A's draft, so an implementation that
        // leaks A's draft into B's detail would show `false` here and fail.
        let before = d.draft_actions();

        d.select_id("wh"); // selection allowed; draft stays A's

        assert!(!d.draft_matches("wh"), "B has no draft of its own");
        // B's detail renders B's SNAPSHOT value — distinct from A's draft —
        // and never A's pending edits.
        assert!(
            d.draft_enabled(Some(&b)),
            "the detail shows B's SNAPSHOT enabled state, not A's draft"
        );
        assert!(
            d.draft_env_edit_for("wh", "A_TOKEN").is_none(),
            "B's detail never reads A's pending credential"
        );
        assert!(
            d.draft_env_edit_for("wh", "A_SECRET").is_none(),
            "B's detail never reads A's pending clear of a present key"
        );

        // STALE IDENTITY 1 — B (the selection): every edit variant is refused,
        // including an attempt to clear the same present key A_SECRET.
        assert!(!d.edit_draft_for("wh", DraftEdit::ToggleEnabled));
        assert!(!d.edit_draft_for("wh", DraftEdit::BeginCredential("WH_API_KEY")));
        assert!(!d.edit_draft_for("wh", DraftEdit::CommitCredential("123:abc")));
        assert!(!d.edit_draft_for("wh", DraftEdit::ClearEnv("A_SECRET")));
        assert!(!d.edit_draft_for("wh", DraftEdit::RevertEnv("A_TOKEN")));
        assert!(!d.edit_draft_for("wh", DraftEdit::RequestSave));

        // STALE IDENTITY 2 — A (the in-flight draft): its controls are stale
        // once B is selected, so every variant is refused here too.
        assert!(!d.edit_draft_for("tg", DraftEdit::ToggleEnabled));
        assert!(!d.edit_draft_for("tg", DraftEdit::BeginCredential("A_TOKEN")));
        assert!(!d.edit_draft_for("tg", DraftEdit::CommitCredential("123:abc")));
        assert!(!d.edit_draft_for("tg", DraftEdit::ClearEnv("A_SECRET")));
        assert!(!d.edit_draft_for("tg", DraftEdit::RevertEnv("A_TOKEN")));
        assert!(!d.edit_draft_for("tg", DraftEdit::RequestSave));

        assert_eq!(d.draft_actions(), before, "A's draft is unchanged");
        assert_eq!(
            d.draft_actions(),
            conf.actions,
            "A's pending actions are still exactly the confirmed set"
        );

        // A's save settles: the selection is unaffected. The settled A draft
        // is closed (its generation is already consumed), and the selection is
        // then synced so B becomes editable.
        assert!(d.finish_save_success(&conf));
        assert!(!d.saving);
        assert_eq!(d.state.selected_id(), Some("wh"));
        d.close_draft();
        d.sync_draft_to_selection();
        assert_eq!(d.draft_id(), Some("wh"), "B's draft opens after the save");
        assert!(
            d.draft_matches("wh"),
            "B is editable after A's save settles"
        );
        // The clear landed only in A's plan: B's fresh draft is untouched by
        // it — staging B's own toggle stages nothing else.
        assert!(
            d.edit_draft_for("wh", DraftEdit::ToggleEnabled),
            "B's toggle is accepted once A's save has settled"
        );
        assert_eq!(
            d.draft_actions(),
            vec![ConnectorAction::UpdateConfig {
                id: "wh".to_string(),
                body: serde_json::json!({ "enabled": false }),
            }],
            "B's fresh draft stages only its own toggle"
        );
    }

    /// Fixture: a connector with TWO required credentials that are BOTH present
    /// (set) in the snapshot, so a staged ClearEnv can target a key that really
    /// exists on the connector.
    fn two_set_platform(id: &str, name: &str, k1: &str, k2: &str) -> MessagingPlatform {
        let mut p = platform(id, name);
        p.enabled = true;
        p.configured = true;
        p.env_vars = vec![req_var(k1, true), req_var(k2, true)];
        p
    }

    /// S2(b) required proof (extended by RVS2B-FIX3): while a save is in
    /// flight, the in-flight actions retain the original connector's identity,
    /// and selecting a different connector does NOT open/edit its draft
    /// (read-only until the save settles). A late, stale completion for the
    /// original connector is dropped without clearing the newer draft. The
    /// confirmed actions are the EXACT ordered three-action set — SetEnv,
    /// ClearEnv and UpdateConfig — all belonging to A.
    #[test]
    fn connector_switch_during_save_keeps_identity() {
        // RVS2B-FIX3: A carries TWO env keys so the draft can stage a real
        // ClearEnv (on `TG_WEBHOOK_SECRET`) alongside the credential Set (on
        // `TG_BOT_TOKEN`); the config toggle is staged last. Key-sorted env
        // order (BOT_TOKEN < WEBHOOK_SECRET) pins the exact three-action order.
        let a = two_cred_platform("tg", "Telegram", "TG_BOT_TOKEN", "TG_WEBHOOK_SECRET");
        let b = env_platform("wh", "WhatsApp");
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![a.clone(), b.clone()]));
        d.state.select_id("tg");
        d.open_draft(Some(&a));
        // RVS2B-FIX1: stage a credential action too — the identity proof must
        // cover credential actions, not just the config toggle. RVS2B-FIX3:
        // and an explicit Clear — the confirmation carries ALL THREE actions,
        // in the exact credential-first order.
        assert!(d.begin_credential_edit("TG_BOT_TOKEN"));
        assert!(d.commit_credential_edit("123:abc"));
        d.clear_env("TG_WEBHOOK_SECRET");
        d.toggle_draft_enabled();
        assert!(d.request_save_confirmation());
        let conf = d.confirm_save().expect("A's confirmation");
        assert_eq!(conf.connector_id, "tg");
        assert!(
            conf.actions
                == vec![
                    crate::messaging_controls::ConnectorAction::SetEnv {
                        key: "TG_BOT_TOKEN".to_string(),
                        value: "123:abc".to_string(),
                    },
                    crate::messaging_controls::ConnectorAction::ClearEnv {
                        key: "TG_WEBHOOK_SECRET".to_string(),
                    },
                    crate::messaging_controls::ConnectorAction::UpdateConfig {
                        id: "tg".to_string(),
                        body: serde_json::json!({ "enabled": false }),
                    },
                ],
            "the in-flight actions are the EXACT ordered three-action set (SetEnv, ClearEnv, UpdateConfig)"
        );
        assert!(d.saving);

        // The user selects B while A's save is in flight. Selection is
        // allowed; opening B's draft is not.
        d.state.select_id("wh");
        assert!(
            !d.open_draft(Some(&b)),
            "no new draft opens while a save is in flight"
        );
        assert_eq!(d.draft_id(), Some("tg"), "A's draft stays the live draft");
        assert!(!d.draft_is_dirty() || d.draft_id() == Some("tg"));

        // RVS2B-FIX1: every edit entry point to B is read-only while A's save
        // is in flight — the guarded entry refuses, and the draft's pending
        // actions are still exactly A's.
        assert!(
            !d.draft_matches("wh"),
            "B's identity does not match the draft"
        );
        assert!(
            !d.edit_draft_for("wh", DraftEdit::ToggleEnabled),
            "B's toggle is refused while A's save is in flight"
        );
        assert!(
            !d.edit_draft_for("wh", DraftEdit::ClearEnv("TG_BOT_TOKEN")),
            "B's clear is refused while A's save is in flight"
        );
        assert_eq!(
            d.draft_actions(),
            conf.actions,
            "A's pending actions unchanged"
        );

        // A late completion for B (e.g. an older save finishing after the
        // switch) must not touch A's live in-flight draft.
        let stale_b = SaveConfirmation {
            connector_id: "wh".to_string(),
            generation: 99,
            actions: vec![],
        };
        assert!(!d.finish_save_success(&stale_b));
        assert!(d.saving, "A's in-flight save is still tracked");

        // A's completion settles the state and clears A's draft.
        assert!(d.finish_save_success(&conf));
        assert!(!d.saving);
        assert!(d.save_success);
        assert!(
            d.draft.is_none(),
            "A's draft is cleared by its own completion"
        );

        // Now B's draft may open for editing (the save has settled).
        assert!(d.open_draft(Some(&b)));
        assert_eq!(d.draft_id(), Some("wh"));
    }

    /// S2(b) required proof: a stale completion for the OLD generation leaves
    /// the NEWER draft and its confirmation intact.
    #[test]
    fn late_completion_cannot_clear_new_draft() {
        let a = env_platform("tg", "Telegram");
        let b = env_platform("wh", "WhatsApp");
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![a.clone(), b.clone()]));
        d.state.select_id("tg");
        d.open_draft(Some(&a));
        d.toggle_draft_enabled();
        assert!(d.request_save_confirmation());
        let old = d.confirm_save().expect("first confirmation");
        let old_gen = old.generation;

        // The user cancels (or closes) and re-drafts B at a newer generation,
        // opening a new confirmation for it.
        d.close_draft();
        d.state.select_id("wh");
        d.open_draft(Some(&b));
        let new_gen = d.draft_generation;
        assert!(
            new_gen > old_gen,
            "the newer draft opened at a strictly newer generation"
        );
        d.toggle_draft_enabled();
        assert!(d.request_save_confirmation());
        assert!(d.is_confirming(), "B's new confirmation is open");

        // The OLD completion arrives late with the OLD generation: it may
        // refresh the list (the host re-polls), but it must NOT clear B's
        // newer draft or confirmation.
        assert!(!d.finish_save_success(&old));
        assert!(d.draft.is_some(), "B's draft survived the stale completion");
        assert!(
            d.is_confirming(),
            "B's confirmation survived the stale completion"
        );
        assert!(
            !d.save_success,
            "a stale completion must not flip the save flags"
        );
        // B's own completion, with its own identity, settles everything.
        let b_conf = d.confirm_save().expect("B's confirmation");
        assert!(d.finish_save_success(&b_conf));
        assert!(d.draft.is_none());
        assert!(d.save_success);
    }

    /// S2(b) required proof (extended by RVS2B-FIX1): the actions Confirm
    /// dispatches are exactly the captured snapshot for the confirmed
    /// connector — SetEnv, ClearEnv AND UpdateConfig, all belonging to A —
    /// and the returned identity carries A's connector id, so they can never
    /// be paired with B's id (the host uses the returned identity, never a
    /// re-read of the live draft). The confirmation dialog's plan matches the
    /// exact action set, is names-only (no values), and belongs to A.
    #[test]
    fn credential_actions_match_confirmed_draft() {
        let a = two_cred_platform("tg", "Telegram", "TG_BOT_TOKEN", "TG_WEBHOOK_SECRET");
        let b = env_platform("wh", "WhatsApp");
        let mut d = MessagingData::default();
        d.state.apply_snapshot(snap(vec![a.clone(), b.clone()]));
        d.state.select_id("tg");
        d.open_draft(Some(&a));
        // A's draft: stage a credential Set (the unset required var), an
        // explicit Clear, and the config toggle (A is enabled in the
        // snapshot, so the toggle disables it).
        assert!(d.begin_credential_edit("TG_BOT_TOKEN"));
        assert!(d.commit_credential_edit("123:abc"));
        d.clear_env("TG_WEBHOOK_SECRET");
        d.toggle_draft_enabled();
        assert!(d.request_save_confirmation());
        // The plan is the exact action set, names-only (no values), and
        // belongs to A.
        let shown = d.confirm_plan().expect("plan shown").clone();
        assert_eq!(shown.connector_id.as_deref(), Some("tg"));
        assert_eq!(shown.set_keys, vec!["TG_BOT_TOKEN".to_string()]);
        assert_eq!(shown.clear_keys, vec!["TG_WEBHOOK_SECRET".to_string()]);
        assert_eq!(shown.enable, Some(false));
        assert_eq!(
            shown.lines(),
            vec![
                "Set TG_BOT_TOKEN".to_string(),
                "Clear TG_WEBHOOK_SECRET".to_string(),
                "Disable tg".to_string(),
            ],
            "the dialog shows exactly the three staged changes"
        );
        assert!(
            shown.lines().iter().all(|l| !l.contains("123:abc")),
            "the plan never shows the credential value"
        );

        let conf = d.confirm_save().expect("A's confirmation");
        assert_eq!(conf.connector_id, "tg");
        // The EXACT captured actions in order: the SetEnv for A's credential,
        // the ClearEnv, and the config toggle carrying A's id — never B's.
        assert_eq!(
            conf.actions,
            vec![
                ConnectorAction::SetEnv {
                    key: "TG_BOT_TOKEN".to_string(),
                    value: "123:abc".to_string(),
                },
                ConnectorAction::ClearEnv {
                    key: "TG_WEBHOOK_SECRET".to_string(),
                },
                ConnectorAction::UpdateConfig {
                    id: "tg".to_string(),
                    body: serde_json::json!({ "enabled": false }),
                },
            ],
            "Confirm applies exactly the snapshot's three actions"
        );
        // The captured actions pair ONLY with A's connector id: the returned
        // identity is A's, so any host-side identity-dependent action
        // (self-test invalidation, the completion call) can only target A.
        assert_ne!(conf.connector_id, b.id);
        // RVS2B-FIX2: the connector identity is asserted explicitly — A's id
        // and never B's — on both the returned confirmation identity and the
        // config-toggle action itself.
        assert_eq!(conf.connector_id, a.id);
        assert_eq!(conf.connector_id, "tg");
        assert_eq!(
            conf.actions
                .iter()
                .find_map(|act| match act {
                    ConnectorAction::UpdateConfig { id, .. } => Some(id),
                    _ => None,
                })
                .map(String::as_str),
            Some("tg"),
            "the config toggle carries A's connector id"
        );
        // RVS2B-FIX2: no action names one of B's keys or B's id — the
        // captured plan belongs to A alone (checked per action, in addition
        // to the identity assertions above).
        for action in &conf.actions {
            let foreign = match action {
                ConnectorAction::SetEnv { key, .. } | ConnectorAction::ClearEnv { key } => {
                    key.starts_with("WH_")
                }
                ConnectorAction::UpdateConfig { id, .. } => id == "wh",
            };
            assert!(!foreign, "no action may name B's key or id: {action:?}");
        }

        // RVS2B-FIX1: switching the selection to B makes B read-only — the
        // guarded edit entry refuses (RVS2B-FIX2: for EVERY edit variant,
        // including the ClearEnv on B's own key), and A's pending actions are
        // untouched.
        d.select_id("wh");
        assert!(
            !d.edit_draft_for("wh", DraftEdit::ClearEnv("WH_VERIFY_TOKEN")),
            "B's clear of its own key refuses while A's save is staged"
        );
        assert!(!d.edit_draft_for("wh", DraftEdit::ToggleEnabled));
        assert_eq!(
            d.draft_actions(),
            conf.actions,
            "A's pending actions unchanged"
        );
    }

    /// S2(b) required proof: two Confirm events (double click) dispatch ONE
    /// save — the second Confirm finds no open confirmation and returns `None`,
    /// so no second op is emitted.
    #[test]
    fn duplicate_confirm_dispatches_once() {
        let mut d = savable_draft();
        d.request_save_confirmation();
        let first = d.confirm_save();
        assert!(
            first.is_some(),
            "first Confirm returns the identity + actions"
        );
        assert_eq!(first.as_ref().map(|c| c.actions.len()), Some(1));
        // The second Confirm (the dialog is closed, a save is in flight)
        // must be a no-op: no actions, no second save op.
        let second = d.confirm_save();
        assert!(second.is_none(), "second Confirm dispatches nothing");
        assert!(d.saving, "exactly one save is in flight");
        // Settling the single save leaves no residue.
        let conf = first.expect("first confirmation");
        assert!(d.finish_save_success(&conf));
        assert!(!d.saving && d.save_success);
    }

    // --- RVS2-FIX5: keyboard entry points go through the single transition --

    /// Open a save confirmation for A ("tg") with a two-connector snapshot so
    /// B ("wh") exists for the keyboard tests.
    fn confirming_first_of_two() -> MessagingData {
        let tg = platform("tg", "Telegram");
        let mut d = MessagingData::default();
        d.state
            .apply_snapshot(snap(vec![tg.clone(), platform("wh", "WhatsApp")]));
        d.open_draft(Some(&tg));
        d.toggle_draft_enabled();
        assert!(d.save_enabled(), "fixture must be savable");
        assert!(d.request_save_confirmation(), "A's confirmation opens");
        assert_eq!(d.state.selected_id(), Some("tg"));
        d
    }

    #[test]
    fn rvs2_fix5_keyboard_round_trip_cancels_confirmation() {
        let mut d = confirming_first_of_two();
        // ↓ then ↑: A→B→A. Selection is A again, draft generation unchanged —
        // but ANY selection change cancels the open confirmation.
        d.select_next();
        d.select_prev();
        assert_eq!(d.state.selected_id(), Some("tg"));
        assert!(
            d.confirm_save().is_none(),
            "round trip must not re-open A's stale confirmation"
        );
        assert!(!d.saving, "no save is in flight");
    }

    #[test]
    fn rvs2_fix5_keyboard_step_to_other_connector_cancels_confirmation() {
        let mut d = confirming_first_of_two();
        d.select_next();
        assert_eq!(d.state.selected_id(), Some("wh"));
        assert!(d.confirm_save().is_none(), "A's confirmation must be gone");
        assert!(!d.saving);
        assert!(
            d.draft_id().is_some_and(|id| id == "wh"),
            "the draft now belongs to B"
        );
    }

    #[test]
    fn rvs2_fix5_filter_edit_that_moves_selection_cancels_confirmation() {
        let mut d = confirming_first_of_two();
        d.push_query("whatsapp");
        assert_eq!(
            d.state.selected_id(),
            Some("wh"),
            "the filter hides A; selection moves to B"
        );
        assert!(
            d.confirm_save().is_none(),
            "a real selection change cancels the confirmation"
        );
        assert!(!d.saving);

        // A filter edit that keeps A selected leaves the confirmation OPEN: the
        // cancel fires only on an actual selection change.
        let mut d = confirming_first_of_two();
        d.push_query("tele");
        assert_eq!(d.state.selected_id(), Some("tg"));
        assert!(d.is_confirming(), "selection unchanged → dialog stays open");
    }
}
