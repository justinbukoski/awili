//! "New" on a native harness with an explicit model, end to end: the real
//! `new_workshop_from_default` opens a fresh native thread on that target, the
//! composer's own submit sends the first message, and what the fake CLI
//! recorded is the argv/env it was launched with. (The sibling test in
//! `recovery_integration_tests` chooses the target on a Work thread; this one
//! covers the native-thread path New takes for an `Agent` target.) Every
//! harness binary is a recording stub or a path that does not exist.

use super::*;

/// Send `text` from the shown native tab through the composer and wait until
/// that tab holds a non-empty reply. Returns the native tab's id and the
/// conversation it is bound to.
fn native_turn(
    cx: &mut gpui::TestAppContext,
    app: &gpui::Entity<AwiliApp>,
    text: &str,
) -> (u64, String) {
    app.update(cx, |a, cx| {
        a.composer.update(cx, |c, cx| c.set_text(text, cx));
        a.submit_composer_draft(cx);
    });
    settle(cx, app, text, |a| {
        a.work.code_tabs.get(a.work.code_active).is_some_and(|tab| {
            !tab.streaming
                && tab
                    .convo
                    .messages()
                    .last()
                    .is_some_and(|m| m.role == awili_core::Role::Assistant && !m.text.is_empty())
        })
    });
    app.read_with(cx, |a, _| {
        let tab = &a.work.code_tabs[a.work.code_active];
        (
            tab.id,
            tab.history
                .conv_id
                .clone()
                .expect("the first send bound the tab to a conversation"),
        )
    })
}

#[gpui::test]
fn new_native_thread_with_an_explicit_model_launches_with_that_model(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "recovery_integration_tests::native_new::new_native_thread_with_an_explicit_model_launches_with_that_model",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let fx = Fx::new();
    let (app, _first) = launch(cx, &fx);
    let mut seen_tabs: Vec<u64> = Vec::new();

    for (engine, model, ask) in [
        (
            CodeEngine::Claude,
            "claude-opus-explicit",
            "ask the new claude thread",
        ),
        (
            CodeEngine::Dsh,
            "deepseek-v4-explicit",
            "ask the new dsh thread",
        ),
    ] {
        app.update(cx, |a, cx| {
            a.new_workshop_from_default(agent(engine, Some(model)), cx)
        });
        cx.run_until_parked();
        // New shows the explicit target as this fresh native tab's next turn,
        // before anything is sent.
        let shown = app.read_with(cx, |a, _| {
            assert_eq!(a.mode, Mode::Code, "a native New opens the native view");
            let tab = &a.work.code_tabs[a.work.code_active];
            assert!(!seen_tabs.contains(&tab.id), "New is a tab of its own");
            assert!(tab.convo.messages().is_empty(), "a fresh thread");
            a.composer_target_for(&ModelOwner::Native(tab.id))
        });
        assert_eq!(shown, Some(agent(engine, Some(model))));

        let (tab_id, conv) = native_turn(cx, &app, ask);
        seen_tabs.push(tab_id);
        app.read_with(cx, |a, _| {
            assert_eq!(
                a.composer_target_for(&ModelOwner::Native(tab_id)),
                Some(agent(engine, Some(model))),
                "sending did not move the thread off its chosen target"
            );
            assert_eq!(
                a.work
                    .conversation_routes
                    .get(&conv)
                    .map(|route| route.next_target().clone()),
                Some(agent(engine, Some(model))),
                "the thread's own route carries the model"
            );
        });
    }

    let claude = fx.calls("claude");
    assert_eq!(claude.len(), 1, "exactly one Claude launch: {claude:?}");
    assert!(
        has_pair(&claude[0], "--model", "claude-opus-explicit"),
        "{:?}",
        claude[0]
    );
    task_arg(&claude[0], "ask the new claude thread");
    let dsh = fx.calls("dsh");
    assert_eq!(dsh.len(), 1, "exactly one dsh launch: {dsh:?}");
    assert_eq!(fx.dsh_model(1), "deepseek-v4-explicit");
    assert!(
        !dsh[0].iter().any(|a| a.contains("deepseek-v4-explicit")),
        "dsh takes its model from the environment, never argv: {:?}",
        dsh[0]
    );
}
