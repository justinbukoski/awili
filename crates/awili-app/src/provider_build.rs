//! Provider routing and construction. Maps a provider kind to a concrete
//! [`ChatProvider`], attaching only the credential that belongs to that kind.
//! These are plain text-in/text-out transports (Council members, memory
//! distillation); they never open a tool-capable gateway session.

use awili_config::{Config, ModelChoice, ProviderKind};
use awili_providers::{AnthropicMessages, ChatProvider, OllamaChat, OpenAiCompat};

/// The provider credentials captured once at launch. Each key is bound to exactly
/// one provider kind — the hosted Anthropic key must NEVER reach a Hermes endpoint
/// (a user-controlled `base_url`), so they are carried in separate fields rather
/// than one shared "api_key" (review slice 2, blocker 1).
#[derive(Clone, Default)]
pub(crate) struct ProviderKeys {
    /// `AWILI_ANTHROPIC_KEY` — only attached to [`ProviderKind::Anthropic`] as
    /// the fallback when the choice's own provider has no configured key.
    pub(crate) anthropic: Option<String>,
    /// The Hermes-agent bearer, attached to [`ProviderKind::Hermes`] on the plain
    /// Chat (compat) path. Sourced from the `[hermes]` section's `api_key` (the
    /// documented hermes-agent credential, the same one Work mode and the working
    /// curl use), falling back to the `AWILI_HERMES_KEY` env var. Usually absent —
    /// a self-hosted box may need no credential at all.
    pub(crate) hermes: Option<String>,
    /// The engine-endpoint bearer, attached by the `[engine]` path's HTTP
    /// transport. Sourced from the `[engine]` section's `api_key` (bound to
    /// THAT endpoint only), falling back to the `AWILI_ENGINE_KEY` env var.
    /// Empty = send no Authorization header (a self-hosted vLLM box needs
    /// none).
    pub(crate) engine: Option<String>,
}

impl ProviderKeys {
    /// Read the keys from the environment, treating empty as absent.
    fn from_env() -> Self {
        let read = |name| std::env::var(name).ok().filter(|k: &String| !k.is_empty());
        Self {
            anthropic: read("AWILI_ANTHROPIC_KEY"),
            hermes: read("AWILI_HERMES_KEY"),
            engine: read("AWILI_ENGINE_KEY"),
        }
    }

    /// Like [`from_env`](Self::from_env), but the Hermes key prefers the
    /// `[hermes]` section's `api_key` (the credential the gateway/Work path and
    /// the owner's working curl already use) and only falls back to
    /// `AWILI_HERMES_KEY` when that is empty. This aligns the plain Chat path's
    /// credential with the documented one, closing the silent-401 trap where the
    /// key lived only in `[hermes]` but Chat read the env var.
    pub(crate) fn from_env_and_config(config: &Config) -> Self {
        let mut keys = Self::from_env();
        if !config.hermes.api_key.is_empty() {
            keys.hermes = Some(config.hermes.api_key.clone());
        }
        if !config.engine.api_key.is_empty() {
            keys.engine = Some(config.engine.api_key.clone());
        }
        keys
    }
}

/// The bearer for one Anthropic choice: the key CONFIGURED ON THAT CHOICE'S
/// PROVIDER (the settings panel's per-provider key row) when set and
/// non-empty — so a configured key stays scoped to its own choice and two
/// same-kind choices never share one — else the legacy `AWILI_ANTHROPIC_KEY`
/// fallback carried in [`ProviderKeys::anthropic`], so an unconfigured provider
/// still works the way it did before. Pure over its inputs (no env read) so the
/// precedence is unit-testable with synthetic keys; [`build_provider`] routes
/// its Anthropic arm through this.
fn anthropic_api_key(keys: &ProviderKeys, choice: &ModelChoice) -> Option<String> {
    choice
        .api_key
        .as_ref()
        .filter(|k| !k.is_empty())
        .cloned()
        .or_else(|| keys.anthropic.clone())
}

/// Which concrete provider implementation a provider kind maps to. Split out
/// from [`build_provider`] so the routing is unit-testable without
/// constructing trait objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRoute {
    Ollama,
    Anthropic,
    /// Plain OpenAI-compatible text-in/text-out Hermes: hermes-agent's REST
    /// surface at `:8642/v1/chat/completions` (Bearer auth), driven through
    /// [`OpenAiCompat`]. No tools, no approvals, no gateway session. (Renamed
    /// from `HermesCompat` — the old name carried an "Anthropic-wire" doc that
    /// mis-routed Chat to `/v1/messages` and 404'd; hermes-agent speaks OpenAI.)
    HermesOpenAi,
    OpenAi,
}

