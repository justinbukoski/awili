//! Gateway pool — one WebSocket connection per profile.
//!
//! Each profile can target its own backend (different host/port). When the
//! user switches profiles, the pool lazily connects on first use and caches
//! the connection for reuse.

use std::collections::HashMap;

use awili_gateway::{GatewayClient, GatewayError};

/// Per-profile backend configuration.
///
/// RC2-APP (D-CHAIN #3): there is no built-in address. The default is empty,
/// and a profile with no configured address is refused (it used to fall
/// back to a fixed plaintext, tokenless LAN URL).
#[derive(Clone, Debug, Default)]
pub struct BackendConfig {
    // Read once the pool is wired into the connect path (Phase 4).
    #[allow(dead_code)]
    pub ws_url: String,
}

/// RC2-APP (D-CHAIN #3): the address a profile connects to — only one that
/// was configured for it; never a built-in default.
fn configured_url<'a>(
    configs: &'a HashMap<String, BackendConfig>,
    profile: &str,
) -> Result<&'a str, GatewayError> {
    configs
        .get(profile)
        .map(|c| c.ws_url.trim())
        .filter(|url| !url.is_empty())
        .ok_or_else(|| {
            GatewayError::Connect(format!("no gateway address is set for profile {profile:?}"))
        })
}

/// Manages gateway connections keyed by profile name.
#[allow(dead_code)] // wired into the app once the pool path ships (Phase 4)
pub struct GatewayPool {
    connections: HashMap<String, GatewayClient>,
    configs: HashMap<String, BackendConfig>,
}

// The pool's surface ships with the multi-backend connect path (Phase 4);
// `new` is what the app holds today.
#[allow(dead_code)]
impl GatewayPool {
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
            configs: HashMap::new(),
        }
    }

    /// Set the backend config for a profile. If a connection already exists
    /// and the URL changed, it will be dropped so the next `get_or_connect`
    /// creates a fresh one.
    pub fn set_config(&mut self, profile: &str, config: BackendConfig) {
        let url_changed = self
            .configs
            .get(profile)
            .is_none_or(|old| old.ws_url != config.ws_url);
        self.configs.insert(profile.to_string(), config);
        if url_changed {
            self.connections.remove(profile);
        }
    }

    /// Get the existing connection for a profile, or connect if needed.
    pub fn get_or_connect(&mut self, profile: &str) -> Result<&GatewayClient, GatewayError> {
        if !self.connections.contains_key(profile) {
            let client = GatewayClient::connect(configured_url(&self.configs, profile)?)?;
            self.connections.insert(profile.to_string(), client);
        }
        Ok(self.connections.get(profile).unwrap())
    }

    /// Get the existing connection for a profile, or connect if needed (mutable).
    pub fn get_or_connect_mut(
        &mut self,
        profile: &str,
    ) -> Result<&mut GatewayClient, GatewayError> {
        if !self.connections.contains_key(profile) {
            let client = GatewayClient::connect(configured_url(&self.configs, profile)?)?;
            self.connections.insert(profile.to_string(), client);
        }
        Ok(self.connections.get_mut(profile).unwrap())
    }

    /// Get an existing connection without connecting.
    pub fn get(&self, profile: &str) -> Option<&GatewayClient> {
        self.connections.get(profile)
    }

    /// Remove and drop a connection.
    pub fn disconnect(&mut self, profile: &str) {
        self.connections.remove(profile);
    }

    /// Disconnect all profiles.
    pub fn disconnect_all(&mut self) {
        self.connections.clear();
    }

    /// How many active connections exist.
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Profile names with active connections.
    pub fn connected_profiles(&self) -> Vec<String> {
        self.connections.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_config_drops_stale_connection() {
        let mut pool = GatewayPool::new();
        pool.set_config(
            "dev",
            BackendConfig {
                ws_url: "ws://a:1/ws".into(),
            },
        );
        assert_eq!(pool.configs.get("dev").unwrap().ws_url, "ws://a:1/ws");
        pool.set_config(
            "dev",
            BackendConfig {
                ws_url: "ws://b:2/ws".into(),
            },
        );
        assert_eq!(pool.configs.get("dev").unwrap().ws_url, "ws://b:2/ws");
        assert!(pool.get("dev").is_none());
    }

    #[test]
    fn disconnect_removes_entry() {
        let mut pool = GatewayPool::new();
        pool.disconnect("nonexistent"); // should not panic
        assert_eq!(pool.connection_count(), 0);
    }

    #[test]
    fn connected_profiles_empty() {
        let pool = GatewayPool::new();
        assert!(pool.connected_profiles().is_empty());
    }

    /// RC2-APP (D-CHAIN #3, deliberate change, name kept): the default no
    /// longer carries the LAN address — it carries none.
    #[test]
    fn default_config_uses_lan_url() {
        let config = BackendConfig::default();
        assert!(config.ws_url.is_empty(), "no built-in address");
        assert!(!config.ws_url.contains("10.77.68.86"));
    }

    /// RC2-APP (D-CHAIN #3): a profile with no address (none set, or an
    /// empty one) is refused before any connection is tried.
    #[test]
    fn rc2app_an_unconfigured_profile_is_refused() {
        let mut pool = GatewayPool::new();
        let refused = |r: Result<&GatewayClient, GatewayError>| match r {
            Err(GatewayError::Connect(m)) => m.contains("no gateway address"),
            _ => false,
        };
        assert!(refused(pool.get_or_connect("unset")));
        pool.set_config("blank", BackendConfig::default());
        assert!(refused(pool.get_or_connect("blank")));
        assert!(pool.get_or_connect_mut("unset").is_err());
        assert_eq!(pool.connection_count(), 0);
    }
}
