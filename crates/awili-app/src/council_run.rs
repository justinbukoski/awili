//! RC2-CNCL6: how a council member is reached (Council plan §6).
//!
//! Any model Awili can reach can sit on a council:
//! - `agent:*` (Claude, Codex, GLM via ZCode, DeepSeek via dsh) — the same
//!   agent spawn path Chat uses ([`agent_chat::chat_provider_for_route`]);
//! - `provider:fleet` (Hermes, the fleet's own model) — the CLOSED fleet
//!   route: when it cannot run, the member says why; it never falls back to
//!   another vendor;
//! - `in-app` — the model Work uses on this Mac (`[engine]`), asked directly
//!   on its OpenAI-compatible endpoint (a text answer, no tool loop).
//!
//! Anything else (the Work gateway, a Build target, an unknown value) cannot
//! answer a council message; the member's card says so in plain words.

use futures::stream::BoxStream;
use futures::StreamExt;

use awili_providers::{ChatProvider, ChatRequest, OpenAiCompat, ProviderEvent};

use crate::agent_chat::{self, ChatRoute};
use crate::council_exec::SeatEvent;
use crate::work::WorkExecutor;

/// The route string of the in-app member.
pub(crate) const IN_APP_ROUTE: &str = "in-app";

/// The route prefix of an additional in-app model (`in-app:<id>`, an
/// `[[engines]]` entry); the bare `in-app` route stays the primary.
pub(crate) const IN_APP_EXTRA_PREFIX: &str = "in-app:";

/// The `[[engines]]` id a route names, when it is an additional in-app model.
pub(crate) fn in_app_extra_id(route: &str) -> Option<&str> {
    route
        .strip_prefix(IN_APP_EXTRA_PREFIX)
        .filter(|id| !id.is_empty())
}

/// The engine an in-app route runs on: the primary for `in-app`, the named
/// extra for `in-app:<id>` (`None` when that id is not configured), the
/// primary for any other route (callers only use it for in-app routes).
pub(crate) fn engine_for_route(
    route: &str,
    primary: &awili_config::EngineConfig,
    extras: &[awili_config::ExtraEngineConfig],
) -> Option<awili_config::EngineConfig> {
    match in_app_extra_id(route) {
        Some(id) => extras
            .iter()
            .find(|e| e.id == id)
            .map(awili_config::ExtraEngineConfig::to_engine_config),
        None => Some(primary.clone()),
    }
}

/// Where one member's answer comes from.
#[derive(Clone, Debug)]
pub(crate) enum MemberRoute {
    /// An agent CLI (`agent:<engine>[:<model>]`).
    Agent(WorkExecutor),
    /// The model Work uses on this Mac.
    InApp,
    /// An additional in-app model (`[[engines]]` id), resolved at run time.
    InAppExtra(String),
    /// The fleet's own model (`provider:fleet`), resolved.
    Fleet(ChatRoute),
    /// RC2-CNCL7: a `[[providers]]` model (`model:<provider>:<model>`).
    Model { provider: String, model: String },
    /// Cannot answer; the sentence says why.
    Unavailable(String),
}

/// The fleet route as Council resolves it from the config (the same closed
/// rules as a Chat pinned to `provider:fleet`).
pub(crate) fn fleet_route(cfg: &awili_config::Config) -> ChatRoute {
    let mut c = cfg.clone();
    c.chat.executor = agent_chat::FLEET_CHAT_ROUTE.to_string();
    agent_chat::chat_route_from_config(&c)
}

/// Parse a member route.
pub(crate) fn member_route(route: &str, fleet: &ChatRoute) -> MemberRoute {
    if route == IN_APP_ROUTE {
        return MemberRoute::InApp;
    }
    if let Some(id) = in_app_extra_id(route) {
        return MemberRoute::InAppExtra(id.to_string());
    }
    if route == agent_chat::FLEET_CHAT_ROUTE {
        return MemberRoute::Fleet(fleet.clone());
    }
    if let Some((provider, model)) = route
        .strip_prefix("model:")
        .and_then(|rest| rest.split_once(':'))
        .filter(|(p, m)| !p.is_empty() && !m.is_empty())
    {
        return MemberRoute::Model {
            provider: provider.to_string(),
            model: model.to_string(),
        };
    }
    match WorkExecutor::from_persist(route) {
        Some(e @ WorkExecutor::Agent { .. }) => MemberRoute::Agent(e),
        Some(WorkExecutor::Gateway) => MemberRoute::Unavailable(
            "The Work gateway runs jobs, it doesn't answer a council. Add Hermes instead."
                .to_string(),
        ),
        _ => MemberRoute::Unavailable(format!(
            "Awili doesn't know how to reach \u{201C}{route}\u{201D}, so it can't answer."
        )),
    }
}

/// What a member's provider needs from the app.
pub(crate) struct MemberSetup<'a> {
    pub(crate) agents: &'a awili_config::AgentBinsConfig,
    pub(crate) engine: &'a awili_config::EngineConfig,
    /// The `[[engines]]` extras, for `in-app:<id>` members.
    pub(crate) extras: &'a [awili_config::ExtraEngineConfig],
    pub(crate) fleet: &'a ChatRoute,
    /// The agent CLIs' working folder (Chat's own empty folder).
    pub(crate) cwd: String,
    /// RC2-CNCL7: every `[[providers]]` chat model, their keys, and Hermes's
    /// REST address (a Hermes-kind provider answers there).
    pub(crate) choices: &'a [awili_config::ModelChoice],
    pub(crate) keys: &'a crate::provider_build::ProviderKeys,
    pub(crate) hermes_base_url: &'a str,
    /// RC2-CNCLF1: the `awili-connectors` server the agent CLIs get, as in
    /// Work.
    pub(crate) connectors: Option<awili_config::McpServerConfig>,
    /// RC2-R2I: the council this run answers in ("" = none): a Codex member
    /// takes only a server made ready for it.
    pub(crate) council: String,
    /// RC2-CNCLF1: this run is the one follow-up an approved plan releases.
    pub(crate) approved: bool,
    /// rc.4 S7: `[work.sandbox]`, handed to Autonomous agent members.
    pub(crate) sandbox: awili_config::SandboxConfig,
}

/// The in-app member: the `[engine]` endpoint with its model forced.
/// A member answering through a fixed model on an HTTP provider (the
/// in-app engine, or a `[[providers]]` model).
struct InAppMember {
    inner: Box<dyn ChatProvider>,
    model: String,
}

impl ChatProvider for InAppMember {
    fn stream(&self, mut req: ChatRequest) -> BoxStream<'static, ProviderEvent> {
        req.model = self.model.clone();
        self.inner.stream(req)
    }
}

/// The provider one member answers through. Never a silent reroute: a member
/// that cannot run gets a provider whose only event is the reason.
pub(crate) fn member_provider(route: &str, s: &MemberSetup<'_>) -> Box<dyn ChatProvider> {
    match member_route(route, s.fleet) {
        // RC2-CNCLF1: an agent member runs like Work in "Ask before
        // changes" (the owner's setup, his tools), never in Chat's posture.
        MemberRoute::Agent(e) => {
            let (engine, model) = agent_chat::pinned_chat_executor(&e);
            Box::new(crate::council_exec::CouncilAgent::new(
                engine,
                model,
                s.agents,
                s.connectors.clone(),
                s.approved,
                &s.council,
                s.sandbox.clone(),
            ))
        }
        MemberRoute::Fleet(r) => agent_chat::chat_provider_for_route(&r, s.agents, s.cwd.clone()),
        MemberRoute::InAppExtra(id) => {
            let Some(e) = s.extras.iter().find(|e| e.id == id) else {
                return Box::new(agent_chat::UnavailableChatProvider::new(format!(
                    "This model isn't set up any more: there is no [[engines]] entry \"{id}\" in \
                     config.toml."
                )));
            };
            let (base, model) = (e.base_url.trim(), e.model.trim());
            if base.is_empty() || model.is_empty() {
                return Box::new(agent_chat::UnavailableChatProvider::new(
                    "This model isn't set up yet: add its address and model in config.toml.",
                ));
            }
            // Its own key only, never the primary's.
            let key = Some(e.api_key.clone()).filter(|k| !k.trim().is_empty());
            Box::new(InAppMember {
                inner: Box::new(OpenAiCompat::new(base.to_string()).with_api_key(key)),
                model: model.to_string(),
            })
        }
        MemberRoute::InApp => {
            let base = s.engine.base_url.trim();
            let model = s.engine.model.trim();
            if base.is_empty() || model.is_empty() {
                return Box::new(agent_chat::UnavailableChatProvider::new(
                    "The model on this Mac isn't set up yet: add its address and model in \
                     Settings, Work.",
                ));
            }
            let key = Some(s.engine.api_key.clone()).filter(|k| !k.trim().is_empty());
            Box::new(InAppMember {
                inner: Box::new(OpenAiCompat::new(base.to_string()).with_api_key(key)),
                model: model.to_string(),
            })
        }
        MemberRoute::Model { provider, model } => {
            match s
                .choices
                .iter()
                .find(|c| c.provider == provider && c.model == model)
            {
                Some(c) => Box::new(InAppMember {
                    inner: crate::provider_build::build_provider(c, s.keys, s.hermes_base_url),
                    model: c.model.clone(),
                }),
                None => Box::new(agent_chat::UnavailableChatProvider::new(format!(
                    "{model} is no longer set up in Awili (provider {provider}), so it can't answer."
                ))),
            }
        }
        MemberRoute::Unavailable(why) => Box::new(agent_chat::UnavailableChatProvider::new(why)),
    }
}

// ---- RC2-CNCL6: one message to every member at once ----------------------------

/// One member answering right now: what ties its stream task to its turn.
pub(crate) struct SeatRun {
    /// Bumped on every (re)start; a late event from an older stream of the
    /// same turn is ignored.
    pub(crate) gen: u64,
    /// Fires to stop this member only (RC2-CNCL9).
    pub(crate) abort_tx: Option<futures::channel::oneshot::Sender<()>>,
    pub(crate) started: std::time::Instant,
    /// Time to the first word.
    pub(crate) first_token_ms: Option<u64>,
    /// What the member reported it read and wrote.
    pub(crate) usage: Option<awili_providers::Usage>,
    /// The fingerprint of exactly what it was sent.
    pub(crate) prompt_hash: String,
    /// RC2-CNCLF1: it runs the read-only planning step (its answer may end
    /// in a plan for the owner to approve).
    pub(crate) plan_first: bool,
    /// RC2-R3M3: this run carries out a plan the owner approved; how it ends
    /// is kept as that plan's outcome (carried out, failed or stopped).
    pub(crate) approved: bool,
    /// RC2-R3M13: one of this run's own steps that changes a file finished
    /// well (its tool result, never its words).
    pub(crate) changed: bool,
    /// RC2-CNCLF2: the line to its tool engine (approval answers), when it
    /// answers through one.
    pub(crate) engine_tx: Option<std::sync::mpsc::Sender<awili_work::WorkCmd>>,
    /// RC2-CNCLF4: when its answer so far was last kept.
    pub(crate) saved_at: Option<std::time::Instant>,
    /// RC2-R3: the time this turn had already answered before this run (a
    /// continue or an approved plan), counted in its footer and header.
    pub(crate) carried_ms: u64,
    /// RC2-R3: this run continues a stopped answer: its first words join
    /// the old ones with a space where both meet at a word.
    pub(crate) join_first: bool,
}

/// Which stream an event belongs to: the council, the round, the member's
/// turn, and the turn's run generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SeatKey {
    pub(crate) conv: String,
    pub(crate) round: u32,
    pub(crate) turn_id: String,
    pub(crate) member: String,
    pub(crate) gen: u64,
}

/// One member to start: its turn id, who it is, and what it is sent.
pub(crate) struct SeatLaunch {
    pub(crate) turn_id: String,
    pub(crate) member: awili_council::Member,
    pub(crate) seat: awili_council::SeatPrompt,
    /// RC2-CNCLF1: the follow-up the owner's approval of its plan releases.
    pub(crate) approved: bool,
}

/// RC2-CNCLG1: a member whose card already shows it answering while the
/// memory lookup for its owner message is still out. Stop takes it off the
/// list: it never starts.
pub(crate) struct Waiting {
    /// The round of the owner message whose lookup it waits on.
    pub(crate) gate: u32,
    /// The round its card is in.
    pub(crate) round: u32,
    pub(crate) launch: SeatLaunch,
    /// Part of a new round (a refusal ends the whole round with one line),
    /// not a retry or a continue of one member.
    pub(crate) fresh: bool,
}

/// What a member is sent, as its card's Details and Retry keep it.
pub(crate) fn sent_facts(
    seat: &awili_council::SeatPrompt,
    owner: &str,
    tools: String,
) -> crate::council_view::SentFacts {
    crate::council_view::SentFacts {
        older_left_out: seat.older_left_out,
        fingerprint: seat.fingerprint(owner),
        prompt: seat.clone(),
        tools,
    }
}

/// RC2-CNCLF4: the saved row of a turn still answering (status `running`),
/// with what it was sent; a relaunch shows it as cut off by the quit.
pub(crate) fn running_row(
    conv: &str,
    r: &awili_council::Round,
    t: &awili_council::SeatTurn,
    sent: Option<&crate::council_view::SentFacts>,
) -> awili_persist::CouncilTurnRow {
    let owner = owner_name();
    awili_persist::CouncilTurnRow {
        id: t.id.clone(),
        conversation_id: conv.to_string(),
        round: i64::from(r.number),
        round_kind: r.kind.as_str().to_string(),
        owner_ts: r.owner_ts,
        member_id: t.member.clone(),
        route: t.route.clone(),
        label: t.label.clone(),
        status: RUNNING.to_string(),
        started_ts: t.started_ts,
        cost_usd: t.cost_usd,
        sent_prompt_hash: sent.map(|s| s.fingerprint.clone()),
        sent_system: sent.map(|s| s.prompt.brief.clone()),
        sent_text: sent.map(|s| s.prompt.user_text(&owner)),
        ..Default::default()
    }
}

/// RC2-R2F5: the tools turn `turn_id` used, one per line, as saved.
pub(crate) fn tools_used_text(
    page: &crate::council_view::CouncilPage,
    turn_id: &str,
) -> Option<String> {
    page.tools_used
        .get(turn_id)
        .filter(|u| !u.is_empty())
        .map(|u| u.join("\n"))
}

/// RC2-CNCLF4: the saved status of a turn still answering.
pub(crate) const RUNNING: &str = "running";

/// RC2-CNCLH3: the saved status of a round the memory lookup refused — a
/// row of its own (no member), its one line in `error_plain`.
pub(crate) const REFUSED: &str = "refused";

/// RC2-CNCLH3: the saved row that keeps a refused round (the owner's
/// message and the refusal line) across a relaunch, with or without any
/// member's turn left in it.
pub(crate) fn refused_row(
    conv: &str,
    r: &awili_council::Round,
    line: &str,
) -> awili_persist::CouncilTurnRow {
    awili_persist::CouncilTurnRow {
        id: format!("{conv}-round-{}-refused", r.number),
        conversation_id: conv.to_string(),
        round: i64::from(r.number),
        round_kind: r.kind.as_str().to_string(),
        owner_ts: r.owner_ts,
        status: REFUSED.to_string(),
        error_plain: Some(line.to_string()),
        started_ts: crate::feed::unix_now(),
        ..Default::default()
    }
}

