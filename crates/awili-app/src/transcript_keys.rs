//! U3e2: transcript scroll keys (Page Up/Down always; Home/End and ⌘↑/⌘↓
//! once something other than the composer has focus) and the "Jump to latest"
//! pill shown when the reader has scrolled away from the bottom.

use gpui::{
    div, point, prelude::*, px, ClickEvent, Context, KeyDownEvent, Keystroke, ScrollHandle, Window,
};

use crate::work::Mode;
use crate::{focus_nav, theme, AwiliApp};

/// The pill's place in the Tab order: after the sidebar rows (`focus_nav::SIDEBAR`).
const JUMP_TAB_INDEX: isize = focus_nav::SIDEBAR + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollKey {
    PageUp,
    PageDown,
    Top,
    Bottom,
}

/// D11: Page Up/Down scroll the transcript even from the composer. Home/End and
/// ⌘↑/⌘↓ (Linux: Ctrl+Home/End) only while the transcript has focus, so the
/// composer keeps its line keys. Shift/⌥ combos are text selection. `function`
/// (set on every macOS arrow/Home/End/Page key) is ignored.
pub fn classify(ks: &Keystroke, transcript_focused: bool, mac: bool) -> Option<ScrollKey> {
    let m = &ks.modifiers;
    let (primary, other) = match mac {
        true => (m.platform, m.control),
        false => (m.control, m.platform),
    };
    if m.shift || m.alt || other {
        return None;
    }
    let key = match (ks.key.as_str(), primary) {
        ("pageup", false) => return Some(ScrollKey::PageUp),
        ("pagedown", false) => return Some(ScrollKey::PageDown),
        ("home", false) => ScrollKey::Top,
        ("end", false) => ScrollKey::Bottom,
        ("up", true) if mac => ScrollKey::Top,
        ("down", true) if mac => ScrollKey::Bottom,
        ("home", true) if !mac => ScrollKey::Top,
        ("end", true) if !mac => ScrollKey::Bottom,
        _ => return None,
    };
    transcript_focused.then_some(key)
}

/// The new offset (gpui's offset y runs from 0 down to `-max_y`); a page is 90%
/// of the viewport.
pub fn target_offset_y(key: ScrollKey, offset_y: f32, max_y: f32, viewport_h: f32) -> f32 {
    let page = 0.9 * viewport_h;
    match key {
        ScrollKey::PageDown => (offset_y - page).max(-max_y),
        ScrollKey::PageUp => (offset_y + page).min(0.),
        ScrollKey::Top => 0.,
        ScrollKey::Bottom => -max_y,
    }
}

/// "↓ Jump to latest", bottom-centre of the transcript while it is scrolled
/// away from the bottom (`follow_stream`'s threshold). A tab stop; a click, or
/// Space/Return when focused, returns to the bottom.
pub fn jump_pill(scroll: &ScrollHandle, cx: &mut Context<AwiliApp>) -> Option<impl IntoElement> {
    if crate::follow_stream(scroll) {
        return None;
    }
    let target = scroll.clone();
    let pill = div()
        .id("jump-to-latest")
        .debug_selector(|| "jump-to-latest".into())
        .tab_index(JUMP_TAB_INDEX)
        .focus_visible(focus_nav::ring)
        .px(theme::z(12.))
        .py(theme::z(4.))
        .rounded(theme::z(theme::RADIUS))
        .bg(theme::p().raised)
        .border_1()
        .border_color(theme::p().border)
        .text_size(theme::z(theme::LABEL_SIZE))
        .text_color(theme::p().text)
        .cursor_pointer()
        .child("↓ Jump to latest")
        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
            // The offset itself (not `scroll_to_bottom`, applied at the next
            // prepaint), so this very render already hides the pill.
            let bottom = point(target.offset().x, -target.max_offset().y);
            target.set_offset(bottom);
            cx.notify();
        }));
    let row = div().absolute().left_0().right_0().bottom(theme::z(12.));
    Some(row.flex().justify_center().child(pill))
}

/// RC1-F3: a transcript this close to its bottom (px) is AT the bottom.
const AT_BOTTOM_PX: f32 = 0.5;

