//! RC1-F4b required proof: in-app ("On this Mac") runs book their tokens as
//! LOCAL when the configured engine is at home; any other engine keeps the
//! P8b "unknown" booking. Offline, temp dirs only.

use awili_code::CodeEngine;
use awili_providers::Usage;

use super::graduation_record_tests::{agent, now, panel, temp_ledger};
use super::*;

/// An in-app run that ended with `status` and reported `usage`.
fn in_app(status: RunStatus, usage: Option<Usage>) -> RunPanel {
    let mut p = panel(
        Some(WorkRunIdentity::OwnedEngine {
            model: "gemma-4-26b".into(), engine_id: None,
        }),
        Some(status),
    );
    p.usage = usage;
    if status == RunStatus::Failed {
        p.error = Some("model server refused".into());
    }
    p
}

fn usage(i: u32, o: u32) -> Option<Usage> {
    Some(Usage {
        cached_input_tokens: None,
        input_tokens: i,
        output_tokens: o,
    })
}

/// One recorded entry for `p` with the engine at `base_url`.
fn record_with(base_url: &str, tag: &str, p: &RunPanel) -> LedgerEntry {
    let mut rec = Recorder::from_config(&temp_ledger(tag));
    rec.set_engine_url(base_url);
    rec.arm("conv-1");
    rec.observe(p, now()).expect("recorded").expect("ok")
}

#[test]
fn rc1f4b_in_app_tokens_are_local_only_on_a_home_engine() {
    let p = in_app(RunStatus::Completed, usage(1_371, 71));
    for url in [
        "http://10.77.68.94:8844",
        "http://127.0.0.1:8851/v1",
        "http://hoste.local:8844",
        "http://hosta:8000",
    ] {
        let e = record_with(url, "f4b-home", &p);
        assert_eq!(e.executor, "owned", "{url}");
        assert_eq!((e.frontier_tokens, e.local_tokens), (0, 1_442), "{url}");
        assert_eq!(e.notes, None, "{url}");
    }
    for url in ["https://api.deepseek.com/v1", "http://8.8.8.8:8000", ""] {
        let e = record_with(url, "f4b-away", &p);
        assert_eq!(
            (e.frontier_tokens, e.local_tokens, e.notes),
            (0, 0, None),
            "not at home: today's booking ({url:?})"
        );
    }
    // A home engine that reported nothing says so, like any local run.
    let silent = in_app(RunStatus::Completed, None);
    let e = record_with("http://10.77.68.94:8844", "f4b-silent", &silent);
    assert_eq!((e.frontier_tokens, e.local_tokens), (0, 0));
    assert_eq!(e.notes.as_deref(), Some("tokens not reported by owned"));
    // Other executors are untouched by the in-app locality.
    let claude = {
        let mut p = panel(
            Some(agent(CodeEngine::Claude, Some("haiku"))),
            Some(RunStatus::Completed),
        );
        p.usage = usage(100, 10);
        p
    };
    let e = record_with("http://10.77.68.94:8844", "f4b-claude", &claude);
    assert_eq!((e.frontier_tokens, e.local_tokens), (110, 0));
}

/// A frontier retry after a failed home in-app run is a Fallback (the local
/// class reaches the fallback detector), and stays Success when the engine
/// is not at home.
#[test]
fn rc1f4b_frontier_retry_after_a_home_in_app_failure_is_a_fallback() {
    for (url, fallback) in [
        ("http://10.77.68.94:8844", true),
        ("https://api.deepseek.com/v1", false),
    ] {
        let mut rec = Recorder::from_config(&temp_ledger("f4b-fallback"));
        rec.set_engine_url(url);
        rec.arm("c");
        rec.observe(&in_app(RunStatus::Failed, None), now())
            .expect("recorded")
            .expect("ok");
        rec.arm("c");
        let retry = panel(
            Some(agent(CodeEngine::Claude, None)),
            Some(RunStatus::Completed),
        );
        let e = rec.observe(&retry, now()).expect("recorded").expect("ok");
        assert_eq!(
            matches!(&e.outcome, Outcome::Fallback { reason } if reason.contains("model server refused")),
            fallback,
            "{url}: {:?}",
            e.outcome
        );
    }
}

/// RC1-F4b-FIX1 (amended): the engine address reaches the recorder from the
/// worker spawn only (the endpoint that serves the turns), never from a
/// config load or re-apply. The behaviour is proven by
/// `work::local_first_tests::rc1f4b_fix1_locality_follows_the_live_worker_not_the_config`.
#[test]
fn rc1f4b_config_reaches_the_recorder() {
    let main = include_str!("main.rs");
    assert_eq!(main.matches("graduation.set_engine_url(").count(), 0);
    let work = include_str!("work.rs");
    assert_eq!(
        work.matches("self.work.graduation.set_engine_url(&engine.base_url);")
            .count(),
        1
    );
}
