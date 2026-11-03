//! Builtin filesystem tool definitions and STRICT argument parsing — pure.
//!
//! C2-u4 (build plan §u4): `fs.read/write/edit/glob/grep` as builtin tools.
//! This module is the PURE half: canonical `ToolDef`s under the reserved
//! authenticated origin [`origin`], and typed argument parsing. Execution
//! lives in the C2-owned IO crate `awili-builtins`, which routes every op
//! through the C3 `Sandbox` — nothing here touches the filesystem.
//!
//! Identity (Codex u4 design finding 6): builtins are keyed by the FULL
//! (origin, name) registry identity. An MCP server advertising `fs.read`
//! coexists under its own origin; `lookup_unique("fs.read")` then refuses
//! the ambiguous bare name rather than guessing, and routing must resolve
//! the builtin via [`origin`] + name. Risk floors come from the resolved
//! def's descriptor, never from a bare-name table.

use crate::identity::{CanonicalDescriptor, ToolOrigin};
use crate::registry::ToolDef;
use serde::Deserialize;

/// The reserved authenticated origin for builtin fs tools.
///
/// Reserved means: nothing else may register under it. The registry keeps
/// (origin, name) unique, so a second registration under this origin is a
/// Duplicate error; MCP origins are namespaced `mcp:*` by the runtime and
/// cannot collide with it.
pub fn origin() -> ToolOrigin {
    ToolOrigin::new("builtin:fs")
}

/// Install the five builtin fs defs into a registry — the ONLY path that
/// can register under the reserved `builtin:*` origin space
/// ([`crate::registry::ToolRegistry::register`] refuses it publicly, so an
/// untrusted registration cannot claim the builtin identity).
pub fn install(
    reg: &mut crate::registry::ToolRegistry,
) -> Result<(), crate::registry::RegistryError> {
    for d in defs() {
        reg.register_unrestricted(d)?;
    }
    Ok(())
}

/// Executor identifier carried in every builtin fs descriptor.
pub const EXECUTOR: &str = "builtin:fs";

fn def(name: &str, side_effect: &str, schema: serde_json::Value) -> ToolDef {
    ToolDef::from_parts(
        origin(),
        name,
        CanonicalDescriptor {
            schema,
            side_effect: side_effect.into(),
            required_capabilities: vec!["filesystem".into()],
            executor: EXECUTOR.into(),
        },
    )
    .with_description(description(name))
}

/// RC1-R (round 6: Gemma wrote with the owner's shell server's `write_file`,
/// which anchors a relative path to HOME, and asked "which folder?" for
/// "list the files in this folder"): what each builtin does, for the model.
fn description(name: &str) -> &'static str {
    match name {
        "fs.read" => "Read a text file in the Work folder. path is relative to the folder (its full path also works).",
        "fs.write" => "Create or overwrite a file in the Work folder with the given contents. path is relative to the folder (its full path also works). Use this, not another tool, for files in the Work folder.",
        "fs.edit" => "Replace the exact text find with replace in a file in the Work folder. path is relative to the folder.",
        "fs.glob" => "List files in the Work folder that match a pattern: \"*\" lists the folder's top level, \"notes/*.md\" a subfolder. Use this to see what is in the folder.",
        "fs.grep" => "Search the text of the files in the Work folder (or a subfolder given as under) for needle.",
        _ => "",
    }
}