/// RC1-F3: the transcript's tail state, one per conversation view. gpui
/// decides a render from the LAST layout, so (1) a row that grows after a
/// reply (the "[answered by …]" tag, the run footer, an activity line) was
/// never followed, and (2) the pill could be drawn from a layout that a
/// pending `scroll_to_bottom` was about to replace. Every render of a
/// transcript that sits exactly at its bottom therefore asks gpui to keep it
/// there (any growth laid out in that frame is followed), and the first
/// render away from the bottom asks for one more frame, so a stale pill is
/// re-checked against the new layout at once.
#[derive(Clone, Debug, Default)]
pub(crate) struct Tail {
    away: std::cell::Cell<u8>,
}

/// What one render of the transcript does (RC1-F3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TailStep {
    /// Keep the view at its bottom through this frame's layout.
    pub pin: bool,
    /// Draw one more frame (the pill is re-checked on the new layout).
    pub again: bool,
}

impl Tail {
    /// `laid_out`: the transcript has been through a layout. `gap`: the last
    /// layout's distance from the bottom (`max_offset.y + offset.y`, px).
    pub(crate) fn step(&self, laid_out: bool, gap: f32) -> TailStep {
        if !laid_out {
            return TailStep {
                pin: false,
                again: false,
            };
        }
        if gap <= AT_BOTTOM_PX {
            self.away.set(0);
            return TailStep {
                pin: true,
                again: false,
            };
        }
        let away = self.away.get().saturating_add(1);
        self.away.set(away);
        TailStep {
            pin: false,
            again: away == 1,
        }
    }
}

/// RC1-F3-FIX1: Awili just put a new message at the end (a send, a reply
/// row, a refusal): go to the bottom NOW. `scroll_to_bottom` alone only acts
/// at the next layout, so the render in between still saw a reader's old
/// offset and drew "Jump to latest" — and a reader who had scrolled up long
/// ago gets no re-check frame (`Tail` asks only once per trip away). Setting
/// the offset to the last known bottom makes that render see the bottom: no
/// pill, and `Tail::step` pins it, so the new layout is followed in the same
/// frame (the bottom pin for layout is `Tail`'s, one place for every path).
pub(crate) fn pin_to_bottom(scroll: &ScrollHandle) {
    let bottom = point(scroll.offset().x, -scroll.max_offset().y);
    scroll.set_offset(bottom);
}

impl AwiliApp {
    /// RC1-F3: once per render, before the transcript is built: apply the
    /// active Chat/Work transcript's [`Tail::step`].
    pub(crate) fn tail_frame(&self, window: &mut Window) {
        if self.mode != Mode::Work {
            return;
        }
        let Some(view) = self.active_view() else {
            return;
        };
        let s = &view.scroll;
        let laid_out = s.bounds().size.height > px(0.);
        let gap: f32 = (s.max_offset().y + s.offset().y).into();
        let step = view.tail.step(laid_out, gap);
        if step.pin {
            s.scroll_to_bottom();
        }
        if step.again {
            window.request_animation_frame();
        }
    }
}

