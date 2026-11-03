//! RC2-CNCLF1: how a council member's agent CLI runs — like Work in "Ask
//! before changes", never in Chat's restricted posture.
//!
//! Owner principle (2026-10-03, Foreman memory `a2362e80`): every model gets
//! everything he can do in Claude Code, with his own setup, and no invented
//! restrictions. So a Claude or Codex member runs exactly as a Work turn does
//! in "Ask before changes": his settings, AGENTS.md, MCP servers, hooks and
//! memory (RC1-M), reading and using read-only tools freely, and the planning
//! step's plan card before anything changes. GLM and DeepSeek cannot ask
//! first; as in Work, they run without asking — in Awili's own council folder,
//! never silently over the whole home folder (user-test F1).

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::{BoxStream, Stream};

use awili_code::{Autonomy, CodeEngine};
use awili_providers::{ChatProvider, ChatRequest, ProviderEvent};

use crate::ask_mode::{autonomy_for, code_support, effective, AskSupport};
use crate::code_provider::CodeProvider;

/// Whether a member on `engine` asks before it changes anything (Claude and
/// Codex: the read-only planning step and the plan card).
pub(crate) fn asks_first(engine: CodeEngine) -> bool {
    code_support(engine) == AskSupport::PlanFirst
}

/// The posture an agent member is launched in: Work's "Ask before changes"
/// for its engine — or, after the owner approved its plan, one full-access
/// follow-up (U4 D3/D10: one approval releases one turn).
pub(crate) fn member_autonomy(engine: CodeEngine, approved: bool) -> Autonomy {
    if approved {
        return Autonomy::Autonomous;
    }
    autonomy_for(effective(
        awili_work::ApprovalMode::AskBeforeChanges,
        code_support(engine),
    ))
}

