//! Settings schema-form — the pure decode / widget-mapping / validation half
//! (plan unit 23, Phase E, review-06 §Settings).
//!
//! Unit 22 ([`awili_gateway::rest`]) exposed the raw settings surface as untyped
//! passthroughs: `get_config()` (the current effective config), `get_config_defaults()`,
//! and — the one that drives the form — `get_config_schema()`, an untyped
//! `serde_json::Value`. The Hermes desktop renders a *schema-driven* settings
//! form: the server describes every configurable field (its type, a human
//! description, a category, and — for dropdowns — the allowed options) and the
//! client turns that description into the right input widget and validates edits
//! against it before writing them back.
//!
//! This module is that client-side half, kept pure so a `#[test]` can drive it
//! without a window: it decodes the schema envelope into a typed
//! [`ConfigSchema`], maps each field's declared type to the [`FieldType`] the
//! renderer switches on, groups the fields into deterministic per-category
//! [`SchemaSection`]s, and — the gate the spec cares about — validates a raw
//! edit against its field's type (a `number` field rejects non-numeric text, a
//! `select` field rejects a value outside its options) via
//! [`SchemaField::validate`]. A light [`SchemaForm`] state layers pending edits
//! over the current values and refuses to produce a write payload while any
//! dirty edit is invalid ([`SchemaForm::commit_payload`]) — the same
//! validation-gate philosophy as [`crate::settings`], but over the gateway's
//! schema rather than awili-config's typed [`awili_config::Config`].
//!
//! ## Ground truth (no invented shape)
//!
//! The schema envelope is `{ "fields": { "<key>": { "type", "description",
//! "category", "options"? } } }`. As captured live from the running Hermes
//! dashboard (`GET /api/config/schema`, 464 fields), the `type` tag is exactly
//! one of `string` / `number` / `boolean` / `list` / `select`; `options` is
//! present only on `select`; no field object carries any other key. Unknown /
//! missing types fall back to [`FieldType::Text`] (the desktop renders an
//! unrecognised field as a plain text input rather than dropping it), so a
//! server that adds a new widget kind degrades gracefully instead of erroring.
//!
//! ## What is code-verifiable vs. Owner-gated
//!
//! Everything here is window-free and `#[test]`-covered: the tolerant schema
//! decode, field→widget mapping, per-type validation, deterministic category
//! grouping, current-value flattening, and the dirty/commit gate. What is
//! **NOT** here — and is intentionally deferred to the live-integration half —
//! is the GPUI render (Owner's eyes), the provider **OAuth** flow, and the
//! `reload.mcp {confirm:true}` action, all of which per the plan (unit 23,
//! **[LIVE]**) need a live server. The write itself goes through the existing
//! [`awili_gateway::rest::RestClient::set_config`]; this module produces the
//! validated typed payload that write consumes. Until the window lands the
//! module carries a `dead_code` allow exactly like [`crate::messaging_pane`],
//! [`crate::usage_chart`] and [`crate::command_palette`]; the engine tests
//! exercise it regardless.
#![allow(dead_code)]

use std::collections::BTreeMap;

use serde_json::Value;

/// The declared type of a schema field — what the renderer switches on to pick
/// an input widget, and what [`SchemaField::validate`] enforces. Mirrors the
/// closed set of `type` tags the gateway emits (`string` / `number` /
/// `boolean` / `list` / `select`); anything else decodes to [`FieldType::Text`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    /// A free-form single-line text input (`string`, and the fallback).
    Text,
    /// A numeric input (`number`) — validated as an `f64`.
    Number,
    /// A toggle (`boolean`) — validated as `true` / `false`.
    Boolean,
    /// A comma-separated list editor (`list`).
    List,
    /// A dropdown constrained to [`SchemaField::options`] (`select`).
    Select,
}

impl FieldType {
    /// Decode a schema `type` tag. Unrecognised / empty tags fall back to
    /// [`FieldType::Text`] so a new server-side widget kind renders as a text
    /// box instead of vanishing.
    pub fn from_tag(tag: &str) -> FieldType {
        match tag {
            "number" => FieldType::Number,
            "boolean" => FieldType::Boolean,
            "list" => FieldType::List,
            "select" => FieldType::Select,
            // "string" and everything unknown.
            _ => FieldType::Text,
        }
    }
}

/// One field of the settings schema: its dotted config key (`agent.max_turns`),
/// the widget it renders as, the human description the label shows, the category
/// it groups under, and — for a [`FieldType::Select`] — the allowed `options`
/// (empty for every other type).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaField {
    pub key: String,
    pub field_type: FieldType,
    pub description: String,
    pub category: String,
    /// Allowed values for a `select`; empty for every other field type. An
    /// empty string is itself a legal option (e.g. `agent.service_tier` offers
    /// `["", "auto", "default", "flex"]`, where `""` means "unset").
    pub options: Vec<String>,
}

