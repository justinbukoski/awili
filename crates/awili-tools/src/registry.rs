//! Tool registry and validation.
//!
//! [`ToolRegistry`] stores [`ToolDef`]s keyed by **(authenticated origin,
//! name)** (ported 2026-08-06): two origins may each provide a tool of the
//! same name and both stay addressable; a repeat registration by the SAME
//! origin is [`RegistryErrorKind::Duplicate`]. Bare-name lookup goes
//! through [`ToolRegistry::lookup_unique`], which refuses an ambiguous
//! name rather than guessing an origin. Registration also verifies that
//! the identity's descriptor digest matches the descriptor itself. The [`CatalogSchemaValidator`] trait is the abstract seam for
//! registration-time schema validation — the caller plugs in their own
//! validation logic (e.g. JSON Schema conformance, size limits, capability
//! checks) without this crate knowing about C1 authz or runtime internals.

use crate::identity::{CanonicalDescriptor, ToolIdentity, ToolOrigin};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// A fully-registered tool definition: identity + descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDef {
    pub identity: ToolIdentity,
    pub descriptor: CanonicalDescriptor,
    /// RC1-R: what the tool does, in words the model reads (the provider
    /// request's `description`). Outside the descriptor and its digest: it
    /// never changes the tool's identity, grants or log records. Empty when
    /// nobody described the tool.
    #[serde(default)]
    pub description: String,
}

impl ToolDef {
    pub fn new(identity: ToolIdentity, descriptor: CanonicalDescriptor) -> Self {
        Self {
            identity,
            descriptor,
            description: String::new(),
        }
    }

    /// RC1-R: this def with `description` (see [`ToolDef::description`]).
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Convenience: build a def from parts, computing the digest automatically.
    pub fn from_parts(
        origin: ToolOrigin,
        name: impl Into<String>,
        descriptor: CanonicalDescriptor,
    ) -> Self {
        let digest = descriptor.digest();
        let identity = ToolIdentity::new(origin, name, digest);
        Self {
            identity,
            descriptor,
            description: String::new(),
        }
    }

    pub fn name(&self) -> &str {
        &self.identity.name
    }
}

/// Registry error classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryErrorKind {
    /// This (origin, name) pair is already registered — or a bare-name
    /// lookup was ambiguous across origins.
    Duplicate,
    /// No tool found for the requested name.
    Unknown,
    /// Schema validation failed at registration time.
    SchemaMismatch { detail: String },
    /// The origin is RESERVED (C2-u4: `builtin:*`): public registration
    /// under it is refused; only the crate-internal install path (e.g.
    /// [`crate::builtins::install`]) may register reserved-origin defs.
    ReservedOrigin,
}

/// A registry error with kind and detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryError {
    pub kind: RegistryErrorKind,
    pub name: String,
    pub detail: String,
}

impl RegistryError {
    pub fn duplicate(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind: RegistryErrorKind::Duplicate,
            name: name.into(),
            detail: detail.into(),
        }
    }

    pub fn unknown(name: impl Into<String>) -> Self {
        Self {
            kind: RegistryErrorKind::Unknown,
            name: name.into(),
            detail: String::new(),
        }
    }

    pub fn schema_mismatch(name: impl Into<String>, detail: impl Into<String>) -> Self {
        let d = detail.into();
        Self {
            kind: RegistryErrorKind::SchemaMismatch { detail: d.clone() },
            name: name.into(),
            detail: d,
        }
    }
}

/// Abstract registration-time schema validator.
///
/// This trait is the seam that keeps C1 authz and runtime concerns out of
/// `awili-tools`. The caller provides an implementation that can reject a
/// descriptor based on policy (schema conformance, size limits, capability
/// checks, etc.) without this crate needing to know about those policies.
pub trait CatalogSchemaValidator {
    /// Validate a descriptor at registration time.
    ///
    /// Return `Ok(())` if the descriptor is acceptable, or
    /// [`RegistryErrorKind::SchemaMismatch`] with a detail string explaining why.
    fn validate(&self, descriptor: &CanonicalDescriptor) -> Result<(), RegistryErrorKind>;
}

