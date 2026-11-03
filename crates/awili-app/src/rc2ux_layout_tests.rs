//! RC2-UX layout proof: the real `AwiliApp` (a private HOME, in a
//! subprocess) drawn in a test window and measured through its
//! `debug_selector` bounds — the sidebar rows (round 3b Minor 4, S-I2), the
//! reply box (R2-26) and the local-model card (R2-20).

use std::path::PathBuf;

use awili_code::CodeEngine;
use awili_core::{ConvKind, Role};
use gpui::{px, size};

use crate::sidebar::ACTIVE_ROW_SELECTOR;
use crate::work::{Mode, WorkExecutor};
use crate::AwiliApp;

const CHILD: &str = "AWILI_RC2UX_LAYOUT_TEST_HOME";

/// Run `test` in a subprocess with a private HOME. `Some(home)` inside the
/// child; `None` in the parent once the child passed.
fn isolated(test: &str) -> Option<PathBuf> {
    if let Some(home) = std::env::var_os(CHILD) {
        return Some(home.into());
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    None
}

/// Work mode on Claude (a binary that does not exist: nothing runs).
fn work_app(app: &mut AwiliApp, home: &std::path::Path) {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.fleet_pack = None;
    app.memory.enabled = false;
    app.mode = Mode::Work;
    app.shell.job_open = true;
    app.work.executor = WorkExecutor::Agent {
        engine: CodeEngine::Claude,
        model: None,
    };
    app.work.agents.claude_bin = Some(home.join("no-such-claude").display().to_string());
}

/// A Work conversation titled `title` holding `rows`, returned by id.
fn conversation(app: &mut AwiliApp, ts: i64, title: &str, rows: &[(Role, &str)]) -> String {
    let meta = app.conversations.create_kind(ts, ConvKind::Work);
    app.conversations.get_mut(&meta.id).unwrap().title = title.to_string();
    let mut state = awili_core::ConversationState::new();
    for (role, text) in rows {
        state.push(*role, *text);
    }
    app.views.insert(
        meta.id.clone(),
        crate::conv_view::ConvView::from_state(state),
    );
    meta.id
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}
/// RC2-UX (round 3b Minor 4, D-R1 long titles, S-I2): at the narrowest
/// window a long title is one line cut with "…" (it wrapped a letter or a
/// word per line), and a row's Rename/Archive/Delete icons show on the
/// selected row and on the row under the pointer only, so titles get the
/// room.
#[gpui::test]
fn rc2ux_sidebar_titles_stay_one_line_and_icons_show_on_hover(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("rc2ux_layout_tests::rc2ux_sidebar_titles_stay_one_line_and_icons_show_on_hover")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(900.), px(712.)));
    view.update(window, |app, cx| {
        work_app(app, &home);
        // History owns this compact interaction (at 900 px it is a sheet); a
        // modal Job sheet would occlude it.
        app.shell.conversations = true;
        app.shell.job_open = false;
        for (i, title) in [
            "Rename every holiday photo from last summer by the date it was taken",
            "Hi",
            "Summarise the garden planting notes into one page for the spring",
        ]
        .iter()
        .enumerate()
        {
            let meta = app
                .conversations
                .create_kind(3_000_000 + i as i64, ConvKind::Work);
            app.conversations.get_mut(&meta.id).unwrap().title = title.to_string();
            app.views.insert(
                meta.id.clone(),
                crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
            );
            if i == 0 {
                app.set_active_conversation(&meta.id);
            }
        }
        cx.notify();
    });
    let rename = |ix: usize| -> &'static str {
        Box::leak(format!("rc2ux-sidebar-rename-c-{ix}").into_boxed_str())
    };
    let sidebar = "awili-sidebar";
    let shown = |w: &mut gpui::VisualTestContext, sel: &'static str| {
        w.debug_bounds(sel).is_some_and(|b| b.size.width > px(0.))
    };
    // Keep the real sidebar at its compact 900×712 viewport while zooming.
    // The Row face, title slot, and hover action must remain inside the
    // sidebar's available width at each supported scale.
    for scale in [0.75_f32, 0.9, 1.0, 1.25, 1.5, 2.5] {
        // Leave the row using the previous layout before changing zoom. The
        // next frame keeps the same screen-space pointer coordinates, which
        // can otherwise land on a different row during resize.
        window.simulate_mouse_move(
            gpui::point(px(899.), px(711.)),
            None,
            gpui::Modifiers::default(),
        );
        window.run_until_parked();
        crate::theme::set_ui_scale(scale);
        window.simulate_resize(size(px(900.), px(712.)));
        window.run_until_parked();
        // The scale decides whether the panel docks or is a sheet at this
        // width, and a docked frame consumes the sheet flag; keep it open by
        // its own setter at every scale.
        view.update(window, |app, cx| app.set_workshop_history_open(true, cx));
        window.run_until_parked();
        let rows: Vec<(usize, gpui::Bounds<gpui::Pixels>)> = (0..12)
            .filter_map(|ix| {
                let sel: &'static str =
                    Box::leak(format!("rc2ux-sidebar-title-c-{ix}").into_boxed_str());
                window.debug_bounds(sel).map(|b| (ix, b))
            })
            .collect();
        assert!(rows.len() >= 3, "zoom {scale}: rows are drawn: {rows:?}");
        let one_line = rows
            .iter()
            .map(|(_, b)| b.size.height)
            .fold(px(f32::MAX), |a, b| a.min(b));
        let panel = window.debug_bounds(sidebar).expect("sidebar panel");
        for (ix, title) in &rows {
            assert_eq!(
                title.size.height, one_line,
                "zoom {scale}: row {ix}'s title is one line: {title:?}"
            );
            assert!(
                title.size.width > px(60.),
                "zoom {scale}: row {ix}'s title has room: {title:?}"
            );
            assert!(
                title.left() >= panel.left() && title.right() <= panel.right(),
                "zoom {scale}: title stays in the sidebar: {title:?} / {panel:?}"
            );
            let row = window
                .debug_bounds(leak(format!("ui-row-conv-{ix}")))
                .expect("the KIT Row face");
            assert!(
                row.left() >= panel.left() && row.right() <= panel.right(),
                "zoom {scale}: Row face stays in the sidebar: {row:?} / {panel:?}"
            );
        }
        let active = window
            .debug_bounds(ACTIVE_ROW_SELECTOR)
            .expect("the selected row");
        let (active_ix, _) = *rows
            .iter()
            .find(|(_, b)| active.contains(&b.center()))
            .expect("the selected row's title");
        // Hover a row whose title is really on screen: at large zoom the list
        // is longer than the sheet and later rows lie below the window (or
        // under the list's clip), where no pointer can reach them.
        // At large zoom the panel's chrome leaves the list a short viewport,
        // so a row is brought into it the way a user does: by scrolling the
        // list with the wheel, until a non-selected row's title is under a
        // point the pointer can reach.
        let reachable = window
            .debug_bounds("awili-history-list")
            .unwrap_or(panel)
            .intersect(&gpui::Bounds::from_corners(
                gpui::point(px(0.), px(0.)),
                gpui::point(px(900.), px(712.)),
            ));
        let mut hovered = None;
        for _ in 0..60 {
            hovered = (0..12)
                .filter(|ix| *ix != active_ix)
                .filter_map(|ix| {
                    window
                        .debug_bounds(leak(format!("rc2ux-sidebar-title-c-{ix}")))
                        .map(|b| (ix, b))
                })
                .find(|(_, b)| reachable.contains(&b.center()));
            if hovered.is_some() {
                break;
            }
            window.simulate_event(gpui::ScrollWheelEvent {
                position: reachable.center(),
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-60.))),
                ..Default::default()
            });
            window.run_until_parked();
        }
        let (other_ix, other) = hovered.unwrap_or_else(|| {
            panic!(
                "zoom {scale}: wheel scrolling never brought a non-selected row into {reachable:?}"
            )
        });
        assert!(
            shown(window, rename(active_ix)),
            "zoom {scale}: selected actions shown"
        );
        assert!(
            !shown(window, rename(other_ix)),
            "zoom {scale}: idle actions hidden"
        );
        window.simulate_mouse_move(other.center(), None, gpui::Modifiers::default());
        window.run_until_parked();
        assert!(
            shown(window, rename(other_ix)),
            "zoom {scale}: hovered actions shown"
        );
        let action = window
            .debug_bounds(leak(format!("awili-sidebar-delete-c-{other_ix}")))
            .expect("hovered delete control");
        assert!(
            action.left() >= panel.left() && action.right() <= panel.right(),
            "zoom {scale}: actions stay in the sidebar: {action:?} / {panel:?}"
        );
    }
    crate::theme::set_ui_scale(1.0);
}

