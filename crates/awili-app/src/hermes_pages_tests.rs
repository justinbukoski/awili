//! RC2-R58: the Hermes pages say what they are, what state Hermes is in and
//! what the owner can do, in the same words.

use super::*;
use crate::work::Mode;

const CHILD: &str = "AWILI_RC2R57_TEST_HOME";

fn in_isolated_child(test: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    false
}

/// A client failure reads as a plain sentence, never "error sending request
/// for url (…)".
#[test]
fn rc2r58_a_read_failure_is_plain() {
    let raw = "dashboard unreachable at http://127.0.0.1:9119: error sending request for url \
               (http://127.0.0.1:9119/)";
    assert_eq!(
        plain_read_failure("http://127.0.0.1:9119", raw),
        "Awili couldn\u{2019}t read Hermes at http://127.0.0.1:9119: nothing answers there."
    );
    assert_eq!(
        plain_read_failure("http://h", "HTTP 401: bad token"),
        "Awili couldn\u{2019}t read Hermes at http://h: HTTP 401: bad token."
    );
}

/// The round-4 setup (the Hermes gateway address set to this Mac, where
/// nothing answers; Hermes's own service address not set): Skills,
/// Automations, System and Messaging each say what they are, then the same
/// state in the same words, with the address and the two buttons — never
/// the raw error, "config.toml" or "[hermes]".
#[gpui::test]
fn rc2r58_the_hermes_pages_agree_in_plain_words(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("hermes_pages::tests::rc2r58_the_hermes_pages_agree_in_plain_words") {
        return;
    }
    let (view, window) = cx.add_window_view(|_window, cx| crate::AwiliApp::new(cx));
    let url = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        format!("http://127.0.0.1:{port}")
    };
    view.update(window, |app, _| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.work.hermes.enabled = false;
        app.work.hermes.base_url = String::new();
        app.work.gateway.enabled = true;
        app.work.gateway.dashboard_url = url.clone();
    });
    let mut texts = Vec::new();
    for (mode, page) in [
        (Mode::Skills, HermesPage::Skills),
        (Mode::Cron, HermesPage::Automations),
        (Mode::CommandCenter, HermesPage::System),
        (Mode::Messaging, HermesPage::Messaging),
    ] {
        crate::gateway_health_app::TEST_PROBE_RESULT
            .with(|slot| *slot.borrow_mut() = Some(Err("connection refused".into())));
        view.update(window, |app, cx| {
            app.set_mode(Mode::Work, cx);
            app.set_mode(mode, cx);
            // The dashboard read failed as the round-4 build saw it.
            app.skills_state.loading = false;
            app.skills_state.error = Some(format!(
                "dashboard unreachable at {url}: error sending request for url ({url}/)"
            ));
            cx.notify();
        });
        window.run_until_parked();
        assert!(
            window
                .debug_bounds(Box::leak(notice_selector(page).into_boxed_str()))
                .is_some(),
            "{page:?} shows its notice"
        );
        let text = view.update(window, |app, _| app.hermes_page_notice(page).unwrap());
        assert!(text.starts_with(page.intro()), "{text}");
        for bad in [
            "error sending request",
            "config.toml",
            "[hermes]",
            "dashboard",
        ] {
            assert!(!text.contains(bad), "{page:?}: {text}");
        }
        assert!(text.contains(&url), "it names the address: {text}");
        assert!(
            text.contains(crate::gateway_health::GATEWAY_UNREACHABLE),
            "the gateway's real state: {text}"
        );
        texts.push(text[page.intro().len()..].to_string());
    }
    assert!(
        texts.windows(2).all(|w| w[0] == w[1]),
        "every page says the same about Hermes: {texts:#?}"
    );
}

