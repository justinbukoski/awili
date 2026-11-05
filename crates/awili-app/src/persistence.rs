//! Off-thread, serialized persistence (review SF6). The SQLite [`Store`] lives
//! on a single worker thread; the GPUI thread only ever *sends* [`PersistCmd`]s
//! down a channel, so a filesystem stall, WAL checkpoint, or lock contention can
//! never pause the UI. Extracted from `main.rs` (the full-review "persistence
//! command handling" split) so the worker, the restore path, and the GPUI-free
//! usage/template helpers live behind one boundary. The wiring tests stay in
//! `main.rs` and reach `spawn_worker`/`apply_usage_event` through it.

use std::collections::HashMap;

use awili_memory::UsageNote;
use awili_persist::{PromptTemplate, Store, StoredProject};
use awili_providers::Usage;

use crate::cost::CostTicker;
use crate::feed;

/// The `app_state` key under which the selected provider·model is remembered.
pub(crate) const SELECTION_KEY: &str = "selection";
/// The `app_state` key remembering the last Work folder (polish slice A —
/// re-choosing it every launch was the top daily friction).
pub(crate) const WORK_FOLDER_KEY: &str = "work_folder";
/// Stable folder identity shared by both former project surfaces.
pub(crate) const WORKSHOP_ACTIVE_PROJECT_KEY: &str = "workshop_active_project_folder";
/// Dismissing a notice never marks a failed import complete. Changed original
/// bytes produce a new notice, and every launch still retries the import.
pub(crate) const PROJECT_IMPORT_DISMISSED_KEY: &str = "workshop_project_import_notice_dismissed";

/// Worker order accompanies each catalog snapshot: independent UI reply tasks
/// can finish out of order without restoring an older list or instructions.
#[derive(Debug)]
pub(crate) struct ProjectSnapshot {
    pub(crate) revision: u64,
    pub(crate) projects: Vec<StoredProject>,
}

/// Parse the complete legacy list before opening the import transaction. A bad
/// blob must not mark a partial import as complete or erase existing projects.
fn restore_workshop_projects(
    store: &Store,
) -> Result<(Vec<StoredProject>, Option<String>), String> {
    if store
        .get_app_state("workshop_projects_imported")
        .map_err(|e| e.to_string())?
        .is_none()
    {
        let raw = store
            .get_app_state(crate::work::CODE_PROJECTS_KEY)
            .map_err(|e| e.to_string())?;
        let legacy: Vec<crate::work::CodeProject> = match raw {
            Some(raw) => {
                serde_json::from_str(&raw).map_err(|e| format!("Legacy project list: {e}"))?
            }
            None => Vec::new(),
        };
        if legacy.iter().any(|p| p.folder.trim().is_empty()) {
            return Err("Legacy project list contains an empty folder".into());
        }
        let active = store
            .get_app_state(crate::work::CODE_ACTIVE_PROJECT_KEY)
            .map_err(|e| e.to_string())?
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let active_folder = legacy
            .get(active)
            .or_else(|| legacy.first())
            .map(|p| p.folder.as_str());
        let projects = legacy
            .iter()
            .map(|p| (p.name.clone(), p.folder.clone()))
            .collect::<Vec<_>>();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        store
            .import_legacy_projects_once(&projects, active_folder, ts)
            .map_err(|e| e.to_string())?;
    }
    Ok((
        store.list_projects().map_err(|e| e.to_string())?,
        store
            .get_app_state(WORKSHOP_ACTIVE_PROJECT_KEY)
            .map_err(|e| e.to_string())?,
    ))
}
/// The `app_state` key remembering the Work executor ("in-app", "gateway",
/// "agent:<engine>[:<model>]" or "local-build:<id>", see
/// `WorkExecutor::to_persist`).
pub(crate) const WORK_EXECUTOR_KEY: &str = "work_executor";
/// Separator between provider and model in the stored selection (an ASCII unit
/// separator — it cannot occur in a provider name or model id).
pub(crate) const SELECTION_SEP: char = '\u{1f}';
/// The `app_state` key remembering which conversation was active (slice 7).
pub(crate) const ACTIVE_CONVERSATION_KEY: &str = "active_conversation";
/// R2g: the `app_state` key remembering the last main mode ("chat", "work"
/// or "code"); a cold launch returns to it.
pub(crate) const LAST_MODE_KEY: &str = "last_mode";
/// The `app_state` key PREFIX for a Work conversation's last run (daily-driver
/// P2e): `work_last_run:<conv_id>` — the per-conversation row the panel's
/// requested identity and terminal status are restored from at activation.
pub(crate) const WORK_LAST_RUN_KEY_PREFIX: &str = "work_last_run:";

