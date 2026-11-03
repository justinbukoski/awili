//! RC2-R4 (F0956): in the app, a spoken stop while Awili reads a reply in
//! conversation mode stops the reading and the reply, and Awili's own voice
//! coming back through the mic does not. The talk-over transcripts are given
//! straight to `on_barge_transcript`, as the listener's worker would; the
//! read-aloud is a queue on a silent backend that speaks until stopped.

use crate::AwiliApp;
use awili_voice::tts::{CancelToken, TtsBackend, TtsError};

const CHILD: &str = "AWILI_RC2R4_BARGE_HOME";

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

/// Speaks each sentence until it is cancelled (no audio, no clock).
struct Silent;
impl TtsBackend for Silent {
    fn speak(&self, _text: &str, cancel: &CancelToken) -> Result<(), TtsError> {
        while !cancel.is_cancelled() {
            std::thread::yield_now();
        }
        Ok(())
    }
    fn name(&self) -> &'static str {
        "silent"
    }
}

#[gpui::test]
fn rc2r4_a_spoken_stop_stops_the_reply_and_awilis_voice_does_not(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "voice::barge_app_tests::rc2r4_a_spoken_stop_stops_the_reply_and_awilis_voice_does_not",
    ) {
        return;
    }
    let (app, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    app.update(window, |a, _| {
        a.onboarding = crate::onboarding::Onboarding::existing();
        let conv = a.sidebar.active_id().to_string();
        a.views
            .entry(conv.clone())
            .or_insert_with(crate::conv_view::ConvView::new);
        // The reply being read aloud belongs to a live Workshop run.
        a.mode = crate::work::Mode::Work;
        a.work.running = true;
        a.work.conv_id = Some(conv.clone());
        a.work
            .cancel
            .store(false, std::sync::atomic::Ordering::SeqCst);
        // Conversation mode, reading a reply aloud (no mic is opened here).
        let (queue, _done) = awili_voice::SpeechQueue::start_with_backend(Box::new(Silent));
        a.voice.conversation_mode = true;
        a.voice.speech = Some(queue);
        a.voice.speaking = true;
        a.voice.reply_streaming = true;
        a.voice.reply_conv = Some(conv);
        a.voice.spoken = vec![
            "Stop the service first.".into(),
            "Then drain the node.".into(),
        ];
        a.voice.barge_gen = 7;
    });
    let speaking =
        |window: &mut gpui::VisualTestContext| app.read_with(window, |a, _| a.voice.speaking);

    // Awili's own sentences, heard back through the mic: nothing stops.
    for echo in ["Stop.", "stop the service first", "Then drain the node."] {
        app.update(window, |a, cx| a.on_barge_transcript(echo, 7, cx));
        assert!(speaking(window), "{echo:?} stopped the reply");
    }
    // A late transcript from an earlier reply is dropped.
    app.update(window, |a, cx| a.on_barge_transcript("stop talking", 6, cx));
    assert!(speaking(window), "a stale talk-over stopped the reply");

    // The owner says stop: the reading and the reply stop; voice mode stays.
    app.update(window, |a, cx| {
        a.on_barge_transcript("Stop talking.", 7, cx)
    });
    app.read_with(window, |a, _| {
        assert!(
            !a.voice.speaking && a.voice.speech.is_none(),
            "still reading"
        );
        assert!(a.voice.spoken.is_empty());
        assert!(a.voice.conversation_mode, "voice mode stays on");
        assert!(
            a.work.cancel.load(std::sync::atomic::Ordering::SeqCst),
            "the reply's run was told to stop"
        );
    });
}

/// Drive the window until `done` holds. The read-aloud and the talk-over
/// listener run on their own threads, so this waits for them, not for a
/// clock; 60 s is only the bound after which `what` fails the test.
fn wait_until(
    app: &gpui::Entity<AwiliApp>,
    window: &mut gpui::VisualTestContext,
    what: &str,
    done: impl Fn(&AwiliApp) -> bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        window.run_until_parked();
        if app.read_with(window, |a, _| done(a)) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{what} within 60 s");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// RC2-R4M2 (slice review P2): a stored reply's speaker button pressed while
/// the conversation mic listens hands that mic off to the talk-over
/// listener, so a spoken stop said over the reading — heard by the listener
/// itself, not handed to `on_barge_transcript` — stops it; the conversation
/// mic is armed again afterwards.
#[gpui::test]
fn rc2r4m2_the_speaker_button_while_listening_starts_the_talk_over_listener(
    cx: &mut gpui::TestAppContext,
) {
    if !in_isolated_child(
        "voice::barge_app_tests::rc2r4m2_the_speaker_button_while_listening_starts_the_talk_over_listener",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let (app, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    let mics = crate::voice::TestMics::default();
    let opened = |m: &crate::voice::TestMics| m.opened.load(std::sync::atomic::Ordering::SeqCst);
    app.update(window, |a, cx| {
        a.onboarding = crate::onboarding::Onboarding::existing();
        a.voice.test_mics = Some(mics.clone());
        a.voice.test_tts = Some(|| Box::new(Silent));
        let conv = a.sidebar.active_id().to_string();
        let view = a
            .views
            .entry(conv)
            .or_insert_with(crate::conv_view::ConvView::new);
        view.convo.push(
            crate::Role::Assistant,
            "Drain the node first. Then reboot it.",
        );
        // Conversation mode, listening.
        a.voice.conversation_mode = true;
        a.start_voice_listening(cx);
    });
    app.read_with(window, |a, _| {
        assert!(a.voice.conv_mic.is_some(), "the conversation mic is armed");
        assert!(a.voice.barge.is_none());
    });
    assert_eq!(opened(&mics), 1);

    // The stored reply's speaker button.
    app.update(window, |a, cx| a.speak_message(0, cx));
    app.read_with(window, |a, _| {
        assert!(a.voice.speaking, "the reply is read aloud");
        assert!(
            a.voice.conv_mic.is_none(),
            "the conversation mic was handed off"
        );
        assert!(
            a.voice.barge.is_some(),
            "no talk-over listener during playback"
        );
    });
    assert_eq!(opened(&mics), 2);

    // The owner says stop over the reading; the listener hears it.
    mics.said.lock().unwrap().push("Stop talking.".into());
    wait_until(&app, window, "the spoken stop ends the reading", |a| {
        !a.voice.speaking && a.voice.speech.is_none()
    });
    wait_until(&app, window, "the conversation mic is armed again", |a| {
        a.voice.conv_mic.is_some() && a.voice.barge.is_none()
    });
    app.read_with(window, |a, _| assert!(a.voice.conversation_mode));
    assert_eq!(opened(&mics), 3);
}
