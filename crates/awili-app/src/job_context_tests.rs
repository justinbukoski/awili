//! Actual Job Context interactions and full owned text in the shared kit.
use crate::{
    consolidation_tests::isolated,
    job_panel::JobTab,
    work::{MemoryInspectorResults, Mode},
    AwiliApp,
};
use gpui::{px, size};

fn click(window: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = window.debug_bounds(selector).expect(selector);
    assert!(
        bounds.size.width >= px(44.) && bounds.size.height >= px(44.),
        "{selector} must be touchable: {bounds:?}"
    );
    let point = if selector.starts_with("ui-section-") {
        gpui::point(
            bounds.center().x,
            bounds.origin.y + crate::ui::Section::header_height() / 2.,
        )
    } else {
        bounds.center()
    };
    window.simulate_click(point, gpui::Modifiers::default());
    window.run_until_parked();
}
fn setup(cx: &mut gpui::TestAppContext) -> (gpui::Entity<AwiliApp>, &mut gpui::VisualTestContext) {
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = None;
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.memory.base_url = "http://127.0.0.1:9".into();
        app.set_mode(Mode::Work, cx);
        app.shell.job_open = true;
        app.shell.job_tab = JobTab::Context;
        cx.notify();
    });
    window.simulate_resize(size(px(1400.), px(1100.)));
    window.run_until_parked();
    (view, window)
}
fn lanes(long: &str) -> MemoryInspectorResults {
    MemoryInspectorResults::V65 {
        policy: vec![
            serde_json::from_value(serde_json::json!({"object":{"content":long}})).unwrap(),
        ],
        items: vec![serde_json::from_value(
            serde_json::json!({"object":{"content":"A short recalled note"}}),
        )
        .unwrap()],
    }
}

#[gpui::test]
fn job_context_memory_section_queries_and_keyboard_search(cx: &mut gpui::TestAppContext) {
    if !isolated("job_context_tests::job_context_memory_section_queries_and_keyboard_search") {
        return;
    }
    let (view, window) = setup(cx);
    view.update(window, |app, cx| {
        app.work.memory_results = Some(lanes("A standing rule"));
        app.work.memory_open = false;
        cx.notify();
    });
    window.run_until_parked();
    assert!(window
        .debug_bounds(crate::work::MEMORY_SEARCH_SELECTOR)
        .is_none());
    click(window, "ui-section-memory-toggle");
    assert!(view.read_with(window, |app, _| app.work.memory_open));
    assert!(window.debug_bounds("ui-button-mem-q-fleet").is_none());
    assert!(window.debug_bounds("ui-button-mem-q-awili").is_none());
    click(window, "ui-button-mem-q-directives");
    assert_eq!(
        view.read_with(window, |app, _| app.work.memory_query.clone()),
        "standing directives rules constraints"
    );
    click(window, "ui-button-mem-q-recent");
    assert_eq!(
        view.read_with(window, |app, _| app.work.memory_query.clone()),
        "recent decisions changes project progress"
    );
    let input = window
        .debug_bounds(crate::work::MEMORY_SEARCH_SELECTOR)
        .unwrap();
    window.simulate_click(input.center(), gpui::Modifiers::default());
    window.run_until_parked();
    window.simulate_keystrokes("boiler space service space date enter");
    window.run_until_parked();
    assert_eq!(
        view.read_with(window, |app, _| app.work.memory_query.clone()),
        "boiler service date"
    );
    click(window, "ui-section-memory-toggle");
    assert!(!view.read_with(window, |app, _| app.work.memory_open));
    assert!(window
        .debug_bounds(crate::work::MEMORY_SEARCH_SELECTOR)
        .is_none());
}

#[gpui::test]
fn job_context_v65_rule_precedes_note_and_full_text_wraps(cx: &mut gpui::TestAppContext) {
    if !isolated("job_context_tests::job_context_v65_rule_precedes_note_and_full_text_wraps") {
        return;
    }
    let (view, window) = setup(cx);
    let long = "Complete standing rule with owned memory text. ".repeat(15);
    view.update(window, |app, cx| {
        app.work.memory_results = Some(lanes(&long));
        app.work.memory_open = true;
        cx.notify();
    });
    window.run_until_parked();
    let first = window
        .debug_bounds("ui-text-memory-result-0-title")
        .unwrap();
    let second = window
        .debug_bounds("ui-text-memory-result-1-title")
        .unwrap();
    assert!(
        first.size.height >= crate::theme::z(200.),
        "the complete long rule must wrap, not be cut at 120 characters: {first:?}"
    );
    assert!(
        second.size.height < crate::theme::z(60.),
        "the short note belongs after the rule"
    );
    assert!(first.origin.y < second.origin.y);
    assert!(window
        .debug_bounds("ui-text-memory-result-0-subtitle")
        .is_some());
    assert!(window
        .debug_bounds("ui-text-memory-result-1-subtitle")
        .is_some());
}

#[gpui::test]
fn job_context_v5_full_text_retains_its_category(cx: &mut gpui::TestAppContext) {
    if !isolated("job_context_tests::job_context_v5_full_text_retains_its_category") {
        return;
    }
    let (view, window) = setup(cx);
    let long = "Full historical memory including the final sentence. ".repeat(15);
    let results = serde_json::from_value(
        serde_json::json!({"facts":[{"id":7,"content":long,"category":"fact","importance":0.5}]}),
    )
    .unwrap();
    view.update(window, |app, cx| {
        app.work.memory_results = Some(MemoryInspectorResults::V5(results));
        app.work.memory_open = true;
        cx.notify();
    });
    window.run_until_parked();
    let title = window
        .debug_bounds("ui-text-memory-result-0-title")
        .unwrap();
    assert!(
        title.size.height >= crate::theme::z(200.),
        "historical memory must keep its complete text: {title:?}"
    );
    assert!(window
        .debug_bounds("ui-text-memory-result-0-subtitle")
        .is_some());
}