/// Pure routing decision — see [`ProviderRoute`].
pub(crate) fn provider_route(kind: ProviderKind) -> ProviderRoute {
    match kind {
        ProviderKind::Ollama => ProviderRoute::Ollama,
        ProviderKind::Anthropic => ProviderRoute::Anthropic,
        // A bare OpenAI-shaped endpoint (vLLM/DeepSeek/etc.) — routed to its own
        // base_url with its own configured key, NOT through the Hermes gateway.
        ProviderKind::OpenAi => ProviderRoute::OpenAi,
        ProviderKind::Hermes => ProviderRoute::HermesOpenAi,
    }
}

/// Build the concrete provider for a model choice, attaching only the credential
/// that belongs to that provider kind (see [`provider_route`]).
pub(crate) fn build_provider(
    choice: &ModelChoice,
    keys: &ProviderKeys,
    hermes_base_url: &str,
) -> Box<dyn ChatProvider> {
    match provider_route(choice.kind) {
        // Every route that takes a user-configured base_url is guarded by host
        // (see `LemonadeGuarded`): a LAN/loopback/tailnet host that is Lemonade
        // gets the residency check, a public cloud host is never probed.
        ProviderRoute::Ollama => Box::new(LemonadeGuarded {
            inner: std::sync::Arc::new(OllamaChat::new(choice.base_url.clone())),
            base_url: choice.base_url.clone(),
            api_key: choice.api_key.clone(),
        }),
        ProviderRoute::Anthropic => Box::new(LemonadeGuarded {
            // The choice's own configured key wins over the env fallback
            // (`anthropic_api_key` is the pure selection seam tests drive).
            inner: std::sync::Arc::new(
                AnthropicMessages::new(choice.base_url.clone())
                    .with_api_key(anthropic_api_key(keys, choice)),
            ),
            base_url: choice.base_url.clone(),
            api_key: anthropic_api_key(keys, choice),
        }),
        // Plain Chat against hermes-agent's OpenAI-compatible REST surface. Target
        // the `[hermes]` section's base_url + key (not the often-empty `[[providers]]`
        // row), so this is byte-for-byte the owner's working curl: POST
        // `…/v1/chat/completions` with `Authorization: Bearer <hermes key>`.
        // `OpenAiCompat` normalises the trailing `/v1`, so no double-`/v1` 404.
        ProviderRoute::HermesOpenAi => Box::new(
            OpenAiCompat::new(hermes_base_url.to_string()).with_api_key(keys.hermes.clone()),
        ),
        // A user-configured OpenAI-compatible endpoint can be a Lemonade host:
        // its request passes the same residency check as an in-app turn.
        ProviderRoute::OpenAi => Box::new(LemonadeGuarded {
            inner: std::sync::Arc::new(
                OpenAiCompat::new(choice.base_url.clone()).with_api_key(choice.api_key.clone()),
            ),
            base_url: choice.base_url.clone(),
            api_key: choice.api_key.clone(),
        }),
    }
}

/// SAFETY (owner rule: nothing Awili sends may make a Lemonade host load a
/// model): a provider whose endpoint might be Lemonade reads the host's
/// `/api/v1/health` (read-only) on its own worker thread before the request.
/// A host without the model resident, or one that gives no answer, gets no
/// request: the stream is one Error and Done.
pub(crate) struct LemonadeGuarded {
    pub(crate) inner: std::sync::Arc<dyn ChatProvider>,
    pub(crate) base_url: String,
    /// The bearer the provider sends, also sent on the health read.
    pub(crate) api_key: Option<String>,
}

impl ChatProvider for LemonadeGuarded {
    fn stream(
        &self,
        req: awili_providers::ChatRequest,
    ) -> futures::stream::BoxStream<'static, awili_providers::ProviderEvent> {
        use awili_providers::ProviderEvent;
        use futures::StreamExt;
        if self.base_url.trim().is_empty() {
            return self.inner.stream(req);
        }
        let engine = awili_config::EngineConfig {
            kind: awili_config::EngineKind::Awili,
            base_url: self.base_url.clone(),
            model: req.model.clone(),
            api_key: self.api_key.clone().unwrap_or_default(),
            ..Default::default()
        };
        let refuse = |card: String| {
            futures::stream::iter(vec![ProviderEvent::Error(card), ProviderEvent::Done]).boxed()
        };
        // Tests read the host inline; the app reads it on a worker thread and
        // only then starts the request, so nothing blocks the caller.
        #[cfg(test)]
        {
            // Only a host that is (or resolves to) this machine or the home
            // network is watched; a public cloud API is left alone.
            if !crate::strip::provider_host_guarded(&engine.base_url, crate::strip::guard_timeout())
            {
                return self.inner.stream(req);
            }
            match crate::AwiliApp::lemonade_refusal(&engine, crate::strip::guard_timeout()) {
                Some((card, _)) => refuse(card),
                None => self.inner.stream(req),
            }
        }
        #[cfg(not(test))]
        {
            let (tx, rx) = futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                // Name resolution and the host read both happen here, never on
                // the caller's thread.
                let refusal = if crate::strip::provider_host_guarded(
                    &engine.base_url,
                    crate::strip::guard_timeout(),
                ) {
                    crate::AwiliApp::lemonade_refusal(&engine, crate::strip::guard_timeout())
                } else {
                    None
                };
                let _ = tx.send(refusal);
            });
            let inner = self.inner.clone();
            futures::stream::once(async move {
                match rx.await {
                    Ok(None) => inner.stream(req),
                    Ok(Some((card, _))) => refuse(card),
                    // The check itself died: closed.
                    Err(_) => {
                        refuse("The model host could not be checked, so nothing was sent.".into())
                    }
                }
            })
            .flatten()
            .boxed()
        }
    }

    fn memory_from_hooks(&self) -> bool {
        self.inner.memory_from_hooks()
    }
}