/// RC2-R5M2 (slice review befb867d, P2): Hermes is turned on, but nothing
/// answers at its address. Automations, System and Messaging (and Skills)
/// each keep the shared notice built from the page's own failed read: what
/// the page is, the plain state sentence with the address, and "Check
/// again" and "Open Settings" — never a bare error line without them.
#[gpui::test]
fn rc2r5m2_an_enabled_but_unreachable_hermes_keeps_the_notice(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "hermes_pages::tests::rc2r5m2_an_enabled_but_unreachable_hermes_keeps_the_notice",
    ) {
        return;
    }
    let (view, window) = cx.add_window_view(|_window, cx| crate::AwiliApp::new(cx));
    let url = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        format!("http://127.0.0.1:{port}")
    };
    view.update(window, |app, _| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.work.hermes.enabled = true;
        app.work.hermes.base_url = url.clone();
        app.work.hermes.dashboard_url = url.clone();
        app.work.gateway.enabled = false;
    });
    let raw = format!("dashboard unreachable at {url}: error sending request for url ({url}/)");
    for (mode, page) in [
        (Mode::Cron, HermesPage::Automations),
        (Mode::CommandCenter, HermesPage::System),
        (Mode::Messaging, HermesPage::Messaging),
        (Mode::Skills, HermesPage::Skills),
    ] {
        crate::gateway_health_app::TEST_PROBE_RESULT
            .with(|slot| *slot.borrow_mut() = Some(Err("connection refused".into())));
        view.update(window, |app, cx| {
            // A read already in flight: opening the page starts none (no
            // worker thread wakes the test scheduler).
            app.skills_state.loading = true;
            app.cron_state.loading = true;
            app.command_center_data.loading = true;
            app.messaging_data.loading = true;
            app.set_mode(Mode::Work, cx);
            app.set_mode(mode, cx);
            // The page's own read failed: nothing answers at the address.
            match page {
                HermesPage::Automations => {
                    app.cron_state.loading = false;
                    app.cron_state.error = Some(raw.clone());
                }
                HermesPage::System => {
                    app.command_center_data.loading = false;
                    app.command_center_data.status = None;
                    app.command_center_data.error = Some(raw.clone());
                }
                HermesPage::Messaging => {
                    app.messaging_data.loading = false;
                    app.messaging_data.loaded = false;
                    app.messaging_data.error = Some(raw.clone());
                }
                HermesPage::Skills => {
                    app.skills_state.loading = false;
                    app.skills_state.error = Some(raw.clone());
                }
            }
            cx.notify();
        });
        window.run_until_parked();
        let text = view
            .update(window, |app, _| app.hermes_page_notice(page))
            .unwrap_or_else(|| panic!("{page:?} has a notice while its read failed"));
        assert!(
            window
                .debug_bounds(Box::leak(notice_selector(page).into_boxed_str()))
                .is_some(),
            "{page:?} shows the shared notice with Check again and Open Settings"
        );
        assert!(text.starts_with(page.intro()), "{text}");
        assert_eq!(
            text,
            format!("{} {}", page.intro(), plain_read_failure(&url, &raw)),
            "{page:?} says the state plainly with the address"
        );
        assert!(text.contains(&url), "{text}");
        assert!(!text.contains("error sending request"), "{text}");
    }
}

