use awili_code::{CodeEngine, CodeEvent};
use serde_json::json;

fn diagnostic(denials: serde_json::Value) -> String {
    let line = json!({"type":"result", "subtype":"error_during_execution", "is_error":true,
        "result":"Authorization: Bearer result-secret", "permission_denials":denials})
    .to_string();
    let events = CodeEngine::Claude.parse_line(&line);
    assert!(!events.iter().any(|e| matches!(e, CodeEvent::TurnComplete)));
    events
        .into_iter()
        .find_map(|e| match e {
            CodeEvent::Error(message) => Some(message),
            _ => None,
        })
        .expect("a denial remains a failed turn")
}

#[test]
fn denial_names_executor_and_tools_without_echoing_inputs() {
    let message = diagnostic(json!([
        {"tool_name":"Bash", "tool_input":{"command":"secret-command"}},
        {"tool_name":"Read", "reason":"Bearer reason-secret"},
        {"tool_name":"Bash", "tool_input":"duplicate-secret"}
    ]));
    assert!(message.contains("Claude CLI permission policy"));
    assert!(message.contains("Bash, Read"));
    assert_eq!(message.matches("Bash").count(), 1);
    assert!(message.contains("terminal result did not confirm success"));
    for secret in [
        "secret-command",
        "reason-secret",
        "result-secret",
        "duplicate-secret",
    ] {
        assert!(!message.contains(secret));
    }
}

#[test]
fn malformed_names_are_omitted_and_output_is_bounded() {
    let message = diagnostic(json!([
        {"tool_name":""}, {"tool_name":42}, {"tool_name":"Authorization: Bearer bad-secret"},
        {"tool":"Read"}, {"tool_name":"A"}, {"tool_name":"B"}, {"tool_name":"C"},
        {"tool_name":"D"}, {"tool_name":"X".repeat(10000)}
    ]));
    assert!(message.len() < 420);
    assert!(message.contains("other tools"));
    assert!(!message.contains("bad-secret"));
    let empty = diagnostic(json!([{}, null, {"tool_name":"../../private"}]));
    assert!(empty.contains("a required tool"));
    assert!(!empty.contains("private"));
}

#[test]
fn historical_denials_require_affirmative_terminal_success() {
    for (metadata, success) in [
        (json!({"subtype":"success","is_error":false}), true),
        (json!({"subtype":"success","is_error":true}), false),
        (
            json!({"subtype":"error_during_execution","is_error":false}),
            false,
        ),
        (json!({"subtype":"success"}), false),
        (json!({"is_error":false}), false),
        (json!({"subtype":"success","is_error":"false"}), false),
        (json!({}), false),
    ] {
        let mut value = metadata;
        value["type"] = json!("result");
        value["result"] = json!("I recovered secret-result");
        value["permission_denials"] = json!([
            {"tool_name":"Bash", "tool_input":"secret-input"},
            {"tool_name":"Authorization: Bearer secret-name"},
            {"tool_name":"Read"}, {"tool_name":"Bash"}
        ]);
        let events = CodeEngine::Claude.parse_line(&value.to_string());
        assert_eq!(
            events.iter().any(|e| matches!(e, CodeEvent::TurnComplete)),
            success
        );
        assert_eq!(
            events.iter().any(|e| matches!(e, CodeEvent::Error(_))),
            !success
        );
        if success {
            use awili_providers::ProviderEvent;
            assert_eq!(
                events[0].to_provider_event(),
                Some(ProviderEvent::ToolAttemptFailed)
            );
            let Some(ProviderEvent::Activity(detail)) = events[1].to_provider_event() else {
                panic!("missing bounded warning detail")
            };
            assert!(detail.contains("Bash, Read"));
            assert!(detail.len() < 420);
            assert!(!detail.contains("Blocked"));
            assert_eq!(events.last(), Some(&CodeEvent::TurnComplete));
        }
        assert!(!format!("{events:?}").contains("secret-"));
        assert!(!format!("{events:?}").contains("automatic recovery"));
    }
}
