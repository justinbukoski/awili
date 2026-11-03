//! Messaging-connector **controls** — the pure, unsaved-edit half of plan unit 25.
//!
//! [`crate::messaging_pane`] is the *read* half: a window-free list/detail/status
//! state machine over a decoded [`MessagingPlatforms`] snapshot. It deliberately
//! stops at reading — its own docs mark the mutating `test` action, and by
//! extension every other write, as **[LIVE]** / Owner-gated. This module builds
//! the mutating half as the same kind of pure, `#[test]`-covered engine: a
//! [`ConnectorDraft`] that captures a user's *pending* changes to one connector
//! (enable/disable + per-var credential edits) and lowers them into an ordered
//! list of [`ConnectorAction`]s the eventual GPUI host applies through the
//! already-built [`RestClient`] calls
//! (`update_messaging` / `set_env_var` / `delete_env_var` / `test_messaging`).
//! The engine never performs I/O; the live wiring is left to the [LIVE] half.
//!
//! ## The secret-safety invariant (why this module is not a trivial form model)
//!
//! The messaging pane shows a connector's credentials **redacted** — a set var
//! renders as its server-supplied preview or, failing that, as a bullet mask
//! (`••••••••`), and an unset var as `(not set)` (see
//! [`crate::messaging_pane::EnvVarView`]). A naive editor that round-trips the
//! *displayed* value straight back into `PUT /api/env` would overwrite the real
//! secret with the mask — silently corrupting the credential the moment the user
//! saves an unrelated change. So this engine enforces two rules:
//!
//! 1. **Untouched keys never move.** A [`ConnectorDraft`] starts empty and only
//!    records a key the user *explicitly* edits; [`ConnectorDraft::actions`]
//!    emits nothing for any key the user left alone — the mask can't be sent
//!    because it was never captured.
//! 2. **A placeholder is never a value.** [`ConnectorDraft::set_env`] rejects a
//!    value that is empty or that *looks like* a redaction placeholder
//!    ([`is_redaction_placeholder`]) — the user must use
//!    [`ConnectorDraft::clear_env`] to unset a var, which lowers to an explicit
//!    `DELETE`, not a write of the mask.
//!
//! ## Apply order
//!
//! [`ConnectorDraft::actions`] returns credential writes **before** the config
//! toggle, so enabling a freshly-configured connector applies its creds first
//! and the enable doesn't race an empty credential set.
use std::collections::BTreeMap;

use awili_gateway::rest::{MessagingEnvVar, MessagingPlatform};

/// The bullet mask [`crate::messaging_pane::EnvVarView`] renders for a set-but-
/// unpreviewable secret. Kept in sync with that module by the
/// `mask_glyph_matches_pane` test.
const MASK_BULLET: char = '\u{2022}'; // '•'

/// The placeholder [`crate::messaging_pane::EnvVarView`] renders for an unset
/// var. Pinned against the pane by `not_set_placeholder_matches_pane`.
const NOT_SET_PLACEHOLDER: &str = "(not set)";

/// One user-pending change to a single credential in a [`ConnectorDraft`].
///
/// Absence of a key from the draft means *untouched* — the crucial third state
/// that keeps a redacted display value from being written back (see the module
/// docs). Only [`Set`](EnvEdit::Set) and [`Clear`](EnvEdit::Clear) are ever
/// recorded, and only for keys the user acted on.
/// RC2-APP (T5): `Debug` never prints the staged plaintext (see the manual
/// impl below).
#[derive(Clone, PartialEq, Eq)]
pub enum EnvEdit {
    /// Replace the credential with this plaintext value. Guaranteed non-empty
    /// and not a redaction placeholder (enforced at [`ConnectorDraft::set_env`]).
    Set(String),
    /// Explicitly unset the credential (lowers to `DELETE /api/env`).
    Clear,
}

/// One concrete mutation [`ConnectorDraft::actions`] emits for the host to apply.
///
/// Each variant maps one-to-one onto an already-built [`awili_gateway::rest::RestClient`]
/// call, so the [LIVE] wiring is a thin `match`:
/// - [`UpdateConfig`](ConnectorAction::UpdateConfig) → `update_messaging(id, &body)`
/// - [`SetEnv`](ConnectorAction::SetEnv)             → `set_env_var(&key, &value)`
/// - [`ClearEnv`](ConnectorAction::ClearEnv)         → `delete_env_var(&key)`
///
/// RC2-APP (T5): `Debug` never prints a credential value (manual impl).
#[derive(Clone, PartialEq, Eq)]
pub enum ConnectorAction {
    /// Flat `PUT /api/messaging/platforms/{id}` config body (passthrough, like
    /// `set_config`). Only emitted when the connector config actually changed.
    UpdateConfig { id: String, body: serde_json::Value },
    /// `PUT /api/env` — set/replace one credential the connector's `.env` reads.
    SetEnv { key: String, value: String },
    /// `DELETE /api/env` — clear one credential.
    ClearEnv { key: String },
}

/// RC2-APP (T5): a staged credential prints as `Set(<redacted>)`, so a debug
/// print of a draft (or of anything holding one) never shows a secret.
impl std::fmt::Debug for EnvEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvEdit::Set(_) => f.write_str("Set(<redacted>)"),
            EnvEdit::Clear => f.write_str("Clear"),
        }
    }
}

