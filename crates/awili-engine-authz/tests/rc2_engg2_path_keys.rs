//! RC2-ENGG2 (RC2-ENGALL slice review P2): an argument key counts as naming
//! a path only by its words — split on `_`, `-` and camelCase — never by a
//! fragment inside another word. `to` named `topic`, `token`, `tool` and
//! `photo` a path, so `{"topic":"release.md"}` became a mandatory approval
//! and `{"topic":"token rotation"}` a privileged one. Real path keys still
//! prompt. One test fn: the fixture's issuance is process-wide.

use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};
use awili_engine_authz::{Authorizer, AuthzContext, Decision, GrantSet};
use awili_engine_log::authz::RiskClass;
use awili_engine_log::test_bundle;

const FOLDER: &str = "/Users/example/work";

/// What the authorizer decided: "allow", "ask", "ask (floor)" or "deny".
fn outcome(d: Decision) -> &'static str {
    match d {
        Decision::Allow { .. } => "allow",
        Decision::Ask(p) if p.floor_required() => "ask (floor)",
        Decision::Ask(_) => "ask",
        Decision::Deny(_) => "deny",
    }
}

#[test]
fn rc2engg2_ordinary_text_fields_are_not_path_keys() {
    let bundle = test_bundle();
    let gate = bundle.0.clone();
    let empty = GrantSet::load(&gate, &[], &[]);
    let authorizer = Authorizer::new(gate);
    let resolved = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("mcp:srv"),
        "search",
        DescriptorDigest([0u8; 32]),
    ));
    let mcp_read = |args: &str| {
        let c = ToolCall {
            awili_call_id: AwiliCallId("t1-call-0000".into()),
            provider_call_id: None,
            name: "search".into(),
            arguments: args.into(),
        };
        outcome(authorizer.authorize(
            &c,
            AuthzContext {
                turn_id: "t1",
                session_id: "s1",
                folder_root: Some(FOLDER),
                grants: &empty,
                registry_floor: Some(RiskClass::ReadOnly),
                resolved_tool: &resolved,
                install_witness: None,
            },
        ))
    };

    // Ordinary text fields: no dialog (before: every one was a floor
    // dialog, a `to` inside the key's word naming it a path).
    for args in [
        r#"{"topic":"release.md"}"#,
        r#"{"topic":"token rotation"}"#,
        r#"{"token":"abc.def"}"#,
        r#"{"tool":"notes.txt"}"#,
        r#"{"photoCaption":"beach.jpg"}"#,
        r#"{"history_note":"v1.2"}"#,
        r#"{"protocol":"v2.json"}"#,
        r#"{"targetURL":"a/b.html"}"#,
    ] {
        assert_eq!(mcp_read(args), "allow", "{args}");
    }

    // Real path keys still prompt: a whole word, any separator or case,
    // a plural, or a word ending in a path noun.
    for args in [
        r#"{"to":"notes.txt"}"#,
        r#"{"from":"a/b"}"#,
        r#"{"toPath":"a.md"}"#,
        r#"{"to_file":"a.md"}"#,
        r#"{"src-dir":"a/b"}"#,
        r#"{"outputFile":"x.txt"}"#,
        r#"{"OutputDir":"build/out"}"#,
        r#"{"inputs":["x.md"]}"#,
        r#"{"filepath":"x.md"}"#,
        r#"{"workdir":"sub/dir"}"#,
        r#"{"outfile":"x.log"}"#,
    ] {
        assert_eq!(mcp_read(args), "ask (floor)", "{args}");
    }
}
