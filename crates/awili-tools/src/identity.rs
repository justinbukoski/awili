//! Tool identity and canonical descriptor types.
//!
//! A [`ToolDef`] is the unit of tool registration: an authenticated identity
//! plus a canonical JSON descriptor that captures the tool's schema, side-effect
//! class, required capabilities, and executor/manifest version. The descriptor
//! digest ([`DescriptorDigest`]) is a SHA-256 of the **deterministically
//! recursively key-sorted canonical JSON** representation, so two equivalent
//! descriptors always produce the same digest regardless of key ordering or
//! whitespace.
//!
//! The identity CURRENCY — [`ToolIdentity`], [`ToolOrigin`],
//! [`DescriptorDigest`] — is owned by `awili-engine` (unit 4, design r5
//! Delta A′) so registry resolution, authorization evidence, and the
//! durable record all share one type. This crate re-exports those types;
//! downstream imports keep compiling unchanged.

use crate::canonical_json;
use serde::{Deserialize, Serialize};

pub use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};

/// The canonical descriptor of a tool — its schema, side-effect class,
/// required capabilities, and executor/manifest version.
///
/// This is the JSON *value* that gets hashed into [`DescriptorDigest`].
/// The fields are a superset of the OpenAI/Anthropic tool-call shape,
/// carrying Awili's own classification metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalDescriptor {
    /// The tool's JSON Schema for its arguments (OpenAI-style `parameters`).
    pub schema: serde_json::Value,
    /// Side-effect classification in C1's CANONICAL vocabulary (port,
    /// arbitration 2026-08-06): "read_only" | "mutating" | "irreversible" |
    /// "privileged". The legacy "none|read|write|dangerous" strings are
    /// accepted on parse and mapped conservatively (see
    /// [`CanonicalDescriptor::risk_floor`]) so old descriptors cannot
    /// silently under-classify.
    pub side_effect: String,
    /// Required capabilities the runtime must provide (e.g. "filesystem",
    /// "network", "process"). Empty if none.
    pub required_capabilities: Vec<String>,
    /// Executor identifier or manifest version (e.g. "builtin:search",
    /// "connector:sqlite", "v1").
    pub executor: String,
}

impl CanonicalDescriptor {
    /// Compute the SHA-256 digest over the recursively-key-sorted canonical
    /// JSON of this descriptor.
    pub fn digest(&self) -> DescriptorDigest {
        let json = serde_json::to_value(self).expect("descriptor is always serialisable");
        let canonical = canonical_json(&json);
        DescriptorDigest::from_canonical_json(&canonical)
    }

    /// Serialise to canonical JSON bytes.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let json = serde_json::to_value(self).expect("descriptor is always serialisable");
        canonical_json(&json)
    }
}

impl CanonicalDescriptor {
    /// The registry-declared risk FLOOR for this tool, in C1's vocabulary.
    /// C1 escalates from here per-arguments and never lowers it; anything
    /// unrecognized classifies `privileged` (contract §3).
    pub fn risk_floor(&self) -> awili_engine::RiskClass {
        use awili_engine::RiskClass;
        match self.side_effect.as_str() {
            "read_only" | "read" | "pure" | "none" => RiskClass::ReadOnly,
            "mutating" | "write" => RiskClass::Mutating,
            "irreversible" | "dangerous" => RiskClass::Irreversible,
            _ => RiskClass::Privileged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_digest_is_deterministic() {
        let d = CanonicalDescriptor {
            schema: serde_json::json!({"type": "object", "properties": {}}),
            side_effect: "none".into(),
            required_capabilities: vec![],
            executor: "builtin:echo".into(),
        };
        let a = d.digest();
        let b = d.digest();
        assert_eq!(a, b);
    }

    #[test]
    fn descriptor_digest_changes_on_field_difference() {
        let d1 = CanonicalDescriptor {
            schema: serde_json::json!({"type": "object"}),
            side_effect: "none".into(),
            required_capabilities: vec![],
            executor: "builtin:a".into(),
        };
        let d2 = CanonicalDescriptor {
            schema: serde_json::json!({"type": "object"}),
            side_effect: "read".into(),
            required_capabilities: vec![],
            executor: "builtin:a".into(),
        };
        assert_ne!(
            d1.digest(),
            d2.digest(),
            "different side_effect => different digest"
        );
    }

    #[test]
    fn descriptor_digest_is_key_order_independent() {
        let d1 = CanonicalDescriptor {
            schema: serde_json::json!({"type": "object", "properties": {}}),
            side_effect: "none".into(),
            required_capabilities: vec![],
            executor: "builtin:echo".into(),
        };
        let d2 = CanonicalDescriptor {
            schema: serde_json::json!({"properties": {}, "type": "object"}),
            side_effect: "none".into(),
            required_capabilities: vec![],
            executor: "builtin:echo".into(),
        };
        assert_eq!(
            d1.digest(),
            d2.digest(),
            "canonical sort makes different key orders equal"
        );
    }

    #[test]
    fn identity_round_trip_serde() {
        let d = CanonicalDescriptor {
            schema: serde_json::json!({"type": "object"}),
            side_effect: "none".into(),
            required_capabilities: vec!["filesystem".into()],
            executor: "builtin:search".into(),
        };
        let identity = ToolIdentity::new(ToolOrigin::new("system"), "search", d.digest());
        let json = serde_json::to_vec(&identity).unwrap();
        let back: ToolIdentity = serde_json::from_slice(&json).unwrap();
        assert_eq!(identity, back);
    }

    #[test]
    fn canonical_bytes_produces_valid_json() {
        let d = CanonicalDescriptor {
            schema: serde_json::json!({"type": "object"}),
            side_effect: "none".into(),
            required_capabilities: vec![],
            executor: "builtin:test".into(),
        };
        let bytes = d.to_canonical_bytes();
        let _: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // Key order must be deterministic
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        if let serde_json::Value::Object(map) = &parsed {
            let keys: Vec<&String> = map.keys().collect();
            assert_eq!(
                keys,
                vec!["executor", "required_capabilities", "schema", "side_effect"]
            );
        }
    }

    /// Gate row 18a (unit 4b): every canonical risk class round-trips
    /// through its `side_effect` string back to the SAME risk floor —
    /// projection can therefore carry risk as a string without ever
    /// widening it.
    #[test]
    fn every_risk_class_round_trips_through_side_effect_to_the_same_risk_floor() {
        for risk in [
            awili_engine::RiskClass::ReadOnly,
            awili_engine::RiskClass::Mutating,
            awili_engine::RiskClass::Irreversible,
            awili_engine::RiskClass::Privileged,
        ] {
            let d = CanonicalDescriptor {
                schema: serde_json::json!({"type": "object"}),
                side_effect: risk.as_str().to_string(),
                required_capabilities: vec![],
                executor: "mcp:test".into(),
            };
            assert_eq!(d.risk_floor(), risk, "{:?} round-trips", risk.as_str());
        }
    }
}
