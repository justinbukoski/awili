//! The settings panel's pure state machine: which tab is showing, which field
//! is being edited, and the commit paths that gate every write through
//! awili-config's validation. No GPUI here — the validation gate (invalid input
//! rejected with an inline error, nothing written; `.bak` on every successful
//! write) is what the specs ask to unit-test, so it lives where a test can
//! drive it against a temp path without a window.
//!
//! Two editing models coexist (slice 7 → slice 11):
//!
//! - The [`Draft`] — the structured panel (slice-11 spec §1). Providers,
//!   default model, memory service, and voice live in an in-memory draft;
//!   nothing touches disk until Save, and Discard throws the draft away.
//! - The per-field path ([`SettingsState::commit_field`] /
//!   [`commit_raw`](SettingsState::commit_raw)) — the "All fields" and raw-TOML
//!   tabs (slice-7 spec §3), which commit immediately per edit.

use std::path::{Path, PathBuf};

use awili_config::{
    doc::save_create_only, save_validated, Config, ConfigDoc, NotificationsConfig, ProviderConfig,
    ProviderKind, Target, ValueKind,
};
use awili_voice::{
    VoiceConfig, VAD_SILENCE_WINDOWS_MAX, VAD_SILENCE_WINDOWS_MIN, VAD_THRESHOLD_MAX,
    VAD_THRESHOLD_MIN,
};

/// Which settings tab is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    /// The structured panel: providers, default model, memory, voice (slice 11).
    Form,
    /// The generic per-scalar form over the whole document (slice 7).
    Fields,
    /// The raw config.toml text with an explicit Save.
    Raw,
}

/// One text field of the structured panel's [`Draft`] — what the shared inline
/// editor is pointed at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    ProviderName(usize),
    ProviderUrl(usize),
    /// The provider's model list, edited as comma-separated text.
    ProviderModels(usize),
    /// The provider's API key, edited as explicit plaintext while the row is
    /// being edited (click-to-reveal contract, same as every other field):
    /// masked at rest, plaintext only for the owner's own explicit edit.
    /// Anthropic and OpenAi provider kinds only (see `main::provider_card`).
    ProviderApiKey(usize),
    #[allow(dead_code)] // SXe: now a catalog row (settings_live); kept for its tests.
    MemoryUrl,
    #[allow(dead_code)] // SXe: now a catalog row (settings_live); kept for its tests.
    TtsModel,
}

/// One provider row of the draft. `models` is the comma-separated edit text —
/// parsed back into a list on save.
#[derive(Clone, PartialEq)]
pub struct ProviderDraft {
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub models: String,
    pub enabled: bool,
    /// The provider's configured secret (H3). The structured panel does not
    /// render it, but it must round-trip: editing any unrelated field rebuilds
    /// the whole provider list, and dropping this would silently wipe the key
    /// from disk. Preserved here verbatim and written back by `to_config()`.
    pub api_key: Option<String>,
}

// Custom `Debug` so a debug-logged draft never leaks the provider `api_key` the
// H3 fix added to this struct (H4); presence stays observable. `Draft` derives
// `Debug` and holds a `Vec<ProviderDraft>`, so redacting this leaf makes the
// whole-draft dump transitively safe — same pattern as `awili_config::Config`.
impl std::fmt::Debug for ProviderDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let api_key = match &self.api_key {
            Some(s) if !s.is_empty() => "Some(<redacted>)",
            Some(_) => "Some(\"\")",
            None => "None",
        };
        f.debug_struct("ProviderDraft")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("models", &self.models)
            .field("enabled", &self.enabled)
            .field("api_key", &api_key)
            .finish()
    }
}

impl ProviderDraft {
    /// The blank row the "Add provider" button appends.
    pub fn blank() -> Self {
        Self {
            name: String::new(),
            kind: ProviderKind::Ollama,
            base_url: "http://".to_string(),
            models: String::new(),
            enabled: true,
            api_key: None,
        }
    }

    fn from_config(p: &ProviderConfig) -> Self {
        Self {
            name: p.name.clone(),
            kind: p.kind,
            base_url: p.base_url.clone(),
            models: p.models.join(", "),
            enabled: p.enabled,
            api_key: p.api_key.clone(),
        }
    }

    fn to_config(&self) -> ProviderConfig {
        ProviderConfig {
            name: self.name.trim().to_string(),
            kind: self.kind,
            base_url: self.base_url.trim().to_string(),
            models: self
                .models
                .split(',')
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .collect(),
            enabled: self.enabled,
            // H3: preserve the existing key. The panel never edits it, so
            // dropping it here would wipe a configured secret on every save.
            api_key: self.api_key.clone(),
        }
    }
}

/// The structured panel's in-memory edit state (slice-11 spec §1). Built from
/// the document at open, mutated by the UI, written back ONLY by
/// [`SettingsState::save_draft`]; Discard simply drops it.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub work_executor: Option<String>,
    pub providers: Vec<ProviderDraft>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub memory_url: String,
    pub memory_recall: bool,
    pub memory_write: bool,
    pub vad_threshold: f64,
    pub vad_silence_windows: u32,
    pub tts_model: String,
    /// N1: the `[notifications]` section (its six switches are pills).
    pub notify: NotificationsConfig,
}

/// N1: the six `[notifications]` switches the Form tab toggles.
#[allow(dead_code)] // SXg: the Notifications page applies live; kept for its tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyToggle {
    Enabled,
    Approvals,
    Finished,
    Failed,
    Sound,
}

impl Draft {
    pub fn from_doc(doc: &ConfigDoc) -> Draft {
        let cfg = doc.config();
        // The [voice] table is awili-voice's; a malformed one must not block the
        // rest of the panel — fall back to voice defaults.
        let voice = VoiceConfig::parse(&doc.text()).unwrap_or_default();
        Draft {
            work_executor: cfg.work.default_executor.clone(),
            providers: cfg
                .providers
                .iter()
                .map(ProviderDraft::from_config)
                .collect(),
            default_provider: cfg.default_provider.clone(),
            default_model: cfg.default_model.clone(),
            memory_url: cfg.memory.base_url.clone(),
            memory_recall: cfg.memory.recall,
            memory_write: cfg.memory.write,
            vad_threshold: voice.vad_threshold,
            vad_silence_windows: voice.vad_silence_windows,
            tts_model: voice.tts_model.unwrap_or_default(),
            notify: cfg.notifications.clone(),
        }
    }

    /// Every `(provider, model)` pair the default-model dropdown offers: the
    /// models of every ENABLED provider in the draft, in order.
    pub fn model_options(&self) -> Vec<(String, String)> {
        self.providers
            .iter()
            .filter(|p| p.enabled)
            .flat_map(|p| {
                let cfg = p.to_config();
                cfg.models
                    .into_iter()
                    .map(move |m| (cfg.name.clone(), m))
                    .collect::<Vec<_>>()
            })
            .filter(|(name, _)| !name.is_empty())
            .collect()
    }

    /// The dropdown's current label.
    #[allow(dead_code)] // SXe: now a catalog row (settings_live); kept for its tests.
    pub fn default_label(&self) -> String {
        match (&self.default_provider, &self.default_model) {
            (Some(p), Some(m)) => format!("{p} · {m}"),
            (Some(p), None) => format!("{p} · (first model)"),
            _ => "(first available)".to_string(),
        }
    }

    /// The position of the log-scale VAD slider's fill, in 0..=1.
    #[allow(dead_code)] // SXe: now a catalog row (settings_live); kept for its tests.
    pub fn vad_position(&self) -> f64 {
        let v = self
            .vad_threshold
            .clamp(VAD_THRESHOLD_MIN, VAD_THRESHOLD_MAX);
        (v / VAD_THRESHOLD_MIN).ln() / (VAD_THRESHOLD_MAX / VAD_THRESHOLD_MIN).ln()
    }
}

/// `Ok` when `url` looks like a usable HTTP(S) endpoint. The bar is deliberately
/// modest — scheme + non-empty host — enough to catch the pastes and typos the
/// slice-11 spec's validation test is about, without dragging in a URL crate.
pub fn validate_url(url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| format!("invalid URL {url:?} — must start with http:// or https://"))?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(format!("invalid URL {url:?} — missing or malformed host"));
    }
    Ok(())
}

/// The footer notice shown after a save that changed provider wiring.
pub const RESTART_NOTICE: &str =
    "Provider changes applied — restart Awili to fully reconnect changed endpoints.";

/// Settings panel state. The document of record is `doc`; the GPUI layer owns
/// the text editors and hands their contents to the commit methods here.
pub struct SettingsState {
    pub open: bool,
    pub tab: Tab,
    /// The secrets hidden from the Raw TOML editor this sync (D1): the
    /// original quoted values, in file order per key name. Restored into the
    /// edited text at save time WHEREVER the mask is still untouched — and
    /// only against the exact [`raw_masked_snapshot`] below.
    pub raw_redactions: Vec<(String, String)>,
    /// The EXACT masked editor text shown for this sync — the snapshot
    /// `restore_secrets` binds edits against (temporary fail-closed policy:
    /// any changed masked document is rejected before any write).
    pub raw_masked_snapshot: String,
    /// The loaded config document; `None` only when the file on disk failed to
    /// parse at open time (the raw tab still works from `load_error` context).
    pub doc: Option<ConfigDoc>,
    /// The form field being edited inline: its target and how to type the text.
    pub editing: Option<(Target, ValueKind)>,
    /// Inline validation error — shown, never written to disk.
    pub error: Option<String>,
    /// Transient "Saved" confirmation after a successful write.
    pub status: Option<String>,
    /// Bumped on every status set, so the 2-second clear timer only clears the
    /// status it was scheduled for (slice-11 spec §1).
    pub status_seq: u64,
    /// The structured panel's edit state; `None` when the document is.
    pub draft: Option<Draft>,
    /// Which draft text field the shared inline editor is pointed at.
    pub field: Option<Field>,
    /// Whether the default-model dropdown list is unfolded.
    pub model_dropdown: bool,
    /// Set by a save that changed provider wiring (name/kind/base_url or
    /// add/remove): shown until the panel closes or the next save (spec §1/§3).
    pub restart_notice: Option<String>,
    /// The ordered candidate config paths, priority order (FIRST-RUN-SPEC
    /// 2026-09-06). The first candidate with ANY directory entry is the
    /// existing config (a malformed one stays authoritative — no fallback is
    /// silently chosen); when none exists the first candidate is where the
    /// first save CREATES the file.
    candidates: Vec<PathBuf>,
    /// Whether a config file is known to stand at the save path. `false` only
    /// in the missing first-run state (the document is the explicit networkless
    /// seed): the next save rechecks every candidate and publishes
    /// create-only. `true` once any file exists or has appeared: saves are
    /// the ordinary `save_validated` writes with `.bak`.
    existing_origin: bool,
    /// Where saves land: the chosen existing config path, else the first
    /// candidate (the create-only target).
    path: PathBuf,
    /// SX: the live changes Undo can walk back, newest last (`settings_live`).
    pub undo: Vec<crate::settings_live::UndoStep>,
    /// SXc: the page the Settings sidebar shows (General on every open).
    pub page: crate::settings_catalog::Page,
    /// SXd: the catalog text/key item the inline editor is pointed at.
    pub editing_item: Option<&'static str>,
    /// SXe: a guarded choice waiting for its second click: `(item, value)`.
    pub pending_choice: Option<(&'static str, &'static str)>,
    /// RC2-UX (R2-21): the row the last change landed on, which shows
    /// "✓ Saved" (or "✓ Back to default") right there, next to its control.
    pub saved_here: Option<(&'static str, &'static str)>,
    /// SXe: closing was asked for while model services were unsaved; the
    /// change bar asks Save / Throw away / Keep editing.
    pub confirm_close: bool,
}

impl SettingsState {
    pub fn new() -> Self {
        // The ordered candidate list (XDG `~/.config/awili/config.toml` first,
        // then the platform dir, de-duplicated by `config_paths`). On a
        // machine with no known home/config dir, fall back to a relative
        // default name — the same final destination `new()` has always used.
        let candidates = {
            let c = Config::config_paths();
            if c.is_empty() {
                vec![PathBuf::from("config.toml")]
            } else {
                c
            }
        };
        Self::with_paths(candidates)
    }

    /// Test seam: same machine, a single explicit save path (delegates to the
    /// ordered [`with_paths`](Self::with_paths) seam).
    #[allow(dead_code)] // test seam — exercised only by the settings tests
    pub fn with_path(path: PathBuf) -> Self {
        Self::with_paths(vec![path])
    }

