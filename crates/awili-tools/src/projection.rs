//! Structural schema projection: [`ToolDef`] ↔ provider wire formats.
//!
//! ## OpenAI Chat Completions tool format
//!
//! ```json
//! {
//!   "type": "function",
//!   "function": {
//!     "name": "...",
//!     "description": "...",
//!     "parameters": { ... schema ... }
//!   }
//! }
//! ```
//!
//! ## Anthropic Messages tool format
//!
//! ```json
//! {
//!   "name": "...",
//!   "description": "...",
//!   "input_schema": { ... schema ... }
//! }
//! ```
//!
//! Both projections are **structural**: they map [`ToolDef`] fields to the
//! provider's shape without adding runtime-specific fields (executor,
//! capabilities, origin). The `parse_back` operations recover what the wire
//! format carries — a name and a schema ([`ProjectedTool`]) — enabling
//! round-trip assertions. RC2-ENGB2 (TOP43 #29): they never rebuild a
//! [`CanonicalDescriptor`]: the wire carries no side effect, capabilities or
//! executor, and inventing them (it used to say `side_effect: "none"`, a
//! read-only floor) would hand any later caller a risk class nobody declared.
//!
//! ## Round-trip property
//!
//! `project → parse_back → project` is identity: projecting a [`ToolDef`] to
//! wire JSON, then parsing that JSON back, recovers a schema whose *next*
//! projection (with the original descriptor's own metadata) is
//! byte-identical to the first.

use crate::identity::CanonicalDescriptor;
use serde::Serialize;
use serde_json::Value;

/// What a provider's tool definition carries: the tool's name and its
/// argument schema — no risk class, capabilities or executor (those live
/// only in the registry's [`CanonicalDescriptor`]).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectedTool {
    pub name: String,
    pub schema: Value,
}

/// Project a [`CanonicalDescriptor`] to the **raw** wire JSON Value that the
/// caller can embed into a provider request. The output is a
/// `serde_json::Value` representing the provider-native tool definition.
///
/// `tool_project_raw` returns the raw Value without any provider-specific
/// envelope; use [`openai_tool_project`] or [`anthropic_tool_project`] for
/// the full envelope.
pub fn tool_project_raw(descriptor: &CanonicalDescriptor, name: &str) -> Value {
    let mut fields = serde_json::Map::new();
    fields.insert("name".into(), Value::String(name.to_owned()));
    fields.insert("description".into(), Value::String(String::new()));
    fields.insert("schema".into(), descriptor.schema.clone());
    Value::Object(fields)
}

/// Project a [`CanonicalDescriptor`] + name to the **OpenAI Chat Completions**
/// tool definition JSON Value:
///
/// ```json
/// { "type": "function", "function": { "name": "...", "description": "", "parameters": { ... } } }
/// ```
pub fn openai_tool_project(descriptor: &CanonicalDescriptor, name: &str) -> Value {
    let mut function = serde_json::Map::new();
    function.insert("name".into(), Value::String(name.to_owned()));
    function.insert("description".into(), Value::String(String::new()));
    function.insert("parameters".into(), descriptor.schema.clone());

    let mut root = serde_json::Map::new();
    root.insert("type".into(), Value::String("function".into()));
    root.insert("function".into(), Value::Object(function));
    Value::Object(root)
}

/// RC1-R: [`openai_tool_project`] with the tool's own `description`, so the
/// model knows what the tool does (a description is outside the descriptor
/// and its digest).
pub fn openai_tool_project_described(
    descriptor: &CanonicalDescriptor,
    name: &str,
    description: &str,
) -> Value {
    let mut v = openai_tool_project(descriptor, name);
    if let Some(f) = v.get_mut("function").and_then(Value::as_object_mut) {
        f.insert("description".into(), Value::String(description.to_owned()));
    }
    v
}

/// Parse an OpenAI Chat Completions tool definition Value back into what it
/// carries ([`ProjectedTool`]: name and schema; RC2-ENGB2, never a
/// descriptor).
///
/// Returns `None` if the shape doesn't match OpenAI's tool format.
pub fn openai_tool_parse_back(value: &Value, name: &str) -> Option<ProjectedTool> {
    let obj = value.as_object()?;
    if obj.get("type")?.as_str()? != "function" {
        return None;
    }
    let function = obj.get("function")?.as_object()?;
    let projected_name = function.get("name")?.as_str()?;
    if projected_name != name {
        return None;
    }
    let schema = function.get("parameters")?.clone();
    Some(ProjectedTool {
        name: projected_name.to_owned(),
        schema,
    })
}