/// A no-op validator that accepts every descriptor.
///
/// Registration without a validator is unrepresentable (contract addendum
/// §5, Codex C2-u2 design review): a registry can only be built
/// `with_validator`. Accepting everything is therefore always an EXPLICIT,
/// visible policy choice at the construction site — tests and early
/// bring-up name this type; production code naming it would not survive
/// review.
#[derive(Clone, Debug)]
pub struct AllowAllValidator;

impl CatalogSchemaValidator for AllowAllValidator {
    fn validate(&self, _descriptor: &CanonicalDescriptor) -> Result<(), RegistryErrorKind> {
        Ok(())
    }
}

/// RC2-R51: the name that tells one origin's tool from another's of the same
/// bare name: the origin's key (`mcp:mac-shell` → `mac-shell`), a dot, the
/// tool's own name (`mac-shell.run_command`).
pub fn qualified_name(origin: &str, name: &str) -> String {
    let key = origin.split_once(':').map_or(origin, |(_, k)| k);
    format!("{key}.{name}")
}

/// RC2-R5M1 (slice review befb867d, P1): the one name each tool is
/// advertised under, allocated over the COMPLETE tool set so no two tools
/// share one. A bare name exactly one tool has is that tool's own; every
/// bare name in the set is reserved, so a tool whose bare name another
/// tool shares never takes a name some tool is literally called. A shared
/// tool gets its qualified name ([`qualified_name`],
/// `mac-shell.run_command`), or, when that is taken (a third server's tool
/// is literally named `mac-shell.run_command`), the first free numbered
/// one (`mac-shell.run_command-2`, `-3`, …). Deterministic: the tools are
/// taken in (origin, name) order. Keyed by (origin, name).
pub fn allocate_aliases<'a>(
    tools: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> BTreeMap<(String, String), String> {
    let tools: BTreeSet<(&str, &str)> = tools.into_iter().collect();
    let mut bare: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, n) in &tools {
        *bare.entry(n).or_default() += 1;
    }
    let mut taken: BTreeSet<String> = bare.keys().map(|n| n.to_string()).collect();
    let mut out = BTreeMap::new();
    for (o, n) in &tools {
        let alias = if bare[n] == 1 {
            n.to_string()
        } else {
            let base = qualified_name(o, n);
            let mut alias = base.clone();
            let mut k = 2usize;
            while taken.contains(&alias) {
                alias = format!("{base}-{k}");
                k += 1;
            }
            taken.insert(alias.clone());
            alias
        };
        out.insert((o.to_string(), n.to_string()), alias);
    }
    out
}

/// The tool registry: maps (authenticated origin, name) → [`ToolDef`].
///
/// PORTED (arbitration 2026-08-06): keying on name alone made origin
/// decorative — two connectors could not coexist, and a lookup could not
/// say WHOSE tool it returned. The key is now the authenticated origin
/// plus the name, so identity is authoritative; `lookup_unique` refuses
/// an ambiguous bare-name lookup rather than guessing.
pub struct ToolRegistry {
    /// Map from (origin, name) → ToolDef.
    by_name: HashMap<(String, String), ToolDef>,
    /// Registration-time schema validator. ALWAYS present — a registry
    /// without validation is unrepresentable (see [`AllowAllValidator`]).
    validator: Box<dyn CatalogSchemaValidator + Send + Sync>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("by_name", &self.by_name)
            .field("validator", &"<validator>")
            .finish()
    }
}

// NO `Clone` (ported, arbitration 2026-08-06): the previous impl copied
// the tool defs but SILENTLY DROPPED the validator, so a cloned registry
// accepted unvalidated registrations. Registration policy must never be
// lost by an implicit copy. Callers that need a detached copy take an
// explicit snapshot instead — which cannot register at all.