/// RC2-R5M6 (slice review 3775ed08, P2): System and Messaging read Hermes
/// once, then a refresh finds nothing answering. Each keeps what it read
/// (the status, the connectors) AND shows the shared notice above it — the
/// failed connection with the address, "Check again" and "Open Settings" —
/// never the stale content alone.
#[gpui::test]
fn rc2r5m6_a_failed_refresh_after_a_good_read_keeps_the_notice(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "hermes_pages::tests::rc2r5m6_a_failed_refresh_after_a_good_read_keeps_the_notice",
    ) {
        return;
    }
    let (view, window) = cx.add_window_view(|_window, cx| crate::AwiliApp::new(cx));
    let url = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        format!("http://127.0.0.1:{port}")
    };
    view.update(window, |app, _| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.work.hermes.enabled = true;
        app.work.hermes.base_url = url.clone();
        app.work.hermes.dashboard_url = url.clone();
        app.work.gateway.enabled = false;
    });
    let raw = format!("dashboard unreachable at {url}: error sending request for url ({url}/)");
    for (mode, page, kept) in [
        (
            Mode::CommandCenter,
            HermesPage::System,
            crate::work::CC_STATUS_SELECTOR,
        ),
        (
            Mode::Messaging,
            HermesPage::Messaging,
            crate::work::MESSAGING_HEADER_SELECTOR,
        ),
    ] {
        crate::gateway_health_app::TEST_PROBE_RESULT
            .with(|slot| *slot.borrow_mut() = Some(Err("connection refused".into())));
        // The first read succeeded (as its success branch leaves the page).
        view.update(window, |app, cx| {
            // A read already in flight: opening the page starts none.
            app.command_center_data.loading = true;
            app.messaging_data.loading = true;
            app.set_mode(Mode::Work, cx);
            app.set_mode(mode, cx);
            app.command_center_data.loading = false;
            app.command_center_data.error = None;
            app.command_center_data.status = Some(serde_json::from_str("{}").unwrap());
            app.messaging_data.loading = false;
            app.messaging_data.error = None;
            app.messaging_data
                .state
                .apply_snapshot(crate::messaging_pane::tests_pub_snap(vec![
                    crate::messaging_pane::tests_pub_platform("telegram", "Telegram"),
                ]));
            app.messaging_data.loaded = true;
            cx.notify();
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds(kept).is_some(),
            "{page:?} shows its read"
        );
        assert!(
            window
                .debug_bounds(Box::leak(notice_selector(page).into_boxed_str()))
                .is_none(),
            "{page:?}: no notice while Hermes answers"
        );
        // The refresh found nothing answering (as its failure branch leaves
        // the page: the earlier read kept, the error set).
        view.update(window, |app, cx| {
            app.command_center_data.error = Some(raw.clone());
            app.messaging_data.error = Some(raw.clone());
            cx.notify();
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds(kept).is_some(),
            "{page:?} keeps what it read"
        );
        assert!(
            window
                .debug_bounds(Box::leak(notice_selector(page).into_boxed_str()))
                .is_some(),
            "{page:?} shows the shared notice with Check again and Open Settings"
        );
        let text = view.update(window, |app, _| app.hermes_page_notice(page));
        assert_eq!(
            text,
            Some(format!(
                "{} {}",
                page.intro(),
                plain_read_failure(&url, &raw)
            )),
            "{page:?} says the failed connection with the address"
        );
    }
}

/// RC2-R65 (usability RC2 round 5, Minor 8): the Hermes pages said the
/// address "is not set yet" and then "Its address … is http://127.0.0.1:9119";
/// System and Messaging had no title; "Check again" showed no feedback. Each
/// address is now named as what it is, System and Messaging carry their
/// title, and a press of Check again shows when it checked.
#[gpui::test]
fn rc2r65_the_hermes_pages_name_each_address_once(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("hermes_pages::tests::rc2r65_the_hermes_pages_name_each_address_once") {
        return;
    }
    let (view, window) = cx.add_window_view(|_window, cx| crate::AwiliApp::new(cx));
    let url = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        format!("http://127.0.0.1:{port}")
    };
    view.update(window, |app, _| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.work.hermes.enabled = false;
        app.work.hermes.base_url = String::new();
        app.work.gateway.enabled = true;
        app.work.gateway.dashboard_url = url.clone();
    });
    for (mode, page) in [
        (Mode::Skills, HermesPage::Skills),
        (Mode::Cron, HermesPage::Automations),
        (Mode::CommandCenter, HermesPage::System),
        (Mode::Messaging, HermesPage::Messaging),
    ] {
        crate::gateway_health_app::TEST_PROBE_RESULT
            .with(|slot| *slot.borrow_mut() = Some(Err("connection refused".into())));
        view.update(window, |app, cx| {
            app.set_mode(Mode::Work, cx);
            app.set_mode(mode, cx);
            app.skills_state.loading = false;
            app.skills_state.error = Some(format!(
                "dashboard unreachable at {url}: error sending request for url ({url}/)"
            ));
            cx.notify();
        });
        window.run_until_parked();
        let text = view.update(window, |app, _| app.hermes_page_notice(page).unwrap());
        assert!(!text.contains("Its address"), "{page:?}: {text}");
        assert!(
            text.contains(&format!("The fleet gateway itself is at {url}")),
            "{page:?}: {text}"
        );
        assert_eq!(
            text.matches("not set yet").count(),
            1,
            "only the missing address is not set: {text}"
        );
        let titled = window
            .debug_bounds(Box::leak(title_selector(page).into_boxed_str()))
            .is_some();
        assert_eq!(titled, page.title().is_some(), "{page:?} title");
        let checked = Box::leak(checked_selector(page).into_boxed_str());
        assert!(
            window.debug_bounds(checked).is_none(),
            "{page:?}: no press yet"
        );
        crate::gateway_health_app::TEST_PROBE_RESULT
            .with(|slot| *slot.borrow_mut() = Some(Err("connection refused".into())));
        view.update(window, |app, cx| app.hermes_check_again(page, cx));
        window.run_until_parked();
        assert!(
            window.debug_bounds(checked).is_some(),
            "{page:?}: Check again shows when it checked"
        );
    }
    assert_eq!(HermesPage::System.title(), Some("System"));
    assert_eq!(HermesPage::Messaging.title(), Some("Messaging"));
}