#[cfg(test)]
mod tests {
    use super::{anthropic_api_key, provider_route, ProviderKeys, ProviderRoute};
    use awili_config::{Config, ModelChoice, ProviderKind};

    /// Silent-401-trap regression guard. The documented fix is that the `[hermes]`
    /// section's `api_key` takes precedence over the `AWILI_HERMES_KEY` env var
    /// (the trap was: the key lived only in `[hermes]` but Chat read the env var,
    /// so an empty env produced a silent 401). This asserts BOTH halves of that
    /// contract: a non-empty config key overrides a *conflicting* env value, and
    /// an empty config key falls back to the env value.
    ///
    /// This test explicitly sets `AWILI_HERMES_KEY` (rather than relying on ambient
    /// env) so the override is proven against a real conflicting value, not merely
    /// against an empty env. Both assertions run in one test to keep the shared,
    /// process-global env var confined to a single test body — `from_env` is the
    /// only reader of `AWILI_HERMES_KEY` and `from_env_and_config` its only caller,
    /// so no other test races on it. The var is removed at the end.
    #[test]
    fn config_hermes_key_overrides_env() {
        // Snapshot any ambient value so this test restores it rather than
        // destroying it for the rest of the process.
        let restore = std::env::var_os("AWILI_HERMES_KEY");
        std::env::set_var("AWILI_HERMES_KEY", "env-key-should-lose");

        // Non-empty config key wins over the conflicting env value.
        let mut config = Config::default();
        config.hermes.api_key = "cfg-hermes-key-sentinel-9c3f".to_string();
        let keys = ProviderKeys::from_env_and_config(&config);
        assert_eq!(keys.hermes.as_deref(), Some("cfg-hermes-key-sentinel-9c3f"));

        // Empty config key falls back to the env value.
        config.hermes.api_key = String::new();
        let keys = ProviderKeys::from_env_and_config(&config);
        assert_eq!(keys.hermes.as_deref(), Some("env-key-should-lose"));

        match restore {
            Some(v) => std::env::set_var("AWILI_HERMES_KEY", v),
            None => std::env::remove_var("AWILI_HERMES_KEY"),
        }
    }

    /// Engine-key precedence, mirroring the hermes contract above: a non-empty
    /// `[engine].api_key` overrides a conflicting `AWILI_ENGINE_KEY` env value;
    /// an empty config key falls back to the env value. Same env-snapshot
    /// discipline as the hermes test (single body, restored at the end) — the
    /// var is confined to one test because `from_env` is the only reader of
    /// `AWILI_ENGINE_KEY`. Values are synthetic; no real key.
    #[test]
    fn config_engine_key_overrides_env() {
        let restore = std::env::var_os("AWILI_ENGINE_KEY");
        std::env::set_var("AWILI_ENGINE_KEY", "env-engine-key-should-lose");

        // Non-empty config key wins over the conflicting env value.
        let mut config = Config::default();
        config.engine.api_key = "sk-test-000".to_string();
        let keys = ProviderKeys::from_env_and_config(&config);
        assert_eq!(keys.engine.as_deref(), Some("sk-test-000"));

        // Empty config key falls back to the env value.
        config.engine.api_key = String::new();
        let keys = ProviderKeys::from_env_and_config(&config);
        assert_eq!(keys.engine.as_deref(), Some("env-engine-key-should-lose"));

        match restore {
            Some(v) => std::env::set_var("AWILI_ENGINE_KEY", v),
            None => std::env::remove_var("AWILI_ENGINE_KEY"),
        }
    }