/// The five builtin fs tool definitions (build plan §u4 surface — no
/// `fs.list`, no shell; C3c owns shell later).
///
/// Schemas mirror the parse structs below: `additionalProperties: false`
/// and every listed property required — the schema a provider sees and
/// the `deny_unknown_fields` parse enforce the SAME shape.
pub fn defs() -> Vec<ToolDef> {
    use serde_json::json;
    let obj = |props: serde_json::Value, req: &[&str]| {
        json!({
            "type": "object",
            "properties": props,
            "required": req,
            "additionalProperties": false,
        })
    };
    vec![
        def(
            "fs.read",
            "read_only",
            obj(
                json!({"path": {"type": "string", "description": "File path relative to the session root."}}),
                &["path"],
            ),
        ),
        def(
            "fs.write",
            "mutating",
            obj(
                json!({
                    "path": {"type": "string", "description": "File path relative to the session root."},
                    "contents": {"type": "string", "description": "Full new file contents."}
                }),
                &["path", "contents"],
            ),
        ),
        def(
            "fs.edit",
            "mutating",
            obj(
                json!({
                    "path": {"type": "string", "description": "File path relative to the session root."},
                    "find": {"type": "string", "description": "Exact substring to replace (must be non-empty)."},
                    "replace": {"type": "string", "description": "Replacement text."}
                }),
                &["path", "find", "replace"],
            ),
        ),
        def(
            "fs.glob",
            "read_only",
            obj(
                json!({"pattern": {"type": "string", "description": "Relative glob; `*` in the final component only (e.g. `docs/*.md`)."}}),
                &["pattern"],
            ),
        ),
        def(
            "fs.grep",
            "read_only",
            obj(
                json!({
                    "needle": {"type": "string", "description": "Fixed string to search for (non-empty)."},
                    "under": {"type": "string", "description": "Relative subdirectory to search; empty string for the whole root."}
                }),
                &["needle", "under"],
            ),
        ),
    ]
}

// -- strict typed arguments -------------------------------------------------
//
// `deny_unknown_fields` everywhere: an argument object with extra keys is
// SCHEMA-INVALID and becomes a definite failed dispatch downstream (design
// finding 7 — distinct from malformed JSON, which never reaches dispatch:
// B2's `ArgumentValidationV1::Malformed` keeps it NotDispatchable).

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    /// RC1-O: models often say `content`; it is the same argument. The
    /// advertised schema still names `contents` only.
    #[serde(alias = "content")]
    contents: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    find: String,
    replace: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobArgs {
    pattern: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepArgs {
    needle: String,
    under: String,
}

/// A parsed, typed builtin fs call. One variant per tool; constructing one
/// is only possible through [`BuiltinFsCall::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltinFsCall {
    Read {
        path: String,
    },
    Write {
        path: String,
        contents: String,
    },
    Edit {
        path: String,
        find: String,
        replace: String,
    },
    Glob {
        pattern: String,
    },
    Grep {
        needle: String,
        under: String,
    },
}

/// Why a call failed strict parsing. Bounded, display-safe.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuiltinFsParseError {
    /// The name is not one of the five builtin fs tools.
    #[error("unknown builtin fs tool: {0}")]
    UnknownTool(String),
    /// Valid JSON, wrong shape (missing/extra/mistyped fields). This is
    /// the schema-invalid case — a DEFINITE failed dispatch downstream.
    #[error("schema-invalid arguments for {tool}: {detail}")]
    SchemaInvalid { tool: String, detail: String },
    /// Not valid JSON at all. Callers behind B2's intent path never see
    /// this (Malformed stays NotDispatchable there); it exists so THIS
    /// function is total on raw strings rather than panicking.
    #[error("malformed JSON arguments for {tool}")]
    MalformedJson { tool: String },
}

const MAX_DETAIL_BYTES: usize = 200;

fn bounded(mut s: String) -> String {
    if s.len() > MAX_DETAIL_BYTES {
        s.truncate(
            (0..=MAX_DETAIL_BYTES)
                .rev()
                .find(|&i| s.is_char_boundary(i))
                .unwrap_or(0),
        );
    }
    s
}

