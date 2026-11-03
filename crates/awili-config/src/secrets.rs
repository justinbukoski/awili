//! API keys in the operating system's secret store (macOS Keychain, Windows
//! Credential Manager) instead of plaintext in `config.toml`.
//!
//! A config value of the form `secret:<account>` is a REFERENCE: the real key
//! lives in the store under service [`SECRET_SERVICE`] and that account.
//! Plain values keep working exactly as before, and nothing here rewrites an
//! existing plaintext key unless [`migrate_plaintext_secrets`] is called.
//!
//! The store is a trait ([`SecretStore`]) so tests use [`MemorySecretStore`]
//! and never touch a real keychain. Account names are stable:
//! `provider:<name>`, `engine`, `engine:<id>`, `hermes`, `memory`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Mutex;

use crate::{Config, ConfigDoc, ConfigError, Target};

/// The service name every Awili secret is filed under.
pub const SECRET_SERVICE: &str = "dev.awili.Awili";

/// The prefix of a config value that points at a stored secret.
pub const SECRET_REF_PREFIX: &str = "secret:";

/// A secret-store failure. The text never contains a secret value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError(pub String);

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SecretError {}

/// Where secrets live. `get` of an absent account is `Ok(None)`, not an error.
pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, account: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

/// The `secret:<account>` reference for `account`.
pub fn secret_ref(account: &str) -> String {
    format!("{SECRET_REF_PREFIX}{account}")
}

/// The account named by a `secret:<account>` value, or `None` for a plain
/// value (or an empty account).
pub fn parse_secret_ref(value: &str) -> Option<&str> {
    value
        .trim()
        .strip_prefix(SECRET_REF_PREFIX)
        .filter(|a| !a.is_empty())
}

/// The account for a provider row: `provider:<name>`.
pub fn provider_account(name: &str) -> String {
    format!("provider:{name}")
}

/// The account for an extra engine: `engine:<id>`.
pub fn engine_account(id: &str) -> String {
    format!("engine:{id}")
}

/// A config value as the key to use: a `secret:<account>` reference is looked
/// up in `store`; anything else is returned unchanged. `None` means the value
/// IS a reference that could not be resolved (nothing stored, a locked store,
/// an unsupported platform) -- the caller must say so, never use the text.
pub fn try_resolve_value(value: &str, store: &dyn SecretStore) -> Option<String> {
    match parse_secret_ref(value) {
        None => Some(value.to_string()),
        Some(account) => match store.get(account) {
            Ok(Some(secret)) => Some(secret),
            Ok(None) => {
                eprintln!("awili: no stored secret for {account}");
                None
            }
            Err(e) => {
                eprintln!("awili: secret store unavailable for {account}: {e}");
                None
            }
        },
    }
}

/// [`try_resolve_value`] with an unresolved reference as the empty string
/// (no key, never the reference text).
pub fn resolve_value(value: &str, store: &dyn SecretStore) -> String {
    try_resolve_value(value, store).unwrap_or_default()
}

/// A `secret:` reference in the config that the store could not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedSecret {
    /// The store account the config points at.
    pub account: String,
    /// The line to show the user (names the provider or route, no value).
    pub notice: String,
}

/// What to write to the config for a key the user typed: empty stays empty, an
/// existing reference is kept, anything else is stored under `account` and
/// replaced by its reference. The plaintext is never returned.
pub fn store_typed_key(
    store: &dyn SecretStore,
    account: &str,
    typed: &str,
) -> Result<String, SecretError> {
    if typed.is_empty() || parse_secret_ref(typed).is_some() {
        return Ok(typed.to_string());
    }
    store.set(account, typed)?;
    Ok(secret_ref(account))
}

impl Config {
    /// Drop every API key written directly in the file (anything that is not a
    /// `secret:<account>` reference). Used when the file could not be made
    /// private to the account: a key in a file other accounts may read is not
    /// used. References are kept (they name the OS store, not a secret). A
    /// plaintext memory token also turns memory off, as an unresolved one does.
    /// Returns whether anything was dropped.
    pub fn scrub_plaintext_secrets(&mut self) -> bool {
        let mut dropped = false;
        let mut one = |value: &mut String| -> bool {
            if value.is_empty() || parse_secret_ref(value).is_some() {
                return true;
            }
            value.clear();
            dropped = true;
            false
        };
        for p in &mut self.providers {
            if let Some(key) = p.api_key.as_mut() {
                one(key);
            }
        }
        one(&mut self.hermes.api_key);
        one(&mut self.engine.api_key);
        for e in &mut self.engines {
            one(&mut e.api_key);
        }
        if !one(&mut self.memory.token) {
            self.memory.base_url.clear();
        }
        dropped
    }