/// RC2-APP (T5): the credential value of a `SetEnv` prints as `<redacted>`.
impl std::fmt::Debug for ConnectorAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectorAction::UpdateConfig { id, body } => f
                .debug_struct("UpdateConfig")
                .field("id", id)
                .field("body", body)
                .finish(),
            ConnectorAction::SetEnv { key, .. } => f
                .debug_struct("SetEnv")
                .field("key", key)
                .field("value", &format_args!("<redacted>"))
                .finish(),
            ConnectorAction::ClearEnv { key } => {
                f.debug_struct("ClearEnv").field("key", key).finish()
            }
        }
    }
}

/// True when `value` is a display-only placeholder the pane renders for a
/// redacted/unset credential, and therefore must **never** be written back as a
/// real secret. Rejects: the empty/blank field, the `(not set)` unset
/// placeholder (case-insensitive), and — crucially — **any** value containing
/// the bullet mask char.
///
/// The mask is not always a *full* `••••••••` run. The server also sends
/// *partial* previews that keep a real prefix/suffix around the masked middle,
/// e.g. `sk-••••1234`; [`crate::messaging_pane::EnvVarView::from_env`] displays
/// those verbatim. Such a preview is real chars mixed with bullets, so an
/// "all-bullets" test would let it through and the user could round-trip it into
/// `set_env`, writing the mask back over the live secret. Since the bullet
/// (`U+2022`) is a display glyph that does not occur in real credentials, any
/// occurrence of it marks the string as a redacted preview.
///
/// This is a *heuristic guard*, not authentication: a user who genuinely wants a
/// secret containing a `•` can't set it through this path — an acceptable trade
/// for never corrupting a credential with its own mask.
pub fn is_redaction_placeholder(value: &str) -> bool {
    let t = value.trim();
    if t.is_empty() {
        return true;
    }
    if t.eq_ignore_ascii_case(NOT_SET_PLACEHOLDER) {
        return true;
    }
    // RC2-APP (T4): other mask styles a preview can use — the other dot
    // glyphs, an ellipsis, a run of three dots or stars ("sk-...1234",
    // "sk-***1234") — are masks too.
    t.chars().any(|c| MASK_GLYPHS.contains(&c)) || t.contains("...") || t.contains("***")
}

/// RC2-APP (T4): the glyphs a redacted preview can be drawn with: the
/// pane's own bullet ([`MASK_BULLET`]) and the other usual mask dots and
/// the ellipsis. None of them occurs in a real credential.
const MASK_GLYPHS: [char; 7] = [
    MASK_BULLET, // •
    '\u{25CF}',  // ●
    '\u{2219}',  // ∙
    '\u{00B7}',  // ·
    '\u{25E6}',  // ◦
    '\u{2026}',  // …
    '\u{22EF}',  // ⋯
];

/// A pending, unsaved edit to one connector's configuration and credentials —
/// the "controls" state the messaging pane's detail view drives. Purely models
/// what the user has changed so the host can (a) show a dirty/Save affordance
/// via [`is_dirty`](Self::is_dirty) and (b) apply the change through the ordered
/// [`actions`](Self::actions) plan. Time-free and I/O-free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorDraft {
    id: String,
    /// The connector's `enabled` flag at draft-open — the baseline the toggle is
    /// diffed against so a flip-then-flip-back is *not* dirty.
    base_enabled: bool,
    /// The pending `enabled` value.
    enabled: bool,
    /// Per-key pending credential edits. A key is present only when the user
    /// explicitly edited it; absence means untouched (never written).
    env: BTreeMap<String, EnvEdit>,
}

impl ConnectorDraft {
    /// Open a fresh draft over a connector, seeding the enable/disable baseline
    /// from its current snapshot state. No credential edits are pending.
    pub fn for_platform(p: &MessagingPlatform) -> ConnectorDraft {
        ConnectorDraft {
            id: p.id.clone(),
            base_enabled: p.enabled,
            enabled: p.enabled,
            env: BTreeMap::new(),
        }
    }

    /// The connector this draft edits.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The pending enabled state.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Set the pending enabled state.
    #[allow(dead_code)] // the toggle path uses `toggle_enabled` until it ships
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Flip the pending enabled state (the enable/disable toggle).
    pub fn toggle_enabled(&mut self) {
        self.enabled = !self.enabled;
    }

    /// True when the enable/disable toggle differs from the snapshot baseline.
    pub fn enabled_changed(&self) -> bool {
        self.enabled != self.base_enabled
    }

    /// Stage a new plaintext credential value for `key`. Returns `false` and
    /// records **nothing** when `value` is empty or a redaction placeholder
    /// ([`is_redaction_placeholder`]) — the guard that stops the displayed mask
    /// from being written back over the real secret. To unset a credential use
    /// [`clear_env`](Self::clear_env), not an empty [`set_env`](Self::set_env).
    pub fn set_env(&mut self, key: impl Into<String>, value: impl Into<String>) -> bool {
        let value = value.into();
        if is_redaction_placeholder(&value) {
            return false;
        }
        self.env.insert(key.into(), EnvEdit::Set(value));
        true
    }

    /// Stage an explicit clear/unset for `key` (lowers to `DELETE /api/env`).
    pub fn clear_env(&mut self, key: impl Into<String>) {
        self.env.insert(key.into(), EnvEdit::Clear);
    }

    /// Drop any pending edit for `key`, returning it to the untouched state so
    /// [`actions`](Self::actions) emits nothing for it. No-op if the key was not
    /// edited.
    pub fn revert_env(&mut self, key: &str) {
        self.env.remove(key);
    }