impl BuiltinFsCall {
    /// Strictly parse `arguments` (a JSON object string) for builtin tool
    /// `name`. Total: never panics; every failure is a typed, bounded error.
    pub fn parse(name: &str, arguments: &str) -> Result<Self, BuiltinFsParseError> {
        fn shape<T: serde::de::DeserializeOwned>(
            tool: &str,
            arguments: &str,
        ) -> Result<T, BuiltinFsParseError> {
            // Malformed JSON and schema-invalid JSON are DIFFERENT failures
            // (design finding 7): probe validity first, then shape.
            let value: serde_json::Value = serde_json::from_str(arguments)
                .map_err(|_| BuiltinFsParseError::MalformedJson { tool: tool.into() })?;
            serde_json::from_value(value).map_err(|e| BuiltinFsParseError::SchemaInvalid {
                tool: tool.into(),
                detail: bounded(e.to_string()),
            })
        }
        match name {
            "fs.read" => {
                let a: ReadArgs = shape(name, arguments)?;
                Ok(Self::Read { path: a.path })
            }
            "fs.write" => {
                let a: WriteArgs = shape(name, arguments)?;
                Ok(Self::Write {
                    path: a.path,
                    contents: a.contents,
                })
            }
            "fs.edit" => {
                let a: EditArgs = shape(name, arguments)?;
                Ok(Self::Edit {
                    path: a.path,
                    find: a.find,
                    replace: a.replace,
                })
            }
            "fs.glob" => {
                let a: GlobArgs = shape(name, arguments)?;
                Ok(Self::Glob { pattern: a.pattern })
            }
            "fs.grep" => {
                let a: GrepArgs = shape(name, arguments)?;
                Ok(Self::Grep {
                    needle: a.needle,
                    under: a.under,
                })
            }
            other => Err(BuiltinFsParseError::UnknownTool(bounded(other.into()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AllowAllValidator, ToolRegistry};

    #[test]
    fn five_defs_register_with_stable_identity() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        let defs = defs();
        assert_eq!(defs.len(), 5);
        let names: Vec<&str> = defs.iter().map(|d| d.name()).collect();
        assert_eq!(
            names,
            ["fs.read", "fs.write", "fs.edit", "fs.glob", "fs.grep"]
        );
        for d in &defs {
            assert_eq!(d.identity.origin, origin());
            assert_eq!(d.descriptor.executor, EXECUTOR);
            // Digest determinism: rebuilding produces the same digest.
            assert_eq!(d.descriptor.digest(), d.identity.descriptor_digest);
        }
        install(&mut reg).expect("install builtins");
        // No shell tool exists anywhere in the builtin surface (C3c gate).
        assert!(reg.get("builtin:fs", "shell.exec").is_err());
        assert!(reg.get("builtin:fs", "fs.list").is_err());
    }

    /// RC1-R: every builtin says what it does; the description never enters
    /// the digest (identity, grants and logs are unchanged).
    #[test]
    fn rc1r_builtins_are_described_outside_the_digest() {
        for d in defs() {
            assert!(!d.description.is_empty(), "{}", d.name());
            assert!(d.description.contains("Work folder"), "{}", d.name());
            assert_eq!(d.identity.descriptor_digest, d.descriptor.digest());
            let bare = ToolDef::from_parts(origin(), d.name(), d.descriptor.clone());
            assert_eq!(bare.identity, d.identity, "the description is not identity");
        }
        let write = defs().into_iter().find(|d| d.name() == "fs.write").unwrap();
        let wire =
            crate::openai_tool_project_described(&write.descriptor, "fs.write", &write.description);
        assert_eq!(wire["function"]["description"], write.description.as_str());
        let glob = defs().into_iter().find(|d| d.name() == "fs.glob").unwrap();
        assert!(glob.description.contains("list"), "{}", glob.description);
    }

    #[test]
    fn rc1o_write_accepts_content_as_contents() {
        let a = BuiltinFsCall::parse("fs.write", r#"{"path":"a.txt","content":"one"}"#).unwrap();
        let b = BuiltinFsCall::parse("fs.write", r#"{"path":"a.txt","contents":"one"}"#).unwrap();
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
        // Both spellings at once is still a shape error, and so is a third name.
        assert!(matches!(
            BuiltinFsCall::parse("fs.write", r#"{"path":"a","content":"x","contents":"y"}"#),
            Err(BuiltinFsParseError::SchemaInvalid { .. })
        ));
        assert!(matches!(
            BuiltinFsCall::parse("fs.write", r#"{"path":"a","text":"x"}"#),
            Err(BuiltinFsParseError::SchemaInvalid { .. })
        ));
    }

    #[test]
    fn risk_floors_are_as_declared() {
        use awili_engine::RiskClass;
        for d in defs() {
            let floor = d.descriptor.risk_floor();
            match d.name() {
                "fs.read" | "fs.glob" | "fs.grep" => assert_eq!(floor, RiskClass::ReadOnly),
                "fs.write" | "fs.edit" => assert_eq!(floor, RiskClass::Mutating),
                other => panic!("unexpected builtin {other}"),
            }
        }
    }

    #[test]
    fn reserved_origin_is_actually_reserved() {
        use crate::registry::RegistryErrorKind;
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        // PUBLIC registration under builtin:* is refused — for the real
        // origin, for a homograph under the prefix, for any name.
        for org in ["builtin:fs", "builtin:fs2", "builtin:shell"] {
            let d = ToolDef::from_parts(
                ToolOrigin::new(org),
                "fs.read",
                CanonicalDescriptor {
                    schema: serde_json::json!({"type": "object"}),
                    side_effect: "read_only".into(),
                    required_capabilities: vec![],
                    executor: "evil".into(),
                },
            );
            let err = reg.register(d).expect_err("reserved origin must refuse");
            assert!(
                matches!(err.kind, RegistryErrorKind::ReservedOrigin),
                "{org}"
            );
        }
        // The install path still works, exactly once.
        install(&mut reg).expect("install");
        assert!(install(&mut reg).is_err(), "second install is a duplicate");
    }

    #[test]
    fn mcp_origin_cannot_shadow_the_builtin() {
        let mut reg = ToolRegistry::with_validator(AllowAllValidator);
        install(&mut reg).unwrap();
        // An MCP server also advertises `fs.read` under ITS origin.
        let imposter = ToolDef::from_parts(
            ToolOrigin::new("mcp:sneaky"),
            "fs.read",
            CanonicalDescriptor {
                schema: serde_json::json!({"type": "object"}),
                side_effect: "read_only".into(),
                required_capabilities: vec![],
                executor: "mcp".into(),
            },
        );
        reg.register(imposter).expect("distinct origin may coexist");
        // Bare-name lookup now REFUSES rather than guessing…
        assert!(reg.lookup_unique("fs.read").is_err());
        // …while the full identity still resolves the real builtin.
        let d = reg
            .get("builtin:fs", "fs.read")
            .expect("builtin resolves by full identity");
        assert_eq!(d.descriptor.executor, EXECUTOR);
        // And PUBLIC registration under the reserved origin is refused
        // outright (see reserved_origin_is_actually_reserved).
        let dup = defs().into_iter().next().unwrap();
        assert!(reg.register(dup).is_err());
    }

    #[test]
    fn parse_is_strict() {
        let ok = BuiltinFsCall::parse("fs.read", r#"{"path":"a.txt"}"#).unwrap();
        assert_eq!(
            ok,
            BuiltinFsCall::Read {
                path: "a.txt".into()
            }
        );

        // Extra key → schema-invalid.
        assert!(matches!(
            BuiltinFsCall::parse("fs.read", r#"{"path":"a","x":1}"#),
            Err(BuiltinFsParseError::SchemaInvalid { .. })
        ));
        // Mistyped field → schema-invalid.
        assert!(matches!(
            BuiltinFsCall::parse("fs.write", r#"{"path":3,"contents":"c"}"#),
            Err(BuiltinFsParseError::SchemaInvalid { .. })
        ));
        // Missing field → schema-invalid.
        assert!(matches!(
            BuiltinFsCall::parse("fs.edit", r#"{"path":"a","find":"x"}"#),
            Err(BuiltinFsParseError::SchemaInvalid { .. })
        ));
        // Malformed JSON is DISTINCT from schema-invalid.
        assert!(matches!(
            BuiltinFsCall::parse("fs.read", "{nope"),
            Err(BuiltinFsParseError::MalformedJson { .. })
        ));
        // Unknown tool.
        assert!(matches!(
            BuiltinFsCall::parse("fs.chmod", "{}"),
            Err(BuiltinFsParseError::UnknownTool(_))
        ));
    }

    #[test]
    fn schemas_match_parse_strictness() {
        // Every schema declares additionalProperties:false and requires
        // every property — the wire contract equals the parse contract.
        for d in defs() {
            let s = &d.descriptor.schema;
            assert_eq!(
                s["additionalProperties"],
                serde_json::json!(false),
                "{}",
                d.name()
            );
            let props: Vec<String> = s["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            let mut req: Vec<String> = s["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            req.sort();
            let mut props = props;
            props.sort();
            assert_eq!(props, req, "{}: every property must be required", d.name());
        }
    }
}
