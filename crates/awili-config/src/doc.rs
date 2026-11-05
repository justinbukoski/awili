//! `ConfigDoc` — the settings panel's WRITE path (slice-7 spec §3).
//!
//! [`Config`](crate::Config) stays the read model. Edits go through this
//! wrapper around a `toml_edit::DocumentMut` instead, so a write-back preserves
//! everything serde would destroy: comments, formatting, key order, and tables
//! this crate does not model (`[voice]` belongs to `awili-voice`).
//!
//! Two invariants, both enforced here and tested:
//!
//! 1. **Invalid TOML never reaches disk.** Every save re-validates the full
//!    document through [`Config::parse`] (syntax + semantic rules like the
//!    `min_priority` floor) BEFORE any file is touched. Typed setters validate
//!    after mutating and roll back on failure, so a `ConfigDoc` in memory is
//!    always saveable.
//! 2. **`config.toml.bak` of the previous version on every successful write.**
//!    The previous file is copied to `<name>.bak` first; the new text then
//!    lands via temp-file + rename so a crash mid-write can never leave a
//!    half-written config.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use toml_edit::{DocumentMut, Item, Value};

use crate::{Config, ConfigError, ProviderConfig};

/// S6(a): back up `src` to `bak` by writing a NEW temp file created at the
/// stricter of 0600 and the source's mode, then renaming it over `bak`.
/// An existing `.bak` inode (possibly broader, possibly a symlink) is never
/// opened for writing; it is replaced.
fn backup_no_broader(src: &Path, bak: &Path) -> Result<(), ConfigError> {
    let bytes = std::fs::read(src).map_err(ConfigError::Io)?;
    let mode = crate::effective_config_mode(src).unwrap_or(crate::SECRET_FILE_MODE);
    crate::write_bytes_and_publish(bak, &bytes, mode)
}

/// Addresses one editable scalar in the document. Our config is at most two
/// levels deep (`[[providers]]` entries), so a closed enum beats a generic
/// path walker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// `default_provider = …` (document root).
    TopLevel { key: String },
    /// `[memory] base_url = …`, `[voice] auto_speak = …`, …
    Table { table: String, key: String },
    /// `[[providers]]` entry `index`, key `key`.
    ArrayTable {
        array: String,
        index: usize,
        key: String,
    },
}

/// What kind of scalar a [`SettingEntry`] holds — drives how the settings form
/// renders/edits it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Str,
    Int,
    Bool,
    /// Arrays and anything else — edited as a raw TOML value expression.
    Other,
}

/// One renderable row of the settings form.
///
/// RC2-SVC2 (F0384): `Debug` is written by hand and shows the masked
/// [`SettingEntry::display_value`], never the raw value of a credential.
#[derive(Clone)]
pub struct SettingEntry {
    /// Section label: `""` (root), `"memory"`, `"providers[0]"`, …
    pub section: String,
    pub key: String,
    pub target: Target,
    /// Display text: unquoted for strings, TOML source for the rest.
    pub value: String,
    pub kind: ValueKind,
}

impl std::fmt::Debug for SettingEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingEntry")
            .field("section", &self.section)
            .field("key", &self.key)
            .field("target", &self.target)
            .field("value", &self.display_value())
            .field("kind", &self.kind)
            .finish()
    }
}

impl SettingEntry {
    /// Whether this field holds a credential that must never render in
    /// cleartext in the All-Fields settings view (user-test 2026-07-01: an
    /// `api_key` was shown verbatim). A credential-named key ([`is_secret_key`])
    /// qualifies UNLESS its value is an integer — that distinguishes a real auth
    /// token (`auth_token`/`access_tokens`, a string) from a numeric token
    /// budget (`max_output_tokens`, an `Int`) without a fragile name blocklist.
    pub fn is_secret(&self) -> bool {
        is_secret_key(&self.key) && self.kind != ValueKind::Int
    }

    /// The value to DISPLAY in the settings form. A *set* secret-keyed field
    /// renders as a fixed-width dot mask — never the real bytes and never the
    /// real length. Any *other* value is still scrubbed of credentials embedded
    /// in a URL ([`redact_embedded_secrets`]) so a `?token=…` in a `dashboard_url`
    /// (or a `user:pass@` in any URL) can't leak in cleartext either. An empty
    /// secret returns empty, so the form keeps its "(empty — click to edit)"
    /// affordance. The edit path still seeds the real value, so the owner can
    /// view/change their own key by clicking the row.
    pub fn display_value(&self) -> String {
        if self.is_secret() && !self.value.is_empty() {
            "••••••••".to_string()
        } else {
            redact_embedded_secrets(&self.value)
        }
    }
}

/// Config keys whose values are credentials and must be masked at rest.
/// Case-insensitive substring match so `api_key`, `openai_api_key`, `*_secret`,
/// `password`, and any `token`-bearing key qualify. Numeric token-*budget*
/// fields (`max_output_tokens`) also match here by name, but
/// [`SettingEntry::is_secret`] clears them via their `Int` value kind.
pub fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("api_key")
        || k.contains("apikey")
        || k.contains("secret")
        || k.contains("password")
        || k.contains("passwd")
        || k.contains("token")
}

/// Query-param names whose values are credentials when embedded in a URL-shaped
/// config value (e.g. `dashboard_url = "http://h:8645?token=SECRET"`). Uses the
/// same substring rule as [`is_secret_key`] — so `session_token`, `bearer_token`,
/// `foo_token`, `*_api_key`, `*_secret` all redact — plus the bare URL forms
/// (`key`, `auth`, `pass`). No `Int`-budget exception here: budgets don't ride in
/// URLs, and redacting a query value is harmless even if one did.
fn is_secret_param(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    is_secret_key(&n) || n == "key" || n == "auth" || n == "pass"
}