/// A command for the serialized persistence worker.
pub(crate) enum PersistCmd {
    SaveWorkshopTabs {
        raw: String,
        /// Present only for an explicit malformed-state recovery.
        recovery: Option<(String, String)>,
        reply: async_channel::Sender<Result<(), String>>,
    },
    /// Ordered readback after prior appends, used by the workspace diagnostic.
    ReadDiagnosticMessages {
        conv: String,
        reply: async_channel::Sender<Option<Vec<(String, String)>>>,
    },
    /// U1: read one `app_state` value in worker order (after every earlier write).
    #[allow(dead_code)]
    // U1d constructs it from `open_code_conversation`; until then the tests are
    // the only constructors (same convention as `Truncate` above).
    ReadState {
        key: String,
        reply: async_channel::Sender<Option<String>>,
    },
    /// Append a completed message to a conversation. `kind` is stored with
    /// it (RC2-RUNF: a plan-card click keeps its mark across a relaunch).
    Append {
        conv: String,
        role: &'static str,
        text: String,
        ts: i64,
        kind: awili_core::MessageKind,
    },
    /// Drop every message AFTER the first `keep` (in restore order) — slice
    /// 12's regenerate/edit-resend history rewrite.
    #[allow(dead_code)]
    // constructed only by the app tests
    Truncate { conv: String, keep: usize },
    /// Upsert a small key/value app-state entry.
    SetState { key: String, value: String },
    /// Create a conversation row (the UUID was generated on the UI side).
    CreateConversation {
        id: String,
        title: String,
        created_ts: i64,
        kind: String,
    },
    /// Set a conversation's title (`custom` = manual rename vs auto-title).
    Rename {
        id: String,
        title: String,
        custom: bool,
    },
    /// Flip the SOFT archive flag.
    SetArchived { id: String, archived: bool },
    /// HARD-delete a conversation and its dependent rows (user-test B2;
    /// always behind an explicit in-UI confirmation).
    DeleteConversation { id: String },
    /// Bump a conversation's `updated_ts`.
    Touch { id: String, updated_ts: i64 },
    /// RC2-CNCL6: a council seat (insert, or update its order, name, removal).
    CouncilMember(awili_persist::CouncilMemberRow),
    /// RC2-CNCL6: a member's answer (an assistant row of kind
    /// `council_answer` at `ts`) and its turn row, in one transaction.
    CouncilAnswer {
        turn: awili_persist::CouncilTurnRow,
        text: String,
        ts: i64,
    },
    /// RC2-CNCL6: a turn that wrote no answer (it failed, or was stopped
    /// before its first word) — kept so a reopened council shows it.
    CouncilTurn(awili_persist::CouncilTurnRow),
    /// RC2-CNCLF4: turns kept for a send the memory gate then refused.
    CouncilTurnsGone(Vec<String>),
    /// Record one turn's token usage (slice 8 §1).
    RecordUsage {
        cached: Option<u32>,
        conv: String,
        turn_ts: i64,
        provider: String,
        model: String,
        input: u32,
        output: u32,
    },
    /// Bind (or clear) a conversation's prompt template (slice 18 §4).
    SetConversationTemplate {
        conv: String,
        template_id: Option<i64>,
    },
    /// Create-or-update a prompt template by name (slice 18 §1). Replies with
    /// the refreshed template list so the UI's in-memory mirror (which the
    /// palette and send-time injection read synchronously) picks up the
    /// DB-assigned id of a new row.
    UpsertTemplate {
        name: String,
        content: String,
        reply: async_channel::Sender<Result<Vec<PromptTemplate>, String>>,
    },
    /// Delete a prompt template (slice 18 §2); replies like `UpsertTemplate`.
    DeleteTemplate {
        id: i64,
        reply: async_channel::Sender<Result<Vec<PromptTemplate>, String>>,
    },
    /// Set the working directory for a conversation (Phase 4 workspace groups).
    SetCwd { id: String, cwd: Option<String> },
    /// Set the profile for a conversation (Phase 4 profiles).
    SetProfile { id: String, profile: Option<String> },
    /// Reverse a summarize-earlier action atomically (slice 21 §D4 "Discard
    /// summary"): un-supersede every turn the summary at `summary_ts` replaced and
    /// delete the summary row. The durable inverse of the retired Summarize
    /// action, kept so a legacy summary can still be discarded; the
    /// in-memory half is applied on the UI thread before this is sent.
    DiscardSummary { conv: String, summary_ts: i64 },
    /// Persist a saved Work-mode project (a folder bound as a named project).
    /// Replies with the fresh project list carrying the DB-assigned ids, exactly
    /// like `UpsertTemplate` — the in-memory `work.projects` mirror is replaced
    /// from the reply so its ids never diverge from the rows.
    SaveProject {
        name: String,
        folder: String,
        instructions: String,
        ts: i64,
        reply: async_channel::Sender<Result<ProjectSnapshot, String>>,
    },
    /// Delete a saved Work-mode project by its DB row id. Replies with the fresh
    /// project list (post-delete) so the in-memory `work.projects` mirror is
    /// replaced from authoritative rows, exactly like `SaveProject` — the mirror
    /// and the rows never diverge.
    RemoveProject {
        project_id: i64,
        reply: async_channel::Sender<Result<ProjectSnapshot, String>>,
    },
    /// Edit a saved project's standing instructions (the per-project field folded
    /// ahead of the task in every Work run via `work::folder_instructions_prefix`).
    /// Replies with the fresh list — same authoritative-mirror contract as
    /// `SaveProject`/`RemoveProject` so `work.projects` and the rows never diverge.
    UpdateProjectInstructions {
        project_id: i64,
        instructions: String,
        reply: async_channel::Sender<Result<ProjectSnapshot, String>>,
    },
    /// Upsert the durable local Build job row for a turn (daily-driver P2d):
    /// the durable request identity is queued BEFORE any Running state is
    /// shown, and a replay of the same request id updates the row in place —
    /// never a second row (the persist layer enforces both).
    CommitBuildJob {
        row: awili_persist::build_jobs::BuildJobRow,
        reply: async_channel::Sender<Result<(), String>>,
    },
    UpsertBuildJob {
        turn_id: String,
        request_id: String,
        target_id: String,
        revision: u64,
        state: String,
        result_sha: Option<String>,
        /// The runner's failure text, or the app-local submit error for the
        /// `unknown` state (P10/F4) — never a synthesized answer.
        failure: Option<String>,
        updated_at: i64,
    },
}

/// Off-thread, serialized persistence (review SF6). The SQLite [`Store`] lives
/// entirely on a dedicated worker thread; the GPUI thread only ever *sends*
/// commands down a channel, so a filesystem stall, WAL checkpoint, or lock
/// contention can never pause the UI. Write failures are logged on the worker —
/// never by mutating a GPUI entity off-thread.
#[derive(Clone)]
pub(crate) struct Persistence {
    pub(crate) tx: std::sync::mpsc::Sender<PersistCmd>,
    /// Set the first time a `send` fails because the worker thread is gone (M7).
    /// A dead worker means queued writes (chat history, app state, usage) are
    /// silently lost; this flag plus a one-time log surface that instead of
    /// swallowing every `send` error with `let _ =`.
    pub(crate) failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) recovery: std::sync::Arc<std::sync::Mutex<Recovery>>,
}