/// Project a [`CanonicalDescriptor`] + name to the **Anthropic Messages**
/// tool definition JSON Value:
///
/// ```json
/// { "name": "...", "description": "", "input_schema": { ... } }
/// ```
pub fn anthropic_tool_project(descriptor: &CanonicalDescriptor, name: &str) -> Value {
    let mut root = serde_json::Map::new();
    root.insert("name".into(), Value::String(name.to_owned()));
    root.insert("description".into(), Value::String(String::new()));
    root.insert("input_schema".into(), descriptor.schema.clone());
    Value::Object(root)
}

/// Parse an Anthropic Messages tool definition Value back into what it
/// carries ([`ProjectedTool`]: name and schema; RC2-ENGB2, never a
/// descriptor).
///
/// Returns `None` if the shape doesn't match Anthropic's tool format.
pub fn anthropic_tool_parse_back(value: &Value, name: &str) -> Option<ProjectedTool> {
    let obj = value.as_object()?;
    let projected_name = obj.get("name")?.as_str()?;
    if projected_name != name {
        return None;
    }
    let schema = obj.get("input_schema")?.clone();
    Some(ProjectedTool {
        name: projected_name.to_owned(),
        schema,
    })
}

/// The descriptor whose schema is the parsed-back one and whose metadata is
/// the ORIGINAL descriptor's own — the wire never carried it.
fn with_parsed_schema(
    descriptor: &CanonicalDescriptor,
    parsed: ProjectedTool,
) -> CanonicalDescriptor {
    CanonicalDescriptor {
        schema: parsed.schema,
        ..descriptor.clone()
    }
}

/// Verify that a projected OpenAI tool round-trips: project → parse_back
/// recovers a schema whose next projection is byte-identical.
pub fn assert_openai_round_trip(descriptor: &CanonicalDescriptor, name: &str) {
    let projected = openai_tool_project(descriptor, name);
    let parsed = openai_tool_parse_back(&projected, name)
        .expect("openai_tool_parse_back should succeed on its own projection");
    let reprojected = openai_tool_project(&with_parsed_schema(descriptor, parsed), name);
    assert_eq!(
        projected, reprojected,
        "OpenAI tool project → parse_back → project is identity"
    );
}