    /// Replace every `secret:<account>` key with the stored secret, in memory.
    /// The result is for use only; it is never written back to disk.
    /// Returns every reference that could not be resolved (its key is left
    /// empty). An unresolved memory token also turns memory off, so the
    /// client cannot fall back to the standard boot-token file.
    pub fn resolve_secrets(&mut self, store: &dyn SecretStore) -> Vec<UnresolvedSecret> {
        let mut missed = Vec::new();
        let mut one = |value: &mut String, who: &str, tail: &str| -> bool {
            let Some(account) = parse_secret_ref(value).map(str::to_string) else {
                return true;
            };
            match try_resolve_value(value, store) {
                Some(secret) => {
                    *value = secret;
                    true
                }
                None => {
                    value.clear();
                    missed.push(UnresolvedSecret {
                        account,
                        notice: format!("{who} isn't in the system keychain{tail}"),
                    });
                    false
                }
            }
        };
        for p in &mut self.providers {
            if let Some(key) = p.api_key.as_mut() {
                one(key, &format!("API key for {}", p.name), "");
            }
        }
        one(&mut self.hermes.api_key, "API key for Hermes", "");
        one(&mut self.engine.api_key, "API key for the in-app model", "");
        for e in &mut self.engines {
            one(&mut e.api_key, &format!("API key for {}", e.id), "");
        }
        if !one(
            &mut self.memory.token,
            "Memory service key",
            "; memory is off",
        ) {
            self.memory.base_url.clear();
        }
        missed
    }
}

/// An in-memory store for tests. `Debug` never prints a value.
#[derive(Default)]
pub struct MemorySecretStore {
    items: Mutex<BTreeMap<String, String>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The accounts held, sorted.
    pub fn accounts(&self) -> Vec<String> {
        self.items.lock().unwrap().keys().cloned().collect()
    }
}

impl fmt::Debug for MemorySecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemorySecretStore")
            .field("accounts", &self.accounts())
            .finish()
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        Ok(self.items.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
        self.items
            .lock()
            .unwrap()
            .insert(account.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.items.lock().unwrap().remove(account);
        Ok(())
    }
}

/// The operating system's store: macOS Keychain or Windows Credential
/// Manager. On any other platform every call fails with a plain message.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsSecretStore;

/// The message for a store failure. On Linux a missing or locked Secret Service
/// is the usual cause, so say what to do (the key is never written to disk).
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn store_error(e: keyring::Error) -> SecretError {
    #[cfg(target_os = "linux")]
    if matches!(
        e,
        keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_)
    ) {
        return SecretError(format!(
            "the Secret Service (GNOME Keyring, KWallet or KeePassXC) is not available or is locked ({e}). Start and unlock one to keep API keys; nothing was saved to disk"
        ));
    }
    SecretError(e.to_string())
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
impl OsSecretStore {
    fn entry(account: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(SECRET_SERVICE, account).map_err(|e| SecretError(e.to_string()))
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
impl SecretStore for OsSecretStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        match Self::entry(account)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(store_error(e)),
        }
    }
    fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
        Self::entry(account)?
            .set_password(value)
            .map_err(store_error)
    }
    fn delete(&self, account: &str) -> Result<(), SecretError> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(store_error(e)),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
impl SecretStore for OsSecretStore {
    fn get(&self, _account: &str) -> Result<Option<String>, SecretError> {
        Err(unsupported())
    }
    fn set(&self, _account: &str, _value: &str) -> Result<(), SecretError> {
        Err(unsupported())
    }
    fn delete(&self, _account: &str) -> Result<(), SecretError> {
        Err(unsupported())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn unsupported() -> SecretError {
    SecretError("no secret store on this platform".to_string())
}

/// The accounts a migration moved (never their values).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub moved: Vec<String>,
}

/// Why a migration stopped. Keys already moved stay moved and are named.
#[derive(Debug)]
pub struct MigrationError {
    pub moved: Vec<String>,
    pub reason: String,
}

impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "secret migration stopped: {}", self.reason)
    }
}

