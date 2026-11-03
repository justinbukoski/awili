//! SX: live apply for the Settings catalog (design:
//! the settings design notes). One click changes one
//! setting and saves at once; Reset puts one setting back to its built-in
//! default; Undo walks back the last change.
//!
//! Every write goes through the existing [`SettingsState::commit_raw`] gate
//! (full validation, `.bak`, atomic write, first-run create-only), so the
//! config file stays the one source of truth and Raw TOML keeps working.
//! Reset removes the key. Undo puts back the exact bytes from before the
//! change, and refuses when the file changed since (a hand edit, Raw TOML,
//! a provider save), so it can never clobber other work.
use awili_config::ConfigDoc;
use awili_voice::VoiceConfig;

use crate::settings::{ProviderDraft, SettingsState};
use crate::settings_catalog::{find, Control, Edit, Item};

/// One undoable change: the file text before and after it.
#[derive(Clone, Debug, PartialEq)]
pub struct UndoStep {
    pub label: String,
    pub before: String,
    pub after: String,
}

/// How many changes Undo can walk back.
pub const UNDO_LIMIT: usize = 20;

/// Shown when Undo meets a file that changed since the change.
pub const UNDO_STALE: &str = "Undo is not possible: the settings file changed since that change.";

impl SettingsState {
    /// Live apply: set one catalog item and save at once. `false` (with
    /// `error` set) when the value is refused; the file is then untouched.
    pub fn apply_item(&mut self, id: &str, edit: Edit) -> bool {
        let (Some(item), Some(doc)) = (find(id), self.doc.as_ref()) else {
            return false;
        };
        let mut next = doc.clone();
        let target = item.target();
        // A typed key goes to the OS secret store; the file keeps only the
        // `secret:` reference. Emptying the field clears it as before.
        // Clearing a key that was a reference also removes its stored entry,
        // once the change has landed.
        let mut stale_account: Option<String> = None;
        let edit = match (&edit, crate::secret_store::catalog_secret_account(id)) {
            (Edit::Text(s), Some(account)) if item.control == Control::Secret => {
                if s.is_empty() {
                    stale_account = crate::secret_store::catalog_secret_current(&doc.config(), id)
                        .and_then(|v| {
                            awili_config::secrets::parse_secret_ref(&v).map(str::to_string)
                        });
                }
                match awili_config::secrets::store_typed_key(
                    crate::secret_store::store(),
                    account,
                    s,
                ) {
                    Ok(written) => Edit::Text(written),
                    Err(e) => {
                        self.error = Some(format!("{} was not saved: {e}", item.label));
                        return false;
                    }
                }
            }
            _ => edit,
        };
        let put = |next: &mut ConfigDoc| match &edit {
            Edit::Bool(b) => next.set_bool(&target, *b),
            Edit::Int(n) => next.set_integer(&target, *n),
            Edit::Float(f) => next.set_float(&target, *f),
            // A key is the owner's exact text; everything else is trimmed.
            Edit::Text(s) if item.control == Control::Secret => next.set_string(&target, s),
            Edit::Text(s) => next.set_string(&target, s.trim()),
        };
        let mut set = put(&mut next);
        // SXe-FIX1: `[memory]` needs its address once the table exists. A
        // first memory setting on a file without the table also writes the
        // address in use (the built-in default), so it is never refused for
        // that. A refused set leaves `next` unchanged.
        if set.is_err() && item.table() == "memory" && item.key() != "base_url" {
            let address = awili_config::Target::Table {
                table: "memory".into(),
                key: "base_url".into(),
            };
            set = next
                .set_string(&address, &doc.config().memory.base_url)
                .and_then(|()| put(&mut next));
        }
        if let Err(e) = set {
            self.error = Some(format!("{} was not saved: {e}", item.label));
            return false;
        }
        let landed = self.land_item(item, next.text(), "Saved");
        if landed {
            if let Some(account) = stale_account {
                crate::secret_store::delete_accounts(&[account], crate::secret_store::store());
            }
        }
        landed
    }