#[derive(Default)]
pub(crate) struct Recovery {
    pub(crate) problem: Option<String>,
    pub(crate) messages: Vec<(String, &'static str, String, i64, String)>,
}

/// Why the app database could not be opened, in words for the person, when it
/// could not (Windows: a profile folder that cannot be made private, above all
/// a FAT or exFAT volume). Shown as a banner in the window, not only logged.
static STORAGE_PROBLEM: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Record why persistence is unavailable. macOS keeps its old behaviour
/// (stderr only): the banner is a Windows addition, so nothing is recorded
/// elsewhere.
pub(crate) fn set_storage_problem(message: String) {
    *STORAGE_PROBLEM.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
}

/// The recorded reason persistence is unavailable, if any.
pub(crate) fn storage_problem() -> Option<String> {
    STORAGE_PROBLEM
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// What [`Persistence::start`] hands back: the worker handle plus the state read
/// synchronously at startup (before the `Store` moved onto the worker thread).
pub(crate) struct PersistInit {
    pub(crate) handle: Persistence,
    /// Every conversation's metadata, archived included.
    pub(crate) conversations: Vec<awili_persist::StoredConversation>,
    /// Every conversation's full transcript, keyed by conversation id.
    pub(crate) messages: HashMap<String, Vec<awili_persist::StoredMessage>>,
    /// The previously-active conversation id, if one was saved.
    pub(crate) active: Option<String>,
    /// The previously-selected `(provider, model)`, if one was saved.
    pub(crate) selection: Option<(String, String)>,
    /// Today's (UTC) usage rolled up per provider·model, seeding the cost
    /// ticker's "today" total at launch (slice 8 §4).
    pub(crate) today_usage: Vec<awili_persist::UsageByModel>,
    /// Every saved prompt template, sorted by name (slice 18).
    pub(crate) templates: Vec<PromptTemplate>,
    /// Each conversation's bound template id, if any (slice 18 §4) — the
    /// in-memory mirror send-time injection reads synchronously.
    pub(crate) conv_templates: HashMap<String, i64>,
    /// Every saved Work-mode project (folder bound as a named project), newest
    /// first — the durable backing for `work.projects`, restored at launch.
    pub(crate) projects: Vec<StoredProject>,
    /// Restore failures are visible to the owner; existing catalog rows survive.
    pub(crate) project_import_error: Option<String>,
    /// The restored shared selection by stable folder identity.
    pub(crate) code_active_project: Option<String>,
    /// The last Work folder, if saved (validated by the app at restore).
    pub(crate) work_folder: Option<String>,
    /// The persisted Work executor, if saved (raw serialized form).
    pub(crate) work_executor: Option<String>,
    /// L1c: the one-time gateway → in-app migration ran on this launch.
    pub(crate) work_executor_notice: bool,
    /// The OPEN local Build job rows (active states only) — the set the app
    /// reattaches by `status` of each row's same request id at startup
    /// (daily-driver P2d). Terminal rows are excluded.
    pub(crate) open_build_jobs: Vec<awili_persist::build_jobs::BuildJobRow>,
    /// Every Work conversation's last run, raw JSON, keyed by conversation id
    /// (daily-driver P2e) — the startup-read map the app holds so the UI
    /// thread restores a panel's `Requested:` line and terminal status
    /// without touching sqlite (SF6). The map is captured here, before the
    /// `Store` moves onto the worker thread.
    pub(crate) work_last_run: HashMap<String, String>,
    /// RC2-RUN (UX-FOOTER): each Work conversation's saved reply footers,
    /// raw JSON (`work_footers:<conv_id>`).
    pub(crate) work_footers: HashMap<String, String>,
    /// RC2-R1F5: each Work conversation's folder moves, raw JSON
    /// (`work_folder_moves:<conv_id>`).
    pub(crate) work_folder_moves: HashMap<String, String>,
    /// R2g: the last main mode, raw ("chat", "work" or "code"), if saved.
    pub(crate) last_mode: Option<String>,
    /// RC2-UX (R2-15): the folders chosen lately, raw JSON (newest first).
    pub(crate) recent_folders: Option<String>,
    /// RC2-UX (R2-17): the open Code tabs, raw JSON, and the `code_conv` meta
    /// of each conversation they name (by conversation id).
    pub(crate) code_open_tabs: Option<String>,
    pub(crate) workshop_tabs: Result<Option<String>, String>,
    pub(crate) code_tab_metas: HashMap<String, String>,
    /// Versioned per-conversation M2 routes read before the Store moves to
    /// its writer thread. Raw values are retained so an unsupported saved
    /// target can stay explicit rather than falling back to a global helper.
    pub(crate) conversation_routes: HashMap<String, String>,
    pub(crate) legacy_general_models: Option<String>,
    /// RC2-UX (R5-9): each conversation's last rules/recall counts, raw JSON
    /// (`memory_chip:<conv_id>`).
    pub(crate) memory_chips: HashMap<String, String>,
    /// RC2-UX (D-R1 m6): each Work conversation's saved reply steps, raw JSON.
    pub(crate) work_steps: HashMap<String, String>,
    /// rc.3 S2: each Work conversation's saved context breakdowns, raw JSON.
    pub(crate) work_contexts: HashMap<String, String>,
    /// rc.3 S3: each conversation's saved run ids (any kind), raw JSON.
    pub(crate) run_ids: HashMap<String, String>,
}

impl Persistence {
    /// Open persistence, restore every conversation and the saved selection, then
    /// spawn the serialized writer thread. Returns `None` if the database could
    /// not be opened (the app still runs, just without persistence/restore).
    /// `configured_executor` is `[work].default_executor`, the target of the
    /// one-time legacy-gateway migration when it parses (L1c).
    pub(crate) fn start(configured_executor: Option<&str>) -> Option<PersistInit> {
        let store = Store::open_default()
            .map_err(|e| {
                eprintln!("awili: persistence unavailable ({e})");
                set_storage_problem(e.to_string());
            })
            .ok()?;

        // Reads happen here, synchronously, while we still own the Store on this
        // thread — before it moves to the worker.
        let conversations = store.conversations().unwrap_or_else(|e| {
            eprintln!("awili: restore failed ({e})");
            Vec::new()
        });
        let (messages, _unreadable) =
            restore_messages_reporting(&conversations, |id| store.messages(id));
        let active = store.get_app_state(ACTIVE_CONVERSATION_KEY).ok().flatten();
        let today_usage = store
            .daily_usage_by_model(feed::unix_now())
            .unwrap_or_else(|e| {
                eprintln!("awili: today's usage read failed ({e})");
                Vec::new()
            });
        let selection = store
            .get_app_state(SELECTION_KEY)
            .ok()
            .flatten()
            .and_then(|s| {
                s.split_once(SELECTION_SEP)
                    .map(|(p, m)| (p.to_string(), m.to_string()))
            });
        // Prompt templates and each conversation's binding (slice 18): read once
        // here, then mirrored in memory — applied/saved/deleted through the
        // worker, so the mirror and the rows never diverge.
        let templates = store.list_templates().unwrap_or_else(|e| {
            eprintln!("awili: template restore failed ({e})");
            Vec::new()
        });
        let mut conv_templates = HashMap::new();
        for c in &conversations {
            if let Ok(Some(t)) = store.get_conversation_template(&c.id) {
                conv_templates.insert(c.id.clone(), t.id);
            }
        }

        // Saved Work-mode projects (folder bound as a named project): the SQLite
        // `projects` table, restored into the `work.projects` mirror at launch
        // and written back through `PersistCmd::SaveProject`.
        let mut project_import_error = None;
        let (projects, code_active_project) =
            restore_workshop_projects(&store).unwrap_or_else(|e| {
                eprintln!("awili: shared project import failed ({e}); retaining saved projects");
                let signature = store
                    .get_app_state(crate::work::CODE_PROJECTS_KEY)
                    .ok()
                    .flatten()
                    .map(|raw| format!("legacy:{raw}"))
                    .unwrap_or_else(|| format!("read:{e}"));
                if store
                    .get_app_state(PROJECT_IMPORT_DISMISSED_KEY)
                    .ok()
                    .flatten()
                    .as_deref()
                    != Some(&signature)
                {
                    project_import_error = Some(signature);
                }
                (
                    store.list_projects().unwrap_or_else(|e| {
                        eprintln!("awili: project restore failed ({e})");
                        Vec::new()
                    }),
                    store
                        .get_app_state(WORKSHOP_ACTIVE_PROJECT_KEY)
                        .ok()
                        .flatten(),
                )
            });

        let work_folder = store.get_app_state(WORK_FOLDER_KEY).ok().flatten();
        let last_mode = store.get_app_state(LAST_MODE_KEY).ok().flatten();
        let recent_folders = store
            .get_app_state(crate::palette_targets::RECENT_FOLDERS_KEY)
            .ok()
            .flatten();
        let code_open_tabs = store
            .get_app_state(crate::code_reopen::CODE_OPEN_TABS_KEY)
            .ok()
            .flatten();
        let workshop_tabs = store
            .get_app_state(crate::workshop::TABS_KEY)
            .map_err(|error| error.to_string());
        let mut meta_ids = code_open_tabs
            .as_deref()
            .map(crate::code_reopen::saved_tab_conversations)
            .unwrap_or_default();
        if let Some(saved) = workshop_tabs
            .as_ref()
            .ok()
            .and_then(|raw| raw.as_deref())
            .and_then(|raw| crate::workshop::SavedWorkshop::parse(raw).ok())
        {
            meta_ids.extend(saved.agent_conversations());
        }
        // A Code conversation may be closed (and therefore absent from both
        // open-tab snapshots) while its `code_conv:<id>` metadata still owns
        // its harness, model, folder and resume identity. Read all such
        // metadata before startup supplies any default route.
        meta_ids.extend(
            conversations
                .iter()
                .filter(|conversation| conversation.kind == "code")
                .map(|conversation| conversation.id.clone()),
        );
        let code_tab_metas: HashMap<String, String> = meta_ids
            .into_iter()
            .filter_map(|id| {
                let meta = store
                    .get_app_state(&crate::code_history::code_conv_key(&id))
                    .ok()
                    .flatten()?;
                Some((id, meta))
            })
            .collect();
        let conversation_routes: HashMap<String, String> = conversations
            .iter()
            .filter_map(|conversation| {
                store
                    .get_app_state(&format!("conversation_route:{}", conversation.id))
                    .ok()
                    .flatten()
                    .map(|raw| (conversation.id.clone(), raw))
            })
            .collect();
        let legacy_general_models = store
            .get_app_state("workshop:general_models")
            .ok()
            .flatten();
        // L1c: the one-time legacy "gateway" → "in-app" migration (it never
        // blocks launch; see `local_first_migrate::migrate_store`).
        let migrated = crate::local_first_migrate::migrate_store(&store, configured_executor);
        // Open local Build jobs (P2d): the rows the app reattaches by `status`
        // of the SAME request id after a restart — a durable job must survive
        // closing Awili, and no fresh submit is ever made.
        let open_build_jobs = store.open_build_jobs().unwrap_or_else(|e| {
            eprintln!("awili: open build jobs read failed ({e})");
            Vec::new()
        });
        // Last Work runs per conversation (P2e): the raw `work_last_run:<conv_id>`
        // rows read once here so the UI thread restores them without touching
        // sqlite (SF6); the app updates its copy on every persist (R2).
        // Only Work-kind conversations can carry one — the per-row read is
        // bounded by the conversation count and happens BEFORE the Store moves
        // onto the worker.
        let mut work_last_run = std::collections::HashMap::new();
        let mut memory_chips = HashMap::new();
        let mut work_steps = HashMap::new();
        let mut work_contexts = HashMap::new();
        let mut run_ids = HashMap::new();
        let mut work_footers = std::collections::HashMap::new();
        let mut work_folder_moves = HashMap::new();
        for c in &conversations {
            if let Ok(Some(v)) = store.get_app_state(&crate::run_error::RunIds::key(&c.id)) {
                run_ids.insert(c.id.clone(), v);
            }
            if let Ok(Some(v)) = store.get_app_state(&crate::recall::memory_chip_key(&c.id)) {
                memory_chips.insert(c.id.clone(), v);
            }
            if c.kind == "work" {
                if let Ok(Some(v)) =
                    store.get_app_state(&format!("{WORK_LAST_RUN_KEY_PREFIX}{}", c.id))
                {
                    work_last_run.insert(c.id.clone(), v);
                }
                // RC2-RUN (UX-FOOTER): its saved reply footers.
                if let Ok(Some(v)) = store.get_app_state(&crate::run_footer::footers_key(&c.id)) {
                    work_footers.insert(c.id.clone(), v);
                }
                // RC2-R1F5: its folder moves.
                if let Ok(Some(v)) =
                    store.get_app_state(&crate::work_folder_moves::moves_key(&c.id))
                {
                    work_folder_moves.insert(c.id.clone(), v);
                }
                // RC2-UX (D-R1 m6): and their steps.
                if let Ok(Some(v)) = store.get_app_state(&crate::run_footer::steps_key(&c.id)) {
                    work_steps.insert(c.id.clone(), v);
                }
                if let Ok(Some(v)) =
                    store.get_app_state(&crate::context_breakdown::contexts_key(&c.id))
                {
                    work_contexts.insert(c.id.clone(), v);
                }
            }
        }

        let (handle, _worker) = Self::spawn_worker(store);
        Some(PersistInit {
            handle,
            conversations,
            messages,
            active,
            selection,
            today_usage,
            templates,
            conv_templates,
            projects,
            project_import_error,
            code_active_project,
            work_folder,
            work_executor: migrated.value,
            work_executor_notice: migrated.notice,
            open_build_jobs,
            work_last_run,
            work_footers,
            work_folder_moves,
            last_mode,
            recent_folders,
            code_open_tabs,
            workshop_tabs,
            code_tab_metas,
            conversation_routes,
            legacy_general_models,
            memory_chips,
            work_steps,
            work_contexts,
            run_ids,
        })
    }

    /// Spawn the serialized writer thread that owns `store` for its lifetime.
    /// Returns the send handle plus the worker's `JoinHandle` — `start` drops
    /// the latter (the worker outlives the app loop), the app-level tests join
    /// it to prove a failed write is logged-and-survived, never a panic.
    pub(crate) fn spawn_worker(mut store: Store) -> (Persistence, std::thread::JoinHandle<()>) {
        let (tx, rx) = std::sync::mpsc::channel::<PersistCmd>();
        let recovery = std::sync::Arc::new(std::sync::Mutex::new(Recovery::default()));
        let worker_recovery = recovery.clone();
        let worker = std::thread::spawn(move || {
            // The Store is owned here for the worker's lifetime; every write is
            // serialized through this single loop.
            let mut project_revision = 0;
            for cmd in rx {
                let result = match cmd {
                    PersistCmd::SaveWorkshopTabs {
                        raw,
                        recovery,
                        reply,
                    } => {
                        let result = match recovery {
                            Some((expected, backup)) => store.replace_app_state_with_backup(crate::workshop::TABS_KEY, &expected, &raw, &backup)
                                .map_err(|e| e.to_string()).and_then(|replaced| if replaced { Ok(()) } else { Err("Saved tabs changed before recovery. Reopen Awili to load them.".into()) }),
                            None => store.set_app_state(crate::workshop::TABS_KEY, &raw).map_err(|e| e.to_string()),
                        };
                        let _ = reply.send_blocking(result);
                        Ok(())
                    }
                    PersistCmd::ReadDiagnosticMessages { conv, reply } => {
                        let rows = store
                            .messages(&conv)
                            .ok()
                            .map(|rows| rows.into_iter().map(|m| (m.role, m.text)).collect());
                        let _ = reply.send_blocking(rows);
                        Ok(())
                    }
                    PersistCmd::ReadState { key, reply } => {
                        let _ = reply.send_blocking(store.get_app_state(&key).ok().flatten());
                        Ok(())
                    }
                    PersistCmd::Append {
                        conv,
                        role,
                        text,
                        ts,
                        kind,
                    } => {
                        let result = store
                            .append_message_kind(&conv, role, &text, ts, kind.as_persist_str())
                            .map(|_| ());
                        if let Err(error) = &result {
                            worker_recovery
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .messages
                                .push((
                                    conv.clone(),
                                    role,
                                    text.clone(),
                                    ts,
                                    kind.as_persist_str().to_string(),
                                ));
                            worker_recovery.lock().unwrap_or_else(|e| e.into_inner()).problem = Some(format!("A message could not be saved ({error}). Copy unsaved messages before closing Awili."));
                        }
                        result
                    }
                    PersistCmd::Truncate { conv, keep } => {
                        store.truncate_messages(&conv, keep).map(|_| ())
                    }
                    PersistCmd::SetState { key, value } => store.set_app_state(&key, &value),
                    PersistCmd::CreateConversation {
                        id,
                        title,
                        created_ts,
                        kind,
                    } => {
                        // RC2-APP (F0189): the row and its kind in one write.
                        store.create_conversation_with_kind(&id, &title, created_ts, &kind)
                    }
                    PersistCmd::Rename { id, title, custom } => {
                        store.rename_conversation(&id, &title, custom)
                    }
                    PersistCmd::SetArchived { id, archived } => store.set_archived(&id, archived),
                    PersistCmd::DeleteConversation { id } => {
                        // RC2-APP: the conversation's own app-state rows (its
                        // last Work run, its saved reply footers) go with it.
                        store
                            .delete_conversation(&id)
                            .and_then(|()| store.delete_app_states(&conversation_state_keys(&id)))
                            // RC2-UX (R5-9): and its rules/recall counts.
                            .and_then(|()| {
                                store.delete_app_states(&[
                                    crate::recall::memory_chip_key(&id),
                                    // RC2-UX (D-R1 m6): and its saved reply steps.
                                    crate::run_footer::steps_key(&id),
                                    crate::context_breakdown::contexts_key(&id),
                                    crate::run_error::RunIds::key(&id),
                                    // RC2-R1F5: and its folder moves.
                                    crate::work_folder_moves::moves_key(&id),
                                ])
                            })
                    }
                    PersistCmd::Touch { id, updated_ts } => {
                        store.touch_conversation(&id, updated_ts)
                    }
                    PersistCmd::RecordUsage {
                        cached,
                        conv,
                        turn_ts,
                        provider,
                        model,
                        input,
                        output,
                    } => store.record_usage_with_cache(
                        &conv, turn_ts, &provider, &model, input, output, cached,
                    ),
                    PersistCmd::CouncilMember(m) => store.put_council_member(&m),
                    PersistCmd::CouncilAnswer { turn, text, ts } => {
                        store.put_council_answer(&turn, &text, ts).map(|_| ())
                    }
                    PersistCmd::CouncilTurn(t) => store.put_council_turn(&t),
                    PersistCmd::CouncilTurnsGone(ids) => store.delete_council_turns(&ids),
                    PersistCmd::SetConversationTemplate { conv, template_id } => {
                        store.set_conversation_template(&conv, template_id)
                    }
                    PersistCmd::UpsertTemplate {
                        name,
                        content,
                        reply,
                    } => {
                        let result = store.upsert_template(&name, &content).map(|_| ());
                        // RC2-APP (F0188): a failed write replies with its
                        // error, never with a list read that happened to work.
                        let _ = reply
                            .send_blocking(reply_after_write(&result, || store.list_templates()));
                        result
                    }
                    PersistCmd::DeleteTemplate { id, reply } => {
                        let result = store.delete_template(id);
                        let _ = reply
                            .send_blocking(reply_after_write(&result, || store.list_templates()));
                        result
                    }
                    PersistCmd::SetCwd { id, cwd } => {
                        store.set_conversation_cwd(&id, cwd.as_deref())
                    }
                    PersistCmd::SetProfile { id, profile } => {
                        store.set_conversation_profile(&id, profile.as_deref())
                    }
                    PersistCmd::DiscardSummary { conv, summary_ts } => {
                        store.discard_summary(&conv, summary_ts).map(|_| ())
                    }
                    PersistCmd::SaveProject {
                        name,
                        folder,
                        instructions,
                        ts,
                        reply,
                    } => {
                        let result = store
                            .add_project(&name, &folder, &instructions, ts)
                            .map(|_| ());
                        // Only report the fresh list when the insert itself
                        // succeeded — otherwise reply with the genuine error so
                        // the UI shows the save failed (never a false success
                        // from a read that happened to work). A duplicate folder
                        // is not an error: `add_project` is `INSERT OR IGNORE`.
                        let reply_payload = match &result {
                            Ok(()) => {
                                project_revision += 1;
                                store
                                    .list_projects()
                                    .map(|projects| ProjectSnapshot {
                                        revision: project_revision,
                                        projects,
                                    })
                                    .map_err(|e| e.to_string())
                            }
                            Err(e) => Err(e.to_string()),
                        };
                        let _ = reply.send_blocking(reply_payload);
                        result
                    }
                    PersistCmd::RemoveProject { project_id, reply } => {
                        let result = store.remove_project(project_id);
                        // Same contract as SaveProject: only report the fresh
                        // list when the delete itself succeeded, otherwise reply
                        // with the genuine error (never a false success from a
                        // read that happened to work).
                        let reply_payload = match &result {
                            Ok(()) => {
                                project_revision += 1;
                                store
                                    .list_projects()
                                    .map(|projects| ProjectSnapshot {
                                        revision: project_revision,
                                        projects,
                                    })
                                    .map_err(|e| e.to_string())
                            }
                            Err(e) => Err(e.to_string()),
                        };
                        let _ = reply.send_blocking(reply_payload);
                        result
                    }
                    PersistCmd::UpdateProjectInstructions {
                        project_id,
                        instructions,
                        reply,
                    } => {
                        let result = store.update_project_instructions(project_id, &instructions);
                        // Stricter than SaveProject/RemoveProject's contract: an
                        // edit that hits ZERO rows (the project was deleted out
                        // from under the open editor) is a genuine failure, not a
                        // success — the user's typed instructions went nowhere, so
                        // the UI must say so rather than toast "saved". Only an
                        // update that actually changed a row replies with the
                        // fresh list (and never a false success from a read that
                        // happened to work after a write error).
                        let reply_payload = match &result {
                            Ok(0) => Err("project no longer exists".to_string()),
                            Ok(_) => {
                                project_revision += 1;
                                store
                                    .list_projects()
                                    .map(|projects| ProjectSnapshot {
                                        revision: project_revision,
                                        projects,
                                    })
                                    .map_err(|e| e.to_string())
                            }
                            Err(e) => Err(e.to_string()),
                        };
                        let _ = reply.send_blocking(reply_payload);
                        // A zero-row update is not a DB error — don't log it as a
                        // write failure; the reply already carried the miss.
                        result.map(|_| ())
                    }
                    PersistCmd::CommitBuildJob { row, reply } => {
                        let result = store.upsert_build_job(&row).map_err(|e| e.to_string());
                        let _ = reply.send_blocking(result);
                        Ok(())
                    }
                    PersistCmd::UpsertBuildJob {
                        turn_id,
                        request_id,
                        target_id,
                        revision,
                        state,
                        result_sha,
                        failure,
                        updated_at,
                    } => store.upsert_build_job(&awili_persist::build_jobs::BuildJobRow {
                        turn_id,
                        request_id,
                        target_id,
                        revision,
                        state,
                        result_sha,
                        failure,
                        updated_at,
                    }),
                };
                if let Err(e) = result {
                    eprintln!("awili: persist write failed: {e}");
                }
            }
        });
        (
            Persistence {
                tx,
                failed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                recovery,
            },
            worker,
        )
    }

    pub(crate) fn storage_problem(&self) -> Option<String> {
        self.recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .problem
            .clone()
    }

    pub(crate) fn has_unsaved_messages(&self) -> bool {
        !self
            .recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .messages
            .is_empty()
    }

    pub(crate) fn unsaved_messages_recovery(&self) -> String {
        self.recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .messages
            .iter()
            .map(|(conv, role, text, ts, kind)| {
                format!("Conversation: {conv}\nRole: {role}\nTime: {ts}\nKind: {kind}\n\n{text}\n")
            })
            .collect::<Vec<_>>()
            .join("\n---\n\n")
    }

    /// Queue a message append (non-blocking; the worker commits it).
    pub(crate) fn append(&self, conv: &str, role: &'static str, text: String, ts: i64) {
        self.append_kind(conv, role, text, ts, awili_core::MessageKind::Normal);
    }

    /// [`Self::append`] with the message's kind (RC2-RUNF: a plan-card
    /// click is stored with its mark).
    pub(crate) fn append_kind(
        &self,
        conv: &str,
        role: &'static str,
        text: String,
        ts: i64,
        kind: awili_core::MessageKind,
    ) {
        self.dispatch(PersistCmd::Append {
            conv: conv.to_string(),
            role,
            text,
            ts,
            kind,
        });
    }

    /// Queue an app-state upsert (non-blocking).
    pub(crate) fn set_state(&self, key: &str, value: String) {
        self.dispatch(PersistCmd::SetState {
            key: key.to_string(),
            value,
        });
    }

    pub(crate) fn send(&self, cmd: PersistCmd) {
        self.dispatch(cmd);
    }

    /// The single send path. A failed `send` means the worker thread is gone and
    /// the write is lost; record it (and log once) instead of silently dropping
    /// it with `let _ =` (M7). Still infallible for callers — a dead worker must
    /// never panic the UI thread.
    fn dispatch(&self, cmd: PersistCmd) {
        if let Err(error) = self.tx.send(cmd) {
            if let PersistCmd::Append {
                conv,
                role,
                text,
                ts,
                kind,
            } = error.0
            {
                self.recovery
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .messages
                    .push((conv, role, text, ts, kind.as_persist_str().to_string()));
            }
            if !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                self.recovery.lock().unwrap_or_else(|e| e.into_inner()).problem = Some("Storage worker stopped. Your conversation is only in memory; copy unsaved messages before closing Awili.".into());
                eprintln!(
                    "awili: persistence worker is gone — queued writes are no longer durable"
                );
            }
        }
    }
}

/// RC2-APP: the `app_state` rows that belong to one conversation and go when
/// it is deleted — its last Work run and its saved reply footers.
pub(crate) fn conversation_state_keys(id: &str) -> Vec<String> {
    vec![
        crate::work_last_run::key_for(id),
        crate::run_footer::footers_key(id),
    ]
}

/// RC2-APP (F0188): the reply to a write that answers with a fresh list —
/// the list only when the write itself succeeded, otherwise the write's own
/// error (never a false success from a read that happened to work). The same
/// contract `SaveProject`/`RemoveProject` keep.
pub(crate) fn reply_after_write<T, E: std::fmt::Display>(
    write: &Result<(), E>,
    list: impl FnOnce() -> Result<T, E>,
) -> Result<T, String> {
    match write {
        Ok(()) => list().map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// RC2-APP (F0190): read every conversation's messages at startup. A read
/// that fails is reported (stderr, naming the conversation) and that
/// conversation opens empty; nothing on disk is touched. Returns the
/// messages and the ids that could not be read.
pub(crate) fn restore_messages_reporting<M, E: std::fmt::Display>(
    conversations: &[awili_persist::StoredConversation],
    mut read: impl FnMut(&str) -> Result<Vec<M>, E>,
) -> (HashMap<String, Vec<M>>, Vec<String>) {
    let mut messages = HashMap::new();
    let mut unreadable = Vec::new();
    for c in conversations {
        match read(&c.id) {
            Ok(rows) => {
                messages.insert(c.id.clone(), rows);
            }
            Err(e) => {
                eprintln!(
                    "awili: could not read the messages of conversation {:?} ({}): {e}; \
                     it opens empty and nothing was deleted",
                    c.title, c.id
                );
                messages.insert(c.id.clone(), Vec::new());
                unreadable.push(c.id.clone());
            }
        }
    }
    (messages, unreadable)
}

/// Decide the template list to display after a worker reply (M6). On success
/// use the refreshed list; on a DB read failure KEEP the current list (never
/// wipe the palette to empty) and return the error string to surface. Pure, so
/// the keep-on-error behavior is unit-testable without a window.
pub(crate) fn merge_template_reply(
    current: Vec<PromptTemplate>,
    reply: Result<Vec<PromptTemplate>, String>,
) -> (Vec<PromptTemplate>, Option<String>) {
    match reply {
        Ok(templates) => (templates, None),
        Err(e) => (current, Some(e)),
    }
}

/// The GPUI-free core of [`AwiliApp::on_usage`] (review SF6): tick the cost
/// counters, queue the durable `usage_events` row, and build the memory note
/// when the turn cost money and still belongs to the current session.
/// Extracted so the app-level tests can drive a real `ProviderEvent::Usage`
/// payload through the exact production wiring without a window.
///
/// P9 (D.18): its only production caller (`on_usage`, the Chat HTTP stream
/// loop) no longer runs — Chat is pinned to an agent executor. Retained per
/// the spec; exercised by its unit tests.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_usage_event(
    cost: &mut CostTicker,
    persist: Option<&Persistence>,
    conv_id: &str,
    provider: &str,
    model: &str,
    usage: Usage,
    now: i64,
    session_gen: u64,
) -> Option<UsageNote> {
    let stale_session = session_gen != cost.generation();
    let event_cost = cost.note_usage(provider, model, usage, now, session_gen);
    // TODO(slice-8 review SF3): the ticker above advances BEFORE the write
    // below commits, and the send is fire-and-forget — there is no result path
    // back from the worker. If the insert fails (FK violation, disk full, lock
    // error, dead worker), the chip shows spend that `usage_events` never
    // recorded, and the next launch re-seeds a lower "today" total. Until the
    // persistence command gains an acknowledgement channel and the daily total
    // is confirmed/refreshed only after the commit, the displayed total is a
    // CEILING on what was durably recorded, not a floor. The app must tolerate
    // a failed write gracefully (logged on the worker, never a panic) — the
    // app-level tests pin that down.
    if let Some(p) = persist {
        p.send(PersistCmd::RecordUsage {
            cached: usage.cached_input_tokens,
            conv: conv_id.to_string(),
            turn_ts: now,
            provider: provider.to_string(),
            model: model.to_string(),
            input: usage.input_tokens,
            output: usage.output_tokens,
        });
    }
    // A PAID turn stages a memory note (§5) — unless its session ended while
    // it was in flight (SF1): the note's running session cost would then be
    // attributed to a session the turn was never part of.
    (event_cost > 0.0 && !stale_session).then(|| UsageNote {
        session_cost_usd: cost.session_usd(),
        provider: provider.to_string(),
        model: model.to_string(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
    })
}

#[cfg(test)]
mod u1b_tests {
    use super::*;

    /// U1b: a `ReadState` issued after a `SetState` for the same key replies
    /// with that value — worker order means the read sees the earlier write.
    #[test]
    fn u1b_read_state_sees_prior_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = awili_persist::Store::open(&dir.path().join("u1b.db")).unwrap();
        let (p, worker) = Persistence::spawn_worker(store);
        p.send(PersistCmd::SetState {
            key: "code_conv:x".into(),
            value: "{}".into(),
        });
        let (tx, rx) = async_channel::bounded(1);
        p.send(PersistCmd::ReadState {
            key: "code_conv:x".into(),
            reply: tx,
        });
        assert_eq!(rx.recv_blocking().unwrap(), Some("{}".into()));
        drop(p);
        worker.join().unwrap();
    }

    /// U1b: a `ReadState` for a key that was never written replies `None`.
    #[test]
    fn u1b_read_state_missing_key_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = awili_persist::Store::open(&dir.path().join("u1b-missing.db")).unwrap();
        let (p, worker) = Persistence::spawn_worker(store);
        let (tx, rx) = async_channel::bounded(1);
        p.send(PersistCmd::ReadState {
            key: "code_conv:never".into(),
            reply: tx,
        });
        assert_eq!(rx.recv_blocking().unwrap(), None);
        drop(p);
        worker.join().unwrap();
    }
}

#[cfg(test)]
mod workshop_project_tests {
    use super::*;

