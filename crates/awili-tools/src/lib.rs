//! `awili-tools` — C2-u1: the tool registry, descriptor hashing, and dialect
//! projection.
//!
//! Adopted from the 2026-07-26 implementation per the C-phase arbitration's
//! ADOPT-WITH-PORTS verdict, then ported onto merged main (2026-08-06):
//!
//! - identity keys on **(authenticated origin, name)**, not name alone;
//!   ambiguous bare-name lookups are refused rather than guessed
//! - side-effect metadata speaks **C1's canonical risk vocabulary**
//!   (`read_only|mutating|irreversible|privileged`); legacy strings map
//!   conservatively, unknown values classify `privileged`, and
//!   [`ToolRegistry::risk_floor`] is what feeds `AuthzContext.registry_floor`
//! - **no `Clone`** — the previous impl silently dropped the validator, so a
//!   cloned registry accepted unvalidated registrations; [`ToolRegistry::snapshot`]
//!   replaces it and cannot register at all
//!
//! Still a **pure** crate: no Tokio, rmcp, GPUI, IO, threads, HTTP, or
//! filesystem. The runtime thread is C2-u2 (see docs/contracts/c2-build-plan.md),
//! gated on the test MCP server fixture.
//!
//! ## Core types
//!
//! - [`ToolIdentity`] — authenticated origin + name + descriptor digest
//! - [`ToolDef`] — identity + canonical JSON descriptor
//! - [`ToolRegistry`] — stores defs, duplicate / unknown errors
//! - [`CatalogSchemaValidator`] — abstract registration seam
//! - OpenAI / Anthropic structural schema projection (round-trip safe)

pub mod builtins;
pub mod catalog;
mod identity;
pub mod interlock;
mod projection;
mod registry;
pub mod runtime;

/// The pure attestation vocabulary, re-exported (unit 4b, design r9 §2.3):
/// `awili-seam`, `awili-engine-worker`, and `awili-builtins` name these types
/// through this path so they gain no `awili-attest-types` manifest edge.
pub mod attest {
    pub use awili_attest_types::{
        AttestationClass, BindRefusal, BoundLaunch, InstallOriginWitness, LaunchKind, LaunchToken,
        SetupIdentityId, SetupIdentityParseError, SetupRecordFacts, UnverifiedLaunch,
        UnverifiedReason, VerifiedLaunch,
    };
}

pub use identity::{CanonicalDescriptor, DescriptorDigest, ToolIdentity, ToolOrigin};
pub use projection::{
    ProjectedTool, anthropic_tool_parse_back, anthropic_tool_project, assert_anthropic_round_trip,
    assert_openai_round_trip, openai_tool_parse_back, openai_tool_project,
    openai_tool_project_described, tool_project_raw,
};
pub use registry::{
    AllowAllValidator, CatalogSchemaValidator, RegistryError, RegistryErrorKind, ToolDef,
    ToolRegistry, allocate_aliases, qualified_name,
};

/// Canonical JSON serialisation: recursively key-sorted, no whitespace,
/// RFC 8785-inspired (not full JCS — number normalisation is not applied),
/// implemented via `serde_json::Value` with recursive key sorting. Sufficient
/// for stable descriptor digests where the dynamic content is already a
/// `serde_json::Value`.
pub fn canonical_json(value: &serde_json::Value) -> Vec<u8> {
    let sorted = sort_value(value);
    serde_json::to_vec(&sorted).expect("canonical JSON serialisation is infallible for valid Value")
}

fn sort_value(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for k in keys {
                sorted.insert(k.clone(), sort_value(&map[k]));
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(sort_value).collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_is_deterministic() {
        let v: serde_json::Value = serde_json::json!({
            "z": 1,
            "a": 2,
            "nested": {"c": 3, "b": 2},
        });
        let a = canonical_json(&v);
        let b = canonical_json(&v);
        assert_eq!(a, b, "same value produces same bytes");
    }

    #[test]
    fn canonical_json_key_order() {
        let v: serde_json::Value = serde_json::json!({"z": 1, "a": 2});
        let bytes = canonical_json(&v);
        // Keys must be sorted: a before z
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        if let serde_json::Value::Object(map) = &parsed {
            let keys: Vec<&String> = map.keys().collect();
            assert_eq!(keys, vec!["a", "z"]);
        } else {
            panic!("expected object");
        }
    }
}