/// The folder a member works in. A member that asks before changes works in
/// the owner's home folder, so it can read and look up anything he could;
/// one that cannot ask (GLM, DeepSeek) works in Awili's own council folder.
pub(crate) fn member_folder(asks: bool) -> String {
    let home = dirs::home_dir().filter(|_| asks);
    let dir = match home {
        Some(h) => Some(h),
        None => dirs::data_dir()
            .map(|d| d.join(awili_config::APP_DIR_NAME).join("council"))
            .filter(|d| awili_config::ensure_private_app_dir(d).is_ok()),
    };
    dir.map(|d| d.display().to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/".to_string())
}

/// What a card's Details says about a member's tools, in plain words.
/// `engine`: whether this run went through Awili's tool engine (`None`:
/// not known, e.g. a turn from before a relaunch — judged by its route).
pub(crate) fn tools_line(route: &str, engine: Option<bool>) -> String {
    // RC2-R2F8: true now for any folder (secret files still ask).
    const ENGINE: &str = "your MCP servers and Awili's own file tools; it reads any of your \
         folders freely (secret files ask first) and asks before it changes anything";
    match crate::work::WorkExecutor::from_persist(route) {
        Some(crate::work::WorkExecutor::Agent { engine, .. }) if asks_first(engine) => {
            "your own setup (settings, MCP servers, memory); it reads freely and asks before \
             it changes anything"
                .to_string()
        }
        Some(crate::work::WorkExecutor::Agent { .. }) => {
            "its own tools; it can't ask before changes, so it runs without asking, in \
             Awili's council folder (as in Work)"
                .to_string()
        }
        _ => match engine {
            Some(true) => ENGINE.to_string(),
            // An Anthropic-format provider: Awili's tool engine speaks
            // OpenAI's format only, so this one answers in words.
            Some(false) if route.starts_with("model:") => {
                "none: this provider uses Anthropic's format, which Awili's tool engine \
                 doesn't speak yet, so it answers in words"
                    .to_string()
            }
            Some(false) => "none (answers in words)".to_string(),
            None if route == crate::council_run::IN_APP_ROUTE
                || route == crate::agent_chat::FLEET_CHAT_ROUTE
                || route.starts_with("model:") =>
            {
                ENGINE.to_string()
            }
            None => "none (answers in words)".to_string(),
        },
    }
}

/// RC2-R2F8: what an API member is told about its file tools (their own
/// descriptions speak of one folder; a council member has none).
pub(crate) const ANY_FOLDER_NOTE: &str = "Your file tools (fs_read, fs_glob, fs_grep, \
     fs_write, fs_edit) take full paths anywhere on this Mac, e.g. /Users/\u{2026} or \
     /Projects/\u{2026}: reading runs at once; a change waits for the owner\u{2019}s OK.";

/// The ask added when the owner approves a member's plan.
pub(crate) fn approved_ask(plan: &str) -> String {
    format!(
        "You proposed this plan:\n\n{}\n\n{}",
        plan.trim(),
        awili_code::PLAN_APPROVED_TEXT
    )
}

/// The line under the council after "Don't change anything".
pub(crate) fn declined_note(label: &str) -> String {
    format!("{label}\u{2019}s plan was declined. Nothing was changed.")
}

/// RC2-R2F6 (usability RC2 round 2, F2: Claude's council answers opened
/// with "no plan required" and "no exploration or design agents needed"):
/// what a member in its planning step is told about a council answer, with
/// the planning text \u{2014} the owner reads it in a card beside the others.
pub(crate) const COUNCIL_ANSWER_POSTURE: &str = "This is your answer on a council: the owner \
     reads it in a card beside the other models\u{2019} answers. If the request needs no \
     changes, reply with the answer only \u{2014} do not mention this planning step, plan \
     mode, a plan, exploration or design agents, or that nothing needs changing. If it does \
     need changes, give the short numbered plan as asked.";

/// One council member's agent CLI turn, launched as Work launches it.
pub(crate) struct CouncilAgent {
    inner: CodeProvider,
    plan_first: bool,
    hooks_memory: bool,
}

impl CouncilAgent {
    /// `approved`: this is the one follow-up an approved plan releases.
    /// RC2-R2I: `council` is the council it answers in ("" = none).
    pub(crate) fn new(
        engine: CodeEngine,
        model: Option<String>,
        agents: &awili_config::AgentBinsConfig,
        connectors: Option<awili_config::McpServerConfig>,
        approved: bool,
        council: &str,
        sandbox: awili_config::SandboxConfig,
    ) -> Self {
        let autonomy = member_autonomy(engine, approved);
        let bin = crate::work::resolve_agent_bin(engine.bin(), agents);
        let mut inner = CodeProvider::new(
            bin,
            engine,
            autonomy,
            member_folder(asks_first(engine)),
            None,
            model,
            std::sync::Arc::new(std::sync::Mutex::new(None)),
        )
        .with_search_paths(agents.search_paths.clone())
        .with_agent_memory(agents.memory.clone())
        .with_agents_md(agents.agents_md.clone())
        .with_sandbox(sandbox);
        if engine == CodeEngine::Dsh {
            inner = inner.with_dsh_env_file(agents.dsh_env_file());
        }
        if let Some(spec) = connectors {
            inner = inner.with_connectors_mcp(spec);
        }
        if !council.is_empty() {
            inner = inner.for_council(council);
        }
        Self {
            inner,
            plan_first: autonomy.plan_first(),
            hooks_memory: crate::work::hooks_bring_memory(engine, agents.memory.as_ref()),
        }
    }
}

impl CouncilAgent {
    /// What the CLI is sent for `req`: the planning step's text leads, as
    /// on a Work turn in "Ask before changes"; the launch moves it into this
    /// turn's prompt. RC2-R2F6: with what a council answer is (no word
    /// about the step).
    fn member_request(&self, mut req: ChatRequest) -> ChatRequest {
        if self.plan_first {
            let planning = format!(
                "{}\n\n{COUNCIL_ANSWER_POSTURE}",
                awili_code::PLAN_FIRST_SYSTEM
            );
            req.system = Some(match req.system.take().filter(|s| !s.trim().is_empty()) {
                Some(s) => format!("{planning}\n\n{s}"),
                None => planning,
            });
        }
        req
    }

    /// RC2-R2F10: make this member's next turn ready while the owner
    /// reads (a Codex server started and its thread open; nothing for
    /// the other CLIs). `brief` is the system text its next turn gets.
    /// RC2-R2G5: made for council `council`, which ends it on closing.
    pub(crate) fn prewarm(&self, brief: &str, council: &str) {
        self.inner.prewarm(
            self.member_request(ChatRequest {
                model: String::new(),
                system: Some(brief.to_string()),
                messages: vec![awili_providers::ChatMessage::user(String::new())],
            }),
            council,
        );
    }
}

impl ChatProvider for CouncilAgent {
    /// RC1-N-FIX1: where the owner says his hooks bring his memory, Awili
    /// pins none of its own (as in Work).
    fn memory_from_hooks(&self) -> bool {
        self.hooks_memory
    }

    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ProviderEvent> {
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
        let rx = self.inner.run(self.member_request(req), cancel_rx);
        Box::pin(MemberStream {
            inner: Box::pin(rx),
            _cancel: cancel_tx,
        })
    }
}

/// The CLI's events; dropping the stream (Stop, turn end) kills the child.
struct MemberStream {
    inner: Pin<Box<async_channel::Receiver<ProviderEvent>>>,
    _cancel: std::sync::mpsc::Sender<()>,
}

impl Stream for MemberStream {
    type Item = ProviderEvent;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<ProviderEvent>> {
        let this = self.get_mut();
        Stream::poll_next(this.inner.as_mut(), cx)
    }
}

// ---- RC2-CNCLF2: HTTP models answer through Awili's own tool engine ----------------

/// One event of a member's run: what its model sent, or a step that needs
/// the owner's OK (the in-app engine's approval card).
pub(crate) enum SeatEvent {
    Provider(ProviderEvent),
    Approval(crate::work::ApprovalCard),
    /// The model server cut a reply off and the engine asks again: this
    /// text, at the end of the answer so far, is taken back.
    TakeBack(String),
    /// RC2-R3M13: one of its own steps that changes a file finished well
    /// (the engine's `fs.write` / `fs.edit`): evidence a change was made.
    Changed,
}

/// Where an HTTP model member answers through Awili's tool engine: the same
/// worker, MCP servers and approval cards as the in-app model in Work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EngineTarget {
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) api_key: String,
}

