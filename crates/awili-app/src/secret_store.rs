//! The app's door to the OS secret store (macOS Keychain, Windows Credential
//! Manager). Config keys written as `secret:<account>` are resolved here at
//! load time; a key typed in Settings is stored here and only its reference
//! reaches `config.toml`. Tests get an in-memory store, never the real one.

use awili_config::secrets::{provider_account, store_typed_key, SecretError, SecretStore};
use awili_config::{Config, ProviderConfig};

/// The store for this build: the OS store, or (under test) a shared
/// in-memory fake (one per test thread) so a test can never touch the login
/// keychain.
pub(crate) fn store() -> &'static dyn SecretStore {
    #[cfg(test)]
    {
        // One store per test thread, so parallel tests never share a key.
        thread_local! {
            static FAKE: &'static awili_config::secrets::MemorySecretStore =
                Box::leak(Box::new(awili_config::secrets::MemorySecretStore::new()));
        }
        FAKE.with(|f| *f)
    }
    #[cfg(not(test))]
    {
        static OS: awili_config::OsSecretStore = awili_config::OsSecretStore;
        &OS
    }
}

/// A runtime copy of `cfg` with every `secret:<account>` key resolved, plus
/// one line for each reference the store could not resolve (shown to the
/// user). Config without references never touches the store.
pub(crate) fn resolved(cfg: &Config) -> (Config, Vec<String>) {
    resolved_with(cfg, awili_config::file_protection_problem())
}

/// [`resolved`] with the config-file protection problem given (Windows: the
/// file could not be made private to the account). That line leads the notices,
/// so the same banner that reports unresolved keys reports it; plaintext keys
/// were already dropped at load, and nothing here puts one back.
pub(crate) fn resolved_with(cfg: &Config, protection: Option<String>) -> (Config, Vec<String>) {
    let mut out = cfg.clone();
    let mut notices: Vec<String> = protection.into_iter().collect();
    notices.extend(out.resolve_secrets(store()).into_iter().map(|m| m.notice));
    (out, notices)
}

/// Before the provider list is saved: a key the user typed (one that differs
/// from what `current` already holds for that provider) goes to `store`, and
/// the row keeps only its `secret:<account>` reference. A key that is
/// unchanged stays exactly as it was, so saving never migrates an old
/// plaintext key behind the owner's back.
pub(crate) fn secure_typed_provider_keys(
    providers: &mut [ProviderConfig],
    current: &Config,
    store: &dyn SecretStore,
) -> Result<(), SecretError> {
    for p in providers {
        let Some(typed) = p.api_key.as_deref().filter(|k| !k.is_empty()) else {
            continue;
        };
        // Identical to a key already in the file (even under a renamed row)
        // is not "typed".
        let unchanged = current
            .providers
            .iter()
            .any(|c| c.api_key.as_deref() == Some(typed));
        if unchanged {
            continue;
        }
        let account = format!("{}:{}", provider_account(&p.name), uuid::Uuid::new_v4());
        let reference = store_typed_key(store, &account, typed)?;
        p.api_key = Some(reference);
    }
    Ok(())
}

/// The store accounts that `providers` point at with a `secret:` reference.
fn referenced_accounts(providers: &[ProviderConfig]) -> std::collections::BTreeSet<String> {
    providers
        .iter()
        .filter_map(|p| p.api_key.as_deref())
        .filter_map(awili_config::secrets::parse_secret_ref)
        .map(str::to_string)
        .collect()
}

/// Accounts the old provider list referenced that the new one no longer does
/// (a provider deleted, or its key cleared or replaced under another name).
pub(crate) fn orphaned_provider_accounts(
    before: &[ProviderConfig],
    after: &[ProviderConfig],
) -> Vec<String> {
    let keep = referenced_accounts(after);
    referenced_accounts(before)
        .into_iter()
        .filter(|a| !keep.contains(a))
        .collect()
}

/// Delete `accounts` from the store once the config that dropped them has
/// been written. A failure is logged (the entry is harmless, just stale).
pub(crate) fn delete_accounts(accounts: &[String], store: &dyn SecretStore) {
    for a in accounts {
        if let Err(e) = store.delete(a) {
            eprintln!("awili: could not remove stored key {a}: {e}");
        }
    }
}