/// Redact credentials embedded inside a value without hiding the rest of it:
/// the value of any sensitive query parameter ([`is_secret_param`]) and any
/// `user:password@` userinfo in a URL become `••••`, so the host/port/path stay
/// legible while the secret does not render at rest. Values with no URL shape
/// pass through unchanged.
pub fn redact_embedded_secrets(value: &str) -> String {
    let redacted = redact_query_params(value);
    redact_userinfo(&redacted)
}

/// Redact sensitive `name=value` pairs in the query string (after the first
/// `?`), preserving the base, parameter order, other params, and any `#frag`.
fn redact_query_params(value: &str) -> String {
    let Some(qpos) = value.find('?') else {
        return value.to_string();
    };
    let (base, after) = value.split_at(qpos);
    let after = &after[1..]; // drop '?'
    let (query, frag) = match after.find('#') {
        Some(h) => (&after[..h], &after[h..]),
        None => (after, ""),
    };
    let scrubbed: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) if is_secret_param(name) => format!("{name}=••••"),
            _ => pair.to_string(),
        })
        .collect();
    format!("{base}?{}{frag}", scrubbed.join("&"))
}

/// Redact the password in a `scheme://user:password@host` authority, leaving the
/// scheme, username, and host visible. No-op when there is no userinfo password.
fn redact_userinfo(value: &str) -> String {
    let Some(spos) = value.find("://") else {
        return value.to_string();
    };
    let auth_start = spos + 3;
    // The authority runs until the first '/', '?', or '#'.
    let rest = &value[auth_start..];
    let auth_end = rest
        .find(['/', '?', '#'])
        .map(|i| auth_start + i)
        .unwrap_or(value.len());
    let authority = &value[auth_start..auth_end];
    let Some(at) = authority.rfind('@') else {
        return value.to_string();
    };
    let userinfo = &authority[..at];
    let Some((user, _pass)) = userinfo.split_once(':') else {
        return value.to_string();
    };
    format!(
        "{}{user}:••••{}",
        &value[..auth_start],
        &value[auth_start + at..]
    )
}

/// A validated, format-preserving TOML document.
#[derive(Clone, Debug)]
pub struct ConfigDoc {
    doc: DocumentMut,
}

impl ConfigDoc {
    /// Parse and validate. Anything [`Config::parse`] rejects is rejected here,
    /// so a `ConfigDoc` can only hold a loadable configuration.
    pub fn parse(text: &str) -> Result<ConfigDoc, ConfigError> {
        let doc: DocumentMut = text
            .parse()
            .map_err(|e: toml_edit::TomlError| ConfigError::Syntax(e.to_string()))?;
        Config::parse(text)?;
        Ok(ConfigDoc { doc })
    }

    /// Load from the first existing config path; when no file exists, an empty
    /// document (everything default, saveable to create the file).
    pub fn load() -> Result<ConfigDoc, ConfigError> {
        for path in Config::config_paths() {
            if path.exists() {
                let text = std::fs::read_to_string(&path).map_err(ConfigError::Io)?;
                return Self::parse(&text);
            }
        }
        Self::parse("")
    }

    /// The explicit first-run seed for a machine with NO config file yet
    /// (FIRST-RUN-SPEC 2026-09-06): a validated document built from the
    /// SERIALIZED [`Config::networkless`], so every service endpoint is an
    /// explicit empty string (`base_url = ""`) in the text — never a serde
    /// default that a future default change could quietly leak into the seed
    /// — and no providers, models, or credentials are present at all.
    ///
    /// Existing documents are never produced this way: they are read as
    /// before ([`ConfigDoc::parse`] on the file's own text) and edited in
    /// place, so their bytes are never reserialized.
    pub fn networkless() -> Result<ConfigDoc, ConfigError> {
        let text = toml::to_string(&Config::networkless())
            .map_err(|e| ConfigError::Syntax(e.to_string()))?;
        Self::parse(&text)
    }

    /// The document's exact TOML source.
    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    /// The validated read model of the current text.
    pub fn config(&self) -> Config {
        // Cannot fail: every constructor/mutator validated the text first.
        Config::parse(&self.text()).expect("ConfigDoc invariant: text always validates")
    }