/// Messaging's list, detail and refresh controls are painted KIT faces in the
/// real page. The same rendered state remains usable at compact width, and a
/// pointer press on Refresh enters the existing loading transition.
#[gpui::test]
fn messaging_painted_kit_controls_keep_targets_and_refresh(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "hermes_pages::tests::messaging_painted_kit_controls_keep_targets_and_refresh",
    ) {
        return;
    }
    let (view, window) = cx.add_window_view(|_window, cx| crate::AwiliApp::new(cx));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.work.hermes.enabled = true;
        app.work.hermes.base_url = address.clone();
        app.work.hermes.dashboard_url = address.clone();
        app.messaging_data.loading = true;
        app.messaging_data.loaded = true;
        let mut telegram = crate::messaging_pane::tests_pub_platform("telegram", "Telegram");
        telegram.enabled = true;
        telegram.configured = true;
        telegram
            .env_vars
            .push(awili_gateway::rest::MessagingEnvVar {
                key: "TG_BOT_TOKEN".into(),
                required: true,
                is_set: true,
                redacted_value: Some("••••••••".into()),
                is_password: true,
                description: "Telegram bot token".into(),
                ..Default::default()
            });
        app.messaging_data
            .state
            .apply_snapshot(crate::messaging_pane::tests_pub_snap(vec![
                telegram,
                crate::messaging_pane::tests_pub_platform("signal", "Signal"),
            ]));
        app.set_mode(Mode::Work, cx);
        app.set_mode(Mode::Messaging, cx);
        app.messaging_data.loading = false;
        app.messaging_data.error = None;
        cx.notify();
    });
    window.run_until_parked();

    for (selector, label) in [
        ("msg-refresh", "Refresh"),
        ("msg-connector-row-0", "first connector row"),
        ("msg-connector-row-1", "second connector row"),
        ("ui-card-msg-connector-detail", "selected connector detail"),
        ("msg-connector-toggle", "enabled toggle"),
        ("msg-test", "self-test action"),
        ("msg-env-edit-TG_BOT_TOKEN", "credential edit action"),
        ("msg-env-clear-TG_BOT_TOKEN", "credential clear action"),
    ] {
        assert!(
            window.debug_bounds(selector).is_some(),
            "{label} is painted"
        );
    }
    for selector in [
        "msg-refresh",
        "msg-connector-row-0",
        "msg-connector-row-1",
        "msg-connector-toggle",
        "msg-test",
        "msg-env-edit-TG_BOT_TOKEN",
        "msg-env-clear-TG_BOT_TOKEN",
    ] {
        let bounds = window.debug_bounds(selector).unwrap();
        assert!(
            f32::from(bounds.size.width) >= 44.0 && f32::from(bounds.size.height) >= 44.0,
            "{selector} meets the shared target floor: {bounds:?}"
        );
    }
    let list = window
        .debug_bounds("ui-section-msg-connections-section")
        .unwrap();
    let detail = window.debug_bounds("ui-card-msg-connector-detail").unwrap();
    assert!(
        list.origin.x + list.size.width <= detail.origin.x + gpui::px(1.0),
        "list and detail do not overlap: {list:?} / {detail:?}"
    );

    for scale in [0.9_f32, 1.0, 1.25, 2.5] {
        window.simulate_resize(gpui::size(gpui::px(900.0), gpui::px(712.0)));
        view.update_in(window, |app, win, cx| {
            app.apply_ui_scale(scale, win, cx);
            app.shell.backstage_scroll.set_offset(Default::default());
            cx.notify();
        });
        window.run_until_parked();

        let viewport = window.update(|win, _| win.viewport_size());
        assert!(
            (f32::from(viewport.width) - 900.0).abs() <= 1.0
                && (f32::from(viewport.height) - 712.0).abs() <= 1.0,
            "the scale {scale} case uses the requested real 900×712 viewport: {viewport:?}"
        );
        let body = window
            .debug_bounds("awili-backstage-body")
            .expect("Backstage allocates its real scroll viewport");
        assert!(
            body.left() >= gpui::px(0.0)
                && body.right() <= viewport.width
                && body.top() >= gpui::px(0.0)
                && body.bottom() <= viewport.height,
            "Backstage's allocated viewport stays on screen at {scale}: {body:?} / {viewport:?}"
        );
        let list = window
            .debug_bounds("ui-section-msg-connections-section")
            .expect("the connector list is painted");
        let detail = window
            .debug_bounds("ui-card-msg-connector-detail")
            .expect("the detail card is painted");
        for (name, face) in [("list", list), ("detail", detail)] {
            assert!(
                face.left() >= body.left() && face.right() <= body.right(),
                "the complete {name} face stays within the allocated Backstage width at {scale}: {face:?} / {body:?}"
            );
        }

        for selector in [
            "msg-refresh",
            "msg-connector-row-0",
            "msg-connector-row-1",
            "msg-connector-toggle",
            "msg-test",
            "msg-env-edit-TG_BOT_TOKEN",
            "msg-env-clear-TG_BOT_TOKEN",
        ] {
            view.update(window, |app, cx| {
                app.shell.backstage_scroll.set_offset(Default::default());
                cx.notify();
            });
            window.run_until_parked();
            for _ in 0..24 {
                let body = window.debug_bounds("awili-backstage-body").unwrap();
                let face = window
                    .debug_bounds(selector)
                    .unwrap_or_else(|| panic!("{selector} remains painted at scale {scale}"));
                if face.top() >= body.top() && face.bottom() <= body.bottom() {
                    break;
                }
                assert!(
                    face.size.height <= body.size.height,
                    "the Backstage viewport can contain the complete {selector} target at {scale}: {face:?} / {body:?}"
                );
                let overflow = if face.bottom() > body.bottom() {
                    -(face.bottom() - body.bottom())
                } else {
                    body.top() - face.top()
                };
                let step = overflow.clamp(gpui::px(-160.0), gpui::px(160.0));
                window.simulate_event(gpui::ScrollWheelEvent {
                    position: gpui::point(body.left() + gpui::px(1.0), body.top() + gpui::px(1.0)),
                    delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), step)),
                    ..Default::default()
                });
                window.run_until_parked();
            }
            let body = window.debug_bounds("awili-backstage-body").unwrap();
            let face = window.debug_bounds(selector).unwrap();
            assert!(
                face.left() >= body.left() && face.right() <= body.right(),
                "the complete {selector} face stays inside Backstage horizontally at {scale}: {face:?} / {body:?}"
            );
            assert!(
                face.top() >= body.top() && face.bottom() <= body.bottom(),
                "real Backstage scrolling reveals the complete {selector} face at {scale}: {face:?} / {body:?}"
            );
        }
    }
    view.update_in(window, |app, win, cx| {
        app.apply_ui_scale(1.0, win, cx);
        app.shell.backstage_scroll.set_offset(Default::default());
        cx.notify();
    });
    window.run_until_parked();

    let second_row = window.debug_bounds("msg-connector-row-1").unwrap().center();
    window.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position: second_row,
        modifiers: gpui::Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    window.simulate_event(gpui::MouseUpEvent {
        button: gpui::MouseButton::Left,
        position: second_row,
        modifiers: gpui::Modifiers::default(),
        click_count: 1,
    });
    window.run_until_parked();
    assert_eq!(
        view.read_with(window, |app, _| app
            .messaging_data
            .state
            .selected_id()
            .map(str::to_owned)),
        Some("signal".to_string())
    );

    window.simulate_resize(gpui::size(gpui::px(600.0), gpui::px(600.0)));
    window.run_until_parked();
    let refresh = window
        .debug_bounds("msg-refresh")
        .expect("refresh remains visible at compact width");
    assert!(f32::from(refresh.size.width) >= 44.0 && f32::from(refresh.size.height) >= 44.0);
    assert!(
        window
            .debug_bounds("ui-card-msg-connector-detail")
            .is_some(),
        "detail remains available in compact scrolling layout"
    );

    let at = refresh.center();
    window.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position: at,
        modifiers: gpui::Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    window.simulate_event(gpui::MouseUpEvent {
        button: gpui::MouseButton::Left,
        position: at,
        modifiers: gpui::Modifiers::default(),
        click_count: 1,
    });
    assert!(
        view.read_with(window, |app, _| app.messaging_data.loading),
        "the real Refresh control enters loading"
    );
    drop(listener);
}