impl AwiliApp {
    /// The root key hook after U3e1's: scroll the active Chat/Work transcript.
    /// `true` when the key was used.
    pub fn handle_transcript_keys(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.mode != Mode::Work || focus_nav::overlay_open(self, cx) {
            return false;
        }
        let Some(scroll) = self.active_view().map(|v| v.scroll.clone()) else {
            return false;
        };
        let editors = [&self.composer, &self.rename_editor];
        let in_text = editors
            .iter()
            .any(|e| e.read(cx).focus_handle().is_focused(window));
        let focused = window.focused(cx).is_some() && !in_text;
        let Some(key) = classify(&ev.keystroke, focused, cfg!(target_os = "macos")) else {
            return false;
        };
        let (offset, max) = (scroll.offset(), scroll.max_offset());
        let viewport = scroll.bounds().size.height;
        let y = target_offset_y(key, offset.y.into(), max.y.into(), viewport.into());
        scroll.set_offset(point(offset.x, px(y)));
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(s: &str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }

    #[test]
    fn u3e2_classify_page_keys_always_home_end_only_in_transcript() {
        for mac in [true, false] {
            for focused in [true, false] {
                assert_eq!(
                    classify(&ks("pageup"), focused, mac),
                    Some(ScrollKey::PageUp)
                );
                assert_eq!(
                    classify(&ks("pagedown"), focused, mac),
                    Some(ScrollKey::PageDown)
                );
                let home = classify(&ks("home"), focused, mac);
                assert_eq!(home, focused.then_some(ScrollKey::Top), "mac={mac}");
                let end = classify(&ks("end"), focused, mac);
                assert_eq!(end, focused.then_some(ScrollKey::Bottom), "mac={mac}");
            }
            assert_eq!(classify(&ks("shift-pagedown"), true, mac), None);
            assert_eq!(classify(&ks("alt-pageup"), true, mac), None);
            assert_eq!(classify(&ks("shift-home"), true, mac), None);
        }
        // macOS sets `function` on Page/Home/End/arrows: still classified.
        let mut fn_page = ks("pagedown");
        fn_page.modifiers.function = true;
        assert_eq!(classify(&fn_page, false, true), Some(ScrollKey::PageDown));
        // ⌘↑/⌘↓ on macOS, Ctrl+Home/End on Linux, only in the transcript.
        assert_eq!(classify(&ks("cmd-up"), true, true), Some(ScrollKey::Top));
        assert_eq!(
            classify(&ks("cmd-down"), true, true),
            Some(ScrollKey::Bottom)
        );
        assert_eq!(classify(&ks("cmd-down"), false, true), None);
        assert_eq!(
            classify(&ks("ctrl-home"), true, false),
            Some(ScrollKey::Top)
        );
        assert_eq!(
            classify(&ks("ctrl-end"), true, false),
            Some(ScrollKey::Bottom)
        );
        assert_eq!(
            classify(&ks("ctrl-end"), true, true),
            None,
            "⌃End is not ⌘↓ on a Mac"
        );
        assert_eq!(classify(&ks("cmd-up"), true, false), None);
        assert_eq!(classify(&ks("up"), true, true), None);
    }

    #[test]
    fn u3e2_target_offset_clamps() {
        let (max, h) = (1000., 400.);
        assert_eq!(target_offset_y(ScrollKey::PageDown, 0., max, h), -360.);
        assert_eq!(target_offset_y(ScrollKey::PageDown, -900., max, h), -1000.);
        assert_eq!(target_offset_y(ScrollKey::PageDown, -1000., max, h), -1000.);
        assert_eq!(target_offset_y(ScrollKey::PageUp, -100., max, h), 0.);
        assert_eq!(target_offset_y(ScrollKey::PageUp, -900., max, h), -540.);
        assert_eq!(target_offset_y(ScrollKey::Top, -500., max, h), 0.);
        assert_eq!(target_offset_y(ScrollKey::Bottom, 0., max, h), -1000.);
    }

    // ---- the real app: an isolated HOME, a window, a long Chat transcript ----

    use awili_core::Role;
    use gpui::{Entity, EntityInputHandler, Focusable, VisualTestContext};

    /// Run `test` in a subprocess with a private HOME (`AwiliApp::new` reads
    /// config and opens persistence; pattern of `s1a_proof_tests::isolated`).
    /// `true` inside the child; `false` in the parent once the child passed.
    fn isolated(test: &str) -> bool {
        const CHILD: &str = "AWILI_U3E2_TEST_HOME";
        if std::env::var_os(CHILD).is_some() {
            return true;
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "isolated test failed:\n{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains(test) && stdout.contains("1 passed"),
            "subprocess must actually execute the test: {stdout}"
        );
        false
    }

    /// Chat with 80 messages in the active conversation (scrolled to the top),
    /// past onboarding, in an active window, the composer focused.
    fn long_chat(cx: &mut gpui::TestAppContext) -> (Entity<AwiliApp>, &mut VisualTestContext) {
        let (app, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
        window.update(|window, _| window.activate_window());
        app.update(window, |a, cx| {
            a.onboarding = crate::onboarding::Onboarding::existing();
            let mut view = crate::conv_view::ConvView::new();
            for i in 0..80 {
                let role = if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                };
                view.convo
                    .push(role, format!("message {i}: one line of the transcript"));
                view.rows
                    .push(crate::MessageRow::new(view.convo.message(i).unwrap()));
            }
            a.views.insert(a.sidebar.active_id().to_string(), view);
            cx.notify();
        });
        window.run_until_parked();
        focus_composer(&app, window);
        (app, window)
    }

    fn focus_composer(app: &Entity<AwiliApp>, window: &mut VisualTestContext) {
        let composer = app.read_with(window, |a, cx| a.composer.focus_handle(cx));
        window.update(|window, cx| window.focus(&composer, cx));
        window.run_until_parked();
    }

    /// (offset y, max offset y) of the active transcript.
    fn scroll(app: &Entity<AwiliApp>, window: &mut VisualTestContext) -> (f32, f32) {
        app.read_with(window, |a, _| {
            let s = &a.active_view().unwrap().scroll;
            (s.offset().y.into(), s.max_offset().y.into())
        })
    }

    fn pill(window: &mut VisualTestContext) -> bool {
        window.debug_bounds("jump-to-latest").is_some()
    }

    #[gpui::test]
    fn u3e2_page_down_scrolls_active_transcript(cx: &mut gpui::TestAppContext) {
        if !isolated("transcript_keys::tests::u3e2_page_down_scrolls_active_transcript") {
            return;
        }
        let (app, window) = long_chat(cx);
        let (top, max) = scroll(&app, window);
        assert_eq!(top, 0.);
        assert!(max > 0., "the transcript overflows");
        window.simulate_keystrokes("pagedown");
        let (down, _) = scroll(&app, window);
        assert!(down < 0., "PageDown scrolls from the composer (D11)");
        window.simulate_keystrokes("pagedown");
        assert!(scroll(&app, window).0 < down, "and again");
        window.simulate_keystrokes("pageup pageup");
        assert_eq!(scroll(&app, window).0, 0.);
        assert_eq!(
            app.read_with(window, |a, cx| a.composer.read(cx).text().to_string()),
            ""
        );
    }

    #[gpui::test]
    fn u3e2_home_in_composer_stays_text_navigation(cx: &mut gpui::TestAppContext) {
        if !isolated("transcript_keys::tests::u3e2_home_in_composer_stays_text_navigation") {
            return;
        }
        let (app, window) = long_chat(cx);
        let composer = app.read_with(window, |a, _| a.composer.clone());
        composer.update(window, |c, cx| c.set_text("abc", cx));
        window.simulate_keystrokes("pagedown");
        let before = scroll(&app, window).0;
        window.simulate_keystrokes("home");
        assert_eq!(scroll(&app, window).0, before, "Home stays in the composer");
        composer.update_in(window, |c, window, cx| {
            c.replace_text_in_range(None, "X", window, cx)
        });
        assert_eq!(
            composer.read_with(window, |c, _| c.text().to_string()),
            "Xabc"
        );
    }

    #[gpui::test]
    fn u3e2_transcript_focus_home_end_and_line_chords(cx: &mut gpui::TestAppContext) {
        if !isolated("transcript_keys::tests::u3e2_transcript_focus_home_end_and_line_chords") {
            return;
        }
        let (app, window) = long_chat(cx);
        // Tab moves focus off the composer (onto Send): the transcript keys apply.
        window.simulate_keystrokes("tab");
        window.simulate_keystrokes("end");
        let (bottom, max) = scroll(&app, window);
        assert_eq!(bottom, -max);
        window.simulate_keystrokes("home");
        assert_eq!(scroll(&app, window).0, 0.);
        let (down, up) = match cfg!(target_os = "macos") {
            true => ("cmd-down", "cmd-up"),
            false => ("ctrl-end", "ctrl-home"),
        };
        window.simulate_keystrokes(down);
        assert_eq!(scroll(&app, window).0, -max);
        window.simulate_keystrokes(up);
        assert_eq!(scroll(&app, window).0, 0.);
    }

    #[gpui::test]
    fn u3e2_keys_inert_with_an_overlay(cx: &mut gpui::TestAppContext) {
        if !isolated("transcript_keys::tests::u3e2_keys_inert_with_an_overlay") {
            return;
        }
        let (app, window) = long_chat(cx);
        app.update(window, |a, cx| {
            a.shortcuts.open = true;
            cx.notify();
        });
        focus_composer(&app, window);
        window.simulate_keystrokes("pagedown");
        assert_eq!(scroll(&app, window).0, 0., "the sheet has the keyboard");
    }

    #[gpui::test]
    fn u3e2_jump_pill_visibility(cx: &mut gpui::TestAppContext) {
        if !isolated("transcript_keys::tests::u3e2_jump_pill_visibility") {
            return;
        }
        let (app, window) = long_chat(cx);
        assert!(
            pill(window),
            "scrolled away from the bottom: the pill shows"
        );
        // End (focus off the composer) puts the transcript at the bottom.
        window.simulate_keystrokes("tab end");
        window.run_until_parked();
        let (bottom, max) = scroll(&app, window);
        assert_eq!(bottom, -max);
        assert!(!pill(window), "at the bottom: no pill");
        // The wheel scrolls up; the pill comes back on that frame.
        let centre = window.update(|window, _| {
            let size = window.viewport_size();
            gpui::point(size.width / 2., size.height / 2.)
        });
        window.simulate_event(gpui::ScrollWheelEvent {
            position: centre,
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(400.))),
            modifiers: Default::default(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        assert!(scroll(&app, window).0 > -max, "the wheel scrolled up");
        assert!(pill(window), "the pill is back");
        // A click on the pill returns to the bottom and hides it.
        let at = window.debug_bounds("jump-to-latest").unwrap().center();
        window.simulate_click(at, Default::default());
        window.run_until_parked();
        assert_eq!(scroll(&app, window).0, -max);
        assert!(!pill(window));
    }

    #[gpui::test]
    fn u3e2_jump_pill_is_a_tab_stop(cx: &mut gpui::TestAppContext) {
        if !isolated("transcript_keys::tests::u3e2_jump_pill_is_a_tab_stop") {
            return;
        }
        let (app, window) = long_chat(cx);
        let stops = |window: &mut VisualTestContext| {
            focus_composer(&app, window);
            let composer = app.read_with(window, |a, cx| a.composer.focus_handle(cx));
            (1..256)
                .find(|_| {
                    window.simulate_keystrokes("tab");
                    window.update(|window, _| composer.is_focused(window))
                })
                .expect("Tab comes back to the composer")
        };
        let with_pill = stops(window);
        app.update(window, |a, _| {
            a.active_view().unwrap().scroll.scroll_to_bottom()
        });
        window.run_until_parked();
        assert_eq!(
            with_pill,
            stops(window) + 1,
            "the pill is one more tab stop"
        );
    }

    // ---- RC1-F3: the transcript stays at its bottom through late growth ----

    #[test]
    fn rc1f3_tail_step_pins_at_the_bottom_and_rechecks_once() {
        let pin = TailStep {
            pin: true,
            again: false,
        };
        let t = Tail::default();
        assert_eq!(
            t.step(false, 0.),
            TailStep {
                pin: false,
                again: false
            },
            "no layout yet: nothing to decide"
        );
        assert_eq!(t.step(true, 0.), pin, "at the bottom: stay there");
        assert_eq!(t.step(true, 0.4), pin, "sub-pixel rounding is the bottom");
        assert_eq!(
            t.step(true, 30.),
            TailStep {
                pin: false,
                again: true
            },
            "the first frame away asks for one more frame"
        );
        assert_eq!(
            t.step(true, 30.),
            TailStep {
                pin: false,
                again: false
            },
            "only once: no frame loop while the reader stays up"
        );
        assert!(!t.step(true, 900.).again);
        assert_eq!(t.step(true, 0.), pin, "back at the bottom");
        assert_eq!(
            t.step(true, 5.),
            TailStep {
                pin: false,
                again: true
            },
            "a new trip away asks again"
        );
    }

    /// A Workshop transcript at its bottom with an empty reply row streaming
    /// as row 80.
    fn chat_at_bottom(
        cx: &mut gpui::TestAppContext,
        mode: Mode,
    ) -> (Entity<AwiliApp>, &mut VisualTestContext, String) {
        let (app, window) = long_chat(cx);
        let conv = app.update(window, |a, cx| {
            a.mode = mode;
            let id = a.sidebar.active_id().to_string();
            let view = a.views.get_mut(&id).unwrap();
            view.convo.push(Role::Assistant, String::new());
            view.rows
                .push(crate::MessageRow::new(view.convo.message(80).unwrap()));
            view.scroll.scroll_to_bottom();
            a.work.running = true;
            a.work.conv_id = Some(id.clone());
            a.work.stream_row_idx = Some(80);
            cx.notify();
            id
        });
        window.run_until_parked();
        focus_composer(&app, window);
        let (o, m) = scroll(&app, window);
        assert_eq!(o, -m, "setup: at the bottom");
        (app, window, conv)
    }

    fn gap(app: &Entity<AwiliApp>, window: &mut VisualTestContext) -> f32 {
        let (o, m) = scroll(app, window);
        m + o
    }

    #[gpui::test]
    fn rc1f3_growth_after_a_reply_stays_at_the_bottom(cx: &mut gpui::TestAppContext) {
        if !isolated("transcript_keys::tests::rc1f3_growth_after_a_reply_stays_at_the_bottom") {
            return;
        }
        for mode in [Mode::Work] {
            let (app, window, conv) = chat_at_bottom(cx, mode);
            for i in 0..12 {
                let chunk = if i % 4 == 3 {
                    format!("word{i}\n\n")
                } else {
                    format!("word{i} ")
                };
                app.update(window, |a, cx| a.work_stream_token(&chunk, cx));
                window.run_until_parked();
                assert_eq!(gap(&app, window), 0., "{mode:?}: token {i} is followed");
            }
            let max_before = scroll(&app, window).1;
            app.update(window, |a, cx| a.work_on_activity("Ran a tool".into(), cx));
            window.run_until_parked();
            assert_eq!(
                gap(&app, window),
                0.,
                "{mode:?}: an activity line is followed"
            );
            app.update(window, |a, cx| {
                a.work_stream_token("\n\nA closing line after the tool ran.", cx)
            });
            window.run_until_parked();
            assert_eq!(
                gap(&app, window),
                0.,
                "{mode:?}: a closing line is followed"
            );
            app.update(window, |a, cx| {
                a.work.running = false;
                let view = a.views.get_mut(&conv).unwrap();
                let msg = view.convo.message(80).unwrap().clone();
                view.rows[80].finish_stream(&msg);
                cx.notify();
            });
            window.run_until_parked();
            let (o, m) = scroll(&app, window);
            assert!(
                m > max_before + 40.,
                "{mode:?}: the reply grew after streaming: {max_before} -> {m}"
            );
            assert_eq!(o, -m, "{mode:?}: the finished reply is followed");
            // Any later frame (a mouse move, a tick) still shows no pill.
            window.update(|window, _| window.refresh());
            window.run_until_parked();
            assert!(
                !pill(window),
                "{mode:?}: no pill at the bottom after the reply"
            );
        }
    }

    #[gpui::test]
    fn rc1f3_a_reader_who_scrolled_up_is_not_pulled_down(cx: &mut gpui::TestAppContext) {
        if !isolated("transcript_keys::tests::rc1f3_a_reader_who_scrolled_up_is_not_pulled_down") {
            return;
        }
        let (app, window, _conv) = chat_at_bottom(cx, Mode::Work);
        let centre = window.update(|window, _| {
            let size = window.viewport_size();
            gpui::point(size.width / 2., size.height / 2.)
        });
        for up in [60., 400.] {
            app.update(window, |a, _| {
                a.active_view().unwrap().scroll.scroll_to_bottom()
            });
            window.update(|window, _| window.refresh());
            window.run_until_parked();
            assert_eq!(gap(&app, window), 0.);
            window.simulate_event(gpui::ScrollWheelEvent {
                position: centre,
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(up))),
                modifiers: Default::default(),
                touch_phase: gpui::TouchPhase::Moved,
            });
            window.run_until_parked();
            let (o, _) = scroll(&app, window);
            assert_eq!(gap(&app, window), up, "the wheel moved {up}px up");
            // The reply grows below the reader. Streamed text is followed only
            // within 48px of the bottom, so a reader further up stays put.
            app.update(window, |a, cx| {
                a.work_stream_token(&format!("\n\nmore after {up}"), cx)
            });
            window.run_until_parked();
            assert_eq!(scroll(&app, window).0, o, "{up}px up: the reader stays put");
            assert!(gap(&app, window) > up, "the growth is below the reader");
        }
        assert!(pill(window), "400px up: the pill shows");
    }

