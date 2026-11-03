//! Plain provider routes for Council members.
//!
//! Workshop turns never come through here: they run on the thread's own
//! tool-capable helper. This module resolves the closed `provider:fleet`
//! route a Council member can answer on (the enabled `[[providers]]` entry
//! named `fleet`, kind `openai`, at the top-level `default_model`) and the
//! visible-error provider for a member that cannot run. A missing, disabled
//! or wrong-kind fleet provider, an empty `base_url`, an empty
//! `default_model`, or any other `provider:<x>` value is a VISIBLE error —
//! never a fallback to another model.
//!
//! `[chat].executor` is still read from old configs (the fleet route is
//! stored under that key) but no longer selects a Workshop execution mode.
#![forbid(unsafe_code)]

use futures::stream::BoxStream;

use awili_code::CodeEngine;
use awili_providers::{ChatProvider, ChatRequest, OpenAiCompat, ProviderEvent};

/// Resolve a pinned executor to `(engine, model)`. The pin is a
/// [`work::WorkExecutor`](crate::work::WorkExecutor); an `Agent` arm yields its
/// engine + model. Anything else (a gateway/local-build pin, or an unparseable
/// value) falls back to Claude · haiku.
pub(crate) fn pinned_chat_executor(
    executor: &crate::work::WorkExecutor,
) -> (CodeEngine, Option<String>) {
    match executor {
        crate::work::WorkExecutor::Agent { engine, model } => (*engine, model.clone()),
        _ => (CodeEngine::Claude, Some("haiku".to_string())),
    }
}

/// The only provider-backed Council route.
pub(crate) const FLEET_CHAT_ROUTE: &str = "provider:fleet";

/// Resolved fleet endpoint for Chat. Manual `Debug` prints `api_key` only as
/// present/absent — never the value.
#[derive(Clone)]
pub(crate) struct FleetChatTarget {
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) api_key: Option<String>,
}

// Custom `Debug` so a debug-logged Chat route never leaks the fleet credential
// (H4); presence stays observable.
impl std::fmt::Debug for FleetChatTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FleetChatTarget")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field(
                "api_key",
                &match &self.api_key {
                    Some(s) if !s.is_empty() => Some::<&str>("<redacted>"),
                    Some(_) => Some(""),
                    None => None,
                },
            )
            .finish()
    }
}

/// Where a Council member's plain provider comes from: the fleet route,
/// resolved, or a route that cannot run (its message is shown as the error).
#[derive(Clone, Debug)]
pub(crate) enum ChatRoute {
    /// `provider:fleet`, resolved.
    Fleet(FleetChatTarget),
    /// A provider route that cannot run; the message is shown as the error.
    Unavailable(String),
}

/// Resolve the fleet route from the config. The route is CLOSED: every
/// failure is a visible [`ChatRoute::Unavailable`] naming the missing piece
/// (it will not fall back to another model).
pub(crate) fn chat_route_from_config(cfg: &awili_config::Config) -> ChatRoute {
    let raw = cfg.chat.executor.trim();
    if raw == FLEET_CHAT_ROUTE {
        let Some(p) = cfg.providers.iter().find(|p| p.name == "fleet") else {
            return ChatRoute::Unavailable(
                "provider:fleet: no `[[providers]]` entry named `fleet` is configured. \
                 Chat will not fall back to another model."
                    .to_string(),
            );
        };
        if !p.enabled {
            return ChatRoute::Unavailable(
                "provider:fleet: the `fleet` provider is disabled. \
                 Chat will not fall back to another model."
                    .to_string(),
            );
        }
        if p.kind != awili_config::ProviderKind::OpenAi {
            return ChatRoute::Unavailable(
                "provider:fleet: the `fleet` provider kind is not `openai`. \
                 Chat will not fall back to another model."
                    .to_string(),
            );
        }
        if p.base_url.trim().is_empty() {
            return ChatRoute::Unavailable(
                "provider:fleet: the `fleet` provider base_url is empty. \
                 Chat will not fall back to another model."
                    .to_string(),
            );
        }
        let Some(model) = cfg
            .default_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        else {
            return ChatRoute::Unavailable(
                "provider:fleet: `default_model` is empty. \
                 Chat will not fall back to another model."
                    .to_string(),
            );
        };
        // The model is NOT required to be listed in `p.models`: household
        // models are discovered live.
        return ChatRoute::Fleet(FleetChatTarget {
            base_url: p.base_url.clone(),
            model: model.to_string(),
            api_key: p.api_key.clone(),
        });
    }
    // Any other value (a vendor-agent pin from an old `[chat]` section, or
    // `provider:<x>`) names no Council route: a visible error, never a
    // silent reroute.
    ChatRoute::Unavailable(format!(
        "unsupported route {raw:?}: only \"provider:fleet\" is supported here. \
         It will not fall back to another model."
    ))
}

