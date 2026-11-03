//! Trusted builtin used to discover canonical tool aliases without
//! advertising every MCP schema in the same request.

use crate::identity::{CanonicalDescriptor, ToolOrigin};
use crate::registry::ToolDef;
use serde_json::json;

/// Reserved identity for the local, read-only tool catalog.
pub fn origin() -> ToolOrigin {
    ToolOrigin::new("builtin:catalog")
}

/// The local query definition. Installation remains the trusted registration path.
pub fn defs() -> Vec<ToolDef> {
    vec![ToolDef::from_parts(
        origin(),
        "tools.find",
        CanonicalDescriptor {
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "maxLength": 256,
                        "description": "Bounded search text. When select is nonempty, selection takes priority; query is used only to suggest matches for an unknown alias.",
                    },
                    "select": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {"type": "string", "maxLength": 256},
                        "description": "Optional exact canonical aliases to activate. A valid nonempty selection replaces the active set and takes priority over query.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 20,
                        "description": "Search page size; defaults to 10 and must be 1 through 20.",
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 4294967295u64,
                        "description": "Search result offset; defaults to 0. Ignored when a valid selection is supplied.",
                    },
                },
                "required": [],
                "additionalProperties": false,
            }),
            side_effect: "read_only".into(),
            required_capabilities: vec!["catalog:read".into()],
            executor: "builtin:catalog".into(),
        },
    )
    .with_description(
        "Discover tools by alias, description, or configured source. First search with query only (for example shell, browser, or archive), omitting select. Comma-separated search terms match alternatives. Source-name separators such as mac_shell and mac-shell are equivalent in search. Each match includes a bounded source_hint identifying its application/server; a tool scoped to one application is not general desktop control. A nonempty search adds its returned MCP tools to the active set when their complete schemas fit within the 20-tool cap. Empty-query browsing does not change the active set. Call their returned call_name directly; never guess aliases. If selection_required is true, narrow the query or lower limit, or put fewer exact returned aliases in select. Search retains already active tools. Explicit selection replaces the active MCP tool set only when the complete schemas fit. A valid explicit selection takes priority over query. Discovery never executes the requested action. Defaults: empty query, no selection, limit 10, offset 0.",
    )]
}

/// Install the catalog query through the registry's trusted builtin path.
pub fn install(
    reg: &mut crate::registry::ToolRegistry,
) -> Result<(), crate::registry::RegistryError> {
    for def in defs() {
        reg.register_unrestricted(def)?;
    }
    Ok(())
}
