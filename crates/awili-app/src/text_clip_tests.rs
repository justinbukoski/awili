//! RC1-I1 proofs (RC1 round 3b Blocker: SIGABRT `core::str::slice_error_fail`
//! in `work_right_panel` when the approval card's argument is "é€" + an emoji, ×40).
//! Every Work-surface place that shortened text by a byte index now uses
//! `text_clip::clip`; these tests cut "é€" + emoji text at every position and render
//! the real right panel with it.

use gpui::{px, size, TestAppContext, VisualTestContext};

use super::gateway_queue_tests::isolated;
use super::text_clip::clip;
use super::*;

/// "é", "€" and the emoji are 2, 3 and 4 bytes: with 0..=8 ASCII letters in front, a byte cut at
/// any fixed offset lands inside each of the three characters somewhere.
fn texts() -> Vec<String> {
    (0..9)
        .map(|pad| {
            format!(
                "{}{}",
                "a".repeat(pad),
                "\u{e9}\u{20ac}\u{1f600}".repeat(40)
            )
        })
        .collect()
}

/// `clip` keeps whole characters at every (limit, keep) the Work surface uses
/// and at every cut position.
#[test]
fn rc1i1_clip_never_splits_a_character() {
    for text in texts() {
        let n = text.chars().count();
        for (limit, keep) in [(60, 57), (50, 50), (80, 77)] {
            let out = clip(&text, limit, keep);
            let want: String = text.chars().take(keep).collect::<String>() + "\u{2026}";
            assert_eq!(out, want, "limit {limit} keep {keep}");
        }
        for keep in 0..n {
            let out = clip(&text, keep, keep);
            assert_eq!(out.chars().count(), keep + 1, "{keep}");
            assert!(text.starts_with(out.trim_end_matches('\u{2026}')));
        }
        assert_eq!(clip(&text, n, 1), text, "short enough: unchanged");
    }
    assert_eq!(clip("", 3, 1), "");
}

/// The subagent "agent done" lines (in-app and gateway) cut "é€" + emoji summaries
/// on characters.
#[test]
fn rc1i1_subagent_summaries_are_cut_on_characters() {
    for summary in texts() {
        let want = format!(
            "> *agent done: {}\u{2026}*\n\n",
            summary.chars().take(77).collect::<String>()
        );
        let ups = gateway_event_to_update(&super::tests::gw_event(
            "subagent.complete",
            serde_json::json!({ "id": "sa-1", "summary": summary }),
        ));
        assert!(
            matches!(&ups[0], WorkUpdate::Token(t) if *t == want),
            "gateway: {:?}",
            ups.first()
        );
        let facts = gateway_event_to_work_events(&super::tests::gw_event(
            "subagent.complete",
            serde_json::json!({ "id": "sa-1", "summary": summary }),
        ));
        let ups: Vec<WorkUpdate> = facts.into_iter().flat_map(work_event_to_updates).collect();
        assert!(
            ups.iter()
                .any(|u| matches!(u, WorkUpdate::Token(t) if *t == want)),
            "in-app: {ups:?}"
        );
    }
}

/// The real right panel renders a Tools card whose call text is the round-3b
/// approval argument, a long "é€" + emoji URL in the preview rail and an "é€" + emoji
/// session title — at every cut position — without aborting.
#[gpui::test]
fn rc1i1_right_panel_renders_accented_and_emoji_text(cx: &mut TestAppContext) {
    if isolated("work::text_clip_tests::rc1i1_right_panel_renders_accented_and_emoji_text")
        .is_none()
    {
        return;
    }
    let (view, window): (gpui::Entity<AwiliApp>, &mut VisualTestContext) =
        cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    for text in texts() {
        view.update(window, |app, cx| {
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.mode = Mode::Work;
            app.work.conv_id = None;
            app.work.panel.tool_calls = vec![ToolCall {
                name: "fs.write".into(),
                status: ToolCallStatus::Running,
                message: Some(format!(
                    "{{\"contents\": \"{text}\", \"path\": \"eighth.txt\"}}"
                )),
                kind: crate::work::RowKind::Tool,
            }];
            app.work.referenced_urls = vec![format!("https://example.com/{text}")];
            app.shell.tabs.menu = Some(crate::workshop::WorkshopMenu::RecentSessions);
            app.work.hermes_sessions = vec![serde_json::from_value(serde_json::json!({
                "id": "s1",
                "title": text,
            }))
            .unwrap()];
            cx.notify();
        });
        window.run_until_parked();
        let tools = window.update(|_, cx| view.read(cx).work.panel.tool_calls.len());
        assert_eq!(tools, 1, "the panel rendered with the text");
    }
}
