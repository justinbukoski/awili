//! Work mode — the agentic "Cowork" surface.
//!
//! **Gateway WebSocket** is the sole transport: `awili-gateway` connects to the
//! Hermes dashboard's `/api/ws`, creates a session with `cwd`, and streams
//! real-time events. This is the path Nous's own desktop app uses — it supports
//! folder scoping, approvals, and the full session lifecycle. Unit 16 of the
//! strangler migration removed the old `awili-hermes` `/v1/runs` + SSE REST
//! fallback, so a Work run now requires `[gateway]` to be configured.
//!
//! The event→UI reducer ([`RunPanel::apply_event`]) is pure; the gateway worker
//! emits [`WorkUpdate`] variants that drive it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use awili_code::CodeEngine;
use awili_core::ConversationState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gpui::{div, prelude::*, Context, FontWeight, PathPromptOptions, ScrollHandle, SharedString};

use awili_core::{ConvKind, Role};
use awili_gateway::events::{
    decode_event, is_turn_end, ApprovalRisk, GatewayEventKind, SubagentPhase, TurnState,
};
use awili_gateway::rest::{HermesSession, RestClient, ScheduledJob};
use awili_gateway::run_events::{ApprovalDecision, RunEvent, RunStatus};
use awili_gateway::subagents::{
    build_subagent_tree, upsert_subagent, SubagentNode, SubagentProgress,
};
use awili_gateway::{gateway_url, GatewayClient, GatewayEvent};
use awili_persist::{Attachment, StoredProject};
use awili_work::{EngineWorker, SubagentUpdate, WorkEvent, WorkerContext};

use crate::ask_mode::AskKey;

use crate::messaging_selftest::{SelfTest, SelfTestFailure};
use crate::work_run_identity::WorkRunIdentity;
use crate::{cron, messaging_pane, theme, AwiliApp};

#[path = "inapp_files.rs"]
pub(crate) mod inapp_files;

#[path = "text_clip.rs"]
pub(crate) mod text_clip;

#[path = "inapp_history.rs"]
pub(crate) mod inapp_history;

/// Which surface is active. The Chat|Work toggle in the composer row flips this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Workshop's conversation surface: one tool-capable path per thread.
    Work,
    Code,
    /// RC2-CNCL5: one message to several models at once, answers side by
    /// side (the Council page).
    Council,
    /// Reserved for visual design work; backing arrives in a later release.
    Design,
    Skills,
    Cron,
    Sessions,
    /// The hosted Command Center: live system status + usage dashboard (plan
    /// unit 21, Phase E). Labelled "System" on the rail for brevity; the panel
    /// header reads "Command Center".
    CommandCenter,
    /// The hosted Messaging connectors pane: live connector health + detail
    /// (plan unit 24). Read-only host — the Test/Update actions render as
    /// deferred [LIVE] chips.
    Messaging,
}

impl Mode {
    /// RC2-UX (D-R1 m1, R2-24: "Cron", "Sessions", "System", "Messaging"
    /// were unexplained): a plain description of each section, now shown
    /// below Backstage's tabs instead of across the navigation host.
    pub fn rail_hint(self) -> &'static str {
        match self {
            Mode::Work => "Ask anything, or get things done on this Mac",
            Mode::Code => "Build and fix software with a coding helper",
            Mode::Council => "Ask several models at once",
            Mode::Design => "Visual design work — coming in a later release",
            Mode::Skills => "What Hermes, your fleet helper, knows how to do",
            Mode::Cron => "Jobs Hermes runs for you on a schedule",
            Mode::Sessions => "Hermes's earlier conversations and profiles",
            Mode::CommandCenter => "How the fleet is doing: status and usage",
            Mode::Messaging => "The chat apps and texts Hermes can answer",
        }
    }

    /// Whether the composer's model chip (the per-turn picker trigger) is shown
    /// in this mode (P9). Work replaces it with the executor chip, and Chat is
    /// pinned to `[chat].executor` (there is no picker in Chat — the pin is a
    /// config value, not a per-turn choice), so it is hidden in BOTH; every
    /// other mode shows the chip exactly as before.
    #[cfg(test)]
    pub fn shows_model_chip(self) -> bool {
        // RC2-CNCL5: Council picks its members in its own bar.
        !matches!(self, Mode::Work | Mode::Council)
    }
}

/// A plain single-line notice for the Command Center panel's non-data states
/// (gateway off, loading, fetch error, empty). Kept muted so it reads as a
/// placeholder, not content.
pub(crate) fn command_center_notice(msg: &str) -> gpui::AnyElement {
    div()
        .p(theme::z(16.))
        .text_size(theme::z(theme::TEXT_SIZE))
        .text_color(theme::p().muted)
        .child(SharedString::from(msg.to_string()))
        .into_any_element()
}

/// Map a connector's [`messaging_pane::ConnectorHealth`] to the status colour
/// its dot/label renders in. Running is the accent (live), Error/NeedsSetup warn
fn self_test_failure_copy(kind: SelfTestFailure) -> &'static str {
    match kind {
        SelfTestFailure::Rejected => "Connector rejected the configuration",
        SelfTestFailure::ServerError => "Connector service failed",
        SelfTestFailure::Unreachable => "Gateway is unreachable",
        SelfTestFailure::Unsupported => "Gateway does not support self-test",
        SelfTestFailure::ReauthRequired => "Gateway sign-in is required",
        SelfTestFailure::Other => "Connector test failed",
    }
}

/// Legacy Code project shape, read only during the one-time shared import.
/// Its autonomy field is compatibility data, never dispatch authorization.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CodeProject {
    pub name: String,
    pub folder: String,
    /// Ignored since U4 (the mode is per conversation, D12); kept so older
    /// saved blobs still deserialize.
    #[serde(default = "default_true")]
    pub autonomous: bool,
}

fn default_true() -> bool {
    true
}

/// Turn one durable app message into its canonical bridge row. Activity is
/// attached to that row when it is presently available, never emitted as its
/// own positional record: render activity is deliberately not persisted.
fn bridge_record(
    message: &awili_core::Message,
    activity: Option<&[SharedString]>,
) -> Option<awili_code::continuity::BridgeRecord> {
    let content = match message.role {
        awili_core::Role::Assistant => crate::conv_view::strip_answered_by_tag(&message.text),
        awili_core::Role::User => message.text.as_str(),
    };
    // Old answer-only refusals remain readable in storage but must not teach a
    // tool-capable harness to refuse the user's next request.
    if message.role == awili_core::Role::Assistant && crate::legacy_chat::is_legacy_decline(content)
    {
        return None;
    }
    let mut content = content.to_string();
    if let Some(activity) = activity.filter(|activity| !activity.is_empty()) {
        content.push_str("\n\n[activity]\n");
        for (index, line) in activity.iter().enumerate() {
            if index > 0 {
                content.push('\n');
            }
            content.push_str(line);
        }
    }
    Some(awili_code::continuity::BridgeRecord::message(
        crate::role_str(message.role),
        content,
    ))
}

/// The complete bridge projection for a live conversation.
///
/// Its cursor counts only durable message positions. `ConvView::rows` carries
/// transient render activity, so the available text is folded into its owning
/// message rather than becoming a record which can disappear after restart.
pub(crate) fn bridge_records_from_view(
    view: &crate::conv_view::ConvView,
) -> Vec<awili_code::continuity::BridgeRecord> {
    view.convo
        .messages()
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            bridge_record(
                message,
                view.rows.get(index).map(|row| row.activity.as_slice()),
            )
        })
        .collect()
}

/// The standing instructions that lead a Work task: the saved project's own
/// instructions field and — RC1-M ("no claude.md just agents.md"), only for
/// the lane that gets AGENTS.md nowhere else (`agents_in_task`: the Hermes
/// gateway) — the AGENTS.md files: RC1-M-FIX8 (whole-slice review P1) the
/// bounded user file (`agents_md`, else the default, under the owner's
/// home), then the folder's project chain. Claude Code, Codex, ZCode and the
/// DeepSeek CLI get theirs from their launch (`agent_launch::launch`), and
/// the in-app engine in its system prompt, so for them only the field is
/// added. No CLAUDE.md is ever read.
///
/// `None` when there is nothing to add; otherwise the `[Project: name]` /
/// `[Folder instructions]` label followed by the text, ready to lead the
/// task.
pub(crate) fn folder_instructions_prefix(
    projects: &[StoredProject],
    folder: Option<&std::path::Path>,
    agents_in_task: bool,
    agents_md: Option<&str>,
) -> Option<String> {
    let folder_str = folder.map(|f| f.display().to_string());
    let proj = projects
        .iter()
        .find(|p| folder_str.as_deref() == Some(p.folder.as_str()));
    let explicit = proj.map(|p| p.instructions.trim()).unwrap_or("");
    // RC1-M-FIX8 (pre-review P2): the user file does not need a folder.
    let chain = agents_in_task
        .then(|| crate::agent_launch::instructions_block(&agents_md_docs(folder, agents_md)))
        .flatten();
    let parts: Vec<&str> = [chain.as_deref(), Some(explicit)]
        .into_iter()
        .flatten()
        .filter(|t| !t.trim().is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    let label = proj
        .map(|p| format!("[Project: {}]", p.name))
        .unwrap_or_else(|| "[Folder instructions]".to_string());
    Some(format!("{label}\n{}", parts.join("\n\n")))
}

/// RC1-M-FIX8: the AGENTS.md files a lane gets as text: the bounded user
/// file (`agents_md`, else the default, under the owner's home), then — with
/// a folder — the project chain.
pub(crate) fn agents_md_docs(
    folder: Option<&std::path::Path>,
    agents_md: Option<&str>,
) -> Vec<crate::agent_launch::AgentsMd> {
    let home = crate::agent_launch::LaunchPaths::production().map(|p| p.home);
    home.and_then(|h| crate::agent_launch::user_agents_md(agents_md, &h))
        .into_iter()
        .chain(
            folder
                .map(crate::agent_launch::project_chain)
                .unwrap_or_default(),
        )
        .collect()
}

/// RC1-M-FIX8 (pre-review P1): the spec a local Build ships to its remote
/// runner — the bounded user AGENTS.md and the folder's project chain once
/// each, then the user's task verbatim.
pub(crate) fn local_build_spec(
    task: &str,
    folder: Option<&std::path::Path>,
    agents_md: Option<&str>,
) -> String {
    match crate::agent_launch::instructions_block(&agents_md_docs(folder, agents_md)) {
        Some(block) => format!("{block}\n\n{task}"),
        None => task.to_string(),
    }
}

/// RC3-S1a: the name the in-app prompt gives the model: the configured label,
/// else the model id; `None` when neither is set (never "no model set").
pub(crate) fn inapp_model_name(engine: &awili_config::EngineConfig) -> Option<String> {
    (!engine.label.trim().is_empty() || !engine.model.trim().is_empty())
        .then(|| engine.display_label())
}

/// The rules files the in-app engine's system prompt carries: the user-level
/// AGENTS.md, then the folder's project chain.
pub(crate) fn inapp_docs(
    folder: Option<&std::path::Path>,
    agents_md: Option<&str>,
    home: Option<&std::path::Path>,
) -> Vec<crate::agent_launch::AgentsMd> {
    let mut docs = Vec::new();
    if let Some(home) = home {
        docs.extend(crate::agent_launch::user_agents_md(agents_md, home));
    }
    if let Some(f) = folder {
        docs.extend(crate::agent_launch::project_chain(f));
    }
    docs
}

/// RC1-M: the in-app engine's system prompt: `policy` (the S5 memory
/// preamble), the AGENTS.md block (the user-level file — `agents_md`, else
/// the default, under `home` — then the folder's project chain), then the
/// in-app brief for `folder`. RC2-FIXA2: `memory_on` (the in-app engine's
/// memory is on) alone decides the memory line, so a turn with recalled
/// notes and one without pin the same bytes.
pub(crate) fn inapp_system(
    policy: Option<String>,
    memory_on: bool,
    folder: Option<&std::path::Path>,
    agents_md: Option<&str>,
    home: Option<&std::path::Path>,
    model_name: Option<&str>,
) -> Option<String> {
    let docs = inapp_docs(folder, agents_md, home);
    let agents = crate::agent_launch::instructions_block(&docs);
    let lead = [policy, agents]
        .into_iter()
        .flatten()
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>();
    let lead = (!lead.is_empty()).then(|| lead.join("\n\n"));
    let system = crate::agent_brief::with_owned_engine_brief(
        lead,
        &folder.map(|p| p.display().to_string()).unwrap_or_default(),
    );
    // RC3-S1a: a local model otherwise answers as the model its weights were
    // trained to claim to be (live: Gemma said it was Claude).
    let system = match (system, model_name.map(str::trim).filter(|n| !n.is_empty())) {
        (Some(s), Some(name)) => Some(format!(
            "{s}\n{}",
            crate::agent_brief::owned_identity_line(name)
        )),
        (other, _) => other,
    };
    // RC1-M-FIX1: with memory on, say the preamble is the bootstrap.
    match system {
        Some(s) if memory_on => Some(format!("{s}\n{}", crate::agent_brief::OWNED_MEMORY_LINE)),
        other => other,
    }
}

/// RC1-M-FIX1: whether the in-app engine's turn gets Awili's memory preamble
/// (S5). The preamble is the in-app engine's equivalent of the memory hooks
/// the owner's Claude Code and Codex run: on his install (`agent_memory`, his
/// `[work.agents.memory]`) it is on even when `[memory] base_url` is empty —
/// the v6.5 service is found from `~/.config/foreman/memory-backend.conf`
/// (live 2026-10-03: with tools alone Gemma bootstrapped, then asked the owner
/// for context instead of recalling it). Elsewhere `[memory]` decides, as
/// before.
pub(crate) fn inapp_memory_on(
    memory: &crate::MemoryCfg,
    agent_memory: Option<&awili_config::AgentMemoryConfig>,
) -> bool {
    memory.enabled || agent_memory.is_some()
}

/// RC1-N (round 4 M3 and 4b; lead, 2026-10-04: "simplest fix first —
/// don't offer the in-app engine the interlock `bootstrap` tool at all"):
/// the memory server's `bootstrap` tool. Awili's memory preamble already
/// delivers the rules and relevant memory, the owner's AGENTS.md already
/// says never to call it in Awili, and Gemma still called it in 1 of 2 runs
/// — its answer (63k characters) filled the whole window. `recall` and every
/// other memory tool stay; another server's tool of that name is untouched.
pub(crate) fn preamble_duplicate(def: &awili_tools::ToolDef) -> bool {
    def.identity.origin.origin == format!("mcp:{}", crate::agent_launch::OWNER_CHAT_MEMORY_SERVER)
        && def.identity.name == "bootstrap"
}

/// RC2-R59: how often one turn open starts again a server that died at start
/// (a server that is only slow to come up, or whose program was being
/// rebuilt, comes up on a second try; one that cannot start is not started
/// 70 times).
pub(crate) const TURN_OPEN_RESTARTS: usize = 2;

/// RC2-R59: count each awaited server's deaths seen in this turn open (one
/// per generation); one that died more than [`TURN_OPEN_RESTARTS`] times is
/// given up on until the next turn open.
fn note_deaths(
    lifecycle: &awili_engine_worker::mcp_coordinator::LifecycleSnapshot,
    awaited: &[awili_tools::ToolOrigin],
    deaths: &mut std::collections::HashMap<String, Vec<String>>,
    gave_up: &mut std::collections::HashSet<String>,
) {
    for o in awaited {
        if let Some(awili_engine_worker::mcp_coordinator::OriginStatus::Dead {
            generation, ..
        }) = lifecycle.origins.get(&o.origin)
        {
            let seen = deaths.entry(o.origin.clone()).or_default();
            let g = generation.to_string();
            if !seen.contains(&g) {
                seen.push(g);
            }
            if seen.len() > TURN_OPEN_RESTARTS {
                gave_up.insert(o.origin.clone());
            }
        }
    }
}

/// RC1-N-FIX1 (lead ruling 2026-10-04; was RC1-M-FIX7's recall-only rule):
/// whether Awili leaves its memory — the standing directives AND the recall —
/// out of a Claude or Codex turn because the owner's own hooks bring it. Only
/// his explicit setting decides (`[work.agents.memory] hooks_provide_memory =
/// true`, alias `hooks_provide_recall`; no guessing from hook files); any
/// other engine, a household install, or the default keeps the whole
/// preamble.
pub(crate) fn hooks_bring_memory(
    engine: CodeEngine,
    mem: Option<&awili_config::AgentMemoryConfig>,
) -> bool {
    mem.is_some_and(|m| m.hooks_provide_memory)
        && matches!(engine, CodeEngine::Claude | CodeEngine::Codex)
}

/// `app_state` key holding the Code projects list (JSON array of [`CodeProject`]).
pub(crate) const CODE_PROJECTS_KEY: &str = "code_projects";
/// `app_state` key holding the active project index (as a decimal string).
pub(crate) const CODE_ACTIVE_PROJECT_KEY: &str = "code_active_project";

pub(crate) type CodeRetryRevealKey = (gpui::Bounds<gpui::Pixels>, gpui::Size<gpui::Pixels>);

/// One Code-mode tab: a native conversation with a coding-agent engine
/// (`claude`/`codex`), driven by [`crate::code_provider::CodeProvider`]. The
/// engine identity, autonomy posture, and per-turn streaming live in `awili_code`;
/// this struct is the per-tab UI/session state.
pub struct CodeSession {
    /// Stable id (survives tab add/remove) so an async turn updates the right
    /// tab even if the active index shifts.
    pub id: u64,
    pub engine: CodeEngine,
    /// Compatibility seed before this tab has a durable conversation id. Once
    /// it has one, `WorkState::conversation_routes` is authoritative so the
    /// same route survives native ↔ Work dispatch without a second tab.
    pub(crate) route: crate::conversation_route::ConversationRoute,
    /// The transcript (source of truth) + its render plans, exactly as a chat
    /// view holds them — so the native bubble renderer is reused.
    pub convo: ConversationState,
    pub rows: Vec<crate::MessageRow>,
    pub(crate) reply_presentation_token: crate::conv_view::TurnToken,
    /// Assistant rows whose provider turn failed. These remain visibly marked
    /// after streaming settles instead of looking like a successful answer.
    pub errored: std::collections::HashSet<usize>,
    /// A turn is streaming into the last (assistant) row.
    pub streaming: bool,
    /// A question/reopen added context the native job's resume may not know.
    pub(crate) context_gap: bool,
    /// The engine's session/thread id, filled after the first turn so follow-ups
    /// resume it. Shared with the running [`CodeProvider`].
    pub session_slot: crate::code_provider::SessionSlot,
    /// The folder this tab's resume session belongs to. If the active project
    /// changes, the next turn detects the mismatch and starts a FRESH session so
    /// a `--resume` never runs an old conversation in the wrong repo (review HIGH).
    pub project_folder: Option<String>,
    /// The model the next turn runs on (`None` = the engine's own default).
    /// Cycled by the toolbar model chip through [`agent_model_options`];
    /// applies from the NEXT turn (a mid-run change never retargets a stream).
    pub model: Option<String>,
    pub scroll: gpui::ScrollHandle,
    /// Stable focus identity and reveal state for the transient Retry action.
    /// The button disappears as soon as retry starts, so its focus must be
    /// owned by the session rather than by that one rendered element.
    pub(crate) retry_focus: Option<gpui::FocusHandle>,
    pub(crate) retry_reveal_key: std::rc::Rc<std::cell::Cell<Option<CodeRetryRevealKey>>>,

    pub(crate) retry_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// The Stop wire for the in-flight turn: sending KILLS the child process
    /// (not just stops reading it). `None` when idle.
    pub stream_abort_tx: Option<std::sync::mpsc::Sender<()>>,
    pub(crate) host_approval: Option<awili_providers::HostApproval>,
    pub(crate) approval_session: crate::codex_approval::Session,
    pub history: crate::code_history::CodeTabHistory,
}

/// The Hermes REST connection (fallback when gateway is unavailable).
#[derive(Clone)]
pub struct HermesCfg {
    pub base_url: String,
    pub api_key: String,
    /// `false` when no Hermes base_url is configured — submit is refused with a
    /// clear message rather than firing a request at nowhere.
    pub enabled: bool,
    /// The dashboard origin (`[gateway].dashboard_url`) carried alongside the
    /// agent endpoint because the `/api/*` REST surface lives on the DASHBOARD
    /// behind its rotating session token — not on `base_url` (user-test G1).
    /// Empty when the gateway is not configured; `/api` callers then fall back
    /// to the (broken-for-`/api`) hermes origin, preserving old error shapes.
    pub dashboard_url: String,
}

/// The Hermes gateway WebSocket connection (preferred transport for Work mode).
#[derive(Clone)]
pub struct GatewayCfg {
    /// HTTP base URL of the dashboard (e.g. `http://127.0.0.1:60142`). Empty =
    /// gateway disabled, fall back to REST.
    pub dashboard_url: String,
    /// `true` when `dashboard_url` is non-empty.
    pub enabled: bool,
}

/// A plan step's lifecycle in the progress checklist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    /// Not started — empty circle + muted text.
    Pending,
    /// The step the agent is on now — numbered accent circle + normal text.
    Running,
    /// Finished — accent check + struck-through muted text.
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgressStep {
    pub label: String,
    pub state: StepState,
}

/// RC2-UX (D-PUNCH F11): the test lookup key of the memory search box.
pub(crate) const MEMORY_SEARCH_SELECTOR: &str = "rc2ux-memory-search";

/// RC2-UX (D-R1 S1 badge): the Work header for a turn that was refused before
/// it ran.
pub(crate) const NOT_SENT_LABEL: &str = "not sent";
/// RC2-UX (R5-4): what the Terminal box in the Work panel is.
pub(crate) const TERMINAL_CAPTION: &str =
    "A Terminal in this folder, for commands you type yourself. Awili's helpers do not type here.";
/// RC2-UX: the test lookup key of that line.
pub(crate) const TERMINAL_CAPTION_SELECTOR: &str = "rc2ux-terminal-caption";

/// RC2-UX (D-PUNCH S2-F16): the kind of a card that is Hermes' question.
pub(crate) const CLARIFY_KIND: &str = "clarify";
/// RC2-UX: the test lookup key of the question card.
pub(crate) const CLARIFY_CARD_SELECTOR: &str = "rc2ux-clarify-card";
/// RC2-UX: how to answer Hermes' question.
pub(crate) const CLARIFY_HOW_TO_ANSWER: &str =
    "Type your answer in the message box below, then press Enter.";

/// RC2-R3 (usability RC2 round 3, Minor 10): the heading of a card for a
/// step Awili never runs on its own (a tool that says nothing about what it
/// does, a secret file, a recovery step) \u{2014} not "this runs with full
/// rights", which a read-only `read_file` showed.
pub(crate) const PRIVILEGED_HEADING: &str =
    "Needs your OK \u{2014} Awili doesn\u{2019}t run this on its own";

/// RC2-UX (D-PUNCH S2-F09, round 2 S2 "APPROVAL · clarify", "[HIGH] … SSRF"):
/// the card's heading in plain words, by what the step would do.
pub(crate) fn approval_heading(card: &ApprovalCard) -> &'static str {
    if card.kind == CLARIFY_KIND {
        return "Hermes asks you";
    }
    match card.risk {
        ApprovalRisk::ReadOnly => "Needs your OK to look at something",
        ApprovalRisk::Mutating => "Needs your OK to make a change",
        ApprovalRisk::Irreversible => "Needs your OK \u{2014} this cannot be undone",
        // RC2-R3 (round 3 Minor 10: a read-only `read_file` in the Work
        // folder read "this runs with full rights"): this class is a tool
        // that says nothing about what it does, a secret file, or a
        // recovery step: Awili does not let it run on its own; it is not
        // "full rights".
        ApprovalRisk::Privileged => PRIVILEGED_HEADING,
    }
}

/// RC2-UX (D-PUNCH S2-F09): Hermes' description of a step in plain words:
/// its leading severity tag ("[HIGH] ") becomes "Risky: " (or goes), and the
/// scanner words the round saw become everyday ones.
pub(crate) fn plain_approval_text(desc: &str) -> String {
    let mut text = desc.trim().to_string();
    let mut risky = false;
    if let Some(rest) = text.strip_prefix('[') {
        if let Some((tag, after)) = rest.split_once(']') {
            if !tag.is_empty() && tag.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
                risky = matches!(tag.to_ascii_uppercase().as_str(), "HIGH" | "CRITICAL");
                text = after.trim_start().to_string();
            }
        }
    }
    for (from, to) in [
        (
            "SSRF or lateral movement",
            "reaching other computers on your network",
        ),
        (
            "private network access",
            "reaches a computer on your home network",
        ),
        (
            "delete in root path",
            "deletes files outside your home folder",
        ),
        ("The agent is requesting approval.", "This needs your OK."),
    ] {
        text = text.replace(from, to);
    }
    if risky {
        format!("Risky: {text}")
    } else {
        text
    }
}

/// An inline approval gate awaiting the user's scoped decision (P3a: the
/// card offers Allow once / Allow for session / Allow for folder /
/// Allow always / Deny on the SAME card).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalCard {
    pub approval_id: String,
    pub kind: String,
    pub prompt: String,
    /// The CLOSED risk class of the gated call (S2(a)): the gateway decode
    /// and the owned-engine seam both land here as a typed `ApprovalRisk` —
    /// never an unclassified string on the decision path.
    pub risk: ApprovalRisk,
    /// Delivery-ack bookkeeping: the card stays up until the gateway worker
    /// acknowledges delivery of the verdict, or the attempt is marked failed
    /// (dead channel / negative acknowledgement). See [`ApprovalDelivery`].
    pub delivery: ApprovalDelivery,
}

impl ApprovalCard {
    /// The capability floor for an approval choice (P3a, contract §8):
    /// whether this card may OFFER `decision` at all.
    ///
    /// `once` and `deny` are always offerable. `folder` requires a bound
    /// folder (a folder grant with no folder to bind to is a grant the
    /// authorizer could never match). `always` is refused for
    /// irreversible/privileged risk — the authorizer re-checks the floor
    /// when minting (fail closed), so the card never offers what can never
    /// be granted. `install` is NEVER offerable: the dialog cannot mint it.
    /// Unsupported choices are never relabeled as `once` (see
    /// [`resolve_approval_choice`]).
    pub fn offers(&self, decision: ApprovalDecision, folder_bound: bool) -> bool {
        match decision {
            ApprovalDecision::Approve | ApprovalDecision::Deny => true,
            ApprovalDecision::Session => true,
            ApprovalDecision::Folder => folder_bound,
            ApprovalDecision::Always => {
                // S2(a): `always` is offered ONLY for the two reversible
                // classes; the gateway decode is conservative (missing /
                // unknown -> `Privileged`), so the conservative default
                // refuses `always`.
                matches!(self.risk, ApprovalRisk::ReadOnly | ApprovalRisk::Mutating)
            }
        }
    }
}

/// Fail-closed resolution of an approval choice (P3a): `None` means the
/// choice is NOT offered by this card and must be refused — no command is
/// sent and it is NEVER relabeled as `once`. `Some(scope)` is the EXACT
/// scope string to send through [`GatewayCmd::Approval`]
/// (`"once" | "session" | "folder" | "always" | "deny"`).
fn resolve_approval_choice(
    card: &ApprovalCard,
    decision: ApprovalDecision,
    folder_bound: bool,
) -> Option<&'static str> {
    if card.offers(decision, folder_bound) {
        Some(decision.scope_str())
    } else {
        None
    }
}

/// Route one already-resolved approval scope to the active engine (P10/F1).
///
/// Hermes ignores the wire id and owns a delivery-ack handshake, so it goes
/// through [`ApprovalDelivery::send`]: the card stays up until the worker
/// acknowledges delivery, and a dead channel marks the attempt failed. The
/// owned `awili` engine correlates verdicts on the ORIGINAL approval id (it
/// drops any other id), so that id is sent directly on the command channel; a
/// successful send clears the card (return `true`), a dead channel marks the
/// delivery failed and keeps it up (return `false`).
/// RC1-N: an in-app Deny waiting for the engine's confirmation. The engine
/// ends the turn Stopped only after the denial is durably recorded; then the
/// reply becomes `line` and `request` goes back in the box with `toast`. Any
/// other end (a failed turn) shows as that failure and nothing is put back
/// (pre-review P1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DenyStop {
    pub(crate) line: String,
    pub(crate) request: Option<String>,
    pub(crate) toast: &'static str,
}

/// RC1-N-FIX2: the slot of a stopped in-app worker of `conv` that has not
/// published its turn's history yet (see `dispatch_work_prompt`'s barrier).
pub(crate) fn stopped_history_pending(
    work: &WorkState,
    conv: &str,
) -> Option<awili_engine_worker::HistorySlot> {
    let (slot, _) = work.stopped_histories.get(conv)?;
    let empty = slot.lock().map(|g| g.is_none()).unwrap_or(false);
    empty.then(|| slot.clone())
}

/// RC1-N-FIX2: the model history a Stop's worker published for `conv` (the
/// dispatch barrier waited for it), as kept history covering the part of
/// `before` (the transcript up to the turn now being submitted) that existed
/// at the Stop. `None` when nothing was
/// stopped there.
pub(crate) fn take_stopped_history(
    work: &mut WorkState,
    conv: &str,
    before: &[awili_core::Message],
) -> Option<inapp_history::KeptHistory> {
    // Pre-review pass 5: only `conv`'s own entry; another conversation's
    // stopped worker stays pending for its own next turn.
    let (slot, at_stop) = work.stopped_histories.remove(conv)?;
    let history = slot.lock().ok().and_then(|mut g| g.take())?;
    // Pre-review pass 3: the history covers the transcript as it was at the
    // Stop; a later helper's turn stays outside it, so `resume` restores it
    // as context.
    let covered = &before[..at_stop.min(before.len())];
    Some(inapp_history::KeptHistory::new(history, covered))
}

/// RC1-Q (round 5 minor 3): the Progress card's text when it has no steps.
/// While a plan waits the header says "waiting for your OK"; the card must not
/// say "No run yet" under it, nor after a run that ended.
/// RC2-R616 (usability RC2 round 5, Major 1: a cold first Gemma turn sat on
/// "Reading your request… 85%" for about 40 s with nothing else; a Codex
/// turn showed nothing for about 75 s): a wait is never frozen. Once a run
/// has waited [`ALIVE_AFTER`], its Progress line says for how long, and a
/// model server that sends nothing new while reading for
/// [`SERVER_QUIET_AFTER`] is said to be quiet. `waited`: the run's time so
/// far (`None` when no run is going); `quiet`: since anything last changed.
pub(crate) fn alive_line(
    line: &str,
    waited: Option<std::time::Duration>,
    quiet: std::time::Duration,
) -> String {
    let Some(waited) = waited.filter(|w| *w >= ALIVE_AFTER) else {
        return line.to_string();
    };
    let mut s = format!(
        "{line} \u{b7} {}",
        crate::executor_status::fmt_silence(waited)
    );
    if quiet >= SERVER_QUIET_AFTER && line.starts_with(READING_PREFIX) {
        s.push_str(&format!(
            " \u{b7} nothing new from the model server for {}",
            crate::executor_status::fmt_silence(quiet)
        ));
    }
    s
}

/// RC2-R616: a run shows its time once it has waited this long.
pub(crate) const ALIVE_AFTER: std::time::Duration = std::time::Duration::from_secs(3);
/// RC2-R616: a reading model server that sent nothing new for this long is
/// said to be quiet.
pub(crate) const SERVER_QUIET_AFTER: std::time::Duration = std::time::Duration::from_secs(15);
/// RC2-R616: how the in-app engine's reading line starts.
pub(crate) const READING_PREFIX: &str = "Reading your request";

pub(crate) fn progress_empty_text(running: bool, plan_waiting: bool, ended: bool) -> &'static str {
    if running {
        "Working on it…"
    } else if plan_waiting {
        "The plan is in the chat. Nothing has changed yet."
    } else if ended {
        "This run has ended. Its reply is in the chat."
    } else {
        "No run yet. Choose a folder, describe a task, press Enter."
    }
}

/// RC1-N: the one plain line an in-app turn's reply becomes after the
/// owner's Deny. `prompt` is the card's text (its first line names the
/// action); `footer` is Awili's own record of the files the turn changed
/// before the card ("changed a.txt"), used only when `changed`. The line
/// claims nothing about other steps of the turn (an earlier tool may have
/// run under a grant; pre-review P2): it says the declined action was not
/// done.
pub(crate) fn deny_stop_line(prompt: &str, footer: &str, changed: bool) -> String {
    // Pre-review P2 (pass 10): one line, whatever the file names hold.
    let footer: String = footer
        .chars()
        .map(|c| {
            if inapp_files::is_hidden_char(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    // Pre-review P3 (pass 13): the card's first line too.
    let first: String = prompt
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| {
            if inapp_files::is_hidden_char(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let first = first.trim();
    let what: String = if first.chars().count() > 120 {
        let cut: String = first.chars().take(119).collect();
        format!("{cut}\u{2026}")
    } else {
        first.to_string()
    };
    let declined = if what.is_empty() {
        "you declined it".to_string()
    } else {
        format!("you declined \u{201C}{what}\u{201D}")
    };
    if changed {
        format!("Stopped \u{2014} {declined}, so it was not done. Before that, this turn {footer}.")
    } else {
        format!("Okay \u{2014} {declined}, so it was not done.")
    }
}

fn route_work_approval(
    engine_kind: awili_config::EngineKind,
    card: &mut ApprovalCard,
    choice: &'static str,
    tx: Option<&std::sync::mpsc::Sender<GatewayCmd>>,
) -> bool {
    if engine_kind == awili_config::EngineKind::HermesGateway {
        card.delivery.send(tx, choice);
        return false;
    }
    // An empty backend id is sent as `None`; the owned engine's gate is
    // correlated, so a real id must cross verbatim.
    let id = Some(card.approval_id.clone()).filter(|id| !id.is_empty());
    let sent = tx.is_some_and(|tx| {
        tx.send(GatewayCmd::Approval {
            id,
            choice: choice.into(),
        })
        .is_ok()
    });
    if sent {
        true
    } else {
        card.delivery.failed = true;
        false
    }
}

/// Delivery-ack state for a sent approval verdict: the card is cleared only
/// when the gateway worker reports the command was delivered. A send that
/// cannot reach the channel (or a negative acknowledgement) marks the
/// delivery failed, so the surface can tell the user to check the run —
/// no automatic retry is made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApprovalDelivery {
    pending: Option<String>,
    failed: bool,
    /// U3e1: the scope of the last verdict sent, for the in-chat outcome line.
    sent_choice: Option<&'static str>,
}

impl ApprovalDelivery {
    fn begin(&mut self) -> Option<String> {
        if self.pending.is_some() {
            return None;
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = format!("awili-delivery-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        self.pending = Some(id.clone());
        self.failed = false;
        Some(id)
    }
    fn send(&mut self, tx: Option<&std::sync::mpsc::Sender<GatewayCmd>>, choice: &'static str) {
        let Some(id) = self.begin() else {
            return;
        };
        self.sent_choice = Some(choice);
        if !tx.is_some_and(|tx| {
            tx.send(GatewayCmd::Approval {
                id: Some(id),
                choice: choice.into(),
            })
            .is_ok()
        }) {
            self.pending = None;
            self.failed = true;
        }
    }
    fn complete(&mut self, id: &str, delivered: bool) -> bool {
        if self.pending.as_deref() != Some(id) {
            return false;
        }
        self.pending = None;
        self.failed = !delivered;
        delivered
    }
}

impl RunPanel {
    /// The delivered verdict's scope when the card clears, else `None` (U3e1).
    fn complete_approval_delivery(
        &mut self,
        id: Option<&str>,
        delivered: bool,
    ) -> Option<&'static str> {
        if let (Some(card), Some(id)) = (&mut self.approval, id) {
            if card.delivery.complete(id, delivered) {
                let choice = card.delivery.sent_choice;
                // RC2-R4 (D-R1 M10c): the answered card keeps its place.
                if let (Some(card), Some(choice)) = (self.approval.take(), choice) {
                    self.decided = Some(DecidedApproval { card, choice });
                }
                return choice;
            }
        }
        None
    }
}

/// A blocking credential request from the agent — a sudo password
/// (`sudo.request`) or a skill credential (`secret.request`). The agent halts
/// until a response lands, so this is surfaced as a card the user can decline to
/// unblock the turn. It holds ONLY the request metadata — never an entered
/// secret value (the value, when supplied, flows straight to the gateway and is
/// never stored here, persisted, or logged).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialPrompt {
    pub request_id: String,
    pub kind: CredentialKind,
    /// The env var a `secret.request` wants (empty for sudo).
    pub env_var: String,
    /// The agent's human-readable prompt text (may be empty).
    pub prompt: String,
}

/// A one-shot Review request for a measured control in the owning transcript.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct BlockingReveal {
    owner: String,
    run_seq: u64,
    request: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialKind {
    Sudo,
    Secret,
}

/// The live state of a run, built up purely from its [`RunEvent`] stream plus the
/// terminal status poll. Everything the right panel paints comes from here.
/// RC2-R4 (D-R1 M10c, "layout jump after approval: Save as project moves under
/// the cursor"): a card the owner answered, with the answer. It keeps the
/// card's place in the Work panel — same size, buttons inert — until the next
/// card or the next run, so nothing below it moves under the pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecidedApproval {
    pub card: ApprovalCard,
    /// The scope sent (`once`, `session`, `folder`, `always`, `deny`).
    pub choice: &'static str,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct RunPanel {
    /// rc.3 S2: the context window the provider reported for this run's model.
    pub context_window: Option<u32>,
    /// rc.3 S2: how many usage reports the run folded together.
    pub usage_frames: u32,
    /// rc.3 S2: the input of the last usage report folded into `usage`.
    pub last_usage_input: Option<u32>,
    pub steps: Vec<ProgressStep>,
    pub approval: Option<ApprovalCard>,
    /// RC2-R4 (D-R1 M10c): the last answered card, shown in its place.
    pub decided: Option<DecidedApproval>,
    /// Presentation anchor retained when the live stream ends. A panel reset
    /// clears this alongside its record; it never owns or routes worker output.
    pub(crate) decided_reply: Option<(String, usize)>,
    /// A pending sudo/secret credential request the agent is blocking on.
    pub credential: Option<CredentialPrompt>,
    pub status: Option<RunStatus>,
    /// The final result (from a [`CompletionEvent`] or the terminal poll).
    pub output: Option<String>,
    /// Tokens streamed so far — shown live as the result builds.
    pub streamed: String,
    /// A failure message (transport error, or a non-`Completed` terminal state).
    pub error: Option<String>,
    /// Failed tool attempts do not imply the whole agent turn failed.
    pub tool_attempt_failed: bool,
    /// Frozen requested configuration at dispatch; distinct from backend facts.
    pub(crate) requested_identity: Option<WorkRunIdentity>,
    /// Backend-reported model, with no inferred location.
    pub model: Option<String>,
    /// P8b: end-of-turn token accounting reported by the executor (vendor
    /// CLI `usage` frames), summed across frames; `None` when not reported.
    pub usage: Option<awili_providers::Usage>,
    /// The latest tool-progress message — a "working…" line so the UI is never frozen.
    pub activity: Option<String>,
    /// Recent tool calls with their status — feeds the TOOLS card.
    pub tool_calls: Vec<ToolCall>,
    /// RC2-CXSF (RC2-CXS review P3): whether THIS run's Codex turn runs on
    /// `codex exec --json` (each message shown once written in full). One
    /// per run: a fresh panel at each dispatch.
    pub(crate) codex_exec: CodexExec,
}

/// RC2-CXSF: set by the provider thread when this run's Codex turn runs on
/// `codex exec --json` instead of streaming over its app server
/// (`CodeProvider::observe_exec`). Two panels are equal when both say the
/// same.
#[derive(Clone, Debug, Default)]
pub(crate) struct CodexExec(Arc<AtomicBool>);

impl CodexExec {
    /// The flag the provider sets.
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }

    /// This run's Codex turn runs on `codex exec --json`.
    pub(crate) fn on_exec(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl PartialEq for CodexExec {
    fn eq(&self, other: &Self) -> bool {
        self.on_exec() == other.on_exec()
    }
}

impl Eq for CodexExec {}

/// A tracked tool invocation for the TOOLS card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub name: String,
    pub status: ToolCallStatus,
    pub message: Option<String>,
    /// RC2-RUN (S-Q1): Awili's own "Thinking" row or a tool's row — typed,
    /// so a tool the owner named "reasoning" is never taken for it.
    pub kind: RowKind,
}

/// What a Tools-card row stands for (RC2-RUN, S-Q1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowKind {
    /// A tool call, keyed by the tool's name.
    #[default]
    Tool,
    /// The model thinking (`WorkUpdate::Reasoning`).
    Reasoning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolCallStatus {
    Running,
    Completed,
    Failed,
    /// RC1-Q-FIX2: the run ended before this call reported an outcome
    /// (denied, or cut off by a Stop). Shown neutral, never ✓ or ✗.
    Ended,
}

impl ToolCall {
    /// This row is the call of the tool `name` (never the thinking row).
    fn is_tool(&self, name: &str) -> bool {
        self.kind == RowKind::Tool && self.name == name
    }

    /// RC2-RUN (round 6 Minor 2): the row's title in plain words —
    /// "Thinking", the built-in file tools by what they do; any other tool
    /// (the owner's own) by its own name.
    pub fn title(&self) -> &str {
        match self.kind {
            RowKind::Reasoning => THINKING_ROW,
            RowKind::Tool => plain_tool(&self.name).map_or(self.name.as_str(), |p| p.title),
        }
    }
}

/// RC2-RUN: the thinking row's title and its line in Progress.
pub(crate) const THINKING_ROW: &str = "Thinking";
pub(crate) const THINKING_LINE: &str = "Thinking…";
pub(crate) const THOUGHT_LINE: &str = "Finished thinking";

/// The plain words for one of Awili's built-in file tools (RC2-RUN, round 6
/// Minor 2: the Tools card said "fs.glob / fs.glob done").
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PlainTool {
    /// The row title.
    pub title: &'static str,
    /// The Progress line while it runs.
    pub running: &'static str,
    /// The row's line when it finished.
    pub done: &'static str,
    /// The row's line when it failed.
    pub failed: &'static str,
}

pub(crate) fn plain_tool(name: &str) -> Option<PlainTool> {
    let (title, running, done, failed) = match name {
        "fs.read" => (
            "Read a file",
            "Reading a file…",
            "Read the file",
            "Could not read the file",
        ),
        "fs.write" => (
            "Write a file",
            "Writing a file…",
            "Wrote the file",
            "Could not write the file",
        ),
        "fs.edit" => (
            "Edit a file",
            "Editing a file…",
            "Edited the file",
            "Could not edit the file",
        ),
        "fs.glob" => (
            "List files",
            "Listing files…",
            "Listed the files",
            "Could not list the files",
        ),
        "fs.grep" => (
            "Search the files",
            "Searching the files…",
            "Searched the files",
            "Could not search the files",
        ),
        _ => return None,
    };
    Some(PlainTool {
        title,
        running,
        done,
        failed,
    })
}

/// A tool call's line while it runs, when the engine gave no description:
/// the plain words for a built-in tool, else the tool's own name.
fn tool_running_line(name: &str) -> String {
    plain_tool(name).map_or_else(|| name.to_string(), |p| p.running.to_string())
}

/// A finished tool call's line when the engine gave no description (a
/// failed call never reads "done", RC1-Q-FIX2).
fn tool_finished_line(name: &str, failed: bool) -> String {
    match (plain_tool(name), failed) {
        (Some(p), false) => p.done.to_string(),
        (Some(p), true) => p.failed.to_string(),
        (None, false) => format!("{name} done"),
        (None, true) => format!("{name} failed"),
    }
}

/// RC2-RUN (round 6 Minor 1): the in-app engine's MCP status lines
/// ("reconciled: 3 live, 0 pending", "<server>: live (…)", "install grants
/// seeded …") are engine internals, not progress. Only a real problem — a
/// tool dropped, a server not accepted yet, a failed seed — reaches the
/// owner, as one plain line.
pub(crate) fn mcp_status_line(message: &str) -> Option<&'static str> {
    let problem = message.contains(" dropped: ")
        || message.contains(": pending acceptance")
        || message.contains("failed");
    problem.then_some(MCP_PROBLEM_LINE)
}

pub(crate) const MCP_PROBLEM_LINE: &str =
    "Some of your connected tools are not available for this run.";

/// RC2-RUN (R3c-7): the Progress line after the owner's Deny while the
/// in-app turn winds down (it can take a while).
pub(crate) const DENY_WINDING_DOWN: &str = "Stopping after your Deny — finishing up…";

impl RunPanel {
    /// Persist a terminal failure in the reply. This notice is appended
    /// before finalization, which otherwise only copies an error into the
    /// transcript when no assistant text exists. RC1-N (round 4 Minor 5): a
    /// tool attempt that failed along the way is no longer announced in the
    /// reply — it stays in the steps list and the run header.
    fn agent_final_notice(&self) -> Option<String> {
        self.error.as_ref().map(|error| format!("\n\n⚠ {error}"))
    }

    fn current_running(&self) -> Option<usize> {
        self.steps
            .iter()
            .position(|s| s.state == StepState::Running)
    }

    /// The result text to display: the explicit output, else the streamed tokens.
    pub fn result_text(&self) -> Option<&str> {
        self.output
            .as_deref()
            .or_else(|| (!self.streamed.is_empty()).then_some(self.streamed.as_str()))
    }

    /// RC1-Q: every row still `Running` ends with the run. Called on every
    /// way a run ends: `Completion`, `Terminal` (Stop) and `Failed`
    /// (RC1-Q-FIX1). The thinking row (`RowKind::Reasoning`) ends `Completed` (`Failed` when the
    /// run failed). A tool row still running never reported its own outcome —
    /// a call the owner denied, or one cut off by a Stop — so it ends
    /// `Ended`: neither a success tick nor a failure (RC1-Q-FIX2, slice
    /// review P1: a denied write showed ✓).
    pub fn close_running_rows(&mut self, failed: bool) {
        for t in &mut self.tool_calls {
            if t.status != ToolCallStatus::Running {
                continue;
            }
            t.status = if t.kind != RowKind::Reasoning {
                ToolCallStatus::Ended
            } else if failed {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            };
        }
    }

    /// RC2-RUN (S-Q1): the model is thinking, or finished thinking — the one
    /// typed thinking row (`RowKind::Reasoning`), never a tool's row.
    pub fn apply_reasoning(&mut self, done: bool) {
        let running = self
            .tool_calls
            .iter_mut()
            .rev()
            .find(|c| c.kind == RowKind::Reasoning && c.status == ToolCallStatus::Running);
        match (running, done) {
            (Some(row), true) => row.status = ToolCallStatus::Completed,
            (Some(_), false) => {}
            (None, done) => self.tool_calls.push(ToolCall {
                name: THINKING_ROW.to_string(),
                status: if done {
                    ToolCallStatus::Completed
                } else {
                    ToolCallStatus::Running
                },
                message: None,
                kind: RowKind::Reasoning,
            }),
        }
        self.activity = Some(if done { THOUGHT_LINE } else { THINKING_LINE }.to_string());
        self.advance(Some(if done { "completed" } else { "running" }));
    }

    /// RC1-Q-FIX1: whether this panel shows a run that ended — a terminal
    /// status, or an error with none (`WorkUpdate::Failed` sets only the
    /// error). The Progress card then never says "No run yet".
    pub fn has_ended(&self) -> bool {
        self.status.is_some() || self.error.is_some()
    }

    pub fn mark_all_done(&mut self) {
        for s in &mut self.steps {
            s.state = StepState::Done;
        }
    }

    /// Advance the checklist on a tool-progress status. The event carries no step
    /// index, so the rule is positional: a "done"-ish status completes the current
    /// running step and promotes the next pending one; any other status just ensures
    /// *something* is running (so a plan that only emits tool events still animates).
    fn advance(&mut self, status: Option<&str>) {
        let done = matches!(
            status,
            Some("completed")
                | Some("complete")
                | Some("done")
                | Some("success")
                | Some("ok")
                | Some("finished")
        );
        if done {
            if let Some(i) = self.current_running() {
                self.steps[i].state = StepState::Done;
                if let Some(next) = self.steps.get_mut(i + 1) {
                    if next.state == StepState::Pending {
                        next.state = StepState::Running;
                    }
                }
            } else if let Some(next) = self
                .steps
                .iter_mut()
                .find(|s| s.state == StepState::Pending)
            {
                next.state = StepState::Running;
            }
        } else if self.current_running().is_none() {
            if let Some(next) = self
                .steps
                .iter_mut()
                .find(|s| s.state == StepState::Pending)
            {
                next.state = StepState::Running;
            }
        }
    }

    /// Fold one lifecycle event into the panel. Pure — the unit tests drive this
    /// directly. PlanEvent (re)builds the checklist; ToolProgress advances it; an
    /// ApprovalEvent raises the card; Token streams output; Completion finishes.
    pub fn apply_event(&mut self, ev: &RunEvent) {
        #[cfg(test)]
        if let Some(tap) = RUN_EVENT_TAP
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            tap.push((std::time::Instant::now(), ev.clone()));
        }
        match ev {
            RunEvent::Plan(p) => {
                self.steps = p
                    .steps
                    .iter()
                    .map(|label| ProgressStep {
                        label: label.clone(),
                        state: StepState::Pending,
                    })
                    .collect();
                if let Some(first) = self.steps.first_mut() {
                    first.state = StepState::Running;
                }
            }
            RunEvent::ToolProgress(t) => {
                let line = t
                    .message
                    .clone()
                    .or_else(|| t.tool.clone())
                    .or_else(|| t.status.clone());
                if line.is_some() {
                    self.activity = line;
                }
                if let Some(name) = &t.tool {
                    let is_done = matches!(
                        t.status.as_deref(),
                        Some("completed")
                            | Some("complete")
                            | Some("done")
                            | Some("success")
                            | Some("ok")
                            | Some("finished")
                    );
                    let is_error = matches!(t.status.as_deref(), Some("error") | Some("failed"));
                    if is_done || is_error {
                        if let Some(tc) = self
                            .tool_calls
                            .iter_mut()
                            .rev()
                            .find(|c| c.is_tool(name) && c.status == ToolCallStatus::Running)
                        {
                            tc.status = if is_error {
                                ToolCallStatus::Failed
                            } else {
                                ToolCallStatus::Completed
                            };
                            tc.message = t.message.clone();
                        } else {
                            self.tool_calls.push(ToolCall {
                                name: name.clone(),
                                status: if is_error {
                                    ToolCallStatus::Failed
                                } else {
                                    ToolCallStatus::Completed
                                },
                                message: t.message.clone(),
                                kind: RowKind::Tool,
                            });
                        }
                    } else if !self
                        .tool_calls
                        .iter()
                        .any(|c| c.is_tool(name) && c.status == ToolCallStatus::Running)
                    {
                        self.tool_calls.push(ToolCall {
                            name: name.clone(),
                            status: ToolCallStatus::Running,
                            message: t.message.clone(),
                            kind: RowKind::Tool,
                        });
                    }
                }
                self.advance(t.status.as_deref());
            }
            RunEvent::Approval(a) => {
                // RC2-R4 (D-R1 M10c): a new card takes the answered one's place.
                self.decided = None;
                self.decided_reply = None;
                self.approval = Some(ApprovalCard {
                    delivery: ApprovalDelivery::default(),
                    approval_id: a.approval_id.clone().unwrap_or_default(),
                    kind: a.kind.clone().unwrap_or_else(|| "action".into()),
                    prompt: a
                        .prompt
                        .clone()
                        .unwrap_or_else(|| "The agent is requesting approval.".into()),
                    // P3a/S2a: the card offers approval scopes from the typed
                    // risk class (`always` only for ReadOnly/Mutating). The
                    // event already carries the closed set TYPED — the decode
                    // is conservative upstream (missing / unknown ->
                    // `Privileged`), so no unclassified value can ever ride
                    // the offer path.
                    risk: a.risk,
                });
            }
            RunEvent::Token(t) => {
                self.streamed.push_str(&t.text);
            }
            RunEvent::Completion(c) => {
                self.status = Some(c.status.unwrap_or(RunStatus::Completed));
                if c.output.is_some() {
                    self.output = c.output.clone();
                }
                // P3i: a failed turn's completion carries its reason in the
                // output — the ledger (`graduation_record::observe`) reads the
                // error, so the reason must land there too (only when the
                // error slot is still empty, so nothing overwrites it).
                if self.status == Some(RunStatus::Failed) && self.error.is_none() {
                    if let Some(text) = c.output.as_deref() {
                        if !text.trim().is_empty() {
                            self.error = Some(text.to_string());
                        }
                    }
                }
                self.approval = None;
                self.mark_all_done();
                // RC1-Q (round 5 minor 5): nothing is still running once the
                // turn is over — a "reasoning / thinking…" row (the in-app
                // engine never sends `ReasoningAvailable`) or any tool row
                // left open ends with the turn.
                self.close_running_rows(self.status == Some(RunStatus::Failed));
            }
            RunEvent::Unknown { .. } => {}
        }
    }
}

/// Commands sent from the UI thread to the persistent gateway worker.
///
/// D1: the enum itself moved to `awili-work` as [`WorkCmd`] — the engine-neutral
/// command vocabulary. Aliased here so every existing `GatewayCmd::` site keeps
/// compiling unchanged (no flag-day rename; Codex D1 consensus).
pub use awili_work::WorkCmd as GatewayCmd;

/// The memory inspector's last results (P4b): the v5 bucket response or the
/// typed v6.5 lanes — kept as DISTINCT variants so the inspector renders each
/// backend's own labeled lanes and never promotes one lane into another.
#[derive(Debug)]
pub(crate) enum MemoryInspectorResults {
    /// The historical v5 `/v5/recall` buckets (the explicit v5 fallback).
    V5(awili_memory::RecallResponse),
    /// The typed v6.5 adapter's two canonical lanes, separate by
    /// construction: the mandatory-policy lane, then the recall-items lane.
    V65 {
        policy: Vec<awili_memory::V65Item>,
        items: Vec<awili_memory::V65Item>,
    },
}

/// All Work-mode state, held on [`AwiliApp`].
/// What executes a Work run (executor-selection design of record 2026-08-22,
/// first UI slice 2026-08-23): the fleet gateway (today's default) or one of
/// the three vendor agent harnesses, each optionally pinned to one of its own
/// models. Selection applies to the NEXT run; session-scoped for now
/// (per-conversation persistence is the design's follow-up).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkExecutor {
    /// The in-app engine (awili-engine) on this Mac: tools run here, the
    /// model is `[engine]` (L1). The default.
    InApp,
    /// An additional in-app model from `[[engines]]`, by its stable id
    /// (persisted `in-app:<id>`). Runs on the same in-app engine as `InApp`,
    /// against that entry's endpoint. An id that is not (or no longer)
    /// configured is an error at turn time, never a fallback to the primary.
    InAppExtra { id: String },
    /// The fleet gateway (Hermes, remote): used only when picked (L1).
    Gateway,
    /// A vendor coding-agent harness (its own binary, its own OAuth).
    Agent {
        engine: CodeEngine,
        model: Option<String>,
    },
    /// A durable local Build (daily-driver P2d): the manual override that
    /// dispatches the next run to a configured `[build_targets.<id>]`
    /// binding through awili-dispatch. It resolves to its actual adapter
    /// before execution and is consumed once after the accepted dispatch.
    LocalBuild { target_id: String },
}

impl WorkExecutor {
    /// The `[[engines]]` id when this is an extra in-app engine.
    pub fn extra_engine_id(&self) -> Option<&str> {
        match self {
            WorkExecutor::InAppExtra { id } => Some(id),
            _ => None,
        }
    }

    /// Serialize for app_state persistence: "in-app", "gateway" or
    /// "agent:<claude|codex|glm|dsh>[:<model>]".
    pub fn to_persist(&self) -> String {
        match self {
            WorkExecutor::InApp => "in-app".to_string(),
            WorkExecutor::InAppExtra { id } => format!("in-app:{id}"),
            WorkExecutor::Gateway => "gateway".to_string(),
            WorkExecutor::Agent { engine, model } => {
                let e = match engine {
                    CodeEngine::Claude => "claude",
                    CodeEngine::Codex => "codex",
                    CodeEngine::Glm => "glm",
                    CodeEngine::Dsh => "dsh",
                };
                match model {
                    Some(m) => format!("agent:{e}:{m}"),
                    None => format!("agent:{e}"),
                }
            }
            WorkExecutor::LocalBuild { target_id } => format!("local-build:{target_id}"),
        }
    }

    /// Parse a persisted executor; anything unrecognizable is `None` (the
    /// caller keeps the InApp default — a stale value can never brick Work).
    /// `local-build:<id>` is the daily-driver P2d override: the target id is
    /// the payload (it may contain ':'), and an empty or oversized id is
    /// unrecognizable, like every other malformed value.
    pub fn from_persist(raw: &str) -> Option<WorkExecutor> {
        if raw == "gateway" {
            return Some(WorkExecutor::Gateway);
        }
        if raw == "in-app" {
            return Some(WorkExecutor::InApp);
        }
        if let Some(id) = raw.strip_prefix("in-app:") {
            return awili_config::is_valid_engine_id(id)
                .then(|| WorkExecutor::InAppExtra { id: id.to_string() });
        }
        if let Some(target_id) = raw.strip_prefix("local-build:") {
            if target_id.is_empty() || target_id.len() > 256 || target_id.contains('\0') {
                return None;
            }
            return Some(WorkExecutor::LocalBuild {
                target_id: target_id.to_string(),
            });
        }
        let rest = raw.strip_prefix("agent:")?;
        let (engine_s, model) = match rest.split_once(':') {
            Some((e, m)) if !m.is_empty() => (e, Some(m.to_string())),
            Some((e, _)) => (e, None),
            None => (rest, None),
        };
        let engine = match engine_s {
            "claude" => CodeEngine::Claude,
            "codex" => CodeEngine::Codex,
            "glm" => CodeEngine::Glm,
            "dsh" => CodeEngine::Dsh,
            _ => return None,
        };
        Some(WorkExecutor::Agent { engine, model })
    }

    /// The chip label: "gateway" or "Claude · fable". A local-build
    /// executor degrades to the bare lane name here (it has no config to
    /// resolve against); the chip and the picker use
    /// [`label_with`](Self::label_with) for the full `Build · <model> on
    /// <ssh_host>` form.
    pub fn label(&self) -> String {
        match self {
            WorkExecutor::InApp | WorkExecutor::InAppExtra { .. } => "in-app".to_string(),
            WorkExecutor::Gateway => "gateway".to_string(),
            WorkExecutor::Agent { engine, model } => match model {
                Some(m) => format!("{} \u{00B7} {m}", engine.label()),
                None => engine.label().to_string(),
            },
            WorkExecutor::LocalBuild { .. } => "build".to_string(),
        }
    }

    /// The picker/chip label for a local-build executor, resolved from the
    /// configured targets: `Build · <model> on <ssh_host>`. `None` when the
    /// target id is not (or no longer) configured — an unknown target is
    /// never offered, and the label falls back to the lane name instead of
    /// inventing a model or host.
    pub fn local_build_label(
        &self,
        targets: &std::collections::BTreeMap<String, awili_config::BuildTargetConfig>,
    ) -> Option<String> {
        let WorkExecutor::LocalBuild { target_id } = self else {
            return None;
        };
        Some(targets.get(target_id)?.label())
    }

    /// [`label`](Self::label) with the local-build form resolved against the
    /// configured `[build_targets]` (the chip and the picker use this).
    pub fn label_with(
        &self,
        targets: &std::collections::BTreeMap<String, awili_config::BuildTargetConfig>,
    ) -> String {
        if let Some(label) = self.local_build_label(targets) {
            return label;
        }
        self.label()
    }
}

/// Which executor a launch starts with. A parseable PERSISTED choice wins;
/// otherwise a parseable CONFIGURED seed (`[work].default_executor`);
/// otherwise `None` (the caller keeps Gateway). The two are parsed
/// independently on purpose: an unparseable persisted value must not
/// shadow a valid config seed (review 2026-09-06, finding 2).
pub fn seed_executor(persisted: Option<&str>, configured: Option<&str>) -> Option<WorkExecutor> {
    persisted
        .and_then(WorkExecutor::from_persist)
        .or_else(|| configured.and_then(WorkExecutor::from_persist))
}

/// P3c round 3 (R5): a Work prompt parked while a gateway Stop is pending —
/// the user's text as typed and the conversation it was typed in. It is
/// replayed through [`AwiliApp::submit_work_run`]'s normal path, which derives
/// the executor-facing task (fleet skills, folder instructions) and any system
/// preamble again at that moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueuedPrompt {
    /// The prompt exactly as submitted.
    pub(crate) text: String,
    /// The Work conversation the prompt was submitted in.
    pub(crate) conv_id: String,
    /// RC1-O-FIX4 (slice review P1): the helper the prompt was typed under.
    /// The replay runs on it, never on whatever chip is shown when the Stop
    /// is acknowledged.
    pub(crate) executor: WorkExecutor,
}

pub struct WorkState {
    pub hermes: HermesCfg,
    pub gateway: GatewayCfg,
    /// Which engine backs Work, and (for `kind = "awili"`) the endpoint it
    /// drives. Held as the config type directly — unlike `HermesCfg`/`GatewayCfg`
    /// there is no derived `enabled` flag to mirror.
    pub engine: awili_config::EngineConfig,
    /// The additional in-app engines (`[[engines]]`), in picker order. Empty =
    /// the single-engine behaviour. Resolved per executor by
    /// [`WorkState::effective_engine`]; the primary stays `engine`.
    pub(crate) extra_engines: Vec<awili_config::ExtraEngineConfig>,
    /// What executes the next run — in-app (default), the fleet gateway, an
    /// agent CLI, or a Build.
    pub executor: WorkExecutor,
    /// The executor picker popover is showing.
    pub executor_picker_open: bool,
    /// RC1-O: the chip the owner picked by hand in each conversation. A
    /// conversation with a pick goes back to it (pre-review 2 P1: pick Codex
    /// in B, open A, return to B — B is on Codex again); one without goes to
    /// its last run's lane.
    pub(crate) chip_picked: std::collections::HashMap<String, WorkExecutor>,
    /// RC2-R1F2 (usability RC2 round 1, N2): the helper the owner chose last
    /// — a chip pick, or the Default Work helper chosen in Settings. "+ New
    /// conversation" (and a conversation with neither a pick nor a run)
    /// starts on it; a one-shot local Build is never it. `None` until a
    /// choice is made: then the Default Work helper applies.
    pub(crate) last_pick: Option<WorkExecutor>,
    /// RC1-S: the folder the owner chose by hand in each conversation (this
    /// app session). Opening the conversation goes back to it; one without a
    /// pick goes back to its own folder (its `cwd`).
    pub(crate) folder_picked: std::collections::HashMap<String, PathBuf>,
    /// Resume eligibility across executor switches and interrupted turns.
    pub continuity: awili_code::continuity::ContinuityTracker,
    /// One persisted next-turn target and native-session map per stable
    /// Workshop conversation. This is the sole M2 selector state; it never
    /// owns a runner, transcript, approval or outcome ledger.
    pub(crate) conversation_routes:
        std::collections::HashMap<String, crate::conversation_route::ConversationRoute>,
    /// Raw unsupported/malformed route records remain persisted and explicit;
    /// callers must not replace them with an ambient default on reopen.
    pub(crate) unavailable_conversation_routes: std::collections::HashMap<String, String>,
    /// Immutable route owners for the existing Work/Code runs. They are not a
    /// second runner: they only let Stop and late completion settle the exact
    /// route snapshot that was accepted at dispatch.
    pub(crate) active_work_route_turn: Option<crate::conversation_route::ActiveTurn>,
    pub(crate) active_code_route_turns:
        std::collections::HashMap<u64, crate::conversation_route::ActiveTurn>,
    /// The kill switch for an in-flight agent Work run (mirrors Code tabs'
    /// stream_abort_tx); Cancel fires it and the child process group dies.
    pub agent_abort_tx: Option<std::sync::mpsc::Sender<()>>,
    /// S5 INT1-FIX1: an agent Work turn is waiting on its memory gate (no
    /// child yet, so no `agent_abort_tx`). A Stop in that window settles the
    /// turn itself (`cancel_work_run`).
    pub agent_gate_pending: bool,
    pub resume_gate_pending: bool,
    /// Attachments consumed from a Work tray for an accepted native turn that
    /// has not spawned yet. Stop/refusal returns only these ids to this owner;
    /// another conversation's tray is never touched.
    pub agent_gate_attachments: Option<(String, Vec<Attachment>)>,
    /// Agent-harness binary paths + search paths, from `[work.agents]`.
    pub agents: awili_config::AgentBinsConfig,
    /// rc.4 S7: `[work.sandbox]` (opt-in shell sandbox, default off).
    pub sandbox: awili_config::SandboxConfig,
    /// R2b: the Default Work helper (`[work].default_executor`) as last
    /// loaded; a new Work conversation starts on it when it parses.
    pub default_helper: Option<String>,
    /// The P12 connector sections (`[outline]` / `[gitea]`): the owned
    /// engine's synthesised MCP server entry, the vendor-CLI MCP flags, and
    /// the Work panel label all read from here.
    pub connectors: crate::connectors_mcp::ConnectorsSections,
    /// Model vocabularies from `[models]` (aliases, non-chat filters).
    pub models_cfg: awili_config::ModelsConfig,
    /// Local Build targets, from `[build_targets.<id>]` (daily-driver P2d) —
    /// the manual `LocalBuild` override resolves to one of these bindings;
    /// absent means the BUILD picker section is simply not offered.
    pub build_targets: std::collections::BTreeMap<String, awili_config::BuildTargetConfig>,
    /// Open local Build rows restored at startup (daily-driver P2d) — each is
    /// reattached by `status` of its SAME request id; cleared once driven to
    /// a terminal state.
    pub open_builds: Vec<awili_persist::build_jobs::BuildJobRow>,
    /// P2f: the local build the panel is driving, if any — Cancel run stops
    /// its poll task and closes its row (Fable ruling 3, option B).
    pub(crate) local_build: Option<crate::build_staleness::LocalBuildHandle>,
    /// Monotonic run identity (Codex night-review MUST-1): every submit and
    /// every cancel bumps it; async harness tasks capture their run's id and
    /// discard events/finalization once it is stale, so a cancelled run can
    /// never clear or finish a REPLACEMENT run's state.
    pub run_seq: u64,
    /// The chosen working folder, passed to Hermes as run context.
    pub folder: Option<PathBuf>,
    /// Top-level entries of the folder (in-scope files), read when it is picked.
    pub folder_files: Vec<String>,
    /// Folder contents snapshot taken at submit, to detect new OUTPUT files.
    pub folder_snapshot: HashSet<String>,
    /// RC2-R612: the folder's entries (size, change time) when an agent
    /// run started, so its footer can say what it changed (RC2-R6M3: at
    /// every depth, bounded; see `run_footer::dir_stamps`).
    pub(crate) folder_stamps: crate::run_footer::Stamps,
    /// RC2-R619: the in-app model's warm-ups (see `inapp_warm`).
    pub(crate) warm: crate::inapp_warm::WarmState,
    /// New top-level entries that appeared after the run (the OUTPUTS list).
    pub outputs: Vec<String>,
    pub panel: RunPanel,
    /// The latest transient owned error survives idle History restoration.
    pub(crate) error_presentation: Option<crate::inline_error::ErrorPresentation>,
    /// A run is in flight — submit is ignored and Cancel is offered.
    pub running: bool,
    /// When the in-flight run was submitted — used to fire a completion
    /// notification only for genuinely long-running tasks (Cowork "step away"),
    /// never for quick back-and-forth turns.
    pub run_started: Option<std::time::Instant>,
    /// The active gateway session ID (gateway transport only).
    pub session_id: Option<String>,
    /// Command channel to the persistent gateway worker (if running).
    pub gateway_cmd_tx: Option<std::sync::mpsc::Sender<GatewayCmd>>,
    /// P3c: a gateway Stop sent `Cancel` and the worker has not yet
    /// acknowledged it with `Terminal { Stopped }` (it is still interrupting
    /// and draining the stopped turn). A submit in this window is parked in
    /// [`WorkState::queued_after_stop`] and runs once the acknowledgement
    /// arrives (round 3, R5). Cleared by that acknowledgement and wherever the
    /// worker is discarded (R6), so it never outlives its worker.
    pub(crate) gateway_stop_pending: bool,
    /// P3c round 3 (R5): the one prompt submitted while a gateway Stop was
    /// pending (M1a: the first parked submit keeps its place — a second is
    /// refused back to the composer). The Stop's acknowledgement
    /// (`Terminal`/`Failed` reducer) takes it and submits it once.
    pub(crate) queued_after_stop: Option<QueuedPrompt>,
    /// RC3-S5: messages typed during a run, per conversation.
    pub(crate) send_queue: crate::send_queue::SendQueues,
    /// P3c round 4 (R9): the working folder changed while a gateway Stop was
    /// pending. The old worker is left alone until its acknowledgement; the
    /// ack reducer ([`end_gateway_stop_barrier`]) then sends it `Shutdown`,
    /// forgets its sender and session, and resets this marker, so the parked
    /// prompt respawns a worker in the new folder.
    pub(crate) folder_changed_during_stop: bool,
    /// M1a: bumped per sent gateway Stop; the ack deadline acts only on its
    /// own Stop.
    pub(crate) gateway_stop_gen: u64,
    /// Configuration of the actual persistent worker, frozen at construction.
    pub(crate) worker_identity: Option<WorkRunIdentity>,
    /// Trips the worker's event loop on Cancel (see [`AwiliApp::cancel_work_run`]).
    pub cancel: Arc<AtomicBool>,
    /// Scheduled jobs fetched from the gateway REST `/api/jobs`.
    pub jobs: Vec<ScheduledJob>,
    /// Saved projects (loaded from persistence at startup).
    pub projects: Vec<StoredProject>,
    pub(crate) project_revision: u64,
    pub scroll: ScrollHandle,
    /// Memory inspector: the last results — one canonical lane set per
    /// backend (P4b: lanes are rendered labeled and disjoint, never blended).
    /// RC3-S4: the rules/doc parts assembled for the last run, with the
    /// conversation they belong to. Live session only: it is not saved with
    /// the turn (the reply inspector reads recorded messages, which carry no
    /// context data).
    pub context_parts: Option<(String, Vec<crate::agent_launch::DocPart>)>,
    /// rc.4 S7: the sandbox line of the last agent run in a conversation
    /// (`agent_sandbox::status_line`), for the Job panel and Inspect reply.
    pub sandbox_note: Option<(String, String)>,
    pub memory_results: Option<MemoryInspectorResults>,
    /// Memory inspector: last recall error (surfaced instead of a silent "0").
    pub memory_error: Option<String>,
    /// Memory inspector: whether the panel is expanded.
    pub memory_open: bool,
    /// The current memory search query (shown in the header).
    pub memory_query: String,
    /// Connection status: None = untested, Some(Ok) = healthy, Some(Err) = error.
    pub hermes_status: Option<Result<String, String>>,
    /// Whether a connection test is in-flight.
    pub hermes_testing: bool,
    /// Available Hermes skills (fetched once on connection test).
    pub skills: Vec<awili_gateway::rest::Skill>,
    /// Pre-leaked skill IDs and labels (leaked once when skills are fetched,
    /// not on every render frame). Safe because skills are fetched once per
    /// connection test and the set is bounded (~100 entries, ~3KB total).
    pub skill_statics: Vec<(&'static str, &'static str)>,
    /// Whether the skills palette is expanded.
    pub skills_open: bool,
    /// Available Hermes toolsets (fetched once on connection test).
    pub toolsets: Vec<awili_gateway::rest::Toolset>,
    /// Recursive file tree for the working folder (legacy, kept for compat).
    pub file_tree: Vec<FileNode>,
    /// Which directories are expanded in the tree (by path string).
    pub tree_expanded: HashSet<String>,
    /// Whether the file tree is shown (collapsed by default until a folder is set).
    pub tree_open: bool,
    /// The new lazy workspace file tree (replaces the eager file_tree).
    pub workspace_tree: Option<crate::workspace::FileTreeState>,
    /// Live terminals in this workspace session.
    pub terminals: Vec<crate::terminal::TerminalState>,
    /// Next terminal ID counter.
    pub next_terminal_id: u64,
    /// The gpui terminal view entity (focusable, keyboard input).
    pub terminal_view: Option<gpui::Entity<crate::terminal_view::TerminalView>>,
    /// Code-mode tabs — one coding-agent CLI (claude/codex) per PTY terminal.
    pub code_tabs: Vec<CodeSession>,
    /// Explicit navigation invalidates a delayed history read, including
    /// selecting a blank tab that has no durable conversation ID yet.
    pub(crate) code_reopen_generation: u64,
    /// M2e: the live turn (sequence number + start) per Code tab id.
    pub(crate) code_runs: crate::code_run::CodeRuns,
    /// Index of the active Code tab in `code_tabs`.
    pub code_active: usize,
    /// Monotonic id source for Code tabs (stable across add/remove).
    pub next_code_id: u64,
    /// Selected folder, including unsaved or deleted rows; catalog edits never re-scope a turn.
    pub workshop_selected_folder: Option<String>,
    /// Index of the selected shared project (clamped on use).
    pub code_active_project: usize,
    /// RC2-UX (R2-15): the folders chosen lately (Work and Code), newest
    /// first, at most [`crate::palette_targets::RECENT_CAP`]; ⌘O lists them.
    pub recent_folders: Vec<String>,
    /// RC2-UX (R2-17): the open Code tabs as last saved (JSON), so a frame
    /// saves them only when they changed.
    pub code_tabs_saved: Option<String>,
    /// RC2-UX (S-N3): a "runs without asking" notice waiting for the next
    /// frame (left by a non-picker helper change).
    pub no_ask_notice: Option<String>,
    /// Live subagent reducer store (unit-18 `awili_gateway::subagents`), folded
    /// from `subagent.*` events. The AGENTS card renders `build_subagent_tree`
    /// over this flat store; the tree is derived at render time, not stored.
    pub subagents: Vec<SubagentProgress>,
    /// Whether the agents panel is expanded.
    pub agents_open: bool,
    /// Recent Hermes sessions (fetched from `/api/sessions`).
    pub hermes_sessions: Vec<HermesSession>,
    /// The Work conversation this run's output belongs to, captured at submit
    /// time. Work output is pushed here — NOT into `sidebar.active_id` — so a
    /// mid-run switch to Chat can never bleed agent tokens into a chat thread.
    pub conv_id: Option<String>,
    /// Index of the assistant row currently being streamed (live Work output).
    pub stream_row_idx: Option<usize>,
    /// RC2-UX (D-PUNCH S2-F15): a tool step or a card came between two parts
    /// of the reply text, so the next part starts a new paragraph (the parts
    /// ran together: "…the write.The folder…").
    pub text_break_due: bool,
    /// True until the Work worker emits its first transcript chunk. Local
    /// preflight text (such as the Ask-first advisory) does not clear it;
    /// gateway progress notices do because they are visible stream output.
    pub waiting_for_stream_output: bool,
    /// Preview rail: files the agent has touched during this run.
    pub touched_files: Vec<String>,
    /// The root captured by the job that produced the preview list. A later
    /// folder selection or a missing old folder must not retarget its links.
    pub preview_root: Option<PathBuf>,
    /// RC1-H1: Awili's own record of the files an in-app run changed (from
    /// the engine's tool results, never the model's words).
    pub(crate) inapp_files: inapp_files::InAppFiles,
    /// RC1-O-FIX8: the owner pressed Stop on the current run (not a Deny);
    /// its footer then claims only what was seen before the Stop. Cleared at
    /// each submit, with `inapp_files`.
    pub(crate) owner_stopped: bool,
    /// RC2-R61: Awili is quitting and is stopping this run itself (the
    /// reply then says the quit stopped it). Set only for that stop.
    pub(crate) quit_stop: bool,
    /// RC1-N: the owner pressed Deny on an in-app approval card. The turn is
    /// ending (`deny-stop`); what happens once the engine confirms it.
    pub(crate) deny_stopped: Option<DenyStop>,
    /// RC1-N-FIX2: a Stop discarded the in-app worker of this conversation
    /// while its turn ran; the worker publishes the turn's model history
    /// (its completed work kept) into this slot as it ends. The next
    /// dispatch keeps it before starting the new worker.
    /// By conversation (pre-review pass 5: a Stop in one conversation stays
    /// pending while another is used): (slot, how many transcript messages
    /// the stopped turn had reached at the Stop — the stopped history covers
    /// exactly those).
    pub(crate) stopped_histories:
        std::collections::HashMap<String, (awili_engine_worker::HistorySlot, usize)>,
    /// RC1-N-FIX2: releases the dispatch of (conversation) waiting on its
    /// stopped worker (a newer Stop, or that worker died without publishing).
    pub(crate) stopped_barrier: Option<(String, Arc<AtomicBool>)>,
    /// RC1-I2: the conversation the live worker was spawned for.
    pub(crate) worker_conv: Option<String>,
    /// RC1-I2-FIX2: the user left the in-app worker's conversation while its
    /// run was going; the worker (its session) ends when that run ends.
    pub(crate) retire_after_run: bool,
    /// RC1-J: where the live in-app worker publishes the model's history
    /// after each turn, with the conversation it serves.
    pub(crate) inapp_history_slot: Option<(String, awili_engine_worker::HistorySlot)>,
    /// RC1-J: each conversation's model history as of its last in-app turn
    /// (also kept on disk; read back on the first spawn after a relaunch).
    pub(crate) kept_histories: std::collections::HashMap<String, inapp_history::KeptHistory>,
    /// rc.3 S2: the prompt parts of the turn being assembled, closed and
    /// saved with its reply.
    /// rc.3 S2: the window each helper route last reported (by route), for
    /// the council's trim.
    pub(crate) reported_windows: std::collections::HashMap<String, u32>,
    /// rc.3 S2: the window the provider last reported for each Code tab.
    pub(crate) code_windows: std::collections::HashMap<u64, u32>,
    pub(crate) ctx_pending: Option<crate::context_breakdown::ContextBreakdown>,
    /// rc.3 S2: (executor, model) of the turn, to look up its context window.
    pub(crate) ctx_identity: Option<(String, String)>,
    /// rc.3 S2: what the owner's submit contributed, before any lane adds its own.
    pub(crate) ctx_draft: Option<crate::context_breakdown::ContextBreakdown>,
    /// Preview rail: URLs referenced in agent output.
    pub referenced_urls: Vec<String>,
    /// P8b: the graduation-ledger recorder (path from `[graduation].ledger`).
    pub graduation: crate::graduation_record::Recorder,
    /// U4 (D2): the per-conversation Ask/Auto table; session-only, seeded
    /// from `[work] default_approval_mode` at startup.
    pub(crate) ask_modes: crate::ask_mode::AskModes,
    /// U4 (D7): the mode cell the in-app engine reads at each approval
    /// gate; `dispatch_work_prompt` installs the conversation's choice into
    /// it before every engine build (U4d).
    pub(crate) approval_handle: awili_work::SharedApprovalMode,
    /// U4e1 (D10): the plan cards — which Ask planning steps are armed, which
    /// plans wait for Approve/Deny, and the one approved follow-up.
    pub(crate) plan_gates: crate::plan_gate::PlanGates,
    /// U4f: the open Ask/Auto mode menu (at most one open at a time; the chip
    /// click opens it, the Auto row escalates it to the confirmation).
    pub(crate) ask_menu: crate::ask_mode_ui::AskMenu,
    /// U2c: gateway reachability + the visible-silence clock.
    pub(crate) health: crate::gateway_health_app::HealthWatch,
    /// L1e2: the in-app model's health, the parked send and the down card.
    pub(crate) local: crate::local_model_health::LocalModelHealth,
}

impl WorkState {
    /// The engine the selected executor runs on (kind set for its lane): the
    /// primary `[engine]`, or the `[[engines]]` entry of an `InAppExtra`
    /// executor. An unknown extra id gives a blank, unconfigured engine
    /// (see [`crate::local_first::effective_engine`]), never the primary.
    pub(crate) fn effective_engine(&self) -> awili_config::EngineConfig {
        crate::local_first::effective_engine(&self.executor, &self.engine, &self.extra_engines)
    }

    /// The id of the selected in-app extra engine when it is not configured
    /// (a restored conversation naming a removed `[[engines]]` entry).
    pub(crate) fn unknown_extra_engine(&self) -> Option<&str> {
        match &self.executor {
            WorkExecutor::InAppExtra { id } if !self.extra_engines.iter().any(|e| &e.id == id) => {
                Some(id)
            }
            _ => None,
        }
    }
}

impl WorkState {
    pub fn new(hermes: HermesCfg, gateway: GatewayCfg, engine: awili_config::EngineConfig) -> Self {
        Self {
            hermes,
            gateway,
            engine,
            extra_engines: Vec::new(),
            folder: None,
            folder_files: Vec::new(),
            folder_snapshot: HashSet::new(),
            folder_stamps: Default::default(),
            warm: Default::default(),
            outputs: Vec::new(),
            panel: RunPanel::default(),
            error_presentation: None,
            graduation: crate::graduation_record::Recorder::default(),
            health: Default::default(),
            local: Default::default(),
            running: false,
            run_started: None,
            session_id: None,
            gateway_cmd_tx: None,
            gateway_stop_pending: false,
            queued_after_stop: None,
            send_queue: Default::default(),
            folder_changed_during_stop: false,
            gateway_stop_gen: 0,
            worker_identity: None,
            cancel: Arc::new(AtomicBool::new(false)),
            jobs: Vec::new(),
            projects: Vec::new(),
            project_revision: 0,
            scroll: ScrollHandle::new(),
            context_parts: None,
            sandbox_note: None,
            memory_results: None,
            memory_error: None,
            memory_open: false,
            memory_query: String::new(),
            hermes_status: None,
            hermes_testing: false,
            skills: Vec::new(),
            skill_statics: Vec::new(),
            skills_open: false,
            toolsets: Vec::new(),
            file_tree: Vec::new(),
            tree_expanded: HashSet::new(),
            tree_open: false,
            workspace_tree: None,
            terminals: Vec::new(),
            next_terminal_id: 1,
            terminal_view: None,
            code_tabs: Vec::new(),
            code_reopen_generation: 0,
            code_runs: Default::default(),
            code_active: 0,
            next_code_id: 1,
            workshop_selected_folder: None,
            code_active_project: 0,
            recent_folders: Vec::new(),
            code_tabs_saved: None,
            no_ask_notice: None,
            subagents: Vec::new(),
            agents_open: true,
            hermes_sessions: Vec::new(),
            conv_id: None,
            executor: WorkExecutor::InApp,
            executor_picker_open: false,
            chip_picked: std::collections::HashMap::new(),
            last_pick: None,
            folder_picked: std::collections::HashMap::new(),
            continuity: awili_code::continuity::ContinuityTracker::default(),
            conversation_routes: std::collections::HashMap::new(),
            unavailable_conversation_routes: std::collections::HashMap::new(),
            active_work_route_turn: None,
            active_code_route_turns: std::collections::HashMap::new(),
            agent_abort_tx: None,
            agent_gate_pending: false,
            resume_gate_pending: false,
            agent_gate_attachments: None,
            agents: awili_config::AgentBinsConfig::default(),
            sandbox: awili_config::SandboxConfig::default(),
            default_helper: None,
            connectors: crate::connectors_mcp::ConnectorsSections::default(),
            models_cfg: awili_config::ModelsConfig::default(),
            build_targets: std::collections::BTreeMap::new(),
            open_builds: Vec::new(),
            local_build: None,
            run_seq: 0,
            stream_row_idx: None,
            text_break_due: false,
            waiting_for_stream_output: false,
            touched_files: Vec::new(),
            preview_root: None,
            inapp_files: Default::default(),
            owner_stopped: false,
            quit_stop: false,
            deny_stopped: None,
            stopped_histories: std::collections::HashMap::new(),
            stopped_barrier: None,
            worker_conv: None,
            retire_after_run: false,
            inapp_history_slot: None,
            kept_histories: std::collections::HashMap::new(),
            reported_windows: std::collections::HashMap::new(),
            code_windows: std::collections::HashMap::new(),
            ctx_pending: None,
            ctx_identity: None,
            ctx_draft: None,
            referenced_urls: Vec::new(),
            ask_modes: Default::default(),
            approval_handle: Default::default(),
            plan_gates: Default::default(),
            ask_menu: Default::default(),
        }
    }
}

/// A message from the run worker thread back to the GPUI thread.
#[derive(Debug)]
pub enum WorkUpdate {
    /// RC2-GEMMA5 (R7-A3): the in-app engine asks the model again after the
    /// server cut its reply off; this text, shown at the end of the reply so
    /// far, is taken back.
    ReplyRestart(String),
    ApprovalDelivery {
        id: Option<String>,
        delivered: bool,
    },
    /// Session created / run started; carries the session or run ID.
    Started(String),
    /// One typed lifecycle event off the event stream.
    Event(RunEvent),
    /// RC2-RUN (S-Q1): the model is thinking (`done: false`) or has
    /// finished thinking — Awili's own "Thinking" row, a typed row kind,
    /// never matched by name against a tool the owner may have named
    /// "reasoning".
    Reasoning {
        done: bool,
    },
    /// A raw `subagent.*` event, folded into `WorkState::subagents` by the
    /// unit-18 reducer on the UI thread (boxed — the enum is large).
    Subagent(Box<GatewayEventKind>),
    /// The terminal status poll — the fail-closed source of truth for success.
    Terminal {
        status: RunStatus,
        output: Option<String>,
        model: Option<String>,
    },
    /// The model name was detected from a gateway event (message.start).
    ModelDetected(String),
    /// A gateway turn completed (message.complete received). The session stays
    /// open for multi-turn — unlike Terminal, this doesn't end the worker.
    TurnComplete,
    /// A text token streamed from the gateway (message.delta).
    Token(String),
    /// The gateway-reported dollar cost for the turn (Hermes sends `cost_usd` on
    /// message.complete). Fed straight into the cost ticker — Awili has no Hermes
    /// token pricing, so this is the only real cost signal for Work mode.
    ReportedCost(f64),
    /// RC1-F4: the turn's token counts (the run footer and the ledger).
    ReportedUsage(awili_providers::Usage),
    /// Submit, streaming, or polling failed at the transport level.
    Failed(String),
    /// The agent is requesting terminal screen text (blocking-refusal).
    TerminalReadRequest {
        request_id: String,
    },
    /// The agent is blocking on a sudo password (`sudo.request`). Carries only
    /// the request id — never a secret value — so it is safe to Debug-log.
    SudoRequest {
        request_id: String,
    },
    /// The agent is blocking on a credential value (`secret.request`) for an
    /// environment variable. Carries the env-var name and prompt text, never the
    /// value the user would supply.
    SecretRequest {
        request_id: String,
        env_var: String,
        prompt: String,
    },
    /// Preview rail: a file was touched by a tool.
    FileTouched(String),
    /// Preview rail: a URL was referenced in output.
    UrlReferenced(String),
}

// ============================================================================
// Subagent state (Cowork parity — live agent hierarchy)
// ============================================================================
//
// The `subagent.*` fold now lives entirely in `awili_gateway::subagents` (unit
// 18): the worker adapter ships each raw `Subagent` event and the UI thread
// folds it into `WorkState::subagents` via `upsert_subagent`. The AGENTS card
// (unit 19) renders `build_subagent_tree` over that store — status glyph,
// per-agent timer, files read/written, and a token/cost header.

// ============================================================================
// Top-level folder snapshot retained for job refresh compatibility.
// ============================================================================

#[derive(Clone, Debug)]
pub struct FileNode {
    pub name: String,
    pub is_dir: bool,
}

impl FileNode {
    fn build(path: &Path) -> Vec<FileNode> {
        let Ok(rd) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        let mut entries: Vec<FileNode> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    return None;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir && IGNORED_DIRS.contains(&name.as_str()) {
                    return None;
                }
                Some(FileNode { name, is_dir })
            })
            .collect();
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        entries
    }
}

const IGNORED_DIRS: &[&str] = &[
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
];

/// Snapshot top-level visible names; title Files owns lazy navigation.
pub fn folder_tree_snapshot(root: &Path) -> Vec<FileNode> {
    FileNode::build(root)
}

/// Read up to `limit` top-level entries of a folder, directories suffixed `/`,
/// sorted. Errors (unreadable folder) yield an empty list — never a panic.
pub(crate) fn list_dir_top(path: &Path, limit: usize) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(path) {
        Ok(rd) => rd
            .flatten()
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    format!("{name}/")
                } else {
                    name
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names.truncate(limit);
    names
}

/// The set of top-level entry names of a folder (for output-diffing). Empty on error.
fn dir_snapshot(path: &Path) -> HashSet<String> {
    match std::fs::read_dir(path) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
        Err(_) => HashSet::new(),
    }
}

/// The wireframe type icon for a filename in the outputs / touched-files
/// lists. One vocabulary app-wide — see [`crate::icons::Icon::for_extension`].
fn file_badge(name: &str) -> crate::icons::Icon {
    let ext = name.rsplit('.').next().unwrap_or("");
    crate::icons::Icon::for_extension(ext)
}

/// Resolve a coding-agent CLI binary to an absolute path, checking the common
/// install locations first (GUI-launched apps often have a minimal PATH that
/// omits Homebrew), then falling back to the bare name for PATH resolution.
/// A sliding toggle switch (track + knob). `on` = right/accent, off = left/muted.
/// `on_click` is a view listener (from `cx.listener`). Used for the Code-mode
/// The model options a coding-agent engine can run, grounded in the vendor's
/// own local state (never a network call, never a guess at unreleased names):
/// Claude Code's documented aliases; Codex's one-time local candidate catalog
/// plus configured models; ZCode's `~/.zcode/cli/config.json` provider model
/// lists. First entry after `None` in the cycle. An unreadable catalog yields
/// just the configured choices (or the engine default when those are absent).
pub(crate) fn agent_model_options(
    engine: CodeEngine,
    cfg: &awili_config::ModelsConfig,
) -> Vec<String> {
    match engine {
        // From `[models].claude_aliases` — vendor naming is data, not code.
        CodeEngine::Claude => cfg.claude_aliases.clone(),
        CodeEngine::Codex => crate::codex_catalog::model_candidates(),
        // zcode 0.16.3 has NO per-run model mechanism: its help documents
        // `--settings` but the parser rejects it (verified 2026-08-23 — the
        // flag made every run fail with printed help). GLM runs on the model
        // configured inside ZCode itself (/model there); offering rows here
        // would silently lie.
        CodeEngine::Glm => Vec::new(),
        // dsh (P9): the headless profile has no model flag; the selectable
        // models are DATA in `[models].dsh_models` and the chosen one travels
        // to the child as `DSH_MODEL`. Vendor names are data — a new DeepSeek
        // model must not require rebuilding Awili.
        CodeEngine::Dsh => cfg.dsh_models.clone(),
    }
}

/// The AGENTS section rows the Work executor popover offers: for EACH engine,
/// one "<label> · default" row plus one "<label> · <model>" row per entry of
/// that engine's [`agent_model_options`]. Pure (driven by the model
/// vocabularies in config) so the Dsh rows are unit-testable without a window
/// (P9 proof). The four engines are Claude, Codex, GLM, and Dsh — in that
/// order — so a Dsh conversation keeps its own section.
pub(crate) fn work_executor_agent_rows(
    models: &awili_config::ModelsConfig,
) -> Vec<(String, WorkExecutor)> {
    let mut rows: Vec<(String, WorkExecutor)> = Vec::new();
    for engine in [
        CodeEngine::Claude,
        CodeEngine::Codex,
        CodeEngine::Glm,
        CodeEngine::Dsh,
    ] {
        rows.push((
            format!("{} \u{00B7} default", engine.label()),
            WorkExecutor::Agent {
                engine,
                model: None,
            },
        ));
        for model in agent_model_options(engine, models) {
            rows.push((
                format!("{} \u{00B7} {model}", engine.label()),
                WorkExecutor::Agent {
                    engine,
                    model: Some(model),
                },
            ));
        }
    }
    rows
}

/// Resolve an agent-harness binary. An explicit `[work.agents].*_bin` wins;
/// otherwise `name` is looked up in the configured search paths (a
/// Finder-launched GUI app inherits no shell `PATH`, which is why discovery
/// exists); finally the bare name is returned for `PATH` resolution.
pub(crate) fn resolve_agent_bin(name: &str, cfg: &awili_config::AgentBinsConfig) -> String {
    let explicit = match name {
        "claude" => cfg.claude_bin.as_deref(),
        "codex" => cfg.codex_bin.as_deref(),
        "zcode" => cfg.zcode_bin.as_deref(),
        // dsh (P9): an explicit `[work.agents].dsh_bin` wins; otherwise the
        // search paths (which include ~/.local/bin, where the MacBook's dsh
        // lives) and finally the bare name for PATH resolution.
        "dsh" => cfg.dsh_bin.as_deref(),
        _ => None,
    };
    if let Some(p) = explicit.map(str::trim).filter(|p| !p.is_empty()) {
        return expand_home(p);
    }
    // Windows: npm's `.cmd` shims, `%LOCALAPPDATA%\Programs`, `~/.local/bin`
    // and `PATH` with `PATHEXT` (std's own PATH lookup only appends `.exe`).
    #[cfg(windows)]
    {
        let dirs: Vec<PathBuf> = cfg
            .search_paths
            .iter()
            .map(|d| PathBuf::from(expand_home(d)))
            .collect();
        if let Some(found) = crate::agent_bin::resolve_windows(name, &dirs) {
            return found.to_string_lossy().into_owned();
        }
    }
    #[cfg(not(windows))]
    for dir in &cfg.search_paths {
        let c = format!("{}/{name}", expand_home(dir).trim_end_matches('/'));
        if std::path::Path::new(&c).exists() {
            return c;
        }
    }
    name.to_string()
}

/// `~` expansion for configured paths — a config file is written by humans,
/// who write `~/bin`, and no shell is involved to expand it.
pub(crate) fn expand_home(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => {
            let home = crate::home::home_string();
            format!("{home}/{rest}")
        }
        None => p.to_string(),
    }
}

impl AwiliApp {
    /// Enter a page (Workshop, Code, Council, or a Backstage page).
    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.close_workshop_archive(cx);
        if self.onboarding.is_active() && mode != Mode::Work {
            return;
        }
        self.shell.more = false;
        self.shell.tabs.menu = None;
        if crate::consolidation::workshop_mode(mode) {
            self.shell.workshop = mode;
        }
        if crate::consolidation::backstage_index(mode).is_some() {
            if self.shell.backstage != mode {
                self.shell.backstage_scroll.set_offset(Default::default());
            }
            self.shell.backstage = mode;
        }
        self.remember_last_mode(mode);
        if self.mode != mode {
            // A Code tab already converted to a conversation backing is the
            // same Workshop owner changing adapters. Keep it selected while
            // entering Work; ordinary Code -> Work navigation still restores
            // the last Work thread below.
            let preserve_converted_code_owner = mode == Mode::Work
                && self.mode == Mode::Code
                && matches!(
                    self.workshop_backing(),
                    Some(crate::workshop::Backing::Conversation(ref id))
                        if self.sidebar.active_id() == id
                            && self.conversations.get(id).is_some_and(|c| c.kind == ConvKind::Code)
                );
            self.cancel_composer_navigation();
            self.work.code_reopen_generation = self.work.code_reopen_generation.wrapping_add(1);
            self.mode = mode;
            // RC2-R54: a note about the page left behind goes with it.
            self.toast.leave_page(mode);
            // R2g: a cold launch comes back here.
            self.remember_last_mode(mode);
            // Entering the mode restores (or creates) the thread of its kind.
            // Utility modes don't touch the active conversation.
            match mode {
                Mode::Work if !preserve_converted_code_owner => {
                    self.activate_kind(ConvKind::Work, cx)
                }
                Mode::Work => {}
                // Code mode lazily spawns a coding-agent CLI terminal (claude).
                Mode::Code => self.ensure_code_tab(cx),
                // RC2-CNCL5: Council reopens the council used last (or a new
                // one seated with the last group).
                Mode::Council => self.activate_kind(ConvKind::Council, cx),
                Mode::Design => {}
                // Fetch from the live Hermes connection on view-open so these
                // panes share Work mode's gateway state instead of rendering a
                // never-populated default (P0.4).
                Mode::Skills => self.refresh_skills(cx),
                Mode::Cron => self.refresh_cron(cx),
                Mode::Sessions => self.refresh_sessions_panel(cx),
                Mode::CommandCenter => self.refresh_command_center(cx),
                Mode::Messaging => self.refresh_messaging(cx),
            }
            // RC2-R58: a Hermes page says the gateway's real state, never
            // "not checked yet".
            if matches!(
                mode,
                Mode::Skills | Mode::Cron | Mode::CommandCenter | Mode::Messaging
            ) && self.work.gateway.enabled
            {
                self.start_gateway_probe(cx);
            }
            if mode == Mode::Work {
                self.enter_work_surface(cx);
            } else if mode == Mode::Council {
                self.composer.update(cx, |c, _cx| {
                    c.set_placeholder(crate::council_view::COMPOSER_PLACEHOLDER)
                });
            } else {
                self.composer.update(cx, |c, _cx| {
                    c.set_placeholder(crate::theme::COMPOSER_PLACEHOLDER);
                });
            }
            // RC2-UX (D-PUNCH S3-F10): the ⌘F panel belongs to the page it was
            // opened on; it closes (and its highlight goes out) on a page change,
            // so it never takes keys meant for the new page.
            if self.search.open {
                self.search.close();
                self.highlight_term =
                    crate::search::next_highlight(crate::search::HighlightEvent::Clear);
            }
            // M2g: a mode change resets the prompt-history walk. The
            // conversation tail is re-derived per mode, and the session
            // candidate list is no longer valid for the new mode.
            self.prompt_history.reset();
            self.composer.update(cx, |c, cx| c.set_recalled(None, cx));
            // RC2-UX (R2-18): the box moves to the conversation on screen now.
            self.sync_composer_draft(cx);
            self.sync_workshop_tabs();
            cx.notify();
        }
    }

    /// What entering the Workshop page does: the gateway refresh, the local
    /// model check and the composer placeholder. The app opens on Work, so a
    /// cold launch runs this from the landing too, not only from `set_mode`.
    pub(crate) fn enter_work_surface(&mut self, cx: &mut Context<Self>) {
        if matches!(self.work.executor, WorkExecutor::Gateway) {
            if self.work.jobs.is_empty() {
                self.refresh_jobs(cx);
            }
            if self.work.hermes_sessions.is_empty() {
                self.refresh_sessions(cx);
            }
            if self.work.hermes_status.is_none() && !self.work.hermes_testing {
                self.test_hermes_connection(cx);
            }
        }
        // L1e2: entering Work with in-app selected checks the local model.
        if matches!(self.work.executor, WorkExecutor::InApp)
            && !self.work.local.health.is_fresh(std::time::Instant::now())
        {
            self.start_local_probe(cx);
        }
        let placeholder = if let Some(f) = &self.work.folder {
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| f.display().to_string());
            format!("Tell Awili what to do in {name}…")
        } else {
            "Tell Awili what to do…".to_string()
        };
        self.composer
            .update(cx, |c, _cx| c.set_placeholder(placeholder));
    }

    /// Ensure at least one Code tab exists (a Claude tab by default) when Code
    /// mode is first opened. Native conversation, not a terminal.
    pub(crate) fn ensure_code_tab(&mut self, _cx: &mut Context<Self>) {
        self.ensure_code_project();
        if self.work.code_tabs.is_empty() {
            let (engine, model) = match self.work.executor.clone() {
                WorkExecutor::Agent { engine, model } => (engine, model),
                // A LocalBuild override (P2d) never opens a Code tab as its
                // engine: the Code lane falls back to the default Claude.
                WorkExecutor::InApp
                | WorkExecutor::InAppExtra { .. }
                | WorkExecutor::Gateway
                | WorkExecutor::LocalBuild { .. } => (CodeEngine::Claude, None),
            };
            self.new_code_tab(engine);
            let tab = self.work.code_tabs.last_mut().unwrap();
            tab.model = model.clone();
            tab.route
                .set_next_target(WorkExecutor::Agent { engine, model });
        }
    }

    /// Anchor the first native tab to its selected or fallback folder without
    /// creating a saved project or a recent-folder entry. Folder selection
    /// does not supply execution approval.
    fn ensure_code_project(&mut self) {
        if self.work.workshop_selected_folder.is_none() {
            let folder = self.fallback_code_folder().display().to_string();
            self.work.workshop_selected_folder = Some(folder);
            self.persist_workshop_project_selection();
        }
    }

    /// Open a new native Code tab for the given engine and make it active.
    pub(crate) fn new_code_tab(&mut self, engine: CodeEngine) {
        self.work.code_reopen_generation = self.work.code_reopen_generation.wrapping_add(1);
        let id = self.work.next_code_id;
        self.work.next_code_id += 1;
        self.work.code_tabs.push(CodeSession {
            id,
            engine,
            route: crate::conversation_route::ConversationRoute::new(WorkExecutor::Agent {
                engine,
                model: None,
            }),
            convo: ConversationState::default(),
            rows: Vec::new(),
            reply_presentation_token: crate::conv_view::TurnToken::fresh(),
            errored: std::collections::HashSet::new(),
            streaming: false,
            context_gap: false,
            session_slot: std::sync::Arc::new(std::sync::Mutex::new(None)),
            project_folder: None,
            model: None,
            scroll: gpui::ScrollHandle::new(),
            retry_focus: None,
            retry_reveal_key: std::rc::Rc::default(),
            retry_bounds: std::rc::Rc::new(std::cell::Cell::new(None)),
            stream_abort_tx: None,
            host_approval: None,
            approval_session: Default::default(),
            history: Default::default(),
        });
        self.work.code_active = self.work.code_tabs.len() - 1;
    }

    /// The fallback Code folder when no project is selected: bound Work folder,
    /// else $HOME, else cwd.
    fn fallback_code_folder(&self) -> PathBuf {
        self.work
            .folder
            .clone()
            .or_else(|| crate::home::home_opt().map(PathBuf::from))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// The saved row for the selected folder; an unsaved selection has no row.
    pub(crate) fn active_code_project(&self) -> Option<&StoredProject> {
        if let Some(folder) = &self.work.workshop_selected_folder {
            self.work.projects.iter().find(|p| &p.folder == folder)
        } else {
            self.work.projects.get(self.work.code_active_project)
        }
    }

    /// The explicit project used by the native header/provider, with no
    /// broad HOME/cwd fallback for an owned-engine identity to inherit.
    pub(crate) fn selected_code_folder(&self) -> Option<PathBuf> {
        self.work
            .workshop_selected_folder
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                self.active_code_project()
                    .filter(|p| !p.folder.trim().is_empty())
                    .map(|p| PathBuf::from(&p.folder))
            })
    }

    /// The native working directory retains its established unbound fallback.
    pub(crate) fn code_cwd(&self) -> PathBuf {
        self.selected_code_folder()
            .unwrap_or_else(|| self.fallback_code_folder())
    }

    /// Select a folder immediately; publish a saved row only after worker ack.
    pub(crate) fn add_code_project(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.remember_recent_folder(&folder);
        let folder_str = folder.display().to_string();
        if let Some(i) = self
            .work
            .projects
            .iter()
            .position(|p| p.folder == folder_str)
        {
            self.switch_code_project(i);
        } else {
            self.work.workshop_selected_folder = Some(folder_str);
            self.persist_workshop_project_selection();
            self.save_project_folder(folder, cx);
            self.follow_project_in_code_tab();
        }
        cx.notify();
    }

    /// Switch the active project by index.
    pub(crate) fn switch_code_project(&mut self, index: usize) {
        if index < self.work.projects.len() {
            self.work.workshop_selected_folder = Some(self.work.projects[index].folder.clone());
            self.work.code_active_project = index;
            self.persist_workshop_project_selection();
            self.follow_project_in_code_tab();
        }
    }

    /// RC2-UX (R2-17, round 2 Minor 8; D-R1 M9c): the Code tab on screen
    /// follows the project just picked. A tab that has not run yet takes the
    /// project as its own (so switching tabs and back keeps it); a tab that
    /// already worked in another folder keeps its transcript and folder, and
    /// the project opens in a fresh tab on the same helper (a blank tab is
    /// reused) — never the old transcript under a new folder name.
    pub(crate) fn follow_project_in_code_tab(&mut self) {
        let folder = self.code_cwd().display().to_string();
        let active = self.work.code_active;
        let Some(tab) = self.work.code_tabs.get(active) else {
            return;
        };
        // Work-adapter turns live in the shared view; native rows can be a
        // stale empty snapshot until the next native turn. A known identity
        // whose view is unloaded is not evidence of an unsent conversation.
        let blank = tab.rows.is_empty()
            && !tab.streaming
            && tab.history.conv_id.as_deref().is_none_or(|id| {
                self.views
                    .get(id)
                    .is_some_and(|v| v.convo.messages().is_empty())
            });
        if tab.project_folder.as_deref() == Some(folder.as_str()) {
            return;
        }
        if blank {
            if let Some(id) = self.work.code_tabs[active].history.conv_id.clone() {
                // This is an explicit owner pick on an unsent conversation,
                // including one already materialized by an attachment.
                let tab_id = self.work.code_tabs[active].id;
                self.sync_code_identity_folder(tab_id, &id, Some(folder));
            } else {
                self.work.code_tabs[active].project_folder = Some(folder);
            }
            return;
        }
        if tab.project_folder.is_none() {
            return;
        }
        let engine = tab.engine;
        let model = tab.model.clone();
        let ix = match self
            .work
            .code_tabs
            .iter()
            .position(|t| t.rows.is_empty() && !t.streaming && t.history.conv_id.is_none())
        {
            Some(ix) => ix,
            None => {
                self.new_code_tab(engine);
                let ix = self.work.code_tabs.len() - 1;
                self.work.code_tabs[ix].model = model.clone();
                self.work.code_tabs[ix]
                    .route
                    .set_next_target(WorkExecutor::Agent { engine, model });
                ix
            }
        };
        self.work.code_tabs[ix].project_folder = Some(folder);
        self.select_code_tab(ix);
    }

    /// Persist stable shared selection. The legacy JSON is never rewritten.
    pub(crate) fn persist_workshop_project_selection(&self) {
        if let (Some(persist), Some(folder)) = (
            self.persist.as_ref(),
            self.work.workshop_selected_folder.as_ref(),
        ) {
            persist.set_state(
                crate::persistence::WORKSHOP_ACTIVE_PROJECT_KEY,
                folder.clone(),
            );
        }
    }

    pub(crate) fn apply_project_snapshot(&mut self, snapshot: crate::persistence::ProjectSnapshot) {
        if snapshot.revision > self.work.project_revision {
            self.work.project_revision = snapshot.revision;
            self.replace_workshop_projects(snapshot.projects);
        }
    }

    /// Refresh the single project mirror while keeping selection by identity.
    pub(crate) fn replace_workshop_projects(&mut self, projects: Vec<StoredProject>) {
        let selected = self.work.workshop_selected_folder.clone().or_else(|| {
            self.work
                .code_tabs
                .get(self.work.code_active)
                .and_then(|tab| tab.project_folder.clone())
        });
        self.work.projects = projects;
        self.work.code_active_project = selected
            .as_deref()
            .and_then(|folder| self.work.projects.iter().position(|p| p.folder == folder))
            .unwrap_or(0);
        self.work.workshop_selected_folder = selected;
        self.persist_workshop_project_selection();
    }

    /// The full-window native Code surface: tab bar + engine/autonomy toggles +
    /// the active tab's conversation transcript + the shared composer.
    pub(crate) fn code_center(
        &mut self,
        header: Option<gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.work.code_active;
        let compact = header.is_some();

        // The shared Workshop registry owns the tab strip.

        // Transcript: the active tab's rows as native markdown bubbles.
        let retry_focus = self.work.code_tabs.get_mut(active).map(|session| {
            session
                .retry_focus
                .get_or_insert_with(|| cx.focus_handle())
                .clone()
        });
        let transcript = self.work.code_tabs.get(active).map(|session| {
            let retry_focus = retry_focus.clone();
            let retry_reveal_key = session.retry_reveal_key.clone();
            let retry_bounds = session.retry_bounds.clone();
            let retry_scroll = session.scroll.clone();
            let reveal_focus = retry_focus.clone();
            let reveal_bounds = retry_bounds.clone();
            let reveal_key = retry_reveal_key.clone();
            let reveal_scroll = retry_scroll.clone();
            let mut col = div()
                .on_children_prepainted(move |_, window, cx| {
                    let Some(focus) = reveal_focus.as_ref() else {
                        reveal_key.set(None);
                        return;
                    };
                    if !focus.is_focused(window) {
                        reveal_key.set(None);
                        return;
                    }
                    let (Some(face), viewport) = (reveal_bounds.get(), reveal_scroll.bounds()) else {
                        reveal_key.set(None);
                        return;
                    };
                    let offset = reveal_scroll.offset();
                    // Bounds are measured in the tracked scroll's current paint
                    // space. Remove the scroll delta so ordinary wheel movement
                    // does not look like a new layout and snap the reader back.
                    let normalized = gpui::Bounds::new(
                        gpui::point(face.origin.x, face.origin.y - offset.y),
                        face.size,
                    );
                    let key = (normalized, viewport.size);
                    if reveal_key.get() == Some(key) {
                        return;
                    }
                    reveal_key.set(Some(key));
                    let focused = focus.clone();
                    let reveal_key = reveal_key.clone();
                    let reveal_bounds = reveal_bounds.clone();
                    let reveal_scroll = reveal_scroll.clone();
                    window.defer(cx, move |window, _cx| {
                        if reveal_key.get() != Some(key)
                            || !focused.is_focused(window)
                        {
                            return;
                        }
                        let Some(face) = reveal_bounds.get() else {
                            return;
                        };
                        let viewport = reveal_scroll.bounds();
                        let mut offset = reveal_scroll.offset();
                        if face.top() < viewport.top() {
                            offset.y += viewport.top() - face.top();
                        } else if face.bottom() > viewport.bottom() {
                            offset.y += viewport.bottom() - face.bottom();
                        }
                        offset.y = offset
                            .y
                            .clamp(-reveal_scroll.max_offset().y, gpui::Pixels::ZERO);
                        if offset != reveal_scroll.offset() {
                            reveal_scroll.set_offset(offset);
                            window.refresh();
                        }
                    });
                })
                .id("code-transcript")
                .debug_selector(|| "awili-code-transcript-viewport".into())
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_8))
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&session.scroll)
                .px(theme::z(theme::SP_12))
                .py(theme::z(theme::SP_8))
                .children(header);
            for (idx, row) in session.rows.iter().enumerate() {
                // Activity fold above the bubble (critique P2) — the tool
                // steps collapse to one line instead of interleaving.
                let tab_id_for_fold = session.id;
                col = col.child(crate::render_code_bubble(
                    session.id,
                    idx,
                    row,
                    session.errored.contains(&idx),
                ));
                if session.errored.contains(&idx) {
                    if let Some((cause, run_id)) = self.code_run_errors.get(&(session.id, idx)) {
                        col = col.child(self.code_error_card(session.id, idx, cause, run_id, cx));
                    }
                }
                // Below the bubble (ocr review R5): expansion grows under
                // the reading anchor.
                col = col.children(crate::activity_fold(
                    ("code-activity", idx),
                    row,
                    cx.listener(move |this, _ev, _w, cx| {
                        this.toggle_code_row_activity(tab_id_for_fold, idx, cx);
                    }),
                ));
                // M2f: the run footer under a finished reply (no-op selector in release).
                col = col.children(row.footer.clone().map(|t| {
                    crate::run_footer::footer_line(t)
                        .debug_selector(move || format!("code-footer-{idx}"))
                }));
                if row.role == Role::Assistant && !row.raw.is_empty() {
                    if let Some(owner) = session.history.conv_id.as_deref() {
                        col = col.children(self.reply_inspect_control(owner, idx, cx));
                    }
                }
            }
            // M2g: the Code-tab Retry chip sits below the last bubble.
            // Visible only when the tab is idle AND the convo has a
            // re-sendable user row (i.e. the last row is an assistant reply
            // to a user prompt). Click re-sends the user row to the tab's
            // engine — `submit_code_turn` — on the current tab (so a tab
            // switch is honored).
            if !session.streaming && crate::prompt_history::retry_text(&session.convo).is_some() {
                let engine_label = session.engine.label();
                let session_id = session.id;
                col = col.child(
                    crate::ui::Button::new("code-retry", format!("Retry on {engine_label}"))
                        .quiet()
                        .icon(crate::icons::Icon::RefreshCw)
                        .selector("awili-code-retry")
                        .focus_handle(retry_focus.as_ref().expect("retry focus initialized"))
                        .bounds_handle(retry_bounds.clone())
                        .tooltip("Send your message again, to the helper shown in the chip")
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            this.retry_code(cx);
                            let retry_started = this
                                .work
                                .code_tabs
                                .get(this.work.code_active)
                                .is_some_and(|session| session.id == session_id && session.streaming);
                            if retry_started {
                                this.focus_composer(window, cx);
                            }
                        })),
                );
            }
            if session.rows.is_empty() {
                col = col.child(crate::ui::EmptyState::new("workshop-empty", "Awili", "Ask a question or tell Awili what to do. Choose a folder for work on your files."));
            }
            col
        });

        div()
            .flex()
            .flex_col()
            .flex_1()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(if compact {
                        theme::target()
                    } else {
                        gpui::Pixels::ZERO
                    })
                    .children(transcript),
            )
            .children(self.code_host_approval_card(cx))
            .children(self.code_plan_gate_card(cx))
            // M2e: the running strip (what runs, elapsed, Stop).
            .children(self.code_run_strip(cx))
            .children((!self.staged_attachments.is_empty()).then(|| self.attachment_tray(cx)))
            .child(self.composer_bar(cx))
    }

    /// Open the native folder picker and bind the chosen directory as run context.
    pub fn pick_work_folder(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose working folder".into()),
        });
        cx.spawn(async move |this, cx| {
            let picked = rx
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
                .and_then(|paths| paths.into_iter().next());
            if let Some(path) = picked {
                this.update(cx, |app, cx| {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string());
                    app.choose_work_folder(path, cx);
                    let placeholder = format!("Tell Awili what to do in {name}…");
                    app.composer
                        .update(cx, |c, _cx| c.set_placeholder(placeholder));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Bind a folder and read its top-level entries (in-scope files).
    /// Tears down any existing gateway session since the cwd changed — except
    /// while a gateway Stop is pending, when the teardown is deferred to the
    /// Stop's acknowledgement (P3c round 4, R9).
    /// RC1-S: the owner chose `path` for the conversation on screen: switch
    /// to it, remember the pick for that conversation, and show it in the
    /// composer's placeholder.
    /// RC2-RUN (round 7 Minor 1): the pick is the conversation's folder from
    /// now on — even one with turns is re-filed under it (sidebar group and
    /// database row), so it survives a restart.
    ///
    /// RC2-R67 (usability RC2 round 5, Minor 4: ⌘O in the w1 conversation
    /// moved it, with its w1 approvals, under c1): a conversation that has
    /// turns keeps the folder they ran in. Choosing another folder there
    /// opens a new Work conversation in it (a blank one is reused, as
    /// "+ New conversation" does); a conversation with no turns yet takes
    /// the folder, as before. (A move an earlier build made still shows.)
    pub(crate) fn choose_work_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // RC2-UX (R2-15): ⌘O lists it next time.
        self.remember_recent_folder(&path);
        let active = self.sidebar.active_id().to_string();
        let path_str = path.display().to_string();
        let keeps_its_folder = self.conversations.get(&active).is_some_and(|c| {
            matches!(c.kind, ConvKind::Work | ConvKind::Code)
                && c.cwd.as_deref().is_some_and(|old| old != path_str)
        }) && self
            .views
            .get(&active)
            .is_some_and(|v| !v.convo.messages().is_empty());
        if keeps_its_folder {
            // The helper the owner was working with carries on there.
            let helper = self.work.executor.clone();
            self.set_work_folder(path.clone(), cx);
            self.open_fresh_work_conversation();
            let id = self.sidebar.active_id().to_string();
            self.work.chip_picked.insert(id.clone(), helper.clone());
            self.work.executor = helper;
            self.work.folder_picked.insert(id.clone(), path);
            self.file_work_conversation(&id);
            self.set_work_folder_placeholder(cx);
            cx.notify();
            return;
        }
        self.work.folder_picked.insert(active.clone(), path.clone());
        self.set_work_folder(path, cx);
        self.set_work_folder_placeholder(cx);
        if let Some(conv) = self
            .conversations
            .get(&active)
            .filter(|c| matches!(c.kind, ConvKind::Work | ConvKind::Code))
        {
            // RC2-R1F5 (round 1 N1): a conversation with turns shows the
            // move — a note at this point, each earlier reply's folder, and
            // where it came from in the sidebar.
            let from = conv.cwd.clone();
            let to = self
                .work
                .folder
                .as_ref()
                .map(|f| f.display().to_string())
                .unwrap_or_default();
            self.note_folder_move(&active, from, &to);
            self.file_work_conversation(&active);
        }
        if self
            .conversations
            .get(&active)
            .is_some_and(|c| c.kind == ConvKind::Code)
        {
            if let Some(tab) = self
                .work
                .code_tabs
                .iter()
                .find(|tab| tab.history.conv_id.as_deref() == Some(active.as_str()))
                .map(|tab| tab.id)
            {
                self.sync_code_identity_folder(
                    tab,
                    &active,
                    self.work.folder.as_ref().map(|p| p.display().to_string()),
                );
            }
        }
    }

    /// RC1-S: the composer names the Work folder ("Tell Awili what to do in
    /// <name>…").
    pub(crate) fn set_work_folder_placeholder(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.work.folder.clone() else {
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let placeholder = format!("Tell Awili what to do in {name}…");
        self.composer
            .update(cx, |c, _cx| c.set_placeholder(placeholder));
    }

    /// RC1-S (round 6 Major 1, also round 3b): opening a Work conversation
    /// puts the Work folder on the conversation's own — the owner's pick
    /// there (`folder_picked`), else the folder it was filed under (`cwd`).
    /// Never while a run is live or stopping, nor while a prompt is parked (a
    /// folder switch retires the worker); a folder that no longer exists is
    /// left alone. Returns whether the folder changed.
    pub(crate) fn restore_work_folder(&mut self, cx: &mut Context<Self>) -> bool {
        if self.shell.archived_reader.is_some()
            || self.work.running
            || self.work.gateway_stop_pending
            || self.work.agent_gate_pending
            || self.work.queued_after_stop.is_some()
            || self.work.local.health.pending_send.is_some()
        {
            return false;
        }
        let active = self.sidebar.active_id().to_string();
        let Some(conv) = self.conversations.get(&active) else {
            return false;
        };
        let native_backed = conv.kind == ConvKind::Code;
        if !matches!(conv.kind, ConvKind::Work | ConvKind::Code)
            || conv.archived
            || (native_backed && self.mode != Mode::Work)
        {
            return false;
        }
        let restored = if native_backed {
            self.code_identity_folder(&active)
        } else {
            self.work.folder_picked.get(&active).cloned().or_else(|| {
                conv.cwd
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
            })
        };
        let Some(path) = restored else {
            if native_backed && self.work.folder.is_some() {
                self.bind_work_folder_state(None, cx);
                self.composer
                    .update(cx, |c, _| c.set_placeholder("Tell Awili what to do…"));
                return true;
            }
            return false;
        };
        if !path.is_dir() {
            return false;
        }
        if self.work.folder.as_ref() == Some(&path) {
            if self.ensure_work_terminals(cx) {
                cx.notify();
            }
            return false;
        }
        if native_backed {
            self.bind_work_folder_state(Some(path), cx);
        } else {
            self.set_work_folder(path, cx);
        }
        self.set_work_folder_placeholder(cx);
        true
    }

    /// RC2-RUN (S-S2): a run just ended while another Work conversation is on
    /// screen — that conversation's folder applies at once, not at its next
    /// opening (the run kept its own folder while it went on). Called next to
    /// every [`Self::settle_work_chip`]. The run's own conversation is left
    /// alone (its folder is the one on screen; a switch would clear the
    /// outputs the run just measured). Deferred to the end of the current
    /// update, so the ended run finishes (reply, outputs, ledger) in its own
    /// folder first; `restore_work_folder` keeps every guard — a parked
    /// prompt, a pending Stop or gate, or another run still holds it.
    pub(crate) fn settle_work_folder(&mut self, cx: &mut Context<Self>) {
        if self.shell.archived_reader.is_some()
            || self.work.conv_id.as_deref() == Some(self.sidebar.active_id())
        {
            return;
        }
        let this = cx.entity();
        cx.defer(move |cx| {
            this.update(cx, |app, cx| {
                if app.work.conv_id.as_deref() != Some(app.sidebar.active_id())
                    && app.restore_work_folder(cx)
                {
                    cx.notify();
                }
            });
        });
    }

    pub fn set_work_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.bind_work_folder_state(Some(path.clone()), cx);
        if let Some(p) = &self.persist {
            p.set_state(
                crate::persistence::WORK_FOLDER_KEY,
                path.display().to_string(),
            );
        }
        self.file_work_conversation_on_folder_switch();
    }

    /// Bind only the worker/workspace state. An adapter handoff must neither
    /// re-file the unrelated sidebar selection nor replace the global default.
    pub(crate) fn bind_work_folder_state(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.work.gateway_stop_pending {
            // The Stop's acknowledgement retires its worker. Never send an
            // early Shutdown or replace its still-owned session here (P3c R9).
            self.work.folder_changed_during_stop = true;
        } else if let Some(cmd_tx) = self.work.gateway_cmd_tx.take() {
            let _ = cmd_tx.send(GatewayCmd::Shutdown);
            self.work.session_id = None;
        }
        self.work.folder_files = path
            .as_deref()
            .map(|p| list_dir_top(p, 14))
            .unwrap_or_default();
        self.work.workspace_tree = path.clone().map(crate::workspace::FileTreeState::new);
        self.work.file_tree = self
            .work
            .workspace_tree
            .as_mut()
            .map(crate::workspace::file_tree_from_workspace)
            .unwrap_or_default();
        self.work.tree_expanded.clear();
        self.work.tree_open = path.is_some();
        self.work.outputs.clear();
        // Replacing a workspace replaces its terminal resources; a cold
        // same-folder restore uses ensure_work_terminals without this reset.
        self.work.terminals.clear();
        self.work.terminal_view = None;
        self.work.folder = path;
        self.ensure_work_terminals(cx);
    }

    /// Populate only missing terminal resources for the current existing
    /// folder. Keep sessions, outputs, ownership and existing terminals intact.
    pub(crate) fn ensure_work_terminals(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(path) = self.work.folder.clone().filter(|path| path.is_dir()) else {
            return false;
        };
        let mut changed = false;
        if self.work.terminals.is_empty() {
            let id = crate::terminal::TerminalId(self.work.next_terminal_id);
            self.work.next_terminal_id += 1;
            if let Ok(terminal) = crate::terminal::TerminalState::spawn(id, path.clone(), 80, 24) {
                self.work.terminals.push(terminal);
                changed = true;
            }
        }
        if self.work.terminal_view.is_none() {
            let id = crate::terminal::TerminalId(self.work.next_terminal_id);
            self.work.next_terminal_id += 1;
            self.work.terminal_view = crate::terminal_view::TerminalView::try_new(id, path, cx);
            changed |= self.work.terminal_view.is_some();
        }
        changed
    }

    /// What this engine needs before a run can start (see
    /// [`engine_precondition`]).
    fn engine_precondition(&self) -> Result<(), String> {
        if let Some(id) = self.work.unknown_extra_engine() {
            return Err(unknown_engine_notice(id));
        }
        engine_precondition(&self.work.effective_engine(), self.work.gateway.enabled)
    }

    /// Build an `awili-engine` worker for this session, including the on-disk
    /// artifacts it owns.
    ///
    /// Two different lifetimes are at play, so they get two different keys:
    /// - the B2 execution log is the replay artifact for ONE session, keyed by
    ///   conversation;
    /// - C1 reusable grants live in the ONE fixed per-user store (contract
    ///   §5, unit 5): per-FOLDER isolation is the `folder_root` FIELD on the
    ///   grant record, which `GrantSet::matching` already enforces — the old
    ///   per-folder FILES were duplicating what the schema carries, and an
    ///   install-scope grant cannot live in a per-folder file at all. The
    ///   legacy files are migrated once (aliased, crash-atomic) on first open.
    pub(crate) fn build_awili_engine(
        cfg: &awili_config::EngineConfig,
        engine_key: Option<&str>,
        workdir: &str,
        conv_id: &str,
        restored: Vec<awili_engine_worker::RestoredTurn>,
        history: Option<awili_engine_worker::ModelHistory>,
        approval: awili_work::SharedApprovalMode,
    ) -> Result<awili_engine_worker::AwiliEngineWorker, String> {
        let root = engine_data_root().ok_or("no data directory for engine artifacts")?;
        let logs = root.join("logs");
        let legacy_grants = root.join("grants");
        awili_config::ensure_private_app_dir(&logs)
            .map_err(|e| format!("cannot create {}: {e}", logs.display()))?;
        let grant_path = root.join("grants.log");
        // Initialize the per-user store service HERE (idempotent), with the
        // legacy per-folder directory offered for one-shot migration; the
        // worker then attaches to the same service by path.
        awili_engine_log::grant_service::GrantStoreService::init(
            &grant_path,
            Some(legacy_grants.as_path()),
        )
        .map_err(|e| format!("grant store: {e:?}"))?;

        let engine_cfg = awili_engine_worker::AwiliEngineConfig {
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
            // The app-resolved engine bearer ([engine].api_key over
            // AWILI_ENGINE_KEY, see ProviderKeys::from_env_and_config) —
            // the worker never reads the environment itself.
            api_key: engine_key.unwrap_or("").to_string(),
            dialect: cfg.dialect.clone(),
            window_tokens: cfg.window_tokens,
            request_timeout: std::time::Duration::from_secs(cfg.request_timeout_s),
            log_path: engine_log_path(&root, conv_id),
            grant_path,
            restored,
            history,
        };
        // The authority is process-wide and issued at most once; a respawn
        // (folder change, dead channel) shares it rather than reissuing.
        let authority = awili_engine_worker::authority().map_err(|e| e.to_string())?;
        // SAFETY (owner rule: Awili never makes a Lemonade host load a model):
        // callers SHOULD have passed `guard_in_app` (or the council's wait)
        // first, for the early, readable refusal -- but this transport gates
        // EVERY request the worker sends (tool-loop steps, retries, resumes
        // after an approval or a question, warm-ups) on a fresh-enough read of
        // the host, so nothing depends on every caller remembering.
        let mut gate_cfg = cfg.clone();
        gate_cfg.api_key = engine_key.unwrap_or("").to_string();
        gate_cfg.label.clear();
        let transport = awili_seam::HttpTransport::with_api_key(
            &cfg.base_url,
            engine_cfg.request_timeout,
            Some(engine_cfg.api_key.clone()),
        )
        .with_max_output_tokens(cfg.resolved_max_output_tokens())
        .with_preflight(Self::transport_preflight(
            &gate_cfg,
            crate::strip::transport_cache_age(),
        ));
        let worker = awili_engine_worker::AwiliEngineWorker::with_transport(
            engine_cfg,
            authority,
            Box::new(transport),
        )
        .map_err(|e| e.to_string())?
        // U4: the worker reads the conversation's Ask/Auto mode at each
        // gate from this shared cell (U4b2); the app installs it at the
        // top of every dispatch.
        .with_approval_mode(approval);
        // Unit 4b commit 5 — the FIRST production launch path: when the
        // current config declares Work-active MCP servers, this worker is
        // composed with the live runtime + coordinator + per-turn drive.
        // No servers → byte-for-byte the unit-4 builtin-only worker.
        Ok(match Self::compose_work_mcp(workdir)? {
            Some(mcp) => worker.with_mcp(mcp),
            None => worker,
        })
    }

    /// Compose the live MCP stack for one Work worker (design r9 §5.4/§5.5):
    /// the Work-active catalog, the runtime + coordinator, the per-worker
    /// `WorkMcpState`, and the turn-open drive. Returns `None` when the
    /// config declares no Work-active servers. Every degraded entry
    /// surfaces as a problem line; one malformed entry degrades one
    /// origin, never silently.
    /// Ship-fix P0-5: a TYPED composition result. `Ok(None)` means the
    /// config genuinely declares no Work-active servers — builtin-only is
    /// then correct, not a degradation. Every FAILURE of a configured
    /// composition is `Err` and fails the run visibly: silently launching
    /// without mac-shell (and with it the memory system) was the exact
    /// hazard the ship-readiness review flagged. Per-origin degradations
    /// that leave OTHER origins usable surface in-band on the mcp status
    /// channel instead.
    fn compose_work_mcp(workdir: &str) -> Result<Option<awili_engine_worker::WorkerMcp>, String> {
        let config = match awili_config::Config::load() {
            Ok(c) => c,
            Err(e) => {
                return Err(format!(
                    "MCP configuration failed to load: {e:?} — refusing to run \
                     Work without the configured servers"
                ))
            }
        };
        Self::compose_work_mcp_from(workdir, &config)
    }

    /// The injectable core of [`Self::compose_work_mcp`] (testable without
    /// the user's real config file).
    fn compose_work_mcp_from(
        workdir: &str,
        config: &awili_config::Config,
    ) -> Result<Option<awili_engine_worker::WorkerMcp>, String> {
        use crate::mcp_catalog::{DeclaredServerPolicy, WorkMcpState};

        // Work-mode gating (design r9 §5.5): only servers offering Work.
        let mut work_mcp = awili_config::McpConfig {
            servers: config
                .mcp
                .active_in(awili_config::McpMode::Work)
                .cloned()
                .collect(),
        };
        // P12: when a connector family is enabled, the shared
        // `awili-connectors` server binary is offered as a synthesised
        // server entry — a user `[[mcp.servers]]` entry under the same
        // name WINS (the synthesised entry is dropped), and an
        // unresolvable binary omits the entry (a relative program path
        // would be rejected by `StdioServerSpec::new`).
        if let Some(entry) = crate::connectors_mcp::server_config(config) {
            if !work_mcp.servers.iter().any(|e| e.name == entry.name) {
                work_mcp.servers.push(entry);
            }
        }
        // RC1-M-FIX1: on the owner's install the in-app engine gets the same
        // MCP servers his Claude Code has (interlock memory, mac-shell, …);
        // an Awili `[[mcp.servers]]` entry under the same name wins.
        // RC1-M-FIX3 (RC1-M-FIX1 review P1): ANY configured entry of that
        // name wins — one switched off (`enabled = false`) or not offered in
        // Work is the owner's explicit opt-out, never re-imported.
        if let (Some(mem), Some(paths)) = (
            config.work.agents.memory.as_ref(),
            crate::agent_launch::LaunchPaths::production(),
        ) {
            for entry in crate::agent_launch::owner_mcp_servers(&paths.home, mem) {
                let configured = config.mcp.servers.iter().any(|e| e.name == entry.name);
                if !configured && !work_mcp.servers.iter().any(|e| e.name == entry.name) {
                    work_mcp.servers.push(entry);
                }
            }
        }
        if work_mcp.servers.is_empty() {
            return Ok(None);
        }
        let (catalog, problems) = crate::mcp_transport::catalog_from_config(&work_mcp);
        let mut startup_lines: Vec<String> = problems.iter().map(|p| format!("mcp: {p}")).collect();
        let origins: Vec<awili_tools::ToolOrigin> = catalog
            .origins()
            .map(|o| awili_tools::ToolOrigin::new(o.clone()))
            .collect();
        if origins.is_empty() {
            return Err(format!(
                "every configured Work MCP server failed composition: {}",
                startup_lines.join("; ")
            ));
        }
        // The reserved server is REQUIRED when configured (P0-5): losing
        // mac-shell silently means losing the memory system silently.
        let reserved_origin = format!("mcp:{}", awili_config::INSTALL_TRUSTED_SERVER);
        let reserved_configured = work_mcp
            .servers
            .iter()
            .any(|e| e.name == awili_config::INSTALL_TRUSTED_SERVER);
        if reserved_configured && !origins.iter().any(|o| o.origin == reserved_origin) {
            return Err(format!(
                "configured required server {:?} failed composition: {}",
                awili_config::INSTALL_TRUSTED_SERVER,
                startup_lines.join("; ")
            ));
        }

        let runtime_cfg = crate::mcp_runtime::McpRuntimeConfig::default();
        // Unit 5 (ruling 27): capture the ADMITTED reserved entry's
        // running identity and the setup-records path at composition —
        // the per-call authority compares the CURRENT file against these.
        let setup_path = runtime_cfg
            .setup_path
            .clone()
            .or_else(awili_attest::setup_record::default_setup_path);
        let running = work_mcp
            .servers
            .iter()
            .find(|e| e.name == awili_config::INSTALL_TRUSTED_SERVER)
            .map(crate::install_authority::RunningServerIdentity::of);
        let handle = crate::mcp_runtime::spawn_mcp_runtime::<awili_engine_log::IntentTicket>(
            catalog,
            runtime_cfg,
        );
        let coordinator = awili_engine_worker::mcp_coordinator::McpCoordinator::new(handle);
        let dispatch_facade = coordinator.facade();
        let drive_facade = coordinator.facade();
        let authority_facade = coordinator.facade();
        let executors = awili_seam::Executors::mcp(dispatch_facade);
        // Unit 5 (rulings 45-46): the one production construction path —
        // the brand registers this exact state as THE current committed
        // catalog with the process-long floor owner, atomically retiring
        // any predecessor (worker respawn = re-registration; the confined
        // authority itself never moves).
        let (state, brand) = WorkMcpState::new_registered();
        if let Err(e) = awili_engine_worker::floor_owner::register_catalog(brand) {
            return Err(format!("MCP catalog registration refused: {e:?}"));
        }
        let workdir = workdir.to_string();

        // Unit 5: the per-call install authority — NoInstallAuthority-
        // equivalent when the reserved server is not composed (queries
        // find no running identity and refuse).
        let concrete_authority = running.map(|running| {
            crate::install_authority::WorkInstallAuthority::new(
                running,
                authority_facade,
                setup_path,
            )
        });
        let install_authority: std::sync::Arc<dyn awili_engine_authz::InstallAuthorityQuery> =
            match &concrete_authority {
                Some(a) => a.clone(),
                None => std::sync::Arc::new(awili_engine_authz::NoInstallAuthority),
            };
        let seed_authority = concrete_authority;
        let reserved_origin =
            awili_tools::ToolOrigin::new(format!("mcp:{}", awili_config::INSTALL_TRUSTED_SERVER));

        let mut startup_lines = std::mem::take(&mut startup_lines);
        // Ship-fix P0-5 (runtime half): the reserved server, when
        // configured, is REQUIRED to be live for the turn — the drive is
        // re-run under a bounded wait until its tools are committed, and
        // a timeout reports the origin as missing (the worker then fails
        // the turn instead of running without the memory system).
        let required: Vec<awili_tools::ToolOrigin> = if reserved_configured {
            vec![reserved_origin.clone()]
        } else {
            Vec::new()
        };
        // RC1-M-FIX1: every configured server gets the same bounded wait at
        // turn open, not only the reserved one. The in-app worker belongs to
        // one conversation (RC1-I2), so a conversation's first turn IS a
        // first turn open: before this, a server that took a moment to
        // connect (the owner's interlock memory) was never ready on it and
        // the model ran without its tools (live 2026-10-03: Gemma saw
        // mac-shell's tools and none of interlock's). Only the reserved
        // server failing to arrive fails the turn.
        let awaited: Vec<awili_tools::ToolOrigin> = origins.clone();
        const REQUIRED_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
        let turn_open = Box::new(move || {
            crate::mcp_catalog::drive_reconcile_once(
                &drive_facade,
                &state,
                &origins,
                &workdir,
                &DeclaredServerPolicy,
            );
            let mut required_missing: Vec<String> = Vec::new();
            if !awaited.is_empty() {
                let deadline = std::time::Instant::now() + REQUIRED_WAIT;
                // RC2-R59 (usability RC2 round 4, Major 2: every in-app turn
                // sat about 11 s before its request left, while a server that
                // could not start — the owner's LightCraft, its library held
                // by another program — was started again every 150 ms, about
                // 70 times a turn): a server that died more than
                // [`TURN_OPEN_RESTARTS`] times in this wait is not waited for,
                // nor started again, until the next turn open.
                let mut gave_up: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                let mut deaths: std::collections::HashMap<String, Vec<String>> =
                    std::collections::HashMap::new();
                loop {
                    let inputs = state.turn_inputs();
                    let has = |o: &awili_tools::ToolOrigin| {
                        inputs.extra_tools.iter().any(|d| d.identity.origin == *o)
                    };
                    // A server given up on is not waited for (its degradation
                    // is reported in-band as before).
                    note_deaths(
                        &drive_facade.lifecycle(),
                        &awaited,
                        &mut deaths,
                        &mut gave_up,
                    );
                    let settled =
                        |o: &awili_tools::ToolOrigin| has(o) || gave_up.contains(&o.origin);
                    if awaited.iter().all(settled) || std::time::Instant::now() >= deadline {
                        required_missing = required
                            .iter()
                            .filter(|o| !has(o))
                            .map(|o| o.origin.clone())
                            .collect();
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    // A death in the pause is counted before the next drive,
                    // which would start the server again.
                    note_deaths(
                        &drive_facade.lifecycle(),
                        &awaited,
                        &mut deaths,
                        &mut gave_up,
                    );
                    let coming: Vec<awili_tools::ToolOrigin> = origins
                        .iter()
                        .filter(|o| !gave_up.contains(&o.origin))
                        .cloned()
                        .collect();
                    crate::mcp_catalog::drive_reconcile_once(
                        &drive_facade,
                        &state,
                        &coming,
                        &workdir,
                        &DeclaredServerPolicy,
                    );
                }
            }
            let mut inputs = state.turn_inputs();
            // RC1-N: the in-app engine is never offered memory `bootstrap`.
            inputs.extra_tools.retain(|d| !preamble_duplicate(d));
            // Ship-fix P0-5: composition-time problems reach the SAME
            // in-band mcp status channel as every other degradation —
            // once, on the first turn open, never only stderr.
            if !startup_lines.is_empty() {
                let mut lines = std::mem::take(&mut startup_lines);
                lines.extend(inputs.degradations);
                inputs.degradations = lines;
            }
            // Unit 5 §5.1: the seed candidate crosses the boundary ONCE
            // per turn open; the worker mints through the store service.
            let candidate = seed_authority
                .as_ref()
                .and_then(|a| a.seed_candidate(&reserved_origin));
            (
                inputs.extra_tools,
                inputs.roots,
                inputs.degradations,
                candidate,
                required_missing,
            )
        });

        Ok(Some(awili_engine_worker::WorkerMcp {
            executors,
            turn_open,
            install_authority,
            coordinator,
        }))
    }

    /// Submit a Work run: record the turn, snapshot the folder, then stream
    /// events back from Hermes.
    ///
    /// Gateway path (preferred): on the first submit for a folder, spawns a
    /// persistent worker thread that creates a session with `cwd` and keeps the
    /// WebSocket open. Subsequent submits in the same folder reuse the session
    /// (multi-turn). Changing the folder tears down the old session.
    ///
    /// REST fallback: spawns a one-shot worker per submit.
    pub fn submit_work_run(&mut self, task: String, cx: &mut Context<Self>) {
        let conv_id = self.sidebar.active_id().to_string();
        self.submit_work_run_in(task, conv_id, cx);
    }

    /// [`Self::submit_work_run`] pinned to `conv_id` — the replay of a prompt
    /// queued behind a gateway Stop runs in the conversation it was typed in,
    /// even if the user navigated away meanwhile (P3c round 3, R5). M2g's
    /// Work-mode Retry chip (in `main.rs`) re-sends through it too.
    pub(crate) fn submit_work_run_in(
        &mut self,
        task: String,
        conv_id: String,
        cx: &mut Context<Self>,
    ) {
        self.submit_work_run_kind(task, conv_id, awili_core::MessageKind::Normal, cx);
    }

    /// [`Self::submit_work_run_in`] whose user row is of `kind` (RC2-RUNF:
    /// the plan card's Approve sends [`awili_core::MessageKind::PlanClick`];
    /// a prompt parked behind a gateway Stop is replayed as an ordinary
    /// message, like any parked prompt). Returns whether the Work adapter
    /// accepted or parked the message; a native Code divert owns its own
    /// acceptance boundary.
    pub(crate) fn submit_work_run_kind(
        &mut self,
        task: String,
        conv_id: String,
        kind: awili_core::MessageKind,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.onboarding.is_active() {
            return false;
        }
        let task = task.trim().to_string();
        if task.is_empty() || self.work.running {
            return false;
        }
        if self.refuse_archived_workshop_send(&conv_id, cx) {
            return false;
        }
        if self
            .work
            .unavailable_conversation_routes
            .contains_key(&conv_id)
        {
            self.work.panel.error = Some(
                "This conversation's saved next-turn target is unavailable; choose another target first."
                    .into(),
            );
            cx.notify();
            return false;
        }

        // Do this before any native->Work hydration or transcript mutation.
        // The route is the authoritative owner even when this Work surface is
        // not currently marked running (for example a retained Code backing).
        if self
            .work
            .conversation_routes
            .get(&conv_id)
            .is_some_and(|route| route.active().is_some())
        {
            let notice = "This conversation already has a native turn running. Stop it before sending another request.";
            self.work.panel.error = Some(notice.into());
            self.show_toast(notice, cx);
            cx.notify();
            return false;
        }
        if self
            .work
            .code_tabs
            .iter()
            .any(|tab| tab.streaming && tab.history.conv_id.as_deref() == Some(conv_id.as_str()))
        {
            let notice = "This conversation already has a native turn running. Stop it before sending another request.";
            self.work.panel.error = Some(notice.into());
            self.show_toast(notice, cx);
            cx.notify();
            return false;
        }

        // M2: dispatch uses this conversation's persisted next target, not
        // the mutable/global chip from whichever conversation was shown last.
        // The target is captured by the existing run identity below; a later
        // chooser change cannot retarget this accepted run.
        let route_seed = self
            .work
            .chip_picked
            .get(&conv_id)
            .cloned()
            .unwrap_or_else(|| self.work.executor.clone());
        let target = self
            .work
            .conversation_routes
            .entry(conv_id.clone())
            .or_insert_with(|| crate::conversation_route::ConversationRoute::new(route_seed))
            .next_target()
            .clone();
        let preserve_plan = matches!(&kind, awili_core::MessageKind::PlanClick)
            .then(|| AskKey::Work(conv_id.clone()));

        // A gateway Stop is still interrupting and draining its turn. Park
        // before a native-tab divert so the captured target cannot start a
        // second provider beside the stopped gateway owner.
        if self.work.gateway_stop_pending {
            if !self.work_conversation_is_live(&conv_id) {
                eprintln!("awili: parked prompt dropped: conversation deleted");
                return false;
            }
            if self.work.queued_after_stop.is_some() {
                self.restore_draft(&task, cx);
                self.show_toast(crate::gateway_stop_deadline::ALREADY_PARKED_TOAST, cx);
                return false;
            }
            self.clear_superseded_conversation_plans(&conv_id, preserve_plan.as_ref());
            if self
                .conversations
                .get(&conv_id)
                .is_some_and(|c| c.kind == ConvKind::Code)
                && self.code_identity_folder(&conv_id) != self.work.folder
            {
                self.work.folder_changed_during_stop = true;
            }
            self.work.queued_after_stop = Some(QueuedPrompt {
                text: task,
                conv_id,
                executor: target,
            });
            self.accept_submit();
            self.work.panel.activity = Some(GATEWAY_STOPPING_ACTIVITY.to_string());
            self.show_toast(crate::gateway_stop_deadline::PARKED_TOAST, cx);
            cx.notify();
            return true;
        }
        // This conversation may have crossed from a native Code session to
        // the in-app adapter. On the way back, return to that same native
        // session instead of launching an unrelated Work-agent turn. Refresh
        // its render snapshot from the durable shared transcript, but retain
        // its session slot, folder, route and Stop owner.
        if matches!(&target, WorkExecutor::Agent { .. }) {
            if let Some(index) =
                self.work.code_tabs.iter().position(|session| {
                    session.history.conv_id.as_deref() == Some(conv_id.as_str())
                })
            {
                if self.work.code_tabs[index].streaming {
                    self.work.panel.error = Some(
                        "This conversation already has a native turn running. Stop it before sending another request."
                            .into(),
                    );
                    cx.notify();
                    return false;
                }
                if let Some(convo) = self.views.get(&conv_id).map(|view| view.convo.clone()) {
                    let session = &mut self.work.code_tabs[index];
                    session.convo = convo;
                    session.rows = session
                        .convo
                        .messages()
                        .iter()
                        .map(crate::MessageRow::new)
                        .collect();
                }
                let tab_id = self.work.code_tabs[index].id;
                self.carry_work_ask_mode_to_code(&conv_id, tab_id);
                self.set_mode(Mode::Code, cx);
                self.activate_code_tab(index);
                self.submit_code_turn_kind(task, kind, cx);
                // Code owns its own acceptance boundary. This return value
                // is consumed only by callers deciding whether a Work-adapter
                // submission may change presentation backing.
                return false;
            }
        }
        // Historical Code identities can now run through Work. Their own
        // folder (including an explicit unbound state) must reach the worker,
        // instead of inheriting whichever Work thread last used this adapter.
        let routed_folder = self
            .conversations
            .get(&conv_id)
            .filter(|conv| conv.kind == ConvKind::Code)
            .map(|_| self.code_identity_folder(&conv_id));
        let dispatch_folder = routed_folder
            .clone()
            .unwrap_or_else(|| self.work.folder.clone());
        // Work has historically invalidated an open plan for a genuinely new
        // message even when its later folder preflight refuses that message.
        // Keep that contract after the busy/Stop guards above, while retaining
        // the current approval gate for a PlanClick.
        self.clear_superseded_conversation_plans(&conv_id, preserve_plan.as_ref());
        // R1a: a bound folder that no longer exists refuses the submission
        // BEFORE any run state is set up and before any executor dispatch,
        // in plain words, and a composer submit gets its text back.
        if let Some(notice) =
            crate::submit_guard::work_folder_gone_notice(dispatch_folder.as_deref())
                .filter(|_| !self.work.gateway_stop_pending)
        {
            self.work_record_graduation();
            self.work.conv_id = Some(conv_id.clone());
            self.work.stream_row_idx = None;
            self.work.panel = RunPanel::default();
            self.work.error_presentation = None;
            self.work.panel.error = Some(notice.clone());
            self.capture_work_error(None);
            // A replay already accepted its composer guard when parked.
            // Return its text to the captured owner, even after navigation.
            if self.submit_guard.is_none() {
                self.hand_back_to_conversation(&conv_id, &task, cx);
            }
            self.refuse_submit(notice, cx);
            cx.notify();
            return false;
        }
        // From here on the submit has passed every refusal preflight. Set the
        // accepted target before deriving lane-specific instructions so those
        // decisions cannot accidentally follow another conversation's chip.
        if routed_folder.is_some() && self.work.folder != dispatch_folder {
            self.bind_work_folder_state(dispatch_folder, cx);
        }
        self.work.executor = target;
        // RC1-M / RC1-M-FIX8: the project's instructions field, plus the
        // AGENTS.md files for the lane that gets them nowhere else (the
        // Hermes gateway); every agent CLI gets them from its launch.
        let agents_in_task = match &self.work.executor {
            WorkExecutor::Agent { .. } | WorkExecutor::LocalBuild { .. } => false,
            WorkExecutor::InApp | WorkExecutor::InAppExtra { .. } | WorkExecutor::Gateway => {
                !matches!(self.work_engine_kind(), awili_config::EngineKind::Awili)
            }
        };
        let agents_md = self.work.agents.agents_md.clone();
        let instructions = folder_instructions_prefix(
            &self.work.projects,
            self.work.folder.as_deref(),
            agents_in_task,
            agents_md.as_deref(),
        );
        // A direct submit supersedes any prompt still parked from an earlier
        // Stop whose acknowledgement can no longer arrive.
        self.work.queued_after_stop = None;
        // Pin this run's output to the active Work conversation. set_mode →
        // activate_kind(Work) guarantees active_id is a Work thread on entry;
        // capturing it now keeps the run anchored even if the user switches
        // modes mid-run.
        self.work.conv_id = Some(conv_id);
        // P8b: every submitted turn gets exactly one ledger line, whatever
        // path ends it (finish, Failed update, Stop, local-build terminal or
        // preflight failure). Flush first: a still-armed previous turn whose
        // panel ended in an error is recorded now rather than forfeited.
        self.work_record_graduation();
        let graduation_conv = self.work_conv_id();
        self.work.graduation.arm(&graduation_conv);
        crate::run_error::log_run_start(&self.work.graduation.run_id(), "work", &graduation_conv);

        let attachments = (kind != awili_core::MessageKind::Retry)
            .then_some(self.search_store.as_ref())
            .flatten()
            .and_then(|store| store.get_staged_attachments(&graduation_conv).ok())
            .unwrap_or_default();
        self.note_submit_context(&graduation_conv, &task, &attachments);
        let task = crate::attachments::compose_message_with_attachments(&task, &attachments);
        self.work_push_kind(Role::User, task.clone(), kind);
        self.accept_submit(); // R1a: the text is in the transcript
        if !attachments.is_empty() {
            if let Some(store) = self.search_store.as_ref() {
                if let Err(error) = store.clear_staged_attachments(&graduation_conv) {
                    self.show_toast(
                        format!("Files sent; clearing the tray failed — {error}"),
                        cx,
                    );
                }
            }
            if self.sidebar.active_id() == graduation_conv {
                self.refresh_staged_attachments();
            }
        }
        let folder = self.work.folder.clone();
        // The local Build lane (P2d) ships the user's task VERBATIM as the
        // remote spec text — no fleet-skill or CLAUDE.md prep. RC1-M-FIX8
        // (pre-review P1): led by the bounded user AGENTS.md and the
        // folder's project chain, once each. Captured before the
        // executor-facing task is prepared below.
        let spec_text = local_build_spec(
            &task,
            folder.as_deref(),
            self.work.agents.agents_md.as_deref(),
        );

        // Auto-title: use the first Work prompt (truncated) as the conversation title.
        let conv_id = self
            .work
            .conv_id
            .clone()
            .unwrap_or_else(|| self.sidebar.active_id().to_string());
        let now = crate::feed::unix_now();
        if let Some(title) = self.conversations.note_user_message(&conv_id, &task, now) {
            if let Some(p) = &self.persist {
                p.send(crate::PersistCmd::Rename {
                    id: conv_id.clone(),
                    title,
                    custom: false,
                });
            }
        }
        if let Some(p) = &self.persist {
            p.send(crate::PersistCmd::Touch {
                id: conv_id.clone(),
                updated_ts: now,
            });
        }

        // R1f: an unfiled conversation files under the folder this turn runs in.
        self.file_work_conversation_at_submit(&conv_id);

        // Clear preview rail state for the new run.
        self.work.touched_files.clear();
        self.work.preview_root = folder.clone();
        self.work.inapp_files = Default::default();
        self.work.owner_stopped = false;
        self.work.deny_stopped = None;
        self.work.referenced_urls.clear();

        // Start a streaming assistant row that will receive live tokens.
        self.work_push(Role::Assistant, String::new());
        if let Some(view) = self.views.get(&conv_id) {
            self.work.stream_row_idx = Some(view.rows.len().saturating_sub(1));
        }

        // P1: prepend the rendered fleet skill block to the executor-facing
        // task. The user row above already shows and persists the ORIGINAL
        // text (and auto-titled from it); this single prepared value is
        // what every executor path — Agent, owned engine, persistent and
        // new gateway — receives.
        // RC1-H2: only in the owner's fleet folders, never for the in-app engine.
        let pack = self.fleet_pack.as_deref().filter(|_| {
            crate::fleet_skills::recipes_apply(
                matches!(self.work_engine_kind(), awili_config::EngineKind::Awili),
                self.work.folder.as_deref(),
                &self.work.connectors.gitea,
            )
        });
        let task =
            crate::fleet_skills::prepare_task(pack, &crate::resume_context::mark_request(&task));

        // Inject the standing instructions read at the top of this submit
        // (RC1-M: the saved project's field, and the AGENTS.md chain for the
        // lanes that cannot read it); `None` when there is nothing.
        let task = match instructions {
            Some(prefix) => format!("{prefix}\n\n{task}"),
            None => task,
        };

        // RC1-J1-FIX2 / RC1-J1-FIX3: an agent or local-Build turn is ACCEPTED
        // here — its user message is already in the transcript. Those lanes run
        // no worker, so the live in-app/gateway worker is retired NOW, before
        // any of their preflight checks (policy gate, build target, folder) can
        // still fail: its event pump would be stale after their run, and a
        // later in-app turn must be rebuilt from the conversation — this turn
        // included — not continue a worker that never saw it.
        if crate::build_dispatch::dispatches_for(&self.work.executor)
            || matches!(self.work.executor, WorkExecutor::Agent { .. })
        {
            crate::local_first::retire_worker_for_workerless_lane(&mut self.work);
        }

        // Local Build (daily-driver P2d): the manual override resolves to its
        // actual adapter BEFORE the Agent/Gateway branch — `dispatches_for`
        // is the ONLY path into the dispatcher, so Ask/Plan/Review (the
        // gateway/agent lanes) never reach it. The override is consumed once
        // after the accepted dispatch.
        if crate::build_dispatch::dispatches_for(&self.work.executor) {
            if let WorkExecutor::LocalBuild { target_id } = self.work.executor.clone() {
                self.run_work_via_local_build(spec_text, target_id, cx);
                return true;
            }
        }

        // Agent-harness executor (design §4 first slice): the run is a vendor
        // CLI turn in the working folder, streamed into the same Work
        // transcript. Diverges before gateway preconditions — the harness
        // needs no gateway. S5 INT1: it awaits the memory gate first.
        if let WorkExecutor::Agent { engine, model } = self.work.executor.clone() {
            self.submit_gated_work_agent(task, engine, model, attachments, graduation_conv, cx);
            return true;
        }

        self.work.continuity.leave_agent(&conv_id);

        // Preconditions differ per engine (D2-u2a). Checked BEFORE any run state
        // is set up, so a misconfigured stanza surfaces as a panel error instead
        // of a half-started run.
        if let Err(msg) = self.engine_precondition() {
            self.work.panel = RunPanel::default();
            self.work.error_presentation = None;
            self.work.panel.error = Some(msg);
            self.work.panel.status = Some(RunStatus::Failed);
            // The user/assistant rows were already accepted. Route every
            // terminal path through the shared completion hook exactly once so
            // usage/provenance settle and hands-free ownership can re-arm.
            self.work_finish_stream(cx);
            cx.notify();
            return true;
        }

        // L1b (D6): a live idle worker of another lane (or model) is retired
        // before EITHER hand-off path (the S5 gate's live-worker offer, or
        // `dispatch_work_prompt`) can reach it.
        crate::local_first::retire_other_lane_worker(&mut self.work);

        self.work.panel = RunPanel::default();

        self.work.error_presentation = None;
        self.work.outputs.clear();
        self.work.folder_snapshot = folder.as_ref().map(|p| dir_snapshot(p)).unwrap_or_default();
        self.work.running = true;
        self.work.run_started = Some(std::time::Instant::now());
        self.work.waiting_for_stream_output = true;
        self.ensure_thinking_timer(cx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.work.cancel = cancel.clone();

        // Ship-fix P0-3: the owned engine receives the CURRENT standing
        // directives + recall as a pinned system preamble, fetched fresh
        // per turn exactly like Chat (the Hermes gateway owns its own
        // memory integration and ignores the field). The rules/recall
        // chips reflect the fetch so degradation is visible, not silent.
        if matches!(self.work_engine_kind(), awili_config::EngineKind::Awili)
            && (inapp_memory_on(&self.memory, self.work.agents.memory.as_ref())
                || crate::resume_context::request(&task).is_some())
        {
            let mem = crate::MemoryCfg {
                enabled: inapp_memory_on(&self.memory, self.work.agents.memory.as_ref()),
                ..self.memory.clone()
            };
            let query = task.clone();
            let folder2 = folder.clone();
            let resume_cwd = folder
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let resume_mem = mem.clone();
            let resume_connectors = self.work.connectors.clone();
            let conv_id2 = conv_id.clone();
            let cancel2 = cancel.clone();
            let resume_epoch = self.work.run_seq;
            self.work.resume_gate_pending = true;
            cx.spawn(async move |this, cx| {
                // RC2-R1F7: the in-app model gets shorter recalled notes.
                let gate = crate::recall::gate_turn_memory_with(
                    mem,
                    query,
                    crate::recall::production_client,
                    crate::recall::INAPP_RECALL_NOTE_BYTES,
                )
                .await;
                let evidence = crate::resume_context::collect(
                    &task,
                    resume_cwd,
                    resume_mem,
                    resume_connectors,
                    vec![conv_id.clone()],
                )
                .await;
                let task = crate::resume_context::enrich(task, evidence);
                this.update(cx, |app, cx| {
                    if cancel2.load(Ordering::Relaxed) || app.work.run_seq != resume_epoch {
                        return;
                    }
                    app.work.resume_gate_pending = false;
                    // RC1-M-FIX3 (RC1-M-FIX1 review P2): the rules and recall
                    // indicators follow every gate that ran — the owner's
                    // install runs it with `[memory]` off too.
                    if inapp_memory_on(&app.memory, app.work.agents.memory.as_ref()) {
                        if let Some(s) = &gate.rules {
                            app.note_rules_transition(s);
                        }
                        app.rules = gate.rules.clone();
                        app.recall = gate.recall.clone();
                        // RC2-UX (R5-9): the chips belong to this conversation.
                        app.note_memory_chip(&conv_id);
                    }
                    // U4: an accepted gate may offer the prompt straight to the
                    // live worker (bypassing `dispatch_work_prompt`), so the
                    // hand-off installs this conversation's mode first; a
                    // refusal touches nothing. RC1-M: the system prompt is
                    // composed BEFORE the hand-off, so the live worker gets
                    // the AGENTS.md chain and the brief too (the M2b brief
                    // used to reach only a new worker).
                    let outcome = gate.outcome.map(|policy| {
                        let system = app.work_system_for_engine(policy.clone(), folder2.as_deref());
                        app.record_inapp_context(
                            &conv_id2,
                            system.as_deref(),
                            policy.as_deref(),
                            folder2.as_deref(),
                            task.chars().count(),
                        );
                        system
                    });
                    let accepted = outcome.is_ok();
                    let guard_cancel = cancel2.clone();
                    let send = move |app: &mut Self, cx: &mut Context<Self>| {
                        match hand_off_gated_work_prompt(&mut app.work, &conv_id2, &task, outcome) {
                            GatedWorkPrompt::Offered => {
                                app.work.panel.requested_identity =
                                    app.work.worker_identity.clone();
                                cx.notify();
                            }
                            GatedWorkPrompt::NeedsWorker(system) => app
                                .dispatch_work_prompt_cleared(
                                    task, system, folder2, conv_id2, cancel2, cx,
                                ),
                            GatedWorkPrompt::Refused(reason) => {
                                app.apply_work_update(
                                    WorkUpdate::Failed(crate::recall::policy_refusal(&reason)),
                                    cx,
                                );
                                app.show_policy_refused_notice(cx);
                            }
                        }
                    };
                    // SAFETY: an accepted prompt may go straight to a LIVE worker
                    // (which would make a Lemonade host load an evicted model),
                    // so it passes the same residency check as every other send.
                    // A refused gate sends nothing and needs no check.
                    if accepted {
                        app.guard_in_app(guard_cancel, cx, send);
                    } else {
                        send(app, cx);
                    }
                })
                .ok();
            })
            .detach();
            return true;
        }
        // RC2-FIXC1 (RC2-FIXB review P2): this send runs no memory gate
        // (memory off, or the Hermes gateway, which keeps its own memory),
        // so the counts an earlier send saved here are not this send's.
        self.clear_memory_chip(&conv_id);
        let system = self.work_system_for_engine(None, folder.as_deref());
        if matches!(self.work_engine_kind(), awili_config::EngineKind::Awili) {
            self.record_inapp_context(
                &conv_id,
                system.as_deref(),
                None,
                folder.as_deref(),
                task.chars().count(),
            );
        }
        if crate::resume_context::request(&task).is_some() {
            let cwd = folder
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mem = self.memory.clone();
            let connectors = self.work.connectors.clone();
            let epoch = self.work.run_seq;
            self.work.resume_gate_pending = true;
            cx.spawn(async move |this, cx| {
                let evidence = crate::resume_context::collect(
                    &task,
                    cwd,
                    mem,
                    connectors,
                    vec![conv_id.clone()],
                )
                .await;
                this.update(cx, |app, cx| {
                    if cancel.load(Ordering::Relaxed) || app.work.run_seq != epoch {
                        return;
                    }
                    app.work.resume_gate_pending = false;
                    app.dispatch_work_prompt(
                        crate::resume_context::enrich(task, evidence),
                        system,
                        folder,
                        conv_id,
                        cancel,
                        cx,
                    );
                })
                .ok();
            })
            .detach();
            return true;
        }
        self.dispatch_work_prompt(task, system, folder, conv_id, cancel, cx);
        true
    }

    /// RC3-S4: keep the rules/doc parts of the run being started, for the Job
    /// panel and the reply inspector.
    pub(crate) fn record_context_parts(
        &mut self,
        conv_id: &str,
        engine: Option<CodeEngine>,
        folder: Option<&std::path::Path>,
    ) {
        let Some(paths) = crate::agent_launch::LaunchPaths::production() else {
            self.work.context_parts = None;
            return;
        };
        let parts = crate::agent_launch::context_parts(
            &paths,
            engine,
            folder,
            self.work.agents.agents_md.as_deref(),
        );
        self.work.context_parts = Some((conv_id.to_string(), parts));
    }

    /// RC1-M: the in-app engine's system prompt for one turn: Awili's memory
    /// preamble (`policy`, S5), then the user-level and project AGENTS.md
    /// files (the same files Claude Code and Codex read), then its brief
    /// (RC1-H1). The in-app engine's preamble is per prompt, so every turn
    /// gets all three. The gateway lane is untouched (Hermes ignores the
    /// field).
    pub(crate) fn work_system_for_engine(
        &self,
        policy: Option<String>,
        folder: Option<&std::path::Path>,
    ) -> Option<String> {
        if !matches!(self.work_engine_kind(), awili_config::EngineKind::Awili) {
            return policy;
        }
        inapp_system(
            policy,
            inapp_memory_on(&self.memory, self.work.agents.memory.as_ref()),
            folder,
            self.work.agents.agents_md.as_deref(),
            crate::agent_launch::LaunchPaths::production()
                .map(|p| p.home)
                .as_deref(),
            inapp_model_name(&self.work.effective_engine()).as_deref(),
        )
    }

    /// Send one Work prompt to the (possibly new) engine worker.
    ///
    /// SAFETY (owner rule, after Awili hung the Hosta box twice by loading
    /// models on demand): before an in-app prompt goes out, a Lemonade host
    /// is asked, read-only (`GET /api/v1/health`), whether the engine's model
    /// is resident ([`Self::guard_in_app`]). If it is not, NOTHING is sent: no
    /// chat request, no load. The strip turns green and an error card says
    /// why. A host that gives no answer is treated the same way.
    fn dispatch_work_prompt(
        &mut self,
        task: String,
        system: Option<String>,
        folder: Option<std::path::PathBuf>,
        conv_id: String,
        cancel: Arc<AtomicBool>,
        cx: &mut Context<Self>,
    ) {
        let keep = cancel.clone();
        self.guard_in_app(keep, cx, move |app, cx| {
            app.dispatch_work_prompt_cleared(task, system, folder, conv_id, cancel, cx)
        });
    }

    /// Run the Lemonade residency check for the in-app engine, off the UI
    /// thread, then `proceed` (send) or refuse with the card. EVERY path that
    /// sends a prompt to an in-app worker, a new one or a live one, goes
    /// through here. Not an in-app engine, or one that is not set up, passes
    /// straight through.
    pub(crate) fn guard_in_app(
        &mut self,
        cancel: Arc<AtomicBool>,
        cx: &mut Context<Self>,
        proceed: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        let engine = self.guard_engine();
        if !matches!(self.work_engine_kind(), awili_config::EngineKind::Awili)
            || !engine.is_configured()
        {
            return proceed(self, cx);
        }
        // Tests read the host on this thread (the test scheduler rejects
        // wake-ups from foreign threads); the app reads it off-thread.
        #[cfg(test)]
        {
            let refusal = Self::lemonade_refusal(&engine, crate::strip::guard_timeout());
            self.finish_guard(refusal, engine, cancel, proceed, cx);
        }
        #[cfg(not(test))]
        {
            let epoch = self.work.run_seq;
            let (tx, rx) = async_channel::bounded(1);
            let probe_engine = engine.clone();
            std::thread::spawn(move || {
                let _ = tx.send_blocking(Self::lemonade_refusal(
                    &probe_engine,
                    crate::strip::guard_timeout(),
                ));
            });
            cx.spawn(async move |this, cx| {
                if let Ok(refusal) = rx.recv().await {
                    this.update(cx, |app, cx| {
                        // A Stop or a newer send while the host was read.
                        if cancel.load(Ordering::Relaxed) || app.work.run_seq != epoch {
                            return;
                        }
                        app.finish_guard(refusal, engine, cancel, proceed, cx);
                    })
                    .ok();
                }
            })
            .detach();
        }
    }

    /// The in-app engine the next send uses, with the bearer it really sends
    /// (the app-resolved key for the primary, its own for an extra).
    pub(crate) fn guard_engine(&self) -> awili_config::EngineConfig {
        let mut engine = self.work.effective_engine();
        if !matches!(self.work.executor, WorkExecutor::InAppExtra { .. }) {
            if let Some(key) = &self.keys.engine {
                engine.api_key = key.clone();
            }
        }
        engine
    }

    /// The Lemonade read finished: send on, or refuse with the card.
    fn finish_guard(
        &mut self,
        refusal: Option<(String, Option<crate::strip::Resident>)>,
        engine: awili_config::EngineConfig,
        cancel: Arc<AtomicBool>,
        proceed: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        // The worker is built from the engine that is effective NOW. If the
        // owner changed it while the host was read, that read says nothing
        // about it: check again.
        let now = self.guard_engine();
        if (now.base_url.as_str(), now.model.as_str())
            != (engine.base_url.as_str(), engine.model.as_str())
        {
            return self.guard_in_app(cancel, cx, proceed);
        }
        let Some((card, resident)) = refusal else {
            return proceed(self, cx);
        };
        if let Some(resident) = resident {
            self.strip_note_residency(&engine.base_url, resident);
        }
        // The turn ends before anything is sent, like an engine that cannot
        // be built: visible, no fallback to another model.
        self.work.running = false;
        self.work.gateway_cmd_tx = None;
        self.work.gateway_stop_pending = false;
        self.work.panel.error = Some(card);
        self.work.panel.status = Some(RunStatus::Failed);
        self.work_finish_stream(cx);
        self.settle_work_chip();
        self.settle_work_folder(cx);
        cx.notify();
    }

    /// Send one Work prompt to the (possibly new) engine worker — the tail
    /// of `submit_work_run`, extracted so the owned-engine path can await
    /// the directives/recall fetch first (ship-fix P0-3).
    pub(crate) fn dispatch_work_prompt_cleared(
        &mut self,
        task: String,
        system: Option<String>,
        folder: Option<std::path::PathBuf>,
        conv_id: String,
        cancel: Arc<AtomicBool>,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.work_engine_kind(), awili_config::EngineKind::Awili) {
            self.record_context_parts(&conv_id, None, folder.as_deref());
        }
        // RC1-M: `system` arrives composed (`work_system_for_engine`).
        // U4: install this conversation's Ask/Auto mode into the shared cell
        // the in-app engine worker reads at each gate (the cell is process-
        // wide, so a different conversation dispatched later re-installs its
        // own choice here).
        crate::ask_mode::install_conversation_mode(
            &self.work.approval_handle,
            &self.work.ask_modes,
            &conv_id,
        );
        // RC1-N-FIX2 (pre-review pass 2): the conversation's previous in-app
        // worker was stopped in a turn and has not published that turn's
        // history yet (a step or a model answer still finishing). The new
        // turn waits for it — a completion barrier, not a timeout — so its
        // worker starts from the stopped turn's completed work. A Stop or a
        // newer submit meanwhile drops this one.
        if matches!(self.work_engine_kind(), awili_config::EngineKind::Awili) {
            if let Some(slot) = stopped_history_pending(&self.work, &conv_id) {
                let epoch = self.work.run_seq;
                self.work.panel.activity = Some("Finishing the stopped turn\u{2026}".into());
                // The wait runs on its own thread (the worker publishes from
                // its thread); a newer Stop or the stopped worker's death
                // releases it.
                let release = Arc::new(AtomicBool::new(false));
                self.work.stopped_barrier = Some((conv_id.clone(), release.clone()));
                let (tx, rx) = async_channel::bounded::<()>(1);
                std::thread::spawn(move || {
                    while !release.load(Ordering::SeqCst)
                        && slot.lock().map(|g| g.is_none()).unwrap_or(false)
                    {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    let _ = tx.send_blocking(());
                });
                cx.notify();
                cx.spawn(async move |this, cx| {
                    let _ = rx.recv().await;
                    this.update(cx, |app, cx| {
                        app.work.stopped_barrier = None;
                        if app.work.run_seq == epoch {
                            app.work.panel.activity = None;
                            app.dispatch_work_prompt(task, system, folder, conv_id, cancel, cx);
                        }
                    })
                    .ok();
                })
                .detach();
                return;
            }
        }
        {
            // RC1-I2: the in-app engine's worker belongs to one conversation.
            if matches!(self.work_engine_kind(), awili_config::EngineKind::Awili) {
                retire_worker_of_other_conversation(&mut self.work, &conv_id);
            }
            // If a persistent worker already exists for this session, just send
            // the prompt. Otherwise spawn a new one.
            if offer_prompt_to_worker(&mut self.work, &task, system.as_deref()) {
                self.work.panel.requested_identity = self.work.worker_identity.clone();
                cx.notify();
                return;
            }
            // Channel broken — worker died; fall through to respawn. A fresh
            // worker starts with no pending Stop and no parked prompt (P3c
            // round 3, R6).
            self.work.gateway_stop_pending = false;
            self.work.queued_after_stop = None;

            self.spawn_work_worker(
                GatewayCmd::Prompt { text: task, system },
                folder,
                conv_id,
                cancel,
                false,
                cx,
            );
        }
        cx.notify();
    }

    /// Start this conversation's engine worker with `first` as its first
    /// command: the spawn half of [`Self::dispatch_work_prompt`].
    /// RC2-R619: a warm-up (`warm`, `first` = [`GatewayCmd::Warm`]) starts it
    /// the same way before the owner's first question; one that cannot be
    /// built touches no run state (no run is going). Returns whether a worker
    /// started.
    pub(crate) fn spawn_work_worker(
        &mut self,
        first: GatewayCmd,
        folder: Option<std::path::PathBuf>,
        conv_id: String,
        cancel: Arc<AtomicBool>,
        warm: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        {
            let gw_cfg = self.work.gateway.clone();
            let engine = self.work.effective_engine();
            // The bearer belongs to the endpoint it was set for: the primary
            // uses the app-resolved key (config over `AWILI_ENGINE_KEY`); an
            // `[[engines]]` extra only its own `api_key` -- never the primary's
            // or the environment's, which would send them to another server.
            let engine_key: Option<String> = match &self.work.executor {
                WorkExecutor::InAppExtra { .. } => {
                    Some(engine.api_key.clone()).filter(|k| !k.is_empty())
                }
                _ => self.keys.engine.clone(),
            };
            let folder_str = folder
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "/tmp".to_string());

            let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<GatewayCmd>();
            let (event_tx, event_rx) = async_channel::unbounded::<WorkEvent>();

            // Send the first command (the prompt, or a warm-up) immediately.
            let _ = cmd_tx.send(first);
            self.work.gateway_cmd_tx = Some(cmd_tx);
            self.work.worker_conv = Some(conv_id.clone());
            self.work.retire_after_run = false;

            // D1 seam: both engines run behind the neutral `EngineWorker`
            // trait, and the app-side adapter maps their factual `WorkEvent`s
            // back onto the existing `WorkUpdate` reducer. Selecting one is
            // therefore a single choice of box — nothing downstream of here
            // knows which engine it is talking to (D2-u2a).
            let worker: Box<dyn awili_work::EngineWorker> = match engine.kind {
                awili_config::EngineKind::HermesGateway => {
                    Box::new(HermesGatewayWorker { cfg: gw_cfg })
                }
                awili_config::EngineKind::Awili => {
                    // Ship-fix P0-2 / RC1-J / RC1-J1-FIX2: the new worker
                    // starts from what came BEFORE the turn being submitted
                    // (submit_work_run already pushed the trailing user +
                    // empty assistant rows): the conversation's exact model
                    // history as its own past when the transcript still
                    // begins with what it covers, and everything else —
                    // another helper's turns, or the whole transcript when
                    // nothing is kept — only as labelled context.
                    let msgs = self
                        .views
                        .get(&conv_id)
                        .map(|v| v.convo.messages())
                        .unwrap_or_default();
                    // RC2-R619: a warm-up comes before the turn's rows are
                    // pushed, so its worker starts from the whole transcript.
                    let before = if warm {
                        msgs
                    } else {
                        &msgs[..msgs.len().saturating_sub(2)]
                    };
                    // RC1-N-FIX2 (pre-review P1): a turn this conversation's
                    // previous worker was stopped in keeps its completed
                    // work — wait (bounded) for that worker to publish it,
                    // then keep it as covering the transcript so far.
                    if let Some(kept) = take_stopped_history(&mut self.work, &conv_id, before) {
                        inapp_history::save(&conv_id, &kept);
                        self.work.kept_histories.insert(conv_id.clone(), kept);
                    }
                    if !self.work.kept_histories.contains_key(&conv_id) {
                        if let Some(kept) = inapp_history::load(&conv_id) {
                            self.work.kept_histories.insert(conv_id.clone(), kept);
                        }
                    }
                    let (history, restored) =
                        inapp_history::resume(self.work.kept_histories.get(&conv_id), before);
                    let slot = awili_engine_worker::HistorySlot::default();
                    match Self::build_awili_engine(
                        &engine,
                        engine_key.as_deref(),
                        &folder_str,
                        &conv_id,
                        restored,
                        history,
                        self.work.approval_handle.clone(),
                    ) {
                        Ok(w) => {
                            self.work.inapp_history_slot = Some((conv_id.clone(), slot.clone()));
                            Box::new(w.with_history_slot(slot))
                        }
                        Err(msg) => {
                            // RC2-R619: a warm-up that cannot start is no run;
                            // the owner's first question will say why.
                            if warm {
                                self.work.gateway_cmd_tx = None;
                                self.work.worker_conv = None;
                                eprintln!("awili: the warm-up worker did not start: {msg}");
                                return false;
                            }
                            // Fail the run visibly rather than falling back to
                            // the other engine: silently running an engine the
                            // user did not select is precisely the drift this
                            // config flag exists to prevent.
                            self.work.running = false;
                            self.work.gateway_cmd_tx = None;
                            self.work.gateway_stop_pending = false;
                            self.work.panel.error = Some(msg);
                            // Finalize the accepted turn even though no worker started.
                            self.work.panel.status = Some(RunStatus::Failed);
                            self.work_finish_stream(cx);
                            // RC1-S-FIX1: the chip settles on this ending too.
                            self.settle_work_chip();
                            self.settle_work_folder(cx);
                            cx.notify();
                            return false;
                        }
                    }
                }
            };
            self.work.run_seq = self.work.run_seq.wrapping_add(1);
            let worker_epoch = self.work.run_seq;
            // RC1-N-FIX2: this worker's history slot (in-app only), to end a
            // Stop barrier if the worker dies without publishing.
            let my_slot = self
                .work
                .inapp_history_slot
                .as_ref()
                .filter(|(c, _)| *c == conv_id)
                .map(|(_, s)| s.clone());
            let identity = WorkRunIdentity::worker_on(
                &engine,
                self.work.executor.extra_engine_id().map(String::from),
            );
            self.work.worker_identity = Some(identity.clone());
            // RC1-F4b-FIX1: the ledger's in-app locality is the endpoint of
            // THIS worker, which serves every turn until it is respawned; a
            // later config re-apply does not move it.
            self.work.graduation.set_engine_url(&engine.base_url);
            // RC2-R619: a warm-up is no run of the panel's.
            if !warm {
                self.work.panel.requested_identity = Some(identity);
            }
            // RC2-R6M2 / RC2-R6M4: this worker's serial, to match the warm-ups
            // sent to it (`inapp_warm`): a folder is counted warm only once the
            // worker says the server read it, and a worker that ends without
            // saying so left it unread. Its folder: a turn it answers reads
            // what a warm-up would.
            self.work.warm.worker_serial = self.work.warm.worker_serial.wrapping_add(1);
            let serial = self.work.warm.worker_serial;
            self.work.warm.worker_engine = Some(crate::inapp_warm::engine_id(
                &engine.base_url,
                &engine.model,
            ));
            // RC2-R6M7: kept by this worker's model server and model too.
            let warm_folder = crate::inapp_warm::work_warm_key(&engine, &folder_str);
            spawn_engine_worker(
                worker,
                WorkerContext {
                    workdir: folder_str,
                    cmd_rx,
                    cancel,
                    events: event_tx,
                },
            );

            // RC1-H1: an in-app run's tool events feed Awili's own record of
            // the files it changed.
            let in_app = matches!(engine.kind, awili_config::EngineKind::Awili);
            // RC1-I3-FIX1: the in-app model is named by its display label in
            // Progress too, never by the model server's file path.
            let model_path = engine.model.clone();
            let model_label = engine.display_label();
            cx.spawn(async move |this, cx| {
                while let Ok(ev) = event_rx.recv().await {
                    let gone = this
                        .update(cx, |app, cx| {
                            let now = std::time::Instant::now();
                            match &ev {
                                WorkEvent::Warmed { ok } => {
                                    app.work.warm.worker_warmed(serial, Some(*ok), now);
                                }
                                WorkEvent::TurnCompleted {
                                    status: RunStatus::Completed,
                                    ..
                                } if in_app => {
                                    app.work.warm.warmed.insert(warm_folder.clone(), now);
                                }
                                _ => {}
                            }
                            if app.work.run_seq != worker_epoch {
                                return;
                            }
                            if in_app {
                                app.work.inapp_files.observe(&ev);
                            }
                            for mut up in work_event_to_updates(ev) {
                                if in_app {
                                    name_model_by_label(&mut up, &model_path, &model_label);
                                }
                                app.apply_work_update(up, cx);
                            }
                        })
                        .is_err();
                    if gone {
                        break;
                    }
                }
                this.update(cx, |app, cx| {
                    app.work
                        .warm
                        .worker_warmed(serial, None, std::time::Instant::now());
                    // RC1-N-FIX2: a stopped worker that ended without
                    // publishing its history (it died) releases the Stop
                    // barrier; the next worker recovers its completed steps
                    // from the log.
                    if let Some(mine) = &my_slot {
                        let dead: Vec<String> = app
                            .work
                            .stopped_histories
                            .iter()
                            .filter(|(_, (stopped, _))| {
                                Arc::ptr_eq(mine, stopped)
                                    && mine.lock().map(|g| g.is_none()).unwrap_or(true)
                            })
                            .map(|(c, _)| c.clone())
                            .collect();
                        for c in dead {
                            app.work.stopped_histories.remove(&c);
                            if let Some((waiting, release)) = &app.work.stopped_barrier {
                                if *waiting == c {
                                    release.store(true, Ordering::SeqCst);
                                }
                            }
                        }
                    }
                    if app.work.run_seq != worker_epoch {
                        return;
                    }
                    // P3c round 3 (R5/R6): the worker is gone. It reports
                    // `Terminal { Stopped }` on every Stop exit, so a Stop
                    // still pending here means it died without one; take its
                    // end as that acknowledgement (same reducer path).
                    if app.work.gateway_stop_pending {
                        app.apply_work_update(
                            WorkUpdate::Terminal {
                                status: RunStatus::Stopped,
                                output: None,
                                model: None,
                            },
                            cx,
                        );
                    }
                    if let Some(card) = &mut app.work.panel.approval {
                        if card.delivery.pending.is_some() {
                            card.delivery.pending = None;
                            card.delivery.failed = true;
                            cx.notify();
                        }
                    }
                })
                .ok();
            })
            .detach();
        }

        cx.notify();
        true
    }

    /// Apply one worker update to the live panel.
    pub fn apply_work_update(&mut self, up: WorkUpdate, cx: &mut Context<Self>) {
        let waiting_before = self.notify_waiting_key();
        match up {
            WorkUpdate::ApprovalDelivery { id, delivered } => {
                let panel = &mut self.work.panel;
                let choice = panel.complete_approval_delivery(id.as_deref(), delivered);
                // U3e1: a delivered verdict leaves its outcome in the chat.
                if let Some(line) = choice.and_then(crate::approval_keys::outcome_line) {
                    self.work_stream_token(&line, cx);
                }
            }
            WorkUpdate::Started(id) => {
                self.work.session_id = Some(id);
                self.work.subagents.clear();
            }
            // P3g: an `approval.request` buffered before a gateway Stop must
            // not reopen the stopped turn's card while the Stop is pending
            // (the drain already consumes the backend's side, P3c R7(a)).
            WorkUpdate::Event(RunEvent::Approval(_)) if self.work.gateway_stop_pending => {}
            WorkUpdate::Event(ev) => {
                // RC2-UX (D-PUNCH S2-F15): text after a tool step or a card
                // is a new part of the reply.
                if matches!(&ev, RunEvent::ToolProgress(p) if p.tool.is_some())
                    || matches!(&ev, RunEvent::Approval(_))
                {
                    self.work.text_break_due = true;
                }
                self.work.panel.apply_event(&ev);
                if self.work.panel.error.is_some() {
                    self.capture_work_error(None);
                }
            }
            WorkUpdate::Reasoning { done } => self.work.panel.apply_reasoning(done),
            // RC1-N: the owner's Deny is ending this in-app turn; nothing
            // more the turn says reaches its reply (it becomes one line).
            WorkUpdate::ReplyRestart(shown) => self.work_take_back_reply(&shown, cx),
            WorkUpdate::Token(_) if self.work.deny_stopped.is_some() => {}
            WorkUpdate::Token(text) => {
                self.work.waiting_for_stream_output = false;
                let text = self.with_text_break(text);
                self.work_stream_token(&text, cx);
                let strip_conv = self.work_conv_id();
                self.strip_on_token(&strip_conv, cx);
            }
            WorkUpdate::ReportedCost(c) => {
                // Hermes priced the turn for us; count it like a usage event so
                // the global status-bar chip shows Work-mode spend too.
                self.note_work_cost(c);
                // P8b: the ledger line for this turn carries the cost note.
                self.work.graduation.note_cost(c);
            }
            WorkUpdate::ReportedUsage(u) => {
                let strip_conv = self.work_conv_id();
                self.strip_on_usage(
                    &strip_conv,
                    u64::from(u.input_tokens),
                    u64::from(u.output_tokens),
                );
                self.note_work_usage(u)
            }
            WorkUpdate::Subagent(kind) => self.apply_subagent_event(*kind),
            WorkUpdate::ModelDetected(m) => {
                self.work.panel.model = Some(m);
            }
            WorkUpdate::Terminal {
                status,
                output,
                model,
            } => {
                // P3c: this is the gateway worker's acknowledgement of a
                // Stop; the next prompt may reuse the session again — unless
                // the folder changed meanwhile, in which case the barrier's
                // end retires the old worker first (round 4, R9).
                end_gateway_stop_barrier(&mut self.work);
                self.work.panel.status = Some(status);
                // RC1-Q-FIX1: a stopped (or otherwise ended) turn closes its
                // running rows too, not only a `Completion`.
                self.work
                    .panel
                    .close_running_rows(status == RunStatus::Failed);
                self.work_settle_deny_stop(cx);
                self.work.panel.model = model;
                self.work.panel.approval = None;
                self.work.panel.credential = None;
                self.credential_input.update(cx, |c, _| {
                    c.take();
                });
                if status.is_success() {
                    self.work.panel.mark_all_done();
                    if self.work.panel.output.is_none() {
                        self.work.panel.output = output;
                    }
                // P3i: a Stop is not an error — the header reads `stopped` from
                // `panel.status` (no synthesized `run ended: stopped`), the
                // worker's output (when sent) is the run's output, and any
                // error already on the panel is left as it was (normally none).
                } else if status == RunStatus::Stopped {
                    // Only a worker-sent output replaces the panel's: every
                    // stop path sends `None`, and a mid-turn Completion may
                    // already hold the run's only text (OCR/DeepSeek review).
                    if output.is_some() {
                        self.work.panel.output = output;
                    }
                } else {
                    self.work.panel.error =
                        output.or_else(|| Some(format!("run ended: {}", status_label(status))));
                }
                self.work_finish_stream(cx);
                self.work.running = false;
                self.refresh_outputs();
                // P3c round 3 (R5): the stopped turn is finished (its row
                // holds every forwarded denial); now run the prompt parked
                // behind the Stop, once.
                self.replay_queued_after_stop(cx);
                // RC1-O (pre-review 5 P1): only after the parked prompt went
                // out on the chip it was queued under — a replayed run is
                // live, so this leaves its chip; with nothing parked, the
                // active conversation gets its own lane.
                self.settle_work_chip();
                self.settle_work_folder(cx);
            }
            WorkUpdate::TurnComplete => {
                // A finished turn can't still be blocked on an approval or
                // credential prompt — drop any open overlay so it can't linger
                // past the turn it belonged to (matches the upstream desktop).
                self.work.panel.approval = None;
                self.work.panel.credential = None;
                self.credential_input.update(cx, |c, _| {
                    c.take();
                });
                // RC2-RUN (round 3 Minor 12): an in-app turn that completed
                // is proof the local model answers — a stale "not answering"
                // from an earlier check goes (never over a check in flight).
                if self.work.panel.status == Some(RunStatus::Completed)
                    && matches!(
                        self.work.panel.requested_identity,
                        Some(WorkRunIdentity::OwnedEngine { .. })
                    )
                    && !self.work.local.health.probing()
                {
                    self.work.local.rechecking = false;
                    self.work
                        .local
                        .health
                        .finish_probe(Ok(()), std::time::Instant::now());
                }
                self.work_settle_deny_stop(cx);
                self.work_finish_stream(cx);
                self.work.running = false;
                self.settle_work_chip();
                self.settle_work_folder(cx);
                self.refresh_outputs();
            }
            WorkUpdate::Failed(msg) => {
                let msg = image_endpoint_error(&msg)
                    .map(str::to_string)
                    .unwrap_or(msg);
                // A missing-provider / missing-key failure gets a next step,
                // not just the raw backend message.
                let msg = match provider_setup_hint(&msg) {
                    Some(hint) => format!("{msg}{hint}"),
                    None => msg,
                };
                // P3c round 6 (R13): while a gateway Stop is pending, only
                // `Terminal { Stopped }` (or the dead-worker fallback that
                // replays it) acknowledges the Stop. A `Failed` the worker
                // emitted before it read the Cancel — e.g. a timed-out
                // submission, already buffered when Stop was pressed — is
                // evidence on the stopped turn, not the end of the barrier:
                // the worker may still be interrupting the backend, so the
                // flag, sender, session, epoch and parked prompt all stay.
                if self.work.gateway_stop_pending {
                    self.work_record_failure_during_stop(&msg, cx);
                    cx.notify();
                    return;
                }
                self.work.panel.error = Some(msg);
                self.work.panel.status = Some(RunStatus::Failed);
                self.work.deny_stopped = None;
                if let Some(card) = &mut self.work.panel.approval {
                    if card.delivery.pending.is_some() || card.delivery.failed {
                        card.delivery.pending = None;
                        card.delivery.failed = true;
                    } else {
                        self.work.panel.approval = None;
                    }
                }
                self.work.panel.credential = None;
                self.credential_input.update(cx, |c, _| {
                    c.take();
                });
                // RC1-Q-FIX1: a failed run closes its running rows.
                self.work.panel.close_running_rows(true);
                self.work.running = false;
                // Round 4 (R9): a deferred folder change retires the old
                // worker with `Shutdown` before its sender is dropped.
                end_gateway_stop_barrier(&mut self.work);
                self.work.gateway_cmd_tx = None;
                self.work.session_id = None;
                self.work.retire_after_run = false; // RC1-I2-FIX2: the worker is gone
                                                    // Failed is terminal just like Terminal/TurnComplete. This
                                                    // owns the once-only transcript, graduation, voice and memory
                                                    // completion path; late callbacks find no stream row.
                self.work_finish_stream(cx);
                // P3c round 3 (R5): no Stop is pending here (round 6, R13
                // returned above), so this replays nothing in practice; kept
                // so the no-Stop path is unchanged.
                self.replay_queued_after_stop(cx);
                // RC1-O (pre-review 5 P1): after any parked prompt went out
                // on its own chip.
                self.settle_work_chip();
                self.settle_work_folder(cx);
                // L1e2: a failed in-app turn checks the local model once.
                self.local_after_failure(cx);
            }
            WorkUpdate::FileTouched(path) => {
                if !self.work.touched_files.contains(&path) {
                    self.work.touched_files.push(path);
                }
            }
            WorkUpdate::UrlReferenced(url) => {
                if !self.work.referenced_urls.contains(&url) {
                    self.work.referenced_urls.push(url);
                }
            }
            WorkUpdate::TerminalReadRequest { request_id } => {
                let text = self
                    .work
                    .terminals
                    .first_mut()
                    .map(|t| {
                        t.poll();
                        t.screen_text()
                    })
                    .unwrap_or_default();
                if let Some(cmd_tx) = &self.work.gateway_cmd_tx {
                    let _ = cmd_tx.send(GatewayCmd::TerminalReadRespond { request_id, text });
                }
            }
            WorkUpdate::SudoRequest { request_id } => {
                self.credential_input.update(cx, |c, _| {
                    c.take();
                });
                self.work.panel.credential = Some(CredentialPrompt {
                    request_id,
                    kind: CredentialKind::Sudo,
                    env_var: String::new(),
                    prompt: String::new(),
                });
                self.work.panel.activity = Some("agent is requesting a sudo password".to_string());
            }
            WorkUpdate::SecretRequest {
                request_id,
                env_var,
                prompt,
            } => {
                self.credential_input.update(cx, |c, _| {
                    c.take();
                });
                let what = if env_var.is_empty() {
                    "a credential".to_string()
                } else {
                    format!("the {env_var} secret")
                };
                self.work.panel.credential = Some(CredentialPrompt {
                    request_id,
                    kind: CredentialKind::Secret,
                    env_var,
                    prompt,
                });
                self.work.panel.activity = Some(format!("agent is requesting {what}"));
            }
        }
        self.notify_work_waiting(waiting_before);
        cx.notify();
    }

    /// R1h: re-read the bound folder's Files listing (flat list and tree)
    /// from disk.
    pub(crate) fn refresh_folder_listing(&mut self) {
        let Some(folder) = self.work.folder.clone() else {
            return;
        };
        self.work.folder_files = list_dir_top(&folder, 14);
        if let Some(tree) = &mut self.work.workspace_tree {
            tree.reload();
            self.work.file_tree = crate::workspace::file_tree_from_workspace(tree);
        }
    }

    /// New top-level entries that appeared since the submit snapshot (the OUTPUTS).
    fn refresh_outputs(&mut self) {
        let Some(folder) = self.work.folder.clone() else {
            return;
        };
        let now = dir_snapshot(&folder);
        let mut new: Vec<String> = now
            .difference(&self.work.folder_snapshot)
            .cloned()
            .collect();
        new.sort();
        self.work.outputs = new;
        self.work.folder_files = list_dir_top(&folder, 14);
        if let Some(tree) = &mut self.work.workspace_tree {
            tree.poll_changes();
            self.work.file_tree = crate::workspace::file_tree_from_workspace(tree);
        } else {
            self.work.file_tree = folder_tree_snapshot(&folder);
        }
        for term in &mut self.work.terminals {
            term.poll();
        }
    }

    /// Fold one subagent lifecycle event into the agents tree.
    /// Fold one raw `subagent.*` event into the live store via the unit-18
    /// reducer. Arrival time (wall clock) drives each agent's `started_at` /
    /// `updated_at`, and hence the per-agent elapsed timer the AGENTS card shows.
    fn apply_subagent_event(&mut self, kind: GatewayEventKind) {
        upsert_subagent(&mut self.work.subagents, &kind, now_ms());
    }

    /// Resolve the pending approval gate with a scoped choice (P3a: Allow
    /// once / for session / for folder / always, or Deny).
    ///
    /// Fail-closed: a choice the card does NOT offer (e.g. `always` for an
    /// irreversible/privileged risk, or `folder` with no bound folder) is
    /// refused — no command is sent and it is never relabeled as `once` —
    /// and the card stays up because the engine gate is still pending.
    ///
    /// The exact P3a scope string then routes by engine kind (P10/F1): the
    /// Hermes gateway keeps the `ApprovalDelivery` ack path, while the owned
    /// `awili` engine receives its ORIGINAL approval id — the id it minted
    /// and the only one it will correlate a verdict against.
    pub fn resolve_work_approval(&mut self, decision: ApprovalDecision, cx: &mut Context<Self>) {
        // U4e1 (D11): an open plan card takes the decision first (any executor).
        if self.resolve_plan_gate(decision, cx) {
            return;
        }
        if !matches!(
            self.work.executor,
            WorkExecutor::Gateway | WorkExecutor::InApp | WorkExecutor::InAppExtra { .. }
        ) {
            return;
        }
        // The bound folder decides whether `folder` is offerable (P3a).
        let folder_bound = self.work.folder.is_some();
        // Read the card (rather than just clearing it): its `approval_id` is
        // what the engine correlates the verdict against (D2-u2a), so it has
        // to be read before the card goes away.
        let Some(card) = self.work.panel.approval.as_ref() else {
            return;
        };
        // RC2-UX (D-PUNCH S2-F16): a question is answered in the message box,
        // never by an Allow/Deny verdict (a keyboard chord included).
        if card.kind == CLARIFY_KIND {
            return;
        }
        // Fail-closed capability check (P3a): refuse what the card cannot
        // offer — never send a relabeled scope.
        let Some(choice) = resolve_approval_choice(card, decision, folder_bound) else {
            return;
        };
        let engine_kind = crate::local_first::identity_engine_kind(
            self.work.worker_identity.as_ref(),
            &self.work.executor,
        );
        // RC1-N: on the in-app engine, Deny ends the turn (`deny-stop`): the
        // reply becomes one plain line and the request goes back in the box,
        // as with the Claude/Codex plan card.
        let deny_stop = choice == "deny" && engine_kind == awili_config::EngineKind::Awili;
        let wire = if deny_stop { "deny-stop" } else { choice };
        let Some(card) = self.work.panel.approval.as_mut() else {
            return;
        };
        let declined = card.prompt.clone();
        if route_work_approval(engine_kind, card, wire, self.work.gateway_cmd_tx.as_ref()) {
            // RC2-R4 (D-R1 M10c): the answered card keeps its place, inert.
            if let Some(card) = self.work.panel.approval.take() {
                self.work.panel.decided = Some(DecidedApproval { card, choice });
            }
            if deny_stop {
                let changed = self.work.inapp_files.changed_any();
                let conv_id = self.work_conv_id();
                self.work.deny_stopped = Some(DenyStop {
                    line: deny_stop_line(&declined, &self.work.inapp_files.footer_part(), changed),
                    request: self
                        .views
                        .get(&conv_id)
                        .and_then(|v| crate::plan_gate::plan_request_text(v.convo.messages())),
                    toast: crate::plan_gate::DENY_STOPPED_TOAST,
                });
                // RC2-RUN (R3c-7): the turn can take a while to wind down;
                // Progress and the reply say it is still working.
                self.work.panel.activity = Some(DENY_WINDING_DOWN.to_string());
                self.work.waiting_for_stream_output = true;
            } else if let Some(line) = crate::approval_keys::outcome_line(choice) {
                // U3e1: the owned engine got the verdict; say so in the chat.
                self.work_stream_token(&line, cx);
            }
        }
        cx.notify();
    }

    /// RC1-N: [`Self::resolve_work_approval`] from a pill or chord that has
    /// the window — after an in-app Deny put the request back in the box,
    /// the cursor goes there too (as with the plan card's Deny).
    pub(crate) fn resolve_work_approval_focusing(
        &mut self,
        decision: ApprovalDecision,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.resolve_work_approval(decision, cx);
        if self.work.deny_stopped.is_some() {
            self.plan_focus_composer(window, cx);
        }
    }

    /// Decline a pending credential request: send the empty (safe-refusal)
    /// response so the agent unblocks immediately instead of stalling to the
    /// backend's timeout, then clear the card. Per the Hermes contract an empty
    /// value is the safe refusal — no command runs, no credential is granted.
    /// (Supplying the actual value is a follow-up needing a masked secure input.)
    pub fn refuse_credential(&mut self, cx: &mut Context<Self>) {
        // Drop anything typed — a declined prompt must leave no value behind.
        self.credential_input.update(cx, |c, _cx| {
            c.take();
        });
        self.send_credential_response(String::new(), "credential request declined", cx);
    }

    /// The user supplied the pending credential value: drain it from the isolated
    /// input (so it never lingers in the buffer), send it to the agent, and clear
    /// the card. An empty value is the safe refusal. The value is never persisted
    /// or logged — it lives only in the transient input and the (Debug-less)
    /// gateway command.
    pub fn submit_credential(&mut self, cx: &mut Context<Self>) {
        let value = self.credential_input.update(cx, |c, _cx| c.take());
        let label = if value.trim().is_empty() {
            "credential request declined"
        } else {
            "credential sent"
        };
        self.send_credential_response(value, label, cx);
    }

    /// Shared tail of refuse/submit: route `value` to the right gateway respond
    /// for the pending credential request, then clear the card.
    fn send_credential_response(&mut self, value: String, label: &str, cx: &mut Context<Self>) {
        let Some(cred) = self.work.panel.credential.take() else {
            return;
        };
        if let Some(cmd_tx) = &self.work.gateway_cmd_tx {
            let cmd = match cred.kind {
                CredentialKind::Sudo => GatewayCmd::SudoRespond {
                    request_id: cred.request_id,
                    password: value,
                },
                CredentialKind::Secret => GatewayCmd::SecretRespond {
                    request_id: cred.request_id,
                    value,
                },
            };
            let _ = cmd_tx.send(cmd);
        }
        self.work.panel.activity = Some(label.to_string());
        cx.notify();
    }

    /// S5 INT1: the agent lane awaits the SAME per-turn memory gate as the
    /// owned engine and Chat BEFORE anything is spawned. Disabled memory
    /// skips the gate (no fetch; RC2-FIXC1: no counts left). While the gate
    /// is in flight the turn counts as running, so a second submit is
    /// ignored; a Stop in that window settles the turn as Stopped
    /// (`cancel_work_run`) and bumps `run_seq`, so the late answer finds the
    /// turn superseded and touches NOTHING — not the chips, not the panel.
    fn submit_gated_work_agent(
        &mut self,
        task: String,
        engine: CodeEngine,
        model: Option<String>,
        gate_attachments: Vec<Attachment>,
        attachment_owner: String,
        cx: &mut Context<Self>,
    ) {
        // Freeze the accepted target and bridge before the asynchronous memory
        // gate. A chooser change while that gate is waiting changes only the
        // next turn, never this provider, Stop owner, or native session key.
        let conv_id = self.work_conv_id();
        let bound = self.work.folder.is_some();
        let plan_folder = self.work.folder.clone();
        let Some(folder) = self
            .work
            .folder
            .clone()
            .or_else(|| crate::home::home_os().map(PathBuf::from))
        else {
            self.work.agent_gate_attachments = Some((attachment_owner, gate_attachments));
            self.refuse_work_agent_turn("agent runs need a working folder — choose one first", cx);
            return;
        };
        let cwd = folder.display().to_string();
        self.work.run_seq = self.work.run_seq.wrapping_add(1);
        let run_id = self.work.run_seq;
        let canonical = self
            .views
            .get(&conv_id)
            .map(bridge_records_from_view)
            .unwrap_or_default();
        let before_current = &canonical[..canonical.len().saturating_sub(2)];
        let target = WorkExecutor::Agent {
            engine,
            model: model.clone(),
        };
        let active = self
            .work
            .conversation_routes
            .get_mut(&conv_id)
            .and_then(|route| {
                route
                    .begin_turn_for_target(
                        conv_id.clone(),
                        run_id,
                        cwd.clone(),
                        awili_code::continuity::canonical_cursor(before_current),
                        target,
                    )
                    .ok()
            });
        let Some(active) = active else {
            self.work.agent_gate_attachments = Some((attachment_owner, gate_attachments));
            self.refuse_work_agent_turn(
                "This conversation already has a native turn running. Stop it before sending another request.",
                cx,
            );
            return;
        };
        let Some(native) = active.native.as_ref() else {
            self.work.agent_gate_attachments = Some((attachment_owner, gate_attachments));
            self.refuse_work_agent_turn("This conversation has no native target selected.", cx);
            return;
        };
        // Consume an approved release while this accepted owner is still
        // synchronous. A memory callback must never resolve posture after a
        // later chooser/folder change, nor consume an approval after Stop.
        let autonomy = self.agent_autonomy_for_accepted_turn(
            &conv_id,
            engine,
            model.as_deref(),
            plan_folder.as_deref(),
        );
        let plan = native.prepare_bridge(
            before_current,
            &task,
            &cwd,
            &[],
            awili_code::continuity::PRODUCTION_HISTORY_BUDGET,
        );
        let slot = std::sync::Arc::new(std::sync::Mutex::new(plan.resume_id().map(str::to_string)));
        self.work.active_work_route_turn = Some(active.clone());
        self.work.agent_gate_attachments = Some((attachment_owner, gate_attachments));
        self.persist_conversation_route(&conv_id);
        if !self.memory.enabled && crate::resume_context::request(&task).is_none() {
            // RC2-FIXC1 (RC2-FIXB review P2): no gate runs, so this
            // conversation keeps no older counts.
            self.clear_memory_chip(&conv_id);
            self.run_work_via_agent(
                task, engine, model, autonomy, None, folder, bound, active, plan, slot, cx,
            );
            return;
        }
        let epoch = run_id;
        self.work.panel = RunPanel::default();
        self.work.error_presentation = None;
        self.work.panel.requested_identity = Some(WorkRunIdentity::Agent {
            engine,
            model: model.clone(),
        });
        self.work.agent_gate_pending = true;
        self.work.running = true;
        self.work.waiting_for_stream_output = true;
        self.ensure_thinking_timer(cx);
        cx.notify();
        let mem = self.memory.clone();
        let query = task.clone();
        let resume_cwd = cwd.clone();
        let resume_mem = mem.clone();
        let resume_connectors = self.work.connectors.clone();
        let mut excluded_threads = vec![conv_id.clone()];
        if let Some(id) = plan.resume_id() {
            excluded_threads.push(id.to_string());
        }
        cx.spawn(async move |this, cx| {
            let gate =
                crate::recall::gate_turn_memory(mem, query, crate::recall::production_client).await;
            let evidence = crate::resume_context::collect(
                &task,
                resume_cwd,
                resume_mem,
                resume_connectors,
                excluded_threads,
            )
            .await;
            let mut plan = plan;
            plan.prompt = crate::resume_context::enrich(plan.prompt, evidence);
            this.update(cx, |app, cx| {
                #[cfg(test)]
                TEST_AGENT_GATE_ANSWERS.with(|n| n.set(n.get() + 1));
                if app.work.run_seq != epoch || !app.work.agent_gate_pending {
                    return;
                }
                app.work.agent_gate_pending = false;
                if app.memory.enabled {
                    if let Some(s) = &gate.rules {
                        app.note_rules_transition(s);
                    }
                    app.rules = gate.rules.clone();
                    app.recall = gate.recall.clone();
                    // RC2-UX (R5-9): the chips belong to this conversation.
                    let id = app
                        .work
                        .conv_id
                        .clone()
                        .unwrap_or_else(|| app.sidebar.active_id().to_string());
                    app.note_memory_chip(&id);
                }
                app.work.running = false;
                match gate.outcome {
                    Ok(policy) => app.run_work_via_agent(
                        task, engine, model, autonomy, policy, folder, bound, active, plan, slot,
                        cx,
                    ),
                    Err(reason) => {
                        app.refuse_work_agent_turn(&reason, cx);
                        // RC1-S-FIX1: a refused turn ends the run too — the
                        // chip settles.
                        app.settle_work_chip();
                        app.settle_work_folder(cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// S5 INT1: a rejected policy ends the agent turn before any spawn, with
    /// the same refusal text the owned engine and Chat show.
    fn refuse_work_agent_turn(&mut self, reason: &str, cx: &mut Context<Self>) {
        let conv_id = self.work_conv_id();
        if let Some(active) = self.work.active_work_route_turn.take() {
            if let Some(route) = self
                .work
                .conversation_routes
                .get_mut(&active.conversation_id)
            {
                route.finish_unstarted_turn(&active);
            }
            self.persist_conversation_route(&active.conversation_id);
        }
        self.restore_agent_gate_attachments(cx);
        self.work.continuity.leave_agent(&conv_id);
        self.work.panel = RunPanel::default();
        self.work.error_presentation = None;
        self.work.panel.error = Some(crate::recall::policy_refusal(reason));
        self.work.panel.status = Some(RunStatus::Failed);
        self.work_finish_stream(cx);
        self.show_policy_refused_notice(cx);
    }

    /// Return files only from the accepted-but-unspawned gate owner. This is
    /// called by both refusal and Stop, before a late gate callback can touch
    /// a newer turn or another conversation's tray.
    fn restore_agent_gate_attachments(&mut self, cx: &mut Context<Self>) {
        let Some((owner, attachments)) = self.work.agent_gate_attachments.take() else {
            return;
        };
        let errors = if let Some(store) = self.search_store.as_ref() {
            let already_staged: HashSet<i64> = store
                .get_staged_attachments(&owner)
                .unwrap_or_default()
                .into_iter()
                .map(|attachment| attachment.id)
                .collect();
            attachments
                .iter()
                .filter(|attachment| !already_staged.contains(&attachment.id))
                .filter_map(|attachment| {
                    store
                        .stage_attachment(&owner, attachment.id)
                        .err()
                        .map(|error| error.to_string())
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for error in errors {
            self.show_toast(format!("Could not return a file to the tray — {error}"), cx);
        }
        if self.sidebar.active_id() == owner {
            self.refresh_staged_attachments();
        }
    }

    /// Cancel the run: stop the worker's event loop and close the session.
    /// Run one Work turn through a vendor agent harness (Claude Code, Codex,
    /// or ZCode/GLM), streaming its output into the Work transcript. Harness
    /// Work follows Code-mode semantics (design of record §2): the folder is
    /// MANDATORY (it is the agent's cwd), the autonomy posture comes from the
    /// folder's saved Code project (Autonomous by default), the harness owns its own
    /// tools and OAuth, and Stop kills the process group. `policy` is the
    /// memory gate's validated block (S5 INT1), pinned ahead of the lane's
    /// own system text.
    #[allow(
        clippy::too_many_arguments,
        reason = "the existing Work orchestration passes one immutable dispatch snapshot to the provider"
    )]
    fn run_work_via_agent(
        &mut self,
        _task: String,
        engine: CodeEngine,
        model: Option<String>,
        autonomy: awili_code::Autonomy,
        policy: Option<String>,
        folder: PathBuf,
        bound: bool,
        active: crate::conversation_route::ActiveTurn,
        plan: awili_code::continuity::BridgePlan,
        slot: std::sync::Arc<std::sync::Mutex<Option<String>>>,
        cx: &mut Context<Self>,
    ) {
        let conv_id = active.conversation_id.clone();
        // A thread with no chosen folder still runs: an ordinary question, a
        // host or browser task or a read-only look needs none. The helper then
        // starts where Code does (the home folder) — it is not a project
        // binding, so nothing below snapshots it or files the thread under it.
        let cwd = folder.display().to_string();

        let run_id = active.run_id;
        if self.work.active_work_route_turn.as_ref() != Some(&active) || self.work.run_seq != run_id
        {
            return;
        }
        self.record_context_parts(&conv_id, Some(engine), Some(&folder));
        self.work.sandbox_note = Some((
            conv_id.clone(),
            crate::agent_sandbox::status_line(
                self.work.sandbox.enabled && autonomy == awili_code::Autonomy::Autonomous,
                &folder.display().to_string(),
            ),
        ));
        // The gate has accepted this child. From now on its attachments belong
        // to the sent transcript, not to a retryable pre-spawn tray.
        self.work.agent_gate_attachments = None;
        if let Some(route) = self.work.conversation_routes.get_mut(&conv_id) {
            route.mark_native_started(&active);
        }
        self.persist_conversation_route(&conv_id);
        let resume_id = plan.resume_id().map(str::to_string);
        self.work.panel = RunPanel::default();
        self.work.error_presentation = None;
        // RC2-R616: plain words while it starts (it then says its time).
        self.work.panel.activity = Some(format!("{} is working\u{2026}", engine.label()));
        self.work.outputs.clear();
        self.work.folder_snapshot = if bound {
            dir_snapshot(&folder)
        } else {
            Default::default()
        };
        // RC2-R612: what the folder held, to say what the run changed.
        self.work.folder_stamps = if bound {
            crate::run_footer::dir_stamps(&folder)
        } else {
            Default::default()
        };
        self.work.running = true;
        self.work.run_started = Some(std::time::Instant::now());
        self.work.waiting_for_stream_output = true;
        self.ensure_thinking_timer(cx);
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
        self.work.agent_abort_tx = Some(cancel_tx);
        cx.notify();

        let bin = resolve_agent_bin(engine.bin(), &self.work.agents);
        let system = autonomy
            .plan_first()
            .then(|| awili_code::PLAN_FIRST_SYSTEM.to_string());
        // M2b: the reply-style brief, once per engine session — RC1-M: on every
        // turn for Claude and Codex, which take it through their system
        // channel (never into the conversation).
        let system = crate::agent_brief::with_brief(
            system,
            &cwd,
            resume_id.is_none() || crate::agent_launch::has_system_channel(engine),
        );
        let user_text = plan.prompt;
        let mut request = awili_providers::ChatRequest {
            model: String::new(),
            system,
            messages: vec![awili_providers::ChatMessage::user(user_text)],
        };
        // RC1-N-FIX1 (lead ruling 2026-10-04): where the owner says his own
        // hooks bring his memory (`hooks_provide_memory`), Awili pins
        // nothing — neither the standing directives nor the recall; his
        // SessionStart hook delivers the rules once. Otherwise the whole
        // validated preamble is pinned.
        let pinned = if hooks_bring_memory(engine, self.work.agents.memory.as_ref()) {
            None
        } else {
            policy.clone()
        };
        crate::recall::pin_policy(&mut request, pinned.as_deref());
        self.record_agent_context(
            engine,
            model.as_deref(),
            request.system.as_deref(),
            pinned.as_deref(),
            &request.messages[0].content,
        );
        self.work.panel.requested_identity = Some(WorkRunIdentity::Agent {
            engine,
            model: model.clone(),
        });
        let mut provider = crate::code_provider::CodeProvider::new(
            bin,
            engine,
            autonomy,
            cwd,
            resume_id,
            model.clone(),
            slot.clone(),
        )
        .with_search_paths(self.work.agents.search_paths.clone())
        .with_agent_memory(self.work.agents.memory.clone())
        .with_agents_md(self.work.agents.agents_md.clone())
        .with_sandbox(self.work.sandbox.clone())
        // RC2-CXSF: this run's panel learns whether its Codex turn runs on
        // `codex exec --json` (its silence reads as whole messages).
        .observe_exec(self.work.panel.codex_exec.flag());
        // Dsh (P9): the child's credentials come from a file Awili reads
        // itself (the .app inherits no login shell). Only Dsh gets this; the
        // chosen model travels in `DSH_MODEL` (already in `model`), and the
        // env file is resolved to its effective path (config or default).
        if engine == CodeEngine::Dsh {
            provider = provider.with_dsh_env_file(self.work.agents.dsh_env_file());
        }
        // P12: when a connector family is enabled and the server binary
        // resolves, the vendor CLIs get the `awili-connectors` MCP server
        // through their own mechanisms (claude `--mcp-config`, codex `-c`;
        // the flag is dropped for engines without one).
        if let Some(spec) = self.work.connectors.server_config(&self.work.agents) {
            provider = provider.with_connectors_mcp(spec);
        }

        let provider = if let Some(observation) = self.workspace_read_observer(&conv_id, run_id) {
            provider.observe_termination(observation)
        } else {
            provider
        };

        let folder2 = folder.clone();
        let finish_slot = slot.clone();
        cx.spawn(async move |this, cx| {
            let rx = provider.run(request, cancel_rx);
            let mut provider_done = false;
            while let Ok(ev) = rx.recv().await {
                match ev {
                    awili_providers::ProviderEvent::HostApproval(request) => {
                        request.answer(awili_providers::ApprovalChoice::Deny);
                    }
                    awili_providers::ProviderEvent::Token(t) => {
                        this.update(cx, |app, cx| {
                            if app.work.run_seq == run_id {
                                app.work.waiting_for_stream_output = false;
                                app.work_stream_token(&t, cx);
                                let strip_conv = app.work_conv_id();
                                app.strip_on_token(&strip_conv, cx);
                            }
                        })
                        .ok();
                    }
                    // RC2-R2G7: a tool that started shows like any activity.
                    awili_providers::ProviderEvent::Activity(line)
                    | awili_providers::ProviderEvent::ToolStarted(line) => {
                        this.update(cx, |app, cx| {
                            if app.work.run_seq == run_id {
                                app.work.panel.activity = Some(line.clone());
                                app.work_on_activity(line, cx);
                            }
                        })
                        .ok();
                    }
                    awili_providers::ProviderEvent::Error(e) => {
                        this.update(cx, |app, cx| {
                            if app.work.run_seq == run_id {
                                app.work.continuity.leave_agent(&conv_id);
                                app.work.panel.error = Some(e);
                                cx.notify();
                            }
                        })
                        .ok();
                    }
                    awili_providers::ProviderEvent::ToolAttemptFailed => {
                        this.update(cx, |app, cx| {
                            if app.work.run_seq == run_id {
                                app.work.panel.tool_attempt_failed = true;
                                app.work_on_activity(
                                    "A step didn't work; the agent can try another way".into(),
                                    cx,
                                );
                                cx.notify();
                            }
                        })
                        .ok();
                    }
                    awili_providers::ProviderEvent::ContextWindow(w) => {
                        this.update(cx, |app, _cx| {
                            if app.work.run_seq == run_id {
                                app.work.panel.context_window = Some(w);
                                app.work
                                    .reported_windows
                                    .insert(app.work.executor.to_persist(), w);
                            }
                        })
                        .ok();
                    }
                    awili_providers::ProviderEvent::Usage(u) => {
                        this.update(cx, |app, _cx| {
                            if app.work.run_seq == run_id {
                                let strip_conv = app.work_conv_id();
                                app.strip_on_usage(
                                    &strip_conv,
                                    u64::from(u.input_tokens),
                                    u64::from(u.output_tokens),
                                );
                                app.note_work_usage(u);
                            }
                        })
                        .ok();
                    }
                    awili_providers::ProviderEvent::Done => {
                        provider_done = true;
                        break;
                    }
                }
            }
            this.update(cx, |app, cx| {
                app.workspace_read_provider_end(&conv_id, run_id, provider_done, cx);
                let native_id_after = finish_slot.lock().ok().and_then(|id| id.clone());
                let native_completed = provider_done && app.work.panel.error.is_none();
                app.work_agent_run_done_in(
                    run_id,
                    &conv_id,
                    &folder2,
                    Some(&active),
                    native_id_after,
                    native_completed,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    /// The end of an agent-executor Work run, once its provider stream has
    /// closed (N1: moved out of `run_work_via_agent`'s task unchanged, so the
    /// notification proof drives the same code the task runs).
    #[cfg(test)]
    pub(crate) fn work_agent_run_done(
        &mut self,
        run_id: u64,
        folder: &Path,
        cx: &mut Context<Self>,
    ) {
        let conv_id = self.work_conv_id();
        self.work_agent_run_done_in(run_id, &conv_id, folder, None, None, false, cx);
    }

    /// `conv_id` is the conversation the run was submitted in (U4's plan
    /// gate settles there even if the user has moved on).
    #[allow(
        clippy::too_many_arguments,
        reason = "completion must settle the captured run owner and its native outcome together"
    )]
    pub(crate) fn work_agent_run_done_in(
        &mut self,
        run_id: u64,
        conv_id: &str,
        folder: &Path,
        route_owner: Option<&crate::conversation_route::ActiveTurn>,
        native_id_after: Option<String>,
        native_completed: bool,
        cx: &mut Context<Self>,
    ) {
        // Stale-run guard (MUST-1): a cancelled/superseded task must
        // not clear or finish the replacement run's state.
        if self.work.run_seq != run_id {
            return;
        }
        if let Some(active) = route_owner {
            if self.work.active_work_route_turn.as_ref() != Some(active) {
                return;
            }
            let through = self
                .views
                .get(conv_id)
                .map(bridge_records_from_view)
                .map(|records| awili_code::continuity::canonical_cursor(&records))
                .unwrap_or_default();
            if let Some(route) = self.work.conversation_routes.get_mut(conv_id) {
                let finish = if native_completed {
                    crate::conversation_route::NativeTurnFinish::Completed {
                        native_id_after,
                        transcript_through: through,
                    }
                } else {
                    crate::conversation_route::NativeTurnFinish::Interrupted {
                        transcript_through: through,
                    }
                };
                route.finish_turn(active, Some(finish));
            }
            self.work.active_work_route_turn = None;
            self.persist_conversation_route(conv_id);
        }
        let engine = route_owner
            .and_then(|owner| match &owner.target {
                WorkExecutor::Agent { engine, .. } => Some(*engine),
                _ => None,
            })
            .or(match &self.work.executor {
                WorkExecutor::Agent { engine, .. } => Some(*engine),
                _ => None,
            });
        self.work.running = false;
        self.settle_work_chip();
        self.settle_work_folder(cx);
        self.work.agent_abort_tx = None;
        self.work.panel.activity = None;
        // RC2-R56 (usability RC2 round 4: a signed-out Claude printed its
        // raw CLI line twice and never said how to sign in): one plain line
        // with the fix, in place of the CLI's words.
        let signed_out = self.work.panel.error.as_deref().and_then(|e| {
            crate::agent_signin::signed_out_line(engine, e).map(|p| (e.to_string(), p))
        });
        let mut said = false;
        if let Some((raw, plain)) = signed_out {
            self.work.panel.error = Some(plain.clone());
            if self.work_reply_drop(&raw, cx) {
                self.work_stream_token(&format!("\u{26A0} {plain}"), cx);
                said = true;
            }
        }
        if said {
            // The reply is that one line.
        } else if let Some(notice) = self.work.panel.agent_final_notice() {
            self.work_stream_token(&notice, cx);
        }
        if self.work.panel.error.is_none() {
            self.work.panel.status = Some(RunStatus::Completed);
        } else {
            self.work.panel.status = Some(RunStatus::Failed);
        }
        // OUTPUTS: new top-level entries since submit, same rule as
        // the gateway path.
        let after = dir_snapshot(folder);
        self.work.outputs = after
            .difference(&self.work.folder_snapshot)
            .cloned()
            .collect();
        self.work.outputs.sort();
        self.plan_gate_after_work_turn(conv_id, cx);
        self.work_finish_stream(cx);
    }

    /// Local Build lane (daily-driver P2d): resolve the manual override to
    /// its actual adapter, submit ONCE through the awili-dispatch SSH
    /// client, persist the durable request id BEFORE any Running state is
    /// shown, then poll `status` of the SAME id until terminal. Preflight
    /// failures (unknown target, unusable repository state, invalid
    /// request) surface as a panel error with ZERO outbound calls, and the
    /// override is NOT consumed — the selection stands so the user can fix
    /// and retry. No usage row is written on this lane (P5a owns usage):
    /// the stable turn id is carried into the build record, and unknown
    /// token usage stays unknown — never a manufactured zero.
    fn run_work_via_local_build(
        &mut self,
        spec_text: String,
        target_id: String,
        cx: &mut Context<Self>,
    ) {
        let conv_id = self.work_conv_id();
        // Target first: an unknown id is a config error naming the id — no
        // client is built and no outbound call is made (not even the folder
        // git read below).
        let resolved = match crate::build_dispatch::ResolvedBuild::resolve(
            &self.work.build_targets,
            &target_id,
        ) {
            Ok(r) => r,
            Err(e) => {
                // P8b: the target could not be resolved, but the attempt was
                // on the local lane; name it so the ledger does not say "unknown".
                self.work.panel.requested_identity = Some(WorkRunIdentity::LocalBuild {
                    model: "unresolved".to_string(),
                    host: target_id.clone(),
                });
                self.fail_local_build(&conv_id, e.to_string(), cx);
                return;
            }
        };
        // P8b: from here every preflight exit names the resolved local lane.
        self.work.panel.requested_identity = Some(WorkRunIdentity::LocalBuild {
            model: resolved.target.model.clone(),
            host: resolved.target.ssh_host.clone(),
        });
        let Some(folder) = self.work.folder.clone() else {
            self.fail_local_build(
                &conv_id,
                "local builds need a working folder — choose one first".to_string(),
                cx,
            );
            return;
        };
        // The runner owns the baseline and branch: both come from the folder
        // git state (a detached or non-repo folder is a preflight failure).
        let (baseline, branch) = match crate::build_dispatch::repo_state(&folder) {
            Ok(s) => s,
            Err(e) => {
                self.fail_local_build(&conv_id, e.to_string(), cx);
                return;
            }
        };
        // The durable turn identity is minted on the UI side (like
        // conversation ids) and carried into the build record.
        let turn_id = uuid::Uuid::new_v4().to_string();
        let inputs = crate::build_dispatch::BuildInputs {
            turn_id: turn_id.clone(),
            conversation_id: conv_id.clone(),
            spec_text,
            baseline_sha: baseline,
            branch,
        };
        let request = match crate::build_dispatch::build_request(&resolved, &inputs) {
            Ok(r) => r,
            Err(e) => {
                self.fail_local_build(&conv_id, e.to_string(), cx);
                return;
            }
        };

        self.work.run_seq = self.work.run_seq.wrapping_add(1);
        let run_id = self.work.run_seq;
        self.work.panel = RunPanel::default();
        self.work.error_presentation = None;
        self.work.panel.requested_identity = Some(WorkRunIdentity::LocalBuild {
            model: resolved.target.model.clone(),
            host: resolved.target.ssh_host.clone(),
        });
        self.work.running = true;
        self.work.run_started = Some(std::time::Instant::now());
        self.work.waiting_for_stream_output = true;
        self.ensure_thinking_timer(cx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.work.cancel = cancel.clone();
        // P2f: the handle Cancel run closes (ruling 3B). It exists before the
        // submit task runs, so a Cancel run in that gap is honoured too.
        let key = crate::build_staleness::LocalBuildHandle {
            turn_id,
            request_id: request.request_id.clone(),
            target_id: resolved.target_id.clone(),
            revision: 0,
            stop: cancel,
        };
        self.work.local_build = Some(key.clone());
        cx.notify();
        self.drive_local_build(run_id, resolved, request, key, cx);
    }

    /// The fresh-submit drive of one local build: submit ONCE, apply the
    /// initial snapshot, then poll `status` of the SAME id until terminal,
    /// cancelled or stale (P2f), and finish the run. An initial snapshot that
    /// is already terminal (the submit's same-id status fallback, a runner
    /// spawn failure) never enters the poll loop (P2f round 2).
    pub(crate) fn drive_local_build(
        &mut self,
        run_id: u64,
        resolved: crate::build_dispatch::ResolvedBuild,
        request: awili_dispatch::protocol::BuildRequest,
        mut key: crate::build_staleness::LocalBuildHandle,
        cx: &mut Context<Self>,
    ) {
        let persist = self.persist.clone();
        cx.spawn(async move |this, cx| {
            // P2f: a Cancel run before this task first ran closed the row.
            if key.stopped() {
                return;
            }
            let ack = match crate::build_dispatch::commit_submitting(
                persist.as_ref(),
                &request,
                &resolved.target_id,
            ) {
                Ok(ack) => ack,
                Err(error) => {
                    this.update(cx, |app, cx| {
                        if app.work.run_seq == run_id {
                            app.fail_local_build(
                                &request.conversation_id,
                                format!("Cannot submit build: {error}"),
                                cx,
                            );
                        }
                    })
                    .ok();
                    return;
                }
            };
            let result = ack
                .recv()
                .await
                .unwrap_or_else(|_| Err("Storage worker stopped".into()));
            if let Err(error) = result {
                this.update(cx, |app, cx| {
                    if app.work.run_seq == run_id && !key.stopped() {
                        app.fail_local_build(
                            &request.conversation_id,
                            format!("Cannot submit build: could not save tracking record: {error}"),
                            cx,
                        );
                    }
                })
                .ok();
                return;
            }
            let current = this
                .update(cx, |app, _| {
                    app.work.run_seq == run_id
                        && app
                            .work
                            .local_build
                            .as_ref()
                            .is_some_and(|active| active.request_id == key.request_id)
                })
                .unwrap_or(false);
            if key.stopped() || !current {
                crate::build_dispatch::close_row(
                    persist.as_ref(),
                    &request.turn_id,
                    &request.request_id,
                    &resolved.target_id,
                    0,
                    "Cancelled before submission".into(),
                );
                return;
            }
            // Submit ONCE; a lost acknowledgement reconciles by `status` of
            // the SAME id inside the client — never a fresh submit. The
            // SUBMITTING row has committed before invoking the runner.
            let outcome = resolved.submit(&request);
            // Record the outcome on the SAME row: the returned state on
            // success, or `unknown` + error text when the submit and its
            // same-id fallback both failed. The row is never deleted.
            crate::build_dispatch::record_submit_outcome(
                persist.as_ref(),
                &request,
                &resolved.target_id,
                &outcome,
            );
            let snapshot = match outcome {
                Ok(s) => s,
                Err(e) => {
                    this.update(cx, |app, cx| {
                        if app.work.run_seq == run_id {
                            app.work.local_build = None;
                            app.work.running = false;
                            app.settle_work_chip();
                            app.settle_work_folder(cx);
                            app.work.waiting_for_stream_output = false;
                            app.work.panel.error = Some(e.to_string());
                            app.work_finish_stream(cx);
                        }
                    })
                    .ok();
                    return;
                }
            };
            key.revision = snapshot.revision;
            // The manual override resolves to its adapter and is consumed
            // ONCE after the accepted dispatch: the picker returns to the
            // fleet lane for the next run.
            this.update(cx, |app, _cx| {
                if app.work.run_seq == run_id {
                    app.work.executor = WorkExecutor::InApp;
                    if let Some(p) = &app.persist {
                        p.set_state(
                            crate::persistence::WORK_EXECUTOR_KEY,
                            WorkExecutor::InApp.to_persist(),
                        );
                    }
                }
            })
            .ok();
            // NOW the actual state, never a fabricated answer.
            this.update(cx, |app, cx| {
                app.apply_build_snapshot(run_id, &snapshot, cx);
            })
            .ok();
            // Poll `status` on the SAME id until the job is terminal, the
            // user cancels, or the polls go stale (P2f). P2f round 2: a
            // terminal initial snapshot is already recorded and its handle
            // cleared, so it goes straight to finalization — the stale close
            // can never run for a row that is already closed.
            if !crate::build_dispatch::is_terminal(snapshot.state) {
                let mut staleness = crate::build_staleness::PollStaleness::default();
                loop {
                    cx.background_executor()
                        .timer(crate::build_dispatch::POLL_INTERVAL)
                        .await;
                    if key.stopped() {
                        return;
                    }
                    let snapshot = match resolved.status(&key.request_id) {
                        Ok(s) => {
                            staleness.observe(Ok(()), std::time::Instant::now());
                            s
                        }
                        Err(e) => {
                            let e = e.to_string();
                            match staleness.observe(Err(&e), std::time::Instant::now()) {
                                crate::build_staleness::PollVerdict::Stale(reason) => {
                                    key.close(persist.as_ref(), reason.clone());
                                    this.update(cx, |app, cx| {
                                        app.fail_stale_local_build(run_id, reason, cx);
                                    })
                                    .ok();
                                    break;
                                }
                                crate::build_staleness::PollVerdict::Failing {
                                    streak,
                                    remaining,
                                } => {
                                    // A dropped poll is not a lost job: say so and
                                    // keep polling the same id.
                                    let line =
                                        crate::build_staleness::failing_line(streak, remaining, &e);
                                    this.update(cx, |app, cx| {
                                        if app.work.run_seq == run_id {
                                            app.work.panel.activity = Some(line);
                                            cx.notify();
                                        }
                                    })
                                    .ok();
                                }
                                crate::build_staleness::PollVerdict::Healthy => {}
                            }
                            continue;
                        }
                    };
                    key.revision = snapshot.revision;
                    if let Some(p) = &persist {
                        p.send(crate::PersistCmd::UpsertBuildJob {
                            turn_id: key.turn_id.clone(),
                            request_id: snapshot.request_id.clone(),
                            target_id: resolved.target_id.clone(),
                            revision: snapshot.revision,
                            state: crate::build_dispatch::state_token(snapshot.state).to_string(),
                            result_sha: snapshot.result_sha.clone(),
                            failure: snapshot.failure.clone(),
                            updated_at: crate::feed::unix_now(),
                        });
                    }
                    let terminal = crate::build_dispatch::is_terminal(snapshot.state);
                    this.update(cx, |app, cx| {
                        app.apply_build_snapshot(run_id, &snapshot, cx);
                    })
                    .ok();
                    if terminal {
                        break;
                    }
                }
            }
            this.update(cx, |app, cx| {
                if app.work.run_seq == run_id {
                    app.work.local_build = None;
                    app.work.running = false;
                    app.settle_work_chip();
                    app.settle_work_folder(cx);
                    app.work_finish_stream(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// A local-build preflight or dispatch failure: the panel carries the
    /// real error, the streaming row finishes with it (work_finish_stream
    /// falls back to the panel error for an empty assistant row), and the
    /// manual override is NOT consumed — the selection stands so the user
    /// can fix the target/folder and retry.
    fn fail_local_build(&mut self, conv_id: &str, message: String, cx: &mut Context<Self>) {
        self.work.continuity.leave_agent(conv_id);
        // P8b: keep the requested LocalBuild identity across the panel reset
        // so the ledger names the local lane, not "unknown".
        let identity = self.work.panel.requested_identity.take();
        self.work.panel = RunPanel::default();
        self.work.error_presentation = None;
        self.work.panel.requested_identity = identity;
        self.work.panel.error = Some(message);
        self.work.running = false;
        self.work_finish_stream(cx);
        cx.notify();
    }

    /// Apply one observed build snapshot to the Work panel (fresh submit or
    /// restart reattach): the ACTUAL state and outcome data, never a
    /// synthesized assistant answer. On a terminal observation the run ends:
    /// success is `awaiting_review` ONLY (it never means merged or
    /// deployed); every other terminal state is a failure carrying the
    /// runner's own text.
    fn apply_build_snapshot(
        &mut self,
        run_id: u64,
        snapshot: &awili_dispatch::protocol::BuildSnapshot,
        cx: &mut Context<Self>,
    ) {
        if self.work.run_seq != run_id {
            return;
        }
        let line = crate::build_dispatch::snapshot_line(snapshot);
        // P2f: Cancel run closes the row at the last observed revision.
        if let Some(handle) = self.work.local_build.as_mut() {
            handle.revision = snapshot.revision;
        }
        if crate::build_dispatch::is_terminal(snapshot.state) {
            self.work.local_build = None;
            self.work.running = false;
            self.work.panel.activity = None;
            self.work.waiting_for_stream_output = false;
            match snapshot.state {
                awili_dispatch::protocol::BuildState::AwaitingReview => {
                    self.work.panel.status = Some(RunStatus::Completed);
                    self.work.panel.output = Some(line);
                }
                _ => {
                    self.work.panel.status = Some(RunStatus::Failed);
                    self.work.panel.error = Some(line);
                }
            }
            // P8b: a local Build ends here, not in `work_finish_stream`.
            self.work_record_graduation();
            self.settle_work_chip();
            self.settle_work_folder(cx);
        } else {
            self.work.waiting_for_stream_output = false;
            self.work.panel.activity = Some(line);
        }
        cx.notify();
    }

    /// P2f scope 3b: the panel-driving local build went stale (its row is
    /// already closed by the caller). Only while `run_id` still owns the panel:
    /// the run ends `Failed` with the `stale: …` reason and the armed recorder
    /// writes its one ledger line. After a Cancel run moved `run_seq` on, the
    /// `Stopped` line already exists, so nothing is recorded again.
    fn fail_stale_local_build(&mut self, run_id: u64, reason: String, cx: &mut Context<Self>) {
        if self.work.run_seq != run_id {
            return;
        }
        self.work.local_build = None;
        self.work.running = false;
        self.work.panel.activity = None;
        self.work.waiting_for_stream_output = false;
        self.work.panel.status = Some(RunStatus::Failed);
        self.work.panel.error = Some(reason);
        self.work_record_graduation();
        self.settle_work_chip();
        self.settle_work_folder(cx);
        cx.notify();
    }

    /// P8e/P2f: the ledger line and last run for a local build that does not
    /// drive the panel (a background reattached row, stale or terminal, or an
    /// orphaned row). A failed ledger append is shown on an idle panel, or
    /// logged while a run owns it.
    fn record_detached_build(
        &mut self,
        conv: &str,
        identity: WorkRunIdentity,
        status: RunStatus,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let now = chrono::Utc::now();
        // P2e R4: the background row's last run is persisted under the
        // captured conversation too, and mirrored into the in-memory map (R2).
        crate::work_last_run::persist_last_run_parts(
            self.persist.as_ref(),
            &mut self.work_last_run,
            conv,
            crate::work_last_run::LastRun {
                identity: identity.clone(),
                model: None,
                status,
                error: error.clone(),
                finished_at: now.to_rfc3339(),
            },
        );
        let outcome = self
            .work
            .graduation
            .record_detached(conv, identity, status, error, now);
        if let Some(Err(text)) = outcome {
            if self.work.running {
                eprintln!("awili: {text}");
            } else {
                self.work.panel.activity = Some(text);
                cx.notify();
            }
        }
    }

    /// Daily-driver P2d: on startup, reattach the OPEN build rows by
    /// `status` of each row's SAME request id — the durable job survives
    /// closing Awili, and no fresh submit is ever made. The most recently
    /// updated row drives the panel and the running state; any other open
    /// rows poll silently (persist only) so their history stays fresh
    /// without stealing the panel.
    ///
    /// P10/F4: the app-local `submitting`/`unknown` rows are OPEN, so they
    /// reconcile exactly here — `status` of the same id, never a fresh
    /// submit.
    ///
    /// P2f: a row whose target is no longer configured is closed at once
    /// (`stale: build target … is no longer configured`) with a detached
    /// ledger line, and a row whose polls go stale is closed the same way —
    /// no row is polled forever.
    pub(crate) fn reattach_open_builds(&mut self, cx: &mut Context<Self>) {
        if self.work.open_builds.is_empty() {
            return;
        }
        let mut rows = std::mem::take(&mut self.work.open_builds);
        rows.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.turn_id.cmp(&a.turn_id))
        });
        // P8e (Codex): one conversation for every re-attached row, captured
        // once here, so rows finishing after the user switches conversations
        // are still attributed like the panel-driving row.
        let reattach_conv = self.work_conv_id();
        // P2f round 3 (R2): the driver is the first open row whose target
        // RESOLVES — an orphaned first row is closed above and must not
        // leave a valid later row polling with no panel, Cancel handle or
        // armed recorder.
        let mut driver_chosen = false;
        for row in rows {
            let mut key = crate::build_staleness::LocalBuildHandle {
                turn_id: row.turn_id.clone(),
                request_id: row.request_id.clone(),
                target_id: row.target_id.clone(),
                revision: row.revision,
                stop: Arc::new(AtomicBool::new(false)),
            };
            let Ok(resolved) = crate::build_dispatch::ResolvedBuild::resolve(
                &self.work.build_targets,
                &row.target_id,
            ) else {
                // P2f scope 4: nothing to talk to — close the orphan now.
                let reason = format!(
                    "stale: build target '{}' is no longer configured",
                    row.target_id
                );
                key.close(self.persist.as_ref(), reason.clone());
                let identity = WorkRunIdentity::LocalBuild {
                    model: "unresolved".to_string(),
                    host: row.target_id.clone(),
                };
                self.record_detached_build(
                    &reattach_conv,
                    identity,
                    RunStatus::Failed,
                    Some(reason),
                    cx,
                );
                continue;
            };
            let identity = WorkRunIdentity::LocalBuild {
                model: resolved.target.model.clone(),
                host: resolved.target.ssh_host.clone(),
            };
            let drives_panel = !driver_chosen;
            if drives_panel {
                driver_chosen = true;
                self.work.run_seq = self.work.run_seq.wrapping_add(1);
                self.work.running = true;
                self.work.run_started = Some(std::time::Instant::now());
                self.ensure_thinking_timer(cx);
                // P8b: a build re-attached after a restart still ends in one
                // ledger line — arm the recorder and give the panel the run's
                // local identity. The persisted row carries no conversation id,
                // so the line is attributed to the active Work conversation.
                self.work.panel.requested_identity = Some(identity.clone());
                self.work.graduation.arm(&reattach_conv);
                // P2f: Cancel run closes this row and stops its poll.
                self.work.local_build = Some(key.clone());
            }
            let run_id = self.work.run_seq;
            let row_conv = reattach_conv.clone();
            let persist = self.persist.clone();
            cx.spawn(async move |this, cx| {
                let mut staleness = crate::build_staleness::PollStaleness::default();
                loop {
                    if key.stopped() {
                        return;
                    }
                    let snapshot = match resolved.status(&key.request_id) {
                        Ok(s) => {
                            staleness.observe(Ok(()), std::time::Instant::now());
                            s
                        }
                        Err(e) => {
                            // Unreachable runner right now: try the SAME id
                            // again (never a fresh submit) until it goes stale.
                            let e = e.to_string();
                            match staleness.observe(Err(&e), std::time::Instant::now()) {
                                crate::build_staleness::PollVerdict::Stale(reason) => {
                                    key.close(persist.as_ref(), reason.clone());
                                    this.update(cx, |app, cx| {
                                        if drives_panel {
                                            app.fail_stale_local_build(run_id, reason, cx);
                                            return;
                                        }
                                        if !app.work.running {
                                            app.work.panel.activity = Some(format!(
                                                "background build {} marked stale",
                                                key.request_id
                                            ));
                                        }
                                        app.record_detached_build(
                                            &row_conv,
                                            identity.clone(),
                                            RunStatus::Failed,
                                            Some(reason),
                                            cx,
                                        );
                                        cx.notify();
                                    })
                                    .ok();
                                    break;
                                }
                                crate::build_staleness::PollVerdict::Failing {
                                    streak,
                                    remaining,
                                } if drives_panel => {
                                    let line =
                                        crate::build_staleness::failing_line(streak, remaining, &e);
                                    this.update(cx, |app, cx| {
                                        if app.work.run_seq == run_id {
                                            app.work.panel.activity = Some(line);
                                            cx.notify();
                                        }
                                    })
                                    .ok();
                                }
                                _ => {}
                            }
                            cx.background_executor()
                                .timer(crate::build_dispatch::POLL_INTERVAL)
                                .await;
                            continue;
                        }
                    };
                    key.revision = snapshot.revision;
                    if let Some(p) = &persist {
                        p.send(crate::PersistCmd::UpsertBuildJob {
                            turn_id: key.turn_id.clone(),
                            request_id: key.request_id.clone(),
                            target_id: key.target_id.clone(),
                            revision: snapshot.revision,
                            state: crate::build_dispatch::state_token(snapshot.state).to_string(),
                            result_sha: snapshot.result_sha.clone(),
                            failure: snapshot.failure.clone(),
                            updated_at: crate::feed::unix_now(),
                        });
                    }
                    let terminal = crate::build_dispatch::is_terminal(snapshot.state);
                    if drives_panel {
                        this.update(cx, |app, cx| {
                            app.apply_build_snapshot(run_id, &snapshot, cx);
                        })
                        .ok();
                    } else if terminal {
                        // P8e: every re-attached local build gets a graduation
                        // line, not only the one driving the panel, attributed
                        // to the captured Work conversation.
                        let (status, error) = match snapshot.state {
                            awili_dispatch::protocol::BuildState::AwaitingReview => {
                                (RunStatus::Completed, None)
                            }
                            _ => (
                                RunStatus::Failed,
                                Some(crate::build_dispatch::snapshot_line(&snapshot)),
                            ),
                        };
                        this.update(cx, |app, cx| {
                            app.record_detached_build(
                                &row_conv,
                                identity.clone(),
                                status,
                                error,
                                cx,
                            );
                        })
                        .ok();
                    }
                    if terminal {
                        break;
                    }
                    cx.background_executor()
                        .timer(crate::build_dispatch::POLL_INTERVAL)
                        .await;
                }
                if drives_panel {
                    this.update(cx, |app, _cx| {
                        if app.work.run_seq == run_id {
                            app.work.local_build = None;
                            app.work.running = false;
                        }
                    })
                    .ok();
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// P3c round 3 (R5): submit the prompt parked behind a gateway Stop, if
    /// any, through the normal submit path — once (the slot is taken).
    ///
    /// P3g: a parked prompt whose conversation was deleted meanwhile is
    /// dropped (slot discarded, one log line), never run invisibly.
    fn replay_queued_after_stop(&mut self, cx: &mut Context<Self>) {
        if let Some(queued) = self.work.queued_after_stop.take() {
            if !self.work_conversation_is_live(&queued.conv_id) {
                eprintln!("awili: parked prompt dropped: conversation deleted");
                return;
            }
            // Keep an owned id after moving the prompt text into the normal
            // submit path. The saved target below is the chooser's NEXT
            // value, not the already-accepted target on this parked prompt.
            let queued_conv_id = queued.conv_id.clone();
            // RC1-O-FIX4: dispatch with the helper captured at acceptance,
            // while retaining the user's current next-target selection for
            // every later turn. An InApp health probe sees a restored changed
            // target and uses its established draft hand-back instead of
            // silently rerouting to that new executor.
            let route_seed = self.work.executor.clone();
            let saved_target = self
                .work
                .conversation_routes
                .entry(queued_conv_id.clone())
                .or_insert_with(|| crate::conversation_route::ConversationRoute::new(route_seed))
                .next_target()
                .clone();
            self.work
                .conversation_routes
                .get_mut(&queued_conv_id)
                .expect("route was just inserted")
                .set_next_target(queued.executor.clone());
            let previous_executor = self.work.executor.clone();
            self.work.executor = queued.executor.clone();
            // L1-INT1: an in-app prompt (sent after a switch to In-app while
            // the gateway Stop was pending) passes the local model's gate
            // first, in the conversation it was typed in.
            if crate::local_model_health::gate_applies(
                self.work.running,
                self.work.gateway_stop_pending,
                &self.work.executor,
            ) {
                self.local_gate_in(queued.text, queued_conv_id.clone(), cx);
                if let Some(route) = self.work.conversation_routes.get_mut(&queued_conv_id) {
                    route.set_next_target(saved_target);
                }
                // A direct healthy local launch owns the visible active
                // executor. A probe has not launched anything, so restore
                // the ambient next/default posture while it waits; its
                // established changed-target path hands the draft back.
                let replay_is_active = self.work.running
                    || self.work.active_work_route_turn.is_some()
                    || self
                        .work
                        .active_code_route_turns
                        .values()
                        .any(|turn| turn.conversation_id == queued_conv_id);
                if !replay_is_active {
                    self.work.executor = previous_executor;
                }
                return;
            }
            self.submit_work_run_in(queued.text, queued_conv_id.clone(), cx);
            if let Some(route) = self.work.conversation_routes.get_mut(&queued_conv_id) {
                route.set_next_target(saved_target);
            }
            // Do not put the NEXT target into the active display/approval
            // identity. `submit_work_run_kind` captured the parked target;
            // once it settles, the canonical route remains the next choice.
            let replay_is_active = self.work.running
                || self.work.active_work_route_turn.is_some()
                || self
                    .work
                    .active_code_route_turns
                    .values()
                    .any(|turn| turn.conversation_id == queued_conv_id);
            if !replay_is_active {
                self.work.executor = previous_executor;
            }
        }
    }

    /// Whether `conv_id` still has a live conversation view (`delete_conversation`
    /// removes it) — the P3g guard for a prompt parked behind a Stop.
    fn work_conversation_is_live(&self, conv_id: &str) -> bool {
        self.views.contains_key(conv_id)
    }

    pub fn cancel_work_run(&mut self, cx: &mut Context<Self>) {
        self.work.panel.credential = None;
        self.credential_input.update(cx, |c, _| {
            c.take();
        });
        // RC1-O-FIX8: this run's footer claims only what was seen before
        // the Stop.
        self.work.owner_stopped = true;
        // N1: the owner's Stop marks its Stopped/Failed as silent. A local
        // build's Stop closes its row without finishing the run (no Stopped
        // follows to consume the mark), so it leaves none: a mark there would
        // silence the next run's failure on this conversation.
        if self.work.local_build.is_none() {
            self.notify_work_user_stop();
        }
        // U4e1 (D10): a Stop clears this conversation's plan card.
        self.work
            .plan_gates
            .clear(&AskKey::Work(self.work_conv_id()));
        // P3c round 3 (R6): a second Stop while the first is still being
        // acknowledged is a no-op — the worker, its sender, run_seq and the
        // cancel flag stay as the first Stop left them, so the pending
        // acknowledgement still lands and the session survives.
        if self.work.gateway_stop_pending {
            cx.notify();
            return;
        }
        self.workspace_read_cancel(&self.work_conv_id(), self.work.run_seq);
        // Freeze the exact accepted native owner before invalidating the run
        // sequence. A later provider callback cannot advance a different
        // harness/cwd entry, and the next target remains untouched.
        let gate_pending = self.work.agent_gate_pending || self.work.resume_gate_pending;
        if let Some(active) = self.work.active_work_route_turn.take() {
            let through = self
                .views
                .get(&active.conversation_id)
                .map(bridge_records_from_view)
                .map(|records| awili_code::continuity::canonical_cursor(&records))
                .unwrap_or_default();
            if let Some(route) = self
                .work
                .conversation_routes
                .get_mut(&active.conversation_id)
            {
                if gate_pending {
                    route.finish_unstarted_turn(&active);
                } else {
                    route.stop_turn(&active, through);
                }
            }
            self.persist_conversation_route(&active.conversation_id);
        }
        // RC1-N: a Stop after an in-app Deny ends the turn as a Stop.
        self.work.deny_stopped = None;
        // P3c round 2 (R1): a turn in flight on a live Hermes-gateway session
        // is stopped by the worker's `Cancel` arm (session.interrupt + drain),
        // so the cancel flag is NOT tripped (the worker would close the
        // session before it ever reads the command).
        let gateway_turn = gateway_turn_in_flight(&self.work);
        if !gateway_turn {
            self.work.cancel.store(true, Ordering::SeqCst);
        }
        // P3d: the card is taken, not just cleared — a gateway Stop answers
        // it with `deny` before its `Cancel` (see `gateway_stop`).
        let pending_approval = self.work.panel.approval.take();
        // P2f (Fable ruling 3B): Cancel run on a local build stops its poll
        // task and closes its row, so no later poll, stale verdict or relaunch
        // reattach can write a second ledger line. The runner job is left as is.
        if let Some(handle) = self.work.local_build.take() {
            handle.stop.store(true, Ordering::SeqCst);
            handle.close(
                self.persist.as_ref(),
                crate::build_staleness::CANCEL_REASON.to_string(),
            );
        }
        // P8b: a Stop is the turn's terminal for every executor (agent,
        // gateway, local build). Record it BEFORE the run_seq bump below makes
        // the executor's own terminal event stale, and while the panel still
        // describes the stopped turn. On the gateway lane this disarms the
        // recorder, so the worker's later `Terminal { Stopped }` (same
        // run_seq) finds nothing armed and writes no second line.
        if self.work.running && self.work.graduation.is_armed() {
            self.work.panel.status = Some(RunStatus::Stopped);
            self.work_record_graduation();
        }
        if gateway_turn {
            // R1: keep run_seq, the sender and session_id — the worker keeps
            // the session for the next prompt, and its `Terminal { Stopped }`
            // arrives under this run_seq and finishes the stopped turn.
            if gateway_stop(&mut self.work, pending_approval.as_ref()) {
                // M1a: arm the bounded barrier — an unacknowledged Stop
                // cannot park Work sends forever.
                self.arm_gateway_stop_deadline(cx);
            }
            self.work.running = false;
            cx.notify();
            return;
        }
        self.work.run_seq = self.work.run_seq.wrapping_add(1);
        // RC2-APP (F0317): the stopped turn finishes under this new number;
        // a stopped workspace diagnostic takes its answer from it alone.
        self.workspace_read_stop_epoch(&self.work_conv_id(), self.work.run_seq);
        // Agent-harness run: the kill switch takes the child process group
        // down. The killed task's own events are already stale (run_seq bump
        // above), so close the run HERE as a deliberate Stop — never as a
        // failure (GLM night-review SHOULD-2).
        if let Some(tx) = self.work.agent_abort_tx.take() {
            self.work.continuity.leave_agent(&self.work_conv_id());
            let _ = tx.send(());
            self.work.panel.activity = None;
            self.work.panel.status = Some(RunStatus::Stopped);
            self.work_finish_stream(cx);
        } else if std::mem::take(&mut self.work.resume_gate_pending) {
            self.work.panel.activity = None;
            self.work.panel.status = Some(RunStatus::Stopped);
            self.work_finish_stream(cx);
        } else if std::mem::take(&mut self.work.agent_gate_pending) {
            // S5 INT1-FIX1: Stop while the agent turn still waits on its
            // memory gate — no child exists yet. Close the turn HERE as a
            // deliberate Stop; the late gate answer is stale (run_seq bump
            // above) and starts nothing.
            self.restore_agent_gate_attachments(cx);
            self.work.continuity.leave_agent(&self.work_conv_id());
            self.work.panel.activity = None;
            self.work.panel.status = Some(RunStatus::Stopped);
            self.work_finish_stream(cx);
        }
        // The cancel flag stops the worker's event loop; the gateway worker also
        // gets an explicit Cancel command. Unit 16 removed the HermesClient
        // `stop_run` fallback — there is no non-gateway run to stop.
        // RC1-N-FIX2: a dispatch still waiting on an earlier stopped worker
        // is dropped by this Stop (run_seq moves on); its wait ends.
        if let Some((_, release)) = self.work.stopped_barrier.take() {
            release.store(true, Ordering::SeqCst);
        }
        // RC1-N-FIX2: the stopped in-app worker still publishes its turn's
        // history (completed work kept); the next dispatch keeps it.
        // Only a run of THIS conversation's live in-app worker arms it (an
        // Agent or local Build Stop never does — pre-review pass 3).
        let in_app_run = matches!(
            self.work.executor,
            WorkExecutor::InApp | WorkExecutor::InAppExtra { .. } | WorkExecutor::Gateway
        ) && self.work_engine_kind() == awili_config::EngineKind::Awili
            && self.work.gateway_cmd_tx.is_some();
        if let Some((conv, slot)) = self.work.inapp_history_slot.clone() {
            if self.work.running && in_app_run && conv == self.work_conv_id() {
                let len = self
                    .views
                    .get(&conv)
                    .map(|v| v.convo.messages().len())
                    .unwrap_or(0);
                self.work.stopped_histories.insert(conv, (slot, len));
            }
        }
        if let Some(cmd_tx) = self.work.gateway_cmd_tx.take() {
            let _ = cmd_tx.send(GatewayCmd::Cancel);
            self.work.session_id = None;
        }
        // P3c round 3 (R6): the worker (if any) is discarded and run_seq moved
        // on, so no acknowledgement can reach a pending Stop or parked prompt.
        self.work.gateway_stop_pending = false;
        self.work.queued_after_stop = None;
        self.work.running = false;
        // RC1-O-FIX7 (whole-slice review P1): the stopped run no longer holds
        // the chip — the conversation on screen gets its own lane.
        self.settle_work_chip();
        self.settle_work_folder(cx);
        cx.notify();
    }

    /// Save an explicit folder as a named project.
    pub(crate) fn save_project_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        let folder_str = folder.display().to_string();
        if self.work.projects.iter().any(|p| p.folder == folder_str) {
            return;
        }
        let name = folder
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("untitled")
            .to_string();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        match &self.persist {
            // Authoritative path: write the row, then replace the in-memory
            // mirror from the reply so `work.projects` carries the DB-assigned
            // ids (mirrors `save_template`). The project appears once the worker
            // confirms it persisted — so a relaunch and the live list never
            // diverge.
            Some(p) => {
                let (tx, rx) = async_channel::bounded(1);
                p.send(crate::PersistCmd::SaveProject {
                    name,
                    folder: folder_str,
                    instructions: String::new(),
                    ts,
                    reply: tx,
                });
                cx.spawn(async move |this, cx| {
                    if let Ok(reply) = rx.recv().await {
                        let _ = this.update(cx, |app, cx| {
                            match reply {
                                Ok(snapshot) => app.apply_project_snapshot(snapshot),
                                Err(e) => {
                                    // Keep the existing list, surface the error —
                                    // never swallow it into a silently-empty list.
                                    eprintln!("awili: project list refresh failed: {e}");
                                    app.show_toast(format!("Couldn't save project: {e}"), cx);
                                }
                            }
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
            // No DB this session: keep the optimistic in-memory entry (id 0) so
            // the feature still works, even though it won't survive a relaunch.
            None => {
                let mut projects = self.work.projects.clone();
                projects.insert(
                    0,
                    StoredProject {
                        id: 0,
                        name,
                        folder: folder_str,
                        instructions: String::new(),
                        created_ts: ts,
                    },
                );
                self.replace_workshop_projects(projects);
                cx.notify();
            }
        }
    }

    /// Delete a saved project by its row id (the inverse of
    /// `save_project_folder`). Mirrors that method's authoritative
    /// path: write through the worker, then replace the in-memory mirror from
    /// the reply so `work.projects` stays in lockstep with the rows. Deleting a
    /// project never touches the current working folder — only the saved list.
    pub fn remove_project_by_id(
        &mut self,
        project_id: i64,
        folder: String,
        cx: &mut Context<Self>,
    ) {
        match &self.persist {
            Some(p) => {
                let (tx, rx) = async_channel::bounded(1);
                p.send(crate::PersistCmd::RemoveProject {
                    project_id,
                    reply: tx,
                });
                cx.spawn(async move |this, cx| {
                    if let Ok(reply) = rx.recv().await {
                        let _ = this.update(cx, |app, cx| {
                            match reply {
                                Ok(snapshot) => app.apply_project_snapshot(snapshot),
                                Err(e) => {
                                    // Keep the existing list, surface the error —
                                    // never swallow it into a silently-empty list.
                                    eprintln!("awili: project delete failed: {e}");
                                    app.show_toast(format!("Couldn't delete project: {e}"), cx);
                                }
                            }
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
            // No DB this session: drop the optimistic in-memory entry directly so
            // the delete still takes effect for the running session. Match on the
            // folder, not the id — no-DB entries all carry id 0, so an id match
            // would remove every saved project at once. Folder is unique in the
            // mirror (`save_project_folder` dedups by folder).
            None => {
                let mut projects = self.work.projects.clone();
                projects.retain(|proj| proj.folder != folder);
                self.replace_workshop_projects(projects);
                cx.notify();
            }
        }
    }

    /// Fetch scheduled jobs from the gateway REST surface and refresh the sidebar.
    pub fn refresh_jobs(&mut self, cx: &mut Context<Self>) {
        let cfg = self.work.hermes.clone();
        if !cfg.enabled {
            return;
        }
        let (tx, rx) = async_channel::bounded::<Vec<ScheduledJob>>(1);
        std::thread::spawn(move || {
            // G1: `/api` lives on the dashboard; hermes origin is the fallback.
            let client = dashboard_rest_client(&cfg.dashboard_url).unwrap_or_else(|_| {
                RestClient::from_base_url(cfg.base_url.clone(), cfg.api_key.clone())
            });
            match client.list_jobs() {
                Ok(jobs) => {
                    let _ = tx.send_blocking(jobs);
                }
                Err(e) => {
                    eprintln!("awili: list_jobs failed: {e}");
                }
            }
        });
        cx.spawn(async move |this, cx| {
            if let Ok(jobs) = rx.recv().await {
                let _ = this.update(cx, |app, cx| {
                    app.work.jobs = jobs;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Fetch recent Hermes sessions from the REST API.
    pub fn refresh_sessions(&mut self, cx: &mut Context<Self>) {
        let cfg = self.work.hermes.clone();
        if !cfg.enabled {
            return;
        }
        let (tx, rx) = async_channel::bounded::<Vec<HermesSession>>(1);
        std::thread::spawn(move || {
            // G1: `/api` lives on the dashboard; hermes origin is the fallback.
            let client = dashboard_rest_client(&cfg.dashboard_url).unwrap_or_else(|_| {
                RestClient::from_base_url(cfg.base_url.clone(), cfg.api_key.clone())
            });
            match client.list_sessions() {
                Ok(sessions) => {
                    let _ = tx.send_blocking(sessions);
                }
                Err(e) => {
                    eprintln!("awili: list_sessions failed: {e}");
                }
            }
        });
        cx.spawn(async move |this, cx| {
            if let Ok(sessions) = rx.recv().await {
                let _ = this.update(cx, |app, cx| {
                    app.work.hermes_sessions = sessions;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// The conversation Work output is written to: the run's pinned thread
    /// (captured at submit), falling back to the active thread before any run.
    pub(crate) fn work_conv_id(&self) -> String {
        self.work
            .conv_id
            .clone()
            .unwrap_or_else(|| self.sidebar.active_id().to_string())
    }

    /// Push a message into the active Work conversation's transcript.
    pub(crate) fn work_push(&mut self, role: Role, text: String) {
        self.work_push_kind(role, text, awili_core::MessageKind::Normal);
    }

    /// [`Self::work_push`] of a message of `kind` (RC2-RUNF: a plan-card
    /// click is kept, shown and stored with its mark).
    pub(crate) fn work_push_kind(
        &mut self,
        role: Role,
        text: String,
        kind: awili_core::MessageKind,
    ) {
        use crate::MessageRow;
        let conv_id = self.work_conv_id();
        if let Some(view) = self.views.get_mut(&conv_id) {
            let idx = view.rows.len();
            let ts = view.convo.push_kind(role, text.clone(), kind);
            if let Some(msg) = view.convo.message(idx) {
                view.rows.push(MessageRow::new(msg));
            }
            crate::transcript_keys::pin_to_bottom(&view.scroll);
            if !text.is_empty() {
                self.persist_message_kind(&conv_id, crate::role_str(role), &text, ts, kind);
            }
        }
    }

    /// RC2-GEMMA5 (R7-A3): the model server cut the reply off and the engine
    /// asks again: the text it had shown (`shown`, the end of the reply so
    /// far) is taken back, and the progress line says why.
    pub(crate) fn work_take_back_reply(&mut self, shown: &str, cx: &mut Context<Self>) {
        let conv_id = self.work_conv_id();
        if let (Some(idx), Some(view)) = (self.work.stream_row_idx, self.views.get_mut(&conv_id)) {
            let kept = view
                .convo
                .message(idx)
                .and_then(|m| taken_back(&m.text, shown));
            if let Some(kept) = kept {
                view.convo.set_text(idx, &kept);
                if let Some(msg) = view.convo.message(idx) {
                    // RC2-FIXA1: the text got shorter; redraw now.
                    view.rows[idx].stream_taken_back(msg);
                }
            }
        }
        if let Some(kept) = taken_back(&self.work.panel.streamed, shown) {
            self.work.panel.streamed = kept;
        }
        self.work.panel.activity = Some(REPLY_RESTART_LINE.into());
        cx.notify();
    }

    /// RC2-UX (D-PUNCH S2-F15): `text` with a paragraph break in front when
    /// a tool step or a card came since the reply's last text and neither
    /// side already has the space (the Hermes gateway and the in-app engine
    /// send the parts with nothing between them).
    pub(crate) fn with_text_break(&mut self, text: String) -> String {
        if !std::mem::take(&mut self.work.text_break_due) {
            return text;
        }
        let conv_id = self.work_conv_id();
        let before = self
            .work
            .stream_row_idx
            .and_then(|idx| {
                self.views
                    .get(&conv_id)?
                    .convo
                    .message(idx)
                    .map(|m| m.text.clone())
            })
            .unwrap_or_default();
        if before.is_empty()
            || before.ends_with(char::is_whitespace)
            || text.starts_with(char::is_whitespace)
        {
            text
        } else {
            format!("\n\n{text}")
        }
    }

    pub(crate) fn work_stream_token(&mut self, chunk: &str, cx: &mut Context<Self>) {
        let conv_id = self.work_conv_id();
        let Some(idx) = self.work.stream_row_idx else {
            return;
        };
        let Some(view) = self.views.get_mut(&conv_id) else {
            return;
        };
        if view.convo.append_text(idx, chunk) {
            if let Some(msg) = view.convo.message(idx) {
                view.rows[idx].stream_update_plan_safe(msg);
            }
            if crate::follow_stream(&view.scroll) {
                view.scroll.scroll_to_bottom();
            }
            cx.notify();
        }
    }

    /// P3c round 6 (R13): a `Failed` that arrives while a gateway Stop is
    /// pending is recorded in the stopped turn's streaming row — the same
    /// transcript path the drain's `HookDenial` evidence takes — so it
    /// survives the acknowledgement: the `Terminal { Stopped }` reducer
    /// overwrites `panel.error` and `panel.activity` may carry the parked
    /// prompt's "stopping" line. With no streaming row the text is logged.
    fn work_record_failure_during_stop(&mut self, msg: &str, cx: &mut Context<Self>) {
        eprintln!("awili: gateway turn failed while a Stop was pending: {msg}");
        if self.work.stream_row_idx.is_some() {
            self.work.waiting_for_stream_output = false;
            self.work_stream_token(&failure_during_stop_line(msg), cx);
        }
    }

    /// RC1-N: the in-app turn the owner denied has ended. Only a Stopped end
    /// confirms the Deny (the engine sends it after the denial is durably
    /// recorded): then the reply becomes the one plain line chosen at Deny
    /// time (the "Approval needed" notice and anything the model said
    /// before the card are replaced) and the request goes back in the box.
    /// Any other end is shown as itself.
    fn work_settle_deny_stop(&mut self, cx: &mut Context<Self>) {
        let Some(stop) = self.work.deny_stopped.take() else {
            return;
        };
        // RC2-RUN (R3c-7): the Deny's "still working" pulse ends with the turn.
        self.work.waiting_for_stream_output = false;
        if self.work.panel.status != Some(RunStatus::Stopped) {
            return;
        }
        self.work.panel.activity = None;
        let conv_id = self.work_conv_id();
        if let (Some(idx), Some(view)) = (self.work.stream_row_idx, self.views.get_mut(&conv_id)) {
            view.convo.set_text(idx, &stop.line);
            if let Some(msg) = view.convo.message(idx) {
                view.rows[idx].stream_update(msg);
            }
        }
        self.restore_request_with(stop.request, stop.toast, cx);
    }

    /// RC2-R56: take `raw` (a CLI's own failure line, streamed as reply text)
    /// out of the streaming Work row. Returns whether the reply is empty now
    /// (nothing else was said), so one plain line can stand alone.
    fn work_reply_drop(&mut self, raw: &str, cx: &mut Context<Self>) -> bool {
        let conv_id = self.work_conv_id();
        let raw = raw.trim();
        let (Some(idx), Some(view)) = (self.work.stream_row_idx, self.views.get_mut(&conv_id))
        else {
            return false;
        };
        let Some(text) = view.convo.message(idx).map(|m| m.text.clone()) else {
            return false;
        };
        let kept = if raw.is_empty() {
            text.clone()
        } else {
            text.replace(raw, "")
        };
        let kept = kept
            .trim()
            .trim_start_matches('\u{26A0}')
            .trim()
            .to_string();
        if kept.is_empty() {
            view.convo.set_text(idx, "");
            if let Some(msg) = view.convo.message(idx) {
                view.rows[idx].stream_update(msg);
            }
            cx.notify();
            return true;
        }
        false
    }

    /// Append one activity step to the streaming Work row's fold.
    pub(crate) fn work_on_activity(&mut self, line: String, cx: &mut Context<Self>) {
        let conv_id = self.work_conv_id();
        let Some(idx) = self.work.stream_row_idx else {
            return;
        };
        // M2b: mark activity that reaches outside the project folder.
        let folder = self
            .work
            .folder
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let line =
            crate::agent_brief::mark_outside_scope(line, &folder, &crate::home::home_string());
        if let Some(view) = self.views.get_mut(&conv_id) {
            if let Some(row) = view.rows.get_mut(idx) {
                row.push_activity(line);
                cx.notify();
            }
        }
    }

    /// P8b: append this run to the graduation ledger, exactly once per
    /// `run_seq`, on every terminal path. A write failure shows in the panel
    /// activity line and is never a failed run.
    pub(crate) fn work_record_graduation(&mut self) {
        self.capture_work_error(None);
        // Daily-driver P2e (R1): capture the recorder's arming BEFORE
        // `observe` consumes it — the last run is persisted only on the
        // recorder's own terminal transition (once per turn; a second call on
        // the same terminal sees `was_armed == false`) and under the
        // conversation the turn was submitted under, never `work_conv_id()`
        // (which `submit_work_run` has already re-pinned to the new turn).
        let was_armed = self.work.graduation.is_armed();
        let turn_conv = self.work.graduation.armed_conv().to_string();
        let panel = std::mem::take(&mut self.work.panel);
        let now = chrono::Utc::now();
        // RC1-H1-FIX1: the footer's facts come before (and without) the
        // ledger, so an in-app reply always says what changed.
        let preview = self.work.graduation.preview(&panel, now);
        let outcome = self.work.graduation.observe(&panel, now);
        self.work.panel = panel;
        match (&outcome, preview) {
            (Some(Ok(entry)), _) => self.attach_work_footer(&turn_conv, entry),
            (_, Some(entry)) if entry.executor == "owned" => {
                self.attach_work_footer(&turn_conv, &entry)
            }
            _ => {}
        }
        if let Some(Err(text)) = outcome {
            eprintln!("awili: {text}");
            self.work.panel.activity = Some(text);
        }
        // Fire-and-forget through the existing persistence worker (the same
        // `SetState` path as `work_executor`) — never blocks the UI on sqlite,
        // and a write failure is logged there and ignored (the run finished).
        // The in-memory map is updated with the same JSON (R2) so a later
        // activation never restores a stale row.
        if was_armed && crate::graduation_record::terminal_status(&self.work.panel).is_some() {
            crate::work_last_run::persist_last_run(
                self.persist.as_ref(),
                &mut self.work_last_run,
                &turn_conv,
                &self.work.panel,
                chrono::Utc::now(),
            );
        }
    }

    pub(crate) fn work_finish_stream(&mut self, cx: &mut Context<Self>) {
        // Owned-engine failures arrive via Terminal, gateway failures via
        // Failed. Normalize the explicit capability rejection at their shared
        // completion boundary before persisting the answer and its ledger.
        if let Some(message) = self
            .work
            .panel
            .error
            .as_deref()
            .and_then(image_endpoint_error)
        {
            self.work.panel.error = Some(message.to_string());
        }
        // RC1-I2-FIX2: the user left this in-app run's conversation while it
        // was going; its session ends now that the run has.
        if std::mem::take(&mut self.work.retire_after_run) {
            retire_in_app_worker(&mut self.work);
        }
        // R1h: every lane's run end (the approved run included) re-reads the
        // folder, so the Files card matches the disk.
        self.refresh_folder_listing();
        let strip_conv = self.work_conv_id();
        self.strip_on_end(&strip_conv);
        let conv_id = self.work_conv_id();
        self.work_record_graduation();
        // If nothing streamed into the center transcript live, the answer may
        // have arrived only via the terminal poll — landing in the RunPanel
        // rather than the live token stream — or the run may have failed. Fall
        // back to the panel result, then its error, so Work's chat-in-the-middle
        // is the single source of truth, never a "(completed without output)"
        // placeholder while the real text hides elsewhere.
        let fallback = self
            .work
            .panel
            .result_text()
            .map(str::to_string)
            .or_else(|| self.work.panel.error.clone().map(|e| format!("⚠ {e}")));
        // When a turn ends with no text and no error, say *why* (the finish
        // status) instead of a bare placeholder (P0.2). A clean finish with no
        // text usually means a tool-only turn.
        let empty_note = match self.work.panel.status {
            // RC2-R61: quitting Awili stopped it.
            _ if self.work.quit_stop => crate::app_lifecycle::QUIT_STOPPED_EMPTY.to_string(),
            Some(RunStatus::Failed) => crate::conv_view::FAILED_EMPTY.to_string(),
            Some(RunStatus::Stopped) => crate::conv_view::STOPPED_EMPTY.to_string(),
            _ => crate::conv_view::NO_TEXT.to_string(),
        };
        let quit_stop = self.work.quit_stop;
        // R1b: neither the plan-ready marker nor, after a Stop or a failure,
        // a start of it cut off at the end is ever shown or saved.
        let interrupted = matches!(
            self.work.panel.status,
            Some(RunStatus::Stopped | RunStatus::Failed)
        );
        let mut persist_data: Option<(String, u64, usize)> = None;
        if let Some(idx) = self.work.stream_row_idx.take() {
            if self.work.panel.decided.is_some() {
                self.work.panel.decided_reply = Some((conv_id.clone(), idx));
            }
            if let Some(view) = self.views.get_mut(&conv_id) {
                let raw = view.convo.message(idx).map(|m| m.text.clone());
                let raw = raw.unwrap_or_default();
                if let std::borrow::Cow::Owned(shown) =
                    awili_code::hide_plan_marker(&raw, interrupted)
                {
                    view.convo.set_text(idx, &shown);
                }
                // RC2-R61: what it wrote stays, and it says the quit stopped it.
                let written = view
                    .convo
                    .message(idx)
                    .map(|m| m.text.trim_end().to_string())
                    .filter(|t| !t.is_empty());
                if let (true, Some(written)) = (quit_stop, written) {
                    view.convo.set_text(
                        idx,
                        format!("{written}\n\n{}", crate::app_lifecycle::QUIT_STOPPED_LINE),
                    );
                }
                if let Some(msg) = view.convo.message(idx) {
                    if msg.text.is_empty() {
                        let text = fallback
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or(empty_note);
                        view.convo.set_text(idx, &text);
                        if let Some(msg) = view.convo.message(idx) {
                            view.rows[idx].finish_stream(msg);
                        }
                    } else {
                        view.rows[idx].finish_stream(msg);
                    }
                }
                if let Some(msg) = view.convo.message(idx) {
                    persist_data = Some((msg.text.clone(), msg.ts, idx));
                }
            }
        }
        if let Some((text, ts, idx)) = persist_data {
            // Taking `stream_row_idx` above is the once-only terminal latch:
            // repeated Failed/precondition events cannot re-notify, re-speak
            // or re-run the shared completion hooks for this turn.
            self.notify_work_end();
            self.capture_work_error(Some(idx));
            self.persist_message(&conv_id, crate::role_str(Role::Assistant), &text, ts);
            // RC2-RUN (UX-FOOTER): the reply's footer is saved with it.
            self.save_work_footer(&conv_id, idx, ts, &text);
            // rc.3 S2: and what went into its prompt.
            self.attach_work_context(&conv_id, idx, ts, &text);
            // The shared completion hooks: what actually ran, reported spend,
            // memory, read-aloud (the speech watcher re-arms the hands-free
            // mic afterwards).
            let who = crate::graduation_record::classify(
                self.work.panel.requested_identity.as_ref(),
                self.work.panel.model.as_deref(),
            );
            let in_app = who.executor == "owned";
            let class = if in_app
                && who.class == crate::graduation_record::Class::Unknown
                && crate::engine_locality::engine_is_local(&self.work.effective_engine().base_url)
            {
                crate::graduation_record::Class::Local
            } else {
                who.class
            };
            let user_text = self
                .views
                .get(&conv_id)
                .and_then(|view| {
                    view.convo.messages()[..idx.min(view.convo.len())]
                        .iter()
                        .rev()
                        .find(|m| m.role == Role::User)
                        .map(|m| m.text.clone())
                })
                .unwrap_or_default();
            let ok = !interrupted && !quit_stop && self.work.panel.error.is_none();
            let turn = crate::workshop_turn::FinishedTurn {
                visible: self.mode == Mode::Work && self.sidebar.active_id() == conv_id,
                reply_idx: idx,
                code_tab: None,
                user_text,
                reply_text: text.clone(),
                model: if in_app {
                    self.work.effective_engine().display_label()
                } else {
                    who.model
                },
                executor: who.executor,
                class,
                usage: self.work.panel.usage,
                ok,
                conv_id: conv_id.clone(),
                run_id: self
                    .work
                    .error_presentation
                    .as_ref()
                    .map(|e| e.run_id.clone())
                    .unwrap_or_else(|| self.work.graduation.run_id()),
            };
            self.finish_workshop_turn(turn, cx);
        } else if self.voice.conversation_mode {
            self.start_voice_listening(cx);
        }
        self.workspace_read_terminal(&conv_id, cx);
        // RC1-J / RC1-J1-FIX2: keep the model history the in-app worker
        // published for this turn — LAST, once the reply's text is final, so
        // the kept fingerprint matches the transcript as it stays.
        self.keep_inapp_history();
        cx.notify();
    }

    // ---- rendering -----------------------------------------------------------

    /// A notice for a job in another conversation remains near the composer.
    pub(crate) fn workshop_work_cards(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        if self.mode != Mode::Work {
            return Vec::new();
        }
        self.work_other_conversation_card(cx)
            .into_iter()
            .map(IntoElement::into_any_element)
            .collect()
    }

    /// The latest owning reply hosts the live gate and its decided record.
    pub(crate) fn workshop_blocking_row(&self) -> Option<usize> {
        if self.mode != Mode::Work || !self.work.panel_is_visible(self.sidebar.active_id()) {
            return None;
        }
        self.work
            .stream_row_idx
            .or_else(|| {
                self.work
                    .panel
                    .decided_reply
                    .as_ref()
                    .filter(|(owner, _)| {
                        self.work.panel.decided.is_some() && owner == self.sidebar.active_id()
                    })
                    .map(|(_, idx)| *idx)
            })
            .filter(|idx| {
                self.active_view().is_some_and(|view| {
                    view.rows
                        .get(*idx)
                        .is_some_and(|row| row.role == awili_core::Role::Assistant)
                })
            })
    }

    pub(crate) fn workshop_blocking_cards(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        if self.mode != Mode::Work || !self.work.panel_is_visible(self.sidebar.active_id()) {
            return Vec::new();
        }
        let mut cards = Vec::new();
        if let Some(card) = self.work_approval_card(cx) {
            cards.push(card);
        } else if self.work.running
            || self.workshop_blocking_row().is_some_and(|idx| {
                self.active_view().is_some_and(|view| {
                    view.rows.get(idx).is_some_and(|row| {
                        row.activity_expanded
                            || (row.activity.is_empty() && row.presentation_activity.is_empty())
                    })
                })
            })
        {
            // Keep the acknowledged gate stable during a run. Once settled,
            // its inert record belongs to the expanded Activity disclosure.
            if let Some(card) = self.work_decided_card() {
                cards.push(card);
            }
        }
        if let Some(card) = self.work_credential_card(cx) {
            cards.push(card.into_any_element());
        }
        cards
    }

    /// Reveal only the requested owning control after layout, including when
    /// zoom or wrapped actions make bottom alignment clip its first row.
    pub(crate) fn reveal_work_blocking_control(
        &self,
        request: String,
    ) -> impl Fn(Vec<gpui::Bounds<gpui::Pixels>>, &mut gpui::Window, &mut gpui::App) + 'static {
        self.reveal_work_control(request, false)
    }

    pub(crate) fn reveal_work_error_head(
        &self,
        request: String,
    ) -> impl Fn(Vec<gpui::Bounds<gpui::Pixels>>, &mut gpui::Window, &mut gpui::App) + 'static {
        self.reveal_work_control(request, true)
    }

    fn reveal_work_control(
        &self,
        request: String,
        show_head: bool,
    ) -> impl Fn(Vec<gpui::Bounds<gpui::Pixels>>, &mut gpui::Window, &mut gpui::App) + 'static {
        let pending = self.shell.blocking_reveal.clone();
        let key = BlockingReveal {
            owner: self.sidebar.active_id().to_string(),
            run_seq: self.work.run_seq,
            request,
        };
        let scroll = self.active_view().map(|view| view.scroll.clone());
        move |children, window, cx| {
            if pending.borrow().as_ref() != Some(&key) {
                return;
            }
            let (Some(target), Some(scroll)) = (children.first(), scroll.as_ref()) else {
                return;
            };
            let target = *target;
            let pending = pending.clone();
            let key = key.clone();
            let scroll = scroll.clone();
            window.defer(cx, move |window, _cx| {
                if pending.borrow().as_ref() != Some(&key) {
                    return;
                }
                // The parent tracked-scroll bounds are updated after its
                // children's prepaint. Read them after that frame completes.
                let viewport = scroll.bounds();
                let mut target = target;
                if viewport.size.height < target.size.height {
                    if !show_head {
                        return;
                    }
                    target.size.height = viewport.size.height;
                }
                pending.borrow_mut().take();
                let delta = if target.origin.y < viewport.origin.y {
                    viewport.origin.y - target.origin.y
                } else if target.origin.y + target.size.height
                    > viewport.origin.y + viewport.size.height
                {
                    viewport.origin.y + viewport.size.height - target.origin.y - target.size.height
                } else {
                    return;
                };
                let offset = scroll.offset();
                let y = (offset.y + delta)
                    .min(gpui::Pixels::ZERO)
                    .max(-scroll.max_offset().y);
                scroll.set_offset(gpui::point(offset.x, y));
                window.refresh();
            });
        }
    }

    /// Review deliberately returns to the current gate, then focuses its first
    /// action/editor. No run state, scope, request or credential is changed.
    pub(crate) fn focus_work_blocking_card(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if !self.work.panel_is_visible(self.sidebar.active_id()) {
            return;
        }
        if let Some(view) = self.active_view() {
            crate::transcript_keys::pin_to_bottom(&view.scroll);
            view.scroll.scroll_to_bottom();
        }
        let request = self
            .work
            .panel
            .approval
            .as_ref()
            .filter(|card| card.kind != CLARIFY_KIND && card.delivery.pending.is_none())
            .map(|card| format!("approval:{}", card.approval_id))
            .or_else(|| {
                self.work
                    .panel
                    .credential
                    .as_ref()
                    .map(|card| format!("credential:{}", card.request_id))
            });
        *self.shell.blocking_reveal.borrow_mut() = request.map(|request| BlockingReveal {
            owner: self.sidebar.active_id().to_string(),
            run_seq: self.work.run_seq,
            request,
        });
        if self
            .work
            .panel
            .approval
            .as_ref()
            .is_some_and(|card| card.kind != CLARIFY_KIND && card.delivery.pending.is_none())
        {
            window.focus(&self.shell.approval_focus, cx);
        } else if self.work.panel.credential.is_some() {
            let input_focus = self.credential_input.read(cx).focus_handle().clone();
            window.focus(&input_focus, cx);
        } else {
            self.focus_shell_page(window, cx);
        }
    }

    pub(crate) fn request_work_blocking_reveal(&mut self, request: String) {
        *self.shell.blocking_reveal.borrow_mut() = Some(BlockingReveal {
            owner: self.sidebar.active_id().to_string(),
            run_seq: self.work.run_seq,
            request,
        });
    }

    /// User-opened job details. The native kit switches this floating face to
    /// a sheet in compact windows; navigation never opens it automatically.
    pub fn work_right_panel(
        &self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use crate::job_panel::JobTab;
        let owns_run = self.work.panel_is_visible(self.sidebar.active_id());
        let body = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(theme::z(theme::SP_12));
        let body = match self.shell.job_tab {
            JobTab::History => body
                .child(self.work_header())
                .children(self.work_other_conversation_card(cx))
                .when(owns_run, |d| {
                    d.child(self.work_progress_card())
                        .children(self.work_tools_card())
                        .children(self.work_agents_card(cx))
                })
                .child(self.work_scheduled_card(cx)),
            JobTab::Terminal => body
                .when(owns_run, |d| d.children(self.work_terminal_card()))
                .when(!owns_run || self.work.terminal_view.is_none(), |d| {
                    d.child(crate::ui::Card::new("job-terminal-empty").body(
                        "The terminal appears here when this conversation has a working folder.",
                    ))
                }),
            JobTab::Previews => body
                .when(owns_run, |d| d.children(self.work_preview_rail()))
                .when(
                    !owns_run
                        || (self.work.touched_files.is_empty()
                            && self.work.referenced_urls.is_empty()),
                    |d| {
                        d.child(
                            crate::ui::Card::new("job-previews-empty")
                                .body("Files and links from this job appear here."),
                        )
                    },
                ),
            JobTab::Context => body
                // Keep every existing working control reachable during the
                // subsequent interior migrations, including the folder tree.
                .children(self.work_rules_card())
                .children(
                    self.work_sandbox_line()
                        .map(|line| crate::ui::Row::new("job-sandbox").title(line).wrap()),
                )
                .child(self.work_memory_card(cx))
                .child(self.work_skills_card(cx))
                .child(
                    crate::ui::Section::new("job-details", "Details for nerds")
                        .nerd()
                        .open(self.shell.job_details)
                        .on_toggle(cx.listener(|app, _, _, cx| {
                            app.shell.job_details = !app.shell.job_details;
                            cx.notify();
                        }))
                        .body(vec![
                            self.work_connection_card(cx).into_any_element(),
                            self.workspace_read_card(cx).into_any_element(),
                        ]),
                ),
        };
        let layout = (
            self.shell.job_tab,
            window.viewport_size(),
            theme::ui_scale(),
        );
        let last = window.use_keyed_state("job-panel-tab-layout", cx, |_, _| None);
        if last.update(cx, |last, _| {
            let changed = last.as_ref() != Some(&layout);
            *last = Some(layout);
            changed
        }) {
            let scroll = self.shell.job_tabs_scroll.clone();
            let selected = self.shell.job_tab.index();
            scroll.scroll_to_item(selected);
            window.defer(cx, move |window, cx| {
                if last.read(cx).as_ref() == Some(&layout) {
                    scroll.scroll_to_item(selected);
                    window.refresh();
                }
            });
        }
        let tab_handle = cx.entity().downgrade();
        let tabs = crate::ui::Tabs::new(
            "job-panel-tabs",
            JobTab::ALL
                .iter()
                .enumerate()
                .map(|(i, tab)| {
                    crate::ui::TabItem::new(SharedString::from(format!("job-tab-{i}")), tab.label())
                })
                .collect(),
            self.shell.job_tab.index(),
        )
        .scroll_handle(self.shell.job_tabs_scroll.clone())
        .on_select(move |index, _, _, cx| {
            let _ = tab_handle.update(cx, |app, cx| {
                app.shell.job_tab = JobTab::ALL[index];
                app.work.scroll.set_offset(gpui::Point::default());
                cx.notify();
            });
        });
        // This Job caller owns a bounded viewport. Generic Panel bodies can
        // still scroll intrinsically wide resources; Terminal gets the real
        // dock width for its grid and header actions.
        let body = div()
            .id("awili-job-body")
            .debug_selector(|| "awili-job-body".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .overflow_y_scroll()
            .track_scroll(&self.work.scroll)
            .child(body);
        let sheet = crate::job_panel::uses_sheet(window);
        div()
            .debug_selector(|| "awili-work-panel".into())
            .flex_none()
            .flex()
            .flex_col()
            .min_h_0()
            .w(theme::z(theme::PANEL_W))
            .my(theme::z(theme::EDGE))
            .mr(theme::z(theme::EDGE))
            .when(sheet, |d| d.absolute().inset_0().w_full().h_full().m_0())
            .child(
                crate::ui::Panel::new("awili-job-panel")
                    .sheet_when(sheet)
                    .header("Job", None)
                    .full_height()
                    .focus_handle(&self.shell.job_focus)
                    .tabs(tabs)
                    .body_fill(body)
                    .footer(self.work_cancel_button(cx).unwrap().into_any_element())
                    .on_close(cx.listener(|app, _, window, cx| app.close_job_panel(window, cx))),
            )
    }

    /// R1h: the Work status while a plan waits for approval.
    pub(crate) const PLAN_WAITING_LABEL: &'static str = "waiting for your OK";
    /// R1h: the test lookup key prefix of the Work status (the label follows).
    pub(crate) const WORK_STATUS_SELECTOR: &'static str = "r1h-work-status-";
    /// RC2-APP (S-Q2): the test lookup key prefix of the Progress card's text
    /// when it has no steps (the text follows).
    pub(crate) const PROGRESS_TEXT_SELECTOR: &'static str = "rc2app-progress-";
    /// RC2-R616: the test lookup key prefix of the Progress card's activity
    /// line (the line as drawn follows).
    pub(crate) const PROGRESS_ACTIVITY_SELECTOR: &'static str = "rc2r616-progress-activity-";

    /// RC2-R616: how long the running Work turn has gone (`None`: none).
    pub(crate) fn work_waited(&self) -> Option<std::time::Duration> {
        self.work
            .running
            .then_some(self.work.run_started)
            .flatten()
            .map(|t| t.elapsed())
    }

    /// RC2-R616: since anything of the running turn last changed.
    pub(crate) fn work_quiet(&self) -> std::time::Duration {
        self.work.health.activity.silence(std::time::Instant::now())
    }
    /// RC2-APP (S-Q2): the test lookup key prefix of a Tools card row; the
    /// row's glyph, title and shown subtitle follow (`<glyph> <title> | <subtitle>`).
    pub(crate) const TOOL_ROW_SELECTOR: &'static str = "rc2app-tool-";

    /// Status plus requested configuration and separately reported model.
    fn work_header(&self) -> impl IntoElement {
        let panel = &self.work.panel;
        let owns_run = self.work.panel_is_visible(self.sidebar.active_id());
        let (label, color) = if !owns_run {
            (SharedString::from("ready"), theme::palette().text_dim)
        } else if panel
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with(crate::recall::POLICY_REFUSAL_PREFIX))
        {
            // RC2-UX (D-R1 S1 badge): a refused turn never ran; it was not
            // sent, it did not fail.
            (
                SharedString::from(NOT_SENT_LABEL),
                theme::palette().text_dim,
            )
        } else if panel.error.is_some() {
            // The message itself lives in the error CARD below (critique:
            // errors are cards with a cause and a fix, not header debris).
            (
                SharedString::from("failed"),
                theme::tone_text(theme::Tone::ActNow),
            )
        } else if self.work.running {
            (SharedString::from("working…"), theme::palette().text_dim)
        } else if self.plan_gate_pending_here() {
            // R1h: a plan waiting for approval is not "completed".
            (
                SharedString::from(Self::PLAN_WAITING_LABEL),
                theme::tone_text(theme::Tone::ActNow),
            )
        } else if let Some(s) = panel.status {
            if s.is_success() {
                if panel.tool_attempt_failed {
                    (
                        SharedString::from("finished with warnings"),
                        theme::tone_text(theme::Tone::ActNow),
                    )
                } else {
                    (SharedString::from("completed"), theme::palette().text_dim)
                }
            } else {
                (
                    SharedString::from(status_label(s)),
                    theme::tone_text(theme::Tone::ActNow),
                )
            }
        } else {
            (SharedString::from("ready"), theme::palette().text_dim)
        };

        let provenance = self
            .work_provenance_lines(owns_run)
            .into_iter()
            .map(|line| crate::ui::label(line).dim().wrap());
        div()
            .flex()
            .flex_col()
            .gap(theme::z(theme::SP_4))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme::z(theme::SP_8))
                    .child(crate::ui::label("Job").strong())
                    .child(
                        div()
                            .debug_selector(|| format!("{}{label}", Self::WORK_STATUS_SELECTOR))
                            .child(
                                crate::ui::label(label.clone())
                                    .text_color_override(color)
                                    .wrap(),
                            ),
                    ),
            )
            .children(provenance)
    }

    /// The PROGRESS checklist: one row per plan step.
    /// RC1-Q (round 5 minor 3): the Progress card's words when it has no
    /// steps — never "No run yet" while a plan waits or after a run ended.
    fn work_progress_card(&self) -> impl IntoElement {
        let panel = &self.work.panel;
        let rows: Vec<_> = panel
            .steps
            .iter()
            .enumerate()
            .map(|(i, step)| step_row(i, step))
            .collect();

        let body: gpui::AnyElement = if rows.is_empty() {
            let msg = progress_empty_text(
                self.work.running,
                self.plan_gate_pending_here(),
                panel.has_ended(),
            );
            // RC2-R616: a run that is waiting says for how long.
            let shown = alive_line(msg, self.work_waited(), self.work_quiet());
            div()
                .debug_selector(move || format!("{}{msg}", Self::PROGRESS_TEXT_SELECTOR))
                .child(crate::ui::label(shown).dim().wrap())
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_8))
                .children(rows)
                .into_any_element()
        };

        let activity = panel
            .activity
            .clone()
            .filter(|_| {
                crate::gateway_stop_deadline::activity_visible(
                    self.work.running,
                    self.work.gateway_stop_pending,
                )
            })
            .map(|a| {
                // RC2-R63: a command step reads as the command itself.
                // RC2-R616: and a wait says for how long.
                let line = alive_line(
                    &awili_code::plain_step(&a),
                    self.work_waited(),
                    self.work_quiet(),
                );
                div()
                    .debug_selector({
                        let line = line.clone();
                        move || format!("{}{line}", Self::PROGRESS_ACTIVITY_SELECTOR)
                    })
                    .child(crate::ui::label(line).dim().wrap())
            });

        let activity = activity.into_iter().chain(self.work_idle_line());
        crate::ui::Card::new("job-progress")
            .title("Progress")
            .children(std::iter::once(body).chain(activity.map(IntoElement::into_any_element)))
    }

    /// The TOOLS card: recent tool invocations with status. Only shown when
    /// there are tool calls to display.
    fn work_tools_card(&self) -> Option<impl IntoElement> {
        if self.work.panel.tool_calls.is_empty() {
            return None;
        }
        let rows = self
            .work
            .panel
            .tool_calls
            .iter()
            .rev()
            .take(20)
            .enumerate()
            .map(|(i, tc)| {
                let (glyph, tone) = match tc.status {
                    ToolCallStatus::Running => ("\u{25CF}", theme::Tone::Neutral),
                    ToolCallStatus::Completed => ("\u{2713}", theme::Tone::Neutral),
                    ToolCallStatus::Failed => ("\u{2717}", theme::Tone::ActNow),
                    ToolCallStatus::Ended => ("\u{2013}", theme::Tone::Neutral),
                };
                let title = tc.title().to_string();
                let subtitle = tc.message.clone().unwrap_or_default();
                let selector = format!(
                    "{}{glyph} {title} | {}",
                    Self::TOOL_ROW_SELECTOR,
                    text_clip::clip(&subtitle, 60, 57)
                );
                div().debug_selector(move || selector.clone()).child(
                    crate::ui::Row::new(format!("job-tool-{i}"))
                        .leading(crate::ui::label(glyph).tone(tone))
                        .title(title)
                        .subtitle(subtitle)
                        .wrap(),
                )
            });
        Some(
            crate::ui::Card::new("job-tools")
                .title(format!("Tools used ({})", self.work.panel.tool_calls.len()))
                .children(rows),
        )
    }

    /// RC2-UX (D-PUNCH S2-F16): Hermes asked a question (a clarify gate)
    /// on the conversation on screen: the next message the owner sends is
    /// the answer.
    pub(crate) fn clarify_waiting(&self) -> bool {
        self.mode == Mode::Work
            && self.work.panel_is_visible(self.sidebar.active_id())
            && self
                .work
                .panel
                .approval
                .as_ref()
                .is_some_and(|c| c.kind == CLARIFY_KIND)
    }

    /// RC2-UX (D-PUNCH S2-F16): send `answer` to Hermes' question (it was
    /// shown as an approval card whose Allow/Deny sent a verdict Hermes
    /// never asked for, and nothing sent the answer). The question and the
    /// answer are kept in the reply, in plain words.
    pub(crate) fn answer_work_clarify(&mut self, answer: String, cx: &mut Context<Self>) {
        let Some(card) = self.work.panel.approval.take() else {
            return;
        };
        if let Some(tx) = self.work.gateway_cmd_tx.as_ref() {
            let _ = tx.send(GatewayCmd::Clarify {
                request_id: card.approval_id.clone(),
                answer: answer.clone(),
            });
        }
        let quote = |t: &str| t.lines().collect::<Vec<_>>().join("\n> ");
        self.work_stream_token(
            &format!(
                "\n> **Hermes asked:** {}\n> **You answered:** {}\n\n",
                quote(&card.prompt),
                quote(&answer)
            ),
            cx,
        );
        cx.notify();
    }

    /// A clarify gate asks for an answer in the composer, not a verdict.
    fn work_clarify_card(&self, card: &ApprovalCard) -> gpui::AnyElement {
        crate::ui::Card::new("work-clarify-card")
            .selector(CLARIFY_CARD_SELECTOR)
            .tone(theme::Tone::ActNow)
            .label(approval_heading(card))
            .body(card.prompt.clone())
            .children([crate::ui::label(CLARIFY_HOW_TO_ANSWER).dim().wrap()])
            .into_any_element()
    }

    /// The owning conversation's inline gate. Scope availability and verdict
    /// delivery still come from the existing card/resolver; the kit owns faces,
    /// disabled reasons, focus/keyboard activation and double-click protection.
    fn work_approval_card(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let card = self.work.panel.approval.clone()?;
        if card.kind == CLARIFY_KIND {
            return Some(self.work_clarify_card(&card));
        }
        let folder_bound = self.work.folder.is_some();
        let pending = card.delivery.pending.is_some();
        let allow_label = if pending {
            "Sending…"
        } else if card.delivery.failed {
            "Retry Allow once"
        } else {
            "Allow once"
        };
        let mut allow = crate::ui::Button::new(
            "work-allow-once",
            format!(
                "{allow_label}  {}",
                crate::approval_keys::hint(cfg!(target_os = "macos"), false)
            ),
        )
        .selector("work-allow-once")
        .focus_handle(&self.shell.approval_focus)
        .primary()
        .tab_index(crate::focus_nav::APPROVAL)
        .tooltip(format!(
            "Approve this action and continue the run ({})",
            crate::approval_keys::hint(cfg!(target_os = "macos"), false)
        ))
        .on_click(
            cx.listener(|this, _, _, cx| this.resolve_work_approval(ApprovalDecision::Approve, cx)),
        );
        if pending {
            allow = allow.disabled("Waiting for the approval decision to be acknowledged");
        }
        let scope_button =
            |id: &'static str, label: &'static str, decision, cx: &mut Context<Self>| {
                let button = crate::ui::Button::new(id, label)
                    .selector(id)
                    .ghost()
                    .tab_index(crate::focus_nav::APPROVAL)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.resolve_work_approval(decision, cx)),
                    );
                if pending {
                    button.disabled("Waiting for the approval decision to be acknowledged")
                } else if !card.offers(decision, folder_bound) {
                    button.disabled(if decision == ApprovalDecision::Folder {
                        "No working folder is bound to this approval"
                    } else {
                        "This action cannot offer a permanent approval"
                    })
                } else {
                    button
                }
            };
        let in_app = crate::local_first::identity_engine_kind(
            self.work.worker_identity.as_ref(),
            &self.work.executor,
        ) == awili_config::EngineKind::Awili;
        let deny_tip = if in_app {
            "Don't do this: stop here and put your request back in the box"
        } else {
            "Refuse this action; the agent must find another way"
        };
        let mut deny = crate::ui::Button::new(
            "work-deny",
            format!(
                "Deny  {}",
                crate::approval_keys::hint(cfg!(target_os = "macos"), true)
            ),
        )
        .selector("work-deny")
        .ghost()
        .tab_index(crate::focus_nav::APPROVAL)
        .tooltip(format!(
            "{deny_tip} ({})",
            crate::approval_keys::hint(cfg!(target_os = "macos"), true)
        ))
        .on_click(cx.listener(|this, _, window, cx| {
            this.resolve_work_approval_focusing(ApprovalDecision::Deny, window, cx)
        }));
        if pending {
            deny = deny.disabled("Waiting for the approval decision to be acknowledged");
        }
        let actions = div()
            .flex()
            .flex_wrap()
            .gap(theme::z(theme::SP_8))
            .on_children_prepainted(
                self.reveal_work_blocking_control(format!("approval:{}", card.approval_id)),
            )
            .child(allow)
            .child(deny)
            .child(scope_button(
                "work-allow-session",
                "Allow for session",
                ApprovalDecision::Session,
                cx,
            ))
            .child(scope_button(
                "work-allow-folder",
                "Allow for folder",
                ApprovalDecision::Folder,
                cx,
            ))
            .child(scope_button(
                "work-allow-always",
                "Allow always",
                ApprovalDecision::Always,
                cx,
            ));
        let mut body = vec![actions.into_any_element()];
        if card.delivery.failed {
            body.insert(0, crate::ui::label("Approval delivery failed or is unconfirmed. Check the run before retrying; no automatic retry was made.").tone(theme::Tone::ActNow).wrap().into_any_element());
        }
        Some(
            crate::ui::Card::new("work-approval-card")
                .tone(theme::Tone::ActNow)
                .label(approval_heading(&card))
                .body(card.prompt)
                .children(body)
                .into_any_element(),
        )
    }

    /// The answered card retains its position and all five inert actions.
    fn work_decided_card(&self) -> Option<gpui::AnyElement> {
        let decided = self.work.panel.decided.as_ref()?;
        let outcome = match decided.choice {
            "once" => "\u{2713} Allowed once",
            "session" => "\u{2713} Allowed for this session",
            "folder" => "\u{2713} Allowed for this folder",
            "always" => "\u{2713} Always allowed",
            _ => "\u{2717} Denied",
        };
        let inert = |id: &'static str, label: String| {
            crate::ui::Button::new(id, label)
                .selector(id)
                .disabled("This approval has already been answered")
        };
        let actions = div()
            .flex()
            .flex_wrap()
            .gap(theme::z(theme::SP_8))
            .child(inert(
                "work-decided-allow",
                format!(
                    "Allow once  {}",
                    crate::approval_keys::hint(cfg!(target_os = "macos"), false)
                ),
            ))
            .child(inert(
                "work-decided-deny",
                format!(
                    "Deny  {}",
                    crate::approval_keys::hint(cfg!(target_os = "macos"), true)
                ),
            ))
            .child(inert("work-decided-session", "Allow for session".into()))
            .child(inert("work-decided-folder", "Allow for folder".into()))
            .child(inert("work-decided-always", "Allow always".into()));
        Some(
            crate::ui::Card::new("work-decided-card")
                .selector("work-decided-card")
                .tone(theme::Tone::Neutral)
                .label(outcome)
                .body(decided.card.prompt.clone())
                .children([actions])
                .into_any_element(),
        )
    }

    /// Keep the existing isolated, masked editor and credential handlers.
    fn work_credential_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let card = self.work.panel.credential.clone()?;
        let (heading, detail) = match card.kind {
            CredentialKind::Sudo => (
                "SUDO PASSWORD REQUESTED",
                if card.prompt.is_empty() {
                    "The agent needs a sudo password to run a privileged command.".to_string()
                } else {
                    card.prompt.clone()
                },
            ),
            CredentialKind::Secret => ("CREDENTIAL REQUESTED", {
                let base = if card.env_var.is_empty() {
                    "The agent needs a credential value.".to_string()
                } else {
                    format!("The agent needs the {} credential.", card.env_var)
                };
                if card.prompt.is_empty() {
                    base
                } else {
                    format!("{base}\n{}", card.prompt)
                }
            }),
        };
        let input = self.credential_input.clone();
        let field = crate::ui::TextField::new("work-credential-input", "Credential value")
            .editor(move |_| input);
        let field = div()
            .w_full()
            .on_children_prepainted(
                self.reveal_work_blocking_control(format!("credential:{}", card.request_id)),
            )
            .child(field);
        let actions = div()
            .flex()
            .flex_wrap()
            .gap(theme::z(theme::SP_8))
            .child(
                crate::ui::Button::new("work-credential-submit", "Send")
                    .selector("work-credential-submit")
                    .primary()
                    .tooltip("Send the credential to the waiting agent")
                    .on_click(cx.listener(|this, _, _, cx| this.submit_credential(cx))),
            )
            .child(
                crate::ui::Button::new("work-credential-refuse", "Decline")
                    .selector("work-credential-refuse")
                    .ghost()
                    .tooltip("Refuse the credential request")
                    .on_click(cx.listener(|this, _, _, cx| this.refuse_credential(cx))),
            );
        Some(
            crate::ui::Card::new("work-credential-card")
                .tone(theme::Tone::ActNow)
                .label(heading)
                .body(detail)
                .children([field.into_any_element(), actions.into_any_element()]),
        )
    }

    /// The AGENTS card: live hierarchy of subagents (unit-18 reducer output).
    /// Only shown once a subagent has been seen. The header carries the
    /// active/total count plus a rolled-up token/cost summary; the body renders
    /// the parent-nested tree via `build_subagent_tree`.
    fn work_agents_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.work.subagents.is_empty() {
            return None;
        }

        let store = &self.work.subagents;
        let total = store.len();
        let active = store
            .iter()
            .filter(|a| !subagent_is_done(&a.status))
            .count();
        let (in_tok, out_tok, cost) = subagent_totals(store);
        let mut header_label = format!("AGENTS · {active} active / {total} total");
        if in_tok + out_tok > 0 {
            header_label.push_str(&format!(" · {} tok", in_tok + out_tok));
        }
        if cost > 0.0 {
            header_label.push_str(&format!(" · ${cost:.4}"));
        }

        let tree = build_subagent_tree(store);
        let rows = tree.iter().map(|node| self.agent_row(node, 0));
        Some(
            crate::ui::Section::new("job-helpers", header_label.replace("AGENTS", "Helpers"))
                .open(self.work.agents_open)
                .on_toggle(cx.listener(|this, _, _, cx| {
                    this.work.agents_open = !this.work.agents_open;
                    cx.notify();
                }))
                .body(rows),
        )
    }

    /// Render one agent row (a `SubagentNode` from unit 18's tree) with recursive
    /// children, indented by depth. Status glyph + color, a subtitle carrying
    /// model / tool count / files read+written / elapsed timer / tokens / cost,
    /// and a live activity line (current tool, or the final summary once done).
    fn agent_row(&self, node: &SubagentNode, depth: usize) -> gpui::AnyElement {
        let agent = &node.progress;
        let indent = theme::z((depth as f32) * theme::SP_16);

        let done = subagent_is_done(&agent.status);
        let status = if subagent_is_error(&agent.status) {
            "Failed"
        } else if done {
            "Finished"
        } else if agent.status == "pending" {
            "Pending"
        } else {
            "Running"
        };
        let tone = if subagent_is_error(&agent.status) {
            theme::Tone::ActNow
        } else {
            theme::Tone::Neutral
        };

        let mut subtitle_parts: Vec<String> = Vec::new();
        if let Some(m) = &agent.model {
            subtitle_parts.push(m.clone());
        }
        if let Some(tc) = agent.tool_count {
            if tc > 0 {
                subtitle_parts.push(format!("{tc} tools"));
            }
        }
        let (nr, nw) = (agent.files_read.len(), agent.files_written.len());
        if nr + nw > 0 {
            subtitle_parts.push(format!("{nr}R/{nw}W"));
        }
        if let Some(d) = agent.duration_seconds {
            if d >= 0.5 {
                subtitle_parts.push(format!("{d:.0}s"));
            }
        }
        let toks = agent.input_tokens.unwrap_or(0) + agent.output_tokens.unwrap_or(0);
        if toks > 0 {
            subtitle_parts.push(format!("{toks} tok"));
        }
        if let Some(c) = agent.cost_usd {
            if c > 0.0 {
                subtitle_parts.push(format!("${c:.4}"));
            }
        }
        let subtitle = subtitle_parts.join(" · ");

        let activity_line = if done {
            agent.summary.clone()
        } else {
            agent
                .current_tool
                .clone()
                .or_else(|| agent.stream.last().map(|e| e.text.clone()))
        };

        let goal = agent.goal.clone().unwrap_or_else(|| "subagent".to_string());

        let children: Vec<gpui::AnyElement> = node
            .children
            .iter()
            .map(|child| self.agent_row(child, depth + 1))
            .collect();

        let detail = [
            Some(status.to_string()),
            (!subtitle.is_empty()).then_some(subtitle),
            activity_line,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        div()
            .flex()
            .flex_col()
            .pl(indent)
            .child(
                crate::ui::Row::new(format!("job-helper-{}", agent.id))
                    .title(goal)
                    .subtitle(detail)
                    .wrap()
                    .leading(crate::ui::Dot::new(tone).label(status)),
            )
            .children(children)
            .into_any_element()
    }

    /// Terminal card — renders the TerminalView entity if one exists.
    fn work_terminal_card(&self) -> Option<impl IntoElement> {
        let view = self.work.terminal_view.as_ref()?;
        // RC2-UX (R5-4: the box "appears … I never touched it. Unexplained"):
        // one plain line says what it is.
        Some(
            div()
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_4))
                .child(
                    div()
                        .debug_selector(|| TERMINAL_CAPTION_SELECTOR.to_string())
                        .child(crate::ui::label(TERMINAL_CAPTION).dim().wrap()),
                )
                .child(view.clone()),
        )
    }

    /// Preview rail: files touched + URLs referenced during the run.
    fn work_preview_rail(&self) -> Option<impl IntoElement> {
        if self.work.touched_files.is_empty() && self.work.referenced_urls.is_empty() {
            return None;
        }
        let files = self.work.touched_files.iter().enumerate().map(|(i, file)| {
            let path = std::path::PathBuf::from(file);
            let path = if path.is_absolute() {
                Some(path)
            } else {
                self.work.preview_root.as_ref().map(|root| root.join(path))
            };
            let mut row = crate::ui::Row::new(format!("job-preview-file-{i}"))
                .title(file.clone())
                .wrap()
                .icon(file_badge(file));
            if let Some(path) = path {
                row = row
                    .subtitle(path.display().to_string())
                    .on_click(move |_, _, _| {
                        open_job_preview(path.to_string_lossy().as_ref());
                    });
            } else {
                row = row.disabled("This job has no working folder for this relative path");
            }
            row
        });
        let links = self
            .work
            .referenced_urls
            .iter()
            .enumerate()
            .map(|(i, url)| {
                let url = url.clone();
                crate::ui::Row::new(format!("job-preview-link-{i}"))
                    .title(url.clone())
                    .wrap()
                    .icon(crate::icons::Icon::Link)
                    .on_click(move |_, _, _| open_job_preview(&url))
            });
        Some(
            crate::ui::Card::new("job-previews")
                .title("Files and links")
                .children(files.chain(links)),
        )
    }

    /// The result card — the run's output rendered as markdown.
    /// The folder-pick + Cancel controls at the foot of the panel.
    fn work_cancel_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let running_here =
            self.work.running && self.work.panel_is_visible(self.sidebar.active_id());
        Some(
            div()
                .flex()
                .w_full()
                .justify_between()
                .items_center()
                .gap(theme::z(theme::SP_8))
                .child(
                    crate::ui::Button::new("work-pick", "Choose folder")
                        .quiet()
                        .on_click(cx.listener(|app, _, _, cx| app.pick_work_folder(cx))),
                )
                .children(running_here.then(|| {
                    crate::ui::Button::new("work-cancel", "Stop")
                        .ghost()
                        .icon(crate::icons::Icon::Square)
                        .on_click(cx.listener(|app, _, _, cx| app.cancel_work_run(cx)))
                })),
        )
    }

    /// The scheduled tasks card — shows Hermes cron jobs with status indicators.
    fn work_scheduled_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.work.jobs.iter().enumerate().map(|(i, job)| {
            let schedule = job.schedule_display.as_deref().unwrap_or("manual");
            let state = if job.enabled { "Enabled" } else { "Disabled" };
            let subtitle = format!(
                "{state} · {schedule} · {}",
                job.last_status.as_deref().unwrap_or("No result yet")
            );
            let tone = if job.last_status.as_deref() == Some("error") {
                theme::Tone::ActNow
            } else {
                theme::Tone::Neutral
            };
            crate::ui::Row::new(format!("job-scheduled-{i}"))
                .title(job.name.clone())
                .subtitle(subtitle)
                .wrap()
                .leading(crate::ui::Dot::new(tone).label(state))
        });
        crate::ui::Card::new("job-scheduled")
            .title("Scheduled jobs")
            .children(
                self.work
                    .jobs
                    .is_empty()
                    .then(|| {
                        crate::ui::label("No scheduled jobs loaded")
                            .dim()
                            .wrap()
                            .into_any_element()
                    })
                    .into_iter()
                    .chain(rows.map(IntoElement::into_any_element)),
            )
            .actions(vec![
                crate::ui::Button::new("jobs-refresh", "Refresh")
                    .quiet()
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_jobs(cx)))
                    .into_any_element(),
                crate::ui::Button::new("job-open-automations", "Automations")
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.set_mode(Mode::Cron, cx)))
                    .into_any_element(),
            ])
    }
}

// ============================================================================
// Worker threads (gateway + REST fallback)
// ============================================================================

/// P4b — the execution half of the four typed interlock tools: runs one
/// parsed `interlock.*` call through the typed v6.5 adapter and returns the
/// lane-labeled, transcript-safe result shape `awili_tools::interlock`
/// defines. Every verb routes through `awili_memory::v65` — there is no
/// freeform endpoint here, and a rejected write is never retried or fallen
/// back from.
///
/// Backend discipline: the verbs are v6.5-only. On the v5 backend they
/// refuse BEFORE any request — v5 is the explicit backend fallback for the
/// boot/recall lanes (see `crate::recall` and [`inspect_call`]), never a
/// fallback for the interlock verb set.
pub(crate) fn execute_interlock(
    client: &awili_memory::MemoryClient,
    call: &awili_tools::interlock::InterlockCall,
) -> Result<serde_json::Value, String> {
    use awili_tools::interlock;
    if client.backend() != awili_memory::MemoryBackend::V65 {
        return Err(
            "interlock verbs require the v6.5 backend; the v5 backend is the \
             explicit boot/recall fallback, not an interlock target"
                .to_string(),
        );
    }
    match call {
        interlock::InterlockCall::Bootstrap => {
            let boot = awili_memory::v65::bootstrap(client).map_err(|e| e.to_string())?;
            Ok(interlock::read_result(&boot.mandatory_policy, &boot.items))
        }
        interlock::InterlockCall::Recall { query } => {
            let rec = awili_memory::v65::recall(client, query).map_err(|e| e.to_string())?;
            Ok(interlock::read_result(&rec.mandatory_policy, &rec.items))
        }
        interlock::InterlockCall::Remember(c) => {
            // The tool boundary already checked the vocabulary; re-derive
            // the typed values strictly here — the double guard that keeps
            // a freeform string out of the serialized body.
            let predicate = awili_memory::v65::Predicate::from_wire(&c.predicate)
                .ok_or_else(|| "remember predicate is outside the wire vocabulary".to_string())?;
            let authority = awili_memory::v65::Authority::from_wire(&c.authority)
                .ok_or_else(|| "remember authority is outside the wire vocabulary".to_string())?;
            let epistemic = awili_memory::v65::EpistemicStatus::from_wire(&c.epistemic_status)
                .ok_or_else(|| {
                    "remember epistemic status is outside the wire vocabulary".to_string()
                })?;
            let req = awili_memory::v65::RememberRequest {
                request_id: c.request_id.clone(),
                subject: c.subject.clone(),
                predicate,
                object: c.object.clone(),
                authority,
                epistemic_status: epistemic,
                source_type: c.source_type.clone(),
                source_ref: c.source_ref.clone(),
                reason: c.reason.clone(),
            };
            let res = awili_memory::v65::remember(client, &req).map_err(|e| e.to_string())?;
            Ok(interlock::remember_result(res.id.as_deref()))
        }
        interlock::InterlockCall::ArchiveSearch { query, limit } => {
            let res = awili_memory::v65::archive_search(client, query, *limit)
                .map_err(|e| e.to_string())?;
            Ok(interlock::archive_result(&res))
        }
    }
}

/// Decode a lane-labeled read-verb result (`interlock.bootstrap` /
/// `interlock.recall`) into the inspector's lane state. Both read verbs
/// share the exact result contract: the mandatory-policy lane, then the
/// recall-items lane — labeled and disjoint, in that order.
fn decode_inspector_read(value: serde_json::Value) -> Result<MemoryInspectorResults, String> {
    let lane = |key: &str| -> Result<Vec<awili_memory::V65Item>, String> {
        let arr = value
            .get(key)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("interlock result is missing the {key} lane"))?
            .clone();
        serde_json::from_value(serde_json::Value::Array(arr))
            .map_err(|e| format!("interlock {key} lane did not decode: {e}"))
    };
    Ok(MemoryInspectorResults::V65 {
        policy: lane(awili_tools::interlock::LANE_MANDATORY_POLICY)?,
        items: lane(awili_tools::interlock::LANE_RECALL_ITEMS)?,
    })
}

/// The inspector's interlock call (P4b): the v6.5 backend routes the READ
/// verbs through [`execute_interlock`] (the typed adapter); the v5 backend
/// runs the historical `/v5/recall` buckets — the explicit backend
/// fallback, unchanged from before P4b. The mutating/archive verbs are not
/// inspector operations (there is no user input for them here).
fn inspect_call(
    client: &awili_memory::MemoryClient,
    call: &awili_tools::interlock::InterlockCall,
) -> Result<MemoryInspectorResults, String> {
    if !matches!(
        call,
        awili_tools::interlock::InterlockCall::Bootstrap
            | awili_tools::interlock::InterlockCall::Recall { .. }
    ) {
        return Err("the memory inspector exposes the read-only interlock verbs".to_string());
    }
    match client.backend() {
        awili_memory::MemoryBackend::V65 => decode_inspector_read(execute_interlock(client, call)?),
        awili_memory::MemoryBackend::V5 => {
            let query = if let awili_tools::interlock::InterlockCall::Recall { query } = call {
                query.as_str()
            } else {
                "current project state" // Bootstrap, the pre-P4b inspector query
            };
            client
                .recall_v5(query, 10)
                .map(MemoryInspectorResults::V5)
                .map_err(|e| e.to_string())
        }
    }
}

/// Auto-discover the Hermes dashboard URL by finding the `hermes dashboard`
/// process and its listening port. Returns `http://127.0.0.1:<port>` or None.
impl AwiliApp {
    /// Toggle the memory inspector panel. Opening with no results yet runs
    /// the typed BOOT fetch (P4b `interlock.bootstrap`): the mandatory
    /// policy lane first, then the ordinary items lane.
    pub fn toggle_memory_panel(&mut self, cx: &mut Context<Self>) {
        self.work.memory_open = !self.work.memory_open;
        if self.work.memory_open && self.work.memory_results.is_none() {
            self.run_inspector_call(awili_tools::interlock::InterlockCall::Bootstrap, cx);
        }
        cx.notify();
    }

    /// Run a semantic recall against the Foreman memory service — the
    /// `interlock.recall` verb through [`inspect_call`].
    pub fn recall_memory(&mut self, query: &str, cx: &mut Context<Self>) {
        self.run_inspector_call(
            awili_tools::interlock::InterlockCall::Recall {
                query: query.to_string(),
            },
            cx,
        );
    }

    /// One interlock tool call on the memory inspector: the verb runs OFF
    /// the UI thread ([`inspect_call`]) and the lane-labeled result is
    /// stored for rendering. A failure surfaces in the panel instead of
    /// leaving it at a silent "0 memories".
    fn run_inspector_call(
        &mut self,
        call: awili_tools::interlock::InterlockCall,
        cx: &mut Context<Self>,
    ) {
        self.work.memory_query = match &call {
            awili_tools::interlock::InterlockCall::Bootstrap => "bootstrap".to_string(),
            awili_tools::interlock::InterlockCall::Recall { query } => query.clone(),
            _ => "interlock".to_string(),
        };
        let mem = self.memory.clone();
        let (tx, rx) = async_channel::bounded::<Result<MemoryInspectorResults, String>>(1);
        std::thread::spawn(move || {
            let client = awili_memory::MemoryClient::resolved(mem.base_url, mem.min_priority)
                .with_token(mem.token);
            let result = inspect_call(&client, &call);
            if let Err(e) = &result {
                eprintln!("awili: memory interlock failed: {e}");
            }
            let _ = tx.send_blocking(result);
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    match result {
                        Ok(results) => {
                            this.work.memory_results = Some(results);
                            this.work.memory_error = None;
                        }
                        Err(e) => {
                            this.work.memory_error = Some(e);
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Context memory uses the shared section, input and rows. Backend lanes
    /// retain their labels and order; full owned text is never silently cut.
    fn work_memory_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::ui::{text, Button, Row, Section, TextField};
        let mut rows = Vec::new();
        let count = match self.work.memory_results.as_ref() {
            Some(MemoryInspectorResults::V5(results)) => {
                for (index, memory) in results.all().take(15).enumerate() {
                    rows.push(
                        Row::new(SharedString::from(format!("memory-result-{index}")))
                            .title(SharedString::from(memory.content.clone()))
                            .subtitle(SharedString::from(format!(
                                "Category: {}",
                                memory.category.as_deref().unwrap_or("unknown")
                            )))
                            .wrap()
                            .into_any_element(),
                    );
                }
                results.count()
            }
            Some(MemoryInspectorResults::V65 { policy, items }) => {
                for entry in policy.iter().take(8) {
                    if let Some(value) = awili_memory::v65_item_text(&entry.object) {
                        rows.push(
                            Row::new(SharedString::from(format!("memory-result-{}", rows.len())))
                                .title(SharedString::from(value))
                                .subtitle("Rule")
                                .wrap()
                                .into_any_element(),
                        );
                    }
                }
                for entry in items.iter().take(15usize.saturating_sub(rows.len())) {
                    if let Some(value) = awili_memory::v65_item_text(&entry.object) {
                        rows.push(
                            Row::new(SharedString::from(format!("memory-result-{}", rows.len())))
                                .title(SharedString::from(value))
                                .subtitle("Recalled note")
                                .wrap()
                                .into_any_element(),
                        );
                    }
                }
                policy.len() + items.len()
            }
            None => 0,
        };
        // A failed read wins over a stale count, including when prior rows exist.
        let header = if let Some(error) = &self.work.memory_error {
            text::ui_text(SharedString::from(format!(
                "Your memory could not be read: {error}"
            )))
            .tone(theme::Tone::ActNow)
            .wrap()
        } else if self.work.memory_results.is_none() {
            text::ui_text("Search your memory, or choose Rules or Recent.")
                .dim()
                .wrap()
        } else {
            let label = if self.work.memory_query.is_empty() {
                format!("{count} memories recalled")
            } else {
                format!("{count} memories for \"{}\"", self.work.memory_query)
            };
            text::ui_text(SharedString::from(label)).dim().wrap()
        };
        let editor = self.memory_search.clone();
        let queries = div()
            .flex()
            .flex_wrap()
            .gap(theme::z(theme::SP_8))
            .child(
                Button::new("mem-q-directives", "Rules").on_click(cx.listener(|this, _, _, cx| {
                    this.recall_memory("standing directives rules constraints", cx);
                })),
            )
            .child(Button::new("mem-q-recent", "Recent").on_click(cx.listener(
                |this, _, _, cx| {
                    this.recall_memory("recent decisions changes project progress", cx);
                },
            )));
        Section::new("memory-toggle", "Memory")
            .open(self.work.memory_open)
            .on_toggle(cx.listener(|this, _, _, cx| this.toggle_memory_panel(cx)))
            .body(vec![
                TextField::new("memory-search", "Search my memory…")
                    .selector(MEMORY_SEARCH_SELECTOR)
                    .editor(move |_| editor)
                    .into_any_element(),
                queries.into_any_element(),
                header.into_any_element(),
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .children(rows)
                    .into_any_element(),
            ])
    }

    pub fn test_hermes_connection(&mut self, cx: &mut Context<Self>) {
        if self.onboarding.is_active() {
            return;
        }
        if self.work.hermes_testing {
            return;
        }
        self.work.hermes_testing = true;
        self.work.hermes_status = None;
        cx.notify();

        let cfg = self.work.hermes.clone();
        let gw = self.work.gateway.clone();
        // P3e: test what the app will actually use — the gateway dashboard when
        // it is configured (it wins over REST, as in routing), else the REST
        // Hermes, else report the card's not-configured message (same
        // precedence as `connection_label`).
        match connection_test_target(cfg.enabled, &gw.dashboard_url) {
            ConnectionTestTarget::Rest => {}
            ConnectionTestTarget::Gateway => {
                // R1e: the launch / Work-entry check also refreshes the U2
                // reach that every gateway status line reads.
                self.start_gateway_probe(cx);
                let (tx, rx) = async_channel::bounded::<Result<String, String>>(1);
                if cfg!(test) {
                    // RC1-G1: the launch check's index fetch never runs on a
                    // real thread in a test build (gpui's test scheduler
                    // rejects its late wake-up); it reports "not probed".
                    let _ = tx.try_send(Err(
                        crate::gateway_health_app::GATEWAY_NOT_PROBED_IN_TESTS.to_string(),
                    ));
                } else {
                    // The index fetch is blocking network I/O: worker thread only.
                    std::thread::spawn(move || {
                        let result =
                            gateway_connection_test(&gw.dashboard_url, fetch_dashboard_index);
                        let _ = tx.send_blocking(result);
                    });
                }
                cx.spawn(async move |this, cx| {
                    if let Ok(result) = rx.recv().await {
                        this.update(cx, |this, cx| {
                            this.work.hermes_status = Some(result);
                            this.work.hermes_testing = false;
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
                return;
            }
            ConnectionTestTarget::None => {
                self.work.hermes_status = Some(Err(CONNECTION_NOT_CONFIGURED.to_string()));
                self.work.hermes_testing = false;
                cx.notify();
                return;
            }
        }
        // `None` skills payload = fetch failed/unattempted; the update below then
        // leaves the existing Work-banner panel untouched rather than wiping it.
        type SkillsPayload = (
            Vec<awili_gateway::rest::Skill>,
            Vec<awili_gateway::rest::Toolset>,
        );
        let (tx, rx) = async_channel::bounded::<(Result<String, String>, Option<SkillsPayload>)>(1);
        std::thread::spawn(move || {
            // Clone the base+key so `cfg` survives for `skills_rest_client` below.
            let client = RestClient::from_base_url(cfg.base_url.clone(), cfg.api_key.clone());
            let (result, skills) = match client.capabilities() {
                Ok(caps) => {
                    let features: Vec<&str> = [
                        caps.feature_bool("run_submission").map(|_| "runs"),
                        caps.feature_bool("chat_completions_streaming")
                            .map(|_| "streaming"),
                        caps.feature_bool("run_events_sse").map(|_| "SSE events"),
                        caps.feature_bool("run_approval_response")
                            .map(|_| "approvals"),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    let mode = caps
                        .runtime
                        .as_ref()
                        .and_then(|r| r.mode.as_deref())
                        .unwrap_or("unknown");
                    // Skills/toolsets live on the dashboard gateway (:9119) with the
                    // scraped session token, NOT the Hermes REST server (:8642) this
                    // `client` targets — hitting the Hermes server for /api/skills
                    // 404s (2026-07-01 bug #2). Fetch them from the same surface
                    // `refresh_skills` uses. A fetch failure yields `None` (panel
                    // preserved) rather than empty vecs that would silently wipe it.
                    let skills = skills_rest_client(&gw, &cfg).ok().and_then(|sc| {
                        match (sc.list_skills(), sc.list_toolsets()) {
                            (Ok(sk), Ok(ts)) => Some((sk, ts)),
                            _ => None,
                        }
                    });
                    (
                        Ok(format!(
                            "Connected ({}, {})",
                            mode,
                            if features.is_empty() {
                                "no features reported".to_string()
                            } else {
                                features.join(", ")
                            }
                        )),
                        skills,
                    )
                }
                Err(e) => (Err(e.to_string()), None),
            };
            let _ = tx.send_blocking((result, skills));
        });

        cx.spawn(async move |this, cx| {
            if let Ok((result, skills)) = rx.recv().await {
                this.update(cx, |this, cx| {
                    this.work.hermes_status = Some(result);
                    if let Some((skills, toolsets)) = skills {
                        this.work.skill_statics = skills
                            .iter()
                            .map(|s| {
                                let id: &'static str =
                                    Box::leak(format!("skill-{}", s.name).into_boxed_str());
                                let label: &'static str =
                                    Box::leak(s.name.clone().into_boxed_str());
                                (id, label)
                            })
                            .collect();
                        this.work.skills = skills;
                        this.work.toolsets = toolsets;
                    }
                    this.work.hermes_testing = false;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Fetch skills + toolsets into `skills_state` from the dashboard gateway
    /// (`/api/skills`, `/api/tools/toolsets`) — the SAME origin the Work banner's
    /// WebSocket rides, authenticated with the scraped session token. These
    /// routes are NOT on the Hermes REST server that serves jobs/sessions, so the
    /// gateway is used when enabled (see [`skills_rest_client`]). Called on
    /// opening the Skills page so it shares the live connection instead of
    /// rendering a dead default ("0 Skills" beside a banner reporting 100). The
    /// `loading` flag guards against a double-fetch; a failure lands in
    /// `skills_state.error` so the page can distinguish "fetch failed" from
    /// "genuinely zero".
    /// Flip one skill's enabled flag through the dashboard (H2) and refresh
    /// the list so the card shows the server's truth, not an optimistic guess.
    pub fn toggle_skill_enabled(&mut self, name: String, enabled: bool, cx: &mut Context<Self>) {
        if self.skills_state.toggling {
            return;
        }
        self.skills_state.toggling = true;
        cx.notify();
        let gw = self.work.gateway.clone();
        let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);
        std::thread::spawn(move || {
            let result = dashboard_rest_client(&gw.dashboard_url)
                .and_then(|c| c.toggle_skill(&name, enabled).map_err(|e| e.to_string()));
            let _ = tx.send_blocking(result);
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    this.skills_state.toggling = false;
                    match result {
                        Ok(()) => this.refresh_skills(cx),
                        Err(e) => this.skills_state.error = Some(e),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Fetch one Hermes session's transcript for the Sessions browser (I1).
    pub fn fetch_session_transcript(&mut self, sid: String, cx: &mut Context<Self>) {
        if self.sessions_state.transcript_loading
            && self.sessions_state.transcript_request.as_deref() == Some(sid.as_str())
        {
            return;
        }
        self.sessions_state.transcript_generation =
            self.sessions_state.transcript_generation.wrapping_add(1);
        let generation = self.sessions_state.transcript_generation;
        self.sessions_state.transcript_request = Some(sid.clone());
        self.sessions_state.transcript_loading = true;
        self.sessions_state.transcript_error = None;
        cx.notify();
        let gw = self.work.gateway.clone();
        let fetch_sid = sid.clone();
        let (tx, rx) = async_channel::bounded::<Result<serde_json::Value, String>>(1);
        std::thread::spawn(move || {
            let result = dashboard_rest_client(&gw.dashboard_url)
                .and_then(|c| c.session_messages(&fetch_sid).map_err(|e| e.to_string()));
            let _ = tx.send_blocking(result);
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    if this.sessions_state.transcript_generation != generation {
                        return;
                    }
                    this.sessions_state.transcript_loading = false;
                    this.sessions_state.transcript_request = None;
                    // Ignore a fetch that outlived the selection (user went
                    // back or clicked another row meanwhile).
                    if this.sessions_state.selected_id.as_deref() != Some(sid.as_str()) {
                        cx.notify();
                        return;
                    }
                    match result {
                        Ok(v) => {
                            let rows = crate::sessions::parse_session_messages(&v);
                            this.sessions_state.transcript = Some((sid.clone(), rows));
                        }
                        Err(e) => this.sessions_state.transcript_error = Some(e),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn refresh_skills(&mut self, cx: &mut Context<Self>) {
        if self.onboarding.is_active() {
            return;
        }
        // Skills come from the dashboard gateway; the Hermes REST config is only
        // a fallback, so proceed when either transport is configured.
        if (!self.work.gateway.enabled && !self.work.hermes.enabled) || self.skills_state.loading {
            return;
        }
        self.skills_state.loading = true;
        self.skills_state.error = None;
        cx.notify();
        let gw = self.work.gateway.clone();
        let hermes = self.work.hermes.clone();
        type SkillsPayload = (
            Vec<awili_gateway::rest::Skill>,
            Vec<awili_gateway::rest::Toolset>,
        );
        let (tx, rx) = async_channel::bounded::<Result<SkillsPayload, String>>(1);
        if cfg!(test) {
            // SX-FLAKE: a test build never fetches on a real thread. That
            // thread's late wake-up tripped gpui's test scheduler ("Detected
            // activity on thread … not deterministic") in whatever window
            // test was running. A test gets the fetch's failure at once, on
            // the scheduler, which is the outcome it got before (no network).
            let _ = tx.try_send(Err(SKILLS_NOT_FETCHED_IN_TESTS.to_string()));
        } else {
            std::thread::spawn(move || {
                // Skills/toolsets are served by the dashboard gateway (:9119) with the
                // scraped session token, not the Hermes REST server (:8642)/api_key
                // that serves jobs+sessions (user-test 2026-07-01 bug #2).
                let result = skills_rest_client(&gw, &hermes).and_then(|client| {
                    client
                        .list_skills()
                        .and_then(|skills| {
                            client.list_toolsets().map(|toolsets| (skills, toolsets))
                        })
                        .map_err(|e| e.to_string())
                });
                let _ = tx.send_blocking(result);
            });
        }
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    match result {
                        Ok((skills, toolsets)) => {
                            this.skills_state.skills = skills;
                            this.skills_state.toolsets = toolsets;
                            this.skills_state.error = None;
                        }
                        Err(e) => this.skills_state.error = Some(e),
                    }
                    this.skills_state.loading = false;
                    // Skills load async: if the user typed `/` before they
                    // arrived, the composer observer left the palette closed
                    // (zero rows) and won't rerun on this app-only notify — so
                    // reopen it here against the current draft.
                    this.sync_skill_palette(cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Fetch scheduled jobs into `cron_state` from the gateway REST surface.
    /// Same rationale as [`refresh_skills`]: the Cron page must use the live
    /// connection, not a never-populated default.
    pub fn refresh_cron(&mut self, cx: &mut Context<Self>) {
        if !self.work.hermes.enabled || self.cron_state.loading {
            return;
        }
        self.cron_state.loading = true;
        self.cron_state.error = None;
        cx.notify();
        let cfg = self.work.hermes.clone();
        let (tx, rx) = async_channel::bounded::<Result<Vec<ScheduledJob>, String>>(1);
        std::thread::spawn(move || {
            // G1: `/api` lives on the dashboard; hermes origin is the fallback.
            let client = dashboard_rest_client(&cfg.dashboard_url).unwrap_or_else(|_| {
                RestClient::from_base_url(cfg.base_url.clone(), cfg.api_key.clone())
            });
            let _ = tx.send_blocking(client.list_jobs().map_err(|e| e.to_string()));
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    match result {
                        Ok(jobs) => {
                            this.cron_state.set_jobs(jobs);
                            this.cron_state.error = None;
                        }
                        Err(e) => this.cron_state.error = Some(e),
                    }
                    this.cron_state.loading = false;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Fetch the hosted Command Center's live data — system status, usage
    /// rollup, and the Hermes update check — from the gateway on view-open. Same
    /// non-blocking shape as [`refresh_cron`]: the blocking REST calls run on a
    /// worker thread and post back through an async channel. The status call is
    /// primary (its failure sets `error`); the usage and update-check calls are
    /// best-effort (a failure leaves the chart empty / the Update chip hidden
    /// rather than erroring the whole panel).
    pub fn refresh_command_center(&mut self, cx: &mut Context<Self>) {
        if !self.work.hermes.enabled || self.command_center_data.loading {
            return;
        }
        self.command_center_data.loading = true;
        self.command_center_data.error = None;
        cx.notify();
        let cfg = self.work.hermes.clone();
        // How much usage history to roll up, and how many top models/skills to
        // keep — matches the Command Center dashboard's compact bar chart.
        const USAGE_DAYS: u32 = 7;
        const USAGE_TOP_N: usize = 6;
        type CcFetch = (
            Result<awili_gateway::rest::SystemStatus, String>,
            Option<awili_gateway::rest::UsageRollup>,
            Option<awili_gateway::rest::HermesUpdateCheck>,
        );
        let (tx, rx) = async_channel::bounded::<CcFetch>(1);
        std::thread::spawn(move || {
            // G1: `/api` lives on the dashboard; hermes origin is the fallback.
            let client = dashboard_rest_client(&cfg.dashboard_url).unwrap_or_else(|_| {
                RestClient::from_base_url(cfg.base_url.clone(), cfg.api_key.clone())
            });
            let status = client.status().map_err(|e| e.to_string());
            let usage = client
                .usage(USAGE_DAYS)
                .ok()
                .map(|a| awili_gateway::rest::roll_up_usage(&a, USAGE_TOP_N));
            // Best-effort: a gateway that can't check for updates just hides the
            // Update Hermes chip (via `update_affordance(None)`).
            let update = client.check_hermes_update().ok();
            let _ = tx.send_blocking((status, usage, update));
        });
        cx.spawn(async move |this, cx| {
            if let Ok((status, usage, update)) = rx.recv().await {
                this.update(cx, |this, cx| {
                    let d = &mut this.command_center_data;
                    d.loading = false;
                    match status {
                        Ok(s) => {
                            d.status = Some(s);
                            d.error = None;
                        }
                        Err(e) => d.error = Some(e),
                    }
                    // Only replace usage on a successful fetch, so a transient
                    // usage error doesn't blank an already-rendered chart.
                    if usage.is_some() {
                        d.usage = usage;
                    }
                    d.state.set_update_check(update);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// [LIVE] action: restart the gateway (plan unit 21, Phase E). Same
    /// non-blocking worker-thread + async-channel shape as [`cron_job_action`].
    /// The guarded reducer ([`command_center::CommandCenterState::begin_restart`])
    /// decides whether to spawn: `false` means a restart is already in flight, so
    /// a double-click launches nothing. On completion the chip settles
    /// (`finish_restart`) and the panel re-fetches so the status header reflects
    /// the freshly-restarted gateway (a failure settles the chip to `✗` and still
    /// re-reads, which surfaces an unreachable gateway rather than hiding it).
    pub fn restart_gateway_action(&mut self, cx: &mut Context<Self>) {
        if !self.work.hermes.enabled {
            return;
        }
        if !self.command_center_data.state.begin_restart() {
            return;
        }
        cx.notify();
        let cfg = self.work.hermes.clone();
        let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);
        std::thread::spawn(move || {
            // G1: `/api` lives on the dashboard; hermes origin is the fallback.
            let client = dashboard_rest_client(&cfg.dashboard_url).unwrap_or_else(|_| {
                RestClient::from_base_url(cfg.base_url.clone(), cfg.api_key.clone())
            });
            let _ = tx.send_blocking(client.restart_gateway().map_err(|e| e.to_string()));
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    this.command_center_data
                        .state
                        .finish_restart(result.is_ok());
                    // Re-read gateway state after the restart settles (success or
                    // failure); refresh_command_center flips `loading` for its pass.
                    this.refresh_command_center(cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// [LIVE] action: apply an available Hermes update (plan unit 21, Phase E).
    /// Same shape as [`Self::restart_gateway_action`]. The guarded reducer
    /// ([`command_center::CommandCenterState::begin_update`]) only spawns when the
    /// held update check is actionable (an available update the gateway can apply
    /// in place) and not already running, so a click on a hidden/dimmed chip or a
    /// double-click does nothing. On completion the chip settles and the panel
    /// re-fetches (re-running the update check, which clears the Update chip once
    /// the gateway is current); passing `loading` into `begin_update` suppresses
    /// a re-spawn during that post-update refresh, while the chip still reads
    /// actionable from the stale check.
    pub fn update_hermes_action(&mut self, cx: &mut Context<Self>) {
        if !self.work.hermes.enabled {
            return;
        }
        let refreshing = self.command_center_data.loading;
        if !self.command_center_data.state.begin_update(refreshing) {
            return;
        }
        cx.notify();
        let cfg = self.work.hermes.clone();
        let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);
        std::thread::spawn(move || {
            // G1: `/api` lives on the dashboard; hermes origin is the fallback.
            let client = dashboard_rest_client(&cfg.dashboard_url).unwrap_or_else(|_| {
                RestClient::from_base_url(cfg.base_url.clone(), cfg.api_key.clone())
            });
            let _ = tx.send_blocking(client.update_hermes().map_err(|e| e.to_string()));
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    this.command_center_data.state.finish_update(result.is_ok());
                    this.refresh_command_center(cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Render the hosted Command Center panel (plan unit 21, Phase E): the live
    /// system-status header + grid and the usage chart, via
    /// [`crate::command_center::command_center`], reading `command_center_data`.
    /// Loading / error / not-yet-connected states render a plain notice instead.
    ///
    /// The chips are [LIVE]: *Restart Gateway* and (when applicable) *Update
    /// Hermes* attach `cx.listener` handlers into [`Self::restart_gateway_action`]
    /// / [`Self::update_hermes_action`]. Opening the view only *reads* gateway
    /// state (via [`Self::refresh_command_center`]); it takes a deliberate click
    /// to restart or update.
    pub(crate) fn command_center_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let d = &self.command_center_data;
        let body: gpui::AnyElement = if !self.work.hermes.enabled {
            self.command_center_unavailable_notice(cx)
        } else if let Some(status) = &d.status {
            let empty = awili_gateway::rest::UsageRollup::default();
            let rollup = d.usage.as_ref().unwrap_or(&empty);
            let content = div()
                .debug_selector(|| CC_STATUS_SELECTOR.to_string())
                .size_full()
                .child(crate::command_center::command_center(
                    status,
                    rollup,
                    d.state.update_check.as_ref(),
                    d.state.restart_phase,
                    d.state.update_phase,
                    cx.listener(|this, _ev, _w, cx| this.restart_gateway_action(cx)),
                    cx.listener(|this, _ev, _w, cx| this.update_hermes_action(cx)),
                ))
                .into_any_element();
            // RC2-R5M6 (slice review 3775ed08, P2): a refresh that failed
            // after a status was read keeps that status on screen, with the
            // shared notice (the failed connection, Check again, Open
            // Settings) above it.
            if d.error.is_some() {
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(self.command_center_unavailable_notice(cx))
                    .child(content)
                    .into_any_element()
            } else {
                content
            }
        } else if d.error.is_some() {
            // RC2-R5M2: the read failed: the shared Hermes notice, with
            // "Check again" and "Open Settings".
            self.command_center_unavailable_notice(cx)
        } else if d.loading {
            command_center_notice("Loading system status\u{2026}")
        } else {
            command_center_notice("No system status yet.")
        };
        div().flex().flex_col().size_full().child(body)
    }

    /// Fetch the messaging connectors into `messaging_data` for the hosted
    /// Messaging pane (plan unit 24). Same non-blocking shape as
    /// [`refresh_command_center`]: the blocking `GET /api/messaging/platforms`
    /// runs on a worker thread; the snapshot is applied through the pure
    /// [`crate::messaging_pane::MessagingPaneState::apply_snapshot`] (which keeps
    /// the current selection by id). A fetch error is surfaced in
    /// `messaging_data.error`, never swallowed, and never blanks an
    /// already-rendered snapshot.
    pub fn refresh_messaging(&mut self, cx: &mut Context<Self>) {
        if !self.work.hermes.enabled || self.messaging_data.loading {
            return;
        }
        self.messaging_data.loading = true;
        self.messaging_data.error = None;
        cx.notify();
        let cfg = self.work.hermes.clone();
        let (tx, rx) =
            async_channel::bounded::<Result<awili_gateway::rest::MessagingPlatforms, String>>(1);
        std::thread::spawn(move || {
            // G1: `/api` lives on the dashboard; hermes origin is the fallback.
            let client = dashboard_rest_client(&cfg.dashboard_url).unwrap_or_else(|_| {
                RestClient::from_base_url(cfg.base_url.clone(), cfg.api_key.clone())
            });
            let res = client.messaging_platforms().map_err(|e| e.to_string());
            let _ = tx.send_blocking(res);
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    let d = &mut this.messaging_data;
                    d.loading = false;
                    match result {
                        Ok(snap) => {
                            d.state.apply_snapshot(snap);
                            // RVS2B-FIX1: a snapshot can change the selection
                            // (reconciliation); atomically cancel any stale
                            // confirmation and sync the draft in the same
                            // transition — the same path a row click takes.
                            d.sync_draft_to_selection();
                            // A fresh server snapshot may reflect configuration
                            // changed elsewhere; old probe verdicts are stale.
                            d.self_tests.clear();
                            d.loaded = true;
                            d.error = None;
                        }
                        Err(e) => d.error = Some(e),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    // --- messaging controls (plan unit 25) ---------------------------------

    /// [LIVE] action: toggle the given connector's enable/disable state in its
    /// draft. No I/O — the draft is pure; the pending state is applied on
    /// save. No-op when the draft and the selection do not both identify
    /// `connector_id` (RVS2B-FIX1): during another connector's in-flight save
    /// or after a selection change, the click is refused instead of touching a
    /// different draft.
    pub fn toggle_messaging_connector_enabled(
        &mut self,
        connector_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.messaging_data.edit_draft_for(
            connector_id,
            crate::messaging_pane::DraftEdit::ToggleEnabled,
        );
        cx.notify();
    }

    /// [LIVE] action: the Save button. Rather than apply immediately, open a
    /// secret-safe confirmation dialog built from the draft's [`ChangePlan`]
    /// (see [`MessagingData::request_save_confirmation`]). The draft is left
    /// untouched; nothing is written until the user confirms. A second click
    /// while the dialog is up is a no-op (the double-fire guard).
    pub fn request_messaging_save(&mut self, connector_id: &str, cx: &mut Context<Self>) {
        if !self.work.hermes.enabled {
            return;
        }
        // RVS2B-FIX1: guarded by the draft/selection identity — a click on a
        // connector whose draft is not the live one (another save in flight)
        // refuses instead of confirming someone else's draft.
        if self
            .messaging_data
            .edit_draft_for(connector_id, crate::messaging_pane::DraftEdit::RequestSave)
        {
            cx.notify();
        }
    }

    /// [LIVE] action: dismiss the save confirmation dialog. Non-destructive —
    /// the draft and its pending edits survive so the user can keep editing.
    pub fn cancel_messaging_save(&mut self, cx: &mut Context<Self>) {
        self.messaging_data.cancel_save_confirmation();
        cx.notify();
    }

    /// [LIVE] action: confirm and apply the pending draft actions for the
    /// selected connector. Each action is applied sequentially through the
    /// RestClient; credential writes (set/delete env vars) run before the config
    /// toggle so a freshly-configured connector doesn't race an empty credential
    /// set. On success the snapshot is refreshed and the draft is closed; on
    /// failure the draft stays open so the user can retry. Called only from the
    /// confirmation dialog's Confirm button — [`MessagingData::confirm_save`]
    /// enters the in-flight state, guards against a duplicate Confirm, and
    /// returns a [`SaveConfirmation`](crate::messaging_pane::SaveConfirmation)
    /// whose identity (connector id + draft generation) the host uses for the
    /// self-test invalidation and the completion call — never a re-read of
    /// `draft_id()`, so a late completion can only ever settle the draft the
    /// user confirmed.
    pub fn confirm_messaging_save(&mut self, cx: &mut Context<Self>) {
        if !self.work.hermes.enabled || self.messaging_data.saving {
            return;
        }
        // S2(b): the confirmation carries its OWN identity — the connector id
        // and draft generation pinned when the dialog opened. The host uses
        // this identity (never a re-read of `draft_id()`) for self-test
        // invalidation and the completion call, so a save can only ever
        // settle the draft the user confirmed.
        let Some(conf) = self.messaging_data.confirm_save() else {
            return;
        };
        let connector_id = conf.connector_id.clone();
        let actions = conf.actions.clone();
        cx.notify();
        let cfg = self.work.hermes.clone();
        let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);
        std::thread::spawn(move || {
            // G1: `/api` lives on the dashboard; hermes origin is the fallback.
            let client = dashboard_rest_client(&cfg.dashboard_url).unwrap_or_else(|_| {
                RestClient::from_base_url(cfg.base_url.clone(), cfg.api_key.clone())
            });
            for act in &actions {
                use crate::messaging_controls::ConnectorAction as A;
                let result = match act {
                    A::SetEnv { key, value } => client.set_env_var(key, value),
                    A::ClearEnv { key } => client.delete_env_var(key),
                    A::UpdateConfig { id, body } => client.update_messaging(id, body),
                };
                if let Err(e) = result {
                    let _ = tx.send_blocking(Err(e.to_string()));
                    return;
                }
            }
            let _ = tx.send_blocking(Ok(()));
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    match result {
                        Ok(()) => {
                            // Identity-gated: applies only if the live draft
                            // still matches the confirmed {id, generation}.
                            let matched = this.messaging_data.finish_save_success(&conf);
                            // Invalidate the self-test only when THIS save
                            // settled the draft (a stale completion refreshed
                            // the list instead — the test is still valid).
                            if matched {
                                this.messaging_data.self_tests.invalidate(&connector_id);
                            }
                            // Refresh the snapshot so the detail reflects the
                            // just-applied changes (a stale completion may
                            // still refresh; it just cannot clear the draft).
                            this.refresh_messaging(cx);
                        }
                        Err(e) => {
                            this.messaging_data.finish_save_error(&conf, e);
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Run the selected connector's live self-test. The generation token keeps
    /// late responses from repainting a newer result, and the pure reducer
    /// strips free-text transport details before anything reaches the UI.
    pub fn test_messaging_action(&mut self, cx: &mut Context<Self>) {
        if !self.work.hermes.enabled {
            return;
        }
        let Some(id) = self.messaging_data.state.test_target().map(str::to_owned) else {
            return;
        };
        if self.messaging_data.self_tests.state(&id).is_running() {
            return;
        }
        let token = self.messaging_data.self_tests.begin(id.clone());
        cx.notify();
        let cfg = self.work.hermes.clone();
        let (tx, rx) = async_channel::bounded(1);
        let worker_id = id.clone();
        std::thread::spawn(move || {
            // G1: `/api` lives on the dashboard; hermes origin is the fallback.
            let client = dashboard_rest_client(&cfg.dashboard_url).unwrap_or_else(|_| {
                RestClient::from_base_url(cfg.base_url.clone(), cfg.api_key.clone())
            });
            let _ = tx.send_blocking(client.test_messaging(&worker_id));
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    this.messaging_data.self_tests.record(&id, token, result);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// [LIVE] action: begin editing a credential value inline. Creates or
    /// focuses the credential editor Composer. No-op when no draft is open or
    /// an edit is already in progress.
    pub fn begin_messaging_credential_edit(
        &mut self,
        connector_id: &str,
        key: &str,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let known_key = self
            .messaging_data
            .state
            .selected_env_vars()
            .iter()
            .any(|env| env.key == key);
        if !known_key {
            return;
        }
        // RVS2B-FIX1: identity from the CLICKED connector (the button lives in
        // that connector's rendered detail) — refused when the live draft does
        // not belong to it (e.g. another connector's save is in flight).
        if !self.messaging_data.edit_draft_for(
            connector_id,
            crate::messaging_pane::DraftEdit::BeginCredential(key),
        ) {
            return;
        }
        // Focus the editor and clear any previous value (never prefill secrets).
        self.messaging_credential_editor.update(cx, |c, cx| {
            c.set_text("", cx);
        });
        let h = self
            .messaging_credential_editor
            .read(cx)
            .focus_handle()
            .clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// [LIVE] action: commit the typed credential value from the editor into
    /// the draft. The value is checked by [`ConnectorDraft::set_env`] — a
    /// redaction placeholder or empty value is rejected and the edit stays open.
    pub fn commit_messaging_credential_edit(&mut self, cx: &mut Context<Self>) {
        let value = self
            .messaging_credential_editor
            .update(cx, |c, _cx| c.take());
        // RVS2B-FIX1: the editor subscription (main.rs) carries no connector
        // id — the identity comes from the SELECTION. With nothing selected
        // there is no draft to commit into: discard the typed text, as before.
        let Some(connector_id) = self.messaging_data.state.selected_id().map(str::to_owned) else {
            return;
        };
        // On reject the edit stays open (commit_credential_edit keeps
        // `editing_key` set); the inline editor retains focus from the Enter
        // press, so no explicit re-focus is needed here (this runs from a
        // `cx.subscribe` handler that has no `Window` in scope). The guarded
        // entry refuses the commit when the live draft does not belong to the
        // selection (another connector's save in flight), so the text is
        // simply discarded instead of landing in someone else's draft.
        self.messaging_data.edit_draft_for(
            &connector_id,
            crate::messaging_pane::DraftEdit::CommitCredential(&value),
        );
        cx.notify();
    }

    /// [LIVE] action: cancel the inline credential edit without saving.
    pub fn cancel_messaging_credential_edit(&mut self, cx: &mut Context<Self>) {
        self.messaging_data.cancel_credential_edit();
        self.messaging_credential_editor.update(cx, |c, _cx| {
            c.take();
        });
        cx.notify();
    }

    /// Render the hosted Messaging pane (plan unit 24): the connector health
    /// summary + a list/detail split over the fetched connectors, via the pure
    /// [`crate::messaging_pane::MessagingPaneState`]. Rows are click-selectable
    /// (a read-only selection change); the Test / Update actions render as
    /// static deferred [LIVE] chips, so opening the view only *reads* connector
    /// state — it can never trigger a connector self-test unattended.
    /// Loading / error / not-yet-connected / empty states render a plain notice.
    ///
    /// The detail view now includes interactive controls (plan unit 25):
    /// an enable/disable toggle, per-credential edit/clear/revert buttons, and a
    /// Save button — all wired through the [`ConnectorDraft`] engine which
    /// enforces the secret-safety invariant (never round-trip a redacted mask).
    /// The exact read-only notice shown on a connector detail whose live
    /// draft belongs to another connector (whose save is still in flight).
    /// Pinned by the `readonly_connector_detail_renders_label_without_plan`
    /// test through the painted frame.
    const MESSAGING_READONLY_LABEL: &str = "Read-only while another connector's save finishes.";

    pub(crate) fn messaging_panel(
        &mut self,
        window: &gpui::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let notice = self
            .hermes_page_notice(crate::hermes_pages::HermesPage::Messaging)
            .map(|text| {
                self.hermes_page_notice_element(
                    crate::hermes_pages::HermesPage::Messaging,
                    text,
                    cx,
                )
            });
        let d = &self.messaging_data;
        let retained_notice = match notice {
            Some(notice) if !d.loaded || !self.work.hermes.enabled => {
                return div().flex().flex_col().size_full().child(notice);
            }
            other => other,
        };
        if !d.loaded {
            if let Some(notice) = retained_notice {
                return div().flex().flex_col().size_full().child(notice);
            }
            let message = if let Some(error) = &d.error {
                crate::hermes_pages::plain_read_failure(&self.work.hermes.base_url, error)
            } else if d.loading {
                "Loading messaging connectors…".to_string()
            } else {
                "No messaging connectors yet.".to_string()
            };
            return div().size_full().child(crate::ui::EmptyState::new(
                "msg-loading-state",
                if d.loading {
                    "Loading connections"
                } else {
                    "Connections unavailable"
                },
                message,
            ));
        }

        let rows = d.state.visible_rows();
        let selected_index = d.state.selected_index();
        let summary = messaging_pane::summary_line(&d.state.health_summary());
        let refreshing = d.loading;
        let refresh_button = if refreshing {
            crate::ui::Button::new("msg-refresh", "Refreshing…")
                .selector("msg-refresh")
                .disabled("A connector refresh is already running")
        } else {
            crate::ui::Button::new("msg-refresh", "Refresh")
                .selector("msg-refresh")
                .on_click(cx.listener(|this, _ev, _window, cx| this.refresh_messaging(cx)))
        };
        let header = div()
            .debug_selector(|| MESSAGING_HEADER_SELECTOR.to_string())
            .child(
                crate::ui::Card::new("msg-page-header")
                    .title("Messaging")
                    .children([div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap(theme::z(theme::SP_8))
                        .child(crate::ui::text::caption(summary))
                        .child(refresh_button)]),
            );

        let mut list_rows = Vec::new();
        for (ix, row) in rows.iter().enumerate() {
            let id = row.id.clone();
            let tone = match row.health {
                messaging_pane::ConnectorHealth::Running
                | messaging_pane::ConnectorHealth::Ready => theme::Tone::Neutral,
                messaging_pane::ConnectorHealth::NeedsSetup => theme::Tone::Attention,
                messaging_pane::ConnectorHealth::Error => theme::Tone::ActNow,
                messaging_pane::ConnectorHealth::Disabled => theme::Tone::Neutral,
            };
            list_rows.push(
                div()
                    .debug_selector(move || format!("msg-connector-row-{ix}"))
                    .child(
                        crate::ui::Row::new(("messaging-row", ix))
                            .title(row.name.clone())
                            .subtitle(row.health.label())
                            .leading(
                                crate::ui::Dot::new(tone)
                                    .id(("msg-health", ix))
                                    .label(format!("{}: {}", row.name, row.health.label())),
                            )
                            .selected(selected_index == Some(ix))
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                if this.messaging_data.state.selected_id() != Some(id.as_str()) {
                                    this.shell
                                        .messaging_env_scroll
                                        .set_offset(Default::default());
                                }
                                this.messaging_data.select_id(&id);
                                cx.notify();
                            })),
                    ),
            );
        }
        let connector_list = if list_rows.is_empty() {
            crate::ui::EmptyState::new(
                "msg-no-connectors",
                "No connections configured",
                "Messaging apps and services connected through Hermes will appear here.",
            )
            .into_any_element()
        } else {
            div()
                .id("msg-connector-list-scroll")
                .debug_selector(|| "msg-connector-list-scroll".to_string())
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_4))
                .max_h(theme::z(theme::BACKSTAGE_MESSAGING_LIST_MAX_H))
                .track_scroll(&self.shell.messaging_connectors_scroll)
                .overflow_hidden()
                .on_scroll_wheel(cx.listener(|this, event, window, cx| {
                    if crate::consolidation::scroll_backstage_list(
                        &this.shell.messaging_connectors_scroll,
                        event,
                        window,
                    ) {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .children(list_rows)
                .into_any_element()
        };
        let list_section = crate::ui::Section::new("msg-connections-section", "Connections")
            .open(true)
            .body([connector_list]);

        let selected_platform = d.state.selected_platform().cloned();
        let selected_env_vars = d.state.selected_env_vars();
        let test_id = d.state.test_target().map(str::to_owned);
        let test_state = test_id
            .as_deref()
            .map(|id| d.self_tests.state(id))
            .unwrap_or(SelfTest::Untested);
        let saving = d.saving;
        let save_error = d.save_error.clone();
        let save_success = d.save_success;
        let editing_key = d.editing_key.clone();
        let refresh_error = d.error.clone();
        let mut action_reveal = Vec::new();
        let compact = crate::ui::is_compact(window);
        let detail: gpui::AnyElement = match selected_platform {
            Some(ref platform) => {
                self.messaging_data.open_draft(Some(platform));
                let editable = self.messaging_data.draft_matches(&platform.id);
                let enabled = self.messaging_data.draft_enabled(Some(platform));
                let mut env_rows = Vec::new();
                for (ix, env) in selected_env_vars.iter().enumerate() {
                    let key = env.key.clone();
                    let editing =
                        editable && editing_key.as_ref().is_some_and(|editing| editing == &key);
                    let edit = self.messaging_data.draft_env_edit_for(&platform.id, &key);
                    let value = match edit {
                        Some(crate::messaging_controls::EnvEdit::Set(_)) if env.is_password => {
                            "→ •••••••• (pending)".to_string()
                        }
                        Some(crate::messaging_controls::EnvEdit::Set(value)) => {
                            format!("→ {value}")
                        }
                        Some(crate::messaging_controls::EnvEdit::Clear) => {
                            "(not set) — pending delete".to_string()
                        }
                        None => env.display_value.clone(),
                    };
                    let mut actions = Vec::new();
                    if !editing && !saving && editable {
                        let cid = platform.id.clone();
                        let edit_key = key.clone();
                        let edit_focus = self
                            .shell
                            .messaging_action_focus
                            .entry(format!("edit:{}:{key}", platform.id))
                            .or_insert_with(|| cx.focus_handle())
                            .clone();
                        let edit_bounds = self
                            .shell
                            .messaging_action_bounds
                            .entry(format!("edit:{}:{key}", platform.id))
                            .or_insert_with(|| std::rc::Rc::new(std::cell::Cell::new(None)))
                            .clone();
                        action_reveal.push((edit_focus.clone(), edit_bounds.clone()));
                        let selector = format!("msg-env-edit-{key}");
                        actions.push(
                            div().debug_selector(move || selector.clone()).child(
                                crate::ui::Button::new(format!("msg-env-edit-{key}"), "Edit")
                                    .quiet()
                                    .focus_handle(&edit_focus)
                                    .bounds_handle(edit_bounds)
                                    .on_click(cx.listener(move |this, _ev, window, cx| {
                                        this.begin_messaging_credential_edit(
                                            &cid, &edit_key, window, cx,
                                        )
                                    })),
                            ),
                        );
                        let cid = platform.id.clone();
                        let clear_key = key.clone();
                        let clear_focus = self
                            .shell
                            .messaging_action_focus
                            .entry(format!("clear:{}:{key}", platform.id))
                            .or_insert_with(|| cx.focus_handle())
                            .clone();
                        let clear_bounds = self
                            .shell
                            .messaging_action_bounds
                            .entry(format!("clear:{}:{key}", platform.id))
                            .or_insert_with(|| std::rc::Rc::new(std::cell::Cell::new(None)))
                            .clone();
                        action_reveal.push((clear_focus.clone(), clear_bounds.clone()));
                        let selector = format!("msg-env-clear-{key}");
                        actions.push(
                            div().debug_selector(move || selector.clone()).child(
                                crate::ui::Button::new(format!("msg-env-clear-{key}"), "Clear")
                                    .quiet()
                                    .focus_handle(&clear_focus)
                                    .bounds_handle(clear_bounds)
                                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                                        this.messaging_data.edit_draft_for(
                                            &cid,
                                            crate::messaging_pane::DraftEdit::ClearEnv(&clear_key),
                                        );
                                        cx.notify();
                                    })),
                            ),
                        );
                    }
                    if edit.is_some() && !editing && editable {
                        let cid = platform.id.clone();
                        let revert_key = key.clone();
                        let selector = format!("msg-env-revert-{key}");
                        actions.push(
                            div().debug_selector(move || selector.clone()).child(
                                crate::ui::Button::new(format!("msg-env-revert-{key}"), "Revert")
                                    .quiet()
                                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                                        this.messaging_data.edit_draft_for(
                                            &cid,
                                            crate::messaging_pane::DraftEdit::RevertEnv(
                                                &revert_key,
                                            ),
                                        );
                                        cx.notify();
                                    })),
                            ),
                        );
                    }
                    let trailing = div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .items_center()
                        .gap(theme::z(theme::SP_4))
                        .when(compact, |d| d.w_full().max_w_full())
                        .when(editing, |d| {
                            d.child(self.messaging_credential_editor.clone())
                        })
                        .children(actions);
                    env_rows.push(
                        crate::ui::Row::new(("msg-env-row", ix))
                            .title(key)
                            .subtitle(if editing {
                                "Enter to apply this value · Esc to cancel".to_string()
                            } else {
                                value
                            })
                            .wrap_trailing()
                            .trailing(trailing),
                    );
                }
                let env_content: gpui::AnyElement = if env_rows.is_empty() {
                    crate::ui::EmptyState::new(
                        "msg-no-env",
                        "No configurable environment",
                        "This connector has no editable settings.",
                    )
                    .into_any_element()
                } else {
                    let inner_scroll = self.shell.messaging_env_scroll.clone();
                    let outer_scroll = self.shell.backstage_scroll.clone();
                    let reveal_key = self.shell.messaging_reveal_key.clone();
                    div()
                        .on_children_prepainted(move |_, window, cx| {
                            let Some((focus, bounds)) = action_reveal
                                .iter()
                                .find(|(focus, _)| focus.is_focused(window))
                            else {
                                *reveal_key.borrow_mut() = None;
                                return;
                            };
                            let Some(face) = bounds.get() else {
                                *reveal_key.borrow_mut() = None;
                                return;
                            };
                            let inner_viewport = inner_scroll.bounds();
                            let outer_viewport = outer_scroll.bounds();
                            let inner_offset = inner_scroll.offset().y;
                            let outer_offset = outer_scroll.offset().y;
                            let key = crate::consolidation::MessagingRevealKey {
                                focus: focus.clone(),
                                face: gpui::Bounds::new(
                                    gpui::point(
                                        face.origin.x,
                                        face.origin.y - inner_offset - outer_offset,
                                    ),
                                    face.size,
                                ),
                                inner: gpui::Bounds::new(
                                    gpui::point(
                                        inner_viewport.origin.x,
                                        inner_viewport.origin.y - outer_offset,
                                    ),
                                    inner_viewport.size,
                                ),
                                body: outer_viewport,
                            };
                            let changed = {
                                let mut previous = reveal_key.borrow_mut();
                                if previous.as_ref() == Some(&key) {
                                    false
                                } else {
                                    *previous = Some(key.clone());
                                    true
                                }
                            };
                            if !changed {
                                return;
                            }

                            let reveal_key = reveal_key.clone();
                            let focus = focus.clone();
                            let bounds = bounds.clone();
                            let inner_scroll = inner_scroll.clone();
                            let outer_scroll = outer_scroll.clone();
                            window.defer(cx, move |window, _cx| {
                                if reveal_key.borrow().as_ref() != Some(&key)
                                    || !focus.is_focused(window)
                                {
                                    return;
                                }
                                let Some(mut face) = bounds.get() else {
                                    return;
                                };
                                let mut inner_offset = inner_scroll.offset();
                                let inner_before = inner_offset;
                                let inner_viewport = inner_scroll.bounds();
                                if face.top() < inner_viewport.top() {
                                    inner_offset.y += inner_viewport.top() - face.top();
                                } else if face.bottom() > inner_viewport.bottom() {
                                    inner_offset.y += inner_viewport.bottom() - face.bottom();
                                }
                                inner_offset.y = inner_offset
                                    .y
                                    .clamp(-inner_scroll.max_offset().y, gpui::Pixels::ZERO);
                                face.origin.y += inner_offset.y - inner_before.y;

                                let mut outer_offset = outer_scroll.offset();
                                let outer_before = outer_offset;
                                let outer_viewport = outer_scroll.bounds();
                                if face.top() < outer_viewport.top() {
                                    outer_offset.y += outer_viewport.top() - face.top();
                                } else if face.bottom() > outer_viewport.bottom() {
                                    outer_offset.y += outer_viewport.bottom() - face.bottom();
                                }
                                outer_offset.y = outer_offset
                                    .y
                                    .clamp(-outer_scroll.max_offset().y, gpui::Pixels::ZERO);

                                let changed =
                                    inner_offset != inner_before || outer_offset != outer_before;
                                if inner_offset != inner_before {
                                    inner_scroll.set_offset(inner_offset);
                                }
                                if outer_offset != outer_before {
                                    outer_scroll.set_offset(outer_offset);
                                }
                                if changed {
                                    window.refresh();
                                }
                            });
                        })
                        .id("msg-env-scroll")
                        .debug_selector(|| "msg-env-scroll".to_string())
                        .flex()
                        .flex_col()
                        .gap(theme::z(theme::SP_4))
                        .max_h(theme::z(theme::BACKSTAGE_MESSAGING_ENV_MAX_H))
                        .track_scroll(&self.shell.messaging_env_scroll)
                        .overflow_hidden()
                        .on_scroll_wheel(cx.listener(|this, event, window, cx| {
                            if crate::consolidation::scroll_backstage_list(
                                &this.shell.messaging_env_scroll,
                                event,
                                window,
                            ) {
                                cx.stop_propagation();
                                cx.notify();
                            }
                        }))
                        .children(env_rows)
                        .into_any_element()
                };
                let toggle_connector_id = platform.id.clone();
                let toggle = if editable {
                    crate::ui::Toggle::new("msg-connector-toggle", enabled)
                        .selector("msg-connector-toggle")
                        .label("Enabled")
                        .on_toggle(cx.listener(move |this, _ev, _window, cx| {
                            this.toggle_messaging_connector_enabled(&toggle_connector_id, cx)
                        }))
                } else {
                    crate::ui::Toggle::new("msg-connector-toggle", enabled)
                        .selector("msg-connector-toggle")
                        .label("Enabled")
                        .disabled("Read-only while another connector's save finishes")
                };
                let save_enabled = self.messaging_data.save_enabled();
                let confirming = editable && self.messaging_data.is_confirming();
                let mut controls = div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(theme::z(theme::SP_8))
                    .when(compact, |d| d.w_full().max_w_full())
                    .child(toggle);
                if editable && !confirming && (save_enabled || saving) {
                    let save_button = if saving {
                        crate::ui::Button::new("msg-save", "Saving…")
                            .selector("msg-save")
                            .disabled("A connector change is being saved")
                    } else {
                        let cid = platform.id.clone();
                        crate::ui::Button::new("msg-save", "Save changes")
                            .selector("msg-save")
                            .primary()
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                this.request_messaging_save(&cid, cx)
                            }))
                    };
                    let save_button = if compact {
                        save_button.full_width()
                    } else {
                        save_button
                    };
                    controls = controls.child(save_button);
                }
                let test_button = match (&test_id, &test_state) {
                    (Some(_), SelfTest::Running) => crate::ui::Button::new("msg-test", "Testing…")
                        .selector("msg-test")
                        .disabled("This connector test is already running"),
                    (Some(_), _) => crate::ui::Button::new(
                        "msg-test",
                        match &test_state {
                            SelfTest::Untested => "Test connector",
                            SelfTest::Running => "Testing…",
                            SelfTest::Passed => "Test passed",
                            SelfTest::Failed { .. } => "Test failed",
                        },
                    )
                    .selector("msg-test")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.test_messaging_action(cx))),
                    (None, _) => crate::ui::Button::new("msg-test", "Test unavailable")
                        .selector("msg-test")
                        .disabled("This connector is not eligible for a self-test"),
                };
                let test_button = if compact {
                    test_button.full_width()
                } else {
                    test_button
                };
                controls = controls.child(test_button);

                let mut content = vec![
                    crate::ui::Row::new("msg-enabled-row")
                        .title("Connector access")
                        .subtitle(if enabled {
                            "Enabled for Hermes"
                        } else {
                            "Disabled for Hermes"
                        })
                        .wrap_trailing()
                        .trailing(controls)
                        .into_any_element(),
                    env_content,
                ];
                if let Some((headline, lines)) = self
                    .messaging_data
                    .draft_validation()
                    .filter(|v| v.blocks_enable())
                    .map(|v| (v.headline(), v.lines()))
                {
                    content.push(
                        crate::ui::Banner::new(
                            "msg-validation",
                            theme::Tone::Attention,
                            format!("{headline}: {}", lines.join(" ")),
                        )
                        .into_any_element(),
                    );
                }
                if let Some(error) = save_error {
                    content.push(
                        crate::ui::Banner::new(
                            "msg-save-error",
                            theme::Tone::ActNow,
                            format!("Save failed: {error}"),
                        )
                        .into_any_element(),
                    );
                }
                if save_success {
                    content.push(
                        crate::ui::Banner::new("msg-save-success", theme::Tone::Attention, "Saved")
                            .into_any_element(),
                    );
                }
                if let SelfTest::Failed { kind, detail } = &test_state {
                    content.push(
                        crate::ui::Banner::new(
                            "msg-test-error",
                            theme::Tone::ActNow,
                            format!("{}: {detail}", self_test_failure_copy(*kind)),
                        )
                        .into_any_element(),
                    );
                }
                if !editable {
                    content.push(
                        crate::ui::Banner::new(
                            "msg-readonly-label",
                            theme::Tone::Neutral,
                            Self::MESSAGING_READONLY_LABEL,
                        )
                        .selector("msg-readonly-label")
                        .into_any_element(),
                    );
                }
                if confirming {
                    if let Some(plan) = self.messaging_data.confirm_plan().cloned() {
                        let title = self
                            .messaging_data
                            .confirm_name()
                            .map(|name| format!("Apply {name} — {}", plan.headline()))
                            .unwrap_or_else(|| format!("Apply {}?", plan.headline()));
                        let confirm = crate::ui::Button::new("msg-confirm-save", "Confirm")
                            .selector("msg-confirm-save")
                            .primary()
                            .on_click(cx.listener(|this, _ev, _window, cx| {
                                this.confirm_messaging_save(cx)
                            }));
                        let cancel =
                            crate::ui::Button::new("msg-cancel-save", "Cancel")
                                .selector("msg-cancel-save")
                                .ghost()
                                .on_click(cx.listener(|this, _ev, _window, cx| {
                                    this.cancel_messaging_save(cx)
                                }));
                        content.push(
                            crate::ui::Card::new("msg-confirm-plan")
                                .selector("msg-confirm-plan")
                                .title(title)
                                .children(plan.lines().into_iter().map(crate::ui::text::read))
                                .actions([cancel, confirm])
                                .into_any_element(),
                        );
                    }
                }
                crate::ui::Card::new("msg-connector-detail")
                    .title(platform.name.clone())
                    .body(format!(
                        "{} · {}",
                        platform.description,
                        crate::messaging_pane::ConnectorHealth::of(platform).label()
                    ))
                    .children(content)
                    .into_any_element()
            }
            None => crate::ui::EmptyState::new(
                "msg-select-connector",
                "Select a connection",
                "Choose a connector to review its health and settings.",
            )
            .into_any_element(),
        };
        let body: gpui::AnyElement = if rows.is_empty() {
            crate::ui::EmptyState::new(
                "msg-no-connectors",
                "No connections configured",
                "Messaging apps and services connected through Hermes will appear here.",
            )
            .into_any_element()
        } else {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_start()
                .gap(theme::z(theme::SP_12))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .w(theme::z(theme::BACKSTAGE_MESSAGING_LIST_W))
                        .min_w_0()
                        .max_w_full()
                        .flex_initial()
                        .child(list_section),
                )
                .child(
                    div()
                        .w(theme::z(theme::BACKSTAGE_MESSAGING_DETAIL_W))
                        .flex_auto()
                        .min_w_0()
                        .max_w_full()
                        .child(detail),
                )
                .into_any_element()
        };
        let mut panel = div()
            .flex()
            .flex_col()
            .gap(theme::z(theme::SP_12))
            .p(theme::z(theme::SP_16))
            .w_full()
            .min_h_full()
            .child(header);
        if let Some(notice) = retained_notice {
            panel = panel.child(notice);
        } else if let Some(error) = refresh_error {
            panel = panel.child(
                crate::ui::Banner::new(
                    "msg-refresh-error",
                    theme::Tone::Attention,
                    format!("Refresh failed: {error}"),
                )
                .selector("msg-refresh-error"),
            );
        }
        panel.child(body)
    }

    /// Pause / resume / run-now a single scheduled job, then re-fetch the list so
    /// the row reflects the new state. Same async, non-blocking shape as
    /// [`refresh_cron`]: the blocking gateway REST call runs on a worker thread. On
    /// failure (e.g. the server denies the action when `jobs_admin` is off) the
    /// error is surfaced in `cron_state.error` rather than swallowed.
    pub fn cron_job_action(
        &mut self,
        job_id: String,
        action: cron::CronAction,
        cx: &mut Context<Self>,
    ) {
        if !self.work.hermes.enabled || self.cron_state.loading {
            return;
        }
        self.cron_state.loading = true;
        self.cron_state.error = None;
        cx.notify();
        let cfg = self.work.hermes.clone();
        let req_id = job_id.clone();
        let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);
        std::thread::spawn(move || {
            // G1: `/api` lives on the dashboard; hermes origin is the fallback.
            let client = dashboard_rest_client(&cfg.dashboard_url).unwrap_or_else(|_| {
                RestClient::from_base_url(cfg.base_url.clone(), cfg.api_key.clone())
            });
            let res = match action {
                cron::CronAction::Pause => client.pause_job(&req_id),
                cron::CronAction::Resume => client.resume_job(&req_id),
                cron::CronAction::Trigger => client.trigger_job(&req_id),
            };
            let _ = tx.send_blocking(res.map_err(|e| e.to_string()));
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    this.cron_state.loading = false;
                    match result {
                        Ok(()) => {
                            this.cron_state.error = None;
                            match action {
                                // Pause: the server stops listing this job, so a
                                // bare refetch would drop the row and strand the
                                // Resume action. Stash a paused copy in the overlay
                                // first; set_jobs re-merges it after the refetch.
                                cron::CronAction::Pause => {
                                    if let Some(mut j) = this
                                        .cron_state
                                        .jobs
                                        .iter()
                                        .find(|j| j.id == job_id)
                                        .cloned()
                                    {
                                        j.state = "paused".to_string();
                                        this.cron_state.paused_overlay.insert(job_id.clone(), j);
                                    }
                                }
                                // Resume: clear the overlay; the job reappears in
                                // the server listing as active on the refetch.
                                cron::CronAction::Resume => {
                                    this.cron_state.paused_overlay.remove(&job_id);
                                }
                                cron::CronAction::Trigger => {}
                            }
                            // refresh_cron flips `loading` back on for its own pass.
                            this.refresh_cron(cx);
                            // The Work-panel scheduled card reads a separate
                            // `work.jobs` store that set_mode only refetches when
                            // empty; refresh it too so it can't show stale state
                            // after an action taken from the Cron panel.
                            this.refresh_jobs(cx);
                        }
                        Err(e) => {
                            this.cron_state.error = Some(e);
                            cx.notify();
                        }
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Fetch remote Hermes sessions into `sessions_state` (the cross-cutting
    /// Sessions browser). Same view-open rationale as [`refresh_skills`]: without
    /// this the Sessions page's Hermes/All tabs render empty even though remote
    /// sessions exist. (`work.hermes_sessions`, populated by [`refresh_sessions`],
    /// is a separate store backing the Work-panel sessions card.)
    pub fn refresh_sessions_panel(&mut self, cx: &mut Context<Self>) {
        if !self.work.hermes.enabled || self.sessions_state.loading {
            return;
        }
        self.sessions_state.loading = true;
        self.sessions_state.error = None;
        cx.notify();
        let cfg = self.work.hermes.clone();
        let (tx, rx) = async_channel::bounded::<Result<Vec<HermesSession>, String>>(1);
        std::thread::spawn(move || {
            // G1: `/api` lives on the dashboard; hermes origin is the fallback.
            let client = dashboard_rest_client(&cfg.dashboard_url).unwrap_or_else(|_| {
                RestClient::from_base_url(cfg.base_url.clone(), cfg.api_key.clone())
            });
            let _ = tx.send_blocking(client.list_sessions().map_err(|e| e.to_string()));
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update(cx, |this, cx| {
                    match result {
                        Ok(sessions) => {
                            this.sessions_state.sessions = sessions;
                            this.sessions_state.error = None;
                        }
                        Err(e) => this.sessions_state.error = Some(e),
                    }
                    this.sessions_state.loading = false;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn work_connection_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::ui::{text, Button, Card};
        let tool_count: usize = self.work.toolsets.iter().map(|t| t.tools.len()).sum();
        let label = connection_label(
            self.work.hermes.enabled,
            &self.work.hermes.base_url,
            &self.work.gateway.dashboard_url,
            self.work.skills.len(),
            self.work.toolsets.len(),
            tool_count,
        );
        // Show gateway health only where this executor needs it or Test was pressed.
        let gateway_here =
            matches!(self.work.executor, WorkExecutor::Gateway) || self.work.health.test_requested;
        let status = if self.work.gateway.enabled {
            gateway_here.then(|| {
                let (label, tone) = self.gateway_status_line();
                let text = text::ui_text(label)
                    .text_color_override(tone.color())
                    .wrap();
                div()
                    .debug_selector(|| crate::gateway_health_app::GATEWAY_LINE_SELECTOR.to_string())
                    .child(text)
                    .into_any_element()
            })
        } else if self.work.hermes_testing {
            Some(
                text::ui_text("Testing connection…")
                    .dim()
                    .wrap()
                    .into_any_element(),
            )
        } else {
            self.work.hermes_status.as_ref().map(|status| match status {
                Ok(label) => text::ui_text(SharedString::from(label.clone()))
                    .dim()
                    .wrap()
                    .into_any_element(),
                Err(label) => text::ui_text(SharedString::from(label.clone()))
                    .tone(theme::Tone::ActNow)
                    .wrap()
                    .into_any_element(),
            })
        };
        let connectors = self
            .work
            .connectors
            .server_config(&self.work.agents)
            .map(|_| {
                text::ui_text(SharedString::from(format!(
                    "Connectors: {}",
                    self.work.connectors.active_families().join(", ")
                )))
                .dim()
                .wrap()
                .into_any_element()
            });
        Card::new("job-connection")
            .title("Connection")
            .body(SharedString::from(label))
            .children(connectors.into_iter().chain(status))
            .actions(vec![
                Button::new("test-hermes", "Test")
                    .selector(crate::gateway_health_app::TEST_BUTTON_SELECTOR)
                    .on_click(cx.listener(|this, _, _, cx| this.test_connection_pressed(cx)))
                    .into_any_element(),
                Button::new("edit-config", "Edit config")
                    .on_click(cx.listener(|_, _, _, _| {
                        if let Some(path) = awili_config::Config::default_path() {
                            #[cfg(windows)]
                            let _ = std::process::Command::new(crate::agent_bin::system_tool(
                                "notepad",
                            ))
                            .arg(&path)
                            .spawn();
                            #[cfg(not(windows))]
                            let _ = std::process::Command::new("open").arg(&path).spawn();
                        }
                    }))
                    .into_any_element(),
            ])
    }

    /// RC3-S4: the rules/doc parts of the last run in this conversation, one
    /// plain sentence each; `None` before any run has recorded them.
    pub(crate) fn work_rule_lines(&self) -> Option<Vec<(String, bool)>> {
        let (owner, parts) = self.work.context_parts.as_ref()?;
        (owner == self.sidebar.active_id() && !parts.is_empty()).then(|| {
            parts
                .iter()
                .map(|p| (p.sentence(), p.status.is_incomplete()))
                .collect()
        })
    }

    /// rc.4 S7: the sandbox line for the last agent run in this conversation
    /// ("Sandbox: on, writes limited to <folder>" or "Sandbox: off"); `None`
    /// before any run.
    pub(crate) fn work_sandbox_line(&self) -> Option<String> {
        let (owner, line) = self.work.sandbox_note.as_ref()?;
        (owner == self.sidebar.active_id()).then(|| line.clone())
    }

    fn work_rules_card(&self) -> Option<impl IntoElement> {
        use crate::ui::{Row, Section};
        let lines = self.work_rule_lines()?;
        let rows: Vec<gpui::AnyElement> = lines
            .into_iter()
            .enumerate()
            .map(|(i, (line, bad))| {
                let row = Row::new(SharedString::from(format!("job-rule-{i}")))
                    .title(line)
                    .wrap();
                if bad {
                    row.subtitle("Not all of it reached the model")
                } else {
                    row
                }
                .into_any_element()
            })
            .collect();
        Some(
            Section::new("job-rules", "Instruction files")
                .open(true)
                .body(rows),
        )
    }

    fn work_skills_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::ui::{text, Row, Section};
        let rows: Vec<gpui::AnyElement> = self
            .work
            .skill_statics
            .iter()
            .map(|(id, label)| {
                let prompt = format!("Use the {label} skill to ");
                Row::new(*id)
                    .title(*label)
                    .subtitle("Add to your message")
                    .wrap()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.composer
                            .update(cx, |composer, cx| composer.set_text(&prompt, cx));
                        let focus = this.composer.read(cx).focus_handle().clone();
                        window.focus(&focus, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        let body = div().flex().flex_col().min_w_0().children(rows).when(
            self.work.skill_statics.is_empty(),
            |body| {
                body.child(
                    text::ui_text("No skills are available from this connection.")
                        .dim()
                        .wrap(),
                )
            },
        );
        Section::new(
            "skills-toggle",
            SharedString::from(format!("Skills ({})", self.work.skills.len())),
        )
        .open(self.work.skills_open)
        .on_toggle(cx.listener(|this, _, _, cx| {
            this.work.skills_open = !this.work.skills_open;
            cx.notify();
        }))
        .body(vec![body.into_any_element()])
    }
}

#[allow(dead_code)] // dashboard discovery is exercised via the CLI entry point
pub fn discover_dashboard_url() -> Option<String> {
    let output = std::process::Command::new("pgrep")
        .args(["-f", "hermes_cli.main gateway"])
        .output()
        .ok()?;
    let pids: Vec<&str> = std::str::from_utf8(&output.stdout).ok()?.lines().collect();
    for pid in pids {
        let lsof = std::process::Command::new("lsof")
            .args(["-p", pid, "-i", "-sTCP:LISTEN", "-Fn"])
            .output()
            .ok()?;
        let text = std::str::from_utf8(&lsof.stdout).ok()?;
        for line in text.lines() {
            // lsof -Fn format: lines starting with 'n' contain the address
            if let Some(addr) = line.strip_prefix('n') {
                if addr.starts_with("127.0.0.1:") || addr.starts_with("localhost:") {
                    let port = addr.rsplit(':').next()?;
                    if port.parse::<u16>().is_ok() {
                        let url = format!("http://127.0.0.1:{port}");
                        // Verify it's actually the dashboard by checking for the token
                        if scrape_dashboard_token(&url).is_ok() {
                            return Some(url);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Scrape the dashboard session token from the index page's injected JS global.
pub(crate) fn scrape_dashboard_token(dashboard_url: &str) -> Result<String, String> {
    scrape_dashboard_token_with(dashboard_url, fetch_dashboard_index)
}

/// Fetch the dashboard index page body at `url` (blocking — worker threads
/// only). The production fetch behind [`scrape_dashboard_token`].
fn fetch_dashboard_index(url: &str) -> Result<String, String> {
    reqwest::blocking::get(url)
        .map_err(|e| format!("dashboard unreachable at {url}: {e}"))?
        .text()
        .map_err(|e| format!("dashboard response unreadable: {e}"))
}

/// [`scrape_dashboard_token`] with the index fetch injected (P3e seam): `fetch`
/// receives the index URL (`<dashboard_url>/`) and returns its body or an
/// error, which is passed through verbatim.
pub(crate) fn scrape_dashboard_token_with(
    dashboard_url: &str,
    fetch: impl FnOnce(&str) -> Result<String, String>,
) -> Result<String, String> {
    let url = format!("{}/", dashboard_url.trim_end_matches('/'));
    let html = fetch(&url)?;
    // window.__HERMES_SESSION_TOKEN__="<token>"
    let start = html
        .find("__HERMES_SESSION_TOKEN__=\"")
        .ok_or_else(|| "dashboard token not found in index HTML".to_string())?;
    let after_prefix = start + "__HERMES_SESSION_TOKEN__=\"".len();
    let end = html[after_prefix..]
        .find('"')
        .ok_or_else(|| "malformed dashboard token injection".to_string())?;
    Ok(html[after_prefix..after_prefix + end].to_string())
}

/// The connection card's message when neither transport is configured (P3e):
/// shown as the label and returned as the Test button's `Err`.
pub(crate) const CONNECTION_NOT_CONFIGURED: &str =
    "Not configured (set [gateway] or [hermes] in config.toml)";

/// What the connection card's Test button probes (P3e) — the transport the app
/// will actually use, in the card label's precedence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionTestTarget {
    /// The REST Hermes (`[hermes].base_url`) is enabled and no gateway is
    /// configured: probe its capabilities.
    Rest,
    /// The gateway (`[gateway].dashboard_url`) is configured — it wins over REST,
    /// as in routing: scrape the dashboard index for its session token.
    Gateway,
    /// Neither is configured: nothing to probe.
    None,
}

/// Choose the Test button's target (P3e round 2), gateway first — the transport
/// Work actually uses (a configured `dashboard_url` wins over REST, per
/// awili-config's `GatewayConfig` and `provider_build`): the gateway when
/// `gateway_dashboard` is non-empty, else REST Hermes when enabled, else
/// nothing. Pure.
pub(crate) fn connection_test_target(
    hermes_enabled: bool,
    gateway_dashboard: &str,
) -> ConnectionTestTarget {
    if !gateway_dashboard.is_empty() {
        ConnectionTestTarget::Gateway
    } else if hermes_enabled {
        ConnectionTestTarget::Rest
    } else {
        ConnectionTestTarget::None
    }
}

/// The connection card's label (P3e round 2), in [`connection_test_target`]'s
/// gateway-first precedence: gateway configured → `Gateway: <dashboard_url>`
/// (the whole string; the card wraps), even when REST is also enabled; else
/// REST Hermes enabled → the skills/toolsets summary when any are loaded, else
/// `Hermes: <base_url>`; neither → [`CONNECTION_NOT_CONFIGURED`]. Pure.
pub(crate) fn connection_label(
    hermes_enabled: bool,
    hermes_base: &str,
    gateway_dashboard: &str,
    skills: usize,
    toolsets: usize,
    tools: usize,
) -> String {
    match connection_test_target(hermes_enabled, gateway_dashboard) {
        ConnectionTestTarget::Rest if skills > 0 || toolsets > 0 => {
            format!("{skills} skills · {toolsets} toolsets ({tools} tools)")
        }
        ConnectionTestTarget::Rest => format!("Hermes: {hermes_base}"),
        ConnectionTestTarget::Gateway => format!("Gateway: {gateway_dashboard}"),
        ConnectionTestTarget::None => CONNECTION_NOT_CONFIGURED.to_string(),
    }
}

/// The Test button's gateway probe (P3e): scrape the dashboard index through
/// `fetch` and report `Ok("Gateway reachable: session token found")`, the
/// scrape error verbatim, or `Err("dashboard session token is empty")` when the
/// page injects an empty token (round 2 — an empty credential is not success).
/// Blocking when `fetch` is — worker threads only.
pub(crate) fn gateway_connection_test(
    dashboard_url: &str,
    fetch: impl FnOnce(&str) -> Result<String, String>,
) -> Result<String, String> {
    let token = scrape_dashboard_token_with(dashboard_url, fetch)?;
    if token.is_empty() {
        return Err("dashboard session token is empty".to_string());
    }
    Ok("Gateway reachable: session token found".to_string())
}

/// Which origin serves the **skills/toolsets** surface (`/api/skills`,
/// `/api/tools/toolsets`). Those routes live on the dashboard gateway
/// (`dashboard_url`, e.g. :9119) — NOT the Hermes REST server
/// (`[hermes].base_url`, :8642) that serves `/api/jobs` + `/api/sessions`.
/// Hitting the Hermes server for skills 404s (user-test 2026-07-01 bug #2).
/// Falls back to the Hermes base only when the gateway is disabled. Pure so the
/// routing choice is testable without a live server.
fn skills_surface_base(gw: &GatewayCfg, hermes: &HermesCfg) -> String {
    if gw.enabled {
        gw.dashboard_url.clone()
    } else {
        hermes.base_url.clone()
    }
}

/// SX-FLAKE: the skills error a test build gets instead of a real fetch.
pub(crate) const SKILLS_NOT_FETCHED_IN_TESTS: &str = "skills are not fetched in a test build";

/// Build the REST client for the skills/toolsets surface. When the gateway is
/// enabled the routes are on the dashboard origin and require the scraped
/// session token (the config `api_key` is 401 there); when it's disabled we fall
/// back to the Hermes REST config. See [`skills_surface_base`].
fn skills_rest_client(gw: &GatewayCfg, hermes: &HermesCfg) -> Result<RestClient, String> {
    let base = skills_surface_base(gw, hermes);
    let token = if gw.enabled {
        scrape_dashboard_token(&gw.dashboard_url)?
    } else {
        hermes.api_key.clone()
    };
    Ok(RestClient::from_base_url(base, token))
}

/// Whether a Code-project root is too broad to run UNATTENDED (user-test F1):
/// an autonomous agent with write access to the whole home directory — or the
/// filesystem root — must never be a silent default. Ask mode stays allowed at
/// any root; only the Autonomous flip is gated on this.
pub(crate) fn code_root_too_broad(folder: &str) -> bool {
    let p = std::path::Path::new(folder.trim());
    let canon = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if canon == std::path::Path::new("/") || (cfg!(windows) && canon.parent().is_none()) {
        return true;
    }
    match crate::home::home_opt() {
        Some(h) => {
            let home = std::path::Path::new(&h);
            canon == home || canon == home.canonicalize().unwrap_or_else(|_| home.to_path_buf())
        }
        None => false,
    }
}

/// The `/api/*` REST client for the desktop-dashboard surface (user-test G1).
/// EVERY `/api` route (sessions, cron, skills, config, status, analytics,
/// messaging) is served by the Hermes DASHBOARD (`[gateway].dashboard_url`,
/// e.g. :9119) behind its rotating session token — the agent origin
/// (`[hermes].base_url`, :8642) serves only the `/v1` OpenAI surface and 404s
/// all of `/api` (live-verified 2026-08-22, matching the 2026-07-01 skills
/// lesson in [`skills_surface_base`]). Blocking (scrapes the dashboard index
/// for the token) — call from worker threads only. Scrape-first; an embedded
/// `?token=` in the configured URL is the fallback (WS-lane precedent).
pub(crate) fn dashboard_rest_client(dashboard_url: &str) -> Result<RestClient, String> {
    let dash = dashboard_url.trim().trim_end_matches('/');
    if dash.is_empty() {
        return Err("gateway dashboard_url is not configured".to_string());
    }
    let u = url::Url::parse(dash).map_err(|e| format!("invalid dashboard_url: {e}"))?;
    let host = u
        .host_str()
        .ok_or_else(|| "dashboard_url has no host".to_string())?;
    let origin = match u.port() {
        Some(p) => format!("{}://{}:{}", u.scheme(), host, p),
        None => format!("{}://{}", u.scheme(), host),
    };
    let embedded = u
        .query_pairs()
        .find(|(k, _)| k == "token")
        .map(|(_, v)| v.into_owned())
        .filter(|t| !t.is_empty());
    let token = match scrape_dashboard_token(dash) {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => embedded.ok_or_else(|| "dashboard returned an empty session token".to_string())?,
        Err(e) => embedded.ok_or(e)?,
    };
    Ok(RestClient::from_base_url(origin, token))
}

/// Connect to the gateway WebSocket, scraping the token from the dashboard.
fn connect_gateway(cfg: &GatewayCfg) -> Result<GatewayClient, String> {
    let url_obj: url::Url = cfg
        .dashboard_url
        .parse()
        .map_err(|e| format!("bad dashboard_url: {e}"))?;
    let host = url_obj.host_str().unwrap_or("127.0.0.1");
    let port = url_obj.port().unwrap_or(9119);
    let token = scrape_dashboard_token(&cfg.dashboard_url)?;
    let ws_url = gateway_url(host, port, &token);
    GatewayClient::connect(&ws_url).map_err(|e| format!("gateway connect failed: {e}"))
}

/// Map a raw gateway event into WorkUpdate(s) for the UI.
fn extract_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for word in text.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| {
            c == '('
                || c == ')'
                || c == '<'
                || c == '>'
                || c == '"'
                || c == '\''
                || c == ','
                || c == '.'
        });
        if (trimmed.starts_with("http://") || trimmed.starts_with("https://")) && trimmed.len() > 10
        {
            urls.push(trimmed.to_string());
        }
    }
    urls
}

/// If a Work-mode failure is a provider/credential **setup** problem (no
/// provider configured, missing API key), return a one-line hint pointing the
/// user at the fix — so a fresh install doesn't show a bare "no provider
/// configured". Mirrors the reference's `isProviderSetupErrorMessage` patterns.
/// Matching is conservative to avoid tagging unrelated runtime errors.
/// Explicit backend capability rejection; never infer vision support from a
/// model name or a generic HTTP status, and never silently switch providers.
pub(crate) fn image_endpoint_error(msg: &str) -> Option<&'static str> {
    msg.contains("Only 'text' content type is supported").then_some(
        "This model endpoint accepts text only and cannot read the image. Your image is saved in this conversation. Choose Codex, Claude, or a vision-capable endpoint to read it."
    )
}

pub(crate) fn provider_setup_hint(msg: &str) -> Option<&'static str> {
    let m = msg.to_ascii_lowercase();
    let is_setup = m.contains("no_provider_configured")
        || m.contains("set an api key")
        || m.contains("openrouter_api_key")
        || m.contains("openai_api_key")
        || m.contains("anthropic_api_key")
        || (m.contains("no ") && m.contains("provider") && m.contains("configured"));
    is_setup.then_some(
        " — set up a model/provider in Settings (a provider API key, or the [hermes]/[gateway] section).",
    )
}

/// Map the gateway's `SubagentPhase` onto the seam's neutral phase.
fn subagent_phase_to_work(p: &SubagentPhase) -> awili_work::SubagentPhase {
    match p {
        SubagentPhase::SpawnRequested => awili_work::SubagentPhase::SpawnRequested,
        SubagentPhase::Start => awili_work::SubagentPhase::Start,
        SubagentPhase::Thinking => awili_work::SubagentPhase::Thinking,
        SubagentPhase::Tool => awili_work::SubagentPhase::Tool,
        SubagentPhase::Progress => awili_work::SubagentPhase::Progress,
        SubagentPhase::Complete => awili_work::SubagentPhase::Complete,
        SubagentPhase::Other => awili_work::SubagentPhase::Other,
    }
}

/// Map the seam's neutral phase back onto the gateway's `SubagentPhase` (the
/// unit-18 fold and AGENTS card consume the gateway enum — the adapter
/// reconstructs it losslessly so that whole path stays untouched).
fn subagent_phase_from_work(p: awili_work::SubagentPhase) -> SubagentPhase {
    match p {
        awili_work::SubagentPhase::SpawnRequested => SubagentPhase::SpawnRequested,
        awili_work::SubagentPhase::Start => SubagentPhase::Start,
        awili_work::SubagentPhase::Thinking => SubagentPhase::Thinking,
        awili_work::SubagentPhase::Tool => SubagentPhase::Tool,
        awili_work::SubagentPhase::Progress => SubagentPhase::Progress,
        awili_work::SubagentPhase::Complete => SubagentPhase::Complete,
        awili_work::SubagentPhase::Other => SubagentPhase::Other,
    }
}

/// Translate one raw gateway event into neutral seam FACTS (D1).
///
/// Hermes-side knowledge only: event decode, status-spelling normalization
/// (`parse_complete_status`), context-percent unit normalization (0-1 vs
/// 0-100), invalid-payload guards (empty request/subagent ids), and
/// turn-lifecycle semantics (`SessionInfo{running:false}` => turn over). NO
/// prose, NO thresholds, NO Markdown — those are app policy and live in
/// [`work_event_to_updates`]. The composition of the two reproduces the
/// legacy `gateway_event_to_update` sequences exactly (pinned by the
/// `seam_translation_equivalent_to_legacy` test).
fn gateway_event_to_work_events(ev: &GatewayEvent) -> Vec<WorkEvent> {
    match decode_event(ev) {
        GatewayEventKind::MessageStart { model } => {
            vec![WorkEvent::TurnStarted { model }]
        }
        GatewayEventKind::MessageDelta { text } => {
            if text.is_empty() {
                return vec![];
            }
            vec![WorkEvent::Token(text)]
        }
        GatewayEventKind::MessageComplete {
            text,
            status,
            cost_usd,
            context_percent,
            ..
        } => {
            let status = status
                .map(|s| parse_complete_status(&s))
                .unwrap_or(RunStatus::Completed);
            // Hermes sends context_percent as 0-1 or 0-100; normalize the unit
            // here (engine quirk). The >=80% surfacing threshold is app policy.
            let context_percent = context_percent.map(|p| if p <= 1.0 { p * 100.0 } else { p });
            vec![WorkEvent::TurnCompleted {
                status,
                output: text,
                cost_usd,
                context_percent,
            }]
        }
        GatewayEventKind::Tool {
            name,
            message,
            file_path,
            ..
        } => {
            vec![WorkEvent::ToolStarted {
                name,
                detail: message,
                file_path,
            }]
        }
        GatewayEventKind::ToolComplete {
            name,
            message,
            duration_s,
        } => {
            // The gateway reports no per-tool outcome: `ok: None` (the adapter
            // renders "completed", exactly as pre-D1).
            vec![WorkEvent::ToolFinished {
                name,
                detail: message,
                duration_s,
                ok: None,
            }]
        }
        GatewayEventKind::Approval {
            command,
            description,
            approval_id,
            allow_permanent,
            risk,
        } => {
            // P3a/S2a: the gateway's CLOSED risk class rides the gate
            // through to the surface as the TYPED enum the seam now carries
            // (no string round-trip, RVS2A-FIX1) — the card offers approval
            // scopes from it.
            vec![WorkEvent::ApprovalRequested {
                approval_id,
                command,
                description,
                allow_permanent,
                risk,
            }]
        }
        // P3a: a TYPED denial fact from the gateway — preserved verbatim
        // (never collapsed into a generic `Failed`); the transcript
        // rendering is the app's policy in [`work_event_to_updates`]. The
        // wire fields are optional (the gateway parses defensively); a
        // fact missing its source/reason is still a denial — name it
        // plainly instead of dropping it.
        GatewayEventKind::HookDenial {
            source,
            reason,
            recovery,
        } => {
            vec![WorkEvent::HookDenial {
                source: source.unwrap_or_else(|| "unknown".to_string()),
                reason: reason.unwrap_or_else(|| "denied".to_string()),
                recovery,
            }]
        }
        GatewayEventKind::ReasoningDelta { text } => {
            if text.is_empty() {
                return vec![];
            }
            vec![WorkEvent::ReasoningDelta { text }]
        }
        GatewayEventKind::ReasoningAvailable => vec![WorkEvent::ReasoningAvailable],
        GatewayEventKind::ThinkingDelta { text } => {
            vec![WorkEvent::ThinkingStatus { text }]
        }
        GatewayEventKind::Clarify {
            question,
            request_id,
        } => {
            vec![WorkEvent::ClarificationRequested {
                request_id,
                question,
            }]
        }
        GatewayEventKind::SessionInfo { running, .. } => {
            if running == Some(false) {
                vec![WorkEvent::TurnComplete]
            } else {
                vec![]
            }
        }
        GatewayEventKind::StatusUpdate { kind, message } => {
            vec![WorkEvent::StatusUpdate { kind, message }]
        }
        GatewayEventKind::TerminalRead { request_id } => {
            let request_id = request_id.unwrap_or_default();
            if request_id.is_empty() {
                vec![]
            } else {
                vec![WorkEvent::TerminalReadRequest { request_id }]
            }
        }
        GatewayEventKind::Sudo { request_id } => {
            let request_id = request_id.unwrap_or_default();
            if request_id.is_empty() {
                return vec![];
            }
            vec![WorkEvent::SudoRequest { request_id }]
        }
        GatewayEventKind::Secret {
            request_id,
            env_var,
            prompt,
        } => {
            let request_id = request_id.unwrap_or_default();
            if request_id.is_empty() {
                return vec![];
            }
            vec![WorkEvent::SecretRequest {
                request_id,
                env_var: env_var.unwrap_or_default(),
                prompt: prompt.unwrap_or_default(),
            }]
        }
        GatewayEventKind::ReviewSummary { text } => {
            let text = text.unwrap_or_default().trim().to_string();
            if text.is_empty() {
                return vec![];
            }
            vec![WorkEvent::ReviewSummary { text }]
        }
        GatewayEventKind::Error { message } => {
            let msg = message.unwrap_or_else(|| "unknown gateway error".to_string());
            vec![WorkEvent::Failed(msg)]
        }
        GatewayEventKind::Subagent {
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
        } => {
            if id.as_deref().unwrap_or_default().is_empty() {
                return vec![];
            }
            vec![WorkEvent::Subagent(Box::new(SubagentUpdate {
                phase: subagent_phase_to_work(&phase),
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
            }))]
        }
        // Intentionally ignored: connection lifecycle and cosmetic theme events.
        GatewayEventKind::GatewayReady | GatewayEventKind::SkinChanged { .. } => vec![],
        // Anything else is a gateway event this build does not model yet. Log it
        // (stderr, not user-facing) so contract drift — a renamed or newly-added
        // event the agent may be BLOCKING on — surfaces in diagnostics instead of
        // vanishing silently.
        GatewayEventKind::Unknown { event_type, .. } => {
            eprintln!("awili: unhandled gateway event type: {event_type}");
            vec![]
        }
    }
}

/// The app-side adapter (D1): neutral seam facts in, the legacy `WorkUpdate`
/// vocabulary out — INCLUDING all presentation policy that used to live in the
/// lines), the `RunEvent` compatibility synthesis, noise floors (0.05s tool
/// duration, cost>0), the >=80% context-fill threshold, the URL scrape for the
/// preview rail, and the subagent spawn/done chat lines. `apply_work_update`
/// and the RunPanel reducer are untouched; this function's output sequences
/// are Debug-identical to the legacy translation's (equivalence-pinned).
/// RC2-GEMMA5 (R7-A3): the progress line while the engine asks the model
/// again after the server cut its reply off.
pub(crate) const REPLY_RESTART_LINE: &str =
    "The model server stopped in the middle of the reply, so Awili asked it again\u{2026}";

/// RC2-GEMMA5: `text` without `shown` at its end (`None` when it does not end
/// with it, or nothing was shown).
pub(crate) fn taken_back(text: &str, shown: &str) -> Option<String> {
    (!shown.is_empty())
        .then(|| text.strip_suffix(shown))
        .flatten()
        .map(str::to_string)
}

fn work_event_to_updates(ev: WorkEvent) -> Vec<WorkUpdate> {
    match ev {
        WorkEvent::ApprovalDelivery { id, delivered } => {
            vec![WorkUpdate::ApprovalDelivery { id, delivered }]
        }
        // RC1-O: read by the in-app files record only (the footer); the
        // tool's own ToolFinished draws its row.
        WorkEvent::ToolMayHaveChangedFiles { .. } => vec![],
        // RC2-R6M2: read by the warm-up record (the worker's event loop).
        WorkEvent::Warmed { .. } => vec![],
        WorkEvent::Started(id) => vec![WorkUpdate::Started(id)],
        WorkEvent::TurnStarted { model } => {
            let mut updates = Vec::with_capacity(2);
            if let Some(m) = &model {
                updates.push(WorkUpdate::ModelDetected(m.clone()));
            }
            updates.push(WorkUpdate::Event(RunEvent::ToolProgress(
                awili_gateway::run_events::ToolProgressEvent {
                    tool: None,
                    status: Some("running".to_string()),
                    message: Some(match &model {
                        Some(m) => format!("Agent is thinking… ({m})"),
                        None => "Agent is thinking…".to_string(),
                    }),
                },
            )));
            updates
        }
        WorkEvent::Token(text) => {
            if text.is_empty() {
                return vec![];
            }
            let mut updates = vec![
                WorkUpdate::Token(text.clone()),
                WorkUpdate::Event(RunEvent::Token(awili_gateway::run_events::TokenEvent {
                    text: text.clone(),
                })),
            ];
            // Extract URLs from streamed text for the preview rail (app-derived
            // enrichment — the engine reports text, not URLs).
            for url in extract_urls(&text) {
                updates.push(WorkUpdate::UrlReferenced(url));
            }
            updates
        }
        WorkEvent::TurnCompleted {
            status,
            output,
            cost_usd,
            context_percent,
        } => {
            let mut updates = vec![];
            if let Some(c) = cost_usd.filter(|c| *c > 0.0) {
                updates.push(WorkUpdate::ReportedCost(c));
            }
            // Surface context fill once it's getting high — compaction must
            // never be silent; below 80% it's not worth the noise.
            if let Some(pct) = context_percent.filter(|p| *p >= 80.0) {
                updates.push(WorkUpdate::Event(RunEvent::ToolProgress(
                    awili_gateway::run_events::ToolProgressEvent {
                        tool: Some("context".to_string()),
                        status: Some("running".to_string()),
                        message: Some(format!("⚠ context {pct:.0}% full")),
                    },
                )));
            }
            updates.push(WorkUpdate::Event(RunEvent::Completion(
                awili_gateway::run_events::CompletionEvent {
                    status: Some(status),
                    output,
                },
            )));
            updates
        }
        WorkEvent::ToolStarted {
            name,
            detail,
            file_path,
        } => {
            let mut updates = vec![];
            // Critique P2: tool activity no longer interleaves with the answer
            // in the transcript — the right rail's tool cards own it.
            let _ = &name;
            updates.push(WorkUpdate::Event(RunEvent::ToolProgress(
                awili_gateway::run_events::ToolProgressEvent {
                    tool: name.clone(),
                    status: Some("running".to_string()),
                    // RC2-RUN (round 6 Minor 2): plain words for a built-in
                    // tool when the engine gave no description.
                    message: detail.or_else(|| name.as_deref().map(tool_running_line)),
                },
            )));
            if let Some(fp) = file_path {
                updates.push(WorkUpdate::FileTouched(fp));
            }
            updates
        }
        WorkEvent::ToolFinished {
            name,
            detail,
            duration_s,
            ok,
        } => {
            let dur = duration_s
                .filter(|d| *d >= 0.05)
                .map(|d| format!(" ({d:.1}s)"))
                .unwrap_or_default();
            // Engine-reported failure renders as a failed row (D2-u1 P0-3);
            // `None`/`Some(true)` keep the exact pre-D1 "completed" rendering.
            let failed = ok == Some(false);
            let mut updates = vec![];
            // Critique P2: completion lines live in the rail's tool cards, but
            // an engine-reported FAILURE stays visible in the transcript.
            if let Some(n) = &name {
                // RC2-R55 (usability RC2 round 4, Major 3: "⚠ run_command
                // FAILED" on a step Awili's rules refused): a refusal reads
                // as what was not done and why, with no tool name.
                let refused = detail.as_deref().filter(|d| d.starts_with("Not done:"));
                if let (true, Some(why)) = (failed, refused) {
                    updates.push(WorkUpdate::Token(format!("\n> **\u{26A0} {why}**\n\n")));
                } else if failed {
                    // RC2-RUN: a built-in tool by its plain title.
                    let n = plain_tool(n).map_or(n.as_str(), |p| p.title);
                    updates.push(WorkUpdate::Token(format!(
                        "\n> **\u{26A0} {n} FAILED{dur}**\n\n"
                    )));
                }
            }
            updates.push(WorkUpdate::Event(RunEvent::ToolProgress(
                awili_gateway::run_events::ToolProgressEvent {
                    tool: name.clone(),
                    status: Some(if failed { "failed" } else { "completed" }.to_string()),
                    // RC1-Q-FIX2 (slice review P2): a failed call never
                    // reads "done".
                    message: detail
                        .or_else(|| name.as_deref().map(|s| tool_finished_line(s, failed))),
                },
            )));
            updates
        }
        WorkEvent::ApprovalRequested {
            approval_id,
            command,
            description,
            allow_permanent,
            risk,
        } => {
            let command = command.unwrap_or_default();
            let desc =
                description.unwrap_or_else(|| "The agent is requesting approval.".to_string());
            // RC2-UX (D-PUNCH S2-F09): plain words on the card and in the chat.
            let desc = plain_approval_text(&desc);
            let notice = if command.is_empty() {
                format!("\n> **\u{26A0} Approval needed:** {desc}\n")
            } else {
                format!("\n> **\u{26A0} Approval needed:** {desc}\n> ```\n> {command}\n> ```\n")
            };
            vec![
                WorkUpdate::Token(notice),
                WorkUpdate::Event(RunEvent::Approval(
                    awili_gateway::run_events::ApprovalEvent {
                        approval_id,
                        kind: Some(if allow_permanent {
                            "command_permanent".to_string()
                        } else {
                            "command".to_string()
                        }),
                        prompt: Some(if command.is_empty() {
                            desc
                        } else {
                            format!("{desc}\n\n```\n{command}\n```")
                        }),
                        // P3a/S2a: the risk class rides the event to the card
                        // (the seam's canonical class is exactly the card's
                        // closed set — no conversion, no string).
                        risk,
                    },
                )),
            ]
        }
        // P3a: a TYPED denial observed at the host hook or delivered by the
        // gateway — render the `{source, reason, recovery}` fact VERBATIM:
        // `Blocked by <source>: <reason>` plus only the supplied recovery.
        // An absent recovery says so explicitly; nothing is invented (no
        // bypass instructions) and the denial never collapses into a
        // generic `Failed`.
        WorkEvent::HookDenial {
            source,
            reason,
            recovery,
        } => {
            let mut text = format!("Blocked by {source}: {reason}\n");
            match recovery {
                Some(r) => text.push_str(&format!("{r}\n")),
                None => text.push_str("No automatic recovery supplied\n"),
            }
            vec![WorkUpdate::Token(text)]
        }
        WorkEvent::ClarificationRequested {
            request_id,
            question,
        } => {
            let question = question.unwrap_or_else(|| "The agent needs clarification.".to_string());
            vec![WorkUpdate::Event(RunEvent::Approval(
                awili_gateway::run_events::ApprovalEvent {
                    approval_id: request_id,
                    kind: Some("clarify".to_string()),
                    prompt: Some(question),
                    // A clarification gate is not a command approval: it
                    // gates no tool call, so the gate is conservative — the
                    // typed `Privileged` class, for which `always` is never
                    // offered (P3a/S2a).
                    risk: ApprovalRisk::Privileged,
                },
            ))]
        }
        WorkEvent::ReasoningDelta { text } => {
            if text.is_empty() {
                return vec![];
            }
            // RC2-RUN (S-Q1): a typed row, never a tool named "reasoning".
            vec![WorkUpdate::Reasoning { done: false }]
        }
        WorkEvent::ReasoningAvailable => {
            vec![WorkUpdate::Reasoning { done: true }]
        }
        WorkEvent::ThinkingStatus { text } => {
            let text = text.unwrap_or_else(|| "thinking…".to_string());
            if text.trim().is_empty() {
                return vec![];
            }
            vec![WorkUpdate::Event(RunEvent::ToolProgress(
                awili_gateway::run_events::ToolProgressEvent {
                    tool: None,
                    status: Some("running".to_string()),
                    message: Some(text),
                },
            ))]
        }
        WorkEvent::StatusUpdate { kind, message } => {
            // RC2-GEMMA5 (R7-A3): the model server cut the in-app reply off
            // and the engine asks again; the text it had shown is taken back.
            if kind.as_deref() == Some(awili_engine_worker::REPLY_RESTART) {
                return vec![WorkUpdate::ReplyRestart(message.unwrap_or_default())];
            }
            // RC1-I3: the in-app engine cannot use the Outline/Gitea connector
            // server yet (it advertises no roots epoch, RC2), so its "not
            // prepared" / "disconnected" lines are engine internals, not
            // progress the owner can act on.
            if kind.as_deref() == Some("mcp")
                && message
                    .as_deref()
                    .is_some_and(|m| m.starts_with("mcp:awili-connectors:"))
            {
                return vec![];
            }
            // RC2-RUN (round 6 Minor 1): the in-app engine's other MCP status
            // lines are internals; only a real problem shows, in plain words.
            if kind.as_deref() == Some("mcp") {
                return message
                    .as_deref()
                    .and_then(mcp_status_line)
                    .map(|line| {
                        vec![WorkUpdate::Event(RunEvent::ToolProgress(
                            awili_gateway::run_events::ToolProgressEvent {
                                tool: None,
                                status: Some("running".to_string()),
                                message: Some(line.to_string()),
                            },
                        ))]
                    })
                    .unwrap_or_default();
            }
            // Hermes tags context compaction here (kind="compacting"). Surface it
            // explicitly so a long turn shows "compacting context…" instead of an
            // unexplained pause — the no-silent-compaction stance.
            if kind.as_deref() == Some("compacting") {
                return vec![WorkUpdate::Event(RunEvent::ToolProgress(
                    awili_gateway::run_events::ToolProgressEvent {
                        tool: Some("context".to_string()),
                        status: Some("running".to_string()),
                        message: Some("compacting context…".to_string()),
                    },
                ))];
            }
            if let Some(m) = message {
                vec![WorkUpdate::Event(RunEvent::ToolProgress(
                    awili_gateway::run_events::ToolProgressEvent {
                        tool: None,
                        status: Some("running".to_string()),
                        message: Some(m),
                    },
                ))]
            } else {
                vec![]
            }
        }
        WorkEvent::TurnComplete => vec![WorkUpdate::TurnComplete],
        WorkEvent::UsageReported {
            input_tokens,
            output_tokens,
        } => {
            let clamp = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
            vec![WorkUpdate::ReportedUsage(awili_providers::Usage {
                cached_input_tokens: None,
                input_tokens: clamp(input_tokens),
                output_tokens: clamp(output_tokens),
            })]
        }
        WorkEvent::Terminal {
            status,
            output,
            model,
        } => {
            vec![WorkUpdate::Terminal {
                status,
                output,
                model,
            }]
        }
        WorkEvent::ReviewSummary { text } => {
            vec![WorkUpdate::Token(format!("\n> saved: {text}\n"))]
        }
        WorkEvent::Failed(msg) => vec![WorkUpdate::Failed(msg)],
        WorkEvent::TerminalReadRequest { request_id } => {
            vec![WorkUpdate::TerminalReadRequest { request_id }]
        }
        WorkEvent::SudoRequest { request_id } => {
            vec![WorkUpdate::SudoRequest { request_id }]
        }
        WorkEvent::SecretRequest {
            request_id,
            env_var,
            prompt,
        } => {
            vec![WorkUpdate::SecretRequest {
                request_id,
                env_var,
                prompt,
            }]
        }
        WorkEvent::Subagent(update) => {
            // The two human-facing chat-stream lines (spawn / done) are
            // presentation — synthesized HERE from the facts; then the raw
            // gateway enum is reconstructed losslessly so the unit-18 fold and
            // the AGENTS card stay untouched.
            let mut updates = vec![];
            match update.phase {
                awili_work::SubagentPhase::Start => {
                    if let Some(g) = &update.goal {
                        updates.push(WorkUpdate::Token(format!("\n> *spawned agent: {g}*\n")));
                    }
                }
                awili_work::SubagentPhase::Complete => {
                    if let Some(s) = &update.summary {
                        let short = text_clip::clip(s, 80, 77); // RC1-I1: char-safe
                        updates.push(WorkUpdate::Token(format!("> *agent done: {short}*\n\n")));
                    }
                }
                _ => {}
            }
            let SubagentUpdate {
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
            } = *update;
            updates.push(WorkUpdate::Subagent(Box::new(GatewayEventKind::Subagent {
                phase: subagent_phase_from_work(phase),
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
            })));
            updates
        }
    }
}

/// LEGACY reference translation — kept ONLY as the equivalence oracle for the
/// D1 seam tests (`seam_translation_equivalent_to_legacy`): the composition
/// `gateway_event_to_work_events` → `work_event_to_updates` must reproduce
/// this function's output Debug-identically. Not compiled into the product.
#[cfg(test)]
fn gateway_event_to_update(ev: &GatewayEvent) -> Vec<WorkUpdate> {
    // Plan units 3/4: the raw-string event vocabulary now lives in the
    // awili-gateway crate (`decode_event` -> `GatewayEventKind`). This reducer is a
    // thin consumer that matches the typed enum; the key fallbacks, field names,
    // and `Unknown` safety net all live in `events.rs`. Behavior is a 1:1 port of
    // the prior hand-rolled `match ev.event_type` — thresholds, status parsing,
    // and rendering stay here.
    match decode_event(ev) {
        GatewayEventKind::MessageStart { model } => {
            let mut updates = Vec::with_capacity(2);
            if let Some(m) = &model {
                updates.push(WorkUpdate::ModelDetected(m.clone()));
            }
            updates.push(WorkUpdate::Event(RunEvent::ToolProgress(
                awili_gateway::run_events::ToolProgressEvent {
                    tool: None,
                    status: Some("running".to_string()),
                    message: Some(match &model {
                        Some(m) => format!("Agent is thinking… ({m})"),
                        None => "Agent is thinking…".to_string(),
                    }),
                },
            )));
            updates
        }
        GatewayEventKind::MessageDelta { text } => {
            if text.is_empty() {
                return vec![];
            }
            let mut updates = vec![
                WorkUpdate::Token(text.clone()),
                WorkUpdate::Event(RunEvent::Token(awili_gateway::run_events::TokenEvent {
                    text: text.clone(),
                })),
            ];
            // Extract URLs from streamed text for the preview rail.
            for url in extract_urls(&text) {
                updates.push(WorkUpdate::UrlReferenced(url));
            }
            updates
        }
        GatewayEventKind::MessageComplete {
            text,
            status,
            cost_usd,
            context_percent,
            ..
        } => {
            // Reflect the real finish status (Hermes sends e.g. "complete",
            // "error", "stopped") instead of always claiming success — so an
            // empty turn can surface *why* it produced no text (P0.2).
            let status = status
                .map(|s| parse_complete_status(&s))
                .unwrap_or(RunStatus::Completed);
            let mut updates = vec![];
            // Hermes reports the turn's dollar cost on completion (Awili has no
            // Hermes token pricing, so this is the only Work-mode cost signal).
            if let Some(c) = cost_usd.filter(|c| *c > 0.0) {
                updates.push(WorkUpdate::ReportedCost(c));
            }
            // Surface context fill once it's getting high — Awili's stance is that
            // compaction must never be silent, so the user sees the window filling
            // before the backend compacts. Hermes sends context_percent in usage
            // (0-1 or 0-100); below 80% it's not worth the noise.
            if let Some(pct) = context_percent
                .map(|p| if p <= 1.0 { p * 100.0 } else { p })
                .filter(|p| *p >= 80.0)
            {
                updates.push(WorkUpdate::Event(RunEvent::ToolProgress(
                    awili_gateway::run_events::ToolProgressEvent {
                        tool: Some("context".to_string()),
                        status: Some("running".to_string()),
                        message: Some(format!("⚠ context {pct:.0}% full")),
                    },
                )));
            }
            updates.push(WorkUpdate::Event(RunEvent::Completion(
                awili_gateway::run_events::CompletionEvent {
                    status: Some(status),
                    output: text,
                },
            )));
            updates
        }
        GatewayEventKind::Tool {
            name,
            message,
            file_path,
            ..
        } => {
            let mut updates = vec![];
            // Critique P2: tool activity no longer interleaves with the answer
            // in the transcript — the right rail's tool cards own it.
            let _ = &name;
            updates.push(WorkUpdate::Event(RunEvent::ToolProgress(
                awili_gateway::run_events::ToolProgressEvent {
                    tool: name.clone(),
                    status: Some("running".to_string()),
                    // The agent's live tool detail (summary/message/text), falling
                    // back to the bare tool name.
                    message: message.or(name),
                },
            )));
            // Extract file paths from tool input for the preview rail.
            if let Some(fp) = file_path {
                updates.push(WorkUpdate::FileTouched(fp));
            }
            updates
        }
        GatewayEventKind::ToolComplete {
            name,
            message,
            duration_s,
        } => {
            // How long the tool ran (Hermes sends `duration_s`), so slow tools are
            // visible in the transcript. Sub-50ms is noise, so it's omitted.
            let dur = duration_s
                .filter(|d| *d >= 0.05)
                .map(|d| format!(" ({d:.1}s)"))
                .unwrap_or_default();
            let mut updates = vec![];
            // Critique P2: the rail's tool cards own completion lines.
            let _ = (&name, &dur);
            updates.push(WorkUpdate::Event(RunEvent::ToolProgress(
                awili_gateway::run_events::ToolProgressEvent {
                    tool: name.clone(),
                    status: Some("completed".to_string()),
                    // Prefer the agent's own completion summary over a generic label.
                    message: message.or_else(|| name.map(|s| format!("{s} done"))),
                },
            )));
            updates
        }
        GatewayEventKind::Approval {
            command,
            description,
            approval_id,
            allow_permanent,
            risk,
        } => {
            // P3a/S2a: the gateway's CLOSED risk class passes through to the
            // event as the TYPED enum (no string round-trip, RVS2A-FIX1);
            // the gateway decode is conservative, so `risk` is always a real
            // class, never an unclassified string.
            let command = command.unwrap_or_default();
            let desc =
                description.unwrap_or_else(|| "The agent is requesting approval.".to_string());
            // RC2-UX (D-PUNCH S2-F09): plain words on the card and in the chat.
            let desc = plain_approval_text(&desc);
            let notice = if command.is_empty() {
                format!("\n> **\u{26A0} Approval needed:** {desc}\n")
            } else {
                format!("\n> **\u{26A0} Approval needed:** {desc}\n> ```\n> {command}\n> ```\n")
            };
            vec![
                WorkUpdate::Token(notice),
                WorkUpdate::Event(RunEvent::Approval(
                    awili_gateway::run_events::ApprovalEvent {
                        approval_id,
                        kind: Some(if allow_permanent {
                            "command_permanent".to_string()
                        } else {
                            "command".to_string()
                        }),
                        prompt: Some(if command.is_empty() {
                            desc
                        } else {
                            format!("{desc}\n\n```\n{command}\n```")
                        }),
                        // P3a: the risk class rides the event to the card.
                        risk,
                    },
                )),
            ]
        }
        GatewayEventKind::ReasoningDelta { text } => {
            if text.is_empty() {
                return vec![];
            }
            // RC2-RUN (S-Q1): a typed row, never a tool named "reasoning".
            vec![WorkUpdate::Reasoning { done: false }]
        }
        GatewayEventKind::ReasoningAvailable => {
            vec![WorkUpdate::Reasoning { done: true }]
        }
        // Hermes' streaming status line ("face + verb" spinner text). It carries
        // its own message; surface it as transient progress rather than dropping
        // it (the upstream desktop renders it as the live status). Falls back to a
        // generic label when the payload omits the text.
        GatewayEventKind::ThinkingDelta { text } => {
            let text = text.unwrap_or_else(|| "thinking…".to_string());
            if text.trim().is_empty() {
                return vec![];
            }
            vec![WorkUpdate::Event(RunEvent::ToolProgress(
                awili_gateway::run_events::ToolProgressEvent {
                    tool: None,
                    status: Some("running".to_string()),
                    message: Some(text),
                },
            ))]
        }
        GatewayEventKind::Clarify {
            question,
            request_id,
        } => {
            let question = question.unwrap_or_else(|| "The agent needs clarification.".to_string());
            vec![WorkUpdate::Event(RunEvent::Approval(
                awili_gateway::run_events::ApprovalEvent {
                    approval_id: request_id,
                    kind: Some("clarify".to_string()),
                    prompt: Some(question),
                    // A clarification gate is not a command approval: it
                    // gates no tool call, so the gate is conservative — the
                    // typed `Privileged` class, for which `always` is never
                    // offered (P3a/S2a).
                    risk: ApprovalRisk::Privileged,
                },
            ))]
        }
        GatewayEventKind::SessionInfo { running, .. } => {
            if running == Some(false) {
                vec![WorkUpdate::TurnComplete]
            } else {
                vec![]
            }
        }
        GatewayEventKind::StatusUpdate { kind, message } => {
            // Hermes tags context compaction here (kind="compacting"). Surface it
            // explicitly so a long turn shows "compacting context…" instead of an
            // unexplained pause — Awili's no-silent-compaction stance (slice 21)
            // extended to the gateway/Work path.
            if kind.as_deref() == Some("compacting") {
                return vec![WorkUpdate::Event(RunEvent::ToolProgress(
                    awili_gateway::run_events::ToolProgressEvent {
                        tool: Some("context".to_string()),
                        status: Some("running".to_string()),
                        message: Some("compacting context…".to_string()),
                    },
                ))];
            }
            if let Some(m) = message {
                vec![WorkUpdate::Event(RunEvent::ToolProgress(
                    awili_gateway::run_events::ToolProgressEvent {
                        tool: None,
                        status: Some("running".to_string()),
                        message: Some(m),
                    },
                ))]
            } else {
                vec![]
            }
        }
        // The current Hermes gateway emits "terminal.read.request"; older builds
        // used the bare "terminal.read". Both decode to `TerminalRead` so the
        // read_terminal tool — which BLOCKS on terminal.read.respond — never hangs
        // on a name mismatch.
        GatewayEventKind::TerminalRead { request_id } => {
            let request_id = request_id.unwrap_or_default();
            if request_id.is_empty() {
                vec![]
            } else {
                vec![WorkUpdate::TerminalReadRequest { request_id }]
            }
        }
        // The agent is blocking on a sudo password (sudo.request) — surface it as
        // a credential prompt the user can refuse (or, later, fill). An unhandled
        // request hangs the turn until the backend's ~5-min timeout.
        GatewayEventKind::Sudo { request_id } => {
            let request_id = request_id.unwrap_or_default();
            if request_id.is_empty() {
                return vec![];
            }
            vec![WorkUpdate::SudoRequest { request_id }]
        }
        // The agent is blocking on a credential value (secret.request) for an env
        // var — e.g. a skill needs an API key.
        GatewayEventKind::Secret {
            request_id,
            env_var,
            prompt,
        } => {
            let request_id = request_id.unwrap_or_default();
            if request_id.is_empty() {
                return vec![];
            }
            vec![WorkUpdate::SecretRequest {
                request_id,
                env_var: env_var.unwrap_or_default(),
                prompt: prompt.unwrap_or_default(),
            }]
        }
        // The backend's self-improvement review saved a skill/memory and emitted a
        // summary. The upstream desktop surfaces this as a PERSISTENT line so the
        // change is never silent (a transient toast could be missed). Mirror that
        // with the same quote-prefixed notice the subagent summaries use.
        GatewayEventKind::ReviewSummary { text } => {
            let text = text.unwrap_or_default().trim().to_string();
            if text.is_empty() {
                return vec![];
            }
            vec![WorkUpdate::Token(format!("\n> saved: {text}\n"))]
        }
        GatewayEventKind::Error { message } => {
            let msg = message.unwrap_or_else(|| "unknown gateway error".to_string());
            vec![WorkUpdate::Failed(msg)]
        }
        kind @ GatewayEventKind::Subagent { .. } => {
            // The full fold (files, tokens, cost, tree nesting) happens in the
            // unit-18 `awili_gateway::subagents` reducer on the UI thread — the
            // worker just forwards the raw event. Here we only surface the two
            // human-facing chat-stream lines (spawn / done) before shipping it.
            let GatewayEventKind::Subagent {
                ref phase,
                ref id,
                ref goal,
                ref summary,
                ..
            } = kind
            else {
                unreachable!("matched Subagent above")
            };
            if id.as_deref().unwrap_or_default().is_empty() {
                return vec![];
            }

            let mut updates = vec![];
            match phase {
                SubagentPhase::Start => {
                    if let Some(g) = goal {
                        updates.push(WorkUpdate::Token(format!("\n> *spawned agent: {g}*\n")));
                    }
                }
                SubagentPhase::Complete => {
                    if let Some(s) = summary {
                        let short = text_clip::clip(s, 80, 77); // RC1-I1: char-safe
                        updates.push(WorkUpdate::Token(format!("> *agent done: {short}*\n\n")));
                    }
                }
                _ => {}
            }
            updates.push(WorkUpdate::Subagent(Box::new(kind)));
            updates
        }
        // P3a: the typed denial fact renders verbatim, exactly as the
        // production path in [`work_event_to_updates`] does.
        GatewayEventKind::HookDenial {
            source,
            reason,
            recovery,
        } => {
            let source = source.unwrap_or_else(|| "unknown".to_string());
            let reason = reason.unwrap_or_else(|| "denied".to_string());
            let mut text = format!("Blocked by {source}: {reason}\n");
            match recovery {
                Some(r) => text.push_str(&format!("{r}\n")),
                None => text.push_str("No automatic recovery supplied\n"),
            }
            vec![WorkUpdate::Token(text)]
        }
        // Intentionally ignored: connection lifecycle and cosmetic theme events.
        GatewayEventKind::GatewayReady | GatewayEventKind::SkinChanged { .. } => vec![],
        // Anything else is a gateway event this build does not model yet. Log it
        // (stderr, not user-facing) so contract drift — a renamed or newly-added
        // event the agent may be BLOCKING on — surfaces in diagnostics instead of
        // vanishing silently. This is exactly how the terminal.read rename would
        // have been caught.
        GatewayEventKind::Unknown { event_type, .. } => {
            eprintln!("awili: unhandled gateway event type: {event_type}");
            vec![]
        }
    }
}

/// The subset of [`GatewayClient`] the worker loop drives — a deterministic
/// test seam INSIDE the Hermes implementation (Codex D1 P1-b). This trait
/// never crosses the neutral `awili-work` boundary; it exists so the loop's
/// command/lifecycle semantics are pinned by a scripted fake instead of only
/// by the live gate. `impl GatewayOps for GatewayClient` is 1:1 delegation.
trait GatewayOps {
    fn session_create(
        &self,
        cwd: &str,
    ) -> Result<awili_gateway::SessionCreateResponse, awili_gateway::GatewayError>;
    fn prompt_submit(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError>;
    fn approval_respond(
        &self,
        session_id: &str,
        choice: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError>;
    fn clarify_respond(
        &self,
        session_id: &str,
        answer: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError>;
    fn session_steer(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError>;
    fn file_attach(
        &self,
        session_id: &str,
        name: &str,
        path: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError>;
    fn terminal_read_respond(
        &self,
        session_id: &str,
        request_id: &str,
        text: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError>;
    fn sudo_respond(
        &self,
        request_id: &str,
        password: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError>;
    fn secret_respond(
        &self,
        request_id: &str,
        value: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError>;
    fn session_close(&self, session_id: &str) -> Result<(), awili_gateway::GatewayError>;
    /// Ask the backend to stop the in-flight turn while keeping the session
    /// open (`session.interrupt`, P3c). The RPC's result payload is discarded.
    fn session_interrupt(&self, session_id: &str) -> Result<(), awili_gateway::GatewayError>;
    fn recv_event(&self, timeout: Duration) -> Option<GatewayEvent>;
    fn is_connected(&self) -> bool;
    fn close(&mut self);
}

impl GatewayOps for GatewayClient {
    fn session_create(
        &self,
        cwd: &str,
    ) -> Result<awili_gateway::SessionCreateResponse, awili_gateway::GatewayError> {
        GatewayClient::session_create(self, cwd)
    }
    fn prompt_submit(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
        GatewayClient::prompt_submit(self, session_id, text)
    }
    fn approval_respond(
        &self,
        session_id: &str,
        choice: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
        GatewayClient::approval_respond(self, session_id, choice)
    }
    fn clarify_respond(
        &self,
        session_id: &str,
        answer: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
        GatewayClient::clarify_respond(self, session_id, answer)
    }
    fn session_steer(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
        GatewayClient::session_steer(self, session_id, text)
    }
    fn file_attach(
        &self,
        session_id: &str,
        name: &str,
        path: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
        GatewayClient::file_attach(self, session_id, name, path)
    }
    fn terminal_read_respond(
        &self,
        session_id: &str,
        request_id: &str,
        text: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
        GatewayClient::terminal_read_respond(self, session_id, request_id, text)
    }
    fn sudo_respond(
        &self,
        request_id: &str,
        password: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
        GatewayClient::sudo_respond(self, request_id, password)
    }
    fn secret_respond(
        &self,
        request_id: &str,
        value: &str,
    ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
        GatewayClient::secret_respond(self, request_id, value)
    }
    fn session_close(&self, session_id: &str) -> Result<(), awili_gateway::GatewayError> {
        GatewayClient::session_close(self, session_id)
    }
    fn session_interrupt(&self, session_id: &str) -> Result<(), awili_gateway::GatewayError> {
        GatewayClient::session_interrupt(self, session_id).map(|_| ())
    }
    fn recv_event(&self, timeout: Duration) -> Option<GatewayEvent> {
        GatewayClient::recv_event(self, timeout)
    }
    fn is_connected(&self) -> bool {
        GatewayClient::is_connected(self)
    }
    fn close(&mut self) {
        GatewayClient::close(self)
    }
}

/// P3c round 2 (R1): whether the in-flight Work turn runs on a live Hermes
/// gateway worker — the only lane whose Stop interrupts and keeps the session.
/// The owned engine shares `gateway_cmd_tx` but observes a mid-turn cancel only
/// through the cancel flag, and an agent-harness or local Build run may be in
/// flight while an idle gateway worker still holds the channel; both keep the
/// pre-P3c Stop.
pub(crate) fn gateway_turn_in_flight(work: &WorkState) -> bool {
    work.running
        && !work.resume_gate_pending
        && work.agent_abort_tx.is_none()
        && work.gateway_cmd_tx.is_some()
        && work.panel.requested_identity == Some(WorkRunIdentity::HermesGateway)
}

/// P3c round 2 (R1): send the live gateway worker its `Cancel` without
/// touching run_seq, the cancel flag, the sender or `session_id`, and mark the
/// Stop pending until the worker's `Terminal { Stopped }` arrives. Returns
/// whether the command was delivered; if the worker is already gone nothing
/// will acknowledge, so the dead sender is forgotten and the next submit
/// respawns.
///
/// P3d: when an approval card was showing (`pending_approval`, taken off the
/// panel by `cancel_work_run`), it is answered with `deny` FIRST, on the same
/// channel, so the worker's in-order queue (P3c R11) runs `approval_respond`
/// before the `Cancel`'s interrupt and the backend can end the turn cleanly.
pub(crate) fn gateway_stop(work: &mut WorkState, pending_approval: Option<&ApprovalCard>) -> bool {
    let sent = work.gateway_cmd_tx.as_ref().is_some_and(|tx| {
        if let Some(card) = pending_approval {
            // A failed send means the worker is gone; the `Cancel` below
            // then fails too and the dead sender is forgotten.
            let _ = tx.send(stop_denial(card));
        }
        tx.send(GatewayCmd::Cancel).is_ok()
    });
    if sent {
        work.gateway_stop_pending = true;
    } else {
        work.gateway_cmd_tx = None;
        work.session_id = None;
    }
    sent
}

/// P3d: the verdict a gateway Stop sends for the approval card that was
/// showing — the Deny button's scope string. The card's backend id crosses
/// when it has one; Hermes' `approval.request` carries none and
/// `approval.respond` is session-scoped, so an id-less card is denied too.
fn stop_denial(card: &ApprovalCard) -> GatewayCmd {
    GatewayCmd::Approval {
        id: Some(card.approval_id.clone()).filter(|id| !id.is_empty()),
        choice: ApprovalDecision::Deny.scope_str().into(),
    }
}

/// P3c round 4 (R8/R9): the pending gateway Stop was acknowledged (the old
/// worker's `Terminal { Stopped }`, or the dead-worker fallback that replays
/// it — never an ordinary `Failed`, round 6 R13). Lifts the stop barrier — the single place that
/// ends it — so panel ownership and the worker epoch may change again. If the
/// working folder changed during the Stop, the now idle old worker gets its
/// deferred `Shutdown` and its sender and `session_id` are forgotten, so the
/// parked prompt (replayed by the caller afterwards) respawns a worker in the
/// new folder.
pub(crate) fn end_gateway_stop_barrier(work: &mut WorkState) {
    work.gateway_stop_pending = false;
    if std::mem::take(&mut work.folder_changed_during_stop) {
        if let Some(cmd_tx) = work.gateway_cmd_tx.take() {
            let _ = cmd_tx.send(GatewayCmd::Shutdown);
        }
        work.session_id = None;
    }
}

/// The transcript line a `Failed` leaves on the stopped turn when it arrives
/// while a gateway Stop is pending (P3c round 6, R13).
pub(crate) fn failure_during_stop_line(msg: &str) -> String {
    format!("\n> \u{26A0} {msg}\n")
}

/// The Work activity line while a prompt waits for a pending gateway Stop to
/// be acknowledged (P3c round 3, R5).
pub(crate) const GATEWAY_STOPPING_ACTIVITY: &str = "Stopping the previous turn…";

/// Hand a Work prompt to the persistent worker, if there is one that can take
/// it. Returns `true` when sent. If the channel is broken (the worker died)
/// the worker is forgotten — `gateway_cmd_tx` and `session_id` cleared, no
/// Stop pending (P3c round 3, R6) — and the caller respawns. Never called
/// while a gateway Stop is pending: `submit_work_run` parks that prompt until
/// the Stop is acknowledged (R5).
pub(crate) fn offer_prompt_to_worker(
    work: &mut WorkState,
    text: &str,
    system: Option<&str>,
) -> bool {
    let Some(cmd_tx) = &work.gateway_cmd_tx else {
        return false;
    };
    if cmd_tx
        .send(GatewayCmd::Prompt {
            text: text.to_string(),
            system: system.map(str::to_string),
        })
        .is_ok()
    {
        return true;
    }
    work.gateway_cmd_tx = None;
    work.session_id = None;
    work.gateway_stop_pending = false;
    false
}

/// S5(a): route one owned-engine Work turn once the memory gate answered.
#[derive(Debug)]
pub(crate) enum GatedWorkPrompt {
    /// The live worker took `GatewayCmd::Prompt { text, system }`.
    Offered,
    /// No live worker: the caller spawns one with this `system`.
    NeedsWorker(Option<String>),
    /// The gate refused: nothing was sent.
    Refused(String),
}

/// S5(a): the gate's verdict decides the turn's routing — a refused gate
/// touches NOTHING (no prompt, no spawn, no state change); an accepted gate
/// carries the gate's pinned preamble to the live worker (or to the spawn
/// when there is none).
pub(crate) fn offer_gated_work_prompt(
    work: &mut WorkState,
    task: &str,
    outcome: Result<Option<String>, String>,
) -> GatedWorkPrompt {
    let Ok(system) = outcome else {
        return GatedWorkPrompt::Refused(outcome.unwrap_err());
    };
    if offer_prompt_to_worker(work, task, system.as_deref()) {
        GatedWorkPrompt::Offered
    } else {
        GatedWorkPrompt::NeedsWorker(system)
    }
}

/// U4 + S5(a): hand one gated owned-engine prompt to the worker. An
/// accepted gate first installs `conv_id`'s Ask/Auto mode into the shared
/// cell the worker reads at each approval gate (the live-worker path
/// bypasses `dispatch_work_prompt`, which installs it otherwise); a refused
/// gate touches nothing, not even the mode.
pub(crate) fn hand_off_gated_work_prompt(
    work: &mut WorkState,
    conv_id: &str,
    task: &str,
    outcome: Result<Option<String>, String>,
) -> GatedWorkPrompt {
    if outcome.is_ok() {
        // RC1-I2 / RC1-I2-FIX1: the in-app worker belongs to one
        // conversation — retired only once the gate accepted the prompt, so a
        // refused turn changes no live worker or session.
        retire_worker_of_other_conversation(work, conv_id);
        crate::ask_mode::install_conversation_mode(&work.approval_handle, &work.ask_modes, conv_id);
    }
    offer_gated_work_prompt(work, task, outcome)
}

/// RC1-I2-FIX2: end the in-app worker's session: shut the worker down and
/// forget its sender and session, so the next prompt spawns a new worker — a
/// new session whose first change asks again.
pub(crate) fn retire_in_app_worker(work: &mut WorkState) {
    if let Some(cmd_tx) = work.gateway_cmd_tx.take() {
        let _ = cmd_tx.send(GatewayCmd::Shutdown);
    }
    work.session_id = None;
    work.retire_after_run = false;
}

impl AwiliApp {
    /// RC1-J: at an in-app run's end, keep the model history its worker
    /// published, matched to the transcript of the worker's conversation as
    /// it stands now (that turn included) — unless that conversation was
    /// deleted meanwhile (RC1-J1-FIX1).
    pub(crate) fn keep_inapp_history(&mut self) {
        let Some((conv, slot)) = self.work.inapp_history_slot.as_ref() else {
            return;
        };
        let Some(history) = slot.lock().ok().and_then(|mut g| g.take()) else {
            return;
        };
        // RC1-J1-FIX1: a conversation deleted while its turn was running keeps
        // nothing — no file, no in-memory entry. Deletion and this check both
        // run on the UI thread, deletion first, so the check always sees it.
        if self.conversations.get(conv).is_none() {
            return;
        }
        let kept = inapp_history::KeptHistory::new(
            history,
            self.views
                .get(conv)
                .map(|v| v.convo.messages())
                .unwrap_or_default(),
        );
        inapp_history::save(conv, &kept);
        self.work.kept_histories.insert(conv.clone(), kept);
    }

    /// RC1-I2-FIX2 / RC1-I2-FIX3: called by `set_active_conversation`
    /// (sidebar.rs), the one setter every change of the active conversation
    /// goes through. Leaving the in-app worker's
    /// conversation ends its session — "Allow for this session" lasts until
    /// you switch to another conversation. An idle worker is retired at once;
    /// a running one is never stopped: it finishes its run and is retired
    /// then. Other lanes are untouched.
    pub(crate) fn leave_worker_conversation(&mut self) {
        // An archive is a temporary stored-transcript reader, not a new live
        // Work owner. Returning to the original tab keeps its session grants.
        if self.shell.archived_reader.is_some() {
            return;
        }
        let in_app = matches!(
            self.work.worker_identity,
            Some(WorkRunIdentity::OwnedEngine { .. })
        );
        let left = self
            .work
            .worker_conv
            .as_deref()
            .is_some_and(|c| c != self.sidebar.active_id());
        if !in_app || !left || self.work.gateway_cmd_tx.is_none() {
            return;
        }
        if self.work.running || self.work.gateway_stop_pending {
            self.work.retire_after_run = true;
            return;
        }
        retire_in_app_worker(&mut self.work);
    }
}

/// RC1-I3-FIX1: in an in-app run's Progress line ("Agent is thinking…
/// (<model>)"), the configured model path becomes the display label. Only a
/// path (it contains '/') is replaced; nothing else in the update changes.
pub(crate) fn name_model_by_label(up: &mut WorkUpdate, model_path: &str, label: &str) {
    if !model_path.contains('/') {
        return;
    }
    if let WorkUpdate::Event(RunEvent::ToolProgress(p)) = up {
        if let Some(m) = p.message.as_mut() {
            if m.contains(model_path) {
                *m = m.replace(model_path, label);
            }
        }
    }
}

/// RC1-I2: the in-app engine keeps one worker — its own history, B2 log and
/// session grants — per conversation. A prompt for another conversation
/// retires the live worker (unless a Stop is still settling), so the next
/// dispatch spawns one for this conversation, hydrated from its own
/// transcript. "Allow for this session" therefore never reaches another
/// conversation, and no conversation sees another's history.
pub(crate) fn retire_worker_of_other_conversation(work: &mut WorkState, conv_id: &str) {
    if work.gateway_stop_pending || work.worker_conv.as_deref() == Some(conv_id) {
        return;
    }
    if let Some(cmd_tx) = work.gateway_cmd_tx.take() {
        let _ = cmd_tx.send(GatewayCmd::Shutdown);
    }
    work.session_id = None;
}

/// Start `worker` on its own thread (`awili_work::spawn_worker`). Tests can
/// take over one spawn through [`TEST_WORKER_SPAWN`] so a respawned gateway
/// worker runs the real loop over the scripted fake instead of connecting.
fn spawn_engine_worker(worker: Box<dyn awili_work::EngineWorker>, ctx: WorkerContext) {
    #[cfg(test)]
    {
        if let Some(hook) = TEST_WORKER_SPAWN.with(|slot| slot.borrow_mut().take()) {
            hook(ctx);
            return;
        }
    }
    awili_work::spawn_worker(worker, ctx);
}

#[cfg(test)]
thread_local! {
    /// Test marker (S5 INT1-FIX2): agent-lane memory-gate answers the UI
    /// thread has processed, stale or current, so a test can wait for a late
    /// answer deterministically instead of sleeping.
    pub(crate) static TEST_AGENT_GATE_ANSWERS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// The one-shot worker-spawn replacement held by [`TEST_WORKER_SPAWN`].
#[cfg(test)]
type TestWorkerSpawn = Box<dyn FnOnce(WorkerContext)>;

#[cfg(test)]
thread_local! {
    /// Test seam (P3c round 4): a one-shot replacement for the next
    /// [`spawn_engine_worker`] on this thread; it receives the worker's
    /// context and the built worker is dropped unstarted.
    pub(crate) static TEST_WORKER_SPAWN: std::cell::RefCell<Option<TestWorkerSpawn>> =
        const { std::cell::RefCell::new(None) };
}

/// The Hermes gateway engine behind the D1 seam: the ONE [`EngineWorker`]
/// implementation (D2 adds `awili-engine` as the second). Owns connect + the
/// whole gateway protocol; only `WorkCmd`/`WorkEvent` cross the boundary.
pub struct HermesGatewayWorker {
    pub cfg: GatewayCfg,
}

impl EngineWorker for HermesGatewayWorker {
    fn run(self: Box<Self>, ctx: WorkerContext) {
        let client = match connect_gateway(&self.cfg) {
            Ok(c) => c,
            Err(e) => {
                let _ = ctx.events.send_blocking(WorkEvent::Failed(e));
                return;
            }
        };
        run_worker_loop(client, ctx);
    }
}

/// Persistent gateway worker loop: create session, then loop on commands
/// (prompts, approvals, cancel) until shutdown. The transport stays open across
/// turns so the session is multi-turn — matching the desktop app's behavior.
/// Generic over [`GatewayOps`] so the deterministic suite drives it with a
/// scripted fake; behavior is the 1:1 body of the pre-D1
/// `run_gateway_worker_persistent`, with facts (`WorkEvent`) at the send sites.
/// P3c: a turn ends on the gateway crate's [`is_turn_end`] conjunction, and a
/// mid-turn Stop interrupts (the session survives) with a
/// [`GATEWAY_STOP_DRAIN`] bound.
fn run_worker_loop<C: GatewayOps>(client: C, ctx: WorkerContext) {
    run_worker_loop_with_drain(client, ctx, GATEWAY_STOP_DRAIN);
}

/// How long a mid-turn Stop waits, after a successful `session.interrupt`, for
/// the interrupted turn's own end signals before reporting `Stopped` anyway.
const GATEWAY_STOP_DRAIN: Duration = Duration::from_secs(10);

/// How the Stop drain of an interrupted gateway turn ended (P3c round 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainOutcome {
    /// The interrupted turn's own end signals arrived: nothing of it is left
    /// queued, so the session can serve the next prompt.
    Ended,
    /// The drain bound elapsed first: late events of the interrupted turn may
    /// still arrive, so the session must not be reused.
    TimedOut,
    /// The transport dropped during the drain.
    Disconnected,
}

/// Drain the interrupted turn after `session.interrupt` succeeded, folding
/// every event through the same [`is_turn_end`] rule (P3c round 2, R3). Only a
/// typed `hook.denied` fact is forwarded (a denial that raced the Stop stays in
/// the transcript); everything else — text, tools, completion, usage — is
/// consumed here. An `approval.request` is consumed too and remembered only as
/// a fact: the drain makes NO RPC (round 3, R7), so nothing but `deadline`
/// bounds it — either the turn ends (the approval is moot) or the drain times
/// out and the session is retired. Returns when the turn ends, `deadline`
/// elapses, or the transport drops.
///
/// RC1-F4-FIX1: a `message.complete` drained here still moves the session's
/// token baseline `usage_seen` (Hermes reports running totals), so the next
/// turn books only its own share; the stopped turn's share is not booked.
fn drain_interrupted_turn<C: GatewayOps>(
    client: &C,
    turn: &mut TurnState,
    deadline: Duration,
    tx: &async_channel::Sender<WorkEvent>,
    usage_seen: &mut (u64, u64),
) -> DrainOutcome {
    let started = std::time::Instant::now();
    let mut drained_approval = false;
    while !turn.ended() {
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            if drained_approval {
                eprintln!(
                    "awili: a stopped gateway turn left an approval unanswered; retiring the session"
                );
            }
            return DrainOutcome::TimedOut;
        }
        match client.recv_event(remaining.min(Duration::from_millis(100))) {
            Some(ev) => {
                let kind = decode_event(&ev);
                let _not_booked = gateway_turn_usage(&kind, usage_seen);
                match &kind {
                    GatewayEventKind::HookDenial { .. } => {
                        for event in gateway_event_to_work_events(&ev) {
                            if matches!(event, WorkEvent::HookDenial { .. }) {
                                let _ = tx.send_blocking(event);
                            }
                        }
                    }
                    GatewayEventKind::Approval { .. } => drained_approval = true,
                    _ => {}
                }
                if is_turn_end(&kind, turn) {
                    return DrainOutcome::Ended;
                }
            }
            None => {
                if !client.is_connected() {
                    return DrainOutcome::Disconnected;
                }
            }
        }
    }
    DrainOutcome::Ended
}

/// End a gateway worker on a Stop that does not keep the session (P3c rounds
/// 2-3): drop the command receiver first, so a prompt the UI sends once it
/// sees `Stopped` fails and respawns instead of vanishing here; then report
/// `Stopped`; only then `session.close` (best effort, skipped when
/// `close_session` is false, i.e. on a dead link) and the transport close, so
/// the UI never waits on the close RPC.
fn retire_stopped_session<C: GatewayOps>(
    mut client: C,
    cmd_rx: std::sync::mpsc::Receiver<GatewayCmd>,
    tx: &async_channel::Sender<WorkEvent>,
    session_id: &str,
    close_session: bool,
) {
    drop(cmd_rx);
    let _ = tx.send_blocking(stopped_terminal());
    if close_session {
        let _ = client.session_close(session_id);
    }
    client.close();
}

/// The `Terminal { Stopped }` a gateway Stop reports on every path.
fn stopped_terminal() -> WorkEvent {
    WorkEvent::Terminal {
        status: RunStatus::Stopped,
        output: None,
        model: None,
    }
}

/// Move every command already queued on `cmd_rx` into `pending` (arrival
/// order kept) and take the first queued `Cancel` out of it (P3c round 5).
/// Returns whether a Stop was waiting; every other command stays in `pending`
/// for the loop to handle in order.
fn take_queued_cancel(
    cmd_rx: &std::sync::mpsc::Receiver<GatewayCmd>,
    pending: &mut std::collections::VecDeque<GatewayCmd>,
) -> bool {
    pending.extend(std::iter::from_fn(|| cmd_rx.try_recv().ok()));
    match pending
        .iter()
        .position(|cmd| matches!(cmd, GatewayCmd::Cancel))
    {
        Some(at) => {
            pending.remove(at);
            true
        }
        None => false,
    }
}

/// Stop the turn the backend may be running (P3c): `session.interrupt`, then
/// the bounded drain through [`is_turn_end`]. Only an `Ended` drain keeps the
/// session — `Stopped` is reported and the client and command receiver are
/// handed back for the next prompt. A failed interrupt, a timed-out drain or a
/// dropped link retire the worker through [`retire_stopped_session`]
/// (receiver dropped, `Stopped`, then any close RPC) and return `None`.
///
/// P3g: the drain folds into a FRESH [`TurnState`], never the interrupted
/// turn's: a completion or `running:false` seen before the interrupt (a
/// tool-call message's completion, say) must not end the drain early and let
/// the interrupted turn's own completion leak into the next turn. The drain
/// ends only on both signals emitted after the interrupt, or at `stop_drain`.
fn stop_turn_in_flight<C: GatewayOps>(
    client: C,
    cmd_rx: std::sync::mpsc::Receiver<GatewayCmd>,
    stop_drain: Duration,
    tx: &async_channel::Sender<WorkEvent>,
    session_id: &str,
    usage_seen: &mut (u64, u64),
) -> Option<(C, std::sync::mpsc::Receiver<GatewayCmd>)> {
    let outcome = match client.session_interrupt(session_id) {
        Ok(()) => {
            drain_interrupted_turn(&client, &mut TurnState::new(), stop_drain, tx, usage_seen)
        }
        Err(e) => {
            eprintln!("awili: gateway interrupt failed: {e}");
            retire_stopped_session(client, cmd_rx, tx, session_id, true);
            return None;
        }
    };
    match outcome {
        DrainOutcome::Ended => {
            let _ = tx.send_blocking(stopped_terminal());
            Some((client, cmd_rx))
        }
        DrainOutcome::TimedOut => {
            retire_stopped_session(client, cmd_rx, tx, session_id, true);
            None
        }
        DrainOutcome::Disconnected => {
            // No RPC on a dead link (round 2, R2).
            retire_stopped_session(client, cmd_rx, tx, session_id, false);
            None
        }
    }
}

/// RC1-F4: Hermes's `message.complete` usage is the SESSION's running total
/// (`session_input_tokens` / `session_output_tokens`), and one worker keeps
/// one session for every turn of a conversation. This turns the total into
/// the turn's own share against `seen` (the last total), or `None` when the
/// event carries no counts or the share is zero. A total that went down
/// (the gateway reset its session) counts from zero again.
pub(crate) fn gateway_turn_usage(
    kind: &GatewayEventKind,
    seen: &mut (u64, u64),
) -> Option<WorkEvent> {
    let GatewayEventKind::MessageComplete {
        input_tokens,
        output_tokens,
        ..
    } = kind
    else {
        return None;
    };
    if input_tokens.is_none() && output_tokens.is_none() {
        return None;
    }
    let total = (
        input_tokens.unwrap_or(seen.0),
        output_tokens.unwrap_or(seen.1),
    );
    let share = |now: u64, before: u64| if now >= before { now - before } else { now };
    let turn = (share(total.0, seen.0), share(total.1, seen.1));
    *seen = total;
    (turn != (0, 0)).then_some(WorkEvent::UsageReported {
        input_tokens: turn.0,
        output_tokens: turn.1,
    })
}

/// [`run_worker_loop`] with the Stop drain bound injected (tests pass a short
/// one so no test waits the production 10 s).
fn run_worker_loop_with_drain<C: GatewayOps>(
    mut client: C,
    ctx: WorkerContext,
    stop_drain: Duration,
) {
    let tx = ctx.events;
    let mut cmd_rx = ctx.cmd_rx;
    let cancel = ctx.cancel;
    // Commands read off the channel early (the round-5 Cancel checks) that
    // the loop still has to handle, in arrival order.
    let mut pending = std::collections::VecDeque::new();

    let session = match client.session_create(&ctx.workdir) {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send_blocking(WorkEvent::Failed(format!("session.create failed: {e}")));
            client.close();
            return;
        }
    };
    let session_id = session.session_id.clone();
    let _ = tx.send_blocking(WorkEvent::Started(session_id.clone()));
    // RC1-F4: this session's last reported token totals.
    let mut usage_seen = (0u64, 0u64);

    // Command loop: wait for prompts, execute turns, repeat. The loop ends
    // when the UI drops the sender (recv error).
    loop {
        let cmd = match pending.pop_front() {
            Some(cmd) => cmd,
            None => match cmd_rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => break,
            },
        };
        match cmd {
            // RC2-R617: the Hermes gateway keeps its own model cache.
            GatewayCmd::Warm { .. } => {}
            GatewayCmd::Prompt { text, .. } => {
                // P3c round 5 (R12): a Stop that is already queued — e.g.
                // pressed while `session.create` was in flight — means this
                // prompt is never submitted. Nothing reached the backend, so
                // no interrupt: retire (receiver dropped, `Stopped`, close).
                // The UI's parked prompt replays on a fresh worker.
                if take_queued_cancel(&cmd_rx, &mut pending) {
                    retire_stopped_session(client, cmd_rx, &tx, &session_id, true);
                    return;
                }
                // Stream events for this turn until the gateway's turn-end
                // conjunction (message.complete AND session.info running:false,
                // either order; `error` alone) — P3c.
                let mut turn = TurnState::new();
                if let Err(e) = client.prompt_submit(&session_id, &text) {
                    // P3c round 5 (R11): a failed or timed-out submission is
                    // uncertain — the backend may have accepted the prompt.
                    // A Stop that queued behind the RPC still interrupts and
                    // drains; only with no Stop pending is it `Failed`.
                    if take_queued_cancel(&cmd_rx, &mut pending) {
                        eprintln!("awili: gateway prompt.submit failed during a Stop: {e}");
                        match stop_turn_in_flight(
                            client,
                            cmd_rx,
                            stop_drain,
                            &tx,
                            &session_id,
                            &mut usage_seen,
                        ) {
                            Some((kept_client, kept_rx)) => {
                                client = kept_client;
                                cmd_rx = kept_rx;
                                continue;
                            }
                            None => return,
                        }
                    }
                    let _ =
                        tx.send_blocking(WorkEvent::Failed(format!("prompt.submit failed: {e}")));
                    continue;
                }
                loop {
                    if cancel.load(Ordering::SeqCst) {
                        let _ = client.session_close(&session_id);
                        client.close();
                        return;
                    }
                    // Check for more commands (approval responses arrive mid-turn).
                    if let Some(mid_cmd) = pending.pop_front().or_else(|| cmd_rx.try_recv().ok()) {
                        match mid_cmd {
                            GatewayCmd::Approval { id, choice } => {
                                let delivered =
                                    client.approval_respond(&session_id, &choice).is_ok();
                                let _ =
                                    tx.send_blocking(WorkEvent::ApprovalDelivery { id, delivered });
                            }
                            GatewayCmd::Clarify {
                                request_id: _,
                                answer,
                            } => {
                                if let Err(e) = client.clarify_respond(&session_id, &answer) {
                                    eprintln!("awili: gateway clarify_respond failed: {e}");
                                }
                            }
                            GatewayCmd::Steer(text) => {
                                if let Err(e) = client.session_steer(&session_id, &text) {
                                    eprintln!("awili: gateway steer failed: {e}");
                                }
                            }
                            GatewayCmd::Attach { path } => {
                                if let Err(e) = client.file_attach(
                                    &session_id,
                                    path.rsplit('/').next().unwrap_or(&path),
                                    &path,
                                ) {
                                    eprintln!("awili: gateway file_attach failed: {e}");
                                }
                            }
                            GatewayCmd::Cancel => {
                                // P3c: Stop interrupts the turn. Only a drain
                                // that sees the turn's own end keeps the
                                // session and this worker for the next prompt
                                // (round 2, R2); otherwise the worker ends so
                                // no stale event can reach a later turn.
                                // Every exit drops the command receiver
                                // BEFORE reporting `Stopped`, so a prompt the
                                // UI sends after that report fails and
                                // respawns instead of vanishing here.
                                // Round 3 (R7): on every exit `Stopped` is
                                // reported BEFORE any close RPC, so the UI
                                // never waits on it.
                                match stop_turn_in_flight(
                                    client,
                                    cmd_rx,
                                    stop_drain,
                                    &tx,
                                    &session_id,
                                    &mut usage_seen,
                                ) {
                                    Some((kept_client, kept_rx)) => {
                                        client = kept_client;
                                        cmd_rx = kept_rx;
                                        break;
                                    }
                                    None => return,
                                }
                            }
                            GatewayCmd::Shutdown => {
                                let _ = client.session_close(&session_id);
                                client.close();
                                return;
                            }
                            GatewayCmd::TerminalReadRespond { request_id, text } => {
                                if let Err(e) =
                                    client.terminal_read_respond(&session_id, &request_id, &text)
                                {
                                    eprintln!("awili: gateway terminal_read_respond failed: {e}");
                                }
                            }
                            // Only the gateway error is logged, never the secret.
                            GatewayCmd::SudoRespond {
                                request_id,
                                password,
                            } => {
                                if let Err(e) = client.sudo_respond(&request_id, &password) {
                                    eprintln!("awili: gateway sudo_respond failed: {e}");
                                }
                            }
                            GatewayCmd::SecretRespond { request_id, value } => {
                                if let Err(e) = client.secret_respond(&request_id, &value) {
                                    eprintln!("awili: gateway secret_respond failed: {e}");
                                }
                            }
                            GatewayCmd::Prompt { .. } => {
                                // Ignore — can't submit mid-turn.
                            }
                            // RC2-R617: Hermes keeps its own model cache.
                            GatewayCmd::Warm { .. } => {}
                        }
                    }

                    match client.recv_event(Duration::from_millis(100)) {
                        Some(ev) => {
                            let kind = decode_event(&ev);
                            if let Some(usage) = gateway_turn_usage(&kind, &mut usage_seen) {
                                if tx.send_blocking(usage).is_err() {
                                    client.close();
                                    return;
                                }
                            }
                            for event in gateway_event_to_work_events(&ev) {
                                // The loop owns turn end (P3c): the translated
                                // `running:false` -> TurnComplete fact is not
                                // forwarded, or a turn would end twice (or
                                // early, when running:false precedes the final
                                // message.complete). See P3c-DEVIATIONS.md.
                                if event == WorkEvent::TurnComplete {
                                    continue;
                                }
                                if tx.send_blocking(event).is_err() {
                                    client.close();
                                    return;
                                }
                            }
                            if is_turn_end(&kind, &mut turn) {
                                // Turn complete — signal the UI, then wait for
                                // the next command (multi-turn).
                                let _ = tx.send_blocking(WorkEvent::TurnComplete);
                                break;
                            }
                        }
                        None => {
                            if !client.is_connected() {
                                let _ = tx.send_blocking(WorkEvent::Failed(
                                    "gateway connection lost".to_string(),
                                ));
                                return;
                            }
                        }
                    }
                }
            }
            GatewayCmd::Approval { id, choice } => {
                let delivered = client.approval_respond(&session_id, &choice).is_ok();
                let _ = tx.send_blocking(WorkEvent::ApprovalDelivery { id, delivered });
            }
            GatewayCmd::Clarify {
                request_id: _,
                answer,
            } => {
                if let Err(e) = client.clarify_respond(&session_id, &answer) {
                    eprintln!("awili: gateway clarify_respond failed: {e}");
                }
            }
            GatewayCmd::TerminalReadRespond { request_id, text } => {
                if let Err(e) = client.terminal_read_respond(&session_id, &request_id, &text) {
                    eprintln!("awili: gateway terminal_read_respond failed: {e}");
                }
            }
            GatewayCmd::SudoRespond {
                request_id,
                password,
            } => {
                if let Err(e) = client.sudo_respond(&request_id, &password) {
                    eprintln!("awili: gateway sudo_respond failed: {e}");
                }
            }
            GatewayCmd::SecretRespond { request_id, value } => {
                if let Err(e) = client.secret_respond(&request_id, &value) {
                    eprintln!("awili: gateway secret_respond failed: {e}");
                }
            }
            GatewayCmd::Steer(text) => {
                if let Err(e) = client.session_steer(&session_id, &text) {
                    eprintln!("awili: gateway steer failed: {e}");
                }
            }
            GatewayCmd::Attach { path } => {
                if let Err(e) =
                    client.file_attach(&session_id, path.rsplit('/').next().unwrap_or(&path), &path)
                {
                    eprintln!("awili: gateway file_attach failed: {e}");
                }
            }
            GatewayCmd::Cancel => {
                // A Stop that reached an idle worker (its turn ended or
                // failed first). P3c round 5 (R11): interrupt best-effort
                // first — a no-op on an idle backend, and the D4 fix when a
                // timed-out submission left the backend working — then close
                // as before. The receiver is dropped before `Stopped` is
                // reported so a prompt sent after it respawns (round 2), and
                // `Stopped` goes out before the close RPC (round 3).
                let _ = client.session_interrupt(&session_id);
                retire_stopped_session(client, cmd_rx, &tx, &session_id, true);
                return;
            }
            GatewayCmd::Shutdown => {
                let _ = client.session_close(&session_id);
                break;
            }
        }
    }

    client.close();
}

/// REST fallback worker: `/v1/runs` + SSE streaming (the pre-gateway path).
/// Map a gateway `message.complete` status string to a [`RunStatus`].
/// Hermes uses "complete"; we also accept the obvious failure/stop spellings.
/// Unknown strings fall back to `Completed` (the turn did finish, just without a
/// status we recognise) rather than poisoning the UI with a failure.
fn parse_complete_status(s: &str) -> RunStatus {
    match s.to_ascii_lowercase().as_str() {
        "complete" | "completed" | "success" | "ok" | "done" => RunStatus::Completed,
        "error" | "failed" | "failure" => RunStatus::Failed,
        "stopped" | "cancelled" | "canceled" | "interrupted" => RunStatus::Stopped,
        _ => RunStatus::Completed,
    }
}

/// A short label for a run status, used in the header / error text.
pub(crate) fn status_label(s: RunStatus) -> &'static str {
    match s {
        RunStatus::Started => "started",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Stopped => "stopped",
        RunStatus::Unknown => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Subagent render helpers (unit 19). Pure — the tree/fold live in unit 18's
// `awili_gateway::subagents`; these classify a reducer status string and roll up
// the store totals for the AGENTS card header.
// ---------------------------------------------------------------------------

/// Wall-clock milliseconds since the Unix epoch. Drives each subagent's arrival
/// timestamps (hence its elapsed timer) as events are folded on the UI thread.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Whether a subagent's reducer status is terminal (no longer running). The
/// reducer emits `complete` for the Complete phase by default, but a
/// backend-explicit `status` field overrides it — so any settled word must
/// count as done, not just `complete`, or the header/glyph would show a
/// finished agent as active. Anything unrecognized (e.g. `running`, `pending`,
/// `thinking`) is treated as still active.
fn subagent_is_done(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "complete" | "completed" | "done" | "finished" | "success" | "succeeded" | "ok" | "stopped"
    ) || subagent_is_error(status)
}

/// Whether a terminal status specifically indicates failure (drives the ✗ glyph
/// and the critical color, distinct from a clean ✓ completion).
fn subagent_is_error(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "failed" | "error" | "cancelled" | "canceled" | "timeout" | "timed_out" | "timedout"
    )
}

/// Roll up the store for the AGENTS card header: total input tokens, total
/// output tokens, total cost (USD). Absent fields contribute nothing.
fn subagent_totals(store: &[SubagentProgress]) -> (u64, u64, f64) {
    let mut in_tok = 0u64;
    let mut out_tok = 0u64;
    let mut cost = 0.0f64;
    for a in store {
        in_tok += a.input_tokens.unwrap_or(0);
        out_tok += a.output_tokens.unwrap_or(0);
        cost += a.cost_usd.unwrap_or(0.0);
    }
    (in_tok, out_tok, cost)
}

/// A small pill button. `filled` fills with `border` color (for the primary Allow);
/// otherwise it is an outlined button in `text` color.
pub(crate) fn pill_button(
    id: &'static str,
    label: &'static str,
    border: gpui::Rgba,
    text: gpui::Rgba,
    filled: bool,
) -> gpui::Stateful<gpui::Div> {
    let base = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .px(theme::z(12.))
        .py(theme::z(6.))
        .rounded(theme::z(theme::RADIUS))
        .border_1()
        .border_color(border)
        .text_size(theme::z(theme::LABEL_SIZE))
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|s| s.border_color(theme::p().accent))
        .child(label);
    if filled {
        base.bg(border).text_color(text)
    } else {
        base.text_color(text)
    }
}

/// Open the complete target captured from the owning job's preview row.
fn open_job_preview(target: &str) {
    #[cfg(test)]
    OPENED_JOB_PREVIEWS.with(|opened| opened.borrow_mut().push(target.to_string()));
    #[cfg(not(test))]
    {
        let _ = open::that_detached(target);
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static OPENED_JOB_PREVIEWS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// One checklist row: a state marker + the step label.
fn step_row(index: usize, step: &ProgressStep) -> impl IntoElement {
    let state = match step.state {
        StepState::Done => "Finished",
        StepState::Running => "Running",
        StepState::Pending => "Pending",
    };
    crate::ui::Row::new(format!("job-step-{index}"))
        .leading(crate::ui::label((index + 1).to_string()).dim())
        .title(step.label.clone())
        .subtitle(state)
        .strong()
        .wrap()
}

#[cfg(test)]
mod tests {
    // ---- P3a acceptance (1), (4), (5): app-side gating and denial rendering ----

    fn card(risk: Option<&str>) -> super::ApprovalCard {
        use super::ApprovalRisk;
        // S2(a): the card carries the CLOSED risk class — the helper maps the
        // wire string through the exact decode the gateway uses (conservative:
        // `None` -> `Privileged`, unknown -> `Privileged`).
        let risk = match risk {
            None => ApprovalRisk::Privileged,
            Some(wire) => ApprovalRisk::decode(Some(wire)),
        };
        super::ApprovalCard {
            approval_id: "a1".into(),
            kind: "approval".into(),
            prompt: "run thing".into(),
            risk,
            delivery: super::ApprovalDelivery::default(),
        }
    }

    /// Acceptance (1): every offerable choice resolves to its EXACT scope
    /// string and nothing else.
    #[test]
    fn p3a_offerable_choices_resolve_to_exact_scope_strings() {
        use super::ApprovalDecision as D;
        let c = card(Some("mutating"));
        assert_eq!(
            super::resolve_approval_choice(&c, D::Approve, true),
            Some("once")
        );
        assert_eq!(
            super::resolve_approval_choice(&c, D::Session, true),
            Some("session")
        );
        assert_eq!(
            super::resolve_approval_choice(&c, D::Folder, true),
            Some("folder")
        );
        assert_eq!(
            super::resolve_approval_choice(&c, D::Always, true),
            Some("always")
        );
        assert_eq!(
            super::resolve_approval_choice(&c, D::Deny, true),
            Some("deny")
        );
    }

    /// Acceptance (1)+(3): `folder` is refused without a bound folder and
    /// `always` is refused for privileged/irreversible risk — refused means
    /// `None`, never relabeled to `once`.
    #[test]
    fn p3a_unofferable_choices_are_refused_not_relabeled() {
        use super::ApprovalDecision as D;
        let c = card(Some("mutating"));
        assert_eq!(super::resolve_approval_choice(&c, D::Folder, false), None);
        assert!(c.offers(D::Approve, false) && c.offers(D::Deny, false));
        for risk in ["privileged", "irreversible"] {
            let r = card(Some(risk));
            assert_eq!(
                super::resolve_approval_choice(&r, D::Always, true),
                None,
                "{risk}"
            );
            assert_eq!(
                super::resolve_approval_choice(&r, D::Session, true),
                Some("session")
            );
            assert_eq!(
                super::resolve_approval_choice(&r, D::Approve, true),
                Some("once")
            );
        }
        // S2(a): an unreported / unknown risk classifies as the conservative
        // `Privileged` closed-set member, which REFUSES `always` (the
        // authorizer re-checks the floor at mint time; the card never offers
        // what can never be granted).
        assert!(!card(None).offers(D::Always, true));
    }

    /// S2(a) required proof: the `always` offering follows the TYPED risk
    /// class on the card — offered ONLY for `ReadOnly`/`Mutating`, refused
    /// for `Irreversible`/`Privileged` (and the conservative default the
    /// gateway decode produces for a missing/unknown wire value).
    #[test]
    fn approval_scopes_follow_typed_risk() {
        use super::ApprovalDecision as D;
        use super::ApprovalRisk;
        // The two reversible classes offer `always` (and the other scopes).
        for (wire, class) in [
            ("read_only", ApprovalRisk::ReadOnly),
            ("mutating", ApprovalRisk::Mutating),
        ] {
            let c = card(Some(wire));
            assert_eq!(c.risk, class, "card risk {wire}");
            assert!(c.offers(D::Always, true), "always offered for {wire}");
            assert!(c.offers(D::Approve, true));
            assert!(c.offers(D::Session, true));
        }
        // The two irreversible/privileged classes refuse `always`.
        for (wire, class) in [
            ("irreversible", ApprovalRisk::Irreversible),
            ("privileged", ApprovalRisk::Privileged),
        ] {
            let c = card(Some(wire));
            assert_eq!(c.risk, class, "card risk {wire}");
            assert!(!c.offers(D::Always, true), "always refused for {wire}");
            // The refused scope is NOT relabeled: `resolve_approval_choice`
            // returns `None` (no command is sent).
            assert_eq!(super::resolve_approval_choice(&c, D::Always, true), None);
            assert_eq!(
                super::resolve_approval_choice(&c, D::Session, true),
                Some("session")
            );
        }
        // The conservative default (missing / unknown wire value) also
        // classifies as `Privileged` and refuses `always` too.
        for wire in [None, Some("totally_unknown")] {
            let c = card(wire);
            assert_eq!(
                c.risk,
                ApprovalRisk::Privileged,
                "conservative class for {wire:?}"
            );
            assert!(!c.offers(D::Always, true), "always refused for {wire:?}");
            assert_eq!(super::resolve_approval_choice(&c, D::Always, true), None);
        }
        // RVS2A-FIX1: the FULL scope matrix, per risk class, through the card
        // built directly (the gateway-frame path is pinned by
        // `gateway_approval_risk_reaches_card_through_reducer`).
        for variant in [
            ApprovalRisk::ReadOnly,
            ApprovalRisk::Mutating,
            ApprovalRisk::Irreversible,
            ApprovalRisk::Privileged,
        ] {
            let c = ApprovalCard {
                approval_id: "a1".into(),
                kind: "approval".into(),
                prompt: "p".into(),
                risk: variant,
                delivery: Default::default(),
            };
            assert_scope_matrix(&c, variant);
        }
    }

    /// Acceptance (4)+(5): a typed denial renders source/reason/recovery
    /// verbatim; an absent recovery is explicit; never a generic failure.
    #[test]
    fn p3a_hook_denial_renders_verbatim_and_absent_recovery_is_explicit() {
        let with = awili_work::WorkEvent::HookDenial {
            source: "foreman-gate".into(),
            reason: "memory not checked".into(),
            recovery: Some("run recall first".into()),
        };
        let ups = super::work_event_to_updates(with);
        assert_eq!(ups.len(), 1);
        match &ups[0] {
            super::WorkUpdate::Token(t) => {
                assert_eq!(
                    t,
                    "Blocked by foreman-gate: memory not checked\nrun recall first\n"
                );
            }
            other => panic!("expected Token, got {other:?}"),
        }
        let without = awili_work::WorkEvent::HookDenial {
            source: "hook".into(),
            reason: "denied".into(),
            recovery: None,
        };
        let ups = super::work_event_to_updates(without);
        match &ups[0] {
            super::WorkUpdate::Token(t) => {
                assert_eq!(
                    t,
                    "Blocked by hook: denied\nNo automatic recovery supplied\n"
                );
                assert!(!t.contains("Failed"));
            }
            other => panic!("expected Token, got {other:?}"),
        }
    }

    /// P10/F1: the owned `awili` engine must receive the ORIGINAL approval id
    /// (it correlates the verdict on that id and drops any other), and a
    /// successful send clears the card.
    #[test]
    fn owned_engine_approval_sends_original_id_and_clears_card() {
        let mut c = card(Some("mutating"));
        c.approval_id = "abc-1".into();
        let (tx, rx) = std::sync::mpsc::channel();
        let cleared = super::route_work_approval(
            awili_config::EngineKind::Awili,
            &mut c,
            "session",
            Some(&tx),
        );
        assert!(cleared, "a delivered owned-engine verdict clears the card");
        match rx.try_recv().expect("exactly one command") {
            super::GatewayCmd::Approval { id, choice } => {
                assert_eq!(id.as_deref(), Some("abc-1"));
                assert_eq!(choice, "session");
            }
            _ => panic!("expected Approval"),
        }
        assert!(rx.try_recv().is_err(), "exactly one command");
        assert!(!c.delivery.failed);
    }

    /// P10/F1: the Hermes path keeps the delivery-ack handshake — a local
    /// delivery id is minted (the backend id is ignored), and the card stays
    /// up until the worker acknowledges, exactly as the `approval_delivery_*`
    /// tests assert.
    #[test]
    fn hermes_approval_uses_delivery_ack_path() {
        let mut c = card(Some("mutating"));
        c.approval_id = "abc-1".into();
        let (tx, rx) = std::sync::mpsc::channel();
        let cleared = super::route_work_approval(
            awili_config::EngineKind::HermesGateway,
            &mut c,
            "once",
            Some(&tx),
        );
        assert!(!cleared, "Hermes waits for the delivery ack");
        match rx.try_recv().expect("exactly one command") {
            super::GatewayCmd::Approval { id, choice } => {
                let id = id.expect("a local delivery id is minted");
                assert!(id.starts_with("awili-delivery-"), "{id}");
                assert_ne!(id, "abc-1", "Hermes delivery uses its own token");
                assert_eq!(choice, "once");
            }
            _ => panic!("expected Approval"),
        }
        assert!(c.delivery.pending.is_some(), "ack still pending");
        assert!(!c.delivery.failed);
    }

    /// RC1-N (pre-review P2, pass 10): the in-app Deny reply is one line —
    /// even when a file the turn changed before the card has a newline in its
    /// name — and names what was declined and the earlier change.
    #[test]
    fn rc1n_the_deny_line_is_one_line() {
        let line = super::deny_stop_line(
            "Write b.txt (2 characters)\n\n```\nraw\n```",
            "changed a\nb.txt",
            true,
        );
        assert!(!line.contains('\n') && !line.contains('\r'), "{line:?}");
        // Pre-review P3 (pass 11): Unicode line separators too.
        let sep = super::deny_stop_line("Write x", "changed a\u{2028}b\u{2029}c.txt", true);
        assert!(!sep.contains(['\u{2028}', '\u{2029}']), "{sep:?}");
        // Pre-review P3 (pass 13): separators in the card's text too.
        let card =
            super::deny_stop_line("run_command a\u{2028}b\u{2029}c", "no files changed", false);
        assert!(!card.contains(['\u{2028}', '\u{2029}', '\n']), "{card:?}");
        assert_eq!(
            line,
            "Stopped \u{2014} you declined \u{201C}Write b.txt (2 characters)\u{201D}, so it was \
             not done. Before that, this turn changed a b.txt."
        );
        assert_eq!(
            super::deny_stop_line("Write b.txt (2 characters)", "no files changed", false),
            "Okay \u{2014} you declined \u{201C}Write b.txt (2 characters)\u{201D}, so it was not done."
        );
    }

    /// The agent's final notice preserves a terminal tool error verbatim —
    /// exactly once, with no assistant text grafted onto it — and, without a
    /// terminal error, adds nothing (RC1-N: was a possibly-recovered tool
    /// attempt warning).
    #[test]
    fn agent_final_notice_preserves_terminal_error_without_assistant_text() {
        for error in [
            "Blocked by Claude CLI permission policy: permission denied for Bash.",
            "the coding engine exited with code 2",
        ] {
            let panel = super::RunPanel {
                tool_attempt_failed: true,
                error: Some(error.into()),
                ..Default::default()
            };
            for prior_text in ["", "partial answer"] {
                let transcript = format!("{prior_text}{}", panel.agent_final_notice().unwrap());
                assert!(transcript.contains(error));
                assert!(!transcript.contains("may have recovered"));
                assert_eq!(transcript.matches(error).count(), 1);
            }
        }
        let mut recovered = super::RunPanel::default();
        assert!(recovered.agent_final_notice().is_none());
        // RC1-N (round 4 Minor 5): a recovered tool attempt adds nothing to
        // the reply.
        recovered.tool_attempt_failed = true;
        assert!(recovered.agent_final_notice().is_none());
        assert!(recovered.error.is_none());
    }

    /// Gate row 19 (unit 4b): a dead origin is reconnected by the NEXT
    /// turn-open re-drive — one idempotent `ConnectOrigin` per pass, never
    /// a retry loop inside a pass.
    #[test]
    fn turn_open_redrive_reconnects_a_dead_origin_without_a_retry_loop() {
        use crate::mcp_catalog::{DeclaredServerPolicy, WorkMcpState};

        let (sink, stream) = awili_tools::runtime::event_channel::<String>(16);
        let (ctx, crx) = async_channel::bounded::<awili_tools::runtime::McpCommand<String>>(8);
        let handle = awili_tools::runtime::McpRuntimeHandle::from_parts(
            ctx,
            stream,
            std::thread::spawn(|| {}),
        );
        let coord = awili_engine_worker::mcp_coordinator::McpCoordinator::new(handle);
        let facade = coord.facade();
        let origin = awili_tools::ToolOrigin::new("mcp:t");
        let mut alloc = awili_engine::generation::GenerationAllocator::new();
        let g = alloc.next();

        // The pump observes a death.
        sink.send_blocking(awili_tools::runtime::McpEvent::ServerDied {
            origin: origin.clone(),
            generation: g,
            cause: awili_tools::runtime::DeathCause::TransportFailed,
            selected_class: None,
            detail: "died".into(),
        })
        .expect("emit died");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let snapshot = facade.lifecycle();
            if matches!(
                snapshot.origins.get("mcp:t"),
                Some(awili_engine_worker::mcp_coordinator::OriginStatus::Dead { .. })
            ) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "death never ingested");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        // ONE drive pass: exactly ONE ConnectOrigin reaches the runtime.
        let state = WorkMcpState::new();
        crate::mcp_catalog::drive_reconcile_once(
            &facade,
            &state,
            std::slice::from_ref(&origin),
            "/work",
            &DeclaredServerPolicy,
        );
        let mut connects = 0;
        while let Ok(cmd) = crx.try_recv() {
            if matches!(cmd, awili_tools::runtime::McpCommand::ConnectOrigin { .. }) {
                connects += 1;
            }
        }
        assert_eq!(
            connects, 1,
            "one pass, one idempotent connect — no retry loop"
        );

        // The NEXT pass (the runtime still shows Dead in this scripted rig)
        // re-drives exactly one more — re-drive semantics, still no loop.
        crate::mcp_catalog::drive_reconcile_once(
            &facade,
            &state,
            std::slice::from_ref(&origin),
            "/work",
            &DeclaredServerPolicy,
        );
        let mut connects = 0;
        while let Ok(cmd) = crx.try_recv() {
            if matches!(cmd, awili_tools::runtime::McpCommand::ConnectOrigin { .. }) {
                connects += 1;
            }
        }
        assert_eq!(connects, 1, "the re-drive is per-pass, not a loop");
        drop(sink);
    }

    use super::*;
    use awili_gateway::run_events::{
        decode_run_event, ApprovalEvent, CompletionEvent, PlanEvent, TokenEvent, ToolProgressEvent,
    };

    fn plan(steps: &[&str]) -> RunEvent {
        RunEvent::Plan(PlanEvent {
            steps: steps.iter().map(|s| s.to_string()).collect(),
            text: None,
        })
    }
    fn tool(status: &str) -> RunEvent {
        RunEvent::ToolProgress(ToolProgressEvent {
            tool: Some("bash".into()),
            status: Some(status.into()),
            message: Some(format!("bash {status}")),
        })
    }

    #[test]
    fn skills_surface_prefers_dashboard_gateway_when_enabled() {
        // Regression for user-test 2026-07-01 bug #2: skills/toolsets routes live
        // on the dashboard gateway (:9119), not the Hermes REST server (:8642)
        // that serves jobs/sessions — so hitting the Hermes base 404'd.
        let gw = GatewayCfg {
            dashboard_url: "http://127.0.0.1:9119".into(),
            enabled: true,
        };
        let hermes = HermesCfg {
            dashboard_url: String::new(),
            base_url: "http://127.0.0.1:8642/v1".into(),
            api_key: "k".into(),
            enabled: true,
        };
        assert_eq!(skills_surface_base(&gw, &hermes), "http://127.0.0.1:9119");
    }

    #[test]
    fn skills_surface_falls_back_to_hermes_when_gateway_disabled() {
        let gw = GatewayCfg {
            dashboard_url: String::new(),
            enabled: false,
        };
        let hermes = HermesCfg {
            dashboard_url: String::new(),
            base_url: "http://127.0.0.1:8642/v1".into(),
            api_key: "k".into(),
            enabled: true,
        };
        assert_eq!(
            skills_surface_base(&gw, &hermes),
            "http://127.0.0.1:8642/v1"
        );
    }

    #[test]
    fn plan_builds_checklist_with_first_running() {
        let mut p = RunPanel::default();
        p.apply_event(&plan(&["read", "edit", "verify"]));
        assert_eq!(p.steps.len(), 3);
        assert_eq!(p.steps[0].state, StepState::Running);
        assert_eq!(p.steps[1].state, StepState::Pending);
        assert_eq!(p.steps[2].state, StepState::Pending);
    }

    #[test]
    fn tool_completion_advances_to_next_step() {
        let mut p = RunPanel::default();
        p.apply_event(&plan(&["read", "edit", "verify"]));
        p.apply_event(&tool("completed"));
        assert_eq!(p.steps[0].state, StepState::Done);
        assert_eq!(p.steps[1].state, StepState::Running);
        assert_eq!(p.steps[2].state, StepState::Pending);
        // The running tool message is surfaced as the activity line.
        assert_eq!(p.activity.as_deref(), Some("bash completed"));
    }

    #[test]
    fn tool_running_status_keeps_a_step_running_without_advancing() {
        let mut p = RunPanel::default();
        p.apply_event(&plan(&["a", "b"]));
        p.apply_event(&tool("running"));
        assert_eq!(p.steps[0].state, StepState::Running);
        assert_eq!(p.steps[1].state, StepState::Pending);
    }

    #[test]
    fn approval_event_raises_card_and_completion_clears_it() {
        let mut p = RunPanel::default();
        p.apply_event(&plan(&["a"]));
        p.apply_event(&RunEvent::Approval(ApprovalEvent {
            approval_id: Some("ap1".into()),
            kind: Some("deletion".into()),
            prompt: Some("Delete build/?".into()),
            // S2(a): no risk reported by the engine -> the lenient decode
            // classifies it as the conservative `Privileged` class on the
            // card, never an unclassified string.
            ..Default::default()
        }));
        let card = p.approval.clone().expect("approval card raised");
        assert_eq!(card.approval_id, "ap1");
        assert_eq!(card.kind, "deletion");
        assert_eq!(card.risk, ApprovalRisk::Privileged);
        assert!(!card.offers(ApprovalDecision::Always, true));
        p.apply_event(&RunEvent::Completion(CompletionEvent {
            status: Some(RunStatus::Completed),
            output: Some("done".into()),
        }));
        assert!(p.approval.is_none(), "completion clears the approval card");
    }

    #[test]
    fn tokens_stream_into_result_until_output_set() {
        let mut p = RunPanel::default();
        p.apply_event(&RunEvent::Token(TokenEvent { text: "Hel".into() }));
        p.apply_event(&RunEvent::Token(TokenEvent { text: "lo".into() }));
        assert_eq!(p.result_text(), Some("Hello"));
        p.apply_event(&RunEvent::Completion(CompletionEvent {
            status: Some(RunStatus::Completed),
            output: Some("Hello, world".into()),
        }));
        // Explicit output wins over the streamed buffer.
        assert_eq!(p.result_text(), Some("Hello, world"));
    }

    #[test]
    fn completion_marks_every_step_done() {
        let mut p = RunPanel::default();
        p.apply_event(&plan(&["a", "b", "c"]));
        p.apply_event(&RunEvent::Completion(CompletionEvent {
            status: Some(RunStatus::Completed),
            output: None,
        }));
        assert!(p.steps.iter().all(|s| s.state == StepState::Done));
        assert_eq!(p.status, Some(RunStatus::Completed));
    }

    // ---- P3i: a failed Completion keeps its reason in `error` ----

    #[test]
    fn failed_completion_moves_reason_into_error() {
        let mut p = RunPanel::default();
        p.apply_event(&RunEvent::Completion(CompletionEvent {
            status: Some(RunStatus::Failed),
            output: Some("sandbox: path outside session root".into()),
        }));
        assert_eq!(
            p.error.as_deref(),
            Some("sandbox: path outside session root")
        );
    }

    #[test]
    fn completed_completion_leaves_error_none() {
        let mut p = RunPanel::default();
        p.apply_event(&RunEvent::Completion(CompletionEvent {
            status: Some(RunStatus::Completed),
            output: Some("done".into()),
        }));
        assert!(p.error.is_none());
    }

    #[test]
    fn stopped_completion_leaves_error_none() {
        let mut p = RunPanel::default();
        p.apply_event(&RunEvent::Completion(CompletionEvent {
            status: Some(RunStatus::Stopped),
            output: Some("interrupted".into()),
        }));
        assert!(p.error.is_none());
    }

    #[test]
    fn unknown_event_is_inert() {
        let mut p = RunPanel::default();
        p.apply_event(&plan(&["a"]));
        let before = p.steps.clone();
        let unknown = decode_run_event(None, r#"{"type":"heartbeat","seq":7}"#);
        assert!(matches!(unknown, RunEvent::Unknown { .. }));
        p.apply_event(&unknown);
        assert_eq!(p.steps, before);
    }

    // ---- Gateway event mapping tests ----

    pub(super) fn gw_event(event_type: &str, payload: serde_json::Value) -> GatewayEvent {
        GatewayEvent {
            event_type: event_type.to_string(),
            session_id: Some("test-sess".to_string()),
            payload: Some(payload),
        }
    }

    /// The FULL scope-offer matrix for one card (RVS2A-FIX2): EVERY scope
    /// — `Approve`/allow-once, `Session`, `Folder`, `Always`, `Deny` — is
    /// checked for EVERY risk variant, with the folder BOUND and UNBOUND,
    /// asserting BOTH (a) which scopes `offers` says are offered and (b) the
    /// EXACT result of `resolve_approval_choice` for each offered scope (and
    /// the `None` refusal for each non-offered one). `Approve`, `Session`
    /// and `Deny` are always offered (bound or unbound); `Folder` exactly
    /// when a folder is bound; `Always` exactly for the two reversible
    /// classes (`ReadOnly`/`Mutating`), whatever `folder_bound` is (the
    /// conservative `Privileged` default refuses it too).
    fn assert_scope_matrix(c: &ApprovalCard, risk: ApprovalRisk) {
        use super::ApprovalDecision as D;
        let resolve = |d: D, bound: bool| super::resolve_approval_choice(c, d, bound);
        // Approve (allow-once): always offered, exact scope `"once"`.
        assert!(c.offers(D::Approve, true), "{risk:?} approve bound");
        assert!(c.offers(D::Approve, false), "{risk:?} approve unbound");
        assert_eq!(resolve(D::Approve, true), Some("once"), "{risk:?} approve");
        assert_eq!(resolve(D::Approve, false), Some("once"), "{risk:?} approve");
        // Session: always offered, exact scope `"session"`.
        assert!(c.offers(D::Session, true), "{risk:?} session bound");
        assert!(c.offers(D::Session, false), "{risk:?} session unbound");
        assert_eq!(
            resolve(D::Session, true),
            Some("session"),
            "{risk:?} session"
        );
        assert_eq!(
            resolve(D::Session, false),
            Some("session"),
            "{risk:?} session"
        );
        // Folder: offered exactly when a folder is bound.
        assert!(c.offers(D::Folder, true), "{risk:?} folder bound");
        assert!(!c.offers(D::Folder, false), "{risk:?} folder unbound");
        assert_eq!(
            resolve(D::Folder, true),
            Some("folder"),
            "{risk:?} folder bound"
        );
        assert_eq!(
            resolve(D::Folder, false),
            None,
            "{risk:?} folder unbound refused"
        );
        // Always: offered exactly for the two reversible classes, whatever
        // `folder_bound` is; the refusal is never relabeled.
        let always_expected = matches!(risk, ApprovalRisk::ReadOnly | ApprovalRisk::Mutating);
        for bound in [true, false] {
            let bound_label = if bound { "bound" } else { "unbound" };
            assert_eq!(
                c.offers(D::Always, bound),
                always_expected,
                "{risk:?} always ({bound_label} folder)"
            );
            let expected = if always_expected {
                Some("always")
            } else {
                None
            };
            assert_eq!(
                resolve(D::Always, bound),
                expected,
                "{risk:?} always ({bound_label} folder)"
            );
        }
        // Deny: always offered, exact scope `"deny"`.
        assert!(c.offers(D::Deny, true), "{risk:?} deny bound");
        assert!(c.offers(D::Deny, false), "{risk:?} deny unbound");
        assert_eq!(resolve(D::Deny, true), Some("deny"), "{risk:?} deny");
        assert_eq!(resolve(D::Deny, false), Some("deny"), "{risk:?} deny");
    }

    /// RVS2A-FIX1/FIX2 required proof: the typed risk survives the FULL path
    /// — a raw gateway frame decodes, the seam carries the TYPED enum (no
    /// string round-trip), and the REDUCER raises the card with the same
    /// closed class, offering exactly the scopes the matrix allows.
    ///
    /// RVS2A-FIX2: the reducer is driven with EVERY input class the gateway
    /// may send — case-insensitive and whitespace-padded wire values, an
    /// unknown string, non-string values (number / object / `null`), a
    /// missing risk, and BOTH alternate risk keys — plus the owned-engine
    /// seam (`WorkEvent::ApprovalRequested`) end to end.
    #[test]
    fn gateway_approval_risk_reaches_card_through_reducer() {
        // Gateway path: raw `approval.request` frames, each driven through
        // the REAL reducer (seam decode -> updates -> `apply_event`).
        let cases: [(&str, serde_json::Value, ApprovalRisk); 12] = [
            // Missing risk: the conservative `Privileged` end to end.
            (
                "absent risk key",
                serde_json::json!({ "command": "x" }),
                ApprovalRisk::Privileged,
            ),
            // Case variations of the closed wire vocabulary.
            (
                "uppercase read_only",
                serde_json::json!({ "command": "x", "risk": "READ_ONLY" }),
                ApprovalRisk::ReadOnly,
            ),
            (
                "surrounding whitespace",
                serde_json::json!({ "command": "x", "risk": "  Mutating " }),
                ApprovalRisk::Mutating,
            ),
            (
                "mixed-case irreversible",
                serde_json::json!({ "command": "x", "risk": "Irreversible" }),
                ApprovalRisk::Irreversible,
            ),
            (
                "uppercase privileged",
                serde_json::json!({ "command": "x", "risk": "PRIVILEGED" }),
                ApprovalRisk::Privileged,
            ),
            // Unknown string: refused by the closed set, conservative decode.
            (
                "unknown string",
                serde_json::json!({ "command": "x", "risk": "totally_unknown" }),
                ApprovalRisk::Privileged,
            ),
            // Non-string values: all classify conservatively as `Privileged`.
            (
                "number",
                serde_json::json!({ "command": "x", "risk": 42 }),
                ApprovalRisk::Privileged,
            ),
            (
                "object",
                serde_json::json!({ "command": "x", "risk": { "risk": "read_only" } }),
                ApprovalRisk::Privileged,
            ),
            (
                "null",
                serde_json::json!({ "command": "x", "risk": null }),
                ApprovalRisk::Privileged,
            ),
            // BOTH alternate risk keys the gateway may send.
            (
                "risk_class key",
                serde_json::json!({ "command": "x", "risk_class": "read_only" }),
                ApprovalRisk::ReadOnly,
            ),
            (
                "riskClass key",
                serde_json::json!({ "command": "x", "riskClass": "MUTATING" }),
                ApprovalRisk::Mutating,
            ),
            // Plain lowercase wire value (the canonical form).
            (
                "plain read_only",
                serde_json::json!({ "command": "x", "risk": "read_only" }),
                ApprovalRisk::ReadOnly,
            ),
        ];
        for (label, payload, expected) in cases {
            let ev = gw_event("approval.request", payload);
            let facts = super::gateway_event_to_work_events(&ev);
            assert_eq!(facts.len(), 1, "one seam fact for {label}");
            let seam_risk = match &facts[0] {
                WorkEvent::ApprovalRequested { risk, .. } => *risk,
                other => panic!("expected ApprovalRequested for {label}, got {other:?}"),
            };
            assert_eq!(seam_risk, expected, "seam risk for {label}");
            let mut p = RunPanel::default();
            // RVS2A-FIX3: drive the REDUCER with EVERY update the frame
            // produces (in order, nothing filtered) — not just the
            // `Approval` event — so a later clearing event can no longer be
            // hidden from the proof loop.
            for up in super::work_event_to_updates(facts[0].clone()) {
                if let WorkUpdate::Event(re) = &up {
                    p.apply_event(re);
                }
            }
            let card = p
                .approval
                .clone()
                .unwrap_or_else(|| panic!("card raised through the reducer for {label}"));
            assert_eq!(card.risk, expected, "card risk for {label}");
            assert_scope_matrix(&card, expected);
            // Unknown / non-string / missing risk conservatively decodes to
            // `Privileged` and must NEVER offer `always`.
            if expected == ApprovalRisk::Privileged {
                assert!(
                    !card.offers(super::ApprovalDecision::Always, true),
                    "{label} must not offer always"
                );
            }
        }
        // Owned-engine seam path: the owned `awili` engine's approval event
        // (already TYPED) must reach the card with the same closed class.
        for variant in [
            ApprovalRisk::ReadOnly,
            ApprovalRisk::Mutating,
            ApprovalRisk::Irreversible,
            ApprovalRisk::Privileged,
        ] {
            let ev = WorkEvent::ApprovalRequested {
                approval_id: Some("o1".into()),
                command: None,
                description: Some("d".into()),
                allow_permanent: false,
                risk: variant,
            };
            let mut p = RunPanel::default();
            // RVS2A-FIX3: EVERY update the seam fact produces is driven
            // through the reducer in order (nothing filtered).
            for up in super::work_event_to_updates(ev) {
                if let WorkUpdate::Event(re) = &up {
                    p.apply_event(re);
                }
            }
            let card = p
                .approval
                .clone()
                .unwrap_or_else(|| panic!("card raised through the reducer for {variant:?}"));
            assert_eq!(card.risk, variant, "owned-engine seam risk {variant:?}");
            assert_scope_matrix(&card, variant);
            if variant == ApprovalRisk::Privileged {
                assert!(
                    !card.offers(super::ApprovalDecision::Always, true),
                    "{variant:?} must not offer always"
                );
            }
        }
        // RVS2A-FIX4: the clearing scenario must go through gateway frames,
        // not constructed `WorkEvent`s. Build the RAW `approval.request` and
        // `message.complete` frames with the same JSON frame helper the other
        // two proof loops use, decode them with the SAME gateway decoder
        // (`gateway_event_to_work_events`), and push every resulting
        // `WorkUpdate::Event(re)` through the SAME reducer loop
        // (`p.apply_event(re)`) the proof loops use.
        {
            let mut p = RunPanel::default();
            // First frame: the raw `approval.request` gateway frame.
            let raise_frame = gw_event(
                "approval.request",
                serde_json::json!({
                    "command": "ls",
                    "description": "d",
                    "approval_id": "o1",
                    "allow_permanent": false,
                    "risk": "read_only"
                }),
            );
            // Decode with the SAME gateway decoder.
            let raise_facts = super::gateway_event_to_work_events(&raise_frame);
            assert_eq!(
                raise_facts.len(),
                1,
                "one seam fact from the raw approval.request frame"
            );
            // Drive the SAME reducer loop with EVERY update the frame
            // produces (in order, nothing filtered).
            for up in super::work_event_to_updates(raise_facts[0].clone()) {
                if let WorkUpdate::Event(re) = &up {
                    p.apply_event(re);
                }
            }
            // Assert the approval card exists after the first frame.
            assert!(
                p.approval.is_some(),
                "card raised after the raw approval.request frame"
            );
            // Later frame: the raw `message.complete` gateway frame.
            let clear_frame = gw_event(
                "message.complete",
                serde_json::json!({
                    "status": "complete",
                    "text": "done"
                }),
            );
            // Decode with the SAME gateway decoder.
            let clear_facts = super::gateway_event_to_work_events(&clear_frame);
            assert_eq!(
                clear_facts.len(),
                1,
                "one seam fact from the raw message.complete frame"
            );
            // Drive the SAME reducer loop with EVERY update the frame
            // produces (in order, nothing filtered).
            for up in super::work_event_to_updates(clear_facts[0].clone()) {
                if let WorkUpdate::Event(re) = &up {
                    p.apply_event(re);
                }
            }
            // Assert the approval card is gone after the last frame.
            assert!(
                p.approval.is_none(),
                "the later message.complete frame clears the approval card"
            );
        }
    }

    #[test]
    fn gateway_message_delta_maps_to_token() {
        let ev = gw_event("message.delta", serde_json::json!({"text": "Hello"}));
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 2);
        match &ups[0] {
            WorkUpdate::Token(t) => assert_eq!(t, "Hello"),
            other => panic!("expected Token, got {other:?}"),
        }
        match &ups[1] {
            WorkUpdate::Event(RunEvent::Token(t)) => assert_eq!(t.text, "Hello"),
            other => panic!("expected Event(Token), got {other:?}"),
        }
    }

    #[test]
    fn gateway_message_complete_maps_to_completion() {
        let ev = gw_event("message.complete", serde_json::json!({"text": "Done."}));
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 1);
        match &ups[0] {
            WorkUpdate::Event(RunEvent::Completion(c)) => {
                assert_eq!(c.status, Some(RunStatus::Completed));
                assert_eq!(c.output.as_deref(), Some("Done."));
            }
            _ => panic!("expected Completion"),
        }
    }

    #[test]
    fn gateway_message_complete_reflects_real_status() {
        // Hermes sends status:"complete" on a clean finish (real payload).
        let ev = gw_event(
            "message.complete",
            serde_json::json!({"status": "complete", "text": "PONG"}),
        );
        match &gateway_event_to_update(&ev)[0] {
            WorkUpdate::Event(RunEvent::Completion(c)) => {
                assert_eq!(c.status, Some(RunStatus::Completed));
            }
            _ => panic!("expected Completion"),
        }
        // A failed finish is reflected, not masked as success.
        let ev = gw_event("message.complete", serde_json::json!({"status": "error"}));
        match &gateway_event_to_update(&ev)[0] {
            WorkUpdate::Event(RunEvent::Completion(c)) => {
                assert_eq!(c.status, Some(RunStatus::Failed));
                assert_eq!(c.output, None);
            }
            _ => panic!("expected Completion"),
        }
    }

    #[test]
    fn thinking_delta_surfaces_status_text() {
        // Previously dropped (silent); now surfaced as transient progress.
        let ev = gw_event(
            "thinking.delta",
            serde_json::json!({"text": "(• ◡ •) pondering"}),
        );
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 1);
        match &ups[0] {
            WorkUpdate::Event(RunEvent::ToolProgress(p)) => {
                assert_eq!(p.message.as_deref(), Some("(• ◡ •) pondering"));
            }
            other => panic!("expected ToolProgress, got {other:?}"),
        }
    }

    #[test]
    fn status_update_compacting_is_made_visible() {
        // Slice-21 stance on the gateway path: compaction must not be silent.
        let ev = gw_event("status.update", serde_json::json!({"kind": "compacting"}));
        match &gateway_event_to_update(&ev)[0] {
            WorkUpdate::Event(RunEvent::ToolProgress(p)) => {
                assert_eq!(p.message.as_deref(), Some("compacting context…"));
            }
            other => panic!("expected ToolProgress, got {other:?}"),
        }
    }

    #[test]
    fn terminal_read_request_alias_is_handled() {
        // Current Hermes emits "terminal.read.request"; the bare "terminal.read"
        // was the old name. Both must produce the read request so the blocking
        // read_terminal tool never hangs on a rename.
        for name in ["terminal.read", "terminal.read.request"] {
            let ev = gw_event(name, serde_json::json!({"request_id": "rq-1"}));
            match &gateway_event_to_update(&ev)[..] {
                [WorkUpdate::TerminalReadRequest { request_id }] => {
                    assert_eq!(request_id, "rq-1", "event {name}");
                }
                other => panic!("expected TerminalReadRequest for {name}, got {other:?}"),
            }
        }
    }

    #[test]
    fn review_summary_surfaces_as_persistent_notice() {
        // Was silently dropped; now a persistent quote-prefixed line so a saved
        // skill/memory is always visible.
        let ev = gw_event(
            "review.summary",
            serde_json::json!({"text": "Saved skill: pr-summary"}),
        );
        match &gateway_event_to_update(&ev)[..] {
            [WorkUpdate::Token(t)] => {
                assert!(t.contains("Saved skill: pr-summary"), "got {t:?}");
                assert!(t.contains("> saved:"), "got {t:?}");
            }
            other => panic!("expected Token notice, got {other:?}"),
        }
        // An empty/whitespace summary produces nothing.
        assert!(gateway_event_to_update(&gw_event(
            "review.summary",
            serde_json::json!({"text": "  "})
        ))
        .is_empty());
    }

    #[test]
    fn message_complete_extracts_reported_cost() {
        let ev = gw_event(
            "message.complete",
            serde_json::json!({"text": "ok", "usage": {"cost_usd": 0.0123}}),
        );
        let ups = gateway_event_to_update(&ev);
        assert!(
            ups.iter()
                .any(|u| matches!(u, WorkUpdate::ReportedCost(c) if (*c - 0.0123).abs() < 1e-9)),
            "cost_usd must surface as ReportedCost"
        );
        assert!(ups
            .iter()
            .any(|u| matches!(u, WorkUpdate::Event(RunEvent::Completion(_)))));
        // No usage => no cost event.
        let ev = gw_event("message.complete", serde_json::json!({"text": "ok"}));
        assert!(!gateway_event_to_update(&ev)
            .iter()
            .any(|u| matches!(u, WorkUpdate::ReportedCost(_))));
    }

    #[test]
    fn setup_errors_get_a_hint_but_runtime_errors_dont() {
        assert!(provider_setup_hint("no_provider_configured").is_some());
        assert!(provider_setup_hint("Error: No inference provider configured").is_some());
        assert!(provider_setup_hint("set an API key to continue").is_some());
        assert!(provider_setup_hint("ANTHROPIC_API_KEY is not set").is_some());
        // A normal runtime failure is left alone (no false-positive hint).
        assert!(provider_setup_hint("gateway connection lost").is_none());
        assert!(provider_setup_hint("session.create failed: timeout").is_none());
    }

    #[test]
    fn tool_complete_shows_duration() {
        fn token(ups: Vec<WorkUpdate>) -> Option<String> {
            ups.into_iter().find_map(|u| match u {
                WorkUpdate::Token(t) => Some(t),
                _ => None,
            })
        }
        // Critique P2: clean completions no longer write transcript tokens at
        // all (the rail's tool card carries the duration); only an
        // engine-reported FAILURE surfaces in the transcript, duration intact.
        let ev = gw_event(
            "tool.complete",
            serde_json::json!({"name": "bash", "duration_s": 2.34}),
        );
        assert!(token(gateway_event_to_update(&ev)).is_none());
        // The gateway reports no per-tool outcome (ok: None) — the FAILED
        // transcript line is the native-engine lane's, driven directly:
        let ups = work_event_to_updates(WorkEvent::ToolFinished {
            name: Some("bash".to_string()),
            detail: None,
            duration_s: Some(2.34),
            ok: Some(false),
        });
        let t = token(ups).unwrap();
        assert!(t.contains("bash FAILED (2.3s)"), "got {t:?}");
    }

    #[test]
    fn message_complete_warns_on_high_context_fill() {
        fn context_msg(ups: Vec<WorkUpdate>) -> Option<String> {
            ups.into_iter().find_map(|u| match u {
                WorkUpdate::Event(RunEvent::ToolProgress(p))
                    if p.tool.as_deref() == Some("context") =>
                {
                    p.message
                }
                _ => None,
            })
        }
        // >= 80% surfaces a warning; accepts both 0-100 and 0-1 encodings.
        for usage in [
            serde_json::json!({"context_percent": 87}),
            serde_json::json!({"context_percent": 0.87}),
        ] {
            let ev = gw_event(
                "message.complete",
                serde_json::json!({"text": "ok", "usage": usage}),
            );
            assert!(context_msg(gateway_event_to_update(&ev))
                .unwrap()
                .contains("87%"));
        }
        // Below 80% => no warning (no clutter).
        let ev = gw_event(
            "message.complete",
            serde_json::json!({"text": "ok", "usage": {"context_percent": 50}}),
        );
        assert!(context_msg(gateway_event_to_update(&ev)).is_none());
    }

    #[test]
    fn tool_events_surface_the_agent_summary() {
        fn progress_msg(ups: Vec<WorkUpdate>) -> Option<String> {
            ups.into_iter().find_map(|u| match u {
                WorkUpdate::Event(RunEvent::ToolProgress(p)) => p.message,
                _ => None,
            })
        }
        // tool.start prefers summary > message > text for the live detail.
        let ev = gw_event(
            "tool.start",
            serde_json::json!({"name": "bash", "summary": "running ls -la"}),
        );
        assert_eq!(
            progress_msg(gateway_event_to_update(&ev)).as_deref(),
            Some("running ls -la")
        );
        // tool.complete surfaces the agent's completion summary, not "<name> done".
        let ev = gw_event(
            "tool.complete",
            serde_json::json!({"name": "bash", "summary": "listed 12 files"}),
        );
        assert_eq!(
            progress_msg(gateway_event_to_update(&ev)).as_deref(),
            Some("listed 12 files")
        );
        // Falls back to "<name> done" when no summary is present.
        let ev = gw_event("tool.complete", serde_json::json!({"name": "bash"}));
        assert_eq!(
            progress_msg(gateway_event_to_update(&ev)).as_deref(),
            Some("bash done")
        );
    }

    #[test]
    fn sudo_request_surfaces_credential() {
        let ev = gw_event("sudo.request", serde_json::json!({"request_id": "sudo-1"}));
        match &gateway_event_to_update(&ev)[..] {
            [WorkUpdate::SudoRequest { request_id }] => assert_eq!(request_id, "sudo-1"),
            other => panic!("expected SudoRequest, got {other:?}"),
        }
        // No request id => nothing to respond to; dropped rather than surfaced.
        assert!(
            gateway_event_to_update(&gw_event("sudo.request", serde_json::json!({}))).is_empty()
        );
    }

    #[test]
    fn secret_request_carries_env_var_and_prompt() {
        let ev = gw_event(
            "secret.request",
            serde_json::json!({
                "request_id": "s-1",
                "env_var": "OPENAI_API_KEY",
                "prompt": "Paste your key"
            }),
        );
        match &gateway_event_to_update(&ev)[..] {
            [WorkUpdate::SecretRequest {
                request_id,
                env_var,
                prompt,
            }] => {
                assert_eq!(request_id, "s-1");
                assert_eq!(env_var, "OPENAI_API_KEY");
                assert_eq!(prompt, "Paste your key");
            }
            other => panic!("expected SecretRequest, got {other:?}"),
        }
    }

    #[test]
    fn unknown_gateway_event_is_dropped_not_panicked() {
        // gateway.ready / skin.changed are intentionally ignored; a genuinely
        // unknown type returns empty (and is logged to stderr by the reducer).
        assert!(
            gateway_event_to_update(&gw_event("gateway.ready", serde_json::json!({}))).is_empty()
        );
        assert!(
            gateway_event_to_update(&gw_event("skin.changed", serde_json::json!({}))).is_empty()
        );
        assert!(
            gateway_event_to_update(&gw_event("totally.new.event", serde_json::json!({})))
                .is_empty()
        );
    }

    #[test]
    fn parse_complete_status_maps_spellings() {
        assert_eq!(parse_complete_status("complete"), RunStatus::Completed);
        assert_eq!(parse_complete_status("DONE"), RunStatus::Completed);
        assert_eq!(parse_complete_status("error"), RunStatus::Failed);
        assert_eq!(parse_complete_status("stopped"), RunStatus::Stopped);
        assert_eq!(parse_complete_status("cancelled"), RunStatus::Stopped);
        assert_eq!(
            parse_complete_status("weird-future-status"),
            RunStatus::Completed
        );
    }

    #[test]
    fn gateway_tool_start_maps_to_tool_progress_running() {
        let ev = gw_event("tool.start", serde_json::json!({"name": "bash"}));
        let ups = gateway_event_to_update(&ev);
        // Critique P2: no transcript Token — the rail's tool card is the
        // single home for tool activity.
        assert_eq!(ups.len(), 1);
        match &ups[0] {
            WorkUpdate::Event(RunEvent::ToolProgress(t)) => {
                assert_eq!(t.tool.as_deref(), Some("bash"));
                assert_eq!(t.status.as_deref(), Some("running"));
            }
            _ => panic!("expected ToolProgress"),
        }
    }

    #[test]
    fn gateway_tool_complete_maps_to_tool_progress_completed() {
        let ev = gw_event("tool.complete", serde_json::json!({"name": "bash"}));
        let ups = gateway_event_to_update(&ev);
        // Critique P2: clean completion emits no transcript Token either.
        assert_eq!(ups.len(), 1);
        match &ups[0] {
            WorkUpdate::Event(RunEvent::ToolProgress(t)) => {
                assert_eq!(t.status.as_deref(), Some("completed"));
            }
            _ => panic!("expected ToolProgress"),
        }
    }

    #[test]
    fn gateway_approval_request_maps_to_approval() {
        let ev = gw_event(
            "approval.request",
            serde_json::json!({
                "command": "rm -rf /tmp/test",
                "description": "dangerous command"
            }),
        );
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 2);
        assert!(matches!(&ups[0], WorkUpdate::Token(_)));
        match &ups[1] {
            WorkUpdate::Event(RunEvent::Approval(a)) => {
                assert!(a.prompt.as_ref().unwrap().contains("rm -rf"));
            }
            _ => panic!("expected Approval"),
        }
    }

    #[test]
    fn gateway_approval_extracts_approval_id() {
        let ev = gw_event(
            "approval.request",
            serde_json::json!({
                "command": "rm -rf /tmp/test",
                "description": "dangerous command",
                "approval_id": "appr-42"
            }),
        );
        let ups = gateway_event_to_update(&ev);
        match &ups[1] {
            WorkUpdate::Event(RunEvent::Approval(a)) => {
                assert_eq!(a.approval_id.as_deref(), Some("appr-42"));
            }
            _ => panic!("expected Approval"),
        }
    }

    #[test]
    fn gateway_error_maps_to_failed() {
        let ev = gw_event("error", serde_json::json!({"message": "session crashed"}));
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 1);
        match &ups[0] {
            WorkUpdate::Failed(msg) => assert_eq!(msg, "session crashed"),
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn gateway_error_without_message_falls_back() {
        // A backend `error` event with no message field must still surface as a
        // failure carrying a generic label — never dropped to empty (which would
        // silently swallow the crash) and never a panic on the missing field.
        let ev = gw_event("error", serde_json::json!({}));
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 1);
        match &ups[0] {
            WorkUpdate::Failed(msg) => assert_eq!(msg, "unknown gateway error"),
            other => panic!("expected Failed with fallback text, got {other:?}"),
        }
    }

    #[test]
    fn gateway_session_info_running_false_completes_turn() {
        // The `session.info running:false` half of the turn-end conjunction: a
        // stopped session must emit TurnComplete so the Work turn finishes. A
        // regression dropping this would leave the turn spinning forever.
        let ev = gw_event("session.info", serde_json::json!({"running": false}));
        assert!(matches!(
            &gateway_event_to_update(&ev)[..],
            [WorkUpdate::TurnComplete]
        ));
        // `running:true` (busy now) and an absent `running` are NOT turn-ends.
        for payload in [serde_json::json!({"running": true}), serde_json::json!({})] {
            let ev = gw_event("session.info", payload);
            assert!(
                gateway_event_to_update(&ev).is_empty(),
                "running!=false must not complete the turn"
            );
        }
    }

    #[test]
    fn gateway_reasoning_available_marks_reasoning_complete() {
        // `reasoning.available` resolves the thinking row so the reasoning
        // phase visibly ends rather than leaving a stuck spinner. RC2-RUN
        // (S-Q1, deliberate change): a typed update, not a tool named
        // "reasoning".
        let ev = gw_event("reasoning.available", serde_json::json!({}));
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 1);
        assert!(
            matches!(ups[0], WorkUpdate::Reasoning { done: true }),
            "expected the thinking row to end, got {:?}",
            ups[0]
        );
        let mut panel = RunPanel::default();
        panel.apply_reasoning(false);
        panel.apply_reasoning(true);
        assert_eq!(panel.tool_calls.len(), 1);
        assert_eq!(panel.tool_calls[0].status, ToolCallStatus::Completed);
        assert_eq!(panel.activity.as_deref(), Some(THOUGHT_LINE));
    }

    #[test]
    fn gateway_terminal_read_maps_to_request() {
        let ev = gw_event("terminal.read", serde_json::json!({"request_id": "tr-99"}));
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 1);
        match &ups[0] {
            WorkUpdate::TerminalReadRequest { request_id } => {
                assert_eq!(request_id, "tr-99");
            }
            _ => panic!("expected TerminalReadRequest"),
        }
    }

    #[test]
    fn gateway_terminal_read_ignores_missing_id() {
        let ev = gw_event("terminal.read", serde_json::json!({}));
        assert!(gateway_event_to_update(&ev).is_empty());
    }

    #[test]
    fn gateway_unknown_event_returns_empty() {
        let ev = gw_event("gateway.ready", serde_json::json!({}));
        assert!(gateway_event_to_update(&ev).is_empty());
    }

    #[test]
    fn file_badge_assigns_correct_icons() {
        use crate::icons::Icon;
        assert_eq!(file_badge("report.pdf"), Icon::FileText);
        assert_eq!(file_badge("data.xlsx"), Icon::BarChart);
        assert_eq!(file_badge("notes.docx"), Icon::FileText);
        assert_eq!(file_badge("photo.png"), Icon::Image);
        assert_eq!(file_badge("archive.zip"), Icon::Package);
        assert_eq!(file_badge("random.bin"), Icon::File);
    }

    #[test]
    fn gateway_message_start_maps_to_thinking_activity() {
        let ev = gw_event("message.start", serde_json::json!({}));
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 1);
        match &ups[0] {
            WorkUpdate::Event(RunEvent::ToolProgress(t)) => {
                assert_eq!(t.message.as_deref(), Some("Agent is thinking\u{2026}"));
            }
            _ => panic!("expected ToolProgress for message.start"),
        }
    }

    #[test]
    fn gateway_message_start_with_model_emits_model_detected() {
        let ev = gw_event(
            "message.start",
            serde_json::json!({"model": "claude-opus-4-6"}),
        );
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 2);
        match &ups[0] {
            WorkUpdate::ModelDetected(m) => assert_eq!(m, "claude-opus-4-6"),
            _ => panic!("expected ModelDetected first"),
        }
        match &ups[1] {
            WorkUpdate::Event(RunEvent::ToolProgress(t)) => {
                assert!(t.message.as_ref().unwrap().contains("claude-opus-4-6"));
            }
            _ => panic!("expected ToolProgress second"),
        }
    }

    #[test]
    fn gateway_subagent_start_maps_to_subagent_event() {
        let ev = gw_event(
            "subagent.start",
            serde_json::json!({
                "id": "sa-1",
                "parentId": "root",
                "goal": "research API docs",
                "model": "claude-sonnet-4-6"
            }),
        );
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 2);
        assert!(matches!(&ups[0], WorkUpdate::Token(_)));
        match &ups[1] {
            WorkUpdate::Subagent(kind) => match kind.as_ref() {
                GatewayEventKind::Subagent {
                    phase,
                    id,
                    parent_id,
                    goal,
                    model,
                    ..
                } => {
                    assert_eq!(*phase, SubagentPhase::Start);
                    assert_eq!(id.as_deref(), Some("sa-1"));
                    assert_eq!(parent_id.as_deref(), Some("root"));
                    assert_eq!(goal.as_deref(), Some("research API docs"));
                    assert_eq!(model.as_deref(), Some("claude-sonnet-4-6"));
                }
                _ => panic!("expected Subagent kind"),
            },
            _ => panic!("expected Subagent"),
        }
    }

    /// The forwarded raw event folds through the unit-18 reducer into a
    /// `SubagentProgress` carrying the rich fields the AGENTS card renders.
    #[test]
    fn gateway_subagent_event_folds_via_unit18_reducer() {
        let ev = gw_event(
            "subagent.start",
            serde_json::json!({
                "id": "sa-1",
                "goal": "research API docs",
                "model": "claude-sonnet-4-6",
                "filesRead": ["a.rs", "b.rs"],
                "inputTokens": 100,
                "outputTokens": 40,
            }),
        );
        let ups = gateway_event_to_update(&ev);
        let kind = ups
            .iter()
            .find_map(|u| match u {
                WorkUpdate::Subagent(k) => Some(k.as_ref().clone()),
                _ => None,
            })
            .expect("subagent update");

        let mut store = Vec::new();
        upsert_subagent(&mut store, &kind, 1_000);
        assert_eq!(store.len(), 1);
        let a = &store[0];
        assert_eq!(a.id, "sa-1");
        assert_eq!(a.files_read, vec!["a.rs", "b.rs"]);
        assert_eq!(a.input_tokens, Some(100));
        assert_eq!(a.output_tokens, Some(40));
        assert_eq!(subagent_totals(&store), (100, 40, 0.0));
        assert!(!subagent_is_done(&a.status));
    }

    #[test]
    fn gateway_subagent_complete_maps_with_summary() {
        let ev = gw_event(
            "subagent.complete",
            serde_json::json!({
                "id": "sa-1",
                "summary": "Found 3 relevant endpoints"
            }),
        );
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 2);
        assert!(matches!(&ups[0], WorkUpdate::Token(_)));
        match &ups[1] {
            WorkUpdate::Subagent(kind) => match kind.as_ref() {
                GatewayEventKind::Subagent {
                    phase, id, summary, ..
                } => {
                    assert_eq!(*phase, SubagentPhase::Complete);
                    assert_eq!(id.as_deref(), Some("sa-1"));
                    assert_eq!(summary.as_deref(), Some("Found 3 relevant endpoints"));
                }
                _ => panic!("expected Subagent kind"),
            },
            _ => panic!("expected Subagent"),
        }
    }

    #[test]
    fn gateway_subagent_no_id_returns_empty() {
        let ev = gw_event("subagent.start", serde_json::json!({"goal": "no id"}));
        assert!(gateway_event_to_update(&ev).is_empty());
    }

    /// A `subagent.start` with a valid id but NO goal must still forward the raw
    /// event to the unit-18 fold (so the AGENTS card learns the agent exists) —
    /// but it must NOT emit a `> *spawned agent: …*` chat line, since there is
    /// no goal text to show. The existing start test only proves the goal-present
    /// path emits two updates; this guards the goal-absent negative (one update,
    /// no spurious Token).
    #[test]
    fn gateway_subagent_start_without_goal_forwards_only() {
        let ev = gw_event("subagent.start", serde_json::json!({"id": "sa-9"}));
        let ups = gateway_event_to_update(&ev);
        assert_eq!(
            ups.len(),
            1,
            "no goal ⇒ no spawn line, just the fold forward"
        );
        assert!(
            matches!(&ups[0], WorkUpdate::Subagent(_)),
            "the sole update must be the raw-event forward, not a Token"
        );
    }

    /// A mid-lifecycle phase (here `subagent.progress`) hits the reducer's
    /// catch-all `_ => {}` arm: it must forward the raw event to the unit-18 fold
    /// (the live tree updates cost/status) WITHOUT spamming the chat stream with a
    /// line. A goal is included to prove the `_` arm ignores goal — only `Start`
    /// consumes it. Guards against a stray Token being added to the middle arm,
    /// which would flood the transcript on every progress tick.
    #[test]
    fn gateway_subagent_progress_phase_forwards_without_chat_line() {
        let ev = gw_event(
            "subagent.progress",
            serde_json::json!({
                "id": "sa-9",
                "goal": "should be ignored on a progress tick",
            }),
        );
        let ups = gateway_event_to_update(&ev);
        assert_eq!(
            ups.len(),
            1,
            "middle phase ⇒ no chat line, just the fold forward"
        );
        match &ups[0] {
            WorkUpdate::Subagent(kind) => match kind.as_ref() {
                GatewayEventKind::Subagent { phase, .. } => {
                    assert_eq!(*phase, SubagentPhase::Progress);
                }
                _ => panic!("expected Subagent kind"),
            },
            _ => panic!("expected the raw-event forward, not a Token"),
        }
    }

    /// A `subagent.complete` summary longer than 80 chars is truncated to the
    /// first 77 chars plus an ellipsis before it goes into the `> *agent done: …*`
    /// chat line (keeps the transcript tidy). Pins the exact 77-char boundary and
    /// format string — untested until now (the existing complete test uses a short
    /// summary that skips the truncation branch entirely).
    #[test]
    fn gateway_subagent_complete_truncates_long_summary() {
        let long = "A".repeat(90);
        let ev = gw_event(
            "subagent.complete",
            serde_json::json!({
                "id": "sa-9",
                "summary": long,
            }),
        );
        let ups = gateway_event_to_update(&ev);
        assert_eq!(ups.len(), 2);
        let expected = format!("> *agent done: {}…*\n\n", "A".repeat(77));
        match &ups[0] {
            WorkUpdate::Token(t) => assert_eq!(*t, expected),
            _ => panic!("expected the truncated done-line Token first"),
        }
        assert!(matches!(&ups[1], WorkUpdate::Subagent(_)));
    }

    #[test]
    fn subagent_status_classification() {
        // Running / pending states are not terminal.
        for s in ["running", "pending", "thinking", ""] {
            assert!(!subagent_is_done(s), "{s} should not be done");
            assert!(!subagent_is_error(s), "{s} should not be error");
        }
        // Clean completion is terminal but not an error — including explicit
        // backend words that override the reducer's default `complete`.
        for s in [
            "complete",
            "completed",
            "DONE",
            "finished",
            "success",
            "succeeded",
            "ok",
            "stopped",
        ] {
            assert!(subagent_is_done(s), "{s} should be done");
            assert!(!subagent_is_error(s), "{s} should not be error");
        }
        // Failure states are terminal AND errors (case/whitespace tolerant).
        for s in [
            "failed",
            " error ",
            "Cancelled",
            "canceled",
            "timeout",
            "timed_out",
        ] {
            assert!(subagent_is_done(s), "{s} should be done");
            assert!(subagent_is_error(s), "{s} should be error");
        }
    }

    #[test]
    fn subagent_totals_sum_tokens_and_cost() {
        let mk = |inp, out, cost| SubagentProgress {
            id: "x".into(),
            parent_id: None,
            goal: None,
            session_id: None,
            model: None,
            status: "running".into(),
            task_count: None,
            task_index: None,
            started_at: 0,
            updated_at: 0,
            duration_seconds: None,
            cost_usd: cost,
            input_tokens: inp,
            output_tokens: out,
            tool_count: None,
            files_read: vec![],
            files_written: vec![],
            stream: vec![],
            summary: None,
            current_tool: None,
        };
        let store = vec![
            mk(Some(100), Some(40), Some(0.01)),
            mk(Some(50), None, None),
            mk(None, Some(10), Some(0.02)),
        ];
        let (in_tok, out_tok, cost) = subagent_totals(&store);
        assert_eq!(in_tok, 150);
        assert_eq!(out_tok, 50);
        assert!((cost - 0.03).abs() < 1e-9);
        // Empty store rolls up to zeros (header then hides the token/cost tail).
        assert_eq!(subagent_totals(&[]), (0, 0, 0.0));
    }

    #[test]
    fn extract_urls_from_text() {
        let urls = super::extract_urls("check https://example.com/path and http://foo.bar/baz");
        assert_eq!(
            urls,
            vec![
                "https://example.com/path".to_string(),
                "http://foo.bar/baz".to_string(),
            ]
        );
    }

    #[test]
    fn extract_urls_ignores_short() {
        let urls = super::extract_urls("not a url: http://x");
        assert!(urls.is_empty());
    }

    #[test]
    fn extract_urls_strips_parens() {
        let urls = super::extract_urls("(https://example.com/foo)");
        assert_eq!(urls, vec!["https://example.com/foo".to_string()]);
    }

    #[test]
    fn gateway_tool_start_extracts_file_path() {
        let ev = gw_event(
            "tool.start",
            serde_json::json!({
                "name": "Edit",
                "input": {"file_path": "/src/main.rs"}
            }),
        );
        let ups = gateway_event_to_update(&ev);
        assert!(ups
            .iter()
            .any(|u| matches!(u, WorkUpdate::FileTouched(p) if p == "/src/main.rs")));
    }

    #[test]
    fn gateway_message_delta_extracts_urls() {
        let ev = gw_event(
            "message.delta",
            serde_json::json!({
                "text": "see https://docs.rs/gpui for details"
            }),
        );
        let ups = gateway_event_to_update(&ev);
        assert!(ups
            .iter()
            .any(|u| matches!(u, WorkUpdate::UrlReferenced(url) if url == "https://docs.rs/gpui")));
    }

    #[test]
    fn relative_time_formats() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert_eq!(super::relative_time(now - 30.0), "just now");
        assert_eq!(super::relative_time(now - 120.0), "2m ago");
        assert_eq!(super::relative_time(now - 3700.0), "1h ago");
        assert_eq!(super::relative_time(now - 90000.0), "1d ago");
    }

    // ========================================================================
    // D1 seam — translation equivalence (Codex merge constraint 4)
    // ========================================================================

    /// The D1 composition (`gateway_event_to_work_events` →
    /// `work_event_to_updates`) must reproduce the legacy
    /// `gateway_event_to_update` output Debug-identically, for every
    /// `GatewayEventKind` decode path including the guard/edge cases.
    #[test]
    fn seam_translation_equivalent_to_legacy() {
        use serde_json::json;
        let long_summary = "s".repeat(120);
        let fixtures: Vec<GatewayEvent> = vec![
            gw_event("message.start", json!({"model": "laguna-s-2.1-nvfp4"})),
            gw_event("message.start", json!({})),
            gw_event(
                "message.delta",
                json!({"text": "see https://example.com/doc and (https://a.io/x)."}),
            ),
            gw_event("message.delta", json!({"text": ""})),
            gw_event(
                "message.complete",
                json!({"text": "done", "status": "complete", "cost_usd": 0.5, "usage": {"context_percent": 0.9}}),
            ),
            gw_event("message.complete", json!({"status": "error"})),
            gw_event(
                "message.complete",
                json!({"status": "stopped", "usage": {"context_percent": 85.0}}),
            ),
            gw_event("message.complete", json!({"cost_usd": 0.0})),
            gw_event("message.complete", json!({})),
            gw_event(
                "tool.start",
                json!({"name": "bash", "message": "running ls", "input": {"file_path": "/tmp/f"}}),
            ),
            gw_event("tool.progress", json!({"name": "bash"})),
            gw_event("tool.generating", json!({})),
            gw_event(
                "tool.complete",
                json!({"name": "bash", "summary": "did it", "duration_s": 1.234}),
            ),
            gw_event("tool.complete", json!({"name": "bash", "duration_s": 0.01})),
            gw_event("tool.complete", json!({})),
            gw_event(
                "approval.request",
                json!({"command": "rm -rf /tmp/x", "description": "Delete", "approval_id": "a1", "allow_permanent": true}),
            ),
            gw_event("approval.request", json!({"description": "Just desc"})),
            gw_event("approval.request", json!({})),
            gw_event(
                "clarify.request",
                json!({"question": "Which?", "request_id": "c1"}),
            ),
            gw_event("clarify.request", json!({})),
            gw_event("reasoning.delta", json!({"text": "because"})),
            gw_event("reasoning.delta", json!({"text": ""})),
            gw_event("reasoning.available", json!({})),
            gw_event("thinking.delta", json!({"text": "pondering"})),
            gw_event("thinking.delta", json!({"text": "  "})),
            gw_event("thinking.delta", json!({})),
            gw_event("session.info", json!({"running": false})),
            gw_event("session.info", json!({"running": true})),
            gw_event("status.update", json!({"kind": "compacting"})),
            gw_event("status.update", json!({"message": "doing stuff"})),
            gw_event("status.update", json!({})),
            gw_event("terminal.read", json!({"request_id": "t1"})),
            gw_event("terminal.read.request", json!({"requestId": "t2"})),
            gw_event("terminal.read", json!({})),
            gw_event("sudo.request", json!({"request_id": "s1"})),
            gw_event("sudo.request", json!({})),
            gw_event(
                "secret.request",
                json!({"request_id": "sec1", "env_var": "API_KEY", "prompt": "key?"}),
            ),
            gw_event("secret.request", json!({})),
            gw_event("review.summary", json!({"text": "  saved skill  "})),
            gw_event("review.summary", json!({"text": ""})),
            gw_event("error", json!({"message": "boom"})),
            gw_event("error", json!({})),
            gw_event("gateway.ready", json!({})),
            gw_event("skin.changed", json!({})),
            gw_event(
                "subagent.start",
                json!({"id": "ag1", "goal": "do thing", "parentId": "root", "model": "m"}),
            ),
            gw_event(
                "subagent.complete",
                json!({"id": "ag1", "summary": long_summary, "costUsd": 0.02}),
            ),
            gw_event(
                "subagent.progress",
                json!({"id": "ag1", "inputTokens": 5, "filesRead": ["a.txt"]}),
            ),
            gw_event("subagent.start", json!({})),
            gw_event("subagent.tool", json!({"id": "ag1", "currentTool": "bash"})),
            gw_event("subagent.spawn_requested", json!({"id": "x"})),
            gw_event("subagent.thinking", json!({"id": "x"})),
            gw_event("subagent.weird", json!({"id": "x"})),
            gw_event("some.unknown.event", json!({"whatever": 1})),
        ];

        for ev in &fixtures {
            let legacy: Vec<String> = gateway_event_to_update(ev)
                .iter()
                .map(|u| format!("{u:?}"))
                .collect();
            let seam: Vec<String> = gateway_event_to_work_events(ev)
                .into_iter()
                .flat_map(work_event_to_updates)
                .map(|u| format!("{u:?}"))
                .collect();
            assert_eq!(legacy, seam, "divergence for event type {}", ev.event_type);
        }
    }

    // ========================================================================
    // D1 seam — worker command/lifecycle semantics (Codex P1-b)
    // ========================================================================

    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Scripted [`GatewayOps`] fake. Records every operation; serves scripted
    /// events per submitted prompt; can simulate session.create failure and a
    /// lost connection. `Send` so lifecycle tests can drive the loop from a
    /// spawned thread with real timing.
    #[derive(Clone)]
    pub(super) struct FakeOps {
        log: Arc<Mutex<Vec<String>>>,
        /// One scripted event batch per expected `prompt_submit`, in order.
        turns: Arc<Mutex<VecDeque<Vec<GatewayEvent>>>>,
        /// The live queue `recv_event` drains (loaded by `prompt_submit`).
        pub(super) live: Arc<Mutex<VecDeque<GatewayEvent>>>,
        pub(super) connected: Arc<AtomicBool>,
        fail_session_create: bool,
        /// `approval_respond` returns `Err` when set (P3d reads it too).
        pub(super) fail_approval: bool,
        /// P3c: `session_interrupt` returns `Err` when set.
        pub(super) fail_interrupt: bool,
        /// P3c: events the backend emits in response to a successful
        /// `session_interrupt` (appended to the live queue).
        pub(super) interrupt_events: Vec<GatewayEvent>,
        /// P3c round 3: run at the start of `session_close`; its result is
        /// recorded just before the call, so a test can observe what the
        /// worker had already done when the close RPC began.
        pub(super) close_probe: Option<Arc<dyn Fn() -> String + Send + Sync>>,
        /// P3c round 4: the id `session_create` hands out (default `s1`), so
        /// a test can tell a respawned worker's session from the old one.
        pub(super) session_name: String,
        /// P3c round 4 (R10): how long `prompt_submit` blocks after loading
        /// its turn — an outstanding RPC when a Stop is pressed.
        pub(super) submit_delay: Duration,
        /// P3c round 5 (R11): `prompt_submit` loads its turn (the backend
        /// accepted the work) and then returns a request timeout.
        pub(super) fail_submit: bool,
        /// P3c round 5: commands the UI sends while a named RPC is in
        /// flight (see [`FakeOps::send_during`]).
        ui_during: Arc<Mutex<Option<UiDuring>>>,
        /// P3c round 6 (R13): when set, `session_interrupt` records itself
        /// and then blocks until the latch is opened (see [`Latch`]) — an
        /// interrupt RPC still outstanding while the UI acts.
        pub(super) interrupt_latch: Option<Latch>,
        /// P3d round 2 (R1): when set, `terminal_read_respond` records itself
        /// and then blocks until the latch is opened — a terminal-read
        /// response still outstanding while the UI presses Stop.
        pub(super) terminal_read_latch: Option<Latch>,
        /// P3d round 2 (R2): events the backend emits in response to a
        /// successful `approval_respond` (appended to the live queue).
        pub(super) approval_events: Vec<GatewayEvent>,
    }

    /// A one-shot gate a fake RPC waits on until the test opens it.
    #[derive(Clone, Default)]
    pub(super) struct Latch(Arc<(Mutex<bool>, std::sync::Condvar)>);

    impl Latch {
        /// Let every waiter (current and future) through.
        pub(super) fn open(&self) {
            let (open, cv) = &*self.0;
            *open.lock().unwrap() = true;
            cv.notify_all();
        }
        fn wait(&self) {
            let (open, cv) = &*self.0;
            let mut guard = open.lock().unwrap();
            while !*guard {
                guard = cv.wait(guard).unwrap();
            }
        }
    }

    /// Commands a test's UI sends while one RPC of the fake is in flight.
    struct UiDuring {
        /// `"session_create"` or `"prompt_submit"`.
        rpc: &'static str,
        tx: std::sync::mpsc::Sender<GatewayCmd>,
        cmds: Vec<GatewayCmd>,
    }

    impl FakeOps {
        /// A connected fake serving `turns` (one batch per prompt).
        pub(super) fn new(turns: Vec<Vec<GatewayEvent>>) -> Self {
            Self {
                log: Arc::new(Mutex::new(Vec::new())),
                turns: Arc::new(Mutex::new(turns.into_iter().collect())),
                live: Arc::new(Mutex::new(VecDeque::new())),
                connected: Arc::new(AtomicBool::new(true)),
                fail_session_create: false,
                fail_approval: false,
                fail_interrupt: false,
                interrupt_events: Vec::new(),
                close_probe: None,
                session_name: "s1".into(),
                submit_delay: Duration::ZERO,
                fail_submit: false,
                ui_during: Arc::new(Mutex::new(None)),
                interrupt_latch: None,
                terminal_read_latch: None,
                approval_events: Vec::new(),
            }
        }
        /// Send `cmds` on `tx` from inside the next `rpc` call (`"session_create"`
        /// or `"prompt_submit"`, after it has taken effect), once — the UI
        /// acting while that RPC is outstanding. The sender clone is released
        /// after firing, so the loop still ends when the test drops its own.
        pub(super) fn send_during(
            &self,
            rpc: &'static str,
            tx: &std::sync::mpsc::Sender<GatewayCmd>,
            cmds: Vec<GatewayCmd>,
        ) {
            *self.ui_during.lock().unwrap() = Some(UiDuring {
                rpc,
                tx: tx.clone(),
                cmds,
            });
        }
        fn fire_ui_during(&self, rpc: &str) {
            let mut slot = self.ui_during.lock().unwrap();
            if slot.as_ref().is_some_and(|u| u.rpc == rpc) {
                if let Some(u) = slot.take() {
                    for cmd in u.cmds {
                        let _ = u.tx.send(cmd);
                    }
                }
            }
        }
        /// Every recorded operation, in call order.
        pub(super) fn ops(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
        /// Append `op` to the call log (tests also mark UI-side moments).
        pub(super) fn record(&self, op: String) {
            self.log.lock().unwrap().push(op);
        }
    }

    impl GatewayOps for FakeOps {
        fn session_create(
            &self,
            cwd: &str,
        ) -> Result<awili_gateway::SessionCreateResponse, awili_gateway::GatewayError> {
            self.record(format!("session_create({cwd})"));
            if self.fail_session_create {
                return Err(awili_gateway::GatewayError::Connect("nope".into()));
            }
            self.fire_ui_during("session_create");
            Ok(awili_gateway::SessionCreateResponse {
                session_id: self.session_name.clone(),
                stored_session_id: None,
                info: None,
            })
        }
        fn prompt_submit(
            &self,
            session_id: &str,
            text: &str,
        ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
            self.record(format!("prompt_submit({session_id},{text})"));
            if let Some(batch) = self.turns.lock().unwrap().pop_front() {
                self.live.lock().unwrap().extend(batch);
            }
            self.fire_ui_during("prompt_submit");
            if !self.submit_delay.is_zero() {
                std::thread::sleep(self.submit_delay);
            }
            if self.fail_submit {
                return Err(awili_gateway::GatewayError::RequestTimeout {
                    method: "prompt.submit".into(),
                });
            }
            Ok(serde_json::Value::Null)
        }
        fn approval_respond(
            &self,
            session_id: &str,
            choice: &str,
        ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
            self.record(format!("approval_respond({session_id},{choice})"));
            if self.fail_approval {
                return Err(awili_gateway::GatewayError::Connect(
                    "private backend diagnostic".into(),
                ));
            }
            self.live
                .lock()
                .unwrap()
                .extend(self.approval_events.iter().cloned());
            Ok(serde_json::Value::Null)
        }
        fn clarify_respond(
            &self,
            session_id: &str,
            answer: &str,
        ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
            self.record(format!("clarify_respond({session_id},{answer})"));
            Ok(serde_json::Value::Null)
        }
        fn session_steer(
            &self,
            session_id: &str,
            text: &str,
        ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
            self.record(format!("session_steer({session_id},{text})"));
            Ok(serde_json::Value::Null)
        }
        fn file_attach(
            &self,
            session_id: &str,
            name: &str,
            path: &str,
        ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
            self.record(format!("file_attach({session_id},{name},{path})"));
            Ok(serde_json::Value::Null)
        }
        fn terminal_read_respond(
            &self,
            session_id: &str,
            request_id: &str,
            text: &str,
        ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
            self.record(format!(
                "terminal_read_respond({session_id},{request_id},{text})"
            ));
            if let Some(latch) = &self.terminal_read_latch {
                latch.wait();
            }
            Ok(serde_json::Value::Null)
        }
        fn sudo_respond(
            &self,
            request_id: &str,
            _password: &str,
        ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
            // The password is deliberately NOT recorded (mirrors the real
            // client's never-log-the-secret discipline).
            self.record(format!("sudo_respond({request_id})"));
            Ok(serde_json::Value::Null)
        }
        fn secret_respond(
            &self,
            request_id: &str,
            _value: &str,
        ) -> Result<serde_json::Value, awili_gateway::GatewayError> {
            self.record(format!("secret_respond({request_id})"));
            Ok(serde_json::Value::Null)
        }
        fn session_close(&self, session_id: &str) -> Result<(), awili_gateway::GatewayError> {
            if let Some(probe) = &self.close_probe {
                self.record(probe());
            }
            self.record(format!("session_close({session_id})"));
            Ok(())
        }
        fn session_interrupt(&self, session_id: &str) -> Result<(), awili_gateway::GatewayError> {
            self.record(format!("session_interrupt({session_id})"));
            if let Some(latch) = &self.interrupt_latch {
                latch.wait();
            }
            if self.fail_interrupt {
                return Err(awili_gateway::GatewayError::Connect(
                    "interrupt refused".into(),
                ));
            }
            self.live
                .lock()
                .unwrap()
                .extend(self.interrupt_events.iter().cloned());
            Ok(())
        }
        fn recv_event(&self, _timeout: Duration) -> Option<GatewayEvent> {
            let popped = self.live.lock().unwrap().pop_front();
            if popped.is_none() {
                // Real recv_event blocks up to its timeout; keep the spin cheap.
                std::thread::sleep(Duration::from_millis(2));
            }
            popped
        }
        fn is_connected(&self) -> bool {
            self.connected.load(Ordering::SeqCst)
        }
        fn close(&mut self) {
            self.record("close".into());
        }
    }

    /// Run the loop to completion on this thread with a pre-loaded command
    /// sequence (the sender is dropped, so the loop exits after draining).
    pub(super) fn drive_loop(fake: &FakeOps, cmds: Vec<GatewayCmd>) -> Vec<WorkEvent> {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        for c in cmds {
            cmd_tx.send(c).unwrap();
        }
        drop(cmd_tx);
        let (ev_tx, ev_rx) = async_channel::unbounded();
        run_worker_loop(
            fake.clone(),
            WorkerContext {
                workdir: "/wd".into(),
                cmd_rx,
                cancel: Arc::new(AtomicBool::new(false)),
                events: ev_tx,
            },
        );
        let mut events = Vec::new();
        while let Ok(ev) = ev_rx.try_recv() {
            events.push(ev);
        }
        events
    }

    #[test]
    fn approval_delivery_worker_reports_failure_and_retry_success() {
        for midturn in [false, true] {
            for fail in [false, true] {
                let mut fake = FakeOps::new(vec![vec![
                    gw_event("message.complete", serde_json::json!({"status":"complete"})),
                    gw_event("session.info", serde_json::json!({"running": false})),
                ]]);
                fake.fail_approval = fail;
                let mut cmds = vec![];
                if midturn {
                    cmds.push(GatewayCmd::Prompt {
                        text: "task".into(),
                        system: None,
                    });
                }
                cmds.push(GatewayCmd::Approval {
                    id: Some("attempt-1".into()),
                    choice: "once".into(),
                });
                let events = drive_loop(&fake, cmds);
                assert!(events.contains(&WorkEvent::ApprovalDelivery {
                    id: Some("attempt-1".into()),
                    delivered: !fail
                }));
                assert!(!format!("{events:?}").contains("private backend diagnostic"));
            }
        }
    }

    #[test]
    fn approval_delivery_disconnected_channel_can_retry_without_duplicates() {
        let mut d = ApprovalDelivery::default();
        d.send(None, "once");
        assert!(d.failed && d.pending.is_none());
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        d.send(Some(&tx), "deny");
        assert!(d.failed && d.pending.is_none());
        let (tx, rx) = std::sync::mpsc::channel();
        d.send(Some(&tx), "deny");
        d.send(Some(&tx), "once");
        assert!(
            matches!(rx.try_recv().unwrap(), GatewayCmd::Approval { choice, .. } if choice == "deny")
        );
        assert!(rx.try_recv().is_err());
        assert!(!d.failed && d.pending.is_some());
    }

    #[test]
    fn connector_save_flow_uses_exact_plan_actions() {
        use crate::messaging_controls::ConnectorAction as A;
        use awili_gateway::rest::MessagingEnvVar;

        let mut p = messaging_pane::tests_pub_platform("tg", "Telegram");
        p.env_vars = vec![MessagingEnvVar {
            key: "TELEGRAM_BOT_TOKEN".to_string(),
            required: true,
            is_set: false,
            redacted_value: None,
            description: String::new(),
            prompt: String::new(),
            help: String::new(),
            url: None,
            is_password: true,
            advanced: false,
        }];
        let mut d = messaging_pane::MessagingData::default();
        d.state
            .apply_snapshot(messaging_pane::tests_pub_snap(vec![p.clone()]));
        d.open_draft(Some(&p));
        d.toggle_draft_enabled();
        assert!(d.begin_credential_edit("TELEGRAM_BOT_TOKEN"));
        assert!(d.commit_credential_edit("123:abc"));
        assert!(
            d.request_save_confirmation(),
            "the required credential makes the dirty draft savable"
        );

        // The dialog snapshot: exactly the two changes staged when it opened,
        // with the connector's name for the header — and names only; the typed
        // secret never appears in the plan.
        let shown = d.confirm_plan().expect("plan shown").clone();
        assert_eq!(shown.change_count(), 2);
        assert_eq!(d.confirm_name(), Some("Telegram"));
        assert!(
            shown.lines().iter().all(|l| !l.contains("123:abc")),
            "the plan is names-only: the typed secret never appears in it"
        );

        // Confirm applies exactly the snapshot — the same actions, in the
        // credential-first order pinned by the draft engine (see
        // `connector_draft_actions_ordered_credentials_before_config` in
        // messaging_controls::tests) — with no re-derivation from the live
        // draft. S2(b): the returned identity carries the confirmed
        // connector id and the draft's generation at open time.
        let conf = d
            .confirm_save()
            .expect("confirm returns the identity + actions");
        assert_eq!(conf.connector_id, "tg");
        assert!(
            conf.generation > 0,
            "the identity pins a live draft generation"
        );
        let applied = conf.actions.clone();
        assert_eq!(applied.len(), 2);
        let expect = [
            A::SetEnv {
                key: "TELEGRAM_BOT_TOKEN".to_string(),
                value: "123:abc".to_string(),
            },
            A::UpdateConfig {
                id: "tg".to_string(),
                body: serde_json::json!({"enabled": true}),
            },
        ];
        assert_eq!(
            applied, expect,
            "Confirm applies the snapshot's exact actions"
        );
        assert!(d.saving, "confirm enters the in-flight save state");

        // What the host executes in confirm_messaging_save is precisely this:
        // each applied action lowers to the REST call named for its kind —
        // `set_env_var` / `update_messaging` — and nothing else is derived.
        for act in &applied {
            let lowers_to = match act {
                A::SetEnv { .. } => "set_env_var",
                A::ClearEnv { .. } => "delete_env_var",
                A::UpdateConfig { .. } => "update_messaging",
            };
            assert!(
                matches!(lowers_to, "set_env_var" | "update_messaging"),
                "only the staged actions appear in the applied plan"
            );
        }
    }

    /// S2(b) required proof (RVS2B-FIX4): rendering a connector DETAIL whose
    /// live draft belongs to ANOTHER connector (A's save is in flight) shows
    /// the exact muted read-only label and renders NO confirmation plan and
    /// NO Confirm / Cancel controls — the `.when_some(confirm_plan)` dialog
    /// block is gated on `editable`, which is `false` whenever the draft does
    /// not belong to the rendered connector.
    ///
    /// The proof is on the PAINTED frame: the panel is rendered in a real
    /// test window and checked with `debug_bounds`, not on state alone. A
    /// positive control first paints A's editable detail with its open
    /// dialog (plan box, Confirm, Cancel all painted; no read-only label),
    /// so the "absent" assertions in the read-only frame mean something.
    ///
    /// End to end through the real renderer: `messaging_panel` auto-opens a
    /// draft for the selected connector, but `open_draft` refuses while a save
    /// is in flight (`saving == true`), so B's detail renders against A's
    /// live in-flight draft — exactly the read-only state.
    #[gpui::test]
    fn readonly_connector_detail_renders_label_without_plan(cx: &mut gpui::TestAppContext) {
        let Some(_home) = super::gateway_queue_tests::isolated(
            "work::tests::readonly_connector_detail_renders_label_without_plan",
        ) else {
            return;
        };
        use awili_gateway::rest::MessagingEnvVar;

        // Test-only view that renders the real `messaging_panel` into a
        // window so the painted frame can be inspected with `debug_bounds`.
        struct PanelHarness {
            app: gpui::Entity<AwiliApp>,
        }
        impl gpui::Render for PanelHarness {
            fn render(
                &mut self,
                window: &mut gpui::Window,
                cx: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                self.app.update(cx, |app, cx| {
                    app.messaging_panel(window, cx).into_any_element()
                })
            }
        }

        let entity = cx.new(AwiliApp::new);
        // Phase 0 (state): the gateway is configured and A's confirmation is
        // requested — but NOT yet confirmed. Stops before `confirm_save()`.
        entity.update(cx, |app, _cx| {
            // The gateway is configured; the panel renders the connector
            // detail (not the "connect a gateway" notice).
            app.work.hermes = super::HermesCfg {
                base_url: "http://127.0.0.1:8443".to_string(),
                api_key: "test".to_string(),
                enabled: true,
                dashboard_url: String::new(),
            };

            // TWO connectors. A (Telegram) carries a required credential so
            // its staged SetEnv makes the draft savable; B (WhatsApp) is the
            // one whose DETAIL we render read-only.
            let mut a = messaging_pane::tests_pub_platform("tg", "Telegram");
            a.enabled = true;
            a.configured = true;
            a.env_vars = vec![MessagingEnvVar {
                key: "TG_BOT_TOKEN".to_string(),
                required: true,
                is_set: true,
                redacted_value: Some("x".to_string()),
                description: String::new(),
                prompt: String::new(),
                help: String::new(),
                url: None,
                is_password: true,
                advanced: false,
            }];
            let b = messaging_pane::tests_pub_platform("wh", "WhatsApp");

            // Snapshot applied, loaded — the panel reaches the detail branch.
            app.messaging_data
                .state
                .apply_snapshot(messaging_pane::tests_pub_snap(vec![a.clone(), b.clone()]));
            app.messaging_data.loaded = true;
            app.messaging_data.state.select_id("tg");
            assert!(
                app.messaging_data.open_draft(Some(&a)),
                "A's draft opens for A"
            );

            // A stages one credential change; the confirmation opens.
            assert!(app.messaging_data.begin_credential_edit("TG_BOT_TOKEN"));
            assert!(app.messaging_data.commit_credential_edit("123:abc"));
            assert!(
                app.messaging_data.request_save_confirmation(),
                "A's dirty draft opens the save confirmation"
            );
        });

        // Phase 1 (positive control: A is editable and its dialog is open):
        // the plan box, Confirm and Cancel ARE painted, and the read-only
        // label is NOT — so the "absent" results in Phase 3 mean something.
        {
            let (_v, window) = cx.add_window_view(|_, _| PanelHarness {
                app: entity.clone(),
            });
            window.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(1000.)));
            window.run_until_parked();
            assert!(
                window.debug_bounds("msg-confirm-plan").is_some(),
                "positive control: the plan box is painted for A's editable detail"
            );
            assert!(
                window.debug_bounds("msg-confirm-save").is_some(),
                "positive control: Confirm is painted for A's editable detail"
            );
            assert!(
                window.debug_bounds("msg-cancel-save").is_some(),
                "positive control: Cancel is painted for A's editable detail"
            );
            assert!(
                window.debug_bounds("msg-readonly-label").is_none(),
                "positive control: the read-only label is absent for A's editable detail"
            );
        }

        // Phase 2 (state): confirm A's save (it is now IN FLIGHT) and select
        // B while it finishes.
        let conf = entity.update(cx, |app, _cx| {
            let conf = app
                .messaging_data
                .confirm_save()
                .expect("A's confirmation dispatches the in-flight save");
            assert!(app.messaging_data.saving, "A's save is in flight");

            // The user selects B while A's save finishes. Selection is
            // allowed; B's draft must NOT open (saving refuses it), so B's
            // detail renders against A's live draft: editable == false.
            app.messaging_data.state.select_id("wh");
            let wh = app
                .messaging_data
                .state
                .selected_platform()
                .expect("B is selected")
                .clone();
            assert!(
                !app.messaging_data.open_draft(Some(&wh)),
                "no draft opens for B while A's save is in flight"
            );
            assert_eq!(
                app.messaging_data.draft_id(),
                Some("tg"),
                "A's draft stays the live draft"
            );
            assert!(
                !app.messaging_data.draft_matches("wh"),
                "the live draft does not belong to B — B's detail is read-only"
            );
            // The in-flight save is pinned by the confirmed identity; no
            // confirmation dialog is open while it runs.
            assert!(
                app.messaging_data.confirm_plan().is_none(),
                "no dialog is open while A's save is in flight"
            );
            conf
        });

        // Phase 3 (the proof: B is read-only) — on the painted frame: the
        // exact read-only label is present and the dialog is absent.
        {
            let (_v, window) = cx.add_window_view(|_, _| PanelHarness {
                app: entity.clone(),
            });
            window.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(1000.)));
            window.run_until_parked();
            let label = window
                .debug_bounds("msg-readonly-label")
                .expect("read-only label is painted");
            assert!(
                label.size.height > gpui::px(0.),
                "the painted read-only label has non-zero height"
            );
            assert_eq!(
                AwiliApp::MESSAGING_READONLY_LABEL,
                "Read-only while another connector's save finishes.",
                "the exact read-only label text is pinned"
            );
            assert!(
                window.debug_bounds("msg-confirm-plan").is_none(),
                "the plan box is NOT painted while another save is in flight"
            );
            assert!(
                window.debug_bounds("msg-confirm-save").is_none(),
                "Confirm is NOT painted while another save is in flight"
            );
            assert!(
                window.debug_bounds("msg-cancel-save").is_none(),
                "Cancel is NOT painted while another save is in flight"
            );
        }

        // Phase 4 (state): the render gates hold, and A's save settles.
        entity.update(cx, |app, _cx| {
            // (a) the exact muted read-only label is present — proven via the
            // state the renderer gates it on (the label is rendered iff
            // `!editable`, and `editable` is `draft_matches`).
            assert!(
                !app.messaging_data.draft_matches("wh"),
                "the rendered label gate: `!editable` is true for B's detail"
            );
            // (b) NO confirmation plan: `confirm_plan` (which the dialog block
            // is built from) is absent while the save is in flight, so the
            // plan headline, its lines, and the `msg-confirm-save` /
            // `msg-cancel-save` controls do not render.
            assert!(
                app.messaging_data.confirm_plan().is_none(),
                "no confirmation plan renders while another save is in flight"
            );
            // (c) NO Confirm control: Confirm's dispatch is the host's
            // `confirm_messaging_save`, which is refused while `saving` — the
            // guard that matches the hidden `msg-confirm-save` element.
            assert!(
                app.messaging_data.confirm_save().is_none(),
                "the (hidden) Confirm control cannot dispatch while saving"
            );

            // A's save settles: the in-flight identity matches, the draft is
            // cleared, and B's selection becomes editable again.
            assert!(app.messaging_data.finish_save_success(&conf));
            assert!(!app.messaging_data.saving);
            assert!(app.messaging_data.draft.is_none());
        });
    }

    #[test]
    fn approval_delivery_card_retries_and_ignores_late_results() {
        fn raise(p: &mut RunPanel) {
            p.apply_event(&RunEvent::Approval(
                awili_gateway::run_events::ApprovalEvent {
                    approval_id: None,
                    kind: None,
                    prompt: Some("approve".into()),
                    // S2(a): no risk reported -> the lenient decode classifies
                    // it as the conservative closed-set member.
                    ..Default::default()
                },
            ));
        }
        let mut p = RunPanel::default();
        raise(&mut p);
        let first = p.approval.as_mut().unwrap().delivery.begin().unwrap();
        assert!(p.approval.as_mut().unwrap().delivery.begin().is_none());
        p.complete_approval_delivery(Some(&first), false);
        assert!(p.approval.as_ref().unwrap().delivery.failed);
        let retry = p.approval.as_mut().unwrap().delivery.begin().unwrap();
        assert_ne!(first, retry);
        p.complete_approval_delivery(Some(&first), true);
        assert!(p.approval.is_some());
        p.complete_approval_delivery(Some(&retry), true);
        assert!(p.approval.is_none());
        // A new card, even with the same/absent backend id, owns a new attempt.
        raise(&mut p);
        let old = p.approval.as_mut().unwrap().delivery.begin().unwrap();
        raise(&mut p);
        p.complete_approval_delivery(Some(&old), true);
        assert!(p.approval.is_some());
        // Cancel/terminal clears the card; late results cannot resurrect it.
        p.approval = None;
        p.complete_approval_delivery(Some(&old), false);
        assert!(p.approval.is_none());
    }

    #[test]
    fn worker_session_create_failure_reports_and_closes() {
        let mut fake = FakeOps::new(vec![]);
        fake.fail_session_create = true;
        let events = drive_loop(&fake, vec![]);
        assert_eq!(
            events.len(),
            1,
            "exactly one Failed, no Started: {events:?}"
        );
        assert!(
            matches!(&events[0], WorkEvent::Failed(m) if m.starts_with("session.create failed")),
            "got {events:?}"
        );
        assert_eq!(fake.ops(), vec!["session_create(/wd)", "close"]);
    }

    #[test]
    fn worker_idle_commands_map_one_to_one() {
        let fake = FakeOps::new(vec![]);
        let events = drive_loop(
            &fake,
            vec![
                GatewayCmd::Approval {
                    id: None,
                    choice: "once".into(),
                },
                GatewayCmd::Clarify {
                    request_id: "r1".into(),
                    answer: "yes".into(),
                },
                GatewayCmd::Steer("go left".into()),
                GatewayCmd::Attach {
                    path: "/a/b/c.txt".into(),
                },
                GatewayCmd::TerminalReadRespond {
                    request_id: "t1".into(),
                    text: "screen".into(),
                },
                GatewayCmd::SudoRespond {
                    request_id: "su1".into(),
                    password: "hunter2".into(),
                },
                GatewayCmd::SecretRespond {
                    request_id: "se1".into(),
                    value: "v".into(),
                },
            ],
        );
        assert_eq!(
            events,
            vec![
                WorkEvent::Started("s1".into()),
                WorkEvent::ApprovalDelivery {
                    id: None,
                    delivered: true
                },
            ]
        );
        assert_eq!(
            fake.ops(),
            vec![
                "session_create(/wd)",
                "approval_respond(s1,once)",
                "clarify_respond(s1,yes)",
                "session_steer(s1,go left)",
                "file_attach(s1,c.txt,/a/b/c.txt)",
                "terminal_read_respond(s1,t1,screen)",
                "sudo_respond(su1)",
                "secret_respond(se1)",
                "close",
            ]
        );
    }

    #[test]
    fn worker_turn_streams_then_completes_in_order() {
        use serde_json::json;
        let fake = FakeOps::new(vec![vec![
            gw_event("message.delta", json!({"text": "hello"})),
            gw_event("message.complete", json!({"status": "complete"})),
            gw_event("session.info", json!({"running": false})),
        ]]);
        let events = drive_loop(
            &fake,
            vec![GatewayCmd::Prompt {
                text: "hi".into(),
                system: None,
            }],
        );
        assert_eq!(events.len(), 4, "{events:?}");
        assert_eq!(events[0], WorkEvent::Started("s1".into()));
        assert_eq!(events[1], WorkEvent::Token("hello".into()));
        assert!(
            matches!(
                &events[2],
                WorkEvent::TurnCompleted {
                    status: RunStatus::Completed,
                    ..
                }
            ),
            "{events:?}"
        );
        assert_eq!(events[3], WorkEvent::TurnComplete);
        assert_eq!(
            fake.ops(),
            vec!["session_create(/wd)", "prompt_submit(s1,hi)", "close"]
        );
    }

    /// A gateway `error` event ends the turn: its translated `Failed` precedes
    /// the `TurnComplete`, and the loop returns to the command wait (the
    /// session survives — matching pre-D1 behavior).
    #[test]
    fn worker_error_event_order_failed_then_turn_complete() {
        use serde_json::json;
        let fake = FakeOps::new(vec![vec![gw_event("error", json!({"message": "boom"}))]]);
        let events = drive_loop(
            &fake,
            vec![GatewayCmd::Prompt {
                text: "hi".into(),
                system: None,
            }],
        );
        assert_eq!(
            events,
            vec![
                WorkEvent::Started("s1".into()),
                WorkEvent::Failed("boom".into()),
                WorkEvent::TurnComplete,
            ]
        );
        assert_eq!(
            fake.ops(),
            vec!["session_create(/wd)", "prompt_submit(s1,hi)", "close"]
        );
    }

    /// Mid-turn: approvals route to the client; a second Prompt is silently
    /// ignored (cannot submit mid-turn — pinned pre-D1 behavior).
    #[test]
    fn worker_midturn_approval_routes_and_prompt_ignored() {
        use serde_json::json;
        let fake = FakeOps::new(vec![vec![
            gw_event("message.delta", json!({"text": "a"})),
            gw_event("message.complete", json!({"status": "complete"})),
            gw_event("session.info", json!({"running": false})),
        ]]);
        let events = drive_loop(
            &fake,
            vec![
                GatewayCmd::Prompt {
                    text: "t1".into(),
                    system: None,
                },
                GatewayCmd::Approval {
                    id: None,
                    choice: "once".into(),
                },
                GatewayCmd::Prompt {
                    text: "t2-must-be-ignored".into(),
                    system: None,
                },
            ],
        );
        // One submit only; the approval landed; the mid-turn prompt vanished.
        assert_eq!(
            fake.ops(),
            vec![
                "session_create(/wd)",
                "prompt_submit(s1,t1)",
                "approval_respond(s1,once)",
                "close",
            ]
        );
        assert!(events.contains(&WorkEvent::TurnComplete));
    }

    /// Mid-turn: ALL six response commands route to the client with their
    /// request ids intact, in arrival order, while the turn is streaming
    /// (Codex round-1 P1). Six deltas keep the turn open for six mid-turn
    /// commands (one `try_recv` per streaming iteration), then the turn
    /// completes. Secrets never reach any log: the recorded ops carry request
    /// ids only, and the assertion proves the password/value strings are
    /// absent from the whole op log.
    #[test]
    fn worker_midturn_all_response_commands_route() {
        use serde_json::json;
        let mut turn: Vec<GatewayEvent> = (0..6)
            .map(|i| gw_event("message.delta", json!({"text": format!("d{i}")})))
            .collect();
        turn.push(gw_event("message.complete", json!({"status": "complete"})));
        turn.push(gw_event("session.info", json!({"running": false})));
        let fake = FakeOps::new(vec![turn]);
        let events = drive_loop(
            &fake,
            vec![
                GatewayCmd::Prompt {
                    text: "t1".into(),
                    system: None,
                },
                GatewayCmd::Clarify {
                    request_id: "r1".into(),
                    answer: "yes".into(),
                },
                GatewayCmd::Steer("go".into()),
                GatewayCmd::Attach {
                    path: "/a/b/c.txt".into(),
                },
                GatewayCmd::TerminalReadRespond {
                    request_id: "t1".into(),
                    text: "screen".into(),
                },
                GatewayCmd::SudoRespond {
                    request_id: "su1".into(),
                    password: "hunter2".into(),
                },
                GatewayCmd::SecretRespond {
                    request_id: "se1".into(),
                    value: "sk-secret".into(),
                },
            ],
        );
        assert_eq!(
            fake.ops(),
            vec![
                "session_create(/wd)",
                "prompt_submit(s1,t1)",
                "clarify_respond(s1,yes)",
                "session_steer(s1,go)",
                "file_attach(s1,c.txt,/a/b/c.txt)",
                "terminal_read_respond(s1,t1,screen)",
                "sudo_respond(su1)",
                "secret_respond(se1)",
                "close",
            ]
        );
        // Secret-safe logging: the secrets appear NOWHERE in the op log.
        let joined = fake.ops().join("|");
        assert!(!joined.contains("hunter2") && !joined.contains("sk-secret"));
        // The turn still streamed and completed around the command handling.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, WorkEvent::Token(_)))
                .count(),
            6
        );
        assert_eq!(events.last(), Some(&WorkEvent::TurnComplete));
    }

    /// Idle Cancel: interrupted best-effort (P3c round 5), session closed,
    /// Terminal{Stopped} reported, worker exits.
    #[test]
    fn worker_idle_cancel_reports_stopped() {
        let fake = FakeOps::new(vec![]);
        let events = drive_loop(&fake, vec![GatewayCmd::Cancel]);
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(
            &events[1],
            WorkEvent::Terminal {
                status: RunStatus::Stopped,
                output: None,
                model: None
            }
        ));
        assert_eq!(
            fake.ops(),
            vec![
                "session_create(/wd)",
                "session_interrupt(s1)",
                "session_close(s1)",
                "close"
            ]
        );
    }

    /// Idle Shutdown: session closed, NO Terminal event (the UI asked; it
    /// doesn't need a report), worker exits.
    #[test]
    fn worker_idle_shutdown_no_terminal() {
        let fake = FakeOps::new(vec![]);
        let events = drive_loop(&fake, vec![GatewayCmd::Shutdown]);
        assert_eq!(events, vec![WorkEvent::Started("s1".into())]);
        assert_eq!(
            fake.ops(),
            vec!["session_create(/wd)", "session_close(s1)", "close"]
        );
    }

    /// Mid-turn Shutdown: checked BEFORE the next event read — session closed,
    /// no further events, worker exits.
    #[test]
    fn worker_midturn_shutdown_exits_promptly() {
        use serde_json::json;
        let fake = FakeOps::new(vec![vec![gw_event(
            "message.delta",
            json!({"text": "never-delivered"}),
        )]]);
        let events = drive_loop(
            &fake,
            vec![
                GatewayCmd::Prompt {
                    text: "t1".into(),
                    system: None,
                },
                GatewayCmd::Shutdown,
            ],
        );
        assert_eq!(events, vec![WorkEvent::Started("s1".into())]);
        assert_eq!(
            fake.ops(),
            vec![
                "session_create(/wd)",
                "prompt_submit(s1,t1)",
                "session_close(s1)",
                "close"
            ]
        );
    }

    /// Connection lost mid-turn: Failed("gateway connection lost") and the
    /// worker exits. Pinned exactly as pre-D1: no session_close, no close.
    #[test]
    fn worker_connection_lost_reports_failed() {
        let fake = FakeOps::new(vec![vec![]]);
        fake.connected.store(false, Ordering::SeqCst);
        let events = drive_loop(
            &fake,
            vec![GatewayCmd::Prompt {
                text: "t1".into(),
                system: None,
            }],
        );
        assert_eq!(
            events,
            vec![
                WorkEvent::Started("s1".into()),
                WorkEvent::Failed("gateway connection lost".into()),
            ]
        );
        assert_eq!(
            fake.ops(),
            vec!["session_create(/wd)", "prompt_submit(s1,t1)"]
        );
    }

    /// The UI dropped the event receiver: the first failed send mid-turn makes
    /// the worker close the client and exit (no panic, no spin).
    #[test]
    fn worker_event_receiver_drop_exits() {
        use serde_json::json;
        let fake = FakeOps::new(vec![vec![
            gw_event("message.delta", json!({"text": "x"})),
            gw_event("message.complete", json!({"status": "complete"})),
        ]]);
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        cmd_tx
            .send(GatewayCmd::Prompt {
                text: "t1".into(),
                system: None,
            })
            .unwrap();
        drop(cmd_tx);
        let (ev_tx, ev_rx) = async_channel::unbounded();
        drop(ev_rx); // UI gone before the worker starts
        run_worker_loop(
            fake.clone(),
            WorkerContext {
                workdir: "/wd".into(),
                cmd_rx,
                cancel: Arc::new(AtomicBool::new(false)),
                events: ev_tx,
            },
        );
        assert_eq!(
            fake.ops(),
            vec!["session_create(/wd)", "prompt_submit(s1,t1)", "close"]
        );
    }

    /// Cancel-flag trip mid-turn: session closed, worker exits silently (the
    /// UI initiated the cancel and already knows — pinned pre-D1 behavior).
    #[test]
    fn worker_cancel_flag_midturn_closes_silently() {
        let fake = FakeOps::new(vec![vec![]]); // empty turn: loop spins on recv
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        cmd_tx
            .send(GatewayCmd::Prompt {
                text: "t1".into(),
                system: None,
            })
            .unwrap();
        let (ev_tx, ev_rx) = async_channel::unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = {
            let fake = fake.clone();
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                run_worker_loop(
                    fake,
                    WorkerContext {
                        workdir: "/wd".into(),
                        cmd_rx,
                        cancel,
                        events: ev_tx,
                    },
                )
            })
        };
        // Wait for the session, then trip the flag.
        let started = ev_rx.recv_blocking().unwrap();
        assert_eq!(started, WorkEvent::Started("s1".into()));
        cancel.store(true, Ordering::SeqCst);
        handle.join().unwrap();
        let mut rest = Vec::new();
        while let Ok(ev) = ev_rx.try_recv() {
            rest.push(ev);
        }
        assert!(rest.is_empty(), "silent exit expected, got {rest:?}");
        assert_eq!(
            fake.ops(),
            vec![
                "session_create(/wd)",
                "prompt_submit(s1,t1)",
                "session_close(s1)",
                "close"
            ]
        );
        drop(cmd_tx);
    }

    /// Session reuse: two prompts on one worker — ONE session_create, no
    /// reconnect, both turns complete (the multi-turn contract).
    #[test]
    fn worker_session_reuse_two_prompts_one_session() {
        use serde_json::json;
        let complete = || {
            vec![
                gw_event("message.complete", json!({"status": "complete"})),
                gw_event("session.info", json!({"running": false})),
            ]
        };
        let fake = FakeOps::new(vec![complete(), complete()]);
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = async_channel::unbounded();
        let handle = {
            let fake = fake.clone();
            std::thread::spawn(move || {
                run_worker_loop(
                    fake,
                    WorkerContext {
                        workdir: "/wd".into(),
                        cmd_rx,
                        cancel: Arc::new(AtomicBool::new(false)),
                        events: ev_tx,
                    },
                )
            })
        };
        let wait_for_turn_complete = |label: &str| loop {
            match ev_rx.recv_blocking() {
                Ok(WorkEvent::TurnComplete) => break,
                Ok(_) => continue,
                Err(e) => panic!("channel closed waiting for {label}: {e}"),
            }
        };
        cmd_tx
            .send(GatewayCmd::Prompt {
                text: "first".into(),
                system: None,
            })
            .unwrap();
        wait_for_turn_complete("turn 1");
        cmd_tx
            .send(GatewayCmd::Prompt {
                text: "second".into(),
                system: None,
            })
            .unwrap();
        wait_for_turn_complete("turn 2");
        drop(cmd_tx);
        handle.join().unwrap();
        let ops = fake.ops();
        assert_eq!(
            ops,
            vec![
                "session_create(/wd)",
                "prompt_submit(s1,first)",
                "prompt_submit(s1,second)",
                "close",
            ],
            "one session, two submits, no reconnect/session_close"
        );
    }

    // ========================================================================
    // D1 seam — LIVE gate (Codex merge constraint 5). Requires the Hermes
    // dashboard gateway on 127.0.0.1:9119. Run explicitly:
    //   cargo test -p awili-app -- --ignored live_gateway_two_turn_seam
    // ========================================================================

    #[test]
    #[ignore = "live gate: requires the Hermes dashboard gateway on 127.0.0.1:9119"]
    fn live_gateway_two_turn_seam() {
        let cfg = GatewayCfg {
            dashboard_url: "http://127.0.0.1:9119".into(),
            enabled: true,
        };
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = async_channel::unbounded();
        let handle = awili_work::spawn_worker(
            Box::new(HermesGatewayWorker { cfg }),
            WorkerContext {
                workdir: std::env::temp_dir().display().to_string(),
                cmd_rx,
                cancel: Arc::new(AtomicBool::new(false)),
                events: ev_tx,
            },
        );

        let deadline = Duration::from_secs(180);
        let mut transcript: Vec<String> = Vec::new();
        let run_turn = |transcript: &mut Vec<String>, prompt: &str, turn: usize| {
            cmd_tx
                .send(GatewayCmd::Prompt {
                    text: prompt.into(),
                    system: None,
                })
                .unwrap();
            let start = std::time::Instant::now();
            let mut tokens = 0usize;
            loop {
                assert!(
                    start.elapsed() < deadline,
                    "turn {turn} timed out; transcript: {transcript:#?}"
                );
                match ev_rx.try_recv() {
                    Ok(ev) => {
                        if matches!(ev, WorkEvent::Token(_)) {
                            tokens += 1;
                        }
                        let line = format!("turn{turn}: {ev:?}");
                        eprintln!("{line}");
                        let done = matches!(ev, WorkEvent::TurnComplete);
                        if let WorkEvent::Failed(msg) = &ev {
                            panic!("turn {turn} failed: {msg}; transcript: {transcript:#?}");
                        }
                        transcript.push(line);
                        if done {
                            break;
                        }
                    }
                    Err(async_channel::TryRecvError::Empty) => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(async_channel::TryRecvError::Closed) => {
                        panic!("worker exited mid-turn {turn}; transcript: {transcript:#?}");
                    }
                }
            }
            tokens
        };

        // Turn 1: prompt → streaming → completion.
        let t1_tokens = run_turn(
            &mut transcript,
            "Reply with exactly the single word: pong",
            1,
        );
        assert!(
            transcript.iter().any(|l| l.contains("Started(")),
            "no Started before turn 1 completed: {transcript:#?}"
        );
        assert!(t1_tokens > 0, "turn 1 streamed no tokens: {transcript:#?}");

        // Turn 2 on the SAME worker: session reuse.
        let t2_tokens = run_turn(
            &mut transcript,
            "Reply with exactly the single word: pong2",
            2,
        );
        assert!(t2_tokens > 0, "turn 2 streamed no tokens: {transcript:#?}");
        assert_eq!(
            transcript.iter().filter(|l| l.contains("Started(")).count(),
            1,
            "session was recreated between turns: {transcript:#?}"
        );

        cmd_tx.send(GatewayCmd::Shutdown).unwrap();
        handle.join().unwrap();
    }

    // -- D2-u2a: engine selection --------------------------------------------

    fn awili_engine_cfg() -> awili_config::EngineConfig {
        awili_config::EngineConfig {
            kind: awili_config::EngineKind::Awili,
            base_url: "http://localhost:18000".into(),
            model: "laguna-s-2.1-nvfp4".into(),
            ..Default::default()
        }
    }

    /// The gateway engine's precondition is unchanged by the new flag: it still
    /// needs `[gateway]`, and still refuses with the same message.
    #[test]
    fn gateway_engine_still_requires_the_gateway() {
        let cfg = awili_config::EngineConfig::default();
        assert!(engine_precondition(&cfg, true).is_ok());
        assert_eq!(
            engine_precondition(&cfg, false).unwrap_err(),
            "Gateway not configured (set [gateway] in config.toml)"
        );
    }

    /// The awili engine does NOT require the gateway — that is the whole point of
    /// the flag — but it does require its own endpoint and model.
    #[test]
    fn awili_engine_requires_its_own_endpoint_not_the_gateway() {
        assert!(
            engine_precondition(&awili_engine_cfg(), false).is_ok(),
            "the awili engine must run with no gateway configured at all"
        );

        let mut no_url = awili_engine_cfg();
        no_url.base_url = "   ".into();
        assert!(engine_precondition(&no_url, true)
            .unwrap_err()
            .contains("engine.base_url"));

        let mut no_model = awili_engine_cfg();
        no_model.model = String::new();
        assert!(engine_precondition(&no_model, true)
            .unwrap_err()
            .contains("engine.model"));
    }

    /// Grants are per-folder: two folders never share a grant file, and the same
    /// folder always resolves to the same one (approvals must survive a restart).
    #[test]
    fn grant_key_is_stable_per_folder_and_distinct_across_folders() {
        assert_eq!(
            folder_grant_key("/Users/j/Projects/awili"),
            folder_grant_key("/Users/j/Projects/awili"),
            "a folder's grants must survive across sessions"
        );
        assert_ne!(
            folder_grant_key("/Users/j/Projects/awili"),
            folder_grant_key("/Users/j/Projects/hub"),
        );
    }

    /// Same basename, different parent — the key must still separate them, or an
    /// approval granted in one project would silently apply in another.
    #[test]
    fn grant_key_separates_folders_sharing_a_basename() {
        assert_ne!(
            folder_grant_key("/a/notes"),
            folder_grant_key("/b/notes"),
            "the full path, not just the basename, must key the grants"
        );
    }

    /// A key can never introduce a path separator or climb out of the artifact
    /// directory, whatever the conversation id or folder name contains.
    #[test]
    fn artifact_keys_stay_a_single_safe_path_component() {
        for raw in ["../../etc/passwd", "a/b", "с..\\win", "", "  "] {
            let key = sanitize_artifact_key(raw);
            assert!(!key.contains('/'), "{key:?} escaped via /");
            assert!(!key.contains('\\'), "{key:?} escaped via backslash");
            assert!(!key.is_empty());
            assert_ne!(key, "..", "{key:?} is a parent reference");
        }
        assert!(!folder_grant_key("/a/../../etc").contains('/'));
    }

    /// RC2-GEMMA5 (R7-A3): the engine's "reply cut off, asking again"
    /// signal takes the cut text back from the end of the reply (never other
    /// text), and the progress line says why in plain words.
    #[test]
    fn rc2gemma5_a_cut_reply_is_taken_back() {
        let ups = work_event_to_updates(WorkEvent::StatusUpdate {
            kind: Some(awili_engine_worker::REPLY_RESTART.into()),
            message: Some("HERON".into()),
        });
        assert!(matches!(ups.as_slice(), [WorkUpdate::ReplyRestart(s)] if s == "HERON"));
        assert_eq!(
            taken_back("Earlier.\n\nHERON", "HERON").as_deref(),
            Some("Earlier.\n\n")
        );
        assert_eq!(taken_back("HERON", "HERON").as_deref(), Some(""));
        assert_eq!(
            taken_back("other text", "HERON"),
            None,
            "only the shown text goes"
        );
        assert_eq!(
            taken_back("anything", ""),
            None,
            "nothing shown, nothing taken"
        );
        assert!(!REPLY_RESTART_LINE.contains("stream") && !REPLY_RESTART_LINE.contains("truncat"));
    }
}

pub fn relative_time(unix_ts: f64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let delta = (now - unix_ts).max(0.0) as u64;
    if delta < 60 {
        "just now".to_string()
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86400 {
        format!("{}h ago", delta / 3600)
    } else {
        format!("{}d ago", delta / 86400)
    }
}

/// What the selected engine needs before a run can start; the user-facing
/// reason when it is not satisfied.
///
/// Pure so the matrix is unit-testable without an app instance.
pub(crate) const GATEWAY_SETUP_NOTICE: &str =
    "Gateway not configured (set [gateway] in config.toml)";
pub(crate) const ENGINE_ENDPOINT_NOTICE: &str =
    "Engine not configured (set engine.base_url in config.toml)";
pub(crate) const ENGINE_MODEL_NOTICE: &str =
    "Engine not configured (set engine.model in config.toml)";
/// The error for a conversation whose chosen in-app model (an `[[engines]]`
/// entry) is no longer configured. A setup error (the model is missing), never
/// a silent switch to another model.
pub(crate) const UNKNOWN_ENGINE_NOTICE: &str =
    "This conversation's in-app model is no longer set up";
pub(crate) fn unknown_engine_notice(id: &str) -> String {
    format!("{UNKNOWN_ENGINE_NOTICE} (no [[engines]] entry with id \"{id}\" in config.toml). Choose another model.")
}
pub(crate) fn is_engine_setup_notice(cause: &str) -> bool {
    if cause.starts_with(UNKNOWN_ENGINE_NOTICE) {
        return true;
    }
    [
        GATEWAY_SETUP_NOTICE,
        ENGINE_ENDPOINT_NOTICE,
        ENGINE_MODEL_NOTICE,
    ]
    .contains(&cause)
}

fn engine_precondition(
    cfg: &awili_config::EngineConfig,
    gateway_enabled: bool,
) -> Result<(), String> {
    match cfg.kind {
        // The typed gateway worker needs `[gateway]` configured (unit 16
        // removed the `/v1/runs` REST fallback; there is no `[hermes]`-only
        // degraded path anymore).
        awili_config::EngineKind::HermesGateway => {
            if gateway_enabled {
                Ok(())
            } else {
                Err(GATEWAY_SETUP_NOTICE.into())
            }
        }
        // The awili engine needs an endpoint and a model. These are checked here
        // rather than at parse time so a half-filled `[engine]` stanza cannot
        // break launch for someone still on the gateway.
        awili_config::EngineKind::Awili => {
            if cfg.base_url.trim().is_empty() {
                return Err(ENGINE_ENDPOINT_NOTICE.into());
            }
            if cfg.model.trim().is_empty() {
                return Err(ENGINE_MODEL_NOTICE.into());
            }
            Ok(())
        }
    }
}

// -- engine artifact paths (D2-u2a) ------------------------------------------

/// Where `awili-engine` keeps its per-session logs and per-folder grants:
/// `<data_dir>/Awili/engine`, alongside `awili.db` and the feed cache.
pub(crate) fn engine_data_root() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(awili_config::APP_DIR_NAME).join("engine"))
}

/// The in-app engine's execution log for conversation `conv_id` under the
/// engine data `root`.
pub(crate) fn engine_log_path(root: &Path, conv_id: &str) -> PathBuf {
    root.join("logs")
        .join(format!("{}.log", sanitize_artifact_key(conv_id)))
}

/// Remove a deleted conversation's engine log once nothing holds it: the
/// worker holds an exclusive lock on the file while it lives, so wait (up to
/// `wait`) for that lock, then unlink. A missing file is fine. `true` when the
/// file is gone.
pub(crate) fn remove_engine_log(path: &Path, wait: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + wait;
    loop {
        let file = match std::fs::OpenOptions::new().append(true).open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
            Err(_) => return false,
        };
        match file.try_lock() {
            Ok(()) => {
                let gone = std::fs::remove_file(path).is_ok();
                drop(file);
                return gone;
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return false,
        }
    }
}

/// Deleting a conversation: its engine log goes once the in-app worker has
/// let go of it (the worker, if it is this conversation's, is retired first).
pub(crate) fn delete_engine_log(work: &mut WorkState, conv_id: &str) {
    // A running turn is never stopped by a delete: it finishes, then the
    // worker is retired (as when the owner leaves its conversation).
    let mut wait = std::time::Duration::from_secs(30);
    if work.worker_conv.as_deref() == Some(conv_id) {
        if work.running || work.gateway_stop_pending {
            work.retire_after_run = true;
            wait = std::time::Duration::from_secs(600);
        } else {
            retire_in_app_worker(work);
        }
    }
    let Some(root) = engine_data_root() else {
        return;
    };
    let path = engine_log_path(&root, conv_id);
    if !path.exists() {
        return;
    }
    std::thread::spawn(move || {
        if !remove_engine_log(&path, wait) {
            eprintln!("awili: could not remove the engine log {}", path.display());
        }
    });
}

/// Reduce an arbitrary identifier to something safe as a single filename
/// component. Anything outside `[A-Za-z0-9._-]` becomes `_`, so a key can never
/// introduce a path separator or escape the artifact directory.
fn sanitize_artifact_key(key: &str) -> String {
    let cleaned: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "unnamed".to_string()
    } else {
        cleaned
    }
}

/// FNV-1a, 64-bit. Written out rather than using `DefaultHasher` because the
/// standard hasher makes NO cross-version stability guarantee — grants keyed by
/// it would silently reset on a toolchain upgrade, quietly re-prompting for
/// approvals the user had already granted. FNV is fixed by its definition.
#[allow(dead_code)] // exercised by the tests below
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The grant-file name for a bound folder: readable basename plus a stable hash
/// of the FULL path, so two folders sharing a basename never share grants.
#[allow(dead_code)] // exercised by the tests below
fn folder_grant_key(workdir: &str) -> String {
    let base = std::path::Path::new(workdir)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    format!(
        "{}-{:016x}",
        sanitize_artifact_key(&base),
        fnv1a64(workdir.as_bytes())
    )
}

/// P3c: gateway turn end (the `is_turn_end` conjunction) and Stop-as-interrupt,
/// driven through the scripted [`GatewayOps`] fake in `tests`.
#[cfg(test)]
#[path = "work_gateway_turn_tests.rs"]
mod gateway_turn_tests;

/// P3c rounds 2-3: the Stop lane end to end — the app's own Stop keeps the
/// gateway session, an unconfirmed drain closes it (reporting `Stopped` before
/// the close RPC), the drain keeps denial evidence and answers no approval.
#[cfg(test)]
#[path = "work_gateway_stop_tests.rs"]
mod gateway_stop_tests;

/// P3c round 3: a prompt submitted during a pending gateway Stop is parked and
/// replayed on the acknowledgement; a repeated Stop keeps the session.
#[cfg(test)]
#[path = "work_gateway_queue_tests.rs"]
mod gateway_queue_tests;

/// RC3-S5: messages typed during a run queue, drain, edit, cancel, send now.
#[cfg(test)]
#[path = "work_send_queue_tests.rs"]
mod send_queue_tests;

/// L1b: the selected Work executor decides the engine — the in-app default,
/// the fleet gateway reading as Hermes regardless of `[engine].kind`,
/// the endpoint preconditions, D6 lane retirement and the in-app approval
/// path, each end to end in an isolated HOME.
#[cfg(test)]
#[path = "work_local_first_tests.rs"]
mod local_first_tests;

#[cfg(test)]
#[path = "work_inapp_files_tests.rs"]
mod inapp_files_tests;

/// `[[engines]]`: additional in-app models next to the primary.
#[cfg(test)]
#[path = "multi_engine_tests.rs"]
mod multi_engine_tests;

#[cfg(test)]
#[path = "work_inapp_approval_tests.rs"]
mod inapp_approval_tests;

#[cfg(test)]
#[path = "work_rc1m_tests.rs"]
mod rc1m_tests;

/// RC1-M-FIX6 test seam: when `Some`, every event [`RunPanel::apply_event`]
/// folds is appended with its arrival time — the live gates read approval
/// and tool events as they happened, not by sampling the panel. Process-wide:
/// only a test running in its own process turns it on.
#[cfg(test)]
pub(crate) static RUN_EVENT_TAP: std::sync::Mutex<Option<Vec<(std::time::Instant, RunEvent)>>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
#[path = "work_rc1m_live_tests.rs"]
mod rc1m_live_tests;

/// P3c round 4: the stop barrier — while a gateway Stop is pending, activation
/// cannot re-own the Work panel and a folder change is deferred to the ack.
#[cfg(test)]
#[path = "work_gateway_barrier_tests.rs"]
mod gateway_barrier_tests;

/// P3c round 5: a Stop around the prompt submission — a failed submission
/// under a pending Stop still interrupts, an idle Stop interrupts first, and a
/// Stop queued before the submission never submits.
#[cfg(test)]
#[path = "work_gateway_submit_tests.rs"]
mod gateway_submit_tests;

/// P2f: Cancel run on a local build closes its row and records once; a stale
/// verdict records `Failed { stale: … }` once; an orphaned row closes.
#[cfg(test)]
#[path = "work_build_staleness_tests.rs"]
mod build_staleness_tests;

/// P3c round 6 (R13): a `Failed` buffered before a Stop is evidence on the
/// stopped turn, never the Stop's acknowledgement.
#[cfg(test)]
#[path = "work_gateway_failed_tests.rs"]
mod gateway_failed_tests;

/// S5(a) FIX1: turn-level proof that a rejected memory policy never
/// reaches the Work worker or the Chat provider.
#[cfg(test)]
#[path = "work_s5a_policy_tests.rs"]
mod s5a_policy_tests;

/// S5 INT1: the Work agent lane and Chat pin the gate's validated policy
/// block into the agent prompt, and a rejected policy spawns no agent.
#[cfg(test)]
#[path = "work_s5_int1_tests.rs"]
mod s5_int1_tests;

/// Workshop dispatch: real contextual utterances, a restored legacy Chat
/// thread, and Stop all go through the one tool-capable path.
#[cfg(test)]
#[path = "workshop_dispatch_tests.rs"]
mod workshop_dispatch_tests;

/// An approved follow-up and a pending memory gate: the captured owner runs
/// once while the next target/folder change; refusal and Stop keep the session
/// and staged files.
#[cfg(test)]
#[path = "work_gate_follow_tests.rs"]
mod gate_follow_tests;

/// P3d: a gateway Stop answers the approval card that was showing with
/// `deny` before the interrupt; without a card it answers nothing.
#[cfg(test)]
#[path = "work_gateway_approval_stop_tests.rs"]
mod gateway_approval_stop_tests;

/// P3d round 2: the Stop's deny keeps its FIFO place behind commands already
/// queued, and a deny that completes the turn routes the `Cancel` to the
/// idle path.
#[cfg(test)]
#[path = "work_gateway_approval_order_tests.rs"]
mod gateway_approval_order_tests;

#[cfg(test)]
#[path = "work_clarify_tests.rs"]
mod clarify_tests;

/// P3i: a `Terminal { Stopped }` leaves the panel error-free (the header
/// reads `stopped`), and a `Terminal { Failed }` with no worker output still
/// synthesizes its `run ended: failed` reason.
#[cfg(test)]
#[path = "work_gateway_terminal_reason_tests.rs"]
mod gateway_terminal_reason_tests;

/// P3e: the connection card's label and Test button recognise a gateway-only
/// setup (label precedence, test target, the gateway scrape probe).
#[cfg(test)]
#[path = "work_connection_card_tests.rs"]
mod connection_card_tests;
/// P3c race hunt: adversarial interleavings of the gateway Work lane not
/// pinned by rounds 1-6 (ignored tests reproduce open defects).
#[cfg(test)]
#[path = "work_gateway_hunt_tests.rs"]
mod gateway_hunt_tests;

/// M2d: the per-reply run footer (F02/F03/S2-F05) — a gateway turn gets its
/// footer from the ledger line, and the Chat context meter is hidden outside
/// Chat.
#[cfg(test)]
#[path = "work_run_footer_tests.rs"]
mod run_footer_tests;

/// M2g: Work-mode Retry (F09/F12) — a gateway turn gets re-sent on the
/// executor the chip showed at click time, and Up recalls the last sent
/// prompt in the main composer.
#[cfg(test)]
#[path = "work_retry_tests.rs"]
mod retry_tests;

/// R1h: a Work run's end re-reads the folder (the Files card matches the
/// disk), and the status reads "waiting for your OK" while a plan waits.
#[cfg(test)]
#[path = "work_r1h_tests.rs"]
mod r1h_tests;

/// M1a (S2-F01): a gateway Stop the worker never acknowledges is abandoned at
/// the 15 s deadline — the old worker is shut down and forgotten, the stopped
/// turn ends via the existing reducer, and the parked prompt is replayed once
/// on a fresh worker.
#[cfg(test)]
#[path = "work_gateway_stop_deadline_tests.rs"]
mod gateway_stop_deadline_tests;

/// M1c (S3-F03): export the conversation on screen — source routing, the
/// code-tab transcript shape, and the never-overwriting file write.
#[cfg(test)]
#[path = "work_export_tests.rs"]
mod work_export_tests;

#[cfg(test)]
mod ship_p0_5_tests {
    //! Ship-fix P0-5: a configured Work MCP composition that cannot be
    //! satisfied FAILS the run instead of silently degrading to
    //! builtin-only — with the reserved server explicitly required
    //! whenever it is configured.

    use super::*;

    fn cfg_with(servers_toml: &str) -> awili_config::Config {
        awili_config::Config::parse(servers_toml).expect("test config parses")
    }

    #[test]
    fn no_configured_servers_is_builtin_only_not_an_error() {
        let cfg = cfg_with("");
        let out = AwiliApp::compose_work_mcp_from("/tmp", &cfg);
        assert!(matches!(out, Ok(None)), "builtin-only is a valid posture");
    }

    #[test]
    fn every_server_failing_composition_fails_the_run() {
        // A relative program path is refused at catalog composition.
        let cfg = cfg_with(
            r#"
[[mcp.servers]]
name    = "helper"
command = "relative/not-absolute"
modes   = ["work"]
"#,
        );
        let out = AwiliApp::compose_work_mcp_from("/tmp", &cfg);
        let err = out.err().expect("must fail, not silently degrade");
        assert!(err.contains("failed composition"), "{err}");
    }

    #[test]
    fn a_failed_reserved_server_fails_the_run_even_with_other_origins() {
        let cfg = cfg_with(
            r#"
[[mcp.servers]]
name    = "helper"
command = "/usr/bin/true"
modes   = ["work"]

[[mcp.servers]]
name    = "mac-shell"
command = "relative/mac-shell"
trust   = "install"
modes   = ["work"]
"#,
        );
        let out = AwiliApp::compose_work_mcp_from("/tmp", &cfg);
        let err = out.err().expect("losing mac-shell must fail the run");
        assert!(err.contains("mac-shell"), "{err}");
    }
}

#[cfg(test)]
mod p12_connectors_tests {
    //! P12: the owned engine's launch catalog includes the synthesised
    //! `awili-connectors` server when a connector family is enabled — the
    //! runtime emits one `OriginDisconnected` per catalog origin at
    //! construction, so the origin is present regardless of the server
    //! child's liveness (the fake binary here exits immediately).

    use super::*;

    #[test]
    fn owned_engine_catalog_includes_connectors() {
        let dir = tempfile::tempdir().expect("temp dir");
        // A spawnable fake: the origin is registered at construction, before
        // the child ever answers — liveness is not what this test proves.
        let bin = dir.path().join(crate::connectors_mcp::CONNECTORS_BIN);
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").expect("fake binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }

        let mut cfg = awili_config::Config::parse("").expect("empty config parses");
        cfg.outline.enabled = true;
        cfg.outline.base_url = "https://outline.example.test/api".to_string();
        cfg.gitea.enabled = true;
        cfg.gitea.base_url = "https://gitea.example.test/api/v1".to_string();
        // The binary resolves from this temp search path (never hard-coded).
        cfg.work.agents.search_paths = vec![dir.path().to_string_lossy().into_owned()];

        let mcp = AwiliApp::compose_work_mcp_from("/tmp", &cfg)
            .expect("composition must succeed")
            .expect("an enabled family ⇒ Some(mcp), not builtin-only");

        // The owned engine's catalog must carry the connectors origin.
        let facade = mcp.coordinator.facade();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut found = false;
        loop {
            let snap = facade.lifecycle();
            if snap.origins.contains_key("mcp:awili-connectors") {
                found = true;
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            found,
            "catalog origins must include mcp:awili-connectors, got: {:?}",
            facade.lifecycle().origins.keys().collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod executor_persist_tests {
    use super::*;

    #[test]
    fn executor_persist_roundtrip() {
        let cases = [
            WorkExecutor::Gateway,
            WorkExecutor::Agent {
                engine: CodeEngine::Claude,
                model: None,
            },
            WorkExecutor::Agent {
                engine: CodeEngine::Codex,
                model: Some("gpt-5.6-sol".into()),
            },
            WorkExecutor::Agent {
                engine: CodeEngine::Glm,
                model: None,
            },
            // A non-empty local-build target id round-trips (P10/F4 finding 5);
            // the id may itself contain ':' — only the `local-build:` prefix is
            // stripped.
            WorkExecutor::LocalBuild {
                target_id: "a:b".into(),
            },
            // P9: dsh round-trips to CodeEngine::Dsh (with and without a model).
            WorkExecutor::Agent {
                engine: CodeEngine::Dsh,
                model: None,
            },
            WorkExecutor::Agent {
                engine: CodeEngine::Dsh,
                model: Some("deepseek-flash".into()),
            },
        ];
        for c in cases {
            assert_eq!(WorkExecutor::from_persist(&c.to_persist()), Some(c));
        }
        // Garbage and unknown engines fall back to None (caller keeps Gateway).
        assert_eq!(WorkExecutor::from_persist(""), None);
        assert_eq!(WorkExecutor::from_persist("agent:unknown"), None);
        assert_eq!(WorkExecutor::from_persist("agent:"), None);
        // An empty local-build target id is unrecognizable, like the rest.
        assert_eq!(WorkExecutor::from_persist("local-build:"), None);
    }

    /// P9: the composer's model chip (the per-turn picker) is hidden in BOTH
    /// Work (the executor chip replaces it) and Chat (pinned to
    /// `[chat].executor` — no picker there), and shown in every other mode.
    #[test]
    fn model_chip_hidden_in_work_and_chat() {
        assert!(!Mode::Work.shows_model_chip());
        for m in [
            Mode::Code,
            Mode::Skills,
            Mode::Cron,
            Mode::Sessions,
            Mode::CommandCenter,
            Mode::Messaging,
        ] {
            assert!(m.shows_model_chip(), "{m:?} must show the model chip");
        }
    }
}

#[cfg(test)]
mod agent_config_tests {
    use super::*;
    use awili_config::{AgentBinsConfig, ModelsConfig};

    /// An explicit `[work.agents].*_bin` wins over discovery, `~` expands,
    /// and an unconfigured engine falls back to the bare name for PATH.
    #[test]
    fn explicit_bin_beats_discovery() {
        let cfg = AgentBinsConfig {
            claude_bin: Some("/custom/claude-cli".into()),
            zcode_bin: Some("~/bin/zcode".into()),
            search_paths: vec!["/nonexistent".into()],
            ..Default::default()
        };
        assert_eq!(resolve_agent_bin("claude", &cfg), "/custom/claude-cli");
        let home = crate::home::home_string();
        assert_eq!(
            resolve_agent_bin("zcode", &cfg),
            format!("{home}/bin/zcode")
        );
        // Not configured and not found in search paths → bare name.
        assert_eq!(resolve_agent_bin("codex", &cfg), "codex");
        // An empty string is treated as unset, not as a path.
        let blank = AgentBinsConfig {
            codex_bin: Some("   ".into()),
            search_paths: vec!["/nonexistent".into()],
            ..Default::default()
        };
        assert_eq!(resolve_agent_bin("codex", &blank), "codex");
    }

    /// Claude's selectable aliases come from config; replacing them requires
    /// no rebuild, and the defaults match what used to be compiled in.
    #[test]
    fn claude_aliases_come_from_config() {
        let d = ModelsConfig::default();
        assert_eq!(
            agent_model_options(CodeEngine::Claude, &d),
            vec!["fable", "opus", "sonnet", "haiku"]
        );
        let custom = ModelsConfig {
            claude_aliases: vec!["opus-next".into(), "haiku-next".into()],
            ..Default::default()
        };
        assert_eq!(
            agent_model_options(CodeEngine::Claude, &custom),
            vec!["opus-next", "haiku-next"]
        );
        // GLM still offers nothing: ZCode has no per-run model mechanism.
        assert!(agent_model_options(CodeEngine::Glm, &custom).is_empty());
    }

    /// P9: dsh's explicit bin wins over discovery (`~` expands), an
    /// unconfigured dsh falls back to the search paths / bare name, and the
    /// selectable models come from `[models].dsh_models` (the two headless
    /// route ids by default; a configured list replaces them).
    #[test]
    fn dsh_bin_and_models() {
        let cfg = AgentBinsConfig {
            dsh_bin: Some("/custom/dsh".into()),
            search_paths: vec!["/nonexistent".into()],
            ..Default::default()
        };
        assert_eq!(resolve_agent_bin("dsh", &cfg), "/custom/dsh");
        let tilde = AgentBinsConfig {
            dsh_bin: Some("~/bin/dsh".into()),
            search_paths: vec!["/nonexistent".into()],
            ..Default::default()
        };
        let home = crate::home::home_string();
        assert_eq!(resolve_agent_bin("dsh", &tilde), format!("{home}/bin/dsh"));
        // Not configured and not in search paths → bare name for PATH.
        let blank = AgentBinsConfig {
            dsh_bin: Some("   ".into()),
            search_paths: vec!["/nonexistent".into()],
            ..Default::default()
        };
        assert_eq!(resolve_agent_bin("dsh", &blank), "dsh");

        // Models: the two default headless-route ids.
        let d = ModelsConfig::default();
        assert_eq!(
            agent_model_options(CodeEngine::Dsh, &d),
            vec!["deepseek-flash".to_string(), "deepseek-v4-pro".to_string()]
        );
        // A configured list replaces the defaults (vendor names are data).
        let custom = ModelsConfig {
            dsh_models: vec!["deepseek-r1".into()],
            ..Default::default()
        };
        assert_eq!(
            agent_model_options(CodeEngine::Dsh, &custom),
            vec!["deepseek-r1".to_string()]
        );
    }

    /// P9: the AGENTS popover section offers Dsh exactly once, labelled
    /// "DeepSeek · …" (one "default" row + one row per dsh_models entry), the
    /// same shape as the Claude rows.
    #[test]
    fn work_executor_rows_offer_dsh_once_with_deepseek_labels() {
        let d = ModelsConfig::default();
        let rows = work_executor_agent_rows(&d);
        // Exactly one "DeepSeek · default" row and one row per default model.
        let dsh_rows: Vec<&(String, WorkExecutor)> = rows
            .iter()
            .filter(|(label, _)| label.starts_with("DeepSeek \u{00B7}"))
            .collect();
        assert_eq!(
            dsh_rows.len(),
            3,
            "expected default + 2 model rows: {rows:?}"
        );
        assert!(dsh_rows
            .iter()
            .any(|(l, _)| l == "DeepSeek \u{00B7} default"));
        assert!(dsh_rows
            .iter()
            .any(|(l, _)| l == "DeepSeek \u{00B7} deepseek-flash"));
        assert!(dsh_rows
            .iter()
            .any(|(l, _)| l == "DeepSeek \u{00B7} deepseek-v4-pro"));
        // The engine array contains Dsh exactly once (no duplicate section).
        let dsh_engine_rows = rows
            .iter()
            .filter(|(_, e)| matches!(e, WorkExecutor::Agent { engine, .. } if engine == &CodeEngine::Dsh))
            .count();
        assert_eq!(dsh_engine_rows, 3);
        // The default row carries model=None, the model rows carry their id.
        let by_label = |l: &str| rows.iter().find(|(lbl, _)| lbl == l).unwrap().1.clone();
        assert_eq!(
            by_label("DeepSeek \u{00B7} default"),
            WorkExecutor::Agent {
                engine: CodeEngine::Dsh,
                model: None,
            }
        );
        assert_eq!(
            by_label("DeepSeek \u{00B7} deepseek-flash"),
            WorkExecutor::Agent {
                engine: CodeEngine::Dsh,
                model: Some("deepseek-flash".into()),
            }
        );
    }

    #[cfg(unix)]
    /// U4d: the posture now comes from the conversation's Ask/Auto mode, not
    /// from a saved Code project — so a Work key with no stored choice
    /// resolves, for Claude, to the read-only planning step under the default
    /// (Ask) mode, and a saved Code project for that folder changes nothing.
    #[test]
    fn unsaved_work_folder_uses_product_default_autonomy() {
        use crate::ask_mode::{autonomy_for, code_support, effective, AskModes};
        use awili_code::Autonomy;

        let modes = AskModes::default();
        let key = AskKey::Work("/tmp/reviewed".to_string());
        // The default mode is Ask.
        assert_eq!(
            modes.mode_for(&key),
            awili_work::ApprovalMode::AskBeforeChanges
        );
        // Claude enforces the planning step, so Ask resolves to AskFirst.
        assert_eq!(
            autonomy_for(effective(
                modes.mode_for(&key),
                code_support(CodeEngine::Claude),
            )),
            Autonomy::AskFirst
        );
        // A saved project for that folder that says AUTONOMOUS changes nothing:
        // the dispatch posture still resolves to AskFirst for Claude and Codex.
        let project = CodeProject {
            name: "reviewed".into(),
            folder: "/tmp/reviewed".into(),
            autonomous: true,
        };
        let projects = [project];
        assert!(projects[0].autonomous);
        for engine in [CodeEngine::Claude, CodeEngine::Codex] {
            assert_eq!(
                autonomy_for(effective(modes.mode_for(&key), code_support(engine))),
                Autonomy::AskFirst,
                "a saved autonomous project must not override Ask for {engine:?}"
            );
        }
        // And the folder-based override no longer exists in the source.
        let src = include_str!("work.rs");
        let needle = concat!("fn autonomy_for", "_folder(");
        assert!(
            !src.contains(needle),
            "autonomy_for_folder must stay deleted"
        );
        // U4d-FIX2: the REAL dispatch, with that autonomous project saved for
        // the very folder the Work turn runs in. A fake `claude` records its
        // argv; the turn must still be the read-only planning step. Runs in a
        // subprocess with a private HOME (`AwiliApp::new` reads config and
        // opens persistence).
        const CHILD: &str = "AWILI_U4D_FIX2_TEST_HOME";
        const TEST: &str =
            "work::agent_config_tests::unsaved_work_folder_uses_product_default_autonomy";
        let Some(home) = std::env::var_os(CHILD).map(std::path::PathBuf::from) else {
            let home = tempfile::tempdir().unwrap();
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env(CHILD, home.path())
                .env("HOME", home.path())
                .env("XDG_CONFIG_HOME", home.path().join("config"))
                .env("XDG_DATA_HOME", home.path().join("data"))
                .env("XDG_CACHE_HOME", home.path().join("cache"))
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && stdout.contains("1 passed"),
                "dispatch check failed:\n{stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        };
        let folder = home.join("reviewed");
        std::fs::create_dir_all(&folder).unwrap();
        let bin = home.join("claude");
        {
            use std::os::unix::fs::PermissionsExt;
            let script = format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> \"{argv}\"; done\n\
                 printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}}'\n\
                 printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"ok\"}}'\n",
                argv = home.join("argv").display()
            );
            std::fs::write(&bin, script).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut cx = gpui::TestAppContext::single();
        // The fake CLI answers from the provider's own thread.
        cx.executor().allow_parking();
        let entity = cx.new(AwiliApp::new);
        entity.update(&mut cx, |app, cx| {
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.fleet_pack = None;
            app.work
                .graduation
                .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
            let conv = "u4d-fix2-conv".to_string();
            app.views.insert(
                conv.clone(),
                crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
            );
            app.set_active_conversation(&conv);
            app.mode = Mode::Work;
            app.work.executor = WorkExecutor::Agent {
                engine: CodeEngine::Claude,
                model: None,
            };
            app.work.agents.claude_bin = Some(bin.display().to_string());
            app.work.folder = Some(folder.clone());
            // A shared saved project never supplies dispatch authorization.
            app.work.projects = vec![StoredProject {
                name: "reviewed".into(),
                folder: folder.display().to_string(),
                id: 0,
                instructions: String::new(),
                created_ts: 0,
            }];
            app.submit_work_run("fix it".into(), cx);
            assert!(app.work.running, "the turn was dispatched");
        });
        let deadline = std::time::Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
        loop {
            cx.run_until_parked();
            if !entity.read_with(&cx, |app, _| app.work.running) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the turn never ended");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let argv: Vec<String> = std::fs::read_to_string(home.join("argv"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        let pm = argv.iter().position(|a| a == "--permission-mode");
        assert_eq!(
            pm.and_then(|i| argv.get(i + 1)).map(String::as_str),
            Some("plan"),
            "a shared saved project must not lift the planning step: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a == "--dangerously-skip-permissions"),
            "no full-access flag under Ask: {argv:?}"
        );
    }
    #[test]
    fn seed_executor_precedence() {
        // valid persisted wins over a valid config seed
        assert_eq!(
            seed_executor(Some("gateway"), Some("agent:glm")),
            Some(WorkExecutor::Gateway)
        );
        // an unparseable persisted value does NOT mask the config seed
        assert_eq!(
            seed_executor(Some("agent:frobnicator"), Some("agent:glm")),
            Some(WorkExecutor::Agent {
                engine: CodeEngine::Glm,
                model: None
            })
        );
        // nothing persisted -> config seed
        assert_eq!(
            seed_executor(None, Some("agent:claude:sonnet")),
            Some(WorkExecutor::Agent {
                engine: CodeEngine::Claude,
                model: Some("sonnet".into())
            })
        );
        // both garbage / both absent -> caller keeps Gateway
        assert_eq!(seed_executor(Some("nope"), Some("also-nope")), None);
        assert_eq!(seed_executor(None, None), None);
    }
}

// ============================================================================
// P2b continuity wiring — the extracted request-preparation helper
// ============================================================================

#[cfg(test)]
mod continuity_wiring_tests {
    use super::*;

    /// RC1-Q (round 5 minor 3): the Progress card never says "No run yet"
    /// while a plan waits or after a run ended.
    #[test]
    fn rc1q_progress_card_words() {
        assert_eq!(
            progress_empty_text(true, false, false),
            // RC2-RUN (round 6 Minor 1, deliberate change): no "plan" word
            // for a run that never sends one.
            "Working on it…"
        );
        assert_eq!(
            progress_empty_text(false, true, true),
            "The plan is in the chat. Nothing has changed yet."
        );
        assert_eq!(
            progress_empty_text(false, false, true),
            "This run has ended. Its reply is in the chat."
        );
        assert_eq!(
            progress_empty_text(false, false, false),
            "No run yet. Choose a folder, describe a task, press Enter."
        );
    }

    /// RC1-Q (round 5 minor 5): a turn's completion closes every row still
    /// running — the in-app "reasoning / thinking…" row stayed blue.
    #[test]
    fn rc1q_completion_closes_running_rows() {
        let start = |tool: &str| {
            RunEvent::ToolProgress(awili_work::run_events::ToolProgressEvent {
                tool: Some(tool.into()),
                status: Some("running".into()),
                message: Some("thinking…".into()),
            })
        };
        let end = |status: RunStatus| {
            RunEvent::Completion(awili_work::run_events::CompletionEvent {
                status: Some(status),
                output: None,
            })
        };
        // RC2-RUN (S-Q1, deliberate change): the thinking row is typed
        // (`apply_reasoning`); a tool named "reasoning" is just a tool —
        // cut off by the turn's end, it ends neutral.
        let mut owner = RunPanel::default();
        owner.apply_event(&start("reasoning"));
        owner.apply_event(&end(RunStatus::Completed));
        assert_eq!(owner.tool_calls[0].status, ToolCallStatus::Ended);
        let mut panel = RunPanel::default();
        panel.apply_reasoning(false);
        assert_eq!(panel.tool_calls[0].status, ToolCallStatus::Running);
        panel.apply_event(&end(RunStatus::Completed));
        assert_eq!(panel.tool_calls[0].status, ToolCallStatus::Completed);
        let mut failed = RunPanel::default();
        failed.apply_reasoning(false);
        failed.apply_event(&end(RunStatus::Failed));
        assert_eq!(failed.tool_calls[0].status, ToolCallStatus::Failed);
    }
}

#[cfg(test)]
mod p4b_interlock_tests {
    //! P4b app surface: the four typed interlock tools execute through the
    //! typed v6.5 adapter (exact method/path/body/auth against a loopback
    //! stub), their results are lane-labeled and disjoint, and the v5
    //! backend refuses the verb set before any I/O.

    use super::*;
    use awili_tools::interlock;
    use serde_json::{json, Value};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// One v6.5 read envelope: one mandatory-policy item, two ordinary items.
    const READ_FIXTURE: &str = r#"{"mandatory_policy":[{"id":"p1","kind":"directive","subject":"policy","authority":"owner_instruction","object":"NEVER shut down the fleet"}],"items":[{"id":"i1","subject":"decision","object":{"content":"we ship P4b","importance":0.9}},{"id":"i2","subject":"note","object":"the gate is cargo test"}]}"#;
    const ARCHIVE_FIXTURE: &str = r#"{"results":[{"id":"arch-1","subject":"chat","object":{"raw":"log line one"},"timestamp":"2026-09-01T10:00:00Z"}]}"#;
    const V5_FIXTURE: &str = r#"{"constraints":[{"id":1,"rule":"one app one database","priority":100}],"directives":[],"facts":[],"notes":[{"id":2,"content":"a note","importance":0.5}],"reflections":[]}"#;

    /// `(method, path, body, auth)` captures of every request.
    type Caps = Arc<Mutex<Vec<(String, String, String, String)>>>;

    fn spawn_stub(caps: Caps) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let caps = Arc::clone(&caps);
                thread::spawn(move || {
                    let mut data = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        data.extend_from_slice(&buf[..n]);
                        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&data[..end]);
                            let len = headers
                                .lines()
                                .find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.trim()
                                        .eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            if data.len() >= end + 4 + len {
                                break;
                            }
                        }
                    }
                    let req = String::from_utf8_lossy(&data).into_owned();
                    let (head, body) = req.split_once("\r\n\r\n").unwrap_or((&req, ""));
                    let method = head.split_whitespace().next().unwrap_or("").to_string();
                    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let auth = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.trim()
                                .eq_ignore_ascii_case("authorization")
                                .then(|| v.trim().to_string())
                        })
                        .unwrap_or_default();
                    caps.lock().unwrap().push((
                        method.clone(),
                        path.clone(),
                        body.to_string(),
                        auth,
                    ));
                    let resp = match (method.as_str(), path.as_str()) {
                        ("POST", "/v6/bootstrap") | ("POST", "/v6/recall") => {
                            ("200 OK", READ_FIXTURE)
                        }
                        ("POST", "/v6/memories") => ("200 OK", r#"{"id":"stored-1"}"#),
                        ("POST", "/v6/archive/search") => ("200 OK", ARCHIVE_FIXTURE),
                        ("POST", "/v5/recall") => ("200 OK", V5_FIXTURE),
                        _ => ("404 Not Found", ""),
                    };
                    let out = format!(
                        "HTTP/1.1 {}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp.0,
                        resp.1.len(),
                        resp.1
                    );
                    let _ = stream.write_all(out.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        format!("http://{addr}")
    }

    fn v65_client(base: &str) -> awili_memory::MemoryClient {
        awili_memory::MemoryClient::new(base, 90).with_v65(
            base,
            Some("test-v65-reader-token"),
            Some("test-v65-writer-token"),
        )
    }

    fn remember_call() -> interlock::InterlockCall {
        let args = json!({
            "request_id": "req-app-1", "subject": "awili.p4b", "predicate": "legacy.note",
            "object": {"content": "shipped"}, "authority": "owner_instruction",
            "epistemic_status": "asserted", "source_type": "awili-app",
            "source_ref": "awili-desktop", "reason": "P4b",
        });
        interlock::parse(interlock::REMEMBER, &args).expect("a valid remember parses")
    }

    #[test]
    fn bootstrap_executes_with_exact_wire_and_labeled_lanes() {
        let caps: Caps = Arc::new(Mutex::new(Vec::new()));
        let base = spawn_stub(Arc::clone(&caps));
        let client = v65_client(&base);
        let value = execute_interlock(&client, &interlock::InterlockCall::Bootstrap).unwrap();
        assert_eq!(
            value["lanes"],
            json!([
                interlock::LANE_MANDATORY_POLICY,
                interlock::LANE_RECALL_ITEMS
            ])
        );
        // The inspector decodes the same labeled result into its lane state.
        let MemoryInspectorResults::V65 { policy, items } =
            decode_inspector_read(value).expect("the read result decodes")
        else {
            panic!("the read result holds the v6.5 lanes")
        };
        assert_eq!(policy.len(), 1, "the mandatory-policy lane maps first");
        assert_eq!(items.len(), 2, "the ordinary items lane stays separate");
        let caps = caps.lock().unwrap();
        assert_eq!(caps.len(), 1, "exactly one request");
        assert_eq!(caps[0].0, "POST");
        assert_eq!(caps[0].1, "/v6/bootstrap");
        assert_eq!(caps[0].3, "Bearer test-v65-reader-token");
        let body: Value = serde_json::from_str(&caps[0].2).unwrap();
        assert_eq!(body, json!({ "token_budget": 16000 }));
    }

    #[test]
    fn remember_serializes_every_provenance_field_on_the_wire() {
        let caps: Caps = Arc::new(Mutex::new(Vec::new()));
        let base = spawn_stub(Arc::clone(&caps));
        let client = v65_client(&base);
        let value = execute_interlock(&client, &remember_call()).unwrap();
        assert_eq!(
            value[interlock::LANE_CANONICAL_MEMORY]["stored_id"],
            json!("stored-1")
        );
        let caps = caps.lock().unwrap();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].1, "/v6/memories");
        assert_eq!(caps[0].3, "Bearer test-v65-writer-token");
        let body: Value = serde_json::from_str(&caps[0].2).unwrap();
        assert_eq!(body["request_id"], "req-app-1");
        assert_eq!(body["predicate"], "legacy.note");
        assert_eq!(body["authority"], "owner_instruction");
        assert_eq!(body["epistemic_status"], "asserted");
        assert_eq!(
            body.as_object().unwrap().len(),
            9,
            "exactly the nine fields"
        );
    }

    #[test]
    fn archive_results_never_blend_into_the_canonical_lanes() {
        let caps: Caps = Arc::new(Mutex::new(Vec::new()));
        let base = spawn_stub(Arc::clone(&caps));
        let client = v65_client(&base);
        let archive = execute_interlock(
            &client,
            &interlock::InterlockCall::ArchiveSearch {
                query: "hostc".into(),
                limit: 25,
            },
        )
        .unwrap();
        assert_eq!(
            archive["lanes"],
            json!([interlock::LANE_ARCHIVE_EVIDENCE]),
            "archive evidence is its own lane, nothing else"
        );
        assert_eq!(
            archive[interlock::LANE_ARCHIVE_EVIDENCE]["results"][0]["id"],
            "arch-1"
        );
        assert!(
            archive.get(interlock::LANE_MANDATORY_POLICY).is_none()
                && archive.get(interlock::LANE_RECALL_ITEMS).is_none()
                && archive.get(interlock::LANE_CANONICAL_MEMORY).is_none(),
            "no canonical lane may ride along with archive evidence"
        );
        let read = execute_interlock(&client, &interlock::InterlockCall::Bootstrap).unwrap();
        assert!(
            read.get(interlock::LANE_ARCHIVE_EVIDENCE).is_none(),
            "archive evidence never appears in canonical recall/policy results"
        );
        let caps = caps.lock().unwrap();
        assert_eq!(caps[0].1, "/v6/archive/search");
        assert_eq!(caps[0].3, "Bearer test-v65-reader-token");
        let body: Value = serde_json::from_str(&caps[0].2).unwrap();
        assert_eq!(body, json!({ "query": "hostc", "limit": 25 }));
    }

    #[test]
    fn v5_backend_refuses_the_verb_set_before_any_io() {
        let caps: Caps = Arc::new(Mutex::new(Vec::new()));
        let base = spawn_stub(Arc::clone(&caps));
        let client = awili_memory::MemoryClient::new(&base, 90).with_token("v5tok");
        let calls = [
            interlock::InterlockCall::Bootstrap,
            interlock::InterlockCall::Recall { query: "q".into() },
            remember_call(),
            interlock::InterlockCall::ArchiveSearch {
                query: "q".into(),
                limit: 5,
            },
        ];
        for call in &calls {
            let err = execute_interlock(&client, call).unwrap_err();
            assert!(
                err.contains("v6.5 backend"),
                "the v5 backend must name the v6.5 requirement: {err}"
            );
        }
        assert!(caps.lock().unwrap().is_empty(), "no request may be sent");
        // The EXPLICIT v5 fallback: the inspector's recall lane still rides
        // /v5/recall on the v5 backend.
        let results = inspect_call(
            &client,
            &interlock::InterlockCall::Recall { query: "q".into() },
        )
        .expect("the v5 fallback is the inspector's recall path");
        assert!(matches!(results, MemoryInspectorResults::V5(_)));
        let caps = caps.lock().unwrap();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].1, "/v5/recall");
        // And the mutating/archive verbs are not inspector operations:
        for call in &[
            remember_call(),
            interlock::InterlockCall::ArchiveSearch {
                query: "q".into(),
                limit: 5,
            },
        ] {
            assert!(
                inspect_call(&client, call)
                    .unwrap_err()
                    .contains("read-only"),
                "the inspector exposes only the read verbs"
            );
        }
    }

    #[test]
    fn errors_carry_no_bearer_token() {
        let dead = v65_client("http://127.0.0.1:1");
        let err = execute_interlock(&dead, &interlock::InterlockCall::Bootstrap).unwrap_err();
        let rendered = format!("{err} | {err:?}");
        assert!(
            !rendered.contains("test-v65-reader-token")
                && !rendered.contains("test-v65-writer-token"),
            "sentinel tokens must never render: {rendered}"
        );
    }
}

#[cfg(test)]
#[path = "sx_flake_tests.rs"]
mod sx_flake_tests;

#[cfg(test)]
#[path = "text_clip_tests.rs"]
mod text_clip_tests;

#[cfg(test)]
#[path = "rc1i3_tests.rs"]
mod rc1i3_tests;

#[cfg(test)]
#[path = "work_decided_card_tests.rs"]
mod decided_card_tests;

/// RC2-R5M6: the test lookup key of the System page's status (shown from a
/// read that succeeded, also after a later refresh failed).
pub(crate) const CC_STATUS_SELECTOR: &str = "rc2r5m6-cc-status";

/// RC2-R5M6: the test lookup key of the Messaging page's header (shown once
/// a snapshot loaded, also after a later refresh failed).
pub(crate) const MESSAGING_HEADER_SELECTOR: &str = "rc2r5m6-messaging-header";