    /// Test seam: explicit ORDERED candidate paths (primary first, fallbacks
    /// after). The first candidate with any directory entry — file,
    /// directory, symlink, even a dangling one — is the existing config; when
    /// none exists, the panel seeds the explicit networkless document and the
    /// first candidate is where the first save creates the file.
    pub fn with_paths(candidates: Vec<PathBuf>) -> Self {
        let candidates = if candidates.is_empty() {
            vec![PathBuf::from("config.toml")]
        } else {
            candidates
        };
        // Resolve the initial origin the same way `open` will; a stale
        // reading is re-resolved there (and again before a first save).
        let (path, existing_origin) = match Self::resolve_existing(&candidates) {
            ResolvedConfig::Existing(p) => (p, true),
            ResolvedConfig::Absent => (candidates.first().cloned().unwrap(), false),
            ResolvedConfig::MetadataError(p, _) => (p, true),
        };
        Self {
            raw_redactions: Vec::new(),
            raw_masked_snapshot: String::new(),
            open: false,
            tab: Tab::Form,
            doc: None,
            editing: None,
            error: None,
            status: None,
            status_seq: 0,
            draft: None,
            field: None,
            model_dropdown: false,
            restart_notice: None,
            candidates,
            existing_origin,
            path,
            undo: Vec::new(),
            page: crate::settings_catalog::Page::General,
            editing_item: None,
            pending_choice: None,
            saved_here: None,
            confirm_close: false,
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Open the panel, (re)loading the document from disk so the form shows
    /// what is actually there — including hand edits since the last open.
    ///
    /// The ordered candidates are re-resolved on every open (FIRST-RUN-SPEC
    /// 2026-09-06): when a config exists the file's own text is loaded as
    /// before (never reserialized); when NO candidate has any directory entry
    /// the panel seeds the EXPLICIT networkless document — every service URL
    /// empty, no providers, no secrets — instead of the old empty-TOML serde
    /// defaults, and the state is flagged missing-origin so the first save
    /// publishes create-only.
    pub fn open(&mut self) {
        self.open = true;
        self.tab = Tab::Form;
        self.page = crate::settings_catalog::Page::General;
        self.editing_item = None;
        self.pending_choice = None;
        self.confirm_close = false;
        self.editing = None;
        self.status = None;
        self.field = None;
        self.model_dropdown = false;
        self.restart_notice = None;
        match Self::resolve_existing(&self.candidates) {
            ResolvedConfig::Absent => {
                self.existing_origin = false;
                self.path = self
                    .candidates
                    .first()
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from("config.toml"));
                match ConfigDoc::networkless() {
                    Ok(doc) => {
                        self.draft = Some(Draft::from_doc(&doc));
                        self.doc = Some(doc);
                        self.error = None;
                    }
                    Err(e) => {
                        self.doc = None;
                        self.draft = None;
                        self.error = Some(format!("config failed to load: {e}"));
                    }
                }
            }
            ResolvedConfig::Existing(path) => {
                self.existing_origin = true;
                self.path = path;
                self.load_from_disk();
            }
            ResolvedConfig::MetadataError(path, e) => {
                // A directory entry is present (or unverifiable): it stays
                // authoritative — the error surfaces, nothing is written.
                self.existing_origin = true;
                self.path = path;
                self.doc = None;
                self.draft = None;
                self.error = Some(format!("config failed to load: {e}"));
            }
        }
    }

    /// Load the document of record from the (known existing) config file at
    /// the save path: the file's OWN text, read as before — an existing
    /// document is never reserialized from the parsed `Config`.
    fn load_from_disk(&mut self) {
        match Self::load_doc(&self.path) {
            Ok(doc) => {
                self.draft = Some(Draft::from_doc(&doc));
                self.doc = Some(doc);
                self.error = None;
            }
            Err(e) => {
                self.doc = None;
                self.draft = None;
                self.error = Some(format!("config failed to load: {e}"));
            }
        }
    }

    /// The document at `path` — known to exist (a `resolve_existing` hit: any
    /// directory entry counts, including a dangling symlink). A load error
    /// from it is the panel's inline error, never a fallback to an empty
    /// (all-defaults) document that a save would then write. The missing
    /// networkless seeding already happens in [`open`](Self::open).
    fn load_doc(path: &PathBuf) -> Result<ConfigDoc, awili_config::ConfigError> {
        let text = std::fs::read_to_string(path).map_err(awili_config::ConfigError::Io)?;
        ConfigDoc::parse(&text)
    }

    /// Re-resolve the ordered candidate paths by SYMLINK metadata, the way
    /// the first-run contract requires: only `NotFound` means absent. ANY
    /// existing entry counts as an existing config — an empty file, a
    /// directory, or a symlink (even a dangling one) — and the FIRST such
    /// entry is authoritative: a malformed or unreadable primary is an
    /// error, not a reason to silently load a valid fallback.
    fn resolve_existing(candidates: &[PathBuf]) -> ResolvedConfig {
        for path in candidates {
            match std::fs::symlink_metadata(path) {
                Ok(_) => return ResolvedConfig::Existing(path.clone()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return ResolvedConfig::MetadataError(path.clone(), e),
            }
        }
        ResolvedConfig::Absent
    }

    /// A config file appeared at `path` after the panel opened missing (the
    /// pre-save recheck found it, or it raced the create-only publish):
    /// adopt `path` as the existing save target and load the external
    /// document (or its load error) for review, with a clear inline conflict
    /// notice. Mirrors [`open`](Self::open)'s reset so the review state is
    /// clean. The external file is NEVER overwritten or backed up here.
    ///
    /// Residual (documented, not fixable at this seam): target creation is
    /// atomic — the create-only `hard_link` publish fails on any existing
    /// entry — but cross-path existence is NOT atomic: a SECONDARY candidate
    /// can appear after the precheck. Such a file is never written to (saves
    /// land only at the chosen path), so it is never overwritten — but no
    /// cross-path atomicity is claimed.
    fn adopt_existing(&mut self, path: &Path) {
        self.path = path.to_path_buf();
        self.existing_origin = true;
        self.tab = Tab::Form;
        self.editing = None;
        self.status = None;
        self.field = None;
        self.model_dropdown = false;
        self.restart_notice = None;
        self.load_from_disk();
        let detail = match &self.error {
            // `load_from_disk` left a load error behind: surface it inside the
            // conflict notice — the external file stays authoritative.
            Some(load_error) => format!(". It could not be loaded for review: {load_error}"),
            None => ". The existing file was loaded for review — edit it and save again to apply your changes.".to_string(),
        };
        self.error = Some(format!(
            "A config file appeared at {} while the settings panel was open — the draft was not saved{detail}",
            path.display()
        ));
    }

    /// The ONE persistence gate every settings save entry point goes through
    /// (`save_draft`, `commit_raw`, `commit_field`, `toggle_bool`).
    ///
    /// Existing origin: the unchanged `save_validated` (validate, `.bak`,
    /// temp + replacing rename) at the save path — existing-config saves keep
    /// their existing behavior, with no new generic conflict rules.
    ///
    /// Missing origin (first-run seed): ALL candidate paths are re-resolved
    /// IMMEDIATELY before the write. If any entry has appeared since open,
    /// the stale save is STOPPED — nothing is written and no success signal
    /// is sent — and the chosen existing document (or its load error) is
    /// reloaded for review via [`adopt_existing`](Self::adopt_existing).
    /// Only when every candidate is still absent does the text publish
    /// through the create-only gate (`save_create_only`); if the target races
    /// into existence in that window the publish fails with `AlreadyExists`
    /// and is rechecked and handled the same way as an appearance. A
    /// successful first save flips the state to existing-origin, so later
    /// saves are ordinary `save_validated` writes with `.bak`.
    ///
    /// Cancel/discard never reach this gate and write nothing.
    fn persist(&mut self, text: &str) -> PersistOutcome {
        if self.existing_origin {
            match save_validated(&self.path, text) {
                Ok(()) => PersistOutcome::Landed,
                Err(e) => {
                    self.error = Some(e.to_string());
                    PersistOutcome::Failed
                }
            }
        } else {
            match Self::resolve_existing(&self.candidates) {
                ResolvedConfig::Absent => match save_create_only(&self.path, text) {
                    Ok(()) => {
                        // The file exists now: from here on this is an
                        // existing config — later saves back it up.
                        self.existing_origin = true;
                        PersistOutcome::Landed
                    }
                    Err(e) => match Self::resolve_existing(&self.candidates) {
                        // A raced appearance failed the create-only publish
                        // safely: adopt it for review and report.
                        ResolvedConfig::Existing(p) => {
                            self.adopt_existing(&p);
                            PersistOutcome::Appeared
                        }
                        ResolvedConfig::MetadataError(p, _) => {
                            self.adopt_existing(&p);
                            PersistOutcome::Failed
                        }
                        ResolvedConfig::Absent => {
                            self.error = Some(e.to_string());
                            PersistOutcome::Failed
                        }
                    },
                },
                ResolvedConfig::Existing(p) => {
                    self.adopt_existing(&p);
                    PersistOutcome::Appeared
                }
                ResolvedConfig::MetadataError(p, _) => {
                    self.adopt_existing(&p);
                    PersistOutcome::Failed
                }
            }
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.editing_item = None;
        self.pending_choice = None;
        self.confirm_close = false;
        self.editing = None;
        self.field = None;
        self.model_dropdown = false;
        self.restart_notice = None;
    }

    pub fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.editing_item = None;
        self.editing = None;
        self.field = None;
        self.model_dropdown = false;
        self.error = None;
        self.status = None;
    }

    /// Successful write: clear the error, show the transient confirmation.
    fn set_saved(&mut self) {
        self.error = None;
        self.status = Some("Saved".to_string());
        self.status_seq += 1;
    }

    /// Rebuild the draft from the (just-saved or just-reloaded) document so the
    /// structured tab agrees with disk.
    fn refresh_draft(&mut self) {
        self.draft = self.doc.as_ref().map(Draft::from_doc);
        self.field = None;
        self.model_dropdown = false;
    }

    /// Begin editing a form field inline.
    pub fn begin_edit(&mut self, target: Target, kind: ValueKind) {
        self.editing = Some((target, kind));
        self.error = None;
        self.status = None;
    }

    pub fn cancel_edit(&mut self) {
        self.editing = None;
        self.error = None;
    }

    /// Commit the in-progress field edit with the editor's text: typed setter
    /// (validates the WHOLE document before accepting), then save (which
    /// re-validates and writes the `.bak`). Any failure leaves the inline
    /// error, the in-memory doc rolled back, and the disk untouched. Returns
    /// `true` when the edit landed on disk.
    pub fn commit_field(&mut self, text: &str) -> bool {
        // RC2-APP (F0232): the document as it was, restored when the write fails.
        let before = self.doc.clone();
        let Some((target, kind)) = self.editing.clone() else {
            return false;
        };
        let Some(doc) = self.doc.as_mut() else {
            return false;
        };
        let set = match kind {
            ValueKind::Str => doc.set_string(&target, text.trim()),
            ValueKind::Int => match text.trim().parse::<i64>() {
                Ok(n) => doc.set_integer(&target, n),
                Err(_) => {
                    self.error = Some(format!("not an integer: {text:?}"));
                    return false;
                }
            },
            ValueKind::Bool => match text.trim() {
                "true" => doc.set_bool(&target, true),
                "false" => doc.set_bool(&target, false),
                other => {
                    self.error = Some(format!("not true/false: {other:?}"));
                    return false;
                }
            },
            ValueKind::Other => doc.set_raw(&target, text),
        };
        if let Err(e) = set {
            self.error = Some(e.to_string());
            return false;
        }
        // The in-memory document is valid now; hand the write to the
        // persistence gate (the borrow of `doc` has ended). The gate
        // rechecks for an externally appeared config before a first save.
        let text = self.doc.as_ref().unwrap().text();
        match self.persist(&text) {
            PersistOutcome::Landed => {
                self.editing = None;
                self.set_saved();
                self.refresh_draft();
                true
            }
            _ => {
                self.restore_unsaved(before, &text);
                false
            }
        }
    }

    /// RC2-APP (F0232): a write that did not land leaves the in-memory
    /// document as it was before the edit (the panel never shows a value the
    /// file does not hold). Not when the gate already replaced the document
    /// (a config that appeared meanwhile was adopted for review).
    fn restore_unsaved(&mut self, before: Option<ConfigDoc>, edited: &str) {
        if self.doc.as_ref().map(ConfigDoc::text).as_deref() == Some(edited) {
            self.doc = before;
        }
    }

    /// Flip a boolean field and save immediately (booleans don't get a text
    /// editor — a click toggles them).
    pub fn toggle_bool(&mut self, target: &Target, current: bool) -> bool {
        let before = self.doc.clone(); // RC2-APP (F0232)
        let Some(doc) = self.doc.as_mut() else {
            return false;
        };
        if let Err(e) = doc.set_bool(target, !current) {
            self.error = Some(e.to_string());
            return false;
        }
        let text = self.doc.as_ref().unwrap().text();
        match self.persist(&text) {
            PersistOutcome::Landed => {
                self.set_saved();
                self.refresh_draft();
                true
            }
            _ => {
                self.restore_unsaved(before, &text);
                false
            }
        }
    }

    /// Save the raw-TOML tab's text: full validation first — invalid TOML
    /// never reaches disk — then the write through the persistence gate (the
    /// existing-config `.bak` + atomic write, or the create-only publish on a
    /// first save). On success the in-memory doc is rebuilt from the saved
    /// text so the form tab agrees.
    pub fn commit_raw(&mut self, text: &str) -> bool {
        let doc = match ConfigDoc::parse(text) {
            Ok(doc) => doc,
            Err(e) => {
                self.error = Some(e.to_string());
                return false;
            }
        };
        // The gate writes the user's EXACT bytes (no reserialization), the
        // same as before.
        match self.persist(text) {
            PersistOutcome::Landed => {
                self.doc = Some(doc);
                self.set_saved();
                self.refresh_draft();
                true
            }
            _ => false,
        }
    }

    /// Save the raw-TOML tab's EDITED text with the D1 fail-closed
    /// restoration guard applied first: [`restore_secrets`] splices the
    /// hidden originals back over untouched masks, or refuses BEFORE
    /// validation or write. On refusal the fixed message lands in
    /// [`error`](Self::error), the editor draft and snapshot stay intact
    /// for repair, and `false` is returned before [`commit_raw`](Self::commit_raw)
    /// is ever reached. On success it delegates to `commit_raw` — one
    /// restoration, one side-effecting path.
    pub fn commit_raw_editor(&mut self, edited: &str) -> bool {
        let text = match restore_secrets(edited, &self.raw_redactions, &self.raw_masked_snapshot) {
            Ok(text) => text,
            Err(msg) => {
                self.error = Some(msg.to_string());
                return false;
            }
        };
        self.commit_raw(&text)
    }

    // ---- the structured panel's draft (slice-11 spec §1) --------------------

    /// The current text of a draft field, for prefill into the inline editor.
    pub fn field_text(&self, field: Field) -> String {
        let Some(d) = self.draft.as_ref() else {
            return String::new();
        };
        match field {
            Field::ProviderName(i) => d.providers.get(i).map(|p| p.name.clone()),
            Field::ProviderUrl(i) => d.providers.get(i).map(|p| p.base_url.clone()),
            Field::ProviderModels(i) => d.providers.get(i).map(|p| p.models.clone()),
            // The real secret, for the owner's own explicit edit — the shared
            // inline editor is plaintext by contract while a field is up.
            Field::ProviderApiKey(i) => d.providers.get(i).and_then(|p| p.api_key.clone()),
            Field::MemoryUrl => Some(d.memory_url.clone()),
            Field::TtsModel => Some(d.tts_model.clone()),
        }
        .unwrap_or_default()
    }

    /// Point the shared inline editor at a draft field.
    pub fn begin_field(&mut self, field: Field) {
        self.field = Some(field);
        self.editing = None;
        self.model_dropdown = false;
        self.error = None;
        self.status = None;
    }

    pub fn cancel_field(&mut self) {
        self.field = None;
    }

    /// Commit the inline editor's text into the draft — memory only, nothing
    /// touches disk until [`save_draft`](Self::save_draft).
    pub fn commit_draft_field(&mut self, text: &str) -> bool {
        let Some(field) = self.field.take() else {
            return false;
        };
        let Some(d) = self.draft.as_mut() else {
            return false;
        };
        match field {
            Field::ProviderName(i) => {
                let Some(p) = d.providers.get_mut(i) else {
                    return false;
                };
                p.name = text.trim().to_string();
            }
            Field::ProviderUrl(i) => {
                let Some(p) = d.providers.get_mut(i) else {
                    return false;
                };
                p.base_url = text.trim().to_string();
            }
            Field::ProviderModels(i) => {
                let Some(p) = d.providers.get_mut(i) else {
                    return false;
                };
                p.models = text.trim().to_string();
            }
            Field::MemoryUrl => d.memory_url = text.trim().to_string(),
            Field::TtsModel => d.tts_model = text.trim().to_string(),
            Field::ProviderApiKey(i) => {
                let Some(p) = d.providers.get_mut(i) else {
                    return false;
                };
                // The editor text IS the secret — never trimmed (a key's
                // whitespace is the owner's), only a fully empty edit
                // collapses to an explicit clear. Everything else (cancel,
                // unrelated edits, reorder, remove) leaves this field
                // untouched.
                p.api_key = if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                };
            }
        }
        true
    }

    /// Append a blank provider row; returns its index so the UI can focus its
    /// name field.
    pub fn add_provider(&mut self) -> Option<usize> {
        let d = self.draft.as_mut()?;
        d.providers.push(ProviderDraft::blank());
        Some(d.providers.len() - 1)
    }

    pub fn remove_provider(&mut self, ix: usize) {
        if let Some(d) = self.draft.as_mut() {
            if ix < d.providers.len() {
                d.providers.remove(ix);
                self.field = None; // indices shifted — drop any in-progress edit
            }
        }
    }

    pub fn toggle_provider_enabled(&mut self, ix: usize) {
        if let Some(p) = self.draft.as_mut().and_then(|d| d.providers.get_mut(ix)) {
            p.enabled = !p.enabled;
        }
    }

    /// Cycle a provider's kind chip: ollama → anthropic → hermes → ollama.
    pub fn cycle_provider_kind(&mut self, ix: usize) {
        if let Some(p) = self.draft.as_mut().and_then(|d| d.providers.get_mut(ix)) {
            let pos = ProviderKind::ALL
                .iter()
                .position(|k| *k == p.kind)
                .unwrap_or(0);
            p.kind = ProviderKind::ALL[(pos + 1) % ProviderKind::ALL.len()];
        }
    }

    /// Pick a default-model dropdown option (`None` clears the default).
    #[allow(dead_code)] // SXe: now a catalog row (settings_live); kept for its tests.
    pub fn set_default_choice(&mut self, choice: Option<(String, String)>) {
        if let Some(d) = self.draft.as_mut() {
            match choice {
                Some((p, m)) => {
                    d.default_provider = Some(p);
                    d.default_model = Some(m);
                }
                None => {
                    d.default_provider = None;
                    d.default_model = None;
                }
            }
        }
        self.model_dropdown = false;
    }

    #[allow(dead_code)] // SXe: now a catalog row (settings_live); kept for its tests.
    pub fn toggle_recall(&mut self) {
        if let Some(d) = self.draft.as_mut() {
            d.memory_recall = !d.memory_recall;
        }
    }

    #[allow(dead_code)] // SXe: now a catalog row (settings_live); kept for its tests.
    pub fn toggle_write(&mut self) {
        if let Some(d) = self.draft.as_mut() {
            d.memory_write = !d.memory_write;
        }
    }

    /// N1: flip one `[notifications]` switch in the draft.
    #[allow(dead_code)] // SXg: the Notifications page applies live; kept for its tests.
    pub fn toggle_notify(&mut self, which: NotifyToggle) {
        if let Some(d) = self.draft.as_mut() {
            let n = &mut d.notify;
            let flag = match which {
                NotifyToggle::Enabled => &mut n.enabled,
                NotifyToggle::Approvals => &mut n.approvals,
                NotifyToggle::Finished => &mut n.finished,
                NotifyToggle::Failed => &mut n.failed,
                NotifyToggle::Sound => &mut n.sound,
            };
            *flag = !*flag;
        }
    }

    /// Step the VAD threshold up or down one notch on a LOG scale (spec §1:
    /// 0.001–0.1, log slider): 16 notches across the two decades, clamped.
    #[allow(dead_code)] // SXe: now a catalog row (settings_live); kept for its tests.
    pub fn vad_step(&mut self, up: bool) {
        const NOTCHES: f64 = 16.0;
        if let Some(d) = self.draft.as_mut() {
            let factor = (VAD_THRESHOLD_MAX / VAD_THRESHOLD_MIN).powf(1.0 / NOTCHES);
            let v = if up {
                d.vad_threshold * factor
            } else {
                d.vad_threshold / factor
            };
            d.vad_threshold = v.clamp(VAD_THRESHOLD_MIN, VAD_THRESHOLD_MAX);
        }
    }

    /// Step the silence-window stepper (spec §1: 10–60), clamped.
    #[allow(dead_code)] // SXe: now a catalog row (settings_live); kept for its tests.
    pub fn silence_step(&mut self, up: bool) {
        const STEP: u32 = 5;
        if let Some(d) = self.draft.as_mut() {
            d.vad_silence_windows = if up {
                (d.vad_silence_windows + STEP).min(VAD_SILENCE_WINDOWS_MAX)
            } else {
                d.vad_silence_windows
                    .saturating_sub(STEP)
                    .max(VAD_SILENCE_WINDOWS_MIN)
            };
        }
    }

    /// Validate the draft. `Err` is the inline message — nothing was written.
    fn validate_draft(d: &Draft) -> Result<(), String> {
        for (i, p) in d.providers.iter().enumerate() {
            let row = i + 1;
            if p.name.trim().is_empty() {
                return Err(format!("provider {row}: name must not be empty"));
            }
            if d.providers[..i]
                .iter()
                .any(|q| q.name.trim() == p.name.trim())
            {
                return Err(format!(
                    "provider {row}: duplicate name {:?}",
                    p.name.trim()
                ));
            }
            validate_url(p.base_url.trim())
                .map_err(|e| format!("provider {row} ({}): {e}", p.name.trim()))?;
        }
        if !d.memory_url.trim().is_empty() {
            validate_url(d.memory_url.trim()).map_err(|e| format!("memory service: {e}"))?;
        }
        if !(VAD_THRESHOLD_MIN..=VAD_THRESHOLD_MAX).contains(&d.vad_threshold) {
            return Err(format!("vad_threshold {} is out of range", d.vad_threshold));
        }
        if !(VAD_SILENCE_WINDOWS_MIN..=VAD_SILENCE_WINDOWS_MAX).contains(&d.vad_silence_windows) {
            return Err(format!(
                "vad_silence_windows {} is out of range",
                d.vad_silence_windows
            ));
        }
        Ok(())
    }

    /// Save the draft (the structured panel's Save button): validate the WHOLE
    /// draft first — any error surfaces inline and nothing reaches disk — then
    /// apply it to the document and write through the atomic gate. Returns
    /// `true` when the save landed; sets [`restart_notice`](Self::restart_notice)
    /// when provider wiring (name/kind/base_url, add/remove) changed (spec §1/§3).
    pub fn save_draft(&mut self) -> bool {
        self.field = None;
        self.model_dropdown = false;
        let Some(draft) = self.draft.clone() else {
            return false;
        };
        // SXe-FIX1: work on a copy. The in-memory document changes only when
        // the save lands, so a refused save leaves the cards unsaved.
        let Some(mut doc) = self.doc.clone() else {
            return false;
        };
        if let Err(msg) = Self::validate_draft(&draft) {
            self.error = Some(msg);
            return false;
        }

        let before = doc.config();
        let voice_before = VoiceConfig::parse(&doc.text()).unwrap_or_default();

        // Providers (the whole list — covers add/remove/edit/toggle).
        let mut providers: Vec<ProviderConfig> =
            draft.providers.iter().map(|p| p.to_config()).collect();
        // A key typed here goes to the OS secret store; the file keeps only
        // its `secret:` reference.
        if let Err(e) = crate::secret_store::secure_typed_provider_keys(
            &mut providers,
            &before,
            crate::secret_store::store(),
        ) {
            self.error = Some(format!("The key was not saved: {e}"));
            return false;
        }
        // Keys whose reference this save drops (provider deleted, key cleared):
        // removed from the secret store once the write has landed.
        let orphans = crate::secret_store::orphaned_provider_accounts(&before.providers, &providers);
        if let Err(e) = doc.set_providers(&providers) {
            self.error = Some(e.to_string());
            return false;
        }

        // Defaults: set or clear.
        let top = |key: &str| Target::TopLevel {
            key: key.to_string(),
        };
        let mem = |key: &str| Target::Table {
            table: "memory".into(),
            key: key.into(),
        };
        let voice = |key: &str| Target::Table {
            table: "voice".into(),
            key: key.into(),
        };
        let result = (|| -> Result<(), awili_config::ConfigError> {
            if draft.work_executor != before.work.default_executor {
                let target = Target::Table {
                    table: "work".into(),
                    key: "default_executor".into(),
                };
                match &draft.work_executor {
                    Some(value) => doc.set_string(&target, value)?,
                    None => doc.remove(&target)?,
                }
            }
            match &draft.default_provider {
                Some(p) => doc.set_string(&top("default_provider"), p)?,
                None => doc.remove(&top("default_provider"))?,
            }
            match &draft.default_model {
                Some(m) => doc.set_string(&top("default_model"), m)?,
                None => doc.remove(&top("default_model"))?,
            }
            // Memory + voice: only write what differs from the document, so an
            // untouched section is not spammed with explicit defaults.
            if draft.memory_url.trim() != before.memory.base_url {
                doc.set_string(&mem("base_url"), draft.memory_url.trim())?;
            }
            if draft.memory_recall != before.memory.recall {
                doc.set_bool(&mem("recall"), draft.memory_recall)?;
            }
            if draft.memory_write != before.memory.write {
                doc.set_bool(&mem("write"), draft.memory_write)?;
            }
            if draft.vad_threshold != voice_before.vad_threshold {
                doc.set_float(&voice("vad_threshold"), draft.vad_threshold)?;
            }
            if draft.vad_silence_windows != voice_before.vad_silence_windows {
                doc.set_integer(
                    &voice("vad_silence_windows"),
                    draft.vad_silence_windows as i64,
                )?;
            }
            let tts_before = voice_before.tts_model.unwrap_or_default();
            if draft.tts_model.trim() != tts_before {
                if draft.tts_model.trim().is_empty() {
                    doc.remove(&voice("tts_model"))?;
                } else {
                    doc.set_string(&voice("tts_model"), draft.tts_model.trim())?;
                }
            }
            // N1: each switch is written only when it differs from the
            // document, so an untouched section stays absent.
            let (now, was) = (&draft.notify, &before.notifications);
            for (key, v, old) in [
                ("enabled", now.enabled, was.enabled),
                ("approvals", now.approvals, was.approvals),
                ("finished", now.finished, was.finished),
                ("failed", now.failed, was.failed),
                ("sound", now.sound, was.sound),
            ] {
                if v != old {
                    let target = Target::Table {
                        table: "notifications".into(),
                        key: key.into(),
                    };
                    doc.set_bool(&target, v)?;
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.error = Some(e.to_string());
            return false;
        }

        // The copy is valid now; hand the write to the persistence gate and
        // keep the copy only when the write lands. An existing
        // config keeps the unchanged save; a still-missing first save
        // publishes create-only and stops if a config appeared meanwhile.
        let text = doc.text();
        match self.persist(&text) {
            PersistOutcome::Landed => {
                crate::secret_store::delete_accounts(&orphans, crate::secret_store::store());
                self.doc = Some(doc);
                let wiring = |ps: &[ProviderConfig]| -> Vec<(String, ProviderKind, String)> {
                    ps.iter()
                        .map(|p| (p.name.clone(), p.kind, p.base_url.clone()))
                        .collect()
                };
                self.restart_notice = (wiring(&before.providers) != wiring(&providers))
                    .then(|| RESTART_NOTICE.to_string());
                self.set_saved();
                self.refresh_draft();
                true
            }
            _ => false,
        }
    }

    /// SXe: the model-service cards differ from the file (unsaved work).
    pub fn services_dirty(&self) -> bool {
        match (&self.draft, &self.doc) {
            (Some(d), Some(doc)) => d.providers != Draft::from_doc(doc).providers,
            _ => false,
        }
    }

    /// SXe: throw the unsaved model-service cards away (the file is untouched).
    pub fn revert_services(&mut self) {
        if let (Some(d), Some(doc)) = (self.draft.as_mut(), self.doc.as_ref()) {
            d.providers = Draft::from_doc(doc).providers;
            self.field = None;
        }
        self.confirm_close = false;
    }

    /// Test the full-draft cancellation behavior: drop the draft and close
    /// without saving.
    #[cfg(test)]
    pub fn discard(&mut self) {
        self.draft = None;
        self.close();
    }
}

/// How an ordered candidate scan resolved (FIRST-RUN-SPEC 2026-09-06).
enum ResolvedConfig {
    /// No candidate has any directory entry: a genuine first run.
    Absent,
    /// The first candidate that has ANY existing entry — file, directory,
    /// symlink, including a dangling one.
    Existing(PathBuf),
    /// A metadata error OTHER than `NotFound` on a candidate (permission
    /// denied, …): surfaced, never treated as absence.
    MetadataError(PathBuf, std::io::Error),
}

/// What the persistence gate did; the save entry points map it to their
/// `bool` result and UI bookkeeping.
enum PersistOutcome {
    /// The write landed at the save path.
    Landed,
    /// A candidate appeared (or raced the create): the stale save stopped
    /// with no success signal, and the existing document — or its load
    /// error — was reloaded for review. Nothing was written.
    Appeared,
    /// The write (or a metadata recheck) failed; the error is in
    /// [`SettingsState::error`].
    Failed,
}

/// Whether edits to `section` (a [`SettingEntry::section`] label like
/// `"voice"` or `"providers[0]"`) only take effect on relaunch. Providers,
/// the defaults, and `[memory]` re-apply live; `[voice]` and `[feed]` spin up
/// workers once at startup, so they're tagged "restart required" (spec §3).
pub fn restart_required(section: &str) -> bool {
    let base = section.split('[').next().unwrap_or(section);
    matches!(base, "voice" | "feed")
}

/// The mask shown in place of a secret value in the Raw TOML editor —
/// deliberately identical to the All Fields pane's mask (D1: the two panes
/// disagreeing about secrecy was the finding).
pub const SECRET_MASK: &str = "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}";

/// Keys whose quoted values never render in the Raw TOML editor.
fn is_secret_key(key: &str) -> bool {
    matches!(
        key,
        "api_key" | "token" | "write_token" | "password" | "secret" | "client_secret" | "bearer"
    )
}

/// Replace every secret value in `text` with [`SECRET_MASK`], returning the
/// masked text and the hidden originals `(key, value)` in file order. Line
/// based and deliberately conservative: only single-line `key = "basic
/// string"` shapes are masked — a secret in an exotic TOML shape stays
/// visible rather than risking a corrupting rewrite.
pub fn redact_secrets(text: &str) -> (String, Vec<(String, String)>) {
    let mut hidden: Vec<(String, String)> = Vec::new();
    let masked: Vec<String> = text
        .lines()
        .map(|line| match split_secret_line(line) {
            Some((prefix, key, value, suffix)) => {
                hidden.push((key.to_string(), value.to_string()));
                format!("{prefix}\"{SECRET_MASK}\"{suffix}")
            }
            None => line.to_string(),
        })
        .collect();
    (masked.join("\n"), hidden)
}

/// The fixed, credential-free message returned when a raw-editor save cannot
/// safely restore the hidden secrets. Never embeds secret or raw text.
pub const RAW_SECRET_RESTORE_ERROR: &str =
    "Hidden credentials cannot be safely restored after Raw edits. Use General for ordinary changes, or replace or remove every masked value before saving.";

/// Put the hidden originals back into the EDITED text — or refuse to touch
/// disk, fail-closed.
///
/// TEMPORARY POLICY (fail-closed, snapshot-bound): restoration is bound to
/// the EXACT masked editor snapshot captured at sync time.
///
/// - `edited == snapshot` (untouched): the hidden originals are restored with
///   the existing per-key-name occurrence-index mechanics.
/// - `edited` contains no [`SECRET_MASK`]: returned verbatim, WITHOUT any
///   hidden-secret substitution — the user deliberately supplied or removed
///   every value.
/// - `edited` contains the mask but differs from the snapshot (any changed
///   line, inserted/unresolved mask, deleted masked line, reordered or
///   removed provider block): [`Err(RAW_SECRET_RESTORE_ERROR)`].
///   The conservative whole-document snapshot binding intentionally rejects
///   even comment, whitespace, or non-secret raw edits while masked values
///   remain — pairing masked lines to the hidden originals by text identity is
///   unsafe once the document shape has changed, and refusing is deliberate.
///   An inserted or residual literal mask is never accepted either.
///
/// The returned string never contains a residual mask.
pub fn restore_secrets(
    edited: &str,
    hidden: &[(String, String)],
    snapshot: &str,
) -> Result<String, &'static str> {
    if edited.contains(SECRET_MASK) {
        if edited != snapshot {
            // The document changed while masked values were visible: any
            // substitution could attach the wrong credential to the wrong
            // key. Refuse before validation or write (fail-closed).
            return Err(RAW_SECRET_RESTORE_ERROR);
        }
        // Untouched exact snapshot: restore every mask by the line's
        // OCCURRENCE INDEX among same-named secret lines — an untouched
        // second `api_key` restores the SECOND original, never its
        // neighbour's. A snapshot with no original for a mask (not
        // producible by [`redact_secrets`]) is rejected as residual.
        let mut seen: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        let restored: Vec<String> = edited
            .lines()
            .map(|line| match split_secret_line(line) {
                Some((prefix, key, value, suffix)) => {
                    let n = {
                        let c = seen.entry(key).or_insert(0);
                        let n = *c;
                        *c += 1;
                        n
                    };
                    if value != SECRET_MASK {
                        return Ok(line.to_string());
                    }
                    match hidden.iter().filter(|(k, _)| k == key).nth(n) {
                        Some((_, original)) => Ok(format!("{prefix}\"{original}\"{suffix}")),
                        // Residual mask with no matching original: reject
                        // rather than inventing or leaving a literal mask.
                        None => Err(RAW_SECRET_RESTORE_ERROR),
                    }
                }
                None => Ok(line.to_string()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Reject any residual literal mask that the line split did not catch
        // (exotic shapes): no mask may survive into the write.
        if restored.join("\n").contains(SECRET_MASK) {
            return Err(RAW_SECRET_RESTORE_ERROR);
        }
        return Ok(restored.join("\n"));
    }
    // No mask in the edited text: the user explicitly supplied or removed
    // every value. Return their text verbatim — NEVER substitute hidden
    // originals over it.
    Ok(edited.to_string())
}

/// Split a `key = "value"` line whose key is secret into
/// `(up-to-the-opening-quote, key, value, after-the-closing-quote)`.
/// `None` for non-secret keys, non-string values, multi-string lines, or
/// values containing escapes (conservatism over cleverness).
fn split_secret_line(line: &str) -> Option<(&str, &str, &str, &str)> {
    let eq = line.find('=')?;
    let key = line[..eq].trim();
    if !is_secret_key(key) {
        return None;
    }
    let after_eq = &line[eq + 1..];
    let open_rel = after_eq.find('"')?;
    if !after_eq[..open_rel].trim().is_empty() {
        return None; // something other than whitespace between = and the quote
    }
    let open = eq + 1 + open_rel;
    let rest = &line[open + 1..];
    let close_rel = rest.find('"')?;
    let value = &rest[..close_rel];
    if value.contains('\\') {
        return None; // escaped content — leave untouched
    }
    let suffix = &rest[close_rel + 1..];
    if suffix.contains('"') {
        return None; // arrays / inline tables with more strings: out of scope
    }
    Some((&line[..open], key, value, suffix))
}

#[cfg(test)]
mod tests {

    #[test]
    fn harness_recipe_saves_without_creating_http_providers_or_credentials() {
        let mut state = state_with_file("providers = []\n[work]\ndefault_executor = \"gateway\"\n");
        state.open();
        for executor in ["agent:claude", "agent:codex", "agent:glm"] {
            state.draft.as_mut().unwrap().work_executor = Some(executor.into());
            assert!(state.save_draft(), "{:?}", state.error);
            let text = std::fs::read_to_string(state.path()).unwrap();
            let cfg = Config::parse(&text).unwrap();
            assert_eq!(cfg.work.default_executor.as_deref(), Some(executor));
            assert!(cfg.providers.is_empty());
            assert!(!text.contains("api_key"));
        }
        state.draft.as_mut().unwrap().work_executor = Some("agent:claude".into());
        state.close();
        state.open();
        assert_eq!(
            state.draft.as_ref().unwrap().work_executor.as_deref(),
            Some("agent:glm")
        );
    }

    // ---- D1: raw-editor secret redaction -----------------------------------

    const D1_CFG: &str = "[hermes]\nbase_url = \"http://x:8642/v1\"\napi_key = \"real-secret-A\"\n\n[[providers]]\nname = \"p\"\napi_key = \"real-secret-B\"\nmodel = \"m\"\n";

    #[test]
    fn secrets_are_masked_and_restored_verbatim_when_untouched() {
        let (masked, hidden) = redact_secrets(D1_CFG);
        assert!(!masked.contains("real-secret-A"));
        assert!(!masked.contains("real-secret-B"));
        assert_eq!(masked.matches(SECRET_MASK).count(), 2);
        assert_eq!(hidden.len(), 2);
        // Non-secret values are untouched.
        assert!(masked.contains("http://x:8642/v1"));
        // Untouched exact snapshot restores the file byte-for-byte (mod trailing \n).
        let restored = restore_secrets(&masked, &hidden, &masked).unwrap();
        assert_eq!(restored.trim_end(), D1_CFG.trim_end());
    }

    /// The user REPLACES EVERY masked value with an explicit one: the edited
    /// text must pass through verbatim, with NO hidden credential spliced
    /// back in (the user intentionally supplied or removed every value).
    #[test]
    fn replacing_all_masks_with_explicit_values_passes_through_without_substitution() {
        let (masked, hidden) = redact_secrets(D1_CFG);
        let edited = masked.replace(SECRET_MASK, "explicit-user-value");
        assert!(!edited.contains(SECRET_MASK));
        let restored = restore_secrets(&edited, &hidden, &masked).unwrap();
        assert_eq!(restored, edited, "edited text passes through unchanged");
        assert!(!restored.contains("real-secret-A"));
        assert!(!restored.contains("real-secret-B"));
    }

    /// The user DELETES every masked line: passes through verbatim, no
    /// substitution of the hidden originals.
    #[test]
    fn deleting_all_masked_lines_passes_through_without_substitution() {
        let (masked, hidden) = redact_secrets(D1_CFG);
        let edited: String = masked
            .lines()
            .filter(|l| !l.contains(SECRET_MASK))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!edited.contains(SECRET_MASK));
        let restored = restore_secrets(&edited, &hidden, &masked).unwrap();
        assert_eq!(restored, edited, "edited text passes through unchanged");
        assert!(!restored.contains("real-secret-A"));
        assert!(!restored.contains("real-secret-B"));
    }

    /// Two-provider fixture: valid TOML with TWO `[[providers]]` blocks
    /// (synthetic distinct credentials), for the deleted-provider and
    /// reordering regressions.
    const D2_CFG: &str =
        "[hermes]\nbase_url = \"http://x:8642/v1\"\napi_key = \"synth-secret-A\"\n\n\
                   [[providers]]\nname = \"pA\"\napi_key = \"synth-secret-B\"\nmodel = \"m\"\n\n\
                   [[providers]]\nname = \"pB\"\napi_key = \"synth-secret-C\"\nmodel = \"n\"\n";

    /// Regression: deleting the FIRST [[providers]] block must NOT assign
    /// provider A's hidden credential to provider B's key — the save is
    /// refused (fail-closed) instead of silently cross-wiring secrets.
    /// Deleting the first block removes ONLY that block: the second
    /// `[[providers]]` header and its mask both remain.
    #[test]
    fn deleting_first_provider_block_with_two_keys_refuses_instead_of_cross_wiring() {
        let (masked, hidden) = redact_secrets(D2_CFG);
        assert_eq!(masked.matches(SECRET_MASK).count(), 3);
        // Delete the first provider block (the one whose key would otherwise
        // consume occurrence index 0 and shift A's credential onto p's key).
        let mut lines = masked.lines().collect::<Vec<_>>();
        // The first [[providers]] block spans from its header to the blank
        // line before the second block.
        let start = lines.iter().position(|l| *l == "[[providers]]").unwrap();
        let end = lines
            .iter()
            .skip(start + 1)
            .position(|l| *l == "[[providers]]")
            .unwrap();
        lines.drain(start..start + 1 + end); // first header .. second header
        let edited = lines.join("\n");
        assert!(edited.contains(SECRET_MASK), "a mask still remains");
        assert_eq!(
            edited.matches(SECRET_MASK).count(),
            2,
            "second provider mask retained"
        );
        assert!(edited.contains("[[providers]]"), "second header retained");
        assert_ne!(edited, masked);
        let err = restore_secrets(&edited, &hidden, &masked).unwrap_err();
        assert_eq!(*err, *RAW_SECRET_RESTORE_ERROR);
        assert!(!err.contains("synth-secret"));
    }

    /// Regression: reordering the provider blocks while masks remain is
    /// refused — occurrence-index pairing cannot be trusted after reordering.
    /// The whole two-provider block (both real `[[providers]]` sections) is
    /// moved before the [hermes] section.
    #[test]
    fn reordering_providers_with_masks_refused() {
        let (masked, hidden) = redact_secrets(D2_CFG);
        let lines = masked.lines().collect::<Vec<_>>();
        // The whole two-provider region (from the first [[providers]] header
        // to the end) is swapped before the [hermes] section.
        let head: String = lines
            .iter()
            .take_while(|l| **l != "[[providers]]")
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        // From the first header to the end = the entire two-provider block.
        let block: String = lines
            .iter()
            .skip_while(|l| **l != "[[providers]]")
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let edited = format!("{block}\n{head}");
        assert_ne!(edited, masked);
        assert!(edited.contains(SECRET_MASK));
        let err = restore_secrets(&edited, &hidden, &masked).unwrap_err();
        assert_eq!(*err, *RAW_SECRET_RESTORE_ERROR);
        assert!(!err.contains("synth-secret"));
    }

    /// Regression: a same-count base_url change with masks still present is
    /// refused — even non-secret edits cannot be trusted while masked values
    /// remain in the document (temporary whole-document snapshot binding).
    #[test]
    fn same_count_base_url_change_with_masks_refused() {
        let (masked, hidden) = redact_secrets(D1_CFG);
        let edited = masked.replace("http://x:8642/v1", "http://other:9999/v1");
        assert_ne!(edited, masked);
        assert!(edited.contains(SECRET_MASK));
        let err = restore_secrets(&edited, &hidden, &masked).unwrap_err();
        assert_eq!(*err, *RAW_SECRET_RESTORE_ERROR);
        assert!(!err.contains("real-secret"));
    }

    /// Regression: inserting a new masked line (an unresolved mask the user
    /// typed or copied) is never accepted — even when the mask would have a
    /// same-key occurrence-index match.
    #[test]
    fn inserting_a_masked_line_refused() {
        let (masked, hidden) = redact_secrets(D1_CFG);
        let edited = format!("{masked}\nextra_api_key = \"{SECRET_MASK}\"\n");
        assert_ne!(edited, masked);
        let err = restore_secrets(&edited, &hidden, &masked).unwrap_err();
        assert_eq!(*err, *RAW_SECRET_RESTORE_ERROR);
        assert!(!err.contains("real-secret"));
    }

    /// Regression: deleting ONE masked key while another mask remains is
    /// refused — the surviving mask must not silently absorb a deleted key's
    /// credential.
    #[test]
    fn deleting_one_masked_key_with_another_mask_remaining_refused() {
        let (masked, hidden) = redact_secrets(D1_CFG);
        // Delete the FIRST masked line only (the [hermes] api_key); the
        // [[providers]] mask survives.
        let mut out = Vec::new();
        let mut dropped_first = false;
        for l in masked.lines() {
            if !dropped_first && l.contains(SECRET_MASK) {
                dropped_first = true;
                continue;
            }
            out.push(l.to_string());
        }
        let edited = out.join("\n");
        assert_ne!(edited, masked);
        assert_eq!(edited.matches(SECRET_MASK).count(), 1, "one mask remains");
        let err = restore_secrets(&edited, &hidden, &masked).unwrap_err();
        assert_eq!(*err, *RAW_SECRET_RESTORE_ERROR);
        assert!(!err.contains("synth-secret"));
    }

    /// Unmatched snapshot mask: the edited text is byte-identical to the
    /// snapshot and contains a mask, but the hidden list is empty or has no
    /// entry for the masked key — restoration must Err with the fixed
    /// message (fail-closed: a mask with no original is never substituted,
    /// never left as a literal, never silently dropped) and the message must
    /// contain no credential strings.
    #[test]
    fn unmatched_mask_in_exact_snapshot_is_rejected_without_secret_in_error() {
        let snapshot = format!("[hermes]\napi_key = \"{SECRET_MASK}\"\n");
        // (a) Empty hidden list.
        let err = restore_secrets(&snapshot, &[], &snapshot).unwrap_err();
        assert_eq!(*err, *RAW_SECRET_RESTORE_ERROR);
        // (b) Hidden list exists but has no entry for the masked key.
        let hidden = vec![("other_key".to_string(), "other-value".to_string())];
        let err = restore_secrets(&snapshot, &hidden, &snapshot).unwrap_err();
        assert_eq!(*err, *RAW_SECRET_RESTORE_ERROR);
        assert!(!err.contains("api_key"));
        assert!(!err.contains(SECRET_MASK));
        assert!(!err.contains("other-value"));
    }

    /// The fixed error must never contain sentinel secret strings.
    #[test]
    fn restore_error_never_contains_secret_strings() {
        assert!(!RAW_SECRET_RESTORE_ERROR.contains("real-secret"));
        assert!(!RAW_SECRET_RESTORE_ERROR.contains("sentinel"));
        assert!(!RAW_SECRET_RESTORE_ERROR.contains(SECRET_MASK));
    }

    /// Save-boundary regression against the PRODUCTION helper `main` uses:
    /// a refused raw restoration must not touch the disk. The user edits the
    /// masked document by deleting ONLY the first `[[providers]]` block —
    /// the second `[[providers]]` header and its mask remain. The guard
    /// refuses inside `commit_raw_editor` BEFORE `commit_raw`, so the on-disk
    /// config is byte-identical to before, the backup is absent (or left
    /// byte-identical to its preexisting sentinel bytes), and the fixed error
    /// surfaces.
    #[test]
    fn rejected_raw_restoration_writes_nothing_to_disk() {
        // Two VALID [[providers]] blocks with synthetic distinct keys/URLs.
        let text = "default_provider = \"pA\"\n\
                    [[providers]]\nname = \"pA\"\nkind = \"openai\"\nbase_url = \"http://a.example:1111\"\n\
                    api_key = \"synth-key-A-000\"\n\
                    [[providers]]\nname = \"pB\"\nkind = \"openai\"\nbase_url = \"http://b.example:2222\"\n\
                    api_key = \"synth-key-B-001\"\n";
        let mut s = state_with_file(text);
        s.open();
        let pre_bak = bak(s.path());
        let pre_bak_bytes = if pre_bak.exists() {
            Some(std::fs::read(&pre_bak).unwrap())
        } else {
            None
        };
        let (masked, hidden) = redact_secrets(text);
        assert_eq!(
            masked.matches(SECRET_MASK).count(),
            2,
            "both provider keys masked"
        );
        s.raw_redactions = hidden;
        s.raw_masked_snapshot = masked.clone();
        // The user edited the masked document: removed ONLY the whole first
        // [[providers]] block, retaining the second header and its mask.
        let mut lines = masked.lines().collect::<Vec<_>>();
        let start = lines.iter().position(|l| *l == "[[providers]]").unwrap();
        let end = lines
            .iter()
            .skip(start + 1)
            .position(|l| *l == "[[providers]]")
            .unwrap();
        lines.drain(start..start + 1 + end);
        let edited = lines.join("\n");
        assert!(
            edited.contains(SECRET_MASK),
            "second provider mask retained"
        );
        assert_eq!(edited.matches(SECRET_MASK).count(), 1);
        assert!(edited.contains("[[providers]]"), "second header retained");
        assert_ne!(edited, masked);
        // The production helper refuses and touches nothing.
        assert!(
            !s.commit_raw_editor(&edited),
            "commit_raw_editor must refuse"
        );
        let on_disk = std::fs::read_to_string(s.path()).unwrap();
        assert_eq!(
            on_disk, text,
            "disk must be unchanged after the refused save"
        );
        // The refusal must not have created or rewritten a backup.
        match pre_bak_bytes {
            None => assert!(!pre_bak.exists(), "refusal must not create a backup"),
            Some(bytes) => assert_eq!(
                std::fs::read(&pre_bak).unwrap(),
                bytes,
                "refusal must not alter the preexisting backup"
            ),
        }
        assert_eq!(s.error.as_deref(), Some(RAW_SECRET_RESTORE_ERROR));
    }

    #[test]
    fn exotic_shapes_are_left_visible_not_corrupted() {
        // Escapes, arrays, and non-string values are out of scope by design.
        let cfg = "api_key = \"has\\\"quote\"\ntokens = [\"a\", \"b\"]\nport = 8080\n";
        let (masked, hidden) = redact_secrets(cfg);
        assert_eq!(masked.trim_end(), cfg.trim_end());
        assert!(hidden.is_empty());
    }
    use super::*;

    const VALID: &str = "default_provider = \"ollama\" # keep\n\n[memory]\nbase_url = \"http://127.0.0.1:8400\"\nmin_priority = 90\nrecall = true\nwrite = true\n";

    /// A unique config path in a fresh temp dir per test.
    fn temp_path() -> PathBuf {
        // RC2-APP (K-flake2): under this test process's own scratch root,
        // which also sweeps what ended test processes left (an empty folder
        // every time, as RC1-K required).
        crate::test_tmp::fresh_dir("settings").join("config.toml")
    }

    fn state_with_file(text: &str) -> SettingsState {
        let path = temp_path();
        std::fs::write(&path, text).unwrap();
        let mut s = SettingsState::with_path(path);
        s.doc = Some(ConfigDoc::parse(text).unwrap());
        s
    }

    fn bak(path: &Path) -> PathBuf {
        let mut p = path.as_os_str().to_owned();
        p.push(".bak");
        PathBuf::from(p)
    }

    // ---- H3: structured settings must not delete provider api keys ---------

    /// A configured provider `api_key` must survive editing an unrelated
    /// structured field (here, the provider's base_url) through the full
    /// open → edit draft → save_draft → disk path.
    #[test]
    fn provider_api_key_survives_unrelated_edit() {
        let text = "default_provider = \"openai\"\n\n\
            [memory]\nbase_url = \"http://127.0.0.1:8400\"\nmin_priority = 90\n\n\
            [[providers]]\nname = \"openai\"\nkind = \"openai\"\n\
            base_url = \"http://old.example:1234\"\nmodels = [\"gpt-x\"]\n\
            api_key = \"secret-key-123\"\n";
        let mut s = state_with_file(text);
        s.open();

        // Edit an UNRELATED field — the bug wiped the key on any such save.
        let draft = s.draft.as_mut().expect("draft built on open");
        assert_eq!(draft.providers.len(), 1);
        assert_eq!(
            draft.providers[0].api_key.as_deref(),
            Some("secret-key-123")
        );
        draft.providers[0].base_url = "http://new.example:5678".to_string();

        assert!(s.save_draft(), "save must land: {:?}", s.error);

        // Re-read from disk: the key survived and the edit applied.
        let on_disk = std::fs::read_to_string(s.path()).unwrap();
        let cfg = Config::parse(&on_disk).expect("saved config parses");
        let p = cfg
            .providers
            .iter()
            .find(|p| p.name == "openai")
            .expect("provider present");
        assert_eq!(
            p.api_key.as_deref(),
            Some("secret-key-123"),
            "api_key must NOT be wiped by an unrelated structured edit"
        );
        assert_eq!(p.base_url, "http://new.example:5678", "the edit must apply");
    }

    // ---- H4 completion: the draft must not leak the key H3 added ------------

    /// The `api_key` the H3 fix added to `ProviderDraft` must not surface in
    /// `Debug` — neither on the row itself nor through the whole-`Draft` dump
    /// (which holds a `Vec<ProviderDraft>`). Built from a real on-disk config so
    /// the key arrives by the same `from_config` path the panel uses.
    #[test]
    fn draft_debug_redacts_provider_api_key() {
        const SECRET: &str = "sk-SECRET123";
        let text = format!(
            "default_provider = \"openai\"\n\n\
             [memory]\nbase_url = \"http://127.0.0.1:8400\"\nmin_priority = 90\n\n\
             [[providers]]\nname = \"openai\"\nkind = \"openai\"\n\
             base_url = \"http://api.example:1234\"\nmodels = [\"gpt-x\"]\n\
             api_key = \"{SECRET}\"\n"
        );
        let mut s = state_with_file(&text);
        s.open();

        let draft = s.draft.as_ref().expect("draft built on open");
        let row = &draft.providers[0];
        assert_eq!(
            row.api_key.as_deref(),
            Some(SECRET),
            "the key is carried into the draft"
        );

        // The leaf row redacts in isolation...
        let row_dump = format!("{row:?}");
        assert!(
            !row_dump.contains(SECRET),
            "ProviderDraft Debug leaked the api_key: {row_dump}"
        );
        assert!(
            row_dump.contains("redacted"),
            "redaction marker must be present: {row_dump}"
        );

        // ...and so does the whole-draft dump that transitively prints it.
        let draft_dump = format!("{draft:?}");
        assert!(
            !draft_dump.contains(SECRET),
            "Draft Debug leaked a provider api_key: {draft_dump}"
        );
    }

    // ---- H5: the provider key row (add / edit / clear / cancel / mask) ------
    //
    // The panel edits a provider's `api_key` through the shared inline editor
    // (`Field::ProviderApiKey(ix)`), exactly like its other fields: click the
    // masked value → plaintext editor → Return commits into the DRAFT (memory),
    // Escape cancels, and the Save button persists through `save_draft`.
    //
    // The `KEY_*` fixtures pair a provider that HAS a configured `api_key`
    // (index 0) with one that does NOT (index 1), so every test can assert
    // both the edited row and — for the preservation contract — the sibling
    // row's key state in the same fixture.

    const KEY_BOTH: &str = "\
default_provider = \"ollama\" # keep
[memory]
base_url = \"http://127.0.0.1:8400\"
min_priority = 90

[[providers]]
name = \"anth\"
kind = \"anthropic\"
base_url = \"http://anth.example:8080\"
models = [\"claude-x\"]
api_key = \"sk-OLD-000\"

[[providers]]
name = \"open\"
kind = \"openai\"
base_url = \"http://open.example:8080\"
models = [\"gpt-x\"]
";

    const KEY_NONE: &str = "\
default_provider = \"ollama\" # keep
[memory]
base_url = \"http://127.0.0.1:8400\"
min_priority = 90

[[providers]]
name = \"anth\"
kind = \"anthropic\"
base_url = \"http://anth.example:8080\"
models = [\"claude-x\"]
";

    /// M4: a typed key is on disk only as `secret:provider:<name>`, and the
    /// (test-build, in-memory) secret store holds the real value verbatim.
    fn assert_key_filed(on_disk_key: Option<&str>, name: &str, key: &str) {
        assert_eq!(on_disk_key, Some(format!("secret:provider:{name}").as_str()));
        assert_eq!(
            crate::secret_store::store()
                .get(&format!("provider:{name}"))
                .unwrap()
                .as_deref(),
            Some(key)
        );
    }

    /// Committing a non-empty key lands in the draft (memory only) and, once
    /// saved, on disk — on a provider row that had NO `api_key` before.
    #[test]
    fn provider_key_commit_adds_the_key_to_disk() {
        const NEW: &str = "sk-NEW-123";
        let mut s = state_with_file(KEY_NONE);
        s.open();
        s.begin_field(Field::ProviderApiKey(0));
        assert!(
            s.commit_draft_field(NEW),
            "a non-empty key commit must land in the draft"
        );
        // Memory only: nothing touches disk before save_draft.
        let on_disk = std::fs::read_to_string(s.path()).unwrap();
        assert!(!on_disk.contains(NEW), "the key must not hit disk pre-save");
        assert!(s.save_draft(), "save must land: {:?}", s.error);

        let cfg = Config::parse(&std::fs::read_to_string(s.path()).unwrap())
            .expect("saved config parses");
        let p = cfg
            .providers
            .iter()
            .find(|p| p.name == "anth")
            .expect("provider present");
        assert_key_filed(p.api_key.as_deref(), "anth", NEW);
    }

    /// Editing an EXISTING key replaces it on disk; the provider's other
    /// fields round-trip untouched.
    #[test]
    fn provider_key_commit_replaces_the_existing_key() {
        const OLD: &str = "sk-OLD-000";
        const NEW: &str = "sk-NEW-123";
        let mut s = state_with_file(KEY_BOTH);
        s.open();
        assert_eq!(
            s.field_text(Field::ProviderApiKey(0)),
            OLD,
            "the editor must prefill with the current key"
        );
        s.begin_field(Field::ProviderApiKey(0));
        assert!(s.commit_draft_field(NEW));
        assert!(s.save_draft(), "save must land: {:?}", s.error);

        let on_disk = std::fs::read_to_string(s.path()).unwrap();
        let cfg = Config::parse(&on_disk).expect("saved config parses");
        let p = cfg
            .providers
            .iter()
            .find(|p| p.name == "anth")
            .expect("provider present");
        assert_key_filed(p.api_key.as_deref(), "anth", NEW);
        assert!(
            !on_disk.contains(OLD),
            "the old key must not survive the edit"
        );
        assert_eq!(
            p.base_url, "http://anth.example:8080",
            "unrelated fields survive"
        );
        assert_eq!(p.models, vec!["claude-x".to_string()]);
    }

    /// A fully empty commit is the EXPLICIT clear: `api_key` becomes `None`
    /// and the line disappears from disk. Whitespace-only is NOT a clear —
    /// it commits as a (weird but verbatim) value.
    #[test]
    fn provider_key_commit_empty_is_an_explicit_clear() {
        let mut s = state_with_file(KEY_BOTH);
        s.open();
        s.begin_field(Field::ProviderApiKey(0));
        assert!(s.commit_draft_field(""), "the explicit clear must commit");
        assert_eq!(
            s.draft.as_ref().unwrap().providers[0].api_key.as_ref(),
            None,
            "an empty edit collapses to an explicit clear"
        );
        assert!(s.save_draft(), "save must land: {:?}", s.error);

        let on_disk = std::fs::read_to_string(s.path()).unwrap();
        let cfg = Config::parse(&on_disk).expect("saved config parses");
        let p = cfg
            .providers
            .iter()
            .find(|p| p.name == "anth")
            .expect("provider present");
        assert_eq!(
            p.api_key.as_ref(),
            None,
            "the explicit clear must drop api_key from disk"
        );
        assert!(
            !on_disk.contains("sk-OLD-000"),
            "the cleared key must not remain on disk"
        );
    }

    /// Cancel (Escape) restores the masked row: the key — Some OR None — is
    /// exactly what it was before the edit session, in memory.
    #[test]
    fn provider_key_cancel_preserves_both_some_and_none() {
        // Some: a configured key survives a cancelled edit session.
        let mut s = state_with_file(KEY_BOTH);
        s.open();
        s.begin_field(Field::ProviderApiKey(0));
        // The editor is up with the raw secret (by contract, plaintext while
        // explicitly editing); cancel must restore the masked idle state and
        // touch nothing in the draft.
        assert_eq!(s.field_text(Field::ProviderApiKey(0)), "sk-OLD-000");
        s.cancel_field();
        let d = s.draft.as_ref().unwrap();
        assert!(s.field.is_none(), "cancel closes the editor");
        assert_eq!(
            d.providers[0].api_key.as_deref(),
            Some("sk-OLD-000"),
            "a cancelled edit must not change a configured key"
        );

        // None: a provider without a key stays keyless through cancel.
        let mut s = state_with_file(KEY_BOTH);
        s.open();
        s.begin_field(Field::ProviderApiKey(1));
        assert_eq!(
            s.field_text(Field::ProviderApiKey(1)),
            "",
            "a keyless provider prefills the editor with the empty string"
        );
        s.cancel_field();
        assert_eq!(
            s.draft.as_ref().unwrap().providers[1].api_key.as_ref(),
            None,
            "a cancelled edit must not create a key"
        );
    }

    /// Editing an UNRELATED field must not trim, replace, or touch either
    /// provider's key — Some values and None alike. (The key arm of
    /// `commit_draft_field` is deliberately untrimmed; this is the read-side
    // guarantee for the panel's "never trim hidden secrets" contract.)
    #[test]
    fn provider_key_edit_of_an_unrelated_field_preserves_both_keys() {
        let mut s = state_with_file(KEY_BOTH);
        s.open();
        s.begin_field(Field::ProviderName(0));
        assert!(s.commit_draft_field("anth-rename"));
        s.begin_field(Field::ProviderUrl(1));
        assert!(s.commit_draft_field("http://open.example:9999"));

        let d = s.draft.as_ref().unwrap();
        assert_eq!(
            d.providers[0].api_key.as_deref(),
            Some("sk-OLD-000"),
            "renaming the provider must not touch its key"
        );
        assert_eq!(
            d.providers[1].api_key.as_ref(),
            None,
            "url-editing the sibling provider must not create a key"
        );
        assert!(s.save_draft(), "save must land: {:?}", s.error);
    }

    /// A committed key round-trips through save → close → open: the fresh
    /// draft (rebuilt from disk) carries it, so `field_text` — the editor
    /// prefill — returns it on the next explicit edit.
    #[test]
    fn provider_key_survives_save_and_reopen() {
        const NEW: &str = "sk-ROUNDTRIP-1";
        let mut s = state_with_file(KEY_NONE);
        s.open();
        s.begin_field(Field::ProviderApiKey(0));
        assert!(s.commit_draft_field(NEW));
        assert!(s.save_draft(), "save must land: {:?}", s.error);

        s.close();
        s.open();
        // The file holds only the reference; the key itself is never shown.
        assert_key_filed(
            s.draft.as_ref().unwrap().providers[0].api_key.as_deref(),
            "anth",
            NEW,
        );
        assert_eq!(
            s.field_text(Field::ProviderApiKey(0)),
            "secret:provider:anth",
            "the editor shows the reference on reopen, not the key"
        );
    }

    /// A failed save (here: the draft violates the name-uniqueness rule)
    /// leaves the on-disk file byte-identical — the committed-but-unsaved key
    /// must not leak through a rejected write.
    #[test]
    fn provider_key_failed_save_leaves_disk_byte_unchanged() {
        const NEW: &str = "sk-UNSAVED-999";
        let mut s = state_with_file(KEY_BOTH);
        s.open();
        s.begin_field(Field::ProviderApiKey(0));
        assert!(s.commit_draft_field(NEW));
        // Make the save fail: duplicate the provider's name.
        s.begin_field(Field::ProviderName(1));
        assert!(s.commit_draft_field("anth"));
        assert!(!s.save_draft(), "the duplicate-name draft must be rejected");
        assert!(s.error.is_some(), "the validation error must surface");
        assert_eq!(
            std::fs::read_to_string(s.path()).unwrap(),
            KEY_BOTH,
            "a rejected save must leave the file byte-identical"
        );
        assert!(
            !bak(s.path()).exists(),
            "a rejected save must not produce a .bak"
        );
        // The key stays committed in the draft (the editor's contract is
        // memory-first; only the write is refused).
        assert_eq!(
            s.draft.as_ref().unwrap().providers[0].api_key.as_deref(),
            Some(NEW)
        );
    }

    /// The key row's MASKED idle display must never contain the raw secret,
    /// and neither may any `Debug` of the draft/row. (The UI builds this
    /// string from `is_some`/`is_empty` booleans only; the Draft-level
    /// `Draft_debug_redacts_provider_api_key` (H4) already covers the Debug
    /// dump — this pins the row-level contract and the idle-display source.)
    #[test]
    fn provider_key_masked_idle_display_never_contains_the_raw_secret() {
        const SECRET: &str = "sk-OLD-000";
        let mut s = state_with_file(KEY_BOTH);
        s.open();
        let d = s.draft.as_ref().unwrap();
        let key = d.providers[0].api_key.clone().unwrap();
        assert!(!key.is_empty());
        // The exact expression main.rs uses for the idle display (the
        // `gpui::SharedString` the row's ElText wraps):
        let masked_idle: gpui::SharedString = if !key.is_empty() {
            "•••• (set — click to edit)".into()
        } else {
            "(not set — click to edit)".into()
        };
        let displayed = masked_idle.to_string();
        assert!(!displayed.contains(SECRET));
        assert!(!displayed.contains(&key));

        // The whole-draft and row Debug dumps redact the same way.
        let dump = format!("{d:?}");
        assert!(!dump.contains(SECRET), "Draft Debug must not leak the key");
        assert!(!dump.contains(&key), "Draft Debug must not leak the key");
        let row = format!("{:?}", d.providers[0]);
        assert!(!row.contains(SECRET), "row Debug must not leak the key");
    }

    /// The key arm is the ONLY untrimmed field of the panel (contract: never
    /// trim hidden secrets on any action). Whitespace around a committed key
    /// is stored verbatim.
    #[test]
    fn provider_key_commit_preserves_whitespace_verbatim() {
        const P: &str = "  sk-padded-42  ";
        let mut s = state_with_file(KEY_BOTH);
        s.open();
        s.begin_field(Field::ProviderApiKey(0));
        assert!(s.commit_draft_field(P));
        assert_eq!(
            s.draft.as_ref().unwrap().providers[0].api_key.as_deref(),
            Some(P),
            "the key commits verbatim — no trim"
        );
        assert!(s.save_draft(), "save must land: {:?}", s.error);
        let cfg = Config::parse(&std::fs::read_to_string(s.path()).unwrap())
            .expect("saved config parses");
        let p = cfg
            .providers
            .iter()
            .find(|p| p.name == "anth")
            .expect("provider present");
        assert_key_filed(p.api_key.as_deref(), "anth", P);
    }

    /// The WRITE analog of the out-of-range guard (mirror of the ProviderName
    /// version): aiming the key editor at a row that no longer exists refuses
    /// with `false` and corrupts nothing.
    #[test]
    fn provider_key_commit_rejects_an_out_of_range_provider_index() {
        let mut s = state_with_file(KEY_BOTH);
        s.open();
        let before_key = s.draft.as_ref().unwrap().providers[0].api_key.clone();
        let before_none = s.draft.as_ref().unwrap().providers[1].api_key.clone();
        s.begin_field(Field::ProviderApiKey(9));
        assert!(
            !s.commit_draft_field("sk-GHOST"),
            "an out-of-range provider key commit must return false"
        );
        assert_eq!(
            s.draft.as_ref().unwrap().providers[0].api_key,
            before_key,
            "a refused key commit must not corrupt the live row"
        );
        assert_eq!(
            s.draft.as_ref().unwrap().providers[1].api_key,
            before_none,
            "a refused key commit must not create a key on a sibling row"
        );
    }

    /// Committing with NO field open is a noop for the key row too — the same
    /// contract as every other draft field (the shared editor's Submit must
    /// not misdirect a write at a stale target).
    #[test]
    fn provider_key_commit_without_field_is_a_noop() {
        let mut s = state_with_file(KEY_BOTH);
        s.open();
        let before = s.draft.as_ref().unwrap().providers[0].api_key.clone();
        assert!(
            !s.commit_draft_field("sk-NOFIELD"),
            "a commit with no field open must refuse"
        );
        assert_eq!(
            s.draft.as_ref().unwrap().providers[0].api_key,
            before,
            "a refused commit must not mutate the draft"
        );
    }

    // ---- the validation gate (spec §4) -------------------------------------

    #[test]
    fn raw_save_rejects_invalid_toml_and_touches_nothing() {
        let mut s = state_with_file(VALID);
        assert!(!s.commit_raw("default_provider = \"unclosed"));
        assert!(s.error.is_some(), "syntax error must surface inline");
        let on_disk = std::fs::read_to_string(s.path()).unwrap();
        assert_eq!(on_disk, VALID, "invalid TOML must never reach disk");
        assert!(
            !bak(s.path()).exists(),
            "a failed save must not produce a .bak"
        );
    }

    #[test]
    fn raw_save_rejects_semantic_violation() {
        let mut s = state_with_file(VALID);
        // Parses as TOML but breaks the min_priority floor — still rejected.
        assert!(!s.commit_raw("[memory]\nmin_priority = 10\n"));
        assert!(s.error.is_some());
        assert_eq!(std::fs::read_to_string(s.path()).unwrap(), VALID);
    }

    #[test]
    fn raw_save_writes_bak_of_previous_version() {
        let mut s = state_with_file(VALID);
        let updated = VALID.replace("ollama", "anthropic");
        assert!(s.commit_raw(&updated));
        assert_eq!(std::fs::read_to_string(s.path()).unwrap(), updated);
        assert_eq!(
            std::fs::read_to_string(bak(s.path())).unwrap(),
            VALID,
            ".bak must hold the version that was just replaced"
        );
        assert!(s.error.is_none());
        // The in-memory doc now agrees with disk, so the form tab is current.
        assert!(s.doc.as_ref().unwrap().text().contains("anthropic"));
    }

    #[test]
    fn field_commit_validates_before_write() {
        let mut s = state_with_file(VALID);
        s.begin_edit(
            Target::Table {
                table: "memory".into(),
                key: "min_priority".into(),
            },
            ValueKind::Int,
        );
        // Below the floor: the typed setter rolls back, nothing written.
        assert!(!s.commit_field("10"));
        assert!(s.error.is_some());
        assert!(s.editing.is_some(), "a failed commit keeps the editor open");
        assert_eq!(std::fs::read_to_string(s.path()).unwrap(), VALID);

        // Not an integer at all.
        assert!(!s.commit_field("ninety"));
        assert!(s.error.is_some());
        assert_eq!(std::fs::read_to_string(s.path()).unwrap(), VALID);
    }

    #[test]
    fn field_commit_saves_and_keeps_comments() {
        let mut s = state_with_file(VALID);
        s.begin_edit(
            Target::TopLevel {
                key: "default_provider".into(),
            },
            ValueKind::Str,
        );
        assert!(s.commit_field("anthropic"));
        assert!(s.editing.is_none());
        assert_eq!(s.status.as_deref(), Some("Saved"));
        let on_disk = std::fs::read_to_string(s.path()).unwrap();
        assert!(on_disk.contains("default_provider = \"anthropic\" # keep"));
        assert_eq!(std::fs::read_to_string(bak(s.path())).unwrap(), VALID);
    }

    #[cfg(unix)]
    /// RC2-APP (F0232): when the file cannot be written, a toggle and a field
    /// edit leave the in-memory document as it was — the panel never shows a
    /// value the file does not hold.
    #[test]
    fn rc2app_a_failed_write_leaves_the_document_as_it_was() {
        use std::os::unix::fs::PermissionsExt;
        let with_bool = format!("{VALID}\n[voice]\nauto_speak = false\n");
        let mut s = state_with_file(&with_bool);
        let dir = s.path().parent().unwrap().to_path_buf();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let target = Target::Table {
            table: "voice".into(),
            key: "auto_speak".into(),
        };
        assert!(!s.toggle_bool(&target, false), "the write fails");
        assert!(s.error.is_some(), "and says so");
        assert_eq!(s.doc.as_ref().unwrap().text(), with_bool, "toggle undone");
        s.begin_edit(
            Target::TopLevel {
                key: "default_provider".into(),
            },
            ValueKind::Str,
        );
        assert!(!s.commit_field("anthropic"));
        assert_eq!(s.doc.as_ref().unwrap().text(), with_bool, "edit undone");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(std::fs::read_to_string(s.path()).unwrap(), with_bool);
    }

    #[test]
    fn toggle_bool_round_trips() {
        let with_bool = format!("{VALID}\n[voice]\nauto_speak = false\n");
        let mut s = state_with_file(&with_bool);
        let target = Target::Table {
            table: "voice".into(),
            key: "auto_speak".into(),
        };
        assert!(s.toggle_bool(&target, false));
        assert!(std::fs::read_to_string(s.path())
            .unwrap()
            .contains("auto_speak = true"));
        assert!(s.toggle_bool(&target, true));
        assert!(std::fs::read_to_string(s.path())
            .unwrap()
            .contains("auto_speak = false"));
    }

    /// The two memory switches are independent: `toggle_write` flips only
    /// `memory_write` and `toggle_recall` flips only `memory_recall`. Both start
    /// at their `true` defaults, so a cross-wired toggle (write flipping recall,
    /// or a no-op that never flips) would leave the sibling flag wrong here.
    #[test]
    fn memory_toggles_flip_only_their_own_flag() {
        let mut s = state_with_file(VALID);
        s.open();
        assert!(s.draft.as_ref().unwrap().memory_recall);
        assert!(s.draft.as_ref().unwrap().memory_write);

        s.toggle_write();
        {
            let d = s.draft.as_ref().unwrap();
            assert!(!d.memory_write, "toggle_write flips write off");
            assert!(d.memory_recall, "toggle_write leaves recall alone");
        }

        s.toggle_recall();
        {
            let d = s.draft.as_ref().unwrap();
            assert!(!d.memory_recall, "toggle_recall flips recall off");
            assert!(
                !d.memory_write,
                "toggle_recall leaves write (already off) alone"
            );
        }
    }

    // ---- tab / edit lifecycle ----------------------------------------------

    #[test]
    fn switching_tabs_abandons_an_edit_and_clears_messages() {
        let mut s = state_with_file(VALID);
        s.begin_edit(
            Target::TopLevel {
                key: "default_provider".into(),
            },
            ValueKind::Str,
        );
        s.error = Some("stale".into());
        s.set_tab(Tab::Raw);
        assert!(s.editing.is_none());
        assert!(s.error.is_none());
    }

    #[test]
    fn open_reloads_hand_edits_from_disk() {
        let mut s = state_with_file(VALID);
        std::fs::write(s.path(), VALID.replace("ollama", "anthropic")).unwrap();
        s.open();
        assert!(s.open);
        assert_eq!(s.tab, Tab::Form);
        assert!(s.doc.as_ref().unwrap().text().contains("anthropic"));
    }

    #[test]
    fn open_with_a_broken_file_surfaces_the_error() {
        let mut s = state_with_file(VALID);
        std::fs::write(s.path(), "not = \"toml").unwrap();
        s.open();
        assert!(s.doc.is_none());
        assert!(s.error.is_some());
    }

    #[test]
    fn commit_without_edit_in_progress_is_a_noop() {
        let mut s = state_with_file(VALID);
        assert!(!s.commit_field("anything"));
        assert_eq!(std::fs::read_to_string(s.path()).unwrap(), VALID);
    }

    #[test]
    fn restart_tags_cover_voice_and_feed_only() {
        assert!(restart_required("voice"));
        assert!(restart_required("feed"));
        assert!(!restart_required(""));
        assert!(!restart_required("memory"));
        assert!(!restart_required("providers[0]"));
    }

    // ---- the structured panel (slice-11 spec §1/§4) --------------------------

    const WITH_PROVIDERS: &str = r#"default_provider = "ollama"
default_model = "llama3.1:8b"

[memory]
base_url = "http://127.0.0.1:8400"
recall = true
write = true

[[providers]]
name = "ollama"
kind = "ollama"
base_url = "http://localhost:11434"
models = ["llama3.1:8b", "qwen2.5:14b"]
"#;

    /// Spec §4: the settings panel renders with no providers configured. The
    /// empty state the panel shows is driven exactly by this draft shape.
    #[test]
    fn structured_panel_empty_state_with_no_providers() {
        let path = temp_path();
        let mut s = SettingsState::with_path(path); // no file on disk at all
        s.open();
        let d = s
            .draft
            .as_ref()
            .expect("an absent file still drafts (all defaults)");
        assert!(
            d.providers.is_empty(),
            "no providers — the panel's empty state"
        );
        assert!(
            d.model_options().is_empty(),
            "nothing for the dropdown to offer"
        );
        assert_eq!(d.default_label(), "(first available)");
        // Memory/voice sections still render their defaults (memory is off).
        assert!(!d.memory_recall && !d.memory_write);
        assert_eq!(d.vad_silence_windows, 30);

        // "Add provider" appends an editable blank row.
        assert_eq!(s.add_provider(), Some(0));
        assert_eq!(s.draft.as_ref().unwrap().providers.len(), 1);
        assert!(s.draft.as_ref().unwrap().providers[0].enabled);
    }

    /// `model_options` drops nameless `("", model)` pairs. A freshly added
    /// provider row starts with a blank name (`add_provider` → `blank()`); a
    /// user who types the models before the name would otherwise inject ghost
    /// entries with no provider label into the default-model dropdown.
    #[test]
    fn model_options_excludes_a_blank_named_provider_with_models() {
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        // Add a blank row (enabled, name = ""), then type models before a name.
        let ix = s.add_provider().unwrap();
        s.begin_field(Field::ProviderModels(ix));
        assert!(s.commit_draft_field("ghost-model"));
        // The blank row is still enabled with an empty name.
        let blank = &s.draft.as_ref().unwrap().providers[ix];
        assert!(blank.enabled && blank.name.is_empty());

        let options = s.draft.as_ref().unwrap().model_options();
        // No nameless pair reaches the dropdown, so the ghost model is absent...
        assert!(
            options.iter().all(|(name, _)| !name.is_empty()),
            "a blank-named provider must not offer nameless entries: {options:?}"
        );
        assert!(
            !options.iter().any(|(_, m)| m == "ghost-model"),
            "the blank row's models must be suppressed with its name: {options:?}"
        );
        // ...while the properly-named enabled provider still offers its models.
        assert!(
            options
                .iter()
                .any(|(name, m)| name == "ollama" && m == "llama3.1:8b"),
            "the named provider's models must still be offered: {options:?}"
        );
    }

    /// `default_label` distinguishes all three `(default_provider, default_model)`
    /// shapes for the dropdown's current-selection label. The empty-state test
    /// pins only the `_ => "(first available)"` arm; the two populated arms — a
    /// fully-pinned provider+model vs. a provider whose model defers to "first
    /// model" — were unpinned, so a swap of the two format strings would silently
    /// mislabel the current default in the Settings panel dropdown.
    #[test]
    fn default_label_distinguishes_provider_and_model_selection() {
        let mut s = state_with_file(WITH_PROVIDERS); // default_provider+default_model set
        s.open();
        // (Some, Some): both fields shown, joined by " · ".
        assert_eq!(
            s.draft.as_ref().unwrap().default_label(),
            "ollama · llama3.1:8b"
        );
        // (Some, None): provider chosen but the model defers to the first available.
        s.draft.as_mut().unwrap().default_model = None;
        assert_eq!(
            s.draft.as_ref().unwrap().default_label(),
            "ollama · (first model)"
        );
        // (_, _): no provider chosen at all — the fully-defaulted label.
        s.draft.as_mut().unwrap().default_provider = None;
        assert_eq!(
            s.draft.as_ref().unwrap().default_label(),
            "(first available)"
        );
    }

    /// Spec §4: Save with an invalid URL shows an inline validation error and
    /// does NOT write.
    #[test]
    fn save_draft_with_invalid_url_errors_and_writes_nothing() {
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        s.draft.as_mut().unwrap().providers[0].base_url = "localhost:11434".into(); // no scheme
        assert!(!s.save_draft());
        let err = s
            .error
            .as_deref()
            .expect("validation error must surface inline");
        assert!(err.contains("URL"), "unexpected error: {err}");
        assert_eq!(
            std::fs::read_to_string(s.path()).unwrap(),
            WITH_PROVIDERS,
            "an invalid draft must never reach disk"
        );
        assert!(!bak(s.path()).exists());

        // A bad memory URL is caught the same way.
        s.open();
        s.draft.as_mut().unwrap().memory_url = "http://".into(); // scheme, no host
        assert!(!s.save_draft());
        assert!(s.error.as_deref().unwrap().contains("memory service"));
        assert_eq!(std::fs::read_to_string(s.path()).unwrap(), WITH_PROVIDERS);
    }

    /// M4: a provider key typed in Settings reaches the secret store, and
    /// `config.toml` keeps only the `secret:` reference. (The test build's
    /// store is an in-memory fake, never the login keychain.)
    #[test]
    fn a_typed_provider_key_is_filed_in_the_secret_store_not_the_config() {
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        s.begin_field(Field::ProviderApiKey(0));
        assert!(s.commit_draft_field("sk-typed-do-not-leak-7731"));
        assert!(s.save_draft(), "save must land: {:?}", s.error);
        let on_disk = std::fs::read_to_string(s.path()).unwrap();
        assert!(!on_disk.contains("sk-typed-do-not-leak-7731"), "{on_disk}");
        assert!(on_disk.contains("secret:provider:ollama"), "{on_disk}");
        assert_eq!(
            crate::secret_store::store()
                .get("provider:ollama")
                .unwrap()
                .as_deref(),
            Some("sk-typed-do-not-leak-7731")
        );
    }

    /// Deleting a provider, or clearing its key, removes its stored entry too.
    #[test]
    fn deleting_a_provider_or_clearing_its_key_removes_the_stored_entry() {
        let store = || crate::secret_store::store();
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        s.begin_field(Field::ProviderApiKey(0));
        assert!(s.commit_draft_field("sk-stale-check"));
        assert!(s.save_draft(), "{:?}", s.error);
        assert!(store().get("provider:ollama").unwrap().is_some());
        // Clear the key.
        s.begin_field(Field::ProviderApiKey(0));
        assert!(s.commit_draft_field(""));
        assert!(s.save_draft(), "{:?}", s.error);
        assert_eq!(store().get("provider:ollama").unwrap(), None);
        // Set it again, then delete the provider.
        s.begin_field(Field::ProviderApiKey(0));
        assert!(s.commit_draft_field("sk-stale-check-2"));
        assert!(s.save_draft(), "{:?}", s.error);
        assert!(store().get("provider:ollama").unwrap().is_some());
        s.remove_provider(0);
        assert!(s.save_draft(), "{:?}", s.error);
        assert_eq!(store().get("provider:ollama").unwrap(), None);
    }

    #[test]
    fn save_draft_round_trips_every_section() {
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        // Providers: edit, toggle, add.
        s.begin_field(Field::ProviderUrl(0));
        assert!(s.commit_draft_field("http://localhost:11435"));
        s.toggle_provider_enabled(0);
        let ix = s.add_provider().unwrap();
        s.begin_field(Field::ProviderName(ix));
        s.commit_draft_field("hermes");
        s.begin_field(Field::ProviderUrl(ix));
        s.commit_draft_field("http://10.77.68.86:8080");
        s.begin_field(Field::ProviderModels(ix));
        s.commit_draft_field("hermes-4, hermes-4-mini");
        s.cycle_provider_kind(ix); // ollama → anthropic
        s.cycle_provider_kind(ix); // anthropic → hermes
                                   // Default model from the dropdown (only the ENABLED provider offers).
        let options = s.draft.as_ref().unwrap().model_options();
        assert!(options.iter().all(|(p, _)| p == "hermes"), "{options:?}");
        s.set_default_choice(Some(options[0].clone()));
        // Memory + voice.
        s.toggle_recall();
        s.draft.as_mut().unwrap().memory_url = "http://10.0.0.9:8841".into();
        s.begin_field(Field::TtsModel);
        s.commit_draft_field("af_bella");
        for _ in 0..3 {
            s.vad_step(true);
        }
        s.silence_step(false);

        assert!(s.save_draft(), "save must land: {:?}", s.error);
        assert_eq!(s.status.as_deref(), Some("Saved"));

        // Read back through the real loaders — UI state became config.toml.
        let cfg = Config::load_from(s.path()).unwrap();
        assert_eq!(cfg.providers.len(), 2);
        assert_eq!(cfg.providers[0].base_url, "http://localhost:11435");
        assert!(!cfg.providers[0].enabled);
        assert_eq!(cfg.providers[1].name, "hermes");
        assert_eq!(cfg.providers[1].kind, awili_config::ProviderKind::Hermes);
        assert_eq!(cfg.providers[1].models, vec!["hermes-4", "hermes-4-mini"]);
        assert_eq!(cfg.default_provider.as_deref(), Some("hermes"));
        assert_eq!(cfg.default_model.as_deref(), Some("hermes-4"));
        assert_eq!(cfg.memory.base_url, "http://10.0.0.9:8841");
        assert!(!cfg.memory.recall);
        assert!(cfg.memory.write, "untouched toggle stays default");
        let voice = VoiceConfig::parse(&std::fs::read_to_string(s.path()).unwrap()).unwrap();
        assert_eq!(voice.tts_model.as_deref(), Some("af_bella"));
        assert!(
            voice.vad_threshold > 0.01,
            "3 up-notches moved it: {}",
            voice.vad_threshold
        );
        assert_eq!(voice.vad_silence_windows, 25);
    }

    #[test]
    fn restart_notice_fires_on_wiring_changes_only() {
        // Enable/disable applies live — no notice.
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        s.toggle_provider_enabled(0);
        assert!(s.save_draft());
        assert!(
            s.restart_notice.is_none(),
            "an enable toggle needs no restart"
        );

        // A base_url change is wiring — notice.
        s.open();
        s.draft.as_mut().unwrap().providers[0].base_url = "http://localhost:9999".into();
        assert!(s.save_draft());
        assert_eq!(s.restart_notice.as_deref(), Some(RESTART_NOTICE));

        // Closing clears it.
        s.close();
        assert!(s.restart_notice.is_none());
    }

    #[test]
    fn discard_throws_the_draft_away_and_writes_nothing() {
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        s.draft.as_mut().unwrap().providers[0].base_url = "http://elsewhere:1".into();
        s.remove_provider(0);
        s.discard();
        assert!(!s.open);
        assert_eq!(std::fs::read_to_string(s.path()).unwrap(), WITH_PROVIDERS);
        // Reopening drafts from disk again — the edits are gone.
        s.open();
        assert_eq!(
            s.draft.as_ref().unwrap().providers[0].base_url,
            "http://localhost:11434"
        );
    }

    #[test]
    fn steppers_clamp_to_their_spec_ranges() {
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        for _ in 0..100 {
            s.vad_step(true);
            s.silence_step(true);
        }
        assert_eq!(s.draft.as_ref().unwrap().vad_threshold, 0.1);
        assert_eq!(s.draft.as_ref().unwrap().vad_silence_windows, 60);
        assert!((s.draft.as_ref().unwrap().vad_position() - 1.0).abs() < 1e-9);
        for _ in 0..100 {
            s.vad_step(false);
            s.silence_step(false);
        }
        assert_eq!(s.draft.as_ref().unwrap().vad_threshold, 0.001);
        assert_eq!(s.draft.as_ref().unwrap().vad_silence_windows, 10);
        assert!(s.draft.as_ref().unwrap().vad_position().abs() < 1e-9);
    }

    #[test]
    fn vad_position_is_log_scaled_and_clamps_out_of_range() {
        // `steppers_clamp_to_their_spec_ranges` only ever reaches the exact
        // endpoints (MIN=>0, MAX=>1), where a log fill and a linear fill agree.
        // The two behaviors it therefore CANNOT catch, both load-bearing for the
        // Settings VAD slider fill (main.rs @7319 multiplies this by the track px):
        //   (a) the LOG scale — the geometric midpoint of [0.001, 0.1] is 0.01,
        //       which must map to exactly 0.5. A "simplify to linear" refactor,
        //       (v-MIN)/(MAX-MIN), still nails both endpoints but puts 0.01 at
        //       ~0.091 — the fill would bunch against the left the whole span.
        //   (b) the CLAMP — every path that sets vad_threshold today (vad_step,
        //       from_doc's parse-or-default) keeps it in range, so this is a
        //       defensive guard on the UI helper: should any future in-memory
        //       write bypass those, the fill stays in 0..=1 instead of going
        //       negative / >1. The stepper test can't reach it (it clamps first).
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        let d = s.draft.as_mut().unwrap();
        // (a) geometric midpoint of the two decades => dead center.
        d.vad_threshold = 0.01;
        assert!((d.vad_position() - 0.5).abs() < 1e-9);
        // (b) below MIN clamps to the left edge, above MAX to the right edge.
        d.vad_threshold = 0.0001;
        assert!(d.vad_position().abs() < 1e-9);
        d.vad_threshold = 1.0;
        assert!((d.vad_position() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn vad_step_moves_geometrically_not_linearly() {
        // `steppers_clamp_to_their_spec_ranges` only ever runs the stepper to its
        // endpoints, where the clamp masks HOW each notch moves; `save_draft_round_
        // trips_every_section` only asserts 3 up-notches land somewhere `> 0.01`.
        // Neither pins the load-bearing shape of a single notch: it multiplies /
        // divides by a GEOMETRIC factor (100^(1/16) across the two decades 0.001–
        // 0.1), NOT a linear ±delta. A "simplify to a fixed +/- step" refactor keeps
        // the endpoints (they clamp) and keeps `> 0.01` true, but every intermediate
        // notch — the whole usable range of the Settings VAD stepper — would be wrong.
        // The expected values below are hard literals (MIN·f and MAX/f, f≈1.333521),
        // computed independently of the production formula so this can't be tautological.
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        // One notch UP from the low edge multiplies by the geometric factor and
        // stays well inside the range (no clamp): 0.001 · 100^(1/16) ≈ 0.00133352.
        s.draft.as_mut().unwrap().vad_threshold = VAD_THRESHOLD_MIN;
        s.vad_step(true);
        assert!(
            (s.draft.as_ref().unwrap().vad_threshold - 0.001_333_521_432).abs() < 1e-9,
            "one up-notch = MIN·100^(1/16), got {}",
            s.draft.as_ref().unwrap().vad_threshold
        );
        // One notch DOWN from the high edge divides by the same factor, in range:
        // 0.1 / 100^(1/16) ≈ 0.07498942.
        s.draft.as_mut().unwrap().vad_threshold = VAD_THRESHOLD_MAX;
        s.vad_step(false);
        assert!(
            (s.draft.as_ref().unwrap().vad_threshold - 0.074_989_420_933).abs() < 1e-9,
            "one down-notch = MAX/100^(1/16), got {}",
            s.draft.as_ref().unwrap().vad_threshold
        );
    }

    /// `silence_step` moves the silence-window stepper by a LINEAR ±5 (spec §1:
    /// 10–60). Its two existing tests both fail to pin the UP notch's magnitude at
    /// an intermediate, non-clamped value: `steppers_clamp_to_their_spec_ranges`
    /// runs the up-loop 100× so it always lands on the 60 CLAMP no matter how far a
    /// single up-notch moves, and `save_draft_round_trips_every_section` exercises
    /// only ONE DOWN notch (30 => 25). So an up arm that added the wrong amount — or
    /// moved the wrong direction — would still clamp to 60 under the loop and never
    /// touch the down-only round-trip, silently mis-stepping the whole usable range
    /// of the stepper's up button.
    #[test]
    fn silence_step_up_moves_one_linear_notch_off_the_clamp() {
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        assert_eq!(
            s.draft.as_ref().unwrap().vad_silence_windows,
            30,
            "default silence window (no [voice] table in the fixture)"
        );
        // One up-notch from the default: exactly +5, nowhere near the 60 clamp.
        s.silence_step(true);
        assert_eq!(s.draft.as_ref().unwrap().vad_silence_windows, 35);
    }

    #[test]
    fn url_validation_accepts_endpoints_and_rejects_garbage() {
        assert!(validate_url("http://localhost:11434").is_ok());
        assert!(validate_url("https://api.anthropic.com").is_ok());
        assert!(validate_url("http://10.77.68.86:8841/api").is_ok());
        assert!(validate_url("localhost:11434").is_err());
        assert!(validate_url("ftp://h:1").is_err());
        assert!(validate_url("http://").is_err());
        assert!(validate_url("http:// spaced host").is_err());
        assert!(validate_url("").is_err());
    }

    #[test]
    fn duplicate_or_unnamed_providers_are_rejected_at_save() {
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        let ix = s.add_provider().unwrap();
        // Unnamed → rejected.
        assert!(!s.save_draft());
        assert!(s.error.as_deref().unwrap().contains("name"));
        // Renamed to a duplicate → rejected; disk untouched throughout.
        s.draft.as_mut().unwrap().providers[ix].name = "ollama".into();
        s.draft.as_mut().unwrap().providers[ix].base_url = "http://h:1".into();
        assert!(!s.save_draft());
        assert!(s.error.as_deref().unwrap().contains("duplicate"));
        assert_eq!(std::fs::read_to_string(s.path()).unwrap(), WITH_PROVIDERS);
    }

    /// `field_text` pre-fills the shared inline editor when the user begins
    /// editing a settings field, so each `Field` variant must read its OWN
    /// draft field — a variant-crossing bug (e.g. `ProviderUrl` returning the
    /// provider's name) would seed the editor with the wrong value. Distinct
    /// sentinel values per field make any two-arm swap observable. Also pins the
    /// two fallbacks: no draft (panel not open) and an out-of-range provider
    /// index both yield empty text rather than a stale value or a panic.
    #[test]
    fn field_text_reads_each_variants_own_draft_field() {
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        {
            let d = s.draft.as_mut().expect("draft built on open");
            d.providers[0].name = "NAME-x".into();
            d.providers[0].base_url = "URL-x".into();
            d.providers[0].models = "MODELS-x".into();
            d.memory_url = "MEMORY-x".into();
            d.tts_model = "TTS-x".into();
        }
        assert_eq!(s.field_text(Field::ProviderName(0)), "NAME-x");
        assert_eq!(s.field_text(Field::ProviderUrl(0)), "URL-x");
        assert_eq!(s.field_text(Field::ProviderModels(0)), "MODELS-x");
        assert_eq!(s.field_text(Field::MemoryUrl), "MEMORY-x");
        assert_eq!(s.field_text(Field::TtsModel), "TTS-x");
        // Out-of-range provider index → empty (unwrap_or_default), not a panic.
        assert_eq!(s.field_text(Field::ProviderName(9)), "");

        // No draft (panel never opened) → every field is empty text.
        let closed = SettingsState::with_path(temp_path());
        assert_eq!(closed.field_text(Field::MemoryUrl), "");
        assert_eq!(closed.field_text(Field::ProviderUrl(0)), "");
    }

    /// The WRITE analog of the read-path OOB guard above: committing the inline
    /// editor into a provider row that does not exist must refuse (`false`) and
    /// leave the existing provider rows untouched, rather than panic or
    /// misdirect the write onto a live row. `begin_field` sets the target
    /// unconditionally, so a stale index (row removed under an open editor)
    /// genuinely reaches this guard.
    #[test]
    fn commit_draft_field_rejects_an_out_of_range_provider_index() {
        let mut s = state_with_file(WITH_PROVIDERS);
        s.open();
        let before = s.draft.as_ref().unwrap().providers[0].name.clone();
        // Aim the editor at a provider index past the end, then commit.
        s.begin_field(Field::ProviderName(9));
        assert!(
            !s.commit_draft_field("ghost"),
            "out-of-range provider commit must return false"
        );
        assert_eq!(
            s.draft.as_ref().unwrap().providers[0].name,
            before,
            "a refused commit must not corrupt an existing provider row"
        );
    }

    // ---- from_doc: the [voice] table is parsed independently of the rest -----

    /// `Draft::from_doc` reads a valid, non-default `[voice]` table THROUGH into
    /// the draft. Sentinel values distinct from `VoiceConfig::default()`
    /// (0.01 / 30 / "") make "always fall back to defaults" observable — if the
    /// parse result were dropped, every assertion below would read a default.
    #[test]
    fn from_doc_reads_a_valid_non_default_voice_table() {
        let text = "default_provider = \"ollama\" # keep\n\n\
            [memory]\nbase_url = \"http://127.0.0.1:8400\"\nmin_priority = 90\n\n\
            [voice]\nvad_threshold = 0.05\nvad_silence_windows = 45\ntts_model = \"af_bella\"\n";
        let doc = ConfigDoc::parse(text).unwrap();
        let d = Draft::from_doc(&doc);
        assert_eq!(d.vad_threshold, 0.05);
        assert_eq!(d.vad_silence_windows, 45);
        assert_eq!(d.tts_model, "af_bella");
    }

    /// A present-but-malformed `[voice]` table (here `vad_threshold = 0.5`, out
    /// of `VoiceConfig`'s accepted range) must NOT block the panel: `from_doc`
    /// swallows the parse error via `unwrap_or_default()` and the whole table
    /// falls back atomically. The load-bearing assertions are the VALID siblings
    /// (`vad_silence_windows = 45`, `tts_model = "af_bella"`) being discarded too
    /// — that only holds if the entire malformed table is rejected as a unit, not
    /// field-by-field — while the non-voice sections (default_provider, memory)
    /// still populate from the same doc. Guards against a `.unwrap()`/`.expect()`
    /// regression that would panic the settings panel open.
    #[test]
    fn from_doc_falls_back_to_defaults_on_a_malformed_voice_table() {
        let text = "default_provider = \"ollama\" # keep\n\n\
            [memory]\nbase_url = \"http://127.0.0.1:8400\"\nmin_priority = 90\n\n\
            [voice]\nvad_threshold = 0.5\nvad_silence_windows = 45\ntts_model = \"af_bella\"\n";
        // The document itself is valid TOML and valid awili-config — only the
        // voice sub-parse (a separate pass over doc.text()) rejects it.
        let doc = ConfigDoc::parse(text).unwrap();
        let d = Draft::from_doc(&doc);

        // Voice falls back WHOLESALE to VoiceConfig::default(), discarding the
        // valid siblings that shared the malformed table.
        assert_eq!(
            d.vad_threshold, 0.01,
            "out-of-range threshold → voice default"
        );
        assert_eq!(
            d.vad_silence_windows, 30,
            "valid sibling discarded with the bad table"
        );
        assert_eq!(
            d.tts_model, "",
            "valid sibling discarded with the bad table"
        );

        // ...but the rest of the panel is unharmed: a bad voice table can't block it.
        assert_eq!(d.default_provider.as_deref(), Some("ollama"));
        assert_eq!(d.memory_url, "http://127.0.0.1:8400");
    }

    /// `from_doc` wires the two `[memory]` toggles to their OWN config flags:
    /// `recall → memory_recall`, `write → memory_write`, un-crossed. Both flags
    /// default to `true`, so every other from_doc test reads them at their
    /// shared default — where a swap (`recall ↔ write`) or a hardcoded `true`
    /// is invisible because `recall == write`. This drives BOTH asymmetric
    /// cases so each field is pinned against every degenerate wiring:
    ///   - `(recall=false, write=true)`: `memory_recall` must be `false`, which
    ///     fails if it is hardcoded `true` OR crossed to read `write`.
    ///   - `(recall=true, write=false)`: `memory_write` must be `false`, which
    ///     fails if it is hardcoded `true` OR crossed to read `recall`.
    ///
    /// A single asymmetric case would leave the field that happens to equal its
    /// default (`true`) unable to distinguish a hardcode from a real read.
    #[test]
    fn from_doc_wires_each_memory_toggle_to_its_own_config_flag() {
        let doc_for = |recall: bool, write: bool| {
            let text = format!(
                "[memory]\nbase_url = \"http://127.0.0.1:8400\"\nrecall = {recall}\nwrite = {write}\n"
            );
            Draft::from_doc(&ConfigDoc::parse(&text).unwrap())
        };

        let d = doc_for(false, true);
        assert!(
            !d.memory_recall,
            "recall=false reads through to memory_recall"
        );
        assert!(d.memory_write, "write=true reads through to memory_write");

        let d = doc_for(true, false);
        assert!(
            d.memory_recall,
            "recall=true reads through to memory_recall"
        );
        assert!(!d.memory_write, "write=false reads through to memory_write");
    }

    /// `ProviderDraft::from_config` joins the config's `Vec<String>` models into
    /// the single comma-space string the panel's models edit-field pre-fills with
    /// on open. Both the separator (`", "`) and "all models, in order" were
    /// unpinned: the suite's only `.models` assertion (line ~1136) is on
    /// `to_config`'s OUTPUT `Vec` for a provider whose models were TYPED into the
    /// draft, never on a from_config'd draft's joined string — and the save
    /// round-trip never asserts `providers[0].models`, so it survives a
    /// join↔split that changes the display form. Concretely, `join(",")` (dropped
    /// space), `.first()` (only the first model), or `String::new()` each survive
    /// the whole suite while silently mangling the models field of an existing
    /// provider on panel open. WITH_PROVIDERS' ollama row carries TWO models, so
    /// this pins the full join, not a single-element degenerate case.
    #[test]
    fn from_config_joins_provider_models_with_comma_space() {
        let d = Draft::from_doc(&ConfigDoc::parse(WITH_PROVIDERS).unwrap());
        assert_eq!(d.providers[0].models, "llama3.1:8b, qwen2.5:14b");
    }

    /// `ProviderDraft::from_config` copies each existing provider's `kind` and
    /// `enabled` verbatim so the panel opens showing the row's real protocol chip
    /// and enable state. Both arms were unpinned: WITH_PROVIDERS' only row is
    /// `kind = "ollama"` / enabled (the field defaults, and `blank()`'s values), so
    /// hardcoding `kind: ProviderKind::Ollama` or `enabled: true` in `from_config`
    /// would survive the whole suite — the sole `.kind`/`!.enabled` assertions
    /// (line ~1135/1133) read back a loaded `Config`, never a from_config'd draft.
    /// A disabled Hermes row exercises both non-default arms at once: mislabeling
    /// its chip as Ollama, or opening it as enabled, would corrupt what the user
    /// sees for a provider they'd deliberately turned off.
    #[test]
    fn from_config_copies_provider_kind_and_enabled_state() {
        const DISABLED_HERMES: &str = r#"default_provider = "hermes"
default_model = "hermes-4"

[[providers]]
name = "hermes"
kind = "hermes"
base_url = "http://10.77.68.86:8080"
models = ["hermes-4"]
enabled = false
"#;
        let d = Draft::from_doc(&ConfigDoc::parse(DISABLED_HERMES).unwrap());
        assert_eq!(d.providers[0].kind, awili_config::ProviderKind::Hermes);
        assert!(!d.providers[0].enabled, "a disabled row opens disabled");
    }

    // ---- FIRST-RUN-SPEC 2026-09-06, slice 2a: the missing-file save path ----

    /// Assert the config directory holds no temp or backup leftovers.
    fn assert_no_leftovers(path: &std::path::Path) {
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-") || n.ends_with(".bak"))
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
    }

    /// Seed a draft with one provider row (name/url/models) — the standard
    /// first-save edit for the missing-file scenarios below.
    fn draft_one_provider(s: &mut SettingsState) {
        let ix = s.add_provider().expect("add provider on an open draft");
        s.begin_field(Field::ProviderName(ix));
        assert!(s.commit_draft_field("local"));
        s.begin_field(Field::ProviderUrl(ix));
        assert!(s.commit_draft_field("http://localhost:11434"));
        s.begin_field(Field::ProviderModels(ix));
        assert!(s.commit_draft_field("llama3.1:8b"));
    }

    /// Opening with no candidate file seeds the EXPLICIT networkless
    /// document (not the old empty-TOML serde defaults), and cancel/close
    /// writes nothing.
    #[test]
    fn missing_open_seeds_networkless_and_cancel_writes_nothing() {
        let path = temp_path();
        let mut s = SettingsState::with_path(path.clone());
        s.open();
        // The seed is networkless: no providers, no default choice, and the
        // memory service URL is explicitly EMPTY — not the serde default host.
        let d = s.draft.as_ref().expect("a missing file still drafts");
        assert!(d.providers.is_empty());
        assert_eq!(d.default_provider, None);
        assert_eq!(d.memory_url, "");
        // ...and the document itself says so.
        let doc = s.doc.as_ref().expect("a missing file still documents");
        assert!(doc.text().contains("base_url = \"\""));
        assert_eq!(doc.config().memory.base_url, "");
        assert!(doc.config().providers.is_empty());

        // Cancel writes nothing: no file, no temp, no backup.
        s.discard();
        assert!(!s.open);
        assert!(!path.exists(), "cancel must not create the config file");
        assert_no_leftovers(&path);
    }

    /// A valid first save creates the file (create-only); the SUBSEQUENT
    /// ordinary edit is an existing-config save and backs up the previous
    /// version — the missing-origin state flips after the first save.
    #[test]
    fn first_save_creates_then_ordinary_edits_back_up() {
        let path = temp_path();
        let mut s = SettingsState::with_path(path.clone());
        s.open();
        draft_one_provider(&mut s);
        assert!(s.save_draft(), "first save must land: {:?}", s.error);
        assert!(path.exists(), "the first save creates the config file");
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("local"));
        assert!(
            !bak(&path).exists(),
            "a first create has no previous version"
        );
        assert_no_leftovers(&path);

        // A SUBSEQUENT ordinary edit goes through the normal existing-config
        // save: the previous version lands in `.bak`.
        s.begin_edit(
            Target::TopLevel {
                key: "default_provider".into(),
            },
            ValueKind::Str,
        );
        assert!(s.commit_field("local"));
        assert_eq!(
            std::fs::read_to_string(bak(&path)).unwrap(),
            first,
            "the second save must back up the first version"
        );
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("default_provider = \"local\""));
    }

    /// An externally created PRIMARY config (between open and save) stops the
    /// stale save: the external file is untouched (no overwrite, no backup),
    /// and the panel reloads it for review with an inline conflict notice.
    #[test]
    fn external_primary_appearing_stops_stale_save_and_reloads() {
        let primary = temp_path();
        let fallback = temp_path();
        let mut s = SettingsState::with_paths(vec![primary.clone(), fallback.clone()]);
        s.open();
        assert!(
            s.draft.as_ref().unwrap().providers.is_empty(),
            "opened missing"
        );
        draft_one_provider(&mut s);

        // ...then an EXTERNAL config appears at the primary path before Save.
        let external =
            "default_provider = \"external\"\n\n[memory]\nbase_url = \"http://ext:9000\"\n";
        std::fs::write(&primary, external).unwrap();

        assert!(!s.save_draft(), "the stale save must not land");
        assert!(s.status.is_none(), "no success signal for a stopped save");
        assert_eq!(
            std::fs::read_to_string(&primary).unwrap(),
            external,
            "the external file must be byte-identical"
        );
        assert!(!bak(&primary).exists(), "no backup of the external file");
        assert_no_leftovers(&primary);
        // The panel reloaded the EXTERNAL document for review...
        let doc = s
            .doc
            .as_ref()
            .expect("the external document must be loaded");
        assert_eq!(doc.config().default_provider.as_deref(), Some("external"));
        assert!(doc.text().contains("ext:9000"));
        // ...with a clear inline conflict notice, and saves now target the
        // adopted primary.
        assert!(s.error.as_deref().unwrap().contains("appeared"));
        assert_eq!(*s.path(), primary);
        assert!(!fallback.exists(), "the fallback was never touched");
    }

    /// A valid FALLBACK candidate (second in order) appearing between open
    /// and save is adopted the same way — even though the primary stays
    /// absent.
    #[test]
    fn external_fallback_appearing_is_adopted_when_primary_stays_absent() {
        let primary = temp_path();
        let fallback = temp_path();
        let mut s = SettingsState::with_paths(vec![primary.clone(), fallback.clone()]);
        s.open();
        draft_one_provider(&mut s);

        let external = "default_provider = \"fallback-ext\"\n";
        std::fs::write(&fallback, external).unwrap();

        assert!(!s.save_draft(), "the stale save must not land");
        assert_eq!(
            std::fs::read_to_string(&fallback).unwrap(),
            external,
            "the fallback must not be touched"
        );
        assert!(!bak(&fallback).exists());
        assert_no_leftovers(&fallback);
        assert_eq!(
            *s.path(),
            fallback,
            "saves now land at the adopted fallback"
        );
        assert_eq!(
            s.doc.as_ref().unwrap().config().default_provider.as_deref(),
            Some("fallback-ext")
        );
        assert!(s.error.as_deref().unwrap().contains("appeared"));
        assert!(!primary.exists());
    }

    /// An INVALID external file appearing at the primary is adopted as its
    /// ERROR: the load error surfaces (the invalid file stays authoritative —
    /// no fallback is silently loaded), and nothing is written.
    #[test]
    fn invalid_external_appearing_surfaces_the_load_error() {
        let primary = temp_path();
        let fallback = temp_path();
        let mut s = SettingsState::with_paths(vec![primary.clone(), fallback.clone()]);
        s.open();
        draft_one_provider(&mut s);

        let invalid = "this is not = = toml";
        std::fs::write(&primary, invalid).unwrap();

        assert!(
            !s.save_draft(),
            "an invalid external file must stop the save"
        );
        assert_eq!(
            std::fs::read_to_string(&primary).unwrap(),
            invalid,
            "the invalid file must remain byte-identical"
        );
        assert!(s.doc.is_none(), "the invalid document cannot be loaded");
        let err = s.error.as_deref().unwrap();
        assert!(err.contains("appeared"), "{err}");
        assert!(err.contains("TOML"), "the load error must surface: {err}");
        // The valid fallback must NOT be silently chosen over the invalid
        // primary.
        assert_eq!(*s.path(), primary);
        assert!(!fallback.exists());
        assert_no_leftovers(&primary);
    }

    /// Every save ENTRY POINT stops the stale save when a config appeared
    /// since open (save_draft is covered above): commit_raw, commit_field,
    /// and toggle_bool — each preserves the external file byte-for-byte.
    #[test]
    fn every_save_entry_point_preserves_an_externally_appeared_file() {
        // commit_raw: the raw tab's text must not clobber the external file.
        {
            let primary = temp_path();
            let fallback = temp_path();
            let mut s = SettingsState::with_paths(vec![primary.clone(), fallback.clone()]);
            s.open();
            let external = "default_provider = \"external\"\n";
            std::fs::write(&primary, external).unwrap();
            assert!(!s.commit_raw("default_provider = \"stale-raw\"\n"));
            assert_eq!(
                std::fs::read_to_string(&primary).unwrap(),
                external,
                "commit_raw must not clobber the external file"
            );
            assert!(!bak(&primary).exists());
            assert_eq!(
                s.doc.as_ref().unwrap().config().default_provider.as_deref(),
                Some("external"),
                "the external document must be loaded for review"
            );
            assert!(s.error.as_deref().unwrap().contains("appeared"));
        }
        // commit_field: the All-fields tab's inline commit.
        {
            let primary = temp_path();
            let fallback = temp_path();
            let mut s = SettingsState::with_paths(vec![primary.clone(), fallback.clone()]);
            s.open();
            s.begin_edit(
                Target::TopLevel {
                    key: "default_provider".into(),
                },
                ValueKind::Str,
            );
            let external = "default_provider = \"external\"\n";
            std::fs::write(&primary, external).unwrap();
            assert!(!s.commit_field("stale-field"));
            assert_eq!(
                std::fs::read_to_string(&primary).unwrap(),
                external,
                "commit_field must not clobber the external file"
            );
            assert!(!bak(&primary).exists());
            assert!(s.error.as_deref().unwrap().contains("appeared"));
        }
        // toggle_bool: the click-to-toggle save.
        {
            let primary = temp_path();
            let fallback = temp_path();
            let mut s = SettingsState::with_paths(vec![primary.clone(), fallback.clone()]);
            s.open();
            let target = Target::Table {
                table: "memory".into(),
                key: "recall".into(),
            };
            let external = "default_provider = \"external\"\n";
            std::fs::write(&primary, external).unwrap();
            assert!(!s.toggle_bool(&target, true));
            assert_eq!(
                std::fs::read_to_string(&primary).unwrap(),
                external,
                "toggle_bool must not clobber the external file"
            );
            assert!(!bak(&primary).exists());
            assert!(s.error.as_deref().unwrap().contains("appeared"));
        }
    }

    /// An EXISTING empty file is an existing config, not first run: the panel
    /// drafts the file's own (all-serde-default) document — NOT the networkless
    /// seed — and a save is a normal existing-config save with a `.bak`.
    #[test]
    fn existing_empty_file_remains_existing_with_backup_save() {
        let path = temp_path();
        std::fs::write(&path, "").unwrap();
        let mut s = SettingsState::with_path(path.clone());
        s.open();
        // An empty file is valid: all serde defaults — the memory URL is the
        // default host, not the networkless seed's explicit empty string.
        let d = s.draft.as_ref().expect("an empty file still drafts");
        assert_eq!(
            d.memory_url,
            awili_config::MemoryConfig::default().base_url,
            "an existing empty file keeps the serde-default document"
        );
        // A save is an ordinary existing-config save: it backs up the empty
        // file as the previous version.
        draft_one_provider(&mut s);
        assert!(
            s.save_draft(),
            "save on an existing empty file: {:?}",
            s.error
        );
        assert_eq!(
            std::fs::read_to_string(bak(&path)).unwrap(),
            "",
            "the empty file must be the .bak"
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains("local"));
    }

    /// An existing config whose endpoints are OFFLINE/unreachable is still
    /// just an existing config: open drafts it (no live probe), and a save
    /// lands with a normal backup — no re-seed, no first-run treatment.
    #[test]
    fn existing_offline_endpoints_remain_existing() {
        let path = temp_path();
        let offline = "default_provider = \"ghost\"\n\n[[providers]]\nname = \"ghost\"\nkind = \"ollama\"\nbase_url = \"http://10.255.255.1:9999\"\nmodels = [\"m1\"]\n";
        std::fs::write(&path, offline).unwrap();
        let mut s = SettingsState::with_path(path.clone());
        s.open();
        assert!(
            s.doc.is_some(),
            "an offline endpoint must not block the panel"
        );
        assert!(s.error.is_none(), "an offline endpoint is not an error");
        // A normal provider edit + save: existing-config behavior with a
        // backup of the offline version.
        s.draft.as_mut().unwrap().providers[0].base_url = "http://localhost:11434".into();
        assert!(s.save_draft(), "save on an offline config: {:?}", s.error);
        assert_eq!(std::fs::read_to_string(bak(&path)).unwrap(), offline);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("http://localhost:11434"));
    }

    /// An existing INVALID file stays an existing config: open surfaces its
    /// load error authoritatively (no networkless re-seed, no fallback
    /// chosen), and the raw tab can still repair it through the normal
    /// existing-config save (backing up the invalid version).
    #[test]
    fn existing_invalid_file_remains_existing_and_is_repairable_via_raw() {
        let path = temp_path();
        let invalid = "this is not = = toml";
        std::fs::write(&path, invalid).unwrap();
        let mut s = SettingsState::with_path(path.clone());
        s.open();
        assert!(s.doc.is_none(), "the invalid document must not be loaded");
        assert!(s.error.is_some(), "the load error must surface");
        // The raw tab repairs it: the invalid version is backed up, the file
        // replaced — the existing-config save path, not a first-run re-seed.
        let repair = "default_provider = \"ollama\"\n";
        assert!(s.commit_raw(repair), "raw repair: {:?}", s.error);
        assert_eq!(
            std::fs::read_to_string(bak(&path)).unwrap(),
            invalid,
            "the invalid version must be the .bak"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), repair);
    }

    // ---- dangling-symlink regression (Unix): a broken primary entry must
    // ---- surface its load error, never become a synthetic writable default --

    /// A dangling symlink at the PRIMARY path is a directory entry, so `open`
    /// must treat it as the existing config and surface its load error — not
    /// fall through to an empty (all-defaults) document that a save would
    /// write. The symlink itself is never rewritten or removed, and the
    /// missing target is never created.
    #[cfg(unix)]
    #[test]
    fn dangling_primary_at_open_surfaces_load_error_and_stays_untouched() {
        let path = temp_path();
        std::os::unix::fs::symlink("missing-target.toml", &path).unwrap();
        let mut s = SettingsState::with_path(path.clone());
        s.open();
        // A dangling config must NOT become a default document: the read
        // through the broken link fails and the error surfaces.
        assert!(
            s.doc.is_none(),
            "dangling config must not become a default document"
        );
        assert!(s.draft.is_none(), "no draft without a loaded document");
        assert!(
            s.error.is_some(),
            "the load error must surface: {:?}",
            s.error
        );
        // A save attempt refuses (no document) and signals nothing.
        assert!(!s.save_draft());
        assert!(s.status.is_none(), "no success signal for an unsaved draft");
        // The symlink is untouched: still a symlink, same target.
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "the primary must stay a symlink"
        );
        assert_eq!(
            std::fs::read_link(&path).unwrap(),
            std::path::Path::new("missing-target.toml"),
            "the link target must be unchanged"
        );
        // The missing target was never created by the open or the save.
        let target = path.parent().unwrap().join("missing-target.toml");
        assert!(!target.exists(), "the missing target must never be created");
        // Cleanup: only this test's own entries.
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(path.parent().unwrap()).unwrap();
    }

    /// First-run open (missing → networkless seed), then a dangling symlink
    /// appears at the chosen primary before the first save: the stale save
    /// must STOP — nothing written, no success signal — and the adopted entry
    /// must surface its load error (`doc`/`draft` `None`), never a synthetic
    /// writable default. The symlink stays byte-identical and its target
    /// absent.
    #[cfg(unix)]
    #[test]
    fn dangling_primary_appearing_after_missing_open_stops_first_save() {
        let path = temp_path();
        let mut s = SettingsState::with_path(path.clone());
        s.open();
        // Opened missing: the networkless seed is documented and drafted.
        assert!(
            s.doc.is_some(),
            "a missing open seeds the networkless document"
        );
        assert!(s.draft.is_some());
        assert!(s.error.is_none());
        // ...then a dangling symlink appears at the chosen primary.
        std::os::unix::fs::symlink("missing-target.toml", &path).unwrap();
        assert!(
            !s.save_draft(),
            "a stale save must not land on a dangling entry: {:?}",
            s.error
        );
        assert!(s.status.is_none(), "no success signal for a stopped save");
        // The adopted entry is dangling: its load error surfaces and it is
        // NOT a synthetic writable default.
        assert!(
            s.doc.is_none(),
            "a dangling config must not become a default document"
        );
        assert!(s.draft.is_none(), "no draft without a loaded document");
        let err = s.error.as_deref().unwrap();
        assert!(err.contains("appeared"), "{err}");
        assert!(err.contains("failed to load"), "{err}");
        // The symlink is untouched and its target is still absent — nothing
        // was written or created.
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "the primary must stay a symlink"
        );
        assert_eq!(
            std::fs::read_link(&path).unwrap(),
            std::path::Path::new("missing-target.toml"),
            "the link target must be unchanged"
        );
        let target = path.parent().unwrap().join("missing-target.toml");
        assert!(
            !target.exists(),
            "the save must not create the symlink's target"
        );
        assert_no_leftovers(&path);
        // Cleanup: only this test's own entries.
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(path.parent().unwrap()).unwrap();
    }

    /// A household-style fixture (comments, a foreign [voice] table,
    /// credentials, work defaults) keeps ALL of it through an ordinary
    /// provider edit — the missing-file path never reserializes an existing
    /// document.
    #[test]
    fn household_fixture_keeps_comments_voice_and_credentials_through_provider_change() {
        const HOUSEHOLD: &str = r#"# household config — comment that must survive
default_provider = "hermes"

[memory]
base_url = "http://10.0.0.5:9000" # the foreman service
min_priority = 95

[work.agents]
search_paths = ["/home/example/bin"]

# awili-voice's table — awili-config must pass it through untouched
[voice]
auto_speak = true
tts_model = "af_bella"

[[providers]]
name = "hermes"
kind = "hermes"
base_url = "http://10.77.68.86:8080"
models = ["hermes-4"]
api_key = "local-secret-key"
"#;
        let path = temp_path();
        std::fs::write(&path, HOUSEHOLD).unwrap();
        let mut s = SettingsState::with_path(path.clone());
        s.open();

        // A normal provider change (base_url) through the structured panel.
        s.begin_field(Field::ProviderUrl(0));
        assert!(s.commit_draft_field("http://10.0.0.7:8080"));
        assert!(s.save_draft(), "household save: {:?}", s.error);

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("# household config"),
            "the comment must survive"
        );
        assert!(
            on_disk.contains("# the foreman service"),
            "the inline comment must survive"
        );
        assert!(
            on_disk.contains("# awili-voice's table"),
            "the foreign table's comment must survive"
        );
        assert!(
            on_disk.contains("auto_speak = true"),
            "the foreign [voice] table must survive"
        );
        assert!(
            on_disk.contains("tts_model = \"af_bella\""),
            "the foreign [voice] values must survive"
        );
        assert!(
            on_disk.contains("search_paths = [\"/home/example/bin\"]"),
            "untouched [work.agents] must survive"
        );
        assert!(
            on_disk.contains("api_key = \"local-secret-key\""),
            "the credential must survive"
        );
        assert!(
            on_disk.contains("http://10.0.0.7:8080"),
            "the provider change must land"
        );
        let cfg = Config::parse(&on_disk).unwrap();
        assert_eq!(
            cfg.providers[0].api_key.as_deref(),
            Some("local-secret-key")
        );
        assert_eq!(
            cfg.memory.base_url, "http://10.0.0.5:9000",
            "untouched sections stay"
        );
    }
    // ---- N1: [notifications] switches -------------------------------------

    /// N1: flipping `enabled` off and saving writes `enabled = false` and
    /// persists; reopening shows it off.
    #[test]
    fn notification_toggle_saves_and_round_trips() {
        let text = "default_provider = \"hermes\"\n\n[notifications]\nenabled = true\n";
        let mut s = state_with_file(text);
        s.open();
        assert!(s.draft.as_ref().unwrap().notify.enabled, "on by default");
        s.toggle_notify(NotifyToggle::Enabled);
        assert!(s.save_draft(), "save must land: {:?}", s.error);
        let on_disk = std::fs::read_to_string(s.path()).unwrap();
        assert!(
            on_disk.contains("[notifications]\nenabled = false"),
            "{on_disk}"
        );
        let mut s2 = SettingsState::with_path(s.path().clone());
        s2.open();
        let notify = &s2.draft.as_ref().unwrap().notify;
        assert!(!notify.enabled, "reopened switch is off");
        assert!(notify.approvals && notify.finished, "the others stay on");
    }

    /// N1: each pill flips exactly its own switch, twice returns it.
    #[test]
    fn notification_toggle_flip_via_api() {
        let mut s = state_with_file("default_provider = \"hermes\"\n");
        s.open();
        let base = s.draft.as_ref().unwrap().notify.clone();
        let cases = [
            (NotifyToggle::Enabled, "enabled"),
            (NotifyToggle::Approvals, "approvals"),
            (NotifyToggle::Finished, "finished"),
            (NotifyToggle::Failed, "failed"),
            (NotifyToggle::Sound, "sound"),
        ];
        for (which, name) in cases {
            s.toggle_notify(which);
            let n = s.draft.as_ref().unwrap().notify.clone();
            let flipped = [
                ("enabled", n.enabled != base.enabled),
                ("approvals", n.approvals != base.approvals),
                ("finished", n.finished != base.finished),
                ("failed", n.failed != base.failed),
                ("sound", n.sound != base.sound),
            ];
            for (field, changed) in flipped {
                assert_eq!(changed, field == name, "{which:?} vs {field}");
            }
            s.toggle_notify(which);
            assert_eq!(s.draft.as_ref().unwrap().notify, base);
        }
    }
}