impl SchemaField {
    /// Validate a raw edit string against this field's declared type, returning
    /// the typed [`FieldValue`] ready to write or the reason it was rejected.
    /// This is the gate the settings form runs before any edit reaches disk.
    pub fn validate(&self, raw: &str) -> Result<FieldValue, ValidationError> {
        match self.field_type {
            FieldType::Text => Ok(FieldValue::Text(raw.to_string())),
            FieldType::Number => raw
                .trim()
                .parse::<f64>()
                .ok()
                // Reject NaN / ±inf / overflow (`1e309` parses to inf): a
                // non-finite float can't be represented in JSON and would
                // serialize to `null`, silently corrupting the written config —
                // exactly what this gate exists to prevent.
                .filter(|n| n.is_finite())
                .map(FieldValue::Number)
                .ok_or_else(|| ValidationError::NotANumber(raw.to_string())),
            FieldType::Boolean => match raw.trim().to_ascii_lowercase().as_str() {
                "true" => Ok(FieldValue::Bool(true)),
                "false" => Ok(FieldValue::Bool(false)),
                _ => Err(ValidationError::NotABoolean(raw.to_string())),
            },
            FieldType::List => Ok(FieldValue::List(parse_list(raw))),
            FieldType::Select => {
                if self.options.iter().any(|o| o == raw) {
                    Ok(FieldValue::Choice(raw.to_string()))
                } else {
                    Err(ValidationError::NotAnOption {
                        value: raw.to_string(),
                        options: self.options.clone(),
                    })
                }
            }
        }
    }

    /// Whether this field holds a secret (an API key, auth token, password, or
    /// client secret) that must be **masked** in the read / All-Fields view
    /// rather than shown in plaintext (user-test nit, 2026-07-01). Classified by
    /// the field's key — not its value — so an unset secret is still recognised.
    ///
    /// Reuses [`awili_config::doc::is_secret_key`] so the gateway schema-form and
    /// the awili-config typed settings doc mask by the **same** rule (a
    /// case-insensitive `api_key`/`secret`/`password`/`token` substring match).
    /// A [`FieldType::Number`] field is excluded — that distinguishes a real
    /// string credential (`access_tokens`, `auth_token`) from a numeric token
    /// *budget* (`max_tokens`, `max_output_tokens`), mirroring the `Int`-kind
    /// exclusion in [`awili_config::doc::SettingEntry::is_secret`] without a
    /// fragile per-name blocklist.
    pub fn is_secret(&self) -> bool {
        self.field_type != FieldType::Number && awili_config::doc::is_secret_key(&self.key)
    }
}

/// A validated field value — the typed result of [`SchemaField::validate`],
/// carrying exactly what the eventual `set_config` write should persist.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Text(String),
    Number(f64),
    Bool(bool),
    List(Vec<String>),
    /// A chosen `select` option (a plain string, but distinguished from free
    /// [`FieldValue::Text`] so a writer knows it was option-constrained).
    Choice(String),
}

/// Why a raw edit failed validation — surfaced inline next to the field, and
/// what makes [`SchemaForm::commit_payload`] refuse to build a write payload.
#[allow(clippy::enum_variant_names)] // the `Not*` prefixes are the error grammar
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationError {
    NotANumber(String),
    NotABoolean(String),
    NotAnOption { value: String, options: Vec<String> },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::NotANumber(v) => write!(f, "“{v}” is not a number"),
            ValidationError::NotABoolean(v) => write!(f, "“{v}” must be true or false"),
            ValidationError::NotAnOption { value, options } => {
                write!(f, "“{value}” is not one of {}", options.join(", "))
            }
        }
    }
}

/// A category's fields in deterministic order — the unit of the settings form's
/// left-nav / section layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaSection {
    pub category: String,
    pub fields: Vec<SchemaField>,
}

/// The decoded settings schema: every configurable field, in a stable order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigSchema {
    pub fields: Vec<SchemaField>,
}

