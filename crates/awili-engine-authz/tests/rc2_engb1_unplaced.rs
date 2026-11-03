//! RC2-ENGB1 (TOP43 #17): a relative or home path for a tool that does not
//! resolve it inside the folder (an MCP server's own file tool) is shown to
//! the owner — never auto-allowed, granted or auto-approved — while the
//! engine's own file tools keep resolving relative paths in the folder.
//! One test fn: the fixture's issuance is process-wide.

use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};
use awili_engine_authz::{Authorizer, AuthzContext, Decision, GrantSet};
use awili_engine_log::authz::RiskClass;
use awili_engine_log::test_bundle;

const FOLDER: &str = "/Users/example/work";

fn call(tool: &str, args: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId("t1-call-0000".into()),
        provider_call_id: None,
        name: tool.into(),
        arguments: args.into(),
    }
}

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
fn rc2engb1_an_mcp_tools_relative_path_is_shown_to_the_owner() {
    let bundle = test_bundle();
    let gate = bundle.0.clone();
    let empty = GrantSet::load(&gate, &[], &[]);
    let authorizer = Authorizer::new(gate);
    let mint = |origin: &str, name: &str| {
        bundle.2.mint(ToolIdentity::new(
            ToolOrigin::new(origin),
            name,
            DescriptorDigest([0u8; 32]),
        ))
    };
    let decide = |origin: &str, tool: &str, floor: RiskClass, args: &str| {
        let resolved = mint(origin, tool);
        let c = call(tool, args);
        outcome(authorizer.authorize(
            &c,
            AuthzContext {
                turn_id: "t1",
                session_id: "s1",
                folder_root: Some(FOLDER),
                grants: &empty,
                registry_floor: Some(floor),
                resolved_tool: &resolved,
                install_witness: None,
            },
        ))
    };
    let mcp_read = |args: &str| decide("mcp:srv", "read_file", RiskClass::ReadOnly, args);
    let mcp_write = |args: &str| decide("mcp:srv", "write_file", RiskClass::Mutating, args);

    // An MCP read tool: a home or relative path cannot be placed in the
    // folder — the owner sees it (before: allowed without asking).
    assert_eq!(mcp_read(r#"{"path":"~/notes.txt"}"#), "ask (floor)");
    assert_eq!(mcp_read(r#"{"path":"notes.txt"}"#), "ask (floor)");
    assert_eq!(mcp_read(r#"{"file_path":"docs/a.md"}"#), "ask (floor)");
    assert_eq!(
        mcp_read(r#"{"paths":["/Users/example/work/a","b"]}"#),
        "ask (floor)"
    );
    assert_eq!(mcp_read(r#"{"dir":"$HOME/Documents"}"#), "ask (floor)");
    assert_eq!(mcp_read(r#"{"source":"notes.txt"}"#), "ask (floor)");
    assert_eq!(mcp_read(r#"{"anything":"./x"}"#), "ask (floor)");
    // An absolute path inside the folder is placed; free text is not a path.
    assert_eq!(
        mcp_read(r#"{"path":"/Users/example/work/notes.txt"}"#),
        "allow"
    );
    assert_eq!(
        mcp_read(r#"{"query":"rc2 eng/sandbox","limit":5}"#),
        "allow"
    );
    assert_eq!(mcp_read(r#"{"source":"awili","to":"a@b.org"}"#), "allow");
    assert_eq!(mcp_read(r#"{"content":"~ about 5 of them"}"#), "allow");
    // An absolute path outside still asks (read) or is refused (write).
    assert_eq!(mcp_read(r#"{"path":"/etc/hosts"}"#), "ask (floor)");
    assert_eq!(mcp_write(r#"{"path":"/etc/hosts","content":"x"}"#), "deny");

    // An MCP write tool: a home path is a floor dialog (before: an ordinary
    // Ask that auto mode or a grant could clear), never a refusal.
    assert_eq!(
        mcp_write(r#"{"path":"~/x.txt","content":"x"}"#),
        "ask (floor)"
    );
    assert_eq!(
        mcp_write(r#"{"content":"see ~/x and a/b","subject":"awili/rc2"}"#),
        "ask"
    );

    // The engine's own file tools resolve relative paths in the folder:
    // unchanged.
    let fs = |tool: &str, floor: RiskClass, args: &str| decide("builtin:fs", tool, floor, args);
    assert_eq!(
        fs("fs.read", RiskClass::ReadOnly, r#"{"path":"notes.md"}"#),
        "allow"
    );
    assert_eq!(
        fs(
            "fs.write",
            RiskClass::Mutating,
            r#"{"path":"out.txt","content":"x"}"#
        ),
        "ask"
    );
    // The call's NAME does not make a tool the builtin: an MCP tool called
    // `fs.read` is judged by its origin.
    assert_eq!(
        decide(
            "mcp:srv",
            "fs.read",
            RiskClass::ReadOnly,
            r#"{"path":"notes.md"}"#
        ),
        "ask (floor)"
    );
}
