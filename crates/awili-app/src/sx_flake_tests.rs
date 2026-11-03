//! SX-FLAKE proof: the launch-time skills fetch never runs on a real thread
//! in a test build, so no GPUI test can be woken from outside its scheduler.
use crate::AwiliApp;
use gpui::TestAppContext;

const CHILD: &str = "AWILI_SXFLAKE_TEST_HOME";

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

/// The first render's skills fetch (main.rs, "Eagerly load skills on
/// launch") settles on the test scheduler itself: by the time the window is
/// parked it has already finished, with the test build's fixed error.
#[gpui::test]
fn sx_flake_launch_skills_fetch_settles_on_the_scheduler(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "work::sx_flake_tests::sx_flake_launch_skills_fetch_settles_on_the_scheduler",
    ) {
        return;
    }
    // Make sure the fetch really starts: a configured dashboard gateway.
    let path = awili_config::Config::config_paths()[0].clone();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "[gateway]\ndashboard_url = \"http://127.0.0.1:9\"\n").unwrap();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        cx.notify();
    });
    window.run_until_parked();
    let (gateway, loading, error) = view.update(window, |app, _| {
        (
            app.work.gateway.enabled,
            app.skills_state.loading,
            app.skills_state.error.clone(),
        )
    });
    assert!(gateway, "the fetch had a gateway to go to");
    assert!(!loading, "the fetch already finished, on the scheduler");
    assert_eq!(
        error.as_deref(),
        Some(crate::work::SKILLS_NOT_FETCHED_IN_TESTS),
        "and it never reached the network"
    );
}