    /// Every editable scalar, grouped in document order — the settings form.
    pub fn entries(&self) -> Vec<SettingEntry> {
        let mut out = Vec::new();
        for (key, item) in self.doc.iter() {
            match item {
                Item::Value(v) => out.push(entry(
                    String::new(),
                    key,
                    Target::TopLevel {
                        key: key.to_string(),
                    },
                    v,
                )),
                Item::Table(t) => {
                    for (k, sub) in t.iter() {
                        if let Item::Value(v) = sub {
                            out.push(entry(
                                key.to_string(),
                                k,
                                Target::Table {
                                    table: key.to_string(),
                                    key: k.to_string(),
                                },
                                v,
                            ));
                        }
                    }
                }
                Item::ArrayOfTables(tables) => {
                    for (i, t) in tables.iter().enumerate() {
                        for (k, sub) in t.iter() {
                            if let Item::Value(v) = sub {
                                out.push(entry(
                                    format!("{key}[{i}]"),
                                    k,
                                    Target::ArrayTable {
                                        array: key.to_string(),
                                        index: i,
                                        key: k.to_string(),
                                    },
                                    v,
                                ));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        {
            let config = self.config();
            if !out.iter().any(|e| e.section == "engine" && e.key == "max_output_tokens") {
                out.push(entry("engine".into(), "max_output_tokens", Target::Table { table: "engine".into(), key: "max_output_tokens".into() }, &Value::from(config.engine.resolved_max_output_tokens() as i64)));
            }
            for (index, engine) in config.engines.iter().enumerate() {
                let section = format!("engines[{index}]");
                if !out.iter().any(|e| e.section == section && e.key == "max_output_tokens") {
                    out.push(entry(section, "max_output_tokens", Target::ArrayTable { array: "engines".into(), index, key: "max_output_tokens".into() }, &Value::from(engine.to_engine_config().resolved_max_output_tokens() as i64)));
                }
            }
        }
        out
    }

    /// Typed setter: a string value. Validates the whole document after the
    /// mutation and rolls back on failure, so the doc never goes bad in memory.
    pub fn set_string(&mut self, target: &Target, value: &str) -> Result<(), ConfigError> {
        self.set(target, Value::from(value))
    }

    /// Typed setter: an integer value (e.g. `memory.min_priority` — the floor
    /// rule is enforced by the validation pass).
    pub fn set_integer(&mut self, target: &Target, value: i64) -> Result<(), ConfigError> {
        self.set(target, Value::from(value))
    }

    /// Typed setter: a boolean value.
    pub fn set_bool(&mut self, target: &Target, value: bool) -> Result<(), ConfigError> {
        self.set(target, Value::from(value))
    }

    /// Typed setter: a floating-point value (e.g. `voice.vad_threshold`).
    pub fn set_float(&mut self, target: &Target, value: f64) -> Result<(), ConfigError> {
        self.set(target, Value::from(value))
    }

    /// Remove a scalar, validating the result (e.g. clearing `default_model`).
    /// Removing a key that does not exist is a no-op, not an error.
    pub fn remove(&mut self, target: &Target) -> Result<(), ConfigError> {
        let before = self.doc.clone();
        match target {
            Target::TopLevel { key } => {
                self.doc.remove(key);
            }
            Target::Table { table, key } => {
                let mut segs = table.split('.');
                let mut cur = segs.next().and_then(|s| self.doc.get_mut(s));
                for seg in segs {
                    cur = cur
                        .and_then(Item::as_table_like_mut)
                        .and_then(|t| t.get_mut(seg));
                }
                if let Some(t) = cur.and_then(Item::as_table_like_mut) {
                    t.remove(key);
                }
            }
            Target::ArrayTable { array, index, key } => {
                if let Some(t) = self
                    .doc
                    .get_mut(array)
                    .and_then(Item::as_array_of_tables_mut)
                    .and_then(|a| a.get_mut(*index))
                {
                    t.remove(key);
                }
            }
        }
        match Config::parse(&self.doc.to_string()) {
            Ok(_) => Ok(()),
            Err(e) => {
                self.doc = before;
                Err(e)
            }
        }
    }

    /// Replace the whole `[[providers]]` list with `providers` — the settings
    /// panel's add/remove/reorder path (slice-11 spec §1). Rebuilds the array
    /// of tables from the typed structs, validates, and rolls back on failure.
    /// Comments INSIDE provider entries do not survive a rebuild; everything
    /// outside `[[providers]]` is untouched.
    pub fn set_providers(&mut self, providers: &[ProviderConfig]) -> Result<(), ConfigError> {
        let before = self.doc.clone();
        let mut arr = toml_edit::ArrayOfTables::new();
        for p in providers {
            let mut t = toml_edit::Table::new();
            t["name"] = toml_edit::value(p.name.as_str());
            t["kind"] = toml_edit::value(p.kind.as_str());
            t["base_url"] = toml_edit::value(p.base_url.as_str());
            let models: toml_edit::Array = p.models.iter().map(String::as_str).collect();
            t["models"] = toml_edit::value(models);
            if !p.enabled {
                // `enabled = true` is the default — only the exception is written.
                t["enabled"] = toml_edit::value(false);
            }
            // H3: write the secret back when present. A keyless provider stays
            // keyless (no empty `api_key` spam); a configured key must survive a
            // rebuild — the settings panel rewrites this whole list on every save.
            if let Some(key) = p.api_key.as_deref().filter(|k| !k.is_empty()) {
                t["api_key"] = toml_edit::value(key);
            }
            arr.push(t);
        }
        if providers.is_empty() {
            self.doc.remove("providers");
        } else {
            self.doc["providers"] = Item::ArrayOfTables(arr);
        }
        match Config::parse(&self.doc.to_string()) {
            Ok(_) => Ok(()),
            Err(e) => {
                self.doc = before;
                Err(e)
            }
        }
    }

    /// Setter from a raw TOML value expression (`["a", "b"]`, `42`, `"text"`) —
    /// how the form edits arrays like a provider's `models`.
    pub fn set_raw(&mut self, target: &Target, raw: &str) -> Result<(), ConfigError> {
        let value: Value = raw
            .trim()
            .parse()
            .map_err(|e: toml_edit::TomlError| ConfigError::Syntax(e.to_string()))?;
        self.set(target, value)
    }

    fn set(&mut self, target: &Target, value: Value) -> Result<(), ConfigError> {
        let before = self.doc.clone();
        let placed = self.place(target, value);
        if !placed {
            return Err(ConfigError::Syntax(format!("no such setting: {target:?}")));
        }
        match Config::parse(&self.doc.to_string()) {
            Ok(_) => Ok(()),
            Err(e) => {
                self.doc = before; // roll back: the doc stays valid
                Err(e)
            }
        }
    }

    /// Put `value` at `target`, creating root/table keys as needed. An
    /// out-of-range `[[array]]` index is the one shape that cannot be created
    /// implicitly (returns `false`).
    fn place(&mut self, target: &Target, value: Value) -> bool {
        match target {
            Target::TopLevel { key } => {
                assign(&mut self.doc[key.as_str()], value);
                true
            }
            Target::Table { table, key } => {
                // rc.4 S7: `table` may be a dotted path (`work.sandbox`).
                let mut segs = table.split('.');
                let mut item = &mut self.doc[segs.next().unwrap_or("")];
                for seg in segs {
                    // A new nested table is a real `[a.b]` table, not an inline one.
                    if item.is_none() {
                        *item = toml_edit::table();
                    }
                    item = &mut item[seg];
                }
                if item.is_none() {
                    *item = toml_edit::table();
                }
                assign(&mut item[key.as_str()], value);
                true
            }
            Target::ArrayTable { array, index, key } => {
                let Some(t) = self
                    .doc
                    .get_mut(array)
                    .and_then(Item::as_array_of_tables_mut)
                    .and_then(|a| a.get_mut(*index))
                else {
                    return false;
                };
                assign(&mut t[key.as_str()], value);
                true
            }
        }
    }

    /// Validate-then-write this document to `path` (see [`save_validated`]).
    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        save_validated(path, &self.text())
    }
}

/// Overwrite `item` with `value`, keeping the old value's decor — that decor is
/// where an inline comment (`base_url = "…" # why`) lives, and edits must not
/// eat user comments.
fn assign(item: &mut Item, mut value: Value) {
    if let Some(existing) = item.as_value() {
        *value.decor_mut() = existing.decor().clone();
    }
    *item = toml_edit::value(value);
}

fn entry(section: String, key: &str, target: Target, v: &Value) -> SettingEntry {
    let (value, kind) = match v {
        Value::String(s) => (s.value().to_string(), ValueKind::Str),
        Value::Integer(i) => (i.value().to_string(), ValueKind::Int),
        Value::Boolean(b) => (b.value().to_string(), ValueKind::Bool),
        other => (other.to_string().trim().to_string(), ValueKind::Other),
    };
    SettingEntry {
        section,
        key: key.to_string(),
        target,
        value,
        kind,
    }
}

/// The write gate both the raw-TOML tab and [`ConfigDoc::save_to`] go through:
///
/// 1. validate `text` via [`Config::parse`] — on ANY error, return it and touch
///    nothing on disk;
/// 2. back up the existing file (if any) to `<file_name>.bak` as a NEW inode
///    created at the stricter of 0600 and the source's mode (S6(a): an
///    existing `.bak` inode is never opened for writing, it is replaced);
/// 3. write `text` to a UNIQUE temp file in the same directory, created 0600
///    (or the stricter existing mode) and fsync'd before rename, then rename it
///    over `path` (atomic on POSIX — no torn config even on a crash mid-save).
pub fn save_validated(path: &Path, text: &str) -> Result<(), ConfigError> {
    Config::parse(text)?;

    if let Some(parent) = path.parent() {
        crate::ensure_private_app_dir(parent).map_err(ConfigError::Io)?;
    }
    if path.exists() {
        backup_no_broader(path, &PathBuf::from(format!("{}.bak", path.display())))?;
    }

    crate::write_temp_and_publish(path, text)
}

/// Per-process counter for [`save_create_only`] temp suffixes. The PID alone
/// would let two saves racing inside the SAME process pick the same name; the
/// atomic counter keeps every name in the process unique.
static CREATE_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The CREATE-ONLY write gate for the FIRST save on a machine with no config
/// file yet (FIRST-RUN-SPEC 2026-09-06: a missing path can become configured
/// while the settings screen is open, so creation must not clobber whatever
/// appeared):
///
/// 1. validate `text` via [`Config::parse`] — on ANY error, touch nothing;
/// 2. write `text` to a unique temp file in the SAME directory, opened
///    `create_new` (mode 0600 on Unix) so a pre-existing entry is never
///    truncated or rewritten;
/// 3. publish it atomically with `std::fs::hard_link(temp, path)`. A hard
///    link fails with `AlreadyExists` if ANY directory entry already stands
///    at `path` — a regular file, a directory, or even a dangling symlink —
///    so a config that appeared between step 1 and step 3 is NEVER
///    overwritten, replaced, or renamed over.
/// 4. remove the temp NAME (the target keeps the fully written data through
///    its own link).
///
/// There is deliberately NO replacing `rename` (see [`save_validated`], which
/// keeps that behavior for existing files), NO `.bak` of a previous version
/// (a first create has none), and NO truncation. Only the temp entry THIS call
/// successfully created is ever removed — a colliding pre-existing temp
/// (another Awili process, a crashed run) is skipped over and left exactly as
/// found, never deleted.
///
/// Durability matches [`save_validated`]: it is synchronous and nothing is
/// fsync'd. The guarantee is that the target never APPEARS half-written (the
/// name is only published once the temp is fully written), not stronger
/// crash durability than that.
pub fn save_create_only(path: &Path, text: &str) -> Result<(), ConfigError> {
    Config::parse(text)?;

    if let Some(parent) = path.parent() {
        crate::ensure_private_app_dir(parent).map_err(ConfigError::Io)?;
    }

    loop {
        let n = CREATE_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".tmp-create-{}-{}", std::process::id(), n));
        let tmp = PathBuf::from(tmp);

        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&tmp) {
            Ok(f) => f,
            // A temp with this name already exists and is NOT ours: skip to
            // the next suffix. Never delete a colliding pre-existing temp.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(ConfigError::Io(e)),
        };
        if let Err(e) = file.write_all(text.as_bytes()) {
            // Only clean up the temp entry this call just created.
            let _ = std::fs::remove_file(&tmp);
            return Err(ConfigError::Io(e));
        }
        drop(file);

        match std::fs::hard_link(&tmp, path) {
            Ok(()) => {
                // The target now references the fully written inode. Remove
                // OUR temp name; the target keeps the data through its link.
                let _ = std::fs::remove_file(&tmp);
                return Ok(());
            }
            Err(e) => {
                // The target appeared (or another IO failure): the foreign
                // entry is left exactly as found.
                let _ = std::fs::remove_file(&tmp);
                return Err(ConfigError::Io(e));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    const SAMPLE: &str = r#"# Awili config — hand comment that must survive edits.
default_provider = "ollama"

[memory]
base_url = "http://10.0.0.5:9000" # inline comment
min_priority = 95

# voice is awili-voice's table; awili-config must pass it through untouched.
[voice]
auto_speak = true
model = "base.en-q5_1"

# P12 connector sections (awili-connectors-mcp); disabled by default.
[outline]
enabled = true

[gitea]
enabled = true
secret_file = "~/.config/foreman/gitea.token"
secret_key = "GITEA_TOKEN"

[[providers]]
name = "ollama"
kind = "ollama"
base_url = "http://localhost:11434"
models = ["llama3.1:8b", "qwen2.5:14b"]

[[providers]]
name = "anthropic"
kind = "anthropic"
base_url = "https://api.anthropic.com"
models = ["claude-opus-4-8"]
"#;

    pub(super) fn temp_path(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("awili-config-doc-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    #[test]
    fn parse_rejects_what_config_rejects() {
        assert!(ConfigDoc::parse("not = = toml").is_err());
        // Syntactically fine, semantically invalid: below the priority floor.
        let below_floor = "[memory]\nbase_url = \"http://h:1\"\nmin_priority = 1";
        match ConfigDoc::parse(below_floor) {
            Err(ConfigError::MinPriorityTooLow(1)) => {}
            other => panic!("expected MinPriorityTooLow, got {other:?}"),
        }
    }

    #[test]
    fn typed_setters_preserve_comments_and_unknown_tables() {
        let mut doc = ConfigDoc::parse(SAMPLE).unwrap();
        doc.set_string(
            &Target::Table {
                table: "memory".into(),
                key: "base_url".into(),
            },
            "http://new-host:1234",
        )
        .unwrap();
        let text = doc.text();
        assert!(text.contains("hand comment that must survive"), "{text}");
        assert!(text.contains("# inline comment"), "{text}");
        assert!(text.contains("http://new-host:1234"));
        // The [voice] table (not modelled by Config) is untouched.
        assert!(text.contains("auto_speak = true"));
        assert_eq!(doc.config().memory.base_url, "http://new-host:1234");
    }

    #[test]
    fn invalid_typed_edit_rolls_back() {
        let mut doc = ConfigDoc::parse(SAMPLE).unwrap();
        let before = doc.text();
        // min_priority below the floor must be rejected AND leave the doc as-was.
        let err = doc.set_integer(
            &Target::Table {
                table: "memory".into(),
                key: "min_priority".into(),
            },
            10,
        );
        assert!(matches!(err, Err(ConfigError::MinPriorityTooLow(10))));
        assert_eq!(doc.text(), before);
        // An unknown provider kind is also semantically invalid → rolled back.
        let err = doc.set_string(
            &Target::ArrayTable {
                array: "providers".into(),
                index: 0,
                key: "kind".into(),
            },
            "frobnicate",
        );
        assert!(err.is_err());
        assert_eq!(doc.text(), before);
    }

    #[test]
    fn array_table_and_raw_setters_work() {
        let mut doc = ConfigDoc::parse(SAMPLE).unwrap();
        doc.set_string(
            &Target::ArrayTable {
                array: "providers".into(),
                index: 1,
                key: "base_url".into(),
            },
            "https://proxy.local",
        )
        .unwrap();
        doc.set_raw(
            &Target::ArrayTable {
                array: "providers".into(),
                index: 0,
                key: "models".into(),
            },
            r#"["llama3.1:70b"]"#,
        )
        .unwrap();
        let c = doc.config();
        assert_eq!(c.providers[1].base_url, "https://proxy.local");
        assert_eq!(c.providers[0].models, vec!["llama3.1:70b".to_string()]);
        // Out-of-range provider index is an error, not a panic.
        assert!(doc
            .set_string(
                &Target::ArrayTable {
                    array: "providers".into(),
                    index: 9,
                    key: "name".into()
                },
                "x"
            )
            .is_err());
    }

    #[test]
    fn entries_cover_every_section() {
        let doc = ConfigDoc::parse(SAMPLE).unwrap();
        let entries = doc.entries();
        let find = |section: &str, key: &str| {
            entries
                .iter()
                .find(|e| e.section == section && e.key == key)
                .unwrap_or_else(|| panic!("missing entry {section}/{key}"))
        };
        assert_eq!(find("", "default_provider").value, "ollama");
        assert_eq!(find("", "default_provider").kind, ValueKind::Str);
        assert_eq!(find("memory", "min_priority").value, "95");
        assert_eq!(find("memory", "min_priority").kind, ValueKind::Int);
        assert_eq!(find("voice", "auto_speak").kind, ValueKind::Bool);
        // P12 connector sections register entries like every other table.
        assert_eq!(find("outline", "enabled").value, "true");
        assert_eq!(find("outline", "enabled").kind, ValueKind::Bool);
        assert_eq!(find("gitea", "enabled").value, "true");
        assert_eq!(find("gitea", "secret_key").value, "GITEA_TOKEN");
        assert_eq!(find("gitea", "secret_key").kind, ValueKind::Str);
        assert_eq!(find("providers[0]", "name").value, "ollama");
        assert_eq!(
            find("providers[1]", "base_url").value,
            "https://api.anthropic.com"
        );
        assert_eq!(find("providers[0]", "models").kind, ValueKind::Other);
    }

    fn secret_str(key: &str, value: &str) -> SettingEntry {
        SettingEntry {
            section: "providers[1]".into(),
            key: key.into(),
            target: Target::ArrayTable {
                array: "providers".into(),
                index: 1,
                key: key.into(),
            },
            value: value.into(),
            kind: ValueKind::Str,
        }
    }

    #[test]
    fn is_secret_uses_key_name_and_value_kind() {
        // Credential-named string fields are secret.
        for k in [
            "api_key",
            "openai_api_key",
            "API_KEY",
            "apikey",
            "client_secret",
            "password",
            "passwd",
            "auth_token",
            "access_tokens",
            "token",
        ] {
            assert!(secret_str(k, "x").is_secret(), "{k} should be secret");
        }
        // Token *budgets* are Int — cleared even though the name matches by
        // substring. This is the false-positive guard.
        let budget = SettingEntry {
            kind: ValueKind::Int,
            ..secret_str("max_output_tokens", "4096")
        };
        assert!(!budget.is_secret());
        let ctx = SettingEntry {
            kind: ValueKind::Int,
            ..secret_str("context_tokens", "8192")
        };
        assert!(!ctx.is_secret());
        // Plain fields are never secret.
        for k in ["model", "base_url", "min_priority", "name"] {
            assert!(!secret_str(k, "x").is_secret(), "{k} should NOT be secret");
        }
    }

    #[test]
    fn display_value_masks_set_secrets_without_leaking_bytes_or_length() {
        let secret = secret_str("api_key", "sk-super-secret-1234567890");
        assert!(secret.is_secret());
        let shown = secret.display_value();
        assert!(!shown.contains("sk-"), "mask leaked the secret: {shown}");
        assert!(!shown.contains("secret"), "mask leaked the secret: {shown}");
        // Fixed-width mask: never reveals the real length.
        assert_eq!(shown, "••••••••");
        assert_ne!(shown.chars().count(), secret.value.chars().count());

        // An empty secret is left empty so the form's "(empty)" hint still shows.
        let empty = SettingEntry {
            value: String::new(),
            ..secret.clone()
        };
        assert_eq!(empty.display_value(), "");

        // A non-secret numeric field is shown verbatim.
        let plain = SettingEntry {
            kind: ValueKind::Int,
            ..secret_str("max_output_tokens", "4096")
        };
        assert!(!plain.is_secret());
        assert_eq!(plain.display_value(), "4096");
    }

    #[test]
    fn display_value_redacts_credentials_embedded_in_urls() {
        // A token in a dashboard_url query (Codex-flagged leak): host stays
        // visible, token does not — and the key `dashboard_url` is NOT secret.
        let url = secret_str("dashboard_url", "http://host:8645?token=abc123&x=1");
        assert!(!url.is_secret());
        let shown = url.display_value();
        assert!(!shown.contains("abc123"), "token leaked: {shown}");
        assert_eq!(shown, "http://host:8645?token=••••&x=1");

        // Fragment and multiple sensitive params survive; order preserved.
        let multi = secret_str("u", "https://h/p?a=1&api_key=SEK&b=2#frag");
        assert_eq!(
            multi.display_value(),
            "https://h/p?a=1&api_key=••••&b=2#frag"
        );

        // Substring param match (Codex): any *_token / bearer / bare key redacts.
        let toks = secret_str(
            "u",
            "https://h/p?session_token=A&bearer_token=B&key=C&safe=1",
        );
        assert_eq!(
            toks.display_value(),
            "https://h/p?session_token=••••&bearer_token=••••&key=••••&safe=1"
        );

        // Userinfo password in a URL is redacted; user + host stay visible.
        let creds = secret_str("base_url", "https://alice:hunter2@host:5432/db");
        let shown = creds.display_value();
        assert!(!shown.contains("hunter2"), "password leaked: {shown}");
        assert_eq!(shown, "https://alice:••••@host:5432/db");

        // A plain non-URL value passes through untouched.
        assert_eq!(
            secret_str("model", "claude-opus-4-8").display_value(),
            "claude-opus-4-8"
        );
        // A URL with no secrets passes through untouched.
        assert_eq!(
            secret_str("base_url", "http://localhost:11434").display_value(),
            "http://localhost:11434"
        );
    }

    #[test]
    fn redact_leaves_at_sign_outside_authority_untouched() {
        // The userinfo scrub is bounded to the authority (up to the first
        // `/`, `?`, or `#`): an `@` that appears only in the path or query —
        // e.g. an email address — must NOT be read as `user:password@`.
        // Otherwise a plausible `value.rfind('@')` / unbounded-authority
        // refactor would splice the host into `host:••••` and corrupt the whole
        // URL. No secret param is present, so the value passes through verbatim.
        let q = secret_str(
            "dashboard_url",
            "https://host:5432/db?email=user@example.com&x=1",
        );
        assert!(!q.is_secret());
        assert_eq!(
            q.display_value(),
            "https://host:5432/db?email=user@example.com&x=1"
        );

        // Same guarantee when the `@` sits in the path segment.
        assert_eq!(
            secret_str("base_url", "https://host:8080/u@team/repo").display_value(),
            "https://host:8080/u@team/repo"
        );
    }

    #[test]
    fn set_providers_rebuilds_the_list_and_spares_everything_else() {
        use crate::ProviderKind;
        let mut doc = ConfigDoc::parse(SAMPLE).unwrap();
        let providers = vec![
            ProviderConfig {
                name: "hermes".into(),
                kind: ProviderKind::Hermes,
                base_url: "http://h:8080".into(),
                models: vec!["hermes-4".into()],
                enabled: true,
                api_key: None,
            },
            ProviderConfig {
                name: "ollama".into(),
                kind: ProviderKind::Ollama,
                base_url: "http://localhost:11434".into(),
                models: vec!["llama3.1:8b".into()],
                enabled: false,
                api_key: None,
            },
        ];
        doc.set_providers(&providers).unwrap();
        let c = doc.config();
        assert_eq!(c.providers, providers);
        let text = doc.text();
        // Comments and the foreign [voice] table survive the rebuild.
        assert!(text.contains("hand comment that must survive"), "{text}");
        assert!(text.contains("auto_speak = true"), "{text}");
        // `enabled` is written only for the disabled provider entry; the
        // two `enabled = true` lines belong to the P12 connector sections,
        // which the providers rebuild must spare.
        assert_eq!(text.matches("enabled = false").count(), 1, "{text}");
        assert_eq!(text.matches("enabled = true").count(), 2, "{text}");

        // Emptying the list removes [[providers]] entirely (parses as none).
        doc.set_providers(&[]).unwrap();
        assert!(doc.config().providers.is_empty());

        // The doc round-trips through the save gate.
        let path = temp_path("set-providers");
        doc.save_to(&path).unwrap();
        assert!(Config::load_from(&path).unwrap().providers.is_empty());
    }

    #[test]
    fn set_float_and_remove_round_trip() {
        let mut doc = ConfigDoc::parse(SAMPLE).unwrap();
        let target = Target::Table {
            table: "voice".into(),
            key: "vad_threshold".into(),
        };
        doc.set_float(&target, 0.02).unwrap();
        assert!(
            doc.text().contains("vad_threshold = 0.02"),
            "{}",
            doc.text()
        );

        // remove() drops a scalar and validates; removing twice is a no-op.
        doc.remove(&target).unwrap();
        assert!(!doc.text().contains("vad_threshold"));
        doc.remove(&target).unwrap();
        doc.remove(&Target::TopLevel {
            key: "default_provider".into(),
        })
        .unwrap();
        assert_eq!(doc.config().default_provider, None);
        // Everything else is untouched.
        assert!(doc.text().contains("# inline comment"));
    }

    // ---- the write gate ---------------------------------------------------------

    #[test]
    fn invalid_toml_never_reaches_disk() {
        let path = temp_path("invalid");
        std::fs::write(&path, "default_provider = \"keep me\"").unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        // Syntax error.
        assert!(save_validated(&path, "this is = = not toml").is_err());
        // Semantic error.
        assert!(save_validated(&path, "[memory]\nbase_url=\"h\"\nmin_priority = 1").is_err());

        // File untouched, and NO .bak appeared (nothing was written).
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        assert!(!bak.exists(), "a failed save must not produce a .bak");
    }

    #[test]
    fn successful_save_keeps_bak_of_previous_version() {
        let path = temp_path("bak");
        save_validated(&path, "default_provider = \"v1\"\n").unwrap();
        // First write of a brand-new file: nothing previous to back up.
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        assert!(!bak.exists());

        save_validated(&path, "default_provider = \"v2\"\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "default_provider = \"v2\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            "default_provider = \"v1\"\n"
        );

        // Every successful write rolls the .bak forward.
        save_validated(&path, "default_provider = \"v3\"\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            "default_provider = \"v2\"\n"
        );
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let path = temp_path("tmp");
        save_validated(&path, "").unwrap();
        let dir = path.parent().unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn doc_save_roundtrips_through_load_path() {
        let path = temp_path("roundtrip");
        let doc = ConfigDoc::parse(SAMPLE).unwrap();
        doc.save_to(&path).unwrap();
        let reread = std::fs::read_to_string(&path).unwrap();
        assert_eq!(reread, doc.text(), "save writes the exact document text");
        // And what we wrote still loads as a Config.
        assert!(Config::load_from(&path).is_ok());
    }

    // ---- create-only first-save gate (FIRST-RUN-SPEC 2026-09-06, slice 2a) ----

    fn assert_no_temp_leftovers(path: &Path) {
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "temp leftovers: {leftovers:?}");
    }

    fn assert_no_bak(path: &Path) {
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        assert!(
            !bak.exists(),
            "a first create must never write a .bak: {bak:?}"
        );
    }

    #[test]
    fn networkless_seed_survives_parse_with_empty_endpoints_and_no_providers() {
        let doc = ConfigDoc::networkless().unwrap();
        let text = doc.text();
        // Every service endpoint is an EXPLICIT empty string in the text —
        // memory, feed, ccd, hermes, engine, outline and gitea each declare
        // `base_url = ""`, so a future serde-default change cannot leak into
        // the seed.
        assert_eq!(
            text.matches("base_url = \"\"").count(),
            7,
            "seven tables declare an explicit empty base_url:\n{text}"
        );
        assert!(
            text.contains("dashboard_url = \"\""),
            "the gateway URL is explicitly empty:\n{text}"
        );
        assert!(text.contains("providers = []"), "no providers:\n{text}");
        // ...and the parsed read model agrees — no providers, no secrets.
        let c = doc.config();
        assert!(c.providers.is_empty());
        assert!(c.memory.base_url.is_empty());
        assert!(c.feed.base_url.is_empty());
        assert!(c.ccd.base_url.is_empty());
        assert!(c.hermes.base_url.is_empty());
        assert!(c.hermes.api_key.is_empty());
        assert!(c.engine.base_url.is_empty());
        assert!(c.gateway.dashboard_url.is_empty());
        assert!(c.default_provider.is_none());
        assert!(c.default_model.is_none());
        // The seed is saveable and round-trips as a Config.
        let path = temp_path("networkless");
        doc.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), Config::networkless());
    }

    #[test]
    fn invalid_first_create_writes_nothing() {
        let path = temp_path("create-only-invalid");
        assert!(!path.exists());
        // Syntax error, then a semantic one — both must touch nothing.
        assert!(save_create_only(&path, "this is = = not toml").is_err());
        assert!(save_create_only(&path, "[memory]\nmin_priority = 1\n").is_err());
        assert!(
            !path.exists(),
            "an invalid first create must not create the file"
        );
        assert_no_temp_leftovers(&path);
        assert_no_bak(&path);
    }

    #[test]
    fn existing_target_is_rejected_untouched_without_backup() {
        let path = temp_path("create-only-existing");
        let external = "default_provider = \"external\"\n";
        std::fs::write(&path, external).unwrap();
        let err = save_create_only(&path, "default_provider = \"us\"\n").unwrap_err();
        match err {
            ConfigError::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            other => panic!("the publish must fail AlreadyExists, got {other:?}"),
        }
        // The external file is byte-identical: not overwritten, not backed up.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            external,
            "a pre-existing target must stay untouched"
        );
        assert_no_bak(&path);
        assert_no_temp_leftovers(&path);
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_and_directory_at_target_are_rejected_unchanged() {
        // A directory stands at the target: rejected, and still a directory.
        let dir = temp_path("create-only-dir");
        std::fs::create_dir(&dir).unwrap();
        assert!(save_create_only(&dir, "").is_err());
        assert!(dir.is_dir(), "the directory must remain");
        assert_no_bak(&dir);
        assert_no_temp_leftovers(&dir);

        // A dangling symlink IS a directory entry: it counts as existing, is
        // rejected, and remains exactly a dangling symlink.
        let link = temp_path("create-only-dangling");
        std::os::unix::fs::symlink("definitely-not-here.toml", &link).unwrap();
        assert!(save_create_only(&link, "").is_err());
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "the symlink entry itself must remain: {meta:?}"
        );
        assert!(
            !link.exists(),
            "it must still dangle — nothing was created through it"
        );
        assert_no_bak(&link);
        assert_no_temp_leftovers(&link);
    }

    #[test]
    fn colliding_foreign_temps_are_never_deleted() {
        let path = temp_path("create-only-collision");
        let dir = path.parent().unwrap();
        let pid = std::process::id();
        // Occupy a wide prefix of the suffix space with FOREIGN temp entries
        // that match the naming scheme exactly: whatever suffix this process
        // collides with, the implementation must skip it, not delete it.
        let foreign: Vec<String> = (0..256)
            .map(|n| format!("config.toml.tmp-create-{pid}-{n}"))
            .collect();
        for name in &foreign {
            std::fs::write(dir.join(name), "foreign").unwrap();
        }
        save_create_only(&path, "default_provider = \"seed\"\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "default_provider = \"seed\"\n"
        );
        // Every foreign temp survives with its foreign content intact...
        for name in &foreign {
            assert_eq!(
                std::fs::read_to_string(dir.join(name)).unwrap(),
                "foreign",
                "a temp we did not create must never be deleted or written: {name:?}"
            );
        }
        // ...and our own temp (whose suffix is outside the foreign range) is
        // gone.
        let mut remaining: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        remaining.retain(|n| n.contains(".tmp-create-"));
        let mut expected = foreign;
        remaining.sort();
        expected.sort();
        assert_eq!(remaining, expected, "only the foreign temps may remain");
        assert_no_bak(&path);
    }

    fn race_config_text(tag: &str) -> String {
        let models: Vec<String> = (0..128).map(|i| format!("{tag}-model-{i:03}")).collect();
        let models = models
            .iter()
            .map(|m| format!("\"{m}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "default_provider = \"{tag}\"\n\n[[providers]]\nname = \"{tag}\"\nkind = \"ollama\"\nbase_url = \"http://{tag}:11434\"\nmodels = [{models}]\n"
        )
    }

    #[test]
    fn concurrent_first_create_has_exactly_one_winner_with_intact_content() {
        use std::sync::{Arc, Barrier};
        let path = temp_path("create-only-race");
        let barrier = Arc::new(Barrier::new(2));
        let text_a = race_config_text("winner-a");
        let text_b = race_config_text("winner-b");
        let keep_a = text_a.clone();
        let keep_b = text_b.clone();
        let p1 = path.clone();
        let b1 = barrier.clone();
        let t1 = std::thread::spawn(move || {
            b1.wait();
            save_create_only(&p1, &text_a)
        });
        let p2 = path.clone();
        let b2 = barrier.clone();
        let t2 = std::thread::spawn(move || {
            b2.wait();
            save_create_only(&p2, &text_b)
        });
        let r1 = t1.join().unwrap();
        let r2 = t2.join().unwrap();
        assert_eq!(
            [r1.is_ok(), r2.is_ok()].iter().filter(|ok| **ok).count(),
            1,
            "exactly one of the two racing first saves may win: {r1:?} {r2:?}"
        );
        // The winner's content lands byte-for-byte — no mix, no truncation.
        let expected = if r1.is_ok() {
            keep_a.as_str()
        } else {
            keep_b.as_str()
        };
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            expected,
            "the winner's content must be intact"
        );
        // The loser failed at the create-only publish, not at the write.
        let loser = if r1.is_ok() { &r2 } else { &r1 };
        match loser {
            Err(ConfigError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            other => panic!("the loser must lose at the publish, got {other:?}"),
        }
        assert_no_temp_leftovers(&path);
        assert_no_bak(&path);
    }

    #[cfg(unix)]
    #[test]
    fn first_create_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_path("create-only-mode");
        save_create_only(&path, "").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "a first-run config carries secrets and must be owner-only"
        );
    }
}

#[cfg(test)]
mod output_settings_tests {
    use super::*;

    #[test]
    fn output_setting_is_editable_and_saved_even_without_an_engine_table() {
        let mut doc = ConfigDoc::parse("# keep this comment\n").unwrap();
        let row = doc.entries().into_iter().find(|e| e.section == "engine" && e.key == "max_output_tokens").unwrap();
        assert_eq!(row.kind, ValueKind::Int);
        assert!(!row.is_secret());
        doc.set_integer(&row.target, 16384).unwrap();
        let path = super::tests::temp_path("output-budget");
        doc.save_to(&path).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("# keep this comment"));
        assert_eq!(ConfigDoc::parse(&text).unwrap().config().engine.max_output_tokens, Some(16384));
    }

    #[test]
    fn model_budgets_are_independent_and_invalid_values_roll_back() {
        let mut doc = ConfigDoc::parse("[engine]\nmax_output_tokens=16384\n[[engines]]\nid='small'\nwindow_tokens=16384\n").unwrap();
        let row = doc.entries().into_iter().find(|e| e.section == "engines[0]" && e.key == "max_output_tokens").unwrap();
        doc.set_integer(&row.target, 4096).unwrap();
        assert_eq!(doc.config().engines[0].max_output_tokens, Some(4096));
        assert_eq!(doc.config().engine.max_output_tokens, Some(16384));
        let before = doc.text();
        assert!(doc.set_integer(&row.target, 0).is_err());
        assert!(doc.set_integer(&row.target, 16384).is_err());
        assert_eq!(doc.text(), before);
    }

    #[test]
    fn older_small_context_configs_remain_loadable() {
        let cfg = Config::parse("[engine]\nwindow_tokens=4096\n").unwrap();
        assert_eq!(cfg.engine.max_output_tokens, None);
        assert!(cfg.engine.resolved_max_output_tokens() + 512 < cfg.engine.window_tokens);
    }
}