/// Verify that a projected Anthropic tool round-trips.
pub fn assert_anthropic_round_trip(descriptor: &CanonicalDescriptor, name: &str) {
    let projected = anthropic_tool_project(descriptor, name);
    let parsed = anthropic_tool_parse_back(&projected, name)
        .expect("anthropic_tool_parse_back should succeed on its own projection");
    let reprojected = anthropic_tool_project(&with_parsed_schema(descriptor, parsed), name);
    assert_eq!(
        projected, reprojected,
        "Anthropic tool project → parse_back → project is identity"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::CanonicalDescriptor;

    fn sample_descriptor() -> CanonicalDescriptor {
        CanonicalDescriptor {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query"}
                },
                "required": ["query"]
            }),
            side_effect: "read".into(),
            required_capabilities: vec!["network".into()],
            executor: "builtin:search".into(),
        }
    }

    // ── OpenAI projections ──────────────────────────────────────────────

    #[test]
    fn openai_project_has_function_envelope() {
        let d = sample_descriptor();
        let v = openai_tool_project(&d, "search");
        let obj = v.as_object().unwrap();
        assert_eq!(obj.get("type").unwrap().as_str().unwrap(), "function");
        let func = obj.get("function").unwrap().as_object().unwrap();
        assert_eq!(func.get("name").unwrap().as_str().unwrap(), "search");
        assert_eq!(func.get("description").unwrap().as_str().unwrap(), "");
        assert!(func.get("parameters").unwrap().is_object());
    }

    #[test]
    fn openai_parse_back_none_on_wrong_type() {
        let v: Value = serde_json::json!({"type": "not-function", "function": {}});
        assert!(openai_tool_parse_back(&v, "x").is_none());
    }

    #[test]
    fn openai_parse_back_none_on_name_mismatch() {
        let v = openai_tool_project(&sample_descriptor(), "expected-name");
        assert!(openai_tool_parse_back(&v, "wrong-name").is_none());
    }

    #[test]
    fn openai_round_trip_preserves_schema() {
        let d = sample_descriptor();
        let projected = openai_tool_project(&d, "search");
        let parsed = openai_tool_parse_back(&projected, "search").unwrap();
        // Schema should be byte-identical after canonical sort
        assert_eq!(
            d.schema, parsed.schema,
            "schema survives OpenAI project → parse_back"
        );
    }

    #[test]
    fn openai_project_parse_back_project_identity() {
        let d = sample_descriptor();
        assert_openai_round_trip(&d, "search");
    }

    #[test]
    fn openai_multiple_round_trips() {
        let descriptors = [
            sample_descriptor(),
            CanonicalDescriptor {
                schema: serde_json::json!({"type": "string"}),
                side_effect: "none".into(),
                required_capabilities: vec![],
                executor: "builtin:echo".into(),
            },
        ];
        for d in &descriptors {
            assert_openai_round_trip(d, "test");
        }
    }

    // ── Anthropic projections ───────────────────────────────────────────

    #[test]
    fn anthropic_project_has_input_schema() {
        let d = sample_descriptor();
        let v = anthropic_tool_project(&d, "search");
        let obj = v.as_object().unwrap();
        assert_eq!(obj.get("name").unwrap().as_str().unwrap(), "search");
        assert_eq!(obj.get("description").unwrap().as_str().unwrap(), "");
        assert!(obj.get("input_schema").unwrap().is_object());
    }

    #[test]
    fn anthropic_parse_back_none_on_missing_input_schema() {
        let v: Value = serde_json::json!({"name": "x"});
        assert!(anthropic_tool_parse_back(&v, "x").is_none());
    }

    #[test]
    fn anthropic_parse_back_none_on_name_mismatch() {
        let v = anthropic_tool_project(&sample_descriptor(), "expected-name");
        assert!(anthropic_tool_parse_back(&v, "wrong-name").is_none());
    }

    #[test]
    fn anthropic_round_trip_preserves_schema() {
        let d = sample_descriptor();
        let projected = anthropic_tool_project(&d, "search");
        let parsed = anthropic_tool_parse_back(&projected, "search").unwrap();
        assert_eq!(
            d.schema, parsed.schema,
            "schema survives Anthropic project → parse_back"
        );
    }

    #[test]
    fn anthropic_project_parse_back_project_identity() {
        let d = sample_descriptor();
        assert_anthropic_round_trip(&d, "search");
    }

    #[test]
    fn anthropic_multiple_round_trips() {
        let descriptors = [
            sample_descriptor(),
            CanonicalDescriptor {
                schema: serde_json::json!({"enum": ["a", "b"]}),
                side_effect: "write".into(),
                required_capabilities: vec!["filesystem".into()],
                executor: "builtin:write".into(),
            },
        ];
        for d in &descriptors {
            assert_anthropic_round_trip(d, "tool");
        }
    }

    // ── Cross-provider schema stability ─────────────────────────────────

    #[test]
    fn same_schema_projects_across_providers_preserves_schema() {
        let d = sample_descriptor();
        let oai = openai_tool_project(&d, "search");
        let ant = anthropic_tool_project(&d, "search");

        // The schema field is named differently but holds the same Value
        let oai_schema = oai
            .as_object()
            .and_then(|o| o.get("function"))
            .and_then(|f| f.as_object())
            .and_then(|f| f.get("parameters"))
            .unwrap();
        let ant_schema = ant.as_object().and_then(|o| o.get("input_schema")).unwrap();

        assert_eq!(
            oai_schema, ant_schema,
            "schema value identical across providers"
        );
    }

    // ── tool_project_raw ────────────────────────────────────────────────

    #[test]
    fn raw_project_contains_name_and_schema() {
        let d = sample_descriptor();
        let v = tool_project_raw(&d, "raw-test");
        let obj = v.as_object().unwrap();
        assert_eq!(obj.get("name").unwrap().as_str().unwrap(), "raw-test");
        assert!(obj.get("schema").unwrap().is_object());
    }

    /// RC2-ENGB2 (TOP43 #29): parsing a write tool's wire definition back
    /// recovers its name and schema and claims nothing else — before, it
    /// returned a descriptor with `side_effect: "none"` (a read-only floor)
    /// and `executor: "projected"`, metadata nobody declared.
    #[test]
    fn rc2engb2_parse_back_claims_no_risk_class() {
        let write = CanonicalDescriptor {
            schema: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            side_effect: "write".into(),
            required_capabilities: vec!["filesystem".into()],
            executor: "mcp:files".into(),
        };
        let expected = serde_json::json!({"name": "write_file", "schema": write.schema});
        let oai = openai_tool_parse_back(&openai_tool_project(&write, "write_file"), "write_file")
            .unwrap();
        assert_eq!(serde_json::to_value(&oai).unwrap(), expected);
        let ant =
            anthropic_tool_parse_back(&anthropic_tool_project(&write, "write_file"), "write_file")
                .unwrap();
        assert_eq!(serde_json::to_value(&ant).unwrap(), expected);
        // The round trip still holds, with the descriptor's own metadata.
        assert_openai_round_trip(&write, "write_file");
        assert_anthropic_round_trip(&write, "write_file");
    }

    #[test]
    fn raw_project_no_envelope() {
        let d = sample_descriptor();
        let v = tool_project_raw(&d, "x");
        // raw projection has no 'type' or 'function' envelope
        assert!(v.as_object().and_then(|o| o.get("type")).is_none());
        assert!(v.as_object().and_then(|o| o.get("function")).is_none());
        assert!(v.as_object().and_then(|o| o.get("input_schema")).is_none());
    }
}