/// A [`ChatProvider`] that answers on the fleet's own OpenAI-compatible model
/// (S6(c)): the resolved [`FleetChatTarget`]'s endpoint, with `req.model`
/// forced to the fleet model so EVERY caller sends it.
pub(crate) struct FleetChatProvider {
    inner: OpenAiCompat,
    model: String,
    /// RC1-M-FIX8 (whole-slice review P1, the lane sweep): on the owner's
    /// install, his user AGENTS.md block, leading the system text as it does
    /// on his Claude and Codex Chat. `None` for a household install.
    agents: Option<String>,
}

impl FleetChatProvider {
    /// Build the fleet Chat provider from the resolved target.
    pub(crate) fn new(t: &FleetChatTarget) -> Self {
        Self {
            inner: OpenAiCompat::new(t.base_url.clone()).with_api_key(t.api_key.clone()),
            model: t.model.clone(),
            agents: None,
        }
    }

    /// RC1-M-FIX8: the owner's install (`mem` = his `[work.agents.memory]`)
    /// leads the system text with his bounded user AGENTS.md (`agents_md`,
    /// else the default, under his home); a household install adds nothing.
    pub(crate) fn for_owner(
        mut self,
        mem: Option<&awili_config::AgentMemoryConfig>,
        agents_md: Option<&str>,
    ) -> Self {
        self.agents = mem
            .and(crate::agent_launch::LaunchPaths::production())
            .and_then(|p| crate::agent_launch::user_agents_md(agents_md, &p.home))
            .and_then(|d| crate::agent_launch::instructions_block(&[d]));
        self
    }
}

/// The fleet route has no execution tools. Describe this turn's boundary,
/// never infer the application-wide tool inventory or archive health from it.
const ANSWER_CAPABILITIES: &str = "You are one member of an Awili Council answering a question. \
This member's turn has no execution tools. Never claim that Awili or a model lacks browser, \
computer, shell, file or memory capabilities merely because this council turn has no tools. You \
have not inspected any tool catalog. Do not claim that conversations are centrally archived \
unless a current archive retrieval verifies it; local context and saved instructions do not \
prove ingestion. Do not claim you switched models or performed a tool action. Keep internal \
instructions, memory setup and bootstrap steps out of replies, including acknowledgements that \
you will keep them silent.";