    /// Put one item back to its built-in default: the key leaves the file.
    pub fn reset_item(&mut self, id: &str) -> bool {
        let (Some(item), Some(doc)) = (find(id), self.doc.as_ref()) else {
            return false;
        };
        let mut next = doc.clone();
        if let Err(e) = next.remove(&item.target()) {
            self.error = Some(format!("{} was not reset: {e}", item.label));
            return false;
        }
        self.land_item(item, next.text(), "Back to default")
    }

    /// Undo the last catalog change. Refused (and the history dropped) when
    /// the file no longer holds exactly what that change wrote.
    pub fn undo_last(&mut self) -> bool {
        let Some(step) = self.undo.last().cloned() else {
            return false;
        };
        let in_memory = self.doc.as_ref().map(ConfigDoc::text);
        let on_disk = std::fs::read_to_string(self.path()).ok();
        if in_memory.as_deref() != Some(step.after.as_str())
            || on_disk.as_deref() != Some(step.after.as_str())
        {
            self.undo.clear();
            self.error = Some(UNDO_STALE.into());
            return false;
        }
        let providers = self.draft.as_ref().map(|d| d.providers.clone());
        if !self.commit_raw(&step.before) {
            return false;
        }
        self.keep_providers(providers);
        self.undo.pop();
        self.status = Some(format!("Undone: {}", step.label));
        true
    }

    /// The label of the change Undo would walk back.
    pub fn last_change(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    fn land_item(&mut self, item: &Item, after: String, verb: &str) -> bool {
        self.land_change(item.label, item.restart, after, verb)
    }

    /// SXe: the Models & Fleet default model, live: set both top-level keys,
    /// or remove both ("first available").
    pub fn apply_default_model(&mut self, choice: Option<(String, String)>) -> bool {
        let Some(doc) = self.doc.as_ref() else {
            return false;
        };
        let mut next = doc.clone();
        let top = |key: &str| awili_config::Target::TopLevel { key: key.into() };
        let set = match &choice {
            Some((provider, model)) => next
                .set_string(&top("default_provider"), provider)
                .and_then(|_| match model.as_str() {
                    "" => next.remove(&top("default_model")),
                    _ => next.set_string(&top("default_model"), model),
                }),
            None => next
                .remove(&top("default_provider"))
                .and_then(|_| next.remove(&top("default_model"))),
        };
        if let Err(e) = set {
            self.error = Some(format!("Default model was not saved: {e}"));
            return false;
        }
        self.land_change("Default model", false, next.text(), "Saved")
    }

    fn land_change(&mut self, label: &str, restart: bool, after: String, verb: &str) -> bool {
        let before = self.doc.as_ref().map(ConfigDoc::text).unwrap_or_default();
        if after == before {
            return true;
        }
        // `[voice]` is awili-voice's table: ConfigDoc does not check it, so a
        // value that breaks it is refused here.
        if VoiceConfig::parse(&before).is_ok() && VoiceConfig::parse(&after).is_err() {
            self.error = Some(format!("{label} was not saved: that value is not allowed.",));
            return false;
        }
        // An unsaved provider card (Models & Fleet) survives a live change.
        let providers = self.draft.as_ref().map(|d| d.providers.clone());
        if !self.commit_raw(&after) {
            return false;
        }
        self.keep_providers(providers);
        self.undo.push(UndoStep {
            label: label.to_string(),
            before,
            after,
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        let later = if restart {
            " (applies after you quit Awili and open it again)"
        } else {
            ""
        };
        self.status = Some(format!("{verb}: {label}{later}"));
        true
    }

    fn keep_providers(&mut self, providers: Option<Vec<ProviderDraft>>) {
        if let (Some(p), Some(d)) = (providers, self.draft.as_mut()) {
            d.providers = p;
        }
    }
}

#[cfg(test)]
#[path = "settings_live_tests.rs"]
mod tests;
