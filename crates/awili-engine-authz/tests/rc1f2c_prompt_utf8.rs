//! RC1-F2c (F0552): an approval prompt over a long argument with multi-byte
//! characters at the cut is built without a panic, in characters.
//! One test fn: the fixture's issuance is process-wide.

use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};
use awili_engine_authz::{Authorizer, AuthzContext, Decision, GrantSet};
use awili_engine_log::test_bundle;

#[test]
fn rc1f2c_approval_prompt_cuts_multibyte_arguments_safely() {
    let bundle = test_bundle();
    let gate = bundle.0.clone();
    let empty = GrantSet::load(&gate, &[], &[]);
    let authorizer = Authorizer::new(gate);
    let resolved = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("builtin:fs"),
        "fs",
        DescriptorDigest([0u8; 32]),
    ));
    // Byte 117 of each argument string falls inside a multi-byte character:
    // "é" is 2 bytes, "€" 3, "🙂" 4 (the JSON prefix below is 31 bytes).
    for filler in ["é", "€", "🙂", "a€"] {
        let content = filler.repeat(200);
        let args = format!(r#"{{"path":"out.txt","content":"{content}"}}"#);
        let call = ToolCall {
            awili_call_id: AwiliCallId("t1-call-0001".into()),
            provider_call_id: None,
            name: "fs.write".into(),
            arguments: args.clone(),
        };
        let d = authorizer.authorize(
            &call,
            AuthzContext {
                turn_id: "t1",
                session_id: "s1",
                folder_root: Some("/Users/example/work"),
                grants: &empty,
                registry_floor: None,
                resolved_tool: &resolved,
                install_witness: None,
            },
        );
        let Decision::Ask(pending) = d else {
            panic!("a write asks");
        };
        let prompt = pending.prompt();
        let summary = prompt
            .strip_prefix("fs.write [")
            .and_then(|rest| rest.split_once("] "))
            .map(|(_, summary)| summary)
            .expect("name [risk] summary");
        assert!(summary.ends_with("..."), "{filler}: {summary}");
        assert_eq!(
            summary.chars().count(),
            120,
            "{filler}: 117 characters + ..."
        );
        let kept: String = args.chars().take(117).collect();
        assert_eq!(summary, format!("{kept}..."), "{filler}");
    }
}