/// RC2-CNCLF4: how often a member's answer so far is saved while it
/// streams (its first words are saved at once).
pub(crate) const PARTIAL_SAVE_EVERY: std::time::Duration = std::time::Duration::from_millis(1500);

impl crate::council_view::CouncilPage {
    /// Stop every member still answering (each keeps what it wrote).
    /// Returns how many were stopped.
    pub(crate) fn stop_all(&mut self) -> usize {
        let mut n = 0;
        for run in self.runs.values_mut() {
            if let Some(tx) = run.abort_tx.take() {
                let _ = tx.send(());
                n += 1;
            }
        }
        n
    }
}

/// How a member's stream ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SeatEnd {
    Done,
    Stopped,
    Error(String),
}

/// RC2-R5M3 (slice review befb867d, P2): the helper CLI a member with this
/// route runs (`agent:codex` → Codex), or `None` for a member that runs no
/// helper CLI (in-app, fleet, Hermes).
pub(crate) fn member_helper_engine(route: &str) -> Option<awili_code::CodeEngine> {
    match WorkExecutor::from_persist(route) {
        Some(WorkExecutor::Agent { engine, .. }) => Some(engine),
        _ => None,
    }
}

/// The plain sentence a card shows when a member didn't answer, and
/// whether the owner must act outside the council (signed out, a key
/// missing) — the only case shown in red.
///
/// RC2-R5M3 (slice review befb867d, P2): `engine` is the helper the member
/// runs, when it runs one, so its failure is read as its own: Codex's "Not
/// logged in" says how to sign Codex in, and an OAuth error from Codex is
/// never labelled Claude's.
pub(crate) fn failure_sentence(
    label: &str,
    engine: Option<awili_code::CodeEngine>,
    raw: &str,
) -> (String, bool) {
    let lower = raw.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if has(&[
        "not logged in",
        "log in",
        "login",
        "sign in",
        "signed out",
        "unauthorized",
        "401",
        "authentication",
        // RC2-R56 (usability RC2 round 4: "Failed to authenticate: OAuth
        // session expired" read as a plain failure, not a sign-in).
        "authenticate",
        "oauth",
    ]) {
        // RC2-R56: and how, when the failure names the helper.
        let how = crate::agent_signin::signed_out_line(engine, raw)
            .filter(|l| !l.starts_with("The helper"))
            .and_then(|l| l.split_once(". ").map(|(_, steps)| steps.to_string()));
        return match how {
            Some(steps) => (format!("{label} needs you to sign in again. {steps}"), true),
            None => (format!("{label} needs you to sign in again."), true),
        };
    }
    if has(&["api key", "api_key", "apikey", "missing key", "invalid key"]) {
        return (format!("{label} needs its key: add it in Settings."), true);
    }
    if has(&["timed out", "timeout", "deadline"]) {
        return (
            format!("{label} didn\u{2019}t answer: its service stopped responding."),
            false,
        );
    }
    if has(&[
        "connection refused",
        "could not connect",
        "error sending request",
        "unreachable",
    ]) {
        return (
            format!("{label} didn\u{2019}t answer: its service isn\u{2019}t reachable."),
            false,
        );
    }
    if has(&[
        "no such file",
        "not found",
        "failed to spawn",
        "could not start",
    ]) {
        return (
            format!("{label} didn\u{2019}t answer: Awili couldn\u{2019}t start it on this Mac."),
            false,
        );
    }
    let line = raw
        .trim()
        .trim_start_matches('\u{26A0}')
        .trim()
        .lines()
        .next()
        .unwrap_or("");
    // RC2-R53 (usability RC2 round 4, Major 1: the card read "… check the
    // steps list. Ask", cut mid-sentence): a long first line keeps its
    // whole sentences that fit.
    let first = if line.chars().count() <= 160 {
        line.to_string()
    } else {
        let head: String = line.chars().take(160).collect();
        match head.rfind(". ") {
            Some(i) => head[..=i].to_string(),
            None => format!("{}\u{2026}", head.trim_end()),
        }
    };
    if first.is_empty() {
        (format!("{label} didn\u{2019}t answer."), false)
    } else {
        (format!("{label} didn\u{2019}t answer: {first}"), false)
    }
}

/// The usage-ledger identity of a member: who to price it as. Subscription
/// CLIs (Claude, Codex) and models at home are not priced; DeepSeek is.
pub(crate) fn usage_identity(
    route: &str,
    engine: &awili_config::EngineConfig,
    extras: &[awili_config::ExtraEngineConfig],
    fleet: &ChatRoute,
) -> (String, String) {
    if route == IN_APP_ROUTE {
        return ("in-app".to_string(), engine.model.clone());
    }
    if in_app_extra_id(route).is_some() {
        let model = engine_for_route(route, engine, extras)
            .map(|e| e.model)
            .unwrap_or_default();
        return ("in-app".to_string(), model);
    }
    if route == agent_chat::FLEET_CHAT_ROUTE {
        let model = match fleet {
            ChatRoute::Fleet(t) => t.model.clone(),
            _ => "fleet".to_string(),
        };
        return ("fleet".to_string(), model);
    }
    match WorkExecutor::from_persist(route) {
        Some(WorkExecutor::Agent { engine: e, model }) => {
            if e == awili_code::CodeEngine::Dsh {
                (
                    "deepseek".to_string(),
                    model.unwrap_or_else(|| "deepseek-flash".to_string()),
                )
            } else {
                (
                    format!("agent:{}", e.bin()),
                    model.unwrap_or_else(|| "default".to_string()),
                )
            }
        }
        _ => match route.strip_prefix("model:").and_then(|r| r.split_once(':')) {
            // RC2-CNCL12: a `[[providers]]` model is priced as its provider.
            Some((provider, model)) => (provider.to_string(), model.to_string()),
            None => (route.to_string(), String::new()),
        },
    }
}

/// RC2-CNCLG2: one attempt's usage so far plus another report. A member
/// that uses tools sends one report per model request (the tool rounds and
/// the answer); the attempt read and wrote all of them.
pub(crate) fn add_usage(
    so_far: Option<awili_providers::Usage>,
    more: awili_providers::Usage,
) -> awili_providers::Usage {
    let a = so_far.unwrap_or_default();
    awili_providers::Usage {
        cached_input_tokens: if so_far.is_none() {
            more.cached_input_tokens
        } else {
            a.cached_input_tokens
                .zip(more.cached_input_tokens)
                .map(|(a, b)| a.saturating_add(b))
        },
        input_tokens: a.input_tokens.saturating_add(more.input_tokens),
        output_tokens: a.output_tokens.saturating_add(more.output_tokens),
    }
}

/// RC2-CNCLF6: what one attempt of a member cost, in dollars, only when it
/// is known: a priced rate (`provider`·`model`, the ledger's identity), or
/// `0.0` for a model at home (this Mac's, the fleet's, a provider on the
/// home network or Ollama). `None` = not known (the card says "cost not
/// reported"; a subscription says "included" by its route).
pub(crate) fn member_cost(
    route: &str,
    provider: &str,
    model: &str,
    choices: &[awili_config::ModelChoice],
    usage: awili_providers::Usage,
) -> Option<f64> {
    if let Some(price) = awili_core::pricing::known_price(provider, model) {
        return Some(awili_core::pricing::cost_usd(
            price,
            usage.input_tokens,
            usage.output_tokens,
        ));
    }
    let at_home = match route.strip_prefix("model:").and_then(|r| r.split_once(':')) {
        Some((p, m)) => choices
            .iter()
            .find(|c| c.provider == p && c.model == m)
            .is_some_and(|c| {
                c.kind == awili_config::ProviderKind::Ollama
                    || crate::engine_locality::engine_is_local(&c.base_url)
            }),
        None => {
            route == IN_APP_ROUTE
                || in_app_extra_id(route).is_some()
                || route == agent_chat::FLEET_CHAT_ROUTE
        }
    };
    at_home.then_some(0.0)
}

/// The trim budget for a member whose window is unknown (a conservative guess
/// used only to cut the transcript, never shown as a limit).
const UNKNOWN_WINDOW_TRIM: u32 = 32_000;

/// NOTE: nothing is kept across a relaunch for this, so after one a Claude or
/// Codex council member trims at this flat figure until its first turn reports
/// a window. That is the safe direction (it can only trim too much, never too
/// little).
/// The trim budget for a CLI member that has not reported its window yet (it
/// reports one with each turn, after which the report is used).
const UNREPORTED_CLI_TRIM: u32 = 100_000;

/// [`window_tokens`], preferring the window the member's own provider
/// reported on its last turn (`reported`, by route).
pub(crate) fn window_tokens_reported(
    route: &str,
    engine: &awili_config::EngineConfig,
    extras: &[awili_config::ExtraEngineConfig],
    reported: &std::collections::HashMap<String, u32>,
) -> u32 {
    match reported.get(route) {
        Some(w) => w / 10 * 9,
        None => window_tokens(route, engine, extras),
    }
}

/// About how many tokens a member can read (its window), for the trim.
pub(crate) fn window_tokens(
    route: &str,
    engine: &awili_config::EngineConfig,
    extras: &[awili_config::ExtraEngineConfig],
) -> u32 {
    if route == IN_APP_ROUTE || in_app_extra_id(route).is_some() {
        // An unknown extra never borrows the primary's window.
        let Some(engine) = engine_for_route(route, engine, extras) else {
            return UNKNOWN_WINDOW_TRIM;
        };
        return u32::try_from(engine.window_tokens)
            .unwrap_or(u32::MAX)
            .max(4_096);
    }
    if route == agent_chat::FLEET_CHAT_ROUTE {
        return UNKNOWN_WINDOW_TRIM;
    }
    match WorkExecutor::from_persist(route) {
        // The model-limits table decides; a member can read about 90% of its
        // window once the council's own framing is counted.
        Some(WorkExecutor::Agent { model, .. }) => {
            awili_core::limits::context_window(model.as_deref().unwrap_or_default())
                .map(|w| w / 10 * 9)
                .unwrap_or(UNREPORTED_CLI_TRIM)
        }
        _ => UNKNOWN_WINDOW_TRIM,
    }
}

/// The owner's first name as the members see it (the Mac account's full
/// name), else "Owner".
pub(crate) fn owner_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        std::process::Command::new("id")
            .arg("-F")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .next()
                    .map(str::to_string)
            })
            .filter(|n| {
                n.chars()
                    .all(|c| c.is_alphabetic() || c == '-' || c == '\'')
            })
            .unwrap_or_else(|| "Owner".to_string())
    })
    .clone()
}

/// "Not sent" lines for a council send nothing took.
pub(crate) const NOBODY_ON_COUNCIL: &str = "Not sent \u{2014} nobody is on this council. \
     Your message is still in the box; add a model first.";
/// RC2-CNCL9: the line under the council when a message waits its turn.
pub(crate) fn queued_note(text: &str) -> String {
    let short: String = text.chars().take(60).collect();
    let more = if text.chars().count() > 60 {
        "\u{2026}"
    } else {
        ""
    };
    format!("Goes to the council when it finishes this round: \u{201C}{short}{more}\u{201D}")
}

/// RC2-R3: what a rerun carries into its next start (a continue's earlier
/// time; whether its first words join the old ones).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Carry {
    pub(crate) carried_ms: u64,
    pub(crate) join: bool,
}

/// RC2-R3 (usability RC2 round 3, Minor 7: "Continue it" on Codex gave
/// "…managedstriping, parity…"): what goes between a stopped answer
/// (`prior`) and the first words of its continuation (`next`) \u{2014} a space
/// where a word or a sentence ends right against a word, else nothing (the
/// model's own space, a new line, or markup stay as they are).
pub(crate) fn continue_join(prior: &str, next: &str) -> &'static str {
    let (Some(last), Some(first)) = (prior.chars().last(), next.chars().next()) else {
        return "";
    };
    let ends_word =
        last.is_alphanumeric() || matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | ')');
    if ends_word && first.is_alphanumeric() {
        " "
    } else {
        ""
    }
}

/// RC2-CNCL9: the ask added when a stopped answer is continued.
pub(crate) fn continue_ask(partial: &str) -> String {
    format!(
        "You were stopped partway. Your answer so far:\n\n{}\n\nContinue it from where it \
         stopped, without repeating it.",
        partial.trim()
    )
}

