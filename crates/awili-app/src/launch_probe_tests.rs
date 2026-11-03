//! RC1-G1 proof: the launch probes (provider health, the gateway probe and the
//! gateway index fetch) never run on a real thread in a test build, so no GPUI
//! test can be woken from outside its scheduler ("Detected activity on thread
//! … Your test is not deterministic", RC-16).
use crate::AwiliApp;
use gpui::TestAppContext;

const CHILD: &str = "AWILI_RC1G1_TEST_HOME";

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

/// The first render's launch block (main.rs, "C1: providers get the same
/// launch probe the gateway gets") with a configured provider and a configured
/// dashboard gateway: by the time the window is parked every probe has
/// settled on the test scheduler itself — no provider batch is still in
/// flight and no health was invented, the gateway probe is done, and the
/// launch connection check reports the test build's fixed "not probed".
#[gpui::test]
fn rc1g1_launch_probes_settle_on_the_scheduler(cx: &mut TestAppContext) {
    if !in_isolated_child("launch_probe_tests::rc1g1_launch_probes_settle_on_the_scheduler") {
        return;
    }
    // Make sure every launch probe really starts: one enabled provider and a
    // configured dashboard gateway, both at a server that accepts and never
    // answers — a probe on a real thread would still be waiting when the
    // window parks, so only a result settled on the scheduler passes.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", silent.local_addr().unwrap());
    let path = awili_config::Config::config_paths()[0].clone();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(
            "[gateway]\ndashboard_url = \"{url}\"\n\n\
             [[providers]]\nname = \"local\"\nkind = \"ollama\"\n\
             base_url = \"{url}\"\nmodels = [\"m\"]\n"
        ),
    )
    .unwrap();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        cx.notify();
    });
    window.run_until_parked();
    view.update(window, |app, _| {
        assert_eq!(app.provider_endpoints.len(), 1, "setup: one provider");
        assert!(app.work.gateway.enabled, "setup: a dashboard gateway");
        assert_ne!(
            app.provider_probe_generation, 0,
            "the launch provider probe ran"
        );
        assert!(
            !app.provider_probe_in_flight,
            "the provider batch already settled, on the scheduler"
        );
        assert!(
            app.provider_health.is_empty(),
            "no health is invented: {:?}",
            app.provider_health
        );
        assert!(
            !app.work.health.gateway.probing(),
            "the gateway probe already settled, on the scheduler"
        );
        assert!(
            !app.work.hermes_testing,
            "the launch connection check ended"
        );
        assert_eq!(
            app.work.hermes_status,
            Some(Err(
                crate::gateway_health_app::GATEWAY_NOT_PROBED_IN_TESTS.to_string()
            )),
            "and it never reached the network"
        );
    });
    drop(silent);
}