/// The engine appends `/v1/chat/completions` itself.
fn engine_base(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    b.strip_suffix("/v1").unwrap_or(b).to_string()
}

/// The tool-engine target of a member, or `None` when it does not answer
/// through the engine (an agent CLI; an Anthropic-format provider, which the
/// engine does not speak yet; a route that can't run, whose card says why).
pub(crate) fn engine_target(
    route: &str,
    s: &crate::council_run::MemberSetup<'_>,
) -> Option<EngineTarget> {
    use crate::council_run::MemberRoute;
    match crate::council_run::member_route(route, s.fleet) {
        MemberRoute::InApp => {
            let (base, model) = (s.engine.base_url.trim(), s.engine.model.trim());
            (!base.is_empty() && !model.is_empty()).then(|| EngineTarget {
                base_url: engine_base(base),
                model: model.to_string(),
                api_key: s
                    .keys
                    .engine
                    .clone()
                    .filter(|k| !k.trim().is_empty())
                    .unwrap_or_else(|| s.engine.api_key.clone()),
            })
        }
        // An additional in-app model: its own endpoint, model and key only.
        MemberRoute::InAppExtra(id) => {
            let e = s.extras.iter().find(|e| e.id == id)?;
            let (base, model) = (e.base_url.trim(), e.model.trim());
            (!base.is_empty() && !model.is_empty()).then(|| EngineTarget {
                base_url: engine_base(base),
                model: model.to_string(),
                api_key: e.api_key.clone(),
            })
        }
        MemberRoute::Fleet(crate::agent_chat::ChatRoute::Fleet(t)) => Some(EngineTarget {
            base_url: engine_base(&t.base_url),
            model: t.model.clone(),
            api_key: t.api_key.clone().unwrap_or_default(),
        }),
        MemberRoute::Model { provider, model } => {
            let c = s
                .choices
                .iter()
                .find(|c| c.provider == provider && c.model == model)?;
            let (base, key) = match c.kind {
                awili_config::ProviderKind::OpenAi => (c.base_url.clone(), c.api_key.clone()),
                // Ollama answers OpenAI's format on the same address.
                awili_config::ProviderKind::Ollama => (c.base_url.clone(), None),
                // Chat's Hermes REST surface (OpenAI-compatible).
                awili_config::ProviderKind::Hermes => {
                    (s.hermes_base_url.to_string(), s.keys.hermes.clone())
                }
                awili_config::ProviderKind::Anthropic => return None,
            };
            (!base.trim().is_empty()).then(|| EngineTarget {
                base_url: engine_base(&base),
                model: c.model.clone(),
                api_key: key.unwrap_or_default(),
            })
        }
        _ => None,
    }
}

/// The owner's user AGENTS.md as a block, on his own install (as the fleet
/// Chat and the in-app engine get it); `None` on a household install.
pub(crate) fn owner_agents_block(agents: &awili_config::AgentBinsConfig) -> Option<String> {
    agents
        .memory
        .as_ref()
        .and(crate::agent_launch::LaunchPaths::production())
        .and_then(|p| crate::agent_launch::user_agents_md(agents.agents_md.as_deref(), &p.home))
        .and_then(|d| crate::agent_launch::instructions_block(&[d]))
}