/// The rendered row controls must keep their two-step delete behavior, stop
/// the parent row click, and confirm the conversation whose control was armed.
#[gpui::test]
fn rc2ux_sidebar_delete_controls_cancel_and_confirm_the_armed_row(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "rc2ux_layout_tests::rc2ux_sidebar_delete_controls_cancel_and_confirm_the_armed_row",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(900.), px(712.)));
    let (active_id, target_id) = view.update(window, |app, cx| {
        work_app(app, &home);
        app.shell.conversations = true;
        app.shell.job_open = false;
        let now = chrono::Local::now().timestamp();
        let active_id = conversation(app, now - 10, "Active sidebar row", &[]);
        let target_id = conversation(app, now - 5, "Delete target sidebar row", &[]);
        app.set_active_conversation(&active_id);
        cx.notify();
        (active_id, target_id)
    });
    window.run_until_parked();

    let active = window
        .debug_bounds(ACTIVE_ROW_SELECTOR)
        .expect("the active row is drawn");
    let active_ix = (0..8)
        .find(|ix| {
            window
                .debug_bounds(leak(format!("rc2ux-sidebar-title-c-{ix}")))
                .is_some_and(|title| active.contains(&title.center()))
        })
        .expect("the active title has a row index");
    let target_row = window
        .debug_bounds(leak(format!("awili-history-row-{target_id}")))
        .expect("the target conversation row is drawn");
    let target_ix = (0..8)
        .find(|ix| {
            *ix != active_ix
                && window
                    .debug_bounds(leak(format!("rc2ux-sidebar-title-c-{ix}")))
                    .is_some_and(|title| target_row.contains(&title.center()))
        })
        .expect("the target conversation title has a row index");
    let target_title = window
        .debug_bounds(leak(format!("rc2ux-sidebar-title-c-{target_ix}")))
        .expect("delete target title");

    window.simulate_mouse_move(target_title.center(), None, gpui::Modifiers::default());
    window.run_until_parked();
    let delete = window
        .debug_bounds(leak(format!("awili-sidebar-delete-c-{target_ix}")))
        .expect("hovered delete control");
    window.simulate_click(delete.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(app.pending_delete.as_deref(), Some(target_id.as_str()));
        assert_eq!(
            app.sidebar.active_id(),
            active_id.as_str(),
            "Delete must not select its row"
        );
    });

    let cancel = window
        .debug_bounds(leak(format!("awili-sidebar-delete-cancel-c-{target_ix}")))
        .expect("inline cancel control");
    window.simulate_click(cancel.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.pending_delete.is_none(), "Cancel disarms delete");
        assert!(
            app.conversations.get(&target_id).is_some(),
            "Cancel keeps the row"
        );
        assert_eq!(
            app.sidebar.active_id(),
            active_id.as_str(),
            "Cancel does not select the row"
        );
    });

    let delete = window
        .debug_bounds(leak(format!("awili-sidebar-delete-c-{target_ix}")))
        .expect("delete control remains available after cancel");
    window.simulate_click(delete.center(), gpui::Modifiers::default());
    window.run_until_parked();
    assert!(
        window
            .debug_bounds(leak(format!("awili-sidebar-delete-confirm-c-{target_ix}")))
            .is_some(),
        "re-arming exposes the inline confirm control"
    );
    let confirm = window
        .debug_bounds(leak(format!("awili-sidebar-delete-confirm-c-{target_ix}")))
        .expect("inline confirm control");
    window.simulate_click(confirm.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(
            app.pending_delete.is_none(),
            "confirm consumes the armed state"
        );
        assert!(
            app.conversations.get(&target_id).is_none(),
            "confirm deletes the armed row"
        );
        assert!(
            app.conversations.get(&active_id).is_some(),
            "the active row remains"
        );
        assert_eq!(
            app.sidebar.active_id(),
            active_id.as_str(),
            "confirm does not select its row"
        );
    });
}

