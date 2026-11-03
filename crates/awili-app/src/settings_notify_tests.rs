//! SXg proof tests: the Notifications page (N1's `[notifications]` as live
//! catalog rows, macOS's answer, a test button and permission guidance).
use super::*;
use crate::notify_platform::Recorded;
use crate::settings_catalog as cat;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

const CHILD: &str = "AWILI_SXG_TEST_HOME";

/// Run `test` in a subprocess with a private HOME. `true` inside the child.
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
        out.status.success(),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(test) && stdout.contains("1 passed"),
        "the subprocess must actually run the test: {stdout}"
    );
    false
}

fn settings_window(cx: &mut TestAppContext) -> (Entity<AwiliApp>, &mut VisualTestContext) {
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.open_settings(cx);
    });
    window.run_until_parked();
    (view, window)
}

fn sel(selector: &str) -> &'static str {
    Box::leak(selector.to_string().into_boxed_str())
}

fn click(window: &mut VisualTestContext, selector: &str) {
    let b = window
        .debug_bounds(sel(selector))
        .unwrap_or_else(|| panic!("{selector} is on screen"));
    window.simulate_click(b.center(), Modifiers::default());
    window.run_until_parked();
}

fn shown(window: &mut VisualTestContext, selector: &str) -> bool {
    window.debug_bounds(sel(selector)).is_some()
}

fn ix(id: &str) -> usize {
    cat::ITEMS.iter().position(|i| i.id == id).unwrap()
}

fn config_text(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> String {
    view.update(window, |app, _| {
        std::fs::read_to_string(app.settings.path()).unwrap_or_default()
    })
}

/// The Notifications page in the sidebar.
const NOTIFICATIONS: &str = "sx-nav-6";

#[gpui::test]
fn sxg_master_off_dims_but_keeps_the_rest(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::notify_tests::sxg_master_off_dims_but_keeps_the_rest") {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, NOTIFICATIONS);
    assert!(
        !shown(window, "sx-dim-notifications.approvals"),
        "nothing is dimmed while Notifications is on"
    );
    click(window, &format!("sx-off-{}", ix("notifications.enabled")));
    assert!(config_text(&view, window).contains("enabled = false"));
    assert!(
        shown(window, "sx-row-notifications.approvals"),
        "the rows stay"
    );
    assert!(
        shown(window, "sx-dim-notifications.approvals"),
        "and say why they do not matter now"
    );
    click(window, &format!("sx-off-{}", ix("notifications.approvals")));
    assert!(
        config_text(&view, window).contains("approvals = false"),
        "a dimmed row still works"
    );
    click(window, &format!("sx-on-{}", ix("notifications.enabled")));
    assert!(!shown(window, "sx-dim-notifications.approvals"));
}

#[gpui::test]
fn sxg_send_test_uses_the_recording_backend(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::notify_tests::sxg_send_test_uses_the_recording_backend")
    {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, NOTIFICATIONS);
    assert!(shown(window, "sx-notify-status"));
    assert!(
        shown(window, "sx-notify-guide"),
        "before macOS has asked, the page says what to click"
    );
    assert!(
        !shown(window, "sx-notify-sys-0"),
        "no System Settings button before a refusal"
    );
    click(window, "sx-notify-test-0");
    let log = view.update(window, |app, _| app.notifier.test_log());
    assert_eq!(
        log,
        [Recorded::RequestPermission],
        "the test asks macOS through the recording backend"
    );
    view.update(window, |app, cx| {
        app.on_notify_signal(
            crate::notify_platform::BackendSignal::Permission(notifications::Permission::Denied),
            cx,
        );
        cx.notify();
    });
    window.run_until_parked();
    assert!(shown(window, "sx-notify-guide"), "a refusal is explained");
    click(window, "sx-notify-sys-0");
    let log = view.update(window, |app, _| app.notifier.test_log());
    assert_eq!(log.last(), Some(&Recorded::OpenSettings));
}

#[gpui::test]
fn sxg_form_has_no_notify_pills_any_more(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::notify_tests::sxg_form_has_no_notify_pills_any_more") {
        return;
    }
    let (_view, window) = settings_window(cx);
    assert!(
        shown(window, "sx-row-notifications.enabled"),
        "General lists Notifications on/off"
    );
    click(window, "sx-nav-3");
    assert!(!shown(window, "notify-toggle-0"), "not on Models & Fleet");
    click(window, NOTIFICATIONS);
    assert!(!shown(window, "notify-toggle-0"), "and not as old pills");
    assert!(!shown(window, "draft-save-0"), "no draft Save on the page");
}
