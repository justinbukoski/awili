//! R2d proof test: the "no local model" card a household member sees after a
//! real composer send speaks plainly (RC1 round 2, Major 4: "set base_url and
//! model under [engine] in config.toml (Settings → All fields)" and "your
//! message is kept here"). The real `AwiliApp` is painted in a GPUI test
//! window (private HOME subprocess).

use std::path::PathBuf;

use crate::local_model_health::{down_body, DownReason};
use crate::settings_catalog::Page;
use crate::work::{Mode, WorkExecutor};
use crate::AwiliApp;

const CHILD: &str = "AWILI_R2D_TEST_HOME";
const CONV: &str = "r2d-work-conv";

/// Run `test` in a subprocess with a private HOME. `Some(home)` inside the
/// child; `None` in the parent once the child passed.
fn isolated(test: &str) -> Option<PathBuf> {
    if let Some(home) = std::env::var_os(CHILD) {
        return Some(home.into());
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
    None
}

/// Enter in Work on "this Mac" with no local model set up: the card is
/// painted, its words carry no config jargon, name a Settings page that
/// exists, and say where the message is and how to get it back.
#[gpui::test]
fn r2d_no_local_model_card_speaks_plainly(cx: &mut gpui::TestAppContext) {
    if isolated("local_model_app::r2d_tests::r2d_no_local_model_card_speaks_plainly").is_none() {
        return;
    }
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = None;
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.work.executor = WorkExecutor::InApp;
        app.composer
            .update(cx, |c, cx| c.set_text("Please summarise my notes.", cx));
        app.submit_composer_draft(cx);
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window
            .debug_bounds(crate::local_model_app::LOCAL_DOWN_CARD_SELECTOR)
            .is_some(),
        "the card is painted"
    );
    // The card's body is exactly `down_body` over the live state (the render
    // calls it with these three values).
    let body = view.update(window, |app, _| {
        let down = app.work.local.down.clone().expect("a card");
        assert_eq!(down.reason, DownReason::NotConfigured);
        assert_eq!(down.text.as_deref(), Some("Please summarise my notes."));
        down_body(&down.reason, &app.work.engine, down.text.is_some())
    });
    for jargon in [
        "base_url",
        "[engine]",
        "config.toml",
        "All fields",
        "In-app",
    ] {
        assert!(!body.contains(jargon), "{jargon} in {body}");
    }
    assert!(body.contains("this Mac"), "{body}");
    assert!(body.contains("Nothing was sent"), "{body}");
    assert!(
        body.contains("saved on this card") && body.contains("Edit message"),
        "where the message is and how to get it back: {body}"
    );
    let page = format!("Settings \u{2192} {}", Page::Advanced.title());
    assert!(body.contains(&page), "names a page that exists: {body}");
}
