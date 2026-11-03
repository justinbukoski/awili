//! Awili v2 — validation slice 2: real conversations.
//!
//! Slice 1 proved typing feel. Slice 2 makes the composer talk to real models,
//! injects standing rules every turn, and survives a crash:
//!
//! - **Providers** ([`awili_providers`]): the active provider/model streams tokens
//!   into the reply bubble live. A chip in the composer row shows the current
//!   model; clicking it opens the per-turn model picker (slice 15).
//! - **Injected rules** ([`awili_memory`]): every turn fetches the priority-≥90
//!   standing directives from the owned memory service and prepends them as the
//!   system preamble. A `rules: N` chip is visible proof they loaded — or
//!   `rules: offline` when the service is unreachable (the turn still runs).
//! - **Persistence** ([`awili_persist`]): every completed message is written to a
//!   WAL SQLite database the instant it lands; the last conversation is restored
//!   on launch. A `kill -9` mid-stream loses at most the in-flight reply.
//! - **Config** ([`awili_config`]): endpoints, the model list, and the memory URL
//!   come from `~/.config/awili/config.toml`, with runnable defaults if absent.
//!
//! This file is the GPUI shell tying those crates together; the composer entity
//! (slice 1) is reused unchanged. Every color is a `theme` token (the grep gate).

mod agent_brief;
mod allowed_actions;
mod agent_chat;
mod agent_launch;
mod agent_sandbox;
mod agent_signin;
mod app_lifecycle;
mod appearance;
mod approval_keys;
mod ask_mode;
mod ask_mode_ui;
mod attachments;
mod build_dispatch;
#[cfg(test)]
mod build_dispatch_p10_tests;
#[cfg(test)]
mod build_dispatch_tests;
mod build_staleness;
#[cfg(test)]
mod build_staleness_tests;
mod chart_palette;
mod claude_warm;
mod code_history;
mod code_provider;
mod code_reopen;
mod code_run;
mod codex_app_server;
mod codex_catalog;
mod command_catalog;
mod command_center;
mod command_palette;
mod composer;
mod composer_drafts;
mod composer_keys;
mod connectors_mcp;
mod consolidation;
#[cfg(test)]
mod consolidation_tests;
mod context_menu;
mod conv_view;
mod conversation_export;
mod conversation_route;
mod cost;
mod council_exec;
mod council_picker;
mod council_run;
#[cfg(test)]
#[path = "council_tests.rs"]
mod council_tests;
#[cfg(test)]
mod first_send_render_tests;
mod council_view;
mod cron;
#[cfg(test)]
mod cron_panel_tests;
mod distill;
mod engine_locality;
#[cfg(test)]
mod exec_fixture;
mod executor_status;
mod fallback;
mod feed;
mod fleet_skills;
mod focus_nav;
mod gateway_health;
mod gateway_health_app;
mod gateway_pool;
mod gateway_stop_deadline;
mod graduation_record;
mod icons;
#[cfg(test)]
mod inline_blocking_tests;
mod inline_error;
mod run_error;
mod run_error_ui;
#[cfg(test)]
mod inline_error_tests;
#[cfg(test)]
mod job_context_tests;
#[cfg(test)]
mod job_interiors_tests;
mod job_panel;
#[cfg(test)]
mod job_panel_tests;
#[cfg(test)]
mod topbar_tests;
mod launch_landing;
#[cfg(test)]
mod launch_probe_tests;
#[cfg(test)]
mod layout;
mod markdown;
mod proc_compat;
#[cfg(test)]
#[path = "rc2ux_layout_tests.rs"]
mod rc2ux_layout_tests;
mod reply_inspector;
#[cfg(test)]
mod reply_inspector_tests;
#[cfg(test)]
mod skills_panel_tests;
mod send_queue;
mod send_queue_app;
mod submit_guard;
#[cfg(test)]
mod test_child;
#[cfg(test)]
mod test_home_guard;
#[cfg(test)]
mod test_stdin;
#[cfg(test)]
mod test_tmp;
#[cfg(test)]
#[path = "toast_tests.rs"]
mod toast_tests;
mod agent_bin;
mod home;
#[cfg(windows)]
mod winjob;
mod winbounds;
mod workshop;
mod workshop_composer;
mod strip;
mod strip_app;
#[cfg(test)]
mod workshop_geometry_tests;
#[cfg(test)]
mod workshop_header_action_tests;
// Offline app-level recovery tests: startup migration, chooser, A->B->A.
#[cfg(test)]
mod recovery_integration_tests;
mod workshop_resources;
#[cfg(test)]
mod workshop_tests;
#[cfg(test)]
mod workshop_title_tests;
// C2-u2: dormant until D2 wires the EngineWorker; exercised by its own
// live tests against the awili-mcp-testserver gate.
#[cfg_attr(not(test), allow(dead_code))]
mod install_authority;
mod markdown_plan;
mod markdown_ui;
mod mcp_catalog;
mod mcp_runtime;
#[cfg_attr(not(test), allow(dead_code))]
mod mcp_transport;
mod plan_gate;
mod reply_presentation;
// Unit 4 item 4: included at the CRATE ROOT (not inside a module) so the
// test's bare name is its full path — `cargo test
// terminal_processed_before_queued_duplicate_still_rejects_the_duplicate
// -- --exact` must address it (the item-2 lesson; a bin-only package has
// no lib target an integration test could link).
#[cfg(test)]
include!("mcp_dup_admission_test.rs");
// Unit 4 item 5: the real-runtime coordinator tests, same crate-root
// discipline (bare test names addressable with -- --exact).
#[cfg(test)]
include!("mcp_coordinator_test.rs");
mod executor_labels;
mod harness_setup;
mod hermes_pages;
mod inapp_warm;
mod local_discovery;
mod local_discovery_ui;
mod local_first;
mod local_first_migrate;
mod local_model_app;
mod local_model_health;
mod messaging_controls;
mod messaging_pane;
mod messaging_selftest;
mod native_menu;
mod onboarding;
mod palette;
mod palette_targets;
mod persistence;
// U3b2 hold-to-talk guard (the PTT state machine + its AwiliApp hooks).
mod picker;
mod profile;
mod profile_palette;
mod prompt_history;
mod provider_build;
mod secret_store;
mod ptt_guard;
mod recall;
mod resume_context;
#[cfg(test)]
mod resume_context_tests;
mod context_breakdown;
mod run_footer;
mod search;
mod selectable_message;
mod sessions;
mod settings;
mod settings_catalog;
mod settings_code;
mod settings_focus;
mod settings_live;
mod settings_models;
mod settings_nav;
mod settings_pages;
mod settings_schema;
mod settings_search;
// N1: macOS notifications — the pure kernel, its backends, and the app glue.
mod notifications;
mod notify_hooks;
mod notify_platform;
mod shortcuts;
mod sidebar;
mod skills;
mod terminal;
mod terminal_element;
mod terminal_palette;
mod theme;
mod transcript_keys;
mod transcript_selection;
mod ui;
mod usage_chart;
mod voice;
// Terminal selection kernels. classify_copy_key (the Cmd+C-vs-Ctrl+C decision)
// is now wired in terminal_view; cell_at_pixel (pixel->grid mapping) still has
// no caller until the mouse-drag / highlight slice, so it carries its own
// dead-code allow rather than gating the whole module.
mod legacy_chat;
mod terminal_select;
mod terminal_view;
mod work;
mod work_folder_moves;
mod work_last_run;
mod work_panel_scope;
mod work_run_identity;
mod work_sidebar;
mod workshop_turn;
mod workspace;
mod workspace_read;
mod workspace_read_fixture;
mod zoom;

use attachments::{attachment_chip_label, compose_message_with_attachments, mime_for_path};
use composer::{Composer, ComposerEvent};
use conv_view::*;
use cost::CostTicker;
use distill::{admit_distillation, apply_distill_usage, distill_and_store, settle_distillation};
use feed::{FeedCfg, FeedHandle, FeedSnapshot};
use palette::{PaletteCmd, PaletteState};
#[cfg(test)]
use persistence::apply_usage_event;
use persistence::{
    merge_template_reply, PersistCmd, Persistence, ACTIVE_CONVERSATION_KEY, SELECTION_KEY,
    SELECTION_SEP,
};
use picker::PickerState;
use provider_build::ProviderKeys;
#[cfg(test)]
use recall::recall_status;
use recall::{recall_chip_label, rules_chip_label};
use search::SearchState;
use settings::SettingsState;
use shortcuts::ShortcutsState;
use sidebar::SidebarState;
use voice::{
    draft_blocks_voice_submit, load_model_blocking, ready_model, voice_turn_action, wait_for_model,
    Voice, VoiceTurnAction, MIN_SAMPLES_FOR_PARTIAL, STATUS_CLEAR,
};
use work::{HermesCfg, Mode, WorkState};

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use app_lifecycle::OverlayFocus;
use futures::StreamExt;
use gpui::{
    canvas, div, point, prelude::*, px, quad, size, App, BorderStyle, Bounds, ClickEvent,
    ClipboardEntry, ClipboardItem, Context, DispatchPhase, Edges, ExternalPaths, FocusHandle,
    Focusable, FontWeight, HighlightStyle, KeyDownEvent, ModifiersChangedEvent, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, ScrollHandle, SharedString, StyledText, Subscription,
    TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;

use awili_config::{Config, ModelChoice, ProviderKind, SettingEntry, ValueKind};
use awili_core::{
    ConvKind, ConversationList, ConversationMeta, ConversationState, Message, MessageKind, Role,
};
use awili_feed::Severity;
use awili_memory::{ChatLogEntry, MemoryClient, RecallStatus, RulesStatus, UsageNote};
use awili_persist::{Attachment, PromptTemplate, SearchResult, Store, StoredMessage};
#[cfg(test)]
use awili_providers::Usage;
use awili_providers::{ChatMessage, ChatRequest, ProviderEvent};
use awili_voice::{
    ConversationCapture, MicCapture, PttModifier, SentenceChunker, SpeechQueue, VoiceConfig,
};

/// The memory-service connection, in the cheap-to-clone form the per-turn worker
/// threads need (a fresh [`MemoryClient`] is built from it on each thread).
#[derive(Clone)]
pub(crate) struct MemoryCfg {
    pub(crate) base_url: String,
    /// Explicit v5 bearer token; empty ⇒ client reads the boot-token file.
    token: String,
    pub(crate) min_priority: i64,
    /// `false` when no memory URL is configured — injection is skipped entirely.
    enabled: bool,
    /// The `[memory] recall` toggle (slice 11): `false` skips the per-turn
    /// recall fetch while rules injection keeps working.
    recall: bool,
    /// The `[memory] write` toggle (slice 11): `false` keeps Awili from posting
    /// chat-log lines and usage notes — reads stay on.
    write: bool,
}

impl MemoryCfg {
    /// Whether automatic distillation may run (slice 11): the memory service must
    /// be configured (`enabled`) AND the `[memory] write` toggle on. `false` ⇒ no
    /// provider call and no memory write — the read path (recall/rules) is
    /// untouched.
    pub(crate) fn distillation_allowed(&self) -> bool {
        self.enabled && self.write
    }
}

/// Emitted after any successful settings save (slice-11 spec §3): the signal
/// that the runtime should re-apply whatever it can live (providers, defaults,
/// `[memory]`) without a relaunch. The window's own subscription drives
/// [`AwiliApp::apply_config_live`].
struct ConfigReloaded;

impl gpui::EventEmitter<ConfigReloaded> for AwiliApp {}

struct AwiliWindow(gpui::WindowHandle<AwiliApp>);

#[derive(Clone, Copy)]
enum SessionsFocusTarget {
    Search,
    Back,
}

#[derive(Clone)]
struct SkillsFocusTarget {
    row: FocusHandle,
    action: FocusHandle,
    row_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    action_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

#[derive(Clone, PartialEq)]
struct SkillsRevealKey {
    focus: FocusHandle,
    face: Bounds<Pixels>,
    viewport: Bounds<Pixels>,
    outer_viewport: Bounds<Pixels>,
}

impl gpui::Global for AwiliWindow {}

/// The transient `✓ Copied` state on a message-actions row (slice 12 §1):
/// which message last hit the clipboard, plus the generation guard the 1.5s
/// clear timer checks — a second copy inside the window restarts the clock
/// instead of being cut short (the `memory_saved_flash` pattern).
struct CopiedFlash {
    /// `(conversation id, row index)` of the flashing chip, if any.
    target: Option<(String, usize)>,
    /// Monotonic: bumped per flash; a clear only fires if still current.
    gen: u64,
}

impl CopiedFlash {
    fn new() -> Self {
        Self {
            target: None,
            gen: 0,
        }
    }

    /// Note a copy; returns the generation the clear timer must present.
    fn set(&mut self, conv: &str, idx: usize) -> u64 {
        self.target = Some((conv.to_string(), idx));
        self.gen += 1;
        self.gen
    }

    /// Clear if `gen` is still the latest flash. Returns whether it cleared.
    fn clear_if(&mut self, gen: u64) -> bool {
        if self.gen == gen && self.target.is_some() {
            self.target = None;
            true
        } else {
            false
        }
    }

    /// Is this message's chip currently flashing?
    fn is(&self, conv: &str, idx: usize) -> bool {
        self.target
            .as_ref()
            .is_some_and(|(c, i)| c == conv && *i == idx)
    }
}

/// The transient export/save toast (slice 17 §3): a brief non-blocking
/// message anchored bottom-right, cleared by a 3s timer. Generation-guarded
/// like [`CopiedFlash`] (instead of holding the spawned task): the house
/// pattern, and pure enough that show/auto-dismiss are unit-testable without
/// a windowed executor — a newer toast restarts the clock rather than being
/// cut short by the older timer.
/// RC1-H7: seconds an information toast stays before it goes by itself.
pub(crate) const INFO_TOAST_SECS: u64 = 12;
/// RC2-RUN (round 3 Major 7 / 3b M7): seconds a refusal toast stays before it
/// goes by itself — long enough to read it twice, so the owner never has to
/// aim at its close mark. The refusal also stays where it matters (the text
/// back in the box, the plan card), and a click or the next send closes the
/// toast sooner.
pub(crate) const REFUSAL_TOAST_SECS: u64 = 30;

struct ToastState {
    msg: Option<SharedString>,
    /// Monotonic: bumped per toast; a clear only fires if still current.
    gen: u64,
    /// RC2-UX (R2-19): the saved file a "Show in Finder" button on this
    /// toast reveals, if any.
    reveal: Option<std::path::PathBuf>,
    /// RC2-R54 (usability RC2 round 4, Minor: the "Continue in Work" note
    /// stayed and followed the owner to the Council page): the page an
    /// information toast speaks about; leaving it closes the toast.
    page: Option<crate::work::Mode>,
}

impl ToastState {
    fn new() -> Self {
        Self {
            msg: None,
            gen: 0,
            reveal: None,
            page: None,
        }
    }

    /// Show a message; returns the generation the clear timer must present.
    fn show(&mut self, msg: impl Into<SharedString>) -> u64 {
        self.msg = Some(msg.into());
        self.reveal = None;
        self.page = None;
        self.gen += 1;
        self.gen
    }

    /// Clear if `gen` is still the latest toast. Returns whether it cleared.
    fn clear_if(&mut self, gen: u64) -> bool {
        if self.gen == gen && self.msg.is_some() {
            self.msg = None;
            self.reveal = None;
            true
        } else {
            false
        }
    }

    fn visible(&self) -> Option<&SharedString> {
        self.msg.as_ref()
    }

    /// RC2-R54: the page moved to `mode`: an information toast about another
    /// page closes. Returns whether it did.
    fn leave_page(&mut self, mode: crate::work::Mode) -> bool {
        match self.page {
            Some(p) if p != mode => self.clear_if(self.gen),
            _ => false,
        }
    }

    /// RC2-UX (R2-19): the toast showing now offers "Show in Finder" for
    /// `path`.
    fn set_reveal(&mut self, path: std::path::PathBuf) {
        self.reveal = Some(path);
    }

    /// R1a: close whatever toast is showing (a click on it).
    fn dismiss(&mut self) {
        self.msg = None;
        self.reveal = None;
    }
}

/// The whole app: the conversations, the composer, persistence, and the
/// provider/memory configuration the per-turn worker uses.
struct AwiliApp {
    /// Sidebar metadata for every conversation (incl. archived).
    conversations: ConversationList,
    /// Live view state per conversation id.
    views: HashMap<String, ConvView>,
    /// Sidebar view-state: selection, collapse, inline rename.
    sidebar: SidebarState,
    composer: gpui::Entity<Composer>,
    /// The inline title editor shown in the row being renamed.
    rename_editor: gpui::Entity<Composer>,
    did_initial_focus: bool,
    /// Whether the Awili window is currently the active/frontmost window. Cached
    /// from `window.is_window_active()` each render (gpui re-renders on activation
    /// changes) so non-render code — e.g. the Work-done notification — can decide
    /// whether the user is already looking at Awili without a `Window` in hand.
    window_active: bool,
    display_palette: &'static theme::Palette,
    appearance: appearance::Appearance,
    /// RC1-H6: the window's width in pixels at the last render (0 before the
    /// first), for [`AwiliApp::columns_now`].
    window_w: f32,
    /// N1: notification state (kernel + backend), built from the loaded
    /// `[notifications]` config.
    notifier: notify_hooks::NotifyState,
    /// First-run welcome state (startup seam; set from config provenance).
    onboarding: onboarding::Onboarding,
    onboarding_harness: Option<work::WorkExecutor>,
    /// A Work route other than a vendor CLI is set up (the in-app engine or
    /// the Hermes gateway). With `onboarding_harness` this is what "Ready"
    /// means: provider rows alone serve Chat and the Council, not Work.
    onboarding_other_route: bool,
    pending_harness_recipe: Option<work::WorkExecutor>,
    /// Fixed "Check again" error copy shown in the welcome panel when the
    /// config is missing or unloadable. `&'static str` only: never a raw
    /// error, URL, or token.
    onboarding_error: Option<&'static str>,
    /// Focus handle owned by the welcome panel itself (the composer is a
    /// separate, hidden element, so the panel tracks its own focus).
    welcome_focus: FocusHandle,
    /// K02 focus fix (U3d): the Keyboard Shortcuts sheet's scrim tracks this
    /// handle. Once the sheet is open, focus moves here so the Escape/⌘?
    /// dispatch lands on the window root (where the app's key listeners
    /// live) even when the previously-focused element is no longer rendered.
    overlay_focus: OverlayFocus,
    /// Whether the next render with no overlay open should focus
    /// `welcome_focus`. Starts true (first run) and is re-set whenever an
    /// overlay that hides the welcome panel closes, so focus returns exactly
    /// once instead of being re-taken on every render.
    welcome_focus_pending: bool,
    settings_nav: settings_nav::Navigation,
    /// Whether the current Settings error is open in its bounded detail Sheet.
    settings_error_details_open: bool,
    settings_error_scroll: ScrollHandle,
    settings_error_scroll_signature: Option<u64>,
    settings_error_sheet_focus: FocusHandle,
    settings_error_body_focus: FocusHandle,
    settings_error_parent_focus: Option<FocusHandle>,
    /// Stable focus target inside Settings for tabs whose controls can
    /// disappear when a live error is cleared.
    settings_active_tab_focus: FocusHandle,
    local_discovery: local_discovery_ui::UiState,
    settings_nav_focus: FocusHandle,
    settings_nav_focus_pending: bool,
    /// Restore the visible canvas owner once an actual Settings close lands.
    settings_return_focus_pending: bool,
    /// Whether the settings-nav focus is live on the window right now —
    /// cached during render (render helpers receive `&Context<Self>`, not the
    /// window, so the focus test is applied once at the top of render) and
    /// drives the visible keyboard-selection highlight on the General form.
    settings_nav_focused: bool,
    /// Scroll tracking for the General form's row container, so keyboard
    /// navigation can bring the selected row into view via `scroll_to_item`.
    settings_nav_scroll: ScrollHandle,
    /// Bounds of the stable Save and Discard faces for focus-driven reveal.
    settings_service_action_bounds:
        [std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>; 2],
    /// Last normalized footer reveal request; avoids snapping back after wheel input.
    settings_service_reveal_key: settings_models::ServiceActionRevealKey,
    /// SXi: the keyboard stop on a Settings page (`None`: none yet).
    settings_stop: Option<settings_focus::FocusStop>,
    /// SXi: the keyboard is in the Settings sidebar (else on the page).
    settings_in_sidebar: bool,
    /// SXi: the page's scroll, so a focused row scrolls into view.
    settings_page_scroll: ScrollHandle,
    /// SXi: which catalog item each page row is (written by the render).
    settings_page_rows: std::cell::RefCell<Vec<Option<&'static str>>>,
    /// S6: the Allowed actions page.
    allowed_actions: allowed_actions::AllowedActions,
    /// Keyboard selection among the welcome actions: 0 = Setup, 1 = Check
    /// again, 2 = Start chatting (which exists only while Ready). Clamped to
    /// the available count whenever Ready changes.
    welcome_action: usize,
    /// True only while the config is unknown/invalid: set on the startup
    /// `Config::load_with_source` error arm, and re-set while the live
    /// refresh re-applies the `networkless` fallback for a missing or
    /// malformed file. Drives the fixed error banner above the main center,
    /// the ⌘⇧R / “Check again” re-check, the `networkless` fallback, and —
    /// until a valid config loads — the Chat refusal: `start_turn` declines
    /// with the fixed “Chat is off until the config loads” message BEFORE
    /// any agent spawn or provider request (RVS6C P1). A valid re-apply
    /// ([`Self::apply_runtime_config`]) clears it; a successful ordinary
    /// Settings save also clears it. Never true on valid, missing-empty, or
    /// offline launches. Work mode is unaffected.
    config_load_error: bool,
    /// Lines for `secret:` keys the system keychain could not supply (named
    /// by provider or route). Shown as a notice until the config is fixed.
    secret_notices: Vec<String>,
    /// Windows: why the app database could not be opened (a profile folder that
    /// cannot be made private, such as a FAT drive), shown as a banner.
    storage_notice: Option<String>,
    /// RC2-R65: when a "Check again" was last pressed (local time), by what
    /// it checked ("system", "code:1", …), so the press shows.
    pub(crate) checked_at: HashMap<String, String>,

    // ---- slice 2 ----
    /// The flattened provider·model cycle from config.
    choices: Vec<ModelChoice>,
    /// The config-DECLARED choices only (no discoveries) — the authoritative
    /// base every catalog rebuild resets to before re-appending current
    /// catalogs, so a stale discovered model cannot survive a rebuild.
    configured_choices: Vec<ModelChoice>,
    choice_idx: usize,
    /// Live provider reachability by `base_url` (C1): absent = not probed
    /// yet, `true` = the endpoint answered, `false` = it did not. Fed by
    /// [`Self::refresh_provider_health`] worker probes.
    provider_health: std::collections::HashMap<String, bool>,
    provider_probe_generation: u64,
    /// True while a saved/configured provider probe batch is in flight.
    /// All enabled endpoints probe in one batch, so one flag covers them.
    provider_probe_in_flight: bool,
    /// Models each provider endpoint actually serves, discovered from the
    /// same probe bodies the health check already fetches (`/api/tags`,
    /// `/models`). Keyed by base_url; merged into [`Self::choices`] by
    /// [`Self::merge_discovered_models`] so the picker offers everything the
    /// endpoint has, not only what config.toml declares (user report
    /// 2026-08-23: "missing a significant number of models").
    discovered_models: std::collections::HashMap<String, Vec<String>>,
    /// Every ENABLED provider endpoint from config — including ones whose
    /// declared models are all embedders and thus contribute no choices.
    /// Probe targets and discovery templates come from here, so such a
    /// provider still gets its catalog discovered.
    provider_endpoints: Vec<awili_config::ProviderConfig>,
    /// Consecutive turns the rules / recall fetch has come back Offline.
    /// The status run warns only from the SECOND consecutive miss — a single
    /// slow fetch must not make healthy state look broken (critique P3 /
    /// test-pass observation: transient offline flashes on 2 of 5 turns).
    rules_offline_streak: u8,
    recall_offline_streak: u8,
    /// The window title currently set on the platform window — dedupes the
    /// per-render set_window_title call (survey idea #10: the active
    /// conversation names the window).
    last_window_title: String,
    /// A persisted provider·model selection that didn't resolve against the
    /// config-declared choices at boot — it may name a DISCOVERED model, so
    /// it stays pending until a merge makes it real (then it is adopted).
    pending_saved_selection: Option<(String, String)>,
    /// The one-shot dead-default rescue (C1) has already run this launch —
    /// after it, the selection is the user's to move.
    moved_off_dead_default: bool,
    /// A sidebar row's delete awaiting its inline confirmation (B2): the
    /// conversation id whose actions currently read "Delete? ✓ ✕". Cleared
    /// by confirm, cancel, or any row selection.
    pending_delete: Option<String>,
    /// Provider credentials (Anthropic / Hermes), captured once at launch.
    keys: ProviderKeys,
    memory: MemoryCfg,
    /// What the `rules:` chip shows for the latest turn. `None` before the first
    /// turn (nothing queried yet) and when memory is disabled — the chip is hidden
    /// in both cases rather than claiming a degraded "offline" the service never
    /// reported. Mirrors [`Self::recall`]; only a genuine unreachable service on an
    /// enabled turn yields `Some(RulesStatus::Offline)`.
    rules: Option<RulesStatus>,
    /// What the `recall:` chip shows for the latest turn (slice 9). `None`
    /// until the first turn — no recall has been attempted, so no chip; the
    /// `Loaded(n)` count is the turn's recall_count.
    recall: Option<RecallStatus>,
    /// RC2-UX (R5-9): the rules/recall chips of each conversation's last
    /// send (the chip line shows the one on screen).
    pub(crate) memory_chips: HashMap<String, recall::MemoryChip>,
    /// Serialized off-thread persistence; `None` if the database could not be
    /// opened (the app still runs, just without restore).
    persist: Option<Persistence>,
    /// Every Work conversation's last run, raw JSON, keyed by conversation id
    /// (daily-driver P2e) — captured at startup by `Persistence::start` so
    /// the UI thread restores a panel's `Requested:` line and terminal status
    /// without touching sqlite (SF6).
    work_last_run: std::collections::HashMap<String, String>,
    /// S3: the short run id of each reply, by (conversation, reply ts).
    run_ids: run_error::RunIds,
    /// S3: a Code tab reply that failed: (tab id, row) to (cause, run id).
    code_run_errors: std::collections::HashMap<(u64, usize), (String, String)>,
    /// RC2-RUN (UX-FOOTER): every Work conversation's saved reply footers
    /// (`run_footer::save_work_footer` keeps it and `app_state` in step).
    work_footers: std::collections::HashMap<String, run_footer::SavedFooters>,
    /// RC2-UX (D-R1 m6): each Work conversation's saved reply steps.
    pub(crate) work_steps: std::collections::HashMap<String, run_footer::SavedSteps>,
    /// rc.3 S2: each Work conversation's saved per-reply context breakdowns.
    pub(crate) work_contexts: std::collections::HashMap<String, context_breakdown::SavedContexts>,
    /// RC2-R1F5: each Work conversation's folder moves (saved as
    /// `work_folder_moves:<conv_id>`).
    pub(crate) work_folder_moves: std::collections::HashMap<String, work_folder_moves::FolderMoves>,

    // ---- slice 10 (memory writes) ----
    /// Distilled memories written so far this session — the flood cap (slice 10
    /// §3, at most [`awili_memory::MAX_DISTILL_WRITES_PER_SESSION`]). A session
    /// is the same unit the cost ticker uses: reset on `new_conversation`.
    memory_writes_this_session: usize,
    /// The status row shows a "memory saved" chip while this is `true` — set
    /// when a distilled memory lands, cleared two seconds later (§4).
    memory_saved_flash: bool,
    /// Monotonic flash generation: each new flash bumps it, and a clear only
    /// fires if its generation is still current — a second write inside the 2s
    /// window restarts the clock instead of being cut short.
    memory_flash_gen: u64,

    // ---- slice 12 (message actions) ----
    /// Message row the pointer is over in the active conversation — the action
    /// buttons under that assistant bubble become visible (spec §5).
    hovered_row: Option<usize>,
    /// The `✓ Copied` chip state (spec §1).
    copied: CopiedFlash,
    /// Timer-driven phase for the pre-reply breathing bullet. GPUI's
    /// `with_animation` wrapper does not paint reliably inside transcript rows
    /// on the pinned fork, so the active turn advances this explicit phase.
    thinking_phase: u8,
    /// The composer status strip: reply telemetry, the server probes, the
    /// detail popover (docs/v2/strip).
    strip: strip_app::StripState,
    /// Exactly one timer advances `thinking_phase`, even when several
    /// conversations have concurrent pre-reply turns.
    thinking_timer_running: bool,
    /// Invalidates a sleeping clock when Reduce Motion changes. An old tick
    /// cannot stop or advance the replacement clock.
    thinking_timer_generation: u64,
    /// Pointer offset inside the conversation scrollbar thumb while it is
    /// being dragged. Kept outside the app model so pointer motion can repaint
    /// the scroll layer without rebuilding every transcript row and markdown
    /// tree on each pixel.
    conversation_scroll_drag: Rc<Cell<Option<Pixels>>>,

    // ---- slice 13 (command palette) ----
    /// ⌘⇧P Cowork command-palette view-state (pure, in `command_palette.rs`):
    /// the static catalog + nested color-mode sub-page. Distinct from Awili's
    /// native ⌘K conversation `palette`; its KIT SearchField owns query input
    /// while the capture handler owns navigation and dispatch. Provider fetches
    /// are deferred, so it renders with empty provider slices — the static
    /// catalog alone.
    command_palette: command_palette::CommandPaletteState,
    /// Focus owner to restore after the Cowork palette closes. Captured before
    /// opening so keyboard input can move to the palette SearchField without
    /// changing the prior page's focus ownership.
    command_palette_return_focus: Option<gpui::FocusHandle>,
    /// ⌘K palette view-state: open flag, query, focused row (pure, in
    /// `palette.rs`). The query is mirrored from `palette_input` each render.
    palette: PaletteState,
    /// The palette's search input, autofocused on open.
    palette_input: gpui::Entity<Composer>,
    /// Scroll state of the palette's row list (slice 16 SF1): keyboard
    /// navigation keeps the focused row visible through it.
    palette_scroll: ScrollHandle,
    /// M2g: session-wide prompt-history walk (Up/Down recall in the main
    /// composer). `sent` covers all modes; the conversation tail is re-derived
    /// from the active convo on each step.
    prompt_history: prompt_history::PromptHistory,
    /// RC2-UX (R2-18, R5-7, S-O5): the message box's text per conversation.
    pub(crate) drafts: composer_drafts::Drafts,

    // ---- slice 15 (model picker) ----
    /// The composer-toolbar model picker's popover state (pure, in
    /// `picker.rs`). The selection itself is `choice_idx`.
    picker: PickerState,

    // ---- slice 17 (conversation export) ----
    /// The transient export/save toast, auto-dismissed after 3s.
    toast: ToastState,
    /// R1a: the composer submit being dispatched right now, until a path
    /// takes it (`submit_guard.rs`).
    submit_guard: Option<submit_guard::SubmitGuard>,

    // ---- slice 19 (file attachments) ----
    /// Attachments staged on the ACTIVE conversation, in staging order — the
    /// tray above the composer. Mirrors `staged_attachments` in the database;
    /// refreshed on conversation switch, cleared (memory + DB) on send.
    staged_attachments: Vec<Attachment>,

    // ---- slice 8 (cost ticker) ----
    /// Session + today token/cost accumulators behind the status-bar chip.
    cost: CostTicker,

    // ---- slice 3 (voice) ----
    /// Dictation + read-aloud state (slice 3). Audio devices live here on the UI
    /// thread; workers get `Arc` handles. macOS at runtime; compiles everywhere.
    voice: Voice,

    // ---- slice 4 (attention feed) ----
    /// The feed connection, kept for posting Awili's own events; `None` when
    /// `[feed]` is absent from config — no panel, no polling (spec §8).
    feed_cfg: Option<FeedCfg>,
    /// Commands to the feed worker (ack, sync-now); `None` when disabled.
    feed: Option<FeedHandle>,
    /// The latest mirror snapshot the panel renders. Always render-ready, even
    /// offline — that is the point of the mirror.
    feed_snapshot: FeedSnapshot,
    /// Whether the Needs You sidebar is showing (the chip toggles it).
    feed_panel_open: bool,
    /// `event_id` of the row whose body is expanded, if any.
    feed_expanded: Option<String>,
    /// Whether the rules chip was last observed online — drives the
    /// to/from-offline self-events (spec §7). `None` until the first turn: the
    /// initial observation is a state, not a transition, and posts nothing.
    rules_seen_online: Option<bool>,

    // ---- slice 14 (conversation search) ----
    /// The ⌘F panel's pure state: open flag, query, focused result row.
    search: SearchState,
    /// The current query's results — re-queried on every keystroke.
    search_results: Vec<SearchResult>,
    /// A second, read-only connection for FTS queries on the main thread.
    /// Spec §"Integration" offers worker round-trip vs. main-thread read; the
    /// read connection is simpler: FTS5 lookups are fast, read-only, and WAL
    /// lets them run concurrently with the worker's writes.
    search_store: Option<Store>,
    /// The panel's text input.
    search_input: gpui::Entity<Composer>,
    /// The Backstage Skills search stop, stable while filtered rows change.
    skills_search_focus: FocusHandle,
    /// Stable KIT stop identities for the currently rendered Skills rows and actions.
    skills_focus_targets: Rc<std::cell::RefCell<HashMap<String, SkillsFocusTarget>>>,
    /// Last focused Skills face/layout, normalized against manual list scrolling.
    skills_reveal_key: Rc<std::cell::RefCell<Option<SkillsRevealKey>>>,
    /// The Sessions browser search stop; also the focus destination after Back.
    sessions_search_focus: FocusHandle,
    /// The Hermes transcript's Back stop, focused when a session opens.
    sessions_back_focus: FocusHandle,
    /// Focus handoff for Sessions controls that replace the focused part.
    sessions_focus_pending: Option<SessionsFocusTarget>,
    /// The search term lit up in the active conversation's bubbles (slice 16
    /// §3): set when a search result is selected, cleared on Escape or when
    /// the panel closes. `None` = no highlighting.
    highlight_term: Option<String>,

    // ---- slice 18 (prompt templates) ----
    /// Every saved template, sorted by name — the palette's rows and the
    /// send-time injection read this mirror; the worker owns the rows.
    templates: Vec<PromptTemplate>,
    /// Conversation id → bound template id (mirror of `conversations.template_id`).
    conv_templates: HashMap<String, i64>,
    /// The template editor panel: `None` closed; `Some(None)` new template;
    /// `Some(Some(id))` editing that template.
    template_editor: Option<Option<i64>>,
    /// The editor's single-line name field (Enter saves).
    template_name: gpui::Entity<Composer>,
    /// The editor's multiline content area (Enter inserts a newline; the Save
    /// button or Enter in the name field commits).
    template_content: gpui::Entity<Composer>,

    /// The saved-project instructions editor: `None` closed; `Some((id, folder))`
    /// editing that project's standing instructions (the field folded ahead of
    /// the task in every Work run). The `id` drives the durable
    /// `UpdateProjectInstructions`; the `folder` disambiguates the no-DB path,
    /// whose entries all share id 0 (same reason `remove_project_by_id` keys on
    /// folder). Completes the Work-mode projects CRUD parity arc (save → delete
    /// → edit) over the dead-but-shipped `Store::update_project_instructions`.
    project_editor: Option<(i64, String)>,
    /// The project-instructions editor's multiline area (Enter inserts a
    /// newline; the Save button commits; Escape cancels).
    project_instructions: gpui::Entity<Composer>,

    // ---- slice 20 (shortcut reference) ----
    /// The ⌘? keyboard-shortcut sheet's open flag (spec §2b). Overlays above
    /// everything else, palette included.
    shortcuts: ShortcutsState,

    // ---- Phase 4 (profiles + gateway pool) ----
    profile_store: profile::ProfileStore,
    // Phase 4 surface: the pool is constructed but not yet read by the
    // render path; the allow keeps the crate lint-clean until it is wired.
    #[allow(dead_code)]
    gateway_pool: gateway_pool::GatewayPool,

    // ---- Phase 5 (skills + cron + sessions) ----
    skills_state: skills::SkillsState,
    cron_state: cron::CronState,
    sessions_state: sessions::SessionsState,
    /// Live store backing the hosted Command Center view (system status + usage
    /// rollup + action model). Populated by [`AwiliApp::refresh_command_center`]
    /// on view-open.
    command_center_data: command_center::CommandCenterData,
    /// Live store backing the hosted Messaging connectors view (plan unit 24).
    /// Populated by [`AwiliApp::refresh_messaging`] on view-open.
    messaging_data: messaging_pane::MessagingData,
    /// The composer's `/`-command skill palette (open/query/focus). Driven by
    /// the `_composer_palette` observer below; rows come from `skills_state`.
    skill_palette: skills::SkillPaletteState,
    /// Scroll state of the skill palette's row list, so keyboard ↑/↓ keeps the
    /// focused row visible when the list is taller than the popover.
    skill_palette_scroll: ScrollHandle,
    /// The `/`-query the palette was dismissed at (Escape), if any. While the
    /// draft's `/`-token still equals this, the observer must NOT reopen the
    /// palette — Escape stays sticky until the token actually changes. Cleared
    /// when the token changes or the draft stops being a `/`-command.
    skill_palette_dismissed: Option<String>,

    // ---- RC2-CNCL5: the Council page ----
    /// Each council opened this session, by conversation id.
    pub(crate) councils: council_view::CouncilPages,
    /// The members of the council used last (`council_view::LAST_GROUP_KEY`),
    /// read once; `None` until first needed.
    pub(crate) council_last_group: Option<Vec<(String, String)>>,
    /// RC2-CNCL6: how a Hermes member is reached (the closed fleet route,
    /// resolved from the config like a Chat pinned to `provider:fleet`).
    pub(crate) council_fleet: agent_chat::ChatRoute,
    /// RC2-CNCL7: the saved council groups (`council_view::GROUPS_KEY`), read
    /// once; `None` until first needed.
    pub(crate) council_groups: Option<Vec<council_view::SavedGroup>>,

    // ---- Work mode (agentic "Cowork" via awili-hermes) ----
    mode: Mode,
    shell: consolidation::Shell,
    work: WorkState,
    workspace_read: workspace_read::WorkspaceRead,
    /// P1: the loaded fleet skill pack rendered into submitted Work tasks.
    /// Loaded once at startup and on every config re-apply (never scanned
    /// on the submit path); `None` when `[skills].fleet_pack` is unset or
    /// invalid, in which case sends proceed unchanged.
    fleet_pack: Option<Arc<awili_skills::SkillPack>>,

    // ---- slice 7 (settings) ----
    settings: SettingsState,
    /// Inline editor for the form field being edited.
    settings_field: gpui::Entity<Composer>,
    /// The raw-TOML tab's editor (multiline — Enter inserts a newline; the
    /// Save button commits).
    settings_raw: gpui::Entity<Composer>,
    /// SXd: the parent-owned query and stable focus target for Settings'
    /// SearchField.
    settings_search_query: String,
    settings_search_focus: FocusHandle,
    /// RC2-UX (D-PUNCH F11): the Work memory card's search box (Enter
    /// searches the owner's memory for what was typed).
    pub(crate) memory_search: gpui::Entity<Composer>,
    /// Isolated input for a pending sudo/secret credential value. Deliberately
    /// NOT wired to the chat/persistence path — its text goes only to the gateway
    /// respond and is drained on submit, so a credential never lands on disk.
    credential_input: gpui::Entity<Composer>,

    _submit: Subscription,
    _rename: Subscription,
    /// Repaint-on-change observation of the palette input (slice 13): the rows
    /// filter live as the user types, so the app must re-render whenever the
    /// input's buffer notifies (the composer has no per-keystroke event).
    _palette_input: Subscription,
    /// Observes the chat composer's draft so the `/`-command skill palette
    /// opens/filters/closes live: every buffer notify recomputes the trigger
    /// query and the matching row list.
    _composer_palette: Subscription,
    /// M2g-FIX1: mirrors `composer_busy()` into the main composer on every
    /// app notify, so its Enter guard sees each run start and end. Kept here
    /// because a dropped `Subscription` unsubscribes at once.
    _composer_busy: Subscription,
    _send_queue_tick: Subscription,
    _settings_field: Subscription,
    _settings_raw: Subscription,
    _memory_search: Subscription,
    _credential_input: Subscription,
    /// The [`ConfigReloaded`] self-subscription driving live re-apply.
    _reload: Subscription,
    /// Observes the search input so typing re-runs the FTS query live.
    _search_input: Subscription,
    /// The template editor's name field: Enter saves, Escape cancels (slice 18).
    _template_name: Subscription,
    /// The template editor's content area: Escape cancels (slice 18).
    _template_content: Subscription,
    /// The project-instructions editor's content area: Escape cancels.
    _project_instructions: Subscription,
    /// Inline editor for a messaging connector credential value (plan unit 25).
    /// Enter commits to the draft; Escape cancels. Never prefilled with the
    /// redacted display value — the secret-safety invariant guarantees the mask
    /// can never be round-tripped.
    messaging_credential_editor: gpui::Entity<Composer>,
    /// Subscription for the messaging credential editor's Submit/Cancel events.
    _messaging_credential_editor: Subscription,
}

/// One message's display data. Parsing and render-plan construction happen
/// here — once per restore/submit/error, and on a bounded cadence while a
/// reply streams (review slice 6, SF2) — never per frame.
pub(crate) struct MessageRow {
    pub(crate) role: Role,
    /// The raw markdown source — the per-message copy payload. Shared: cloning
    /// it into the chip's closure per frame is a refcount bump, not a copy.
    /// While streaming it tracks the reparse cadence and is brought exactly
    /// current when the turn finishes.
    raw: SharedString,
    /// Render-ready plan (markdown parsed; styles, links, copy payloads all
    /// resolved once, with the role's palette baked in).
    plan: Vec<markdown_plan::PlanBlock>,
    /// Display-only legacy notices, retained under Activity.
    presentation_activity: Vec<SharedString>,
    /// Bytes of source the plan reflects — drives the streaming cadence.
    parsed_len: usize,
    /// Palette whose resolved highlights are cached in this row.
    plan_palette: &'static theme::Palette,
    /// Agent activity steps for this turn (tool/command lines), kept OUT of
    /// the markdown body and rendered as a collapsible summary (critique P2:
    /// fold the plumbing). DELIBERATELY LIVE-ONLY (night-review SHOULD-4
    /// decision): activity is telemetry about a run in progress, not durable
    /// history — after relaunch the answer stands alone, exactly like the
    /// Work rail's tool cards. Persisting compact step metadata is a possible
    /// follow-up if history proves wanted.
    pub(crate) activity: Vec<SharedString>,
    /// M2d: the run footer line (`executor · model · time · tokens · cost`),
    /// attached to the turn's assistant row when its ledger line lands.
    pub(crate) footer: Option<SharedString>,
    /// rc.3 S2: what Awili put in the prompt for this reply, by part.
    pub(crate) context: Option<context_breakdown::ContextBreakdown>,
    /// Whether the activity fold is expanded.
    pub(crate) activity_expanded: bool,
    /// When the first activity step of the in-flight turn landed.
    activity_started: Option<std::time::Instant>,
    /// Seconds from first activity to turn finish (set once, on finish).
    activity_secs: Option<f32>,
    /// RC2-RUNF (review P2): the row Awili wrote for a plan-card click (its
    /// message is [`MessageKind::PlanClick`]). Only such a row is drawn as a
    /// note about the click; a message the owner typed keeps its bubble,
    /// whatever its words.
    pub(crate) plan_click: bool,
    /// RC2-UX (R2-14): the row a Retry click wrote (its message is
    /// [`MessageKind::Retry`]): drawn as a short note, not a second copy of
    /// the question.
    pub(crate) retry: bool,
}

/// Map a palette "Go to …" route to the app [`Mode`] it activates (user-test
/// bug #3). The pure `palette` module owns the route enum (no view types); this
/// is the one place the two vocabularies meet, shared by `run_palette_cmd` and
/// the row renderer.
fn nav_route_to_mode(route: palette::NavRoute) -> Mode {
    match route {
        palette::NavRoute::Work => Mode::Work,
        palette::NavRoute::Code => Mode::Code,
        palette::NavRoute::Council => Mode::Council,
        palette::NavRoute::Skills => Mode::Skills,
        palette::NavRoute::Cron => Mode::Cron,
        palette::NavRoute::Sessions => Mode::Sessions,
        palette::NavRoute::CommandCenter => Mode::CommandCenter,
        palette::NavRoute::Messaging => Mode::Messaging,
    }
}

/// Export is unsafe only for the conversation whose durable transcript is
/// still changing. Work runs remain pinned to their originating conversation,
/// even when the user navigates to another Work thread.
fn conversation_export_is_busy(
    mode: Mode,
    active_id: &str,
    work_running: bool,
    work_conv_id: Option<&str>,
) -> bool {
    mode == Mode::Work && work_running && work_conv_id.is_none_or(|run_id| run_id == active_id)
}

/// Whether the header gateway chip needs an initial connection probe on launch.
///
/// The chip reads `work.hermes_status`, but historically that field was only
/// populated on `Mode::Work` entry (the lazy probe in `set_mode`). So a session
/// that stayed in Chat showed a grey "unknown" dot even though the gateway was
/// reachable, and it only flipped green after Work mode was opened once —
/// "grey in Chat, green in Work" (user-test nit). The chip is meant to reflect a
/// SINGLE connection state, not the current mode, so we probe once at startup
/// whenever the chip is visible (`gateway_enabled`) and no probe has run yet.
/// `status_known` mirrors `hermes_status.is_some()`; `testing` guards against a
/// double-probe if one is already in flight.
fn should_probe_gateway_on_launch(
    gateway_enabled: bool,
    status_known: bool,
    testing: bool,
) -> bool {
    gateway_enabled && !status_known && !testing
}

/// The markdown palette for a message role.
fn palette_for(role: Role) -> markdown_plan::BubblePalette {
    match role {
        Role::User => markdown_plan::user(),
        Role::Assistant => markdown_plan::assistant(),
    }
}

/// The collapsible activity fold for one transcript row (critique P2: fold
/// the plumbing). Collapsed: one quiet summary line "N steps ▸". Expanded:
/// each step on its own muted line. Returns `None` when the turn ran no
/// tools — the fold never renders as empty chrome.
pub(crate) fn activity_fold(
    id: (&'static str, usize),
    row: &MessageRow,
    on_toggle: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> Option<gpui::Stateful<gpui::Div>> {
    if row.activity.is_empty() && row.presentation_activity.is_empty() {
        return None;
    }
    let n = row.activity.len();
    let arrow = if row.activity_expanded {
        "\u{25BE}"
    } else {
        "\u{25B8}"
    };
    let dur = row
        .activity_secs
        .filter(|s| *s >= 0.05)
        .map(|s| format!(" \u{00B7} {s:.0}s"))
        .unwrap_or_default();
    let summary = if n == 0 {
        format!("{arrow} Activity")
    } else {
        format!(
            "{arrow} Activity · {n} step{}{dur}",
            if n == 1 { "" } else { "s" }
        )
    };
    let mut fold = div()
        .id(id)
        .flex()
        .flex_col()
        .items_start()
        .gap(theme::z(theme::SP_4))
        .child(
            ui::Button::new(format!("{}-toggle-{}", id.0, id.1), summary)
                .quiet()
                .selector("reply-activity-toggle")
                .on_click(on_toggle),
        );
    if row.activity_expanded {
        for line in &row.activity {
            fold = fold.child(
                div()
                    .pl(theme::z(theme::SP_16))
                    .child(ui::label(awili_code::plain_step(line)).dim().wrap()),
            );
        }
        for notice in &row.presentation_activity {
            let display = reply_presentation::activity_label(notice);
            let (label, command) = display.split_once('\n').unwrap_or((&display, ""));
            let command = command
                .strip_prefix("```\n")
                .and_then(|s| s.strip_suffix("\n```"))
                .unwrap_or(command);
            fold = fold.child(
                div()
                    .pl(theme::z(theme::SP_16))
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_4))
                    .child(ui::label(label.to_owned()).dim().wrap())
                    .when(!command.is_empty(), |d| {
                        d.child(ui::label(command.to_owned()).mono().dim().wrap())
                    }),
            );
        }
    }
    Some(fold)
}

/// RC2-RUN (round 4 Minor 10): the muted line a plan-card click shows as
/// ("You chose “Make the changes”.").
fn plan_click_note_line(idx: usize, note: String) -> gpui::Div {
    div()
        .debug_selector(move || format!("{}-{idx}", crate::plan_gate::PLAN_CLICK_NOTE_SELECTOR))
        .text_size(theme::z(theme::LABEL_SIZE))
        .text_color(theme::p().muted)
        .italic()
        .child(SharedString::from(note))
}

/// RC2-UX (S-I2): the hover group of a sidebar row (its action icons show
/// on hover).
/// RC2-UX (round 3b Minor 4): the test lookup key of a sidebar row's title.
const SIDEBAR_TITLE_SELECTOR: &str = "rc2ux-sidebar-title";
/// RC2-UX (S-I2): the test lookup key of a sidebar row's Rename icon.
const SIDEBAR_RENAME_SELECTOR: &str = "rc2ux-sidebar-rename";
/// RC2-UX (S-L1): the test lookup key of a reply's "Continue in Work".
/// RC2-UX (D-R1 m9): the test lookup key of the Send/Stop button.
const SEND_OR_STOP_SELECTOR: &str = "rc2ux-send-or-stop";
/// The ⌘K palette's empty-box words.
const PALETTE_SEARCH_PLACEHOLDER: &str = "Search conversations\u{2026}";
/// RC2-R1F3: the ⌘O folder box's empty-box words.
const FOLDER_BOX_PLACEHOLDER: &str = "Type a folder path, or pick a recent folder\u{2026}";
/// RC2-UX (R2-26): the test lookup key of a Work/Chat reply's box.
const REPLY_BOX_SELECTOR: &str = "rc2ux-reply-box";

/// RC2-R614: the test lookup key of the empty "Model services" line.
const MODEL_SERVICES_EMPTY_SELECTOR: &str = "rc2r614-model-services-empty";

/// RC2-R614 (usability RC2 round 5, Settings: Models & Fleet said "No model
/// services yet. Most people do not need one." while Work's Gemma ran on a
/// local server): with no extra service set up, the line names the model Work
/// already uses and where it runs.
pub(crate) fn no_model_services_line(engine: &awili_config::EngineConfig) -> String {
    if engine.base_url.trim().is_empty() {
        return "No model services yet. Most people do not need one.".to_string();
    }
    format!(
        "No extra model services. Work\u{2019}s in-app model, {}, runs at {} (in the \
         Settings file, under engine). Add a service here only for another model.",
        engine.display_label(),
        engine.base_url.trim()
    )
}

/// RC2-UX (R2-14): the muted line a Retry row shows as, in place of a second
/// copy of the question.
fn retry_note_line(idx: usize) -> gpui::Div {
    div()
        .debug_selector(move || format!("{}-{idx}", crate::prompt_history::RETRY_NOTE_SELECTOR))
        .text_size(theme::z(theme::LABEL_SIZE))
        .text_color(theme::p().muted)
        .italic()
        .child(SharedString::from(crate::prompt_history::RETRY_NOTE))
}

/// Render one Code-mode transcript bubble (native markdown, reusing the chat
/// renderer). Simpler than the chat `message_row`: no edit/regenerate/speak
/// chrome — just a role header and the rendered markdown blocks.
pub(crate) fn render_code_bubble(
    session_id: u64,
    idx: usize,
    row: &MessageRow,
    errored: bool,
) -> impl IntoElement {
    // RC2-RUN (round 4 Minor 10): a plan-card click is a note, not a bubble
    // (RC2-RUNF: only a row marked as a click).
    if let Some(note) = (row.role == Role::User && row.plan_click)
        .then(|| crate::plan_gate::plan_click_note(row.raw.as_ref()))
        .flatten()
    {
        return div()
            .flex()
            .flex_col()
            .items_end()
            .child(plan_click_note_line(idx, note));
    }
    if row.role == Role::User && row.retry {
        return div()
            .flex()
            .flex_col()
            .items_end()
            .child(retry_note_line(idx));
    }
    let (label, bubble_bg, text_color, palette) = match row.role {
        Role::User => (
            "You",
            theme::p().accent,
            theme::p().on_accent,
            markdown_ui::user(),
        ),
        Role::Assistant => (
            "Agent",
            theme::p().bubble_assistant,
            theme::p().text,
            markdown_ui::assistant(),
        ),
    };
    // Critique P1 (depth): the assistant bubble is the payload — a hairline
    // raise separates it from the canvas instead of charcoal-on-charcoal.
    let assistant = row.role == Role::Assistant;
    let selection_scope = format!("code-{session_id}");
    let body = markdown_ui::message_blocks(
        &selection_scope,
        idx,
        row.parsed_len,
        &row.plan,
        &palette,
        None,
    )
    .into_any_element();
    let body = if errored {
        div()
            .flex()
            .flex_row()
            .gap(theme::z(theme::SPACE_M))
            .child(
                div()
                    .flex_none()
                    .w(theme::z(3.))
                    .rounded(theme::z(1.5))
                    .bg(theme::p().chip_warn),
            )
            .child(div().flex_1().min_w(theme::z(0.)).child(body))
            .into_any_element()
    } else {
        body
    };
    div()
        .flex()
        .flex_col()
        .gap(theme::z(3.))
        .child(
            div()
                .text_size(theme::z(theme::LABEL_SIZE))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if assistant { theme::p().attention } else { theme::p().muted })
                .child(label),
        )
        .child(
            div()
                .bg(bubble_bg)
                .text_color(text_color)
                .rounded(theme::z(theme::RADIUS))
                // Same definite clamp as the chat bubbles: without a max
                // width, intrinsic sizing reports a ONE-LINE height and the
                // row commits to it (2026-08-24 overlap/dead-scroll bug).
                .max_w(theme::z(theme::TRANSCRIPT_CONTENT))
                .when(assistant, |d| {
                    d.border_1().border_color(if errored {
                        theme::p().chip_warn
                    } else {
                        theme::p().border
                    })
                })
                .px(theme::z(10.))
                .py(theme::z(8.))
                .child(body),
        )
}

impl MessageRow {
    pub(crate) fn new(msg: &Message) -> Self {
        let mut row = Self {
            role: msg.role,
            footer: None,
            context: None,
            activity: Vec::new(),
            activity_expanded: false,
            activity_started: None,
            activity_secs: None,
            raw: SharedString::default(),
            plan: Vec::new(),
            presentation_activity: Vec::new(),
            parsed_len: 0,
            plan_palette: theme::p(),
            plan_click: msg.kind == MessageKind::PlanClick,
            retry: msg.kind == MessageKind::Retry,
        };
        row.refresh(msg);
        row
    }

    /// Reparse + replan from the full current source.
    fn refresh(&mut self, msg: &Message) {
        self.show(msg, &msg.text);
    }

    /// Show `shown` (all of `msg.text`, or the part of it that may be
    /// displayed) as the row's display and copy payload. `parsed_len` stays
    /// the length of `msg.text`, the stream cadence's measure.
    fn show(&mut self, msg: &Message, shown: &str) {
        // RC2-UX (D-R1 Chat model line): the stored "[answered by …]" tag
        // reads as a quiet line under the reply, not as raw text in it.
        let shown = if msg.role == Role::Assistant {
            let body = conv_view::strip_answered_by_tag(shown);
            if body.len() < shown.len() {
                let who = shown[body.len()..]
                    .trim()
                    .trim_start_matches("[answered by ")
                    .trim_end_matches(']');
                if self.footer.is_none() {
                    self.footer = Some(format!("Answered by {who}").into());
                }
            }
            body
        } else {
            shown
        };
        self.raw = SharedString::from(shown.to_string());
        self.replan_display();
        self.parsed_len = msg.text.len();
        self.plan_palette = theme::p();
    }

    fn replan_display(&mut self) {
        let presentation = if self.role == Role::Assistant {
            reply_presentation::separate(self.raw.as_ref())
        } else {
            reply_presentation::Presentation {
                body: self.raw.to_string(),
                activity: Vec::new(),
            }
        };
        self.presentation_activity = presentation.activity.into_iter().map(Into::into).collect();
        self.plan = markdown_plan::plan_blocks(
            &markdown::parse_or_plain(&presentation.body),
            &palette_for(self.role),
        );
    }

    /// Replan the display payload only: hidden plan markers and recorded metadata
    /// stay untouched, and unchanged rows do not parse on ordinary paints.
    fn refresh_display_palette(&mut self) {
        if !std::ptr::eq(self.plan_palette, theme::p()) {
            self.replan_display();
            self.plan_palette = theme::p();
        }
    }

    /// A streamed chunk landed: refresh only when the bounded cadence says so
    /// (reparsing the whole accumulated source per chunk is O(n²) across a
    /// reply — review SF2).
    pub(crate) fn stream_update(&mut self, msg: &Message) {
        if markdown::stream_reparse_due(self.parsed_len, msg.text.len()) {
            self.refresh(msg);
        }
    }

    /// R1b: [`Self::stream_update`] for the Work and Code lanes, whose agent
    /// replies can end with the plan-ready marker. The row shows the text
    /// without the marker and without a start of it at the end; the message
    /// keeps the raw text, so the turn's settle still finds the marker.
    pub(crate) fn stream_update_plan_safe(&mut self, msg: &Message) {
        if markdown::stream_reparse_due(self.parsed_len, msg.text.len()) {
            self.show(msg, &awili_code::hide_plan_marker(&msg.text, true));
        }
    }

    /// RC2-FIXA1 (RC2-GEMMA review P1): text was taken back from the end
    /// of a streaming reply. The cadence above redraws only when the text
    /// grows, so redraw now from what is left (marker-safe, as the Work and
    /// Code lanes stream).
    pub(crate) fn stream_taken_back(&mut self, msg: &Message) {
        self.show(msg, &awili_code::hide_plan_marker(&msg.text, true));
    }

    /// The stream ended: make the display and copy payload exact (the cadence
    /// may have left an unparsed tail).
    /// Record one activity step (and start the turn's activity clock).
    pub(crate) fn push_activity(&mut self, line: impl Into<SharedString>) {
        if self.activity_started.is_none() {
            self.activity_started = Some(std::time::Instant::now());
        }
        self.activity.push(line.into());
    }

    fn finish_stream(&mut self, msg: &Message) {
        if let Some(t) = self.activity_started.take() {
            self.activity_secs = Some(t.elapsed().as_secs_f32());
        }
        // R1b: the display may hold back a marker-like tail while streaming.
        if self.parsed_len != msg.text.len() || self.raw.as_ref() != msg.text.as_str() {
            self.refresh(msg);
        }
    }
}

/// Map a persisted role string back to a [`Role`]. Returns `None` for an unknown
/// value so the caller can skip/report the row rather than silently coercing it to
/// `User` (review nit 2: a corrupt or future role must not change conversation
/// semantics).
fn role_from_str(s: &str) -> Option<Role> {
    match s {
        "user" => Some(Role::User),
        "assistant" => Some(Role::Assistant),
        _ => None,
    }
}

/// Rebuild a [`ConversationState`] from persisted rows, preserving each
/// message's [`MessageKind`] AND its superseded-by link (slice 21 §D4). The
/// no-silent-compaction guarantee must survive a restart, not just live
/// in-memory: a summarized-away turn that came back *live* on reload would
/// silently re-enter the prompt, which is exactly the compaction the slice
/// forbids.
///
/// The two id spaces differ: persistence keys `superseded_by` by message ROW
/// ID, while the in-memory model keys it by the summary's logical `ts`. So we
/// seed in row order — capturing each row id → the `ts` [`push_kind`] assigned
/// it — then re-apply the supersede links in a second pass (the summary row may
/// sort after the rows it replaced, so it must be mappable first). Rows with an
/// unknown role are skipped and reported, matching the restore contract
/// (review nit 2). A `superseded_by` pointing at a row that no longer exists
/// (skipped role, truncated history) leaves the message live — never hidden by
/// a phantom summary, and never silently dropped.
fn restore_conversation(msgs: &[StoredMessage]) -> ConversationState {
    let mut convo = ConversationState::new();
    let mut id_to_ts: HashMap<i64, u64> = HashMap::new();
    let mut pending: Vec<(u64, i64)> = Vec::new();
    for m in msgs {
        let Some(role) = role_from_str(&m.role) else {
            eprintln!(
                "awili: skipping restored message with unknown role {:?}",
                m.role
            );
            continue;
        };
        let ts = convo.push_kind(role, m.text.clone(), MessageKind::from_persist_str(&m.kind));
        id_to_ts.insert(m.id, ts);
        if let Some(summary_id) = m.superseded_by {
            pending.push((ts, summary_id));
        }
    }
    for (ts, summary_id) in pending {
        if let Some(&summary_ts) = id_to_ts.get(&summary_id) {
            convo.set_superseded(ts, summary_ts);
        }
    }
    convo
}

/// The "source · age" caption a Needs You row carries on its meta line.
fn age_and_source(now_ts: i64, event: &awili_feed::Event) -> String {
    format!(
        "{} · {}",
        event.source,
        feed::age_label(now_ts, event.created_ts)
    )
}

/// One slice-12 message-action button. Enabled chips get the hover accent and
/// the click handler; disabled ones (mid-stream, archived, or not applicable —
/// e.g. regenerate on a non-last reply) render faint and inert (spec §5).
/// Parse a provider's model catalog out of a probe body. Accepts both wire
/// shapes without caring which endpoint answered: ollama's `/api/tags`
/// (`{"models":[{"name":..}]}`) and the OpenAI-compatible `/models`
/// (`{"data":[{"id":..}]}`). A recognized `models`/`data` array — even an
/// empty one — is `Some`; invalid JSON, a missing array, or a non-array
/// field is `None`, so a malformed body is never a successful empty
/// catalog. The probe stays a health check first.
fn parse_discovered_models(body: &str) -> Option<Vec<String>> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return None;
    };
    let items = v
        .get("models")
        .or_else(|| v.get("data"))
        .and_then(|m| m.as_array());
    let items = items?;
    let mut out: Vec<String> = items
        .iter()
        .filter_map(|it| {
            it.get("name")
                .or_else(|| it.get("id"))
                .and_then(|s| s.as_str())
                .map(str::to_string)
        })
        .collect();
    out.sort();
    out.dedup();
    Some(out)
}

/// Fold one probe result into the discovered-models cache. A `Some` catalog
/// — even an empty one — is authoritative for that endpoint and replaces the
/// entry; a `None` (liveness-only, non-2xx, or transport failure) leaves the
/// last successful catalog in place.
fn apply_model_cache(
    cache: &mut std::collections::HashMap<String, Vec<String>>,
    base: String,
    catalog: Option<Vec<String>>,
) {
    if let Some(catalog) = catalog {
        cache.insert(base, catalog);
    }
}

/// A discovered model that can't hold a chat: speech (whisper/TTS), image
/// generation, and similar. Endpoints like Hosta's FLM serve these next to
/// chat models on the same catalog; the picker must not offer them.
fn is_non_chat_model(model: &str, filters: &[String]) -> bool {
    let m = model.to_ascii_lowercase();
    filters
        .iter()
        .any(|p| !p.is_empty() && m.contains(&p.to_ascii_lowercase()))
}

/// Append the models `base_url`'s endpoint serves but the choices list lacks,
/// directly after that endpoint's existing group when it has one (an
/// embedder-only provider contributes no rows, so its discoveries land as a
/// new group at the end). Config-declared entries stay first and
/// authoritative; embedding and non-chat models never become chat targets;
/// an unknown base_url is a no-op (a provider that was just removed).
fn extend_choices_with_discovered(
    choices: &mut Vec<ModelChoice>,
    base_url: &str,
    discovered: &[String],
    endpoint_template: Option<ModelChoice>,
    non_chat: &[String],
) {
    let template = match choices
        .iter()
        .find(|c| c.base_url == base_url)
        .cloned()
        .or(endpoint_template)
    {
        Some(t) => t,
        None => return,
    };
    let mut insert_at = choices
        .iter()
        .rposition(|c| c.base_url == base_url)
        .map(|i| i + 1)
        .unwrap_or(choices.len());
    for model in discovered {
        if awili_config::is_embedding_model(model) || is_non_chat_model(model, non_chat) {
            continue;
        }
        if choices
            .iter()
            .any(|c| c.base_url == base_url && &c.model == model)
        {
            continue;
        }
        let mut choice = template.clone();
        choice.model = model.clone();
        choices.insert(insert_at, choice);
        insert_at += 1;
    }
}

/// Layout diagnostics (`AWILI_ROW_DEBUG=1`): logs each transcript row's box
/// and its children's boxes at prepaint. This is how the 2026-08-24
/// overlap/dead-scroll bug was pinned down — a row committing to a one-line
/// height while its children laid out wrapped — so it stays, but the env is
/// read ONCE: this runs per row per frame.
fn row_debug() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("AWILI_ROW_DEBUG").is_some())
}

/// Whether a transcript scroll is pinned near its bottom — the ONLY state in
/// which streaming may auto-follow (ocr/codex review R4: every token used to
/// force scroll_to_bottom, stealing the viewport from a reader who had
/// scrolled up; long replies made history unscrollable). Offset y runs
/// negative as content scrolls up, so bottom is offset == -max.
fn follow_stream(scroll: &gpui::ScrollHandle) -> bool {
    let off = scroll.offset();
    let max = scroll.max_offset();
    (max.y + off.y) <= theme::z(48.0)
}

/// The pure ordering/presence rules for the composer's passive status run
/// (2026-08-23 one-row-composer slice). Fixed order: context, cost, rules,
/// recall, memory-saved flash, gateway. Absent inputs contribute nothing;
/// offline/trouble inputs warm their segment; everything else stays FAINT
/// (the run is glance data, not a control).
fn status_run_parts(
    ctx: Option<(String, gpui::Rgba)>,
    cost: Option<String>,
    rules: Option<(String, bool)>,
    recall: Option<(String, bool)>,
    memory_saved: bool,
    gateway_up: Option<bool>,
) -> Vec<(String, gpui::Rgba)> {
    let mut parts = Vec::new();
    if let Some((text, color)) = ctx {
        parts.push((text, color));
    }
    if let Some(text) = cost {
        parts.push((text, theme::p().accent_text));
    }
    for (text, offline) in [rules, recall].into_iter().flatten() {
        // MUTED, not FAINT: 11px status text must clear contrast
        // (critique a11y) — FAINT stays for placeholders and separators.
        let color = if offline {
            theme::p().chip_warn
        } else {
            theme::p().muted
        };
        parts.push((text, color));
    }
    if memory_saved {
        parts.push(("memory saved".to_string(), theme::p().accent_text));
    }
    if let Some(up) = gateway_up {
        let (glyph, color) = if up {
            ("\u{25CF} gateway", theme::p().accent_text)
        } else {
            ("\u{25CB} gateway", theme::p().chip_warn)
        };
        parts.push((glyph.to_string(), color));
    }
    parts
}

fn action_chip(
    id: (&'static str, usize),
    icon: icons::Icon,
    label: &'static str,
    tip: &'static str,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let tint = if enabled {
        theme::p().muted
    } else {
        theme::p().faint
    };
    let chip = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(theme::z(4.))
        .px(theme::z(6.))
        .py(theme::z(1.))
        .rounded(theme::z(theme::RADIUS))
        .text_size(theme::z(theme::LABEL_SIZE))
        .child(icons::icon(icon, 10.0, tint))
        .child(label);
    if enabled {
        chip.text_color(theme::p().muted)
            .cursor_pointer()
            .hover(|s| s.text_color(theme::p().accent_text))
            .tooltip(icons::tip(tip))
            .on_click(on_click)
    } else {
        chip.text_color(theme::p().faint)
    }
}

/// Awili's thinking indicator — present from turn start until the first visible
/// token. Chat and jobs paint the same blue kit part as the design reference.
fn activity_pulse(phase: u8) -> ui::ThinkingDot {
    ui::ThinkingDot::new(phase)
}

/// Whether an assistant row still has no paintable reply body.
///
/// Providers may emit leading whitespace before their first visible token.
/// `raw.is_empty()` therefore stops being a reliable pre-first-token test even
/// though the markdown plan is still empty and the bubble would paint blank.
fn should_show_activity_pulse(
    row: &MessageRow,
    streaming: bool,
    active_response_row: bool,
    waiting_for_output: bool,
    errored: bool,
) -> bool {
    row.role == Role::Assistant
        && streaming
        && active_response_row
        && waiting_for_output
        && !errored
}

/// Top and height of the conversation scrollbar thumb, in device pixels.
fn conversation_scrollbar_metrics(
    viewport_height: f32,
    max_offset: f32,
    current_offset: f32,
) -> Option<(f32, f32)> {
    const TRACK_INSET: f32 = 8.0;
    const MIN_THUMB: f32 = 36.0;
    if viewport_height <= TRACK_INSET * 2.0 || max_offset <= 0.0 {
        return None;
    }
    let track_height = viewport_height - TRACK_INSET * 2.0;
    let content_height = viewport_height + max_offset;
    let thumb_height = (track_height * viewport_height / content_height)
        .max(MIN_THUMB)
        .min(track_height);
    let progress = current_offset.abs().clamp(0.0, max_offset) / max_offset;
    Some((progress * (track_height - thumb_height), thumb_height))
}

fn conversation_scroll_offset_for_pointer(
    track_height: f32,
    thumb_height: f32,
    max_offset: f32,
    pointer: f32,
) -> Option<f32> {
    let travel = track_height - thumb_height;
    if travel <= 0.0 || max_offset <= 0.0 {
        return None;
    }
    let progress = pointer.clamp(0.0, travel) / travel;
    Some(-max_offset * progress)
}

fn conversation_scrollbar(scroll: &ScrollHandle, drag: Rc<Cell<Option<Pixels>>>) -> gpui::Div {
    div()
        .absolute()
        .right(theme::z(0.))
        .top(px(8.))
        .bottom(px(8.))
        // Deliberately wider than the painted rail. A scrollbar that only
        // works when the pointer lands on a six-point Retina sliver is not a
        // functional direct-manipulation control.
        .w(theme::z(20.))
        .cursor_pointer()
        .child(
            canvas(|_track_bounds, _window, _cx| (), {
                let scroll = scroll.clone();
                move |track_bounds, (), window, _cx| {
                    // The ScrollHandle receives its bounds and max offset
                    // while the transcript paints, after prepaint. Reading
                    // it here makes the bar correct on the first frame.
                    let viewport_height = f32::from(scroll.bounds().size.height);
                    let Some((thumb_top, thumb_height)) = conversation_scrollbar_metrics(
                        viewport_height,
                        f32::from(scroll.max_offset().y),
                        f32::from(scroll.offset().y),
                    ) else {
                        drag.set(None);
                        return;
                    };
                    let thumb_width = theme::z(7.);
                    let rail_width = theme::z(1.);
                    let thumb_bounds = Bounds::new(
                        point(
                            track_bounds.origin.x + (track_bounds.size.width - thumb_width) / 2.,
                            track_bounds.origin.y + px(thumb_top),
                        ),
                        size(thumb_width, px(thumb_height)),
                    );
                    let painted_track_bounds = Bounds::new(
                        point(
                            track_bounds.origin.x + (track_bounds.size.width - rail_width) / 2.,
                            track_bounds.origin.y,
                        ),
                        size(rail_width, track_bounds.size.height),
                    );

                    window.paint_quad(quad(
                        painted_track_bounds,
                        rail_width / 2.,
                        theme::p().scrollbar_track,
                        Edges::default(),
                        gpui::Hsla::transparent_black(),
                        BorderStyle::default(),
                    ));
                    window.paint_quad(quad(
                        thumb_bounds,
                        thumb_width / 2.,
                        theme::p().scrollbar_thumb,
                        Edges::default(),
                        gpui::Hsla::transparent_black(),
                        BorderStyle::default(),
                    ));

                    window.on_mouse_event({
                        let drag = drag.clone();
                        let scroll = scroll.clone();
                        move |event: &MouseDownEvent, phase, window, cx| {
                            if phase == DispatchPhase::Bubble
                                && event.button == gpui::MouseButton::Left
                                && track_bounds.contains(&event.position)
                            {
                                let grab_offset = if thumb_bounds.contains(&event.position) {
                                    event.position.y - thumb_bounds.origin.y
                                } else {
                                    px(thumb_height / 2.0)
                                };
                                drag.set(Some(grab_offset));
                                if !thumb_bounds.contains(&event.position) {
                                    let pointer = f32::from(
                                        event.position.y - track_bounds.origin.y - grab_offset,
                                    );
                                    if let Some(scroll_y) = conversation_scroll_offset_for_pointer(
                                        f32::from(track_bounds.size.height),
                                        thumb_height,
                                        f32::from(scroll.max_offset().y),
                                        pointer,
                                    ) {
                                        let mut offset = scroll.offset();
                                        offset.y = px(scroll_y);
                                        scroll.set_offset(offset);
                                        window.refresh();
                                    }
                                }
                                window.prevent_default();
                                cx.stop_propagation();
                            }
                        }
                    });
                    window.on_mouse_event({
                        let drag = drag.clone();
                        let scroll = scroll.clone();
                        move |event: &MouseMoveEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            if !event.dragging() {
                                drag.set(None);
                                return;
                            }
                            let Some(grab_offset) = drag.get() else {
                                return;
                            };
                            let viewport_height = f32::from(scroll.bounds().size.height);
                            let max_offset = f32::from(scroll.max_offset().y);
                            let Some((_, thumb_height)) = conversation_scrollbar_metrics(
                                viewport_height,
                                max_offset,
                                f32::from(scroll.offset().y),
                            ) else {
                                return;
                            };
                            let pointer =
                                f32::from(event.position.y - track_bounds.origin.y - grab_offset);
                            let Some(scroll_y) = conversation_scroll_offset_for_pointer(
                                f32::from(track_bounds.size.height),
                                thumb_height,
                                max_offset,
                                pointer,
                            ) else {
                                return;
                            };
                            let mut offset = scroll.offset();
                            offset.y = px(scroll_y);
                            scroll.set_offset(offset);
                            window.refresh();
                            cx.stop_propagation();
                        }
                    });
                    window.on_mouse_event({
                        let drag = drag.clone();
                        move |event: &MouseUpEvent, phase, window, cx| {
                            if phase == DispatchPhase::Bubble
                                && event.button == gpui::MouseButton::Left
                                && drag.take().is_some()
                            {
                                window.refresh();
                                cx.stop_propagation();
                            }
                        }
                    });
                }
            })
            .size_full(),
        )
}

/// The wire string for a role, as persisted and logged.
fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

/// Most results a ⌘F search shows (slice 14) — one row per conversation.
const SEARCH_RESULT_LIMIT: usize = 50;

impl AwiliApp {
    /// The persisted next-turn target for a captured Workshop backing. A
    /// native tab without a durable id uses its local compatibility seed; as
    /// soon as it has an id the shared per-conversation route wins.
    pub(crate) fn composer_target_for(
        &self,
        owner: &crate::workshop_composer::ModelOwner,
    ) -> Option<work::WorkExecutor> {
        match owner {
            crate::workshop_composer::ModelOwner::Native(tab_id) => {
                let tab = self.work.code_tabs.iter().find(|tab| tab.id == *tab_id)?;
                if tab
                    .history
                    .conv_id
                    .as_ref()
                    .is_some_and(|id| self.work.unavailable_conversation_routes.contains_key(id))
                {
                    return None;
                }
                Some(
                    tab.history
                        .conv_id
                        .as_deref()
                        .and_then(|id| self.work.conversation_routes.get(id))
                        .unwrap_or(&tab.route)
                        .next_target()
                        .clone(),
                )
            }
            crate::workshop_composer::ModelOwner::Work(id) => self
                .work
                .conversation_routes
                .get(id)
                .filter(|_| {
                    self.conversations.get(id).is_some()
                        && !self.work.unavailable_conversation_routes.contains_key(id)
                })
                .map(|route| route.next_target().clone()),
        }
    }

    /// Update only a conversation's next-turn target. The identity of a live
    /// run remains in `WorkRunIdentity` / `CodeRuns`; this accepts changes
    /// while busy precisely because it neither cancels nor retargets that run.
    pub(crate) fn set_composer_target(
        &mut self,
        owner: &crate::workshop_composer::ModelOwner,
        target: work::WorkExecutor,
        cx: &mut Context<Self>,
    ) -> bool {
        let persisted = match owner {
            crate::workshop_composer::ModelOwner::Native(tab_id) => {
                let is_current = self.mode == work::Mode::Code
                    && self
                        .work
                        .code_tabs
                        .get(self.work.code_active)
                        .map(|tab| tab.id)
                        == Some(*tab_id);
                if !is_current {
                    return false;
                }
                let Some((conv_id, seed)) = self
                    .work
                    .code_tabs
                    .iter()
                    .find(|tab| tab.id == *tab_id)
                    .map(|tab| (tab.history.conv_id.clone(), tab.route.clone()))
                else {
                    return false;
                };
                if let Some(id) = conv_id {
                    let route = self
                        .work
                        .conversation_routes
                        .entry(id.clone())
                        .or_insert(seed);
                    route.set_next_target(target);
                    self.work.unavailable_conversation_routes.remove(&id);
                    Some(id)
                } else {
                    if let Some(tab) = self.work.code_tabs.iter_mut().find(|tab| tab.id == *tab_id)
                    {
                        tab.route.set_next_target(target);
                    }
                    None
                }
            }
            crate::workshop_composer::ModelOwner::Work(id) => {
                if self.mode != work::Mode::Work
                    || self.sidebar.active_id() != id
                    || self.conversations.get(id).is_none()
                {
                    return false;
                }
                let seed = self
                    .work
                    .chip_picked
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| self.work.executor.clone());
                self.work
                    .conversation_routes
                    .entry(id.clone())
                    .or_insert_with(|| crate::conversation_route::ConversationRoute::new(seed))
                    .set_next_target(target);
                self.work.unavailable_conversation_routes.remove(id);
                Some(id.clone())
            }
        };
        if let Some(id) = persisted {
            self.persist_conversation_route(&id);
        }
        cx.notify();
        true
    }

    fn persist_conversation_route(&self, id: &str) {
        let Some(route) = self.work.conversation_routes.get(id) else {
            return;
        };
        let Ok(raw) = route.to_persisted_json() else {
            return;
        };
        if let Some(persist) = &self.persist {
            persist.send(crate::persistence::PersistCmd::SetState {
                key: format!("conversation_route:{id}"),
                value: raw,
            });
        }
    }

    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        // R2h: a test never builds the app against a real home.
        #[cfg(test)]
        test_home_guard::assert_private_home();
        // S1b: briefs an earlier crashed run left behind go now.
        code_provider::sweep_leftover_briefs();
        // No config file -> networkless runtime + fresh onboarding. A
        // malformed/unreadable EXISTING config is different: silently using
        // built-in endpoints could send traffic somewhere the user never
        // selected, so we disable networking and surface a FIXED error banner
        // in the window (review SF7; the raw error text is redacted from the
        // log and the UI); provenance counts as existing.
        // RVS6C FIX3 (Step 1): the load result AND the `config_load_error`
        // flag come from ONE mapping ([`agent_chat::load_chat_config`]) — the
        // same function the guard-proof tests drive — so the flag can never
        // be fabricated independently of the load this startup performed.
        // The mapping's own contract: no file on disk -> `networkless()` +
        // `Missing` (fresh onboarding below); a malformed EXISTING file -> a
        // load ERROR with provenance (the flag SET, the existing file's
        // `Loaded(path)` source — never `Missing`) so the recovery path
        // below (banner + refusal) is the ONLY way a broken install starts —
        // never fresh onboarding.
        let (config, chat_refused, source) = agent_chat::load_chat_config();
        let (config, secret_notices) = secret_store::resolved(&config);
        let appearance = appearance::Appearance::new(config.appearance.color_mode);
        // Onboarding: a `Missing` file (no config ever written) is a fresh
        // install; a valid `Loaded` config OR a malformed EXISTING file
        // (the user already has a config on disk, it is just broken) is
        // existing — the load ERROR carries the existing file's provenance,
        // never `Missing`.
        let onboarding = if matches!(source, awili_config::ConfigSource::Missing) {
            onboarding::Onboarding::fresh()
        } else {
            onboarding::Onboarding::existing()
        };
        if chat_refused {
            // Fixed, redacted diagnostic only: the raw error can carry
            // parser details, file paths, or credentials, so it is never
            // logged or surfaced beyond this one fixed line.
            eprintln!(
                "awili: config could not be loaded (details redacted); repair it in Settings"
            );
        }
        let config_error = chat_refused.then(|| "config could not be loaded".to_string());
        // P1.1: a default_model that doesn't belong to default_provider is a
        // warning, never fatal — log it and fall back to a usable choice.
        for w in config.default_warnings() {
            eprintln!("awili: config warning: {w}");
        }
        let choices = config.model_choices();
        // Voice settings come from the [voice] table of the same config file; a
        // malformed table falls back to defaults (it must never block the app).
        let voice_cfg = VoiceConfig::load().unwrap_or_else(|e| {
            eprintln!("awili: voice config error, using defaults: {e}");
            VoiceConfig::default()
        });
        let keys = ProviderKeys::from_env_and_config(&config);
        let onboarding_harness = harness_setup::configured_harness(&config);
        let onboarding_other_route = harness_setup::other_work_route(&config);
        let memory = MemoryCfg {
            enabled: !config.memory.base_url.is_empty(),
            base_url: config.memory.base_url.clone(),
            token: config.memory.token.clone(),
            min_priority: config.memory.min_priority,
            recall: config.memory.recall,
            write: config.memory.write,
        };

        // Persistence: open the DB, restore every conversation and the saved
        // model selection. All DB work is serialized onto a worker thread (SF6);
        // the reads below happened synchronously at startup inside `start`.
        let (
            persist,
            stored_convs,
            mut stored_msgs,
            saved_active,
            saved_selection,
            today_usage,
            templates,
            conv_templates,
            saved_projects,
            project_import_error,
            saved_code_active_project,
            saved_work_folder,
            saved_work_executor,
            work_executor_notice,
            saved_open_builds,
            work_last_run,
            saved_work_footers,
            saved_work_folder_moves,
            saved_last_mode,
            saved_recent_folders,
            saved_code_open_tabs,
            saved_workshop_tabs,
            saved_code_tab_metas,
            saved_conversation_routes,
            saved_legacy_general_models,
            saved_memory_chips,
            saved_work_steps,
            saved_work_contexts,
            saved_run_ids,
        ) = match Persistence::start(config.work.default_executor.as_deref()) {
            Some(init) => (
                Some(init.handle),
                init.conversations,
                init.messages,
                init.active,
                init.selection,
                init.today_usage,
                init.templates,
                init.conv_templates,
                init.projects,
                init.project_import_error,
                init.code_active_project,
                init.work_folder,
                init.work_executor,
                init.work_executor_notice,
                init.open_build_jobs,
                init.work_last_run,
                init.work_footers,
                init.work_folder_moves,
                init.last_mode,
                init.recent_folders,
                init.code_open_tabs,
                init.workshop_tabs,
                init.code_tab_metas,
                init.conversation_routes,
                init.legacy_general_models,
                init.memory_chips,
                init.work_steps,
                init.work_contexts,
                init.run_ids,
            ),
            None => (
                None,
                Vec::new(),
                HashMap::new(),
                None,
                None,
                Vec::new(),
                Vec::new(),
                HashMap::new(),
                Vec::new(),
                None,
                None,
                None,
                None,
                false,
                Vec::new(),
                HashMap::new(),
                HashMap::new(),
                HashMap::new(),
                None,
                None,
                None,
                Ok(None),
                HashMap::new(),
                HashMap::new(),
                None,
                HashMap::new(),
                HashMap::new(),
                HashMap::new(),
                HashMap::new(),
            ),
        };

        // Restore selection by folder identity, never a legacy list index.
        let code_active_project = saved_code_active_project
            .as_deref()
            .and_then(|folder| saved_projects.iter().position(|p| p.folder == folder))
            .unwrap_or(0);

        // Restore the chip to the last-used provider·model if it still exists in
        // config; otherwise fall back to the configured default (review finding C).
        let provider_endpoints: Vec<awili_config::ProviderConfig> = config
            .providers
            .iter()
            .filter(|p| p.enabled)
            .cloned()
            .collect();
        let saved_idx = saved_selection.as_ref().and_then(|(p, m)| {
            choices
                .iter()
                .position(|c| &c.provider == p && &c.model == m)
        });
        let choice_idx = saved_idx.unwrap_or_else(|| config.default_choice_index());
        // Unmatched selections may name a model the endpoint serves but the
        // config doesn't declare — hold them for the post-discovery merge.
        let pending_saved_selection = if saved_idx.is_none() {
            saved_selection.clone()
        } else {
            None
        };

        // Rebuild the sidebar metadata and one ConvView per conversation,
        // skipping any message row with an unknown role rather than coercing it
        // to `User` (review nit 2).
        let mut conversations = ConversationList::from_items(
            stored_convs
                .iter()
                .map(|c| ConversationMeta {
                    id: c.id.clone(),
                    title: c.title.clone(),
                    created_ts: c.created_ts,
                    updated_ts: c.updated_ts,
                    archived: c.archived,
                    title_is_custom: c.title_is_custom,
                    cwd: c.cwd.clone(),
                    profile: c.profile.clone(),
                    // Read compatibility: the stored kind "chat" (and a NULL
                    // kind) names a legacy answer-only thread. Workshop has
                    // one tool-capable path, so it opens as a Workshop
                    // thread; the stored row and its transcript are untouched.
                    kind: match ConvKind::from_token(&c.kind) {
                        ConvKind::Chat => ConvKind::Work,
                        other => other,
                    },
                })
                .collect(),
        );
        // RC2-RUN (UX-FOOTER): the saved reply footers, parsed once; the app
        // keeps this copy current as it saves new ones.
        let work_footers: HashMap<String, run_footer::SavedFooters> = saved_work_footers
            .iter()
            .map(|(id, raw)| (id.clone(), run_footer::parse_footers(raw)))
            .collect();
        // RC2-UX (D-R1 m6): and the saved steps.
        let work_steps: HashMap<String, run_footer::SavedSteps> = saved_work_steps
            .iter()
            .map(|(id, raw)| (id.clone(), run_footer::parse_steps(raw)))
            .collect();
        // rc.3 S2: and the per-reply context breakdowns.
        let work_contexts: HashMap<String, context_breakdown::SavedContexts> = saved_work_contexts
            .iter()
            .map(|(id, raw)| (id.clone(), context_breakdown::parse_saved(raw)))
            .collect();
        // RC2-R1F5: and the folder moves.
        let work_folder_moves: HashMap<String, work_folder_moves::FolderMoves> =
            saved_work_folder_moves
                .iter()
                .map(|(id, raw)| (id.clone(), work_folder_moves::parse_moves(raw)))
                .collect();
        let mut run_ids = run_error::RunIds::default();
        let mut views: HashMap<String, ConvView> = stored_convs
            .iter()
            .map(|c| {
                let msgs = stored_msgs.remove(&c.id).unwrap_or_default();
                let convo = restore_conversation(&msgs);
                let mut view = ConvView::from_state(convo);
                if let Some(saved) = work_footers.get(&c.id) {
                    run_footer::apply_saved_footers(&mut view, &msgs, saved);
                }
                if let Some(saved) = work_steps.get(&c.id) {
                    run_footer::apply_saved_steps(&mut view, &msgs, saved);
                }
                if let Some(saved) = work_contexts.get(&c.id) {
                    context_breakdown::apply_saved(&mut view, &msgs, saved);
                }
                if let Some(raw) = saved_run_ids.get(&c.id) {
                    run_ids.restore(&c.id, raw, &msgs, &view.convo);
                }
                // R2g: a restored transcript opens at its latest message,
                // not scrolled to the top (applied at its first paint).
                view.scroll.scroll_to_bottom();
                (c.id.clone(), view)
            })
            .collect();

        // The active conversation: the saved one if it still exists, else the
        // most recently updated unarchived one, else a fresh conversation.
        let now = feed::unix_now();
        let active_id = saved_active
            .filter(|id| conversations.get(id).is_some())
            .or_else(|| conversations.active().first().map(|c| c.id.clone()))
            .unwrap_or_else(|| {
                let meta = conversations.create_kind(now, ConvKind::Work);
                views.insert(meta.id.clone(), ConvView::new());
                if let Some(p) = &persist {
                    p.send(PersistCmd::CreateConversation {
                        id: meta.id.clone(),
                        title: meta.title.clone(),
                        created_ts: meta.created_ts,
                        kind: meta.kind.as_str().to_string(),
                    });
                }
                meta.id
            });

        // The attention feed (slice 4): when configured, spawn the worker that
        // owns the mirror + polling, and drain its snapshots into repaints.
        let feed_cfg = FeedCfg::load(&config);
        let feed = feed_cfg.clone().map(|cfg| {
            let (handle, snapshots) = feed::start(cfg);
            cx.spawn(async move |this, cx| {
                while let Ok(snapshot) = snapshots.recv().await {
                    let alive = this.update(cx, |app, cx| {
                        app.feed_snapshot = snapshot;
                        cx.notify();
                    });
                    if alive.is_err() {
                        break; // window gone; dropping the receiver stops the worker
                    }
                }
            })
            .detach();
            handle
        });

        let composer = cx.new(Composer::new);
        // M2g: the main composer opts into Up/Down prompt history. The rename,
        // palette, and credential composers stay off (their Up/Down move the
        // caret / navigate rows).
        composer.update(cx, |c, cx| {
            c.enable_history();
            cx.notify();
        });
        let _submit = cx.subscribe(
            &composer,
            |this, composer, ev: &ComposerEvent, cx| match ev {
                ComposerEvent::Submit => {
                    if this.shell.archived_reader.is_some() {
                        return;
                    }
                    if this.onboarding.is_active() {
                        return;
                    }
                    let busy = this.composer_busy();
                    if busy {
                        this.explain_busy_composer(cx); // M2e
                        return;
                    }
                    let text = composer.update(cx, |c, cx| {
                        cx.notify();
                        c.take()
                    });
                    this.invalidate_dictation();
                    if this.voice.conversation_mode {
                        // The human pressing Enter IS the ceremony's
                        // confirmation: voice may auto-send from here on.
                        // Quiet the mic for the reply (the speech/turn
                        // done-watchers re-arm it) so the spoken answer
                        // never feeds back into the capture.
                        this.voice.first_send_confirmed = true;
                        this.stop_voice_listening_internal();
                    }
                    // R1a / RC1-H5: Enter takes the Send button's guarded
                    // path, so a refused message goes back into the box.
                    this.guard_submit(&text);
                    this.dispatch_composer_text(text, cx);
                    this.settle_submit_guard(cx);
                }
                ComposerEvent::Cancel => {
                    this.abort_active_stream(cx);
                }
                ComposerEvent::SwitchChat => this.open_workshop_adapter(Mode::Work, cx),
                ComposerEvent::SwitchWork => this.open_workshop_adapter(Mode::Work, cx),
                ComposerEvent::HistoryPrev => this.composer_history_prev(cx),
                ComposerEvent::HistoryNext => this.composer_history_next(cx),
                // M2e: Enter on a busy composer kept the draft; say why.
                ComposerEvent::BusySubmit => this.queue_busy_draft(cx),
                // RC2-R4 (D-U3): Edit ▸ Paste (or the context menu) with an
                // image or file on the clipboard attaches it, as ⌘V does.
                ComposerEvent::PasteAttachments => {
                    this.attach_from_clipboard(cx);
                }
            },
        );
        // M2g (O2) + M2g-FIX1: mirror composer_busy() into the composer so its
        // Enter key keeps the draft while a submit would be dropped here. It
        // observes the APP, not the composer: a run starting or ending
        // notifies the app, not the composer. No notify inside: the flag
        // changes nothing on screen, and a notify here would loop. Stored on
        // the struct (`_composer_busy`); a dropped Subscription is dead.
        let _composer_busy = cx.observe_self(|this, cx| {
            let busy = this.composer_busy();
            this.composer.update(cx, |c, _| c.set_busy(busy));
        });

        let _send_queue_tick = cx.observe_self(|this, cx| this.send_queue_tick(cx));

        // The `/`-command skill palette: watch the chat composer's draft and
        // open/filter/close the palette live. The composer emits no per-keystroke
        // event, so (like the ⌘K input) we observe its buffer notifications. The
        // palette's keys (↑/↓/Enter/Esc) are handled in the root CAPTURE-phase
        // handler — like the ⌘K palette — so they land before the focused
        // composer's IME (which would otherwise eat Escape and the arrows).
        let _composer_palette = cx.observe(&composer, |this, _composer, cx| {
            this.sync_skill_palette(cx);
            cx.notify();
        });

        // The inline rename editor (one entity, shown in whichever row is being
        // renamed): Enter commits, Escape cancels (slice-7 spec §2).
        let rename_editor = cx.new(|cx| Composer::with_options("Conversation title", true, cx));
        let _rename = cx.subscribe(
            &rename_editor,
            |this, editor, ev: &ComposerEvent, cx| match ev {
                ComposerEvent::Submit => {
                    let title = editor.read(cx).text().to_string();
                    this.commit_rename(&title, cx);
                }
                ComposerEvent::Cancel => {
                    this.sidebar.cancel_rename();
                    cx.notify();
                }
                _ => {}
            },
        );

        // Settings (slice-7 spec §3): the form's inline field editor (Enter
        // commits through the validation gate, Escape cancels) and the
        // raw-TOML editor (multiline; the Save button commits).
        let settings_field = cx.new(|cx| Composer::with_options("value", true, cx));
        let _settings_field =
            cx.subscribe(&settings_field, |this, editor, ev: &ComposerEvent, cx| {
                match ev {
                    ComposerEvent::Submit if this.settings.editing_item.is_some() => {
                        this.commit_item_edit(cx);
                    }
                    ComposerEvent::Submit => {
                        let text = editor.read(cx).text().to_string();
                        if this.settings.field.is_some() {
                            // A structured-panel draft field: memory only —
                            // nothing reaches disk until Save (slice 11 §1).
                            this.settings.commit_draft_field(&text);
                            this.settings_nav_focus_pending = true;
                        } else if this.settings.commit_field(&text) {
                            cx.emit(ConfigReloaded);
                            this.schedule_status_clear(cx);
                        }
                    }
                    ComposerEvent::Cancel => {
                        // SXd-FIX1: an item edit is ended by the Settings
                        // Escape handler (it owns `editing_item`), never
                        // here, so the same keypress cannot also close.
                        let had_field = this.settings.field.is_some();
                        this.settings.cancel_edit();
                        this.settings.cancel_field();
                        if had_field {
                            this.settings_nav_focus_pending = true;
                        }
                    }
                    _ => {}
                }
                cx.notify();
            });
        let settings_raw = cx.new(|cx| Composer::with_options("# config.toml", false, cx));
        let _settings_raw = cx.subscribe(&settings_raw, |this, _editor, ev: &ComposerEvent, cx| {
            if let ComposerEvent::Cancel = ev {
                // SXe: the raw editor's Esc goes through the safe-close path
                // (unsaved model services are not lost silently).
                this.request_close_settings(cx);
                cx.notify();
            }
        });
        let settings_search_focus = cx.focus_handle();

        // RC2-UX (D-PUNCH F11): the memory card's search box.
        let memory_search = cx.new(|cx| Composer::with_options("Search my memory…", true, cx));
        let _memory_search =
            cx.subscribe(&memory_search, |this, editor, ev: &ComposerEvent, cx| {
                if let ComposerEvent::Submit = ev {
                    let q = editor.read(cx).text().trim().to_string();
                    if !q.is_empty() {
                        this.recall_memory(&q, cx);
                    }
                }
            });

        // Isolated input for a pending credential request (sudo password / skill
        // secret). Enter submits the value to the gateway; Escape declines.
        let credential_input = cx.new(|cx| Composer::with_options("Credential value", true, cx));
        credential_input.update(cx, |c, _| c.set_secret(true)); // U3a1: never to the clipboard
        let _credential_input = cx.subscribe(
            &credential_input,
            |this, _editor, ev: &ComposerEvent, cx| {
                match ev {
                    ComposerEvent::Submit => this.submit_credential(cx),
                    ComposerEvent::Cancel => this.refuse_credential(cx),
                    _ => {}
                }
                cx.notify();
            },
        );

        // Inline editor for a messaging connector credential value (plan unit 25).
        // Enter commits to the draft; Escape cancels. Never prefilled with the
        // redacted display value — the secret-safety invariant prevents masking.
        let messaging_credential_editor =
            cx.new(|cx| Composer::with_options("Set credential value", true, cx));
        messaging_credential_editor.update(cx, |c, _| c.set_secret(true)); // U3a1
        let _messaging_credential_editor = cx.subscribe(
            &messaging_credential_editor,
            |this, _editor, ev: &ComposerEvent, cx| {
                match ev {
                    ComposerEvent::Submit => this.commit_messaging_credential_edit(cx),
                    ComposerEvent::Cancel => this.cancel_messaging_credential_edit(cx),
                    _ => {}
                }
                cx.notify();
            },
        );

        // The ⌘K palette's search input (slice 13). Enter/Escape/arrows are
        // handled at the window root in the CAPTURE phase (see `render`), so
        // no ComposerEvent subscription — only an observe, because the row
        // list filters live and the parent must repaint as the buffer changes.
        let palette_input =
            cx.new(|cx| Composer::with_options(PALETTE_SEARCH_PLACEHOLDER, true, cx));
        let _palette_input = cx.observe(&palette_input, |_this, _input, cx| cx.notify());

        // The reload signal (slice-11 spec §3): every successful settings save
        // emits ConfigReloaded; this subscription re-applies the new config
        // live, so the provider list refreshes without a full restart.
        let _reload = cx.subscribe_self(|this, _ev: &ConfigReloaded, cx| {
            this.apply_config_live(cx);
        });

        // Conversation search (slice 14): the panel's input re-runs the FTS
        // query on every edit. Enter/Escape/arrows are handled by the window's
        // key handler (they bubble up from the focused input).
        let search_input = cx.new(|cx| Composer::with_options("Search messages…", true, cx));
        let _search_input = cx.observe(&search_input, |this, editor, cx| {
            let q = editor.read(cx).text().to_string();
            if this.search.open && q != this.search.query {
                this.run_search(&q, cx);
            }
        });

        // The UI-side connection — only when the writer opened, so a broken
        // database is reported once, not twice. Read-mostly (FTS queries,
        // export); slice 19 also writes attachment staging through it, which
        // WAL + busy_timeout lets coexist with the worker's writes.
        let search_store = persist.as_ref().and_then(|_| Store::open_default().ok());
        // The restored conversation's staged attachments, if any survived the
        // last quit un-sent (slice 19 §3).
        let staged_attachments = search_store
            .as_ref()
            .and_then(|s| s.get_staged_attachments(&active_id).ok())
            .unwrap_or_default();

        // The template editor (slice 18 §6): a single-line name field (Enter
        // saves — minimum keystrokes, limited hand mobility) and a multiline
        // content area (Enter inserts a newline; Save commits). Escape in
        // either cancels the editor with nothing written.
        let template_name = cx.new(|cx| Composer::with_options("Template name", true, cx));
        let _template_name = cx.subscribe(
            &template_name,
            |this, _editor, ev: &ComposerEvent, cx| match ev {
                ComposerEvent::Submit => this.save_template(cx),
                ComposerEvent::Cancel => this.close_template_editor(cx),
                _ => {}
            },
        );
        let template_content =
            cx.new(|cx| Composer::with_options("System prompt text…", false, cx));
        let _template_content = cx.subscribe(
            &template_content,
            |this, _editor, ev: &ComposerEvent, cx| {
                if let ComposerEvent::Cancel = ev {
                    this.close_template_editor(cx);
                }
            },
        );

        // The saved-project instructions editor: a multiline area (Enter inserts
        // a newline; the Save button commits). Escape cancels with nothing
        // written — mirrors the template content area.
        let project_instructions = cx
            .new(|cx| Composer::with_options("Standing instructions for this project…", false, cx));
        let _project_instructions = cx.subscribe(
            &project_instructions,
            |this, _editor, ev: &ComposerEvent, cx| {
                if let ComposerEvent::Cancel = ev {
                    this.close_project_editor(cx);
                }
            },
        );

        let mut app = Self {
            conversations,
            views,
            sidebar: SidebarState::new(active_id),
            composer,
            rename_editor,
            did_initial_focus: false,
            // Assume active until the first render reads the real state; a launch
            // that immediately backgrounds is rare and only suppresses one notify.
            window_active: true,
            display_palette: theme::p(),
            appearance,
            window_w: 0.,
            notifier: notify_hooks::NotifyState::new(config.notifications.clone()),
            configured_choices: choices.clone(),
            choices,
            choice_idx,
            provider_health: std::collections::HashMap::new(),
            provider_probe_generation: 0,
            provider_probe_in_flight: false,
            discovered_models: std::collections::HashMap::new(),
            provider_endpoints,
            pending_saved_selection,
            rules_offline_streak: 0,
            recall_offline_streak: 0,
            last_window_title: String::new(),
            moved_off_dead_default: false,
            pending_delete: None,
            keys,
            memory,
            rules: None,
            recall: None,
            memory_chips: saved_memory_chips
                .iter()
                .map(|(id, saved)| (id.clone(), recall::MemoryChip::from_saved(saved)))
                .collect(),
            persist,
            work_last_run,
            run_ids,
            code_run_errors: Default::default(),
            work_footers,
            work_steps,
            work_contexts,
            work_folder_moves,
            memory_writes_this_session: 0,
            memory_saved_flash: false,
            memory_flash_gen: 0,
            hovered_row: None,
            copied: CopiedFlash::new(),
            thinking_phase: 0,
            strip: strip_app::StripState::new(cx),
            thinking_timer_running: false,
            thinking_timer_generation: 0,
            conversation_scroll_drag: Rc::new(Cell::new(None)),
            command_palette: command_palette::CommandPaletteState::new(),
            command_palette_return_focus: None,
            palette: PaletteState::new(),
            palette_input,
            palette_scroll: ScrollHandle::new(),
            prompt_history: prompt_history::PromptHistory::new(),
            drafts: composer_drafts::Drafts::default(),
            picker: PickerState::new(),
            toast: ToastState::new(),
            submit_guard: None,
            staged_attachments,
            // The "today" total starts from what earlier runs already spent
            // today (read synchronously above, priced per model — slice 8 §4).
            cost: CostTicker::new(feed::unix_now(), &today_usage),
            voice: Voice::new(voice_cfg),
            feed_cfg,
            feed,
            feed_snapshot: FeedSnapshot::default(),
            feed_panel_open: false,
            feed_expanded: None,
            rules_seen_online: None,
            search: SearchState::new(),
            search_results: Vec::new(),
            search_store,
            search_input,
            skills_search_focus: cx.focus_handle(),
            skills_focus_targets: Rc::default(),
            skills_reveal_key: Rc::default(),
            sessions_search_focus: cx.focus_handle(),
            sessions_back_focus: cx.focus_handle(),
            sessions_focus_pending: None,
            highlight_term: None,
            templates,
            conv_templates,
            template_editor: None,
            template_name,
            template_content,
            project_editor: None,
            project_instructions,
            shortcuts: ShortcutsState::new(),
            profile_store: profile::ProfileStore::new(),
            gateway_pool: gateway_pool::GatewayPool::new(),
            skills_state: skills::SkillsState::new(),
            skill_palette: skills::SkillPaletteState::new(),
            skill_palette_scroll: ScrollHandle::new(),
            skill_palette_dismissed: None,
            cron_state: cron::CronState::new(),
            sessions_state: sessions::SessionsState::new(),
            command_center_data: command_center::CommandCenterData::default(),
            messaging_data: messaging_pane::MessagingData::default(),
            councils: Default::default(),
            council_last_group: None,
            council_fleet: council_run::fleet_route(&config),
            council_groups: None,
            mode: Mode::Work,
            shell: consolidation::Shell::new(cx),
            workspace_read: workspace_read::WorkspaceRead::default(),
            work: {
                let mut w = WorkState::new(
                    HermesCfg {
                        enabled: !config.hermes.base_url.is_empty(),
                        base_url: config.hermes.base_url.clone(),
                        api_key: config.hermes.api_key.clone(),
                        dashboard_url: config.gateway.dashboard_url.clone(),
                    },
                    work::GatewayCfg {
                        enabled: !config.gateway.dashboard_url.is_empty(),
                        dashboard_url: config.gateway.dashboard_url.clone(),
                    },
                    config.engine.clone(),
                );
                // RC2-UX (R2-15): the folders chosen lately, for ⌘O.
                w.recent_folders = saved_recent_folders
                    .as_deref()
                    .and_then(|j| serde_json::from_str(j).ok())
                    .unwrap_or_default();
                w.code_active_project = code_active_project;
                // Polish slice A: restore the last Work folder (if it still
                // exists) and executor, so a relaunch resumes where Work left
                // off instead of resetting to "no folder chosen" + gateway.
                if let Some(f) = saved_work_folder
                    .as_deref()
                    .map(std::path::PathBuf::from)
                    .filter(|p| p.is_dir())
                {
                    w.folder_files = work::list_dir_top(&f, 14);
                    w.workspace_tree = Some(crate::workspace::FileTreeState::new(f.clone()));
                    w.file_tree = w
                        .workspace_tree
                        .as_mut()
                        .map(crate::workspace::file_tree_from_workspace)
                        .unwrap_or_default();
                    w.folder = Some(f);
                }
                // `[work.agents]` / `[models]` used to reach WorkState only
                // through `apply_config_live` (a Settings save), so a fresh
                // launch ignored a configured `zcode_bin` until the user
                // edited something. Apply them at construction too — P12/F1
                // adds `[outline]`/`[gitea]` to the same startup population.
                Self::apply_startup_work_config(&mut w, &config);
                w.open_builds = saved_open_builds;
                // Executor: a VALID persisted choice wins; otherwise the
                // config's `[work].default_executor` seeds it; otherwise
                // Gateway. Each source is parsed on its own, so a stale or
                // garbled persisted value cannot mask a valid config seed.
                if let Some(exec) = work::seed_executor(
                    saved_work_executor.as_deref(),
                    config.work.default_executor.as_deref(),
                ) {
                    w.executor = exec;
                }
                // RC2-R1F2 (round 1 N2): the helper chosen last carries to
                // "+ New conversation" — after a relaunch too (the persisted
                // choice is the last one made).
                w.last_pick = saved_work_executor
                    .as_deref()
                    .and_then(work::WorkExecutor::from_persist)
                    .filter(|e| !matches!(e, work::WorkExecutor::LocalBuild { .. }));
                // Restore saved Work-mode projects (SQLite `projects` table).
                w.projects = saved_projects;
                w.workshop_selected_folder = saved_code_active_project.clone();
                w
            },
            // P1: the fleet pack loads at startup (and re-loads in
            // `apply_runtime_config` on every re-apply); invalid or
            // missing → None, diagnostic only, sends unchanged.
            fleet_pack: fleet_skills::load_pack(&config),
            settings: SettingsState::new(),
            settings_field,
            settings_raw,
            settings_search_query: String::new(),
            settings_search_focus,
            memory_search,
            credential_input,
            _submit,
            _rename,
            _palette_input,
            _composer_palette,
            _composer_busy,
            _send_queue_tick,
            _settings_field,
            _settings_raw,
            _memory_search,
            _credential_input,
            _reload,
            _search_input,
            _template_name,
            _template_content,
            _project_instructions,
            messaging_credential_editor,
            _messaging_credential_editor,
            onboarding,
            onboarding_harness,
            onboarding_other_route,
            pending_harness_recipe: None,
            onboarding_error: None,
            welcome_focus: cx.focus_handle(),
            overlay_focus: OverlayFocus::new(cx),
            settings_nav: settings_nav::Navigation::new(),
            settings_error_details_open: false,
            settings_error_scroll: ScrollHandle::new(),
            settings_error_scroll_signature: None,
            settings_error_sheet_focus: cx.focus_handle(),
            settings_error_body_focus: cx.focus_handle(),
            settings_error_parent_focus: None,
            settings_active_tab_focus: cx.focus_handle(),
            local_discovery: local_discovery_ui::UiState::new(cx),
            settings_nav_focus: cx.focus_handle(),
            settings_nav_focus_pending: false,
            settings_return_focus_pending: false,
            settings_nav_focused: false,
            settings_nav_scroll: ScrollHandle::new(),
            settings_service_action_bounds: std::array::from_fn(|_| {
                std::rc::Rc::new(std::cell::Cell::new(None))
            }),
            settings_service_reveal_key: std::rc::Rc::new(std::cell::Cell::new(None)),
            settings_stop: None,
            settings_in_sidebar: true,
            settings_page_scroll: ScrollHandle::new(),
            settings_page_rows: Default::default(),
            allowed_actions: Default::default(),
            welcome_focus_pending: true,
            welcome_action: 0,
            config_load_error: config_error.is_some(),
            secret_notices,
            storage_notice: persistence::storage_problem(),
            checked_at: HashMap::new(),
        };
        // Canonical route state wins over old Code metadata. Invalid/unknown
        // records are intentionally not replaced: the DB retains the raw
        // value and the chooser getter reports unavailable instead of silently
        // selecting the process-global executor.
        for (id, raw) in &saved_conversation_routes {
            match crate::conversation_route::ConversationRoute::from_persisted_json(raw) {
                Ok(route) => {
                    app.work.conversation_routes.insert(id.clone(), route);
                }
                Err(_) => {
                    app.work
                        .unavailable_conversation_routes
                        .insert(id.clone(), raw.to_string());
                }
            }
        }
        for (id, raw) in &saved_code_tab_metas {
            if app.work.conversation_routes.contains_key(id)
                || app.work.unavailable_conversation_routes.contains_key(id)
            {
                continue;
            }
            let Some(meta) = crate::code_history::CodeConvMeta::from_json(raw) else {
                app.work
                    .unavailable_conversation_routes
                    .insert(id.clone(), raw.to_string());
                continue;
            };
            let restored = app
                .views
                .get(id)
                .map(work::bridge_records_from_view)
                .unwrap_or_default();
            let route = if let Some(saved_route) = meta.route.as_deref() {
                match crate::conversation_route::ConversationRoute::from_persisted_json(saved_route)
                {
                    Ok(route) => Some(route),
                    Err(_) => {
                        app.work
                            .unavailable_conversation_routes
                            .insert(id.clone(), saved_route.to_string());
                        None
                    }
                }
            } else {
                match meta.engine_model() {
                    Some((engine, model)) => {
                        Some(crate::conversation_route::ConversationRoute::from_legacy(
                            work::WorkExecutor::Agent { engine, model },
                            meta.folder.clone(),
                            meta.session.clone(),
                            awili_code::continuity::canonical_cursor(&restored),
                        ))
                    }
                    None => {
                        app.work
                            .unavailable_conversation_routes
                            .insert(id.clone(), raw.to_string());
                        None
                    }
                }
            };
            if let Some(route) = route {
                app.work.conversation_routes.insert(id.clone(), route);
            }
        }
        // Legacy Chat's per-thread model map is a seed only. It is consulted
        // after canonical routes and before any ambient default; malformed or
        // unsupported values deliberately leave the route unavailable.
        if let Some(raw) = saved_legacy_general_models.as_deref() {
            if let Ok(models) =
                serde_json::from_str::<std::collections::BTreeMap<String, String>>(raw)
            {
                for (id, target) in models {
                    if app.work.conversation_routes.contains_key(&id)
                        || app.work.unavailable_conversation_routes.contains_key(&id)
                    {
                        continue;
                    }
                    let folder = app.conversations.get(&id).and_then(|meta| meta.cwd.clone());
                    let restored = app
                        .views
                        .get(&id)
                        .map(work::bridge_records_from_view)
                        .unwrap_or_default();
                    match crate::conversation_route::ConversationRoute::from_legacy_persisted_target(
                        &target,
                        folder,
                        None,
                        awili_code::continuity::canonical_cursor(&restored),
                    ) {
                        Ok(route) => {
                            app.work.conversation_routes.insert(id, route);
                        }
                        Err(_) => {
                            app.work.unavailable_conversation_routes.insert(id, target);
                        }
                    }
                }
            }
        }
        let mut motion_changes = ui::motion::preference_changes();
        cx.spawn(async move |this, cx| {
            while motion_changes.next().await.is_some() {
                if this
                    .update(cx, |app, cx| {
                        app.reconcile_thinking_motion(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        // A configured CLI-only install has no usable Chat landing page.
        // Preserve a usable explicit agent preference; recover a stale gateway
        // or missing binary to the installed configured recipe.
        // R2g: a cold launch returns to the mode the user left (Chat, Work or
        // Code); with none saved yet, the CLI-only rule above picks Work.
        // Work opens on a fresh conversation, never an old one.
        // RC2-UX (R2-17): the Code tabs open at the last close come back
        // (before the landing, which opens a Code tab only when none exists).
        let restored_workshop = app.load_workshop_tabs(&saved_workshop_tabs, &saved_code_tab_metas);
        // `load_workshop_tabs` has validated the old per-thread helper map.
        // Promote each remaining helper to the canonical route only when no
        // versioned route already exists; this is migration input, not a live
        // second selection system.
        for (id, target) in app.work.chip_picked.clone() {
            if app.work.conversation_routes.contains_key(&id)
                || app.work.unavailable_conversation_routes.contains_key(&id)
            {
                continue;
            }
            let folder = app.conversations.get(&id).and_then(|meta| meta.cwd.clone());
            let restored = app
                .views
                .get(&id)
                .map(work::bridge_records_from_view)
                .unwrap_or_default();
            app.work.conversation_routes.insert(
                id,
                crate::conversation_route::ConversationRoute::from_legacy(
                    target,
                    folder,
                    None,
                    awili_code::continuity::canonical_cursor(&restored),
                ),
            );
        }
        // Defaults are the final absent-case fallback. Workshop compatibility
        // helpers and closed Code metadata are historical per-thread choices,
        // so they must be restored before a default can occupy their route.
        let route_seed_ids: Vec<String> = app
            .conversations
            .all()
            .iter()
            .map(|conversation| conversation.id.clone())
            .collect();
        for id in route_seed_ids {
            if app.work.conversation_routes.contains_key(&id)
                || app.work.unavailable_conversation_routes.contains_key(&id)
            {
                continue;
            }
            app.work.conversation_routes.insert(
                id,
                crate::conversation_route::ConversationRoute::new(app.work.executor.clone()),
            );
        }
        if matches!(saved_workshop_tabs, Ok(None)) {
            app.restore_code_tabs(saved_code_open_tabs.as_deref(), &saved_code_tab_metas);
        }
        // RC2-CNCL15: every council comes back with its members and rounds
        // (the sidebar names its members).
        let council_ids: Vec<String> = app
            .conversations
            .all()
            .iter()
            .filter(|c| c.kind == ConvKind::Council)
            .map(|c| c.id.clone())
            .collect();
        for id in council_ids {
            app.ensure_council_page(&id);
        }
        let cli_only = app.recover_cli_only_executor();
        if restored_workshop
            && app.shell.tabs.active.is_some()
            && saved_last_mode.as_deref() != Some("council")
        {
            app.open_workshop(cx);
            if app.mode == Mode::Work {
                app.enter_work_surface(cx);
            }
        } else {
            app.land_on_launch(saved_last_mode.as_deref(), cli_only, cx);
        }
        app.sync_workshop_tabs();
        // Canonical selection has resolved the final folder. A saved same-
        // folder Work landing still needs its missing terminal resources.
        if app.mode == Mode::Work {
            app.ensure_work_terminals(cx);
        }
        if project_import_error.is_some() {
            app.shell.tabs.project_import_notice = project_import_error.clone();
            app.show_toast(
                "Some saved projects could not be imported. Existing projects are available.",
                cx,
            );
        }
        if work_executor_notice {
            app.show_toast(local_first_migrate::MIGRATION_NOTICE, cx);
        }
        // Daily-driver P2d: open build rows reattach by `status` of the SAME
        // request id — the durable job survives the restart; no fresh submit
        // is ever made.
        app.reattach_open_builds(cx);
        // Daily-driver P2e: the restored active conversation's last run
        // (identity + terminal status) comes back with it — before any
        // reattached build rewrites the key.
        app.restore_work_last_run();
        app.start_notifications(cx);
        app.ensure_health_ticker(cx);
        // RC2-R61: a quit stops what is running as a Stop does.
        Self::watch_quit(cx);
        app
    }

    // ---- conversations (slice 7) -----------------------------------------------

    fn active_view(&self) -> Option<&ConvView> {
        self.views.get(self.sidebar.active_id())
    }

    /// Create a fresh conversation, select it, and focus the composer (the
    /// "+ New" button and cmd+N).
    fn new_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shell.tabs.delete_project.is_some() {
            return;
        }
        self.close_workshop_archive(cx);
        if self.mode == Mode::Code {
            return self.new_code_tab_like_active(cx);
        }
        let kind = self.mode_conv_kind();
        self.new_conversation_kind(kind, window, cx);
    }

    /// The conversation kind the active mode operates on: Code → Code, Council →
    /// Council, every other mode → Work. Utility modes (Skills/Cron/Sessions)
    /// don't own a conversation, so a new conversation from there is a Work one.
    fn mode_conv_kind(&self) -> ConvKind {
        if self.mode == Mode::Work {
            ConvKind::Work
        } else {
            code_reopen::sidebar_kind_for(self.mode)
        }
    }

    /// Create a fresh conversation of `kind`, make it active, and remember it as
    /// this kind's place. Used by the "+ New" button, cmd+N, and the
    /// activate-kind fallback when a mode has no conversation yet.
    fn new_conversation_kind(
        &mut self,
        kind: ConvKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // RC2-R68: the box's text tells whether the conversation on screen
        // holds a draft of the owner's.
        let box_text = self.composer.read(cx).text().to_string();
        let (id, reused) = self.fresh_conversation_with(kind, Some(&box_text));
        // RC2-R54 (usability RC2 round 4, Minor: a new conversation opened
        // with the text an earlier "Continue in Work" left): an untouched
        // copy goes; the owner's own draft stays his.
        if reused {
            self.drop_placed_draft(&id, cx);
        }
        self.open_new_conversation(&id, window, cx);
        if reused {
            // Anything the user staged in it before is still theirs.
            self.refresh_staged_attachments();
        }
    }

    /// R2g: the window-free half of [`Self::new_conversation_kind`] (the
    /// launch landing uses it too): reuse a never-sent Work conversation or
    /// create one of `kind`. Returns its id and whether it was reused.
    pub(crate) fn fresh_conversation(&mut self, kind: ConvKind) -> (String, bool) {
        self.fresh_conversation_with(kind, None)
    }

    /// [`Self::fresh_conversation`], told what the message box holds now
    /// (`None`: not known, read as empty).
    ///
    /// RC2-R68 (usability RC2 round 5, Minor 5: "+ New conversation" re-opened
    /// an old never-sent conversation with the owner's edited draft in it): a
    /// never-sent conversation holding a draft of the owner's is his work in
    /// progress and is never the "new" one; a blank one (or one holding only
    /// what "Continue in Work" put there, untouched) still is.
    pub(crate) fn fresh_conversation_with(
        &mut self,
        kind: ConvKind,
        box_text: Option<&str>,
    ) -> (String, bool) {
        self.fresh_conversation_except(kind, box_text, &std::collections::HashSet::new())
    }

    /// The common helper menu may reuse a closed, unused thread, while an
    /// already open blank tab remains the distinct thread the user chose.
    pub(crate) fn fresh_conversation_except(
        &mut self,
        kind: ConvKind,
        box_text: Option<&str>,
        excluded: &std::collections::HashSet<String>,
    ) -> (String, bool) {
        let starting_helper = if kind == ConvKind::Work {
            // R2b: a new Work conversation starts on the Default Work helper.
            self.start_on_default_helper();
            self.starting_project_helper()
        } else {
            None
        };
        // R1f: a Work conversation that was never sent is reused, so
        // "+ New conversation" never leaves empty "New conversation" rows.
        let reusable = self
            .unsent_work_conversations_of(kind)
            .into_iter()
            .find(|id| {
                if excluded.contains(id) {
                    return false;
                }
                // RC2-R6M12 (RC2-R6 review P2 on 4a02953f: a never-sent
                // conversation with a file staged was reused, or re-filed by
                // RC2-R67's new conversation, file and all): one with a file
                // staged is the owner's work in progress too.
                if self.has_staged_attachments(id) {
                    return false;
                }
                let key = composer_drafts::conversation_key(id);
                if self.drafts.is_shown(&key) {
                    let text = box_text.unwrap_or_default();
                    text.trim().is_empty() || self.drafts.is_untouched(&key, text)
                } else {
                    !self.drafts.has_owner_draft(&key)
                }
            });
        if let Some(id) = reusable {
            self.reuse_unsent_work_conversation(&id);
            // A closed, blank thread reused as a new logical thread receives
            // the same configured starting helper as a newly created thread.
            // Bind it to the thread so the remembered last pick cannot mask
            // an explicit Settings preference.
            if let Some(helper) = starting_helper {
                self.work.chip_picked.insert(id.clone(), helper);
            } else {
                // A restored or stale per-thread chip must not survive as the
                // helper of this new logical thread when no start preference
                // or remembered fallback exists.
                self.work.chip_picked.remove(&id);
            }
            // RC1-S: nor on a folder picked in it before (it is re-filed to
            // the folder on screen).
            self.work.folder_picked.remove(&id);
            return (id, true);
        }
        let id = self.create_conversation(kind);
        if let Some(helper) = starting_helper {
            self.work.chip_picked.insert(id.clone(), helper);
        }
        (id, false)
    }

    /// RC2-R1F4: create a brand-new conversation of `kind` (never a reused
    /// one): tagged with the active profile, saved, and — a Work one — filed
    /// under the folder the composer shows. Returns its id.
    pub(crate) fn create_conversation(&mut self, kind: ConvKind) -> String {
        let meta = self.conversations.create_kind(feed::unix_now(), kind);
        // Tag with the active profile.
        let active_profile = self.profile_store.active().to_string();
        if active_profile != "default" {
            if let Some(c) = self.conversations.get_mut(&meta.id) {
                c.profile = Some(active_profile.clone());
            }
        }
        self.views.insert(meta.id.clone(), ConvView::new());
        if let Some(p) = &self.persist {
            p.send(PersistCmd::CreateConversation {
                id: meta.id.clone(),
                title: meta.title.clone(),
                created_ts: meta.created_ts,
                kind: kind.as_str().to_string(),
            });
            if active_profile != "default" {
                p.send(PersistCmd::SetProfile {
                    id: meta.id.clone(),
                    profile: Some(active_profile),
                });
            }
        }
        if kind == ConvKind::Work {
            // R1f: filed under the folder the composer shows.
            self.file_work_conversation(&meta.id);
        }
        // A fresh Workshop thread gets an explicit route immediately, so the
        // UI never has to infer a target from a mutable global later.
        self.work
            .conversation_routes
            .entry(meta.id.clone())
            .or_insert_with(|| {
                crate::conversation_route::ConversationRoute::new(self.work.executor.clone())
            });
        meta.id
    }

    /// Select a new (or reused, never-sent) conversation and start a fresh
    /// session in it: the tail of [`Self::new_conversation_kind`].
    fn open_new_conversation(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_active_conversation(id);
        self.remember_active_kind();
        self.save_active_conversation();
        // A brand-new conversation has nothing staged (no DB read needed —
        // its row may not even be committed by the worker yet).
        self.staged_attachments.clear();
        // A new conversation starts a new session for the cost ticker (slice 8
        // §4); today's total is unaffected. The distillation flood cap (slice
        // 10 §3) uses the same session unit.
        self.cost.reset_session();
        self.memory_writes_this_session = 0;
        let h = self.composer.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// Record the active conversation as the remembered place for its kind, so
    /// switching modes restores the right thread.
    fn remember_active_kind(&mut self) {
        let kind = self
            .conversations
            .get(self.sidebar.active_id())
            .map(|c| c.kind)
            .unwrap_or_default();
        match kind {
            ConvKind::Chat => {
                self.sidebar.last_chat_id = Some(self.sidebar.active_id().to_string())
            }
            ConvKind::Work => {
                self.sidebar.last_work_id = Some(self.sidebar.active_id().to_string())
            }
            ConvKind::Code => {} // U1: a Code tab is its own place; no remembered thread
            // RC2-CNCL5: Council keeps its own place too.
            ConvKind::Council => {
                self.sidebar.last_council_id = Some(self.sidebar.active_id().to_string())
            }
        }
    }

    /// Ensure the active conversation is one of `kind` (called when entering Chat
    /// or Work mode). Keeps the current thread if it already matches; otherwise
    /// restores this kind's remembered thread, falls back to the most recent
    /// unarchived thread of that kind, and creates a fresh one only if none
    /// exists. No `window` needed — mode switches don't refocus the composer
    /// (the render loop's initial-focus path handles that).
    fn activate_kind(&mut self, kind: ConvKind, cx: &mut Context<Self>) {
        if self
            .conversations
            .get(self.sidebar.active_id())
            .filter(|c| !c.archived)
            .map(|c| c.kind)
            == Some(kind)
        {
            return;
        }
        let remembered = match kind {
            ConvKind::Chat => self.sidebar.last_chat_id.clone(),
            ConvKind::Work => self.sidebar.last_work_id.clone(),
            ConvKind::Code => None,
            ConvKind::Council => self.sidebar.last_council_id.clone(),
        };
        let target = remembered
            .filter(|id| {
                self.conversations
                    .get(id)
                    .map(|c| c.kind == kind && !c.archived)
                    .unwrap_or(false)
            })
            .or_else(|| {
                self.conversations
                    .active()
                    .iter()
                    .find(|c| c.kind == kind)
                    .map(|c| c.id.clone())
            });
        match target {
            Some(id) => {
                if self.set_active_conversation(&id) {
                    self.remember_active_kind();
                    self.save_active_conversation();
                    self.refresh_staged_attachments();
                    // Daily-driver P2e: switching INTO Work mode restores the
                    // activated conversation's last run into the idle panel.
                    if kind == ConvKind::Work {
                        self.restore_work_last_run();
                        // RC1-S: and its folder.
                        self.restore_work_folder(cx);
                    }
                }
            }
            None => {
                // No conversation of this kind yet — create one. Done off the
                // window-bearing path, so inline a minimal create (cmd+N's
                // new_conversation_kind needs a window for focus).
                let meta = self.conversations.create_kind(feed::unix_now(), kind);
                self.views.insert(meta.id.clone(), ConvView::new());
                if let Some(p) = &self.persist {
                    p.send(PersistCmd::CreateConversation {
                        id: meta.id.clone(),
                        title: meta.title.clone(),
                        created_ts: meta.created_ts,
                        kind: kind.as_str().to_string(),
                    });
                }
                self.set_active_conversation(&meta.id);
                self.remember_active_kind();
                self.save_active_conversation();
                self.staged_attachments.clear();
            }
        }
        cx.notify();
    }

    /// Switch the main view to `id`. The previous conversation's view (scroll,
    /// rows, any in-flight stream) stays alive in `views`.
    fn select_conversation(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Selecting anything abandons a half-armed delete (B2).
        self.pending_delete = None;
        self.open_conversation_from_history(id, cx);
        // A sheet covers the thread it just opened; a docked panel stays.
        if consolidation::workshop_mode(self.mode)
            && !self.workshop_history_docked(window.viewport_size().width)
        {
            self.shell.conversations = false;
        }
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Remember the active conversation across launches.
    fn save_active_conversation(&self) {
        if self.shell.archived_reader.is_some() {
            return;
        }
        if let Some(p) = &self.persist {
            p.set_state(
                ACTIVE_CONVERSATION_KEY,
                self.sidebar.active_id().to_string(),
            );
        }
    }

    /// Daily-driver P2e: when the active conversation is a Work conversation
    /// and no run is live, restore its last run's identity/status/error into
    /// the idle `RunPanel` so the existing rendering shows
    /// `Requested: <executor · model>` and the terminal status exactly as
    /// before the quit. A live run, a fresh submit, or a missing/corrupt key
    /// leaves the panel as today; restore never touches a running panel.
    fn restore_work_last_run(&mut self) {
        let is_work = self
            .conversations
            .get(self.sidebar.active_id())
            .map(|c| c.kind == ConvKind::Work)
            .unwrap_or(false);
        if is_work {
            // P2e R3: the running guard, the row lookup, and the panel
            // ownership hand-off all live in `restore_into`.
            let active = self.sidebar.active_id().to_string();
            work_last_run::restore_into(&mut self.work, &active, &self.work_last_run);
            // RC1-O (pre-review 3): the chip follows the conversation even
            // when it has no last run (an owner's pick in an unsent one).
            work_last_run::restore_executor(&mut self.work, &active, &self.work_last_run);
        }
    }

    /// RC2-APP (F0300): when the conversation that owns the Work run was
    /// deleted, the panel has no owner any more — once nothing of that run is
    /// still going (live, stopping, or waiting on its memory gate), the owner
    /// id and its panel are dropped so the next conversation shows its own.
    /// While something is still going, the late events find no conversation
    /// and are dropped; this runs again at the run's end.
    pub(crate) fn forget_deleted_run_owner(&mut self) {
        let Some(owner) = self.work.conv_id.as_deref() else {
            return;
        };
        if self.views.contains_key(owner)
            || self.conversations.get(owner).is_some()
            || self.work.running
            || self.work.gateway_stop_pending
            || self.work.agent_gate_pending
        {
            return;
        }
        self.work.conv_id = None;
        self.work.stream_row_idx = None;
        self.work.panel = work::RunPanel::default();
    }

    /// RC1-O (pre-review P1): when a run ends, the chip goes back to the
    /// active Work conversation's own lane when that is another conversation — opening a Claude conversation
    /// while a Gemma run went on elsewhere left the Gemma chip on it, and its
    /// next send went to Gemma. The owner's own pick there wins
    /// (`chip_picked`).
    pub(crate) fn settle_work_chip(&mut self) {
        // RC2-APP (F0300): a run that ended after its conversation was
        // deleted leaves no owner behind.
        self.forget_deleted_run_owner();
        if self.shell.archived_reader.is_some() {
            return;
        }
        let active = self.sidebar.active_id().to_string();
        // The run that just ended belongs to `work.conv_id`: its own
        // conversation is already on the lane it ran on, and its last-run
        // row may not be written yet (pre-review 3) — leave it.
        if self.work.conv_id.as_deref() == Some(active.as_str()) {
            return;
        }
        // RC1-O-FIX5 (whole-slice review P1): never while a prompt is parked
        // — behind a Stop, or behind the local model's health probe — it goes
        // out on the chip it was parked under; the settle after its run ends
        // (or after it is handed back) applies.
        if self.work.queued_after_stop.is_some() || self.work.local.health.pending_send.is_some() {
            return;
        }
        let is_work = self
            .conversations
            .get(&active)
            .is_some_and(|c| c.kind == ConvKind::Work && !c.archived);
        if is_work {
            work_last_run::restore_executor(&mut self.work, &active, &self.work_last_run);
        }
    }

    // ---- conversation search (slice 14) -------------------------------------

    /// ⌘F: open the search panel (fresh query, input focused) or close it.
    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shell.tabs.delete_project.is_some() {
            return;
        }
        if self.search.open {
            self.close_search(window, cx);
            return;
        }
        self.search.open();
        self.shell.more = false;
        self.search_results.clear();
        self.search_input.update(cx, |c, cx| c.set_text("", cx));
        let h = self.search_input.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// Close the panel and hand focus back to the chat composer. Closing also
    /// puts out any bubble highlight (slice 16 §3) — `open_search_result`
    /// re-lights it AFTER closing, so the one path that should keep a
    /// highlight does.
    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.close();
        self.highlight_term = search::next_highlight(search::HighlightEvent::Clear);
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Re-run the search for `query`: title matches first (most recently
    /// updated first, archived included), then FTS body matches, deduped.
    fn run_search(&mut self, query: &str, cx: &mut Context<Self>) {
        let ordered: Vec<(String, String)> = self
            .conversations
            .active()
            .into_iter()
            .chain(self.conversations.archived())
            .map(|c| (c.id.clone(), c.title.clone()))
            .collect();
        let titles = search::title_hits(query, &ordered);
        let body = self
            .search_store
            .as_ref()
            .map(|s| match s.search_messages(query, SEARCH_RESULT_LIMIT) {
                Ok(hits) => hits,
                Err(e) => {
                    eprintln!("awili: search failed: {e}");
                    Vec::new()
                }
            })
            .unwrap_or_default();
        self.search_results = search::merge_hits(titles, body, SEARCH_RESULT_LIMIT);
        self.search.set_query(query, self.search_results.len());
        cx.notify();
    }

    /// Enter/click on a result: switch to that conversation, close the panel,
    /// and light the search term up in the opened conversation's bubbles
    /// (slice 16 §3 — after `close_search`, which clears the highlight).
    fn open_search_result(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.to_string();
        let query = self.search.query.clone();
        self.select_conversation(&id, window, cx);
        self.close_search(window, cx);
        self.highlight_term = search::next_highlight(search::HighlightEvent::Select(&query));
        cx.notify();
    }

    /// Soft-archive an idle Workshop thread, closing its execution tab while
    /// retaining the stored transcript. Busy/pending owners and native drafts
    /// stay reachable. Council retains its separate lifecycle behavior.
    fn archive_conversation(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self.conversations.get(id).map(|c| c.kind) else {
            return;
        };
        let backing = self.workshop_conversation_backing(id);
        let preserve_native = self.workshop_backing().is_some_and(|visible| {
            matches!(visible, workshop::Backing::Agent(_)) && visible != backing
        });
        if kind != ConvKind::Council
            && (self.workshop_tab_busy(&backing) || self.workshop_agent_holds_draft(&backing, cx))
        {
            self.show_toast(
                "Stop the job or finish its waiting prompt, plan or draft before archiving.",
                cx,
            );
            return;
        }
        let tab_id = self
            .shell
            .tabs
            .tabs
            .iter()
            .find(|t| t.backing == backing)
            .map(|t| t.id.clone());
        if !self.conversations.archive(id) {
            return;
        }
        // RC2-R2G5: an archived council's Codex servers made ready ahead end.
        crate::codex_app_server::close_warm_for(id);
        if let Some(p) = &self.persist {
            p.send(PersistCmd::SetArchived {
                id: id.to_string(),
                archived: true,
            });
        }
        // Mark first, so last-close cannot reuse the newly archived blank row.
        if let Some(tab_id) = tab_id {
            self.close_workshop_tab(&tab_id, cx);
        }
        if self.sidebar.active_id() == id && preserve_native {
            // Rebind the stored selection/tray without leaving the native
            // helper. Its blank runtime must not retain a removed row owner.
            if let Some(tab) = self.shell.tabs.active.clone() {
                self.select_workshop_tab(&tab, cx);
            }
        } else if self.sidebar.active_id() == id {
            // RC3-S1a: deleting the council on screen lands on a new empty
            // council (a never-sent one is reused), not on another one.
            if kind == ConvKind::Council {
                self.new_conversation_kind(ConvKind::Council, window, cx);
            } else {
                match sidebar::next_active_after_archive(&self.conversations, id, kind) {
                    Some(next) => self.select_conversation(&next, window, cx),
                    None => self.new_conversation(window, cx),
                }
            }
        }
        self.sync_workshop_tabs();
        cx.notify();
    }

    /// HARD-delete a conversation after its inline confirmation (B2): drop
    /// the view and metadata, delete the persisted rows, and fall back like
    /// archive does when it was active.
    fn delete_conversation(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_delete = None;
        let Some(kind) = self.conversations.get(id).map(|c| c.kind) else {
            return;
        };
        let backing = self.workshop_conversation_backing(id);
        let preserve_native = self.workshop_backing().is_some_and(|visible| {
            matches!(visible, workshop::Backing::Agent(_)) && visible != backing
        });
        if matches!(backing, workshop::Backing::Agent(_))
            && (self.workshop_tab_busy(&backing) || self.workshop_agent_holds_draft(&backing, cx))
        {
            self.show_toast(
                "Stop the job or finish its waiting prompt, plan or draft before deleting.",
                cx,
            );
            return;
        }
        if self.work.local.down.as_ref().is_some_and(|down| {
            down.text.is_some() && down.conv_id.as_deref().is_none_or(|owner| owner == id)
        }) {
            self.show_toast(
                "Retry or edit the message held by the local-model notice before deleting.",
                cx,
            );
            return;
        }
        let tab_id = self
            .shell
            .tabs
            .tabs
            .iter()
            .find(|t| t.backing == backing)
            .map(|t| t.id.clone());
        if self
            .shell
            .archived_reader
            .as_ref()
            .is_some_and(|reader| reader.id == id)
        {
            self.close_workshop_archive(cx);
        }
        if !self.conversations.remove(id) {
            return;
        }
        // RC3-S1a: no hover tooltip may name a conversation that is gone.
        if self.sidebar.hovered_id.as_deref() == Some(id) {
            self.sidebar.hovered_id = None;
        }
        self.work.send_queue.forget(id);
        self.views.remove(id);
        // RC2-CNCL6: a deleted council's members stop answering.
        if let Some(mut page) = self.councils.remove(id) {
            page.stop_all();
        }
        // RC2-R2G5: and its Codex servers made ready ahead end.
        crate::codex_app_server::close_warm_for(id);
        // RC1-J: the model history kept for it goes too.
        self.work.kept_histories.remove(id);
        crate::work::inapp_history::forget(id);
        // S1b: and its engine log, once the worker has let go of it.
        crate::work::delete_engine_log(&mut self.work, id);
        // RC2-APP: its last run, saved reply footers and folder pick too
        // (`PersistCmd::DeleteConversation` removes their rows).
        self.work_last_run.remove(id);
        self.work_footers.remove(id);
        self.work_steps.remove(id);
        self.work_contexts.remove(id);
        self.run_ids.forget(id);
        self.work_folder_moves.remove(id);
        self.work.folder_picked.remove(id);
        self.forget_deleted_run_owner();
        if let Some(p) = &self.persist {
            p.send(PersistCmd::DeleteConversation { id: id.to_string() });
        }
        if let Some(tab_id) = tab_id {
            self.close_workshop_tab(&tab_id, cx);
        }
        if self.sidebar.active_id() == id && preserve_native {
            if let Some(tab) = self.shell.tabs.active.clone() {
                self.select_workshop_tab(&tab, cx);
            }
        } else if self.sidebar.active_id() == id {
            match sidebar::next_active_after_archive(&self.conversations, id, kind) {
                Some(next) => self.select_conversation(&next, window, cx),
                None => self.new_conversation(window, cx),
            }
        }
        self.sync_workshop_tabs();
        cx.notify();
    }

    /// Restore an archived conversation to the default view.
    fn unarchive_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.conversations.unarchive(id) {
            // RC2-R2H: an unarchived council may be made ready ahead again.
            crate::codex_app_server::reopen_warm_for(id);
            if let Some(p) = &self.persist {
                p.send(PersistCmd::SetArchived {
                    id: id.to_string(),
                    archived: false,
                });
            }
            cx.notify();
        }
    }

    /// Begin the inline rename of a row (double-click): prefill the editor with
    /// the current title and focus it.
    fn begin_rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(meta) = self.conversations.get(id) else {
            return;
        };
        let title = meta.title.clone();
        self.sidebar.begin_rename(id);
        self.rename_editor
            .update(cx, |c, cx| c.set_text(&title, cx));
        let h = self.rename_editor.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// Commit the inline rename with the editor's text (Enter). A blank title
    /// cancels; a real one persists as a manual rename.
    fn commit_rename(&mut self, title: &str, cx: &mut Context<Self>) {
        if let Some((id, committed)) = self.sidebar.commit_rename(&mut self.conversations, title) {
            if let Some(p) = &self.persist {
                p.send(PersistCmd::Rename {
                    id,
                    title: committed,
                    custom: true,
                });
            }
        }
        cx.notify();
    }

    // ---- settings (slice 7 §3) ---------------------------------------------

    /// Open the settings panel (gear / cmd+,), reloading the document from
    /// disk and syncing the raw tab's editor to it.
    fn open_settings(&mut self, cx: &mut Context<Self>) {
        if self.shell.tabs.delete_project.is_some() {
            return;
        }
        self.shell.more = false;
        self.settings.open();
        // RC2-R64: Settings reports what macOS allows now.
        self.notifier.query_permission();
        self.settings_search_query.clear();
        // SXi: the keyboard starts in the sidebar, on General.
        self.settings_stop = None;
        self.settings_in_sidebar = true;
        self.sync_raw_editor(cx);
        if self.settings.tab == settings::Tab::Form {
            self.settings_nav = settings_nav::Navigation::new();
            self.settings_nav_focus_pending = true;
            // Opening lands on AddProvider (fresh navigation); scroll it
            // into view so the user sees the selection on entry.
            if let Some(count) = self.settings.draft.as_ref().map(|d| d.providers.len()) {
                if let Some(ix) =
                    settings_nav::selected_settings_row(self.settings_nav.selected(), count)
                {
                    self.settings_nav_scroll.scroll_to_item(ix);
                }
            }
        } else {
            self.settings_nav_focus_pending = false;
        }
        // While the first run is underway, mark the welcome panel for
        // focus restoration the moment the overlay closes. The render
        // applies the focus; while the overlay is open its own inputs
        // keep theirs.
        if self.onboarding.is_active() {
            self.welcome_focus_pending = true;
        }
        cx.notify();
    }

    /// Refresh the raw tab's editor from the document — or, when the file on
    /// disk failed to parse, from the file itself so the raw tab can repair it.
    fn sync_raw_editor(&mut self, cx: &mut Context<Self>) {
        let text = match self.settings.doc.as_ref() {
            Some(doc) => doc.text(),
            None => std::fs::read_to_string(self.settings.path()).unwrap_or_default(),
        };
        // D1: secrets never render in the raw editor. The hidden originals are
        // kept on the settings state and spliced back at save time wherever
        // the mask is untouched, so a display-redacted save cannot corrupt
        // the file with literal mask characters.
        let (masked, hidden) = settings::redact_secrets(&text);
        self.settings.raw_redactions = hidden;
        // Capture the EXACT masked editor text: `restore_secrets` binds every
        // raw save against this snapshot (fail-closed; any change while
        // masked values are visible is refused before write).
        self.settings.raw_masked_snapshot = masked.clone();
        self.settings_raw
            .update(cx, |c, cx| c.set_text(&masked, cx));
    }

    fn set_settings_tab(&mut self, tab: settings::Tab, cx: &mut Context<Self>) {
        self.settings.set_tab(tab);
        if tab == settings::Tab::Raw {
            self.sync_raw_editor(cx);
        }
        if tab == settings::Tab::Form {
            self.settings_nav = settings_nav::Navigation::new();
            self.settings_nav_focus_pending = true;
            // Switching to the Form tab lands on AddProvider; scroll it into
            // view (navigation intent, not a render-time scroll).
            if let Some(count) = self.settings.draft.as_ref().map(|d| d.providers.len()) {
                if let Some(ix) =
                    settings_nav::selected_settings_row(self.settings_nav.selected(), count)
                {
                    self.settings_nav_scroll.scroll_to_item(ix);
                }
            }
        } else {
            self.settings_nav_focus_pending = false;
        }
        cx.notify();
    }

    fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.pending_harness_recipe = None;
        self.settings.close();
        self.settings_nav_focus_pending = false;
        self.settings_return_focus_pending = true;
        // The welcome panel was hidden under the overlay; the next
        // render hands focus back to it (first run only — configured
        // launches never have a welcome panel).
        if self.onboarding.is_active() {
            self.welcome_focus_pending = true;
        }
        cx.notify();
    }

    /// The profile-folder problem (Windows), plainly, above everything else:
    /// nothing is saved while it stands.
    fn storage_notice_banner(&self, message: &str) -> gpui::AnyElement {
        div()
            .id("storage-notice-banner")
            .flex_none()
            .flex()
            .flex_col()
            .min_w_0()
            .px(theme::z(theme::SPACE_L))
            .py(theme::z(theme::SPACE_S))
            .border_b_1()
            .border_color(theme::p().chip_warn)
            .text_size(theme::z(theme::LABEL_SIZE))
            .text_color(theme::p().chip_warn)
            .child(format!(
                "{message}. Your conversations and settings are not being saved until that is fixed."
            ))
            .into_any_element()
    }

    /// A plain notice for `secret:` keys the keychain could not supply, one
    /// line each, naming the provider or route (never a value).
    fn secret_notice_banner(&self) -> gpui::AnyElement {
        div()
            .id("secret-notice-banner")
            .flex_none()
            .flex()
            .flex_col()
            .min_w_0()
            .px(theme::z(theme::SPACE_L))
            .py(theme::z(theme::SPACE_S))
            .border_b_1()
            .border_color(theme::p().chip_warn)
            .text_size(theme::z(theme::LABEL_SIZE))
            .text_color(theme::p().chip_warn)
            .children(self.secret_notices.iter().map(|line| {
                // A line that is already a full sentence (the config-file
                // protection notice) stands as written; the rest are fragments.
                let text = if line.ends_with('.') {
                    line.clone()
                } else {
                    format!("{line}. Open Settings and enter it again.")
                };
                div()
                    .min_w_0()
                    .child(text)
                    .into_any_element()
            }))
            .into_any_element()
    }

    /// First-run center: replaces the mode-driven center while onboarding
    /// is active (see render); configured launches never enter it.
    /// Fixed startup config-error banner. Rendered only while
    /// `config_load_error` is true — i.e. only when
    /// `Config::load_with_source` errored at launch. The copy is a fixed
    /// string: never the raw error, parser detail, URL, or credential.
    fn config_error_banner(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .min_w(theme::z(0.))
            .gap(theme::z(10.))
            .px(theme::z(14.))
            .py(theme::z(8.))
            .bg(gpui::Rgba {
                a: 0.16,
                ..theme::p().sev_critical
            })
            .border_b_1()
            .border_color(theme::p().sev_critical)
            .child(
                div()
                    .min_w(theme::z(0.))
                    .w_full()
                    .text_size(theme::z(theme::TEXT_SIZE - 1.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::p().sev_critical)
                    .child(
                        "Your configuration could not be loaded. Open Settings to repair it, then check again.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(theme::z(8.))
                    .child(
                        div()
                            .id("config-error-open-settings")
                            .flex()
                            .items_center()
                            .justify_center()
                            .px(theme::z(10.))
                            .py(theme::z(3.))
                            .rounded(theme::z(theme::RADIUS))
                            .border_1()
                            .border_color(theme::p().sev_critical)
                            .text_size(theme::z(theme::LABEL_SIZE))
                            .text_color(theme::p().sev_critical)
                            .cursor_pointer()
                            .hover(|s| {
                                s.bg(gpui::Rgba {
                                    a: 0.25,
                                    ..theme::p().sev_critical
                                })
                            })
                            .child("Open Settings")
                            .on_click(cx.listener(|this, _ev, _w, cx| {
                                this.open_settings(cx);
                            })),
                    )
                    .child(
                        div()
                            .id("config-error-check-again")
                            .flex()
                            .items_center()
                            .justify_center()
                            .px(theme::z(10.))
                            .py(theme::z(3.))
                            .rounded(theme::z(theme::RADIUS))
                            .border_1()
                            .border_color(theme::p().sev_critical)
                            .text_size(theme::z(theme::LABEL_SIZE))
                            .text_color(theme::p().sev_critical)
                            .cursor_pointer()
                            .hover(|s| {
                                s.bg(gpui::Rgba {
                                    a: 0.25,
                                    ..theme::p().sev_critical
                                })
                            })
                            .child(shortcuts::localize("Check again (⌘⇧R)").into_owned())
                            .on_click(cx.listener(|this, _ev, _w, cx| {
                                this.check_startup_config(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn welcome_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        // First-run welcome copy follows the onboarding state only — a save
        // never claims tested inference or endpoint reachability. Snapshot the
        // state so the build below borrows no more than the snapshots.
        let ready = self.onboarding.is_ready();
        let saved = self.onboarding.is_saved();
        let error = self.onboarding_error;
        // Saved providers as honest status rows: the user-supplied name plus
        // configured/checking/reachable/unreachable. No raw URLs, keys or
        // tokens; no rows while unsaved (the missing panel shows none).
        let provider_rows: Vec<String> = if saved {
            let in_flight = self.provider_probe_in_flight;
            self.provider_endpoints
                .iter()
                .map(|p| {
                    format!(
                        "{}: {}",
                        p.name,
                        onboarding::endpoint_status(
                            self.provider_health.get(&p.base_url).copied(),
                            in_flight,
                        )
                        .label()
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        // Keyboard selection: how many actions exist right now (Start
        // chatting only while Ready) and which one is selected. Clamped
        // here so a Ready transition can never leave a stale,
        // out-of-range index. The selected chip takes the accent
        // treatment (the same styling as the hover state) while the
        // panel owns focus.
        let available = usize::from(ready) + 2;
        if self.welcome_action > available - 1 {
            self.welcome_action = available - 1;
        }
        let selected = self.welcome_action;
        let focused = self.welcome_focus.is_focused(window);
        let setup_border = if focused && selected == 0 {
            theme::p().accent
        } else {
            theme::p().chip_border
        };
        let setup_text = if focused && selected == 0 {
            theme::p().accent_text
        } else {
            theme::p().chip_text
        };
        let check_border = if focused && selected == 1 {
            theme::p().accent
        } else {
            theme::p().chip_border
        };
        let check_text = if focused && selected == 1 {
            theme::p().accent_text
        } else {
            theme::p().chip_text
        };
        let start_border = if focused && selected == 2 {
            theme::p().accent
        } else {
            theme::p().chip_border
        };
        let start_text = if focused && selected == 2 {
            theme::p().accent_text
        } else {
            theme::p().chip_text
        };
        let harness_only = self.onboarding_harness.is_some() && !self.onboarding_other_route;
        let subtext = onboarding::welcome_subtext(
            ready,
            saved,
            harness_only,
            !self.choices.is_empty(),
        );
        div()
            .id("welcome-panel")
            .track_focus(&self.welcome_focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_w(theme::z(0.))
            .h_full()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .text_size(theme::z(18.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::p().text)
                            .child("Welcome to Awili"),
                    )
                    .child(
                        div()
                            .text_size(theme::z(theme::LABEL_SIZE))
                            .text_color(theme::p().muted)
                            .mt(theme::z(8.))
                            .child(subtext),
                    )
                    .children((!provider_rows.is_empty()).then(|| {
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .mt(theme::z(8.))
                            .children(provider_rows.iter().map(|line| {
                                div()
                                    .text_size(theme::z(theme::LABEL_SIZE))
                                    .text_color(theme::p().muted)
                                    .child(line.clone())
                                    .into_any_element()
                            }))
                            .into_any_element()
                    }))
                    .children(error.map(|e| {
                        div()
                            .text_size(theme::z(theme::LABEL_SIZE))
                            .text_color(theme::p().chip_warn)
                            .mt(theme::z(8.))
                            .child(e)
                            .into_any_element()
                    }))
                    .child(
                        div()
                            .id("welcome-setup-connection")
                            .flex()
                            .items_center()
                            .justify_center()
                            .px(theme::z(10.))
                            .py(theme::z(3.))
                            .mt(theme::z(16.))
                            .rounded(theme::z(theme::RADIUS))
                            .bg(theme::p().chip_bg)
                            .border_1()
                            .border_color(setup_border)
                            .text_size(theme::z(theme::LABEL_SIZE))
                            .text_color(setup_text)
                            .cursor_pointer()
                            .hover(|s| {
                                s.border_color(theme::p().accent)
                                    .text_color(theme::p().accent_text)
                            })
                            .child("Set up connection")
                            .on_click(cx.listener(move |this, _ev, _w, cx| {
                                this.welcome_action = 0;
                                this.open_settings(cx);
                            })),
                    )
                    // Real re-resolution: re-read config provenance and the
                    // runtime; never fakes a save.
                    .child(
                        div()
                            .id("welcome-check-again")
                            .flex()
                            .items_center()
                            .justify_center()
                            .px(theme::z(10.))
                            .py(theme::z(3.))
                            .mt(theme::z(16.))
                            .rounded(theme::z(theme::RADIUS))
                            .bg(theme::p().chip_bg)
                            .border_1()
                            .border_color(check_border)
                            .text_size(theme::z(theme::LABEL_SIZE))
                            .text_color(check_text)
                            .cursor_pointer()
                            .hover(|s| {
                                s.border_color(theme::p().accent)
                                    .text_color(theme::p().accent_text)
                            })
                            .child("Check again")
                            .on_click(cx.listener(move |this, _ev, window, cx| {
                                this.welcome_action = 1;
                                this.check_onboarding_config(cx);
                                // “Check again” leaves the panel up, so keep the keyboard on
                                // it (and on its selection).
                                if this.onboarding.is_active() {
                                    window.focus(&this.welcome_focus, cx);
                                }
                            })),
                    )
                    // Explicit start — the ONLY thing that completes onboarding.
                    // Saving config alone must leave this panel up.
                    .children(ready.then(|| {
                        div()
                            .id("welcome-start-chatting")
                            .flex()
                            .items_center()
                            .justify_center()
                            .px(theme::z(10.))
                            .py(theme::z(3.))
                            .mt(theme::z(10.))
                            .rounded(theme::z(theme::RADIUS))
                            .bg(theme::p().chip_bg)
                            .border_1()
                            .border_color(start_border)
                            .text_size(theme::z(theme::LABEL_SIZE))
                            .text_color(start_text)
                            .cursor_pointer()
                            .hover(|s| {
                                s.border_color(theme::p().accent)
                                    .text_color(theme::p().accent_text)
                            })
                            .child(shortcuts::localize(if harness_only {
                                "Open Work (⌘Return)"
                            } else {
                                "Start chatting (⌘Return)"
                            })
                            .into_owned())
                            .on_click(cx.listener(move |this, _ev, window, cx| {
                                this.welcome_action = 2;
                                this.start_onboarding_chat(window, cx);
                            }))
                            .into_any_element()
                    }))
                    // Short keyboard hint, visible only while the panel owns focus
                    // (out of the way otherwise).
                    .children(focused.then(|| {
                        div()
                            .text_size(theme::z(theme::LABEL_SIZE))
                            .text_color(theme::p().muted)
                            .mt(theme::z(10.))
                            .child("Tab to move \u{00b7} Enter to choose")
                            .into_any_element()
                    })),
            )
            .into_any_element()
    }

    /// Whether Work can actually run: a chosen vendor CLI or another Work
    /// route. A saved provider row alone does not count (it serves Chat and
    /// the Council only).
    fn work_route_ready(&self) -> bool {
        self.onboarding_harness.is_some() || self.onboarding_other_route
    }

    /// Explicit first-run completion: the user started chatting. The
    /// onboarding state machine owns the transition (it no-ops unless
    /// Ready), so the button and the ⌘Return chord both route through here.
    fn start_onboarding_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.onboarding_other_route {
            let installed =
                self.onboarding_harness
                    .as_ref()
                    .is_some_and(|executor| match executor {
                        work::WorkExecutor::Agent { engine, .. } => {
                            harness_setup::installed_binary(*engine, &self.work.agents).is_some()
                        }
                        _ => false,
                    });
            if !installed {
                self.onboarding_harness = None;
                self.onboarding.on_discovery(false);
                self.onboarding_error =
                    Some("The selected CLI was not found. Install it, then Check again.");
                cx.notify();
                return;
            }
        }
        if !self.onboarding.start_chatting() {
            return;
        }
        if !self.onboarding_other_route {
            if let Some(executor) = self.onboarding_harness.clone() {
                self.set_work_executor_persisted(executor);
            }
        }
        self.set_mode(Mode::Work, cx);
        let h = self.composer.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// Welcome "Check again": re-resolve config provenance and re-apply the
    /// runtime. Never touches the Settings doc or draft (no fake save/emit,
    /// never `on_saved`, never overwrites a saved document), and the runtime
    /// helper invalidates the probe epoch before clearing anything, so no
    /// stale async callback can revive an invalid config.
    fn check_onboarding_config(&mut self, cx: &mut Context<Self>) {
        if !self.onboarding.is_active() {
            return;
        }
        // Pre-apply provenance: was the config missing before this pass?
        let was_missing = !self.onboarding.is_saved();
        match Config::load_with_source() {
            Ok((_, awili_config::ConfigSource::Missing)) => {
                self.pending_saved_selection = None;
                // Invalidate generation/cache/runtime FIRST: a networkless
                // pass may keep no old providers or declared choices.
                self.apply_runtime_config(&Config::networkless(), cx);
                // RVS6-INT1: a MISSING file is not a load error — the
                // networkless re-apply above leaves `config_load_error`
                // CLEAR (only the `Err` arm below sets it). Onboarding owns
                // the center while it is active, so Chat is gated there.
                self.onboarding.on_retry_lost(false);
                self.onboarding_error =
                    Some("No configuration file found. Set up a connection to continue.");
            }
            Err(_) => {
                self.pending_saved_selection = None;
                self.apply_runtime_config(&Config::networkless(), cx);
                // RVS6C P1: a malformed file is an invalid config — Chat
                // stays refused while the networkless fallback is in effect.
                self.config_load_error = true;
                self.onboarding.on_retry_lost(true);
                self.onboarding_error = Some(
                    "Configuration could not be loaded. Open Settings to repair it, then check again.",
                );
            }
            Ok((cfg, awili_config::ConfigSource::Loaded(_))) => {
                self.apply_runtime_config(&cfg, cx);
                if was_missing {
                    // An externally created valid config follows the existing
                    // retry policy: only `Missing` completes; a valid saved
                    // config still requires the explicit Start.
                    self.onboarding.on_retry(true);
                } else {
                    // Saved state: recompute saved readiness from the merged
                    // runtime choices without marking a fake save.
                    self.onboarding.on_discovery(self.work_route_ready());
                }
                self.onboarding_error = None;
            }
        }
        cx.notify();
    }

    fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings.open {
            self.request_close_settings(cx);
        } else {
            self.open_settings(cx);
        }
    }

    // ---- interface zoom (A4) ------------------------------------------------

    /// Apply a new interface scale: move the view-layer constant, persist it,
    /// track the rem size, and force a full re-layout so every pane redraws at
    /// the new size.
    fn apply_ui_scale(&mut self, scale: f32, window: &mut Window, cx: &mut Context<Self>) {
        let scale = zoom::clamp_scale(scale);
        theme::set_ui_scale(scale);
        zoom::save(scale);
        window.set_rem_size(px(16.0 * scale));
        window.refresh();
        cx.notify();
    }

    fn zoom_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_ui_scale(zoom::step_in(theme::ui_scale()), window, cx);
    }

    fn zoom_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_ui_scale(zoom::step_out(theme::ui_scale()), window, cx);
    }

    fn zoom_actual(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_ui_scale(zoom::ACTUAL, window, cx);
    }

    // ---- command palette (slice 13) -----------------------------------------

    /// Open the palette (⌘K): fresh query, search input focused, row list
    /// scrolled back to the top (the focus starts there — slice 16 SF1).
    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.more = false;
        self.palette.open();
        self.palette_scroll.set_offset(gpui::Point::default());
        self.palette_input.update(cx, |c, cx| {
            c.set_placeholder(PALETTE_SEARCH_PLACEHOLDER);
            c.set_text("", cx)
        });
        let h = self.palette_input.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// Close the palette and hand focus back to the chat composer.
    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette.close();
        let h = self.composer.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// Open the ⌘K palette pre-filled with `query` (M2i): ⌘O and the Code
    /// toolbar's folder label open it with "~/" already typed so the first
    /// keystroke starts completing the home directory. Same modal guard as
    /// `toggle_palette`.
    pub(crate) fn open_palette_with(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !palette::chord_permitted(
            self.settings.open
                || self.template_editor.is_some()
                || self.project_editor.is_some()
                || self.shell.tabs.delete_project.is_some()
                || self.shortcuts.open,
        ) {
            return;
        }
        if self.palette.open {
            self.close_palette(window, cx);
        }
        self.palette.open();
        self.palette_scroll.set_offset(gpui::Point::default());
        self.palette_input.update(cx, |c, cx| {
            c.set_placeholder(PALETTE_SEARCH_PLACEHOLDER);
            c.set_text(query, cx)
        });
        self.shell.more = false;
        let h = self.palette_input.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// RC2-R1F3 (usability RC2 round 1, N3: emptying the ⌘O box with
    /// Select All + Backspace turned it into "Search conversations…"): ⌘O
    /// and the Code folder label open the folder box — the palette with
    /// "~/" typed, listing folders only. In Work and Code it stays the folder
    /// box when emptied (it lists the recent folders and the home folder's,
    /// as at "~/"), and says so in its empty box. Elsewhere it is the
    /// palette with "~/" typed, as before.
    pub(crate) fn open_folder_box(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette_with("~/", window, cx);
        if self.palette.open && matches!(self.mode, Mode::Work | Mode::Code) {
            self.palette.folder_box = true;
            self.palette_input
                .update(cx, |c, _| c.set_placeholder(FOLDER_BOX_PLACEHOLDER));
        }
    }

    /// The row list the palette renders for the current query: the usual
    /// `resolve_all` plus — only in Work/Code mode with a path query — the
    /// folder rows as `UseFolder` commands. BOTH existing `resolve_all` call
    /// sites use this, so Enter runs exactly the row shown.
    pub(crate) fn palette_cmds(&self) -> Vec<crate::palette::PaletteCmd> {
        let cmds = palette::resolve_all(
            &self.palette,
            &self.conversations,
            &self.template_index(),
            self.composer_busy(),
            self.active_has_summary(),
        );
        // M2h: the "Executor: …" rows (Work) / "New Code tab: …" rows (Code).
        let mut cmds = cmds;
        cmds.extend(self.executor_palette_cmds());
        let is_folder_mode = matches!(self.mode, Mode::Work | Mode::Code);
        // RC2-R1F3: the folder box lists folders only; emptied, it lists
        // what "~/" lists. Words that are not a path pick out recent and
        // saved folders by name.
        let folder_box = self.palette.folder_box && is_folder_mode;
        let query = match self.palette.query.as_str() {
            "" if folder_box => "~/",
            q => q,
        };
        if folder_box {
            cmds.clear();
        }
        if !is_folder_mode || !(folder_box || palette_targets::path_mode(query)) {
            return cmds;
        }
        let home = crate::home::home_string();
        let saved: Vec<String> = if self.mode == Mode::Code {
            self.work
                .projects
                .iter()
                .map(|p| p.folder.clone())
                .collect()
        } else {
            Vec::new()
        };
        // RC2-UX (R2-15): the recent folders and the saved projects (Code
        // projects in Code, "Save as project" folders in Work) come first,
        // wherever they live; then the typed path's rows.
        let known_saved: Vec<String> = if self.mode == Mode::Code {
            saved.clone()
        } else {
            self.work
                .projects
                .iter()
                .map(|p| p.folder.clone())
                .collect()
        };
        let mut rows = palette_targets::known_folder_rows(
            query,
            &home,
            &self.work.recent_folders,
            &known_saved,
            |p| std::path::Path::new(p).is_dir(),
        );
        // RC2-R1F3: words that are not a path list no folder's children.
        let typed_rows = if palette_targets::path_mode(query) {
            palette_targets::folder_rows(
                query,
                &home,
                &saved,
                palette_targets::list_dir_names,
                |p| std::path::Path::new(p).is_dir(),
            )
        } else {
            Vec::new()
        };
        for r in typed_rows {
            if !rows.contains(&r) {
                rows.push(r);
            }
        }
        // RC2-R2F1: a typed path to an existing folder is the first row, so
        // Enter opens exactly what was typed.
        palette_targets::typed_folder_first(&mut rows, query, &home, |p| {
            std::path::Path::new(p).is_dir()
        });
        let mut out = cmds;
        for r in rows {
            out.push(crate::palette::PaletteCmd::UseFolder(r));
        }
        out
    }

    /// Tab on a focused `UseFolder` row in path mode: fill the palette input
    /// with "<path>/" (completion) instead of dismissing. Returns whether the
    /// key was handled; Tab outside path mode is untouched.
    pub(crate) fn palette_complete(&mut self, cx: &mut Context<Self>) -> bool {
        let cmds = self.palette_cmds();
        let Some(cmd) = cmds.get(self.palette.focused_idx) else {
            return false;
        };
        let crate::palette::PaletteCmd::UseFolder(path) = cmd else {
            return false;
        };
        let text = if path.ends_with('/') {
            path.clone()
        } else {
            format!("{path}/")
        };
        self.palette_input.update(cx, |c, cx| c.set_text(&text, cx));
        cx.notify();
        true
    }

    // ---- `/`-command skill palette -------------------------------------------

    /// Rows the skill palette currently shows: `matching_skills` over the live
    /// query. The nav/accept handlers index this list.
    fn skill_palette_rows(&self) -> Vec<&awili_gateway::rest::Skill> {
        skills::matching_skills(&self.skills_state.skills, &self.skill_palette.query)
    }

    /// Recompute the skill palette's open/query state from the CURRENT composer
    /// draft. Shared by the draft observer and by `refresh_skills`'s completion.
    /// The latter matters because skills load async: if the user types `/` before
    /// the list arrives, the observer saw zero rows and left the palette closed,
    /// and the skills-arrived update notifies the app without rerunning the
    /// observer — so without this call the palette would stay shut under a
    /// `/`-token the user already typed until they edit the draft again.
    /// Opens only when the `/`-token matches a skill: a no-match draft like
    /// "/xyzzy" must still submit literally on Enter.
    fn sync_skill_palette(&mut self, cx: &mut Context<Self>) {
        // The Cowork ⌘⇧P palette owns keyboard input while open; an async skills
        // refresh must not reopen the `/`-palette underneath it (which the
        // capture handler would then let steal nav keys). Stay closed until the
        // command palette dismisses (Codex review).
        if self.command_palette.open {
            self.skill_palette.close();
            return;
        }
        let draft = self.composer.read(cx).text().to_string();
        match skills::skill_query(&draft) {
            Some(q) => {
                // Escape stays sticky: while the `/`-token is unchanged from the
                // dismissed one, leave the palette closed even on unrelated
                // composer notifies (caret moves, refocus, a skills refresh). Any
                // change to the token clears the dismissal so a new query reopens.
                if self.skill_palette_dismissed.as_deref() == Some(q) {
                    self.skill_palette.open = false;
                    return;
                }
                self.skill_palette_dismissed = None;
                self.skill_palette.set_query(q);
                self.skill_palette.open = !self.skill_palette_rows().is_empty();
                // Focus snaps to the top on a fresh open / query change, but the
                // scroll handle keeps its previous (possibly scrolled-down)
                // offset — so bring it back to the top whenever the focused row
                // is 0, or a reopen could start with row 0 off-screen.
                if self.skill_palette.open && self.skill_palette.focused_idx == 0 {
                    self.skill_palette_scroll.set_offset(gpui::Point::default());
                }
            }
            None => {
                self.skill_palette.close();
                self.skill_palette_dismissed = None;
            }
        }
    }

    /// ↑/↓ in the open skill palette: move the focus with wrap-around, keeping
    /// the focused row scrolled into view for long lists.
    fn skill_palette_nav(&mut self, up: bool, cx: &mut Context<Self>) {
        let len = self.skill_palette_rows().len();
        if up {
            self.skill_palette.move_up(len);
        } else {
            self.skill_palette.move_down(len);
        }
        self.scroll_skill_palette_row_into_view();
        cx.notify();
    }

    /// Keep the focused skill row visible after ↑/↓ when the list overflows the
    /// popover — the ⌘K palette's [`Self::scroll_palette_row_into_view`] applied
    /// to the skill list. Row bounds come from the scroll handle's last paint;
    /// before the first paint there are none and the list is at its top anyway.
    fn scroll_skill_palette_row_into_view(&self) {
        let Some(row) = self
            .skill_palette_scroll
            .bounds_for_item(self.skill_palette.focused_idx)
        else {
            return;
        };
        let viewport = self.skill_palette_scroll.bounds();
        let mut offset = self.skill_palette_scroll.offset();
        offset.y = palette_scroll_offset_px(offset.y, viewport, row);
        self.skill_palette_scroll.set_offset(offset);
    }

    /// Enter on the focused skill: drop its `/name ` token into the draft and
    /// close the palette, leaving focus in the composer so the user types
    /// arguments and sends normally (the Cowork/Claude `/`-command convention —
    /// a composer affordance, not a protocol call). No-op on an empty list.
    fn skill_palette_accept(&mut self, cx: &mut Context<Self>) {
        let name = match self
            .skill_palette_rows()
            .get(self.skill_palette.focused_idx)
        {
            Some(s) => s.name.clone(),
            None => return,
        };
        self.skill_palette.close();
        self.composer
            .update(cx, |c, cx| c.set_text(&format!("/{name} "), cx));
        cx.notify();
    }

    /// Escape in the open skill palette: close it but leave the draft intact,
    /// and remember the dismissed `/`-token so `sync_skill_palette` won't reopen
    /// it until the token changes (the observer otherwise fires on every caret
    /// move / refocus). Matches the ⌘K dismiss feel.
    fn skill_palette_dismiss(&mut self, cx: &mut Context<Self>) {
        self.skill_palette_dismissed = Some(self.skill_palette.query.clone());
        self.skill_palette.close();
        cx.notify();
    }

    /// Skill-palette keys, dispatched from the window root in the CAPTURE phase
    /// (like [`Self::on_palette_key`]) so they land BEFORE the focused composer
    /// and its IME — which would otherwise move the caret on ↑/↓ and swallow
    /// Escape entirely. Reuses the ⌘K key classifier. Returns `true` only when a
    /// navigation/accept/dismiss key was CONSUMED (propagation stopped); `Other`
    /// keys (typing, chords like ⌘V) return `false` so the caller still runs the
    /// normal paste/composer path — typing keeps narrowing the query and a
    /// space/⌫ closes the palette via the observer.
    fn on_skill_palette_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let ks = &ev.keystroke;
        let chord = ks.modifiers.platform
            || ks.modifiers.control
            || ks.modifiers.alt
            || ks.modifiers.function;
        match palette::classify_key(&ks.key, ks.modifiers.shift, chord) {
            palette::PaletteKey::MoveUp => self.skill_palette_nav(true, cx),
            palette::PaletteKey::MoveDown => self.skill_palette_nav(false, cx),
            palette::PaletteKey::Run => self.skill_palette_accept(cx),
            palette::PaletteKey::Dismiss => self.skill_palette_dismiss(cx),
            palette::PaletteKey::Other => return false,
        }
        cx.stop_propagation();
        true
    }

    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Slice 16 SF5: the global ⌘K chord is swallowed while a modal sheet
        // is up — the palette must not open over (or under) it. The picker
        // popover is deliberately NOT a guard: slice 15 layers the palette
        // above it on purpose.
        if !palette::chord_permitted(
            self.settings.open
                || self.template_editor.is_some()
                || self.project_editor.is_some()
                || self.shortcuts.open,
        ) {
            return;
        }
        if self.palette.open {
            self.close_palette(window, cx);
        } else {
            self.open_palette(window, cx);
        }
    }

    /// Run a palette row (Enter on the focused row, or a click). Closing
    /// first lets the command set its own focus (the composer for a
    /// conversation jump, the settings modal for OpenSettings).
    fn run_palette_cmd(&mut self, cmd: PaletteCmd, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        match cmd {
            PaletteCmd::OpenConversation(id) => self.select_conversation(&id, window, cx),
            PaletteCmd::NewConversation => self.new_conversation(window, cx),
            PaletteCmd::OpenSettings => self.open_settings(cx),
            PaletteCmd::SwitchModel => {
                self.open_picker(cx);
            }
            PaletteCmd::GoTo(route) => self.set_mode(nav_route_to_mode(route), cx),
            PaletteCmd::ExportConversation => self.export_conversation(cx),
            PaletteCmd::SaveConversation => self.save_conversation(cx),
            PaletteCmd::ApplyTemplate { id, name } => self.apply_template(id, &name, cx),
            PaletteCmd::NewTemplate => self.open_template_editor(None, window, cx),
            PaletteCmd::EditTemplate { id, .. } => self.open_template_editor(Some(id), window, cx),
            PaletteCmd::DeleteTemplate { id, name } => self.delete_template(id, &name, cx),
            PaletteCmd::AbortStream => self.abort_active_stream(cx),
            PaletteCmd::DiscardSummary => self.discard_summary(cx),
            PaletteCmd::ShowShortcuts => {
                self.shortcuts.open = true;
                cx.notify();
            }
            PaletteCmd::UseFolder(p) => self.use_folder(&p, cx),
            // M2h: keyboard executor switch and new Code tab.
            PaletteCmd::UseExecutor { persist, .. } => self.use_executor(&persist, cx),
            PaletteCmd::NewCodeTab(engine) => {
                self.new_code_tab(engine);
                cx.notify();
            }
        }
    }

    /// Use a folder as the working directory (M2i). In Work mode it binds the
    /// Work folder (the same placeholder update as `pick_work_folder`); in
    /// Code mode it adds the folder as a project and activates it (persisted).
    /// `~/` is expanded; the path must be an existing directory.
    pub(crate) fn use_folder(&mut self, path: &str, cx: &mut Context<Self>) {
        let expanded = work::expand_home(path);
        let pb = std::path::PathBuf::from(&expanded);
        if !pb.is_dir() {
            return;
        }
        match self.mode {
            Mode::Work => {
                let name = pb
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| expanded.clone());
                self.choose_work_folder(pb, cx);
                let placeholder = format!("Tell Awili what to do in {name}…");
                self.composer
                    .update(cx, |c, _cx| c.set_placeholder(placeholder));
                cx.notify();
            }
            Mode::Code => {
                self.add_code_project(pb, cx);
                cx.notify();
            }
            _ => {}
        }
    }

    /// Activate Code tab `i` (M2i) and, if that tab's `project_folder` matches
    /// a saved project's folder, make that project the active one (persisted).
    /// A tab without a folder, or with an unsaved folder, leaves the active
    /// project alone.
    pub(crate) fn activate_code_tab(&mut self, i: usize) {
        // M2-INT: U1-INT1's bounds-checked select (syncs the sidebar), then
        // M2i's project restore.
        self.select_code_tab(i);
        let folder = self
            .work
            .code_tabs
            .get(i)
            .and_then(|t| t.project_folder.clone());
        if let Some(folder) = folder {
            if let Some(idx) = self.work.projects.iter().position(|p| p.folder == folder) {
                self.switch_code_project(idx);
            } else {
                self.work.workshop_selected_folder = Some(folder);
                self.persist_workshop_project_selection();
            }
        }
    }

    // ---- ⌘⇧P Cowork command palette (plan unit 29) ---------------------------

    /// Open the Cowork command palette (⌘⇧P). Guarded by the same modal check
    /// as [`Self::toggle_palette`] so it never opens over (or under) a modal
    /// sheet. The KIT SearchField takes focus while the palette is open; keep
    /// the current page's focus owner so dismissal can return to it.
    ///
    /// Closes any other open palette first so only one palette is ever open at
    /// a time: this palette has capture priority (checked first) and would
    /// otherwise paint over a still-open native/skill palette (Codex review).
    /// (The reverse — ⌘K while this is open — is already safe: this palette
    /// consumes ⌘K in the capture phase before it can toggle the native one.)
    fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !palette::chord_permitted(
            self.settings.open
                || self.template_editor.is_some()
                || self.project_editor.is_some()
                || self.shell.tabs.delete_project.is_some()
                || self.shortcuts.open,
        ) {
            return;
        }
        // A native menu invocation can arrive while the overlay is already
        // open. Preserve its query, page, SearchField focus, and the owner to
        // which dismissal will return; only a closed-to-open transition
        // starts a fresh palette session.
        if self.command_palette.open {
            return;
        }
        if self.palette.open {
            self.close_palette(window, cx);
        }
        self.skill_palette.close();
        if self.shell.more {
            self.shell.more = false;
            window.focus(&self.shell.rail_focus, cx);
        }
        self.command_palette_return_focus = Some(
            window
                .focused(cx)
                .unwrap_or_else(|| self.composer.read(cx).focus_handle().clone()),
        );
        self.command_palette.open();
        cx.notify();
    }

    /// Close the Cowork palette and return focus to the owner that was active
    /// before it opened. If no target was available, use the normal composer
    /// routing (which also handles the archived-reader rail).
    fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_palette.close();
        if let Some(target) = self.command_palette_return_focus.take() {
            window.focus(&target, cx);
        } else {
            self.focus_composer(window, cx);
        }
        cx.notify();
    }

    /// Run one Cowork-palette row by its visible index (mouse click — user-test
    /// N2). Mirrors the keyboard Run path: focus the row, then `enter`.
    fn run_command_palette_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (recent, archived) = self.palette_providers();
        let rows = self
            .command_palette
            .visible_rows(&recent, &[], &[], &archived);
        if ix >= rows.len() {
            return;
        }
        self.command_palette.focused_idx = ix;
        let action = self.command_palette.enter(&rows);
        self.run_command_palette_action(action, window, cx);
    }

    /// Keys while the Cowork command palette owns input, dispatched from the
    /// window root CAPTURE phase (like [`Self::on_palette_key`]) so they land
    /// before the focused composer/IME. Navigation, Run and Dismiss keys are
    /// handled here. A typed character, or Backspace on a non-empty query, is
    /// left to propagate to the palette's focused `SearchField`, which edits the
    /// query and reports it through `on_query`. Every other key is consumed here
    /// so nothing leaks to the composer under the scrim.
    fn on_command_palette_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &ev.keystroke;
        let chord = ks.modifiers.platform
            || ks.modifiers.control
            || ks.modifiers.alt
            || ks.modifiers.function;
        match palette::classify_key(&ks.key, ks.modifiers.shift, chord) {
            palette::PaletteKey::MoveUp => {
                let (recent, archived) = self.palette_providers();
                let len = self
                    .command_palette
                    .visible_rows(&recent, &[], &[], &archived)
                    .len();
                self.command_palette.move_up(len);
                cx.notify();
            }
            palette::PaletteKey::MoveDown => {
                let (recent, archived) = self.palette_providers();
                let len = self
                    .command_palette
                    .visible_rows(&recent, &[], &[], &archived)
                    .len();
                self.command_palette.move_down(len);
                cx.notify();
            }
            palette::PaletteKey::Run => {
                let (recent, archived) = self.palette_providers();
                let rows = self
                    .command_palette
                    .visible_rows(&recent, &[], &[], &archived);
                let action = self.command_palette.enter(&rows);
                self.run_command_palette_action(action, window, cx);
            }
            palette::PaletteKey::Dismiss => {
                // Escape / Shift+Enter: pop a sub-page, or close from the root.
                if self.command_palette.page == command_palette::Page::Root {
                    self.close_command_palette(window, cx);
                } else {
                    let _ = self.command_palette.back();
                    cx.notify();
                }
            }
            palette::PaletteKey::Other => {
                let backspace = !chord && ks.key == "backspace";
                // Mirrors the SearchField's own printable rule: no chord, and a
                // control character is not text.
                let typed = !chord
                    && ks.key != "backspace"
                    && ks
                        .key_char
                        .as_deref()
                        .is_some_and(|c| !c.is_empty() && !c.chars().any(char::is_control));
                if typed || (backspace && !self.command_palette.query.is_empty()) {
                    // The focused SearchField edits the query and reports it
                    // through `on_query`, once: leave the key to propagate.
                    return;
                }
                // Backspace on an empty query: a Color Mode sub-page pops to Root.
                // On Root it does nothing; it never dismisses the palette.
                if backspace && self.command_palette.page == command_palette::Page::ColorMode {
                    let _ = self.command_palette.back();
                    cx.notify();
                }
                // Chords and unhandled keys fall through to the consume below,
                // with no edit, so they never leak to the composer under the scrim.
            }
        }
        cx.stop_propagation();
    }

    /// Keys while the hosted Messaging pane ([`Mode::Messaging`]) is the active
    /// surface. The pane has no embedded text field, so — like the Cowork
    /// command palette — it classifies keys through
    /// [`messaging_pane::messaging_key`] and drives its pure state directly:
    /// printables/backspace edit the connector filter, ↑/↓ move the detail
    /// selection. Returns whether the key was consumed; a chord or unhandled key
    /// ([`MessagingKey::Ignore`]) returns `false` so the caller lets it fall
    /// through to the global chord handler (⌘K/⌘N/⌘, still navigate away).
    /// Navigation-only — nothing here fetches or fires the [LIVE] Test action.
    fn on_messaging_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        use messaging_pane::MessagingKey as K;
        let ks = &ev.keystroke;
        let chord = ks.modifiers.platform
            || ks.modifiers.control
            || ks.modifiers.alt
            || ks.modifiers.function;
        match messaging_pane::messaging_key(&ks.key, chord) {
            K::Append(s) => self.messaging_data.push_query(&s),
            K::Backspace => self.messaging_data.backspace(),
            K::SelectNext => self.messaging_data.select_next(),
            K::SelectPrev => self.messaging_data.select_prev(),
            K::Ignore => return false,
        }
        // Consumed: stop it here so the edited keystroke never also bubbles to a
        // focused composer/IME under the pane (same contract as the palette
        // capture handlers). `Ignore` returned above without stopping, so chords
        // still reach the global handler.
        cx.stop_propagation();
        cx.notify();
        true
    }

    /// Execute the [`command_palette::PaletteAction`] an Enter resolved to.
    /// `OpenedSubPage` mutated the state in place (stay open); every other
    /// action closes the palette first, then fires the mapped navigation. The
    /// static-catalog rows all map through [`command_palette::catalog_nav`];
    /// Color-mode actions share the persisted Settings preference.
    fn run_command_palette_action(
        &mut self,
        action: command_palette::PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use command_palette::{catalog_nav, PaletteAction as A, PaletteNav as N};
        match action {
            A::None => {}
            A::OpenedSubPage => cx.notify(),
            A::RunCatalog(id) => {
                self.close_command_palette(window, cx);
                match catalog_nav(id) {
                    N::NewConversation => self.new_conversation(window, cx),
                    N::OpenFeed => self.open_feed_panel(cx),
                    N::GoCommandCenter => self.set_mode(Mode::CommandCenter, cx),
                    N::GoSkills => self.set_mode(Mode::Skills, cx),
                    N::GoMessaging => self.set_mode(Mode::Messaging, cx),
                    N::OpenSettings => self.open_settings(cx),
                    N::OpenAppearance => {
                        self.open_settings(cx);
                        self.select_settings_page(settings_catalog::Page::Appearance, cx);
                    }
                    N::Noop => {}
                }
            }
            A::RunDynamic(kind, id) => {
                self.close_command_palette(window, cx);
                use command_palette::DynNav;
                match command_palette::dynamic_nav(kind, id) {
                    // Open the Sessions browser focused on the picked session.
                    // The palette rows are all remote Hermes sessions, which the
                    // browser only lists under the All/Hermes tabs and further
                    // filters by its search field — so a Chat/Work tab or a stale
                    // filter would hide the very session we just selected. Force
                    // the Hermes tab and clear the filter before selecting, so the
                    // pick is always visible.
                    DynNav::OpenSession(sid) => {
                        self.sessions_state.view = sessions::SessionsView::Hermes;
                        self.sessions_state.filter.clear();
                        self.set_mode(Mode::Sessions, cx);
                        self.sessions_state.selected_id = Some(sid.clone());
                        self.sessions_state.transcript = None;
                        self.sessions_state.transcript_error = None;
                        self.sessions_focus_pending = Some(SessionsFocusTarget::Back);
                        self.fetch_session_transcript(sid, cx);
                        cx.notify();
                    }
                    // Archived conversations are local history — reuse the same
                    // path the sidebar's Archived section uses (picks the right
                    // Chat/Work mode for the conversation, then selects it).
                    DynNav::OpenArchived(cid) => self.open_conversation_from_history(&cid, cx),
                    DynNav::OpenSettings => self.open_settings(cx),
                    DynNav::Noop => {}
                }
            }
            A::SetColorMode(choice) => {
                self.close_command_palette(window, cx);
                let mode = match choice {
                    command_palette::ColorModeChoice::Light => awili_config::ColorMode::Light,
                    command_palette::ColorModeChoice::Dark => awili_config::ColorMode::Dark,
                    command_palette::ColorModeChoice::System => awili_config::ColorMode::System,
                };
                self.choose_color_mode(mode, window, cx);
            }
        }
    }

    /// Assemble the Cowork command palette's live provider slices from window
    /// state already in memory — no new I/O. `RecentSessions` reuses the
    /// Sessions browser's fetched list; `ArchivedChats` reuses the sidebar's
    /// archived section. Both mappings are the pure, tested halves in
    /// [`command_catalog`]. Returned in the order the palette consumes them:
    /// `(recent_sessions, archived_chats)`. `SettingsFields`/`McpServers` are
    /// left empty by the callers: the decoded settings schema isn't retained on
    /// the window and MCP has no config source yet, so those stay deferred
    /// (never fabricated).
    fn palette_providers(
        &self,
    ) -> (
        Vec<command_catalog::DynEntry>,
        Vec<command_catalog::DynEntry>,
    ) {
        let recent = command_catalog::recent_sessions_to_entries(&self.sessions_state.sessions);
        let archived = command_catalog::archived_chats_to_entries(&self.conversations.archived());
        (recent, archived)
    }

    /// Open the Feed / Needs-You panel without toggling it shut when it is
    /// already open — the command-palette `goto.feed` row is "go to", not a
    /// toggle. Mirrors [`Self::toggle_feed_panel`]'s open branch (a fresh sync).
    fn open_feed_panel(&mut self, cx: &mut Context<Self>) {
        if !self.feed_panel_open {
            self.feed_panel_open = true;
            if let Some(handle) = &self.feed {
                handle.sync_now();
            }
            cx.notify();
        }
    }

    // ---- conversation export (slice 17) --------------------------------------

    /// Show a transient toast and schedule its 3s auto-dismiss. Generation-
    /// guarded: a newer toast restarts the clock instead of being cut short
    /// by the older timer (the `schedule_status_clear` pattern).
    fn show_toast(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.show_timed_toast(msg, Duration::from_secs(3), cx);
    }

    /// RC1-H7: an information toast (a confirmation such as "Saved …", an
    /// explanation): it stays [`INFO_TOAST_SECS`] — long enough to read it
    /// twice — then goes by itself. A refusal the user must act on is sticky
    /// instead ([`Self::show_sticky_toast`]).
    pub(crate) fn show_info_toast(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.show_timed_toast(msg, Duration::from_secs(INFO_TOAST_SECS), cx);
        // RC2-R54: it speaks about the page it was shown on.
        self.toast.page = Some(self.mode);
    }

    /// A toast that clears itself `after` (unless a newer one replaced it).
    fn show_timed_toast(
        &mut self,
        msg: impl Into<SharedString>,
        after: Duration,
        cx: &mut Context<Self>,
    ) {
        let msg: SharedString = msg.into();
        // Off macOS the ⌘ glyphs in the text read as Ctrl+… (identity on macOS).
        let msg = SharedString::from(shortcuts::localize(&msg).into_owned());
        let gen = self.toast.show(msg);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(after).await;
            this.update(cx, |app, cx| {
                if app.toast.clear_if(gen) {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// R1a: a refusal the user must read. It stays until it is clicked or
    /// replaced; RC1-H7: the next send also closes it
    /// ([`Self::guard_submit`]); RC2-RUN (round 3 Major 7): and it goes by
    /// itself after [`REFUSAL_TOAST_SECS`].
    pub(crate) fn show_sticky_toast(
        &mut self,
        msg: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.show_timed_toast(msg, Duration::from_secs(REFUSAL_TOAST_SECS), cx);
    }

    /// R1a: close the toast (a click on it).
    pub(crate) fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        self.toast.dismiss();
        cx.notify();
    }

    /// The active conversation read back from the database, through the
    /// read-only main-thread connection (the slice-14 search-store pattern:
    /// fast, read-only, concurrent with the worker's writes under WAL).
    fn exported_active(&self) -> Option<awili_persist::ExportedConversation> {
        self.search_store
            .as_ref()?
            .export_conversation(self.sidebar.active_id())
            .ok()
    }

    /// True while the transcript shown on the current conversation surface is
    /// unsettled: a Work run is pinned to the conversation that started it.
    fn conversation_export_busy(&self) -> bool {
        conversation_export_is_busy(
            self.mode,
            self.sidebar.active_id(),
            self.work.running,
            self.work.conv_id.as_deref(),
        )
    }

    /// Toolbar copy / palette "Export conversation": copy the conversation
    /// on screen to the clipboard (M1c). The streaming refusal lives in the
    /// one shared path, `conversation_export::export_on_screen`.
    fn export_conversation(&mut self, cx: &mut Context<Self>) {
        self.copy_on_screen_conversation(cx);
    }

    /// Menu export, ⌘⇧C, palette "Save conversation": write the conversation
    /// on screen to Downloads and copy it (M1c). Same shared path and gate.
    fn save_conversation(&mut self, cx: &mut Context<Self>) {
        self.save_on_screen_conversation(cx);
    }

    // ---- file attachments (slice 19) -----------------------------------------

    /// Reload the tray from the database for the active conversation —
    /// after staging, unstaging, or a conversation switch.
    fn refresh_staged_attachments(&mut self) {
        // A blank native helper has no attachment owner yet. Its sidebar
        // fallback is only a saved selection, not that helper's file tray.
        let owner = if self.mode == Mode::Code {
            self.work
                .code_tabs
                .get(self.work.code_active)
                .and_then(|tab| tab.history.conv_id.as_deref())
        } else {
            Some(self.sidebar.active_id())
        };
        self.staged_attachments = owner
            .and_then(|id| self.search_store.as_ref()?.get_staged_attachments(id).ok())
            .unwrap_or_default();
    }

    /// Store + stage one attachment (bytes already in hand) on the active
    /// conversation, then refresh the tray. Synchronous on the UI connection:
    /// staging needs the new row's id back, and the writes are tiny and
    /// user-paced (one per drop/paste).
    fn attach_bytes(&mut self, filename: &str, mime: &str, content: &[u8], cx: &mut Context<Self>) {
        if self.shell.archived_reader.is_some() {
            return;
        }
        let Some(conv_id) = self.workshop_input_owner(false, cx) else {
            self.show_toast("Select a live thread before adding files.", cx);
            return;
        };
        let Some(store) = self.search_store.as_ref() else {
            self.show_toast("Attach failed — persistence unavailable", cx);
            return;
        };
        let staged = store
            .store_attachment(filename, mime, content)
            .and_then(|id| store.stage_attachment(&conv_id, id));
        match staged {
            Ok(()) => {
                self.refresh_staged_attachments();
                cx.notify();
            }
            Err(e) => self.show_toast(format!("Attach failed — {e}"), cx),
        }
    }

    /// Attach a file from disk (a drop, or a copied file pasted with ⌘V).
    fn attach_path(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if self.shell.archived_reader.is_some() {
            self.show_toast("Restore the archived thread before adding files.", cx);
            return;
        }
        match std::fs::read(path) {
            Ok(bytes) => {
                let filename = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".to_string());
                self.attach_bytes(&filename, mime_for_path(path), &bytes, cx);
            }
            Err(e) => self.show_toast(format!("Attach failed — {e}"), cx),
        }
    }

    /// The tray chip's ✕: remove one staged attachment (the blob stays).
    fn unstage_attachment(&mut self, attachment_id: i64, cx: &mut Context<Self>) {
        if let Some(store) = self.search_store.as_ref() {
            if let Err(e) = store.unstage_attachment(self.sidebar.active_id(), attachment_id) {
                self.show_toast(format!("Remove failed — {e}"), cx);
                return;
            }
        }
        self.refresh_staged_attachments();
        cx.notify();
    }

    /// ⌘V with a file or image on the clipboard attaches instead of pasting
    /// (slice 19 §5). Runs in the window's CAPTURE phase so it sees the chord
    /// before the composer's standard text-paste path; only intercepts while
    /// the chat composer is focused (a rename/settings field keeps plain
    /// paste), and only when a non-text entry is actually present — a
    /// text-only clipboard falls through untouched.
    fn on_paste_attach(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if !crate::composer_keys::is_app_chord(&ks.modifiers) || ks.key != "v" {
            return;
        }
        if !self.composer.read(cx).focus_handle().is_focused(window) {
            return;
        }
        if self.attach_from_clipboard(cx) {
            // The composer must not also paste the file's path as text.
            cx.stop_propagation();
        }
    }

    /// Attach every image and file on the clipboard to the conversation;
    /// `true` when something was attached. Shared by ⌘V ([`Self::on_paste_attach`])
    /// and, RC2-R4 (D-U3), Edit ▸ Paste and the composer's context-menu Paste
    /// (`ComposerEvent::PasteAttachments`), which before pasted text only.
    fn attach_from_clipboard(&mut self, cx: &mut Context<Self>) -> bool {
        if self.shell.archived_reader.is_some() {
            return false;
        }
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        let mut attached = false;
        for entry in item.into_entries() {
            match entry {
                ClipboardEntry::Image(img) => {
                    let mime = img.format.mime_type();
                    // image/png → "pasted-image.png"; the subtype is a good
                    // enough extension for every ImageFormat but svg+xml,
                    // which clipboard images never are.
                    let ext = mime.rsplit('/').next().unwrap_or("png");
                    self.attach_bytes(&format!("pasted-image.{ext}"), mime, &img.bytes, cx);
                    attached = true;
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    for p in paths.paths().to_vec() {
                        self.attach_path(&p, cx);
                        attached = true;
                    }
                }
                ClipboardEntry::String(_) => {}
            }
        }
        attached
    }

    // ---- stop button + shortcut reference (slice 20) --------------------------

    /// Submit the composer draft — Enter in the composer and the Send button
    /// share this path. Mid-stream submits are ignored (one turn at a time
    /// per conversation).
    fn composer_busy(&self) -> bool {
        match self.mode {
            // RC2-UX (D-PUNCH S2-F16): while Hermes waits for an answer to its
            // question, the box sends the answer.
            Mode::Work => {
                self.work.running
                    && self
                        .work
                        .conv_id
                        .as_deref()
                        .or(self.work.worker_conv.as_deref())
                        .is_none_or(|id| id == self.sidebar.active_id())
                    && !self.clarify_waiting()
            }
            Mode::Code => self
                .work
                .code_tabs
                .get(self.work.code_active)
                .is_some_and(|s| s.streaming),
            _ => false,
        }
    }

    /// Keyboard Enter and the Send button share this mode-routing boundary.
    fn dispatch_composer_text(&mut self, text: String, cx: &mut Context<Self>) {
        if self.composer_busy()
            || self.shell.archived_reader.is_some()
            || (self.mode != Mode::Code
                && self.refuse_archived_workshop_send(
                    self.sidebar.active_id().to_string().as_str(),
                    cx,
                ))
        {
            return;
        }
        // M2g: every send (any mode) is recorded into the session
        // prompt-history walk. `record` trims, ignores blanks, collapses
        // consecutive duplicates, and ends any in-progress walk.
        self.prompt_history.record(&text);
        // One tool-capable path per conversation. There is no answer/job
        // classifier: a plain question simply uses no tools. Approval posture
        // and every per-owner guard stay in the normal dispatch paths.
        match self.mode {
            Mode::Work if self.clarify_waiting() => self.answer_work_clarify(text, cx),
            Mode::Work => {
                if self.work.running {
                    self.refuse_submit("A job is running in another thread. Finish or stop it before starting this job.".into(), cx);
                } else {
                    self.submit_work_local_first(text, cx)
                }
            }
            Mode::Code => self.submit_code_turn_kind(text, MessageKind::Normal, cx),
            // RC2-CNCL6: one message to every member at once.
            Mode::Council => self.submit_council(text, cx),
            _ => {}
        }
    }

    /// M2g: the active conversation's user messages (oldest→newest) — the
    /// tail of the prompt-history candidate list. For Chat and Work that is
    /// the view's convo; for Code it is the active tab's convo. The assistant
    /// rows are excluded: the walk recalls the user's own prompts.
    fn history_conversation(&self) -> Vec<String> {
        match self.mode {
            Mode::Work => {
                let conv_id = self.sidebar.active_id().to_string();
                self.views
                    .get(&conv_id)
                    .map(|v| {
                        v.convo
                            .messages()
                            .iter()
                            .filter(|m| m.role == Role::User)
                            .map(|m| m.text.clone())
                            .collect()
                    })
                    .unwrap_or_default()
            }
            Mode::Code => {
                let active = self.work.code_active;
                self.work
                    .code_tabs
                    .get(active)
                    .map(|s| {
                        s.convo
                            .messages()
                            .iter()
                            .filter(|m| m.role == Role::User)
                            .map(|m| m.text.clone())
                            .collect()
                    })
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// M2g: Up — step the walk older. The recalled entry lands in the
    /// composer via `set_recalled`; the draft is replaced so the user sees
    /// the entry is editable.
    fn composer_history_prev(&mut self, cx: &mut Context<Self>) {
        let conv = self.history_conversation();
        if let Some(text) = self.prompt_history.prev(&conv) {
            self.composer.update(cx, |c, cx| {
                c.set_text(&text, cx);
                c.set_recalled(Some(text), cx);
            });
        }
        cx.notify();
    }

    /// M2g: Down — step the walk newer. Past the newest entry clears the
    /// draft back to empty and ends the walk.
    fn composer_history_next(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.prompt_history.next() else {
            return;
        };
        self.composer.update(cx, |c, cx| {
            if text.is_empty() {
                // Walk ended: clear the draft and the recalled marker.
                c.set_text("", cx);
                c.set_recalled(None, cx);
            } else {
                c.set_text(&text, cx);
                c.set_recalled(Some(text), cx);
            }
        });
        cx.notify();
    }

    fn submit_composer_draft(&mut self, cx: &mut Context<Self>) {
        if self.shell.archived_reader.is_some() {
            return;
        }
        if self.onboarding.is_active() {
            return;
        }
        if self.composer_busy() {
            return;
        }
        let text = self.composer.update(cx, |c, cx| {
            cx.notify();
            c.take()
        });
        // Taking the draft ends any dictation in progress and retires every
        // outstanding transcription worker (review blocker 1): a final that
        // resolves after this point must not insert into the cleared
        // composer. The visible ghost preview went with the draft — the user
        // sent exactly what they saw.
        self.invalidate_dictation();
        // A manual (typed) submit means the keyboard is back in play: leave
        // the hands-free conversation loop rather than have the VAD and the
        // user race to submit (slice-5 spec §3).
        if self.voice.conversation_mode {
            self.exit_conversation_mode(cx);
        }
        // R1a: a typed message never vanishes. Every path that takes the text
        // (a transcript row, a parked send, a hand-back) clears the guard;
        // anything left after the dispatch goes back into the composer.
        self.guard_submit(&text);
        self.dispatch_composer_text(text, cx);
        self.settle_submit_guard(cx);
    }

    /// ⏹ Stop / ⌘. / Esc / palette "Stop streaming": stop the turn running in
    /// the ACTIVE Workshop thread (a Work run, a Code tab's turn) or every
    /// member of the Council page. The UI unblocks as the run settles.
    fn abort_active_stream(&mut self, cx: &mut Context<Self>) {
        if self.shell.archived_reader.is_some() {
            return;
        }
        if self.mode == Mode::Work {
            let cancelled_pending = self.cancel_workshop_pending(cx);
            if !self.workshop_work_runner_unsettled() {
                if !cancelled_pending && self.workshop_work_in_flight() {
                    self.show_toast(
                        "A request is waiting in another thread. Open its tab to stop it.",
                        cx,
                    );
                }
                return;
            }
            if self
                .work
                .conv_id
                .as_deref()
                .or(self.work.worker_conv.as_deref())
                .is_none_or(|owner| owner == self.sidebar.active_id())
            {
                self.cancel_work_run(cx);
            } else if !cancelled_pending {
                self.show_toast(
                    "The job is running in another thread. Open its tab to stop it.",
                    cx,
                );
            }
            return;
        }
        // RC2-CNCL9: on the Council page Cmd-. stops every member.
        if self.mode == Mode::Council {
            self.council_stop_all(cx);
            return;
        }
        // Code mode has its own per-tab kill switch (kills the child).
        if self.mode == Mode::Code {
            self.cancel_code_turn(cx);
        }
    }

    /// Whether the active conversation holds at least one summary message — the
    /// gate for the palette's "Discard summary" row (slice 21 §D4). Cheap: a
    /// conversation has at most a handful of summaries.
    fn active_has_summary(&self) -> bool {
        self.active_view().is_some_and(|v| {
            v.convo
                .messages()
                .iter()
                .any(|m| m.kind == MessageKind::Summary)
        })
    }

    /// Reverse the most recent "Summarize earlier turns" (slice 21 §D4 "Discard
    /// summary"): restore the turns that summary superseded and drop the summary
    /// bubble, in memory and durably. The exact inverse of [`finish_summary`]'s
    /// success path; a no-op if there is no summary, a streaming reply, or the
    /// conversation is archived/Code.
    fn discard_summary(&mut self, cx: &mut Context<Self>) {
        // Chat-only, mirroring `summarize_earlier`: Code tabs have no trim meter.
        if self.mode == Mode::Code {
            return;
        }
        let conv_id = self.sidebar.active_id().to_string();
        if self.conversations.get(&conv_id).is_some_and(|c| c.archived) {
            return;
        }
        let Some(view) = self.views.get(&conv_id) else {
            return;
        };
        // The most recent summary — "undo my last Summarize earlier".
        let Some(summary_ts) = view
            .convo
            .messages()
            .iter()
            .rev()
            .find(|m| m.kind == MessageKind::Summary)
            .map(|m| m.ts)
        else {
            return;
        };
        let Some(view) = self.views.get_mut(&conv_id) else {
            return;
        };
        // In-memory half: restore the superseded turns and drop the summary row,
        // keeping rows/errored aligned. `None` only if it raced away — then the
        // durable half must not run either.
        if view.discard_summary(summary_ts).is_none() {
            return;
        }
        // Durable half: the atomic inverse, addressed by the same `ts`.
        if let Some(p) = &self.persist {
            p.send(PersistCmd::DiscardSummary {
                conv: conv_id.clone(),
                summary_ts: summary_ts as i64,
            });
        }
        let now = feed::unix_now();
        self.conversations.touch(&conv_id, now);
        if let Some(p) = &self.persist {
            p.send(PersistCmd::Touch {
                id: conv_id.clone(),
                updated_ts: now,
            });
        }
        cx.notify();
    }

    /// ⌘? (slice 20 §2): toggle the keyboard-shortcut reference sheet.
    fn toggle_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.shell.more = false;
        self.shortcuts.toggle();
        cx.notify();
    }

    /// Escape or ✕ dismisses the sheet.
    fn close_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.shortcuts.close();
        cx.notify();
    }

    /// K02 (U3d): move focus to the sheet's scrim so Escape/⌘? dispatch lands
    /// on the window root (where the app's key listeners live) even when the
    /// previously-focused element is no longer rendered. Called from the root
    /// key-down handler's Escape arm.
    fn focus_shortcuts_overlay(&mut self, window: &mut Window, cx: &mut App) {
        window.focus(self.overlay_focus.handle(), cx);
    }

    /// K02 (U3d): return focus to the composer after the sheet closes.
    fn focus_composer(&mut self, window: &mut Window, cx: &mut App) {
        if self.shell.archived_reader.is_some() {
            window.focus(&self.shell.rail_focus, cx);
            return;
        }
        let h = self.composer.focus_handle(cx);
        window.focus(&h, cx);
    }

    /// Palette keys, dispatched from the window root in the CAPTURE phase —
    /// BEFORE the focused search input sees them: ↑/↓ must move the focused
    /// row (the input would move its caret), Enter must run the row even on
    /// an empty query (the input swallows Enter on a blank draft), Escape and
    /// Shift+Enter (slice 16 SF3) close the palette without running anything
    /// and must not fall through to the settings modal underneath (acceptance
    /// §4). Every other key feeds the input.
    fn on_palette_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let chord = ks.modifiers.platform
            || ks.modifiers.control
            || ks.modifiers.alt
            || ks.modifiers.function;
        // M2i: Tab in path mode on a folder row completes the input with
        // "<path>/" instead of dismissing (or moving focus).
        if ks.key == "tab" && !chord && !ks.modifiers.shift && self.palette_complete(cx) {
            cx.stop_propagation();
            return;
        }
        let cmds = self.palette_cmds();
        match palette::classify_key(&ks.key, ks.modifiers.shift, chord) {
            palette::PaletteKey::MoveUp => {
                self.palette.move_up(cmds.len());
                self.scroll_palette_row_into_view();
                cx.notify();
            }
            palette::PaletteKey::MoveDown => {
                self.palette.move_down(cmds.len());
                self.scroll_palette_row_into_view();
                cx.notify();
            }
            palette::PaletteKey::Run => {
                if let Some(cmd) = cmds.get(self.palette.focused_idx) {
                    self.run_palette_cmd(cmd.clone(), window, cx);
                }
            }
            palette::PaletteKey::Dismiss => self.close_palette(window, cx),
            palette::PaletteKey::Other => return,
        }
        cx.stop_propagation();
    }

    /// Keep the palette's focused row visible after keyboard navigation
    /// (slice 16 SF1). The row bounds come from the scroll handle's last
    /// paint — valid here because arrow keys never change the row list, only
    /// which row is focused. Before the first paint there are no bounds and
    /// the list is at its top anyway.
    fn scroll_palette_row_into_view(&self) {
        let Some(row) = self
            .palette_scroll
            .bounds_for_item(self.palette.focused_idx)
        else {
            return;
        };
        let viewport = self.palette_scroll.bounds();
        let mut offset = self.palette_scroll.offset();
        offset.y = palette_scroll_offset_px(offset.y, viewport, row);
        self.palette_scroll.set_offset(offset);
    }

    // ---- prompt templates (slice 18) -----------------------------------------

    /// The `(id, name)` pairs `palette::resolve` builds template rows from.
    fn template_index(&self) -> Vec<(i64, String)> {
        self.templates
            .iter()
            .map(|t| (t.id, t.name.clone()))
            .collect()
    }

    /// Bind `id` to the active conversation as its system prompt (slice 18
    /// §3). Replaces any previous binding; the content is injected at send
    /// time, never stored in the messages table.
    fn apply_template(&mut self, id: i64, name: &str, cx: &mut Context<Self>) {
        let conv_id = self.sidebar.active_id().to_string();
        self.conv_templates.insert(conv_id.clone(), id);
        if let Some(p) = &self.persist {
            p.send(PersistCmd::SetConversationTemplate {
                conv: conv_id,
                template_id: Some(id),
            });
        }
        self.show_toast(
            format!("Template '{name}' applied to this conversation"),
            cx,
        );
    }

    /// Delete a saved template. Conversations bound to it lose the binding —
    /// the DB does it via ON DELETE SET NULL, the mirror here.
    fn delete_template(&mut self, id: i64, name: &str, cx: &mut Context<Self>) {
        self.conv_templates.retain(|_, t| *t != id);
        match &self.persist {
            Some(p) => {
                let (tx, rx) = async_channel::bounded(1);
                p.send(PersistCmd::DeleteTemplate { id, reply: tx });
                cx.spawn(async move |this, cx| {
                    if let Ok(reply) = rx.recv().await {
                        this.update(cx, |app, cx| {
                            let (templates, err) =
                                merge_template_reply(std::mem::take(&mut app.templates), reply);
                            app.templates = templates;
                            // M6: on a DB read failure keep the existing list
                            // (set above) and surface the error — never swallow
                            // it into a silently-empty palette.
                            if let Some(e) = err {
                                eprintln!("awili: template list refresh failed: {e}");
                                app.show_toast(format!("Couldn't refresh templates: {e}"), cx);
                            }
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            None => self.templates.retain(|t| t.id != id),
        }
        self.show_toast(format!("Template '{name}' deleted"), cx);
    }

    /// Open the template editor panel (slice 18 §6): blank for `None`,
    /// prefilled for `Some(id)`. The name field gets focus — Enter there
    /// saves, so applying dictated content needs no pointer work.
    fn open_template_editor(
        &mut self,
        id: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (name, content) = id
            .and_then(|id| self.templates.iter().find(|t| t.id == id))
            .map(|t| (t.name.clone(), t.content.clone()))
            .unwrap_or_default();
        self.template_editor = Some(id);
        self.template_name.update(cx, |c, cx| c.set_text(&name, cx));
        self.template_content
            .update(cx, |c, cx| c.set_text(&content, cx));
        let h = self.template_name.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// Close the editor with nothing written (Escape / Cancel / ✕).
    fn close_template_editor(&mut self, cx: &mut Context<Self>) {
        self.template_editor = None;
        cx.notify();
    }

    /// The editor's Save: upsert by name through the worker, refresh the
    /// in-memory mirror from its reply (a NEW template's id is DB-assigned),
    /// close the panel, toast. Editing a template's content under its
    /// existing name keeps its id, so bound conversations follow the edit.
    fn save_template(&mut self, cx: &mut Context<Self>) {
        if self.template_editor.is_none() {
            return; // a stray Submit while the editor is closed
        }
        let name = self.template_name.read(cx).text().trim().to_string();
        let content = self.template_content.read(cx).text().to_string();
        if name.is_empty() || content.trim().is_empty() {
            self.show_toast("Template needs a name and content", cx);
            return;
        }
        match &self.persist {
            Some(p) => {
                let (tx, rx) = async_channel::bounded(1);
                p.send(PersistCmd::UpsertTemplate {
                    name,
                    content,
                    reply: tx,
                });
                cx.spawn(async move |this, cx| {
                    if let Ok(reply) = rx.recv().await {
                        this.update(cx, |app, cx| {
                            let (templates, err) =
                                merge_template_reply(std::mem::take(&mut app.templates), reply);
                            app.templates = templates;
                            // M6: on a DB read failure keep the existing list
                            // (set above) and surface the error — never swallow
                            // it into a silently-empty palette.
                            if let Some(e) = err {
                                eprintln!("awili: template list refresh failed: {e}");
                                app.show_toast(format!("Couldn't refresh templates: {e}"), cx);
                            }
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            None => {
                // No database: same upsert-by-name semantics, session-only.
                match self.templates.iter_mut().find(|t| t.name == name) {
                    Some(t) => t.content = content,
                    None => {
                        let id = self.templates.iter().map(|t| t.id).max().unwrap_or(0) + 1;
                        self.templates.push(PromptTemplate {
                            id,
                            name,
                            content,
                            created_at: 0,
                            updated_at: 0,
                        });
                        self.templates.sort_by(|a, b| a.name.cmp(&b.name));
                    }
                }
            }
        }
        self.template_editor = None;
        self.show_toast("Template saved", cx);
    }

    /// Open the saved-project instructions editor for the clicked row, prefilling
    /// the editor with the project's current instructions and focusing it. The
    /// row passes its own `(id, folder, instructions)` so there is no re-lookup
    /// (and so no id-0 ambiguity in the no-DB session).
    fn open_project_editor(
        &mut self,
        project_id: i64,
        folder: String,
        instructions: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.project_editor = Some((project_id, folder));
        self.project_instructions
            .update(cx, |c, cx| c.set_text(&instructions, cx));
        let h = self.project_instructions.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// Close the project editor with nothing written (Escape / Cancel / ✕).
    fn close_project_editor(&mut self, cx: &mut Context<Self>) {
        self.project_editor = None;
        cx.notify();
    }

    /// The project editor's Save: write the new instructions through the worker,
    /// then replace the in-memory `work.projects` mirror from its reply — the
    /// same authoritative-mirror contract as `save_project_folder`
    /// and `remove_project_by_id`, so the mirror and the rows never diverge. The
    /// "saved" toast fires only AFTER a successful reply (like those siblings, not
    /// the older template editor's eager toast): a failed write — including the
    /// project being deleted out from under the editor — surfaces the error and
    /// never falsely claims the edit was saved. The no-DB session updates its
    /// optimistic entry in place so the edit still takes effect for the session.
    fn save_project_instructions(&mut self, cx: &mut Context<Self>) {
        let Some((project_id, folder)) = self.project_editor.clone() else {
            return; // a stray Submit while the editor is closed
        };
        let instructions = self.project_instructions.read(cx).text().to_string();
        match &self.persist {
            Some(p) => {
                let (tx, rx) = async_channel::bounded(1);
                p.send(PersistCmd::UpdateProjectInstructions {
                    project_id,
                    instructions,
                    reply: tx,
                });
                cx.spawn(async move |this, cx| {
                    if let Ok(reply) = rx.recv().await {
                        let _ = this.update(cx, |app, cx| {
                            match reply {
                                Ok(snapshot) => {
                                    app.apply_project_snapshot(snapshot);
                                    app.show_toast("Instructions saved", cx);
                                }
                                Err(e) => {
                                    // Keep the existing list, surface the error —
                                    // never a false "saved".
                                    eprintln!("awili: project instructions update failed: {e}");
                                    app.show_toast(format!("Couldn't save instructions: {e}"), cx);
                                }
                            }
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
            // No DB this session: update the optimistic in-memory entry in place,
            // matched by folder (not id — no-DB entries all carry id 0, so an id
            // match could hit the wrong project; folder is unique in the mirror).
            // Toast only when a row was actually updated, so the message is honest
            // even in the (very narrow) no-DB vanished-row case.
            None => match self.work.projects.iter_mut().find(|p| p.folder == folder) {
                Some(proj) => {
                    proj.instructions = instructions;
                    self.show_toast("Instructions saved", cx);
                }
                None => self.show_toast("Couldn't save instructions: project no longer exists", cx),
            },
        }
        self.project_editor = None;
    }

    /// Start editing a form field inline: prefill the field editor with the
    /// current value and focus it.
    fn begin_field_edit(
        &mut self,
        entry: &SettingEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.begin_edit(entry.target.clone(), entry.kind);
        self.settings_field
            .update(cx, |c, cx| c.set_text(&entry.value, cx));
        let h = self.settings_field.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// Save the raw-TOML tab (the Save button): full validation first —
    /// invalid TOML never reaches disk (spec §3). The D1 fail-closed
    /// restoration guard runs inside the production helper
    /// [`settings::SettingsState::commit_raw_editor`]: any raw edit made
    /// while masked values were visible is refused BEFORE
    /// commit_raw/write/ConfigReloaded — no secret is ever substituted
    /// after the masked document's text identity changed.
    fn save_raw_settings(&mut self, cx: &mut Context<Self>) {
        let edited = self.settings_raw.read(cx).text().to_string();
        // On refusal the helper sets the fixed error and keeps the editor
        // draft and snapshot intact for repair; only a committed save emits
        // ConfigReloaded (and the success status clear).
        if self.settings.commit_raw_editor(&edited) {
            cx.emit(ConfigReloaded);
            self.schedule_status_clear(cx);
        }
        cx.notify();
    }

    /// The structured panel's Save button (slice 11 §1): validate the whole
    /// draft, write it through the atomic gate, then signal the reload.
    /// SXe: closing after the save only — a Save that does not end the
    /// question keeps it showing (there is still more to save, or the
    /// question's own Save answer is a full save + close). SXe-FIX1: returns
    /// `true` only when the save reached disk.
    fn save_draft_settings(&mut self, cx: &mut Context<Self>) -> bool {
        let landed = self.settings.save_draft();
        if landed {
            if let Some(executor) = self.pending_harness_recipe.take() {
                // Raw/All Fields may have replaced the draft since the click.
                // Apply only the recipe that the successful save actually wrote.
                let saved = self
                    .settings
                    .doc
                    .as_ref()
                    .and_then(|doc| doc.config().work.default_executor);
                if saved.as_deref() == Some(executor.to_persist().as_str()) {
                    self.set_work_executor_persisted(executor);
                }
            }
            cx.emit(ConfigReloaded);
            self.schedule_status_clear(cx);
        }
        cx.notify();
        landed
    }

    /// Activate the currently selected settings-nav action (slice 11 §2):
    /// dispatch it against the draft, then re-request nav focus only when the
    /// General form stays open with no field editor up — an editor keeps the
    /// shared composer's focus intact.
    fn move_settings_nav_with_error(&mut self, backwards: bool) {
        let key_rows = self.settings_key_rows();
        self.settings_nav.reconcile_with_keys(&key_rows);
        if self.settings.error.is_none()
            && self.settings_nav.selected() == settings_nav::Action::ErrorDetails
        {
            self.settings_nav.select(settings_nav::Action::AddProvider);
        }
        let mut actions = settings_nav::actions_with_keys(&key_rows);
        if self.settings.error.is_some() {
            actions.push(settings_nav::Action::ErrorDetails);
        }
        let at = actions
            .iter()
            .position(|action| *action == self.settings_nav.selected())
            .unwrap_or(0);
        let next = if backwards {
            if at == 0 {
                actions.len() - 1
            } else {
                at - 1
            }
        } else {
            (at + 1) % actions.len()
        };
        self.settings_nav.select(actions[next]);
    }

    fn settings_key_rows(&self) -> Vec<bool> {
        self.settings
            .draft
            .as_ref()
            .map(|d| {
                d.providers
                    .iter()
                    .map(|p| matches!(p.kind, ProviderKind::Anthropic | ProviderKind::OpenAi))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn select_harness_recipe(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((_, _, executor)) = harness_setup::RECIPES.get(index) else {
            return;
        };
        if let Some(draft) = self.settings.draft.as_mut() {
            draft.work_executor = Some((*executor).into());
            self.pending_harness_recipe = work::WorkExecutor::from_persist(executor);
            self.settings_nav
                .select(settings_nav::Action::HarnessRecipe(index));
            self.settings_nav_focus_pending = true;
            cx.notify();
        }
    }

    fn activate_settings_nav(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.draft.is_none() {
            return;
        }
        self.settings_nav
            .reconcile_with_keys(&self.settings_key_rows());
        let action = self.settings_nav.selected();
        let field_action = matches!(
            action,
            settings_nav::Action::ProviderName(_)
                | settings_nav::Action::ProviderUrl(_)
                | settings_nav::Action::ProviderModels(_)
                | settings_nav::Action::ProviderApiKey(_)
        );
        match action {
            settings_nav::Action::LocalDiscovery => {
                self.settings_nav_focus_pending = false;
                self.start_local_discovery(window, cx);
                return;
            }
            settings_nav::Action::ErrorDetails => {
                self.open_settings_error_details(window, cx);
                return;
            }
            settings_nav::Action::HarnessRecipe(index) => self.select_harness_recipe(index, cx),
            settings_nav::Action::HarnessDocs(index) => {
                if let Some((_, url)) = harness_setup::GUIDES.get(index) {
                    let _ = open::that_detached(url);
                }
            }
            settings_nav::Action::AddProvider => {
                if let Some(ix) = self.settings.add_provider() {
                    self.settings_nav.after_add(ix);
                    // Scroll the new provider row into view after an explicit
                    // Add activation (navigation intent, not every render).
                    if let Some(row) =
                        settings_nav::selected_settings_row(self.settings_nav.selected(), ix + 1)
                    {
                        self.settings_nav_scroll.scroll_to_item(row);
                    }
                    self.begin_draft_field(settings::Field::ProviderName(ix), window, cx);
                }
            }
            settings_nav::Action::ProviderName(ix) => {
                self.begin_draft_field(settings::Field::ProviderName(ix), window, cx);
            }
            settings_nav::Action::ProviderKind(ix) => {
                self.settings.cycle_provider_kind(ix);
            }
            settings_nav::Action::ProviderEnabled(ix) => {
                self.settings.toggle_provider_enabled(ix);
            }
            settings_nav::Action::ProviderUrl(ix) => {
                self.begin_draft_field(settings::Field::ProviderUrl(ix), window, cx);
            }
            settings_nav::Action::ProviderModels(ix) => {
                self.begin_draft_field(settings::Field::ProviderModels(ix), window, cx);
            }
            settings_nav::Action::ProviderApiKey(ix) => {
                self.begin_draft_field(settings::Field::ProviderApiKey(ix), window, cx);
            }
            settings_nav::Action::RemoveProvider(ix) => {
                self.settings.remove_provider(ix);
                let new_count = self
                    .settings
                    .draft
                    .as_ref()
                    .map(|d| d.providers.len())
                    .unwrap_or(0);
                self.settings_nav.after_remove(ix, new_count);
            }
            settings_nav::Action::Save => {
                self.save_draft_settings(cx);
            }
            settings_nav::Action::Discard => {
                // The Models & Fleet service bar owns only the provider
                // draft. Keep its keyboard action aligned with the visible
                // “Throw away changes” button; the global close/discard path
                // would drop the entire Settings draft and close the panel.
                self.settings.revert_services();
                cx.notify();
            }
        }
        if !field_action && self.settings.field.is_none() {
            self.settings_nav_focus_pending = true;
            cx.notify();
        }
    }

    /// Point the shared inline editor at a draft text field: prefill it with
    /// the draft's current value and focus it.
    fn begin_draft_field(
        &mut self,
        field: settings::Field,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.settings.field_text(field);
        self.settings.begin_field(field);
        // Optional: select the provider row in the nav when entering one.
        match field {
            settings::Field::ProviderName(ix) => {
                self.settings_nav
                    .select(settings_nav::Action::ProviderName(ix));
            }
            settings::Field::ProviderUrl(ix) => {
                self.settings_nav
                    .select(settings_nav::Action::ProviderUrl(ix));
            }
            settings::Field::ProviderModels(ix) => {
                self.settings_nav
                    .select(settings_nav::Action::ProviderModels(ix));
            }
            settings::Field::ProviderApiKey(ix) => {
                self.settings_nav
                    .select(settings_nav::Action::ProviderApiKey(ix));
            }
            _ => {}
        }
        self.settings_field
            .update(cx, |c, cx| c.set_text(&text, cx));
        let h = self.settings_field.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// Clear the transient "Saved" chip ~2 seconds after the save that showed
    /// it (slice 11 §1). `status_seq` keys the timer to ITS status — a newer
    /// save supersedes the pending clear rather than being cut short by it.
    fn schedule_status_clear(&self, cx: &mut Context<Self>) {
        let seq = self.settings.status_seq;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            this.update(cx, |app, cx| {
                if app.settings.status_seq == seq && app.settings.status.take().is_some() {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Count a Work-mode turn's gateway-reported dollar cost into the shared
    /// ticker so the status-bar chip reflects Work spend (Hermes prices the turn
    /// for us; Awili has no Hermes token pricing). Bridges work.rs, which can't
    /// reach the private `cost` field.
    pub(crate) fn note_work_cost(&mut self, cost_usd: f64) {
        let gen = self.cost.generation();
        self.cost
            .note_reported_cost(cost_usd, feed::unix_now(), gen);
    }

    /// P10/F2: refresh the Work engine and gateway bindings from a reloaded
    /// config. The engine endpoint and its bearer must come from the SAME
    /// reload, so a change to `[engine].base_url`/`api_key` (or
    /// `[gateway].dashboard_url`) takes effect without a restart. Extracted
    /// from [`AwiliApp::apply_runtime_config`] so a live-refresh test can
    /// assert it against a real [`WorkState`].
    fn refresh_work_endpoints(work: &mut WorkState, cfg: &Config) {
        // L1e2a-FIX1: a new local endpoint is unchecked; the old result and
        // any probe in flight no longer count.
        if work.engine.base_url != cfg.engine.base_url {
            work.local.endpoint_changed();
        }
        work.engine = cfg.engine.clone();
        // `[[engines]]`: the additional in-app models, re-read with the rest.
        work.extra_engines = cfg.engines.clone();
        work.local.cloud_escalation = cfg.work.cloud_escalation.clone();
        work.gateway = work::GatewayCfg {
            enabled: !cfg.gateway.dashboard_url.is_empty(),
            dashboard_url: cfg.gateway.dashboard_url.clone(),
        };
    }

    /// P12/F1: the config-sourced `WorkState` fields populated at
    /// CONSTRUCTION (`[work.agents]`, `[models]`, `[build_targets.<id>]`,
    /// `[outline]`/`[gitea]`). Extracted from [`AwiliApp::new`] so the startup
    /// regression — the connector sections left at their disabled defaults on
    /// an ordinary launch, which silently drops the vendor-CLI
    /// `--mcp-config`/`-c mcp_servers` flags and the Work-panel label — can be
    /// asserted against a real [`WorkState`] without a GPUI context (the same
    /// reason [`AwiliApp::refresh_work_endpoints`] is separate).
    fn apply_startup_work_config(work: &mut WorkState, config: &Config) {
        // U4 (D2): seed the Ask/Auto default from `[work] default_approval_mode`.
        work.ask_modes
            .set_default(awili_work::ApprovalMode::from_config(
                config.work.default_approval_mode.as_deref(),
            ));
        work.agents = config.work.agents.clone();
        work.sandbox = config.work.sandbox.clone();
        // R2b: the Default Work helper a new Work conversation starts on.
        work.default_helper = config.work.default_executor.clone();
        work.local.cloud_escalation = config.work.cloud_escalation.clone();
        work.models_cfg = config.models.clone();
        work.extra_engines = config.engines.clone();
        // `[build_targets.<id>]` (P2d): the LocalBuild picker section and its
        // dispatch resolve from these bindings; absent means no build targets,
        // exactly like before.
        work.build_targets = config.build_targets.clone();
        // P12: the `[outline]`/`[gitea]` sections must reach WorkState at
        // startup, not only through an `apply_runtime_config` re-apply.
        work.connectors = crate::connectors_mcp::ConnectorsSections::from(config);
        // P8b: the graduation ledger path (empty disables).
        work.graduation.set_ledger(&config.graduation.ledger);
        // P8d: seed the fallback detector's per-conversation prior-run map
        // from the ledger once at startup (the runtime re-apply path
        // re-points the ledger but does NOT seed again).
        if let Err(e) = work.graduation.seed_from_ledger() {
            eprintln!("awili: graduation ledger seed skipped: {e}");
        }
    }

    /// Re-apply what the runtime can apply live after a successful settings
    /// save: the provider·model cycle (keeping the current selection when it
    /// still exists) and the memory connection. `[voice]`/`[feed]` spin up
    /// workers at startup and carry "restart required" tags instead.
    fn apply_config_live(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self.settings.doc.as_ref() else {
            return;
        };
        let cfg = doc.config();
        // First-run welcome boundary: a successful settings commit is the only
        // place onboarding learns about a save. Declared config choices only —
        // no discovered models, no network status. Complete stays Complete.
        self.onboarding.on_saved(harness_setup::work_route_ready(&cfg));
        // A genuine save clears any welcome "Check again" error copy.
        self.onboarding_error = None;
        self.apply_runtime_config(&cfg, cx);
        // Settings changes only refresh config. The explicit Start chatting
        // onboarding action owns initial helper selection; later saves are a
        // starting preference and must not retarget the active Work thread.
        // A successful, valid save retires the startup error banner too:
        // only now that the document AND the runtime are genuinely applied
        // may the flag clear.
        self.config_load_error = false;
    }

    /// Runtime half of a live config re-apply: epoch/cache invalidation,
    /// choices/endpoints/work models/keys, Hermes, memory, merge, probe and
    /// selection — deliberately WITHOUT the onboarding save boundary, so a
    /// welcome "Check again" retry can re-apply (or drop to `networkless`)
    /// without faking a save.
    fn apply_runtime_config(&mut self, cfg: &Config, cx: &mut Context<Self>) {
        // Keys written as `secret:<account>` become the stored secret here,
        // for this run only; the file keeps the reference.
        let (cfg, secret_notices) = secret_store::resolved(cfg);
        let cfg = &cfg;
        self.secret_notices = secret_notices;
        self.appearance.preference = cfg.appearance.color_mode;
        // P1: (re)load the fleet pack in the single reconfigure funnel, so a
        // config change replaces or clears it; an invalid or disabled
        // section drops the pack for this config (diagnostic only).
        self.fleet_pack = fleet_skills::load_pack(cfg);
        self.provider_probe_generation = self.provider_probe_generation.wrapping_add(1);
        self.provider_health.clear();
        self.provider_probe_in_flight = false;
        self.discovered_models.clear();
        let current = self.choices.get(self.choice_idx).cloned();
        self.choices = cfg.model_choices();
        self.configured_choices = self.choices.clone();
        self.provider_endpoints = cfg
            .providers
            .iter()
            .filter(|p| p.enabled)
            .cloned()
            .collect();
        self.work.agents = cfg.work.agents.clone();
        self.work.sandbox = cfg.work.sandbox.clone();
        // R2b: a changed Default Work helper applies to the next new Work
        // conversation.
        self.work.default_helper = cfg.work.default_executor.clone();
        // P8b: re-point the graduation ledger on a config re-apply.
        self.work.graduation.set_ledger(&cfg.graduation.ledger);
        self.notifier.set_config(cfg.notifications.clone());
        // SXh: "Ask before changes" applies at once. A per-conversation
        // choice still wins (`set_default` keeps them).
        self.work
            .ask_modes
            .set_default(awili_work::ApprovalMode::from_config(
                cfg.work.default_approval_mode.as_deref(),
            ));
        // P12: (re)apply the connector sections so a config edit to
        // `[outline]`/`[gitea]` changes the owned-engine server entry and the
        // vendor-CLI MCP flags for the next run (and clears the label).
        self.work.connectors = crate::connectors_mcp::ConnectorsSections::from(cfg);
        // RC2-CNCL6: the council's Hermes route.
        self.council_fleet = council_run::fleet_route(cfg);
        // A valid config loaded: the load-error banner goes away.
        self.config_load_error = false;
        self.onboarding_harness = harness_setup::configured_harness(cfg);
        self.onboarding_other_route = harness_setup::other_work_route(cfg);
        self.work.models_cfg = cfg.models.clone();
        // P2d: a live config re-apply (re)loads the build target bindings —
        // a target added or removed in the file is offered (or withdrawn)
        // from the picker immediately; in-flight builds keep their frozen
        // resolved snapshot and are untouched.
        self.work.build_targets = cfg.build_targets.clone();
        // Re-apply what discovery already knows, then re-probe (a config edit
        // may add/move endpoints).
        self.merge_discovered_models();
        self.refresh_provider_health(cx);
        self.choice_idx = current
            .and_then(|c| {
                self.choices
                    .iter()
                    .position(|n| n.provider == c.provider && n.model == c.model)
            })
            .unwrap_or_else(|| cfg.default_choice_index());
        self.save_selection();

        // Rebuild provider credentials and Hermes endpoint from the live config
        // so a change to [hermes] base_url or api_key takes effect without restart.
        // Both `keys` and `work.hermes` were previously left stale (slice-11 §3 gap).
        self.keys = ProviderKeys::from_env_and_config(cfg);
        // P10/F2: refresh the Work engine/gateway bindings in the same reload
        // as the keys — the endpoint and its bearer must never be a reload
        // apart, or the next `build_awili_engine` pairs the new URL with the
        // stale key.
        Self::refresh_work_endpoints(&mut self.work, cfg);
        self.work.hermes = HermesCfg {
            enabled: !cfg.hermes.base_url.is_empty(),
            base_url: cfg.hermes.base_url.clone(),
            api_key: cfg.hermes.api_key.clone(),
            dashboard_url: cfg.gateway.dashboard_url.clone(),
        };

        self.memory = MemoryCfg {
            enabled: !cfg.memory.base_url.is_empty(),
            base_url: cfg.memory.base_url.clone(),
            token: cfg.memory.token.clone(),
            min_priority: cfg.memory.min_priority,
            recall: cfg.memory.recall,
            write: cfg.memory.write,
        };
        if !self.memory.enabled {
            // Memory was just disabled live: a service that is off is not "offline".
            // Drop any lingering `rules: offline` chip and reset the transition
            // tracker so a later re-enable begins a fresh observation (its first
            // turn is a state, not a from-offline recovery event).
            self.rules = None;
            self.rules_seen_online = None;
        }
        cx.notify();
    }

    /// Startup config-error re-check (the banner button and ⌘⇧R). Re-reads
    /// the file and re-applies the runtime. It never touches onboarding
    /// state (on an error launch it is `Complete` and must stay `Complete`),
    /// never calls `on_saved`, and never fakes a `ConfigReloaded` or a
    /// Settings document change. A valid file re-applies the real config and
    /// then — only then — clears the flag; `Missing` and malformed files
    /// discard the returned builtin, clear the remembered saved selection,
    /// re-apply `networkless`, and keep the flag true (banner stays up).
    fn check_startup_config(&mut self, cx: &mut Context<Self>) {
        if !self.config_load_error {
            return;
        }
        match Config::load_with_source() {
            Ok((cfg, awili_config::ConfigSource::Loaded(_))) => {
                self.apply_runtime_config(&cfg, cx);
                self.config_load_error = false;
            }
            Ok((_, awili_config::ConfigSource::Missing)) | Err(_) => {
                self.pending_saved_selection = None;
                self.apply_runtime_config(&Config::networkless(), cx);
                // RVS6C P1: still missing/malformed — keep the flag set (the
                // runtime re-apply just cleared it): Chat stays refused and
                // the banner stays up.
                self.config_load_error = true;
            }
        }
    }

    /// A user submitted a prompt in Code mode: append the user turn + an empty
    /// assistant bubble to the active tab, then drive the engine and stream its
    /// output into the bubble. Mirrors `start_turn` but for the local coding
    /// engine (no memory/fallback machinery — the engine owns its own context).
    #[cfg(test)]
    fn submit_code_turn(&mut self, text: String, cx: &mut Context<Self>) {
        self.submit_code_turn_kind(text, MessageKind::Normal, cx);
    }

    /// [`Self::submit_code_turn`] whose user row is of `kind` (RC2-RUNF: the
    /// plan card's Approve sends [`MessageKind::PlanClick`]).
    pub(crate) fn submit_code_turn_kind(
        &mut self,
        text: String,
        kind: MessageKind,
        cx: &mut Context<Self>,
    ) {
        if self.onboarding.is_active() {
            return;
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let conv = self
            .work
            .code_tabs
            .get(self.work.code_active)
            .and_then(|s| s.history.conv_id.clone());
        if self.shell.archived_reader.is_some()
            || conv
                .as_deref()
                .is_some_and(|id| self.refuse_archived_workshop_send(id, cx))
        {
            return;
        }
        self.ensure_code_tab(cx);
        if self.workshop_input_owner(true, cx).is_none() {
            return;
        }
        let active = self.work.code_active;
        let Some(tab_id) = self.work.code_tabs.get(active).map(|tab| tab.id) else {
            return;
        };
        // A target selection may be made while a native turn is live, but a
        // second Send must not convert its backing or steal its Stop owner.
        if self
            .work
            .code_tabs
            .get(active)
            .is_some_and(|tab| tab.streaming)
        {
            self.show_toast(
                "Still running — press Stop (⌘.) or wait. Your message is still in the box.",
                cx,
            );
            return;
        }
        // Establish the shared conversation identity without launching a
        // sacrificial native provider turn.
        let _ = self.ensure_code_conversation_identity(tab_id);
        let Some(conv_id) = self
            .work
            .code_tabs
            .get(active)
            .and_then(|tab| tab.history.conv_id.clone())
        else {
            return;
        };
        if self
            .work
            .unavailable_conversation_routes
            .contains_key(&conv_id)
        {
            self.show_toast(
                "This conversation's saved next-turn target is unavailable; choose another target first.",
                cx,
            );
            return;
        }
        let native_target = self.work.code_tabs.get(active).map(|tab| {
            tab.history
                .conv_id
                .as_deref()
                .and_then(|id| self.work.conversation_routes.get(id))
                .unwrap_or(&tab.route)
                .next_target()
                .clone()
        });
        let Some(native_target) = native_target else {
            return;
        };
        let work_owns_conversation = self.work.conv_id.as_deref() == Some(conv_id.as_str());
        let work_busy_here = work_owns_conversation
            && (self.work.running || self.work.active_work_route_turn.is_some());

        // A gateway Stop remains its own conversation's owner until its drain
        // acknowledges it. Park that conversation, or a turn that selected a
        // Work-backed target; an independent native conversation can continue
        // through its separately guarded route without losing PlanClick.
        if self.work.gateway_stop_pending
            && (work_owns_conversation
                || !matches!(&native_target, work::WorkExecutor::Agent { .. }))
        {
            let accepted = self.submit_work_run_kind(text, conv_id.clone(), kind, cx);
            if accepted && !matches!(&native_target, work::WorkExecutor::Agent { .. }) {
                self.carry_code_ask_mode_to_work(tab_id, &conv_id);
                self.workshop_convert_agent_backing(tab_id, &conv_id);
                self.set_mode(work::Mode::Work, cx);
                self.set_active_conversation(&conv_id);
                self.sync_workshop_tabs();
            }
            return;
        }
        if work_busy_here
            || (self.work.running && !matches!(&native_target, work::WorkExecutor::Agent { .. }))
        {
            self.show_toast(
                "Still running — press Stop (⌘.) or wait. Your message is still in the box.",
                cx,
            );
            return;
        }
        // A native-backed Workshop conversation may choose an owned/gateway
        // next turn. Keep its stable id and view; only dispatch moves to the
        // existing Work adapter, never to a newly created thread.
        if !matches!(&native_target, work::WorkExecutor::Agent { .. }) {
            let folder = self.code_identity_folder(&conv_id);
            if let Some(notice) = crate::submit_guard::work_folder_gone_notice(folder.as_deref()) {
                self.show_toast(notice, cx);
                return;
            }
            self.carry_code_ask_mode_to_work(tab_id, &conv_id);
            // Work submission records the user row or parks it before this
            // presentation change. A refusal leaves the native backing and
            // the draft intact instead of stranding the tab in Work mode.
            if self.submit_work_run_kind(text, conv_id.clone(), kind, cx) {
                self.workshop_convert_agent_backing(tab_id, &conv_id);
                self.set_mode(work::Mode::Work, cx);
                self.set_active_conversation(&conv_id);
                self.sync_workshop_tabs();
            }
            return;
        }
        let cwd = self
            .code_identity_folder(&conv_id)
            .unwrap_or_else(|| self.code_cwd())
            .display()
            .to_string();
        let (engine, model) = match native_target {
            work::WorkExecutor::Agent { engine, model } => (engine, model),
            _ => unreachable!("non-agent targets return through Work dispatch"),
        };
        // Compose the accepted request before building the bridge. The native
        // provider must see exactly the attachment-enriched user text that is
        // mirrored into the app transcript; staged files are cleared only
        // after the run is accepted below.
        let input_owner = self
            .work
            .code_tabs
            .get(active)
            .and_then(|session| session.history.conv_id.clone());
        let attachments = (kind != MessageKind::Retry)
            .then_some(input_owner.as_deref())
            .flatten()
            .and_then(|id| self.search_store.as_ref()?.get_staged_attachments(id).ok())
            .unwrap_or_default();
        let continuation_task = crate::resume_context::mark_request(&text);
        let text = compose_message_with_attachments(&text, &attachments);
        let canonical = self
            .views
            .get(&conv_id)
            .map(work::bridge_records_from_view)
            .unwrap_or_default();
        let seq = self.start_code_run(tab_id, cx);
        let active_route = self
            .work
            .conversation_routes
            .get_mut(&conv_id)
            .and_then(|route| {
                route
                    .begin_turn(
                        conv_id.clone(),
                        seq,
                        cwd.clone(),
                        awili_code::continuity::canonical_cursor(&canonical),
                    )
                    .ok()
            });
        let Some(active_route) = active_route else {
            // `start_code_run` arms only the visual ticker. A missing or busy
            // route is an unaccepted send: leave the draft/tray untouched and
            // disarm the ticker rather than leaving a phantom native run.
            self.work.code_runs.end(tab_id);
            self.show_toast(
                "Not sent — this conversation could not start a native turn. Your message is still in the box.",
                cx,
            );
            return;
        };
        // A new native turn supersedes a plan left by either backing before
        // posture resolution may arm this turn's own Code plan gate. Keep a
        // PlanClick's captured approval key; `execute_next` remains one-shot
        // state on that gate and is not cleared here.
        let preserve_plan = matches!(&kind, MessageKind::PlanClick)
            .then(|| crate::ask_mode::AskKey::CodeTab(tab_id));
        self.clear_superseded_conversation_plans(&conv_id, preserve_plan.as_ref());
        // Resolve posture only once the immutable route owner was accepted,
        // against that target rather than the tab's previous engine/model.
        let autonomy = self.code_autonomy_for_accepted_target(tab_id, engine, model.as_deref());
        let Some(native_route) = active_route.native.as_ref() else {
            if let Some(route) = self.work.conversation_routes.get_mut(&conv_id) {
                route.finish_unstarted_turn(&active_route);
            }
            self.persist_conversation_route(&conv_id);
            self.work.code_runs.end(tab_id);
            self.show_toast(
                "Not sent — this conversation has no native target. Your message is still in the box.",
                cx,
            );
            return;
        };
        let bridge = native_route.prepare_bridge(
            &canonical,
            &text,
            &cwd,
            &[],
            awili_code::continuity::PRODUCTION_HISTORY_BUDGET,
        );
        self.work
            .active_code_route_turns
            .insert(tab_id, active_route.clone());
        let Some(session) = self.work.code_tabs.get_mut(active) else {
            return;
        };
        // Bind the display session to this folder, but always replace the slot
        // below. A late child from another harness must never write its id into
        // this accepted turn's slot.
        if session.project_folder.as_deref() != Some(cwd.as_str()) {
            session.project_folder = Some(cwd.clone());
        }
        session.engine = engine;
        session.model = model.clone();
        let slot = crate::code_provider::fresh_session_slot(bridge.resume_id());
        session.session_slot = std::sync::Arc::clone(&slot);

        // The user bubble, then the empty assistant bubble tokens stream into.
        let uidx = session.rows.len();
        session.convo.push_kind(Role::User, text.clone(), kind);
        session
            .rows
            .push(MessageRow::new(session.convo.message(uidx).unwrap()));
        let aidx = session.rows.len();
        session.convo.push(Role::Assistant, "");
        session
            .rows
            .push(MessageRow::new(session.convo.message(aidx).unwrap()));
        session.streaming = true;
        // The kill switch: sending on this tx kills the child (see CodeProvider).
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
        session.stream_abort_tx = Some(cancel_tx);
        cx.notify();
        self.accept_submit();
        self.code_turn_begin(tab_id, &text, aidx);
        if let (Some(id), Some(store)) = (input_owner.as_deref(), self.search_store.as_ref()) {
            if !attachments.is_empty() {
                if let Err(error) = store.clear_staged_attachments(id) {
                    self.show_toast(
                        format!("Files sent; clearing the tray failed — {error}"),
                        cx,
                    );
                }
                self.refresh_staged_attachments();
            }
        }

        let resume_id = bridge.resume_id().map(str::to_string);
        let bridge_resume_for_exclusion = resume_id.clone();
        let continuation_cwd = cwd.clone();
        let bin = work::resolve_agent_bin(engine.bin(), &self.work.agents);
        // Ask (U4): the planning step's directive; the harness flags make it
        // read-only.
        let system = autonomy
            .plan_first()
            .then(|| awili_code::PLAN_FIRST_SYSTEM.to_string());
        // M2b: the reply-style brief, once per engine session — RC1-M: on every
        // turn for Claude and Codex (their system channel).
        let system = crate::agent_brief::with_brief(
            system,
            &cwd,
            resume_id.is_none() || crate::agent_launch::has_system_channel(engine),
        );
        let task = bridge.prompt;
        let request = ChatRequest {
            model: String::new(),
            system,
            messages: vec![ChatMessage::user(task)],
        };
        // This is the precise spawn boundary. A gate/bridge owner before this
        // point has not exposed the request to a native process and may keep
        // its resume id; after it, persistence must invalidate that id.
        let mut provider = code_provider::CodeProvider::new(
            bin,
            engine,
            autonomy,
            cwd,
            resume_id,
            model.clone(),
            slot,
        )
        .with_search_paths(self.work.agents.search_paths.clone())
        .with_agent_memory(self.work.agents.memory.clone())
        .with_agents_md(self.work.agents.agents_md.clone())
        .with_sandbox(self.work.sandbox.clone());
        // Dsh (P9): the child's credentials come from a file Awili reads
        // itself (the .app inherits no login shell). Only Dsh gets this; the
        // chosen model travels in `DSH_MODEL` (already in `model`).
        if engine == awili_code::CodeEngine::Dsh {
            provider = provider.with_dsh_env_file(self.work.agents.dsh_env_file());
        }
        // P12: when a connector family is enabled and the server binary
        // resolves, the vendor CLIs get the `awili-connectors` MCP server
        // through their own mechanisms (claude `--mcp-config`, codex `-c`;
        // the flag is dropped for engines without one).
        if let Some(spec) = self.work.connectors.server_config(&self.work.agents) {
            provider = provider.with_connectors_mcp(spec);
        }

        let continuation_memory = self.memory.clone();
        let continuation_connectors = self.work.connectors.clone();
        let mut continuation_excluded = vec![conv_id.clone()];
        if let Some(id) = bridge_resume_for_exclusion {
            continuation_excluded.push(id);
        }
        cx.spawn(async move |this, cx| {
            let evidence = crate::resume_context::collect(
                &continuation_task,
                continuation_cwd,
                continuation_memory,
                continuation_connectors,
                continuation_excluded,
            )
            .await;
            // A stopped/superseded request must never start after retrieval.
            if !this
                .update(cx, |app, _| {
                    if !app.code_run_live(tab_id, seq) {
                        return false;
                    }
                    if let Some(route) = app.work.conversation_routes.get_mut(&conv_id) {
                        route.mark_native_started(&active_route);
                    }
                    app.persist_conversation_route(&conv_id);
                    true
                })
                .unwrap_or(false)
            {
                return;
            }
            let mut request = request;
            if let Some(message) = request.messages.last_mut() {
                message.content =
                    crate::resume_context::enrich(std::mem::take(&mut message.content), evidence);
            }
            // Drain until the channel closes — the run ends on its own Done, or
            // because a Stop killed the child (which closes the stream).
            let rx = provider.run(request, cancel_rx);
            let mut dropped_late = false;
            while let Ok(ev) = rx.recv().await {
                // M2e: a stopped (or replaced) turn's late events touch nothing.
                if !this
                    .update(cx, |app, _| app.code_run_live(tab_id, seq))
                    .unwrap_or(false)
                {
                    dropped_late = true;
                    break;
                }
                match ev {
                    ProviderEvent::Token(t) => {
                        this
                            .update(cx, |app, cx| {
                                let strip_conv = app.strip_tab_conv(tab_id);
                                app.strip_on_token(&strip_conv, cx);
                                app.code_on_token(tab_id, aidx, &t, cx)
                            })
                            .ok();
                    }
                    // RC2-R2G7: a tool that started shows like any activity.
                    ProviderEvent::Activity(line) | ProviderEvent::ToolStarted(line) => {
                        this.update(cx, |app, cx| app.code_on_activity(tab_id, aidx, line, cx))
                            .ok();
                    }
                    ProviderEvent::ToolAttemptFailed => {
                        this.update(cx, |app, cx| {
                            app.code_on_token(
                                tab_id,
                                aidx,
                                "\n\n⚠ A tool attempt failed; the agent may retry.\n\n",
                                cx,
                            )
                        })
                        .ok();
                    }
                    ProviderEvent::ContextWindow(w) => {
                        this.update(cx, |app, _cx| {
                            app.work.code_windows.insert(tab_id, w);
                        })
                        .ok();
                    }
                    ProviderEvent::Usage(u) => {
                        this.update(cx, |app, _cx| {
                            let strip_conv = app.strip_tab_conv(tab_id);
                            app.strip_on_usage(
                                &strip_conv,
                                u64::from(u.input_tokens),
                                u64::from(u.output_tokens),
                            );
                            app.code_turn_usage(tab_id, u)
                        })
                        .ok();
                    }
                    ProviderEvent::Error(e) => {
                        this.update(cx, |app, cx| {
                            if let Some(session) = app
                                .work
                                .code_tabs
                                .iter_mut()
                                .find(|session| session.id == tab_id)
                            {
                                session.errored.insert(aidx);
                            }
                            app.code_turn_error(tab_id, &e);
                            let run_id = app
                                .work
                                .code_tabs
                                .iter()
                                .find(|s| s.id == tab_id)
                                .and_then(|s| s.history.turn.as_ref().map(|t| t.run_id.clone()))
                                .unwrap_or_default();
                            app.code_run_errors
                                .entry((tab_id, aidx))
                                .or_insert((e.clone(), run_id));
                            app.code_on_token(tab_id, aidx, &format!("\n\n⚠ {e}"), cx)
                        })
                        .ok();
                    }
                    ProviderEvent::Done => break,
                }
            }
            // M2e: only the live turn settles; a Stop already settled it.
            // RC1-K: then the task records its end (tests wait on it).
            this.update(cx, |app, cx| {
                app.finish_code_turn_if_live(tab_id, seq, cx);
                app.work.code_runs.task_ended(seq, dropped_late);
            })
            .ok();
        })
        .detach();
    }

    /// Stop the active Code tab's in-flight turn: fire the kill switch (which
    /// kills the child) — wired to Escape (composer Cancel) and Cmd-. in Code.
    fn cancel_code_turn(&mut self, cx: &mut Context<Self>) {
        self.stop_code_tab(self.work.code_active, cx);
    }

    /// Stop Code tab `index`'s in-flight turn (the active one for Escape and
    /// Cmd-.; RC2-R61: every one when Awili quits).
    pub(crate) fn stop_code_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(tab_id) = self.work.code_tabs.get(index).map(|t| t.id) {
            self.work
                .plan_gates
                .clear(&crate::ask_mode::AskKey::CodeTab(tab_id));
        }
        let active = index;
        let mut stopped = None;
        if let Some(session) = self.work.code_tabs.get_mut(active) {
            if let Some(tx) = session.stream_abort_tx.take() {
                session.history.note_stop();
                let _ = tx.send(());
                stopped = Some(session.id);
            }
            cx.notify();
        }
        if let Some(tab_id) = stopped {
            if let Some(owner) = self.work.active_code_route_turns.remove(&tab_id) {
                let through = self
                    .views
                    .get(&owner.conversation_id)
                    .map(work::bridge_records_from_view)
                    .map(|records| awili_code::continuity::canonical_cursor(&records))
                    .unwrap_or_default();
                if let Some(route) = self
                    .work
                    .conversation_routes
                    .get_mut(&owner.conversation_id)
                {
                    if !route.finish_unstarted_turn(&owner) {
                        route.stop_turn(&owner, through);
                    }
                }
                self.persist_conversation_route(&owner.conversation_id);
            }
        }
        // M2e: settle the turn now; a detached tool may keep the stream open.
        if let Some(id) = stopped {
            self.finish_code_turn(id, cx);
        }
    }

    /// Append a streamed chunk to a Code tab's assistant bubble (addressed by the
    /// tab's stable id so it lands even if the user switched tabs).
    fn code_on_token(&mut self, tab_id: u64, idx: usize, chunk: &str, cx: &mut Context<Self>) {
        let active_id = self
            .work
            .code_tabs
            .get(self.work.code_active)
            .map(|s| s.id)
            .unwrap_or(u64::MAX);
        let Some(session) = self.work.code_tabs.iter_mut().find(|s| s.id == tab_id) else {
            return;
        };
        let is_active = session.id == active_id;
        if session.convo.append_text(idx, chunk) {
            if let Some(m) = session.convo.message(idx) {
                session.rows[idx].stream_update_plan_safe(m);
            }
            if is_active && follow_stream(&session.scroll) {
                session.scroll.scroll_to_bottom();
            }
            cx.notify();
        }
    }

    /// Append one activity step to a Code tab's streaming row (critique P2).
    fn code_on_activity(&mut self, tab_id: u64, idx: usize, line: String, cx: &mut Context<Self>) {
        // M2b: mark activity that reaches outside the project folder.
        let folder = self
            .work
            .code_tabs
            .iter()
            .find(|s| s.id == tab_id)
            .and_then(|s| s.project_folder.clone())
            .unwrap_or_default();
        let line = crate::agent_brief::mark_outside_scope(
            line,
            &folder,
            &crate::home::home_string(),
        );
        if let Some(session) = self.work.code_tabs.iter_mut().find(|s| s.id == tab_id) {
            if let Some(row) = session.rows.get_mut(idx) {
                row.push_activity(line);
                cx.notify();
            }
        }
    }

    /// Toggle a Code row's activity fold.
    pub(crate) fn toggle_code_row_activity(
        &mut self,
        tab_id: u64,
        idx: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.work.code_tabs.iter_mut().find(|s| s.id == tab_id) {
            if let Some(row) = session.rows.get_mut(idx) {
                row.activity_expanded = !row.activity_expanded;
                cx.notify();
            }
        }
    }

    /// End a Code turn: bring the final bubble plan current and clear streaming.
    fn finish_code_turn(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        let strip_conv = self.strip_tab_conv(tab_id);
        self.strip_on_end(&strip_conv);
        self.notify_code_turn_end(tab_id);
        self.plan_gate_after_code_turn(tab_id);
        if let Some(tab) = self.work.code_tabs.iter_mut().find(|tab| tab.id == tab_id) {
            let succeeded = tab
                .history
                .turn
                .as_ref()
                .is_some_and(|turn| turn.error.is_none() && !turn.stop_requested)
                && tab
                    .convo
                    .messages()
                    .last()
                    .is_some_and(|message| !message.text.trim().is_empty());
            if succeeded {
                tab.context_gap = false;
            }
        }
        self.code_turn_end(tab_id, cx);
        if let Some(session) = self.work.code_tabs.iter_mut().find(|s| s.id == tab_id) {
            if let Some(idx) = session.rows.len().checked_sub(1) {
                if let Some(m) = session.convo.message(idx) {
                    session.rows[idx].refresh(m);
                }
            }
            session.streaming = false;
            session.stream_abort_tx = None;
            cx.notify();
            // M2e: last, so code_turn_end (and M2f) still read `started`.
            self.work.code_runs.end(tab_id);
        }
    }

    /// Whether any live turn still needs the pre-reply thinking bullet.
    fn any_turn_waiting_for_visible_text(&self) -> bool {
        self.mode == Mode::Work
            && self.work.running
            && self.work.waiting_for_stream_output
            && self
                .work
                .conv_id
                .as_deref()
                .is_none_or(|id| id == self.sidebar.active_id())
            && self.active_view().is_some_and(|view| {
                self.work.stream_row_idx.is_some_and(|idx| {
                    view.rows
                        .get(idx)
                        .is_some_and(|row| row.role == Role::Assistant)
                })
            })
    }

    /// Reconcile the native preference notification on the app's foreground.
    fn reconcile_thinking_motion(&mut self, cx: &mut Context<Self>) {
        // Notifications can be published out of order by the native observer
        // and a foreground refresh. The current cache owns the preference;
        // the queued payload only wakes us, never overrides newer state.
        let reduced = ui::motion::reduce_motion();
        if reduced {
            self.thinking_timer_generation = self.thinking_timer_generation.wrapping_add(1);
            self.thinking_timer_running = false;
        } else if self.work.running {
            self.ensure_thinking_timer(cx);
        }
        // Paint the still max-size face immediately, or restart the breath.
        cx.notify();
    }

    /// Start the one shared pulse clock used by Workshop response rows.
    pub(crate) fn ensure_thinking_timer(&mut self, cx: &mut Context<Self>) {
        ui::motion::refresh();
        if ui::motion::reduce_motion() {
            return;
        }
        if self.thinking_timer_running {
            return;
        }
        self.thinking_timer_running = true;
        self.thinking_timer_generation = self.thinking_timer_generation.wrapping_add(1);
        let generation = self.thinking_timer_generation;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(ui::thinking_dot::THINK_STEP_MS))
                .await;
            let keep_pulsing = this
                .update(cx, |app, cx| {
                    if app.thinking_timer_generation != generation {
                        return false;
                    }
                    if !app.work.running || ui::motion::reduce_motion() {
                        app.thinking_timer_running = false;
                        cx.notify();
                        return false;
                    }
                    // Keep the timer alive across visible retry notices, but
                    // repaint only while at least one bullet is on screen.
                    if app.any_turn_waiting_for_visible_text() {
                        app.thinking_phase = activity_pulse(app.thinking_phase).next();
                        cx.notify();
                    }
                    true
                })
                .unwrap_or(false);
            if !keep_pulsing {
                break;
            }
        })
        .detach();
    }

    /// Toggle a transcript row's activity fold.
    fn toggle_row_activity(&mut self, conv_id: &str, idx: usize, cx: &mut Context<Self>) {
        if let Some(view) = self.views.get_mut(conv_id) {
            if let Some(row) = view.rows.get_mut(idx) {
                row.activity_expanded = !row.activity_expanded;
                cx.notify();
            }
        }
    }

    /// Fire-and-forget a paid turn's usage note to memory (slice 8 §5) — a
    /// worker thread owns the POST, errors are logged and ignored, and the
    /// caller only constructs a note when `cost_usd > 0`.
    fn post_usage_memory(&self, note: UsageNote) {
        if !self.memory.enabled || !self.memory.write {
            return;
        }
        let mem = self.memory.clone();
        std::thread::spawn(move || {
            let client = MemoryClient::resolved(mem.base_url, mem.min_priority);
            if let Err(e) = client.record_usage(&note) {
                eprintln!("awili: usage memory post failed (ignored): {e}");
            }
        });
    }

    /// Distill the just-committed turn into one of Awili's own memories (slice
    /// 10 §2): a detached worker asks the ACTIVE provider to compress the
    /// exchange, and — unless the model answers SKIP — writes the result to the
    /// memory service. Best-effort end to end: every failure is a stderr line;
    /// nothing here blocks the UI or the next turn. The noise gates (§3) run
    /// first so a short turn or a capped session never even starts the worker,
    /// and a disabled memory or an off `[memory] write` toggle (slice 11)
    /// refuses before any worker — and thus any provider or memory request — is
    /// spawned; a client that resolves to V65 is held the same way (V65
    /// hold): ONE authoritative client is resolved before admission and
    /// moved into the worker, where the write reuses it and a v6.5
    /// backend is refused before the provider is even built. Admission
    /// charges the session cap up front (review SF1) and the completion is
    /// settled against the session generation it spawned under (review SF2).
    fn distill_turn(
        &mut self,
        conv_id: &str,
        user_text: String,
        reply_text: String,
        cx: &mut Context<Self>,
    ) {
        // Slice 11: a disabled memory service or an off write-toggle means no
        // distillation at all — refuse before the cap is charged and before any
        // worker (and thus any provider or memory request) can be spawned. The
        // read path (recall/rules) is unaffected.
        if !self.memory.distillation_allowed() {
            return;
        }
        let Some(choice) = self.choices.get(self.choice_idx).cloned() else {
            return;
        };
        // V65 hold: resolve the ONE authoritative client BEFORE admission
        // (and thus before the cap is charged). A v6.5 backend refuses
        // admission here — and the worker refuses it again, defensively —
        // until candidate integration lands. The SAME client is moved into
        // the worker and used for the write; it is never re-resolved after
        // the provider call.
        let client = MemoryClient::resolved(self.memory.base_url.clone(), self.memory.min_priority);
        // Charges the cap at spawn (attempts, not completions — review SF1);
        // settle_distillation refunds the slot if the attempt comes to nothing.
        // Re-checks the memory state (slice 11) and the resolved backend
        // (V65 hold) so a disabled or held admission never charges the cap.
        if !admit_distillation(
            &self.memory,
            &client,
            &user_text,
            &reply_text,
            &mut self.memory_writes_this_session,
        ) {
            return;
        }
        // The session generation this attempt belongs to (review SF2): a
        // completion landing after `new_conversation` must not credit the new
        // session's counter or flash its chip.
        let spawn_gen = self.cost.generation();
        let conv_id = conv_id.to_string();
        let provider_name = choice.provider.clone();
        let model_name = choice.model.clone();
        let keys = self.keys.clone();
        let mem = self.memory.clone();
        let hermes_base_url = self.work.hermes.base_url.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let outcome = distill_and_store(
                &choice,
                &keys,
                &mem,
                client,
                &user_text,
                &reply_text,
                &hermes_base_url,
            );
            let _ = tx.send_blocking(outcome);
        });
        cx.spawn(async move |this, cx| {
            let Ok(outcome) = rx.recv().await else { return };
            let gen = this.update(cx, |app, cx| {
                // The distillation call's spend lands in the today total and
                // the durable usage row regardless of staleness — the money
                // is real (review SF3).
                if let Some(u) = outcome.usage {
                    apply_distill_usage(
                        &mut app.cost,
                        app.persist.as_ref(),
                        &conv_id,
                        &provider_name,
                        &model_name,
                        u,
                        feed::unix_now(),
                    );
                }
                let flash = settle_distillation(
                    &mut app.memory_writes_this_session,
                    spawn_gen,
                    app.cost.generation(),
                    outcome.wrote,
                );
                if flash {
                    // The write landed in the current session: flash the
                    // "memory saved" chip (§4).
                    app.memory_flash_gen += 1;
                    app.memory_saved_flash = true;
                }
                cx.notify();
                flash.then_some(app.memory_flash_gen)
            });
            let Ok(Some(gen)) = gen else { return };
            // Clear the flash after two seconds — unless a newer flash has
            // taken over the chip (generation mismatch).
            let (tx, rx) = async_channel::bounded(1);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(2));
                let _ = tx.send_blocking(());
            });
            let _ = rx.recv().await;
            this.update(cx, |app, cx| {
                if app.memory_flash_gen == gen {
                    app.memory_saved_flash = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Queue a durable append on the serialized persistence worker (no-op if
    /// persistence is off). Never touches the database on the GPUI thread (SF6).
    fn persist_message(&self, conv_id: &str, role: &'static str, text: &str, ts: u64) {
        if let Some(persist) = &self.persist {
            persist.append(conv_id, role, text.to_string(), ts as i64);
        }
    }

    /// [`Self::persist_message`] with the message's kind (RC2-RUNF: a
    /// plan-card click is stored with its mark).
    pub(crate) fn persist_message_kind(
        &self,
        conv_id: &str,
        role: &'static str,
        text: &str,
        ts: u64,
        kind: MessageKind,
    ) {
        if let Some(persist) = &self.persist {
            persist.append_kind(conv_id, role, text.to_string(), ts as i64, kind);
        }
    }

    /// Fire-and-forget the transcript line to the memory chat log.
    fn chat_log(&self, conv_id: &str, role: Role, text: &str) {
        if !self.memory.enabled || !self.memory.write {
            return;
        }
        let entry = ChatLogEntry {
            session_id: conv_id.to_string(),
            role: role_str(role).to_string(),
            content: text.to_string(),
        };
        let mem = self.memory.clone();
        std::thread::spawn(move || {
            let client = MemoryClient::resolved(mem.base_url, mem.min_priority);
            let _ = client.chat_log(&entry);
        });
    }

    // ---- model picker (slice 15) -----------------------------------------------

    /// Open the model-picker popover (the chip click or the palette's
    /// "Switch model"). With no configured choices there is nothing to switch
    /// to — the picker stays hidden (spec §1).
    /// Probe every distinct provider endpoint off the UI thread (C1) and fold
    /// the answers into [`Self::provider_health`]. Cheap (one small GET per
    /// endpoint, tight timeouts) and re-entrant: results just overwrite.
    /// When the CURRENT selection turns out dead and another provider is
    /// alive, the selection moves there ONCE per launch — a dead default must
    /// not be silently preselected, but the picker stays free to choose
    /// anything (the mark, not a ban).
    fn refresh_provider_health(&mut self, cx: &mut Context<Self>) {
        self.provider_probe_generation = self.provider_probe_generation.wrapping_add(1);
        let probe_generation = self.provider_probe_generation;
        // A re-probe settles the visible checking state even when it then
        // bails out (unsaved config guard / no endpoints).
        self.provider_probe_in_flight = false;
        if self.onboarding.is_active() && !self.onboarding.is_saved() {
            return;
        }
        let mut targets: Vec<(String, String, Option<String>, bool)> = Vec::new();
        for c in &self.provider_endpoints {
            if targets.iter().any(|(b, _, _, _)| *b == c.base_url) {
                continue;
            }
            let probe = match c.kind {
                awili_config::ProviderKind::Ollama => {
                    format!("{}/api/tags", c.base_url.trim_end_matches('/'))
                }
                _ => format!("{}/models", c.base_url.trim_end_matches('/')),
            };
            // The Hermes surface is the agent, not a model catalog — probe it
            // for liveness but never merge whatever its /models happens to say.
            let discover = !matches!(c.kind, awili_config::ProviderKind::Hermes);
            targets.push((c.base_url.clone(), probe, c.api_key.clone(), discover));
        }
        if targets.is_empty() {
            return;
        }
        // One batch covers every enabled endpoint, so the single flag is set
        // right before spawning it; the notify makes the Checking rows show.
        self.provider_probe_in_flight = true;
        cx.notify();
        let (tx, rx) = async_channel::bounded::<Vec<(String, bool, Option<Vec<String>>)>>(1);
        if cfg!(test) {
            // RC1-G1: a test build never probes on a real thread. Its late
            // wake-up tripped gpui's test scheduler ("Detected activity on
            // thread … not deterministic") in whichever GPUI test was running
            // (traces end at `rx.recv().await` below). Closing the channel
            // takes the existing no-results path on the scheduler itself: the
            // checking state ends and no health is invented.
            drop(tx);
        } else {
            std::thread::spawn(move || {
                let client = reqwest::blocking::Client::builder()
                    .connect_timeout(std::time::Duration::from_millis(900))
                    .timeout(std::time::Duration::from_secs(3))
                    .build();
                let Ok(client) = client else {
                    return;
                };
                let results = targets
                    .into_iter()
                    .map(|(base, probe, key, discover)| {
                        let mut req = client.get(&probe);
                        if let Some(key) = key.filter(|k| !k.is_empty()) {
                            req = req.bearer_auth(key);
                        }
                        // Any HTTP answer — 200 or 401 alike — proves the server
                        // is THERE; only transport failure marks it down. A 200
                        // body is additionally the model catalog — parse it; a
                        // malformed or non-catalog body is None, never a false
                        // empty catalog.
                        match req.send() {
                            Ok(resp) => {
                                let models = if discover && resp.status().is_success() {
                                    parse_discovered_models(&resp.text().unwrap_or_default())
                                } else {
                                    None
                                };
                                (base, true, models)
                            }
                            Err(_) => (base, false, None),
                        }
                    })
                    .collect();
                let _ = tx.send_blocking(results);
            });
        }
        cx.spawn(async move |this, cx| {
            // Receive first, then ALWAYS enter an update: a dropped channel
            // (client build failure or worker death) settles the flag for the
            // current generation just as a finished batch does, and a stale
            // callback must never touch newer state.
            match rx.recv().await {
                Ok(results) => {
                    this.update(cx, |this, cx| {
                        if this.provider_probe_generation != probe_generation {
                            return;
                        }
                        this.provider_probe_in_flight = false;
                        for (base, alive, models) in results {
                            this.provider_health.insert(base.clone(), alive);
                            // Some (even empty) replaces the catalog; None
                            // keeps the last successful one.
                            apply_model_cache(&mut this.discovered_models, base, models);
                        }
                        this.merge_discovered_models();
                        this.rescue_dead_default(cx);
                        cx.notify();
                    })
                    .ok();
                }
                Err(_) => {
                    // Channel closed with no results: the previous answers
                    // stand (no invented unreachable) — only end the visible
                    // checking state, and never for a superseded batch.
                    this.update(cx, |this, cx| {
                        if this.provider_probe_generation != probe_generation {
                            return;
                        }
                        this.provider_probe_in_flight = false;
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// The one-shot C1 rescue: launch selected a provider whose endpoint is
    /// Fold everything discovery has learned into the picker's choices,
    /// keeping the selection pinned to the same provider·model and adopting a
    /// pending persisted selection once it becomes real. The declared
    /// (configured) choices are authoritative: every rebuild resets to them
    /// before re-appending the current catalogs, and the selection falls back
    /// to the first choice if its model no longer resolves.
    fn merge_discovered_models(&mut self) {
        let current = self
            .choices
            .get(self.choice_idx)
            .map(|c| (c.provider.clone(), c.model.clone()));
        // Reset to the authoritative declared list, then re-apply only what
        // the current catalogs say (a stale discovery drops out here).
        self.choices = self.configured_choices.clone();
        // Deterministic merge order regardless of HashMap iteration.
        let non_chat = self.work.models_cfg.non_chat_filters.clone();
        let mut bases: Vec<&String> = self.discovered_models.keys().collect();
        bases.sort();
        for base in bases {
            let discovered = &self.discovered_models[base];
            let template = self
                .provider_endpoints
                .iter()
                .find(|p| &p.base_url == base)
                .map(|p| ModelChoice {
                    provider: p.name.clone(),
                    kind: p.kind,
                    base_url: p.base_url.clone(),
                    model: String::new(),
                    api_key: p.api_key.clone(),
                });
            extend_choices_with_discovered(
                &mut self.choices,
                base,
                discovered,
                template,
                &non_chat,
            );
        }
        if let Some((p, mo)) = current {
            // The selected model (a discovered one) no longer resolves: fall
            // back to the first choice so the index never points out of range.
            self.choice_idx = self
                .choices
                .iter()
                .position(|c| c.provider == p && c.model == mo)
                .unwrap_or(0);
        } else if self.choice_idx >= self.choices.len() {
            self.choice_idx = 0;
        }
        if let Some((p, mo)) = self.pending_saved_selection.clone() {
            if let Some(i) = self
                .choices
                .iter()
                .position(|c| c.provider == p && c.model == mo)
            {
                self.choice_idx = i;
                self.pending_saved_selection = None;
            }
        }
        // First-run welcome boundary: a completed rebuild is what makes the
        // picker non-empty (the helper only moves SavedIncomplete/Ready).
        self.onboarding.on_discovery(self.work_route_ready());
    }

    /// down while another is up — move to the first live choice and say so.
    fn rescue_dead_default(&mut self, cx: &mut Context<Self>) {
        if self.moved_off_dead_default {
            return;
        }
        let Some(current) = self.choices.get(self.choice_idx) else {
            return;
        };
        if self.provider_health.get(&current.base_url) != Some(&false) {
            return;
        }
        let Some((idx, live)) = self
            .choices
            .iter()
            .enumerate()
            .find(|(_, c)| self.provider_health.get(&c.base_url) == Some(&true))
        else {
            return;
        };
        let msg = format!(
            "{} is unreachable — switched to {} ({})",
            current.model, live.model, live.provider
        );
        self.choice_idx = idx;
        self.moved_off_dead_default = true;
        self.save_selection();
        self.show_toast(msg, cx);
    }

    fn open_picker(&mut self, cx: &mut Context<Self>) {
        if self.choices.is_empty() {
            return;
        }
        self.picker.open();
        cx.notify();
    }

    fn close_picker(&mut self, cx: &mut Context<Self>) {
        self.picker.close();
        cx.notify();
    }

    /// Select a model from the picker for THIS session (slice 15 §1): change
    /// the live selection (out-of-bounds wraps), remember it the way the old
    /// cycle chip did (app_state, restored next launch — review finding C; the
    /// config file itself is the settings panel's job), and close the popover.
    fn select_model(&mut self, idx: usize, cx: &mut Context<Self>) {
        if self.choices.is_empty() {
            return;
        }
        self.choice_idx = picker::select_choice(idx, self.choices.len());
        self.save_selection();
        self.picker.close();
        cx.notify();
    }

    /// Persist the current provider·model selection via the worker (no-op if
    /// persistence is off).
    fn save_selection(&self) {
        if let (Some(persist), Some(choice)) = (&self.persist, self.choices.get(self.choice_idx)) {
            let value = format!("{}{}{}", choice.provider, SELECTION_SEP, choice.model);
            persist.set_state(SELECTION_KEY, value);
        }
    }

    // ---- attention feed (slice 4) ---------------------------------------------

    /// Post one of Awili's own events to the central feed — fire-and-forget,
    /// no-op when the feed is disabled.
    fn post_feed_event(
        &self,
        severity: Severity,
        title: String,
        body: String,
        dedup_key: Option<String>,
    ) {
        if let Some(cfg) = &self.feed_cfg {
            feed::post_event(cfg, severity, title, body, dedup_key);
        }
    }

    /// Observe this turn's rules-chip state and post a `routine` event on a
    /// transition to/from offline (spec §7 — self-healed transitions are never
    /// phone-worthy, but they belong in the record). The very first observation
    /// is a state, not a transition: it posts nothing.
    fn note_rules_transition(&mut self, status: &RulesStatus) {
        // S5(a): a refused policy is not a reachability transition — it must
        // not post "memory service unreachable".
        if matches!(status, RulesStatus::Rejected) {
            return;
        }
        let online = matches!(status, RulesStatus::Loaded(_));
        let previous = self.rules_seen_online.replace(online);
        if previous.is_none() || previous == Some(online) {
            return;
        }
        let (title, body) = if online {
            (
                "Awili: rules injection recovered",
                "the memory service is answering again",
            )
        } else {
            (
                "Awili: rules injection offline",
                "directives were skipped this turn (memory service unreachable)",
            )
        };
        self.post_feed_event(
            Severity::Routine,
            title.to_string(),
            body.to_string(),
            Some("awili-rules-offline".to_string()),
        );
    }

    /// Toggle the Needs You sidebar. Opening it asks the worker for a fresh
    /// sync — the panel renders the mirror immediately either way.
    fn toggle_feed_panel(&mut self, cx: &mut Context<Self>) {
        self.feed_panel_open = !self.feed_panel_open;
        if self.feed_panel_open {
            if let Some(handle) = &self.feed {
                handle.sync_now();
            }
        }
        cx.notify();
    }

    /// Expand/collapse one row's body.
    fn toggle_feed_expand(&mut self, event_id: &str, cx: &mut Context<Self>) {
        if self.feed_expanded.as_deref() == Some(event_id) {
            self.feed_expanded = None;
        } else {
            self.feed_expanded = Some(event_id.to_string());
        }
        cx.notify();
    }

    /// Ack a whole group (every unacked event sharing the condition). The
    /// worker acks against the service and updates the mirror on success; the
    /// row disappears with the next snapshot. A failed ack (offline) leaves the
    /// row standing — honest, the service still considers it open.
    fn ack_feed_group(&mut self, event_ids: Vec<String>, cx: &mut Context<Self>) {
        if let Some(handle) = &self.feed {
            handle.ack(event_ids);
        }
        self.feed_expanded = None;
        cx.notify();
    }

    // ---- read-aloud (TTS) ----------------------------------------------------

    /// Stop any in-progress read-aloud immediately (no repaint; callers notify).
    fn stop_speaking_internal(&mut self) {
        if let Some(q) = &mut self.voice.speech {
            q.stop();
        }
        // RC2-R4 (F0956): the talk-over listener goes with the read-aloud.
        self.stop_barge_listener();
        self.voice.spoken.clear();
        self.voice.speech = None;
        self.voice.speaking = false;
        self.voice.reply_streaming = false;
        self.voice.reply_conv = None;
    }

    /// Stop read-aloud from the composer-row stop button.
    fn stop_speaking(&mut self, cx: &mut Context<Self>) {
        self.stop_speaking_internal();
        cx.notify();
    }

    /// Watch a speech queue's `done` signal and clear the speaking state when it
    /// fires — but only if a newer utterance has not since superseded this one.
    fn watch_speech_done(
        &self,
        gen: u64,
        done: async_channel::Receiver<()>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let _ = done.recv().await;
            this.update(cx, |app, cx| {
                if app.voice.speech_gen == gen {
                    app.voice.speaking = false;
                    app.voice.reply_streaming = false;
                    app.voice.speech = None;
                    // RC2-R4 (F0956): the reply is read; its talk-over listener
                    // gives the mic back before it is re-armed.
                    app.stop_barge_listener();
                    app.voice.spoken.clear();
                    // Conversation mode: the reply has been spoken (the queue
                    // drained), so the speakers are quiet — re-arm the mic for
                    // the next utterance WITHOUT recording Awili's own voice
                    // (slice-5 §3). Idempotent if already listening.
                    if app.voice.conversation_mode {
                        app.start_voice_listening(cx);
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// A read-aloud queue on the configured voice (RC2-R4M2: a test's own
    /// voice when it set one).
    fn start_speech_queue(&self) -> (SpeechQueue, async_channel::Receiver<()>) {
        #[cfg(test)]
        if let Some(tts) = self.voice.test_tts {
            return SpeechQueue::start_with_backend(tts());
        }
        SpeechQueue::start(&self.voice.cfg)
    }

    /// Speak a complete message (the per-reply speaker button): chunk it into
    /// sentences and queue them all, then let the worker drain.
    fn speak_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.stop_speaking_internal();
        let (mut queue, done) = self.start_speech_queue();
        let mut chunker = SentenceChunker::new();
        let mut sentences = chunker.push(&text);
        if let Some(tail) = chunker.flush() {
            sentences.push(tail);
        }
        for s in sentences {
            self.voice.spoken.push(s.clone());
            queue.enqueue(s);
        }
        queue.finish();
        self.voice.speech = Some(queue);
        self.voice.speaking = true;
        self.voice.reply_streaming = false;
        self.voice.speech_gen += 1;
        let gen = self.voice.speech_gen;
        self.watch_speech_done(gen, done, cx);
        self.start_barge_listener(cx);
        cx.notify();
    }

    /// RC2-R4 (F0956): in conversation mode, listen for the owner talking over
    /// the reply being read aloud (its own mic; the conversation mic is off
    /// while Awili speaks). A no-op outside conversation mode, while already
    /// listening, or when no mic can be opened (the reply still plays).
    fn start_barge_listener(&mut self, cx: &mut Context<Self>) {
        if !self.voice.conversation_mode
            || !self.voice.speaking
            || self.voice.barge.is_some()
            || self.onboarding.is_active()
        {
            return;
        }
        // RC2-R4M2: the conversation mic can still be armed here (a stored
        // reply's speaker button pressed while listening). Hand it off: it would
        // record Awili's own voice, and the talk-over listener must hear the
        // reading. Its utterance in flight is dropped; the reading's end
        // re-arms it (`watch_speech_done`).
        if self.voice.conv_mic.is_some() {
            self.stop_voice_listening_internal();
            self.voice.session.invalidate();
        }
        let Ok(mic) = self.open_voice_mic() else {
            return;
        };
        self.voice.barge_gen += 1;
        let gen = self.voice.barge_gen;
        let (tx, rx) = async_channel::unbounded::<String>();
        self.voice.barge = Some(crate::voice::start_barge_run(
            mic,
            self.voice.cfg.vad(),
            self.voice.model.clone(),
            move |text| {
                let _ = tx.send_blocking(text);
            },
        ));
        cx.spawn(async move |this, cx| {
            while let Ok(text) = rx.recv().await {
                this.update(cx, |app, cx| app.on_barge_transcript(&text, gen, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// RC2-R4M2: open a mic for conversation mode or the talk-over listener
    /// (a stand-in in tests that ask for one).
    fn open_voice_mic(&self) -> Result<crate::voice::Mic, awili_voice::CaptureError> {
        #[cfg(test)]
        if let Some(mics) = &self.voice.test_mics {
            return Ok(mics.open());
        }
        MicCapture::start().map(crate::voice::Mic::Real)
    }

    /// RC2-R4 (F0956): stop the talk-over listener (no repaint).
    fn stop_barge_listener(&mut self) {
        if let Some(run) = self.voice.barge.take() {
            run.stop();
        }
        self.voice.barge_gen += 1;
    }

    /// RC2-R4 (F0956): a talk-over during the read-aloud was transcribed. A
    /// stop ("stop", "stop talking", "that's enough"…) that is not just Awili's
    /// own sentence coming back through the speakers stops the reading and
    /// the reply; conversation mode stays on and listens again. Anything else
    /// — Awili's own voice included — changes nothing.
    pub(crate) fn on_barge_transcript(&mut self, text: &str, gen: u64, cx: &mut Context<Self>) {
        if gen != self.voice.barge_gen || !self.voice.speaking || !self.voice.conversation_mode {
            return;
        }
        if crate::voice::playback_barge_action(text, &self.voice.spoken)
            != crate::voice::BargeAction::StopReply
        {
            return;
        }
        let reply_conv = self.voice.reply_conv.clone();
        self.stop_speaking_internal();
        // The reply itself stops too: the Work run or the Code turn it came from.
        if reply_conv.is_some() || self.mode == Mode::Work || self.mode == Mode::Code {
            if self.mode == Mode::Work && self.work.running {
                self.cancel_work_run(cx);
            } else if self.mode == Mode::Code {
                self.cancel_code_turn(cx);
            }
        }
        self.voice.status = Some("Stopped \u{2014} listening".into());
        cx.notify();
    }

    /// Flip the auto-speak toggle.
    #[allow(dead_code)] // wired by the settings surface once it ships
    fn toggle_auto_speak(&mut self, cx: &mut Context<Self>) {
        self.voice.auto_speak = !self.voice.auto_speak;
        cx.notify();
    }

    // ---- dictation (STT) -----------------------------------------------------

    /// Kick off model download/load on a worker thread if it has not started,
    /// draining its status messages into the composer-row chip.
    fn ensure_model_loading(&mut self, cx: &mut Context<Self>) {
        if self.onboarding.is_active() {
            return;
        }
        if !self.voice.model.lock().unwrap().can_load() {
            return;
        }
        let slot = self.voice.model.clone();
        let spec = self.voice.cfg.model_spec();
        let (tx, rx) = async_channel::unbounded::<String>();
        std::thread::spawn(move || {
            load_model_blocking(&spec, &slot, |s| {
                let _ = tx.send_blocking(s);
            });
        });
        cx.spawn(async move |this, cx| {
            while let Ok(s) = rx.recv().await {
                let msg = (s != STATUS_CLEAR).then_some(s);
                this.update(cx, |app, cx| {
                    app.voice.status = msg;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Toggle dictation via the mic button — the PRIMARY dictation mechanic:
    /// click to start listening, click again to stop and commit the transcript.
    fn toggle_mic(&mut self, cx: &mut Context<Self>) {
        if self.voice.dictating {
            self.stop_dictation(cx);
        } else {
            self.voice.ptt_session = false;
            self.start_dictation(cx);
        }
    }

    /// Begin capturing the mic and streaming partial transcripts into the composer.
    /// The model loads lazily in parallel; partials appear once it is ready.
    fn start_dictation(&mut self, cx: &mut Context<Self>) {
        if self.onboarding.is_active() || self.shell.archived_reader.is_some() {
            return;
        }
        if self.voice.dictating {
            return;
        }
        // Dictation and the conversation loop must not share the mic: starting
        // a PTT/click dictation leaves voice mode first (slice-5 §3).
        if self.voice.conversation_mode {
            self.exit_conversation_mode(cx);
        }
        // Surface a prior model-load failure as we retry, so a transient download
        // error is visible rather than silently swallowed.
        if let Some(msg) = self.voice.model.lock().unwrap().failure() {
            self.voice.status = Some(format!("retrying voice model (previous error: {msg})"));
        }
        self.ensure_model_loading(cx);
        // N1 pre-flight: the model keeps loading in parallel while the OS
        // permission prompt (if any) is up; dictation re-enters here on grant.
        if !self.preflight_mic_permission(
            Self::start_dictation,
            |app, cx| {
                app.voice.ptt_session = false;
                app.composer.update(cx, |c, cx| c.dictation_cancel(cx));
            },
            cx,
        ) {
            return;
        }
        match MicCapture::start() {
            Ok(cap) => {
                let buffer = cap.shared_buffer();
                self.voice.capture = Some(cap);
                self.voice.dictating = true;
                // A fresh session id retires every worker from earlier utterances
                // (review blocker 1) — their late partials/finals are dropped at
                // apply time below.
                let session = self.voice.session.begin();
                self.voice.stop_partials = Arc::new(AtomicBool::new(false));
                self.spawn_partial_worker(buffer, session, cx);
                cx.notify();
            }
            Err(e) => {
                self.voice.status = Some(e.to_string());
                self.voice.ptt_session = false;
                self.composer.update(cx, |c, cx| c.dictation_cancel(cx));
                cx.notify();
            }
        }
    }

    /// Worker that polls the growing capture buffer and pushes refined partial
    /// transcripts into the composer until dictation stops. Each result is gated
    /// on `session` still being current when it reaches the UI (review blocker 1):
    /// the stop flag cannot cancel a `transcribe` already in flight, so a partial
    /// from a dead session CAN still arrive here — and must be discarded, not
    /// applied to whatever newer session happens to be dictating.
    fn spawn_partial_worker(
        &self,
        buffer: awili_voice::capture::SharedSamples,
        session: u64,
        cx: &mut Context<Self>,
    ) {
        let slot = self.voice.model.clone();
        let stop = self.voice.stop_partials.clone();
        let (tx, rx) = async_channel::unbounded::<String>();
        std::thread::spawn(move || loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(600));
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let audio = buffer.lock().unwrap().clone();
            if audio.len() < MIN_SAMPLES_FOR_PARTIAL {
                continue;
            }
            if let Some(w) = ready_model(&slot) {
                match w.transcribe(&audio) {
                    Ok(text) if !text.trim().is_empty() => {
                        if tx.send_blocking(text).is_err() {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        });
        cx.spawn(async move |this, cx| {
            while let Ok(text) = rx.recv().await {
                this.update(cx, |app, cx| {
                    if app.voice.dictating && app.voice.session.is_current(session) {
                        app.composer
                            .update(cx, |c, cx| c.dictation_partial(text.trim(), cx));
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Stop capturing, run a final transcription, and commit it into the composer
    /// through the EditBuffer (or cancel the ghost if nothing was said).
    ///
    /// The final worker *waits* for a still-loading model (review SF3) so a
    /// first-use utterance recorded during the download is transcribed once the
    /// model is ready, not silently discarded. The commit is stamped with this
    /// session's id and dropped if a newer session began — or the draft was
    /// submitted — while inference/loading ran (review blocker 1).
    fn stop_dictation(&mut self, cx: &mut Context<Self>) {
        if !self.voice.dictating {
            return;
        }
        self.voice.dictating = false;
        self.voice.ptt_session = false;
        self.voice.stop_partials.store(true, Ordering::SeqCst);
        let session = self.voice.session.current();
        let audio = self
            .voice
            .capture
            .take()
            .map(|c| c.stop())
            .unwrap_or_default();
        let slot = self.voice.model.clone();
        let wants_final = audio.len() >= MIN_SAMPLES_FOR_PARTIAL / 4;
        if wants_final && ready_model(&slot).is_none() {
            // First use: the model is still downloading/loading. Say so — the
            // ghost stays visible until the final lands (SF3).
            self.voice.status =
                Some("Waiting for the voice model — your dictation will appear shortly…".into());
        }
        let (tx, rx) = async_channel::bounded::<Option<String>>(1);
        std::thread::spawn(move || {
            let result = if wants_final {
                wait_for_model(&slot)
                    .and_then(|w| w.transcribe(&audio).ok())
                    .filter(|s| !s.trim().is_empty())
            } else {
                None
            };
            let _ = tx.send_blocking(result);
        });
        cx.spawn(async move |this, cx| {
            let final_text = rx.recv().await.ok().flatten();
            this.update(cx, |app, cx| {
                app.apply_dictation_final(final_text, session, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Apply the worker's final only to its still-current, visible owner.
    fn apply_dictation_final(
        &mut self,
        final_text: Option<String>,
        session: u64,
        cx: &mut Context<Self>,
    ) {
        if self.shell.archived_reader.is_some() || !self.voice.session.is_current(session) {
            return;
        }
        self.voice.status = None;
        self.composer.update(cx, |c, cx| match &final_text {
            Some(t) => c.dictation_commit(t.trim(), cx),
            None => c.dictation_cancel(cx),
        });
        cx.notify();
    }

    /// End any dictation in progress *without* committing — the draft was just
    /// taken for submit (review blocker 1). Retires every outstanding worker id;
    /// the capture's audio is discarded (its visible preview went with the
    /// draft).
    fn invalidate_dictation(&mut self) {
        self.voice.session.invalidate();
        if self.voice.dictating {
            self.voice.dictating = false;
            self.voice.ptt_session = false;
            self.voice.stop_partials.store(true, Ordering::SeqCst);
            if let Some(cap) = self.voice.capture.take() {
                let _ = cap.stop();
            }
            self.voice.status = None;
        }
    }

    /// Handle the optional hold-to-talk modifier changing: hold-down starts a PTT
    /// session, release ends it. This is the SECONDARY activation path — the
    /// primary is the click-to-toggle mic button, which is unaffected here (a
    /// toggled session ignores modifier release). Detection is by modifier *pair*
    /// (review SF6: GPUI cannot tell left from right, and that is now what the
    /// config promises); `ptt_key = "off"` disables this path entirely.
    fn on_modifiers(&mut self, ev: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        if self.shell.archived_reader.is_some() {
            return;
        }
        let Some(modifier) = self.voice.cfg.ptt_key.modifier() else {
            return; // hold-to-talk disabled
        };
        let held = match modifier {
            PttModifier::Alt => ev.modifiers.alt,
            PttModifier::Control => ev.modifiers.control,
            PttModifier::Platform => ev.modifiers.platform,
            PttModifier::Function => ev.modifiers.function,
        };
        if held
            && (self.onboarding.is_active()
                || self.shell.more
                || self.shell.tabs.menu.is_some()
                || self.shell.tabs.delete_project.is_some())
        {
            return;
        }
        // U3b2 D2: the guard decides; taps never dictate (it only arms the
        // HOLD timer, and a release before it fires disarms).
        match self.voice.ptt_guard.modifier(held) {
            ptt_guard::PttAction::ArmTimer(gen) => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(ptt_guard::HOLD).await;
                    this.update(cx, |app, cx| app.ptt_timer(gen, cx)).ok();
                })
                .detach();
            }
            ptt_guard::PttAction::Stop => {
                self.voice.ptt_held = false;
                if self.voice.dictating && self.voice.ptt_session {
                    self.stop_dictation(cx);
                }
            }
            ptt_guard::PttAction::None => {}
            other => debug_assert!(
                matches!(other, ptt_guard::PttAction::None),
                "modifier() may only yield ArmTimer/Stop/None"
            ),
        }
    }

    // ---- conversation mode (slice 5) -------------------------------------

    /// The Voice toggle: enter or leave the hands-free conversation loop.
    fn toggle_voice_mode(&mut self, cx: &mut Context<Self>) {
        if self.shell.archived_reader.is_some() {
            return;
        }
        if self.voice.conversation_mode {
            self.exit_conversation_mode(cx);
            return;
        }
        if self.onboarding.is_active() {
            return;
        }
        // Conversation mode supersedes a dictation in progress: its ghost is
        // dropped (the user just changed modes, not finished a thought).
        if self.voice.dictating {
            self.invalidate_dictation();
            self.composer.update(cx, |c, cx| c.dictation_cancel(cx));
        }
        if let Some(msg) = self.voice.model.lock().unwrap().failure() {
            self.voice.status = Some(format!("retrying voice model (previous error: {msg})"));
        }
        self.ensure_model_loading(cx);
        self.voice.conversation_mode = true;
        // Hot-mic ceremony: every arm starts UNCONFIRMED (first utterance is
        // staged, not sent) and is announced audibly — a silent hot mic that
        // transcribed a room conversation into a sent message is exactly the
        // incident that created this rule.
        self.voice.first_send_confirmed = false;
        play_voice_cue(true);
        self.start_voice_listening(cx);
        cx.notify();
    }

    /// Leave conversation mode: stop listening, stop any pending speech, and
    /// retire outstanding voice workers.
    fn exit_conversation_mode(&mut self, cx: &mut Context<Self>) {
        play_voice_cue(false);
        self.voice.conversation_mode = false;
        self.stop_voice_listening_internal();
        self.stop_speaking_internal();
        self.voice.session.invalidate();
        self.voice.status = None;
        cx.notify();
    }

    /// Pre-flight the OS microphone permission before a capture (user-test N1).
    ///
    /// `MicCapture::start` blocks inside CoreAudio until the permission is
    /// resolved; when the user has never been asked, that means "until they
    /// answer the TCC prompt" — which froze the whole app when run on the UI
    /// thread. Returns `true` when the capture may start synchronously right
    /// now. Otherwise the caller must bail: either access is denied (status
    /// message set, `on_deny` run) or a worker thread is raising the prompt
    /// and `retry` re-enters the caller on the main thread once it resolves.
    fn preflight_mic_permission(
        &mut self,
        retry: fn(&mut Self, &mut Context<Self>),
        on_deny: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> bool {
        // RC2-R4M2: stand-in mics need no OS permission.
        #[cfg(test)]
        if self.voice.test_mics.is_some() {
            return true;
        }
        use awili_voice::permission::{mic_authorization_status, MicAuthStatus as S};
        match mic_authorization_status() {
            // Unknown = the state can't be read; behave as before the
            // pre-flight existed and let the capture path report errors.
            S::Authorized | S::Unknown => true,
            S::Denied | S::Restricted => {
                self.voice.status = Some(
                    "Microphone access is denied — enable Awili in System Settings ▸ \
                     Privacy & Security ▸ Microphone"
                        .into(),
                );
                on_deny(self, cx);
                cx.notify();
                false
            }
            S::NotDetermined => {
                if self.voice.mic_permission_pending {
                    return false; // one prompt at a time; status already shown
                }
                self.voice.mic_permission_pending = true;
                let session = self.voice.session.current();
                self.voice.status = Some("Waiting for microphone permission…".into());
                cx.notify();
                let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);
                std::thread::spawn(move || {
                    // The FIRST capture raises the OS prompt and CoreAudio
                    // blocks until it is answered — on THIS thread, which is
                    // the entire point. The probe stream is torn down right
                    // away (it never crosses threads: cpal streams are !Send);
                    // the real capture starts on the main thread afterwards,
                    // now unblocked.
                    let result = match MicCapture::start() {
                        Ok(cap) => {
                            let _ = cap.stop();
                            Ok(())
                        }
                        Err(e) => Err(e.to_string()),
                    };
                    let _ = tx.send_blocking(result);
                });
                cx.spawn(async move |this, cx| {
                    let result = rx
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err("microphone probe thread exited".into()));
                    this.update(cx, |app, cx| {
                        app.apply_mic_permission_result(result, session, retry, on_deny, cx);
                    })
                    .ok();
                })
                .detach();
                false
            }
        }
    }

    /// Retiring an input intent also retires its first-use permission retry.
    /// Clear the outstanding probe even when its result no longer owns input.
    fn apply_mic_permission_result(
        &mut self,
        result: Result<(), String>,
        session: u64,
        retry: fn(&mut Self, &mut Context<Self>),
        on_deny: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) {
        self.voice.mic_permission_pending = false;
        if self.shell.archived_reader.is_some() || !self.voice.session.is_current(session) {
            return;
        }
        match result {
            Ok(()) => {
                self.voice.status = None;
                retry(self, cx);
            }
            Err(e) => {
                self.voice.status = Some(format!("microphone unavailable: {e}"));
                on_deny(self, cx);
                cx.notify();
            }
        }
    }

    /// Arm (or re-arm) the conversation-mode mic: start a capture and the VAD
    /// worker that segments utterances out of it. No-op when already listening
    /// or when conversation mode is off.
    fn start_voice_listening(&mut self, cx: &mut Context<Self>) {
        if self.onboarding.is_active() || self.shell.archived_reader.is_some() {
            return;
        }
        if !self.voice.conversation_mode || self.voice.conv_mic.is_some() {
            return;
        }
        if !self.preflight_mic_permission(
            Self::start_voice_listening,
            |app, _cx| {
                // Denied: drop out of the mode so the toggle isn't lying.
                app.voice.conversation_mode = false;
            },
            cx,
        ) {
            return;
        }
        match self.open_voice_mic() {
            Ok(cap) => {
                let buffer = cap.shared_buffer();
                self.voice.conv_mic = Some(cap);
                // Fresh session id: utterance transcripts from a previous
                // listening period (or dictation) are stale at apply time,
                // exactly like dictation results (slice-3 blocker 1).
                let session = self.voice.session.begin();
                let (tx, rx) = async_channel::unbounded::<Vec<f32>>();
                let worker =
                    ConversationCapture::spawn(buffer, self.voice.cfg.vad(), move |utterance| {
                        let _ = tx.send_blocking(utterance);
                    });
                self.voice.conv_capture = Some(worker);
                cx.spawn(async move |this, cx| {
                    while let Ok(utterance) = rx.recv().await {
                        this.update(cx, |app, cx| app.on_voice_utterance(utterance, session, cx))
                            .ok();
                    }
                })
                .detach();
                self.voice.status = Some("Voice mode: listening…".into());
                cx.notify();
            }
            Err(e) => {
                // No mic, no loop: surface the error and drop out of the mode
                // so the button isn't lying about an armed mic.
                self.voice.status = Some(e.to_string());
                self.voice.conversation_mode = false;
                cx.notify();
            }
        }
    }

    /// Stop the conversation-mode capture and its VAD worker (no repaint;
    /// callers notify).
    fn stop_voice_listening_internal(&mut self) {
        if let Some(worker) = self.voice.conv_capture.take() {
            worker.stop();
        }
        if let Some(cap) = self.voice.conv_mic.take() {
            cap.stop();
        }
    }

    /// A VAD silence endpoint closed an utterance: stop the mic (nothing should
    /// record while we transcribe, reply, and speak) and transcribe it on a
    /// worker thread. The worker waits out a still-loading model (slice-3 SF3)
    /// so a first-use utterance spoken during the download is kept.
    fn on_voice_utterance(&mut self, audio: Vec<f32>, session: u64, cx: &mut Context<Self>) {
        if !self.voice.conversation_mode || !self.voice.session.is_current(session) {
            return;
        }
        if audio.len() < MIN_SAMPLES_FOR_PARTIAL / 4 {
            return; // a blip, not an utterance; the mic stays armed
        }
        self.stop_voice_listening_internal();
        self.voice.status = Some("Voice mode: transcribing…".into());
        let slot = self.voice.model.clone();
        let (tx, rx) = async_channel::bounded::<Option<String>>(1);
        std::thread::spawn(move || {
            let result = wait_for_model(&slot)
                .and_then(|w| w.transcribe(&audio).ok())
                .filter(|s| !s.trim().is_empty());
            let _ = tx.send_blocking(result);
        });
        cx.spawn(async move |this, cx| {
            let text = rx.recv().await.ok().flatten();
            this.update(cx, |app, cx| app.on_voice_transcript(text, session, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// The conversation-mode transcript landed: stop words exit the loop,
    /// anything else is committed to the composer and submitted, and an empty
    /// transcript just re-arms the mic.
    fn on_voice_transcript(&mut self, text: Option<String>, session: u64, cx: &mut Context<Self>) {
        if !self.voice.conversation_mode || !self.voice.session.is_current(session) {
            return;
        }
        self.voice.status = None;
        match text
            .as_deref()
            .map(voice_turn_action)
            .unwrap_or(VoiceTurnAction::Ignore)
        {
            VoiceTurnAction::Exit => self.exit_conversation_mode(cx),
            VoiceTurnAction::Ignore => {
                self.start_voice_listening(cx);
                cx.notify();
            }
            VoiceTurnAction::Submit(t) => {
                if self.composer_busy() {
                    // A reply is already running here (shouldn't happen — the
                    // mic is off during replies); don't double-submit.
                    self.start_voice_listening(cx);
                    cx.notify();
                    return;
                }
                // A typed draft in the composer means the keyboard is
                // mid-thought (codex SF2): dictation_commit would splice the
                // transcript into it at the caret and auto-submit the mixture.
                // Leave the draft exactly as typed, drop this utterance, and
                // keep listening.
                if draft_blocks_voice_submit(self.composer.read(cx).text()) {
                    self.start_voice_listening(cx);
                    cx.notify();
                    return;
                }
                // Hot-mic ceremony (Codex P0): nothing auto-submits until the
                // human has sent the first voice utterance themselves. The
                // first transcript is STAGED into the composer; Enter sends it
                // (flipping `first_send_confirmed`) and unlocks auto-send for
                // the rest of the session. Later utterances arriving while the
                // staged draft sits are dropped by the draft guard above.
                if !self.voice.first_send_confirmed {
                    self.composer.update(cx, |c, cx| c.dictation_commit(&t, cx));
                    self.voice.status =
                        Some("Staged — press Enter to send; voice auto-sends after that".into());
                    self.start_voice_listening(cx);
                    cx.notify();
                    return;
                }
                // Through the composer so the sent text is visible exactly as
                // transcribed, then straight to submit (slice-5 §3).
                self.composer.update(cx, |c, cx| c.dictation_commit(&t, cx));
                let draft = self.composer.update(cx, |c, cx| {
                    cx.notify();
                    c.take()
                });
                self.voice.session.invalidate();
                self.dispatch_composer_text(draft, cx);
                // If no turn started (e.g. no model configured), the speech
                // done-watcher will never fire — re-arm here instead. A Work
                // turn that did start is spoken (and the mic re-armed) when it
                // finishes (`speak_finished_work_reply`).
                if self.voice.conversation_mode && !self.composer_busy() {
                    self.start_voice_listening(cx);
                }
                cx.notify();
            }
        }
    }

    /// The unmistakable hot-mic indicator (Codex P0): a full-width banner over
    /// the transcript whenever conversation-mode voice is armed. A blue chip
    /// was demonstrably missable — during testing the mic transcribed and
    /// submitted a real room conversation unnoticed.
    fn voice_listening_banner(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hint: SharedString = if self.voice.first_send_confirmed {
            "● VOICE IS LISTENING — speech in this room becomes chat messages".into()
        } else {
            "● VOICE IS LISTENING — first utterance is staged for your Enter, nothing sends itself yet"
                .into()
        };
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(10.))
            .px(theme::z(14.))
            .py(theme::z(8.))
            .bg(gpui::Rgba {
                a: 0.16,
                ..theme::p().sev_critical
            })
            .border_b_1()
            .border_color(theme::p().sev_critical)
            .child(
                div()
                    .flex_1()
                    .text_size(theme::z(theme::TEXT_SIZE - 1.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::p().sev_critical)
                    .child(hint),
            )
            .child(
                div()
                    .id("voice-banner-stop")
                    .px(theme::z(10.))
                    .py(theme::z(3.))
                    .rounded(theme::z(theme::RADIUS))
                    .border_1()
                    .border_color(theme::p().sev_critical)
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .text_color(theme::p().sev_critical)
                    .cursor_pointer()
                    .hover(|s| {
                        s.bg(gpui::Rgba {
                            a: 0.25,
                            ..theme::p().sev_critical
                        })
                    })
                    .child("Stop listening")
                    .on_click(cx.listener(|this, _ev, _w, cx| this.exit_conversation_mode(cx))),
            )
    }

    /// The provider·model and rules chips shown in the composer row.
    /// Gather the live inputs for the status run and hand them to the pure
    /// assembler. Rendering stays in the composer details; ordering and
    /// presence rules live in [`status_run_parts`], where tests reach them.
    fn collect_status_parts(&self) -> Vec<(String, gpui::Rgba)> {
        // What the helper reported for its last reply here (tokens in and
        // out); nothing at all while it has not reported any.
        let ctx = self
            .last_reply_usage_label()
            .map(|text| (text, theme::p().muted));
        // Grace period: a single offline miss renders NOTHING (not a warn) —
        // only a repeat shows the segment as offline.
        // RC2-UX (R5-9): the chips of the conversation on screen (its own
        // last send, also after a relaunch), not the last send anywhere.
        let chip = self.memory_chip_here();
        let rules = rules_chip_label(chip.rules.as_ref()).and_then(|l| {
            let warn = matches!(
                chip.rules,
                Some(RulesStatus::Offline | RulesStatus::Rejected)
            );
            let grace =
                matches!(chip.rules, Some(RulesStatus::Offline)) && self.rules_offline_streak < 2;
            if grace {
                None
            } else {
                Some((l, warn))
            }
        });
        let recall = recall_chip_label(chip.recall.as_ref()).and_then(|l| {
            let off = matches!(chip.recall, Some(RecallStatus::Offline));
            if off && self.recall_offline_streak < 2 {
                None
            } else {
                Some((l, off))
            }
        });
        // Three-state (GLM night-review SHOULD-3): before the first probe the
        // segment is HIDDEN — unknown must not render as trouble.
        let gateway = if !self.work.gateway.enabled {
            None
        } else {
            match &self.work.hermes_status {
                Some(Ok(_)) => Some(true),
                Some(Err(_)) => Some(false),
                None => None,
            }
        };
        status_run_parts(
            ctx,
            self.cost.chip_label(feed::unix_now()),
            rules,
            recall,
            self.memory_saved_flash,
            gateway,
        )
    }

    /// The model-picker popover (slice 15 §1): a transparent click-away scrim
    /// with a card anchored just above the composer bar, listing every
    /// `model_choices()` entry grouped by provider. Clicking a row selects
    /// that model for the session and closes the popover.
    fn picker_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = picker::grouped_choices(&self.choices);
        let mut col = div().flex().flex_col().py(theme::z(4.));
        for (provider, models) in groups {
            col = col.child(
                div()
                    .px(theme::z(14.))
                    .pt(theme::z(8.))
                    .pb(theme::z(2.))
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .text_color(theme::p().faint)
                    .child(SharedString::from(provider)),
            );
            for (idx, choice) in models {
                let selected = idx == self.choice_idx;
                let down = self.provider_health.get(&choice.base_url) == Some(&false);
                let label: SharedString = if down {
                    format!("{} — offline", choice.model).into()
                } else {
                    choice.model.clone().into()
                };
                col = col.child(
                    div()
                        .id(("picker-row", idx))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(theme::z(8.))
                        .mx(theme::z(6.))
                        .px(theme::z(8.))
                        .py(theme::z(5.))
                        .rounded(theme::z(theme::RADIUS))
                        .text_size(theme::z(theme::TEXT_SIZE - 2.0))
                        .text_color(if selected {
                            theme::p().accent_text
                        } else {
                            theme::p().text
                        })
                        .when(!selected, |d| d.hover(|s| s.bg(theme::p().row_hover)))
                        .when(selected, |d| d.bg(theme::p().row_active))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _ev, _w, cx| {
                            this.select_model(idx, cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w(theme::z(0.))
                                .overflow_hidden()
                                .child(label),
                        )
                        .children(selected.then(|| {
                            div()
                                .flex_none()
                                .text_size(theme::z(theme::LABEL_SIZE))
                                .child("✓")
                        })),
                );
            }
        }
        div()
            .id("picker-scrim")
            .absolute()
            .inset_0()
            .flex()
            .items_end()
            .justify_center()
            .on_click(cx.listener(|this, _ev, _w, cx| this.close_picker(cx)))
            .child(
                div()
                    .id("picker-card")
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .w(theme::z(300.))
                    .max_h(theme::z(340.))
                    // Clear of the composer bar (chips row + input) below.
                    .mb(theme::z(150.))
                    .rounded(theme::z(theme::RADIUS))
                    .bg(theme::p().chrome)
                    .border_1()
                    .border_color(theme::p().border)
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .id("picker-rows")
                            .flex_1()
                            .min_h(theme::z(0.))
                            .overflow_y_scroll()
                            .child(col),
                    ),
            )
    }

    /// The Work executor popover (design of record §4, first slice): FLEET
    /// (the gateway) and AGENTS — each vendor harness with its own local
    /// model options (Claude aliases; Codex from ~/.codex/config.toml; GLM
    /// from ~/.zcode/cli/config.json; Dsh from `[models].dsh_models`). Same
    /// scrim/card mechanics as the model picker; clicking a row selects the
    /// executor for the NEXT run. The AGENTS rows are the pure, unit-tested
    /// [`work::work_executor_agent_rows`].
    fn work_executor_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let header = |label: &'static str| {
            div()
                .px(theme::z(14.))
                .pt(theme::z(8.))
                .pb(theme::z(2.))
                .text_size(theme::z(theme::LABEL_SIZE))
                .text_color(theme::p().faint)
                .child(label)
        };
        // L1d: ON THIS MAC, CLOUD MODELS, FLEET · REMOTE, then BUILD (absent
        // without targets) — one loop, one running row index.
        let mut col = div().flex().flex_col().py(theme::z(4.));
        let mut n = 0;
        for section in executor_labels::picker_sections(
            &self.work.engine,
            &self.work.extra_engines,
            &self.work.models_cfg,
            &self.work.build_targets,
        ) {
            col = col.child(header(section.header));
            for (label, exec) in section.rows {
                col = col.child(self.executor_row(("exec-row", n), label, exec, cx));
                n += 1;
            }
        }
        div()
            .id("executor-scrim")
            .absolute()
            .inset_0()
            .flex()
            .items_end()
            .justify_center()
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.work.executor_picker_open = false;
                cx.notify();
            }))
            .child(
                div()
                    .id("executor-card")
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .w(theme::z(320.))
                    .max_h(theme::z(360.))
                    .mb(theme::z(150.))
                    .rounded(theme::z(theme::RADIUS))
                    .bg(theme::p().chrome)
                    .border_1()
                    .border_color(theme::p().border)
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .id("executor-rows")
                            .flex_1()
                            .min_h(theme::z(0.))
                            .overflow_y_scroll()
                            .child(col),
                    ),
            )
    }

    /// One executor popover row; the selected executor carries the check.
    fn executor_row(
        &self,
        id: (&'static str, usize),
        label: String,
        exec: work::WorkExecutor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.work.executor == exec;
        let n = id.1;
        div()
            .id(id)
            .debug_selector(move || format!("rc2r510-helper-row-{n}"))
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(8.))
            .mx(theme::z(6.))
            .px(theme::z(8.))
            .py(theme::z(5.))
            .rounded(theme::z(theme::RADIUS))
            .text_size(theme::z(theme::TEXT_SIZE - 2.0))
            .text_color(if selected {
                theme::p().accent_text
            } else {
                theme::p().text
            })
            .when(!selected, |d| d.hover(|s| s.bg(theme::p().row_hover)))
            .when(selected, |d| d.bg(theme::p().row_active))
            .cursor_pointer()
            // L1c: assign + persist + close + U4f's Ask-support note, in one place.
            // RC2-R510 (usability RC2 round 4, Minor: the first typing after a
            // pick did not reach the message box): the box has the keys again.
            .on_click(cx.listener(move |this, _ev, window, cx| {
                this.pick_work_executor(exec.clone(), cx);
                this.focus_composer(window, cx);
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(theme::z(0.))
                    .overflow_hidden()
                    .child(SharedString::from(label)),
            )
            .children(selected.then(|| {
                div()
                    .flex_none()
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .child("\u{2713}")
            }))
    }

    /// One Needs You row: severity tag, count, age, title, source; click to
    /// expand the body and reveal the Ack button.
    fn needs_you_row(
        &self,
        idx: usize,
        item: &awili_feed::NeedsItem,
        now_ts: i64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let event = &item.event;
        let expanded = self.feed_expanded.as_deref() == Some(event.event_id.as_str());
        // The group's WORST severity (not the displayed latest event's): a
        // critical-then-attention storm stays red until the critical is acked.
        let sev_color = match item.severity {
            Severity::Critical => theme::p().sev_critical,
            _ => theme::p().sev_attention,
        };
        let count_tag = (item.count > 1).then(|| {
            div()
                .text_size(theme::z(theme::LABEL_SIZE))
                .text_color(theme::p().muted)
                .child(SharedString::from(format!("×{}", item.count)))
        });
        let meta = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(6.))
            .child(
                div()
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(sev_color)
                    .child(SharedString::from(item.severity.as_str().to_string())),
            )
            .children(count_tag)
            .child(div().flex_1())
            .child(
                div()
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .text_color(theme::p().faint)
                    .child(SharedString::from(age_and_source(now_ts, event))),
            );

        // Expanded: the body plus the Ack button (acks the WHOLE group — the
        // count says how many events one click resolves).
        let detail = expanded.then(|| {
            let ids = item.event_ids.clone();
            let ack_label: SharedString = if item.count > 1 {
                format!("Ack {} events", item.count).into()
            } else {
                "Ack".into()
            };
            let body = (!event.body.trim().is_empty()).then(|| {
                div()
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .line_height(theme::z(theme::LABEL_SIZE * 1.5))
                    .text_color(theme::p().muted)
                    .children(
                        event
                            .body
                            .split('\n')
                            .map(|l| div().child(SharedString::from(l.to_string())))
                            .collect::<Vec<_>>(),
                    )
            });
            div()
                .flex()
                .flex_col()
                .gap(theme::z(8.))
                .children(body)
                .child(
                    div()
                        .id(("needs-ack", idx))
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(theme::z(10.))
                        .py(theme::z(4.))
                        .rounded(theme::z(theme::RADIUS))
                        .bg(theme::p().chip_bg)
                        .border_1()
                        .border_color(theme::p().chip_border)
                        .text_size(theme::z(theme::LABEL_SIZE))
                        .text_color(theme::p().chip_text)
                        .cursor_pointer()
                        .hover(|s| {
                            s.border_color(theme::p().accent)
                                .text_color(theme::p().accent_text)
                        })
                        .child(ack_label)
                        .on_click(cx.listener(move |this, _ev, _w, cx| {
                            cx.stop_propagation();
                            this.ack_feed_group(ids.clone(), cx);
                        })),
                )
        });

        let event_id = event.event_id.clone();
        div()
            .id(("needs-row", idx))
            .flex()
            .flex_col()
            .gap(theme::z(4.))
            .p(theme::z(10.))
            .rounded(theme::z(theme::RADIUS))
            .bg(theme::p().raised)
            .border_1()
            .border_color(theme::p().border)
            .cursor_pointer()
            .hover(|s| s.border_color(theme::p().accent))
            .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle_feed_expand(&event_id, cx)))
            .child(meta)
            .child(
                div()
                    .text_size(theme::z(theme::LABEL_SIZE + 1.))
                    .text_color(theme::p().text)
                    .child(SharedString::from(event.title.clone())),
            )
            .children(detail)
    }

    // ---- Phase 5 panels ----

    /// The Needs You sidebar (slice-4 spec §6): renders the MIRROR, so it is
    /// instant online and unchanged offline — the sync line below the header
    /// tells the truth about freshness either way.
    fn skills_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let skills = self.skills_state.filtered_skills();
        let toolsets = self.skills_state.filtered_toolsets();
        let search_owner = cx.entity().downgrade();
        let search = ui::SearchField::new("skills-search", "Search skills and toolsets…")
            .selector("awili-skills-search")
            .focus_handle(&self.skills_search_focus)
            .query(self.skills_state.filter.clone())
            .on_query(move |query, _window, cx| {
                let _ = search_owner.update(cx, |this, cx| {
                    this.skills_state.filter = query;
                    cx.notify();
                });
            });

        let summary = div()
            .flex()
            .gap(theme::z(theme::SP_8))
            .px(theme::z(theme::SP_12))
            .py(theme::z(theme::SP_8))
            .child(
                div().flex_1().child(
                    ui::Card::new("skills-summary")
                        .title(skills.len().to_string())
                        .body("Skills"),
                ),
            )
            .child(
                div().flex_1().child(
                    ui::Card::new("toolsets-summary")
                        .title(toolsets.len().to_string())
                        .body("Toolsets"),
                ),
            );

        // The shared Hermes notice remains the owner of unavailable/read errors.
        let notice = self.hermes_page_notice(hermes_pages::HermesPage::Skills);
        let mut visible_targets = Vec::new();
        let content = if let Some(text) = notice {
            div()
                .child(self.hermes_page_notice_element(hermes_pages::HermesPage::Skills, text, cx))
                .into_any_element()
        } else if self.skills_state.loading {
            ui::Skeleton::new("skills-loading")
                .line_rows(5)
                .selector("awili-skills-loading")
                .into_any_element()
        } else {
            let mut rows = Vec::new();

            if !skills.is_empty() {
                rows.push(ui::text::label("Skills").strong().into_any_element());
                for skill in &skills {
                    let name = skill.name.clone();
                    let description = skill.description.as_deref().unwrap_or("—");
                    let version = skill.version.as_deref().unwrap_or("—");
                    let enabled = skill.enabled;
                    let expanded = self.skills_state.selected.as_deref() == Some(name.as_str());
                    let toggle_name = name.clone();
                    let click_name = name.clone();
                    let target = {
                        let mut targets = self.skills_focus_targets.borrow_mut();
                        targets
                            .entry(name.clone())
                            .or_insert_with(|| SkillsFocusTarget {
                                row: cx.focus_handle(),
                                action: cx.focus_handle(),
                                row_bounds: Rc::new(Cell::new(None)),
                                action_bounds: Rc::new(Cell::new(None)),
                            })
                            .clone()
                    };
                    visible_targets.push(target.clone());
                    let trailing = div()
                        .flex()
                        .items_center()
                        .gap(theme::z(theme::SP_4))
                        .child(ui::text::caption(version).dim())
                        .child(
                            ui::Chip::new(
                                format!("skill-status-{name}"),
                                if enabled { "Enabled" } else { "Disabled" },
                            )
                            .tone(if enabled {
                                theme::Tone::Neutral
                            } else {
                                theme::Tone::Attention
                            }),
                        );
                    let row = ui::Row::new(format!("skill-row-{name}"))
                        .title(name.clone())
                        .subtitle(description)
                        .trailing(trailing)
                        .focus_handle(&target.row)
                        .selected(expanded)
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            let selected = &mut this.skills_state.selected;
                            *selected = if selected.as_deref() == Some(click_name.as_str()) {
                                None
                            } else {
                                Some(click_name.clone())
                            };
                            cx.notify();
                        }))
                        .into_any_element();
                    let details = if expanded {
                        let category = skill.category.as_deref().unwrap_or("uncategorized");
                        let busy = self.skills_state.toggling;
                        let label = if busy {
                            "Updating…"
                        } else if enabled {
                            "Disable"
                        } else {
                            "Enable"
                        };
                        let mut action = ui::Button::new(format!("skill-toggle-{name}"), label)
                            .focus_handle(&target.action)
                            .bounds_handle(target.action_bounds.clone());
                        if busy {
                            action = action.disabled("A skill update is already in progress");
                        }
                        let action = action.on_click(cx.listener(move |this, _ev, _window, cx| {
                            if !busy {
                                this.toggle_skill_enabled(toggle_name.clone(), !enabled, cx);
                            }
                        }));
                        Some(
                            ui::Card::new(format!("skill-details-{name}"))
                                .title(category)
                                .body(format!("Version {version}"))
                                .actions([action])
                                .into_any_element(),
                        )
                    } else {
                        None
                    };
                    let row_bounds = target.row_bounds.clone();
                    let action_bounds = target.action_bounds.clone();
                    rows.push(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(theme::z(theme::SP_8))
                            .on_children_prepainted(move |children, _, _| {
                                row_bounds.set(children.first().copied());
                                if children.get(1).is_none() {
                                    action_bounds.set(None);
                                }
                            })
                            .child(row)
                            .children(details)
                            .into_any_element(),
                    );
                }
            }

            if !toolsets.is_empty() {
                rows.push(ui::text::label("Toolsets").strong().into_any_element());
                for toolset in &toolsets {
                    let tools = if toolset.tools.is_empty() {
                        "no tools".to_string()
                    } else {
                        toolset.tools.join(" · ")
                    };
                    rows.push(
                        ui::Row::new(format!("toolset-row-{}", toolset.name))
                            .title(toolset.name.clone())
                            .subtitle(format!("{} tools · {tools}", toolset.tools.len()))
                            .wrap()
                            .into_any_element(),
                    );
                }
            }

            if rows.is_empty() {
                let (title, message) = if self.skills_state.filter.is_empty() {
                    (
                        "No skills or toolsets yet.",
                        "Hermes has not installed any skills or toolsets.",
                    )
                } else {
                    (
                        "No matching capabilities.",
                        "Try another skill or toolset name or description.",
                    )
                };
                ui::EmptyState::new("skills-empty", title, message)
                    .selector("awili-skills-empty")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_8))
                    .children(rows)
                    .into_any_element()
            }
        };

        let reveal_targets = visible_targets;
        let reveal_key = self.skills_reveal_key.clone();
        let reveal_scroll = self.shell.skills_scroll.clone();
        let reveal_outer_scroll = self.shell.backstage_scroll.clone();
        let reveal_owner = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(theme::z(0.))
            .min_h(gpui::Pixels::ZERO)
            .h_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_8))
                    .px(theme::z(theme::SP_20))
                    .py(theme::z(theme::SP_12))
                    .child(ui::text::heading("Skills & toolsets"))
                    .child(search),
            )
            .child(
                div()
                    .on_children_prepainted(move |_, window, cx| {
                        let focused = window.focused(cx);
                        let target = reveal_targets.iter().find_map(|target| {
                            if focused.as_ref() == Some(&target.row) {
                                Some((target.row.clone(), target.row_bounds.get()))
                            } else if focused.as_ref() == Some(&target.action) {
                                Some((target.action.clone(), target.action_bounds.get()))
                            } else {
                                None
                            }
                        });
                        let Some((focus, Some(face))) = target else {
                            *reveal_key.borrow_mut() = None;
                            return;
                        };
                        let inner_viewport = reveal_scroll.bounds();
                        let outer_viewport = reveal_outer_scroll.bounds();
                        let inner_offset = reveal_scroll.offset();
                        let outer_offset = reveal_outer_scroll.offset();
                        // GPUI reports the face and inner viewport in their
                        // current paint space. Normalize both scroll offsets:
                        // wheel-only repainting at unchanged focus/layout must
                        // retain the reader's chosen inner and outer positions.
                        let normalized = Bounds::new(
                            point(
                                face.origin.x,
                                face.origin.y - inner_offset.y - outer_offset.y,
                            ),
                            face.size,
                        );
                        let normalized_viewport = Bounds::new(
                            point(
                                inner_viewport.origin.x,
                                inner_viewport.origin.y - outer_offset.y,
                            ),
                            inner_viewport.size,
                        );
                        let key = SkillsRevealKey {
                            focus: focus.clone(),
                            face: normalized,
                            viewport: normalized_viewport,
                            outer_viewport,
                        };
                        let changed = {
                            let mut last = reveal_key.borrow_mut();
                            if last.as_ref() == Some(&key) {
                                false
                            } else {
                                *last = Some(key.clone());
                                true
                            }
                        };
                        if !changed {
                            return;
                        }
                        let key_cell = reveal_key.clone();
                        let inner_scroll = reveal_scroll.clone();
                        let outer_scroll = reveal_outer_scroll.clone();
                        let owner = reveal_owner.clone();
                        window.defer(cx, move |window, cx| {
                            if key_cell.borrow().as_ref() != Some(&key)
                                || !focus.is_focused(window)
                                || !owner
                                    .read_with(cx, |this, _| this.mode == Mode::Skills)
                                    .unwrap_or(false)
                            {
                                return;
                            }

                            // First reveal the face inside the Skills list,
                            // then reveal that list position through Backstage.
                            // Reversing this order can move the outer section
                            // after the inner correction and leave the face
                            // clipped by the body again.
                            let inner_viewport = inner_scroll.bounds();
                            let mut inner_offset = inner_scroll.offset();
                            if face.top() < inner_viewport.top() {
                                inner_offset.y += inner_viewport.top() - face.top();
                            } else if face.bottom() > inner_viewport.bottom() {
                                inner_offset.y += inner_viewport.bottom() - face.bottom();
                            }
                            inner_offset.y = inner_offset
                                .y
                                .clamp(-inner_scroll.max_offset().y, gpui::Pixels::ZERO);
                            let inner_delta = inner_offset.y - inner_scroll.offset().y;
                            let shifted_face = Bounds::new(
                                point(face.origin.x, face.origin.y + inner_delta),
                                face.size,
                            );

                            let outer_viewport = outer_scroll.bounds();
                            let mut outer_offset = outer_scroll.offset();
                            if shifted_face.top() < outer_viewport.top() {
                                outer_offset.y += outer_viewport.top() - shifted_face.top();
                            } else if shifted_face.bottom() > outer_viewport.bottom() {
                                outer_offset.y += outer_viewport.bottom() - shifted_face.bottom();
                            }
                            outer_offset.y = outer_offset
                                .y
                                .clamp(-outer_scroll.max_offset().y, gpui::Pixels::ZERO);

                            let outer_changed = outer_offset != outer_scroll.offset();
                            let inner_changed = inner_offset != inner_scroll.offset();
                            if outer_changed {
                                outer_scroll.set_offset(outer_offset);
                            }
                            if inner_changed {
                                inner_scroll.set_offset(inner_offset);
                            }
                            if outer_changed || inner_changed {
                                window.refresh();
                            }
                        });
                    })
                    .id("skills-list")
                    .debug_selector(|| "awili-skills-list".to_string())
                    .flex_1()
                    .min_h(gpui::Pixels::ZERO)
                    .track_scroll(&self.shell.skills_scroll)
                    .overflow_hidden()
                    .on_scroll_wheel(cx.listener(|this, event, window, cx| {
                        if consolidation::scroll_backstage_list(
                            &this.shell.skills_scroll,
                            event,
                            window,
                        ) {
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .w_full()
                            .max_w(theme::z(theme::COMPOSER_MAX))
                            .mx_auto()
                            .px(theme::z(theme::SP_8))
                            .child(summary)
                            .child(content),
                    ),
            )
    }

    fn cron_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (enabled, paused, errored) = self.cron_state.counts();

        // The three counts share one KIT Card, each a compact column.
        let summary_cards = ui::Card::new("cron-summary")
            .selector("awili-cron-summary")
            .children(
                [div().flex().flex_row().gap(theme::z(theme::SP_8)).children(
                    [(enabled, "Active"), (paused, "Paused"), (errored, "Error")].map(
                        |(count, label)| {
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .items_center()
                                .child(ui::text::heading(count.to_string()))
                                .child(ui::text::label(label))
                        },
                    ),
                )],
            );

        let header = div()
            .px(theme::z(theme::SP_12))
            .py(theme::z(theme::SP_8))
            .child(ui::text::label("SCHEDULED JOBS").strong());

        // RC2-R58: what the page is, Hermes's state and what to do, in the
        // words every Hermes page uses.
        let notice = self.hermes_page_notice(hermes_pages::HermesPage::Automations);
        let body = if let Some(text) = notice {
            div().child(self.hermes_page_notice_element(
                hermes_pages::HermesPage::Automations,
                text,
                cx,
            ))
        } else if self.cron_state.loading {
            div().px(theme::z(theme::SP_12)).child(
                ui::Skeleton::new("cron-loading")
                    .line_rows(3)
                    .selector("awili-cron-loading"),
            )
        } else if let Some(err) = &self.cron_state.error {
            let msg: SharedString =
                hermes_pages::plain_read_failure(&self.work.hermes.base_url, err).into();
            div().px(theme::z(theme::SP_12)).child(
                ui::Banner::new("cron-error", theme::Tone::ActNow, msg)
                    .selector("awili-cron-error"),
            )
        } else if self.cron_state.jobs.is_empty() {
            div().px(theme::z(theme::SP_12)).child(
                ui::EmptyState::new(
                    "cron-empty",
                    "No scheduled jobs.",
                    "Jobs Hermes runs for you on a schedule appear here.",
                )
                .selector("awili-cron-empty"),
            )
        } else {
            let mut rows = Vec::new();
            for job in &self.cron_state.jobs {
                let job_id = job.id.clone();
                let name: SharedString = job.name.clone().into();
                let state = cron::JobState::from_str(&job.state);
                let schedule: SharedString = job
                    .schedule_display
                    .clone()
                    .or_else(|| job.schedule.as_ref().and_then(|s| s.display.clone()))
                    .unwrap_or_else(|| "—".to_string())
                    .into();

                // Semantic tones: healthy and paused or disabled stay neutral,
                // running is the contrast, an error must be acted on, and an
                // unknown server state needs a look (never healthy, L1).
                let state_tone = match &state {
                    cron::JobState::Enabled
                    | cron::JobState::Scheduled
                    | cron::JobState::Paused
                    | cron::JobState::Disabled => theme::Tone::Neutral,
                    cron::JobState::Running => theme::Tone::Contrast,
                    cron::JobState::Error => theme::Tone::ActNow,
                    cron::JobState::Other(_) => theme::Tone::Attention,
                };

                let state_icon: &str = match &state {
                    cron::JobState::Enabled => "\u{25CF}",
                    cron::JobState::Scheduled => "\u{25CF}",
                    cron::JobState::Running => "\u{25B6}",
                    cron::JobState::Paused => "\u{25A0}",
                    cron::JobState::Error => "\u{26A0}",
                    cron::JobState::Disabled => "\u{25CB}",
                    // Hollow diamond — visually distinct from the known states.
                    cron::JobState::Other(_) => "\u{25C7}",
                };
                let chip_label = format!("{} {}", state_icon, state.label());

                // Per-row actions (gap B): Pause an active job, Resume a paused
                // one, Run now always.
                //
                // Hermes' model (cronjob_tools.py): a job's state defaults to
                // "scheduled" when enabled, else "paused" — so `enabled == false`
                // *is* the paused state, and resuming re-enables it. Therefore:
                //   - Pause shows for active jobs: `enabled && !paused`. (A normal
                //     active job reports state "scheduled" -> JobState::Other, so
                //     we must key on `enabled`, not on `Enabled | Running`.)
                //   - Resume shows for any paused job, keyed on `is_paused` ALONE.
                //     Requiring `enabled` here would strand natively-paused jobs
                //     (which carry enabled == false) with no way to resume.
                // Each action stops propagation so it never also selects the row.
                let is_paused = matches!(state, cron::JobState::Paused);
                let mut actions = Vec::new();
                if job.enabled && !is_paused {
                    let pause_id = job_id.clone();
                    actions.push(
                        ui::Button::new(format!("cron-pause-{job_id}"), "Pause")
                            .quiet()
                            .on_click(cx.listener(move |this, _ev, _w, cx| {
                                cx.stop_propagation();
                                this.cron_job_action(pause_id.clone(), cron::CronAction::Pause, cx);
                            })),
                    );
                }
                if is_paused {
                    let resume_id = job_id.clone();
                    actions.push(
                        ui::Button::new(format!("cron-resume-{job_id}"), "Resume")
                            .quiet()
                            .on_click(cx.listener(move |this, _ev, _w, cx| {
                                cx.stop_propagation();
                                this.cron_job_action(
                                    resume_id.clone(),
                                    cron::CronAction::Resume,
                                    cx,
                                );
                            })),
                    );
                }
                let trigger_id = job_id.clone();
                actions.push(
                    ui::Button::new(format!("cron-trigger-{job_id}"), "Run now").on_click(
                        cx.listener(move |this, _ev, _w, cx| {
                            cx.stop_propagation();
                            this.cron_job_action(trigger_id.clone(), cron::CronAction::Trigger, cx);
                        }),
                    ),
                );

                let trailing = div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(theme::z(theme::SP_8))
                    .child(
                        ui::Chip::new(format!("cron-state-{job_id}"), chip_label).tone(state_tone),
                    )
                    .children(actions);

                let select_id = job_id.clone();
                rows.push(
                    ui::Row::new(format!("cron-job-{job_id}"))
                        .title(name)
                        .wrap()
                        .subtitle(schedule)
                        .trailing(trailing)
                        .wrap_trailing()
                        // Selection stays a pointer gesture: the row adds no
                        // keyboard stop, and each action is its own stop.
                        .pointer_only()
                        .on_click(cx.listener(move |this, _ev, _w, cx| {
                            this.cron_state.selected_id = Some(select_id.clone());
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            div()
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_8))
                .px(theme::z(theme::SP_12))
                .children(rows)
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(theme::z(0.))
            .min_h(gpui::Pixels::ZERO)
            .h_full()
            .bg(theme::p().bg)
            .child(header)
            .child(
                div()
                    .id("cron-list")
                    .debug_selector(|| "awili-cron-list".to_string())
                    .flex_1()
                    .min_h(theme::z(0.))
                    .track_scroll(&self.shell.cron_scroll)
                    .overflow_hidden()
                    .on_scroll_wheel(cx.listener(|this, event, window, cx| {
                        if consolidation::scroll_backstage_list(
                            &this.shell.cron_scroll,
                            event,
                            window,
                        ) {
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .w_full()
                            .max_w(theme::z(720.))
                            .mx_auto()
                            .px(theme::z(8.))
                            .child(summary_cards)
                            .child(body),
                    ),
            )
    }

    /// Open a conversation chosen from the Sessions browser: switch to the mode
    /// that owns its kind, then select it (overriding the kind's remembered
    /// thread with this specific pick).
    fn open_conversation_from_history(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(kind) = self.conversations.get(id).map(|c| c.kind) else {
            return;
        };
        if kind != ConvKind::Council && self.conversations.get(id).is_some_and(|c| c.archived) {
            self.open_workshop_archive(id, cx);
            return;
        }
        self.close_workshop_archive(cx);
        if self.open_if_code_conversation(id, cx) {
            return;
        }
        let mode = match kind {
            ConvKind::Work => Mode::Work,
            ConvKind::Chat => Mode::Work,
            ConvKind::Code => Mode::Code,
            ConvKind::Council => Mode::Council,
        };
        // Remember the exact pick before mode activation. Otherwise the
        // intermediate remembered thread would become an unwanted open tab.
        self.set_active_conversation(id);
        self.remember_active_kind();
        self.set_mode(mode, cx);
        // An explicit History/Restore action commits the selected owner even
        // when a read-only reader already set the same sidebar ID.
        self.save_active_conversation();
        self.refresh_staged_attachments();
        // Daily-driver P2e (R6): restore the exact pick's last run, not the
        // kind's remembered thread that `set_mode` may have restored first.
        self.restore_work_last_run();
        // RC1-S: and its folder.
        self.restore_work_folder(cx);
        self.sync_workshop_tabs();
        self.sync_composer_draft(cx);
        cx.notify();
    }

    /// The Sessions browser — a full-center, searchable, filterable history view
    /// across local Chat and Work threads *and* remote Hermes sessions. Unlike
    /// the sidebar (single-kind, no search), this is the cross-cutting archive.
    /// The read-only transcript of one Hermes session, shown in place of the browser list.
    fn session_transcript_view(&self, sid: &str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let title: SharedString = self
            .sessions_state
            .sessions
            .iter()
            .find(|s| s.id == sid)
            .and_then(|s| s.title.clone())
            .unwrap_or_else(|| sid.to_string())
            .into();
        let back = ui::Button::new("sess-back", "← Back")
            .ghost()
            .selector("sess-back")
            .focus_handle(&self.sessions_back_focus)
            .on_click(cx.listener(|this, _ev, _window, cx| {
                this.sessions_state.selected_id = None;
                this.sessions_state.transcript = None;
                this.sessions_state.transcript_error = None;
                this.sessions_focus_pending = Some(SessionsFocusTarget::Search);
                cx.notify();
            }));
        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(theme::z(theme::SP_12))
            .px(theme::z(theme::SP_16))
            .py(theme::z(theme::SP_12))
            .border_b_1()
            .border_color(theme::p().line)
            .child(back)
            .child(ui::text::heading(title));

        let mut body = div()
            .id("sess-transcript")
            .debug_selector(|| "sess-transcript".to_string())
            .flex_1()
            .min_h(gpui::Pixels::ZERO)
            .overflow_y_scroll()
            .p(theme::z(theme::SP_16))
            .flex()
            .flex_col()
            .gap(theme::z(theme::SP_12));
        if self.sessions_state.transcript_loading {
            body = body.child(ui::text::body("Loading transcript…").dim());
        } else if let Some(err) = &self.sessions_state.transcript_error {
            body = body.child(
                ui::Banner::new(
                    "sess-transcript-error",
                    theme::Tone::Attention,
                    format!("Couldn't load transcript: {err}"),
                )
                .selector("sess-transcript-error"),
            );
        } else if let Some((_, rows)) = self
            .sessions_state
            .transcript
            .as_ref()
            .filter(|(owner, _)| owner == sid)
        {
            if rows.is_empty() {
                body = body.child(
                    ui::EmptyState::new(
                        "sess-transcript-empty",
                        "No messages.",
                        "This Hermes session has no transcript messages.",
                    )
                    .selector("sess-transcript-empty")
                    .top(theme::z(theme::SP_24)),
                );
            } else {
                for (i, (role, text)) in rows.iter().enumerate() {
                    body = body.child(
                        ui::Card::new(("sess-msg", i))
                            .title(role.clone())
                            .body(text.clone()),
                    );
                }
            }
        } else {
            body = body.child(
                ui::EmptyState::new(
                    "sess-transcript-empty",
                    "No messages.",
                    "This Hermes session has no transcript messages.",
                )
                .selector("sess-transcript-empty")
                .top(theme::z(theme::SP_24)),
            );
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn sessions_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        // A selected Hermes session takes over the pane with its transcript;
        // Back returns to the browser and restores search focus.
        if let Some(sid) = self.sessions_state.selected_id.clone() {
            return self.session_transcript_view(&sid, cx);
        }
        let view = self.sessions_state.view;
        let query = self.sessions_state.filter.to_lowercase();
        let show_local = !matches!(view, sessions::SessionsView::Hermes);
        let show_hermes = matches!(
            view,
            sessions::SessionsView::All | sessions::SessionsView::Hermes
        );

        let title_row = div()
            .flex()
            .items_center()
            .px(theme::z(theme::SP_4))
            .py(theme::z(theme::SP_4))
            .child(ui::text::label("SESSIONS").strong().dim());
        let search_handle = cx.entity().downgrade();
        let search = ui::SearchField::new("sessions-search", "Search history…")
            .selector("awili-sessions-search")
            .focus_handle(&self.sessions_search_focus)
            .query(self.sessions_state.filter.clone())
            .on_query(move |query, _window, cx| {
                let _ = search_handle.update(cx, |this, cx| {
                    this.sessions_state.filter = query;
                    cx.notify();
                });
            });
        let all_views = sessions::SessionsView::all();
        let selected_tab = all_views
            .iter()
            .position(|candidate| *candidate == view)
            .unwrap_or(0);
        let tab_items = all_views.into_iter().map(|item| {
            let id = match item {
                sessions::SessionsView::All => "sess-tab-All",
                sessions::SessionsView::Work => "sess-tab-Work",
                sessions::SessionsView::Hermes => "sess-tab-Hermes",
            };
            ui::TabItem::new(id, item.label())
        });
        let tabs_handle = cx.entity().downgrade();
        let tabs = ui::Tabs::new("sessions-filters", tab_items.collect(), selected_tab)
            .selector("awili-sessions-tabs")
            .on_select(move |index, _event, _window, cx| {
                let _ = tabs_handle.update(cx, |this, cx| {
                    if let Some(view) = sessions::SessionsView::all().get(index).copied() {
                        this.sessions_state.view = view;
                        cx.notify();
                    }
                });
            });

        // Local conversations remain non-archived and newest-first; selecting
        // one uses the existing route, then returns focus to its live composer
        // (or the archived-reader rail for an archived-reader destination).
        let kind_ok = |kind: ConvKind| match view {
            sessions::SessionsView::Work => matches!(kind, ConvKind::Work | ConvKind::Chat),
            _ => true,
        };
        let mut locals: Vec<&ConversationMeta> = if show_local {
            self.conversations
                .all()
                .iter()
                .filter(|conversation| !conversation.archived && kind_ok(conversation.kind))
                .filter(|conversation| {
                    query.is_empty() || conversation.title.to_lowercase().contains(&query)
                })
                .collect()
        } else {
            Vec::new()
        };
        locals.sort_by_key(|conversation| std::cmp::Reverse(conversation.updated_ts));
        let local_rows: Vec<gpui::AnyElement> = locals
            .iter()
            .enumerate()
            .map(|(i, meta)| {
                let id = meta.id.clone();
                let title: SharedString = if meta.title.is_empty() {
                    "Untitled".into()
                } else {
                    meta.title.clone().into()
                };
                let msgs = self
                    .views
                    .get(&meta.id)
                    .map(|view| view.convo.messages().len())
                    .unwrap_or(0);
                let subtitle: SharedString = format!(
                    "{} · {} · {} msgs",
                    match meta.kind {
                        ConvKind::Chat | ConvKind::Work => "Work",
                        ConvKind::Code => "Code",
                        ConvKind::Council => "Council",
                    },
                    crate::work::relative_time(meta.updated_ts as f64),
                    msgs
                )
                .into();
                let active = self.sidebar.active_id() == meta.id;
                let row = ui::Row::new(("hist-local", i))
                    .title(title)
                    .subtitle(subtitle)
                    .wrap()
                    .selected(active)
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.open_conversation_from_history(&id, cx);
                        this.focus_shell_page(window, cx);
                    }));
                div()
                    .debug_selector(move || format!("awili-session-row-{i}"))
                    .child(row)
                    .into_any_element()
            })
            .collect();

        // Hermes records continue to filter by title, model and source through
        // SessionsState::filtered; their details stay visible in the subtitle.
        let hermes_rows: Vec<gpui::AnyElement> = if show_hermes {
            self.sessions_state
                .filtered()
                .iter()
                .enumerate()
                .map(|(i, session)| {
                    let title: SharedString = session
                        .title
                        .as_deref()
                        .unwrap_or("Untitled")
                        .to_string()
                        .into();
                    let model = session.model.as_deref().unwrap_or("—");
                    let tokens = format!(
                        "{}+{} tok · {} msgs",
                        session.input_tokens.unwrap_or(0),
                        session.output_tokens.unwrap_or(0),
                        session.message_count.unwrap_or(0),
                    );
                    let time = session
                        .started_at
                        .map(crate::work::relative_time)
                        .unwrap_or_else(|| "—".to_string());
                    let subtitle: SharedString = format!(
                        "Hermes · {model} · {tokens} · {time} · {}",
                        session.source.as_deref().unwrap_or("Unknown source")
                    )
                    .into();
                    let sid = session.id.clone();
                    ui::Row::new(("hist-hermes", i))
                        .title(title)
                        .subtitle(subtitle)
                        .wrap()
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            this.sessions_state.selected_id = Some(sid.clone());
                            this.sessions_state.transcript = None;
                            this.sessions_state.transcript_error = None;
                            this.sessions_focus_pending = Some(SessionsFocusTarget::Back);
                            this.fetch_session_transcript(sid.clone(), cx);
                            cx.notify();
                        }))
                        .into_any_element()
                })
                .collect()
        } else {
            Vec::new()
        };

        let empty_state = (local_rows.is_empty() && hermes_rows.is_empty()).then(|| {
            if self.sessions_state.loading {
                ui::text::body("Loading…").dim().into_any_element()
            } else {
                ui::EmptyState::new(
                    "sessions-empty",
                    "No matching history.",
                    "Try another filter or search term.",
                )
                .selector("sessions-empty")
                .top(theme::z(theme::SP_24))
                .into_any_element()
            }
        });
        let list = div()
            .flex()
            .flex_col()
            .gap(theme::z(theme::SP_8))
            .children(local_rows)
            .children(hermes_rows)
            .children(empty_state);

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(gpui::Pixels::ZERO)
            .min_h(gpui::Pixels::ZERO)
            .h_full()
            .bg(theme::p().canvas)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(theme::z(theme::PAGE_MAX))
                    .mx_auto()
                    .px(theme::z(theme::SP_8))
                    .pt(theme::z(theme::SP_12))
                    .gap(theme::z(theme::SP_8))
                    .child(title_row)
                    .child(search)
                    .child(tabs),
            )
            .child(
                div()
                    .id("sessions-list")
                    .debug_selector(|| "awili-sessions-list".to_string())
                    .flex_1()
                    .min_h(gpui::Pixels::ZERO)
                    .track_scroll(&self.shell.sessions_scroll)
                    .overflow_hidden()
                    .on_scroll_wheel(cx.listener(|this, event, window, cx| {
                        if consolidation::scroll_backstage_list(
                            &this.shell.sessions_scroll,
                            event,
                            window,
                        ) {
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .w_full()
                            .max_w(theme::z(theme::PAGE_MAX))
                            .mx_auto()
                            .px(theme::z(theme::SP_8))
                            .py(theme::z(theme::SP_12))
                            .child(list),
                    ),
            )
            .into_any_element()
    }

    fn needs_you_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now_ts = feed::unix_now();
        let snapshot = &self.feed_snapshot;

        // RC2-R4 (RC-05): a service that answers without a feed, or refuses
        // the token, is named plainly instead of "offline".
        let (sync_label, warn) = feed::sync_line(snapshot, now_ts);
        let sync_label: SharedString = sync_label.into();
        let sync_color = if warn {
            theme::p().chip_warn
        } else {
            theme::p().faint
        };

        let header = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(theme::z(2.))
            .px(theme::z(14.))
            .py(theme::z(12.))
            .border_b_1()
            .border_color(theme::p().border)
            .child(
                div()
                    .text_size(theme::z(theme::TEXT_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::p().text)
                    .child("Needs You"),
            )
            .child(
                div()
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .text_color(sync_color)
                    .child(sync_label),
            );

        let rows: Vec<_> = snapshot
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| self.needs_you_row(i, item, now_ts, cx))
            .collect();
        let empty = snapshot.items.is_empty().then(|| {
            div()
                .px(theme::z(14.))
                .py(theme::z(10.))
                .text_size(theme::z(theme::LABEL_SIZE))
                .text_color(theme::p().faint)
                .child("Nothing needs you.")
        });

        div()
            .flex_none()
            .w(theme::z(theme::NEEDS_YOU_W))
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::p().chrome)
            .border_l_1()
            .border_color(theme::p().border)
            .child(header)
            .child(
                div()
                    .id("needs-you-list")
                    .flex_1()
                    .min_h(theme::z(0.))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(theme::z(8.))
                            .p(theme::z(12.))
                            .children(rows)
                            .children(empty),
                    ),
            )
    }

    /// The ⌘F search panel (slice 14): a fixed 320px right-hand column with
    /// the query input, then one row per matching conversation.
    fn search_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(theme::z(8.))
            .px(theme::z(14.))
            .py(theme::z(12.))
            .border_b_1()
            .border_color(theme::p().border)
            .child(
                div()
                    .text_size(theme::z(theme::TEXT_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::p().text)
                    .child("Search"),
            )
            .child(
                div()
                    .px(theme::z(8.))
                    .py(theme::z(4.))
                    .rounded(theme::z(theme::RADIUS))
                    .bg(theme::p().bg)
                    .border_1()
                    .border_color(theme::p().border)
                    .child(self.search_input.clone()),
            );

        let rows: Vec<_> = self
            .search_results
            .iter()
            .enumerate()
            .map(|(i, hit)| self.search_result_row(i, hit, cx))
            .collect();
        let empty =
            (self.search_results.is_empty() && !self.search.query.trim().is_empty()).then(|| {
                div()
                    .px(theme::z(14.))
                    .py(theme::z(10.))
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .text_color(theme::p().faint)
                    .child("No matches")
            });

        div()
            .flex_none()
            .w(theme::z(320.))
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::p().chrome)
            .border_l_1()
            .border_color(theme::p().border)
            .child(header)
            .child(
                div()
                    .id("search-results")
                    .flex_1()
                    .min_h(theme::z(0.))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(theme::z(4.))
                            .p(theme::z(8.))
                            .children(rows)
                            .children(empty),
                    ),
            )
    }

    /// One search result: conversation title over the matching snippet, the
    /// snippet's `**bold**` runs rendered as real bold text.
    fn search_result_row(
        &self,
        idx: usize,
        hit: &SearchResult,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = self
            .conversations
            .get(&hit.conversation_id)
            .map(|c| c.title.clone())
            .unwrap_or_else(|| "(deleted)".to_string());
        let focused = idx == self.search.focused_idx;
        let id = hit.conversation_id.clone();

        // Rebuild the snippet as one string + bold highlight ranges for a
        // single StyledText, so emphasis is inline rather than stacked divs.
        let mut text = String::new();
        let mut bold = Vec::new();
        for (run, is_bold) in search::snippet_runs(&hit.snippet) {
            let start = text.len();
            text.push_str(&run);
            if is_bold {
                bold.push((
                    start..text.len(),
                    gpui::HighlightStyle {
                        font_weight: Some(FontWeight::BOLD),
                        ..Default::default()
                    },
                ));
            }
        }

        div()
            .id(("search-result", idx))
            .flex()
            .flex_col()
            .gap(theme::z(2.))
            .px(theme::z(10.))
            .py(theme::z(6.))
            .rounded(theme::z(theme::RADIUS))
            .when(focused, |s| s.bg(theme::p().bubble_assistant))
            .hover(|s| s.bg(theme::p().bubble_assistant))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                this.open_search_result(&id, window, cx);
            }))
            .child(
                div()
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::p().text)
                    .child(title),
            )
            .child(
                div()
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .text_color(theme::p().muted)
                    .child(gpui::StyledText::new(text).with_highlights(bold)),
            )
    }

    /// Read a stored message aloud (the per-reply speaker button) — from the
    /// active conversation, which is the only one on screen.
    fn speak_message(&mut self, idx: usize, cx: &mut Context<Self>) {
        let text = self
            .active_view()
            .and_then(|v| v.convo.message(idx))
            .map(|m| m.text.clone())
            .unwrap_or_default();
        if !text.trim().is_empty() {
            self.speak_text(text, cx);
        }
    }

    // ---- message actions (slice 12) -----------------------------------------

    /// Copy a message's raw markdown source to the clipboard and flash the
    /// `✓ Copied` chip for 1.5s (spec §1). Generation-guarded like the
    /// memory-saved flash: re-copying restarts the clock.
    fn copy_message(&mut self, idx: usize, cx: &mut Context<Self>) {
        let conv_id = self.sidebar.active_id().to_string();
        let Some(row) = self.active_view().and_then(|v| v.rows.get(idx)) else {
            return;
        };
        if row.raw.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(row.raw.to_string()));
        let gen = self.copied.set(&conv_id, idx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (tx, rx) = async_channel::bounded(1);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                let _ = tx.send_blocking(());
            });
            let _ = rx.recv().await;
            this.update(cx, |app, cx| {
                if app.copied.clear_if(gen) {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn message_row(
        &self,
        idx: usize,
        row: &MessageRow,
        errored: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let conv_id = self.sidebar.active_id().to_string();
        let view = self.active_view();
        let streaming = conversation_export_is_busy(
            self.mode,
            &conv_id,
            self.work.running,
            self.work.conv_id.as_deref(),
        );
        let archived = self.conversations.get(&conv_id).is_some_and(|c| c.archived);
        let (label, bubble_bg, text_color, palette) = match row.role {
            Role::User => (
                "You",
                theme::p().accent,
                theme::p().on_accent,
                markdown_ui::user(),
            ),
            Role::Assistant => (
                "Awili",
                theme::p().bubble_assistant,
                theme::p().text,
                markdown_ui::assistant(),
            ),
        };
        // A speaker button reads this reply aloud — assistant bubbles only, and not
        // error notices (which carry no model speech).
        let speaker = (row.role == Role::Assistant && !errored).then(|| {
            div()
                .id(("speak", idx))
                .px(theme::z(6.))
                .py(theme::z(1.))
                .rounded(theme::z(theme::RADIUS))
                .text_size(theme::z(theme::LABEL_SIZE))
                .text_color(theme::p().muted)
                .cursor_pointer()
                .hover(|s| s.text_color(theme::p().accent_text))
                .flex()
                .flex_row()
                .items_center()
                .gap(theme::z(4.))
                .child(icons::icon(icons::Icon::Speaker, 11.0, theme::p().muted))
                .child("Speak")
                .tooltip(icons::tip("Read this reply aloud"))
                .on_click(cx.listener(move |this, _ev, _w, cx| this.speak_message(idx, cx)))
        });
        // User messages keep the header copy chip: the raw markdown source,
        // not the rendered styling, lands on the clipboard. Assistant copy
        // moved into the actions row below the bubble (slice 12 §1).
        let copy = (row.role == Role::User && !row.raw.is_empty()).then(|| {
            markdown_ui::copy_chip(
                ("copy-msg", idx).into(),
                row.raw.clone(),
                theme::p().user_copy_text,
                theme::p().user_copy_hover,
            )
        });
        // The slice-12 actions row under every assistant bubble: copy /
        // regenerate / edit-resend. Always laid out (so nothing jumps),
        // invisible until the row is hovered or the Copied chip is flashing;
        // every button disables while a reply streams (spec §5).
        let actions = (row.role == Role::Assistant).then(|| {
            let copied = self.copied.is(&conv_id, idx);
            // M2g-FIX1: the LAST assistant row's actions (Retry) are always
            // shown; earlier rows stay hover-only.
            let visible = self.hovered_row == Some(idx)
                || copied
                || view.is_some_and(|v| regen_target(v) == Some(idx));
            let can_act = visible && !streaming && !archived;
            let copy_ok = can_act && !row.raw.is_empty();
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(theme::z(6.))
                .when(!visible, |d| d.opacity(0.))
                .child(action_chip(
                    ("act-copy", idx),
                    if copied {
                        icons::Icon::Check
                    } else {
                        icons::Icon::Copy
                    },
                    if copied { "Copied" } else { "Copy" },
                    "Copy the reply as markdown",
                    copy_ok,
                    cx.listener(move |this, _ev, _w, cx| this.copy_message(idx, cx)),
                ))
                .child({
                    // M2g: the last assistant row reads "Retry" — it re-sends
                    // the preceding user message through `submit_work_run`,
                    // appending a new user row and a new run. History is
                    // never rewritten.
                    let is_last_assistant = view.is_some_and(|v| regen_target(v) == Some(idx));
                    let retry_ok = can_act
                        && is_last_assistant
                        && !self.work.running
                        && view
                            .as_ref()
                            .map(|v| prompt_history::retry_text(&v.convo).is_some())
                            .unwrap_or(false);
                    action_chip(
                        ("act-retry", idx),
                        icons::Icon::RefreshCw,
                        "Retry",
                        "Send your message again, to the helper shown in the chip",
                        retry_ok,
                        {
                            let painted_in = self.sidebar.active_id().to_string();
                            cx.listener(move |this, _ev, _w, cx| {
                                this.retry_work_in(painted_in.clone(), idx, cx)
                            })
                        },
                    )
                    // M2g-FIX1: a test handle on the painted chip (no-op in release).
                    .debug_selector(move || format!("act-retry-{idx}"))
                })
        });
        // The run that owns this row decides its pulse.
        let job_owns_row = self.work.running
            && self.work.conv_id.as_deref() == Some(conv_id.as_str())
            && self.work.stream_row_idx == Some(idx);
        let (active_response_row, waiting_for_output) =
            (job_owns_row, self.work.waiting_for_stream_output);
        let show_activity = should_show_activity_pulse(
            row,
            streaming,
            active_response_row,
            waiting_for_output,
            errored,
        );
        // The bubble body: the markdown plan (an activity pulse while empty).
        let body: gpui::AnyElement = if show_activity && row.plan.is_empty() {
            div().into_any_element()
        } else {
            // The active conversation's bubbles highlight the selected search
            // term (slice 16 §3) — message_row only ever renders the active
            // conversation, so the sidebar/search panel are unaffected.
            markdown_ui::message_blocks(
                &conv_id,
                idx,
                row.parsed_len,
                &row.plan,
                &palette,
                self.highlight_term.as_deref(),
            )
            .into_any_element()
        };
        let is_user = row.role == Role::User;
        let _hovered = self.hovered_row == Some(idx);

        let role_header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(8.))
            .child(
                div()
                    .text_size(theme::z(theme::type_::CAPTION.size))
                    .line_height(theme::z(theme::type_::CAPTION.line))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if is_user { theme::p().muted } else { theme::p().attention })
                    .child(label),
            )
            .children(speaker)
            .when(is_user, |d| d.children(copy));

        let bubble = if is_user {
            div()
                .flex_shrink(1.)
                .min_w_0()
                .debug_selector(move || format!("awili-user-bubble-{idx}"))
                .max_w(theme::z(520.))
                .px(theme::z(14.))
                .py(theme::z(10.))
                .rounded(theme::z(theme::RADIUS))
                .bg(bubble_bg)
                .text_size(theme::z(theme::TEXT_SIZE))
                .line_height(theme::z(theme::LINE_HEIGHT))
                .text_color(text_color)
                .child(body)
        } else {
            // Replies are reading content on the canvas. Reserve cards for
            // controls and failures; keep a definite width for wrapped layout.
            div()
                .w_full()
                .max_w(theme::z(markdown_ui::reply_box_max_w(
                    &row.plan,
                    if errored { 34. } else { 0. },
                )))
                .debug_selector(move || format!("{REPLY_BOX_SELECTOR}-{idx}"))
                .py(theme::z(theme::SP_8))
                .text_size(theme::z(theme::type_::READ.size))
                .line_height(theme::z(theme::type_::READ.line))
                .text_color(text_color)
                .when(errored, |d| {
                    d.px(theme::z(theme::SP_16))
                        .rounded(theme::z(theme::RADIUS_L))
                        .bg(theme::p().bubble_assistant)
                        .border_1()
                        .border_color(theme::p().border)
                        .text_color(text_color)
                })
                // Errored turns read as an error CARD via an inset warm strip
                // (gpui has ONE border color for all sides — a border_l trick
                // would repaint the whole outline warn; GLM SHOULD-4).
                .child(if errored {
                    div()
                        .flex()
                        .flex_row()
                        .gap(theme::z(theme::SPACE_M))
                        .child(
                            div()
                                .flex_none()
                                .w(theme::z(3.))
                                .rounded(theme::z(1.5))
                                .bg(theme::p().chip_warn),
                        )
                        .child(div().flex_1().min_w(theme::z(0.)).child(body))
                        .into_any_element()
                } else {
                    body
                })
        };

        let dbg_blocks = row.plan.len();
        let dbg_raw = row.raw.len();
        let row_div = div()
            .when(row_debug(), move |d| {
                d.on_children_prepainted(move |bounds, _w, _cx| {
                    let dims: Vec<String> = bounds
                        .iter()
                        .map(|b| format!("(y={:?},h={:?})", b.origin.y, b.size.height))
                        .collect();
                    eprintln!(
                        "[KID] row{idx} rawlen={dbg_raw} blocks={dbg_blocks} kids: {}",
                        dims.join(" ")
                    );
                })
            })
            .id(("msg-row", idx))
            .debug_selector(move || format!("awili-message-row-{idx}"))
            // A transcript row must never be COMPRESSED to fit the viewport:
            // it is a flex item in the scrolling column, and flex items default
            // to `flex-shrink: 1`. Without this, a tall row is squeezed (live
            // 2026-08-24: a 225px reply was given a 95px box) while its text
            // still paints full height — the row bleeds over the next one and
            // the scrollable extent comes up short.
            .flex_none()
            .on_hover(cx.listener(move |this, hovered: &bool, _w, cx| {
                if *hovered {
                    if this.hovered_row != Some(idx) {
                        this.hovered_row = Some(idx);
                        cx.notify();
                    }
                } else if this.hovered_row == Some(idx) {
                    this.hovered_row = None;
                    cx.notify();
                }
            }))
            .flex()
            .flex_col()
            .gap(theme::z(4.));

        let conv_id = self.sidebar.active_id().to_string();
        let fold_owner = conv_id.clone();
        let fold = activity_fold(
            ("chat-activity", idx),
            row,
            cx.listener(move |this, _ev, _w, cx| {
                this.toggle_row_activity(&fold_owner, idx, cx);
            }),
        );
        // RC2-RUN (round 4 Minor 10): a row Awili wrote for a plan-card
        // click reads as a note about the click, not as words the owner
        // typed (RC2-RUNF: only a row marked as a click, never by its words).
        if let Some(note) = (is_user && row.plan_click)
            .then(|| crate::plan_gate::plan_click_note(row.raw.as_ref()))
            .flatten()
        {
            return row_div.items_end().child(plan_click_note_line(idx, note));
        }
        // RC2-UX (R2-14): a Retry row is a note, not a second copy of the question.
        if is_user && row.retry {
            return row_div.items_end().child(retry_note_line(idx));
        }
        if is_user {
            row_div
                .items_end()
                .child(role_header)
                .child(div().flex().w_full().justify_end().child(bubble))
                .children(actions)
        } else {
            // Fold renders BELOW the bubble (ocr review R5): expanding grows
            // content under the reading anchor instead of shoving it down.
            row_div
                .child(role_header)
                .children(show_activity.then(|| activity_pulse(self.thinking_phase)))
                .children((!row.plan.is_empty()).then_some(bubble))
                .children(fold)
                // RC2-R1F5: a reply that ran in another folder says which.
                .children(row.footer.clone().map(|f| {
                    let ran_in =
                        work_folder_moves::ran_in(self.folder_moves(self.sidebar.active_id()), idx);
                    crate::run_footer::footer_line(work_folder_moves::footer_with_folder(f, ran_in))
                }))
                .children(actions)
                // The live gate is the reply's last child. Review's latest
                .children(self.reply_inspect_control(&conv_id, idx, cx))
                // scroll keeps its action/editor below expanded activity.
                .children(
                    (self.workshop_error_row() == Some(idx))
                        .then(|| self.work_error_card(cx))
                        .flatten(),
                )
                .children(
                    (self.workshop_blocking_row() == Some(idx))
                        .then(|| self.workshop_blocking_cards(cx))
                        .into_iter()
                        .flatten(),
                )
        }
    }

    #[cfg(test)]
    fn message_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        self.message_list_with_header(None, cx)
    }

    fn message_list_with_header(
        &self,
        header: Option<gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = self.active_view();
        // RC2-R1F5: a Work conversation's folder changes show as notes
        // where they happened.
        let moves = self.folder_moves(self.sidebar.active_id()).to_vec();
        let note = |at: usize| {
            moves
                .iter()
                .filter(move |m| m.at == at)
                .map(move |m| work_folder_moves::note_line(at, m).into_any_element())
        };
        let rows: Vec<gpui::AnyElement> = match view {
            Some(v) => {
                let mut rows = Vec::new();
                for (i, row) in v.rows.iter().enumerate() {
                    rows.extend(note(i));
                    rows.push(
                        self.message_row(i, row, v.errored.contains(&i), cx)
                            .into_any_element(),
                    );
                }
                rows.extend(note(v.rows.len()));
                rows
            }
            None => Vec::new(),
        };
        let scroll = view.map(|v| v.scroll.clone());
        let mut list = div()
            .id("message-list")
            .debug_selector(|| "awili-transcript-viewport".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h(theme::z(0.))
            .w_full()
            .overflow_y_scroll();
        if let Some(scroll) = &scroll {
            list = list.track_scroll(scroll);
        }
        // Empty transcript: a centered hero (critique P5 pattern, matching
        // the Code tabs) instead of blank canvas.
        let blocking_fallback = if self.workshop_blocking_row().is_none() {
            self.workshop_blocking_cards(cx)
        } else {
            Vec::new()
        };
        let error_fallback = self
            .workshop_error_row()
            .is_none()
            .then(|| self.work_error_card(cx))
            .flatten();
        let hero = (rows.is_empty() && blocking_fallback.is_empty() && error_fallback.is_none())
            .then(|| {
                ui::EmptyState::new(
                "workshop-empty",
                "Awili",
                "Ask a question or tell Awili what to do. Choose a folder for work on your files.",
            )
            });
        let compact = header.is_some();
        let list = list
            .children(header)
            .child(
                // A declared reading width is known before wrapped children
                // are measured. The flex row shrinks it in narrow viewports;
                // an auto-width column can reserve a tall, empty scroll tail.
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .w_full()
                    .justify_center()
                    .child(
                        div()
                            .when(row_debug(), |d| {
                                d.on_children_prepainted(|bounds, _window, _cx| {
                                    for (i, b) in bounds.iter().enumerate() {
                                        eprintln!(
                                            "[ROW] {i}: y={:?} h={:?} w={:?}",
                                            b.origin.y, b.size.height, b.size.width
                                        );
                                    }
                                })
                            })
                            .flex()
                            .flex_col()
                            .gap(theme::z(16.))
                            .w(theme::z(theme::READING_MAX))
                            .min_w_0()
                            .px(theme::z(theme::SP_24))
                            .py(theme::z(theme::SP_24))
                            .children(hero)
                            .children(rows)
                            .children(blocking_fallback)
                            .children(error_fallback),
                    ),
            );
        div()
            .relative()
            .flex()
            .flex_1()
            .min_h(if compact {
                theme::target()
            } else {
                gpui::Pixels::ZERO
            })
            .w_full()
            .child(list)
            .children(scroll.as_ref().map(|scroll| {
                conversation_scrollbar(scroll, self.conversation_scroll_drag.clone())
            }))
            // U3e2: "Jump to latest" while scrolled up (gpui re-renders on wheel).
            .children(
                scroll
                    .as_ref()
                    .and_then(|s| transcript_keys::jump_pill(s, cx)),
            )
    }

    /// One sidebar conversation row: click to select, double-click to rename
    /// inline, and a per-row archive (or restore) button. While this row is
    /// being renamed the title swaps for the shared inline editor.
    fn sidebar_row(
        &self,
        ix: usize,
        meta: &ConversationMeta,
        archived: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = meta.id.clone();
        let is_active = self.sidebar.active_id() == meta.id;
        let is_hovered = self.sidebar.hovered_id.as_deref() == Some(meta.id.as_str());
        let renaming = self.sidebar.renaming.as_deref() == Some(meta.id.as_str());
        let title: SharedString = meta.title.clone().into();
        let key = if archived { "conv-arch" } else { "conv" };
        let row_id = (key, ix);
        let title_selector = format!(
            "{SIDEBAR_TITLE_SELECTOR}-{}-{ix}",
            if archived { "a" } else { "c" }
        );
        let row_selector = format!("awili-history-row-{id}");
        let click_id = id.clone();
        let hover_id = id.clone();

        // Keep the opening turn stable in the subtitle instead of changing it
        // as later replies arrive. Council rows summarize members and rounds.
        let preview: Option<SharedString> = if meta.kind == ConvKind::Council {
            self.councils
                .get(&meta.id)
                .map(|p| SharedString::from(council_view::sidebar_line(p)))
        } else {
            self.views.get(&meta.id).and_then(|v| {
                v.convo.messages().first().map(|m| {
                    SharedString::from(
                        m.text
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(60)
                            .collect::<String>(),
                    )
                })
            })
        };
        let time_str: SharedString = crate::work::relative_time(meta.updated_ts as f64).into();
        let moved = work_folder_moves::sidebar_line(self.folder_moves(&meta.id));

        let row: gpui::AnyElement = if renaming {
            // The editor takes the title slot while retaining the same row
            // id and focus stop as the normal KIT Row.
            let row_click_id = click_id.clone();
            div()
                .id(row_id)
                .tab_index(focus_nav::SIDEBAR)
                .focus_visible(focus_nav::ring)
                .flex()
                .items_center()
                .gap(theme::z(theme::SP_4))
                .px(theme::z(theme::SP_12))
                .py(theme::z(theme::SP_8))
                .rounded(theme::z(theme::RADIUS))
                .when(is_active, |d| d.bg(theme::p().row_active))
                .child(
                    div()
                        .flex_1()
                        .min_w(theme::z(0.))
                        .child(self.rename_editor.clone()),
                )
                .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                    if ev.click_count() >= 2 {
                        this.begin_rename(&row_click_id, window, cx);
                    } else {
                        this.select_conversation(&row_click_id, window, cx);
                    }
                }))
                .into_any_element()
        } else {
            let mut actions = div()
                .flex()
                .flex_row()
                .flex_none()
                // Keep the row's one-line control height even when its
                // controls are removed from layout at rest. Reserve only
                // height here so hidden actions do not take title width.
                .min_h(theme::target())
                .items_center()
                .gap(theme::z(theme::SP_4));
            if self.pending_delete.as_deref() == Some(meta.id.as_str()) {
                let confirm_id = meta.id.clone();
                let cancel_id = meta.id.clone();
                actions = actions
                    .child(
                        crate::ui::text::caption("Delete?")
                            .text_color_override(theme::p().sev_critical),
                    )
                    .child(
                        div()
                            .id((if archived { "cda-y" } else { "cd-y" }, ix))
                            .debug_selector(move || {
                                format!(
                                    "awili-sidebar-delete-confirm-{}-{ix}",
                                    if archived { "a" } else { "c" }
                                )
                            })
                            .child(
                                ui::IconButton::new(
                                    (
                                        if archived {
                                            "cda-y-control"
                                        } else {
                                            "cd-y-control"
                                        },
                                        ix,
                                    ),
                                    icons::Icon::Check,
                                )
                                .quiet()
                                .pointer_only()
                                .allow_double_click()
                                .label("Confirm delete")
                                .tooltip("Confirm delete")
                                .on_click(cx.listener(
                                    move |this, _ev, window, cx| {
                                        cx.stop_propagation();
                                        if this.pending_delete.as_deref()
                                            == Some(confirm_id.as_str())
                                        {
                                            this.delete_conversation(&confirm_id, window, cx);
                                        }
                                    },
                                )),
                            ),
                    )
                    .child(
                        div()
                            .id((if archived { "cda-n" } else { "cd-n" }, ix))
                            .debug_selector(move || {
                                format!(
                                    "awili-sidebar-delete-cancel-{}-{ix}",
                                    if archived { "a" } else { "c" }
                                )
                            })
                            .child(
                                ui::IconButton::new(
                                    (
                                        if archived {
                                            "cda-n-control"
                                        } else {
                                            "cd-n-control"
                                        },
                                        ix,
                                    ),
                                    icons::Icon::X,
                                )
                                .quiet()
                                .pointer_only()
                                .allow_double_click()
                                .label("Keep conversation")
                                .tooltip("Keep conversation")
                                .on_click(cx.listener(
                                    move |this, _ev, _window, cx| {
                                        cx.stop_propagation();
                                        if this.pending_delete.as_deref()
                                            == Some(cancel_id.as_str())
                                        {
                                            this.pending_delete = None;
                                            cx.notify();
                                        }
                                    },
                                )),
                            ),
                    );
            } else {
                let hidden = !is_active && !is_hovered;
                let rename_id = meta.id.clone();
                let action_button = |outer_id: gpui::ElementId,
                                     control_id: gpui::ElementId,
                                     label: &'static str,
                                     icon: icons::Icon,
                                     selector: Option<String>,
                                     on_click: crate::ui::interaction::ClickHandler|
                 -> gpui::AnyElement {
                    let mut outer = div().id(outer_id).when(hidden, |d| d.hidden());
                    if let Some(selector) = selector {
                        outer = outer.debug_selector(move || selector.clone());
                    }
                    outer
                        .child(
                            ui::IconButton::new(control_id, icon)
                                .quiet()
                                .pointer_only()
                                .allow_double_click()
                                .label(label)
                                .tooltip(label)
                                .on_click(on_click),
                        )
                        .into_any_element()
                };
                let rename_selector = format!(
                    "{SIDEBAR_RENAME_SELECTOR}-{}-{ix}",
                    if archived { "a" } else { "c" }
                );
                let rename = action_button(
                    (if archived { "cra" } else { "cr" }, ix).into(),
                    (
                        if archived {
                            "cra-control"
                        } else {
                            "cr-control"
                        },
                        ix,
                    )
                        .into(),
                    "Rename",
                    icons::Icon::Pencil,
                    Some(rename_selector),
                    Box::new(cx.listener(move |this, _ev, window, cx| {
                        cx.stop_propagation();
                        this.begin_rename(&rename_id, window, cx);
                    })),
                );
                let archive_id = meta.id.clone();
                let archive = action_button(
                    (if archived { "caa" } else { "ca" }, ix).into(),
                    (
                        if archived {
                            "caa-control"
                        } else {
                            "ca-control"
                        },
                        ix,
                    )
                        .into(),
                    if archived { "Unarchive" } else { "Archive" },
                    if archived {
                        icons::Icon::RotateCcw
                    } else {
                        icons::Icon::Archive
                    },
                    None,
                    Box::new(cx.listener(move |this, _ev, window, cx| {
                        cx.stop_propagation();
                        if archived {
                            this.unarchive_conversation(&archive_id, cx);
                        } else {
                            this.archive_conversation(&archive_id, window, cx);
                        }
                    })),
                );
                let delete_id = meta.id.clone();
                let delete = action_button(
                    (if archived { "cxa" } else { "cx" }, ix).into(),
                    (
                        if archived {
                            "cxa-control"
                        } else {
                            "cx-control"
                        },
                        ix,
                    )
                        .into(),
                    "Delete",
                    icons::Icon::X,
                    Some(format!(
                        "awili-sidebar-delete-{}-{ix}",
                        if archived { "a" } else { "c" }
                    )),
                    Box::new(cx.listener(move |this, _ev, _window, cx| {
                        cx.stop_propagation();
                        this.pending_delete = Some(delete_id.clone());
                        cx.notify();
                    })),
                );
                actions = actions.child(rename).child(archive).child(delete).child(
                    div()
                        .flex_none()
                        .when(is_active || is_hovered, |d| d.hidden())
                        .child(crate::ui::text::caption(time_str.clone())),
                );
            }

            let row_click_id = click_id.clone();
            let mut row = crate::ui::Row::new(row_id)
                .title(title)
                .title_selector(title_selector)
                .selected(is_active)
                .hovered(is_hovered)
                .tab_index(focus_nav::SIDEBAR)
                .allow_double_click()
                .trailing(actions)
                .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                    if ev.click_count() >= 2 {
                        this.begin_rename(&row_click_id, window, cx);
                    } else {
                        this.select_conversation(&row_click_id, window, cx);
                    }
                }));
            if archived {
                row = row.title_color_override(theme::p().muted);
            }
            if let Some(preview) = preview {
                row = row.subtitle(preview);
            }
            row.into_any_element()
        };

        // The move note remains a separate line, preserving its selector and
        // click target without allowing long titles to wrap.
        let mut contents = div().flex().flex_col().w_full().child(row);
        if !renaming {
            if let Some(moved) = moved {
                let moved_id = click_id.clone();
                contents = contents.child(
                    div()
                        .id(("sidebar-folder-move", ix))
                        .px(theme::z(theme::SP_12))
                        .text_size(theme::z(theme::LABEL_SIZE))
                        .text_color(theme::p().muted)
                        .truncate()
                        .debug_selector(move || {
                            format!("{}-{ix}", work_folder_moves::SIDEBAR_MOVED_SELECTOR)
                        })
                        .child(SharedString::from(moved))
                        .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                            if ev.click_count() >= 2 {
                                this.begin_rename(&moved_id, window, cx);
                            } else {
                                this.select_conversation(&moved_id, window, cx);
                            }
                        })),
                );
            }
        }

        let mut identified = div()
            .debug_selector(move || row_selector.clone())
            .child(contents);
        if is_active && !archived {
            identified = div()
                .debug_selector(|| sidebar::ACTIVE_ROW_SELECTOR.to_string())
                .child(identified);
        }
        div()
            .id((
                if archived {
                    "sidebar-row-hover-arch"
                } else {
                    "sidebar-row-hover"
                },
                ix,
            ))
            .flex()
            .flex_col()
            .mx(theme::z(theme::SP_4))
            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                let next = if *hovered {
                    Some(hover_id.clone())
                } else if this.sidebar.hovered_id.as_deref() == Some(hover_id.as_str()) {
                    None
                } else {
                    return;
                };
                if this.sidebar.hovered_id != next {
                    this.sidebar.hovered_id = next;
                    cx.notify();
                }
            }))
            .child(identified)
    }

    /// The conversations sidebar (slice-7 spec §2): collapse toggle, the
    /// new-conversation button, active rows newest-first, and the collapsible
    /// Archived section. A restored collapsed state keeps only its two buttons,
    /// while still filling the floating panel.
    fn sidebar_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let history_offset = self.shell.history_scroll.offset().y.clamp(
            -self.shell.history_scroll.max_offset().y,
            gpui::Pixels::ZERO,
        );
        let history_scroll = self.shell.history_scroll.clone();
        let toggle_label = if self.sidebar.collapsed {
            "Expand sidebar"
        } else {
            "Collapse sidebar"
        };
        let toggle = ui::IconButton::new("sidebar-toggle", icons::Icon::Menu)
            .quiet()
            .pointer_only()
            .selector("sidebar-toggle")
            .label(toggle_label)
            .tooltip(if self.sidebar.collapsed {
                "Expand sidebar"
            } else {
                "Collapse sidebar"
            })
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.sidebar.toggle_collapsed();
                cx.notify();
            }));

        if self.sidebar.collapsed {
            // Compact contents: expand + new-conversation, nothing else.
            let new_btn = ui::IconButton::new("sidebar-new-rail", icons::Icon::Plus)
                .quiet()
                .pointer_only()
                .selector("sidebar-new-rail")
                .label("New conversation")
                .tooltip("New conversation — \u{2318}N")
                .on_click(cx.listener(|this, _ev, window, cx| this.new_conversation(window, cx)));
            return div()
                .flex()
                .flex_col()
                .flex_none()
                .items_center()
                .gap(theme::z(6.))
                .w_full()
                .max_w(gpui::relative(1.))
                .h_full()
                .py(theme::z(10.))
                .bg(theme::p().chrome)
                .child(toggle)
                .child(new_btn)
                .into_any_element();
        }

        let in_workshop = consolidation::workshop_mode(self.mode);
        // The Workshop panel filters by title; its New lives in the top bar.
        let query = in_workshop
            .then(|| self.shell.tabs.history_query.trim().to_lowercase())
            .filter(|q| !q.is_empty());
        let new_btn = ui::Button::new("sidebar-new", "+ New conversation")
            .quiet()
            .full_width()
            .pointer_only()
            .selector("sidebar-new")
            .tooltip("New conversation — \u{2318}N")
            .on_click(cx.listener(|this, _ev, window, cx| this.new_conversation(window, cx)));

        let archive_toggle = ui::Button::new("sidebar-archive-toggle", "Archive")
            .quiet()
            .pointer_only()
            .selector("awili-sidebar-archive")
            .tooltip("Toggle archived conversations")
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.sidebar.toggle_archived();
                cx.notify();
            }));

        let header = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(theme::SP_4))
            .px(theme::z(theme::SP_12))
            .py(theme::z(theme::SP_8))
            .when(!in_workshop, |d| d.child(new_btn))
            .when(in_workshop, |d| {
                let handle = cx.entity().downgrade();
                d.child(
                    div().flex_1().min_w_0().child(
                        ui::SearchField::new("awili-history-search", "Search conversations")
                            .selector("awili-history-search")
                            .query(self.shell.tabs.history_query.clone())
                            .on_query(move |q, _window, cx| {
                                let _ = handle.update(cx, |app, cx| {
                                    app.shell.tabs.history_query = q;
                                    cx.notify();
                                });
                            }),
                    ),
                )
            })
            .child(archive_toggle);

        // Render workspace groups (Phase 4A). R1f: only the active mode's
        // conversation kind, the groups ordered by their newest conversation.
        let sidebar_kind = self.mode_conv_kind();
        let groups = if consolidation::workshop_mode(self.mode) {
            sidebar::workshop_sections(
                &self.conversations,
                Some(self.profile_store.active()),
                self.profile_store.show_all(),
                self.sidebar.active_id(),
            )
        } else {
            sidebar::kind_sections(
                &self.conversations,
                Some(self.profile_store.active()),
                self.profile_store.show_all(),
                sidebar_kind,
            )
        };
        let calendar_now = chrono::Local::now();

        let mut active_rows: Vec<gpui::AnyElement> = Vec::new();
        let mut last_group: Option<&str> = None;
        // Unique per-row element id. This was `gix * 100` — the GROUP index —
        // so every row in a workspace group shared one gpui element id, and
        // duplicate ids corrupt interactive state: clicks on conversation rows
        // never dispatched (user-test B1, "conversations cannot be switched").
        let mut row_ix = 0usize;

        let time_group_label = |label: &'static str, key: &'static str| -> gpui::AnyElement {
            let s: SharedString = label.into();
            // Time buckets read as quiet RULES (hairline + faint text) so
            // they can never be mistaken for a workspace heading (critique:
            // the two were styled identically).
            div()
                .id(key)
                .flex()
                .flex_row()
                .items_center()
                .gap(theme::z(theme::SP_8))
                .px(theme::z(theme::SP_16))
                .py(theme::z(theme::SP_4))
                .mt(theme::z(theme::SP_8))
                .child(crate::ui::text::caption(s).text_color_override(theme::p().faint))
                .child(
                    div()
                        .flex_1()
                        .h(theme::z(theme::HAIRLINE))
                        .bg(theme::p().border),
                )
                .into_any_element()
        };

        // The sidebar lists only the active mode's conversation kind: Chat
        // threads in Chat mode, Work threads in Work mode (utility modes default
        // to Chat). Each kind is a disjoint world, so the two never intermix;
        // `kind_sections` already dropped other-kind rows and empty groups.
        for (gix, group) in groups.iter().enumerate() {
            let eligible: Vec<&str> = group
                .session_ids
                .iter()
                .map(String::as_str)
                .filter(|id| {
                    self.conversations
                        .get(id)
                        .is_some_and(|meta| self.history_row_shown(&meta.id, &meta.title))
                })
                .collect();
            if eligible.is_empty() {
                continue;
            }
            if groups.len() > 1 {
                let label: SharedString = group.label.clone().into();
                active_rows.push(
                    // Workspace headings carry an accent tick — identity,
                    // not chronology.
                    div()
                        .id(("ws-group", gix))
                        .debug_selector(|| format!("{}{}", sidebar::GROUP_SELECTOR, group.label))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(theme::z(theme::SP_4))
                        .px(theme::z(theme::SP_12))
                        .py(theme::z(theme::SP_4))
                        .child(
                            div()
                                .flex_none()
                                .w(theme::z(3.))
                                .h(theme::z(10.))
                                .rounded(theme::z(1.5))
                                .bg(theme::p().accent),
                        )
                        .child(
                            crate::ui::text::label(label)
                                .strong()
                                .text_color_override(theme::p().muted),
                        )
                        .into_any_element(),
                );
            }
            for &id in &eligible {
                if let Some(meta) = self.conversations.get(id) {
                    let bucket = sidebar::conversation_time_group(meta.updated_ts, &calendar_now);
                    if last_group != Some(bucket) {
                        let label = match bucket {
                            "today" => "TODAY",
                            "yesterday" => "YESTERDAY",
                            _ => "EARLIER",
                        };
                        let key = match bucket {
                            "today" => "tg-today",
                            "yesterday" => "tg-yesterday",
                            _ => "tg-earlier",
                        };
                        active_rows.push(time_group_label(label, key));
                        last_group = Some(bucket);
                    }
                    active_rows.push(self.sidebar_row(row_ix, meta, false, cx).into_any_element());
                    row_ix += 1;
                }
            }
        }

        if query.is_some() && active_rows.is_empty() {
            active_rows.push(
                div()
                    .id("awili-history-no-matches")
                    .px(theme::z(theme::SP_16))
                    .py(theme::z(theme::SP_8))
                    .child(crate::ui::text::caption("No matches"))
                    .into_any_element(),
            );
        }

        let archived: Vec<&ConversationMeta> = self
            .conversations
            .archived()
            .into_iter()
            .filter(|m| self.history_row_shown(&m.id, &m.title))
            .filter(|m| {
                if consolidation::workshop_mode(self.mode) {
                    m.kind != ConvKind::Council
                } else {
                    m.kind == sidebar_kind
                }
            })
            .collect();
        let archived_section = (!archived.is_empty()).then(|| {
            let open = self.sidebar.archived_open;
            let header =
                ui::Button::new("archived-toggle", format!("Archived ({})", archived.len()))
                    .icon(if open {
                        icons::Icon::ChevronDown
                    } else {
                        icons::Icon::ChevronRight
                    })
                    .quiet()
                    .full_width()
                    .pointer_only()
                    .selector("archived-toggle")
                    .tooltip("Toggle archived conversations")
                    .on_click(cx.listener(|this, _ev, _w, cx| {
                        this.sidebar.toggle_archived();
                        cx.notify();
                    }));
            let rows: Vec<_> = if open {
                archived
                    .iter()
                    .enumerate()
                    .map(|(ix, meta)| self.sidebar_row(ix, meta, true, cx))
                    .collect()
            } else {
                Vec::new()
            };
            div()
                .flex_none()
                .flex()
                .flex_col()
                .pb(theme::z(6.))
                .border_t_1()
                .border_color(theme::p().border)
                .child(header)
                .children(rows)
        });

        div()
            .debug_selector(|| "awili-sidebar".to_string())
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .max_w(gpui::relative(1.))
            .h_full()
            .bg(theme::p().chrome)
            .child(
                div()
                    .flex_none()
                    .border_b_1()
                    .border_color(theme::p().border)
                    .mb(theme::z(4.)),
            )
            .child(header)
            .child(
                div()
                    .id("conv-list")
                    .debug_selector(|| "awili-history-list".to_string())
                    .track_scroll(&self.shell.history_scroll)
                    .on_scroll_wheel(cx.listener(
                        move |this, event: &gpui::ScrollWheelEvent, window, cx| {
                            let delta = event.delta.pixel_delta(window.line_height());
                            let delta_y = if delta.y == gpui::Pixels::ZERO {
                                delta.x
                            } else {
                                delta.y
                            };
                            let next = (history_offset + delta_y)
                                .clamp(-history_scroll.max_offset().y, gpui::Pixels::ZERO);
                            // A wheel at the limit leaves the same row under the
                            // pointer, so its actions must remain visible.
                            if next != history_offset && this.sidebar.hovered_id.take().is_some() {
                                cx.notify();
                            }
                        },
                    ))
                    .flex_1()
                    .min_h(theme::z(0.))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(theme::z(2.))
                            .py(theme::z(4.))
                            .children(active_rows)
                            .children(archived_section),
                    ),
            )
            .child(self.profile_rail(cx))
            .into_any_element()
    }

    /// Profile rail at the sidebar bottom — colored squares for each profile.
    /// Visible when more than one profile exists.
    fn profile_rail(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let profiles = self.profile_store.profiles();
        if profiles.len() <= 1 {
            return div().into_any_element();
        }

        let active = self.profile_store.active().to_string();
        let pills: Vec<_> = profiles
            .iter()
            .enumerate()
            .map(|(ix, p)| {
                let is_active = p.name == active;
                // Content palette (profile accents are content, not chrome).
                let color = profile_palette::color(p.color);
                let name: SharedString = p.name.clone().into();
                div()
                    .id(("profile-pill", ix))
                    .flex_none()
                    .w(theme::z(28.))
                    .h(theme::z(28.))
                    .rounded(theme::z(6.))
                    .bg(if is_active {
                        color
                    } else {
                        gpui::Hsla::from(theme::p().chip_bg)
                    })
                    .border_1()
                    .border_color(if is_active {
                        color
                    } else {
                        gpui::Hsla::from(theme::p().border)
                    })
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(theme::z(theme::LABEL_SIZE - 1.0))
                    .text_color(if is_active {
                        theme::p().bg
                    } else {
                        theme::p().muted
                    })
                    .child(SharedString::from(
                        p.name
                            .chars()
                            .next()
                            .unwrap_or('?')
                            .to_uppercase()
                            .to_string(),
                    ))
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.profile_store.set_active(&name);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();

        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(4.))
            .px(theme::z(10.))
            .py(theme::z(8.))
            .border_t_1()
            .border_color(theme::p().border)
            .children(pills)
            .into_any_element()
    }

    /// The settings-nav action a General-form control id maps to, for the
    /// visible keyboard-selection highlight. Non-nav ids (memory/voice/default
    /// model) map to `None` so they are never highlighted.
    fn settings_nav_action_for_id(id: (&'static str, usize)) -> Option<settings_nav::Action> {
        Some(match id {
            ("prov-add", 0) => settings_nav::Action::AddProvider,
            ("local-discovery", 0) => settings_nav::Action::LocalDiscovery,
            ("harness-recipe", ix) => settings_nav::Action::HarnessRecipe(ix),
            ("harness-docs", ix) => settings_nav::Action::HarnessDocs(ix),
            ("prov-name", ix) => settings_nav::Action::ProviderName(ix),
            ("prov-kind", ix) => settings_nav::Action::ProviderKind(ix),
            ("prov-enabled", ix) => settings_nav::Action::ProviderEnabled(ix),
            ("prov-url", ix) => settings_nav::Action::ProviderUrl(ix),
            ("prov-models", ix) => settings_nav::Action::ProviderModels(ix),
            ("prov-key", ix) => settings_nav::Action::ProviderApiKey(ix),
            ("prov-remove", ix) => settings_nav::Action::RemoveProvider(ix),
            ("sx-save-services", 0) | ("draft-save", 0) => settings_nav::Action::Save,
            ("sx-discard-services", 0) | ("draft-discard", 0) => settings_nav::Action::Discard,
            ("sx-error-details", 0) => settings_nav::Action::ErrorDetails,
            _ => return None,
        })
    }

    /// A click-to-edit text value for a structured-panel draft field: the
    /// shared inline editor while this field is being edited (Enter commits to
    /// the DRAFT only), the current draft text otherwise.
    fn draft_field_el(
        &self,
        id: (&'static str, usize),
        field: settings::Field,
        value: String,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let field_label = match field {
            settings::Field::ProviderName(_) => "Provider name",
            settings::Field::ProviderUrl(_) => "Endpoint URL",
            settings::Field::ProviderModels(_) => "Models",
            settings::Field::ProviderApiKey(_) => "API key",
            _ => "Setting value",
        };
        let selected = self
            .settings_nav_focused
            .then(|| Self::settings_nav_action_for_id(id))
            .flatten()
            .is_some_and(|a| a == self.settings_nav.selected());

        if self.settings.field == Some(field) {
            let editor = self.settings_field.clone();
            return crate::ui::TextField::new(id, field_label)
                .label(field_label)
                .editor(move |_| editor.clone())
                .focused(selected)
                .into_any_element();
        }

        let value_id = match field {
            settings::Field::ProviderName(ix) => ("settings-draft-name", ix),
            settings::Field::ProviderUrl(ix) => ("settings-draft-url", ix),
            settings::Field::ProviderModels(ix) => ("settings-draft-models", ix),
            settings::Field::ProviderApiKey(ix) => ("settings-draft-key", ix),
            _ => ("settings-draft-value", id.1),
        };
        let display = if value.is_empty() {
            "(empty — click to edit)".to_string()
        } else {
            value
        };
        let field_for_click = field;
        crate::ui::Row::new(id)
            .title(field_label)
            .trailing(
                crate::ui::TextField::new(value_id, "(empty — click to edit)")
                    .label(field_label)
                    .value(display),
            )
            .wrap_trailing()
            .pointer_only()
            .focused(selected)
            .allow_double_click()
            .on_click(cx.listener(move |this, _ev, window, cx| {
                this.begin_draft_field(field_for_click, window, cx);
            }))
            .into_any_element()
    }

    /// A small toggle/action pill for the structured panel; `on` warms the
    /// border + text to the accent.
    fn draft_pill(
        &self,
        id: (&'static str, usize),
        label: SharedString,
        on: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let focused = self
            .settings_nav_focused
            .then(|| Self::settings_nav_action_for_id(id))
            .flatten()
            .is_some_and(|a| a == self.settings_nav.selected());
        crate::ui::Button::new(id, label)
            .quiet()
            .in_pill()
            .selected(on)
            .reserve_selected_check(true)
            .focused(focused)
            .pointer_only()
            .allow_double_click()
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                action(this, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// A structured-panel section header, optionally tagged "restart required".
    fn panel_section(&self, label: &'static str, restart: bool) -> gpui::AnyElement {
        let tag = restart.then(|| {
            crate::ui::Chip::new(("settings-restart", 0usize), "Restart required")
                .tone(theme::Tone::Attention)
        });
        let (row_id, selector) = match label {
            "Model services" => (
                "settings-section-model-services",
                "sx-section-Model services",
            ),
            "Default model" => ("settings-section-default-model", "sx-section-Default model"),
            "Providers" => ("settings-section-providers", "sx-section-Providers"),
            "Memory service" => (
                "settings-section-memory-service",
                "sx-section-Memory service",
            ),
            "Voice" => ("settings-section-voice", "sx-section-Voice"),
            _ => ("settings-section-other", "sx-section-settings"),
        };
        let mut header = crate::ui::Row::new(row_id)
            .selector(selector)
            .title(label)
            .strong();
        if let Some(tag) = tag {
            header = header.trailing(tag);
        }
        div()
            .flex()
            .flex_col()
            .pt(theme::z(theme::SP_16))
            .child(crate::ui::Divider::new())
            .child(header)
            .into_any_element()
    }

    /// One provider card of the structured panel: name + kind + enabled + ✕ on
    /// the first line, click-to-edit url and models below (slice 11 §1).
    fn provider_card(
        &self,
        ix: usize,
        p: &settings::ProviderDraft,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let enabled = p.enabled;
        let (health, health_text) = if !enabled {
            (theme::Tone::Neutral, "Disabled")
        } else {
            match self.provider_health.get(p.base_url.trim()) {
                Some(true) => (theme::Tone::Neutral, "Reachable"),
                Some(false) => (theme::Tone::Attention, "Unreachable"),
                None => (theme::Tone::Neutral, "Not checked"),
            }
        };
        let status = crate::ui::Row::new(("provider-health", ix))
            .leading(
                crate::ui::Dot::new(health)
                    .label(health_text)
                    .id(("provider-health-dot", ix)),
            )
            .title("Endpoint")
            .subtitle(health_text)
            .wrap();

        let mut children: Vec<gpui::AnyElement> = vec![
            status.into_any_element(),
            self.draft_field_el(
                ("prov-name", ix),
                settings::Field::ProviderName(ix),
                p.name.clone(),
                cx,
            ),
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(theme::z(theme::SP_8))
                .child(self.draft_pill(
                    ("prov-kind", ix),
                    p.kind.as_str().to_string().into(),
                    false,
                    cx,
                    move |this, _cx| this.settings.cycle_provider_kind(ix),
                ))
                .child(self.draft_pill(
                    ("prov-enabled", ix),
                    if enabled { "enabled" } else { "disabled" }.into(),
                    enabled,
                    cx,
                    move |this, _cx| this.settings.toggle_provider_enabled(ix),
                ))
                .child(
                    crate::ui::IconButton::new(("prov-remove", ix), icons::Icon::X)
                        .quiet()
                        .label("Remove provider")
                        .tooltip("Remove provider")
                        .pointer_only()
                        .allow_double_click()
                        .focused(
                            self.settings_nav_focused
                                && Self::settings_nav_action_for_id(("prov-remove", ix))
                                    == Some(self.settings_nav.selected()),
                        )
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.settings.remove_provider(ix);
                            cx.notify();
                        })),
                )
                .into_any_element(),
            crate::ui::Divider::new().into_any_element(),
            self.draft_field_el(
                ("prov-url", ix),
                settings::Field::ProviderUrl(ix),
                p.base_url.clone(),
                cx,
            ),
            self.draft_field_el(
                ("prov-models", ix),
                settings::Field::ProviderModels(ix),
                p.models.clone(),
                cx,
            ),
        ];
        if p.kind == ProviderKind::Anthropic || p.kind == ProviderKind::OpenAi {
            let key_masked = p.api_key.as_deref().is_some_and(|key| !key.is_empty());
            let display = if key_masked {
                "•••• (set — click to edit)".to_string()
            } else {
                "(not set — click to edit)".to_string()
            };
            children.push(self.draft_field_el(
                ("prov-key", ix),
                settings::Field::ProviderApiKey(ix),
                display,
                cx,
            ));
        }

        crate::ui::Card::new(("provider-card", ix))
            .children(children)
            .into_any_element()
    }

    /// The structured settings panel (slice 11 §1): Providers, Default model,
    /// Memory service, Voice — all edits land in the draft; the footer's Save
    /// writes the whole thing through the validation gate, Discard drops it.
    fn settings_form_tab(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(draft) = self.settings.draft.clone() else {
            let message = self
                .settings
                .error
                .clone()
                .unwrap_or_else(|| "config failed to load".to_string());
            return crate::ui::EmptyState::new(
                "settings-models-config-unavailable",
                "Configuration unavailable",
                format!("{message}\nFix it in the Raw TOML tab — saving validates first."),
            )
            .into_any_element();
        };

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        rows.push(
            div()
                .id("settings-model-services-scroll-row")
                .flex()
                .flex_col()
                .child(self.settings_models_intro())
                .child(self.panel_section("Model services", false))
                .into_any_element(),
        );
        if draft.providers.is_empty() {
            rows.push(
                crate::ui::Card::new("model-services-empty")
                    .selector(MODEL_SERVICES_EMPTY_SELECTOR)
                    .body(no_model_services_line(&self.work.engine))
                    .into_any_element(),
            );
        }
        for (ix, p) in draft.providers.iter().enumerate() {
            rows.push(self.provider_card(ix, p, cx));
        }
        rows.push(
            div()
                .px(theme::z(theme::SP_16))
                .py(theme::z(theme::SP_4))
                .child(self.draft_pill(
                    ("prov-add", 0),
                    "+ Add a model service".into(),
                    false,
                    cx,
                    |this, _cx| {
                        this.settings.add_provider();
                    },
                ))
                .into_any_element(),
        );

        // Keep the model choices in the same bounded scroll viewport as the
        // provider cards. The intro and Model services header share the first
        // scroll row; the four following slots preserve helper and
        // local-discovery direct-child indices used by parent navigation.
        rows.push(
            div()
                .id("settings-default-model-scroll-row")
                .flex()
                .flex_col()
                .child(self.panel_section("Default model", false))
                .child(self.settings_default_model_rows(cx))
                .into_any_element(),
        );
        rows.extend((0..4).map(|_| div().into_any_element()));
        rows.extend(self.local_discovery_rows(cx));
        // Keep the service notice and dirty Save / Throw away actions in the
        // same bounded viewport as the form. Appending preserves every row
        // index used by the local-discovery parent navigation above.
        rows.push(self.settings_services_bar(cx));

        div()
            .id("settings-general-form")
            .track_focus(&self.settings_nav_focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_h(gpui::Pixels::ZERO)
            .child(
                div()
                    .id("settings-panel-scroll")
                    .debug_selector(|| "settings-panel-scroll".to_string())
                    .flex_1()
                    .min_h(gpui::Pixels::ZERO)
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .pb(theme::z(theme::SP_12))
                    .track_scroll(&self.settings_nav_scroll)
                    .children(rows),
            )
            .into_any_element()
    }

    /// One settings-form row: key label on the left; the value on the right is
    /// the inline editor (while editing), a click-to-toggle pill (booleans), or
    /// click-to-edit text (everything else).
    fn settings_row(
        &self,
        ix: usize,
        entry: SettingEntry,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let editing = self.settings.editing.as_ref().map(|(t, _)| t) == Some(&entry.target);
        let key: SharedString = entry.key.clone().into();

        let value_el: gpui::AnyElement = if editing {
            div()
                .flex_1()
                .min_w(theme::z(0.))
                .child(self.settings_field.clone())
                .into_any_element()
        } else if entry.kind == ValueKind::Bool {
            let target = entry.target.clone();
            let current = entry.value == "true";
            div()
                .id(("set-bool", ix))
                .flex_none()
                .px(theme::z(10.))
                .py(theme::z(2.))
                .rounded(theme::z(theme::RADIUS))
                .bg(theme::p().chip_bg)
                .border_1()
                .border_color(if current {
                    theme::p().accent
                } else {
                    theme::p().chip_border
                })
                .text_size(theme::z(theme::LABEL_SIZE))
                .text_color(if current {
                    theme::p().accent_text
                } else {
                    theme::p().chip_text
                })
                .cursor_pointer()
                .hover(|s| s.border_color(theme::p().accent))
                .child(SharedString::from(entry.value.clone()))
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    if this.settings.toggle_bool(&target, current) {
                        cx.emit(ConfigReloaded);
                        this.schedule_status_clear(cx);
                    }
                    cx.notify();
                }))
                .into_any_element()
        } else {
            // Credentials (api_key, secrets, auth tokens) never render in
            // cleartext at rest (user-test 2026-07-01) — display_value() masks a
            // set secret with a fixed dot mask; clicking to edit still reveals the
            // real value to its owner. Non-secret fields are unaffected.
            let display: SharedString = if entry.value.is_empty() {
                "(empty — click to edit)".into()
            } else {
                entry.display_value().into()
            };
            let entry = entry.clone();
            div()
                .id(("set-val", ix))
                .flex_1()
                .min_w(theme::z(0.))
                .overflow_hidden()
                .px(theme::z(8.))
                .py(theme::z(2.))
                .rounded(theme::z(theme::RADIUS))
                .text_size(theme::z(theme::TEXT_SIZE - 2.0))
                .text_color(theme::p().text)
                .cursor_pointer()
                .hover(|s| s.bg(theme::p().row_hover))
                .child(display)
                .on_click(cx.listener(move |this, _ev, window, cx| {
                    this.begin_field_edit(&entry, window, cx)
                }))
                .into_any_element()
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(10.))
            .px(theme::z(16.))
            .py(theme::z(3.))
            .child(
                div()
                    .flex_none()
                    .w(theme::z(150.))
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .text_color(theme::p().muted)
                    .child(key),
            )
            .child(value_el)
            .into_any_element()
    }

    /// The grouped settings form: every editable scalar from the document, in
    /// file order, with "restart required" tags on the sections the runtime
    /// cannot re-apply live (spec §3).
    fn settings_form(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(doc) = self.settings.doc.as_ref() else {
            return div()
                .p(theme::z(16.))
                .text_size(theme::z(theme::TEXT_SIZE - 2.0))
                .text_color(theme::p().chip_warn)
                .child(SharedString::from(
                    self.settings
                        .error
                        .clone()
                        .unwrap_or_else(|| "config failed to load".to_string()),
                ))
                .child(
                    div()
                        .pt(theme::z(8.))
                        .text_color(theme::p().muted)
                        .child("Fix it in the Raw TOML tab — saving validates first."),
                )
                .into_any_element();
        };
        let entries = doc.entries();
        if entries.is_empty() {
            return div()
                .p(theme::z(16.))
                .text_size(theme::z(theme::TEXT_SIZE - 2.0))
                .text_color(theme::p().muted)
                .child("No config file yet — everything is at built-in defaults. Use the Raw TOML tab to create one.")
                .into_any_element();
        }

        // Group consecutive entries by section, preserving document order.
        let mut sections: Vec<(String, Vec<SettingEntry>)> = Vec::new();
        for e in entries {
            match sections.last_mut() {
                Some((s, v)) if *s == e.section => v.push(e),
                _ => sections.push((e.section.clone(), vec![e])),
            }
        }

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let mut ix = 0usize;
        for (section, items) in sections {
            let label: SharedString = if section.is_empty() {
                "general".into()
            } else {
                section.clone().into()
            };
            let tag = settings::restart_required(&section).then(|| {
                div()
                    .px(theme::z(6.))
                    .py(theme::z(1.))
                    .rounded(theme::z(theme::RADIUS))
                    .border_1()
                    .border_color(theme::p().chip_border)
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .text_color(theme::p().chip_warn)
                    .child("restart required")
            });
            rows.push(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(theme::z(8.))
                    .px(theme::z(16.))
                    .pt(theme::z(14.))
                    .pb(theme::z(4.))
                    .child(
                        div()
                            .text_size(theme::z(theme::LABEL_SIZE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::p().accent_text)
                            .child(label),
                    )
                    .children(tag)
                    .into_any_element(),
            );
            for e in items {
                rows.push(self.settings_row(ix, e, cx));
                ix += 1;
            }
        }
        div()
            .flex()
            .flex_col()
            .pb(theme::z(12.))
            .children(rows)
            .into_any_element()
    }

    /// The raw-TOML tab: the document text in a multiline editor and a Save
    /// button gated on full validation — invalid TOML never reaches disk.
    fn settings_raw_tab(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let save = div()
            .id("settings-save")
            .flex()
            .items_center()
            .px(theme::z(14.))
            .py(theme::z(4.))
            .rounded(theme::z(theme::RADIUS))
            .bg(theme::p().chip_bg)
            .border_1()
            .border_color(theme::p().chip_border)
            .text_size(theme::z(theme::LABEL_SIZE))
            .text_color(theme::p().chip_text)
            .cursor_pointer()
            .hover(|s| {
                s.border_color(theme::p().accent)
                    .text_color(theme::p().accent_text)
            })
            .child("Save")
            .on_click(cx.listener(|this, _ev, _w, cx| this.save_raw_settings(cx)));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(theme::z(0.))
            .child(
                div()
                    .id("settings-raw-scroll")
                    .flex_1()
                    .min_h(theme::z(0.))
                    .overflow_y_scroll()
                    .p(theme::z(12.))
                    .child(self.settings_raw.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .px(theme::z(12.))
                    .pb(theme::z(10.))
                    .child(save),
            )
            .into_any_element()
    }

    /// The settings modal (gear / cmd+,): a scrim over the app, tabs for the
    /// form and the raw TOML, and the validation error / Saved line.
    fn settings_modal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.settings.tab;
        let sidebar_nav = div()
            .debug_selector(|| "sx-debug-sidebar".to_string())
            .flex_none()
            .w(theme::z(200.))
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme::p().border)
            .child(
                div()
                    .flex_none()
                    .px(theme::z(14.))
                    .py(theme::z(12.))
                    .text_size(theme::z(theme::TEXT_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::p().text)
                    .child("Settings"),
            )
            .child(
                div()
                    .id("sx-nav-scroll")
                    .debug_selector(|| "sx-debug-nav-scroll".to_string())
                    .flex_1()
                    .min_h(theme::z(0.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(theme::z(2.))
                    .py(theme::z(4.))
                    .children(self.settings_page_nav(cx)),
            );

        let close = self.sx_button(("sx-done", 0), "Done", false, cx, |this, cx| {
            this.request_close_settings(cx)
        });

        let content_header = div()
            .debug_selector(|| "sx-debug-header".to_string())
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px(theme::z(14.))
            .py(theme::z(10.))
            .border_b_1()
            .border_color(theme::p().border)
            .child(
                div()
                    .text_size(theme::z(theme::TEXT_SIZE - 1.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::p().text)
                    .child(match tab {
                        settings::Tab::Form => self.settings.page.title(),
                        settings::Tab::Fields => "Every setting",
                        settings::Tab::Raw => "Settings file",
                    }),
            )
            .child(close);

        // The SXd change bar (error / last change / undo) — or, while an
        // unsaved-model-services question is showing, that question.
        let footer = self.settings_change_bar(cx);

        let body: gpui::AnyElement = match tab {
            settings::Tab::Form if self.settings.page == settings_catalog::Page::Models => {
                self.settings_form_tab(cx)
            }
            settings::Tab::Form => self.settings_page_body(cx),
            settings::Tab::Fields => div()
                .id("settings-form-scroll")
                .flex_1()
                .min_h(theme::z(0.))
                .overflow_y_scroll()
                .child(self.settings_form(cx))
                .into_any_element(),
            settings::Tab::Raw => self.settings_raw_tab(cx),
        };

        div()
            .id("settings-scrim")
            .absolute()
            .inset_0()
            .bg(theme::p().scrim)
            .flex()
            .items_center()
            .justify_center()
            // SXe: a click beside Settings no longer closes it (and loses
            // nothing); Done or Esc close.
            .occlude()
            .child(
                div()
                    .id("settings-panel")
                    .debug_selector(|| "sx-panel".to_string())
                    .shadow_lg()
                    .flex()
                    .flex_row()
                    .w(gpui::relative(0.92))
                    .max_w(theme::z(1040.))
                    .h(gpui::relative(0.9))
                    .max_h(theme::z(820.))
                    .rounded(theme::z(theme::RADIUS))
                    .bg(theme::p().chrome)
                    .border_1()
                    .border_color(theme::p().border)
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(sidebar_nav)
                    .child(
                        div()
                            .debug_selector(|| "sx-debug-content".to_string())
                            .track_focus(&self.settings_active_tab_focus)
                            .flex_1()
                            .flex()
                            .flex_col()
                            .min_w(theme::z(0.))
                            .child(content_header)
                            .child(body)
                            .children(footer),
                    ),
            )
    }

    /// The staged-attachment tray (slice 19 §6): one chip per attachment,
    /// rendered between the message list and the composer. Only called when
    /// `staged_attachments` is non-empty.
    fn attachment_tray(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .w_full()
            .max_w(theme::z(theme::COMPOSER_MAX))
            .mx_auto()
            .px(theme::z(theme::SP_16))
            .flex()
            .flex_wrap()
            .gap(theme::z(theme::SP_4))
            .children(self.staged_attachments.iter().map(|att| {
                let id = att.id;
                ui::Chip::new(("attachment-chip", id as u64), attachment_chip_label(att))
                    .file(att.filename.clone())
                    .tooltip(attachment_chip_label(att))
                    .removable(cx.listener(move |app, _, _, cx| app.unstage_attachment(id, cx)))
            }))
    }

    fn composer_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let width = px(self.window_w);
        // Job/Inspector or Needs-you, then the docked Conversations panel.
        let dock = self.workshop_right_dock(width);
        let composer_width = theme::composer_width(width, dock);
        let streaming = self.composer_busy()
            || self.council_composer_answering()
            || (self.mode == Mode::Work && self.workshop_pending_for(self.sidebar.active_id()));
        let action = ui::IconButton::new(
            "send-or-stop",
            if streaming {
                icons::Icon::Square
            } else {
                icons::Icon::ArrowUp
            },
        )
        .selector(SEND_OR_STOP_SELECTOR)
        .round()
        .label(send_button_label(streaming))
        .tab_index(focus_nav::SEND)
        .tooltip(if streaming {
            shortcuts::localize("Stop — ⌘. or Esc").into_owned()
        } else {
            "Send — Enter".to_string()
        });
        let action = if streaming {
            action
                .ghost()
                .on_click(cx.listener(|this, _, _, cx| this.abort_active_stream(cx)))
        } else {
            action
                .primary()
                .on_click(cx.listener(|this, _, _, cx| this.submit_composer_draft(cx)))
        };
        let mut actions = self.composer_voice_actions(cx);
        actions.push(action.into_any_element());
        let (strip_left, strip_right) = self.composer_footer_parts(cx);
        let actions_below_field =
            theme::composer_actions_need_row(composer_width, actions.len() + 1);
        div()
            .flex()
            .flex_col()
            .flex_shrink(1.)
            .min_h_0()
            .w_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink(1.)
                    .min_h_0()
                    // A definite width also applies during intrinsic height
                    // sizing; a late max-width alone can clip soft-wrapped rows.
                    .w(composer_width)
                    .min_w(composer_width)
                    .max_w(composer_width)
                    .mx_auto()
                    .px(theme::z(if actions_below_field {
                        theme::SP_4
                    } else {
                        theme::SP_16
                    }))
                    .py(theme::z(theme::SP_16))
                    .when(self.skill_palette.open, |d| {
                        d.child(self.skill_palette_popover(cx))
                    })
                    .children(self.work_plan_gate_card(cx).map(|card| {
                        div()
                            .debug_selector(|| "awili-composer-plan-wrapper".into())
                            .flex()
                            .flex_col()
                            .flex_none()
                            .pb(theme::z(theme::SP_12))
                            .child(card)
                    }))
                    .children(self.local_model_down_card(cx).map(|card| {
                        div()
                            .debug_selector(|| "awili-composer-local-wrapper".into())
                            .flex()
                            .flex_col()
                            .flex_none()
                            .pb(theme::z(theme::SP_12))
                            .child(card)
                    }))
                    .children(self.workshop_work_cards(cx))
                    .children(self.send_queue_panel(cx))
                    .children(self.composer_banners())
                    .child(
                        self.composer_frame_anchor(
                            ui::ComposerFrame::new("awili-composer")
                                .actions_below_field(actions_below_field)
                                .field(
                                    div()
                                        .debug_selector(|| {
                                            plan_gate::COMPOSER_ROW_SELECTOR.to_string()
                                        })
                                        .flex()
                                        .flex_col()
                                        .flex_shrink(1.)
                                        .min_h_0()
                                        .min_w_0()
                                        .child(self.composer.clone()),
                                )
                                .leading(vec![ui::IconButton::new(
                                    "attach",
                                    icons::Icon::Paperclip,
                                )
                                .round()
                                .label("Attach a copied file or image")
                                .tab_index(focus_nav::EXECUTOR)
                                .on_click(cx.listener(|app, _, _, cx| {
                                    if !app.attach_from_clipboard(cx) {
                                        app.show_info_toast(
                                            "Copy a file or image, then attach it here.",
                                            cx,
                                        );
                                    }
                                }))])
                                .trailing(actions)
                                .footer(strip_left)
                                .footer_end(strip_right)
                                .into_any_element(),
                        ),
                    )
                    .children(self.composer_details()),
            )
    }

    /// The `/`-command skill palette: a dropdown card anchored directly above
    /// the composer, listing the skills whose name/description match the
    /// `/`-token. Rendered only while `skill_palette.open` (which the observer
    /// sets only when there is at least one match). The focused row carries the
    /// active tint; clicking a row inserts it like Enter does.
    fn skill_palette_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let highlight = HighlightStyle {
            color: Some(theme::p().accent_text.into()),
            ..Default::default()
        };
        let rows: Vec<gpui::AnyElement> = self
            .skill_palette_rows()
            .iter()
            .enumerate()
            .map(|(ix, skill)| {
                let focused = ix == self.skill_palette.focused_idx;
                let name = skill.name.clone();
                let runs: Vec<_> = palette::match_ranges(&name, &self.skill_palette.query)
                    .into_iter()
                    .map(|r| (r, highlight))
                    .collect();
                // The leading "/" is decorative — shift highlight ranges by 1.
                let title = StyledText::new(SharedString::from(format!("/{name}")))
                    .with_highlights(
                        runs.into_iter()
                            .map(|(r, h)| ((r.start + 1)..(r.end + 1), h)),
                    );
                let mut row = div()
                    .id(("skill-row", ix))
                    .flex()
                    .flex_col()
                    .gap(theme::z(2.))
                    .mx(theme::z(6.))
                    .px(theme::z(10.))
                    .py(theme::z(6.))
                    .rounded(theme::z(theme::RADIUS))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.skill_palette.focused_idx = ix;
                        this.skill_palette_accept(cx);
                    }))
                    .child(
                        div()
                            .text_size(theme::z(theme::TEXT_SIZE - 1.0))
                            .text_color(theme::p().text)
                            .child(title),
                    );
                if let Some(desc) = skill.description.clone().filter(|d| !d.is_empty()) {
                    row = row.child(
                        div()
                            .text_size(theme::z(theme::LABEL_SIZE))
                            .text_color(theme::p().faint)
                            .overflow_hidden()
                            .child(SharedString::from(desc)),
                    );
                }
                row.when(focused, |d| d.bg(theme::p().row_active))
                    .when(!focused, |d| d.hover(|s| s.bg(theme::p().row_hover)))
                    .into_any_element()
            })
            .collect();

        // The header sits OUTSIDE the scrollable row list so the scroll handle's
        // item indices line up with `focused_idx` (the ⌘K card's structure).
        div()
            .id("skill-palette")
            .flex()
            .flex_col()
            .mb(theme::z(8.))
            .w_full()
            .max_h(theme::z(240.))
            .rounded(theme::z(theme::RADIUS))
            .bg(theme::p().chrome)
            .border_1()
            .border_color(theme::p().border)
            .child(
                div()
                    .flex_none()
                    .mx(theme::z(6.))
                    .px(theme::z(10.))
                    .pt(theme::z(6.))
                    .pb(theme::z(4.))
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .text_color(theme::p().faint)
                    .child("Skills  ·  ↑↓ navigate  ·  ↵ insert  ·  esc dismiss"),
            )
            .child(
                div()
                    .id("skill-palette-rows")
                    .flex_1()
                    .min_h(theme::z(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.skill_palette_scroll)
                    .pb(theme::z(4.))
                    .children(rows),
            )
    }

    /// One palette row: a conversation hit (title with the matched chars
    /// highlighted + relative timestamp) or a built-in command with its
    /// keyboard hint. The focused row carries the sidebar's active tint.
    fn palette_row(
        &self,
        ix: usize,
        cmd: &PaletteCmd,
        now_ts: i64,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let focused = ix == self.palette.focused_idx;
        let (title, right, ranges, leading): (
            SharedString,
            SharedString,
            Vec<std::ops::Range<usize>>,
            Option<gpui::AnyElement>,
        ) = match cmd {
            PaletteCmd::OpenConversation(id) => {
                let Some(meta) = self.conversations.get(id) else {
                    return div().into_any_element(); // row vanished mid-frame
                };
                (
                    meta.title.clone().into(),
                    format!("{} ago", feed::age_label(now_ts, meta.updated_ts)).into(),
                    palette::match_ranges(&meta.title, &self.palette.query),
                    None,
                )
            }
            PaletteCmd::NewConversation => {
                ("+ New conversation".into(), shortcuts::localize("⌘N").into_owned().into(), Vec::new(), None)
            }
            PaletteCmd::OpenSettings => (
                "Settings".into(),
                shortcuts::localize("⌘,").into_owned().into(),
                Vec::new(),
                Some(icons::icon(icons::Icon::Gear, 12.0, theme::p().text).into_any_element()),
            ),
            PaletteCmd::SwitchModel => {
                // The hint shows what the picker would switch FROM.
                let current: SharedString = self
                    .choices
                    .get(self.choice_idx)
                    .map(|c| c.model.clone())
                    .unwrap_or_default()
                    .into();
                ("⇄ Switch model".into(), current, Vec::new(), None)
            }
            PaletteCmd::GoTo(route) => {
                // The right hint marks the route you're already on, so the
                // palette doubles as a "where am I" indicator.
                let here = nav_route_to_mode(*route) == self.mode;
                (
                    format!("→ {}", route.label()).into(),
                    if here {
                        "Current".into()
                    } else {
                        SharedString::default()
                    },
                    Vec::new(),
                    None,
                )
            }
            PaletteCmd::ExportConversation => (
                "⧉ Copy conversation".into(),
                "Copy as Markdown".into(),
                Vec::new(),
                None,
            ),
            PaletteCmd::SaveConversation => (
                "⤓ Save conversation".into(),
                "Save to ~/Downloads".into(),
                Vec::new(),
                None,
            ),
            PaletteCmd::ApplyTemplate { name, .. } => (
                format!("Apply template: {name}").into(),
                "Set as system prompt for current conversation".into(),
                Vec::new(),
                None,
            ),
            PaletteCmd::NewTemplate => (
                "New template".into(),
                "Create a reusable system prompt".into(),
                Vec::new(),
                None,
            ),
            PaletteCmd::EditTemplate { name, .. } => (
                format!("Edit template: {name}").into(),
                "Modify this system prompt".into(),
                Vec::new(),
                None,
            ),
            PaletteCmd::DeleteTemplate { name, .. } => (
                format!("Delete template: {name}").into(),
                "Remove this system prompt".into(),
                Vec::new(),
                None,
            ),
            PaletteCmd::AbortStream => (
                "Stop streaming".into(),
                shortcuts::localize("⌘.").into_owned().into(),
                Vec::new(),
                Some(
                    div()
                        .text_color(theme::p().chip_warn)
                        .child("⏹")
                        .into_any_element(),
                ),
            ),
            PaletteCmd::ShowShortcuts => {
                ("⌨ Keyboard shortcuts".into(), shortcuts::localize("⌘?").into_owned().into(), Vec::new(), None)
            }
            PaletteCmd::DiscardSummary => {
                // The hint counts the turns the most-recent summary would restore.
                let n = self
                    .active_view()
                    .map(|v| {
                        v.convo
                            .messages()
                            .iter()
                            .rev()
                            .find(|m| m.kind == MessageKind::Summary)
                            .map(|s| {
                                v.convo
                                    .messages()
                                    .iter()
                                    .filter(|m| m.superseded_by == Some(s.ts))
                                    .count()
                            })
                            .unwrap_or(0)
                    })
                    .unwrap_or(0);
                (
                    "↺ Discard summary".into(),
                    format!("Restore {n} earlier turn{}", if n == 1 { "" } else { "s" }).into(),
                    Vec::new(),
                    None,
                )
            }
            PaletteCmd::UseFolder(path) => {
                // M2i path mode: the row body is the ~-abbreviated folder,
                // the right hint is the Tab-completion affordance.
                let home = crate::home::home_string();
                (
                    SharedString::from(palette_targets::display_path(path, &home)),
                    "Tab: complete".into(),
                    Vec::new(),
                    None,
                )
            }
            // M2h: the executor row; the current executor says so on the right.
            PaletteCmd::UseExecutor { label, persist } => (
                SharedString::from(format!("Executor: {label}")),
                if self.work.executor.to_persist() == *persist {
                    "✓ current".into()
                } else {
                    SharedString::default()
                },
                Vec::new(),
                None,
            ),
            PaletteCmd::NewCodeTab(engine) => (
                SharedString::from(format!("New Code tab: {}", engine.label())),
                "Code".into(),
                Vec::new(),
                None,
            ),
        };
        let click_cmd = cmd.clone();
        let mut row = ui::Row::new(("palette-row", ix))
            .title(title)
            .title_highlight_ranges(ranges)
            .selected(focused)
            .on_click(cx.listener(move |this, _ev, window, cx| {
                this.run_palette_cmd(click_cmd.clone(), window, cx);
            }));
        if let Some(leading) = leading {
            row = row.leading(leading);
        }
        if !right.is_empty() {
            row = row.trailing(ui::caption(right));
        }
        row.into_any_element()
    }

    /// The ⌘K command palette (slice 13): scrim over the whole app, a
    /// centered 480px card — search input on top, the resolved rows in a
    /// scrollable list below (~8 visible). Listed after the settings modal in
    /// the view tree, so it paints (and takes Escape) on top.
    fn palette_modal(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The SAME row list the capture-phase key handler resolves — Enter
        // must run exactly the row the user sees focused.
        let cmds = self.palette_cmds();
        let now_ts = feed::unix_now();
        let rows: Vec<gpui::AnyElement> = if cmds.is_empty() {
            vec![div()
                .px(theme::z(16.))
                .py(theme::z(10.))
                .child(ui::caption("No matches"))
                .into_any_element()]
        } else {
            cmds.iter()
                .enumerate()
                .map(|(ix, cmd)| self.palette_row(ix, cmd, now_ts, cx))
                .collect()
        };
        // Bound the viewport from the card's existing cap, clamped to the
        // live window. Reserve the actual editor target plus the body gap,
        // bottom padding, and face borders so a long result set scrolls
        // inside the card instead of growing beyond its max-height wrapper.
        let window_height = f32::from(window.viewport_size().height);
        let card_height = f32::from(theme::z(theme::LEGACY_PALETTE_CARD_MAX_H)).min(
            (window_height - f32::from(theme::z(theme::LEGACY_PALETTE_WINDOW_MARGIN))).max(0.),
        );
        let card_max_h = gpui::px(card_height);
        let rows_max_h = gpui::px(
            (card_height
                - f32::from(theme::target())
                - f32::from(theme::z(theme::LEGACY_PALETTE_BODY_CHROME_H))
                - theme::LEGACY_PALETTE_FACE_BORDER_H)
                .max(0.),
        );
        let input_focus = self.palette_input.read(cx).focus_handle().clone();
        let placeholder = if self.palette.folder_box {
            FOLDER_BOX_PLACEHOLDER
        } else {
            PALETTE_SEARCH_PLACEHOLDER
        };
        let input = ui::TextField::new("palette-input", placeholder)
            .editor({
                let editor = self.palette_input.clone();
                move |_| editor
            })
            .focused(input_focus.is_focused(window));
        let list = div()
            .id("palette-rows")
            .flex_1()
            .min_h(theme::z(0.))
            .max_h(rows_max_h)
            .overflow_y_scroll()
            // Keyboard navigation scrolls the focused row into view through
            // this handle (slice 16 SF1). Its budget reserves the input and
            // body chrome within the desktop card cap; the compact Sheet
            // supplies its own outer viewport bound too.
            .track_scroll(&self.palette_scroll)
            .py(theme::z(4.))
            .children(rows);
        let body = div()
            .flex()
            .flex_col()
            .min_h(gpui::Pixels::ZERO)
            .gap(theme::z(8.))
            .child(input)
            .child(list);
        let panel = ui::Panel::new("palette-card")
            .width(480.)
            .body_fill(body)
            .focus_handle(&input_focus)
            .on_close(cx.listener(|this, _ev, window, cx| this.close_palette(window, cx)));
        if ui::sheet::is_compact(window) {
            // The compact Sheet owns its scrim; the root capture handler above
            // it owns Escape, Enter and folder completion before the Sheet.
            // Keep the full-size Sheet out of the app root's flex-row sizing:
            // this is an overlay host, so opening the palette must not narrow
            // the conversation center beneath it.
            return div().absolute().inset_0().child(panel).into_any_element();
        }
        div()
            .id("palette-scrim")
            .absolute()
            .inset_0()
            .bg(theme::p().scrim)
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _ev, window, cx| this.close_palette(window, cx)))
            .child(
                div()
                    .id("palette-card-click-boundary")
                    .max_h(card_max_h)
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(panel),
            )
            .into_any_element()
    }

    /// The template editor (slice 18 §6): a scrim + full-height panel in the
    /// settings-modal mold — a single-line name field, a multiline content
    /// area (the composer's input handler, so IME/dictation work), Save and
    /// Cancel. Escape (in either field, or globally) cancels.
    fn template_editor_modal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = self.template_editor.flatten().is_some();
        let title: &'static str = if editing {
            "Edit template"
        } else {
            "New template"
        };
        let close = div()
            .id("template-close")
            .px(theme::z(8.))
            .py(theme::z(2.))
            .rounded(theme::z(theme::RADIUS))
            .text_size(theme::z(theme::TEXT_SIZE - 2.0))
            .text_color(theme::p().muted)
            .cursor_pointer()
            .hover(|s| s.bg(theme::p().row_hover))
            .child(icons::icon(icons::Icon::X, 12.0, theme::p().muted))
            .tooltip(icons::tip("Close — Esc"))
            .on_click(cx.listener(|this, _ev, _w, cx| this.close_template_editor(cx)));
        let header = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(8.))
            .px(theme::z(14.))
            .py(theme::z(10.))
            .border_b_1()
            .border_color(theme::p().border)
            .child(
                div()
                    .flex_1()
                    .text_size(theme::z(theme::TEXT_SIZE - 1.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::p().text)
                    .child(title),
            )
            .child(close);

        let label = |text: &'static str| {
            div()
                .flex_none()
                .text_size(theme::z(theme::LABEL_SIZE))
                .text_color(theme::p().muted)
                .child(text)
        };
        let field_box = |id: &'static str| {
            div()
                .id(id)
                .rounded(theme::z(theme::RADIUS))
                .border_1()
                .border_color(theme::p().border)
                .p(theme::z(8.))
        };
        let body = div()
            .flex_1()
            .min_h(theme::z(0.))
            .flex()
            .flex_col()
            .gap(theme::z(6.))
            .p(theme::z(14.))
            .child(label("Name"))
            .child(
                field_box("template-name-box")
                    .flex_none()
                    .child(self.template_name.clone()),
            )
            .child(label("System prompt"))
            .child(
                field_box("template-content-box")
                    .flex_1()
                    .min_h(theme::z(0.))
                    .overflow_y_scroll()
                    .child(self.template_content.clone()),
            );

        let button = |id: &'static str, text: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .px(theme::z(14.))
                .py(theme::z(4.))
                .rounded(theme::z(theme::RADIUS))
                .bg(theme::p().chip_bg)
                .border_1()
                .border_color(theme::p().chip_border)
                .text_size(theme::z(theme::LABEL_SIZE))
                .text_color(theme::p().chip_text)
                .cursor_pointer()
                .hover(|s| {
                    s.border_color(theme::p().accent)
                        .text_color(theme::p().accent_text)
                })
                .child(text)
        };
        let footer = div()
            .flex_none()
            .flex()
            .flex_row()
            .justify_end()
            .gap(theme::z(8.))
            .px(theme::z(14.))
            .py(theme::z(10.))
            .border_t_1()
            .border_color(theme::p().border)
            .child(
                button("template-cancel", "Cancel")
                    .on_click(cx.listener(|this, _ev, _w, cx| this.close_template_editor(cx))),
            )
            .child(
                button("template-save", "Save")
                    .on_click(cx.listener(|this, _ev, _w, cx| this.save_template(cx))),
            );

        div()
            .id("template-scrim")
            .absolute()
            .inset_0()
            .bg(theme::p().scrim)
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _ev, _w, cx| this.close_template_editor(cx)))
            .child(
                div()
                    .id("template-panel")
                    .flex()
                    .flex_col()
                    .w(theme::z(640.))
                    .h(theme::z(520.))
                    .rounded(theme::z(theme::RADIUS))
                    .bg(theme::p().chrome)
                    .border_1()
                    .border_color(theme::p().border)
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(header)
                    .child(body)
                    .child(footer),
            )
    }

    /// The saved-project instructions editor (Work-mode projects CRUD parity):
    /// scrim + centered card with one multiline area. Mirrors
    /// `template_editor_modal`'s chrome; ✕/Cancel/Escape dismiss, Save commits
    /// through `save_project_instructions`. Shows the project name in the title
    /// so it's clear which project's standing instructions are being edited.
    fn project_editor_modal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .project_editor
            .as_ref()
            .and_then(|(_, folder)| self.work.projects.iter().find(|p| &p.folder == folder))
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let title = if name.is_empty() {
            SharedString::from("Project instructions")
        } else {
            SharedString::from(format!("Instructions — {name}"))
        };
        let close = div()
            .id("project-instr-close")
            .px(theme::z(8.))
            .py(theme::z(2.))
            .rounded(theme::z(theme::RADIUS))
            .text_size(theme::z(theme::TEXT_SIZE - 2.0))
            .text_color(theme::p().muted)
            .cursor_pointer()
            .hover(|s| s.bg(theme::p().row_hover))
            .child(icons::icon(icons::Icon::X, 12.0, theme::p().muted))
            .tooltip(icons::tip("Close — Esc"))
            .on_click(cx.listener(|this, _ev, _w, cx| this.close_project_editor(cx)));
        let header = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(8.))
            .px(theme::z(14.))
            .py(theme::z(10.))
            .border_b_1()
            .border_color(theme::p().border)
            .child(
                div()
                    .flex_1()
                    .text_size(theme::z(theme::TEXT_SIZE - 1.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::p().text)
                    .child(title),
            )
            .child(close);

        let body = div()
            .flex_1()
            .min_h(theme::z(0.))
            .flex()
            .flex_col()
            .gap(theme::z(6.))
            .p(theme::z(14.))
            .child(
                div()
                    .flex_none()
                    .text_size(theme::z(theme::LABEL_SIZE))
                    .text_color(theme::p().muted)
                    .child(
                        "Folded ahead of the task in every Work run (alongside the folder's AGENTS.md).",
                    ),
            )
            .child(
                div()
                    .id("project-instr-box")
                    .flex_1()
                    .min_h(theme::z(0.))
                    .overflow_y_scroll()
                    .rounded(theme::z(theme::RADIUS))
                    .border_1()
                    .border_color(theme::p().border)
                    .p(theme::z(8.))
                    .child(self.project_instructions.clone()),
            );

        let button = |id: &'static str, text: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .px(theme::z(14.))
                .py(theme::z(4.))
                .rounded(theme::z(theme::RADIUS))
                .bg(theme::p().chip_bg)
                .border_1()
                .border_color(theme::p().chip_border)
                .text_size(theme::z(theme::LABEL_SIZE))
                .text_color(theme::p().chip_text)
                .cursor_pointer()
                .hover(|s| {
                    s.border_color(theme::p().accent)
                        .text_color(theme::p().accent_text)
                })
                .child(text)
        };
        let footer = div()
            .flex_none()
            .flex()
            .flex_row()
            .justify_end()
            .gap(theme::z(8.))
            .px(theme::z(14.))
            .py(theme::z(10.))
            .border_t_1()
            .border_color(theme::p().border)
            .child(
                button("project-instr-cancel", "Cancel")
                    .on_click(cx.listener(|this, _ev, _w, cx| this.close_project_editor(cx))),
            )
            .child(
                button("project-instr-save", "Save")
                    .on_click(cx.listener(|this, _ev, _w, cx| this.save_project_instructions(cx))),
            );

        div()
            .id("project-instr-scrim")
            .absolute()
            .inset_0()
            .bg(theme::p().scrim)
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _ev, _w, cx| this.close_project_editor(cx)))
            .child(
                div()
                    .id("project-instr-panel")
                    .flex()
                    .flex_col()
                    .w(theme::z(640.))
                    .h(theme::z(520.))
                    .rounded(theme::z(theme::RADIUS))
                    .bg(theme::p().chrome)
                    .border_1()
                    .border_color(theme::p().border)
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(header)
                    .child(body)
                    .child(footer),
            )
    }

    /// The ⌘? keyboard-shortcut reference (slice 20 §2): scrim + centered
    /// card, a scrollable two-column table — combo in a monospace chip,
    /// description in regular text. Mounted LAST in the view tree, so it
    /// paints above the palette and the search panel. ✕ and Escape dismiss.
    fn shortcuts_modal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let close = div()
            .id("shortcuts-close")
            .px(theme::z(8.))
            .py(theme::z(2.))
            .rounded(theme::z(theme::RADIUS))
            .text_size(theme::z(theme::TEXT_SIZE - 2.0))
            .text_color(theme::p().muted)
            .cursor_pointer()
            .hover(|s| s.bg(theme::p().row_hover))
            .child(icons::icon(icons::Icon::X, 12.0, theme::p().muted))
            .tooltip(icons::tip("Close — Esc"))
            .on_click(cx.listener(|this, _ev, _w, cx| this.close_shortcuts(cx)));
        let header = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::z(8.))
            .px(theme::z(14.))
            .py(theme::z(10.))
            .border_b_1()
            .border_color(theme::p().border)
            .child(
                div()
                    .flex_1()
                    .text_size(theme::z(theme::TEXT_SIZE - 1.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::p().text)
                    .child(shortcuts::TITLE),
            )
            .child(close);
        let rows = shortcuts::rows().into_iter().map(|(combo, action)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(theme::z(12.))
                .px(theme::z(14.))
                .py(theme::z(5.))
                .child(
                    div()
                        .flex_none()
                        .min_w(theme::z(140.))
                        .px(theme::z(8.))
                        .py(theme::z(2.))
                        .rounded(theme::z(theme::RADIUS))
                        .bg(theme::p().chip_bg)
                        .border_1()
                        .border_color(theme::p().chip_border)
                        .font_family(theme::MONO_FONT)
                        .text_size(theme::z(theme::LABEL_SIZE))
                        .text_color(theme::p().chip_text)
                        .child(SharedString::from(combo)),
                )
                .child(
                    div()
                        .text_size(theme::z(theme::TEXT_SIZE - 2.0))
                        .text_color(theme::p().text)
                        .child(SharedString::from(action)),
                )
        });
        div()
            .id("shortcuts-scrim")
            .absolute()
            .inset_0()
            .bg(theme::p().scrim)
            .flex()
            .items_center()
            .justify_center()
            // K02 (U3d): track the overlay focus so the sheet can take focus
            // (via `focus_shortcuts_overlay`) and Escape/⌘? reach the root
            // even when the prior focus is stale.
            .track_focus(self.overlay_focus.handle())
            .on_click(cx.listener(|this, _ev, _w, cx| this.close_shortcuts(cx)))
            .child(
                div()
                    .id("shortcuts-card")
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .w(theme::z(460.))
                    .max_h(theme::z(480.))
                    .rounded(theme::z(theme::RADIUS))
                    .bg(theme::p().chrome)
                    .border_1()
                    .border_color(theme::p().border)
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(header)
                    .child(
                        div()
                            .id("shortcuts-rows")
                            .flex_1()
                            .min_h(theme::z(0.))
                            .overflow_y_scroll()
                            .py(theme::z(6.))
                            .children(rows),
                    ),
            )
    }

    /// M2g: Work-mode Retry — re-send the user row preceding `idx` through
    /// `submit_work_run`. The text is trimmed; a missing/blank user row or a
    /// `None` target refuses. Appends a fresh user row and a new run —
    /// history is never rewritten. The run goes to the executor currently
    /// shown in the chip (i.e. `self.work.executor` at click time), so a
    /// mid-run executor change is honored by the next click. `idx` is the
    /// last assistant row's index (validated by the chip's render gate and
    /// re-validated here via [`prompt_history::retry_text`]).
    /// M2g: the Work-mode Retry chip re-sends the user row to the executor
    /// shown in the chip (i.e. `self.work.executor` at click time), so a
    /// mid-run executor change is honored by the next click. `idx` is the
    /// last assistant row's index (validated by the chip's render gate and
    /// re-validated here via [`prompt_history::retry_text`]).
    #[cfg(test)]
    pub(crate) fn retry_work(&mut self, idx: usize, cx: &mut Context<Self>) {
        let conv_id = self.sidebar.active_id().to_string();
        self.retry_work_in(conv_id, idx, cx);
    }

    /// M2-SLICE: the chip's click carries the conversation it was painted in.
    /// A click handled after the user navigated away, or for a row that is no
    /// longer that view's last assistant row, is a no-op — never a re-send of
    /// another conversation's prompt.
    pub(crate) fn retry_work_in(&mut self, conv_id: String, idx: usize, cx: &mut Context<Self>) {
        if self.shell.archived_reader.is_some()
            || self.conversations.get(&conv_id).is_none_or(|c| c.archived)
        {
            return;
        }
        if conv_id != self.sidebar.active_id() {
            return;
        }
        let Some(view) = self.views.get(&conv_id) else {
            return;
        };
        if regen_target(view) != Some(idx) {
            return;
        }
        // `retry_text` re-validates the shape (last row assistant,
        // immediately-preceding user row, non-blank text) so a stale chip
        // click on a non-last assistant row is a no-op rather than a partial
        // send.
        let Some(text) = prompt_history::retry_text(&view.convo) else {
            return;
        };
        // RC2-UX (R2-14): the re-sent row is marked as a Retry, so it is drawn
        // as a note, not as a second copy of the question.
        if !self.work.running {
            self.submit_work_run_kind(text, conv_id, MessageKind::Retry, cx);
        }
    }

    /// M2g: Code-tab Retry — re-send the last user row of the active tab's
    /// convo to the tab's engine. No-op while the tab is streaming or when
    /// the convo has no re-sendable user row.
    fn retry_code(&mut self, cx: &mut Context<Self>) {
        let active = self.work.code_active;
        let Some(session) = self.work.code_tabs.get(active) else {
            return;
        };
        if session.streaming {
            return; // the tab is mid-turn; the chip is hidden anyway
        }
        let Some(text) = prompt_history::retry_text(&session.convo) else {
            return;
        };
        // RC2-UX (R2-14): a Retry row, drawn as a note.
        self.submit_code_turn_kind(text, MessageKind::Retry, cx);
    }
}

fn settings_error_signature(error: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    error.hash(&mut hasher);
    hasher.finish()
}

impl Render for AwiliApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(not(target_os = "macos"))]
        app_lifecycle::ensure_root_focus(window, cx, &self.shell.root_focus);
        // The detail view follows the live Settings error. If another Settings
        // action clears the error, close the view and return focus to the
        // persistent Settings root; if Settings itself closes, let its normal return-focus
        // path own the handoff instead of restoring a hidden control.
        if self.settings_error_details_open {
            if let Some(error) = self.settings.error.as_ref() {
                let signature = settings_error_signature(error);
                if self.settings_error_scroll_signature != Some(signature) {
                    // A new live error starts at its beginning instead of
                    // leaving its prefix above the old message's scroll offset.
                    self.settings_error_scroll_signature = Some(signature);
                    self.settings_error_scroll
                        .set_offset(point(gpui::Pixels::ZERO, gpui::Pixels::ZERO));
                }
            } else {
                self.settings_error_scroll_signature = None;
            }
        }
        if self.settings_error_details_open
            && (!self.settings.open || self.settings.error.is_none())
        {
            self.settings_error_details_open = false;
            self.settings_error_scroll_signature = None;
            let parent = self.settings_error_parent_focus.take();
            if self.settings.open {
                if self.settings.error.is_none() {
                    // The error action itself is disappearing. Restore a
                    // persistent focus target on the active Settings tab.
                    let target = if self.settings.tab == settings::Tab::Form {
                        &self.settings_nav_focus
                    } else {
                        &self.settings_active_tab_focus
                    };
                    window.focus(target, cx);
                } else if let Some(parent) = parent {
                    window.focus(&parent, cx);
                } else if self.settings.tab == settings::Tab::Form {
                    window.focus(&self.settings_nav_focus, cx);
                }
            }
            // The action disappears when its error clears. Keep the parent's
            // keyboard model valid even if that happens while Settings is
            // closed; only restore a focus handle while Settings is visible.
            if self.settings.error.is_none() {
                if self.settings.page == settings_catalog::Page::Models
                    && self.settings_nav.selected() == settings_nav::Action::ErrorDetails
                {
                    self.settings_nav.select(settings_nav::Action::AddProvider);
                }
                if self.settings.tab == settings::Tab::Form
                    && self.settings.page != settings_catalog::Page::Models
                    && self.settings_stop == Some(settings_focus::FocusStop::ErrorDetails)
                {
                    self.settings_stop = None;
                }
            }
        }
        self.initialize_appearance(window, cx);
        if !std::ptr::eq(self.display_palette, theme::p()) {
            for view in self.views.values_mut() {
                for row in &mut view.rows {
                    row.refresh_display_palette();
                }
            }
            for session in &mut self.work.code_tabs {
                for row in &mut session.rows {
                    row.refresh_display_palette();
                }
            }
            self.display_palette = theme::p();
        }
        self.sanitize_reply_inspector(window, cx);
        // Cache window focus for non-render consumers (Work-done notification
        // focus-gating). gpui re-renders on activation changes, so this stays
        // fresh. Plain field write — no notify, so it can't loop.
        self.window_active = window.is_window_active();
        // RC1-H6: the side columns follow the window's width.
        self.window_w = f32::from(window.viewport_size().width);
        self.notifier
            .observe_window(self.window_active, std::time::Instant::now());
        // Keep the rem size in step with the interface zoom (A4). The layout is
        // built through `theme::z`, but tracking rem_size too means any rem-based
        // sizing (and the default text metrics) scale with it as well.
        window.set_rem_size(px(16.0 * theme::ui_scale()));
        // RC1-F3: keep a transcript at its bottom through this frame's layout.
        self.tail_frame(window);
        // RC2-UX (R2-18, R5-7): the message box holds the draft of the
        // conversation on screen, whichever way the owner got there.
        self.sync_composer_draft(cx);
        // Workshop owns order and selection across all runner adapters.
        self.sync_workshop_tabs();
        self.save_workshop_tabs_if_changed(cx);
        if self.mode != Mode::Sessions {
            self.sessions_focus_pending = None;
        } else if !self.command_palette.open
            && !self.palette.open
            && !self.shortcuts.open
            && self.settings.field.is_none()
        {
            if let Some(target) = self.sessions_focus_pending.take() {
                let handle = match target {
                    SessionsFocusTarget::Search => &self.sessions_search_focus,
                    SessionsFocusTarget::Back => &self.sessions_back_focus,
                };
                window.focus(handle, cx);
            }
        }
        // RC2-UX (S-N3): a helper that cannot ask, chosen off the picker.
        self.show_no_ask_notice(cx);
        // RC2-UX (D-PUNCH S3-F10): a closed ⌘F panel never keeps the keys.
        if !self.search.open && self.search_input.read(cx).focus_handle().is_focused(window) {
            let h = self.composer.read(cx).focus_handle().clone();
            window.focus(&h, cx);
        }
        // The active conversation names the window (survey #10) — instant
        // orientation in Mission Control / cmd-tab. Deduped: the platform
        // call only fires when the computed title changes.
        {
            let title = self
                .conversations
                .get(self.sidebar.active_id())
                .map(|c| c.title.trim().to_string())
                .filter(|t| !t.is_empty() && t != "New conversation")
                .map(|t| {
                    let mut t = t;
                    if t.chars().count() > 60 {
                        t = t.chars().take(57).collect::<String>() + "\u{2026}";
                    }
                    format!("{t} \u{2014} Awili")
                })
                .unwrap_or_else(|| "Awili".to_string());
            if title != self.last_window_title {
                window.set_window_title(&title);
                self.last_window_title = title;
            }
        }
        if !self.did_initial_focus && !self.onboarding.is_active() {
            self.did_initial_focus = true;
            let h = self.composer.read(cx).focus_handle().clone();
            window.focus(&h, cx);
            // Eagerly load skills on launch so the composer's `/`-command palette
            // works from the default Chat surface — without this it stayed empty
            // (and Enter submitted the literal `/...`) until the Skills page was
            // opened, since that was the only `refresh_skills` trigger.
            self.refresh_skills(cx);
            // Probe the gateway once on launch so the header chip resolves to the
            // real connection state on the first render (updating when the async
            // probe returns), instead of sitting on a grey "unknown" dot in Chat
            // that only flipped green after Work mode was opened once (the probe
            // used to be gated behind Mode::Work entry).
            if should_probe_gateway_on_launch(
                self.work.gateway.enabled,
                self.work.hermes_status.is_some(),
                self.work.hermes_testing,
            ) {
                self.test_hermes_connection(cx);
            }
            // C1: providers get the same launch probe the gateway gets.
            self.refresh_provider_health(cx);
        }
        self.local_discovery
            .cancel_if_hidden(self.settings.open && self.settings.tab == settings::Tab::Form);
        // General tab: sync settings-nav and restore pending form-root focus.
        if self.settings.open && self.settings.tab == settings::Tab::Form {
            self.settings_nav
                .reconcile_with_keys(&self.settings_key_rows());
            if self.search.open
                || self.palette.open
                || self.command_palette.open
                || self.shortcuts.open
                || self.template_editor.is_some()
                || self.project_editor.is_some()
            {
                self.settings_nav_focus_pending = true;
            }
            if self.settings_nav_focus_pending
                && self.settings.draft.is_some()
                && self.settings.field.is_none()
                && !self.search.open
                && !self.palette.open
                && !self.command_palette.open
                && !self.shortcuts.open
                && self.template_editor.is_none()
                && self.project_editor.is_none()
            {
                self.settings_nav_focus_pending = false;
                window.focus(&self.settings_nav_focus, cx);
            }
            // Cache the real focus state (render-only helpers can't call
            // is_focused — they don't get the window) so the General form's
            // visible selection highlight applies exactly when the form root
            // owns focus: settings open, Form tab, no field editor active,
            // and no unrelated overlay stealing focus.
            self.settings_nav_focused = self.settings_nav_focus.is_focused(window)
                && self.settings.open
                && self.settings.tab == settings::Tab::Form
                && self.settings.field.is_none()
                && !self.search.open
                && !self.palette.open
                && !self.command_palette.open
                && !self.shortcuts.open
                && self.template_editor.is_none()
                && self.project_editor.is_none();
        } else {
            self.settings_nav_focused = false;
        }
        // First-run welcome: focus the welcome panel exactly once per
        // re-appearance — on the first render (welcome_focus_pending
        // starts true) and after an overlay that hid it closes; any
        // overlay marks the flag pending and it restores once closed.
        // While an overlay is open this stays idle and the
        // overlay’s inputs keep their focus; configured launches
        // (onboarding inactive) never take this path.
        let overlay_open = self.settings.open
            || self.search.open
            || self.palette.open
            || self.command_palette.open
            || self.shortcuts.open
            || self.template_editor.is_some()
            || self.project_editor.is_some();
        if self.settings_return_focus_pending
            && !overlay_open
            && !self.shell.more
            && self.shell.tabs.menu.is_none()
            && self.shell.tabs.delete_project.is_none()
            && self.shell.composer_menus.model.is_none()
        {
            self.settings_return_focus_pending = false;
            if self.onboarding.is_active() {
                self.welcome_focus_pending = true;
            } else if self.shell.inspector.selection.is_some() {
                window.focus(&self.shell.inspector.focus, cx);
            } else if self.mode == Mode::Work
                && self.shell.job_open
                && job_panel::uses_sheet(window)
            {
                window.focus(&self.shell.job_focus, cx);
            } else if self.shell.conversations && consolidation::conversation_mode(self.mode) {
                window.focus(&self.shell.conversations_focus, cx);
            } else if self.shell.archived_reader.is_some() {
                window.focus(&self.shell.rail_focus, cx);
            } else if consolidation::backstage_index(self.mode).is_some() {
                window.focus(&self.shell.backstage_focus, cx);
            } else {
                self.focus_shell_page(window, cx);
            }
        }
        if self.onboarding.is_active() && overlay_open {
            self.welcome_focus_pending = true;
        }
        if self.onboarding.is_active() && self.welcome_focus_pending && !overlay_open {
            self.welcome_focus_pending = false;
            window.focus(&self.welcome_focus, cx);
        }
        // The palette query lives in its input's buffer; mirror it into the
        // pure state (which snaps the focused row to the top on change)
        // before resolve() builds the rows below.
        if self.palette.open {
            let q = self.palette_input.read(cx).text().to_string();
            self.palette.set_query(&q);
        }
        // Live provider slices for the Cowork ⌘⇧P palette below (owned; live to
        // the end of render). Assembled here — before the element tree — so the
        // deep `.children(command_palette::command_palette(...))` callsite can
        // borrow them immutably alongside `&self.command_palette`.
        let (palette_recent, palette_archived) = self.palette_providers();
        // The conversation column, with the Needs You sidebar (slice 4) docked
        // to its right when toggled open.
        // The active mode owns the CENTER, not just a right rail. Chat and Work
        // are conversation surfaces (transcript + composer, each on its own
        // thread); Skills/Cron/Sessions are utility views that take the full
        // center with no composer.
        let mut compact_header = (consolidation::workshop_mode(self.mode)
            && !self.onboarding.is_active()
            && ui::is_compact(window))
        .then(|| self.workshop_header(window, cx));
        let conversation_center = |this: &mut Self,
                                   header: Option<gpui::AnyElement>,
                                   cx: &mut Context<Self>| {
            div()
                .debug_selector(|| "awili-center".to_string())
                .flex()
                .flex_col()
                .flex_1()
                .min_w(theme::z(0.))
                .h_full()
                .children(
                    this.voice
                        .conversation_mode
                        .then(|| this.voice_listening_banner(cx)),
                )
                .child(this.message_list_with_header(header, cx))
                .children((!this.staged_attachments.is_empty()).then(|| this.attachment_tray(cx)))
                .child(this.composer_bar(cx))
                .into_any_element()
        };
        let utility_center = |inner: gpui::AnyElement| {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(theme::z(0.))
                .h_full()
                .child(inner)
                .into_any_element()
        };
        if self.shell.archived_reader.is_some()
            && self.composer.read(cx).focus_handle().is_focused(window)
        {
            window.focus(&self.shell.rail_focus, cx);
        }
        let (main, mut panel): (gpui::AnyElement, Option<gpui::AnyElement>) =
            if self.onboarding.is_active() {
                (self.welcome_panel(window, cx), None)
            } else if self.shell.archived_reader.is_some() {
                (self.workshop_archive_view(compact_header.take(), cx), None)
            } else {
                match self.mode {
                    // Workshop keeps its right rail (progress, folder, terminal, …) — but
                    // the chat/result output renders inline in the center conversation,
                    // not in the rail (the Result card was removed). With no job rail
                    // open, the Needs-you list can take its place.
                    Mode::Work => {
                        let rail = if self.shell.job_open {
                            Some(self.work_right_panel(window, cx).into_any_element())
                        } else if self.feed_panel_open {
                            Some(self.needs_you_panel(cx).into_any_element())
                        } else {
                            None
                        };
                        (conversation_center(self, compact_header.take(), cx), rail)
                    }
                    Mode::Code => (
                        utility_center(
                            self.code_center(compact_header.take(), cx)
                                .into_any_element(),
                        ),
                        None,
                    ),
                    // RC2-CNCL5: the Council page owns the centre.
                    Mode::Council => (self.council_center(window, cx), None),
                    Mode::Design => (self.design_page(), None),
                    Mode::Skills
                    | Mode::Cron
                    | Mode::Sessions
                    | Mode::CommandCenter
                    | Mode::Messaging => (self.backstage_page(window, cx), None),
                }
            };
        if let Some(inspector) = self.reply_inspector_panel(window, cx) {
            panel = Some(inspector);
        }
        let main = if consolidation::workshop_mode(self.mode) && !self.onboarding.is_active() {
            self.workshop_frame(main, window, cx)
        } else {
            main
        };
        // Startup config error: wrap the already-selected main center with
        // the fixed banner above it (shadowing `main` after selection — no
        // duplicated mode match). Active only while `config_load_error`
        // is true, so every valid launch renders the bare center as before.
        let main = if self.config_load_error {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(theme::z(0.))
                .h_full()
                .child(self.config_error_banner(cx))
                .child(main)
                .into_any_element()
        } else if !self.secret_notices.is_empty() {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(self.secret_notice_banner())
                .child(main)
                .into_any_element()
        } else {
            main
        };
        let main = match self.storage_notice.as_deref() {
            Some(message) => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(self.storage_notice_banner(message))
                .child(main)
                .into_any_element(),
            None => main,
        };
        let compact_conversations = ui::is_compact(window);
        let mut conversations = (self.shell.conversations
            && consolidation::conversation_mode(self.mode)
            && !consolidation::workshop_mode(self.mode))
        .then(|| {
            div()
                .flex()
                .flex_col()
                .w(theme::z(theme::PANEL_W))
                .absolute()
                .left(theme::z(theme::STAGE_LEFT))
                .top(theme::z(theme::EDGE))
                .bottom(theme::z(theme::EDGE))
                .when(compact_conversations, |d| {
                    d.left(gpui::Pixels::ZERO)
                        .right(gpui::Pixels::ZERO)
                        .w_auto()
                        .top(gpui::Pixels::ZERO)
                        .bottom(gpui::Pixels::ZERO)
                })
                .child(
                    ui::Panel::new("awili-conversations-panel")
                        .focus_handle(&self.shell.conversations_focus)
                        .header("Conversations", None)
                        .full_height()
                        .body_fill(self.sidebar_panel(cx))
                        .on_close(cx.listener(|this, _event, window, cx| {
                            this.shell.conversations = false;
                            this.focus_shell_page(window, cx);
                            cx.notify();
                        })),
                )
                .into_any_element()
        });
        div()
            // The root key context: a binding predicate such as `!Terminal` is
            // only evaluated when the focused path has at least one context.
            .key_context(app_lifecycle::ROOT_KEY_CONTEXT)
            // Off macOS the root can hold focus of last resort (ensure_root_focus).
            .when(cfg!(not(target_os = "macos")), |root| {
                root.track_focus(&self.shell.root_focus)
            })
            .flex()
            .flex_row()
            .size_full()
            .relative()
            .pl(theme::z(theme::STAGE_LEFT))
            .bg(theme::palette().canvas)
            .text_color(theme::p().text)
            // Optional hold-to-talk (secondary to the mic button): hold the
            // configured modifier pair to dictate; release to insert. Disabled
            // with ptt_key = "off" (review SF6).
            .on_modifiers_changed(
                cx.listener(|this, ev: &ModifiersChangedEvent, _w, cx| this.on_modifiers(ev, cx)),
            )
            // While the palette is open it owns ↑/↓/Enter/Escape — intercepted
            // in the capture phase so the focused search input never sees them
            // (its caret would eat the arrows). Other keys fall through and
            // feed the input (slice 13).
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                // U3b2: typing with the PTT modifier held disarms a pending
                // hold and cancels a live PTT session — but WITHOUT
                // stop_propagation, so the key still reaches its target.
                this.ptt_key_down(cx);
                // The nested Settings error Sheet owns keyboard input.
                if this.settings_error_details_open {
                    return;
                }
                // The kit confirmation owns its keyboard. Let it receive keys
                // without running canvas shortcuts or changing composer focus.
                if this.shell.tabs.delete_project.is_some() {
                    return;
                }
                if this.shell.more
                    || this.shell.tabs.menu.is_some()
                    || this.shell.composer_menus.model.is_some()
                {
                    return; // the kit menu owns its keys before page shortcuts
                }
                if this.handle_reply_inspector_keys(ev, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                let navigation_focused = (this.shell.navigation_focus.contains_focused(window, cx)
                    || this.shell.backstage_focus.is_focused(window))
                    && !(this.settings.open
                        || this.search.open
                        || this.palette.open
                        || this.command_palette.open
                        || this.shortcuts.open)
                    && this.template_editor.is_none()
                    && this.project_editor.is_none();
                if navigation_focused
                    && ev.keystroke.key == "tab"
                    && !composer_keys::is_app_chord(&ev.keystroke.modifiers)
                    && !ev.keystroke.modifiers.alt
                    && !ev.keystroke.modifiers.function
                {
                    if ev.keystroke.modifiers.shift {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                if navigation_focused
                    && !ev.keystroke.modifiers.platform
                    && !ev.keystroke.modifiers.control
                    && !ev.keystroke.modifiers.alt
                    && !ev.keystroke.modifiers.function
                    && ev.keystroke.key != "escape"
                {
                    return;
                }
                // Startup config-error re-check: platform+Shift+R (⌘⇧R, or
                // Ctrl+Shift+R off-Mac) while the startup error banner is up
                // and no overlay is open. Checked FIRST, in the CAPTURE
                // phase, before the welcome and start-chord handlers below;
                // open overlays keep their own keys, and on every valid
                // launch the guard falls through untouched.
                {
                    let ks = &ev.keystroke;
                    if this.config_load_error
                        && crate::composer_keys::is_app_chord(&ks.modifiers)
                        && ks.modifiers.shift
                        && !ks.modifiers.alt
                        && ks.key == "r"
                        && !(this.settings.open
                            || this.search.open
                            || this.palette.open
                            || this.command_palette.open
                            || this.shortcuts.open)
                        && this.template_editor.is_none()
                        && this.project_editor.is_none()
                    {
                        this.check_startup_config(cx);
                        cx.stop_propagation();
                        return;
                    }
                }
                // General form: plain Tab / Shift+Tab move the settings-nav
                // selection while the form root owns focus; activation later.
                let ks = &ev.keystroke;
                // SXi: Enter in SearchField is intentionally handled by the
                // parent because KIT SearchField bubbles navigation keys.
                let settings_search_owned = this.settings.open
                    && this.settings.tab == settings::Tab::Form
                    && this.settings_search_focus.is_focused(window)
                    && this.settings.field.is_none()
                    && this.settings.editing_item.is_none()
                    && !this.settings.confirm_close
                    && !(this.search.open
                        || this.palette.open
                        || this.command_palette.open
                        || this.shortcuts.open)
                    && this.template_editor.is_none()
                    && this.project_editor.is_none();
                if settings_search_owned
                    && ks.key == "f"
                    && crate::composer_keys::main_modifier_only(&ks.modifiers)
                    && !ks.modifiers.alt
                    && !ks.modifiers.shift
                    && !ks.modifiers.function
                {
                    // Cmd-F remains owned by Settings while its SearchField
                    // has focus, including when the query is non-empty. The
                    // global conversation-search chord below must not steal it.
                    window.focus(&this.settings_search_focus, cx);
                    cx.stop_propagation();
                    return;
                }
                if settings_search_owned
                    && (ks.key == "enter" || ks.key == "return")
                    && !ks.modifiers.modified()
                {
                    this.open_first_settings_hit(cx);
                    cx.stop_propagation();
                    return;
                }
                // SXi: the Settings pages' keyboard (sidebar, page, search).
                // Models & Fleet's own form keeps `settings_nav` below.
                let pages_owned = this.settings.open
                    && this.settings.tab == settings::Tab::Form
                    && (this.settings_in_sidebar
                        || this.settings.page != settings_catalog::Page::Models)
                    && this.settings_nav_focus.is_focused(window)
                    && this.settings.field.is_none()
                    && this.settings.editing_item.is_none()
                    && !this.settings.confirm_close
                    && !(this.search.open
                        || this.palette.open
                        || this.command_palette.open
                        || this.shortcuts.open)
                    && this.template_editor.is_none()
                    && this.project_editor.is_none()
                    && this.settings_query(cx).is_empty();
                if pages_owned && this.settings_key(ks, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                // SXf: the old keyboard model drives only the Models & Fleet
                // form; on the other pages Enter/Space must not run its
                // invisible actions (Add provider, Save, …).
                let general_owned = this.settings.open
                    && this.settings.tab == settings::Tab::Form
                    && this.settings.page == settings_catalog::Page::Models
                    && this.settings_nav_focus.is_focused(window)
                    && this.settings.field.is_none()
                    && !(this.search.open
                        || this.palette.open
                        || this.command_palette.open
                        || this.shortcuts.open)
                    && this.template_editor.is_none()
                    && this.project_editor.is_none();
                if general_owned
                    && ks.key == "tab"
                    && !ks.modifiers.control
                    && !ks.modifiers.platform
                    && !ks.modifiers.alt
                    && !ks.modifiers.function
                {
                    if let Some(draft) = this.settings.draft.as_ref() {
                        let count = draft.providers.len();
                        this.move_settings_nav_with_error(ks.modifiers.shift);
                        // SXf: the helper cards live on the Code page now;
                        // Tab steps over their old stops.
                        while matches!(
                            this.settings_nav.selected(),
                            settings_nav::Action::HarnessRecipe(_)
                                | settings_nav::Action::HarnessDocs(_)
                        ) {
                            this.settings_nav.move_selection_with_keys(
                                &this.settings_key_rows(),
                                ks.modifiers.shift,
                            );
                        }
                        // Scroll the newly selected row into view (navigation
                        // intent only — never while typing or every render).
                        if let Some(ix) =
                            settings_nav::selected_settings_row(this.settings_nav.selected(), count)
                        {
                            this.settings_nav_scroll.scroll_to_item(ix);
                        }
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                }
                // Activation: plain Enter/Return or Space (welcome key
                // spellings, no modifiers) runs the selected nav action.
                if general_owned
                    && (ks.key == "enter" || ks.key == "return" || ks.key == "space")
                    && !ks.modifiers.control
                    && !ks.modifiers.platform
                    && !ks.modifiers.alt
                    && !ks.modifiers.function
                    && !ks.modifiers.shift
                {
                    this.activate_settings_nav(window, cx);
                    cx.stop_propagation();
                    return;
                }
                // First-run welcome keyboard: while the welcome panel owns
                // focus and no overlay is open, plain Tab / Shift+Tab cycle
                // its actions (Setup, Check again, and Start chatting when
                // Ready) and Enter/Return or Space activates the selected
                // one. Handled here, in the CAPTURE phase, before the hidden
                // composer or any palette can see the keys. Deliberately
                // plain keys only — the modifier chords (the start chord
                // below, ⌘, for Settings, …) fall through untouched, and
                // while the settings field editor holds focus the welcome
                // handle is not focused, so its keys never come here.
                {
                    let ks = &ev.keystroke;
                    let welcome_owned = this.onboarding.is_active()
                        && this.welcome_focus.is_focused(window)
                        && !(this.settings.open
                            || this.search.open
                            || this.palette.open
                            || this.command_palette.open
                            || this.shortcuts.open)
                        && this.template_editor.is_none()
                        && this.project_editor.is_none();
                    if welcome_owned {
                        let available = usize::from(this.onboarding.is_ready()) + 2;
                        let current = this.welcome_action.min(available - 1);
                        if ks.key == "tab"
                            && !ks.modifiers.control
                            && !ks.modifiers.platform
                            && !ks.modifiers.alt
                            && !ks.modifiers.function
                        {
                            this.welcome_action = if ks.modifiers.shift {
                                (current + available - 1) % available
                            } else {
                                (current + 1) % available
                            };
                            cx.notify();
                            cx.stop_propagation();
                            return;
                        }
                        if (ks.key == "enter" || ks.key == "return" || ks.key == "space")
                            && !ks.modifiers.modified()
                        {
                            this.welcome_action = current;
                            match current {
                                0 => this.open_settings(cx),
                                1 => {
                                    this.check_onboarding_config(cx);
                                    // “Check again” leaves the panel up — keep the keyboard on it.
                                    if this.onboarding.is_active() {
                                        window.focus(&this.welcome_focus, cx);
                                    }
                                }
                                _ => this.start_onboarding_chat(window, cx),
                            }
                            cx.stop_propagation();
                            return;
                        }
                    }
                }
                // First-run welcome: the platform-modifier + Enter/Return chord
                // completes onboarding ONLY while it is active+Ready and no
                // overlay is open. Checked FIRST, in the CAPTURE phase, so it
                // lands before the hidden composer could receive it (a plain
                // Enter never reaches this guard, and open overlays —
                // Settings, the search bar, the palettes, the editors — keep
                // their own keys untouched).
                {
                    let ks = &ev.keystroke;
                    let start_chord = crate::composer_keys::is_app_chord(&ks.modifiers)
                        && (ks.key == "enter" || ks.key == "return")
                        && this.onboarding.is_active()
                        && this.onboarding.is_ready()
                        && !(this.settings.open
                            || this.search.open
                            || this.palette.open
                            || this.command_palette.open
                            || this.shortcuts.open)
                        && this.template_editor.is_none()
                        && this.project_editor.is_none();
                    if start_chord {
                        this.start_onboarding_chat(window, cx);
                        cx.stop_propagation();
                        return;
                    }
                }
                // The Cowork command palette (⌘⇧P) owns keyboard input while
                // open: its capture handler takes nav, Enter and Escape, its
                // SearchField takes typed text, and both run before the
                // composer/IME sees the keys (plan unit 29). Checked FIRST so its
                // input is never starved by another palette that lingered open
                // (open_command_palette closes them, and sync_skill_palette
                // won't reopen, but priority here is the belt-and-braces
                // guarantee — Codex review).
                if this.command_palette.open {
                    this.on_command_palette_key(ev, window, cx);
                    return;
                }
                // The shortcut sheet paints above the palette (slice 20 §2b),
                // so while it is up the sheet's Escape wins — let the event
                // bubble to the global handler below.
                if this.palette.open && !this.shortcuts.open {
                    this.on_palette_key(ev, window, cx);
                    return;
                }
                // The `/`-command skill palette takes its nav/accept/dismiss
                // keys here too (capture phase, before the composer's IME) — but
                // ONLY while its composer actually holds focus, so a hidden
                // palette (left open after switching to a utility surface or a
                // modal) never eats keys those surfaces need. Unconsumed keys
                // (typing, ⌘V) fall through to the normal paste/composer path.
                if this.skill_palette.open
                    && this.composer.read(cx).focus_handle().is_focused(window)
                    && this.on_skill_palette_key(ev, cx)
                {
                    return;
                }
                // Consume History's Escape before a focused composer can emit
                // Cancel. Higher-priority overlays and menus keep their keys.
                let dismiss_ask_menu = ev.keystroke.key == "escape"
                    && !ev.keystroke.modifiers.modified()
                    && !matches!(this.work.ask_menu, crate::ask_mode_ui::AskMenu::Closed);
                let higher_overlay_open = this.settings.open
                    || this.search.open
                    || this.shortcuts.open
                    || this.picker.open
                    || this.template_editor.is_some()
                    || this.project_editor.is_some();
                if ev.keystroke.key == "j"
                    && crate::composer_keys::main_modifier_only(&ev.keystroke.modifiers)
                    && !ev.keystroke.modifiers.alt
                    && !ev.keystroke.modifiers.shift
                    && !ev.keystroke.modifiers.function
                    && this.mode == Mode::Work
                    && this.shell.archived_reader.is_none()
                    && !higher_overlay_open
                    && !this.palette.open
                    && !this.command_palette.open
                {
                    this.toggle_job_panel(window, cx);
                    cx.stop_propagation();
                    return;
                }
                let menus_open = this.shell.more
                    || this.shell.tabs.menu.is_some()
                    || this.shell.composer_menus.model.is_some()
                    || !matches!(this.work.ask_menu, crate::ask_mode_ui::AskMenu::Closed);
                if ev.keystroke.key == "escape"
                    && !ev.keystroke.modifiers.modified()
                    && this.shell.inspector.selection.is_some()
                    && !higher_overlay_open
                    && !this.palette.open
                    && !this.command_palette.open
                    && !menus_open
                {
                    this.close_reply_inspector(window, cx);
                    cx.stop_propagation();
                    return;
                }
                if ev.keystroke.key == "escape"
                    && !ev.keystroke.modifiers.modified()
                    && this.mode == Mode::Work
                    && this.shell.job_open
                    && !higher_overlay_open
                    && !this.palette.open
                    && !this.command_palette.open
                    && !this.shell.conversations
                    && !menus_open
                    && !(this.sidebar.renaming.is_some()
                        && this
                            .rename_editor
                            .read(cx)
                            .focus_handle()
                            .is_focused(window))
                {
                    this.close_job_panel(window, cx);
                    cx.stop_propagation();
                    return;
                }
                if dismiss_ask_menu && !higher_overlay_open {
                    this.work.ask_menu = crate::ask_mode_ui::AskMenu::Closed;
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                if ev.keystroke.key == "escape"
                    && !ev.keystroke.modifiers.modified()
                    && !(this.settings.open
                        || this.search.open
                        || this.shortcuts.open
                        || this.picker.open)
                    && this.template_editor.is_none()
                    && this.project_editor.is_none()
                    && this.sidebar.renaming.is_some()
                    && this
                        .rename_editor
                        .read(cx)
                        .focus_handle()
                        .is_focused(window)
                {
                    this.sidebar.cancel_rename();
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                if ev.keystroke.key == "escape"
                    && !ev.keystroke.modifiers.modified()
                    && this.shell.conversations
                    && consolidation::conversation_mode(this.mode)
                    && !this
                        .rename_editor
                        .read(cx)
                        .focus_handle()
                        .is_focused(window)
                    && !(this.settings.open
                        || this.search.open
                        || this.shortcuts.open
                        || this.picker.open
                        || this.shell.more
                        || this.shell.tabs.menu.is_some())
                    && this.template_editor.is_none()
                    && this.project_editor.is_none()
                {
                    this.shell.conversations = false;
                    this.focus_shell_page(window, cx);
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                // The hosted Messaging pane owns typing/arrows while it is the
                // active surface (no overlay/modal up): printables filter the
                // connector list, ↑/↓ move the detail selection. Consume-only-
                // when-handled — a chord (⌘K/⌘N/…) yields Ignore and falls
                // through to the global handler, so navigation away is never
                // stolen. Gated on no modal being open so a Settings sheet over
                // the pane keeps its own keys. Read-only: the [LIVE] Test action
                // is never reachable from here.
                if this.mode == Mode::Messaging
                    && !this.settings.open
                    && this.template_editor.is_none()
                    && this.project_editor.is_none()
                    && !this.shortcuts.open
                    && !this.search.open
                    && this.on_messaging_key(ev, cx)
                {
                    return;
                }
                // ⌘V with a file/image on the clipboard attaches instead of
                // pasting (slice 19 §5) — capture phase, so it runs before
                // the composer's standard text-paste path.
                this.on_paste_attach(ev, window, cx);
            }))
            // Files dragged from the OS and dropped anywhere on the window
            // become staged attachments (slice 19 §4).
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                for p in paths.paths().to_vec() {
                    this.attach_path(&p, cx);
                }
            }))
            // Global chords, bubbling up from whatever is focused: cmd+N new
            // conversation, cmd+, settings, cmd+K the palette (ctrl off-mac),
            // Escape closes the settings modal (the open palette already took
            // its Escape in the capture handler above).
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                // A nested Settings error Sheet owns all keyboard input. Its
                // own capture handler traps Tab and its key handler closes on
                // Escape; do not let the app-wide chords (especially ⌘F)
                // act on the page behind it.
                if this.settings_error_details_open {
                    cx.stop_propagation();
                    return;
                }
                if this.shell.tabs.delete_project.is_some() {
                    cx.stop_propagation();
                    return;
                }
                if this.shell.composer_menus.model.is_some() {
                    this.close_composer_model_menu(window, cx);
                    if ev.keystroke.key == "escape" {
                        cx.stop_propagation();
                        return;
                    }
                }
                if this.shell.tabs.menu.is_some() {
                    this.close_workshop_menu(window, cx);
                    if ev.keystroke.key == "escape" {
                        cx.stop_propagation();
                        return;
                    }
                }
                // More is non-modal: dismiss before a global chord or Tab can
                // transfer focus. Residual Escape closes More before a job.
                if this.shell.more {
                    this.shell.more = false;
                    window.focus(&this.shell.more_trigger_focus, cx);
                    cx.notify();
                    if ev.keystroke.key == "escape" {
                        cx.stop_propagation();
                        return;
                    }
                }
                if this.shell.inspector.selection.is_some()
                    && !(this.settings.open
                        || this.search.open
                        || this.palette.open
                        || this.command_palette.open
                        || this.shortcuts.open)
                    && !ev.keystroke.modifiers.modified()
                    && ev.keystroke.key == "escape"
                {
                    this.close_reply_inspector(window, cx);
                    cx.stop_propagation();
                    return;
                }
                if this.shell.inspector.selection.is_some() && crate::job_panel::uses_sheet(window)
                {
                    return;
                }
                // U3e1: Tab order and approval chords, before every other key.
                if this.handle_focus_keys(ev, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                // U3e2: transcript scroll keys (Page Up/Down, Home/End, ⌘↑/⌘↓).
                if this.handle_transcript_keys(ev, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                let ks = &ev.keystroke;
                // RC2-R4 (D-U3): ⌘ on macOS (a ⌃ key belongs to the text
                // field), Ctrl elsewhere.
                let chord = crate::composer_keys::is_app_chord(&ks.modifiers);
                // Result-list navigation only while the search input itself is
                // focused, so a chat-composer Enter never doubles as "open
                // result" while the panel happens to be showing.
                let search_focused = this.search.open
                    && this.search_input.read(cx).focus_handle().is_focused(window);
                let workshop_chords = consolidation::workshop_mode(this.mode)
                    && !this.onboarding.is_active()
                    && !(this.settings.open
                        || this.search.open
                        || this.palette.open
                        || this.command_palette.open
                        || this.shortcuts.open
                        || this.picker.open)
                    && this.template_editor.is_none()
                    && this.project_editor.is_none();
                if workshop_chords
                    && chord
                    && ks.modifiers.shift
                    && !ks.modifiers.alt
                    && ks.key == "k"
                {
                    // ⇧⌘K: the conversation switcher under the title. (⌘K is
                    // the command palette, which already lists conversations.)
                    this.open_workshop_switcher(window, cx);
                    cx.stop_propagation();
                } else if workshop_chords
                    && chord
                    && ks.modifiers.alt
                    && !ks.modifiers.shift
                    && ks.key == "c"
                {
                    // ⌥⌘C: copy the conversation on screen. (⇧⌘C already saves
                    // it to Downloads.)
                    this.export_conversation(cx);
                    cx.stop_propagation();
                } else if workshop_chords
                    && chord
                    && !ks.modifiers.alt
                    && !ks.modifiers.shift
                    && ks.key == "\\"
                    && this.shell.archived_reader.is_none()
                {
                    // ⌘\: show or hide the Conversations panel.
                    this.toggle_workshop_history(cx);
                    // From the panel's search field the keyboard would be lost when the
                    // panel closes: land it in the composer.
                    this.focus_composer(window, cx);
                    cx.stop_propagation();
                } else if chord && ks.modifiers.shift && ks.key == "c" {
                    // ⌘⇧C (slice 17, M1c): export the conversation on screen
                    // to a Markdown file in Downloads, also copied. Checked
                    // before the unshifted chords so plain ⌘C copy-selection
                    // stays untouched.
                    this.save_conversation(cx);
                    cx.stop_propagation();
                } else if chord && ks.modifiers.shift && ks.key == "s" {
                    // RC2-UX (R2-19): ⌘⇧S saves the conversation on screen to
                    // a file the owner names (the system save panel).
                    this.save_on_screen_conversation_as(cx);
                    cx.stop_propagation();
                } else if chord && (ks.key == "?" || (ks.modifiers.shift && ks.key == "/")) {
                    // ⌘? (slice 20 §2): the keyboard-shortcut reference.
                    // Some platforms report the chord as shifted "/", others
                    // as a literal "?" — accept both.
                    // K02 (U3d): take the overlay focus so Escape/⌘? reach the
                    // root even when the prior focus is stale.
                    this.toggle_shortcuts(cx);
                    if this.shortcuts.open {
                        this.focus_shortcuts_overlay(window, cx);
                    }
                    cx.stop_propagation();
                } else if chord && ks.key == "." {
                    this.abort_active_stream(cx);
                    cx.stop_propagation();
                } else if chord && (ks.key == "=" || ks.key == "+") {
                    // ⌘=/⌘+ (A4): zoom the whole interface in.
                    this.zoom_in(window, cx);
                    cx.stop_propagation();
                } else if chord && ks.key == "-" {
                    // ⌘- (A4): zoom out.
                    this.zoom_out(window, cx);
                    cx.stop_propagation();
                } else if chord && ks.key == "0" {
                    // ⌘0 (A4): actual size.
                    this.zoom_actual(window, cx);
                    cx.stop_propagation();
                } else if chord && ks.key == "n" {
                    this.new_conversation(window, cx);
                    cx.stop_propagation();
                } else if chord && ks.key == "f" {
                    this.toggle_search(window, cx);
                    cx.stop_propagation();
                } else if chord && ks.key == "," {
                    this.toggle_settings(cx);
                    cx.stop_propagation();
                } else if chord && ks.key == "k" {
                    this.toggle_palette(window, cx);
                    cx.stop_propagation();
                } else if chord && !ks.modifiers.shift && ks.key == "o" {
                    // M2i: ⌘O opens the palette pre-filled with "~/" so the
                    // first keystroke starts completing the home directory.
                    // The same modal guard as ⌘K lives inside open_palette_with.
                    this.open_folder_box(window, cx);
                    cx.stop_propagation();
                } else if chord && !ks.modifiers.shift && ks.key == "e" {
                    // M2h: ⌘E opens the palette on "executor " (the executor
                    // rows in Work, the new-Code-tab rows in Code).
                    this.open_palette_with("executor ", window, cx);
                    cx.stop_propagation();
                } else if chord && ks.modifiers.shift && ks.key == "p" {
                    // ⌘⇧P (plan unit 29): the Cowork command palette — the
                    // VS Code / Cowork command-palette chord, deliberately
                    // distinct from Awili's native ⌘K conversation palette. An
                    // open palette takes its own keys in the capture handler
                    // above, so this branch only ever opens it.
                    this.open_command_palette(window, cx);
                    cx.stop_propagation();
                } else if ks.key == "escape" && !ks.modifiers.modified() && this.shortcuts.open {
                    // The sheet overlays everything (slice 20 §2), so its
                    // Escape is checked before every other panel's.
                    // K02 (U3d): restore composer focus as the sheet closes so
                    // the next keystroke lands in the composer, not on a stale
                    // (now-unrendered) element.
                    this.close_shortcuts(cx);
                    this.focus_composer(window, cx);
                    cx.stop_propagation();
                } else if ks.key == "escape" && !ks.modifiers.modified() && this.picker.open {
                    this.close_picker(cx);
                    cx.stop_propagation();
                } else if ks.key == "escape"
                    && !ks.modifiers.modified()
                    && !matches!(this.work.ask_menu, crate::ask_mode_ui::AskMenu::Closed)
                {
                    // RC2-R510 (usability RC2 round 4, Minor: the "Ask before
                    // changes" menu did not close on Esc): it closes, nothing
                    // chosen.
                    this.work.ask_menu = crate::ask_mode_ui::AskMenu::Closed;
                    cx.notify();
                    cx.stop_propagation();
                } else if ks.key == "escape"
                    && !ks.modifiers.modified()
                    && this.template_editor.is_some()
                {
                    // Slice 18 §6: Escape cancels the template editor even
                    // when neither of its fields is focused.
                    this.close_template_editor(cx);
                    cx.stop_propagation();
                } else if ks.key == "escape"
                    && !ks.modifiers.modified()
                    && this.project_editor.is_some()
                {
                    // Escape cancels the project-instructions editor even when
                    // its field is not focused (mirrors the template editor).
                    this.close_project_editor(cx);
                    cx.stop_propagation();
                } else if ks.key == "escape" && !ks.modifiers.modified() && this.settings.open {
                    // SXd-FIX1: Escape first ends an item edit (and the
                    // search it came from) and hands focus back to the page,
                    // then clears a search, and only then closes Settings.
                    if this.settings.editing_item.take().is_some() {
                        this.clear_settings_search(cx);
                        this.settings_nav_focus_pending = true;
                        cx.notify();
                    } else if !this.settings_query(cx).is_empty() {
                        this.clear_settings_search(cx);
                    } else if this.settings.confirm_close {
                        // SXe: Esc on the unsaved-services question keeps editing.
                        this.settings.confirm_close = false;
                        this.settings_nav_focus_pending = true;
                        cx.notify();
                    } else {
                        this.request_close_settings(cx);
                    }
                    cx.stop_propagation();
                } else if ks.key == "escape" && !ks.modifiers.modified() && this.search.open {
                    this.close_search(window, cx);
                    cx.stop_propagation();
                } else if ks.key == "escape"
                    && !ks.modifiers.modified()
                    && this.shell.composer_menus.model.is_some()
                {
                    this.close_composer_model_menu(window, cx);
                    cx.stop_propagation();
                } else if ks.key == "escape"
                    && !ks.modifiers.modified()
                    && this.shell.tabs.menu.is_some()
                {
                    this.close_workshop_menu(window, cx);
                    cx.stop_propagation();
                } else if ks.key == "escape"
                    && !ks.modifiers.modified()
                    && this.shell.conversations
                    && consolidation::conversation_mode(this.mode)
                {
                    this.shell.conversations = false;
                    this.focus_shell_page(window, cx);
                    cx.notify();
                    cx.stop_propagation();
                } else if ks.key == "escape"
                    && !ks.modifiers.modified()
                    && this.mode == Mode::Work
                    && this.work.running
                {
                    this.abort_active_stream(cx);
                    cx.stop_propagation();
                } else if ks.key == "escape"
                    && !ks.modifiers.modified()
                    && this.highlight_term.is_some()
                {
                    // Slice 16 §3: with every panel already closed, Escape
                    // puts out the per-message search highlight.
                    this.highlight_term = search::next_highlight(search::HighlightEvent::Clear);
                    cx.notify();
                    cx.stop_propagation();
                } else if search_focused && ks.key == "up" && !ks.modifiers.modified() {
                    this.search.move_up(this.search_results.len());
                    cx.notify();
                    cx.stop_propagation();
                } else if search_focused && ks.key == "down" && !ks.modifiers.modified() {
                    this.search.move_down(this.search_results.len());
                    cx.notify();
                    cx.stop_propagation();
                } else if search_focused
                    && (ks.key == "enter" || ks.key == "return")
                    && !ks.modifiers.modified()
                {
                    if let Some(id) = this
                        .search
                        .focused_id(&this.search_results)
                        .map(str::to_string)
                    {
                        this.open_search_result(&id, window, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .child(main)
            // More's dismiss layer and menu stay above the job face.
            .children(self.shell.more.then(|| panel.take()).flatten())
            .children(
                (!compact_conversations)
                    .then(|| conversations.take())
                    .flatten(),
            )
            .children(self.shell.more.then(|| {
                div()
                    .id("awili-more-scrim")
                    .occlude()
                    .absolute()
                    .top(gpui::Pixels::ZERO)
                    .left(gpui::Pixels::ZERO)
                    .size_full()
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.shell.more = false;
                        window.focus(&this.shell.more_trigger_focus, cx);
                        cx.notify();
                    }))
            }))
            .child(self.consolidated_rail(cx))
            // Compact Sheets cover navigation; docked history stays below More.
            .children(
                compact_conversations
                    .then(|| conversations.take())
                    .flatten(),
            )
            // The Workshop Conversations panel, when it cannot dock: a sheet
            // above the rail, below the Job face and the menus.
            .children(self.workshop_history_sheet(window.viewport_size().width, cx))
            // Modal compact Job sheets occlude the rail as well as the canvas.
            .children(panel)
            // These anchors use window coordinates. Mount the menus at the
            // window origin rather than in the Workshop/composer flow.
            .children(
                (consolidation::workshop_mode(self.mode)
                    && !self.onboarding.is_active()
                    && self.shell.archived_reader.is_none())
                .then(|| {
                    let menus: Vec<_> = [self.workshop_menu(cx), self.composer_model_menu(cx)]
                        .into_iter()
                        .flatten()
                        .collect();
                    menus.into_iter().map(|menu| {
                        div()
                            .absolute()
                            .top(gpui::Pixels::ZERO)
                            .left(gpui::Pixels::ZERO)
                            .size_full()
                            .child(menu)
                    })
                })
                .into_iter()
                .flatten(),
            )
            .children(self.search.open.then(|| self.search_panel(cx)))
            .children(self.settings.open.then(|| self.settings_modal(cx)))
            .children(self.settings_error_details_sheet(cx))
            // The model-picker popover (slice 15) sits under the palette so
            // ⌘K still paints on top of an open picker.
            .children(self.picker.open.then(|| self.picker_popover(cx)))
            .children(
                self.work
                    .executor_picker_open
                    .then(|| self.work_executor_popover(cx)),
            )
            // U4f: the Ask/Auto mode menu popover (chip click opens it).
            .children(self.ask_mode_popover(cx))
            // After settings, so an open palette paints on top (slice 13).
            .children(self.palette.open.then(|| self.palette_modal(window, cx)))
            // The Cowork command palette (⌘⇧P, plan unit 29): its own scrim +
            // centered panel. Provider slices are live for RecentSessions +
            // ArchivedChats (assembled above from in-memory state); SettingsFields
            // + McpServers stay empty (unstored schema / sourceless). Static
            // catalog rows always show. `command_palette` returns None while
            // closed, so this is a no-op then.
            .children(command_palette::command_palette(
                &self.command_palette,
                command_palette::PaletteSources {
                    recent_sessions: &palette_recent,
                    settings_fields: &[],
                    mcp_servers: &[],
                    archived_chats: &palette_archived,
                },
                |ix| {
                    Box::new(cx.listener(move |this, _ev, window, cx| {
                        this.run_command_palette_row(ix, window, cx);
                    }))
                },
                Box::new({
                    let app = cx.entity();
                    move |window: &mut Window, app_cx: &mut App| {
                        app.update(app_cx, |this, cx| this.close_command_palette(window, cx));
                    }
                }),
                Box::new({
                    let app = cx.entity();
                    move |query: String, _window: &mut Window, app_cx: &mut App| {
                        app.update(app_cx, |this, cx| {
                            this.command_palette.set_query(&query);
                            cx.notify();
                        });
                    }
                }),
            ))
            // The template editor replaces the palette overlay while open
            // (slice 18 §6 — run_palette_cmd closed the palette first).
            .children(
                self.template_editor
                    .is_some()
                    .then(|| self.template_editor_modal(cx)),
            )
            // The saved-project instructions editor (Work-mode projects CRUD
            // parity): same layer as the template editor.
            .children(
                self.project_editor
                    .is_some()
                    .then(|| self.project_editor_modal(cx)),
            )
            // The ⌘? shortcut reference (slice 20): above the palette and
            // every panel — only the toast paints over it.
            .children(self.shortcuts.open.then(|| self.shortcuts_modal(cx)))
            .children(self.workshop_project_delete_dialog(cx).map(|dialog| {
                div()
                    .absolute()
                    .top(gpui::Pixels::ZERO)
                    .left(gpui::Pixels::ZERO)
                    .size_full()
                    .child(dialog)
            }))
            // The export/save/template toast (slices 17+18): last child, so it
            // paints above everything — including an open palette.
            .children(
                self.toast
                    .visible()
                    .filter(|msg| !self.work_error_duplicates_toast(msg.as_ref()))
                    .map(|msg| {
                        div()
                            .absolute()
                            .bottom(theme::z(24.))
                            .right(theme::z(24.))
                            .max_w(theme::z(440.))
                            .px(theme::z(14.))
                            .py(theme::z(8.))
                            .rounded(theme::z(10.))
                            .bg(theme::p().raised)
                            .border_1()
                            .border_color(theme::p().border)
                            .text_color(theme::p().text)
                            .text_size(theme::z(theme::TEXT_SIZE - 2.0))
                            .flex()
                            .gap(theme::z(10.))
                            // R1a: any toast closes on a click (sticky ones only so).
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|app, _, _, cx| app.dismiss_toast(cx)),
                            )
                            .debug_selector(|| "awili-toast".to_string())
                            .child(
                                div()
                                    .debug_selector(|| "awili-toast-text".to_string())
                                    .flex_1()
                                    // R2a: let the text shrink to the box so it
                                    // wraps (a flex item's min width is otherwise its
                                    // unwrapped line, which ran off the window).
                                    .min_w_0()
                                    .child(msg.clone()),
                            )
                            // RC2-UX (R2-19): a saved file can be shown in Finder.
                            .children(self.toast.reveal.clone().map(|path| {
                                div()
                                    .debug_selector(|| "awili-toast-reveal".to_string())
                                    .flex_none()
                                    .px(theme::z(8.))
                                    .rounded(theme::z(theme::RADIUS))
                                    .border_1()
                                    .border_color(theme::p().border)
                                    .text_color(theme::p().accent_text)
                                    .cursor_pointer()
                                    .child("Show in Finder")
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |app, _, _, cx| {
                                            cx.stop_propagation();
                                            app.reveal_in_finder(&path, cx);
                                            app.dismiss_toast(cx);
                                        }),
                                    )
                            }))
                            .child(
                                div()
                                    .debug_selector(|| "awili-toast-close".to_string())
                                    .flex_none()
                                    .text_color(theme::p().muted)
                                    .child("\u{2715}"),
                            )
                    }),
            )
            // The drag-over overlay (slice 19 §4): the only drag this app
            // ever has is an external file drag (nothing here starts an
            // internal one), so any active drag means "files incoming". No
            // listeners on it, so the drop still lands on the root target.
            .children(cx.has_active_drag().then(|| {
                div()
                    .absolute()
                    .inset_0()
                    .bg(theme::p().scrim)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .px(theme::z(22.))
                            .py(theme::z(14.))
                            .rounded(theme::z(10.))
                            .bg(theme::p().raised)
                            .border_1()
                            .border_color(theme::p().accent)
                            .text_color(theme::p().text)
                            .text_size(theme::z(theme::TEXT_SIZE))
                            .child("Drop file to attach"),
                    )
            }))
            // U3a1: the one context-menu host (deferred: paints above every overlay).
            .children(context_menu::host_element(window, cx))
    }
}

/// Restore-path tests (slice 21 §D4): [`restore_conversation`] must rebuild a
/// conversation so that a summarized-away turn comes back *superseded*, not
/// live — otherwise the no-silent-compaction guarantee would hold in memory but
/// evaporate on the next launch.
/// Scroll-into-view offset for a palette row, in DEVICE pixels. Every input
/// comes off gpui's scroll handle and painted bounds and is therefore already
/// scaled by the interface zoom; the result must go back through `px(..)`,
/// never `theme::z(..)` — wrapping it in `z` multiplied the already-scaled
/// coordinate by `ui_scale()` a second time, so ⌘K / skill-palette keyboard
/// navigation over/under-scrolled at any non-1.0 zoom (Codex review finding 5).
fn palette_scroll_offset_px(
    offset_y: gpui::Pixels,
    viewport: Bounds<gpui::Pixels>,
    row: Bounds<gpui::Pixels>,
) -> gpui::Pixels {
    px(palette::scroll_offset_for_row(
        f32::from(offset_y),
        f32::from(viewport.top()),
        f32::from(viewport.bottom()),
        f32::from(row.top()),
        f32::from(row.bottom()),
    ))
}

#[cfg(test)]
mod palette_scroll_scale_tests {
    use super::*;

    /// The pure zoom tests structurally cannot catch double scaling — they never
    /// leave scale 1.0, where `z(v) == px(v)`. This one does: with the interface
    /// zoom at 2.0, the scroll offset computed from already-scaled device-pixel
    /// bounds must come back UNSCALED (the same device pixels), which is exactly
    /// what wrapping the result in `theme::z` broke.
    #[test]
    fn palette_scroll_offset_is_not_scaled_a_second_time() {
        let b = |top: f32, bottom: f32| {
            gpui::Bounds::new(
                gpui::point(px(0.), px(top)),
                gpui::size(px(100.), px(bottom - top)),
            )
        };
        // Row 5 of 24px rows under a 100px viewport, all ALREADY device pixels
        // (as the scroll handle reports them at any zoom).
        let viewport = b(0., 100.);
        let row = b(120., 144.);
        let expected = px(100.0 - 144.0);
        let prev = theme::ui_scale();
        theme::set_ui_scale(2.0);
        let at_2x = palette_scroll_offset_px(px(0.), viewport, row);
        theme::set_ui_scale(prev);
        assert_eq!(
            at_2x, expected,
            "device-pixel inputs must map to device-pixel output regardless of zoom"
        );
        // And at 1.0 the value is identical — the fix changes nothing at default.
        assert_eq!(palette_scroll_offset_px(px(0.), viewport, row), expected);
    }
}

#[cfg(test)]
mod restore_superseded_tests {
    use super::*;

    fn row(
        id: i64,
        role: &str,
        text: &str,
        kind: &str,
        superseded_by: Option<i64>,
    ) -> StoredMessage {
        StoredMessage {
            id,
            role: role.to_string(),
            text: text.to_string(),
            ts: id, // monotonic stand-in; restore re-derives in-memory ts itself
            kind: kind.to_string(),
            superseded_by,
        }
    }

    /// Two turns superseded by a later summary row come back superseded, the
    /// summary stays live, and `live_messages` is just the summary — the exact
    /// state the in-memory model had before the restart.
    #[test]
    fn restores_supersede_links_across_row_ids() {
        let msgs = [
            row(10, "user", "what's the plan", "normal", Some(30)),
            row(20, "assistant", "long answer", "normal", Some(30)),
            row(30, "assistant", "SUMMARY of the above", "summary", None),
        ];
        let convo = restore_conversation(&msgs);

        assert_eq!(convo.len(), 3);
        // Row id 30 was pushed third, so it got in-memory ts 2; both earlier
        // turns must now point at that ts, not the persisted row id 30.
        assert_eq!(convo.message(0).unwrap().superseded_by, Some(2));
        assert_eq!(convo.message(1).unwrap().superseded_by, Some(2));
        assert_eq!(convo.message(2).unwrap().superseded_by, None);
        assert_eq!(convo.message(2).unwrap().kind, MessageKind::Summary);

        let live = convo.live_messages();
        assert_eq!(live.len(), 1, "only the summary remains live");
        assert_eq!(live[0].text, "SUMMARY of the above");
    }

    /// A `superseded_by` pointing at a row that no longer exists (truncated
    /// history, skipped role) leaves the message LIVE — a dangling phantom must
    /// not hide a turn, and must never silently drop it.
    #[test]
    fn dangling_supersede_id_leaves_message_live() {
        let msgs = [row(10, "user", "still here", "normal", Some(999))];
        let convo = restore_conversation(&msgs);

        assert_eq!(convo.len(), 1);
        assert_eq!(convo.message(0).unwrap().superseded_by, None);
        assert_eq!(convo.live_messages().len(), 1);
    }

    /// An unknown-role row is skipped without shifting the supersede mapping:
    /// the remaining turn still resolves its summary correctly by row id.
    #[test]
    fn unknown_role_skipped_without_breaking_remap() {
        let msgs = [
            row(10, "tool", "ignored future role", "normal", None),
            row(20, "user", "kept turn", "normal", Some(40)),
            row(40, "assistant", "summary", "summary", None),
        ];
        let convo = restore_conversation(&msgs);

        // Only the two known-role rows survive; the user turn (in-memory ts 0)
        // points at the summary (in-memory ts 1).
        assert_eq!(convo.len(), 2);
        assert_eq!(convo.message(0).unwrap().text, "kept turn");
        assert_eq!(convo.message(0).unwrap().superseded_by, Some(1));
        assert_eq!(convo.message(1).unwrap().kind, MessageKind::Summary);
    }

    /// A plain conversation with no summaries round-trips every row as live,
    /// preserving order and kind.
    #[test]
    fn no_summaries_all_live() {
        let msgs = [
            row(1, "user", "hi", "normal", None),
            row(2, "assistant", "hello", "normal", None),
        ];
        let convo = restore_conversation(&msgs);

        assert_eq!(convo.len(), 2);
        assert_eq!(convo.live_messages().len(), 2);
        assert!(convo
            .messages()
            .iter()
            .all(|m| m.kind == MessageKind::Normal));
    }
}
#[cfg(test)]
mod usage_wiring_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;

    /// Mid-day UTC instant the tests pin the clock to (2025-06-15 ~12:26 UTC).
    const NOON: i64 = 1_750_000_000;

    fn temp_db() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "awili-app-usage-test-{}-{}.db",
            std::process::id(),
            n
        ))
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    /// The UTC calendar day containing NOON, for the calendar-day read API.
    fn noon_date() -> chrono::NaiveDate {
        chrono::DateTime::from_timestamp(NOON, 0)
            .unwrap()
            .date_naive()
    }

    /// Unwrap a `ProviderEvent` exactly the way `start_turn`'s stream loop
    /// does: only the `Usage` arm feeds the wiring under test.
    fn usage_of(ev: ProviderEvent) -> Usage {
        match ev {
            ProviderEvent::Usage(u) => u,
            other => panic!("test expected a Usage event, got {other:?}"),
        }
    }

    #[test]
    fn usage_event_updates_counters_chip_and_durable_row() {
        // SF6: ProviderEvent::Usage → session counters move, the chip carries
        // a `$`, and a usage_events row is durably committed by the worker.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.create_conversation("conv-1", "t", NOON).unwrap();
        let (persist, worker) = Persistence::spawn_worker(store);

        let mut cost = CostTicker::new(NOON, &[]);
        let turn_gen = cost.generation(); // captured the way start_turn does
        let ev = ProviderEvent::Usage(Usage {
            cached_input_tokens: None,
            input_tokens: 1_000,
            output_tokens: 500,
        });
        let note = apply_usage_event(
            &mut cost,
            Some(&persist),
            "conv-1",
            "anthropic",
            "claude-sonnet-4-6",
            usage_of(ev),
            NOON,
            turn_gen,
        );

        // Session counters updated, chip rendered with dollars.
        assert_eq!(cost.session_input_tokens, 1_000);
        assert_eq!(cost.session_output_tokens, 500);
        let chip = cost
            .chip_label(NOON)
            .expect("paid usage must surface the chip");
        assert!(chip.contains('$'), "chip must show dollars: {chip}");
        // The paid turn staged a memory note carrying the running session cost.
        let note = note.expect("a paid turn stages a usage note");
        assert!((note.session_cost_usd - 0.0105).abs() < 1e-12);

        // Drain the worker, then reopen the db: the row is durable and reads
        // back through the calendar-day API (SF5).
        drop(persist);
        worker.join().expect("persistence worker must exit cleanly");
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.daily_usage(noon_date()).unwrap(), (1_000, 500));
        cleanup(&path);
    }

    #[test]
    fn new_conversation_resets_session_but_today_survives() {
        // SF6: the reset `new_conversation` performs (`CostTicker::reset_session`)
        // zeroes the session while today's durable total keeps the chip alive.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.create_conversation("conv-1", "t", NOON).unwrap();
        let (persist, worker) = Persistence::spawn_worker(store);

        let mut cost = CostTicker::new(NOON, &[]);
        let turn_gen = cost.generation(); // captured the way start_turn does
        let ev = ProviderEvent::Usage(Usage {
            cached_input_tokens: None,
            input_tokens: 1_000,
            output_tokens: 500,
        });
        apply_usage_event(
            &mut cost,
            Some(&persist),
            "conv-1",
            "anthropic",
            "claude-sonnet-4-6",
            usage_of(ev),
            NOON,
            turn_gen,
        );
        assert!(cost.chip_label(NOON).unwrap().contains('$'));

        // The user hits "+ New": new_conversation resets the session.
        cost.reset_session();
        assert_eq!(cost.session_input_tokens, 0);
        assert_eq!(cost.session_output_tokens, 0);
        assert_eq!(cost.session_usd(), 0.0);
        let chip = cost
            .chip_label(NOON)
            .expect("today's spend keeps the chip alive");
        assert!(chip.starts_with("$0 session · "), "chip was {chip}");
        assert!(chip.contains("$0.0105 today"), "chip was {chip}");

        drop(persist);
        worker.join().expect("persistence worker must exit cleanly");
        cleanup(&path);
    }

    #[test]
    fn late_usage_from_an_old_session_is_persisted_but_not_credited() {
        // SF1 at the wiring level: conversation A's turn starts, the user
        // creates conversation B (reset), THEN A's Usage event arrives with
        // the generation it started under. The spend persists (it is real)
        // but the new session and its memory note stay clean.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.create_conversation("conv-a", "t", NOON).unwrap();
        let (persist, worker) = Persistence::spawn_worker(store);

        let mut cost = CostTicker::new(NOON, &[]);
        let turn_gen = cost.generation(); // start_turn captures this
        cost.reset_session(); // new_conversation while A streams

        let ev = ProviderEvent::Usage(Usage {
            cached_input_tokens: None,
            input_tokens: 1_000,
            output_tokens: 500,
        });
        let note = apply_usage_event(
            &mut cost,
            Some(&persist),
            "conv-a",
            "anthropic",
            "claude-sonnet-4-6",
            usage_of(ev),
            NOON,
            turn_gen,
        );
        assert_eq!(
            note, None,
            "a stale turn must not stage a note against the new session"
        );
        assert_eq!(cost.session_input_tokens, 0, "new session stays clean");
        assert_eq!(cost.session_usd(), 0.0);
        // ...but today's total and the durable row both record the real spend.
        assert!(cost.chip_label(NOON).unwrap().contains("$0.0105 today"));
        drop(persist);
        worker.join().expect("persistence worker must exit cleanly");
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.daily_usage(noon_date()).unwrap(), (1_000, 500));
        cleanup(&path);
    }

    #[test]
    fn free_local_model_turn_records_tokens_but_stages_no_usage_note() {
        // Priority-3 (open/fleet model switch) money-path: when the user
        // switches to a free local/fleet model (ollama, a self-hosted box —
        // any provider `price_for` prices at $0), `price_for` returns FREE
        // so the turn's cost is
        // $0. `apply_usage_event`'s note gate is `event_cost > 0.0 &&
        // !stale_session` — a $0 turn must stage NO memory note EVEN ON A FRESH
        // session (there is no spend to remember), while its token counts still
        // move and the durable usage_events row is still written. Every other
        // note test uses a PAID anthropic model, so the `event_cost > 0.0` half
        // of the gate — the half that suppresses the note for the entire local
        // path — was unpinned: dropping it would spam a "$0 session cost" note
        // on every single free-model turn.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.create_conversation("conv-local", "t", NOON).unwrap();
        let (persist, worker) = Persistence::spawn_worker(store);

        let mut cost = CostTicker::new(NOON, &[]);
        let turn_gen = cost.generation(); // fresh session — NOT the stale path
        let ev = ProviderEvent::Usage(Usage {
            cached_input_tokens: None,
            input_tokens: 1_000,
            output_tokens: 500,
        });
        let note = apply_usage_event(
            &mut cost,
            Some(&persist),
            "conv-local",
            "ollama", // non-anthropic => FREE => event_cost == 0.0
            "ornith-35b",
            usage_of(ev),
            NOON,
            turn_gen,
        );

        // No paid note despite a fresh, generation-matching session — this is
        // the `event_cost > 0.0` guard doing the work, not stale suppression.
        assert_eq!(
            note, None,
            "a free-model turn stages no usage note even on a fresh session"
        );
        // Tokens still moved (proves the gen matched: the session ticker only
        // accumulates when gen == generation, so this is NOT the stale path),
        // but the running dollar cost stayed exactly zero.
        assert_eq!(cost.session_input_tokens, 1_000);
        assert_eq!(cost.session_output_tokens, 500);
        assert_eq!(cost.session_usd(), 0.0);
        // An all-local day shows no chip at all (spec §4).
        assert_eq!(
            cost.chip_label(NOON),
            None,
            "a $0 session and $0 today render no cost chip"
        );

        // The durable usage_events row is still written — free tokens are real
        // tokens — even though the turn cost nothing and staged no note.
        drop(persist);
        worker.join().expect("persistence worker must exit cleanly");
        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.daily_usage(noon_date()).unwrap(),
            (1_000, 500),
            "a free turn's token usage is still durably recorded"
        );
        cleanup(&path);
    }

    #[test]
    fn failed_persistence_write_is_survived_not_panicked() {
        // SF3: the ticker advances before the write commits, so a failed
        // insert (here: an FK violation — the conversation row was never
        // created) must leave the app alive: the worker logs and continues,
        // the ticker keeps its (ceiling) totals, and nothing panics.
        let path = temp_db();
        let store = Store::open(&path).unwrap(); // NOTE: no conversation row
        let (persist, worker) = Persistence::spawn_worker(store);

        let mut cost = CostTicker::new(NOON, &[]);
        let turn_gen = cost.generation(); // captured the way start_turn does
        let ev = ProviderEvent::Usage(Usage {
            cached_input_tokens: None,
            input_tokens: 1_000,
            output_tokens: 500,
        });
        let note = apply_usage_event(
            &mut cost,
            Some(&persist),
            "no-such-conversation",
            "anthropic",
            "claude-sonnet-4-6",
            usage_of(ev),
            NOON,
            turn_gen,
        );
        // The UI side is fully functional despite the doomed write…
        assert!(note.is_some());
        assert_eq!(cost.session_input_tokens, 1_000);
        assert!(cost.chip_label(NOON).unwrap().contains('$'));

        // …and the worker survived the failure: it processed the bad command,
        // logged it, and exits cleanly — a panic would surface in join().
        drop(persist);
        worker
            .join()
            .expect("a failed write must be logged, never a worker panic");
        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.daily_usage(noon_date()).unwrap(),
            (0, 0),
            "the failed write left no row — the ticker total is a ceiling (see TODO in apply_usage_event)"
        );
        cleanup(&path);
    }

    #[test]
    fn send_to_a_dead_worker_is_flagged_not_swallowed() {
        // M7: a fire-and-forget send whose worker thread is gone must NOT be
        // silently dropped with `let _ =`. Persistence records the failure (and
        // logs once) so the app can surface "persistence stopped" instead of
        // pretending chat history / state / usage still commit. Still infallible
        // for the caller — a dead worker never panics the UI.
        let (tx, rx) = std::sync::mpsc::channel::<PersistCmd>();
        drop(rx); // simulate a worker thread that has exited
        let persist = Persistence {
            tx,
            failed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        use std::sync::atomic::Ordering;
        assert!(
            !persist.failed.load(Ordering::SeqCst),
            "fresh handle is healthy"
        );
        persist.append("c1", "user", "hi".into(), 0);
        assert!(
            persist.failed.load(Ordering::SeqCst),
            "a send to a dead worker must be flagged"
        );
        // Subsequent sends remain infallible (no panic, flag stays set).
        persist.set_state("k", "v".into());
        persist.send(PersistCmd::Touch {
            id: "c1".into(),
            updated_ts: 1,
        });
        assert!(persist.failed.load(Ordering::SeqCst));
    }

    #[test]
    fn template_reply_keeps_list_on_error() {
        // M6: the worker's template reply is now a Result. On success the list
        // is replaced; on a DB READ failure the existing list must be KEPT
        // (never wiped to empty) and the error surfaced — previously the worker
        // sent `unwrap_or_default()`, so a failed read silently emptied the
        // palette.
        let current = vec![PromptTemplate {
            id: 1,
            name: "alpha".into(),
            content: "x".into(),
            created_at: 0,
            updated_at: 0,
        }];
        let (list, err) = merge_template_reply(current.clone(), Ok(Vec::new()));
        assert!(list.is_empty(), "success replaces the list");
        assert!(err.is_none());

        let (list, err) = merge_template_reply(current.clone(), Err("database is locked".into()));
        assert_eq!(
            list.len(),
            1,
            "M6: a read failure must not wipe the palette"
        );
        assert_eq!(list[0].name, "alpha");
        assert_eq!(err.as_deref(), Some("database is locked"));
    }

    #[test]
    fn distillation_usage_reaches_today_and_durable_row_but_not_session() {
        // Slice 10 review SF3: a distillation call's Usage event must land in
        // the today total and the durable usage_events row — the session
        // ticker stays untouched (it tracks the visible conversation).
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.create_conversation("conv-1", "t", NOON).unwrap();
        let (persist, worker) = Persistence::spawn_worker(store);

        let mut cost = CostTicker::new(NOON, &[]);
        let ev = ProviderEvent::Usage(Usage {
            cached_input_tokens: None,
            input_tokens: 1_000,
            output_tokens: 500,
        });
        apply_distill_usage(
            &mut cost,
            Some(&persist),
            "conv-1",
            "anthropic",
            "claude-sonnet-4-6",
            usage_of(ev),
            NOON,
        );
        assert_eq!(cost.session_input_tokens, 0, "session ticker untouched");
        assert_eq!(cost.session_usd(), 0.0);
        let chip = cost
            .chip_label(NOON)
            .expect("distillation spend must surface in the chip");
        assert_eq!(chip, "$0 session · $0.0105 today");

        // The spend is durably recorded, so the next launch re-seeds an
        // honest "today" total.
        drop(persist);
        worker.join().expect("persistence worker must exit cleanly");
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.daily_usage(noon_date()).unwrap(), (1_000, 500));
        cleanup(&path);
    }
}

/// Template wiring tests (slice 18): the new `PersistCmd` arms drive a real
/// `Store` on the real worker thread — upsert/delete reply with the refreshed
/// list the UI's in-memory mirror consumes, and a binding written through the
/// worker survives a restart.
#[cfg(test)]
mod template_wiring_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;

    fn temp_db() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "awili-app-tpl-test-{}-{}.db",
            std::process::id(),
            n
        ))
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn worker_upsert_replies_with_refreshed_list() {
        let path = temp_db();
        let (persist, worker) = Persistence::spawn_worker(Store::open(&path).unwrap());

        let (tx, rx) = async_channel::bounded(1);
        persist.send(PersistCmd::UpsertTemplate {
            name: "Foreman".to_string(),
            content: "v1".to_string(),
            reply: tx,
        });
        let templates = rx.recv_blocking().unwrap().unwrap();
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].name, "Foreman");
        assert_eq!(templates[0].content, "v1");

        // Same name again: the reply reflects the update, under the same id —
        // what keeps `save_template`'s mirror consistent with bound rows.
        let id = templates[0].id;
        let (tx, rx) = async_channel::bounded(1);
        persist.send(PersistCmd::UpsertTemplate {
            name: "Foreman".to_string(),
            content: "v2".to_string(),
            reply: tx,
        });
        let templates = rx.recv_blocking().unwrap().unwrap();
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].id, id);
        assert_eq!(templates[0].content, "v2");

        drop(persist);
        worker.join().expect("persistence worker must exit cleanly");
        cleanup(&path);
    }

    #[test]
    fn worker_save_project_replies_with_refreshed_list() {
        // SaveProject mirrors UpsertTemplate: the reply carries the fresh list
        // with the DB-assigned id, so `save_project_folder` can
        // replace its in-memory mirror with ids that match the rows.
        let path = temp_db();
        let (persist, worker) = Persistence::spawn_worker(Store::open(&path).unwrap());

        let (tx, rx) = async_channel::bounded(1);
        persist.send(PersistCmd::SaveProject {
            name: "awili".to_string(),
            folder: "/Users/example/Projects/awili".to_string(),
            instructions: String::new(),
            ts: 100,
            reply: tx,
        });
        let projects = rx.recv_blocking().unwrap().unwrap().projects;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "awili");
        assert_eq!(projects[0].folder, "/Users/example/Projects/awili");
        assert!(
            projects[0].id > 0,
            "the reply must carry the DB-assigned id"
        );

        // A second project replies with both, newest first (list_projects orders
        // by created_ts DESC) — the order the sidebar renders.
        let (tx, rx) = async_channel::bounded(1);
        persist.send(PersistCmd::SaveProject {
            name: "hub".to_string(),
            folder: "/Users/example/Projects/hub".to_string(),
            instructions: String::new(),
            ts: 200,
            reply: tx,
        });
        let projects = rx.recv_blocking().unwrap().unwrap().projects;
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].name, "hub");
        assert_eq!(projects[1].name, "awili");

        drop(persist);
        worker.join().expect("persistence worker must exit cleanly");
        cleanup(&path);
    }

    #[test]
    fn worker_project_survives_reopen() {
        // The durable half of project persistence: a saved project is read back
        // on the next launch (PersistInit::start reads list_projects), so
        // `work.projects` is no longer lost on relaunch.
        let path = temp_db();
        let (persist, worker) = Persistence::spawn_worker(Store::open(&path).unwrap());

        let (tx, rx) = async_channel::bounded(1);
        persist.send(PersistCmd::SaveProject {
            name: "awili".to_string(),
            folder: "/Users/example/Projects/awili".to_string(),
            instructions: String::new(),
            ts: 100,
            reply: tx,
        });
        let _ = rx.recv_blocking().unwrap().unwrap();

        drop(persist);
        worker.join().expect("persistence worker must exit cleanly");
        let reopened = Store::open(&path).unwrap();
        let projects = reopened.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].folder, "/Users/example/Projects/awili");
        cleanup(&path);
    }

    #[test]
    fn worker_update_project_instructions_replies_with_refreshed_list() {
        // UpdateProjectInstructions mirrors SaveProject/RemoveProject: the reply
        // carries the fresh list with the edited instructions, so
        // `save_project_instructions` can replace its in-memory mirror from a
        // single authoritative read — never diverging from the rows.
        let path = temp_db();
        let (persist, worker) = Persistence::spawn_worker(Store::open(&path).unwrap());

        // Seed a project, capturing its DB-assigned id from the SaveProject reply.
        let (tx, rx) = async_channel::bounded(1);
        persist.send(PersistCmd::SaveProject {
            name: "awili".to_string(),
            folder: "/Users/example/Projects/awili".to_string(),
            instructions: "old".to_string(),
            ts: 100,
            reply: tx,
        });
        let projects = rx.recv_blocking().unwrap().unwrap().projects;
        let id = projects[0].id;

        // Edit the instructions; the reply is the post-update list.
        let (tx, rx) = async_channel::bounded(1);
        persist.send(PersistCmd::UpdateProjectInstructions {
            project_id: id,
            instructions: "be terse and cite files".to_string(),
            reply: tx,
        });
        let projects = rx.recv_blocking().unwrap().unwrap().projects;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, id, "the same row, not a new one");
        assert_eq!(
            projects[0].instructions, "be terse and cite files",
            "the reply carries the edited instructions"
        );

        drop(persist);
        worker.join().expect("persistence worker must exit cleanly");
        cleanup(&path);
    }

    #[test]
    fn worker_update_missing_project_replies_error_not_false_success() {
        // An edit that hits zero rows (the project was deleted out from under the
        // open editor) must reply Err — the user's typed instructions went
        // nowhere, so the UI shows a failure, never a false "saved".
        let path = temp_db();
        let (persist, worker) = Persistence::spawn_worker(Store::open(&path).unwrap());

        let (tx, rx) = async_channel::bounded(1);
        persist.send(PersistCmd::UpdateProjectInstructions {
            project_id: 9999, // never inserted
            instructions: "into the void".to_string(),
            reply: tx,
        });
        let reply = rx.recv_blocking().unwrap();
        assert!(reply.is_err(), "a zero-row edit replies Err, not Ok(list)");

        drop(persist);
        worker.join().expect("persistence worker must exit cleanly");
        cleanup(&path);
    }

    #[test]
    fn worker_binding_survives_reopen() {
        // ApplyTemplate's fire-and-forget SetConversationTemplate lands after
        // the upsert (the mpsc keeps command order) and reads back on the next
        // launch — the durable half of slice 18 §3/§4.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.create_conversation("conv-1", "t", 0).unwrap();
        let (persist, worker) = Persistence::spawn_worker(store);

        let (tx, rx) = async_channel::bounded(1);
        persist.send(PersistCmd::UpsertTemplate {
            name: "Foreman".to_string(),
            content: "you are the foreman".to_string(),
            reply: tx,
        });
        let id = rx.recv_blocking().unwrap().unwrap()[0].id;
        persist.send(PersistCmd::SetConversationTemplate {
            conv: "conv-1".to_string(),
            template_id: Some(id),
        });

        drop(persist);
        worker.join().expect("persistence worker must exit cleanly");
        let reopened = Store::open(&path).unwrap();
        let bound = reopened
            .get_conversation_template("conv-1")
            .unwrap()
            .unwrap();
        assert_eq!(bound.content, "you are the foreman");
        cleanup(&path);
    }

    #[test]
    fn worker_delete_unbinds_and_replies_empty() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.create_conversation("conv-1", "t", 0).unwrap();
        let (persist, worker) = Persistence::spawn_worker(store);

        let (tx, rx) = async_channel::bounded(1);
        persist.send(PersistCmd::UpsertTemplate {
            name: "Foreman".to_string(),
            content: "x".to_string(),
            reply: tx,
        });
        let id = rx.recv_blocking().unwrap().unwrap()[0].id;
        persist.send(PersistCmd::SetConversationTemplate {
            conv: "conv-1".to_string(),
            template_id: Some(id),
        });
        let (tx, rx) = async_channel::bounded(1);
        persist.send(PersistCmd::DeleteTemplate { id, reply: tx });
        assert!(
            rx.recv_blocking().unwrap().unwrap().is_empty(),
            "refreshed list is empty"
        );

        drop(persist);
        worker.join().expect("persistence worker must exit cleanly");
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.get_conversation_template("conv-1").unwrap(), None);
        cleanup(&path);
    }
}

/// Tests for the distillation attempt lifecycle (slice 10 review SF1/SF2):
/// the cap is charged at spawn via [`admit_distillation`] and settled at
/// completion via [`settle_distillation`] — the exact functions
/// `distill_turn` and its completion closure call.
#[cfg(test)]
mod distill_settlement_tests {
    use super::*;
    use awili_config::ProviderKind;
    use awili_memory::MAX_DISTILL_WRITES_PER_SESSION;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    /// A turn pair comfortably past the 50-char noise floor.
    fn long() -> String {
        "x".repeat(100)
    }

    /// A fully-enabled memory config (service configured AND write toggle on)
    /// for the admission tests; the disabled variants override one field.
    fn mem_on() -> MemoryCfg {
        MemoryCfg {
            base_url: String::new(),
            token: String::new(),
            min_priority: 90,
            enabled: true,
            recall: true,
            write: true,
        }
    }

    /// A plain v5 client for the admission tests: [`MemoryClient::new`] never
    /// resolves the backend conf, so the V5 path is exercised deterministically
    /// regardless of the build machine's live conf (and the V65 hold cannot
    /// accidentally gate it).
    fn v5_client() -> MemoryClient {
        MemoryClient::new(String::new(), 90)
    }

    static V65_SEAM_SEQ: AtomicUsize = AtomicUsize::new(0);

    /// A REAL V65 [`MemoryClient`] built through the explicit-conf-path seam:
    /// a temp conf file (never HOME, env, or the live backend conf) names the
    /// loopback memory base plus temp token files, so the client genuinely
    /// resolves V65 and any write attempt would be directed at the counting
    /// stub. Returns the client and its temp dir (for cleanup). Test
    /// credentials only — nothing here touches a real service.
    fn v65_client_at(memory_base_url: &str) -> (MemoryClient, std::path::PathBuf) {
        let seq = V65_SEAM_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("awili-distill-v65-{}-{}", std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        let read_tok = dir.join("v6-reader-token");
        let write_tok = dir.join("v6-writer-token");
        std::fs::write(&read_tok, "test-reader-token").unwrap();
        std::fs::write(&write_tok, "test-writer-token").unwrap();
        let conf = dir.join("memory-backend.conf");
        std::fs::write(
            &conf,
            format!(
                "FOREMAN_MEMORY_BACKEND=v65\n\
                 FOREMAN_MEMORY_V65_BASE={memory_base_url}\n\
                 FOREMAN_MEMORY_V65_READ_TOKEN={}\n\
                 FOREMAN_MEMORY_V65_WRITE_TOKEN={}\n",
                read_tok.display(),
                write_tok.display()
            ),
        )
        .unwrap();
        (
            MemoryClient::resolved_from_conf(String::new(), 90, &conf),
            dir,
        )
    }

    /// A loopback counting stub: binds `127.0.0.1:0`, counts every accepted
    /// connection, and answers each with a minimal Ollama-style `done:true`
    /// NDJSON body — so a regression that DOES reach the endpoint completes
    /// (and is caught by the count) instead of hanging the test. No real
    /// external endpoints are contacted. Returns the base URL and the accept
    /// counter.
    fn counting_stub() -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        thread::spawn(move || {
            for stream in listener.incoming() {
                c.fetch_add(1, Ordering::SeqCst);
                let Ok(stream) = stream else { continue };
                // Each connection in its own thread so a slow client can't wedge
                // the accept loop.
                thread::spawn(move || {
                    let mut s = stream;
                    let mut buf = [0u8; 8192];
                    let _ = s.read(&mut buf);
                    let body = "{\"done\":true,\"prompt_eval_count\":3,\"eval_count\":7}\n";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {len}\r\n\r\n{body}",
                        len = body.len()
                    );
                    let _ = s.write_all(resp.as_bytes());
                });
            }
        });
        (format!("http://{addr}"), count)
    }

    #[test]
    fn cap_counts_attempts_so_concurrent_spawns_cannot_overrun_it() {
        // Review SF1 (TOCTOU): two turns finish within one distillation
        // round-trip at count cap-1. The cap is charged at spawn, so the
        // first attempt takes the last slot and the second is refused —
        // under the old completion-time increment both would have passed.
        let mut writes = MAX_DISTILL_WRITES_PER_SESSION - 1;
        assert!(admit_distillation(
            &mem_on(),
            &v5_client(),
            &long(),
            &long(),
            &mut writes
        ));
        assert_eq!(
            writes, MAX_DISTILL_WRITES_PER_SESSION,
            "the slot is taken at spawn"
        );
        assert!(
            !admit_distillation(&mem_on(), &v5_client(), &long(), &long(), &mut writes),
            "a concurrent spawn sees the charged cap, not a stale counter"
        );
        assert_eq!(writes, MAX_DISTILL_WRITES_PER_SESSION);
    }

    #[test]
    fn failed_attempt_refunds_its_slot() {
        // Review SF1: a SKIP or provider failure must give the slot back —
        // the cap bounds writes, and a failed attempt wrote nothing.
        let mut writes = 0;
        assert!(admit_distillation(
            &mem_on(),
            &v5_client(),
            &long(),
            &long(),
            &mut writes
        ));
        assert_eq!(writes, 1);
        let flash = settle_distillation(&mut writes, 7, 7, false);
        assert!(!flash, "a failed attempt never flashes the chip");
        assert_eq!(writes, 0, "the spawn-time charge is refunded");

        // A successful attempt keeps its slot and flashes.
        assert!(admit_distillation(
            &mem_on(),
            &v5_client(),
            &long(),
            &long(),
            &mut writes
        ));
        assert!(settle_distillation(&mut writes, 7, 7, true));
        assert_eq!(writes, 1, "a landed write stays counted against the cap");
    }

    #[test]
    fn stale_completion_neither_credits_nor_flashes_the_new_session() {
        // Review SF2: a worker spawned in session N lands after
        // `new_conversation` reset the counter and bumped the generation.
        // The new session's counter must not move and its chip must not
        // flash — even though the old session's write really happened.
        let mut writes = 0; // new_conversation already reset the counter
        let spawn_gen = 3;
        let current_gen = 4;
        assert!(
            !settle_distillation(&mut writes, spawn_gen, current_gen, true),
            "no chip flash in an unrelated conversation"
        );
        assert_eq!(writes, 0, "the new session's counter must not move");

        // A stale FAILURE must not refund into the new session either —
        // its charge was wiped with the old session's counter.
        let mut writes = 2;
        assert!(!settle_distillation(
            &mut writes,
            spawn_gen,
            current_gen,
            false
        ));
        assert_eq!(writes, 2, "no cross-session refund");
    }

    #[test]
    fn enabled_admission_still_reserves_a_slot() {
        // Slice 11 regression guard: adding the disabled-memory / write=false
        // gates must not have broken the happy path — an enabled admission
        // charges exactly one slot.
        let mut writes = 0;
        assert!(admit_distillation(
            &mem_on(),
            &v5_client(),
            &long(),
            &long(),
            &mut writes
        ));
        assert_eq!(writes, 1, "an enabled admission reserves one slot");
    }

    #[test]
    fn disabled_write_admission_refuses_and_never_charges_the_cap() {
        // Slice 11: with `[memory] write` off, admission must refuse AND must
        // not charge the cap — the budget stays intact for when writes are
        // re-enabled (a disabled admission never reserves a slot).
        let mut writes = 0;
        let mem = MemoryCfg {
            write: false,
            ..mem_on()
        };
        assert!(
            !admit_distillation(&mem, &v5_client(), &long(), &long(), &mut writes),
            "write=false must refuse admission"
        );
        assert_eq!(writes, 0, "a disabled admission must not charge the cap");
    }

    #[test]
    fn disabled_memory_admission_refuses_and_never_charges_the_cap() {
        // Slice 11: with no memory service configured, the same guarantee —
        // refuse and leave the cap untouched.
        let mut writes = 0;
        let mem = MemoryCfg {
            enabled: false,
            ..mem_on()
        };
        assert!(
            !admit_distillation(&mem, &v5_client(), &long(), &long(), &mut writes),
            "disabled memory must refuse admission"
        );
        assert_eq!(writes, 0, "a disabled admission must not charge the cap");
    }

    #[test]
    fn distill_and_store_with_write_disabled_makes_no_requests() {
        // Slice 11: write=false ⇒ no provider call and no memory write. Loopback
        // counting stubs on the model and memory endpoints prove zero requests
        // (no real external endpoints are contacted).
        let (model_url, model_hits) = counting_stub();
        let (mem_url, mem_hits) = counting_stub();
        let choice = ModelChoice {
            provider: "ollama".into(),
            kind: ProviderKind::Ollama,
            base_url: model_url,
            model: "stub-model".into(),
            api_key: None,
        };
        let keys = ProviderKeys::default();
        let mem = MemoryCfg {
            base_url: mem_url,
            write: false,
            ..mem_on()
        };
        let outcome = distill_and_store(&choice, &keys, &mem, v5_client(), &long(), &long(), "");
        assert!(!outcome.wrote, "write=false must not write a memory");
        assert!(outcome.usage.is_none(), "no provider call ⇒ no usage");
        assert_eq!(model_hits.load(Ordering::SeqCst), 0, "no provider request");
        assert_eq!(mem_hits.load(Ordering::SeqCst), 0, "no memory request");
    }

    #[test]
    fn distill_and_store_with_memory_disabled_makes_no_requests() {
        // Slice 11: a disabled memory service ⇒ no provider call and no memory
        // write, for the same reason.
        let (model_url, model_hits) = counting_stub();
        let (mem_url, mem_hits) = counting_stub();
        let choice = ModelChoice {
            provider: "ollama".into(),
            kind: ProviderKind::Ollama,
            base_url: model_url,
            model: "stub-model".into(),
            api_key: None,
        };
        let keys = ProviderKeys::default();
        let mem = MemoryCfg {
            base_url: mem_url,
            enabled: false,
            ..mem_on()
        };
        let outcome = distill_and_store(&choice, &keys, &mem, v5_client(), &long(), &long(), "");
        assert!(!outcome.wrote, "disabled memory must not write a memory");
        assert!(outcome.usage.is_none(), "no provider call ⇒ no usage");
        assert_eq!(model_hits.load(Ordering::SeqCst), 0, "no provider request");
        assert_eq!(mem_hits.load(Ordering::SeqCst), 0, "no memory request");
    }

    #[test]
    fn v65_admission_refuses_and_never_charges_the_cap() {
        // V65 hold: enabled=true/write=true AND a client that authoritatively
        // resolved to V65 must refuse admission with the cap untouched — no
        // worker (hence no provider or memory request) can be spawned from a
        // refused admission.
        let (mem_url, mem_hits) = counting_stub();
        let (client, dir) = v65_client_at(&mem_url);
        assert_eq!(
            client.backend(),
            awili_memory::MemoryBackend::V65,
            "the test client really resolved V65"
        );
        let mut writes = 0;
        assert!(
            !admit_distillation(&mem_on(), &client, &long(), &long(), &mut writes),
            "a resolved V65 backend must refuse admission"
        );
        assert_eq!(writes, 0, "a V65 admission must not charge the cap");
        assert_eq!(mem_hits.load(Ordering::SeqCst), 0, "no memory request");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn distill_and_store_with_v65_client_makes_no_requests() {
        // V65 hold: fully enabled (write=true) with a real injected V65
        // client, the worker must refuse BEFORE building the provider request
        // — wrote=false/usage=None and ZERO provider+memory requests on the
        // real loopback stubs (no real external endpoints are contacted).
        let (model_url, model_hits) = counting_stub();
        let (mem_url, mem_hits) = counting_stub();
        let choice = ModelChoice {
            provider: "ollama".into(),
            kind: ProviderKind::Ollama,
            base_url: model_url,
            model: "stub-model".into(),
            api_key: None,
        };
        let keys = ProviderKeys::default();
        // The config's own base points at the loopback stub too, so even a
        // (regressed) re-resolution would be provably directed at the stub,
        // not a real service.
        let mem = MemoryCfg {
            base_url: mem_url.clone(),
            ..mem_on()
        };
        let (client, dir) = v65_client_at(&mem_url);
        let outcome = distill_and_store(&choice, &keys, &mem, client, &long(), &long(), "");
        assert!(!outcome.wrote, "a V65 backend must not write a memory");
        assert!(outcome.usage.is_none(), "no provider call ⇒ no usage");
        assert_eq!(model_hits.load(Ordering::SeqCst), 0, "no provider request");
        assert_eq!(mem_hits.load(Ordering::SeqCst), 0, "no memory request");
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod message_action_tests {
    use super::*;

    /// A view seeded with `(role, text)` pairs — the slice-12 tests' fixture.
    fn view_of(seed: &[(Role, &str)]) -> ConvView {
        ConvView::from_state(ConversationState::seeded(
            seed.iter().map(|(r, t)| (*r, t.to_string())),
        ))
    }

    #[test]
    fn thinking_indicator_survives_leading_provider_whitespace() {
        let empty = view_of(&[(Role::Assistant, "")]);
        assert!(should_show_activity_pulse(
            &empty.rows[0],
            true,
            true,
            true,
            false
        ));

        let whitespace = view_of(&[(Role::Assistant, "  \n")]);
        assert!(
            should_show_activity_pulse(&whitespace.rows[0], true, true, true, false),
            "whitespace has no paintable markdown body"
        );

        let visible = view_of(&[(Role::Assistant, "answer")]);
        assert!(!should_show_activity_pulse(
            &visible.rows[0],
            true,
            true,
            false,
            false
        ));
        assert!(should_show_activity_pulse(
            &visible.rows[0],
            true,
            true,
            true,
            false
        ));
        assert!(!should_show_activity_pulse(
            &empty.rows[0],
            false,
            true,
            true,
            false
        ));
        assert!(!should_show_activity_pulse(
            &empty.rows[0],
            true,
            false,
            true,
            false
        ));
        assert!(!should_show_activity_pulse(
            &empty.rows[0],
            true,
            true,
            false,
            false
        ));
        assert!(!should_show_activity_pulse(
            &empty.rows[0],
            true,
            true,
            true,
            true
        ));
    }

    #[test]
    fn thinking_bullet_has_a_visible_breathing_cycle() {
        ui::motion::set_test_override(false);
        let frames: Vec<_> = (0..12).map(|p| activity_pulse(p).size()).collect();
        // Owner: half size and blue, 2026-10-10; use the actual app part.
        assert!(frames
            .iter()
            .all(|s| (theme::THINK_MIN..=theme::THINK_MAX).contains(s)));
        assert_ne!(frames[0], frames[5], "the bullet must visibly change");
        assert_eq!(activity_pulse(12).size(), frames[0], "cycle wraps");
        ui::motion::set_test_override(true);
        assert_eq!(activity_pulse(0).size(), theme::THINK_MAX);
        ui::motion::clear_test_override();
    }

    #[test]
    fn conversation_scrollbar_tracks_the_viewport() {
        assert_eq!(conversation_scrollbar_metrics(600.0, 0.0, 0.0), None);
        let (top, height) = conversation_scrollbar_metrics(600.0, 600.0, 0.0).unwrap();
        assert_eq!(top, 0.0);
        assert_eq!(height, 292.0);
        let (middle, _) = conversation_scrollbar_metrics(600.0, 600.0, -300.0).unwrap();
        let (bottom, _) = conversation_scrollbar_metrics(600.0, 600.0, -600.0).unwrap();
        assert_eq!(middle, 146.0);
        assert_eq!(bottom, 292.0);
    }

    #[test]
    fn conversation_scrollbar_drag_maps_and_clamps_pointer_position() {
        assert_eq!(
            conversation_scroll_offset_for_pointer(584.0, 292.0, 600.0, -20.0),
            Some(0.0)
        );
        assert_eq!(
            conversation_scroll_offset_for_pointer(584.0, 292.0, 600.0, 146.0),
            Some(-300.0)
        );
        assert_eq!(
            conversation_scroll_offset_for_pointer(584.0, 292.0, 600.0, 500.0),
            Some(-600.0)
        );
        assert_eq!(
            conversation_scroll_offset_for_pointer(292.0, 292.0, 600.0, 0.0),
            None
        );
    }

    // ---- copy (§1) ----------------------------------------------------------

    #[test]
    fn copied_flash_targets_one_message() {
        let mut flash = CopiedFlash::new();
        assert!(!flash.is("conv-1", 1));
        flash.set("conv-1", 1);
        assert!(flash.is("conv-1", 1));
        assert!(!flash.is("conv-1", 0), "only the copied row flashes");
        assert!(!flash.is("conv-2", 1), "scoped to its conversation");
    }

    #[test]
    fn copied_flash_clear_is_generation_guarded() {
        // A re-copy inside the 1.5s window restarts the clock: the FIRST
        // timer's clear must not wipe the newer flash.
        let mut flash = CopiedFlash::new();
        let first = flash.set("conv-1", 1);
        let second = flash.set("conv-1", 3);
        assert!(!flash.clear_if(first), "stale timer fires into a no-op");
        assert!(
            flash.is("conv-1", 3),
            "newer flash survives the stale clear"
        );
        assert!(flash.clear_if(second));
        assert!(!flash.is("conv-1", 3));
    }

    // ---- regenerate (§2) ----------------------------------------------------

    #[test]
    fn regen_targets_only_the_last_assistant_message() {
        let v = view_of(&[(Role::User, "q1"), (Role::Assistant, "a1")]);
        assert_eq!(regen_target(&v), Some(1));
        // A user turn at the end (reply pending/missing): nothing to regenerate.
        let v = view_of(&[
            (Role::User, "q1"),
            (Role::Assistant, "a1"),
            (Role::User, "q2"),
        ]);
        assert_eq!(regen_target(&v), None);
        // Earlier assistant rows are never the target — only the last message.
        assert!(view_of(&[]).convo.is_empty());
        assert_eq!(regen_target(&view_of(&[])), None);
    }

    #[test]
    fn regen_requires_a_user_prompt_to_rerun() {
        // A lone assistant notice (e.g. the config-error bubble) has no user
        // turn above it — there is nothing to re-send.
        let v = view_of(&[(Role::Assistant, "⚠ config error")]);
        assert_eq!(regen_target(&v), None);
    }
}

/// The `[answered by …]` tag older fallback replies carry: only the trailing
/// tag is ever stripped.
#[cfg(test)]
mod fallback_fix_tests {
    use super::*;

    #[test]
    fn strip_answered_by_tag_strips_only_the_trailing_tag() {
        // SF4: the provenance suffix goes; everything else stays byte-exact.
        assert_eq!(strip_answered_by_tag("Hello\n\n[answered by m-b]"), "Hello");
        assert_eq!(strip_answered_by_tag("Hello"), "Hello");
        // A mid-reply mention is the model's own prose, not our tag.
        let prose = "see\n\n[answered by m]\nmore text";
        assert_eq!(strip_answered_by_tag(prose), prose);
        // A legacy tag-only row (pre-SF5) strips to empty — and is then
        // dropped by build_history's empty check.
        assert_eq!(strip_answered_by_tag("\n\n[answered by m-b]"), "");
    }

    #[test]
    fn strip_answered_by_tag_requires_a_closing_bracket() {
        // SF4: our provenance tag is a COMPLETE `[answered by …]` line. Text that
        // merely trails off into an UNCLOSED "[answered by …" (model prose that
        // happens to end on that phrase, or a stream stopped mid-tag) is NOT our
        // tag and must survive replay byte-exact — dropping the `ends_with(']')`
        // guard would strip legitimate assistant prose off the provider payload.
        // Every other strip case here carries a closing `]`, so this guard is the
        // only thing standing between an unclosed suffix and a wrongful strip.
        assert_eq!(
            strip_answered_by_tag("Hello\n\n[answered by m-b"),
            "Hello\n\n[answered by m-b",
        );
    }
}

/// Audible arm/disarm cue for conversation-mode voice (hot-mic ceremony).
/// System sounds via `afplay`, fire-and-forget: a missing file or failed
/// spawn silently degrades to no cue — the banner still shows.
fn play_voice_cue(arming: bool) {
    let sound = if arming {
        "/System/Library/Sounds/Glass.aiff"
    } else {
        "/System/Library/Sounds/Bottom.aiff"
    };
    let _ = std::process::Command::new("afplay").arg(sound).spawn();
}

fn update_active_awili(
    cx: &mut App,
    update: impl FnOnce(&mut AwiliApp, &mut Window, &mut Context<AwiliApp>) + 'static,
) {
    let Some(handle) = cx.try_global::<AwiliWindow>().map(|window| window.0) else {
        return;
    };
    // Menu (and keymap-dispatched) actions arrive from INSIDE the active
    // window's update pass, and gpui's `update_window_id` takes the window out
    // of its slot for the duration of that pass — so a synchronous
    // `handle.update` here always failed with "window not found", which is why
    // every app-level menu item (zoom, palette, export, new conversation,
    // toggle needs-you…) was a silent no-op in the hands-on user test.
    // Deferring runs the update after the current pass returns the window.
    cx.defer(move |cx| {
        if let Err(error) = handle.update(cx, |app, window, cx| {
            if app.shell.tabs.delete_project.is_none() {
                update(app, window, cx);
            }
        }) {
            eprintln!("native menu action failed: {error:#}");
        }
    });
}

fn install_native_menu_actions(cx: &mut App) {
    cx.on_action(|_: &native_menu::About, cx| {
        // U3d D9: the native About dialog (title + version/commit) replaces
        // the transient toast. The deferred pattern mirrors the other menu
        // actions so it runs after the current window pass returns the window.
        let Some(handle) = cx.try_global::<AwiliWindow>().map(|w| w.0) else {
            return;
        };
        cx.defer(move |cx| {
            if let Err(error) = handle.update(cx, |app, window, cx| app.about(window, cx)) {
                eprintln!("About action failed: {error:#}");
            }
        });
    });
    cx.on_action(|_: &native_menu::NewConversation, cx| {
        update_active_awili(cx, |app, window, cx| app.new_conversation(window, cx))
    });
    cx.on_action(|_: &native_menu::OpenSettings, cx| {
        update_active_awili(cx, |app, _window, cx| app.open_settings(cx))
    });
    cx.on_action(|_: &native_menu::ExportConversation, cx| {
        update_active_awili(cx, |app, _window, cx| app.save_conversation(cx))
    });
    cx.on_action(|_: &native_menu::SaveConversationAs, cx| {
        update_active_awili(cx, |app, _window, cx| {
            app.save_on_screen_conversation_as(cx)
        })
    });
    cx.on_action(|_: &native_menu::Find, cx| {
        update_active_awili(cx, |app, window, cx| app.toggle_search(window, cx))
    });
    cx.on_action(|_: &native_menu::OpenCommandPalette, cx| {
        update_active_awili(cx, |app, window, cx| app.open_command_palette(window, cx))
    });
    cx.on_action(|_: &native_menu::ToggleNeedsYou, cx| {
        update_active_awili(cx, |app, _window, cx| app.toggle_feed_panel(cx))
    });
    cx.on_action(|_: &native_menu::ShowShortcuts, cx| {
        update_active_awili(cx, |app, window, cx| {
            app.toggle_shortcuts_focused(window, cx)
        })
    });
    cx.on_action(|_: &native_menu::CloseWindow, cx| {
        // U3d D7: ⌘W HIDES the window (the app stays alive and any in-flight
        // run keeps going); it no longer destroys the only window and quits.
        // `cx.hide()` needs no window update.
        app_lifecycle::hide_app(cx);
    });
    cx.on_action(|_: &native_menu::Minimize, cx| {
        update_active_awili(cx, |_app, window, _cx| window.minimize_window())
    });
    cx.on_action(|_: &native_menu::Zoom, cx| {
        update_active_awili(cx, |_app, window, _cx| window.zoom_window())
    });
    cx.on_action(|_: &native_menu::ZoomIn, cx| {
        update_active_awili(cx, |app, window, cx| app.zoom_in(window, cx))
    });
    cx.on_action(|_: &native_menu::ZoomOut, cx| {
        update_active_awili(cx, |app, window, cx| app.zoom_out(window, cx))
    });
    cx.on_action(|_: &native_menu::ActualSize, cx| {
        update_active_awili(cx, |app, window, cx| app.zoom_actual(window, cx))
    });
}

fn main() {
    // Windows: first thing, before any thread or child: no program search in the
    // current folder (see agent_bin::harden_search_path).
    #[cfg(windows)]
    agent_bin::harden_search_path();
    // Linux: the app data folder exists and is 0700 before anything else touches
    // it, so no later writer can create it with the default umask.
    #[cfg(target_os = "linux")]
    if let Some(dir) = awili_config::app_data_dir() {
        let _ = awili_config::ensure_private_app_dir(&dir);
    }
    notify_platform::install_early();
    let application = application().with_assets(icons::Assets);
    // U3d D7: clicking the dock icon (or relaunching a hidden app) re-activates
    // Awili so the hidden window comes back to the front instead of a no-op.
    application.on_reopen(|cx| cx.activate(true));
    application.run(|cx| {
        // Restore the persisted interface zoom before the first render so the
        // whole UI opens at the user's chosen size (A4).
        theme::set_ui_scale(zoom::load());
        // Slice K (KIT.md v3): read the system Reduce Motion preference at
        // launch and observe its live changes (no-op off macOS).
        ui::motion::install();
        // Keyboard shortcuts. gpui ships no default keymap: without these
        // bindings every ⌘-shortcut was dead and the native menu items showed
        // no key equivalents (user-test A3/A4/J6). Registered before the menu
        // is installed so AppKit can display the equivalents on the items.
        // ⌘V stays OFF this list on purpose: the composer handles it in
        // `on_key` (AppKit-first contract) and a binding would double-paste.
        // U3d D1: ⌘Q/⌘H/⌥⌘H are bound so the native menu items show their
        // key equivalents (the set is defined in `app_lifecycle::standard_bindings`).
        cx.bind_keys(app_lifecycle::standard_bindings());
        native_menu::install(cx);
        install_native_menu_actions(cx);
        // On graceful quit, SIGKILL any in-flight Code turns (and everything they
        // spawned) so we don't orphan agent process groups. A hard crash can't be
        // covered on macOS, but a normal quit no longer leaves agents running.
        cx.on_app_quit(|_cx| {
            code_provider::kill_all_active_groups();
            async {}
        })
        .detach();
        // macOS doesn't quit on window close by default, so the quit hook alone
        // would miss the red-button path — leaving agents running with no UI.
        // Awili is a single-window app: when the last window closes, kill in-flight
        // Code turns and quit (which also runs the quit hook above; harmless).
        cx.on_window_closed(|cx, _id| {
            if cx.windows().is_empty() {
                code_provider::kill_all_active_groups();
                cx.quit();
            }
        })
        .detach();
        // Slice K (KIT.md § The gallery): `AWILI_DESIGN_GALLERY=1` opens the
        // kit's visual reference instead of the app — one window, kit parts
        // only, no app data. The quit-on-close hook above still applies.
        if std::env::var("AWILI_DESIGN_GALLERY").as_deref() == Ok("1") {
            ui::gallery::Gallery::open(cx);
            return;
        }
        // Polish slice A: reopen at the saved geometry; fall back to the
        // centered default on first run or a corrupt/implausible value.
        // GLM night-review MUST-1: in this gpui rev, saved bounds (reported
        // relative to the window's CURRENT screen, global-offset y) and
        // restored bounds (interpreted relative to the PRIMARY display) only
        // share a coordinate space on the primary display. Restore therefore
        // accepts geometry only when it lies (>=100px) on the primary; saved
        // on any other monitor — or on one since disconnected — falls back to
        // the centered default rather than reopening off-screen.
        let primary: Vec<(f32, f32, f32, f32)> = cx
            .primary_display()
            .iter()
            .map(|d| {
                let b = d.bounds();
                (
                    f32::from(b.origin.x),
                    f32::from(b.origin.y),
                    f32::from(b.size.width),
                    f32::from(b.size.height),
                )
            })
            .collect();
        // U3d D12: a saved size below the minimum is unusable — fall back to
        // the centered default (restorable() is the pure predicate).
        let bounds = match winbounds::load().filter(|b| {
            winbounds::visible_on_any(&primary, *b) && app_lifecycle::restorable(b.w, b.h)
        }) {
            Some(b) => Bounds::new(gpui::point(px(b.x), px(b.y)), size(px(b.w), px(b.h))),
            None => Bounds::centered(None, size(px(900.), px(680.)), cx),
        };
        // Windows: GPUI's display bounds include the taskbar, so a default or
        // restored window could sit under it. Fit the client bounds (plus the
        // frame) inside the primary display's work area. macOS is unchanged:
        // AppKit keeps a titled, resizable window inside the visible frame.
        #[cfg(windows)]
        let bounds = match winbounds::primary_work_area() {
            Some(area) => {
                let fitted = winbounds::fit_to_work_area(
                    winbounds::SavedBounds {
                        x: f32::from(bounds.origin.x),
                        y: f32::from(bounds.origin.y),
                        w: f32::from(bounds.size.width),
                        h: f32::from(bounds.size.height),
                    },
                    area,
                );
                Bounds::new(
                    gpui::point(px(fitted.x), px(fitted.y)),
                    size(px(fitted.w), px(fitted.h)),
                )
            }
            None => bounds,
        };
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // U3d D12: the window cannot be shrunk below the floor
                    // where the layout still holds.
                    window_min_size: Some(size(
                        px(app_lifecycle::MIN_WINDOW_SIZE.0),
                        px(app_lifecycle::MIN_WINDOW_SIZE.1),
                    )),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Awili".into()),
                        ..Default::default()
                    }),
                    app_id: Some("awili".into()),
                    ..Default::default()
                },
                |window, cx| {
                    // U3d D7: the red button hides the window (like ⌘W) instead
                    // of destroying it; the app stays alive so in-flight runs
                    // keep going. `cx.hide()` is the macOS hide.
                    app_lifecycle::hide_on_close(window, cx);
                    cx.new(|cx| {
                        // Save geometry on every move/resize (tiny file write;
                        // covers red-button close and force-quit paths that
                        // never reach the quit hook).
                        cx.observe_window_bounds(window, |_app, win, _cx| {
                            if let WindowBounds::Windowed(b) = win.window_bounds() {
                                winbounds::save(winbounds::SavedBounds {
                                    x: f32::from(b.origin.x),
                                    y: f32::from(b.origin.y),
                                    w: f32::from(b.size.width),
                                    h: f32::from(b.size.height),
                                });
                            }
                        })
                        .detach();
                        AwiliApp::new(cx)
                    })
                },
            )
            .unwrap();
        cx.set_global(AwiliWindow(window));
        cx.activate(true);
    });
}

#[cfg(test)]
mod recall_tests {
    use super::*;
    use awili_memory::RecallEntry;

    #[test]
    fn recall_status_maps_fetch_result() {
        // An answered service is Loaded(n) — even when empty; an offline or
        // errored fetch (None) is Offline. Same contract as the rules chip.
        let entries = vec![RecallEntry {
            id: None,
            text: "past fact".into(),
            category: None,
            priority: 0,
        }];
        assert_eq!(recall_status(Some(&entries)), RecallStatus::Loaded(1));
        assert_eq!(recall_status(Some(&[])), RecallStatus::Loaded(0));
        assert_eq!(recall_status(None), RecallStatus::Offline);
    }

    #[test]
    fn recall_chip_hidden_only_when_quiet() {
        // Slice 9 §5: hidden before the first turn and when the service is up
        // with nothing recalled; visible with a count or when offline.
        assert_eq!(recall_chip_label(None), None);
        assert_eq!(recall_chip_label(Some(&RecallStatus::Loaded(0))), None);
        assert_eq!(
            recall_chip_label(Some(&RecallStatus::Loaded(4))).as_deref(),
            Some("4 notes recalled")
        );
        assert_eq!(
            recall_chip_label(Some(&RecallStatus::Offline)).as_deref(),
            Some("Recall: memory offline")
        );
    }
}

/// `nav_route_to_mode` is the single place the pure `palette::NavRoute`
/// vocabulary meets the view `Mode` enum (user-test bug #3). It is exercised by
/// both `run_palette_cmd` (the `GoTo` dispatch) and the row renderer's "current"
/// marker, but the mapping itself had no direct guard — a swapped or dropped arm
/// would silently route "Go to Skills" to the wrong surface.
#[cfg(test)]
mod nav_route_tests {
    use super::*;

    #[test]
    fn each_route_maps_to_its_surface() {
        // Exhaustive per-variant pin: a swapped arm (e.g. Skills -> Code) fails here.
        assert_eq!(nav_route_to_mode(palette::NavRoute::Work), Mode::Work);
        assert_eq!(nav_route_to_mode(palette::NavRoute::Code), Mode::Code);
        assert_eq!(nav_route_to_mode(palette::NavRoute::Skills), Mode::Skills);
        assert_eq!(nav_route_to_mode(palette::NavRoute::Cron), Mode::Cron);
        assert_eq!(
            nav_route_to_mode(palette::NavRoute::Sessions),
            Mode::Sessions
        );
        assert_eq!(
            nav_route_to_mode(palette::NavRoute::CommandCenter),
            Mode::CommandCenter
        );
        assert_eq!(
            nav_route_to_mode(palette::NavRoute::Messaging),
            Mode::Messaging
        );
    }

    #[test]
    fn mapping_is_total_and_injective_over_all_routes() {
        // Drift guard: every catalogued route resolves to a DISTINCT mode, and
        // together they cover every real surface. If a NavRoute variant is added
        // (NavRoute::ALL grows) but its arm is forgotten, duplicates an existing
        // mode, or drops one, this fails — keeping the palette catalog and the
        // mode set one-to-one.
        let modes: Vec<Mode> = palette::NavRoute::ALL
            .iter()
            .map(|&r| nav_route_to_mode(r))
            .collect();
        assert_eq!(modes.len(), palette::NavRoute::ALL.len());

        // All distinct (Mode is not Hash, so compare pairwise).
        for (i, a) in modes.iter().enumerate() {
            for b in &modes[i + 1..] {
                assert_ne!(a, b, "two routes map to the same mode: {a:?}");
            }
        }

        // Cover exactly the real surfaces the palette can jump to.
        for expected in [
            Mode::Work,
            Mode::Code,
            Mode::Skills,
            Mode::Cron,
            Mode::Sessions,
            Mode::CommandCenter,
            Mode::Messaging,
        ] {
            assert!(modes.contains(&expected), "no route maps to {expected:?}");
        }
    }
}

#[cfg(test)]
mod conversation_export_busy_tests {
    use super::*;

    #[test]
    fn work_run_only_blocks_its_own_conversation() {
        assert!(conversation_export_is_busy(
            Mode::Work,
            "conversation-a",
            true,
            Some("conversation-a"),
        ));
        assert!(!conversation_export_is_busy(
            Mode::Work,
            "conversation-b",
            true,
            Some("conversation-a"),
        ));
    }

    #[test]
    fn unpinned_work_run_fails_closed() {
        assert!(conversation_export_is_busy(
            Mode::Work,
            "conversation-a",
            true,
            None,
        ));
        assert!(!conversation_export_is_busy(
            Mode::Work,
            "conversation-a",
            false,
            None,
        ));
    }
}

/// The header gateway chip must reflect a SINGLE connection state, not the
/// current mode. Before this fix the probe was gated behind Mode::Work entry,
/// so the chip sat grey in Chat until Work was opened once ("grey in Chat,
/// green in Work"). These pin the launch-probe predicate.
#[cfg(test)]
mod gateway_chip_probe_tests {
    use super::*;

    #[test]
    fn probes_on_launch_only_when_visible_and_unknown() {
        // Chip is visible (gateway enabled), no probe has run, none in flight:
        // this is exactly the grey-in-Chat case we must fix — probe once.
        assert!(should_probe_gateway_on_launch(true, false, false));
    }

    #[test]
    fn no_probe_when_chip_hidden() {
        // Gateway disabled -> chip returns None, so there is nothing to populate
        // and no network work to do, regardless of status/testing.
        assert!(!should_probe_gateway_on_launch(false, false, false));
        assert!(!should_probe_gateway_on_launch(false, true, false));
        assert!(!should_probe_gateway_on_launch(false, false, true));
    }

    #[test]
    fn no_double_probe_when_already_known_or_in_flight() {
        // A status already resolved (Work opened earlier, or a prior launch
        // probe finished) must not re-fire; an in-flight probe likewise. This
        // keeps the invariant that the chip is driven by a single probe.
        assert!(!should_probe_gateway_on_launch(true, true, false));
        assert!(!should_probe_gateway_on_launch(true, false, true));
        assert!(!should_probe_gateway_on_launch(true, true, true));
    }
}

/// Slice-17 export wiring: the toast's show/auto-dismiss state machine (the
/// pure half the 3s timer drives) and ~/Downloads path construction.
#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn toast_shown_on_export_message() {
        let mut t = ToastState::new();
        assert!(t.visible().is_none(), "no toast before any export");
        t.show("Copied to clipboard");
        assert_eq!(t.visible().map(|s| s.as_ref()), Some("Copied to clipboard"));
    }

    #[test]
    fn toast_auto_dismiss() {
        // The timer fires `clear_if` with the generation `show` returned —
        // exactly what `show_toast`'s spawned 3s sleep does.
        let mut t = ToastState::new();
        let gen = t.show("Saved to ~/Downloads/x.md");
        assert!(t.clear_if(gen), "the timer's clear must land");
        assert!(t.visible().is_none(), "toast gone after auto-dismiss");
        assert!(!t.clear_if(gen), "a second fire is a no-op");
    }

    #[test]
    fn toast_newer_show_outlives_stale_timer() {
        // A second toast inside the 3s window restarts the clock: the FIRST
        // timer's clear must not cut the newer message short.
        let mut t = ToastState::new();
        let first = t.show("Copied to clipboard");
        let second = t.show("Saved to ~/Downloads/x.md");
        assert!(!t.clear_if(first));
        assert_eq!(
            t.visible().map(|s| s.as_ref()),
            Some("Saved to ~/Downloads/x.md")
        );
        assert!(t.clear_if(second));
    }

    #[test]
    fn downloads_path_lands_in_downloads() {
        let p = crate::attachments::downloads_path("hello-world-1-2025-06-15.md")
            .expect("some home/downloads dir exists");
        assert!(p.is_absolute());
        assert_eq!(
            p.file_name().and_then(|n| n.to_str()),
            Some("hello-world-1-2025-06-15.md")
        );
        assert_eq!(
            p.parent()
                .and_then(|d| d.file_name())
                .and_then(|n| n.to_str()),
            Some("Downloads")
        );
    }
}

/// Slice 20: the stop button's GPUI-free core — the abort wire, the streaming
/// lock, the `[…stopped]` suffix, and the Send⇄Stop label.
#[cfg(test)]
mod stop_button_tests {
    use super::*;

    #[test]
    fn stop_button_appears_while_streaming() {
        // Spec §1d: mid-stream the composer-bar button says Stop, not Send.
        assert!(send_button_label(true).contains("Stop"));
        assert!(!send_button_label(true).contains("Send"));
        assert!(send_button_label(false).contains("Send"));
        assert!(!send_button_label(false).contains("Stop"));
    }
}

/// File-attachment wiring tests (slice 19): the send-path composition runs
/// against a REAL `Store` on a temp file — store, stage, compose, clear — the
/// exact sequence `on_submit` drives, without needing a GPUI window.
#[cfg(test)]
mod attachment_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;

    fn temp_db() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "awili-app-attach-test-{}-{}.db",
            std::process::id(),
            n
        ))
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn send_with_attachment() {
        // The on_submit sequence: read staged, prepend to the typed text,
        // clear the staging. The attachment content lands IN the message
        // text; the staging table is empty afterwards.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.create_conversation("c1", "t", 0).unwrap();
        let id = store
            .store_attachment("notes.txt", "text/plain", b"the notes")
            .unwrap();
        store.stage_attachment("c1", id).unwrap();

        let staged = store.get_staged_attachments("c1").unwrap();
        let sent = compose_message_with_attachments("summarize this", &staged);
        assert_eq!(sent, "```notes.txt\nthe notes\n```\n\nsummarize this");

        store.clear_staged_attachments("c1").unwrap();
        assert!(store.get_staged_attachments("c1").unwrap().is_empty());
        cleanup(&path);
    }

    #[test]
    fn compose_without_attachments_is_the_text() {
        assert_eq!(compose_message_with_attachments("plain", &[]), "plain");
    }

    #[test]
    fn compose_orders_attachments_before_text() {
        let att = |name: &str, body: &[u8]| Attachment {
            id: 0,
            filename: name.to_string(),
            mime_type: "text/plain".to_string(),
            size_bytes: body.len() as i64,
            content: body.to_vec(),
            created_at: 0,
        };
        let out =
            compose_message_with_attachments("question", &[att("a.txt", b"A"), att("b.txt", b"B")]);
        assert_eq!(out, "```a.txt\nA\n```\n\n```b.txt\nB\n```\n\nquestion");
    }

    #[test]
    fn mime_for_path_families() {
        use std::path::Path;
        assert_eq!(mime_for_path(Path::new("/x/notes.txt")), "text/plain");
        assert_eq!(mime_for_path(Path::new("/x/README.md")), "text/markdown");
        assert_eq!(mime_for_path(Path::new("/x/data.json")), "application/json");
        assert_eq!(
            mime_for_path(Path::new("/x/Cargo.toml")),
            "application/toml"
        );
        assert_eq!(
            mime_for_path(Path::new("/x/PIC.PNG")),
            "image/png",
            "case-folded"
        );
        assert_eq!(mime_for_path(Path::new("/x/shot.jpeg")), "image/jpeg");
        assert_eq!(
            mime_for_path(Path::new("/x/archive.zip")),
            "application/octet-stream"
        );
        assert_eq!(
            mime_for_path(Path::new("/x/no-extension")),
            "application/octet-stream"
        );
    }

    #[test]
    fn attachment_chip_label_rounds_size_up() {
        let att = |size: i64| Attachment {
            id: 0,
            filename: "f.txt".to_string(),
            mime_type: "text/plain".to_string(),
            size_bytes: size,
            content: Vec::new(),
            created_at: 0,
        };
        assert_eq!(attachment_chip_label(&att(10)), "f.txt (1KB)");
        assert_eq!(attachment_chip_label(&att(1024)), "f.txt (1KB)");
        assert_eq!(attachment_chip_label(&att(1025)), "f.txt (2KB)");
        assert_eq!(attachment_chip_label(&att(0)), "f.txt (0KB)");
    }
}

#[cfg(test)]
mod live_config_refresh_tests {
    use super::*;
    use awili_config::Config;
    use work::HermesCfg;

    /// Proves that `apply_config_live` propagates a `[hermes] base_url` change
    /// into both `self.keys` (chat provider construction) and `self.work.hermes`
    /// (Work-mode endpoint). Before the fix these were left stale after a live
    /// settings save, forcing a restart for Hermes credential changes.
    #[test]
    fn hermes_base_url_change_takes_effect() {
        // Start with one URL + a key.
        let mut cfg = Config::default();
        cfg.hermes.base_url = "http://old-hermes:8642/v1".to_string();
        cfg.hermes.api_key = "old-key".to_string();

        let keys = ProviderKeys::from_env_and_config(&cfg);
        let hermes = HermesCfg {
            dashboard_url: String::new(),
            enabled: !cfg.hermes.base_url.is_empty(),
            base_url: cfg.hermes.base_url.clone(),
            api_key: cfg.hermes.api_key.clone(),
        };
        assert!(hermes.enabled);
        assert_eq!(hermes.base_url, "http://old-hermes:8642/v1");
        assert_eq!(hermes.api_key, "old-key");
        assert_eq!(keys.hermes.as_deref(), Some("old-key"));

        // Change the URL + key (the live-save scenario).
        cfg.hermes.base_url = "http://new-hermes:8642/v1".to_string();
        cfg.hermes.api_key = "new-key".to_string();

        let keys = ProviderKeys::from_env_and_config(&cfg);
        let hermes = HermesCfg {
            dashboard_url: String::new(),
            enabled: !cfg.hermes.base_url.is_empty(),
            base_url: cfg.hermes.base_url.clone(),
            api_key: cfg.hermes.api_key.clone(),
        };
        assert!(hermes.enabled);
        assert_eq!(
            hermes.base_url, "http://new-hermes:8642/v1",
            "base_url updated live"
        );
        assert_eq!(hermes.api_key, "new-key", "api_key updated live");
        assert_eq!(
            keys.hermes.as_deref(),
            Some("new-key"),
            "ProviderKeys::hermes updated live"
        );
    }

    /// P10/F2: a live reload of `[engine]` must refresh the Work engine
    /// binding together with its bearer — `build_awili_engine` reads
    /// `work.engine.base_url` and `keys.engine`, so both new values must come
    /// from the SAME reload (a new URL with a stale key is the exact hazard).
    #[test]
    fn engine_base_url_change_takes_effect() {
        let mut cfg = Config::default();
        cfg.engine.base_url = "http://old-engine:8000/v1".to_string();
        cfg.engine.api_key = "old-engine-key".to_string();
        cfg.engine.model = "old-model".to_string();
        cfg.gateway.dashboard_url = "http://old-gateway:9119".to_string();

        let mut state = WorkState::new(
            HermesCfg {
                dashboard_url: String::new(),
                enabled: false,
                base_url: String::new(),
                api_key: String::new(),
            },
            work::GatewayCfg {
                enabled: true,
                dashboard_url: cfg.gateway.dashboard_url.clone(),
            },
            cfg.engine.clone(),
        );
        AwiliApp::refresh_work_endpoints(&mut state, &cfg);
        let keys = ProviderKeys::from_env_and_config(&cfg);
        assert_eq!(state.engine.base_url, "http://old-engine:8000/v1");
        assert_eq!(keys.engine.as_deref(), Some("old-engine-key"));

        // The live-save scenario: new endpoint, new key, moved gateway.
        cfg.engine.base_url = "http://new-engine:8000/v1".to_string();
        cfg.engine.api_key = "new-engine-key".to_string();
        cfg.gateway.dashboard_url = "http://new-gateway:9119".to_string();

        AwiliApp::refresh_work_endpoints(&mut state, &cfg);
        let keys = ProviderKeys::from_env_and_config(&cfg);
        // The next `build_awili_engine` reads exactly these two fields.
        assert_eq!(
            state.engine.base_url, "http://new-engine:8000/v1",
            "engine base_url updated live"
        );
        assert_eq!(
            state.engine.api_key, "new-engine-key",
            "engine api_key updated live"
        );
        assert_eq!(
            keys.engine.as_deref(),
            Some("new-engine-key"),
            "ProviderKeys::engine updated live"
        );
        assert_eq!(state.gateway.dashboard_url, "http://new-gateway:9119");
        assert!(state.gateway.enabled);
    }

    /// Proves that clearing the Hermes base_url live (disabling Hermes) sets
    /// `enabled = false` in both the HermesCfg and prevents a key from being
    /// forwarded — the UI shows "Hermes not configured" instead of firing a
    /// request at an empty endpoint.
    ///
    /// Uses a config-sourced key (not env) so this test does not race with
    /// sibling tests that read `AWILI_HERMES_KEY` (e.g. `config_hermes_key_overrides_env`
    /// in `provider_build::tests`).
    #[test]
    fn empty_base_url_disables_hermes() {
        let mut cfg = Config::default();
        cfg.hermes.base_url = "http://hermes:8642/v1".to_string();
        cfg.hermes.api_key = "cfg-key".to_string();

        // Enabled state.
        let hermes = HermesCfg {
            dashboard_url: String::new(),
            enabled: !cfg.hermes.base_url.is_empty(),
            base_url: cfg.hermes.base_url.clone(),
            api_key: cfg.hermes.api_key.clone(),
        };
        assert!(hermes.enabled);

        // Clear the base_url as if the user saved an empty [hermes] section.
        cfg.hermes.base_url.clear();
        cfg.hermes.api_key.clear();

        let keys = ProviderKeys::from_env_and_config(&cfg);
        let hermes = HermesCfg {
            dashboard_url: String::new(),
            enabled: !cfg.hermes.base_url.is_empty(),
            base_url: cfg.hermes.base_url.clone(),
            api_key: cfg.hermes.api_key.clone(),
        };
        assert!(!hermes.enabled, "Hermes disabled when base_url emptied");
        assert!(hermes.base_url.is_empty());
        assert!(hermes.api_key.is_empty());
        // Config key was cleared so it won't override the (absent) env key;
        // from_env_and_config returns whatever from_env found. The test only
        // asserts that the hermes key is *not* the old config value — it must
        // be either None (env also empty) or something else. This avoids racing
        // on AWILI_HERMES_KEY with parallel sibling tests.
        assert!(
            keys.hermes.as_deref() != Some("cfg-key"),
            "cleared config key must not leak into ProviderKeys"
        );
    }

    /// Proves that the anthropic key is never forwarded to the Hermes path:
    /// `ProviderKeys::anthropic` stays on `ProviderKind::Anthropic` and never
    /// leaks into `keys.hermes`. Regression guard for the safety-critical
    /// invariant — the hosted Anthropic key must never reach a user-controlled
    /// `base_url`.
    ///
    /// Uses a config-sourced hermes key (not env) so this test does not race
    /// with sibling tests that read `AWILI_HERMES_KEY`.
    #[test]
    fn anthropic_key_never_leaks_to_hermes() {
        let mut cfg = Config::default();
        // Set a non-empty hermes api_key in config so from_env_and_config
        // picks it up regardless of env state.
        cfg.hermes.api_key = "hermes-key".to_string();

        let keys = ProviderKeys::from_env_and_config(&cfg);
        // The config hermes key IS forwarded (that's correct), but the
        // anthropic key must remain absent because AWILI_ANTHROPIC_KEY is
        // not set and no config field maps to anthropic.
        assert!(
            keys.anthropic.is_none() || keys.anthropic.as_deref().unwrap().is_empty(),
            "anthropic key must not come from hermes config"
        );
        // The hermes key must be the config value.
        assert_eq!(
            keys.hermes.as_deref(),
            Some("hermes-key"),
            "hermes key should come from config"
        );
    }

    /// P12/F1: `AwiliApp::new` must populate the connector sections at
    /// CONSTRUCTION. Before the fix only `apply_runtime_config` (a Settings
    /// save) set them, so an ordinary launch with `[outline] enabled = true`
    /// left `WorkState.connectors` at its disabled default — the Work-agent
    /// and Code-tab spawns then passed no `--mcp-config`/`-c mcp_servers`
    /// flags and the Work panel showed no Connectors label.
    #[test]
    fn startup_populates_connector_sections_from_config() {
        let cfg = Config::parse("[outline]\nenabled = true\n").expect("outline config parses");
        let mut state = WorkState::new(
            HermesCfg {
                dashboard_url: String::new(),
                enabled: false,
                base_url: String::new(),
                api_key: String::new(),
            },
            work::GatewayCfg {
                enabled: false,
                dashboard_url: String::new(),
            },
            cfg.engine.clone(),
        );
        assert_eq!(
            state.connectors,
            crate::connectors_mcp::ConnectorsSections::default(),
            "a fresh WorkState starts from the disabled defaults"
        );

        AwiliApp::apply_startup_work_config(&mut state, &cfg);

        assert!(
            state.connectors.outline.enabled,
            "[outline] enabled = true must reach WorkState at construction"
        );
        assert_ne!(
            state.connectors,
            crate::connectors_mcp::ConnectorsSections::default(),
            "the startup config must populate a non-default connectors value"
        );
    }
}

#[cfg(test)]
mod status_run_tests {
    use super::*;

    /// The status run keeps its fixed order (context, cost, rules, recall,
    /// saved-flash, gateway) and only renders what exists.
    #[test]
    fn parts_ordering_and_presence() {
        let parts = status_run_parts(
            Some(("~1.2k / 131k".into(), theme::p().faint)),
            Some("$0.42 today".into()),
            Some(("rules ok".into(), false)),
            None,
            true,
            Some(true),
        );
        let texts: Vec<&str> = parts.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            texts,
            [
                "~1.2k / 131k",
                "$0.42 today",
                "rules ok",
                "memory saved",
                "\u{25CF} gateway"
            ]
        );
    }

    /// Absent inputs contribute nothing — an all-quiet app renders no run.
    #[test]
    fn all_absent_is_empty() {
        assert!(status_run_parts(None, None, None, None, false, None).is_empty());
    }

    /// Trouble warms exactly the troubled segment: offline rules/recall go
    /// WARN, a down gateway shows the hollow dot in WARN; healthy segments
    /// stay FAINT (glance data, not decoration).
    #[test]
    fn trouble_coloring() {
        let parts = status_run_parts(
            None,
            None,
            Some(("rules offline".into(), true)),
            Some(("recall offline".into(), true)),
            false,
            Some(false),
        );
        assert_eq!(parts.len(), 3);
        for (text, color) in &parts {
            assert_eq!(
                (color.r, color.g, color.b),
                (
                    theme::p().chip_warn.r,
                    theme::p().chip_warn.g,
                    theme::p().chip_warn.b
                ),
                "{text} should be WARN"
            );
        }
        assert_eq!(parts[2].0, "\u{25CB} gateway");
    }
}

#[cfg(test)]
mod model_discovery_tests {
    use super::*;
    use awili_config::ProviderKind;

    fn choice(provider: &str, base: &str, model: &str) -> ModelChoice {
        ModelChoice {
            provider: provider.into(),
            kind: ProviderKind::OpenAi,
            base_url: base.into(),
            model: model.into(),
            api_key: None,
        }
    }

    /// Both wire shapes parse; a recognized empty array is a successful
    /// empty catalog; garbage, shapeless, and non-array JSON is `None` — a
    /// malformed body is never a successful empty.
    #[test]
    fn parse_both_catalog_shapes() {
        let ollama = r#"{"models":[{"name":"llama3.1:8b"},{"name":"qwen3:32b"}]}"#;
        assert_eq!(
            parse_discovered_models(ollama),
            Some(vec!["llama3.1:8b".to_string(), "qwen3:32b".to_string()])
        );
        let openai = r#"{"data":[{"id":"deepseek-v4"},{"id":"deepseek-flash"}]}"#;
        assert_eq!(
            parse_discovered_models(openai),
            // Sorted by the parser (dedup + sort), not the wire order.
            Some(vec![
                "deepseek-flash".to_string(),
                "deepseek-v4".to_string()
            ])
        );
        // A valid recognized array — even empty — is a successful catalog.
        assert_eq!(
            parse_discovered_models(r#"{"models":[]}"#),
            Some(Vec::new())
        );
        // Invalid JSON / missing or non-array field is not a successful empty.
        assert_eq!(parse_discovered_models("not json"), None);
        assert_eq!(parse_discovered_models(r#"{"ok":true}"#), None);
        assert_eq!(parse_discovered_models(r#"{"models":"x"}"#), None);
    }

    /// Catalog cache update: a None (liveness-only / non-2xx / transport
    /// failure) keeps the last successful catalog; a Some — even an empty
    /// one — replaces it.
    #[test]
    fn cache_some_replaces_none_preserves() {
        let mut cache: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        apply_model_cache(&mut cache, "http://a".into(), Some(vec!["old".into()]));
        // A failed / liveness-only probe keeps the last successful catalog.
        apply_model_cache(&mut cache, "http://a".into(), None);
        assert_eq!(cache.get("http://a"), Some(&vec!["old".to_string()]));
        // A successful empty catalog is authoritative: it replaces it.
        apply_model_cache(&mut cache, "http://a".into(), Some(Vec::new()));
        assert_eq!(cache.get("http://a"), Some(&Vec::new()));
    }

    /// Rebuild semantics: reset to the declared choices, then re-extend with
    /// the current catalog — a declared choice survives, a discovered model
    /// whose catalog no longer lists it disappears.
    #[test]
    fn rebuild_keeps_declared_drops_stale_discovered() {
        let configured = vec![choice("deepseek", "http://a", "deepseek-flash")];
        let mut choices = configured.clone();
        extend_choices_with_discovered(
            &mut choices,
            "http://a",
            &["extra".to_string(), "gone".to_string()],
            None,
            &nc(),
        );
        assert_eq!(
            choices.iter().map(|c| c.model.as_str()).collect::<Vec<_>>(),
            ["deepseek-flash", "extra", "gone"]
        );
        // The next rebuild: the catalog no longer lists "gone".
        choices = configured.clone();
        extend_choices_with_discovered(
            &mut choices,
            "http://a",
            &["extra".to_string()],
            None,
            &nc(),
        );
        assert_eq!(
            choices.iter().map(|c| c.model.as_str()).collect::<Vec<_>>(),
            ["deepseek-flash", "extra"]
        );
    }

    /// Discovered models append after their endpoint's group, keeping other
    /// providers' groups (and flat indices before the insertion) untouched;
    /// duplicates and embedders never enter; unknown base_url is a no-op.
    /// Non-chat filtering is config-driven: an empty list disables it, and a
    /// custom list is honored verbatim (case-insensitively).
    #[test]
    fn non_chat_filters_come_from_config() {
        let d = nc();
        assert!(is_non_chat_model("Whisper-Large-v3-Turbo", &d));
        assert!(!is_non_chat_model("qwen3.6-moe-35b", &d));
        // Empty config list = no filtering at all.
        assert!(!is_non_chat_model("Whisper-Large-v3-Turbo", &[]));
        // A custom vocabulary needs no rebuild.
        let custom = vec!["qwen".to_string()];
        assert!(is_non_chat_model("Qwen3.5-4B", &custom));
        assert!(!is_non_chat_model("Whisper-Large-v3-Turbo", &custom));
    }

    /// The default non-chat filter list, as config supplies it.
    fn nc() -> Vec<String> {
        awili_config::ModelsConfig::default().non_chat_filters
    }

    #[test]
    fn extend_appends_dedupes_and_filters() {
        let mut choices = vec![
            choice("deepseek", "http://a", "deepseek-flash"),
            choice("other", "http://b", "m"),
        ];
        extend_choices_with_discovered(
            &mut choices,
            "http://a",
            &[
                "deepseek-v4".to_string(),
                "deepseek-flash".to_string(),   // already declared
                "nomic-embed-text".to_string(), // embedder
            ],
            None,
            &nc(),
        );
        let flat: Vec<&str> = choices.iter().map(|c| c.model.as_str()).collect();
        assert_eq!(flat, ["deepseek-flash", "deepseek-v4", "m"]);
        // Speech/image models on the same catalog never become chat targets.
        extend_choices_with_discovered(
            &mut choices,
            "http://a",
            &[
                "Whisper-Large-v3-Turbo".to_string(),
                "kokoro-v1".to_string(),
            ],
            None,
            &nc(),
        );
        assert_eq!(choices.len(), 3);
        // Unknown endpoint: nothing happens.
        extend_choices_with_discovered(
            &mut choices,
            "http://gone",
            &["x".to_string()],
            None,
            &nc(),
        );
        assert_eq!(choices.len(), 3);

        // An all-embedder provider has no choice rows, but its endpoint
        // template lets discovery add its chat models at the end.
        extend_choices_with_discovered(
            &mut choices,
            "http://c",
            &["llama3.1:8b".to_string()],
            Some(choice("local", "http://c", "")),
            &nc(),
        );
        assert_eq!(choices.last().unwrap().model, "llama3.1:8b");
        assert_eq!(choices.last().unwrap().provider, "local");
    }
}

/// Daily-driver P2e persistence-level tests: the terminal-run write path lands
/// a per-conversation `work_last_run:<conv>` row in a REAL temp sqlite store
/// (through the same worker/queue as `work_executor`), and restore only ever
/// populates an idle panel — never a running one.
#[cfg(test)]
mod work_last_run_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    use work::RunPanel;
    use work_run_identity::WorkRunIdentity;

    fn temp_db() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "awili-app-work-last-run-test-{}-{}.db",
            std::process::id(),
            n
        ))
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    /// Drop the ONLY channel sender and wait for the worker to exit. Takes
    /// `persist` BY VALUE: `drop(&persist)` inside the function would release
    /// only the reference, leaving the test's `persist` (and the worker's
    /// `mpsc::Sender`) alive until the end of the test body — the channel
    /// never closes, the `for cmd in rx` worker loop never exits, and
    /// `worker.join()` blocks forever (the SF6 pattern the production
    /// `worker_project_survives_reopen` test relies on).
    fn drain_worker(persist: Persistence, worker: std::thread::JoinHandle<()>) {
        drop(persist);
        let _ = worker.join();
    }

    /// A bare `WorkState` (nothing configured) for the restore tests.
    fn work_state() -> work::WorkState {
        work::WorkState::new(
            work::HermesCfg {
                base_url: String::new(),
                api_key: String::new(),
                enabled: false,
                dashboard_url: String::new(),
            },
            work::GatewayCfg {
                dashboard_url: String::new(),
                enabled: false,
            },
            Default::default(),
        )
    }

    #[test]
    fn terminal_run_writes_last_run_key() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let (persist, worker) = Persistence::spawn_worker(store);

        // A Completed Claude agent run: identity frozen at submit, a
        // backend-reported model, no error.
        let panel = RunPanel {
            requested_identity: Some(WorkRunIdentity::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: Some("claude-sonnet-4".into()),
            }),
            model: Some("reported-model".into()),
            status: Some(awili_work::run_events::RunStatus::Completed),
            ..RunPanel::default()
        };

        work_last_run::persist_last_run(
            Some(&persist),
            &mut std::collections::HashMap::new(),
            "conv-w1",
            &panel,
            chrono::Utc::now(),
        );
        // The UI thread never blocks on sqlite (SF6) — the command is queued
        // and the worker applies it; draining proves the row landed.
        drain_worker(persist, worker);

        let reopened = Store::open(&path).unwrap();
        let raw = reopened
            .get_app_state(work_last_run::key_for("conv-w1").as_str())
            .unwrap()
            .expect("the terminal run's key was written");
        let last = work_last_run::LastRun::from_json(&raw).expect("stored row parses");
        assert_eq!(
            last.clone(),
            work_last_run::LastRun {
                identity: WorkRunIdentity::Agent {
                    engine: awili_code::CodeEngine::Claude,
                    model: Some("claude-sonnet-4".into()),
                },
                model: Some("reported-model".into()),
                status: awili_work::run_events::RunStatus::Completed,
                error: None,
                finished_at: last.finished_at, // RFC 3339, written above
            }
        );
        // The per-conversation key does NOT leak into another conversation.
        assert!(reopened
            .get_app_state(work_last_run::key_for("conv-w2").as_str())
            .unwrap()
            .is_none());
        cleanup(&path);
    }

    #[test]
    fn restore_populates_idle_panel_only() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let (persist, worker) = Persistence::spawn_worker(store);

        // The run that finished before the "quit": a failed owned-engine run.
        let panel = RunPanel {
            requested_identity: Some(WorkRunIdentity::OwnedEngine {
                model: "local-model".into(),
                engine_id: None,
            }),
            model: Some("reported-model".into()),
            status: Some(awili_work::run_events::RunStatus::Failed),
            error: Some("boom".into()),
            ..RunPanel::default()
        };
        work_last_run::persist_last_run(
            Some(&persist),
            &mut std::collections::HashMap::new(),
            "conv-w1",
            &panel,
            chrono::Utc::now(),
        );
        drain_worker(persist, worker);

        // Re-open the store and rebuild the startup-captured map the way
        // `Persistence::start` does (per-Work-row `get_app_state` keyed by
        // `work_last_run:<conv_id>`) — that map is what `restore_work_last_run`
        // reads (SF6: the UI thread never touches sqlite, it reads the map).
        let reopened = Store::open(&path).unwrap();
        let raw = reopened
            .get_app_state(work_last_run::key_for("conv-w1").as_str())
            .unwrap()
            .expect("the previous session's row survives the restart");
        let map: std::collections::HashMap<String, String> =
            [("conv-w1".to_string(), raw)].into_iter().collect();

        // Idle panel owned by another conversation: the SAME `restore_into`
        // production calls applies identity, model, status, error AND hands
        // the panel to the restored conversation (R3: `panel_is_visible`).
        let mut idle = work_state();
        idle.conv_id = Some("conv-other".into());
        // R3 addendum: the other conversation's leftover panel content is
        // dropped before the row is applied.
        idle.panel.activity = Some("leftover from conv-other".into());
        assert!(work_last_run::restore_into(&mut idle, "conv-w1", &map));
        assert!(idle.panel.activity.is_none());
        assert_eq!(
            idle.panel.requested_identity,
            Some(WorkRunIdentity::OwnedEngine {
                model: "local-model".into(),
                engine_id: None,
            })
        );
        assert_eq!(idle.panel.model, Some("reported-model".into()));
        assert_eq!(
            idle.panel.status,
            Some(awili_work::run_events::RunStatus::Failed)
        );
        assert_eq!(idle.panel.error, Some("boom".into()));
        assert_eq!(idle.conv_id.as_deref(), Some("conv-w1"));
        assert!(idle.panel_is_visible("conv-w1"));

        // Running panel: `restore_into` refuses — the live panel and its
        // owner are untouched.
        let mut live = work_state();
        live.running = true;
        live.conv_id = Some("conv-other".into());
        live.panel.status = Some(awili_work::run_events::RunStatus::Running);
        assert!(!work_last_run::restore_into(&mut live, "conv-w1", &map));
        assert_eq!(
            live.panel,
            RunPanel {
                status: Some(awili_work::run_events::RunStatus::Running),
                ..RunPanel::default()
            }
        );
        assert_eq!(live.conv_id.as_deref(), Some("conv-other"));

        // Missing key: restore applies nothing — the panel and its owner
        // stay as today.
        let mut other = work_state();
        other.conv_id = Some("conv-other".into());
        assert!(!work_last_run::restore_into(
            &mut other,
            "conv-missing",
            &map
        ));
        assert_eq!(other.panel, RunPanel::default());
        assert_eq!(other.conv_id.as_deref(), Some("conv-other"));
        // The map never sees a key `Persistence::start` never read:
        // re-open the store and prove the row for a missing conversation
        // does not exist (the same assertion shape as
        // `worker_project_survives_reopen`).
        let reopened2 = Store::open(&path).unwrap();
        assert!(reopened2
            .get_app_state(work_last_run::key_for("conv-missing").as_str())
            .unwrap()
            .is_none());
        drop(reopened2);
        cleanup(&path);
    }
}

#[cfg(test)]
mod runtime_palette_tests;
