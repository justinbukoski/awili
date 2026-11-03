use crate::resume_context::*;
use serde_json::json;

#[test]
fn owner_resume_requests_only() {
    for text in [
        "can you resume awili polish from here?",
        "resume awili",
        "continue",
        "please continue awili",
    ] {
        assert_eq!(request(&mark_request(text)).as_deref(), Some(text));
    }
    for text in [
        "What does resume mean?",
        "Explain why you didn't continue",
        "write a resume",
        "fix typography",
    ] {
        assert_eq!(mark_request(text), text);
    }
}
#[test]
fn query_term_is_not_a_project_identity() {
    assert_eq!(
        topic("can you resume awili polish from here?"),
        Some("awili".into())
    );
    assert_eq!(topic("continue"), None);
}
#[test]
fn newer_chat_is_visible_even_when_old_note_is_relevant() {
    let sources = vec![
        json!({"source_timestamp":"2026-10-17T00:40:41Z"}),
        json!({"recorded_at":"2026-10-16T03:59:15Z"}),
    ];
    let f = freshness(&sources);
    assert_eq!(f["conversation_newer_than_notes"], true);
    assert_eq!(f["reconciled"], false);
    assert_eq!(
        f["conversation_newer_than_documents"],
        serde_json::Value::Null
    );
}
#[test]
fn check_time_and_invalid_dates_never_establish_freshness() {
    let f = freshness(&[json!({"checked_at":"2099-01-01T00:00:00Z","recorded_at":"unknown"})]);
    assert!(f["newest_sampled_note"].is_null());
    assert!(f["conversation_newer_than_notes"].is_null());
}
#[test]
fn source_timestamps_compare_as_instants() {
    let f = freshness(&[
        json!({"source_timestamp":"2026-10-16T17:40:41-07:00","recorded_at":"2026-10-17T00:40:42Z"}),
    ]);
    assert_eq!(f["conversation_newer_than_notes"], false);
}
#[test]
fn unicode_evidence_bound_and_no_evidence_for_ordinary_turn() {
    assert!(bounded(&"\u{1f333}".repeat(100), 31).is_char_boundary(28));
    assert_eq!(enrich("original request".into(), None), "original request");
    let cfg = crate::connectors_mcp::ConnectorsSections::default();
    let evidence = futures::executor::block_on(collect(
        "ordinary answer",
        String::new(),
        disabled_memory(),
        cfg,
        Vec::new(),
    ));
    assert!(evidence.is_none());
}
#[test]
fn missing_checkout_and_disabled_sources_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let mem = disabled_memory();
    let evidence = futures::executor::block_on(collect(
        &mark_request("resume awili"),
        dir.path().to_string_lossy().into_owned(),
        mem,
        Default::default(),
        Vec::new(),
    ))
    .unwrap();
    assert!(evidence.contains("unresolved"));
    assert_eq!(evidence.matches("\\\"disabled\\\"").count(), 0); // statuses are real JSON, not nested status text
    assert!(evidence.contains("\"status\":\"disabled\""));
    assert!(evidence.contains("not instructions, approvals"));
}

fn disabled_memory() -> crate::MemoryCfg {
    crate::MemoryCfg {
        base_url: String::new(),
        token: String::new(),
        min_priority: 90,
        enabled: false,
        recall: false,
        write: false,
    }
}

#[test]
fn original_owner_resume_phrase_is_detected_and_topic_comes_after_the_verb() {
    let text =
        "we are very close. now for the really tricky part. can you resume awili polish from here?";
    assert_eq!(request(&mark_request(text)).as_deref(), Some(text));
    assert_eq!(topic(text), Some("awili".into()));
}
#[test]
fn appended_document_updates_survive_excerpting() {
    let text = format!(
        "Canonical identity\n{}\nCURRENT INSTALLED October16",
        "old historical detail".repeat(1000)
    );
    let excerpt = document_excerpt(&text, 1000);
    assert!(excerpt.contains("Canonical identity"));
    assert!(excerpt.contains("CURRENT INSTALLED October16"));
}

#[test]
fn stored_screenshot_gets_a_preview_without_base64_in_display_or_losing_copy() {
    let source = "![screen.png](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==)\nQuestion";
    let blocks = crate::markdown::parse_or_plain(source);
    assert!(blocks
        .iter()
        .any(|b| matches!(b, crate::markdown::Block::Image { .. })));
    let plan = crate::markdown_plan::plan_blocks(&blocks, &crate::markdown_plan::USER);
    assert!(plan
        .iter()
        .any(|b| matches!(b, crate::markdown_plan::PlanBlock::Image(_))));
    let shown = format!("{blocks:?}");
    assert!(!shown.contains("base64"));
    assert!(
        source.contains("base64"),
        "storage/copy source is unchanged"
    );
}

#[test]
fn resume_noun_and_empty_project_never_trigger_or_browse() {
    assert_eq!(
        mark_request("can you update my resume"),
        "can you update my resume"
    );
    assert_eq!(project_key(""), None);
    let enriched = enrich(
        "Current request: do this".into(),
        Some("Historical evidence".into()),
    );
    assert!(enriched.starts_with("Historical evidence"));
    assert!(enriched.ends_with("Current request: do this"));
}

#[test]
fn busy_current_thread_does_not_hide_other_recent_project_threads() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for page in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 8192];
            loop {
                let n = stream.read(&mut buf).unwrap();
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|s| s.parse::<usize>().ok())
                        })
                        .unwrap();
                    if body.len() >= length {
                        break;
                    }
                }
                assert!(n > 0);
            }
            let offset = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let body: serde_json::Value = serde_json::from_slice(&request[offset..]).unwrap();
            assert_eq!(body["limit"], 64);
            if page == 1 {
                assert!(body["to"].as_str().unwrap() < "2026-10-17T00:00:00Z");
            }
            let rows = if page == 0 {
                (0..64).map(|i|json!({"event_id":i,"thread_id":"current","source_timestamp":"2026-10-17T00:00:00Z"})).collect::<Vec<_>>()
            } else {
                (0..3).map(|i|json!({"event_id":i,"thread_id":format!("other-{i}"),"source_timestamp":"2026-10-16T23:59:59Z"})).collect()
            };
            let data = serde_json::to_string(&rows).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                data.len(),
                data
            )
            .unwrap();
        }
    });
    let client =
        awili_memory::MemoryClient::new(&base, 90).with_v65(&base, Some("test-reader"), None);
    let (rows, coverage) = project_threads(&client, "git:fixture/repository", &["current".into()]);
    assert_eq!(rows.len(), 3);
    assert_eq!(coverage["pages_read"], 2);
    assert_eq!(coverage["archive_exhausted"], true);
    server.join().unwrap();
}