/// RC2-UX (R2-26, round 2 N-M2a: "the text column is about 430 px wide
/// inside a wider reply box, leaving dead space on the right"): a prose
/// reply's box fits its reading column; a reply with a code block keeps the
/// full width the code needs.
#[gpui::test]
fn rc2ux_a_prose_reply_box_fits_its_text(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("rc2ux_layout_tests::rc2ux_a_prose_reply_box_fits_its_text") else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(1000.)));
    let prose = "Hostas, ferns and astilbe all do well in deep shade. ".repeat(6);
    let code =
        format!("{prose}\n\n```sh\nls -la ~/garden/notes/2026/spring/plantings/shade-beds\n```");
    view.update(window, |app, cx| {
        work_app(app, &home);
        let id = conversation(
            app,
            4_000_000,
            "Shade plants",
            &[
                (Role::User, "Which plants grow in shade?"),
                (Role::Assistant, prose.as_str()),
                (Role::User, "And the command?"),
                (Role::Assistant, code.as_str()),
            ],
        );
        app.set_active_conversation(&id);
        cx.notify();
    });
    window.run_until_parked();
    let prose_box = window
        .debug_bounds(leak(format!("rc2ux-reply-box-{}", 1)))
        .expect("the prose reply's box");
    let code_box = window
        .debug_bounds(leak(format!("rc2ux-reply-box-{}", 3)))
        .expect("the code reply's box");
    // Replies now sit on the canvas: both prose and code occupy the bounded
    // reading lane. There is no card whose intrinsic width must shrink to
    // short text; retain this legacy test name and verify the actual cap.
    let scale = crate::theme::ui_scale();
    let fits = px(crate::theme::READING_MEASURE * scale);
    assert!(
        prose_box.size.width <= fits + px(1.),
        "the prose box fits the reading column: {prose_box:?} (max {fits:?})"
    );
    assert!(
        code_box.size.width >= prose_box.size.width
            && (code_box.size.width
                - px(crate::theme::TRANSCRIPT_CONTENT
                    .min(crate::theme::READING_MAX - 2. * crate::theme::SP_24)
                    * scale))
            .abs()
                <= px(1.),
        "a reply with code keeps the wide box: {code_box:?} vs {prose_box:?}"
    );
}