/// Council members run in Awili's own empty `chat` folder (in Awili's data
/// folder), never in the Work folder or the home: nothing about the owner's
/// folders reaches them. If that folder cannot be made, `/` keeps the spawn's
/// `current_dir` valid.
fn council_cwd() -> String {
    dirs::data_dir()
        .map(|d| d.join(awili_config::APP_DIR_NAME).join("chat"))
        .filter(|d| awili_config::ensure_private_app_dir(d).is_ok())
        .map(|d| d.display().to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/".to_string())
}

impl crate::AwiliApp {
    /// What a member's provider needs from the app right now.
    pub(crate) fn member_setup(&self) -> MemberSetup<'_> {
        MemberSetup {
            agents: &self.work.agents,
            engine: &self.work.engine,
            extras: &self.work.extra_engines,
            fleet: &self.council_fleet,
            cwd: council_cwd(),
            choices: &self.choices,
            keys: &self.keys,
            hermes_base_url: &self.work.hermes.base_url,
            connectors: self.work.connectors.server_config(&self.work.agents),
            council: String::new(),
            approved: false,
            sandbox: self.work.sandbox.clone(),
        }
    }

    /// RC2-R2F10 (usability RC2 round 2, F7: Codex wrote nothing for 19-33
    /// s): while council `conv` waits for the owner, each Codex member's
    /// next turn is made ready \u{2014} its server started and its thread open,
    /// with the system text the next message will carry \u{2014} so that
    /// message skips the start-up (`codex_app_server::prewarm`). Off when
    /// turning ahead is off (tests, unless one turns it on). RC2-R3 (round 3,
    /// F7 / Finding 5): each Claude member too, started and waiting for
    /// its message, so its start-up (the owner's SessionStart hook, 7.3 s)
    /// runs while he reads (`claude_warm`); on opening a council too, so the
    /// first message is covered.
    pub(crate) fn council_prewarm(&self, conv: &str) {
        if !crate::codex_app_server::PREWARM.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        // RC2-R2H (RC2-R2F review P2 on d5cbd4ca): a council archived or
        // deleted while its round ran is not made ready when the round ends.
        if self.conversations.get(conv).is_none_or(|m| m.archived) {
            return;
        }
        let Some(page) = self.councils.get(conv).filter(|p| !p.council.is_running()) else {
            return;
        };
        let opts = awili_council::PlanOptions::new(&owner_name());
        let plan = awili_council::plan_round(
            &page.council,
            awili_council::RoundKind::Ask,
            Some(""),
            &opts,
        );
        let setup = self.member_setup();
        for seat in plan.seats {
            let Some(member) = page.council.member(&seat.member) else {
                continue;
            };
            if let Some(
                e @ WorkExecutor::Agent {
                    engine: awili_code::CodeEngine::Codex | awili_code::CodeEngine::Claude,
                    ..
                },
            ) = WorkExecutor::from_persist(&member.route)
            {
                let (engine, model) = agent_chat::pinned_chat_executor(&e);
                crate::council_exec::CouncilAgent::new(
                    engine,
                    model,
                    setup.agents,
                    setup.connectors.clone(),
                    false,
                    conv,
                    setup.sandbox.clone(),
                )
                .prewarm(&seat.brief, conv);
            }
        }
    }

    /// The owner pressed Enter on the Council page: the message goes to
    /// every member at once (or only the `@`-named ones).
    pub(crate) fn submit_council(&mut self, text: String, cx: &mut gpui::Context<Self>) {
        if text.trim().is_empty() {
            return;
        }
        let conv = self.sidebar.active_id().to_string();
        self.ensure_council_page(&conv);
        // RC2-CNCL9: typed while the council is still answering, the message
        // waits its turn and goes when the round is done.
        if let Some(page) = self
            .councils
            .get_mut(&conv)
            .filter(|p| p.council.is_running())
        {
            page.note = Some(queued_note(&text));
            page.queued.push_back(text);
            // RC2-CNCLF4: kept the moment it is accepted.
            self.council_save_queue(&conv);
            self.accept_submit();
            cx.notify();
            return;
        }
        if let Err(reason) = self.council_send_message(&conv, text, cx) {
            self.refuse_submit(reason, cx);
        }
    }

    /// Record the owner's message in council `conv` and send it to every
    /// member (or the `@`-named ones). `Err` carries the plain reason it was
    /// not sent (nothing was recorded then).
    pub(crate) fn council_send_message(
        &mut self,
        conv: &str,
        text: String,
        cx: &mut gpui::Context<Self>,
    ) -> Result<(), String> {
        let Some(page) = self.councils.get(conv) else {
            return Err(crate::submit_guard::NOT_STARTED_NOTICE.to_string());
        };
        let members: Vec<awili_council::Member> =
            page.council.active_members().into_iter().cloned().collect();
        if members.is_empty() {
            return Err(NOBODY_ON_COUNCIL.to_string());
        }
        let mentions = awili_council::resolve_mentions(&text, &members);
        if mentions.to.is_empty() {
            if let Some(ask) = mentions.ask_owner() {
                return Err(format!("Not sent \u{2014} {ask}"));
            }
        }
        // The owner's message is a row of the conversation like any other.
        let Some(view) = self.views.get_mut(conv) else {
            return Err(crate::submit_guard::NOT_STARTED_NOTICE.to_string());
        };
        let idx = view.rows.len();
        let ts = view.convo.push(awili_core::Role::User, text.clone());
        if let Some(m) = view.convo.message(idx) {
            view.rows.push(crate::MessageRow::new(m));
        }
        self.persist_message(conv, "user", &text, ts);
        self.accept_submit();
        let now = crate::feed::unix_now();
        if let Some(title) = self.conversations.note_user_message(conv, &text, now) {
            if let Some(p) = &self.persist {
                p.send(crate::persistence::PersistCmd::Rename {
                    id: conv.to_string(),
                    title,
                    custom: false,
                });
            }
        }
        if let Some(p) = &self.persist {
            p.send(crate::persistence::PersistCmd::Touch {
                id: conv.to_string(),
                updated_ts: now,
            });
        }
        self.council_send_round(
            conv,
            awili_council::RoundKind::Ask,
            Some((text, ts as i64)),
            mentions.to,
            None,
            cx,
        );
        Ok(())
    }

    /// Open one round: plan who gets what, run the memory gate ONCE, then
    /// start every member's stream at the same time. `false` when nobody
    /// could be asked (the page's note says why).
    pub(crate) fn council_send_round(
        &mut self,
        conv: &str,
        kind: awili_council::RoundKind,
        owner: Option<(String, i64)>,
        to: Vec<String>,
        summarizer: Option<String>,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let owner_name = owner_name();
        let engine = self.work.engine.clone();
        let extras = self.work.extra_engines.clone();
        let reported_windows = self.work.reported_windows.clone();
        let Some(page) = self.councils.get_mut(conv) else {
            return false;
        };
        let mut opts = awili_council::PlanOptions::new(&owner_name);
        opts.to = to;
        opts.summarizer = summarizer;
        // RC2-R3: each plan as the owner decided it.
        opts.plan_outcomes = page
            .plan_outcomes
            .iter()
            .map(|(id, o)| (id.clone(), *o))
            .collect();
        let plan = awili_council::plan_round(
            &page.council,
            kind,
            owner.as_ref().map(|(t, _)| t.as_str()),
            &opts,
        );
        if plan.seats.is_empty() {
            page.note = Some(plan.left_out.join(" "));
            cx.notify();
            return false;
        }
        page.note = None;
        let number = page.council.next_round_number();
        let mut round = awili_council::Round::new(number, kind);
        round.owner_text = owner.as_ref().map(|(t, _)| t.clone());
        round.owner_ts = owner.as_ref().map(|(_, ts)| *ts);
        round.left_out = plan.left_out.clone();
        let now = crate::feed::unix_now();
        let mut launches = Vec::new();
        for seat in plan.seats {
            let Some(m) = page.council.member(&seat.member).cloned() else {
                continue;
            };
            let turn_id = uuid::Uuid::new_v4().to_string();
            crate::run_error::log_run_start(
                &crate::run_error::run_id_of_token(&turn_id),
                "council",
                conv,
            );
            round
                .turns
                .push(awili_council::SeatTurn::running(&turn_id, &m, now));
            let seat = awili_council::trim_to_budget(
                seat,
                window_tokens_reported(&m.route, &engine, &extras, &reported_windows),
                &owner_name,
            );
            launches.push(SeatLaunch {
                turn_id,
                member: m,
                seat,
                approved: false,
            });
        }
        page.council.rounds.push(round);
        // RC2-CNCLF4: the round is kept the moment it is accepted — the
        // owner's question and each member's turn, still answering — so a
        // quit before the answers come back loses neither.
        let owner_now = owner_name.clone();
        let pending: Vec<awili_persist::CouncilTurnRow> = page
            .council
            .round(number)
            .map(|r| {
                launches
                    .iter()
                    .filter_map(|l| {
                        let t = r.turn(&l.member.id)?;
                        Some(running_row(
                            conv,
                            r,
                            t,
                            Some(&sent_facts(&l.seat, &owner_now, String::new())),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let members: Vec<awili_council::Member> = page.council.members.clone();
        let query = owner
            .map(|(t, _)| t)
            .or_else(|| {
                page.council
                    .rounds
                    .iter()
                    .rev()
                    .find_map(|r| r.owner_text.clone())
            })
            .unwrap_or_default();
        // Who sits on the council (and in what order) is kept with it.
        if let Some(p) = &self.persist {
            for m in &members {
                p.send(crate::persistence::PersistCmd::CouncilMember(
                    awili_persist::CouncilMemberRow {
                        conversation_id: conv.to_string(),
                        member_id: m.id.clone(),
                        position: i64::from(m.position),
                        route: m.route.clone(),
                        label: m.label.clone(),
                        added_ts: now,
                        removed_ts: m.removed.then_some(now),
                    },
                ));
            }
            for row in &pending {
                p.send(crate::persistence::PersistCmd::CouncilTurn(row.clone()));
            }
        }
        cx.notify();
        let conv = conv.to_string();
        // RC2-CNCLF5: the memory gate runs ONCE per owner message: a reply
        // round or a sum-up after it (on a tap or automatic) uses the outcome
        // of the message it answers.
        let gate_key = self
            .councils
            .get(&conv)
            .and_then(|p| owner_round(&p.council, number))
            .unwrap_or(number);
        // RC2-R3 (usability RC2 round 3, F7 / Finding 5): a member whose
        // memory the owner's own hooks bring (`hooks_provide_memory`: his
        // Claude and Codex) is given none of Awili's, so it does not wait
        // for Awili's memory lookup (about 2 s, measured) \u{2014} it starts now;
        // its hooks do their own lookup. The others wait for the lookup as
        // before; with none left, no lookup is made.
        // RC2-R3M2 (RC2-R3 review P2): they are taken out FIRST, before a
        // kept outcome of this message is used: a refused lookup is about
        // Awili's own memory, which they never get, so it cannot stop them
        // on a later round either.
        let (now, launches): (Vec<SeatLaunch>, Vec<SeatLaunch>) = launches
            .into_iter()
            .partition(|l| self.council_memory_from_hooks(&l.member.route));
        for launch in now {
            self.council_start_seat(&conv, number, launch, &Ok(None), cx);
        }
        if launches.is_empty() {
            return true;
        }
        let kept = self
            .councils
            .get(&conv)
            .and_then(|p| p.gates.get(&gate_key).cloned());
        if let Some(outcome) = kept {
            self.council_open_seats(&conv, number, launches, &outcome, cx);
            return true;
        }
        let seats = launches
            .into_iter()
            .map(|launch| Waiting {
                gate: gate_key,
                round: number,
                launch,
                fresh: true,
            })
            .collect();
        self.council_gate_then(&conv, gate_key, query, seats, cx);
        true
    }

    /// RC2-R3: whether the member on `route` takes its memory from the
    /// owner's own hooks (his Claude or Codex with `hooks_provide_memory`),
    /// not from Awili's lookup (`CouncilAgent::memory_from_hooks`).
    fn council_memory_from_hooks(&self, route: &str) -> bool {
        matches!(
            WorkExecutor::from_persist(route),
            Some(WorkExecutor::Agent { engine, .. })
                if crate::work::hooks_bring_memory(engine, self.work.agents.memory.as_ref())
        )
    }

    /// RC2-CNCLG1: ask the memory gate for the owner message of round
    /// `gate`; `seats` wait on the page (their cards show them answering)
    /// until it answers, and Stop can take any of them off before then.
    fn council_gate_then(
        &mut self,
        conv: &str,
        gate: u32,
        query: String,
        seats: Vec<Waiting>,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(page) = self.councils.get_mut(conv) else {
            return;
        };
        page.waiting.extend(seats);
        // RC2-CNCLG3: one lookup per owner message: a second caller (two
        // cards retried at once after a relaunch) waits on the one in flight.
        if !page.gating.insert(gate) {
            return;
        }
        let mem = self.memory.clone();
        let conv = conv.to_string();
        cx.spawn(async move |this, cx| {
            // RC2-R3 (usability RC2 round 3, Finding 5): the members that take
            // Awili's memory are models that read every recalled word again
            // each round (Gemma on the Studio: five full notes, about 2,100
            // tokens, 5.6-7.6 s before the first word; five short ones about
            // 2 s), so each note is cut as for the in-app model in Work
            // (RC2-R1F7); a member can ask its memory for more.
            let answer = crate::recall::gate_turn_memory_with(
                mem,
                query,
                crate::recall::production_client,
                crate::recall::INAPP_RECALL_NOTE_BYTES,
            )
            .await;
            this.update(cx, |app, cx| {
                app.council_gate_answered(&conv, gate, answer, cx)
            })
            .ok();
        })
        .detach();
    }

    /// RC2-CNCLG1: the memory gate answered for round `gate`: start every
    /// member still waiting on it. When the owner stopped them all
    /// meanwhile, the answer is dropped and nothing starts.
    fn council_gate_answered(
        &mut self,
        conv: &str,
        gate: u32,
        answer: crate::recall::TurnGate,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(page) = self.councils.get_mut(conv) else {
            return;
        };
        page.gating.remove(&gate);
        let (mine, rest): (Vec<Waiting>, Vec<Waiting>) = std::mem::take(&mut page.waiting)
            .into_iter()
            .partition(|w| w.gate == gate);
        page.waiting = rest;
        if mine.is_empty() {
            cx.notify();
            return;
        }
        // RC2-CNCL9 / RC2-CNCLF5: kept for this message's later rounds, a
        // retry and a continue.
        page.gates.insert(gate, answer.outcome.clone());
        if self.memory.enabled {
            self.rules = answer.rules.clone();
            self.recall = answer.recall.clone();
            self.note_memory_chip(conv);
        }
        // A new round's members start together (a refusal is one line for
        // the round); a retried or continued member starts on its own.
        let mut fresh: Vec<(u32, Vec<SeatLaunch>)> = Vec::new();
        for w in mine {
            if !w.fresh {
                self.council_start_seat(conv, w.round, w.launch, &answer.outcome, cx);
                continue;
            }
            match fresh.iter_mut().find(|(r, _)| *r == w.round) {
                Some((_, launches)) => launches.push(w.launch),
                None => fresh.push((w.round, vec![w.launch])),
            }
        }
        for (round, launches) in fresh {
            self.council_open_seats(conv, round, launches, &answer.outcome, cx);
        }
        cx.notify();
    }

    /// Start a round's members once its memory gate answered; a refusal
    /// stops the whole send with one line.
    fn council_open_seats(
        &mut self,
        conv: &str,
        number: u32,
        launches: Vec<SeatLaunch>,
        outcome: &Result<Option<String>, String>,
        cx: &mut gpui::Context<Self>,
    ) {
        if let Err(reason) = outcome {
            // One line for the whole send, not one error per member.
            let mut gone = Vec::new();
            let mut refused = None;
            if let Some(r) = self
                .councils
                .get_mut(conv)
                .and_then(|p| p.council.round_mut(number))
            {
                // RC2-CNCLH3: only the members still waiting go; one the
                // owner stopped while the lookup was out keeps its card.
                r.turns.retain(|t| {
                    let waiting = launches.iter().any(|l| l.turn_id == t.id);
                    if waiting {
                        gone.push(t.id.clone());
                    }
                    !waiting
                });
                let line = crate::recall::policy_refusal(reason);
                refused = Some(refused_row(conv, r, &line));
                r.left_out = vec![line];
            }
            // RC2-CNCLF4: nothing went out; the kept turns go.
            // RC2-CNCLH3: the round itself is kept, refusal and all.
            if let Some(p) = &self.persist {
                p.send(crate::persistence::PersistCmd::CouncilTurnsGone(gone));
                if let Some(row) = refused {
                    p.send(crate::persistence::PersistCmd::CouncilTurn(row));
                }
            }
            self.show_policy_refused_notice(cx);
            cx.notify();
            // RC2-CNCLH2: the round is over: a message typed while the
            // lookup was out goes now, as after any round.
            self.council_after_seat(conv, cx);
            return;
        }
        for launch in launches {
            self.council_start_seat(conv, number, launch, outcome, cx);
        }
    }

    /// Start one member's stream (its own task, its own stop wire).
    ///
    /// SAFETY (owner rule: Awili never makes a Lemonade host load a model):
    /// a member on Awili's tool engine first has its host read, read-only and
    /// off the UI thread, while it waits on the page like a member waiting for
    /// its memory lookup (so Stop takes it off). A host without the model
    /// resident, or one that gives no answer, gets no request: the member's
    /// card ends with the explanation.
    pub(crate) fn council_start_seat(
        &mut self,
        conv: &str,
        round: u32,
        launch: SeatLaunch,
        outcome: &Result<Option<String>, String>,
        cx: &mut gpui::Context<Self>,
    ) {
        let mut setup = self.member_setup();
        setup.approved = launch.approved;
        let Some(target) = crate::council_exec::engine_target(&launch.member.route, &setup) else {
            return self.council_start_seat_cleared(conv, round, launch, outcome, None, cx);
        };
        let engine = awili_config::EngineConfig {
            kind: awili_config::EngineKind::Awili,
            base_url: target.base_url.clone(),
            model: target.model.clone(),
            ..Default::default()
        };
        // Tests read the host on this thread (the test scheduler rejects
        // wake-ups from foreign threads); the app reads it off-thread.
        #[cfg(test)]
        {
            let blocked = Self::lemonade_refusal(&engine, crate::strip::guard_timeout())
                .map(|(card, _)| card);
            self.council_start_seat_cleared(conv, round, launch, outcome, blocked, cx);
        }
        #[cfg(not(test))]
        {
            let Some(page) = self.councils.get_mut(conv) else {
                return;
            };
            let gate = self.strip.next_guard_gate();
            let turn_id = launch.turn_id.clone();
            page.waiting.push(Waiting {
                gate,
                round,
                launch,
                fresh: false,
            });
            let (tx, rx) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let _ = tx.send_blocking(Self::lemonade_refusal(
                    &engine,
                    crate::strip::guard_timeout(),
                ));
            });
            let (conv, outcome) = (conv.to_string(), outcome.clone());
            cx.spawn(async move |this, cx| {
                if let Ok(refusal) = rx.recv().await {
                    this.update(cx, |app, cx| {
                        // Still waiting? (Stop takes a waiting member off.)
                        let Some(page) = app.councils.get_mut(&conv) else {
                            return;
                        };
                        let Some(i) = page
                            .waiting
                            .iter()
                            .position(|w| w.gate == gate && w.launch.turn_id == turn_id)
                        else {
                            return;
                        };
                        let w = page.waiting.remove(i);
                        let blocked = refusal.map(|(card, _)| card);
                        app.council_start_seat_cleared(
                            &conv, w.round, w.launch, &outcome, blocked, cx,
                        );
                    })
                    .ok();
                }
            })
            .detach();
        }
    }

    fn council_start_seat_cleared(
        &mut self,
        conv: &str,
        round: u32,
        launch: SeatLaunch,
        outcome: &Result<Option<String>, String>,
        blocked: Option<String>,
        cx: &mut gpui::Context<Self>,
    ) {
        let SeatLaunch {
            turn_id,
            member,
            seat,
            approved,
        } = launch;
        let owner = owner_name();
        let mut setup = self.member_setup();
        setup.approved = approved;
        setup.council = conv.to_string();
        // RC2-CNCLF2: an HTTP model answers through Awili's own tool engine
        // (the owner's MCP servers, the approval card), not as plain text.
        let target = crate::council_exec::engine_target(&member.route, &setup);
        let provider = match target {
            Some(_) => None,
            None => Some(member_provider(&member.route, &setup)),
        };
        let window = window_tokens_reported(
            &member.route,
            &self.work.engine,
            &self.work.extra_engines,
            &self.work.reported_windows,
        );
        let member_route = member.route.clone();
        let engine_base =
            engine_for_route(&member.route, &self.work.engine, &self.work.extra_engines)
                .unwrap_or_default();
        let agents_block = crate::council_exec::owner_agents_block(&self.work.agents);
        // RC2-R53 (usability RC2 round 4, Major 1: Gemma's card failed "needed
        // more room than the model can hold (32768 tokens)" in 4 of 6 turns):
        // a member on Awili's tool engine gets a transcript that fits its
        // model's window with its brief, its tools, its answer and its
        // steps' results — the oldest answers go first.
        let seat = match (&target, outcome) {
            (Some(_), Ok(policy)) => {
                let system =
                    engine_member_system(policy.as_deref(), agents_block.as_deref(), &seat);
                let room = awili_seam::user_room_chars(u64::from(window), &system);
                awili_council::trim_to_chars(seat, room, &owner)
            }
            _ => seat,
        };
        // RC2-CNCLF1: Claude and Codex members run the planning step.
        let plan_first = !approved
            && matches!(
                WorkExecutor::from_persist(&member.route),
                Some(WorkExecutor::Agent { engine, .. }) if crate::council_exec::asks_first(engine)
            );
        let request = ChatRequest {
            model: String::new(),
            system: Some(seat.brief.clone()),
            messages: vec![awili_providers::ChatMessage::user(seat.user_text(&owner))],
        };
        let Some(page) = self.councils.get_mut(conv) else {
            return;
        };
        let gen = page.runs.get(&turn_id).map(|r| r.gen + 1).unwrap_or(1);
        let (abort_tx, abort_rx) = futures::channel::oneshot::channel::<()>();
        // RC2-R3: a continue (or an approved plan) carries its earlier time.
        let carry = page.carry.remove(&turn_id).unwrap_or_default();
        page.runs.insert(
            turn_id.clone(),
            SeatRun {
                gen,
                abort_tx: Some(abort_tx),
                started: std::time::Instant::now(),
                first_token_ms: None,
                usage: None,
                prompt_hash: seat.fingerprint(&owner),
                plan_first,
                approved,
                changed: false,
                engine_tx: None,
                saved_at: None,
                carried_ms: carry.carried_ms,
                join_first: carry.join,
            },
        );
        page.sent.entry(turn_id.clone()).or_insert_with(|| {
            sent_facts(
                &seat,
                &owner,
                crate::council_exec::tools_line(&member.route, Some(target.is_some())),
            )
        });
        // RC2-CNCLF4: kept as still answering (a retry, a continue or an
        // approved plan too), so a quit now shows it cut off.
        let pending = page.council.round(round).and_then(|r| {
            r.turn(&member.id).map(|t| {
                let mut row = running_row(conv, r, t, page.sent.get(&turn_id));
                // RC2-R2M (RC2-R2F review P3 on 93676322): with the tools it
                // has used so far (a retry or a continue keeps them).
                row.tools_used = tools_used_text(page, &turn_id);
                row
            })
        });
        page.cut.remove(&turn_id);
        // A turn that already wrote an answer keeps it linked (a continue).
        let kept = self.council_kept_answer(conv, &turn_id);
        if let (Some(p), Some(row)) = (&self.persist, pending) {
            p.send(match kept {
                Some((text, ts)) => crate::persistence::PersistCmd::CouncilAnswer {
                    turn: row,
                    text,
                    ts: ts as i64,
                },
                None => crate::persistence::PersistCmd::CouncilTurn(row),
            });
        }
        let Some(page) = self.councils.get_mut(conv) else {
            return;
        };
        let refusal = |reason: String| -> BoxStream<'static, SeatEvent> {
            futures::stream::iter(vec![
                SeatEvent::Provider(ProviderEvent::Error(reason)),
                SeatEvent::Provider(ProviderEvent::Done),
            ])
            .boxed()
        };
        let (stream, engine_tx): (BoxStream<'static, SeatEvent>, _) = match (&target, outcome) {
            (Some(_), _) if blocked.is_some() => {
                (refusal(blocked.clone().unwrap_or_default()), None)
            }
            (Some(_), Err(reason)) => (refusal(crate::recall::policy_refusal(reason)), None),
            (Some(t), Ok(policy)) => {
                // The memory preamble, the owner's AGENTS.md, then the
                // council brief — as the in-app engine gets them in Work.
                let system =
                    engine_member_system(policy.as_deref(), agents_block.as_deref(), &seat);
                match crate::council_exec::start_engine_member(
                    t,
                    &engine_base,
                    window,
                    &crate::council_exec::member_folder(true),
                    // RC2-CNCLF4: a log of its own per start — a relaunched
                    // council never reopens an earlier session's log.
                    &format!("council-{conv}-{turn_id}-{}", uuid::Uuid::new_v4()),
                    Some(system).filter(|s| !s.is_empty()),
                    seat.user_text(&owner),
                ) {
                    Ok(run) => (run.stream, Some(run.cmd_tx)),
                    Err(e) => (refusal(e), None),
                }
            }
            (None, _) => match &provider {
                Some(p) => (
                    crate::recall::open_gated_stream(p.as_ref(), request, outcome)
                        .map(SeatEvent::Provider)
                        .boxed(),
                    None,
                ),
                None => (refusal("nothing to run".to_string()), None),
            },
        };
        if let Some(r) = page.runs.get_mut(&turn_id) {
            r.engine_tx = engine_tx;
        }
        // RC2-R2F11: the header's time moves while it answers.
        self.council_ensure_ticker(conv, cx);
        let key = SeatKey {
            conv: conv.to_string(),
            round,
            turn_id,
            member: member.id.clone(),
            gen,
        };
        cx.spawn(async move |this, cx| {
            use futures::{FutureExt, StreamExt};
            // The provider lives as long as its stream (dropping an agent
            // stream kills its CLI).
            let _provider = provider;
            let mut stream = stream.fuse();
            let mut abort_rx = abort_rx.fuse();
            let mut error: Option<String> = None;
            let mut stopped = false;
            loop {
                let ev = futures::select! {
                    ev = stream.next() => ev,
                    cancelled = abort_rx => {
                        if cancelled.is_ok() {
                            stopped = true;
                            break;
                        }
                        continue;
                    }
                };
                let Some(ev) = ev else { break };
                let ev = match ev {
                    SeatEvent::Provider(ev) => ev,
                    // RC2-CNCLF2: a step that needs the owner's OK.
                    SeatEvent::Approval(card) => {
                        this.update(cx, |app, cx| app.council_on_approval(&key, card, cx))
                            .ok();
                        continue;
                    }
                    SeatEvent::TakeBack(shown) => {
                        this.update(cx, |app, cx| app.council_take_back(&key, &shown, cx))
                            .ok();
                        continue;
                    }
                    SeatEvent::Changed => {
                        this.update(cx, |app, _| app.council_on_changed(&key)).ok();
                        continue;
                    }
                };
                match ev {
                    ProviderEvent::Token(t) => {
                        let alive = this
                            .update(cx, |app, cx| app.council_on_token(&key, &t, cx))
                            .unwrap_or(false);
                        if !alive {
                            return;
                        }
                    }
                    ProviderEvent::Usage(u) => {
                        this.update(cx, |app, _| {
                            if let Some(r) = app
                                .councils
                                .get_mut(&key.conv)
                                .and_then(|p| p.runs.get_mut(&key.turn_id))
                                .filter(|r| r.gen == key.gen)
                            {
                                // RC2-CNCLG2: every request of this attempt
                                // (each tool round, the answer) counts.
                                r.usage = Some(add_usage(r.usage, u));
                            }
                        })
                        .ok();
                    }
                    ProviderEvent::ContextWindow(w) => {
                        let route = member_route.clone();
                        this.update(cx, |app, _| {
                            app.work.reported_windows.insert(route, w);
                        })
                        .ok();
                    }
                    ProviderEvent::Error(e) => error = Some(e),
                    ProviderEvent::Done => break,
                    // RC2-R2F5: a tool it used, for its card (RC2-R2G8: a
                    // typed start only; a note such as a retry is no tool).
                    ProviderEvent::ToolStarted(line) => {
                        this.update(cx, |app, cx| app.council_on_tool_started(&key, &line, cx))
                            .ok();
                    }
                    // RC2-R3M13: a change step its CLI reports finished well.
                    ProviderEvent::Activity(line) => {
                        if crate::council_exec::finished_change(&line).is_some() {
                            this.update(cx, |app, _| app.council_on_changed(&key)).ok();
                        }
                    }
                    ProviderEvent::HostApproval(request) => { request.answer(awili_providers::ApprovalChoice::Deny); }
                    ProviderEvent::ToolAttemptFailed => {}
                }
            }
            let end = if stopped {
                SeatEnd::Stopped
            } else if let Some(e) = error {
                SeatEnd::Error(e)
            } else {
                SeatEnd::Done
            };
            this.update(cx, |app, cx| app.council_finish_seat(&key, end, cx))
                .ok();
        })
        .detach();
    }

    /// RC2-R2F11 (usability RC2 round 2, F4: the header said "0 s" until the
    /// round was over): while council `conv` answers, the page is drawn
    /// again every second, so its time moves while a member is silent.
    pub(crate) fn council_ensure_ticker(&mut self, conv: &str, cx: &mut gpui::Context<Self>) {
        let Some(page) = self.councils.get_mut(conv).filter(|p| !p.ticking) else {
            return;
        };
        page.ticking = true;
        let conv = conv.to_string();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            let again = this
                .update(cx, |app, cx| {
                    let Some(page) = app.councils.get_mut(&conv) else {
                        return false;
                    };
                    if !page.council.is_running() {
                        page.ticking = false;
                        return false;
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !again {
                break;
            }
        })
        .detach();
    }

    /// A word from a member. `false` when the turn is no longer this
    /// stream's (stopped, retried, council closed): the task then ends.
    fn council_on_token(
        &mut self,
        key: &SeatKey,
        chunk: &str,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let (conv, round, turn_id, member, gen) =
            (&key.conv, key.round, &key.turn_id, &key.member, key.gen);
        let Some(page) = self.councils.get_mut(conv) else {
            return false;
        };
        let Some(run) = page.runs.get_mut(turn_id).filter(|r| r.gen == gen) else {
            return false;
        };
        let first = run.first_token_ms.is_none();
        let join = first && run.join_first;
        if first {
            run.first_token_ms = Some(run.started.elapsed().as_millis() as u64);
        }
        if let Some(t) = page
            .council
            .round_mut(round)
            .and_then(|r| r.turn_mut(member))
        {
            // RC2-R3 (round 3 Minor 7: "managedstriping"): a continued
            // answer's first words meet the old ones with a space.
            if join {
                t.text.push_str(continue_join(&t.text, chunk));
            }
            t.text.push_str(chunk);
        }
        // RC2-CNCLF4: the answer so far is kept as it streams (its first
        // words at once, then every PARTIAL_SAVE_EVERY), so a quit cuts it
        // off but never loses it.
        if run
            .saved_at
            .is_none_or(|t| t.elapsed() >= PARTIAL_SAVE_EVERY)
        {
            run.saved_at = Some(std::time::Instant::now());
            self.council_save_partial(key);
        }
        cx.notify();
        true
    }

    /// RC2-R3M13: one of this run's own steps changed a file.
    fn council_on_changed(&mut self, key: &SeatKey) {
        if let Some(r) = self
            .councils
            .get_mut(&key.conv)
            .and_then(|p| p.runs.get_mut(&key.turn_id))
            .filter(|r| r.gen == key.gen)
        {
            r.changed = true;
        }
    }

    /// RC2-R2F5: a tool the member started (its plain words: "Read
    /// notes.txt", "$ ls -la"), kept once each for its card. RC2-R2G8: only a
    /// typed tool start comes here; any other activity line (a finished
    /// tool's echo, a retry note) is no tool.
    fn council_on_tool_started(&mut self, key: &SeatKey, line: &str, cx: &mut gpui::Context<Self>) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Some(page) = self.councils.get_mut(&key.conv) else {
            return;
        };
        if page.runs.get(&key.turn_id).is_none_or(|r| r.gen != key.gen) {
            return;
        }
        let used = page.tools_used.entry(key.turn_id.clone()).or_default();
        if used.iter().any(|u| u == line) {
            return;
        }
        used.push(line.to_string());
        self.council_save_tools(key);
        cx.notify();
    }

    /// RC2-R2L (RC2-R2F review P3 on a38e5e9b: a quit mid-answer lost the
    /// tools a member had used): a member still answering keeps its tools
    /// as each one starts, with its answer so far, or on its running row
    /// while it has written nothing.
    fn council_save_tools(&mut self, key: &SeatKey) {
        let Some(page) = self.councils.get(&key.conv) else {
            return;
        };
        let Some(r) = page.council.round(key.round) else {
            return;
        };
        let Some(t) = r.turn(&key.member) else {
            return;
        };
        if !t.text.trim().is_empty() {
            self.council_save_partial(key);
            return;
        }
        let mut row = running_row(&key.conv, r, t, page.sent.get(&key.turn_id));
        row.tools_used = tools_used_text(page, &key.turn_id);
        // RC2-R2M (RC2-R2F review P3 on 93676322): an answer row it already
        // has (a retry that has written nothing new yet) is written with it,
        // so it stays linked.
        let had_answer = page.answer_ts.contains_key(&key.turn_id);
        let cmd = match self.council_kept_answer(&key.conv, &key.turn_id) {
            Some((text, ts)) => crate::persistence::PersistCmd::CouncilAnswer {
                turn: row,
                text,
                ts: ts as i64,
            },
            None if !had_answer => crate::persistence::PersistCmd::CouncilTurn(row),
            None => return,
        };
        if let Some(p) = &self.persist {
            p.send(cmd);
        }
    }

    /// The answer row turn `turn_id` of `conv` already wrote (its text and
    /// `ts`), if it has one.
    fn council_kept_answer(&self, conv: &str, turn_id: &str) -> Option<(String, u64)> {
        let ts = *self.councils.get(conv)?.answer_ts.get(turn_id)?;
        let m = self
            .views
            .get(conv)?
            .convo
            .messages()
            .iter()
            .find(|m| m.ts == ts)?;
        Some((m.text.clone(), ts))
    }

    /// The conversation row of a member's answer in `conv`: written in place
    /// when the turn has one (a continue, a retry, the answer so far), else
    /// added. Returns its `ts`, kept with the page.
    fn council_answer_row(&mut self, conv: &str, turn_id: &str, text: &str) -> Option<u64> {
        let earlier = self
            .councils
            .get(conv)
            .and_then(|p| p.answer_ts.get(turn_id).copied());
        let ts = self.views.get_mut(conv).map(|view| {
            let at = earlier.and_then(|ts| view.convo.messages().iter().position(|m| m.ts == ts));
            match at {
                Some(idx) => {
                    view.convo.set_text(idx, text.to_string());
                    if let (Some(m), Some(row)) = (view.convo.message(idx), view.rows.get_mut(idx))
                    {
                        *row = crate::MessageRow::new(m);
                    }
                    view.convo.messages()[idx].ts
                }
                None => {
                    let idx = view.rows.len();
                    let ts = view.convo.push_kind(
                        awili_core::Role::Assistant,
                        text.to_string(),
                        awili_core::MessageKind::CouncilAnswer,
                    );
                    if let Some(m) = view.convo.message(idx) {
                        view.rows.push(crate::MessageRow::new(m));
                    }
                    ts
                }
            }
        })?;
        if let Some(page) = self.councils.get_mut(conv) {
            page.answer_ts.insert(turn_id.to_string(), ts);
        }
        Some(ts)
    }

    /// RC2-CNCLF4: keep a member's answer so far, its turn still answering.
    fn council_save_partial(&mut self, key: &SeatKey) {
        let Some(page) = self.councils.get(&key.conv) else {
            return;
        };
        let Some(r) = page.council.round(key.round) else {
            return;
        };
        let Some(t) = r.turn(&key.member) else {
            return;
        };
        if t.text.trim().is_empty() {
            return;
        }
        let mut row = running_row(&key.conv, r, t, page.sent.get(&key.turn_id));
        // RC2-R2L: with the tools it has used so far.
        row.tools_used = tools_used_text(page, &key.turn_id);
        let text = t.text.clone();
        let Some(ts) = self.council_answer_row(&key.conv, &key.turn_id, &text) else {
            return;
        };
        if let Some(p) = &self.persist {
            p.send(crate::persistence::PersistCmd::CouncilAnswer {
                turn: row,
                text,
                ts: ts as i64,
            });
        }
    }

    /// RC2-CNCLF2: the member's tool engine stopped at a step that needs the
    /// owner's OK; its card shows the approval card.
    fn council_on_approval(
        &mut self,
        key: &SeatKey,
        card: crate::work::ApprovalCard,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(page) = self.councils.get_mut(&key.conv) else {
            return;
        };
        if page
            .runs
            .get(&key.turn_id)
            .is_some_and(|r| r.gen == key.gen)
        {
            page.approvals.insert(key.turn_id.clone(), card);
            cx.notify();
        }
    }

    /// RC2-CNCLF2: the model server cut a reply off and the engine asks
    /// again: the text it had shown comes off the card.
    fn council_take_back(&mut self, key: &SeatKey, shown: &str, cx: &mut gpui::Context<Self>) {
        let Some(page) = self.councils.get_mut(&key.conv) else {
            return;
        };
        if page.runs.get(&key.turn_id).is_none_or(|r| r.gen != key.gen) {
            return;
        }
        if let Some(t) = page
            .council
            .round_mut(key.round)
            .and_then(|r| r.turn_mut(&key.member))
        {
            if !shown.is_empty() && t.text.ends_with(shown) {
                let keep = t.text.len() - shown.len();
                t.text.truncate(keep);
            }
        }
        cx.notify();
    }

    /// RC2-CNCLF2: the owner's answer on a member's approval card. Only a
    /// choice the card offers is sent (Allow once, for this answer, always
    /// when the step allows it, or Deny — the member is told and goes on).
    pub(crate) fn council_answer_approval(
        &mut self,
        turn_id: &str,
        decision: awili_gateway::run_events::ApprovalDecision,
        cx: &mut gpui::Context<Self>,
    ) {
        let conv = self.sidebar.active_id().to_string();
        let Some(page) = self.councils.get_mut(&conv) else {
            return;
        };
        let Some(card) = page.approvals.get(turn_id) else {
            return;
        };
        if !card.offers(decision, false) {
            return;
        }
        let id = Some(card.approval_id.clone()).filter(|id| !id.is_empty());
        let sent = page
            .runs
            .get(turn_id)
            .and_then(|r| r.engine_tx.as_ref())
            .is_some_and(|tx| {
                tx.send(awili_work::WorkCmd::Approval {
                    id,
                    choice: decision.scope_str().to_string(),
                })
                .is_ok()
            });
        // A worker that is gone ends its stream; the card goes with the turn.
        if sent {
            page.approvals.remove(turn_id);
        }
        cx.notify();
    }

    /// A member finished (answered, was stopped, or failed): settle its
    /// turn, keep its answer as a row of the conversation, save the turn and
    /// its usage. The other members are untouched.
    pub(crate) fn council_finish_seat(
        &mut self,
        key: &SeatKey,
        end: SeatEnd,
        cx: &mut gpui::Context<Self>,
    ) {
        let (conv, round, turn_id, member, gen) = (
            key.conv.as_str(),
            key.round,
            key.turn_id.as_str(),
            key.member.as_str(),
            key.gen,
        );
        let engine = self.work.engine.clone();
        let extras = self.work.extra_engines.clone();
        let fleet = self.council_fleet.clone();
        let choices = self.choices.clone();
        let Some(page) = self.councils.get_mut(conv) else {
            return;
        };
        if page.runs.get(turn_id).is_none_or(|r| r.gen != gen) {
            return;
        }
        let Some(run) = page.runs.remove(turn_id) else {
            return;
        };
        // RC2-CNCLF2: an approval card ends with its turn.
        page.approvals.remove(turn_id);
        let Some(turn) = page
            .council
            .round(round)
            .and_then(|r| r.turn(member))
            .cloned()
        else {
            return;
        };
        let kind = page
            .council
            .round(round)
            .map(|r| r.kind)
            .unwrap_or(awili_council::RoundKind::Ask);
        let owner_ts = page.council.round(round).and_then(|r| r.owner_ts);
        let done = end == SeatEnd::Done;
        // RC2-CNCLF1: a planning step that ended cleanly with the plan
        // marker opens the plan card; the marker never shows.
        let (clean, plan) = awili_code::take_plan_marker(&turn.text);
        let text = if plan {
            clean
        } else {
            awili_code::hide_plan_marker(&turn.text, !done).into_owned()
        };
        let opens_plan = plan && done && run.plan_first && !text.trim().is_empty();
        if opens_plan {
            page.plans.insert(turn_id.to_string(), text.clone());
        }
        let status = match end {
            SeatEnd::Stopped => awili_council::TurnStatus::Stopped,
            SeatEnd::Error(e) => {
                // RC2-R5M3: read as the failure of the helper this member runs.
                let helper = page
                    .council
                    .member(member)
                    .and_then(|m| member_helper_engine(&m.route));
                let (plain, needs_owner) = failure_sentence(&turn.label, helper, &e);
                awili_council::TurnStatus::Failed {
                    plain,
                    detail: e,
                    needs_owner,
                }
            }
            SeatEnd::Done if text.trim().is_empty() => awili_council::TurnStatus::Failed {
                plain: format!("{} sent back an empty answer.", turn.label),
                detail: "empty reply".to_string(),
                needs_owner: false,
            },
            SeatEnd::Done => awili_council::TurnStatus::Answered,
        };
        // RC2-R3M3 (RC2-R3 review P2): an approved plan counts as carried
        // out only when its follow-up answered without failing; a failed or
        // stopped one is kept as such, so no later round is told it was done.
        let follow_up = run.approved.then_some(match &status {
            // RC2-R3M13 (RC2-R3 review P2 on 74e18e33): an answer is words;
            // carried out only when its own steps show a change made.
            awili_council::TurnStatus::Answered if run.changed => {
                awili_council::PlanOutcome::Approved
            }
            awili_council::TurnStatus::Answered => awili_council::PlanOutcome::Answered,
            awili_council::TurnStatus::Stopped => awili_council::PlanOutcome::Stopped,
            awili_council::TurnStatus::Failed { .. } => awili_council::PlanOutcome::Failed,
            awili_council::TurnStatus::Running => awili_council::PlanOutcome::Started,
        });
        page.council.settle(round, member, status.clone(), &text);
        // RC2-R3: with the time a continued answer had already taken.
        let elapsed_ms = run.carried_ms + run.started.elapsed().as_millis() as u64;
        let (provider, model) = usage_identity(&turn.route, &engine, &extras, &fleet);
        // RC2-CNCLF6: this attempt's cost at a KNOWN rate (a model at home
        // is free; an unknown rate is not reported, never "free"), added to
        // the earlier attempts' — the usage ledger counts every attempt, so
        // the council's tallies do too.
        let attempt = run
            .usage
            .and_then(|u| member_cost(&turn.route, &provider, &model, &choices, u));
        let cost = match (turn.cost_usd, attempt) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
        let settled = page
            .council
            .round_mut(round)
            .and_then(|r| r.turn_mut(member));
        let stance = settled.as_ref().and_then(|t| t.stance);
        if let Some(t) = settled {
            t.elapsed_ms = Some(elapsed_ms);
            t.first_token_ms = run.first_token_ms;
            t.usage = run
                .usage
                .map(|u| (u64::from(u.input_tokens), u64::from(u.output_tokens)));
            t.cost_usd = cost;
        }
        let (error_plain, error_detail, needs_owner) = match &status {
            awili_council::TurnStatus::Failed {
                plain,
                detail,
                needs_owner,
            } => (Some(plain.clone()), Some(detail.clone()), *needs_owner),
            _ => (None, None, false),
        };
        // RC2-CNCLF3: exactly what it was first sent this round (a continue
        // adds to it; a Retry sends this again), and its fingerprint.
        let owner = owner_name();
        let (sent_system, sent_text, sent_hash) = match page.sent.get(turn_id) {
            Some(s) => (
                Some(s.prompt.brief.clone()),
                Some(s.prompt.user_text(&owner)),
                Some(s.fingerprint.clone()),
            ),
            None => (None, None, Some(run.prompt_hash.clone())),
        };
        let row = awili_persist::CouncilTurnRow {
            id: turn_id.to_string(),
            conversation_id: conv.to_string(),
            round: i64::from(round),
            round_kind: kind.as_str().to_string(),
            owner_ts,
            member_id: member.to_string(),
            route: turn.route.clone(),
            label: turn.label.clone(),
            message_id: None,
            status: status.as_str().to_string(),
            stance: stance.map(|s| s.as_str().to_string()),
            error_plain,
            error_detail,
            needs_owner,
            started_ts: turn.started_ts,
            first_token_ms: run.first_token_ms.map(|v| v as i64),
            elapsed_ms: Some(elapsed_ms as i64),
            input_tokens: run.usage.map(|u| i64::from(u.input_tokens)),
            output_tokens: run.usage.map(|u| i64::from(u.output_tokens)),
            cost_usd: cost,
            sent_prompt_hash: sent_hash,
            sent_system,
            sent_text,
            // RC2-R2F5: the tools it used, one per line.
            tools_used: tools_used_text(page, turn_id),
        };
        // The answer is a row of the conversation (search, export, sidebar);
        // a continued or retried answer replaces its own row (RC2-CNCL9).
        let answer_ts = if text.trim().is_empty() {
            None
        } else {
            self.council_answer_row(conv, turn_id, &text)
        };
        if let Some(p) = &self.persist {
            match answer_ts {
                Some(ts) => p.send(crate::persistence::PersistCmd::CouncilAnswer {
                    turn: row,
                    text,
                    ts: ts as i64,
                }),
                None => p.send(crate::persistence::PersistCmd::CouncilTurn(row)),
            }
        }
        if let Some(u) = run.usage {
            let gen = self.cost.generation();
            let _ = crate::persistence::apply_usage_event(
                &mut self.cost,
                self.persist.as_ref(),
                conv,
                &provider,
                &model,
                u,
                crate::feed::unix_now(),
                gen,
            );
        }
        // RC2-R3M3: kept with the council like the approval (page and
        // awili.db), so a relaunch tells later rounds the same.
        if let Some(outcome) = follow_up {
            self.council_record_plan(conv, turn_id, outcome);
        }
        cx.notify();
        // RC2-CNCL9: a message typed meanwhile goes once the round is done.
        self.council_after_seat(conv, cx);
    }
}

// ---- RC2-CNCL9: stop, continue, and the message that waits its turn --------------

impl crate::AwiliApp {
    /// After a member settles: when the round is done, the message typed
    /// meanwhile (if any) goes to the council now.
    pub(crate) fn council_after_seat(&mut self, conv: &str, cx: &mut gpui::Context<Self>) {
        if self
            .councils
            .get(conv)
            .is_none_or(|p| p.council.is_running())
        {
            return;
        }
        let Some(next) = self
            .councils
            .get_mut(conv)
            .and_then(|p| p.queued.pop_front())
        else {
            // RC2-CNCL11: nothing typed meanwhile: the automatic rounds.
            if self
                .councils
                .get(conv)
                .is_some_and(|p| auto_round_due(&p.council, p.auto_rounds))
            {
                self.council_send_round(
                    conv,
                    awili_council::RoundKind::Respond,
                    None,
                    Vec::new(),
                    None,
                    cx,
                );
            } else {
                // RC2-R2F10: the council waits for the owner: ready the
                // next turn.
                self.council_prewarm(conv);
            }
            return;
        };
        if let Some(p) = self.councils.get_mut(conv) {
            p.note = p.queued.front().map(|t| queued_note(t));
        }
        self.council_save_queue(conv);
        if let Err(reason) = self.council_send_message(conv, next.clone(), cx) {
            if let Some(p) = self.councils.get_mut(conv) {
                p.note = Some(reason);
            }
            if self.sidebar.active_id() == conv {
                self.restore_draft(&next, cx);
            }
        }
        cx.notify();
    }

    /// Stop one member of the council on screen; the others keep going and
    /// its words so far are kept.
    pub(crate) fn council_stop_member(&mut self, turn_id: &str, cx: &mut gpui::Context<Self>) {
        let conv = self.sidebar.active_id().to_string();
        if let Some(tx) = self
            .councils
            .get_mut(&conv)
            .and_then(|p| p.runs.get_mut(turn_id))
            .and_then(|r| r.abort_tx.take())
        {
            let _ = tx.send(());
        } else {
            // RC2-CNCLG1: still waiting for its memory lookup.
            self.council_stop_waiting(&conv, turn_id, cx);
        }
        cx.notify();
    }

    /// Stop every member of the council on screen (Stop all, Esc, ⌘.),
    /// those still waiting for the memory lookup too. `true` when something
    /// was answering.
    pub(crate) fn council_stop_all(&mut self, cx: &mut gpui::Context<Self>) -> bool {
        let conv = self.sidebar.active_id().to_string();
        let n = self
            .councils
            .get_mut(&conv)
            .map(crate::council_view::CouncilPage::stop_all)
            .unwrap_or(0);
        let waiting: Vec<String> = self
            .councils
            .get(&conv)
            .map(|p| p.waiting.iter().map(|w| w.launch.turn_id.clone()).collect())
            .unwrap_or_default();
        for id in &waiting {
            self.council_stop_waiting(&conv, id, cx);
        }
        cx.notify();
        n + waiting.len() > 0
    }

    /// RC2-CNCLG1: Stop on a member still waiting for its memory lookup: it
    /// never starts (the lookup's answer won't start it), and its card ends
    /// stopped as any stopped member's does — kept, with what it was sent,
    /// so Retry works. `false` when it wasn't waiting.
    fn council_stop_waiting(
        &mut self,
        conv: &str,
        turn_id: &str,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let Some(route) = self.councils.get(conv).and_then(|p| {
            p.waiting
                .iter()
                .find(|w| w.launch.turn_id == turn_id)
                .map(|w| w.launch.member.route.clone())
        }) else {
            return false;
        };
        let engine = crate::council_exec::engine_target(&route, &self.member_setup()).is_some();
        let Some(page) = self.councils.get_mut(conv) else {
            return false;
        };
        let Some(i) = page
            .waiting
            .iter()
            .position(|w| w.launch.turn_id == turn_id)
        else {
            return false;
        };
        let Waiting { round, launch, .. } = page.waiting.remove(i);
        let owner = owner_name();
        page.sent.entry(launch.turn_id.clone()).or_insert_with(|| {
            sent_facts(
                &launch.seat,
                &owner,
                crate::council_exec::tools_line(&route, Some(engine)),
            )
        });
        // RC2-R3M9 (RC2-R36 review P2): a continue stopped before it began
        // keeps the time its answer had already taken.
        let carry = page.carry.remove(turn_id).unwrap_or_default();
        // A run that ended before it began: it settles like any stopped one.
        let gen = page.runs.get(turn_id).map(|r| r.gen + 1).unwrap_or(1);
        page.runs.insert(
            launch.turn_id.clone(),
            SeatRun {
                gen,
                abort_tx: None,
                started: std::time::Instant::now(),
                first_token_ms: None,
                usage: None,
                prompt_hash: launch.seat.fingerprint(&owner),
                plan_first: false,
                approved: launch.approved,
                changed: false,
                engine_tx: None,
                saved_at: None,
                carried_ms: carry.carried_ms,
                join_first: carry.join,
            },
        );
        let key = SeatKey {
            conv: conv.to_string(),
            round,
            turn_id: launch.turn_id,
            member: launch.member.id,
            gen,
        };
        self.council_finish_seat(&key, SeatEnd::Stopped, cx);
        true
    }

    /// Ask one member again with what it was sent in that round. With
    /// `continue_from` its words so far stay and it is asked to go on from
    /// there; otherwise its turn starts over. The other members are not
    /// touched.
    pub(crate) fn council_rerun(
        &mut self,
        conv: &str,
        round: u32,
        member: &str,
        how: Rerun,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let continue_from = how != Rerun::Retry;
        let Some(page) = self.councils.get_mut(conv) else {
            return false;
        };
        let Some(turn) = page
            .council
            .round(round)
            .and_then(|r| r.turn(member))
            .cloned()
        else {
            return false;
        };
        if turn.status.is_running() {
            return false;
        }
        // RC2-CNCLF3: exactly what the member was sent (kept with the turn,
        // also across a relaunch), checked against its fingerprint — never
        // rebuilt from today's council.
        let Some(sent) = page.sent.get(&turn.id).cloned() else {
            page.note = Some(not_kept_note(&turn.label));
            cx.notify();
            return false;
        };
        let mut seat = sent.prompt.clone();
        if seat.fingerprint(&owner_name()) != sent.fingerprint {
            page.note = Some(not_matching_note(&turn.label));
            cx.notify();
            return false;
        }
        let Some(mut m) = page.council.member(member).cloned() else {
            return false;
        };
        // RC2-CNCLF3: the route it answered on then (frozen with the turn).
        m.route = turn.route.clone();
        m.label = turn.label.clone();
        match &how {
            Rerun::Retry => {}
            Rerun::Continue => {
                seat.ask = format!("{}\n\n{}", seat.ask, continue_ask(&turn.text));
            }
            // RC2-CNCLF1: the approved plan, carried out in one full-access
            // follow-up; the card keeps the plan above what it does.
            Rerun::Approved(plan) => {
                seat.ask = format!(
                    "{}\n\n{}",
                    seat.ask,
                    crate::council_exec::approved_ask(plan)
                );
            }
        }
        // RC2-CNCLF5: the outcome of the owner message this round answers.
        let gate_key = owner_round(&page.council, round).unwrap_or(round);
        // RC2-CNCLI (review P2 on a2c34605): a kept refusal is not reused —
        // Retry asks the memory gate again, which may have recovered.
        let outcome = page.gates.get(&gate_key).cloned().filter(|o| o.is_ok());
        let query = page
            .council
            .rounds
            .iter()
            .filter(|r| r.number <= round)
            .rev()
            .find_map(|r| r.owner_text.clone())
            .unwrap_or_default();
        if let Some(t) = page
            .council
            .round_mut(round)
            .and_then(|r| r.turn_mut(member))
        {
            t.status = awili_council::TurnStatus::Running;
            t.stance = None;
            if !continue_from {
                t.text.clear();
            } else if matches!(how, Rerun::Approved(_)) {
                t.text.push_str("\n\n");
            }
            // RC2-R3 (usability RC2 round 3, Minor 7: the header stayed at
            // "1m 30s" while "Continue it" ran): the turn answers again, so
            // its time runs again \u{2014} on from where it stopped for a continue
            // or an approved plan, from zero for a retry.
            let carried_ms = if continue_from {
                t.elapsed_ms.unwrap_or(0)
            } else {
                0
            };
            t.elapsed_ms = None;
            t.started_ts = crate::feed::unix_now() - i64::try_from(carried_ms / 1000).unwrap_or(0);
            page.carry.insert(
                turn.id.clone(),
                Carry {
                    carried_ms,
                    join: how == Rerun::Continue,
                },
            );
        }
        let launch = SeatLaunch {
            turn_id: turn.id,
            member: m,
            seat,
            approved: matches!(how, Rerun::Approved(_)),
        };
        // RC2-R3: a member whose memory the owner's hooks bring never waits
        // for Awili's lookup (see `council_send_round`), and gets none of
        // Awili's memory even when this message's outcome is kept (RC2-R3M2).
        let outcome = if self.council_memory_from_hooks(&launch.member.route) {
            Some(Ok(None))
        } else {
            outcome
        };
        match outcome {
            Some(o) => self.council_start_seat(conv, round, launch, &o, cx),
            // RC2-CNCL15: no gate outcome kept (a relaunch): ask the memory
            // gate again first, like any send (RC2-CNCLG1: Stop can still
            // take it back while it waits).
            None => {
                let seat = Waiting {
                    gate: gate_key,
                    round,
                    launch,
                    fresh: false,
                };
                self.council_gate_then(conv, gate_key, query, vec![seat], cx);
            }
        }
        cx.notify();
        true
    }

    /// RC2-CNCLF4: keep the messages waiting their turn in `conv` (a quit
    /// gives them back in the box).
    pub(crate) fn council_save_queue(&self, conv: &str) {
        let Some(page) = self.councils.get(conv) else {
            return;
        };
        let list: Vec<&String> = page.queued.iter().collect();
        if let (Some(p), Ok(json)) = (&self.persist, serde_json::to_string(&list)) {
            p.set_state(&crate::council_view::queue_key(conv), json);
        }
    }

    /// "Continue it" on a stopped answer.
    pub(crate) fn council_continue_member(
        &mut self,
        round: u32,
        member: &str,
        cx: &mut gpui::Context<Self>,
    ) {
        let conv = self.sidebar.active_id().to_string();
        self.council_rerun(&conv, round, member, Rerun::Continue, cx);
    }

    /// RC2-CNCLF1: "Make the changes" on a member's plan: that member alone
    /// carries out the plan it proposed, in one full-access follow-up (the
    /// Ask approval of Work: one approval releases one turn).
    pub(crate) fn council_plan_approve(
        &mut self,
        round: u32,
        member: &str,
        cx: &mut gpui::Context<Self>,
    ) {
        let conv = self.sidebar.active_id().to_string();
        let Some(page) = self.councils.get_mut(&conv) else {
            return;
        };
        let Some(id) = page
            .council
            .round(round)
            .and_then(|r| r.turn(member))
            .map(|t| t.id.clone())
        else {
            return;
        };
        let Some(plan) = page.plans.remove(&id) else {
            return;
        };
        if self.council_rerun(&conv, round, member, Rerun::Approved(plan.clone()), cx) {
            // RC2-R3M3: approved and started, not yet carried out: the
            // follow-up's end (on its own task, later) says how it went
            // (`council_finish_seat`).
            self.council_record_plan(&conv, &id, awili_council::PlanOutcome::Started);
        } else if let Some(page) = self.councils.get_mut(&conv) {
            page.plans.insert(id, plan);
        }
    }

    /// RC2-CNCLF1: "Don't change anything" on a member's plan: nothing runs.
    pub(crate) fn council_plan_decline(
        &mut self,
        round: u32,
        member: &str,
        cx: &mut gpui::Context<Self>,
    ) {
        let conv = self.sidebar.active_id().to_string();
        let Some(page) = self.councils.get_mut(&conv) else {
            return;
        };
        let Some(t) = page.council.round(round).and_then(|r| r.turn(member)) else {
            return;
        };
        let label = t.label.clone();
        let id = t.id.clone();
        if page.plans.remove(&id).is_some() {
            page.note = Some(crate::council_exec::declined_note(&label));
            self.council_record_plan(&conv, &id, awili_council::PlanOutcome::Declined);
        }
        cx.notify();
    }

    /// RC2-R3 (usability RC2 round 3, Finding 8): keep what the owner did
    /// with turn `turn_id`'s plan in council `conv` \u{2014} every later round,
    /// the sum-up too, is told \u{2014} also across a relaunch.
    fn council_record_plan(
        &mut self,
        conv: &str,
        turn_id: &str,
        outcome: awili_council::PlanOutcome,
    ) {
        let Some(page) = self.councils.get_mut(conv) else {
            return;
        };
        page.plan_outcomes.insert(turn_id.to_string(), outcome);
        if let Some(p) = &self.persist {
            p.set_state(
                &crate::council_view::plans_key(conv),
                crate::council_view::plan_outcomes_json(&page.plan_outcomes),
            );
        }
    }
}

/// RC2-CNCLF1: how a member is asked again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Rerun {
    /// Retry: start over with what it was sent.
    Retry,
    /// Continue a stopped answer from where it stopped.
    Continue,
    /// Carry out the plan the owner approved.
    Approved(String),
}

// ---- RC2-CNCL10: a member that didn't answer ---------------------------------------

/// RC2-CNCLF3: Retry or Continue when what the member was sent wasn't kept.
pub(crate) fn not_kept_note(label: &str) -> String {
    format!(
        "Awili didn\u{2019}t keep exactly what {label} was sent in that round, so it can\u{2019}t \
         send it again. Ask the council again instead."
    )
}

/// RC2-CNCLF3: Retry or Continue when what was kept doesn't match what was
/// sent (its fingerprint).
pub(crate) fn not_matching_note(label: &str) -> String {
    format!(
        "What Awili kept for {label} doesn\u{2019}t match what it was sent, so it didn\u{2019}t \
         ask again. Ask the council again instead."
    )
}

/// The line under the council after a member is taken off it.
pub(crate) fn removed_note(label: &str) -> String {
    format!(
        "{label} is off the council. Its earlier answers stay; it won\u{2019}t be asked again \
         unless you add it back."
    )
}

impl crate::AwiliApp {
    /// "Retry" on a card that didn't answer: ask only that member again,
    /// with what it was sent; its turn row is replaced.
    pub(crate) fn council_retry_member(
        &mut self,
        round: u32,
        member: &str,
        cx: &mut gpui::Context<Self>,
    ) {
        let conv = self.sidebar.active_id().to_string();
        self.council_rerun(&conv, round, member, Rerun::Retry, cx);
    }

    /// "Remove from council" on a card that didn't answer (its answers so
    /// far stay; the seat row keeps when it was removed).
    pub(crate) fn council_remove_member(&mut self, member: &str, cx: &mut gpui::Context<Self>) {
        let conv = self.sidebar.active_id().to_string();
        let Some(page) = self.councils.get_mut(&conv) else {
            return;
        };
        if !page.council.remove_member(member) {
            return;
        }
        let Some(m) = page.council.member(member).cloned() else {
            return;
        };
        page.note = Some(removed_note(&m.label));
        let now = crate::feed::unix_now();
        if let Some(p) = &self.persist {
            p.send(crate::persistence::PersistCmd::CouncilMember(
                awili_persist::CouncilMemberRow {
                    conversation_id: conv.clone(),
                    member_id: m.id.clone(),
                    position: i64::from(m.position),
                    route: m.route.clone(),
                    label: m.label.clone(),
                    added_ts: now,
                    removed_ts: Some(now),
                },
            ));
        }
        cx.notify();
    }
}

// ---- RC2-CNCL11: answer each other ---------------------------------------------------

/// RC2-CNCLF5: the round of the owner's message that `round` answers — the
/// latest round at or before it that he opened with a message. Its memory
/// gate serves every round after it until his next message.
pub(crate) fn owner_round(c: &awili_council::Council, round: u32) -> Option<u32> {
    c.rounds
        .iter()
        .filter(|r| r.number <= round && r.owner_text.is_some())
        .map(|r| r.number)
        .max()
}

/// Whether the council should answer each other again on its own: fewer
/// automatic rounds than `auto` since the owner's last message, the last
/// round finished (not stopped by the owner, not a sum-up), and at least
/// two members answered it. The cap counts per owner message.
pub(crate) fn auto_round_due(c: &awili_council::Council, auto: u8) -> bool {
    use awili_council::{RoundKind, TurnStatus};
    let Some(last) = c.rounds.last() else {
        return false;
    };
    if last.is_running()
        || last.kind == RoundKind::SumUp
        || last.turns.iter().any(|t| t.status == TurnStatus::Stopped)
        || last
            .turns
            .iter()
            .filter(|t| t.status == TurnStatus::Answered)
            .count()
            < 2
    {
        return false;
    }
    let since_ask = c
        .rounds
        .iter()
        .rev()
        .take_while(|r| r.kind == RoundKind::Respond)
        .count();
    since_ask < usize::from(auto)
}

impl crate::AwiliApp {
    /// "Let them answer each other" / "Another round": every member that
    /// answered reads the others' answers and replies.
    pub(crate) fn council_answer_each_other(&mut self, cx: &mut gpui::Context<Self>) {
        let conv = self.sidebar.active_id().to_string();
        if self
            .councils
            .get(&conv)
            .is_none_or(|p| p.council.is_running())
        {
            return;
        }
        self.council_send_round(
            &conv,
            awili_council::RoundKind::Respond,
            None,
            Vec::new(),
            None,
            cx,
        );
    }

    /// "Answer each other automatically": Off, 1, 2 or 3 rounds after each
    /// message, kept with this council.
    pub(crate) fn council_set_auto_rounds(&mut self, n: u8, cx: &mut gpui::Context<Self>) {
        let conv = self.sidebar.active_id().to_string();
        let n = n.min(crate::council_view::MAX_AUTO_ROUNDS);
        if let Some(page) = self.councils.get_mut(&conv) {
            page.auto_rounds = n;
        }
        if let Some(p) = &self.persist {
            p.set_state(
                &crate::council_view::settings_key(&conv),
                serde_json::json!({ "auto_rounds": n }).to_string(),
            );
        }
        cx.notify();
    }
}

// ---- RC2-CNCL13: ask one to sum up ---------------------------------------------------

impl crate::AwiliApp {
    /// Show or hide "Who sums up?".
    pub(crate) fn council_toggle_sum_up(&mut self, cx: &mut gpui::Context<Self>) {
        let conv = self.sidebar.active_id().to_string();
        if let Some(page) = self.councils.get_mut(&conv) {
            page.sum_up_open = !page.sum_up_open;
        }
        cx.notify();
    }

    /// One member reads the whole council and writes one combined answer:
    /// where it agrees, where it splits and why, and what it would do. The
    /// summary leads the page.
    pub(crate) fn council_sum_up(&mut self, member: &str, cx: &mut gpui::Context<Self>) {
        let conv = self.sidebar.active_id().to_string();
        let Some(page) = self.councils.get_mut(&conv) else {
            return;
        };
        if page.council.is_running() {
            return;
        }
        page.sum_up_open = false;
        self.council_send_round(
            &conv,
            awili_council::RoundKind::SumUp,
            None,
            Vec::new(),
            Some(member.to_string()),
            cx,
        );
    }
}

// ---- RC2-CNCL14: take an answer into Work -------------------------------------------

/// The Work helper a council member can continue as: the same agent CLI,
/// the in-app model, or (Hermes) the fleet gateway. A `[[providers]]` model
/// has no Work lane (`None`: Work keeps its default helper).
pub(crate) fn work_helper_for(route: &str) -> Option<WorkExecutor> {
    if route == IN_APP_ROUTE {
        return Some(WorkExecutor::InApp);
    }
    if let Some(id) = in_app_extra_id(route) {
        return Some(WorkExecutor::InAppExtra { id: id.to_string() });
    }
    if route == agent_chat::FLEET_CHAT_ROUTE {
        return Some(WorkExecutor::Gateway);
    }
    match WorkExecutor::from_persist(route) {
        Some(e @ WorkExecutor::Agent { .. }) => Some(e),
        _ => None,
    }
}

/// What lands in the Work box: the owner's question, then the answer.
pub(crate) fn work_box_text(question: Option<&str>, label: &str, answer: &str) -> String {
    match question.map(str::trim).filter(|q| !q.is_empty()) {
        // RC2-R54: the answer as plain text (no markdown marks).
        Some(q) => format!(
            "{q}\n\n{label} answered:\n{}",
            crate::composer_drafts::plain_box_text(answer.trim())
        ),
        None => format!(
            "{label} answered:\n{}",
            crate::composer_drafts::plain_box_text(answer.trim())
        ),
    }
}

/// The note after "Continue in Work".
pub(crate) fn continue_in_work_note(label: &str, helper: bool) -> String {
    if helper {
        format!(
            "Your question and {label}\u{2019}s answer are in the Work box, with {label} as the \
             helper. Nothing is sent until you press Enter."
        )
    } else {
        "Your question and the answer are in the Work box. Nothing is sent until you press \
         Enter."
            .to_string()
    }
}

impl crate::AwiliApp {
    /// "Continue in Work" on a card: a new Work conversation with the
    /// owner's question and that answer in the box, that member as the
    /// helper when it can be one; nothing is sent until Enter (RC2-UX22
    /// rule).
    pub(crate) fn council_continue_in_work(
        &mut self,
        round: u32,
        member: &str,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let conv = self.sidebar.active_id().to_string();
        let Some(page) = self.councils.get(&conv) else {
            return;
        };
        let Some(t) = page
            .council
            .round(round)
            .and_then(|r| r.turn(member))
            .cloned()
        else {
            return;
        };
        let question = page
            .council
            .rounds
            .iter()
            .filter(|r| r.number <= round)
            .rev()
            .find_map(|r| r.owner_text.clone());
        let text = work_box_text(question.as_deref(), &t.label, t.body());
        let helper = work_helper_for(&t.route);
        self.set_mode(crate::work::Mode::Work, cx);
        // RC2-R2F3 (usability RC2 round 2: the box of an earlier, never-sent
        // "Continue in Work" conversation was reused and its text stayed
        // under the new one): always a brand-new conversation, as Chat's
        // "Continue in Work" (RC2-R1F4), so the box holds exactly this text.
        self.start_on_default_helper();
        // RC2-R3 (round 3 Minor 14): a blank never-sent one is used first
        // (one with a draft never is), so no empty rows pile up.
        let id = self.blank_or_new_work_conversation(cx);
        self.open_new_conversation(&id, window, cx);
        if let Some(h) = helper.clone() {
            self.pick_work_executor(h, cx);
        }
        // The box moves to the new conversation first, then takes the text.
        self.sync_composer_draft(cx);
        self.restore_draft(&text, cx);
        // RC2-R54: an untouched copy never holds the conversation.
        self.drafts
            .place(&crate::composer_drafts::conversation_key(&id), &text);
        self.show_info_toast(continue_in_work_note(&t.label, helper.is_some()), cx);
        let h = self.composer.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
    }
}

/// A loopback OpenAI-compatible server for council tests: each request is
/// captured, then answered per the handler (status, chunks, a gap between
/// them, or held open until the client goes away).
#[cfg(test)]
pub(crate) mod stub {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    /// How the stub answers one request.
    #[derive(Clone, Debug)]
    pub(crate) struct Reply {
        pub(crate) status: u16,
        pub(crate) chunks: Vec<String>,
        /// Wait this long before each chunk.
        pub(crate) gap_ms: u64,
        /// After the chunks, keep the connection open (a member that never
        /// finishes) until the client hangs up.
        pub(crate) hold: bool,
        /// Words in / out to report at the end (a usage chunk).
        pub(crate) usage: Option<(u32, u32)>,
    }

    impl Reply {
        pub(crate) fn text(chunks: &[&str]) -> Self {
            Self {
                status: 200,
                chunks: chunks.iter().map(|c| c.to_string()).collect(),
                gap_ms: 0,
                hold: false,
                usage: None,
            }
        }
    }

    /// One captured request.
    #[derive(Clone, Debug)]
    pub(crate) struct Seen {
        pub(crate) path: String,
        pub(crate) body: String,
        pub(crate) at: std::time::Instant,
    }

    pub(crate) type Capture = Arc<Mutex<Vec<Seen>>>;

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut data = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            if let Some(h) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&data[..h]).to_ascii_lowercase();
                let cl = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if data.len() >= h + 4 + cl {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&data).into_owned()
    }

    fn sse(content: &str) -> String {
        let v = serde_json::json!({"choices":[{"delta":{"content": content}}]});
        format!("data: {v}\n\n")
    }

    /// Start the stub; returns its base URL and the capture.
    pub(crate) fn spawn(
        handler: impl Fn(&str) -> Reply + Send + Sync + 'static,
    ) -> (String, Capture) {
        let capture: Capture = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let cap = Arc::clone(&capture);
        let handler = Arc::new(handler);
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                let cap = Arc::clone(&cap);
                let handler = Arc::clone(&handler);
                std::thread::spawn(move || {
                    let mut stream = s;
                    let req = read_request(&mut stream);
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                    // The status strip's read-only Lemonade check (a 404 here: not
                    // Lemonade) is no turn: not counted, not answered as one.
                    if path == "/api/v1/health" {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        return;
                    }
                    let body = req
                        .split_once("\r\n\r\n")
                        .map(|(_, b)| b.to_string())
                        .unwrap_or_default();
                    let reply = handler(&body);
                    cap.lock().unwrap().push(Seen {
                        path,
                        body,
                        at: std::time::Instant::now(),
                    });
                    if reply.status != 200 {
                        let msg = "{\"error\":{\"message\":\"stub says no\"}}";
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {} Error\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{msg}",
                            reply.status,
                            msg.len()
                        );
                        return;
                    }
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                         Connection: close\r\n\r\n"
                    );
                    let _ = stream.flush();
                    for c in &reply.chunks {
                        std::thread::sleep(std::time::Duration::from_millis(reply.gap_ms));
                        if stream.write_all(sse(c).as_bytes()).is_err() {
                            return;
                        }
                        let _ = stream.flush();
                    }
                    if reply.hold {
                        // Until the client hangs up (Stop drops the stream).
                        loop {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            if stream.write_all(b": keep-alive\n\n").is_err() {
                                return;
                            }
                            let _ = stream.flush();
                        }
                    }
                    // RC2-CNCLF2: a well-formed reply says why it ended (the
                    // tool engine refuses a stream without a finish reason).
                    let stop = serde_json::json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]});
                    let _ = stream.write_all(format!("data: {stop}\n\n").as_bytes());
                    if let Some((i, o)) = reply.usage {
                        let u = serde_json::json!({"choices": [], "usage": {"prompt_tokens": i, "completion_tokens": o}});
                        let _ = stream.write_all(format!("data: {u}\n\n").as_bytes());
                    }
                    let _ = stream.write_all(b"data: [DONE]\n\n");
                    let _ = stream.flush();
                });
            }
        });
        (format!("http://{addr}"), capture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[cfg(not(unix))]
    #[allow(unused_variables, dead_code)]
    fn agents_with_markers(dir: &std::path::Path) -> awili_config::AgentBinsConfig {
        unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
    }
    #[cfg(unix)]
    fn agents_with_markers(dir: &std::path::Path) -> awili_config::AgentBinsConfig {
        use std::os::unix::fs::PermissionsExt;
        let mut a = awili_config::AgentBinsConfig::default();
        let make = |name: &str| {
            let bin = dir.join(name);
            let marker = dir.join(format!("{name}-ran"));
            std::fs::write(
                &bin,
                format!("#!/bin/sh\ntouch {}\nexit 1\n", marker.display()),
            )
            .unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            Some(bin.display().to_string())
        };
        a.claude_bin = make("claude");
        a.codex_bin = make("codex");
        a.zcode_bin = make("zcode");
        a.dsh_bin = make("dsh");
        a
    }

    fn drain(p: Box<dyn ChatProvider>, user: &str) -> Vec<ProviderEvent> {
        let req = ChatRequest {
            model: String::new(),
            system: Some("You are Gemma 4 26B, one of 2 models on Owner's council.".into()),
            messages: vec![awili_providers::ChatMessage::user(user.to_string())],
        };
        futures::executor::block_on(p.stream(req).collect::<Vec<_>>())
    }

    fn ran(dir: &std::path::Path) -> Vec<String> {
        ["claude", "codex", "zcode", "dsh"]
            .iter()
            .filter(|n| dir.join(format!("{n}-ran")).exists())
            .map(|n| n.to_string())
            .collect()
    }

    /// RC2-CNCL6: the in-app member asks the `[engine]` endpoint directly,
    /// with the engine's model, the brief as the system text and the council
    /// prompt as the message — and streams its words back.
    #[test]
    fn in_app_member_uses_the_engine_endpoint() {
        let (base, seen) = stub::spawn(|_| stub::Reply::text(&["Keep ", "Gemma."]));
        let dir = tempfile::tempdir().unwrap();
        let agents = agents_with_markers(dir.path());
        let keys = crate::provider_build::ProviderKeys::default();
        let engine = awili_config::EngineConfig {
            base_url: format!("{base}/v1"),
            model: "gemma-4-26b-a4b-it-qat-mlx-4bit".into(),
            ..Default::default()
        };
        let fleet = ChatRoute::Unavailable("no fleet".into());
        let s = MemberSetup {
            agents: &agents,
            engine: &engine,
            extras: &[],
            fleet: &fleet,
            cwd: dir.path().display().to_string(),
            choices: &[],
            keys: &keys,
            hermes_base_url: "",
            connectors: None,
            council: String::new(),
            approved: false,
            sandbox: Default::default(),
        };
        let events = drain(member_provider(IN_APP_ROUTE, &s), "Gemma or Qwen?");
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::Token(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Keep Gemma.");
        assert!(matches!(events.last(), Some(ProviderEvent::Done)));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].path, "/v1/chat/completions");
        let body: serde_json::Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(body["model"], "gemma-4-26b-a4b-it-qat-mlx-4bit");
        let msgs = body["messages"].to_string();
        assert!(
            msgs.contains("one of 2 models on Owner's council"),
            "{msgs}"
        );
        assert!(msgs.contains("Gemma or Qwen?"), "{msgs}");
        assert!(ran(dir.path()).is_empty(), "no agent CLI started");
    }

    /// RC2-CNCL6: a member that cannot run says why and never reaches
    /// another model — no agent starts, no request leaves.
    #[test]
    fn offline_member_is_unavailable_not_rerouted() {
        let (base, seen) = stub::spawn(|_| stub::Reply::text(&["should not be asked"]));
        let dir = tempfile::tempdir().unwrap();
        let agents = agents_with_markers(dir.path());
        let keys = crate::provider_build::ProviderKeys::default();
        let unset = awili_config::EngineConfig::default();
        let fleet = ChatRoute::Unavailable(
            "provider:fleet: the `fleet` provider is disabled. Chat will not fall back to \
             another model."
                .into(),
        );
        let s = MemberSetup {
            agents: &agents,
            engine: &unset,
            extras: &[],
            fleet: &fleet,
            cwd: dir.path().display().to_string(),
            choices: &[],
            keys: &keys,
            hermes_base_url: "",
            connectors: None,
            council: String::new(),
            approved: false,
            sandbox: Default::default(),
        };
        for route in [
            IN_APP_ROUTE,
            "gateway",
            "local-build:spark",
            "agent:nope",
            "garbage",
        ] {
            let events = drain(member_provider(route, &s), "hello");
            assert!(
                matches!(events.first(), Some(ProviderEvent::Error(m)) if !m.is_empty()),
                "{route}: {events:?}"
            );
            assert!(
                matches!(events.last(), Some(ProviderEvent::Done)),
                "{route}"
            );
            assert!(
                !events.iter().any(|e| matches!(e, ProviderEvent::Token(_))),
                "{route}"
            );
        }
        assert!(matches!(
            member_route("gateway", &fleet),
            MemberRoute::Unavailable(_)
        ));
        assert!(ran(dir.path()).is_empty(), "no agent CLI started");
        assert!(seen.lock().unwrap().is_empty(), "nothing reached {base}");
    }

    /// RC2-CNCL6: Hermes on a council is the closed fleet route — its own
    /// endpoint and model, or a plain refusal; never an agent CLI.
    #[test]
    fn fleet_member_stays_closed_route() {
        let (base, seen) = stub::spawn(|_| stub::Reply::text(&["fleet answer"]));
        let dir = tempfile::tempdir().unwrap();
        let agents = agents_with_markers(dir.path());
        let keys = crate::provider_build::ProviderKeys::default();
        let toml = format!(
            "default_model = \"qwen38-27b\"\n[chat]\nexecutor = \"agent:claude:haiku\"\n\
             [[providers]]\nname = \"fleet\"\nkind = \"openai\"\nbase_url = \"{base}\"\n\
             models = []\nenabled = true\n"
        );
        let cfg = awili_config::Config::parse(&toml).unwrap();
        let fleet = fleet_route(&cfg);
        assert!(matches!(fleet, ChatRoute::Fleet(_)), "{fleet:?}");
        let engine = awili_config::EngineConfig::default();
        let s = MemberSetup {
            agents: &agents,
            engine: &engine,
            extras: &[],
            fleet: &fleet,
            cwd: dir.path().display().to_string(),
            choices: &[],
            keys: &keys,
            hermes_base_url: "",
            connectors: None,
            council: String::new(),
            approved: false,
            sandbox: Default::default(),
        };
        let events = drain(member_provider("provider:fleet", &s), "hello");
        assert!(events
            .iter()
            .any(|e| matches!(e, ProviderEvent::Token(t) if t == "fleet answer")));
        let body: serde_json::Value = serde_json::from_str(&seen.lock().unwrap()[0].body).unwrap();
        assert_eq!(body["model"], "qwen38-27b");
        // No fleet provider configured: a refusal, still no agent.
        let none: awili_config::Config =
            awili_config::Config::parse("[chat]\nexecutor = \"agent:claude:haiku\"\n").unwrap();
        let closed = fleet_route(&none);
        let s = MemberSetup {
            agents: &agents,
            engine: &engine,
            extras: &[],
            fleet: &closed,
            cwd: dir.path().display().to_string(),
            choices: &[],
            keys: &keys,
            hermes_base_url: "",
            connectors: None,
            council: String::new(),
            approved: false,
            sandbox: Default::default(),
        };
        let events = drain(member_provider("provider:fleet", &s), "hello");
        assert!(matches!(events.first(), Some(ProviderEvent::Error(m)) if m.contains("fleet")));
        assert!(ran(dir.path()).is_empty(), "no agent CLI started");
    }

    #[cfg(not(unix))]
    #[allow(unused_variables, dead_code)]
    fn recording_agents(dir: &std::path::Path) -> awili_config::AgentBinsConfig {
        unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
    }
    #[cfg(unix)]
    /// Agent CLIs in `dir` that write their argv (one per line) and working
    /// folder, then answer one stream-json line (Claude's shape).
    fn recording_agents(dir: &std::path::Path) -> awili_config::AgentBinsConfig {
        use std::os::unix::fs::PermissionsExt;
        let mut a = awili_config::AgentBinsConfig::default();
        let make = |name: &str| {
            let bin = dir.join(name);
            std::fs::write(
                &bin,
                format!(
                    "#!/bin/sh\npwd -P > {d}/{name}-pwd\n\
                     for a in \"$@\"; do printf '%s\\n' \"$a\"; done > {d}/{name}-rawargv\n\
                     prev=\"\"; for a in \"$@\"; do if [ \"$prev\" = --append-system-prompt-file ]; then cat \"$a\"; echo; else printf '%s\\n' \"$a\"; fi; prev=\"$a\"; done > {d}/{name}-argv\n\
                     cat > {d}/{name}-stdin\n\
                     printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"ok\"}}'\n",
                    d = dir.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            Some(bin.display().to_string())
        };
        a.claude_bin = make("claude");
        a.codex_bin = make("codex");
        a.zcode_bin = make("zcode");
        a.dsh_bin = make("dsh");
        a
    }

    fn launched(dir: &std::path::Path, name: &str) -> (Vec<String>, String) {
        let argv = std::fs::read_to_string(dir.join(format!("{name}-argv"))).unwrap_or_default();
        let stdin = std::fs::read_to_string(dir.join(format!("{name}-stdin"))).unwrap_or_default();
        let argv: Vec<String> = argv.lines().map(str::to_string).collect();
        let pwd = std::fs::read_to_string(dir.join(format!("{name}-pwd"))).unwrap_or_default();
        (
            if stdin.is_empty() {
                argv
            } else {
                crate::test_stdin::argv_with_prompt(argv, &stdin)
            },
            pwd.trim().to_string(),
        )
    }

    fn has_pair(argv: &[String], a: &str, b: &str) -> bool {
        argv.windows(2).any(|w| w[0] == a && w[1] == b)
    }

    /// RC2-CNCLF1 (review P1 on cf08877c): a council's agent members run
    /// RC2-CNCLF6 (review P2 on cf08877c): a member whose rate Awili does not
    /// know says "cost not reported" — never "free" — even when it reported
    /// its words; "free" is only for a model at home; a known rate is priced.
    #[test]
    fn rc2cnclf6_an_unknown_rate_is_not_reported() {
        let usage = awili_providers::Usage {
            cached_input_tokens: None,
            input_tokens: 100_000,
            output_tokens: 50_000,
        };
        let choice = |provider: &str, kind, base: &str, model: &str| awili_config::ModelChoice {
            provider: provider.into(),
            kind,
            base_url: base.into(),
            model: model.into(),
            api_key: None,
        };
        let choices = vec![
            choice(
                "openrouter",
                awili_config::ProviderKind::OpenAi,
                "https://openrouter.ai/api/v1",
                "mystery-70b",
            ),
            choice(
                "spark",
                awili_config::ProviderKind::OpenAi,
                "http://10.77.68.60:8000/v1",
                "qwen38-27b",
            ),
            choice(
                "home",
                awili_config::ProviderKind::Ollama,
                "http://localhost:11434",
                "qwen3",
            ),
        ];
        let engine = awili_config::EngineConfig::default();
        let fleet = ChatRoute::Unavailable("no fleet".into());
        let word = |route: &str| {
            let (provider, model) = usage_identity(route, &engine, &[], &fleet);
            let cost = member_cost(route, &provider, &model, &choices, usage);
            crate::run_footer::council_cost_word(route, cost, Some((100_000, 50_000)))
        };
        // Unknown rates: not reported, whatever the words.
        assert_eq!(word("model:openrouter:mystery-70b"), "cost not reported");
        assert_eq!(word("agent:dsh:deepseek-mystery"), "cost not reported");
        // At home: free.
        assert_eq!(word("model:spark:qwen38-27b"), "free");
        assert_eq!(word("model:home:qwen3"), "free");
        assert_eq!(word(IN_APP_ROUTE), "free");
        // A known rate: its price.
        assert_eq!(word("agent:dsh:deepseek-flash"), "$0.09");
        // Subscriptions and GLM keep their own words.
        assert_eq!(word("agent:claude:sonnet"), "included");
        assert_eq!(word("agent:glm"), "billed by Z.ai");
    }

    /// like Work in "Ask before changes" — Claude in its plan mode with the
    /// owner's settings, Codex in its read-only planning step, in the
    /// owner's home folder — never in Chat's restricted posture (no
    /// web-search-only tool list, no safe mode, no Chat persona). GLM and
    /// DeepSeek, which can't ask, run without asking in Awili's council
    /// folder. An approved plan releases one full-access follow-up.
    #[test]
    fn rc2cnclf1_agent_members_run_like_work_in_ask_mode() {
        let dir = tempfile::tempdir().unwrap();
        let agents = recording_agents(dir.path());
        let keys = crate::provider_build::ProviderKeys::default();
        let engine = awili_config::EngineConfig::default();
        let fleet = ChatRoute::Unavailable("no fleet".into());
        let mut s = MemberSetup {
            agents: &agents,
            engine: &engine,
            extras: &[],
            fleet: &fleet,
            cwd: dir.path().display().to_string(),
            choices: &[],
            keys: &keys,
            hermes_base_url: "",
            connectors: None,
            council: String::new(),
            approved: false,
            sandbox: Default::default(),
        };
        let home = dirs::home_dir()
            .and_then(|h| h.canonicalize().ok())
            .unwrap()
            .display()
            .to_string();
        let council = dirs::data_dir()
            .unwrap()
            .join(awili_config::APP_DIR_NAME)
            .join("council")
            .display()
            .to_string();
        for route in [
            "agent:claude:sonnet",
            "agent:codex",
            "agent:glm",
            "agent:dsh:deepseek-flash",
        ] {
            drain(member_provider(route, &s), "Look at my notes.");
        }
        let (claude, cwd) = launched(dir.path(), "claude");
        assert!(has_pair(&claude, "--permission-mode", "plan"), "{claude:?}");
        assert!(claude.iter().any(|a| a == "--settings"), "{claude:?}");
        for chat_only in ["--tools=WebSearch", "--safe-mode", "--strict-mcp-config"] {
            assert!(
                !claude.iter().any(|a| a == chat_only),
                "{chat_only}: {claude:?}"
            );
        }
        assert!(
            claude
                .iter()
                .any(|a| a.contains("You are in a read-only planning step.")),
            "the planning step: {claude:?}"
        );
        assert_eq!(cwd, home, "Claude works in the owner's home folder");
        let (codex, cwd) = launched(dir.path(), "codex");
        assert!(has_pair(&codex, "--sandbox", "read-only"), "{codex:?}");
        assert!(
            !codex
                .iter()
                .any(|a| a.contains("You are the Chat helper") || a.contains("answer-only turn")),
            "no Chat persona: {codex:?}"
        );
        assert_eq!(cwd, home, "Codex works in the owner's home folder");
        let (glm, cwd) = launched(dir.path(), "zcode");
        assert!(has_pair(&glm, "--mode", "yolo"), "{glm:?}");
        assert!(cwd.ends_with(&format!("{}/council", awili_config::APP_DIR_NAME)), "{cwd} vs {council}");
        let (_, cwd) = launched(dir.path(), "dsh");
        assert!(cwd.ends_with(&format!("{}/council", awili_config::APP_DIR_NAME)), "{cwd} vs {council}");
        // "Make the changes": one full-access follow-up.
        s.approved = true;
        drain(member_provider("agent:claude:sonnet", &s), "Go on.");
        let (claude, _) = launched(dir.path(), "claude");
        assert!(
            claude.iter().any(|a| a == "--dangerously-skip-permissions"),
            "{claude:?}"
        );
        assert!(!claude
            .iter()
            .any(|a| a.contains("You are in a read-only planning step.")));
    }

    /// RC2-R2F6 (usability RC2 round 2, F2: Claude's council answers began
    /// "This is a simple question with no changes needed \u{2014} no plan
    /// required." and "\u{2026} no exploration or design agents needed."): a
    /// member in its planning step is told, with the planning text, that a
    /// council answer to a question is just the answer \u{2014} no word about the
    /// planning step, plans or agents. The approved follow-up is not a
    /// planning step and gets neither.
    #[test]
    fn rc2r2f6_planning_members_are_told_to_just_answer() {
        let dir = tempfile::tempdir().unwrap();
        let agents = recording_agents(dir.path());
        let keys = crate::provider_build::ProviderKeys::default();
        let engine = awili_config::EngineConfig::default();
        let fleet = ChatRoute::Unavailable("no fleet".into());
        let mut s = MemberSetup {
            agents: &agents,
            engine: &engine,
            extras: &[],
            fleet: &fleet,
            cwd: dir.path().display().to_string(),
            choices: &[],
            keys: &keys,
            hermes_base_url: "",
            connectors: None,
            council: String::new(),
            approved: false,
            sandbox: Default::default(),
        };
        let posture = crate::council_exec::COUNCIL_ANSWER_POSTURE;
        assert!(posture.contains("do not mention"), "{posture}");
        for (route, bin) in [("agent:claude:sonnet", "claude"), ("agent:codex", "codex")] {
            drain(member_provider(route, &s), "ZFS or btrfs?");
            let (argv, _) = launched(dir.path(), bin);
            assert!(
                argv.iter().any(|a| a.contains(posture)),
                "{bin} is told to just answer: {argv:?}"
            );
            // S1b: and never on the command line itself, but for Codex's
            // `-c developer_instructions` (no file key exists for it).
            let raw = std::fs::read_to_string(dir.path().join(format!("{bin}-rawargv"))).unwrap();
            let on_argv: Vec<&str> = raw
                .lines()
                .filter(|l| l.contains("do not mention"))
                .collect();
            if bin == "claude" {
                assert!(on_argv.is_empty(), "{bin}: {on_argv:?}");
            } else {
                assert!(
                    on_argv
                        .iter()
                        .all(|l| l.starts_with("developer_instructions=")),
                    "{bin}: {on_argv:?}"
                );
            }
        }
        s.approved = true;
        drain(member_provider("agent:claude:sonnet", &s), "Go on.");
        let (claude, _) = launched(dir.path(), "claude");
        assert!(!claude.iter().any(|a| a.contains(posture)), "{claude:?}");
    }
}

/// RC2-R53: the system text of a member on Awili's tool engine: the memory
/// preamble, the owner's AGENTS.md, what its file tools reach, then the
/// council brief (as the in-app engine gets them in Work).
pub(crate) fn engine_member_system(
    policy: Option<&str>,
    agents_block: Option<&str>,
    seat: &awili_council::SeatPrompt,
) -> String {
    [
        policy,
        agents_block,
        // RC2-R2F8: its file tools reach any folder.
        Some(crate::council_exec::ANY_FOLDER_NOTE),
        Some(seat.brief.as_str()),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
}