    /// The pending edit for `key`, or `None` when the user hasn't touched it.
    pub fn env_edit(&self, key: &str) -> Option<&EnvEdit> {
        self.env.get(key)
    }

    /// How many credentials have a pending edit.
    #[allow(dead_code)] // painted by the draft footer once it ships
    pub fn env_edit_count(&self) -> usize {
        self.env.len()
    }

    /// True when anything would be applied — a changed toggle or ≥1 credential
    /// edit. Drives the Save affordance; also the guard the host checks before
    /// prompting "discard unsaved changes?".
    pub fn is_dirty(&self) -> bool {
        self.enabled_changed() || !self.env.is_empty()
    }

    /// Lower the draft into the ordered list of mutations to apply. Empty when
    /// the draft is not dirty. Credential writes come first (sorted by key for a
    /// deterministic apply and stable tests), then the single config
    /// [`UpdateConfig`](ConnectorAction::UpdateConfig) if the toggle changed — so
    /// enabling a just-configured connector applies its creds before the enable.
    pub fn actions(&self) -> Vec<ConnectorAction> {
        let mut out = Vec::new();
        // BTreeMap iterates in key order → deterministic.
        for (key, edit) in &self.env {
            match edit {
                EnvEdit::Set(value) => out.push(ConnectorAction::SetEnv {
                    key: key.clone(),
                    value: value.clone(),
                }),
                EnvEdit::Clear => out.push(ConnectorAction::ClearEnv { key: key.clone() }),
            }
        }
        if self.enabled_changed() {
            out.push(ConnectorAction::UpdateConfig {
                id: self.id.clone(),
                body: serde_json::json!({ "enabled": self.enabled }),
            });
        }
        out
    }

    /// A redaction-safe preview of what [`actions`](Self::actions) will apply —
    /// the confirmation a Save affordance shows before the [LIVE] host fires the
    /// mutations at the gateway. Derived from `actions()` so the two never drift.
    pub fn change_plan(&self) -> ChangePlan {
        summarize_actions(&self.actions())
    }

    /// Whether `key` would be **set** after this draft applies, given its
    /// snapshot baseline `base_is_set`. A pending [`Set`](EnvEdit::Set) forces
    /// `true`, a [`Clear`](EnvEdit::Clear) forces `false`, and an untouched key
    /// keeps the baseline. The projection that lets [`validate`](Self::validate)
    /// judge required-credential coverage against the *pending* state, not just
    /// the snapshot.
    fn effective_is_set(&self, key: &str, base_is_set: bool) -> bool {
        match self.env.get(key) {
            Some(EnvEdit::Set(_)) => true,
            Some(EnvEdit::Clear) => false,
            None => base_is_set,
        }
    }

    /// Check the draft's *resulting* state against the connector's env-var
    /// schema `vars` (typically `&platform.env_vars`): which **required**
    /// credentials would still be unset once the draft's edits apply. This is
    /// the precondition the [LIVE] host checks before offering Save/enable —
    /// [`actions`](Self::actions) will happily emit `enabled: true`, so without
    /// this gate the host could enable a connector that instantly fails its
    /// self-test for a missing key.
    ///
    /// Unlike the gateway's baseline-only [`configured`](MessagingPlatform::configured)
    /// flag, this accounts for pending edits in *both* directions — setting a
    /// previously-unset required key clears it, and clearing a set one flags it.
    /// Secret-safe: the returned [`DraftValidation`] carries only key **names**.
    pub fn validate(&self, vars: &[MessagingEnvVar]) -> DraftValidation {
        let mut missing_required = Vec::new();
        for v in vars {
            if v.required && !self.effective_is_set(&v.key, v.is_set) {
                missing_required.push(v.key.clone());
            }
        }
        missing_required.sort();
        // Defensive: a malformed schema with a duplicate `key` must not double-
        // count a gap (`headline` count / `lines`). Well-formed `platform.env_vars`
        // never duplicates, so this is a no-op on the real path.
        missing_required.dedup();
        DraftValidation {
            missing_required,
            enabled: self.enabled,
        }
    }
}

/// A redaction-safe, human-readable preview of the mutations a dirty
/// [`ConnectorDraft`] will apply — the text a Save/confirm affordance shows the
/// user *before* the [LIVE] host fires the ordered [`actions`](ConnectorDraft::actions)
/// against the gateway.
///
/// It structurally holds **only credential key names**, never their pending
/// plaintext values, so a just-typed secret can never leak into a confirmation
/// string, tooltip, or log line. That is the same secret-safety stance the rest
/// of this module enforces — here proven *by construction* (there is no field a
/// value could live in) rather than by a runtime guard like
/// [`is_redaction_placeholder`]. Time-free and I/O-free.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangePlan {
    /// The connector id — populated only when the enable/disable flag is
    /// changing (the sole action that carries it). `None` for a creds-only plan.
    pub connector_id: Option<String>,
    /// `Some(true)` to enable, `Some(false)` to disable, `None` if unchanged.
    pub enable: Option<bool>,
    /// Credential keys being set/replaced — **names only**, sorted.
    pub set_keys: Vec<String>,
    /// Credential keys being cleared — **names only**, sorted.
    pub clear_keys: Vec<String>,
}

impl ChangePlan {
    /// True when nothing would be applied (matches `!draft.is_dirty()`).
    pub fn is_empty(&self) -> bool {
        self.enable.is_none() && self.set_keys.is_empty() && self.clear_keys.is_empty()
    }