/// RC2-UX (R2-20, round 2 Major 4 rest): the "local model not answering"
/// card sits right above the message box (where the owner is looking), not
/// in the side panel.
#[gpui::test]
fn rc2ux_the_local_model_card_sits_above_the_message_box(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("rc2ux_layout_tests::rc2ux_the_local_model_card_sits_above_the_message_box")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        work_app(app, &home);
        let id = conversation(app, 5_000_000, "Local", &[]);
        app.set_active_conversation(&id);
        app.work.executor = WorkExecutor::InApp;
        app.work.local.down = Some(crate::local_model_health::LocalDown {
            reason: crate::local_model_health::DownReason::Unreachable("refused".into()),
            text: Some("hello".into()),
            conv_id: Some(id),
        });
        cx.notify();
    });
    window.run_until_parked();
    let card = window
        .debug_bounds(crate::local_model_app::LOCAL_DOWN_CARD_SELECTOR)
        .expect("the card is drawn");
    let composer = window
        .debug_bounds(crate::plan_gate::COMPOSER_ROW_SELECTOR)
        .expect("the message box row");
    assert!(
        card.bottom() <= composer.top(),
        "above the box: {card:?} {composer:?}"
    );
    assert!(
        composer.top() - card.bottom() < px(40.),
        "right above it: {card:?} {composer:?}"
    );
    assert!(
        card.left() < composer.right() && composer.left() < card.right(),
        "in the same column as the box: {card:?} {composer:?}"
    );
}

/// RC2-UX (R5-4): the Terminal box in the Work panel says what it is; (D-R1
/// S1 badge) a turn refused before it ran reads "not sent", not "failed".
#[gpui::test]
fn rc2ux_the_terminal_says_what_it_is_and_a_refused_turn_was_not_sent(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "rc2ux_layout_tests::rc2ux_the_terminal_says_what_it_is_and_a_refused_turn_was_not_sent",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let folder = home.join("w1");
    std::fs::create_dir_all(&folder).unwrap();
    view.update(window, |app, cx| {
        work_app(app, &home);
        let id = conversation(app, 6_000_000, "Notes", &[]);
        app.set_active_conversation(&id);
        app.set_work_folder(folder.clone(), cx);
        app.work.conv_id = Some(id);
        app.work.panel.error = Some(crate::recall::policy_refusal("over a bound"));
        cx.notify();
    });
    window.run_until_parked();
    let terminal = window.debug_bounds("ui-tab-job-tab-1").unwrap().center();
    window.simulate_click(terminal, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(
        window
            .debug_bounds(crate::work::TERMINAL_CAPTION_SELECTOR)
            .is_some(),
        "the Terminal box says what it is"
    );
    assert!(crate::work::TERMINAL_CAPTION.contains("for commands you type yourself"));
    let history = window.debug_bounds("ui-tab-job-tab-0").unwrap().center();
    window.simulate_click(history, gpui::Modifiers::default());
    window.run_until_parked();
    let status = |s: &str| leak(format!("{}{s}", AwiliApp::WORK_STATUS_SELECTOR));
    assert!(
        window.debug_bounds(status("not sent")).is_some(),
        "not sent"
    );
    assert!(
        window.debug_bounds(status("failed")).is_none(),
        "not failed"
    );
    // Any other error still reads "failed".
    view.update(window, |app, cx| {
        app.work.panel.error = Some("the helper crashed".into());
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds(status("failed")).is_some());
}

/// RC2-UX (D-R1 m9): after Stop, Send appears under the same pointer; the
/// second click of a double click never sends what is in the box, a single
/// click does.
///
/// RC2-FIXB5 (RC2-UX review P3: the test put the second click straight onto
/// Send): the whole double click is played on a reply that is running — the
/// first click lands on Stop and stops the reply, Send takes its place, and
/// the second click of the same double click lands on Send and sends nothing.
#[gpui::test]
fn rc2ux_a_double_click_never_sends(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("rc2ux_layout_tests::rc2ux_a_double_click_never_sends") else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.memory.enabled = false;
        app.mode = Mode::Work;
        let meta = app.conversations.create_kind(7_000_000, ConvKind::Work);
        // A reply that is still coming in: the button is Stop.
        let conv = crate::conv_view::ConvView::from_state(awili_core::ConversationState::seeded([
            (Role::User, "Which plants grow in shade?".to_string()),
            (Role::Assistant, "Hostas and".to_string()),
        ]));
        app.views.insert(meta.id.clone(), conv);
        app.set_active_conversation(&meta.id);
        app.work.agents.claude_bin = Some(home.join("no-such-claude").display().to_string());
        app.work.running = true;
        app.work.conv_id = Some(meta.id.clone());
        app.work
            .cancel
            .store(false, std::sync::atomic::Ordering::SeqCst);
        cx.notify();
    });
    window.run_until_parked();
    view.update(window, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("typed while it ran", cx));
    });
    window.run_until_parked();
    assert!(
        view.read_with(window, |app, _| app.composer_busy()),
        "the button is Stop"
    );
    let stop = window
        .debug_bounds("rc2ux-send-or-stop")
        .expect("the Stop button");
    let at = stop.center();
    let mouse = |window: &mut gpui::VisualTestContext, clicks: usize| {
        window.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: clicks,
            first_mouse: false,
        });
        window.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: clicks,
        });
        window.run_until_parked();
    };
    let messages = |view: &gpui::Entity<AwiliApp>, window: &mut gpui::VisualTestContext| {
        view.read_with(window, |app, _| {
            app.active_view().map_or(0, |v| v.convo.messages().len())
        })
    };
    // The first click of the double click: Stop.
    mouse(window, 1);
    assert!(
        view.read_with(window, |app, _| app
            .work
            .cancel
            .load(std::sync::atomic::Ordering::SeqCst)),
        "the first click stopped the reply"
    );
    assert!(
        !view.read_with(window, |app, _| app.composer_busy()),
        "the button is Send now"
    );
    let send = window
        .debug_bounds("rc2ux-send-or-stop")
        .expect("the Send button");
    assert!(send.contains(&at), "Send is under the same pointer");
    // The second click of the same double click lands on Send.
    mouse(window, 2);
    let text = view.read_with(window, |app, cx| app.composer.read(cx).text().to_string());
    assert_eq!(text, "typed while it ran", "a double click sent nothing");
    assert_eq!(messages(&view, window), 2, "no new message");
    window.simulate_click(at, gpui::Modifiers::default());
    window.run_until_parked();
    let text = view.read_with(window, |app, cx| app.composer.read(cx).text().to_string());
    assert_eq!(text, "", "a single click sends");
}
/// RC2-UX (D-R1 M9b: "Code mode menu opens at the composer while its chip is
/// at the top"): the Ask/Auto menu opens next to its chip — at the top for
/// a Code tab, at the bottom for Work.
#[gpui::test]
fn rc2ux_the_mode_menu_opens_by_its_chip(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("rc2ux_layout_tests::rc2ux_the_mode_menu_opens_by_its_chip") else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    let h = px(900.);
    window.simulate_resize(size(px(1400.), h));
    view.update(window, |app, cx| {
        work_app(app, &home);
        app.set_mode(Mode::Code, cx);
        let tab = app.work.code_tabs[app.work.code_active].id;
        app.work.ask_menu =
            crate::ask_mode_ui::AskMenu::Open(crate::ask_mode::AskKey::CodeTab(tab));
        cx.notify();
    });
    window.run_until_parked();
    let card = window
        .debug_bounds(crate::ask_mode_ui::ASK_MENU_CARD_SELECTOR)
        .expect("the menu is open");
    assert!(
        card.top() < h / 3.,
        "Code: at the top, by the tab bar: {card:?}"
    );
    view.update(window, |app, cx| {
        app.set_mode(Mode::Work, cx);
        let conv = app.sidebar.active_id().to_string();
        app.work.ask_menu = crate::ask_mode_ui::AskMenu::Open(crate::ask_mode::AskKey::Work(conv));
        cx.notify();
    });
    window.run_until_parked();
    let card = window
        .debug_bounds(crate::ask_mode_ui::ASK_MENU_CARD_SELECTOR)
        .expect("the menu is open");
    // The top bar redesign moved the Work mode chip into the bar, so its
    // menu now opens at the top, by the chip.
    assert!(
        card.top() < h / 3.,
        "Work: at the top, by the bar's mode chip: {card:?}"
    );
}