    #[test]
    fn malformed_legacy_projects_do_not_mark_or_partially_import() {
        for raw in [
            "broken json",
            r#"[{"name":"ok","folder":"/work/ok"},{"name":"bad","folder":""}]"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(&dir.path().join("projects.db")).unwrap();
            store
                .add_project("Existing", "/work/existing", "Keep rules", 10)
                .unwrap();
            store
                .set_app_state(crate::work::CODE_PROJECTS_KEY, raw)
                .unwrap();
            assert!(restore_workshop_projects(&store).is_err());
            assert_eq!(store.list_projects().unwrap().len(), 1);
            assert_eq!(store.list_projects().unwrap()[0].instructions, "Keep rules");
            assert_eq!(
                store.get_app_state("workshop_projects_imported").unwrap(),
                None
            );
            assert_eq!(
                store
                    .get_app_state(crate::work::CODE_PROJECTS_KEY)
                    .unwrap()
                    .as_deref(),
                Some(raw)
            );
        }
    }

    #[test]
    fn legacy_active_index_restores_folder_despite_canonical_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("projects.db")).unwrap();
        store
            .add_project("Existing", "/work/existing", "Keep rules", i64::MAX)
            .unwrap();
        store
            .add_project(
                "Chosen before import",
                "/work/chosen",
                "Keep chosen rules",
                10,
            )
            .unwrap();
        store.set_app_state(crate::work::CODE_PROJECTS_KEY,
            r#"[{"name":"First","folder":"/work/first"},{"name":"Chosen","folder":"/work/chosen","autonomous":true}]"#).unwrap();
        store
            .set_app_state(crate::work::CODE_ACTIVE_PROJECT_KEY, "1")
            .unwrap();
        let (projects, folder) = restore_workshop_projects(&store).unwrap();
        assert_eq!(projects.len(), 3);
        assert_eq!(projects[0].folder, "/work/existing");
        assert_eq!(folder.as_deref(), Some("/work/chosen"));
        assert!(projects.iter().any(|p| p.folder == "/work/chosen"));
        let selected = projects
            .iter()
            .position(|p| Some(p.folder.as_str()) == folder.as_deref())
            .unwrap();
        assert_ne!(
            selected, 1,
            "legacy index must not select a different folder"
        );
    }
}

