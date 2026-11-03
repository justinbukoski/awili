//! Local, bounded candidates for the Codex model chooser.
//!
//! `models_cache.json` is a local client cache, not an availability contract:
//! entries can be stale or account-inapplicable. It deliberately supplements
//! (rather than replaces) the configured model/profile values until the app
//! server's asynchronous `model/list` discovery is available to this surface.

use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;

/// The vendor cache is normally about 400 KiB. Refuse unexpectedly large
/// files, and read at most this much even if the file changes while opening.
const MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CONFIG_BYTES: u64 = 128 * 1024;

/// One app-process read prevents chooser rendering from repeatedly reading or
/// parsing the vendor cache. Restarting Awili refreshes this candidate catalog.
static LOCAL_CODEX_CATALOG: LocalCatalog = LocalCatalog::new();

struct LocalCatalog {
    models: OnceLock<Vec<String>>,
}

impl LocalCatalog {
    const fn new() -> Self {
        Self {
            models: OnceLock::new(),
        }
    }

    fn get_or_load<F>(&self, load: F) -> &[String]
    where
        F: FnOnce() -> Vec<String>,
    {
        self.models.get_or_init(load).as_slice()
    }
}

#[derive(Deserialize, Default)]
struct CacheFile {
    #[serde(default)]
    models: Vec<CacheModel>,
}

#[derive(Deserialize, Default)]
struct CacheModel {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    visibility: String,
}

/// Return locally discovered Codex candidate ids. No process, network, OAuth,
/// or provider request occurs here.
pub(crate) fn model_candidates() -> Vec<String> {
    LOCAL_CODEX_CATALOG.get_or_load(load_from_home).to_vec()
}

fn load_from_home() -> Vec<String> {
    let Some(home) = crate::home::home_os() else {
        return Vec::new();
    };
    let codex = Path::new(&home).join(".codex");
    let configured = read_bounded(&codex.join("config.toml"), MAX_CONFIG_BYTES)
        .map(|bytes| configured_models(&bytes))
        .unwrap_or_default();
    let cached = read_bounded(&codex.join("models_cache.json"), MAX_CACHE_BYTES)
        .map(|bytes| visible_cache_models(&bytes))
        .unwrap_or_default();
    merge_models(configured, cached)
}

/// Reads no more than `limit + 1` bytes, including if the file grows after it
/// was opened. Cache/config read failures are intentionally non-fatal.
fn read_bounded(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    let mut limited = file.take(limit + 1);
    limited.read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

fn configured_models(bytes: &[u8]) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Vec::new();
    };
    let Ok(config) = text.parse::<toml::Value>() else {
        return Vec::new();
    };
    let mut models = Vec::new();
    let Some(root) = config.as_table() else {
        return models;
    };
    if let Some(model) = root.get("model").and_then(toml::Value::as_str) {
        push_unique(&mut models, model);
    }
    if let Some(profiles) = root.get("profiles").and_then(toml::Value::as_table) {
        // Keep the workspace TOML default: profile keys have deterministic
        // lexical table order. Root `model` remains the first configured choice.
        for profile in profiles.values() {
            if let Some(model) = profile
                .as_table()
                .and_then(|profile| profile.get("model"))
                .and_then(toml::Value::as_str)
            {
                push_unique(&mut models, model);
            }
        }
    }
    models
}

fn visible_cache_models(bytes: &[u8]) -> Vec<String> {
    let Ok(cache) = serde_json::from_slice::<CacheFile>(bytes) else {
        return Vec::new();
    };
    let mut models = Vec::new();
    for model in cache.models {
        // `list` is the cache's user-visible state; hidden rows must never
        // surface in the chooser even if their slugs are otherwise valid.
        if model.visibility == "list" {
            push_unique(&mut models, &model.slug);
        }
    }
    models
}

fn merge_models(configured: Vec<String>, cached: Vec<String>) -> Vec<String> {
    let mut models = Vec::new();
    for model in configured.into_iter().chain(cached) {
        push_unique(&mut models, &model);
    }
    models
}

fn push_unique(models: &mut Vec<String>, model: &str) {
    let model = model.trim();
    if !model.is_empty() && !models.iter().any(|known| known == model) {
        models.push(model.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn cache_exposes_only_visible_slugs_and_ignores_malformed_data() {
        let cache = br#"{
          "models": [
            {"slug":"gpt-visible", "display_name":"Visible", "visibility":"list"},
            {"slug":"gpt-hidden", "visibility":"hide"},
            {"slug":"  ", "visibility":"list"},
            {"slug":"gpt-visible", "visibility":"list"}
          ]
        }"#;
        assert_eq!(visible_cache_models(cache), vec!["gpt-visible"]);
        assert!(visible_cache_models(br#"{not json"#).is_empty());
    }

    #[test]
    fn configured_models_remain_fallback_and_deduplicate_cache_candidates() {
        let configured = configured_models(
            b"model = 'configured' # a valid TOML comment\n[profiles.z_first]\nmodel = \"profile-z\" # another comment\n[profiles.a_second]\nmodel = 'profile-a'\n[profiles.same]\nmodel = 'configured'\n[other]\nmodel = 'not-a-codex-choice'",
        );
        let cached = visible_cache_models(
            br#"{"models":[
              {"slug":"profile-a","visibility":"list"},
              {"slug":"catalog-only","visibility":"list"},
              {"slug":"hidden","visibility":"hide"}
            ]}"#,
        );
        // The source declares z_first before a_second, but the plain TOML table
        // has deterministic lexical profile-key order: a_second, same, z_first.
        assert_eq!(
            merge_models(configured, cached),
            vec!["configured", "profile-a", "profile-z", "catalog-only"]
        );
        assert_eq!(
            merge_models(
                configured_models(b"model = \"unterminated"),
                vec!["catalog-fallback".to_string()]
            ),
            vec!["catalog-fallback"],
            "a malformed config must fall back to valid cache candidates"
        );
    }

    #[test]
    fn local_catalog_runs_its_loader_only_once() {
        let catalog = LocalCatalog::new();
        let reads = AtomicUsize::new(0);
        assert_eq!(
            catalog.get_or_load(|| {
                reads.fetch_add(1, Ordering::SeqCst);
                vec!["once".to_string()]
            }),
            ["once".to_string()]
        );
        assert_eq!(
            catalog.get_or_load(|| {
                reads.fetch_add(1, Ordering::SeqCst);
                vec!["twice".to_string()]
            }),
            ["once".to_string()]
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }
}