impl ToolRegistry {
    /// Create an empty registry with a schema validator. This is the ONLY
    /// constructor: registration without validation is unrepresentable
    /// (contract addendum §5). Explicitly-permissive policy is spelled
    /// [`AllowAllValidator`] at the construction site.
    pub fn with_validator<V: CatalogSchemaValidator + Send + Sync + 'static>(validator: V) -> Self {
        Self {
            by_name: HashMap::new(),
            validator: Box::new(validator),
        }
    }

    /// Register a tool def. Returns an error on duplicate, schema
    /// mismatch, or a RESERVED origin (`builtin:*` — those defs enter
    /// only through the crate-internal install path, so an untrusted
    /// caller cannot claim the builtin identity).
    pub fn register(&mut self, def: ToolDef) -> Result<(), RegistryError> {
        if def.identity.origin.origin.starts_with("builtin:") {
            return Err(RegistryError {
                kind: RegistryErrorKind::ReservedOrigin,
                name: def.name().to_string(),
                detail: format!(
                    "origin '{}' is reserved for builtin tools",
                    def.identity.origin.origin
                ),
            });
        }
        self.register_unrestricted(def)
    }

    /// Crate-internal registration that may use reserved origins. The
    /// ONLY callers are this crate's own install paths ([`crate::builtins`]).
    pub(crate) fn register_unrestricted(&mut self, def: ToolDef) -> Result<(), RegistryError> {
        // Schema validation is unconditional — the validator always exists.
        match self.validator.validate(&def.descriptor) {
            Err(RegistryErrorKind::SchemaMismatch { detail }) => {
                return Err(RegistryError::schema_mismatch(def.name(), detail));
            }
            Err(other) => {
                // Non-mismatch errors from validator are unexpected; wrap as detail.
                return Err(RegistryError::schema_mismatch(
                    def.name(),
                    format!("validator error: {other:?}"),
                ));
            }
            Ok(()) => {}
        }

        // Identity must MATCH the descriptor it claims (Codex C2-u1 P1:
        // ToolDef::new took an arbitrary digest, so a registered identity
        // could disagree with its own descriptor).
        if def.identity.descriptor_digest != def.descriptor.digest() {
            return Err(RegistryError::schema_mismatch(
                def.name(),
                "identity digest does not match the descriptor".to_string(),
            ));
        }
        let key = (def.identity.origin.origin.clone(), def.name().to_string());
        if self.by_name.contains_key(&key) {
            return Err(RegistryError::duplicate(
                def.name().to_string(),
                format!(
                    "tool '{}' already registered by origin '{}'",
                    def.name(),
                    def.identity.origin.origin
                ),
            ));
        }
        self.by_name.insert(key, def);
        Ok(())
    }

    /// Look up a tool by its FULL identity (origin + name).
    pub fn get(&self, origin: &str, name: &str) -> Result<&ToolDef, RegistryError> {
        self.by_name
            .get(&(origin.to_string(), name.to_string()))
            .ok_or_else(|| RegistryError::unknown(name))
    }

    /// Look up by bare name, but ONLY when exactly one origin provides it.
    /// An ambiguous name is an error, never a guess — the model names a
    /// tool, and silently picking one origin's implementation would be an
    /// authorization-relevant confusion.
    ///
    /// RC2-R51 (usability RC2 round 4, Major 3: three servers each offered
    /// `run_command`, so the model could call none of them): a name no tool
    /// has as its bare name may be a QUALIFIED one ([`qualified_name`],
    /// `mac-shell.run_command`), which names exactly one origin's tool.
    ///
    /// RC2-R5M1 (slice review befb867d, P1): an advertised name
    /// ([`ToolRegistry::aliases`]) resolves to exactly the tool it was
    /// allocated to, first. Before, a bare-name match won: a third server's
    /// tool literally named `mac-shell.run_command` took the call meant for
    /// mac-shell's `run_command`. A qualified name that is no tool's alias
    /// resolves only when exactly one tool has it.
    pub fn lookup_unique(&self, name: &str) -> Result<&ToolDef, RegistryError> {
        if let Some(def) = self
            .aliases()
            .into_iter()
            .find(|(_, alias)| alias == name)
            .and_then(|(key, _)| self.by_name.get(&key))
        {
            return Ok(def);
        }
        let mut hits = self.by_name.iter().filter(|((_, n), _)| n == name);
        let Some(first) = hits.next() else {
            let mut qualified = self
                .by_name
                .iter()
                .filter(|((o, n), _)| qualified_name(o, n) == name);
            let Some((_, def)) = qualified.next() else {
                return Err(RegistryError::unknown(name));
            };
            if qualified.next().is_some() {
                return Err(RegistryError::duplicate(
                    name.to_string(),
                    format!("tool '{name}' is provided by multiple origins; qualify it"),
                ));
            }
            return Ok(def);
        };
        if hits.next().is_some() {
            return Err(RegistryError::duplicate(
                name.to_string(),
                format!("tool '{name}' is provided by multiple origins; qualify it"),
            ));
        }
        Ok(first.1)
    }

    /// RC2-R5M1: the name each registered tool is advertised under, keyed by
    /// (origin, name) — [`allocate_aliases`] over the complete registered
    /// set, so every name is distinct and [`ToolRegistry::lookup_unique`]
    /// resolves it to exactly that tool.
    pub fn aliases(&self) -> BTreeMap<(String, String), String> {
        allocate_aliases(self.by_name.keys().map(|(o, n)| (o.as_str(), n.as_str())))
    }
    /// The registry-declared risk floor for a uniquely-named tool, in C1's
    /// canonical vocabulary — this is what `AuthzContext.registry_floor`
    /// receives (contract §3: registry metadata is the floor).
    ///
    /// RC2-ENGB2 (TOP43 #30): an unknown or ambiguous name is the lookup's
    /// own error, never `None` — "no floor" must not be mistaken for "no
    /// such tool" or "several tools, pick one".
    pub fn risk_floor(&self, name: &str) -> Result<awili_engine::RiskClass, RegistryError> {
        self.lookup_unique(name).map(|d| d.descriptor.risk_floor())
    }

    /// Remove a tool by full identity. Returns an error if unknown.
    pub fn remove(&mut self, origin: &str, name: &str) -> Result<ToolDef, RegistryError> {
        self.by_name
            .remove(&(origin.to_string(), name.to_string()))
            .ok_or_else(|| RegistryError::unknown(name))
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Iterate over all registered tool defs ((origin, name), def).
    pub fn iter(&self) -> impl Iterator<Item = (&(String, String), &ToolDef)> {
        self.by_name.iter()
    }

    /// An immutable snapshot of the registered defs. Replaces `Clone`:
    /// it carries no validator BECAUSE it cannot register anything.
    pub fn snapshot(&self) -> Vec<ToolDef> {
        let mut v: Vec<ToolDef> = self.by_name.values().cloned().collect();
        v.sort_by(|a, b| {
            (a.identity.origin.origin.as_str(), a.name())
                .cmp(&(b.identity.origin.origin.as_str(), b.name()))
        });
        v
    }

    /// Replace the schema validator. There is no way to REMOVE validation;
    /// the most permissive replacement is an explicit [`AllowAllValidator`].
    pub fn set_validator<V: CatalogSchemaValidator + Send + Sync + 'static>(
        &mut self,
        validator: V,
    ) {
        self.validator = Box::new(validator);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::CanonicalDescriptor;

    fn make_def(name: &str) -> ToolDef {
        ToolDef::from_parts(
            ToolOrigin::new("system"),
            name,
            CanonicalDescriptor {
                schema: serde_json::json!({"type": "object"}),
                side_effect: "none".into(),
                required_capabilities: vec![],
                executor: "builtin:test".into(),
            },
        )
    }

    #[test]
    fn register_and_get() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        let def = make_def("search");
        assert!(reg.register(def).is_ok());
        let got = reg.get("system", "search").unwrap();
        assert_eq!(got.name(), "search");
    }

    #[test]
    fn register_duplicate_error() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        assert!(reg.register(make_def("dup")).is_ok());
        let err = reg.register(make_def("dup")).unwrap_err();
        assert_eq!(err.kind, RegistryErrorKind::Duplicate);
        assert_eq!(err.name, "dup");
    }

    #[test]
    fn get_unknown_error() {
        let reg = ToolRegistry::with_validator(AllowAllValidator);
        let err = reg.get("system", "nonexistent").unwrap_err();
        assert_eq!(err.kind, RegistryErrorKind::Unknown);
        assert_eq!(err.name, "nonexistent");
    }

    #[test]
    fn remove_unknown_error() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        let err = reg.remove("system", "nonexistent").unwrap_err();
        assert_eq!(err.kind, RegistryErrorKind::Unknown);
    }

    #[test]
    fn remove_and_re_register() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        let def = make_def("t");
        assert!(reg.register(def).is_ok());
        let removed = reg.remove("system", "t").unwrap();
        assert_eq!(removed.name(), "t");
        assert!(reg.register(make_def("t")).is_ok());
    }

    #[test]
    fn validator_rejects_schema_mismatch() {
        struct Rejector;
        impl CatalogSchemaValidator for Rejector {
            fn validate(&self, _d: &CanonicalDescriptor) -> Result<(), RegistryErrorKind> {
                Err(RegistryErrorKind::SchemaMismatch {
                    detail: "rejected by test policy".into(),
                })
            }
        }

        let mut reg = ToolRegistry::with_validator(Rejector);
        let err = reg.register(make_def("bad")).unwrap_err();
        assert_eq!(
            err.kind,
            RegistryErrorKind::SchemaMismatch {
                detail: "rejected by test policy".into()
            }
        );
        assert!(err.detail.contains("rejected by test policy"));
    }

    #[test]
    fn allow_all_validator_passes() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        assert!(reg.register(make_def("ok")).is_ok());
    }

    #[test]
    fn registry_len_and_iter() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        reg.register(make_def("a")).unwrap();
        reg.register(make_def("b")).unwrap();
        assert!(!reg.is_empty());
        assert_eq!(reg.len(), 2);
        let names: Vec<String> = reg.iter().map(|((_, n), _)| n.clone()).collect();
        assert!(names.iter().any(|n| n == "a"));
        assert!(names.iter().any(|n| n == "b"));
    }
}