/// Inner Backstage lists consume a wheel while they can scroll and pass the
/// same real event to the outer section only when they reach their limit.
fn messaging_bounds_intersection(
    a: gpui::Bounds<gpui::Pixels>,
    b: gpui::Bounds<gpui::Pixels>,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    let left = a.left().max(b.left());
    let top = a.top().max(b.top());
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (right > left && bottom > top).then(|| {
        gpui::Bounds::new(
            gpui::point(left, top),
            gpui::size(right - left, bottom - top),
        )
    })
}

fn messaging_face_is_contained(
    viewport: gpui::Bounds<gpui::Pixels>,
    face: gpui::Bounds<gpui::Pixels>,
) -> bool {
    face.left() >= viewport.left()
        && face.right() <= viewport.right()
        && face.top() >= viewport.top()
        && face.bottom() <= viewport.bottom()
}

#[gpui::test]
fn messaging_inner_lists_scroll_before_backstage(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("hermes_pages::tests::messaging_inner_lists_scroll_before_backstage") {
        return;
    }
    fn offsets(app: &crate::AwiliApp, selector: &str) -> (gpui::Pixels, gpui::Pixels) {
        let handle = if selector == "msg-env-scroll" {
            &app.shell.messaging_env_scroll
        } else {
            &app.shell.messaging_connectors_scroll
        };
        (handle.offset().y, handle.max_offset().y)
    }
    fn reset(app: &crate::AwiliApp) {
        app.shell
            .messaging_env_scroll
            .set_offset(Default::default());
        app.shell
            .messaging_connectors_scroll
            .set_offset(Default::default());
        app.shell.backstage_scroll.set_offset(Default::default());
    }
    let (view, window) = cx.add_window_view(|_window, cx| crate::AwiliApp::new(cx));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    window.simulate_resize(gpui::size(gpui::px(900.0), gpui::px(712.0)));
    view.update_in(window, |app, win, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.work.hermes.enabled = true;
        app.work.hermes.base_url = address.clone();
        app.work.hermes.dashboard_url = address.clone();
        app.messaging_data.loading = true;
        app.messaging_data.loaded = true;
        let mut telegram = crate::messaging_pane::tests_pub_platform("telegram", "Telegram");
        telegram.enabled = true;
        telegram.configured = true;
        telegram.env_vars = (0..16)
            .map(|ix| awili_gateway::rest::MessagingEnvVar {
                key: format!("ENV_{ix}"),
                required: false,
                is_set: true,
                redacted_value: Some("••••••••".into()),
                is_password: true,
                description: "Test credential".into(),
                ..Default::default()
            })
            .collect();
        let mut platforms = vec![telegram];
        for ix in 0..16 {
            platforms.push(crate::messaging_pane::tests_pub_platform(
                &format!("connector-{ix}"),
                &format!("Connector {ix}"),
            ));
        }
        app.set_mode(Mode::Work, cx);
        app.set_mode(Mode::Messaging, cx);
        // Entering Messaging starts the real refresh path; seed the stable
        // fixture after that transition so its state cannot race the test.
        app.messaging_data
            .state
            .apply_snapshot(crate::messaging_pane::tests_pub_snap(platforms));
        app.messaging_data.state.select_id("telegram");
        app.messaging_data.loading = false;
        app.messaging_data.error = None;
        app.apply_ui_scale(1.0, win, cx);
        cx.notify();
    });
    window.run_until_parked();
    let (selected_id, env_count) = view.read_with(window, |app, _| {
        (
            app.messaging_data.state.selected_id().map(str::to_owned),
            app.messaging_data.state.selected_env_vars().len(),
        )
    });
    assert_eq!(selected_id.as_deref(), Some("telegram"));
    assert_eq!(env_count, 16, "the selected connector has 16 env rows");

    for selector in ["msg-env-scroll", "msg-connector-list-scroll"] {
        view.update(window, |app, _| reset(app));
        window.run_until_parked();
        // In the short Backstage stage, the selected detail's env list may
        // begin below the outer viewport. Reveal it through the same blank
        // body padding users can wheel; do not access a clipped target by
        // mutating a scroll offset.
        let mut viewport = None;
        for _ in 0..32 {
            let body = window
                .debug_bounds("awili-backstage-body")
                .expect("Backstage owns an outer scroll viewport");
            if let Some(inner) = window.debug_bounds(selector) {
                let visible_top = inner.top().max(body.top());
                let visible_bottom = inner.bottom().min(body.bottom());
                if visible_bottom > visible_top {
                    viewport = Some(inner);
                    break;
                }
            }
            window.simulate_event(gpui::ScrollWheelEvent {
                position: gpui::point(body.left() + gpui::px(1.0), body.top() + gpui::px(1.0)),
                delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(-160.0))),
                ..Default::default()
            });
            window.run_until_parked();
        }
        let Some(viewport) = viewport else {
            let (selected, env_count, outer_offset, outer_max) =
                view.read_with(window, |app, _| {
                    (
                        app.messaging_data.state.selected_id().map(str::to_owned),
                        app.messaging_data.state.selected_env_vars().len(),
                        app.shell.backstage_scroll.offset().y,
                        app.shell.backstage_scroll.max_offset().y,
                    )
                });
            panic!(
                "real outer wheels reveal {selector} inside Backstage; selected={selected:?}, env_count={env_count}, body={:?}, section={:?}, outer_offset={outer_offset:?}, outer_max={outer_max:?}, inner={:?}",
                window.debug_bounds("awili-backstage-body"),
                window.debug_bounds("awili-backstage-section"),
                window.debug_bounds(selector)
            );
        };
        let (inner_before, inner_max, outer_before, outer_max) = view.update(window, |app, _| {
            let (offset, max) = offsets(app, selector);
            (
                offset,
                max,
                app.shell.backstage_scroll.offset().y,
                app.shell.backstage_scroll.max_offset().y,
            )
        });
        assert!(
            inner_max > gpui::Pixels::ZERO,
            "{selector} has real overflow"
        );
        assert!(
            outer_max > gpui::Pixels::ZERO,
            "Backstage can receive a handoff"
        );
        let body = window.debug_bounds("awili-backstage-body").unwrap();
        let wheel_y = (viewport.top().max(body.top()) + viewport.bottom().min(body.bottom())) / 2.0;
        let wheel_position = gpui::point(viewport.center().x, wheel_y);
        window.simulate_event(gpui::ScrollWheelEvent {
            position: wheel_position,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(-80.0))),
            ..Default::default()
        });
        window.run_until_parked();
        let (inner_after, outer_after) = view.update(window, |app, _| {
            (
                offsets(app, selector).0,
                app.shell.backstage_scroll.offset().y,
            )
        });
        assert!(inner_after < inner_before, "{selector} consumes the wheel");
        assert_eq!(
            outer_after, outer_before,
            "Backstage does not double-scroll"
        );

        let (_, max) = view.update(window, |app, _| offsets(app, selector));
        window.simulate_event(gpui::ScrollWheelEvent {
            position: wheel_position,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), -max - gpui::px(80.0))),
            ..Default::default()
        });
        window.run_until_parked();
        let at_limit = view.update(window, |app, _| offsets(app, selector).0);
        assert_eq!(at_limit, -inner_max, "{selector} reached its inner limit");
        let outer_before_handoff =
            view.update(window, |app, _| app.shell.backstage_scroll.offset().y);
        window.simulate_event(gpui::ScrollWheelEvent {
            position: wheel_position,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(-80.0))),
            ..Default::default()
        });
        window.run_until_parked();
        let (inner_at_handoff, outer_after_handoff) = view.update(window, |app, _| {
            (
                offsets(app, selector).0,
                app.shell.backstage_scroll.offset().y,
            )
        });
        assert_eq!(inner_at_handoff, at_limit, "{selector} stays clamped");
        assert!(
            outer_after_handoff < outer_before_handoff,
            "the same wheel reaches Backstage at {selector}'s limit"
        );
    }

    window.simulate_resize(gpui::size(gpui::px(900.0), gpui::px(712.0)));
    view.update_in(window, |app, win, cx| {
        app.apply_ui_scale(2.5, win, cx);
        app.shell
            .messaging_env_scroll
            .set_offset(Default::default());
        app.shell.backstage_scroll.set_offset(Default::default());
        cx.notify();
    });
    window.run_until_parked();
    let edit_focus = view.update(window, |app, _| {
        app.shell.messaging_action_focus["edit:telegram:ENV_15"].clone()
    });
    let clear_focus = view.update(window, |app, _| {
        app.shell.messaging_action_focus["clear:telegram:ENV_15"].clone()
    });
    for (selector, focus) in [
        ("msg-env-edit-ENV_15", edit_focus),
        ("msg-env-clear-ENV_15", clear_focus),
    ] {
        let tab_focus = view.update(window, |app, _| app.shell.backstage_focus.clone());
        window.update(|window, cx| {
            window.focus(&tab_focus, cx);
        });
        view.update(window, |app, cx| {
            reset(app);
            cx.notify();
        });
        window.run_until_parked();
        assert!(
            window.update(|window, _| tab_focus.is_focused(window)),
            "the visible Backstage tab strip is the real Tab traversal start"
        );
        let mut viewport_visible = false;
        for _ in 0..32 {
            let viewport = window
                .debug_bounds("msg-env-scroll")
                .expect("the environment list has a real clipped viewport");
            let body = window
                .debug_bounds("awili-backstage-body")
                .expect("the Backstage body has a real clipped viewport");
            if viewport.bottom() > body.top() && viewport.top() < body.bottom() {
                viewport_visible = true;
                break;
            }
            window.simulate_event(gpui::ScrollWheelEvent {
                position: gpui::point(body.left() + gpui::px(1.0), body.top() + gpui::px(1.0)),
                delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(-160.0))),
                ..Default::default()
            });
            window.run_until_parked();
        }
        assert!(
            viewport_visible,
            "outer real wheels reveal the env viewport for {selector}"
        );
        let initial = window
            .debug_bounds(selector)
            .expect("the last environment action remains painted");
        let viewport = window.debug_bounds("msg-env-scroll").unwrap();
        assert!(
            initial.top() < viewport.top() || initial.bottom() > viewport.bottom(),
            "{selector} starts clipped by the inner env viewport before keyboard traversal: {initial:?} / {viewport:?}"
        );
        let mut seen = Vec::<gpui::FocusHandle>::new();
        for _ in 0..512 {
            if window.update(|window, _| focus.is_focused(window)) {
                break;
            }
            let current = window
                .update(|window, cx| window.focused(cx))
                .expect("Tab traversal retains a focus target");
            assert!(
                !seen.contains(&current),
                "Tab cycled before reaching {selector}; current={current:?}, target={focus:?}, stops_seen={} ({seen:?}), target_bounds={:?}",
                seen.len(),
                window.debug_bounds(selector)
            );
            seen.push(current);
            window.simulate_keystrokes("tab");
            window.run_until_parked();
        }
        assert!(
            window.update(|window, _| focus.is_focused(window)),
            "Tab reaches {selector}"
        );
        let focused = window
            .debug_bounds(selector)
            .expect("the focused action remains painted");
        let viewport = window
            .debug_bounds("msg-env-scroll")
            .expect("the environment viewport remains painted");
        let body = window
            .debug_bounds("awili-backstage-body")
            .expect("the Backstage viewport remains painted");
        let window_size = window.update(|window, _| window.viewport_size());
        let window_bounds =
            gpui::Bounds::new(gpui::point(gpui::px(0.0), gpui::px(0.0)), window_size);
        let physical_viewport = [body, window_bounds]
            .into_iter()
            .try_fold(viewport, messaging_bounds_intersection)
            .expect("the environment list intersects the physical window");
        assert!(
            messaging_face_is_contained(physical_viewport, focused),
            "focused {selector} is fully visible in the inner, outer, and physical window viewports: {focused:?} / {viewport:?} ∩ {body:?} ∩ {window_bounds:?}"
        );

        let before_wheel =
            view.read_with(window, |app, _| app.shell.messaging_env_scroll.offset().y);
        assert!(
            before_wheel < gpui::Pixels::ZERO,
            "Tab reveal scrolled the env list"
        );
        let inner = window.debug_bounds("msg-env-scroll").unwrap();
        let body = window.debug_bounds("awili-backstage-body").unwrap();
        let window_size = window.update(|window, _| window.viewport_size());
        let window_bounds =
            gpui::Bounds::new(gpui::point(gpui::px(0.0), gpui::px(0.0)), window_size);
        let physical_viewport = [body, window_bounds]
            .into_iter()
            .try_fold(inner, messaging_bounds_intersection)
            .expect("the environment wheel viewport intersects the physical window");
        let wheel_position = physical_viewport.center();
        assert!(
            wheel_position.x >= physical_viewport.left()
                && wheel_position.x <= physical_viewport.right()
                && wheel_position.y >= physical_viewport.top()
                && wheel_position.y <= physical_viewport.bottom(),
            "real wheel is dispatched inside the nested physical viewport: {wheel_position:?} / {physical_viewport:?}"
        );
        window.simulate_event(gpui::ScrollWheelEvent {
            position: wheel_position,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(40.0))),
            ..Default::default()
        });
        window.run_until_parked();
        let after_wheel =
            view.read_with(window, |app, _| app.shell.messaging_env_scroll.offset().y);
        assert!(
            after_wheel > before_wheel,
            "real wheel moves the focused env list"
        );
        window.run_until_parked();
        assert_eq!(
            view.read_with(window, |app, _| app.shell.messaging_env_scroll.offset().y),
            after_wheel,
            "focus reveal retains the user's manual wheel offset"
        );
    }
    drop(listener);
}