impl ConfigSchema {
    /// Decode the `GET /api/config/schema` envelope. Tolerant by design: reads
    /// the `fields` object, skips any entry that is not an object, defaults a
    /// missing/blank `type` to [`FieldType::Text`], treats missing
    /// `description`/`category` as empty, and only records `options` when it is
    /// an array of strings. A missing or non-object `fields` yields an empty
    /// schema rather than an error, so a partial server response still renders.
    pub fn from_value(value: &Value) -> ConfigSchema {
        let mut fields = Vec::new();
        if let Some(map) = value.get("fields").and_then(Value::as_object) {
            for (key, spec) in map {
                let Some(spec) = spec.as_object() else {
                    continue;
                };
                let field_type = FieldType::from_tag(
                    spec.get("type").and_then(Value::as_str).unwrap_or_default(),
                );
                let description = spec
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let category = spec
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                // Options only belong to a `select`; enforce the invariant the
                // `SchemaField::options` doc promises rather than trusting the
                // server never to attach a stray `options` to another type.
                let options = if field_type == FieldType::Select {
                    spec.get("options")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                fields.push(SchemaField {
                    key: key.clone(),
                    field_type,
                    description,
                    category,
                    options,
                });
            }
        }
        // Deterministic order regardless of the JSON map's backing (a
        // BTreeMap-backed `serde_json` sorts keys already, but never rely on
        // that): category-precedence first, then key. `general` and `agent`
        // lead because they are the two the desktop opens on; the rest follow
        // alphabetically so the layout is stable across server versions.
        fields.sort_by(|a, b| {
            category_rank(&a.category)
                .cmp(&category_rank(&b.category))
                .then_with(|| a.category.cmp(&b.category))
                .then_with(|| a.key.cmp(&b.key))
        });
        ConfigSchema { fields }
    }

    /// Look up a field by its dotted config key.
    pub fn field(&self, key: &str) -> Option<&SchemaField> {
        self.fields.iter().find(|f| f.key == key)
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Group the fields into per-category [`SchemaSection`]s, preserving the
    /// deterministic order [`from_value`](Self::from_value) established: the
    /// sections come out in category order, and each section's fields keep
    /// their sorted order. Because the field list is pre-sorted by category, a
    /// single pass is enough.
    pub fn sections(&self) -> Vec<SchemaSection> {
        let mut sections: Vec<SchemaSection> = Vec::new();
        for field in &self.fields {
            match sections.last_mut() {
                Some(section) if section.category == field.category => {
                    section.fields.push(field.clone());
                }
                _ => sections.push(SchemaSection {
                    category: field.category.clone(),
                    fields: vec![field.clone()],
                }),
            }
        }
        sections
    }
}

/// A settings form over a [`ConfigSchema`]: the current values (as flattened
/// from `get_config`) with pending edits layered on top. Pure and window-free —
/// the render layer reads [`effective`](Self::effective) / [`dirty_keys`](Self::dirty_keys)
/// and the commit path runs [`commit_payload`](Self::commit_payload), which is
/// the validation gate.
#[derive(Clone, Debug, Default)]
pub struct SchemaForm {
    schema: ConfigSchema,
    /// The current on-disk values, keyed by the schema's dotted keys.
    values: BTreeMap<String, String>,
    /// Pending, not-yet-committed edits layered over `values`.
    edits: BTreeMap<String, String>,
}

impl SchemaForm {
    /// Build a form from a decoded schema and the current values (typically
    /// [`flatten_config`] applied to `get_config`).
    pub fn new(schema: ConfigSchema, values: BTreeMap<String, String>) -> SchemaForm {
        SchemaForm {
            schema,
            values,
            edits: BTreeMap::new(),
        }
    }

    pub fn schema(&self) -> &ConfigSchema {
        &self.schema
    }

    /// The value that should show in the field's input: a pending edit if one
    /// exists, otherwise the current value, otherwise empty.
    pub fn effective(&self, key: &str) -> &str {
        self.edits
            .get(key)
            .or_else(|| self.values.get(key))
            .map(String::as_str)
            .unwrap_or_default()
    }

    /// The value to render in a **read / All-Fields** cell. A *set* secret field
    /// ([`SchemaField::is_secret`]) renders as a fixed-width dot mask; any other
    /// value is still scrubbed of credentials embedded in a URL
    /// ([`awili_config::doc::redact_embedded_secrets`]) so a `?token=…` in a
    /// `dashboard_url` can't leak either — the same display rule as
    /// [`awili_config::doc::SettingEntry::display_value`]. An empty secret returns
    /// empty so the form keeps its "(empty — click to edit)" affordance.
    ///
    /// The *edit* path still reads [`effective`](Self::effective) (the real
    /// value), and because [`commit_payload`](Self::commit_payload) writes only
    /// dirty edits an unedited secret is never rewritten — so the mask can never
    /// leak into a `set_config` write.
    pub fn display_value(&self, key: &str) -> String {
        let value = self.effective(key);
        let is_secret = self.schema.field(key).is_some_and(SchemaField::is_secret);
        if is_secret && !value.is_empty() {
            "••••••••".to_string()
        } else {
            awili_config::doc::redact_embedded_secrets(value)
        }
    }

    /// Record a pending edit, returning whether it was accepted. A key not in
    /// the schema is rejected outright (returns `false`, nothing recorded) — the
    /// form only edits schema-described fields, so an unknown key is a UI bug or
    /// schema drift, not a silently-dropped write. An edit that returns the
    /// field to its current value clears the pending edit (the field is no
    /// longer dirty) rather than leaving a no-op edit behind.
    pub fn set_edit(&mut self, key: &str, raw: impl Into<String>) -> bool {
        if self.schema.field(key).is_none() {
            return false;
        }
        let raw = raw.into();
        let current = self.values.get(key).map(String::as_str).unwrap_or_default();
        if raw == current {
            self.edits.remove(key);
        } else {
            self.edits.insert(key.to_string(), raw);
        }
        true
    }

    /// Whether the field has a pending edit differing from its current value.
    pub fn is_dirty(&self, key: &str) -> bool {
        self.edits.contains_key(key)
    }

    /// The keys of all pending edits, in sorted (BTreeMap) order.
    pub fn dirty_keys(&self) -> Vec<String> {
        self.edits.keys().cloned().collect()
    }

    /// Throw away every pending edit.
    pub fn discard(&mut self) {
        self.edits.clear();
    }

    /// Validate every pending edit against its field, returning the
    /// `(key, error)` pairs for the ones that failed (empty when all pass). An
    /// edit whose key is not in the schema is skipped — it cannot be validated
    /// and is not part of the schema-driven write.
    pub fn validation_errors(&self) -> Vec<(String, ValidationError)> {
        let mut errors = Vec::new();
        for (key, raw) in &self.edits {
            if let Some(field) = self.schema.field(key) {
                if let Err(e) = field.validate(raw) {
                    errors.push((key.clone(), e));
                }
            }
        }
        errors
    }

    /// Build the typed write payload from the pending edits — the gate. Returns
    /// `Err` with every validation failure if any dirty edit is invalid (and
    /// writes nothing), or `Ok` with the validated `key → FieldValue` map when
    /// all pass. Edits whose key is absent from the schema are dropped.
    pub fn commit_payload(
        &self,
    ) -> Result<BTreeMap<String, FieldValue>, Vec<(String, ValidationError)>> {
        let mut payload = BTreeMap::new();
        let mut errors = Vec::new();
        for (key, raw) in &self.edits {
            let Some(field) = self.schema.field(key) else {
                continue;
            };
            match field.validate(raw) {
                Ok(value) => {
                    payload.insert(key.clone(), value);
                }
                Err(e) => errors.push((key.clone(), e)),
            }
        }
        if errors.is_empty() {
            Ok(payload)
        } else {
            Err(errors)
        }
    }
}

/// Flatten a nested config object (as returned by `get_config`) into the schema's
/// dotted-key space, stringifying each leaf so it can seed a [`SchemaForm`].
/// `agent.max_turns` reads from `config["agent"]["max_turns"]`; numbers/booleans
/// stringify plainly, lists join on `", "`, strings pass through, and nested
/// objects recurse. Non-leaf shapes the schema never references (e.g. an array
/// of objects) are simply not emitted.
pub fn flatten_config(value: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(map) = value.as_object() {
        for (key, child) in map {
            flatten_into(key, child, &mut out);
        }
    }
    out
}

fn flatten_into(prefix: &str, value: &Value, out: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                flatten_into(&format!("{prefix}.{key}"), child, out);
            }
        }
        Value::Array(items) => {
            // A list field: join the scalar items so it round-trips through the
            // comma-separated list editor. Arrays of objects are not scalar
            // config fields, so they are skipped.
            let parts: Vec<String> = items.iter().filter_map(scalar_string).collect();
            if parts.len() == items.len() {
                out.insert(prefix.to_string(), parts.join(", "));
            }
        }
        _ => {
            if let Some(s) = scalar_string(value) {
                out.insert(prefix.to_string(), s);
            }
        }
    }
}