/// RC2-UX: the global Cowork palette sends real text and Backspace through its
/// KIT SearchField once, keeps command chords out of the query, and routes its
/// keyboard back/choose behavior through the app state.
#[gpui::test]
fn rc2ux_the_palette_takes_what_was_typed(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("rc2ux_layout_tests::rc2ux_the_palette_takes_what_was_typed") else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        work_app(app, &home);
        cx.notify();
    });
    window.run_until_parked();
    let composer_focus = view.read_with(window, |app, cx| {
        app.composer.read(cx).focus_handle().clone()
    });
    window.update(|win, _| {
        assert!(
            composer_focus.is_focused(win),
            "the composer owns focus before opening"
        );
    });
    window.simulate_keystrokes("cmd-shift-p");
    window.run_until_parked();
    view.read_with(window, |app, _| assert!(app.command_palette.open));

    // One user keystroke becomes exactly one query character through the
    // embedded SearchField. Backspace edits once, while command chords do not
    // type into the field or leak to the composer underneath it.
    window.simulate_keystrokes("m");
    window.run_until_parked();
    view.read_with(window, |app, _| assert_eq!(app.command_palette.query, "m"));
    window.simulate_keystrokes("backspace");
    window.run_until_parked();
    window.simulate_keystrokes("cmd-v");
    window.run_until_parked();
    view.read_with(window, |app, cx| {
        assert_eq!(app.command_palette.query, "");
        assert_eq!(app.composer.read(cx).text(), "");
    });

    window.simulate_keystrokes("c o l o r space m o d e");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(app.command_palette.query, "color mode")
    });
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.command_palette.open);
        assert_eq!(
            app.command_palette.page,
            crate::command_palette::Page::ColorMode
        );
        assert_eq!(app.command_palette.query, "");
    });

    // Keyboard drill-in leaves the SearchField ready to accept text. Escape
    // returns to Root; empty Backspace also pops to Root; Shift+Enter then
    // dismisses from Root.
    window.simulate_keystrokes("d");
    window.run_until_parked();
    view.read_with(window, |app, _| assert_eq!(app.command_palette.query, "d"));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.command_palette.open);
        assert_eq!(app.command_palette.page, crate::command_palette::Page::Root);
        assert_eq!(app.command_palette.query, "");
    });
    window.simulate_keystrokes("c o l o r space m o d e enter");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(
            app.command_palette.page,
            crate::command_palette::Page::ColorMode
        );
    });
    window.simulate_keystrokes("backspace");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.command_palette.open);
        assert_eq!(app.command_palette.page, crate::command_palette::Page::Root);
    });
    window.simulate_keystrokes("shift-enter");
    window.run_until_parked();
    view.read_with(window, |app, _| assert!(!app.command_palette.open));
    window.update(|win, _| {
        assert!(
            composer_focus.is_focused(win),
            "dismissing the global palette returns focus to the prior composer"
        );
    });

    // Reopening restores the search stop. A real keyboard selection chooses
    // Dark in the Color Mode sub-page and persists the preference.
    window.simulate_keystrokes("cmd-shift-p");
    window.run_until_parked();
    window.simulate_keystrokes("c o l o r space m o d e enter down enter");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(!app.command_palette.open);
        assert_eq!(app.appearance.preference, awili_config::ColorMode::Dark);
        assert_eq!(
            app.saved_config().unwrap().appearance.color_mode,
            awili_config::ColorMode::Dark
        );
    });

    // A normal catalog action still closes the overlay and changes the route.
    window.simulate_keystrokes("cmd-shift-p");
    window.run_until_parked();
    window.simulate_keystrokes("cmd-a");
    window.run_until_parked();
    window.simulate_keystrokes("s k i l l s");
    window.run_until_parked();
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(!app.command_palette.open);
        assert_eq!(app.mode, Mode::Skills);
    });
}