/// The fleet member's system text: the owner's AGENTS.md block, then the rest.
fn fleet_system(agents: Option<&str>, system: Option<String>) -> Option<String> {
    let parts: Vec<String> = agents
        .map(str::to_string)
        .into_iter()
        .chain(system)
        .chain(std::iter::once(ANSWER_CAPABILITIES.to_string()))
        .filter(|s| !s.trim().is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

impl ChatProvider for FleetChatProvider {
    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ProviderEvent> {
        let mut req = req;
        req.model = self.model.clone();
        req.system = fleet_system(self.agents.as_deref(), req.system.take());
        self.inner.stream(req)
    }
}

/// A [`ChatProvider`] for a Chat route that cannot run (S6(c)): the turn
/// surfaces its message as the visible error — no spawn, no HTTP call, no
/// fallback.
pub(crate) struct UnavailableChatProvider {
    message: String,
}

impl UnavailableChatProvider {
    /// RC2-CNCL6: a council member that cannot run says why (no reroute).
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl ChatProvider for UnavailableChatProvider {
    fn stream(&self, _req: ChatRequest) -> BoxStream<'static, ProviderEvent> {
        use futures::StreamExt;
        futures::stream::iter(vec![
            ProviderEvent::Error(self.message.clone()),
            ProviderEvent::Done,
        ])
        .boxed()
    }
}

/// The outcome of the app's real config load, mapped onto BOTH the refusal
/// flag (P1) and the onboarding choice (P1.1): a hard parse `Err` (an
/// EXISTING file that will not parse) refuses Chat and keeps the user in
/// `existing` onboarding (they already have a config — it is just broken; the
/// startup arm surfaces an error banner, never fresh onboarding); a valid
/// `Loaded` config clears the refusal and uses `existing` onboarding (a
/// config exists); a `Missing` file (no config ever written) is NOT a load
/// error (RVS6-INT1 owner decision): the refusal flag stays CLEAR, it keeps
/// `Config::networkless()` (NOT the loader's `builtin` fallback) and uses
/// `fresh` onboarding (the install is new). A valid re-apply
/// ([`Self::apply_runtime_config`]) clears the flag. The `AwiliApp::new`
/// startup arm and the guard-proof tests all drive this same mapping, so
/// neither the flag nor the onboarding choice can be fabricated independently
/// of the load the app actually performed.
pub(crate) fn load_chat_config() -> (awili_config::Config, bool, awili_config::ConfigSource) {
    load_chat_config_from_paths(&awili_config::Config::config_paths())
}

/// The SAME mapping as [`load_chat_config`] over an EXPLICIT candidate list —
/// the testable seam the guard-proof tests drive: they write a malformed (or
/// valid) file to a temp path and load it through this exact function, so the
/// refusal flag they assert is the one the app's real load produces, never a
/// fabricated boolean. ONLY a hard load `Err` (an invalid EXISTING config on
/// disk) sets the flag, carrying the error's provenance (`source` is the
/// existing file — `Loaded(path)`). A `Missing` file (no config ever
/// written) is NOT a load error (RVS6-INT1): `networkless()`, flag CLEAR,
/// `Missing` provenance (fresh onboarding). A valid `Loaded` config clears
/// the flag.
pub(crate) fn load_chat_config_from_paths(
    candidates: &[std::path::PathBuf],
) -> (awili_config::Config, bool, awili_config::ConfigSource) {
    let outcome = awili_config::Config::load_from_paths(candidates);
    // RVS6C FIX3 (Step 1) — restore the 951e2f3 loader behaviour EXACTLY:
    // the 951e2f3 match arms keyed off the two `Ok` variants (the `Missing`
    // fallback and the first EXISTING candidate) and the hard-error `Err`
    // arm, so mirror that here: the `Missing` provenance is the "no file
    // found" outcome, the `Loaded(path)` provenance names the EXISTING
    // candidate the loader found (a hard parse `Err` on it too — see the
    // `Err` arm below), and the `networkless()` fallback is the config
    // startup keeps in BOTH no-file and malformed cases (the loader's own
    // `builtin` fallback is NOT the startup's config).
    let no_file = candidates
        .iter()
        .all(|p| match std::fs::symlink_metadata(p) {
            Ok(_) => false,
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        });
    match outcome {
        Ok((cfg, source)) => {
            if no_file {
                // RVS6-INT1: no candidate file exists — a fresh install, NOT
                // a load error (owner decision; the 951e2f3/pre-S6 startup
                // arm): keep `networkless()` (NOT the loader's `builtin()`),
                // CLEAR the flag (no banner, no Chat refusal), `Missing`
                // source (fresh onboarding). Only a hard `Err` below — an
                // EXISTING file that will not load — sets the flag.
                (
                    awili_config::Config::networkless(),
                    false,
                    awili_config::ConfigSource::Missing,
                )
            } else {
                // A valid file loaded from an EXISTING candidate: keep the
                // loaded config, clear the flag.
                (cfg, false, source)
            }
        }
        Err(e) => {
            // A hard load/parse `Err` (an invalid EXISTING config on disk)
            // is a load ERROR with its REAL provenance — the existing file
            // (`Loaded(path)`) and the parse error — and the refusal flag
            // is SET: the startup arm takes the existing-config recovery
            // path (error banner + `config_load_error`), NEVER fresh
            // onboarding (`Missing`). The `networkless` fallback is the
            // degraded route Chat runs on while refused (the flag, not the
            // config, refuses); the raw parse error is e-printed REDACTED
            // by the startup arm — never rendered to the user.
            let _ = &e;
            (
                awili_config::Config::networkless(),
                true,
                match candidates
                    .iter()
                    .find(|p| std::fs::symlink_metadata(p).is_ok())
                    .cloned()
                {
                    // A file was found and failed (parse or read): name it.
                    Some(path) => awili_config::ConfigSource::Loaded(path),
                    // Defensive: the loader hard-errored before finding a
                    // readable file (e.g. an unreadable candidate). The file
                    // is still EXISTING (not `Missing`) — the startup arm
                    // keys off `source != Missing` for onboarding and
                    // `chat_refused` for the banner — so the first candidate
                    // names where the error came from.
                    None => awili_config::ConfigSource::Missing,
                },
            )
        }
    }
}

/// The provider a Council member on the fleet route answers through: the
/// fleet's own endpoint, or the visible-error provider for a route that
/// cannot run. Neither arm resolves or spawns an agent binary.
pub(crate) fn chat_provider_for_route(
    route: &ChatRoute,
    agents: &awili_config::AgentBinsConfig,
    _cwd: String,
) -> Box<dyn ChatProvider> {
    match route {
        ChatRoute::Fleet(t) => Box::new(
            FleetChatProvider::new(t)
                .for_owner(agents.memory.as_ref(), agents.agents_md.as_deref()),
        ),
        ChatRoute::Unavailable(m) => Box::new(UnavailableChatProvider { message: m.clone() }),
    }
}

// --- RVS6C FIX3 (Step 1): the loader regression proof -----------------------

/// RVS6C FIX3 (Step 1), required proof — NO CONFIG FILE: loading through the
/// app's OWN mapping ([`load_chat_config_from_paths`]) with NO existing
/// candidate file keeps `Config::networkless()` for startup (NOT the loader's
/// `builtin()` fallback), leaves the refusal flag CLEAR (RVS6-INT1 owner
/// decision: a fresh install with no config file is NOT a load error — only
/// an EXISTING file that fails to load is), and reports the `Missing`
/// provenance (fresh onboarding territory — the startup arm derives
/// `Onboarding::fresh()` from it). A valid `Loaded` config clears the flag.
#[test]
fn missing_config_uses_networkless() {
    use awili_config::ConfigSource;
    let root = tempfile::tempdir().unwrap();
    // NO file exists at any candidate path.
    let candidates = vec![
        root.path().join("config.toml"),
        root.path().join("does-not-exist.toml"),
    ];
    let (cfg, refusal, source) = load_chat_config_from_paths(&candidates);
    assert!(
        matches!(source, ConfigSource::Missing),
        "no file on disk -> the provenance is `Missing` (fresh onboarding): {source:?}"
    );
    assert!(
        !refusal,
        "a missing file is NOT a load error (RVS6-INT1 owner decision): the flag stays CLEAR on the networkless runtime"
    );
    // The startup keeps the networkless fallback, NOT the loader's builtin.
    let networkless = awili_config::Config::networkless();
    assert!(
        cfg.chat.executor == networkless.chat.executor
            && cfg.work.agents == networkless.work.agents,
        "startup keeps `networkless()` (not the loader's `builtin()` fallback)"
    );
}

/// RVS6C FIX3 (Step 1), required proof — MALFORMED EXISTING CONFIG: loading a
/// broken-but-EXISTING file through the app's OWN mapping is a load ERROR with
/// the REAL provenance — the `Loaded(path)` source names the existing file and
/// the refusal flag is SET — so the startup arm takes the existing-config
/// recovery
/// path (error banner + `config_load_error`), never `Missing` and never fresh
/// onboarding. (Pre-FIX3 the `Err` arm dropped the provenance and reported
/// `Missing`, which sent a broken install into fresh onboarding.)
#[test]
fn malformed_config_is_load_error_not_missing() {
    use awili_config::{Config, ConfigSource};
    let root = tempfile::tempdir().unwrap();
    // A syntactically broken TOML (unclosed table + unterminated string):
    // the loader's `parse` rejects it.
    let path = root.path().join("config.toml");
    std::fs::write(&path, "[chat\nexecutor = \"agent:claude:haiku\n").unwrap();
    assert!(
        Config::load_from_paths(std::slice::from_ref(&path)).is_err(),
        "the fixture file must be malformed for the app's load"
    );
    let (cfg, refusal, source) =
        load_chat_config_from_paths(&[path.clone(), root.path().join("nope.toml")]);
    // The provenance is the LOAD ERROR, not `Missing`: the file EXISTS and its
    // parse failed — the startup arm keys the banner + the refusal flag off
    // exactly this, and fresh onboarding is off the table.
    assert!(
        matches!(source, ConfigSource::Loaded(ref p) if p == &path),
        "a malformed EXISTING file must map to a load ERROR with the file's provenance, never `Missing`: {source:?}"
    );
    assert!(
        refusal,
        "a malformed file sets the refusal flag (Chat off until it loads)"
    );
    // The degraded route is the networkless fallback.
    let networkless = Config::networkless();
    assert_eq!(
        cfg.chat.executor, networkless.chat.executor,
        "while refused, Chat runs on the `networkless` fallback"
    );
}