#[cfg(test)]
mod rc2app_tests {
    use super::*;

    #[test]
    fn m2_route_and_legacy_target_rows_round_trip_through_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = awili_persist::Store::open(&dir.path().join("m2-route.db")).unwrap();
        let route =
            crate::conversation_route::ConversationRoute::new(crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: Some("claude-haiku".into()),
            });
        let raw = route.to_persisted_json().unwrap();
        store
            .set_app_state("conversation_route:stable-conversation", &raw)
            .unwrap();
        store
            .set_app_state(
                "workshop:general_models",
                r#"{"legacy-conversation":"agent:claude:claude-haiku"}"#,
            )
            .unwrap();
        let restored = store
            .get_app_state("conversation_route:stable-conversation")
            .unwrap()
            .unwrap();
        assert_eq!(
            crate::conversation_route::ConversationRoute::from_persisted_json(&restored)
                .unwrap()
                .next_target(),
            route.next_target()
        );
        let legacy = store
            .get_app_state("workshop:general_models")
            .unwrap()
            .unwrap();
        let targets: std::collections::BTreeMap<String, String> =
            serde_json::from_str(&legacy).unwrap();
        assert_eq!(
            targets.get("legacy-conversation").map(String::as_str),
            Some("agent:claude:claude-haiku")
        );
        assert!(
            crate::conversation_route::ConversationRoute::from_persisted_json(
                r#"{"version":99,"next_target":"in-app","native_sessions":[]}"#
            )
            .is_err()
        );
    }

    fn read_state(p: &Persistence, key: &str) -> Option<String> {
        let (tx, rx) = async_channel::bounded(1);
        p.send(PersistCmd::ReadState {
            key: key.to_string(),
            reply: tx,
        });
        rx.recv_blocking().unwrap()
    }

    /// RC2-APP: deleting a conversation removes its own `work_last_run:` and
    /// `work_footers:` rows; another conversation's rows stay.
    #[test]
    fn rc2app_deleting_a_conversation_removes_its_footer_and_last_run_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = awili_persist::Store::open(&dir.path().join("rc2app.db")).unwrap();
        let (p, worker) = Persistence::spawn_worker(store);
        for id in ["gone", "kept"] {
            p.send(PersistCmd::CreateConversation {
                id: id.into(),
                title: id.into(),
                created_ts: 1,
                kind: "work".into(),
            });
            for key in conversation_state_keys(id) {
                p.send(PersistCmd::SetState {
                    key,
                    value: "{}".into(),
                });
            }
        }
        p.send(PersistCmd::DeleteConversation { id: "gone".into() });
        for key in conversation_state_keys("gone") {
            assert_eq!(read_state(&p, &key), None, "{key} is removed");
        }
        for key in conversation_state_keys("kept") {
            assert_eq!(read_state(&p, &key).as_deref(), Some("{}"), "{key} stays");
        }
        assert_eq!(
            conversation_state_keys("c"),
            ["work_last_run:c", "work_footers:c"]
        );
        drop(p);
        worker.join().unwrap();
    }

    /// rc.3 S2: a deleted conversation takes its saved context breakdowns (and
    /// the other per-conversation rows deleted beside them) with it.
    #[test]
    fn rc3s2_deleting_a_conversation_removes_its_saved_contexts() {
        let dir = tempfile::tempdir().unwrap();
        let store = awili_persist::Store::open(&dir.path().join("s2.db")).unwrap();
        let (p, worker) = Persistence::spawn_worker(store);
        let extra = |id: &str| {
            vec![
                crate::context_breakdown::contexts_key(id),
                crate::run_error::RunIds::key(id),
                crate::run_footer::steps_key(id),
                crate::work_folder_moves::moves_key(id),
                crate::recall::memory_chip_key(id),
            ]
        };
        for id in ["gone", "kept"] {
            p.send(PersistCmd::CreateConversation {
                id: id.into(),
                title: id.into(),
                created_ts: 1,
                kind: "work".into(),
            });
            for key in extra(id) {
                p.send(PersistCmd::SetState {
                    key,
                    value: "{}".into(),
                });
            }
        }
        p.send(PersistCmd::DeleteConversation { id: "gone".into() });
        for key in extra("gone") {
            assert_eq!(read_state(&p, &key), None, "{key} is removed");
        }
        for key in extra("kept") {
            assert_eq!(read_state(&p, &key).as_deref(), Some("{}"), "{key} stays");
        }
        assert_eq!(
            crate::context_breakdown::contexts_key("c"),
            "work_contexts:c"
        );
        drop(p);
        worker.join().unwrap();
    }

    /// RC2-APP (F0188): a failed template write answers with its own error,
    /// even when the list read after it would succeed.
    #[test]
    fn rc2app_a_failed_write_replies_with_its_error_not_a_list() {
        let failed: Result<(), String> = Err("disk full".into());
        let mut listed = false;
        let reply = reply_after_write(&failed, || {
            listed = true;
            Ok::<_, String>(vec![1])
        });
        assert_eq!(reply, Err("disk full".to_string()));
        assert!(!listed, "no list is read after a failed write");
        let ok: Result<(), String> = Ok(());
        assert_eq!(
            reply_after_write(&ok, || Ok::<_, String>(vec![1])),
            Ok(vec![1])
        );
        assert_eq!(
            reply_after_write(&ok, || Err::<Vec<i32>, _>("read failed".to_string())),
            Err("read failed".to_string())
        );
    }

    /// RC2-APP (F0190): a transcript that cannot be read is reported by id
    /// and opens empty; the others restore. RC2-FIXA4 (RC2-RUNAPP review
    /// P3): the report itself is checked — the test runs again in a child
    /// process whose stderr must name the conversation's title and id.
    #[test]
    fn rc2app_an_unreadable_transcript_is_reported() {
        const CHILD: &str = "AWILI_RC2FIXA_UNREADABLE_CHILD";
        const TEST: &str = "persistence::rc2app_tests::rc2app_an_unreadable_transcript_is_reported";
        if std::env::var_os(CHILD).is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success() && stdout.contains("1 passed"),
                "child failed:\n{stdout}\n{stderr}"
            );
            let report: Vec<&str> = stderr
                .lines()
                .filter(|l| l.starts_with("awili: could not read the messages of conversation"))
                .collect();
            assert_eq!(
                report.len(),
                1,
                "one report, for the unreadable one: {stderr}"
            );
            for part in [
                "\"Garden plans\"",
                "(b-0002)",
                "database disk image is malformed",
            ] {
                assert!(report[0].contains(part), "{part} in {}", report[0]);
            }
            assert!(
                report[0].contains("it opens empty and nothing was deleted"),
                "{}",
                report[0]
            );
            return;
        }
        let conv = |id: &str| awili_persist::StoredConversation {
            id: id.into(),
            title: if id == "b-0002" {
                "Garden plans".into()
            } else {
                id.into()
            },
            created_ts: 1,
            updated_ts: 1,
            archived: false,
            title_is_custom: false,
            cwd: None,
            profile: None,
            kind: "chat".into(),
        };
        let convs = [conv("a"), conv("b-0002")];
        let (messages, unreadable) = restore_messages_reporting(&convs, |id| {
            if id == "b-0002" {
                Err("database disk image is malformed".to_string())
            } else {
                Ok(vec![id.to_string()])
            }
        });
        assert_eq!(unreadable, ["b-0002"]);
        assert_eq!(messages["a"], ["a"]);
        assert!(messages["b-0002"].is_empty());
    }
}