/// What the tool engine's events mean for a council card.
pub(crate) fn seat_events(ev: awili_work::WorkEvent) -> Vec<SeatEvent> {
    use awili_gateway::run_events::RunStatus;
    use awili_work::WorkEvent as W;
    let p = |e: ProviderEvent| SeatEvent::Provider(e);
    match ev {
        W::Token(t) if !t.is_empty() => vec![p(ProviderEvent::Token(t))],
        W::UsageReported {
            input_tokens,
            output_tokens,
        } => vec![p(ProviderEvent::Usage(awili_providers::Usage {
            cached_input_tokens: None,
            input_tokens: u32::try_from(input_tokens).unwrap_or(u32::MAX),
            output_tokens: u32::try_from(output_tokens).unwrap_or(u32::MAX),
        }))],
        W::ToolStarted { name, detail, .. } => {
            let line = detail.or(name).unwrap_or_default();
            if line.is_empty() {
                vec![]
            } else {
                vec![p(ProviderEvent::ToolStarted(line))]
            }
        }
        // RC2-R3M13: a file-changing step that finished well, from the
        // engine's own result (never the model's words).
        W::ToolFinished {
            name: Some(name),
            ok: Some(true),
            ..
        } if crate::work::inapp_files::changes_a_file(&name) => vec![SeatEvent::Changed],
        W::ApprovalRequested {
            approval_id,
            command,
            description,
            risk,
            ..
        } => {
            let command = command.unwrap_or_default();
            let desc = crate::work::plain_approval_text(
                &description.unwrap_or_else(|| "The model wants to take a step.".to_string()),
            );
            vec![SeatEvent::Approval(crate::work::ApprovalCard {
                approval_id: approval_id.unwrap_or_default(),
                kind: "command".to_string(),
                prompt: if command.is_empty() {
                    desc
                } else {
                    format!("{desc}\n\n{command}")
                },
                risk,
                delivery: Default::default(),
            })]
        }
        // The owner's own "Deny" needs no line; anything else that blocked
        // a step is said, in the engine's words.
        W::HookDenial { source, reason, .. } if reason != "denied by the user" => {
            vec![p(ProviderEvent::Token(format!(
                "\n\n(Blocked by {source}: {reason})\n\n"
            )))]
        }
        W::StatusUpdate {
            kind: Some(k),
            message,
        } if k == awili_engine_worker::REPLY_RESTART => {
            vec![SeatEvent::TakeBack(message.unwrap_or_default())]
        }
        W::TurnCompleted { status, output, .. } | W::Terminal { status, output, .. } => {
            match status {
                RunStatus::Completed | RunStatus::Stopped => vec![p(ProviderEvent::Done)],
                _ => vec![
                    p(ProviderEvent::Error(
                        output.unwrap_or_else(|| "the model's turn failed".to_string()),
                    )),
                    p(ProviderEvent::Done),
                ],
            }
        }
        W::Failed(e) => vec![p(ProviderEvent::Error(e)), p(ProviderEvent::Done)],
        _ => vec![],
    }
}

/// One member turn on the tool engine: its events, and the line to it
/// (approval answers). Dropping the stream stops the turn and ends the
/// worker.
pub(crate) struct EngineRun {
    pub(crate) stream: BoxStream<'static, SeatEvent>,
    pub(crate) cmd_tx: std::sync::mpsc::Sender<awili_work::WorkCmd>,
}

/// Start one member's turn on the tool engine: a worker of its own (the
/// owner's MCP servers, "Ask before changes", working in `workdir`), sent
/// `system` and `user_text` as one prompt. `log_key` names its execution
/// log (one per turn attempt).
pub(crate) fn start_engine_member(
    target: &EngineTarget,
    base: &awili_config::EngineConfig,
    window_tokens: u32,
    workdir: &str,
    log_key: &str,
    system: Option<String>,
    user_text: String,
) -> Result<EngineRun, String> {
    let cfg = awili_config::EngineConfig {
        kind: awili_config::EngineKind::Awili,
        base_url: target.base_url.clone(),
        model: target.model.clone(),
        api_key: String::new(),
        dialect: awili_engine_worker::CHAT_COMPLETIONS.to_string(),
        window_tokens: u64::from(window_tokens),
        ..base.clone()
    };
    let key = Some(target.api_key.as_str()).filter(|k| !k.trim().is_empty());
    let worker = crate::AwiliApp::build_awili_engine(
        &cfg,
        key,
        workdir,
        log_key,
        Vec::new(),
        None,
        awili_work::SharedApprovalMode::new(awili_work::ApprovalMode::AskBeforeChanges),
    )?;
    // RC2-R2F8: a council member has no folder of its own: its file tools
    // reach any folder by a full path, as Claude's and Codex's do (reads
    // pass; a change still asks).
    let worker = worker.with_any_folder(true);
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<awili_work::WorkCmd>();
    let (event_tx, event_rx) = async_channel::unbounded::<awili_work::WorkEvent>();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _ = cmd_tx.send(awili_work::WorkCmd::Prompt {
        text: user_text,
        system,
    });
    awili_work::spawn_worker(
        Box::new(worker),
        awili_work::WorkerContext {
            workdir: workdir.to_string(),
            cmd_rx,
            cancel: cancel.clone(),
            events: event_tx,
        },
    );
    use futures::StreamExt;
    let events = event_rx
        .map(|ev| futures::stream::iter(seat_events(ev)))
        .flatten()
        .boxed();
    Ok(EngineRun {
        stream: Box::pin(EngineStream {
            inner: events,
            cmd_tx: cmd_tx.clone(),
            cancel,
        }),
        cmd_tx,
    })
}

