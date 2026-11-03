//! Local vendor CLI setup; discovery never launches a process or reads auth state.
use crate::work::{self, WorkExecutor};
use awili_code::CodeEngine;
use awili_config::AgentBinsConfig;

pub const RECIPES: [(CodeEngine, &str, &str); 4] = [
    (CodeEngine::Claude, "Claude Code", "agent:claude"),
    (CodeEngine::Codex, "Codex", "agent:codex"),
    (CodeEngine::Glm, "ZCode", "agent:glm"),
    (CodeEngine::Dsh, "DeepSeek Harness", "agent:dsh"),
];

pub const GUIDES: [(&str, &str); 4] = [
    ("Install the official Claude Code CLI. Run claude in Terminal and follow its sign-in prompts.", "https://code.claude.com/docs/en/quickstart"),
    ("Install the official Codex CLI. Run codex login in Terminal and complete the browser sign-in.", "https://learn.chatgpt.com/docs/auth"),
    ("Install the official ZCode app and configure its CLI launcher as zcode. Set up the model and sign-in or coding-plan key inside ZCode.", "https://zcode.z.ai/en/docs/install"),
    ("Install the dsh harness (DeepSeek Harness) on your PATH. Put your DeepSeek credentials in a key file as DEEPSEEK_API_KEY=… and set dsh_env_file to its path — Awili hands them to the child itself.", "https://www.npmjs.com/package/@deepseek-ai/dsh"),
];

pub fn installed_binary(engine: CodeEngine, cfg: &AgentBinsConfig) -> Option<String> {
    let resolved = work::resolve_agent_bin(engine.bin(), cfg);
    let path = std::path::Path::new(&resolved);
    if path.components().count() > 1 {
        return executable(path).then_some(resolved);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(&resolved))
            .find(|path| executable(path))
            .map(|path| path.to_string_lossy().into_owned())
    })
}

fn executable(path: &std::path::Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub fn configured_harness(cfg: &awili_config::Config) -> Option<WorkExecutor> {
    let executor = WorkExecutor::from_persist(cfg.work.default_executor.as_deref()?)?;
    if let WorkExecutor::Agent { engine, .. } = &executor {
        installed_binary(*engine, &cfg.work.agents)?;
        Some(executor)
    } else {
        None
    }
}

/// Whether a Work route other than a vendor CLI is set up: the in-app
/// `[engine]` (or an extra engine) with an address and a model, or the Hermes
/// gateway with a key or a dashboard address. Work turns never run on
/// `[[providers]]` rows (those serve Chat and the Council), so providers alone
/// are not a Work route. Config only: nothing is contacted, so "set up" is
/// all this can promise. The built-in loopback Hermes address does not count
/// without a key, since that server answers 401.
pub fn other_work_route(cfg: &awili_config::Config) -> bool {
    cfg.engine.is_configured()
        || cfg
            .engines
            .iter()
            .any(|e| e.to_engine_config().is_configured())
        || !cfg.gateway.dashboard_url.trim().is_empty()
        || (!cfg.hermes.base_url.trim().is_empty() && !cfg.hermes.api_key.trim().is_empty())
}

/// Whether Work can run at all: an installed vendor CLI that is the default
/// helper, or another route from [`other_work_route`]. This, not the presence
/// of `[[providers]]` rows, is what the welcome calls "Ready".
pub fn work_route_ready(cfg: &awili_config::Config) -> bool {
    configured_harness(cfg).is_some() || other_work_route(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Providers alone are never a Work route; each real route is.
    #[test]
    fn providers_alone_are_not_a_work_route() {
        let mut cfg = awili_config::Config::networkless();
        cfg.providers.push(awili_config::ProviderConfig {
            name: "openai".into(),
            kind: awili_config::ProviderKind::OpenAi,
            base_url: "https://api.openai.com".into(),
            models: vec!["gpt".into()],
            enabled: true,
            api_key: Some("k".into()),
        });
        assert!(!cfg.model_choices().is_empty(), "Council has a model");
        assert!(!work_route_ready(&cfg));

        let mut engine = cfg.clone();
        engine.engine.base_url = "http://127.0.0.1:8851".into();
        assert!(!work_route_ready(&engine), "an address without a model");
        engine.engine.model = "m".into();
        assert!(work_route_ready(&engine));

        let mut gateway = cfg.clone();
        gateway.gateway.dashboard_url = "http://127.0.0.1:60142".into();
        assert!(work_route_ready(&gateway));

        // The loopback Hermes default needs its key to count.
        let mut hermes = awili_config::Config::parse("").unwrap();
        assert!(!work_route_ready(&hermes));
        hermes.hermes.api_key = "k".into();
        assert!(work_route_ready(&hermes));
    }
    #[test]
    #[cfg(unix)]
    fn readiness_requires_selected_executable_not_auth_or_http_provider() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("vendor");
        std::fs::write(&bin, "#!/bin/sh\nexit 99\n").unwrap();
        let mut cfg = awili_config::Config::networkless();
        cfg.work.agents.claude_bin = Some(bin.display().to_string());
        cfg.work.default_executor = Some("agent:claude".into());
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(configured_harness(&cfg).is_none());
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(configured_harness(&cfg).is_some());
        assert!(cfg.model_choices().is_empty());
        cfg.work.default_executor = Some("gateway".into());
        assert!(configured_harness(&cfg).is_none());
        cfg.work.default_executor = Some("agent:claude".into());
        std::fs::remove_file(bin).unwrap();
        assert!(configured_harness(&cfg).is_none());
    }
}