/// The native menu can request the command palette while its SearchField is
/// already focused. That request is an idempotent open: it keeps the live
/// query and the original focus owner used by the eventual close.
#[gpui::test]
fn rc2ux_native_palette_action_preserves_an_open_session(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("rc2ux_layout_tests::rc2ux_native_palette_action_preserves_an_open_session")
    else {
        return;
    };
    cx.executor().allow_parking();
    cx.update(|cx| {
        cx.bind_keys(crate::app_lifecycle::standard_bindings());
        crate::install_native_menu_actions(cx);
    });
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.update(|window, cx| {
        cx.set_global(crate::AwiliWindow(
            window.window_handle().downcast::<AwiliApp>().unwrap(),
        ));
    });
    window.simulate_resize(size(px(1280.), px(800.)));
    view.update(window, |app, cx| {
        work_app(app, &home);
        cx.notify();
    });
    window.run_until_parked();
    let composer_focus = view.read_with(window, |app, cx| {
        app.composer.read(cx).focus_handle().clone()
    });

    window.simulate_keystrokes("cmd-shift-p");
    window.run_until_parked();
    window.simulate_keystrokes("color space mode enter d");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.command_palette.open);
        assert_eq!(
            app.command_palette.page,
            crate::command_palette::Page::ColorMode
        );
        assert_eq!(app.command_palette.query, "d");
    });

    window.dispatch_action(crate::native_menu::OpenCommandPalette);
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.command_palette.open);
        assert_eq!(
            app.command_palette.page,
            crate::command_palette::Page::ColorMode
        );
        assert_eq!(app.command_palette.query, "d");
    });
    window.simulate_keystrokes("a");
    window.run_until_parked();
    view.read_with(window, |app, _| assert_eq!(app.command_palette.query, "da"));

    window.simulate_keystrokes("escape escape");
    window.run_until_parked();
    view.read_with(window, |app, _| assert!(!app.command_palette.open));
    window.update(|window, _| {
        assert!(
            composer_focus.is_focused(window),
            "a repeated native open does not replace the original return-focus owner"
        );
    });
}