/// RC2-R620 (usability RC2 round 5, Major 1: Gemma's first words in a
/// council's first round came after 24-42 s): warm the member's model server
/// with what its next turn begins with — a worker built as
/// [`start_engine_member`] builds it, sent `system` as a warm-up instead of
/// a question, then ended. Its events are drained and dropped (nothing is
/// shown). The turn that follows waits for it if it is still reading.
///
/// RC2-R6M1 (RC2-R6 review P1 on 0fc8434b: a `Shutdown` queued right after
/// `Warm` was read by the warm-up's own host as a stop, so nothing was ever
/// sent): the worker is ended by dropping its command sender once `Warm` is
/// queued — the worker's loop runs the warm-up, then finds the channel
/// closed and exits. The returned receiver resolves when the worker has
/// ended (its events closed): RC2-R6M2, `true` when it reported its
/// warm-up read through ([`awili_work::WorkEvent::Warmed`]).
pub(crate) fn warm_engine_member(
    target: &EngineTarget,
    base: &awili_config::EngineConfig,
    window_tokens: u32,
    workdir: &str,
    log_key: &str,
    system: Option<String>,
) -> Result<futures::channel::oneshot::Receiver<bool>, String> {
    let cfg = awili_config::EngineConfig {
        kind: awili_config::EngineKind::Awili,
        base_url: target.base_url.clone(),
        model: target.model.clone(),
        api_key: String::new(),
        dialect: awili_engine_worker::CHAT_COMPLETIONS.to_string(),
        window_tokens: u64::from(window_tokens),
        ..base.clone()
    };
    let key = Some(target.api_key.as_str()).filter(|k| !k.trim().is_empty());
    let worker = crate::AwiliApp::build_awili_engine(
        &cfg,
        key,
        workdir,
        log_key,
        Vec::new(),
        None,
        awili_work::SharedApprovalMode::new(awili_work::ApprovalMode::AskBeforeChanges),
    )?
    .with_any_folder(true);
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<awili_work::WorkCmd>();
    let (event_tx, event_rx) = async_channel::unbounded::<awili_work::WorkEvent>();
    let _ = cmd_tx.send(awili_work::WorkCmd::Warm { system });
    drop(cmd_tx);
    let (done_tx, done_rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let mut read = false;
        while let Ok(ev) = event_rx.recv_blocking() {
            if let awili_work::WorkEvent::Warmed { ok } = ev {
                read = ok;
            }
        }
        let _ = done_tx.send(read);
    });
    awili_work::spawn_worker(
        Box::new(worker),
        awili_work::WorkerContext {
            workdir: workdir.to_string(),
            cmd_rx,
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            events: event_tx,
        },
    );
    Ok(done_rx)
}

/// The engine member's events; dropping it stops the turn (the cancel flag
/// and a Cancel) and ends the worker.
struct EngineStream {
    inner: BoxStream<'static, SeatEvent>,
    cmd_tx: std::sync::mpsc::Sender<awili_work::WorkCmd>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Stream for EngineStream {
    type Item = SeatEvent;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<SeatEvent>> {
        self.get_mut().inner.as_mut().poll_next(cx)
    }
}

impl Drop for EngineStream {
    fn drop(&mut self) {
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = self.cmd_tx.send(awili_work::WorkCmd::Cancel);
        let _ = self.cmd_tx.send(awili_work::WorkCmd::Shutdown);
    }
}

/// RC2-R3M13 (RC2-R3 review P2 on 74e18e33): the file a finished change step
/// names, read from an agent CLI's activity line ("✓ Write /w/notes.txt",
/// "✓ edit a.md"): Claude's or Codex's own tool result, never its prose.
pub(crate) fn finished_change(line: &str) -> Option<String> {
    crate::run_footer::changed_files_from_activity(
        &[gpui::SharedString::from(line.to_string())],
        "",
    )
    .into_iter()
    .next()
}