    /// Total distinct mutations — equals `draft.actions().len()`.
    pub fn change_count(&self) -> usize {
        self.set_keys.len() + self.clear_keys.len() + self.enable.is_some() as usize
    }

    /// One-line headline for the Save button / confirm banner, e.g.
    /// `"3 changes \u{00b7} enable, 2 set, 1 cleared"`. Never names a value.
    pub fn headline(&self) -> String {
        if self.is_empty() {
            return "No changes".to_string();
        }
        let mut parts = Vec::new();
        match self.enable {
            Some(true) => parts.push("enable".to_string()),
            Some(false) => parts.push("disable".to_string()),
            None => {}
        }
        if !self.set_keys.is_empty() {
            parts.push(format!("{} set", self.set_keys.len()));
        }
        if !self.clear_keys.is_empty() {
            parts.push(format!("{} cleared", self.clear_keys.len()));
        }
        let n = self.change_count();
        let noun = if n == 1 { "change" } else { "changes" };
        format!("{n} {noun} \u{00b7} {}", parts.join(", "))
    }

    /// Per-mutation lines for a confirmation list, grouped: credentials set,
    /// then cleared, then the enable/disable toggle. Each names only a key or a
    /// connector id — never a plaintext value.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for k in &self.set_keys {
            out.push(format!("Set {k}"));
        }
        for k in &self.clear_keys {
            out.push(format!("Clear {k}"));
        }
        if let Some(en) = self.enable {
            let verb = if en { "Enable" } else { "Disable" };
            match &self.connector_id {
                Some(id) => out.push(format!("{verb} {id}")),
                None => out.push(verb.to_string()),
            }
        }
        out
    }
}

/// Summarize an ordered [`ConnectorAction`] plan into a redaction-safe
/// [`ChangePlan`]. The `SetEnv` value is deliberately ignored (`..`) — only its
/// key is recorded, so no secret can flow into the summary.
///
/// `pub(crate)` because its 1:1 [`change_count`](ChangePlan::change_count)↔
/// `actions.len()` guarantee only holds for [`ConnectorDraft::actions`]-shaped
/// input (at most one `UpdateConfig`, carrying a boolean `"enabled"`). A
/// hand-built slice with two `UpdateConfig`s, or one whose `body` lacks a bool
/// `"enabled"`, would collapse both into a single `connector_id`/`enable` and
/// desync the count — a footgun kept off the public surface. Host callers reach
/// it through [`ConnectorDraft::change_plan`], never raw.
pub(crate) fn summarize_actions(actions: &[ConnectorAction]) -> ChangePlan {
    let mut plan = ChangePlan::default();
    for a in actions {
        match a {
            ConnectorAction::SetEnv { key, .. } => plan.set_keys.push(key.clone()),
            ConnectorAction::ClearEnv { key } => plan.clear_keys.push(key.clone()),
            ConnectorAction::UpdateConfig { id, body } => {
                plan.connector_id = Some(id.clone());
                plan.enable = body.get("enabled").and_then(|v| v.as_bool());
            }
        }
    }
    plan.set_keys.sort();
    plan.clear_keys.sort();
    plan
}

/// The verdict [`ConnectorDraft::validate`] returns: whether a draft's resulting
/// state satisfies the connector's required-credential schema, and if not, which
/// required keys are still missing.
///
/// Like [`ChangePlan`], it holds **only credential key names** — never a pending
/// plaintext value — so surfacing it in an inline field-error, a disabled-Save
/// tooltip, or a log line cannot leak a just-typed secret. Time-free and I/O-free.
///
/// ## The two questions a Save affordance asks
///
/// A connector may be left **disabled** with required credentials still missing —
/// that is a legitimate partial setup, and [`can_save`](Self::can_save) allows it.
/// What is never allowed is *enabling* a connector while a required credential
/// would be unset: [`blocks_enable`](Self::blocks_enable) is the gate the host
/// checks to disable the Save/enable button and show
/// [`missing_required`](Self::missing_required) inline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftValidation {
    /// Required credential keys that would still be unset after the draft
    /// applies — **names only**, sorted. Empty means fully configured.
    pub missing_required: Vec<String>,
    /// The draft's pending `enabled` state. An enabled connector with any
    /// missing required credential is invalid to save; a disabled one is not.
    enabled: bool,
}

// The validators run with the connector dialog's commit path once it ships.
#[allow(dead_code)]
impl DraftValidation {
    /// True when every required credential would be set after the draft applies.
    pub fn fully_configured(&self) -> bool {
        self.missing_required.is_empty()
    }

    /// The required credential keys still missing — names only, sorted.
    pub fn missing_required(&self) -> &[String] {
        &self.missing_required
    }

    /// True when the draft tries to leave the connector **enabled** while a
    /// required credential is missing — the one state a Save must refuse. The
    /// host disables the Save/enable control on this.
    pub fn blocks_enable(&self) -> bool {
        self.enabled && !self.missing_required.is_empty()
    }

    /// True when the draft may be saved: always, unless it would enable a
    /// connector with a missing required credential ([`blocks_enable`](Self::blocks_enable)).
    /// Saving credential edits on a connector left disabled is allowed.
    pub fn can_save(&self) -> bool {
        !self.blocks_enable()
    }