/// The account a typed catalog secret is filed under, if it has one.
pub(crate) fn catalog_secret_account(item_id: &str) -> Option<&'static str> {
    match item_id {
        "memory.token" => Some("memory"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_config::secrets::MemorySecretStore;

    fn cfg(text: &str) -> Config {
        Config::parse(text).unwrap()
    }

    const ONE: &str = "[[providers]]\nname = \"openai\"\nkind = \"openai\"\nbase_url = \"https://x\"\napi_key = \"sk-old\"\n";

    #[test]
    fn a_config_file_that_is_not_private_leads_the_banner_notices() {
        let c = cfg(ONE);
        let (_, none) = resolved_with(&c, None);
        assert!(none.is_empty(), "{none:?}");
        let line = "config.toml could not be made private to your account (test)".to_string();
        let (out, notices) = resolved_with(&c, Some(line.clone()));
        assert_eq!(notices.first(), Some(&line), "{notices:?}");
        // The notice is added; it does not invent or restore keys.
        assert_eq!(out.providers[0].api_key.as_deref(), Some("sk-old"));
    }

    #[test]
    fn a_typed_key_is_stored_and_replaced_by_its_reference() {
        let store = MemorySecretStore::new();
        let current = cfg(ONE);
        let mut next = current.providers.clone();
        next[0].api_key = Some("sk-new".into());
        secure_typed_provider_keys(&mut next, &current, &store).unwrap();
        let account =
            awili_config::secrets::parse_secret_ref(next[0].api_key.as_deref().unwrap()).unwrap();
        assert!(account.starts_with("provider:openai:"));
        assert_eq!(store.get(account).unwrap().as_deref(), Some("sk-new"));
    }

    #[test]
    fn an_unchanged_plaintext_key_is_not_migrated_by_a_save() {
        let store = MemorySecretStore::new();
        let current = cfg(ONE);
        let mut next = current.providers.clone();
        secure_typed_provider_keys(&mut next, &current, &store).unwrap();
        assert_eq!(next[0].api_key.as_deref(), Some("sk-old"));
        assert!(store.accounts().is_empty());
    }

    #[test]
    fn a_new_provider_key_and_a_cleared_key() {
        let store = MemorySecretStore::new();
        let current = cfg(ONE);
        let mut next = current.providers.clone();
        next[0].api_key = None; // cleared
        let mut added = next[0].clone();
        added.name = "second".into();
        added.api_key = Some("sk-second".into());
        next.push(added);
        secure_typed_provider_keys(&mut next, &current, &store).unwrap();
        assert_eq!(next[0].api_key, None);
        let account =
            awili_config::secrets::parse_secret_ref(next[1].api_key.as_deref().unwrap()).unwrap();
        assert!(account.starts_with("provider:second:"));
        assert_eq!(store.accounts(), vec![account]);
    }

    #[test]
    fn removed_or_cleared_provider_keys_leave_the_store() {
        let store = MemorySecretStore::new();
        store.set("provider:a", "ka").unwrap();
        store.set("provider:b", "kb").unwrap();
        let before = cfg(
            "[[providers]]\nname = \"a\"\nkind = \"openai\"\nbase_url = \"https://x\"\napi_key = \"secret:provider:a\"\n\
             [[providers]]\nname = \"b\"\nkind = \"openai\"\nbase_url = \"https://y\"\napi_key = \"secret:provider:b\"\n",
        )
        .providers;
        // `a` is deleted, `b` keeps its key: only a's entry is orphaned.
        let orphans = orphaned_provider_accounts(&before, &before[1..]);
        assert_eq!(orphans, ["provider:a"]);
        delete_accounts(&orphans, &store);
        assert_eq!(store.accounts(), vec!["provider:b"]);
        // `b`'s key cleared.
        let mut cleared = before[1..].to_vec();
        cleared[0].api_key = None;
        assert_eq!(
            orphaned_provider_accounts(&before[1..], &cleared),
            ["provider:b"]
        );
        // Unchanged lists orphan nothing.
        assert!(orphaned_provider_accounts(&before, &before).is_empty());
    }

    #[test]
    fn the_test_build_uses_the_fake_store() {
        // A reference with nothing behind it resolves to no key.
        let (c, notices) = resolved(&cfg("[hermes]\napi_key = \"secret:no-such-account\"\n"));
        assert_eq!(c.hermes.api_key, "");
        assert_eq!(notices, ["API key for Hermes isn't in the system keychain"]);
    }
}