    /// Synthetic Anthropic choice; `key` is the configured per-provider key.
    fn anthropic_choice(key: Option<&str>) -> ModelChoice {
        ModelChoice {
            provider: "claude-pro".to_string(),
            kind: ProviderKind::Anthropic,
            base_url: "https://api.anthropic.com".to_string(),
            model: "claude-sonnet-4-5".to_string(),
            api_key: key.map(str::to_string),
        }
    }

    /// `ProviderKeys` built from explicit synthetic values — no process-env
    /// mutation, so the precedence is deterministic no matter what ambient env
    /// the test process carries.
    fn synthetic_keys(anthropic: Option<&str>, hermes: Option<&str>) -> ProviderKeys {
        ProviderKeys {
            anthropic: anthropic.map(str::to_string),
            hermes: hermes.map(str::to_string),
            engine: None,
        }
    }

    /// Pure hermes-credential selection (env value already resolved), mirroring
    /// [`ProviderKeys::from_env_and_config`]: `[hermes].api_key` when set and
    /// non-empty, else the env-derived value. No process-env read, so hermes
    /// precedence is testable with synthetic keys.
    fn hermes_key(keys: &ProviderKeys, config: &Config) -> Option<String> {
        if !config.hermes.api_key.is_empty() {
            Some(config.hermes.api_key.clone())
        } else {
            keys.hermes.clone()
        }
    }

    /// The Anthropic selection seam: a configured key on the choice's own
    /// provider wins over the `AWILI_ANTHROPIC_KEY` fallback; an unset OR
    /// empty-string configured key falls back to the env-sourced value; with
    /// neither present the provider gets no key at all. (Synthetic — the seam
    /// is a pure function of its inputs, no network, no env read.)
    #[test]
    fn anthropic_configured_key_beats_env_fallback() {
        let keys = synthetic_keys(Some("env-anthropic-key"), None);
        assert_eq!(
            anthropic_api_key(&keys, &anthropic_choice(Some("cfg-anthropic-key"))),
            Some("cfg-anthropic-key".to_string())
        );
    }

    /// Absent and empty-string configured keys both fall back to the env key —
    /// the explicit-clear (None) and blank-edit cases must not break the
    /// pre-existing `AWILI_ANTHROPIC_KEY` behavior.
    #[test]
    fn anthropic_absent_and_empty_configured_key_fall_back_to_env() {
        let keys = synthetic_keys(Some("env-anthropic-key"), None);
        assert_eq!(
            anthropic_api_key(&keys, &anthropic_choice(None)),
            Some("env-anthropic-key".to_string())
        );
        assert_eq!(
            anthropic_api_key(&keys, &anthropic_choice(Some(""))),
            Some("env-anthropic-key".to_string())
        );
    }

    /// With no configured key and no env key the Anthropic choice gets no
    /// bearer — same as before the key row existed.
    #[test]
    fn anthropic_no_configured_key_and_no_env_means_none() {
        let keys = synthetic_keys(None, None);
        assert_eq!(anthropic_api_key(&keys, &anthropic_choice(None)), None);
    }

    /// Configured keys are scoped to their OWN choice: two same-kind Anthropic
    /// providers never share a configured key. Choice A's key must not leak
    /// into choice B (which has none) — B falls back to the env key, not A's.
    #[test]
    fn anthropic_configured_keys_are_choice_scoped() {
        let keys = synthetic_keys(Some("env-anthropic-key"), None);
        let a = anthropic_choice(Some("key-of-provider-a"));
        let b = anthropic_choice(None);
        assert_eq!(
            anthropic_api_key(&keys, &a),
            Some("key-of-provider-a".to_string())
        );
        assert_eq!(
            anthropic_api_key(&keys, &b),
            Some("env-anthropic-key".to_string()),
            "provider B must get the env fallback, never A's configured key"
        );
    }

    /// Hermes routing stays independent of per-provider key rows: Hermes
    /// routes to the plain OpenAI-compat surface, and Hermes credentialing
    /// keeps its own documented precedence — `[hermes].api_key` over the
    /// `AWILI_HERMES_KEY` env var — untouched by the Anthropic seam (which
    /// never touches `keys.hermes`).
    #[test]
    fn hermes_isolation_from_provider_key_rows() {
        assert_eq!(
            provider_route(ProviderKind::Hermes),
            ProviderRoute::HermesOpenAi
        );
        let keys = synthetic_keys(Some("env-anthropic-key"), Some("env-hermes-key"));
        let mut config = Config::default();
        config.hermes.api_key = "cfg-hermes-key".to_string();
        assert_eq!(
            hermes_key(&keys, &config),
            Some("cfg-hermes-key".to_string()),
            "hermes precedence must remain config-over-env; the seam never touches it"
        );
        config.hermes.api_key = String::new();
        assert_eq!(
            hermes_key(&keys, &config),
            Some("env-hermes-key".to_string()),
            "empty [hermes].api_key must still fall back to the env value"
        );
    }
}