impl std::error::Error for MigrationError {}

/// One-shot: move every plaintext key in `doc` into `store` and leave a
/// `secret:<account>` reference in its place. Already-referenced and empty
/// keys are skipped, so running it twice is harmless. Each key is stored
/// BEFORE its text is replaced, so a failure never loses a key. The caller
/// saves `doc`; nothing calls this at startup.
pub fn migrate_plaintext_secrets(
    doc: &mut ConfigDoc,
    store: &dyn SecretStore,
) -> Result<MigrationReport, MigrationError> {
    let cfg = doc.config();
    // (target, account, plaintext)
    let mut work: Vec<(Target, String, String)> = Vec::new();
    for (i, p) in cfg.providers.iter().enumerate() {
        if let Some(key) = p.api_key.as_deref() {
            work.push((
                Target::ArrayTable {
                    array: "providers".into(),
                    index: i,
                    key: "api_key".into(),
                },
                provider_account(&p.name),
                key.to_string(),
            ));
        }
    }
    let table = |table: &str, key: &str| Target::Table {
        table: table.into(),
        key: key.into(),
    };
    work.push((
        table("hermes", "api_key"),
        "hermes".into(),
        cfg.hermes.api_key,
    ));
    work.push((
        table("engine", "api_key"),
        "engine".into(),
        cfg.engine.api_key,
    ));
    work.push((table("memory", "token"), "memory".into(), cfg.memory.token));
    for (i, e) in cfg.engines.iter().enumerate() {
        work.push((
            Target::ArrayTable {
                array: "engines".into(),
                index: i,
                key: "api_key".into(),
            },
            engine_account(&e.id),
            e.api_key.clone(),
        ));
    }

    let mut report = MigrationReport::default();
    for (target, account, plain) in work {
        if plain.is_empty() || parse_secret_ref(&plain).is_some() {
            continue;
        }
        let stop = |reason: String, report: &MigrationReport| MigrationError {
            moved: report.moved.clone(),
            reason,
        };
        store
            .set(&account, &plain)
            .map_err(|e| stop(format!("could not store {account}: {e}"), &report))?;
        doc.set_string(&target, &secret_ref(&account))
            .map_err(|e: ConfigError| stop(format!("could not update {account}: {e}"), &report))?;
        report.moved.push(account);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Linux: with a Secret Service on the session bus the key round-trips; with
    /// none every call fails with a plain message that names it (never a silent
    /// success, never a write to disk). Passes in both environments.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_secret_service_round_trips_or_fails_plainly() {
        let store = OsSecretStore;
        let account = format!("awili-test-{}", std::process::id());
        match store.set(&account, "test-value") {
            Ok(()) => {
                assert_eq!(store.get(&account).unwrap().as_deref(), Some("test-value"));
                store.delete(&account).unwrap();
                assert_eq!(store.get(&account).unwrap(), None);
                eprintln!("secret-service: round trip OK");
            }
            Err(e) => {
                assert!(e.0.contains("Secret Service"), "{e}");
                eprintln!("secret-service: unavailable, plain error: {e}");
            }
        }
    }

    const CFG: &str = r#"
[memory]
base_url = "http://localhost:1"
token = "mem-plain"

[hermes]
api_key = "hermes-plain"

[engine]
base_url = "http://localhost:2"
api_key = "engine-plain"

[[engines]]
id = "big"
base_url = "http://localhost:3"
api_key = "big-plain"

[[providers]]
name = "openai"
kind = "openai"
base_url = "https://api.openai.com"
api_key = "sk-plain-1"

[[providers]]
name = "local"
kind = "ollama"
base_url = "http://localhost:11434"
"#;

    #[test]
    fn scrub_drops_plaintext_keys_and_keeps_references() {
        let mut cfg = Config::parse(CFG).unwrap();
        cfg.providers[0].api_key = Some("secret:provider:openai".to_string());
        assert!(cfg.scrub_plaintext_secrets());
        // The reference survives; every plaintext key is gone.
        assert_eq!(
            cfg.providers[0].api_key.as_deref(),
            Some("secret:provider:openai")
        );
        assert_eq!(cfg.hermes.api_key, "");
        assert_eq!(cfg.engine.api_key, "");
        assert_eq!(cfg.engines[0].api_key, "");
        // A plaintext memory token turns memory off, as an unresolved one does.
        assert_eq!(cfg.memory.token, "");
        assert_eq!(cfg.memory.base_url, "");
        // Nothing left to drop the second time.
        assert!(!cfg.scrub_plaintext_secrets());
    }

    #[test]
    fn scrub_leaves_a_config_without_plaintext_alone() {
        let mut cfg = Config::parse("[hermes]\napi_key = \"secret:hermes\"\n").unwrap();
        let before = cfg.clone();
        assert!(!cfg.scrub_plaintext_secrets());
        assert_eq!(cfg.hermes.api_key, before.hermes.api_key);
    }

    #[test]
    fn reference_syntax() {
        assert_eq!(secret_ref("provider:x"), "secret:provider:x");
        assert_eq!(parse_secret_ref("secret:hermes"), Some("hermes"));
        assert_eq!(parse_secret_ref("  secret:hermes "), Some("hermes"));
        assert_eq!(parse_secret_ref("secret:"), None);
        assert_eq!(parse_secret_ref("sk-abc"), None);
        assert_eq!(parse_secret_ref("keychain:hermes"), None);
    }

    #[test]
    fn plain_values_pass_through_and_refs_resolve() {
        let store = MemorySecretStore::new();
        store.set("hermes", "real").unwrap();
        assert_eq!(resolve_value("sk-plain", &store), "sk-plain");
        assert_eq!(resolve_value("secret:hermes", &store), "real");
        // A reference to nothing is no key, never the reference text.
        assert_eq!(resolve_value("secret:gone", &store), "");
    }

    #[test]
    fn config_resolves_every_reference_in_memory() {
        let store = MemorySecretStore::new();
        for (a, v) in [
            ("provider:openai", "k-openai"),
            ("hermes", "k-hermes"),
            ("engine", "k-engine"),
            ("engine:big", "k-big"),
            ("memory", "k-mem"),
        ] {
            store.set(a, v).unwrap();
        }
        let mut cfg = Config::parse(
            r#"
[memory]
base_url = "http://localhost:1"
token = "secret:memory"
[hermes]
api_key = "secret:hermes"
[engine]
api_key = "secret:engine"
[[engines]]
id = "big"
api_key = "secret:engine:big"
[[providers]]
name = "openai"
kind = "openai"
base_url = "https://api.openai.com"
api_key = "secret:provider:openai"
[[providers]]
name = "plain"
kind = "openai"
base_url = "https://x"
api_key = "still-plain"
"#,
        )
        .unwrap();
        cfg.resolve_secrets(&store);
        assert_eq!(cfg.providers[0].api_key.as_deref(), Some("k-openai"));
        assert_eq!(cfg.providers[1].api_key.as_deref(), Some("still-plain"));
        assert_eq!(cfg.hermes.api_key, "k-hermes");
        assert_eq!(cfg.engine.api_key, "k-engine");
        assert_eq!(cfg.engines[0].api_key, "k-big");
        assert_eq!(cfg.memory.token, "k-mem");
    }

    #[test]
    fn debug_output_never_shows_a_key_or_a_stored_value() {
        let store = MemorySecretStore::new();
        store.set("hermes", "TOP-SECRET-VALUE").unwrap();
        assert!(!format!("{store:?}").contains("TOP-SECRET-VALUE"));
        let mut cfg = Config::parse(CFG).unwrap();
        cfg.resolve_secrets(&store);
        let text = format!("{cfg:?}");
        for plain in [
            "sk-plain-1",
            "hermes-plain",
            "engine-plain",
            "mem-plain",
            "big-plain",
        ] {
            assert!(!text.contains(plain), "{plain} leaked: {text}");
        }
    }

    #[test]
    fn typed_key_goes_to_the_store_and_the_config_gets_a_reference() {
        let store = MemorySecretStore::new();
        let written = store_typed_key(&store, "provider:openai", "sk-typed").unwrap();
        assert_eq!(written, "secret:provider:openai");
        assert_eq!(
            store.get("provider:openai").unwrap().as_deref(),
            Some("sk-typed")
        );
        // Empty and already-a-reference values are left alone.
        assert_eq!(store_typed_key(&store, "x", "").unwrap(), "");
        assert_eq!(
            store_typed_key(&store, "x", "secret:y").unwrap(),
            "secret:y"
        );
        assert_eq!(store.accounts(), vec!["provider:openai"]);
    }

    #[test]
    fn migration_moves_plaintext_and_is_idempotent() {
        let store = MemorySecretStore::new();
        let mut doc = ConfigDoc::parse(CFG).unwrap();
        let report = migrate_plaintext_secrets(&mut doc, &store).unwrap();
        assert_eq!(
            report.moved,
            [
                "provider:openai",
                "hermes",
                "engine",
                "memory",
                "engine:big"
            ]
        );
        let text = doc.text();
        for plain in [
            "sk-plain-1",
            "hermes-plain",
            "engine-plain",
            "mem-plain",
            "big-plain",
        ] {
            assert!(!text.contains(plain), "{plain} still in config: {text}");
        }
        assert_eq!(
            store.get("provider:openai").unwrap().as_deref(),
            Some("sk-plain-1")
        );
        assert_eq!(
            store.get("engine:big").unwrap().as_deref(),
            Some("big-plain")
        );
        // The migrated config resolves back to the same keys.
        let mut cfg = doc.config();
        cfg.resolve_secrets(&store);
        assert_eq!(cfg.providers[0].api_key.as_deref(), Some("sk-plain-1"));
        assert_eq!(cfg.memory.token, "mem-plain");
        assert_eq!(
            cfg.providers[1].api_key, None,
            "a keyless provider stays keyless"
        );
        // A second run finds nothing to do.
        assert!(migrate_plaintext_secrets(&mut doc, &store)
            .unwrap()
            .moved
            .is_empty());
    }

    struct FailingStore;
    impl SecretStore for FailingStore {
        fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
            Err(SecretError("locked".into()))
        }
        fn set(&self, _: &str, _: &str) -> Result<(), SecretError> {
            Err(SecretError("locked".into()))
        }
        fn delete(&self, _: &str) -> Result<(), SecretError> {
            Err(SecretError("locked".into()))
        }
    }

    #[test]
    fn a_failing_store_never_loses_a_key() {
        let mut doc = ConfigDoc::parse(CFG).unwrap();
        let before = doc.text();
        let err = migrate_plaintext_secrets(&mut doc, &FailingStore).unwrap_err();
        assert!(err.moved.is_empty());
        assert!(!err.to_string().contains("sk-plain-1"));
        assert_eq!(doc.text(), before, "config untouched when the store fails");
        let mut cfg = Config::parse("[hermes]\napi_key = \"secret:hermes\"\n").unwrap();
        cfg.resolve_secrets(&FailingStore);
        assert_eq!(
            cfg.hermes.api_key, "",
            "unreadable store: no key, not the ref"
        );
    }

    #[test]
    fn unresolved_references_are_reported_by_name() {
        let store = MemorySecretStore::new();
        let mut cfg = Config::parse(
            r#"
[memory]
base_url = "http://localhost:1"
token = "secret:memory"
[hermes]
api_key = "secret:hermes"
[[providers]]
name = "openai"
kind = "openai"
base_url = "https://x"
api_key = "secret:provider:openai"
"#,
        )
        .unwrap();
        let missed = cfg.resolve_secrets(&store);
        let notices: Vec<_> = missed.iter().map(|m| m.notice.as_str()).collect();
        assert!(notices.contains(&"API key for openai isn't in the system keychain"));
        assert!(notices.contains(&"API key for Hermes isn't in the system keychain"));
        assert!(missed.iter().any(|m| m.account == "provider:openai"));
        assert_eq!(cfg.providers[0].api_key.as_deref(), Some(""));
        // A memory key that cannot be found turns memory off instead of
        // letting the client use the boot-token file.
        assert!(cfg.memory.base_url.is_empty());
        assert!(notices.iter().any(|n| n.starts_with("Memory service key")));
    }

    #[test]
    fn a_locked_store_is_reported_too() {
        let mut cfg = Config::parse("[hermes]\napi_key = \"secret:hermes\"\n").unwrap();
        assert_eq!(cfg.resolve_secrets(&FailingStore).len(), 1);
        // Plain keys and a fully resolved config report nothing.
        let mut plain = Config::parse("[hermes]\napi_key = \"abc\"\n").unwrap();
        assert!(plain.resolve_secrets(&FailingStore).is_empty());
    }
}
