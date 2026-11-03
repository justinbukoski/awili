//! P12 connectors-as-tools: the synthesised `awili-connectors` MCP server
//! entry.
//!
//! When either connector family is enabled in config, the app offers the
//! P11 connector clients to the executors through ONE shared server binary
//! (`awili-connectors-mcp`, see that crate): the owned engine gets it as a
//! regular `[mcp]`-shaped server entry (work.rs composes it into the launch
//! catalog), and the vendor CLIs get it through their own MCP mechanisms
//! (claude `--mcp-config`, codex `-c` — see `code_provider`).
//!
//! The entry carries only the binary path and the config path — never a
//! secret. The server reads the same config file the app loaded and resolves
//! credentials at call time.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use awili_config::{
    AgentBinsConfig, Config, GiteaConfig, McpMode, McpServerConfig, McpTrust, OutlineConfig,
};

/// The synthesised server name. A user `[[mcp.servers]]` entry under the same
/// name WINS (the synthesised entry is deduped away) — this name is not
/// reserved.
pub const CONNECTORS_SERVER_NAME: &str = "awili-connectors";
/// The binary name the app looks for (the `awili-connectors-mcp` package).
pub const CONNECTORS_BIN: &str = "awili-connectors-mcp";

/// The two connector sections the app carries for the P12 wiring (the
/// enabled families drive the synthesised server entry, the vendor-CLI
/// flags, and the Work panel label).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectorsSections {
    /// The `[outline]` section.
    pub outline: OutlineConfig,
    /// The `[gitea]` section.
    pub gitea: GiteaConfig,
}

impl From<&Config> for ConnectorsSections {
    fn from(cfg: &Config) -> Self {
        Self {
            outline: cfg.outline.clone(),
            gitea: cfg.gitea.clone(),
        }
    }
}

impl ConnectorsSections {
    /// The synthesised `awili-connectors` server entry for THESE sections
    /// (the spawn sites hold the sections, not a full `Config`). `None` when
    /// no family is enabled or the binary cannot be resolved.
    pub fn server_config(&self, agents: &AgentBinsConfig) -> Option<McpServerConfig> {
        server_config_for(&self.outline, &self.gitea, agents)
    }

    /// The enabled families in display order (`outline` then `gitea`) — the
    /// Work panel label ("Connectors: outline, gitea") and the server's
    /// `--list-tools` agree on this order.
    pub fn active_families(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.outline.enabled && !self.outline.base_url.trim().is_empty() {
            out.push("outline");
        }
        if self.gitea.enabled && !self.gitea.base_url.trim().is_empty() {
            out.push("gitea");
        }
        out
    }
}

/// The config path the server should read: the first EXISTING candidate
/// (the file the app actually loaded — `load_from_paths` picks the same
/// first-existing-wins rule), else the default candidate. `None` when no
/// candidate can be computed at all (no home dir): the server then relies on
/// its own default.
pub fn config_path() -> Option<PathBuf> {
    let paths = Config::config_paths();
    paths
        .iter()
        .find(|p| p.exists())
        .cloned()
        .or_else(|| paths.first().cloned())
}