/// A pointer choice of the Color Mode drill-in keeps the real global palette
/// open and restores its SearchField focus. Escape and outside click dismiss
/// through the rendered overlay, and reopening accepts text immediately.
#[gpui::test]
fn rc2ux_palette_mouse_drill_in_keeps_search_and_scrim_dismisses(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "rc2ux_layout_tests::rc2ux_palette_mouse_drill_in_keeps_search_and_scrim_dismisses",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        work_app(app, &home);
        cx.notify();
    });
    window.run_until_parked();
    let composer_focus = view.read_with(window, |app, cx| {
        app.composer.read(cx).focus_handle().clone()
    });
    window.simulate_keystrokes("cmd-shift-p");
    window.run_until_parked();
    window.simulate_keystrokes("c o l o r space m o d e");
    window.run_until_parked();

    let panel = window
        .debug_bounds("ui-palette-cowork-command-palette-panel")
        .expect("global palette panel is rendered");
    let row = window
        .debug_bounds("ui-palette-row-cowork-command-palette-row-0")
        .expect("the filtered Color Mode row is rendered");
    assert!(
        row.left() >= panel.left()
            && row.top() >= panel.top()
            && row.right() <= panel.right()
            && row.bottom() <= panel.bottom(),
        "the full pointer target is inside the palette panel: {row:?} in {panel:?}"
    );
    assert!(
        row.left() >= px(0.)
            && row.top() >= px(0.)
            && row.right() <= px(1280.)
            && row.bottom() <= px(800.),
        "the full pointer target is inside the physical window: {row:?}"
    );
    window.simulate_click(row.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.command_palette.open);
        assert_eq!(
            app.command_palette.page,
            crate::command_palette::Page::ColorMode
        );
    });
    window.simulate_keystrokes("d");
    window.run_until_parked();
    view.read_with(window, |app, _| assert_eq!(app.command_palette.query, "d"));

    window.simulate_keystrokes("shift-enter");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.command_palette.open);
        assert_eq!(app.command_palette.page, crate::command_palette::Page::Root);
    });
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    view.read_with(window, |app, _| assert!(!app.command_palette.open));
    window.update(|win, _| {
        assert!(
            composer_focus.is_focused(win),
            "Escape returns focus to the prior composer"
        );
    });

    window.simulate_keystrokes("cmd-shift-p");
    window.run_until_parked();
    window.simulate_keystrokes("r");
    window.run_until_parked();
    view.read_with(window, |app, _| assert_eq!(app.command_palette.query, "r"));
    let scrim = window
        .debug_bounds("ui-palette-scrim")
        .expect("the rendered palette scrim receives outside clicks");
    let panel = window
        .debug_bounds("ui-palette-cowork-command-palette-panel")
        .expect("the palette panel remains open");
    let outside = gpui::point(scrim.right() - px(8.), scrim.bottom() - px(8.));
    assert!(
        scrim.contains(&outside),
        "the click is inside the scrim: {scrim:?}"
    );
    assert!(
        !panel.contains(&outside),
        "the click is outside the panel: {panel:?}"
    );
    window.simulate_click(outside, gpui::Modifiers::default());
    window.run_until_parked();
    view.read_with(window, |app, _| assert!(!app.command_palette.open));
    window.update(|win, _| {
        assert!(
            composer_focus.is_focused(win),
            "the outside-click close returns focus to the prior composer"
        );
    });

    // Choose Light by pointer from the real Color Mode page. The row face is
    // measured again after the page changes, then the app's saved preference
    // is checked after the action closes the palette.
    window.simulate_keystrokes("cmd-shift-p");
    window.run_until_parked();
    window.simulate_keystrokes("c o l o r space m o d e enter");
    window.run_until_parked();
    let light = window
        .debug_bounds("ui-palette-row-cowork-command-palette-row-0")
        .expect("Light row is rendered");
    let panel = window
        .debug_bounds("ui-palette-cowork-command-palette-panel")
        .expect("Color Mode panel is rendered");
    assert!(
        light.left() >= panel.left()
            && light.top() >= panel.top()
            && light.right() <= panel.right()
            && light.bottom() <= panel.bottom()
            && light.left() >= px(0.)
            && light.top() >= px(0.)
            && light.right() <= px(1280.)
            && light.bottom() <= px(800.),
        "Light's full pointer target is visible: {light:?} in {panel:?}"
    );
    window.simulate_click(light.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(!app.command_palette.open);
        assert_eq!(app.appearance.preference, awili_config::ColorMode::Light);
        assert_eq!(
            app.saved_config().unwrap().appearance.color_mode,
            awili_config::ColorMode::Light
        );
    });

    // System is the third real row; choosing it by pointer persists the
    // platform-following preference rather than leaving the prior Light mode.
    window.simulate_keystrokes("cmd-shift-p");
    window.run_until_parked();
    window.simulate_keystrokes("c o l o r space m o d e enter");
    window.run_until_parked();
    let system = window
        .debug_bounds("ui-palette-row-cowork-command-palette-row-2")
        .expect("System row is rendered");
    let panel = window
        .debug_bounds("ui-palette-cowork-command-palette-panel")
        .expect("Color Mode panel is rendered");
    assert!(
        system.left() >= panel.left()
            && system.top() >= panel.top()
            && system.right() <= panel.right()
            && system.bottom() <= panel.bottom()
            && system.left() >= px(0.)
            && system.top() >= px(0.)
            && system.right() <= px(1280.)
            && system.bottom() <= px(800.),
        "System's full pointer target is visible: {system:?} in {panel:?}"
    );
    window.simulate_click(system.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(!app.command_palette.open);
        assert_eq!(app.appearance.preference, awili_config::ColorMode::System);
        assert_eq!(
            app.saved_config().unwrap().appearance.color_mode,
            awili_config::ColorMode::System
        );
    });
}

/// RC2-UX (D-PUNCH F11): the Work memory card has a search box — Enter
/// searches the owner's memory for what was typed (only four set questions
/// before).
#[gpui::test]
fn rc2ux_the_memory_card_searches_what_was_typed(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("rc2ux_layout_tests::rc2ux_the_memory_card_searches_what_was_typed")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        work_app(app, &home);
        let id = conversation(app, 9_000_000, "Mem", &[]);
        app.set_active_conversation(&id);
        app.memory.base_url = "http://127.0.0.1:9".into();
        app.work.memory_open = true;
        app.shell.job_tab = crate::job_panel::JobTab::Context;
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window
            .debug_bounds(crate::work::MEMORY_SEARCH_SELECTOR)
            .is_some(),
        "the search box is in the memory card"
    );
    view.update(window, |app, cx| {
        app.memory_search.update(cx, |c, cx| {
            c.set_text("boiler service date", cx);
            cx.emit(crate::composer::ComposerEvent::Submit);
        });
    });
    window.run_until_parked();
    let asked = view.read_with(window, |app, _| app.work.memory_query.clone());
    assert_eq!(asked, "boiler service date");
}