    /// One-line status for a Save button / inline banner. Never names a value:
    /// `"Cannot enable \u{00b7} 2 required credentials missing"` when it blocks,
    /// a softer `"2 required credentials missing"` when merely incomplete but
    /// disabled, or `"All required credentials set"` when configured.
    pub fn headline(&self) -> String {
        let n = self.missing_required.len();
        if n == 0 {
            return "All required credentials set".to_string();
        }
        let noun = if n == 1 { "credential" } else { "credentials" };
        if self.blocks_enable() {
            format!("Cannot enable \u{00b7} {n} required {noun} missing")
        } else {
            format!("{n} required {noun} missing")
        }
    }

    /// Per-key inline error lines, e.g. `"Missing TELEGRAM_BOT_TOKEN"` — one per
    /// [`missing_required`](Self::missing_required) key, names only.
    pub fn lines(&self) -> Vec<String> {
        self.missing_required
            .iter()
            .map(|k| format!("Missing {k}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messaging_pane::EnvVarView;
    use awili_gateway::rest::{MessagingEnvVar, MessagingPlatform};

    fn platform(id: &str, enabled: bool, configured: bool) -> MessagingPlatform {
        MessagingPlatform {
            id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            docs_url: String::new(),
            enabled,
            configured,
            gateway_running: false,
            state: String::new(),
            error_code: None,
            error_message: None,
            updated_at: None,
            home_channel: None,
            env_vars: Vec::new(),
        }
    }

    fn env_var(key: &str, is_set: bool, redacted: Option<&str>) -> MessagingEnvVar {
        MessagingEnvVar {
            key: key.to_string(),
            required: true,
            is_set,
            redacted_value: redacted.map(|s| s.to_string()),
            description: String::new(),
            prompt: String::new(),
            help: String::new(),
            url: None,
            is_password: true,
            advanced: false,
        }
    }

    // --- redaction guard --------------------------------------------------

    #[test]
    fn placeholder_detects_mask_not_set_and_blank() {
        assert!(is_redaction_placeholder(""));
        assert!(is_redaction_placeholder("   "));
        assert!(is_redaction_placeholder("(not set)"));
        assert!(is_redaction_placeholder("(NOT SET)"));
        assert!(is_redaction_placeholder("••••••••"));
        assert!(is_redaction_placeholder("  ••••  ")); // padded mask
                                                       // Partial server previews (real prefix/suffix around a masked middle)
                                                       // MUST be caught — this is the corruption vector: displaying `sk-••••1234`
                                                       // and letting the user save it would write the mask over the live secret.
        assert!(is_redaction_placeholder("sk-••••1234"));
        assert!(is_redaction_placeholder("pass•word")); // any bullet at all → rejected
                                                        // Real, bullet-free values stay writable.
        assert!(!is_redaction_placeholder("sk-live-abc123"));
        assert!(!is_redaction_placeholder("smtp.example.com"));
    }

    /// RC2-APP (T4): dot-, star- and ellipsis-style masked previews are
    /// masks too; real credentials (single dots included) stay writable.
    #[test]
    fn rc2app_other_mask_styles_are_refused() {
        for masked in [
            "sk-...1234",
            "sk-***1234",
            "sk-…1234",
            "sk-●●●●1234",
            "sk-∙∙∙∙1234",
            "sk-····1234",
            "sk-◦◦◦◦1234",
            "sk-⋯1234",
        ] {
            assert!(is_redaction_placeholder(masked), "{masked}");
        }
        for real in [
            "sk-ant-api03-abc_DEF-123",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
            "smtp.example.com",
            "p*ss",
        ] {
            assert!(!is_redaction_placeholder(real), "{real}");
        }
    }

    /// RC2-APP (T5): a debug print of a draft or of its actions never shows
    /// a staged credential.
    #[test]
    fn rc2app_debug_never_prints_a_staged_credential() {
        let mut d = ConnectorDraft::for_platform(&platform("e", true, true));
        assert!(d.set_env("SMTP_PASS", "hunter2-secret"));
        let shown = format!("{d:?} {:?}", d.actions());
        assert!(!shown.contains("hunter2-secret"), "{shown}");
        assert!(
            shown.contains("SMTP_PASS") && shown.contains("<redacted>"),
            "{shown}"
        );
    }

    #[test]
    fn mask_glyph_and_placeholder_match_the_pane() {
        // The pane and this guard must agree on the exact glyph/text, else a
        // value the pane displays as redacted could slip past the guard. Build
        // the pane's own views and assert the guard rejects both placeholders.
        let mut p = platform("e", true, true);
        p.env_vars = vec![
            env_var("SET_NO_PREVIEW", true, None), // pane → bullet mask
            env_var("UNSET", false, None),         // pane → "(not set)"
        ];
        let views: Vec<EnvVarView> = p.env_vars.iter().map(EnvVarView::from_env).collect();
        assert!(
            is_redaction_placeholder(&views[0].display_value),
            "pane mask {:?} must be caught by the guard",
            views[0].display_value
        );
        assert!(
            is_redaction_placeholder(&views[1].display_value),
            "pane (not set) {:?} must be caught by the guard",
            views[1].display_value
        );
    }

    // --- draft baseline + toggle -----------------------------------------

    #[test]
    fn fresh_draft_is_clean_and_mirrors_baseline() {
        let d = ConnectorDraft::for_platform(&platform("tg", true, true));
        assert_eq!(d.id(), "tg");
        assert!(d.enabled());
        assert!(!d.is_dirty());
        assert!(d.actions().is_empty());
    }

    #[test]
    fn toggle_makes_dirty_and_toggle_back_is_clean() {
        let mut d = ConnectorDraft::for_platform(&platform("tg", false, true));
        assert!(!d.is_dirty());
        d.toggle_enabled();
        assert!(d.enabled());
        assert!(d.enabled_changed());
        assert!(d.is_dirty());
        // Flip back to baseline → no net change, not dirty, no actions.
        d.toggle_enabled();
        assert!(!d.enabled_changed());
        assert!(!d.is_dirty());
        assert!(d.actions().is_empty());
    }

    #[test]
    fn enable_lowers_to_flat_update_config_body() {
        let mut d = ConnectorDraft::for_platform(&platform("sig_nal-7", false, true));
        d.set_enabled(true);
        let acts = d.actions();
        assert_eq!(
            acts,
            vec![ConnectorAction::UpdateConfig {
                id: "sig_nal-7".to_string(),
                body: serde_json::json!({ "enabled": true }),
            }]
        );
    }

    // --- credential editing safety ---------------------------------------

    #[test]
    fn set_env_rejects_placeholders_and_records_nothing() {
        let mut d = ConnectorDraft::for_platform(&platform("e", true, true));
        // The naive-corruption cases: writing back the mask or the (not set)
        // placeholder or an empty field must be refused and leave no edit.
        assert!(!d.set_env("SMTP_PASS", "••••••••"));
        assert!(!d.set_env("SMTP_PASS", "sk-••••1234")); // partial server preview
        assert!(!d.set_env("SMTP_PASS", "(not set)"));
        assert!(!d.set_env("SMTP_PASS", ""));
        assert!(!d.set_env("SMTP_PASS", "   "));
        assert_eq!(d.env_edit("SMTP_PASS"), None);
        assert_eq!(d.env_edit_count(), 0);
        assert!(!d.is_dirty());
        assert!(d.actions().is_empty());
    }

    #[test]
    fn set_env_records_real_value_and_is_dirty() {
        let mut d = ConnectorDraft::for_platform(&platform("e", true, true));
        assert!(d.set_env("SMTP_HOST", "smtp.example.com"));
        assert_eq!(
            d.env_edit("SMTP_HOST"),
            Some(&EnvEdit::Set("smtp.example.com".to_string()))
        );
        assert!(d.is_dirty());
        assert_eq!(
            d.actions(),
            vec![ConnectorAction::SetEnv {
                key: "SMTP_HOST".to_string(),
                value: "smtp.example.com".to_string(),
            }]
        );
    }

    #[test]
    fn clear_env_lowers_to_delete_not_a_masked_write() {
        let mut d = ConnectorDraft::for_platform(&platform("e", true, true));
        d.clear_env("SMTP_PASS");
        assert_eq!(d.env_edit("SMTP_PASS"), Some(&EnvEdit::Clear));
        assert!(d.is_dirty());
        assert_eq!(
            d.actions(),
            vec![ConnectorAction::ClearEnv {
                key: "SMTP_PASS".to_string()
            }]
        );
    }

    #[test]
    fn untouched_keys_emit_no_action() {
        // The crown-jewel safety property: a connector with several redacted
        // creds, of which the user edits exactly one, must emit an action ONLY
        // for the edited key. Every untouched (redacted) key stays put — the
        // mask is never round-tripped.
        let mut d = ConnectorDraft::for_platform(&platform("e", true, true));
        d.set_env("API_KEY", "new-key-value");
        let acts = d.actions();
        assert_eq!(acts.len(), 1);
        assert_eq!(
            acts[0],
            ConnectorAction::SetEnv {
                key: "API_KEY".to_string(),
                value: "new-key-value".to_string(),
            }
        );
    }

    #[test]
    fn revert_env_returns_key_to_untouched() {
        let mut d = ConnectorDraft::for_platform(&platform("e", true, true));
        d.set_env("API_KEY", "v");
        assert!(d.is_dirty());
        d.revert_env("API_KEY");
        assert_eq!(d.env_edit("API_KEY"), None);
        assert!(!d.is_dirty());
        assert!(d.actions().is_empty());
        // Reverting an untouched key is a harmless no-op.
        d.revert_env("NEVER_TOUCHED");
        assert!(!d.is_dirty());
    }

    // --- apply order + combined plan -------------------------------------

    #[test]
    fn actions_write_creds_before_enabling_and_sort_env_by_key() {
        // Configure two creds and enable in one draft. Credential writes must
        // come first (so the enable doesn't race an empty cred set), and be in
        // deterministic key order; the config toggle comes last.
        let mut d = ConnectorDraft::for_platform(&platform("tg", false, false));
        d.set_env("BOT_TOKEN", "abc");
        d.clear_env("ALT_TOKEN");
        d.set_enabled(true);
        let acts = d.actions();
        assert_eq!(
            acts,
            vec![
                // sorted by key: ALT_TOKEN before BOT_TOKEN
                ConnectorAction::ClearEnv {
                    key: "ALT_TOKEN".to_string()
                },
                ConnectorAction::SetEnv {
                    key: "BOT_TOKEN".to_string(),
                    value: "abc".to_string()
                },
                // toggle last
                ConnectorAction::UpdateConfig {
                    id: "tg".to_string(),
                    body: serde_json::json!({ "enabled": true }),
                },
            ]
        );
    }

    #[test]
    fn later_edit_to_same_key_replaces_earlier() {
        let mut d = ConnectorDraft::for_platform(&platform("e", true, true));
        d.set_env("K", "first");
        d.set_env("K", "second");
        assert_eq!(d.env_edit_count(), 1);
        assert_eq!(d.env_edit("K"), Some(&EnvEdit::Set("second".to_string())));
        // set → clear on the same key ends as a clear.
        d.clear_env("K");
        assert_eq!(d.env_edit("K"), Some(&EnvEdit::Clear));
        assert_eq!(d.env_edit_count(), 1);
    }

    // --- change-plan summary (redaction-safe Save confirmation) -----------

    #[test]
    fn change_plan_of_clean_draft_is_empty() {
        let d = ConnectorDraft::for_platform(&platform("s", true, true));
        let plan = d.change_plan();
        assert!(plan.is_empty());
        assert_eq!(plan.change_count(), 0);
        assert_eq!(plan.headline(), "No changes");
        assert!(plan.lines().is_empty());
        // Empty plan mirrors an empty action list.
        assert_eq!(plan.change_count(), d.actions().len());
    }

    #[test]
    fn change_plan_never_contains_the_secret_value() {
        // The core secret-safety invariant: a just-typed token must not appear
        // anywhere in the plan — not in a field, a line, the headline, or Debug.
        const SECRET: &str = "xoxb-super-secret-token-9999";
        let mut d = ConnectorDraft::for_platform(&platform("slack", false, false));
        assert!(d.set_env("SLACK_BOT_TOKEN", SECRET));
        d.set_enabled(true);
        let plan = d.change_plan();

        // Only the KEY is recorded, never the value.
        assert_eq!(plan.set_keys, vec!["SLACK_BOT_TOKEN".to_string()]);
        assert!(!format!("{plan:?}").contains(SECRET));
        assert!(!plan.headline().contains(SECRET));
        assert!(plan.lines().iter().all(|l| !l.contains(SECRET)));
        // Sanity: it still names the key so the user knows what changes.
        assert!(plan.lines().iter().any(|l| l.contains("SLACK_BOT_TOKEN")));
    }

    #[test]
    fn change_plan_count_matches_actions_len_and_sorts_keys() {
        let mut d = ConnectorDraft::for_platform(&platform("tg", false, false));
        d.set_env("ZED", "v");
        d.set_env("ABLE", "v");
        d.clear_env("OLD");
        d.set_enabled(true);
        let plan = d.change_plan();
        // 2 set + 1 clear + 1 enable = 4, equal to actions().len().
        assert_eq!(plan.change_count(), 4);
        assert_eq!(plan.change_count(), d.actions().len());
        // Keys sorted for a stable, deterministic confirmation list.
        assert_eq!(plan.set_keys, vec!["ABLE".to_string(), "ZED".to_string()]);
        assert_eq!(plan.clear_keys, vec!["OLD".to_string()]);
        // connector_id present because the toggle changed; enable = true.
        assert_eq!(plan.connector_id.as_deref(), Some("tg"));
        assert_eq!(plan.enable, Some(true));
        assert_eq!(
            plan.headline(),
            "4 changes \u{00b7} enable, 2 set, 1 cleared"
        );
        assert_eq!(
            plan.lines(),
            vec![
                "Set ABLE".to_string(),
                "Set ZED".to_string(),
                "Clear OLD".to_string(),
                "Enable tg".to_string(),
            ]
        );
    }

    #[test]
    fn change_plan_disable_and_creds_only_wording() {
        // Disable-only.
        let mut d = ConnectorDraft::for_platform(&platform("d", true, true));
        d.set_enabled(false);
        let plan = d.change_plan();
        assert_eq!(plan.enable, Some(false));
        assert_eq!(plan.connector_id.as_deref(), Some("d"));
        assert_eq!(plan.headline(), "1 change \u{00b7} disable");
        assert_eq!(plan.lines(), vec!["Disable d".to_string()]);

        // Creds-only: no enable line, no connector_id.
        let mut c = ConnectorDraft::for_platform(&platform("c", true, true));
        c.clear_env("TOKEN");
        let cp = c.change_plan();
        assert_eq!(cp.enable, None);
        assert_eq!(cp.connector_id, None);
        assert_eq!(cp.headline(), "1 change \u{00b7} 1 cleared");
        assert_eq!(cp.lines(), vec!["Clear TOKEN".to_string()]);
    }

    #[test]
    fn summarize_actions_agrees_with_draft_change_plan() {
        // The free function over a raw action list matches the draft-derived plan.
        let mut d = ConnectorDraft::for_platform(&platform("m", false, false));
        d.set_env("A", "1");
        d.clear_env("B");
        d.set_enabled(true);
        assert_eq!(summarize_actions(&d.actions()), d.change_plan());
    }

    // --- draft validation (required-credential precondition for enable) ---

    /// `env_var` hardcodes `required: true`; this lets a test pin an *optional*
    /// var (or a required one) with an explicit set state.
    fn env_var_req(key: &str, required: bool, is_set: bool) -> MessagingEnvVar {
        MessagingEnvVar {
            required,
            ..env_var(key, is_set, None)
        }
    }

    #[test]
    fn validate_configured_enabled_connector_has_no_missing() {
        // Required key already set, connector staying enabled → fully valid.
        let vars = vec![env_var("BOT_TOKEN", true, None)];
        let d = ConnectorDraft::for_platform(&platform("tg", true, true));
        let v = d.validate(&vars);
        assert!(v.fully_configured());
        assert!(v.missing_required().is_empty());
        assert!(!v.blocks_enable());
        assert!(v.can_save());
        assert_eq!(v.headline(), "All required credentials set");
        assert!(v.lines().is_empty());
    }

    #[test]
    fn validate_flags_unset_required_and_blocks_enable() {
        // Enabled connector with a required key that is not set → cannot enable.
        let vars = vec![env_var("BOT_TOKEN", false, None)];
        let d = ConnectorDraft::for_platform(&platform("tg", true, false));
        let v = d.validate(&vars);
        assert_eq!(v.missing_required(), ["BOT_TOKEN".to_string()]);
        assert!(!v.fully_configured());
        assert!(v.blocks_enable());
        assert!(!v.can_save());
        assert_eq!(
            v.headline(),
            "Cannot enable \u{00b7} 1 required credential missing"
        );
        assert_eq!(v.lines(), vec!["Missing BOT_TOKEN".to_string()]);
    }

    #[test]
    fn setting_a_required_var_clears_the_gap() {
        // Pending Set on an unset required key makes it effectively set — the
        // check must see the *resulting* state, not the snapshot baseline.
        let vars = vec![env_var("BOT_TOKEN", false, None)];
        let mut d = ConnectorDraft::for_platform(&platform("tg", true, false));
        assert!(d.set_env("BOT_TOKEN", "real-token-value"));
        let v = d.validate(&vars);
        assert!(v.fully_configured());
        assert!(!v.blocks_enable());
        assert!(v.can_save());
    }

    #[test]
    fn clearing_a_set_required_var_reopens_the_gap() {
        // The case the gateway's baseline-only `configured` flag can't catch:
        // a currently-set required key the user clears in this draft.
        let vars = vec![env_var("BOT_TOKEN", true, None)];
        let mut d = ConnectorDraft::for_platform(&platform("tg", true, true));
        d.clear_env("BOT_TOKEN");
        let v = d.validate(&vars);
        assert_eq!(v.missing_required(), ["BOT_TOKEN".to_string()]);
        assert!(v.blocks_enable());
        assert!(!v.can_save());
    }

    #[test]
    fn disabled_connector_with_missing_required_can_still_save() {
        // Partial setup is legitimate: leaving a connector disabled with keys
        // still missing is reported but NOT blocked.
        let vars = vec![env_var("BOT_TOKEN", false, None)];
        let d = ConnectorDraft::for_platform(&platform("tg", false, false));
        let v = d.validate(&vars);
        assert_eq!(v.missing_required(), ["BOT_TOKEN".to_string()]);
        assert!(!v.blocks_enable());
        assert!(v.can_save());
        // Softer wording when incomplete-but-disabled (no "Cannot enable").
        assert_eq!(v.headline(), "1 required credential missing");
    }

    #[test]
    fn optional_vars_never_count_as_missing() {
        // Only `required` vars gate enable; an unset optional var is fine.
        let vars = vec![
            env_var_req("REQUIRED_KEY", true, true),
            env_var_req("OPTIONAL_KEY", false, false),
        ];
        let d = ConnectorDraft::for_platform(&platform("tg", true, true));
        let v = d.validate(&vars);
        assert!(v.fully_configured());
        assert!(v.can_save());
    }

    #[test]
    fn missing_required_keys_are_sorted_and_plural_wording() {
        let vars = vec![
            env_var_req("ZED_TOKEN", true, false),
            env_var_req("ABLE_TOKEN", true, false),
            env_var_req("OPTIONAL", false, false),
        ];
        let d = ConnectorDraft::for_platform(&platform("tg", true, false));
        let v = d.validate(&vars);
        assert_eq!(
            v.missing_required(),
            ["ABLE_TOKEN".to_string(), "ZED_TOKEN".to_string()]
        );
        assert_eq!(
            v.headline(),
            "Cannot enable \u{00b7} 2 required credentials missing"
        );
        assert_eq!(
            v.lines(),
            vec![
                "Missing ABLE_TOKEN".to_string(),
                "Missing ZED_TOKEN".to_string(),
            ]
        );
    }

    #[test]
    fn validation_never_leaks_a_pending_secret_value() {
        // A just-typed value on one required key (which resolves it) plus a
        // still-missing required key: the secret must not surface anywhere in
        // the verdict — fields, headline, lines, or Debug.
        const SECRET: &str = "xoxb-do-not-leak-me-4242";
        let vars = vec![
            env_var("SET_ME", false, None),
            env_var("STILL_MISSING", false, None),
        ];
        let mut d = ConnectorDraft::for_platform(&platform("slack", true, false));
        assert!(d.set_env("SET_ME", SECRET));
        let v = d.validate(&vars);
        // Only the unresolved required key is reported, by name.
        assert_eq!(v.missing_required(), ["STILL_MISSING".to_string()]);
        assert!(!format!("{v:?}").contains(SECRET));
        assert!(!v.headline().contains(SECRET));
        assert!(v.lines().iter().all(|l| !l.contains(SECRET)));
    }

    #[test]
    fn no_schema_vars_is_trivially_configured() {
        // A connector that declares no env vars can't be missing any.
        let d = ConnectorDraft::for_platform(&platform("noop", true, true));
        let v = d.validate(&[]);
        assert!(v.fully_configured());
        assert!(v.can_save());
    }
}