#[cfg(test)]
mod storage_problem_tests {
    use super::*;

    /// The banner reason is recorded on Windows only; macOS keeps stderr-only.
    #[test]
    fn the_storage_problem_is_recorded_on_windows_only() {
        let before = storage_problem();
        set_storage_problem(
            "Awili needs an NTFS profile folder to keep your keys private".to_string(),
        );
        assert!(storage_problem().is_some());
        *STORAGE_PROBLEM.lock().unwrap() = before;
    }
}

#[cfg(test)]
mod unsaved_recovery_tests {
    use super::*;

    #[test]
    fn rejected_sql_append_is_retained_for_copy() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("failure.db")).unwrap();
        let (p, worker) = Persistence::spawn_worker(store);
        p.append(
            "missing-conversation-recovery-test",
            "user",
            "recover-this-message".into(),
            42,
        );
        let (reply, ack) = async_channel::bounded(1);
        p.send(PersistCmd::ReadState {
            key: "barrier".into(),
            reply,
        });
        ack.recv_blocking().unwrap();
        let recovery = p.unsaved_messages_recovery();
        assert!(recovery.contains("missing-conversation-recovery-test"));
        assert!(recovery.contains("recover-this-message"));
        assert!(p.storage_problem().is_some());
        drop(p);
        worker.join().unwrap();
    }
}