#[cfg(test)]
mod port_tests {
    use super::*;
    use crate::identity::{CanonicalDescriptor, ToolIdentity};
    use awili_engine::RiskClass;

    fn def(origin: &str, name: &str, side_effect: &str) -> ToolDef {
        let d = CanonicalDescriptor {
            schema: serde_json::json!({"type":"object"}),
            side_effect: side_effect.into(),
            required_capabilities: vec![],
            executor: "builtin".into(),
        };
        let digest = d.digest();
        ToolDef::new(ToolIdentity::new(ToolOrigin::new(origin), name, digest), d)
    }

    /// PORT: identity is (origin, name) — two origins may provide the same
    /// tool name, and each is retrievable as its own entry.
    #[test]
    fn origin_and_name_key_together() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        reg.register(def("connector:a", "search", "read_only"))
            .unwrap();
        reg.register(def("connector:b", "search", "read_only"))
            .unwrap();
        assert_eq!(reg.len(), 2);
        assert_eq!(
            reg.get("connector:a", "search")
                .unwrap()
                .identity
                .origin
                .origin,
            "connector:a"
        );
        // Same origin twice is still a duplicate.
        assert!(reg
            .register(def("connector:a", "search", "read_only"))
            .is_err());
    }

    /// An ambiguous bare-name lookup is refused, never guessed.
    #[test]
    fn ambiguous_bare_name_is_refused() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        reg.register(def("connector:a", "search", "read_only"))
            .unwrap();
        assert!(reg.lookup_unique("search").is_ok());
        reg.register(def("connector:b", "search", "read_only"))
            .unwrap();
        let e = reg.lookup_unique("search").unwrap_err();
        assert!(format!("{e:?}").contains("multiple origins"));
    }

    /// PORT: risk floors speak C1's vocabulary; legacy strings map
    /// conservatively and unknown values classify privileged.
    #[test]
    fn risk_floor_uses_canonical_vocabulary() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        reg.register(def("system", "reader", "read_only")).unwrap();
        reg.register(def("system", "writer", "write")).unwrap(); // legacy
        reg.register(def("system", "nuker", "dangerous")).unwrap(); // legacy
        reg.register(def("system", "mystery", "wat")).unwrap(); // unknown
        assert_eq!(reg.risk_floor("reader").ok(), Some(RiskClass::ReadOnly));
        assert_eq!(reg.risk_floor("writer").ok(), Some(RiskClass::Mutating));
        assert_eq!(reg.risk_floor("nuker").ok(), Some(RiskClass::Irreversible));
        assert_eq!(reg.risk_floor("mystery").ok(), Some(RiskClass::Privileged));
        assert_eq!(reg.risk_floor("absent").ok(), None);
    }

    /// RC2-ENGB2 (TOP43 #30): an ambiguous name keeps its refusal through
    /// `risk_floor` (before: `None`, as if no tool were there), and an
    /// unknown name says it is unknown.
    #[test]
    fn rc2engb2_an_ambiguous_name_keeps_its_refusal() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        reg.register(def("connector:a", "search", "read_only"))
            .unwrap();
        reg.register(def("connector:b", "search", "dangerous"))
            .unwrap();
        let ambiguous = format!("{:?}", reg.risk_floor("search"));
        assert!(ambiguous.contains("Duplicate"), "{ambiguous}");
        assert!(ambiguous.contains("multiple origins"), "{ambiguous}");
        let unknown = format!("{:?}", reg.risk_floor("absent"));
        assert!(unknown.contains("Unknown"), "{unknown}");
    }

    /// RC2-R51: a bare name two origins share stays refused, and each
    /// origin's tool is found by its qualified name; an unknown qualified
    /// name is unknown.
    #[test]
    fn rc2r51_a_qualified_name_finds_one_origins_tool() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        reg.register(def("mcp:mac-shell", "run_command", "dangerous"))
            .unwrap();
        reg.register(def("mcp:lightcraft", "run_command", "write"))
            .unwrap();
        assert!(reg.lookup_unique("run_command").is_err());
        let shell = reg.lookup_unique("mac-shell.run_command").unwrap();
        assert_eq!(shell.identity.origin.origin, "mcp:mac-shell");
        assert_eq!(shell.name(), "run_command");
        let light = reg.lookup_unique("lightcraft.run_command").unwrap();
        assert_eq!(light.identity.origin.origin, "mcp:lightcraft");
        assert_eq!(
            qualified_name("mcp:mac-shell", "run_command"),
            "mac-shell.run_command"
        );
        let unknown = format!("{:?}", reg.lookup_unique("photocraft.run_command"));
        assert!(unknown.contains("Unknown"), "{unknown}");
    }

    /// RC2-R5M1 (slice review befb867d, P1): mac-shell and lightcraft each
    /// offer `run_command`, and a third server offers a tool literally named
    /// `mac-shell.run_command`. Every tool gets its own advertised name, and
    /// each name resolves to exactly the tool it was given to — never the
    /// third server's tool for a call meant for mac-shell's.
    #[test]
    fn rc2r5m1_a_qualified_alias_never_collides_with_a_third_servers_tool() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        reg.register(def("mcp:mac-shell", "run_command", "dangerous"))
            .unwrap();
        reg.register(def("mcp:lightcraft", "run_command", "write"))
            .unwrap();
        reg.register(def("mcp:third", "mac-shell.run_command", "read_only"))
            .unwrap();
        let aliases = reg.aliases();
        assert_eq!(aliases.len(), 3, "{aliases:?}");
        let distinct: BTreeSet<&String> = aliases.values().collect();
        assert_eq!(
            distinct.len(),
            3,
            "every advertised name is distinct: {aliases:?}"
        );
        for ((origin, name), alias) in &aliases {
            let got = reg.lookup_unique(alias).unwrap();
            assert_eq!(
                (got.identity.origin.origin.as_str(), got.name()),
                (origin.as_str(), name.as_str()),
                "{alias} resolves to exactly its own tool"
            );
            assert_eq!(&got.identity, &reg.get(origin, name).unwrap().identity);
        }
        let shell = &aliases[&("mcp:mac-shell".to_string(), "run_command".to_string())];
        assert_eq!(shell, "mac-shell.run_command-2");
        assert_eq!(
            reg.risk_floor(shell).ok(),
            Some(RiskClass::Irreversible),
            "mac-shell's own floor, not the third server's"
        );
        let third = &aliases[&("mcp:third".to_string(), "mac-shell.run_command".to_string())];
        assert_eq!(
            third, "mac-shell.run_command",
            "a unique bare name is its own"
        );
        assert_eq!(
            reg.lookup_unique("mac-shell.run_command")
                .unwrap()
                .identity
                .origin
                .origin,
            "mcp:third"
        );
        let light = &aliases[&("mcp:lightcraft".to_string(), "run_command".to_string())];
        assert_eq!(light, "lightcraft.run_command");
        // The shared bare name stays refused, never guessed.
        assert!(reg.lookup_unique("run_command").is_err());
        // Registration order does not change the allocation.
        let mut rev = ToolRegistry::with_validator(AllowAllValidator);
        rev.register(def("mcp:third", "mac-shell.run_command", "read_only"))
            .unwrap();
        rev.register(def("mcp:lightcraft", "run_command", "write"))
            .unwrap();
        rev.register(def("mcp:mac-shell", "run_command", "dangerous"))
            .unwrap();
        assert_eq!(rev.aliases(), aliases);
    }

    /// RC2-R5M1: a numbered alias skips every name already taken, and a
    /// qualified name two tools share is refused, not guessed.
    #[test]
    fn rc2r5m1_numbered_aliases_skip_taken_names() {
        let aliases = allocate_aliases([
            ("mcp:a", "x"),
            ("mcp:b", "x"),
            ("mcp:c", "a.x"),
            ("mcp:d", "a.x-2"),
        ]);
        assert_eq!(aliases[&("mcp:a".into(), "x".into())], "a.x-3");
        assert_eq!(aliases[&("mcp:b".into(), "x".into())], "b.x");
        assert_eq!(aliases[&("mcp:c".into(), "a.x".into())], "a.x");
        assert_eq!(aliases[&("mcp:d".into(), "a.x-2".into())], "a.x-2");
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        reg.register(def("mcp:a", "b.c", "read_only")).unwrap();
        reg.register(def("mcp:a.b", "c", "write")).unwrap();
        // Both qualify to `a.b.c`, which is neither's alias.
        let e = format!("{:?}", reg.lookup_unique("a.b.c"));
        assert!(e.contains("Duplicate"), "{e}");
    }
    /// Registration refuses an identity whose digest disagrees with its
    /// descriptor (Codex C2-u1 P1).
    #[test]
    fn forged_digest_is_refused() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        let good = def("system", "a", "read_only");
        let other = def("system", "b", "irreversible");
        // Claim a's name/origin but carry b's digest.
        let forged = ToolDef::new(
            ToolIdentity::new(ToolOrigin::new("system"), "a", other.descriptor.digest()),
            good.descriptor.clone(),
        );
        assert!(
            reg.register(forged).is_err(),
            "digest mismatch must be refused"
        );
        assert!(reg.register(good).is_ok());
    }

    /// PORT: no Clone — a snapshot cannot register, so validation policy
    /// can never be lost by an implicit copy.
    #[test]
    fn snapshot_replaces_clone_and_cannot_register() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        reg.register(def("system", "a", "read_only")).unwrap();
        reg.register(def("system", "b", "mutating")).unwrap();
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 2);
        // Deterministic order (origin, name) — snapshots are comparable.
        assert_eq!(snap[0].name(), "a");
        assert_eq!(snap[1].name(), "b");
        // Compile-time property: ToolRegistry is not Clone. Asserted by
        // the absence of an impl; a `let _ = reg.clone();` here would fail
        // to compile, which is the guarantee we want.
    }
}