    // ---- RC1-F3-FIX1: a new message after scrolling up hides the pill at once ----

    /// Codex T2 P2 (6cdf333): a reader scrolled up long ago (the one re-check
    /// frame already spent), then Awili puts a new message at the end
    /// (`work_push`).
    /// The very next frame is at the bottom with no pill; nothing else has
    /// to re-render.
    #[gpui::test]
    fn rc1f3_fix1_a_new_message_after_scrolling_up_hides_the_pill_at_once(
        cx: &mut gpui::TestAppContext,
    ) {
        if !isolated(
            "transcript_keys::tests::rc1f3_fix1_a_new_message_after_scrolling_up_hides_the_pill_at_once",
        ) {
            return;
        }
        for mode in [Mode::Work] {
            let (app, window, _conv) = chat_at_bottom(cx, mode);
            let centre = window.update(|window, _| {
                let size = window.viewport_size();
                gpui::point(size.width / 2., size.height / 2.)
            });
            window.simulate_event(gpui::ScrollWheelEvent {
                position: centre,
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(400.))),
                modifiers: Default::default(),
                touch_phase: gpui::TouchPhase::Moved,
            });
            window.run_until_parked();
            // Two more frames: the reader has been up for a while.
            for _ in 0..2 {
                window.update(|window, _| window.refresh());
                window.run_until_parked();
            }
            assert_eq!(gap(&app, window), 400., "{mode:?}: the reader is up");
            assert!(pill(window), "{mode:?}: the pill shows while up");
            // Awili adds a message: ONE frame later the view is at the
            // bottom and the pill is gone.
            app.update(window, |a, cx| {
                a.work_push(Role::User, "next task".into());
                cx.notify();
            });
            window.run_until_parked();
            assert_eq!(gap(&app, window), 0., "{mode:?}: at the bottom");
            assert!(!pill(window), "{mode:?}: no pill on the next frame");
        }
    }

    /// `pin_to_bottom` moves the visible offset to the last known bottom at
    /// once and still follows the next layout.
    #[gpui::test]
    fn rc1f3_fix1_pin_to_bottom_moves_now_and_follows_the_layout(cx: &mut gpui::TestAppContext) {
        if !isolated(
            "transcript_keys::tests::rc1f3_fix1_pin_to_bottom_moves_now_and_follows_the_layout",
        ) {
            return;
        }
        let (app, window) = long_chat(cx);
        let (top, max) = scroll(&app, window);
        assert_eq!(top, 0.);
        app.update(window, |a, _| {
            pin_to_bottom(&a.active_view().unwrap().scroll)
        });
        assert_eq!(scroll(&app, window).0, -max, "moved before any frame");
        app.update(window, |a, cx| {
            let id = a.sidebar.active_id().to_string();
            let view = a.views.get_mut(&id).unwrap();
            let i = view.rows.len();
            view.convo
                .push(Role::User, "one more\n\nline\n\nand more".to_string());
            view.rows
                .push(crate::MessageRow::new(view.convo.message(i).unwrap()));
            pin_to_bottom(&view.scroll);
            cx.notify();
        });
        window.run_until_parked();
        let (o, m) = scroll(&app, window);
        assert!(m > max, "the transcript grew");
        assert_eq!(o, -m, "and the view followed the new layout");
    }
}
