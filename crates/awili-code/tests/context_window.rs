//! rc.3 S2: the context window each CLI reports for its own turn.

use awili_code::app_server::{AppServerTurn, TurnSpec};
use awili_code::{Autonomy, CodeEngine, CodeEvent};
use awili_providers::ProviderEvent;
use serde_json::json;

fn windows(events: &[CodeEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|e| match e {
            CodeEvent::ContextWindow(w) => Some(*w),
            _ => None,
        })
        .collect()
}

#[test]
fn claude_result_reports_the_models_context_window() {
    let line = r#"{"type":"result","subtype":"success","result":"ok","usage":{"input_tokens":3,"output_tokens":4},"modelUsage":{"claude-haiku-4-5-20251001":{"inputTokens":3,"outputTokens":4,"contextWindow":1000000,"maxOutputTokens":128000}}}"#;
    let events = CodeEngine::Claude.parse_line(line);
    assert_eq!(windows(&events), vec![1_000_000]);
    assert_eq!(
        events
            .iter()
            .filter_map(CodeEvent::to_provider_event)
            .find(|e| matches!(e, ProviderEvent::ContextWindow(_))),
        Some(ProviderEvent::ContextWindow(1_000_000))
    );
}

#[test]
fn claude_result_without_model_usage_reports_no_window() {
    let line = r#"{"type":"result","subtype":"success","result":"ok","usage":{"input_tokens":3,"output_tokens":4}}"#;
    assert!(windows(&CodeEngine::Claude.parse_line(line)).is_empty());
}

#[test]
fn codex_token_count_reports_model_context_window() {
    for line in [
        r#"{"type":"token_count","info":{"model_context_window":475000}}"#,
        r#"{"type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":475000}}}"#,
        r#"{"id":"1","msg":{"type":"token_count","info":{"model_context_window":475000}}}"#,
    ] {
        assert_eq!(
            windows(&CodeEngine::Codex.parse_line(line)),
            vec![475_000],
            "{line}"
        );
    }
}

#[test]
fn codex_app_server_reports_the_window_with_the_turn_end() {
    let mut turn = AppServerTurn::new(TurnSpec {
        cwd: "/work/folder".into(),
        prompt: "Say hi".into(),
        resume_id: None,
        model: None,
        autonomy: Autonomy::Autonomous,
        host_approvals: false,
    });
    turn.start();
    turn.on_line(r#"{"id":1,"result":{"userAgent":"awili/0.160.1","codexHome":"/h/.codex","platformFamily":"unix","platformOs":"macos"}}"#);
    turn.on_line(r#"{"id":2,"result":{"thread":{"id":"thr-1","sessionId":"thr-1"},"model":"gpt-6.1-sol","approvalPolicy":"never","sandbox":{"type":"dangerFullAccess"}}}"#);
    turn.on_line(r#"{"id":3,"result":{"turn":{"id":"turn-1","status":"inProgress"}}}"#);
    turn.on_line(
        &json!({"method":"thread/tokenUsage/updated","params":{"threadId":"thr-1","turnId":"turn-1",
            "tokenUsage":{"total":{"inputTokens":10,"outputTokens":2},"modelContextWindow":475000}}})
        .to_string(),
    );
    let end = turn.on_line(r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}"#);
    assert_eq!(windows(&end.events), vec![475_000]);
}

const MIXED: &str = r#"{"type":"result","subtype":"success","result":"ok","modelUsage":{"claude-haiku-4-5-20251001":{"contextWindow":200000},"claude-opus-4-8":{"contextWindow":1000000}}}"#;

#[test]
fn a_mixed_model_result_reports_the_window_of_the_model_that_ran_the_turn() {
    // By id and by alias: the primary model's own entry, not the biggest.
    assert_eq!(
        windows(&CodeEngine::Claude.parse_line_for_model(MIXED, Some("claude-haiku-4-5-20251001"))),
        vec![200_000]
    );
    assert_eq!(
        windows(&CodeEngine::Claude.parse_line_for_model(MIXED, Some("haiku"))),
        vec![200_000]
    );
    assert_eq!(
        windows(&CodeEngine::Claude.parse_line_for_model(MIXED, Some("opus"))),
        vec![1_000_000]
    );
    // Through the stream parser, as the app runs it.
    let mut p =
        awili_code::stream::StreamParser::new(CodeEngine::Claude).with_model(Some("haiku".into()));
    assert_eq!(windows(&p.parse_line(MIXED)), vec![200_000]);
}

#[test]
fn an_unmatched_model_reports_the_smallest_window_never_the_largest() {
    for model in [None, Some("default"), Some("mystery-model")] {
        assert_eq!(
            windows(&CodeEngine::Claude.parse_line_for_model(MIXED, model)),
            vec![200_000],
            "{model:?}"
        );
    }
}

#[test]
fn malformed_or_empty_model_usage_reports_no_window() {
    for line in [
        r#"{"type":"result","modelUsage":{}}"#,
        r#"{"type":"result","modelUsage":null}"#,
        r#"{"type":"result","modelUsage":[1,2]}"#,
        r#"{"type":"result","modelUsage":"x"}"#,
        r#"{"type":"result","modelUsage":{"m":{}}}"#,
        r#"{"type":"result","modelUsage":{"m":{"contextWindow":"1000000"}}}"#,
        r#"{"type":"result","modelUsage":{"m":{"contextWindow":-5}}}"#,
        r#"{"type":"result","modelUsage":{"m":{"contextWindow":0}}}"#,
        r#"{"type":"result","modelUsage":{"m":{"contextWindow":null}}}"#,
        r#"{"type":"result","modelUsage":{"m":5}}"#,
    ] {
        assert!(
            windows(&CodeEngine::Claude.parse_line(line)).is_empty(),
            "{line}"
        );
    }
    // One bad entry does not hide a good one.
    let line = r#"{"type":"result","modelUsage":{"a":{"contextWindow":"x"},"b":{"contextWindow":300000}}}"#;
    assert_eq!(windows(&CodeEngine::Claude.parse_line(line)), vec![300_000]);
}

#[test]
fn codex_non_numeric_or_zero_window_reports_nothing() {
    for line in [
        r#"{"type":"token_count","info":{"model_context_window":"475000"}}"#,
        r#"{"type":"token_count","info":{"model_context_window":0}}"#,
        r#"{"type":"token_count","info":{"model_context_window":null}}"#,
        r#"{"type":"token_count","info":{}}"#,
        r#"{"type":"token_count"}"#,
    ] {
        assert!(
            windows(&CodeEngine::Codex.parse_line(line)).is_empty(),
            "{line}"
        );
    }
}