/// Resolve the `awili-connectors-mcp` binary the same way the vendor CLIs
/// are resolved: the running executable's own directory first, then the
/// configured agent search paths. `None` when no file exists anywhere — a
/// bare name would be a RELATIVE program, which the owned-engine catalog
/// refuses (`StdioServerSpec::new`), so the entry is omitted rather than
/// shipped broken.
pub fn resolve_binary(agents: &AgentBinsConfig) -> Option<String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join(CONNECTORS_BIN);
            if cand.is_file() {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    for dir in &agents.search_paths {
        let candidate = format!(
            "{}/{CONNECTORS_BIN}",
            crate::work::expand_home(dir).trim_end_matches('/')
        );
        if Path::new(&candidate).is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The synthesised `awili-connectors` MCP server entry for a full config
/// (spec C7). `None` when no family is enabled or the binary cannot be
/// resolved.
pub fn server_config(config: &Config) -> Option<McpServerConfig> {
    server_config_for(&config.outline, &config.gitea, &config.work.agents)
}

/// The section-level core (the spawn sites hold the sections, not a full
/// `Config`): `None` when no family is enabled or the binary cannot be
/// resolved. `trust` is the least-privileged existing value (`Prompt` —
/// `Install` is reserved for `mac-shell`); `modes` is `[Work, Code]` — the
/// same "a work tool and a code tool, not a chat tool" scope as the
/// reserved server (the Chat surface deliberately gets no connector tools).
pub fn server_config_for(
    outline: &OutlineConfig,
    gitea: &GiteaConfig,
    agents: &AgentBinsConfig,
) -> Option<McpServerConfig> {
    // A family with no address has nothing to offer: no entry for it.
    let active = |enabled: bool, base_url: &str| enabled && !base_url.trim().is_empty();
    if !active(outline.enabled, &outline.base_url) && !active(gitea.enabled, &gitea.base_url) {
        return None;
    }
    let command = resolve_binary(agents)?;
    let mut args = Vec::new();
    if let Some(path) = config_path() {
        args.push("--config".to_string());
        args.push(path.to_string_lossy().into_owned());
    }
    Some(McpServerConfig {
        name: CONNECTORS_SERVER_NAME.to_string(),
        command,
        args,
        env: Default::default(),
        trust: McpTrust::Prompt,
        modes: vec![McpMode::Work, McpMode::Code],
        enabled: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C7 proof: with both families disabled (the defaults) there is no
    /// synthesised entry at all.
    #[test]
    fn connectors_server_config_absent_when_disabled() {
        let cfg = awili_config::Config::parse("").expect("empty config parses");
        assert!(!cfg.outline.enabled && !cfg.gitea.enabled);
        assert!(
            server_config(&cfg).is_none(),
            "no enabled family ⇒ no synthesised server entry"
        );
    }

    /// C7 proof: with a family enabled the entry is synthesised, the binary
    /// is resolved from a TEMP SEARCH PATH (never hard-coded), and the entry
    /// carries only the binary path + the config path (no secrets).
    #[test]
    fn connectors_server_config_synthesised_when_enabled() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bin = dir.path().join(CONNECTORS_BIN);
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").expect("fake binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }

        let mut cfg = awili_config::Config::parse("").expect("empty config parses");
        cfg.outline.enabled = true; // gitea stays disabled
        cfg.outline.base_url = "https://outline.example.test/api".to_string();
        cfg.work.agents.search_paths = vec![dir.path().to_string_lossy().into_owned()];

        let entry = server_config(&cfg).expect("an enabled family ⇒ a synthesised entry");
        assert_eq!(entry.name, CONNECTORS_SERVER_NAME);
        assert_eq!(
            entry.command,
            bin.to_string_lossy().into_owned(),
            "the binary must resolve from the temp search path"
        );
        // The args carry ONLY the config path (never a secret); a home dir
        // exists on every sane machine, so the path is present and absolute.
        assert_eq!(entry.args.len(), 2);
        assert_eq!(entry.args[0], "--config");
        assert!(
            Path::new(&entry.args[1]).is_absolute(),
            "the --config path must be absolute: {}",
            entry.args[1]
        );
        assert!(entry.enabled);
        assert_eq!(entry.trust, McpTrust::Prompt, "least-privileged trust");
        assert_eq!(entry.modes, vec![McpMode::Work, McpMode::Code]);
        assert!(entry.env.is_empty());
    }

    /// The label helper: display order and the empty case.
    #[test]
    fn active_families_order_and_empty() {
        let none = ConnectorsSections::default();
        assert!(none.active_families().is_empty());
        let mut both = ConnectorsSections::default();
        both.outline.enabled = true;
        both.gitea.enabled = true;
        // Enabled but with no address: nothing to show.
        assert!(both.active_families().is_empty());
        both.outline.base_url = "https://o.example.test/api".to_string();
        both.gitea.base_url = "https://g.example.test/api/v1".to_string();
        assert_eq!(both.active_families(), vec!["outline", "gitea"]);
        both.outline.enabled = false;
        assert_eq!(both.active_families(), vec!["gitea"]);
    }
}