/// RC2-UX (D-R1 m6: after a relaunch the "N steps" disclosure was gone): a
/// Work reply's steps are saved with its footer and are back after a
/// relaunch.
#[gpui::test]
fn rc2ux_reply_steps_come_back_after_a_relaunch(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("rc2ux_layout_tests::rc2ux_reply_steps_come_back_after_a_relaunch")
    else {
        return;
    };
    let store = awili_persist::Store::open_default().unwrap();
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    let folder = home.join("st");
    std::fs::create_dir_all(&folder).unwrap();
    let id = view.update_in(window, |app, window, cx| {
        work_app(app, &home);
        app.work.folder = Some(folder.clone());
        app.new_conversation(window, cx);
        let id = app.sidebar.active_id().to_string();
        // A new thread starts on the default helper; this one is Claude's.
        assert!(app.set_composer_target(
            &crate::workshop_composer::ModelOwner::Work(id.clone()),
            WorkExecutor::Agent {
                engine: CodeEngine::Claude,
                model: None,
            },
            cx,
        ));
        app.submit_work_run("plan the trip".into(), cx);
        let idx = app.work.stream_row_idx.expect("the reply row");
        let row = &mut app.views.get_mut(&id).unwrap().rows[idx];
        row.push_activity("Read notes.txt");
        row.push_activity("Listed the folder");
        app.cancel_work_run(cx);
        id
    });
    window.run_until_parked();
    let key = crate::run_footer::steps_key(&id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while store.get_app_state(&key).unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline, "the steps are saved");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (again, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    again.update_in(window2, |app, _window, _cx| {
        let last = app.views[&id].rows.last().expect("the reply");
        let lines: Vec<String> = last.activity.iter().map(|l| l.to_string()).collect();
        assert_eq!(lines, ["Read notes.txt", "Listed the folder"]);
    });
}

/// RC2-UX (D-R1 m6: identical Code titles could not be told apart): a new
/// Code conversation is filed under its project's folder, as Work ones are.
#[gpui::test]
fn rc2ux_a_code_conversation_is_filed_under_its_project(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("rc2ux_layout_tests::rc2ux_a_code_conversation_is_filed_under_its_project")
    else {
        return;
    };
    cx.executor().allow_parking();
    let proj = home.join("garden-app");
    std::fs::create_dir_all(&proj).unwrap();
    let entity = gpui::AppContext::new(cx, AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.work.agents.claude_bin = Some(home.join("no-such-claude").display().to_string());
        app.set_mode(Mode::Code, cx);
        app.add_code_project(proj.clone(), cx);
        app.submit_code_turn("fix the build".into(), cx);
        let tab = &app.work.code_tabs[app.work.code_active];
        let conv = tab.history.conv_id.clone().expect("a conversation");
        assert_eq!(
            app.conversations
                .get(&conv)
                .and_then(|c| c.cwd.clone())
                .as_deref(),
            Some(proj.to_str().unwrap())
        );
    });
}
/// RC2-R510 (usability RC2 round 4, Minor: the "Ask before changes" menu did
/// not close on Esc): Esc closes it, nothing chosen.
#[gpui::test]
fn rc2r510_esc_closes_the_mode_menu(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("rc2ux_layout_tests::rc2r510_esc_closes_the_mode_menu") else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let before = view.update_in(window, |app, window, cx| {
        work_app(app, &home);
        app.set_mode(Mode::Work, cx);
        let conv = app.sidebar.active_id().to_string();
        app.work.ask_menu = crate::ask_mode_ui::AskMenu::Open(crate::ask_mode::AskKey::Work(conv));
        let h = app.composer.read(cx).focus_handle().clone();
        window.focus(&h, cx);
        cx.notify();
        app.work.ask_modes.mode_for(&app.work_chip_key())
    });
    window.run_until_parked();
    assert!(window
        .debug_bounds(crate::ask_mode_ui::ASK_MENU_CARD_SELECTOR)
        .is_some());
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(
        window
            .debug_bounds(crate::ask_mode_ui::ASK_MENU_CARD_SELECTOR)
            .is_none(),
        "Esc closed it"
    );
    view.read_with(window, |app, _| {
        assert_eq!(
            app.work.ask_modes.mode_for(&app.work_chip_key()),
            before,
            "nothing chosen"
        );
    });
}

/// RC2-R510 (usability RC2 round 4, Minor: after choosing a helper in the
/// picker, the first typing did not reach the message box): after a pick,
/// typing goes into the box.
#[gpui::test]
fn rc2r510_typing_after_a_helper_pick_reaches_the_box(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("rc2ux_layout_tests::rc2r510_typing_after_a_helper_pick_reaches_the_box")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        work_app(app, &home);
        app.set_mode(Mode::Work, cx);
        app.work.executor_picker_open = true;
        cx.notify();
    });
    window.run_until_parked();
    let row = window
        .debug_bounds("rc2r510-helper-row-0")
        .expect("the picker lists helpers");
    window.simulate_click(row.center(), gpui::Modifiers::none());
    window.run_until_parked();
    window.simulate_keystrokes("h i");
    window.run_until_parked();
    view.read_with(window, |app, cx| {
        assert_eq!(
            app.composer.read(cx).text(),
            "hi",
            "the keys reached the box"
        );
    });
}
