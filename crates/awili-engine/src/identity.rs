//! Canonical tool identity types (unit 4, design r5 Delta A′).
//!
//! `ToolIdentity` / `ToolOrigin` / `DescriptorDigest` live HERE — in
//! `awili-engine`, the shared base both `awili-tools` and `awili-engine-log`
//! depend on — so the identity a registry resolves, an authorization binds,
//! and a durable record persists is ONE type with zero conversion at any
//! trust boundary. `awili-tools` re-exports them; downstream imports compile
//! unchanged. Descriptor construction and hashing policy (canonical JSON,
//! the `CanonicalDescriptor` shape) stay in `awili-tools` beside the
//! registry; only the identity currency itself moved.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The authenticated origin of a tool definition — *who* registered it.
///
/// In C2 v1 this is an opaque string (e.g. "system", "user", "agent:<id>",
/// "connector:<name>"). C1 authz integration is future work — no shadow
/// AuthorizedRequest or authz check lives here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOrigin {
    /// Origin identifier (e.g. "system", "user", "agent:<id>", "connector:<name>").
    pub origin: String,
}

impl ToolOrigin {
    pub fn new(origin: impl Into<String>) -> Self {
        Self {
            origin: origin.into(),
        }
    }
}

/// A SHA-256 descriptor digest: `[u8; 32]`.
///
/// Produced by hashing the canonical JSON of a descriptor. Two equivalent
/// descriptors (same keys, same values) always hash the same regardless of
/// original key ordering or whitespace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescriptorDigest(pub [u8; 32]);

impl DescriptorDigest {
    pub fn from_canonical_json(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

/// The full identity of a tool: authenticated origin + name + descriptor digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolIdentity {
    /// Who registered this tool.
    pub origin: ToolOrigin,
    /// The tool name (used in wire protocol calls: `name` in OpenAI/Anthropic).
    pub name: String,
    /// SHA-256 of the canonical descriptor JSON.
    pub descriptor_digest: DescriptorDigest,
}

impl ToolIdentity {
    pub fn new(
        origin: ToolOrigin,
        name: impl Into<String>,
        descriptor_digest: DescriptorDigest,
    ) -> Self {
        Self {
            origin,
            name: name.into(),
            descriptor_digest,
        }
    }

    /// The canonical identity digest (unit 4, design r3 Delta 2): SHA-256
    /// hex over the identity's canonical JSON serialization — origin +
    /// name + descriptor digest in ONE collision-free form. This is the
    /// value B2 persists in the durable intent record and replay exposes;
    /// equality of identities is equality of digests.
    pub fn digest_hex(&self) -> String {
        // Struct serde is deterministic (fixed field order, no maps), so
        // to_string IS the canonical form here.
        let canonical = serde_json::to_string(self).expect("ToolIdentity serialization is total");
        let mut h = Sha256::new();
        h.update(canonical.as_bytes());
        format!("{:x}", h.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_sha256_of_bytes() {
        // Fixed vector: SHA-256 of the empty byte string.
        assert_eq!(
            DescriptorDigest::from_canonical_json(b""),
            DescriptorDigest([
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
                0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
                0x78, 0x52, 0xb8, 0x55,
            ])
        );
    }

    #[test]
    fn identity_round_trip_serde() {
        let identity = ToolIdentity::new(
            ToolOrigin::new("system"),
            "search",
            DescriptorDigest([1u8; 32]),
        );
        let json = serde_json::to_vec(&identity).unwrap();
        let back: ToolIdentity = serde_json::from_slice(&json).unwrap();
        assert_eq!(identity, back);
    }

    /// The digest is pinned to its definition — SHA-256 over the identity's
    /// canonical JSON — so the persisted form cannot drift silently. Equal
    /// identities digest equal; any differing component digests different.
    #[test]
    fn digest_hex_is_sha256_of_canonical_json() {
        use sha2::Digest;
        let identity = ToolIdentity::new(
            ToolOrigin::new("builtin:fs"),
            "fs.read",
            DescriptorDigest([1u8; 32]),
        );
        let expected = {
            let canonical = serde_json::to_string(&identity).unwrap();
            let mut h = sha2::Sha256::new();
            h.update(canonical.as_bytes());
            format!("{:x}", h.finalize())
        };
        assert_eq!(identity.digest_hex(), expected);
        assert_eq!(identity.digest_hex(), identity.clone().digest_hex());
        assert_ne!(
            identity.digest_hex(),
            ToolIdentity::new(
                ToolOrigin::new("mcp:other"),
                "fs.read",
                DescriptorDigest([1u8; 32])
            )
            .digest_hex()
        );
        assert_ne!(
            identity.digest_hex(),
            ToolIdentity::new(
                ToolOrigin::new("builtin:fs"),
                "fs.write",
                DescriptorDigest([1u8; 32])
            )
            .digest_hex()
        );
        assert_ne!(
            identity.digest_hex(),
            ToolIdentity::new(
                ToolOrigin::new("builtin:fs"),
                "fs.read",
                DescriptorDigest([2u8; 32])
            )
            .digest_hex()
        );
    }
}