/// Stringify a scalar JSON value the way a form input shows it; `None` for
/// non-scalars (object/array) and JSON `null`.
fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Parse the list editor's comma-separated text into trimmed, non-empty items.
fn parse_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Category sort precedence: the two the desktop leads with come first, every
/// other category sorts alphabetically after them (via the equal rank + the
/// name tiebreak in [`ConfigSchema::from_value`]).
fn category_rank(category: &str) -> u8 {
    match category {
        "general" => 0,
        "agent" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The exact live fixture slice captured from `GET /api/config/schema` on
    /// the running Hermes dashboard — one field of every type plus two selects.
    fn fixture() -> Value {
        json!({
            "fields": {
                "model": {
                    "type": "string",
                    "description": "Default model (e.g. anthropic/claude-sonnet-4.6)",
                    "category": "general"
                },
                "model_context_length": {
                    "type": "number",
                    "description": "Context window override (0 = auto-detect from model metadata)",
                    "category": "general"
                },
                "agent.task_completion_guidance": {
                    "type": "boolean",
                    "description": "Agent → Task Completion Guidance",
                    "category": "agent"
                },
                "agent.disabled_toolsets": {
                    "type": "list",
                    "description": "Agent → Disabled Toolsets",
                    "category": "agent"
                },
                "agent.service_tier": {
                    "type": "select",
                    "description": "API service tier (OpenAI/Anthropic)",
                    "category": "agent",
                    "options": ["", "auto", "default", "flex"]
                },
                "terminal.backend": {
                    "type": "select",
                    "description": "Terminal execution backend",
                    "category": "terminal",
                    "options": ["local", "docker", "ssh", "modal", "daytona", "singularity"]
                }
            }
        })
    }

    #[test]
    fn from_tag_maps_the_closed_set_and_falls_back_to_text() {
        assert_eq!(FieldType::from_tag("string"), FieldType::Text);
        assert_eq!(FieldType::from_tag("number"), FieldType::Number);
        assert_eq!(FieldType::from_tag("boolean"), FieldType::Boolean);
        assert_eq!(FieldType::from_tag("list"), FieldType::List);
        assert_eq!(FieldType::from_tag("select"), FieldType::Select);
        // Unknown and empty degrade to a text input rather than erroring.
        assert_eq!(FieldType::from_tag("colorpicker"), FieldType::Text);
        assert_eq!(FieldType::from_tag(""), FieldType::Text);
    }

    #[test]
    fn decodes_every_field_type_from_the_live_fixture() {
        let schema = ConfigSchema::from_value(&fixture());
        assert_eq!(schema.len(), 6);
        assert_eq!(schema.field("model").unwrap().field_type, FieldType::Text);
        assert_eq!(
            schema.field("model_context_length").unwrap().field_type,
            FieldType::Number
        );
        assert_eq!(
            schema
                .field("agent.task_completion_guidance")
                .unwrap()
                .field_type,
            FieldType::Boolean
        );
        assert_eq!(
            schema.field("agent.disabled_toolsets").unwrap().field_type,
            FieldType::List
        );
        let tier = schema.field("agent.service_tier").unwrap();
        assert_eq!(tier.field_type, FieldType::Select);
        assert_eq!(tier.options, vec!["", "auto", "default", "flex"]);
        assert_eq!(
            tier.description,
            "API service tier (OpenAI/Anthropic)".to_string()
        );
    }

    #[test]
    fn options_only_populated_for_select() {
        let schema = ConfigSchema::from_value(&fixture());
        assert!(schema.field("model").unwrap().options.is_empty());
        assert!(schema
            .field("model_context_length")
            .unwrap()
            .options
            .is_empty());
        assert!(!schema.field("terminal.backend").unwrap().options.is_empty());
    }

    #[test]
    fn decode_is_tolerant_of_missing_pieces() {
        // Missing `fields` → empty, not an error.
        assert!(ConfigSchema::from_value(&json!({})).is_empty());
        // A blank/missing type falls back to Text; missing description/category
        // become empty; a non-object entry is skipped entirely.
        let schema = ConfigSchema::from_value(&json!({
            "fields": {
                "a": { "description": "no type" },
                "b": "not an object",
                "c": { "type": "boolean", "category": "x" }
            }
        }));
        assert_eq!(schema.len(), 2); // "b" skipped
        assert_eq!(schema.field("a").unwrap().field_type, FieldType::Text);
        assert_eq!(schema.field("a").unwrap().category, "");
        assert_eq!(schema.field("c").unwrap().description, "");
    }

    #[test]
    fn sections_group_by_category_in_precedence_order() {
        let schema = ConfigSchema::from_value(&fixture());
        let sections = schema.sections();
        let cats: Vec<&str> = sections.iter().map(|s| s.category.as_str()).collect();
        // general and agent lead; terminal (alphabetical remainder) trails.
        assert_eq!(cats, vec!["general", "agent", "terminal"]);
        // Every field lands in exactly one section.
        let total: usize = sections.iter().map(|s| s.fields.len()).sum();
        assert_eq!(total, schema.len());
        // Within general, keys are sorted.
        let general = &sections[0];
        assert_eq!(
            general
                .fields
                .iter()
                .map(|f| f.key.as_str())
                .collect::<Vec<_>>(),
            vec!["model", "model_context_length"]
        );
    }

    #[test]
    fn non_privileged_categories_sort_alphabetically_not_by_key() {
        // The `from_value` sort is a three-level tiebreak:
        // category_rank → category name → key. `general`(0) and `agent`(1) lead;
        // every *other* category shares rank 2 and — per the docstring's promise
        // that "the rest follow alphabetically so the layout is stable across
        // server versions" — must then order by category NAME, not by field key.
        //
        // This pins that middle `category.cmp` tiebreak, which no other test
        // exercises: `sections_group_by_category...` and
        // `ordering_is_deterministic...` each carry at most ONE rank-2 category,
        // so dropping the category tiebreak (falling straight through to the key
        // tiebreak) would survive them both. Here the keys are chosen so key
        // order and category order DISAGREE — category "alpha" holds the high key
        // "zzz", category "zeta" holds the low key "aaa" — so a key-only fallback
        // would put zeta first and reorder the sections.
        let schema = ConfigSchema::from_value(&json!({
            "fields": {
                "aaa": { "type": "string", "category": "zeta" },
                "zzz": { "type": "string", "category": "alpha" }
            }
        }));
        // Category "alpha" leads despite its higher key; a key-only fallback
        // would emit ["aaa", "zzz"] (zeta first) instead.
        assert_eq!(
            schema
                .fields
                .iter()
                .map(|f| f.key.as_str())
                .collect::<Vec<_>>(),
            vec!["zzz", "aaa"]
        );
        // …and the sections come out in alphabetical category order, grouped —
        // the stable left-nav layout the docstring guarantees.
        let sections = schema.sections();
        let cats: Vec<&str> = sections.iter().map(|s| s.category.as_str()).collect();
        assert_eq!(cats, vec!["alpha", "zeta"]);
    }

    #[test]
    fn ordering_is_deterministic_regardless_of_input_order() {
        // Same fields, declared in a different order, must decode identically.
        let a = ConfigSchema::from_value(&json!({
            "fields": {
                "terminal.backend": { "type": "select", "category": "terminal", "options": ["local"] },
                "model": { "type": "string", "category": "general" },
                "agent.x": { "type": "number", "category": "agent" }
            }
        }));
        let b = ConfigSchema::from_value(&json!({
            "fields": {
                "agent.x": { "type": "number", "category": "agent" },
                "model": { "type": "string", "category": "general" },
                "terminal.backend": { "type": "select", "category": "terminal", "options": ["local"] }
            }
        }));
        assert_eq!(a, b);
        assert_eq!(
            a.fields.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(),
            vec!["model", "agent.x", "terminal.backend"]
        );
    }

    #[test]
    fn validate_text_always_succeeds() {
        let schema = ConfigSchema::from_value(&fixture());
        let f = schema.field("model").unwrap();
        assert_eq!(
            f.validate("anthropic/claude-opus-4-8"),
            Ok(FieldValue::Text("anthropic/claude-opus-4-8".to_string()))
        );
        assert_eq!(f.validate(""), Ok(FieldValue::Text(String::new())));
    }

    #[test]
    fn validate_number_parses_or_rejects() {
        let schema = ConfigSchema::from_value(&fixture());
        let f = schema.field("model_context_length").unwrap();
        assert_eq!(f.validate(" 200000 "), Ok(FieldValue::Number(200000.0)));
        assert_eq!(f.validate("0"), Ok(FieldValue::Number(0.0)));
        assert_eq!(
            f.validate("lots"),
            Err(ValidationError::NotANumber("lots".to_string()))
        );
        assert_eq!(f.validate("-1.5"), Ok(FieldValue::Number(-1.5)));
        // Empty is not a number.
        assert!(matches!(
            f.validate(""),
            Err(ValidationError::NotANumber(_))
        ));
        // Non-finite floats are rejected: they can't be represented in JSON and
        // would serialize to null, corrupting the config.
        for bad in ["NaN", "inf", "-inf", "1e309"] {
            assert!(
                matches!(f.validate(bad), Err(ValidationError::NotANumber(_))),
                "expected {bad} to be rejected"
            );
        }
    }

    #[test]
    fn validate_boolean_is_true_false_case_insensitive() {
        let schema = ConfigSchema::from_value(&fixture());
        let f = schema.field("agent.task_completion_guidance").unwrap();
        assert_eq!(f.validate("true"), Ok(FieldValue::Bool(true)));
        assert_eq!(f.validate(" FALSE "), Ok(FieldValue::Bool(false)));
        assert_eq!(
            f.validate("yes"),
            Err(ValidationError::NotABoolean("yes".to_string()))
        );
    }

    #[test]
    fn validate_list_splits_and_trims_dropping_empties() {
        let schema = ConfigSchema::from_value(&fixture());
        let f = schema.field("agent.disabled_toolsets").unwrap();
        assert_eq!(
            f.validate(" web , terminal ,, files "),
            Ok(FieldValue::List(vec![
                "web".to_string(),
                "terminal".to_string(),
                "files".to_string()
            ]))
        );
        assert_eq!(f.validate(""), Ok(FieldValue::List(vec![])));
    }

    #[test]
    fn validate_select_enforces_options_including_empty() {
        let schema = ConfigSchema::from_value(&fixture());
        let f = schema.field("agent.service_tier").unwrap();
        assert_eq!(
            f.validate("flex"),
            Ok(FieldValue::Choice("flex".to_string()))
        );
        // "" is a legal option (means unset).
        assert_eq!(f.validate(""), Ok(FieldValue::Choice(String::new())));
        assert_eq!(
            f.validate("turbo"),
            Err(ValidationError::NotAnOption {
                value: "turbo".to_string(),
                options: vec![
                    "".to_string(),
                    "auto".to_string(),
                    "default".to_string(),
                    "flex".to_string()
                ]
            })
        );
    }

    #[test]
    fn form_effective_prefers_edit_then_value_then_empty() {
        let schema = ConfigSchema::from_value(&fixture());
        let mut values = BTreeMap::new();
        values.insert("model".to_string(), "anthropic/sonnet".to_string());
        let mut form = SchemaForm::new(schema, values);
        assert_eq!(form.effective("model"), "anthropic/sonnet");
        assert_eq!(form.effective("agent.service_tier"), "");
        form.set_edit("model", "anthropic/opus");
        assert_eq!(form.effective("model"), "anthropic/opus");
        assert!(form.is_dirty("model"));
    }

    #[test]
    fn setting_a_field_back_to_current_clears_the_dirty_edit() {
        let schema = ConfigSchema::from_value(&fixture());
        let mut values = BTreeMap::new();
        values.insert("model".to_string(), "anthropic/sonnet".to_string());
        let mut form = SchemaForm::new(schema, values);
        form.set_edit("model", "anthropic/opus");
        assert!(form.is_dirty("model"));
        form.set_edit("model", "anthropic/sonnet"); // back to current
        assert!(!form.is_dirty("model"));
        assert!(form.dirty_keys().is_empty());
    }

    #[test]
    fn commit_payload_gates_on_validation() {
        let schema = ConfigSchema::from_value(&fixture());
        let mut form = SchemaForm::new(schema, BTreeMap::new());
        form.set_edit("model_context_length", "not-a-number");
        form.set_edit("agent.service_tier", "flex");
        // One dirty edit is invalid → the whole commit is refused, and every
        // failure is reported.
        let err = form.commit_payload().unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].0, "model_context_length");
        assert_eq!(form.validation_errors().len(), 1);
        // Fix it → commit yields the typed payload for both edits.
        form.set_edit("model_context_length", "128000");
        let payload = form.commit_payload().unwrap();
        assert_eq!(payload.len(), 2);
        assert_eq!(
            payload.get("model_context_length"),
            Some(&FieldValue::Number(128000.0))
        );
        assert_eq!(
            payload.get("agent.service_tier"),
            Some(&FieldValue::Choice("flex".to_string()))
        );
    }

    #[test]
    fn set_edit_rejects_keys_absent_from_the_schema() {
        let schema = ConfigSchema::from_value(&fixture());
        let mut form = SchemaForm::new(schema, BTreeMap::new());
        assert!(form.set_edit("model", "anthropic/opus"));
        // An unknown key is rejected outright — never becomes a dirty edit, so
        // it can't silently vanish from a Save payload later.
        assert!(!form.set_edit("ghost.field", "whatever"));
        assert!(!form.is_dirty("ghost.field"));
        assert_eq!(form.dirty_keys(), vec!["model".to_string()]);
        let payload = form.commit_payload().unwrap();
        assert_eq!(payload.len(), 1);
        assert!(payload.contains_key("model"));
    }

    #[test]
    fn decode_ignores_stray_options_on_non_select() {
        // A server that mistakenly attaches options to a non-select must not
        // leak them onto the field (the SchemaField::options invariant).
        let schema = ConfigSchema::from_value(&json!({
            "fields": {
                "weird": { "type": "string", "category": "x", "options": ["a", "b"] }
            }
        }));
        assert!(schema.field("weird").unwrap().options.is_empty());
    }

    #[test]
    fn discard_clears_all_edits() {
        let schema = ConfigSchema::from_value(&fixture());
        let mut form = SchemaForm::new(schema, BTreeMap::new());
        form.set_edit("model", "x");
        form.set_edit("agent.service_tier", "flex");
        assert_eq!(form.dirty_keys().len(), 2);
        form.discard();
        assert!(form.dirty_keys().is_empty());
    }

    #[test]
    fn flatten_config_maps_nested_values_to_dotted_keys() {
        let config = json!({
            "model": "anthropic/sonnet",
            "model_context_length": 200000,
            "agent": {
                "task_completion_guidance": true,
                "disabled_toolsets": ["web", "terminal"]
            },
            "toolsets": [ { "name": "web" } ]  // array of objects → skipped
        });
        let flat = flatten_config(&config);
        assert_eq!(flat.get("model"), Some(&"anthropic/sonnet".to_string()));
        assert_eq!(
            flat.get("model_context_length"),
            Some(&"200000".to_string())
        );
        assert_eq!(
            flat.get("agent.task_completion_guidance"),
            Some(&"true".to_string())
        );
        assert_eq!(
            flat.get("agent.disabled_toolsets"),
            Some(&"web, terminal".to_string())
        );
        // The array-of-objects toolsets is not a scalar field → not flattened.
        assert!(!flat.contains_key("toolsets"));
    }

    #[test]
    fn flatten_config_omits_json_null_leaves() {
        // A JSON `null` config value is not a scalar the form can show, so
        // `scalar_string` returns `None` and the key is dropped entirely —
        // the form then falls back to the schema default rather than seeding
        // the field with the literal text "null". Every other flatten test
        // uses only string/bool/number leaves, so the null branch of
        // `scalar_string` (its `_ => None` arm) is otherwise unpinned.
        let config = json!({ "model": "anthropic/sonnet", "unset_field": null });
        let flat = flatten_config(&config);
        assert_eq!(flat.get("model"), Some(&"anthropic/sonnet".to_string()));
        // The null leaf is omitted — no key, and certainly not "null".
        assert!(!flat.contains_key("unset_field"));
    }

    #[test]
    fn flattened_config_seeds_the_form_by_schema_key() {
        let schema = ConfigSchema::from_value(&fixture());
        let config = json!({
            "model": "anthropic/sonnet",
            "agent": { "service_tier": "auto" }
        });
        let form = SchemaForm::new(schema, flatten_config(&config));
        assert_eq!(form.effective("model"), "anthropic/sonnet");
        assert_eq!(form.effective("agent.service_tier"), "auto");
    }

    #[test]
    fn validation_error_display_is_human_readable() {
        assert_eq!(
            ValidationError::NotANumber("x".to_string()).to_string(),
            "“x” is not a number"
        );
        assert_eq!(
            ValidationError::NotAnOption {
                value: "turbo".to_string(),
                options: vec!["auto".to_string(), "flex".to_string()]
            }
            .to_string(),
            "“turbo” is not one of auto, flex"
        );
    }

    /// A bare `SchemaField` with the given key/type — the other fields don't
    /// affect [`SchemaField::is_secret`], which classifies on key + type only.
    fn field(key: &str, field_type: FieldType) -> SchemaField {
        SchemaField {
            key: key.to_string(),
            field_type,
            description: String::new(),
            category: String::new(),
            options: Vec::new(),
        }
    }

    #[test]
    fn is_secret_matches_credential_keys_not_numeric_budgets() {
        // Recognised string credentials — the shared `is_secret_key` substring
        // rule, so PLURAL token names (`access_tokens`) qualify too, not just the
        // singular. This is the false-negative Codex caught in the first cut.
        for key in [
            "providers.openai.api_key",
            "API_KEY",
            "x.access_token",
            "x.access_tokens",
            "y.client_secret",
            "auth.password",
            "svc.session_token",
            "z.custom_api_key",
        ] {
            assert!(
                field(key, FieldType::Text).is_secret(),
                "{key} should be secret"
            );
        }
        // The false-positive guard: a numeric token *budget* matches the name
        // rule (`contains("token")`) but is excluded by its `Number` type —
        // mirroring the `Int`-kind exclusion in awili_config's SettingEntry.
        assert!(!field("agent.max_tokens", FieldType::Number).is_secret());
        assert!(!field("model_context_length", FieldType::Number).is_secret());
        // Keys with no credential substring are never secret, whatever the type.
        for key in ["agent.max_turns", "model", "terminal.backend"] {
            assert!(
                !field(key, FieldType::Text).is_secret(),
                "{key} should not be secret"
            );
        }
    }

    #[test]
    fn display_value_scrubs_embedded_url_secrets_in_non_secret_fields() {
        // A field that is NOT a secret by key (no credential substring, so
        // `is_secret` is false and it takes `display_value`'s *else* branch)
        // can still hold a URL with a credential embedded in it — a `?token=`
        // query param. The read view must scrub that via
        // `redact_embedded_secrets` (the docstring's promise), so a
        // `dashboard_url` can't leak a token even though the field itself isn't
        // classed secret. Every OTHER `display_value` test uses a plain value
        // (`gpt-5.5`) where that scrub is a no-op, so a refactor of the else
        // branch to a bare `value.to_string()` would pass them all yet leak
        // here — this is the only test that pins the scrub as material.
        let schema = ConfigSchema::from_value(&json!({
            "fields": {
                "general.dashboard_url": { "type": "string", "category": "general" }
            }
        }));
        // Genuinely the redact path, not the mask path: the key carries no
        // credential substring, so it is not a secret.
        assert!(!schema.field("general.dashboard_url").unwrap().is_secret());

        let mut values = BTreeMap::new();
        values.insert(
            "general.dashboard_url".to_string(),
            "http://dash:8645/panel?token=SECRET&view=live".to_string(),
        );
        let form = SchemaForm::new(schema, values);

        // Host / port / path / other params stay legible; only the token value
        // is masked.
        assert_eq!(
            form.display_value("general.dashboard_url"),
            "http://dash:8645/panel?token=••••&view=live"
        );
        // The edit buffer still holds the REAL value — only the display is
        // scrubbed, so an edit round-trips the true URL, never the mask.
        assert_eq!(
            form.effective("general.dashboard_url"),
            "http://dash:8645/panel?token=SECRET&view=live"
        );
    }

    #[test]
    fn display_value_masks_secrets_but_write_keeps_the_real_value() {
        let schema = ConfigSchema::from_value(&json!({
            "fields": {
                "providers.openai.api_key": { "type": "string", "category": "providers" },
                "model": { "type": "string", "category": "general" }
            }
        }));
        let mut values = BTreeMap::new();
        values.insert(
            "providers.openai.api_key".to_string(),
            "sk-live-SECRET".to_string(),
        );
        values.insert("model".to_string(), "gpt-5.5".to_string());
        let mut form = SchemaForm::new(schema, values);

        // Read view masks the secret but shows the ordinary field in the clear…
        assert_eq!(
            form.display_value("providers.openai.api_key"),
            "••••••••".to_string()
        );
        assert_eq!(form.display_value("model"), "gpt-5.5");
        // …while the edit buffer still holds the real value.
        assert_eq!(form.effective("providers.openai.api_key"), "sk-live-SECRET");

        // An UNEDITED secret is never part of the write (only dirty keys are), so
        // the mask can't overwrite the real key.
        assert!(form.commit_payload().unwrap().is_empty());

        // Editing the secret writes the REAL new value, never the mask.
        assert!(form.set_edit("providers.openai.api_key", "sk-live-ROTATED"));
        assert_eq!(
            form.display_value("providers.openai.api_key"),
            "••••••••".to_string()
        );
        let payload = form.commit_payload().unwrap();
        assert_eq!(
            payload.get("providers.openai.api_key"),
            Some(&FieldValue::Text("sk-live-ROTATED".to_string()))
        );
    }
}
