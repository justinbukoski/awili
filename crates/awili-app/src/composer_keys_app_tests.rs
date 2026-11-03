//! RC2-R4 (D-U3, the U3 "Post-RC1" macOS list): in the real app window, a ⌃
//! key never runs an app shortcut on macOS (⌃K, ⌃N, ⌃F… belong to the text
//! field), and an Appearance choice applies through the persisted setting.

use crate::command_palette::{ColorModeChoice, PaletteAction};

const CHILD: &str = "AWILI_RC2R4_KEYS_HOME";

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
        out.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    false
}

#[gpui::test]
fn rc2r4_control_keys_run_no_app_shortcut_on_macos(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "composer_keys::app_tests::rc2r4_control_keys_run_no_app_shortcut_on_macos",
    ) {
        return;
    }
    cx.update(|cx| cx.bind_keys(crate::app_lifecycle::standard_bindings()));
    let (app, window) = cx.add_window_view(|_, cx| crate::AwiliApp::new(cx));
    window.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(1000.)));
    app.update(window, |a, cx| {
        a.onboarding = crate::onboarding::Onboarding::existing();
        a.set_mode(crate::work::Mode::Work, cx);
    });
    window.run_until_parked();
    let open = |window: &mut gpui::VisualTestContext| app.read_with(window, |a, _| a.palette.open);

    window.simulate_keystrokes("ctrl-k");
    window.run_until_parked();
    assert_eq!(
        open(window),
        !cfg!(target_os = "macos"),
        "⌃K opens the palette only where Ctrl is the chord key"
    );
    if open(window) {
        window.simulate_keystrokes("escape");
        window.run_until_parked();
    }
    assert!(!open(window));
    window.simulate_keystrokes("cmd-k");
    window.run_until_parked();
    assert!(open(window), "⌘K opens the palette");

    // An Appearance choice applies and persists without opening Settings.
    app.update_in(window, |a, w, cx| {
        a.run_command_palette_action(PaletteAction::SetColorMode(ColorModeChoice::Light), w, cx);
    });
    window.run_until_parked();
    app.read_with(window, |a, _| {
        assert!(crate::theme::is_light_mode());
        assert_eq!(a.appearance.preference, awili_config::ColorMode::Light);
        assert_eq!(
            awili_config::Config::load().unwrap().appearance.color_mode,
            awili_config::ColorMode::Light
        );
        assert!(!a.settings.open);
        assert!(!a.command_palette.open);
    });
}

/// RC2-R4 (D-U3, U3 list "image paste from the menu"): with only an image on
/// the clipboard, Edit ▸ Paste in the message box attaches it, as ⌘V does
/// (it pasted nothing before), and types no text.
#[gpui::test]
fn rc2r4_edit_paste_attaches_an_image(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("composer_keys::app_tests::rc2r4_edit_paste_attaches_an_image") {
        return;
    }
    let store = awili_persist::Store::open_default().unwrap();
    cx.executor().allow_parking();
    cx.update(|cx| cx.bind_keys(crate::app_lifecycle::standard_bindings()));
    let (app, window) = cx.add_window_view(|_, cx| crate::AwiliApp::new(cx));
    app.update_in(window, |a, w, cx| {
        a.onboarding = crate::onboarding::Onboarding::existing();
        a.set_mode(crate::work::Mode::Work, cx);
        a.new_conversation(w, cx);
        let id = a.sidebar.active_id().to_string();
        // Staging needs the conversation's database row (foreign key).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !store.conversations().unwrap().iter().any(|c| c.id == id) {
            assert!(std::time::Instant::now() < deadline, "the row is saved");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let focus = a.composer.read(cx).focus_handle().clone();
        focus.focus(w, cx);
    });
    window.run_until_parked();
    let png = gpui::Image::from_bytes(gpui::ImageFormat::Png, b"\x89PNG\r\n\x1a\nrc2r4".to_vec());
    app.update_in(window, |_, _, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_image(&png))
    });
    window.dispatch_action(crate::native_menu::Paste);
    window.run_until_parked();
    app.read_with(window, |a, cx| {
        let names: Vec<_> = a
            .staged_attachments
            .iter()
            .map(|s| s.filename.clone())
            .collect();
        assert_eq!(
            names,
            ["pasted-image.png"],
            "Edit ▸ Paste attached the image"
        );
        assert_eq!(a.composer.read(cx).text(), "", "no text was pasted");
    });
}