#[gpui::test]
fn job_context_skill_row_edits_and_focuses_without_submitting(cx: &mut gpui::TestAppContext) {
    if !isolated("job_context_tests::job_context_skill_row_edits_and_focuses_without_submitting") {
        return;
    }
    let (view, window) = setup(cx);
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        app.work.skill_statics =
            vec![("skill-context-fixture", "A skill with a complete long name")];
        app.work.skills_open = false;
        app.work.running = true;
        app.work.conv_id = Some(app.sidebar.active_id().into());
        app.work.agent_abort_tx = Some(tx);
        cx.notify();
    });
    window.run_until_parked();
    click(window, "ui-section-skills-toggle");
    click(window, "ui-row-skill-context-fixture");
    assert_eq!(
        view.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        "Use the A skill with a complete long name skill to "
    );
    assert!(window.update(|win, cx| view
        .read(cx)
        .composer
        .read(cx)
        .focus_handle()
        .is_focused(win)));
    assert!(view.read_with(window, |app, _| app.work.running));
    assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    view.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
    });
}

#[gpui::test]
fn job_context_diagnostics_are_closed_and_busy_action_explains_why(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "job_context_tests::job_context_diagnostics_are_closed_and_busy_action_explains_why",
    ) {
        return;
    }
    let (view, window) = setup(cx);
    assert!(window
        .debug_bounds(crate::gateway_health_app::TEST_BUTTON_SELECTOR)
        .is_none());
    assert!(window
        .debug_bounds("ui-button-workspace-read-start")
        .is_none());
    click(window, "ui-section-job-details");
    assert!(window
        .debug_bounds(crate::gateway_health_app::TEST_BUTTON_SELECTOR)
        .is_some());
    view.update(window, |app, cx| {
        app.work.running = true;
        cx.notify();
    });
    window.run_until_parked();
    let reason =
        crate::ui::interaction::test_util::tooltip_at(window, "ui-button-workspace-read-start");
    assert_eq!(
        reason.as_deref(),
        Some("Finish the current job or diagnostic first")
    );
    click(window, "ui-button-workspace-read-start");
    assert!(view.read_with(window, |app, _| app.workspace_read.active.is_none()));
    view.update(window, |app, _| app.work.running = false);
}

#[gpui::test]
fn job_context_compact_skill_rows_fit_at_100_and_150(cx: &mut gpui::TestAppContext) {
    if !isolated("job_context_tests::job_context_compact_skill_rows_fit_at_100_and_150") {
        return;
    }
    let (view, window) = setup(cx);
    view.update(window, |app, cx| {
        app.work.memory_open = false;
        app.work.skills_open = true;
        app.work.skill_statics = vec![(
            "skill-context-compact",
            "A complete skill name that wraps across the compact Context row and keeps every word available to the owner, including the final sentence after the old short chip would have ended",
        )];
        cx.notify();
    });
    for scale in [1.0, 1.5] {
        crate::theme::set_ui_scale(scale);
        window.simulate_resize(size(px(800.), px(600.)));
        window.run_until_parked();
        let panel = window.debug_bounds("ui-sheet-awili-job-panel").unwrap();
        let row = window.debug_bounds("ui-row-skill-context-compact").unwrap();
        assert!(
            row.origin.x >= panel.origin.x && row.right() <= panel.right(),
            "the entire skill row stays inside the sheet: {row:?}, {panel:?}"
        );
        assert!(
            row.size.height >= crate::theme::target() && row.size.width >= crate::theme::target()
        );
        let title = window
            .debug_bounds("ui-text-skill-context-compact-title")
            .unwrap();
        assert!(
            title.size.height > crate::theme::z(20.),
            "the complete title wraps at compact width: {title:?}"
        );
    }
    crate::theme::set_ui_scale(1.0);
}

/// RC3-S4: a rules file that was cut shows in the Context tab in plain words.
#[gpui::test]
fn job_context_lists_a_cut_instruction_file(cx: &mut gpui::TestAppContext) {
    if !isolated("job_context_tests::job_context_lists_a_cut_instruction_file") {
        return;
    }
    use crate::agent_launch::{DocPart, DocStatus};
    let (view, window) = setup(cx);
    assert!(
        window.debug_bounds("ui-row-job-rule-0").is_none(),
        "nothing recorded yet, nothing shown"
    );
    view.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_string();
        app.work.context_parts = Some((
            owner,
            vec![
                DocPart {
                    label: "AGENTS.md".into(),
                    status: DocStatus::Cut {
                        kept: 32 * 1024,
                        total: 51 * 1024,
                    },
                },
                DocPart {
                    label: "docs/AGENTS.md".into(),
                    status: DocStatus::Included { bytes: 100 },
                },
            ],
        ));
        cx.notify();
    });
    window.run_until_parked();
    let lines = view.read_with(window, |app, _| app.work_rule_lines().unwrap());
    assert_eq!(
        lines[0],
        ("AGENTS.md was cut at 32 KB of 51 KB".to_string(), true)
    );
    assert!(!lines[1].1);
    assert!(window.debug_bounds("ui-row-job-rule-0").is_some());
    assert!(window.debug_bounds("ui-row-job-rule-1").is_some());
    // Another conversation's parts are not shown here.
    view.update(window, |app, cx| {
        if let Some((owner, _)) = app.work.context_parts.as_mut() {
            owner.push_str("-other");
        }
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("ui-row-job-rule-0").is_none());
}