#[cfg(test)]
mod build_commit_regression {
    use super::*;
    #[test]
    fn commit_ack_reports_durable_row_and_real_sql_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build.db");
        let (p, worker) = Persistence::spawn_worker(Store::open(&path).unwrap());
        let row = awili_persist::build_jobs::BuildJobRow {
            turn_id: "turn".into(),
            request_id: "request".into(),
            target_id: "target".into(),
            revision: 0,
            state: "submitting".into(),
            result_sha: None,
            failure: None,
            updated_at: 1,
        };
        let (reply, ack) = async_channel::bounded(1);
        p.send(PersistCmd::CommitBuildJob {
            row: row.clone(),
            reply,
        });
        assert!(ack.recv_blocking().unwrap().is_ok());
        assert_eq!(
            Store::open(&path)
                .unwrap()
                .build_job_for_turn("turn")
                .unwrap(),
            Some(row.clone())
        );
        let (reply, ack) = async_channel::bounded(1);
        let mut bad = row.clone();
        bad.turn_id = "other-turn".into(); // duplicate request ID violates the unique index
        p.send(PersistCmd::CommitBuildJob { row: bad, reply });
        assert!(ack.recv_blocking().unwrap().is_err());
        assert!(Store::open(&path)
            .unwrap()
            .build_job_for_turn("other-turn")
            .unwrap()
            .is_none());
        drop(p);
        worker.join().unwrap();
    }
}
