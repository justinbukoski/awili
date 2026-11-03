//! Rendered regressions for the Backstage Automations (Cron) page: the actions
//! each job state is offered, row and action clicks, the loading and empty
//! placeholders, notice precedence, and a populated list at a narrow zoom.
//! Opening the page starts no read; the listing is seeded directly.
use crate::hermes_pages::{notice_selector, HermesPage};
use crate::{work::Mode, AwiliApp};
use awili_gateway::rest::ScheduledJob;
use gpui::{px, size, Bounds, Entity, Pixels, VisualTestContext};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn job(id: &str, name: &str, state: &str, enabled: bool) -> ScheduledJob {
    ScheduledJob {
        id: id.to_string(),
        name: name.to_string(),
        prompt: "test".to_string(),
        skills: Vec::new(),
        skill: None,
        model: None,
        schedule: None,
        schedule_display: Some("every day at 09:00".to_string()),
        enabled,
        state: state.to_string(),
        paused_at: None,
        paused_reason: None,
        created_at: None,
        next_run_at: None,
        last_run_at: None,
        last_status: None,
        last_error: None,
        enabled_toolsets: Vec::new(),
        workdir: None,
    }
}

/// One job for each state the action rules distinguish.
fn fixture() -> Vec<ScheduledJob> {
    vec![
        job("active", "Daily digest", "scheduled", true),
        job("native-paused", "Weekly report", "paused", false),
        job("errored", "Backup sync", "error", true),
        job("unknown", "Mystery run", "queued", false),
    ]
}

/// An address with nothing listening, so a Hermes read fails plainly.
fn unreachable_hermes() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

fn painted(window: &mut VisualTestContext, selector: &str) -> Option<Bounds<Pixels>> {
    window.debug_bounds(Box::leak(selector.to_string().into_boxed_str()))
}

fn selected_id(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> Option<String> {
    view.read_with(window, |app, _| app.cron_state.selected_id.clone())
}

fn click_center(window: &mut VisualTestContext, face: Bounds<Pixels>) {
    window.simulate_click(face.center(), gpui::Modifiers::default());
    window.run_until_parked();
}

fn wheel(window: &mut VisualTestContext, at: gpui::Point<Pixels>, delta_y: f32) {
    window.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(delta_y))),
        touch_phase: gpui::TouchPhase::Moved,
        ..Default::default()
    });
    window.run_until_parked();
}

fn contains(viewport: Bounds<Pixels>, face: Bounds<Pixels>) -> bool {
    let tolerance = px(0.1);
    face.left() >= viewport.left() - tolerance
        && face.right() <= viewport.right() + tolerance
        && face.top() >= viewport.top() - tolerance
        && face.bottom() <= viewport.bottom() + tolerance
}

fn assert_contained(viewport: Bounds<Pixels>, face: Bounds<Pixels>, label: &str) {
    assert!(
        contains(viewport, face),
        "the complete {label} face is inside its viewport: {face:?} / {viewport:?}"
    );
}

/// The live window the fixture is laid out in, read from the test window.
fn window_viewport(window: &mut VisualTestContext) -> Bounds<Pixels> {
    let window_size = window.update(|window, _| window.viewport_size());
    Bounds::new(gpui::point(px(0.), px(0.)), window_size)
}

/// Overlap of two rectangles; empty when they do not meet.
fn intersect(a: Bounds<Pixels>, b: Bounds<Pixels>) -> Bounds<Pixels> {
    let left = a.left().max(b.left());
    let top = a.top().max(b.top());
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    Bounds::new(
        gpui::point(left, top),
        size((right - left).max(px(0.)), (bottom - top).max(px(0.))),
    )
}

/// The smallest rectangle holding both faces.
fn union(a: Bounds<Pixels>, b: Bounds<Pixels>) -> Bounds<Pixels> {
    let left = a.left().min(b.left());
    let top = a.top().min(b.top());
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Bounds::new(gpui::point(left, top), size(right - left, bottom - top))
}

/// One reading of the geometry the wheel checks depend on. `visible` is the
/// part of the Cron list a pointer can reach: inside the list, inside the
/// Backstage body, and inside the window.
#[derive(Clone, Copy, Debug)]
struct Frame {
    list: Bounds<Pixels>,
    body: Bounds<Pixels>,
    section: Bounds<Pixels>,
    viewport: Bounds<Pixels>,
    visible: Bounds<Pixels>,
    row: Bounds<Pixels>,
    pause: Bounds<Pixels>,
    trigger: Bounds<Pixels>,
    outer: Pixels,
    outer_max: Pixels,
    inner: Pixels,
    inner_max: Pixels,
}

impl Frame {
    fn read(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> Self {
        let (outer, outer_max, inner, inner_max) = view.read_with(window, |app, _| {
            (
                app.shell.backstage_scroll.offset().y,
                app.shell.backstage_scroll.max_offset().y,
                app.shell.cron_scroll.offset().y,
                app.shell.cron_scroll.max_offset().y,
            )
        });
        let viewport = window_viewport(window);
        let list = painted(window, "awili-cron-list").expect("the Cron list");
        let body = painted(window, "awili-backstage-body").expect("the Backstage body");
        Self {
            list,
            body,
            section: painted(window, "awili-backstage-section").expect("the Backstage section"),
            viewport,
            visible: intersect(intersect(list, body), viewport),
            row: painted(window, "ui-row-cron-job-t0").expect("the first job row"),
            pause: painted(window, "ui-button-cron-pause-t0").expect("the first job's Pause"),
            trigger: painted(window, "ui-button-cron-trigger-t0").expect("the first job's Run now"),
            outer,
            outer_max,
            inner,
            inner_max,
        }
    }
}

/// A point in the visible Backstage body above the list. A wheel there cannot
/// reach the list, so it scrolls the outer section. `None` when no such point
/// exists, which is the case once the list reaches the top of the body.
fn outer_probe(frame: &Frame) -> Option<gpui::Point<Pixels>> {
    let region = intersect(frame.body, frame.viewport);
    let bottom = frame.list.top().min(region.bottom());
    if bottom - region.top() < px(2.) {
        return None;
    }
    Some(gpui::point(region.center().x, (region.top() + bottom) / 2.))
}

/// A real wheel at the middle of the visible list. While the list can still
/// scroll, the list viewport, the Backstage body, and the outer section must
/// all stay where they were.
fn wheel_list(
    view: &Entity<AwiliApp>,
    window: &mut VisualTestContext,
    before: &Frame,
    delta_y: f32,
) -> Frame {
    wheel(window, before.visible.center(), delta_y);
    let after = Frame::read(view, window);
    assert_eq!(
        after.list, before.list,
        "the list viewport stays put while its rows scroll: {before:?} -> {after:?}"
    );
    assert_eq!(
        after.body, before.body,
        "the Backstage body stays put while the list scrolls: {before:?} -> {after:?}"
    );
    assert_eq!(
        after.section, before.section,
        "the outer section does not scroll while the list can move: {before:?} -> {after:?}"
    );
    after
}

fn open_cron_page(
    view: &Entity<AwiliApp>,
    window: &mut VisualTestContext,
    jobs: Vec<ScheduledJob>,
    loading: bool,
    error: Option<&str>,
) {
    let url = unreachable_hermes();
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.work.hermes.enabled = true;
        app.work.hermes.base_url = url.clone();
        app.work.hermes.dashboard_url = url.clone();
        app.work.gateway.enabled = false;
        // A read already in flight, so opening the page starts none.
        app.cron_state.loading = true;
        app.set_mode(Mode::Cron, cx);
        app.cron_state.loading = loading;
        app.cron_state.error = error.map(str::to_string);
        app.cron_state.set_jobs(jobs);
        cx.notify();
    });
    window.run_until_parked();
}

/// Longest a capture fixture stays up, so no listener outlives its test.
const CAPTURE_LIFETIME: Duration = Duration::from_secs(30);
/// Longest a dispatch may take to reach the fixture after its input.
const DISPATCH_DEADLINE: Duration = Duration::from_secs(5);
/// Poll interval while a dispatch is awaited.
const CAPTURE_POLL: Duration = Duration::from_millis(5);
/// The list read the action callback issues once its POST has succeeded.
const LIST_REQUEST: &str = "GET /api/cron/jobs";

/// A loopback stand-in for the Hermes dashboard. It answers each request it
/// receives and records only the method and path, never headers or a body, so
/// an action's real request can be read off the wire. The index carries no
/// session token, so the client falls back to the configured (empty) API key.
struct CaptureFixture {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl CaptureFixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, halt) = (seen.clone(), stop.clone());
        std::thread::spawn(move || {
            let deadline = Instant::now() + CAPTURE_LIFETIME;
            while !halt.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((stream, _)) => answer(stream, &log),
                    Err(_) => std::thread::sleep(CAPTURE_POLL),
                }
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}"),
            seen,
            stop,
        }
    }

    /// Every request so far, as "METHOD /path".
    fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    /// The action POSTs so far, each as "POST /api/cron/jobs/{id}/{action}".
    fn posts(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|r| r.starts_with("POST "))
            .collect()
    }
}

impl Drop for CaptureFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Reads one request head, records its method and path, and answers it.
fn answer(mut stream: TcpStream, seen: &Mutex<Vec<String>>) {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => break,
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut parts = text.lines().next().unwrap_or_default().split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return;
    };
    let path = target.split('?').next().unwrap_or(target);
    seen.lock().unwrap().push(format!("{method} {path}"));
    let (status, kind, body) = match (method, path) {
        ("GET", "/") => ("200 OK", "text/html", "<html><body>capture</body></html>"),
        ("GET", "/api/cron/jobs") => ("200 OK", "application/json", "[]"),
        ("POST", p) if p.starts_with("/api/cron/jobs/") => ("200 OK", "application/json", "{}"),
        _ => ("404 Not Found", "application/json", "{}"),
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.shutdown(Shutdown::Both);
}

/// Points the Hermes and dashboard origins at the fixture with no API key, so
/// the action's own request is the one the fixture receives.
fn point_at(view: &Entity<AwiliApp>, window: &mut VisualTestContext, capture: &CaptureFixture) {
    view.update(window, |app, cx| {
        app.work.hermes.base_url = capture.url.clone();
        app.work.hermes.dashboard_url = capture.url.clone();
        app.work.hermes.api_key = String::new();
        cx.notify();
    });
    window.run_until_parked();
}

/// Pumps the window until the action's own list refresh reaches the fixture.
/// That refresh only follows a successful action callback, so its arrival shows
/// the dispatch completed. Bounded by [`DISPATCH_DEADLINE`].
fn await_refresh(window: &mut VisualTestContext, capture: &CaptureFixture) {
    let deadline = Instant::now() + DISPATCH_DEADLINE;
    loop {
        window.run_until_parked();
        if capture.requests().iter().any(|r| r == LIST_REQUEST) || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(CAPTURE_POLL);
    }
}

/// Asserts that exactly one action request reached the fixture, and that it is
/// `route`. A missing or no-op callback never sends it, so it fails here.
fn assert_dispatched(window: &mut VisualTestContext, capture: &CaptureFixture, route: &str) {
    await_refresh(window, capture);
    assert_eq!(
        capture.posts(),
        vec![format!("POST {route}")],
        "the action dispatches exactly one request to {route}; requests: {:?}",
        capture.requests()
    );
}

/// Asserts that no action request has reached the fixture yet.
fn assert_no_dispatch(capture: &CaptureFixture, label: &str) {
    assert!(
        capture.posts().is_empty(),
        "{label} dispatches no action: {:?}",
        capture.posts()
    );
}

#[gpui::test]
fn cron_panel_pins_action_availability_per_state(cx: &mut gpui::TestAppContext) {
    const TEST: &str = "cron_panel_tests::cron_panel_pins_action_availability_per_state";
    if !crate::consolidation_tests::isolated(TEST) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    open_cron_page(&view, window, fixture(), false, None);
    // Pause: an enabled job that is not paused. Resume: every paused job,
    // including a native paused one (enabled = false). Run now: every job.
    for (id, pause, resume) in [
        ("active", true, false),
        ("native-paused", false, true),
        ("errored", true, false),
        ("unknown", false, false),
    ] {
        assert!(
            painted(window, &format!("ui-row-cron-job-{id}")).is_some(),
            "{id}: its row"
        );
        assert!(
            painted(window, &format!("ui-chip-cron-state-{id}")).is_some(),
            "{id}: its state chip"
        );
        assert_eq!(
            painted(window, &format!("ui-button-cron-pause-{id}")).is_some(),
            pause,
            "{id}: Pause"
        );
        assert_eq!(
            painted(window, &format!("ui-button-cron-resume-{id}")).is_some(),
            resume,
            "{id}: Resume"
        );
        assert!(
            painted(window, &format!("ui-button-cron-trigger-{id}")).is_some(),
            "{id}: Run now"
        );
    }
}

#[gpui::test]
fn cron_row_click_assigns_the_selected_job(cx: &mut gpui::TestAppContext) {
    const TEST: &str = "cron_panel_tests::cron_row_click_assigns_the_selected_job";
    if !crate::consolidation_tests::isolated(TEST) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    open_cron_page(&view, window, fixture(), false, None);
    let list = painted(window, "awili-cron-list").expect("the Cron list");
    assert_eq!(selected_id(&view, window), None);
    for id in ["active", "native-paused"] {
        let texts =
            painted(window, &format!("ui-row-cron-job-{id}-texts")).expect("the row's text slots");
        assert_contained(list, texts, "row text");
        click_center(window, texts);
        assert_eq!(selected_id(&view, window).as_deref(), Some(id));
    }
}

#[gpui::test]
fn cron_pause_click_acts_without_selecting_its_row(cx: &mut gpui::TestAppContext) {
    const TEST: &str = "cron_panel_tests::cron_pause_click_acts_without_selecting_its_row";
    if !crate::consolidation_tests::isolated(TEST) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    open_cron_page(&view, window, fixture(), false, None);
    let capture = CaptureFixture::start();
    point_at(&view, window, &capture);
    let list = painted(window, "awili-cron-list").expect("the Cron list");
    let row = painted(window, "ui-row-cron-job-active").expect("the active row");
    let pause = painted(window, "ui-button-cron-pause-active").expect("Pause is offered");
    assert_contained(row, pause, "Pause (in its row)");
    assert_contained(list, pause, "Pause (in the list)");
    click_center(window, pause);
    assert_dispatched(window, &capture, "/api/cron/jobs/active/pause");
    assert_eq!(
        selected_id(&view, window),
        None,
        "an action click never selects the row"
    );
}

#[gpui::test]
fn cron_run_now_click_keeps_the_existing_selection(cx: &mut gpui::TestAppContext) {
    const TEST: &str = "cron_panel_tests::cron_run_now_click_keeps_the_existing_selection";
    if !crate::consolidation_tests::isolated(TEST) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    open_cron_page(&view, window, fixture(), false, None);
    let capture = CaptureFixture::start();
    point_at(&view, window, &capture);
    let list = painted(window, "awili-cron-list").expect("the Cron list");
    let texts = painted(window, "ui-row-cron-job-active-texts").expect("the active row");
    click_center(window, texts);
    assert_eq!(selected_id(&view, window).as_deref(), Some("active"));
    assert_no_dispatch(&capture, "selecting a row");
    let run_now = painted(window, "ui-button-cron-trigger-native-paused")
        .expect("Run now is offered on a native paused job");
    assert_contained(list, run_now, "Run now");
    click_center(window, run_now);
    assert_dispatched(window, &capture, "/api/cron/jobs/native-paused/trigger");
    assert_eq!(
        selected_id(&view, window).as_deref(),
        Some("active"),
        "Run now does not reassign the selected row"
    );
}

#[gpui::test]
fn cron_loading_and_empty_use_kit_placeholders(cx: &mut gpui::TestAppContext) {
    const TEST: &str = "cron_panel_tests::cron_loading_and_empty_use_kit_placeholders";
    if !crate::consolidation_tests::isolated(TEST) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    open_cron_page(&view, window, Vec::new(), true, None);
    assert!(painted(window, "awili-cron-list").is_some());
    assert!(
        painted(window, "awili-cron-loading").is_some(),
        "a read in progress shows the loading placeholder"
    );
    assert!(
        painted(window, "awili-cron-empty").is_none(),
        "no empty state while a read is in progress"
    );
    view.update(window, |app, cx| {
        app.cron_state.loading = false;
        cx.notify();
    });
    window.run_until_parked();
    assert!(painted(window, "awili-cron-loading").is_none());
    assert!(
        painted(window, "awili-cron-empty").is_some(),
        "a finished read with no jobs shows the empty state"
    );
}

#[gpui::test]
fn cron_notice_outranks_loading_and_the_job_list(cx: &mut gpui::TestAppContext) {
    const TEST: &str = "cron_panel_tests::cron_notice_outranks_loading_and_the_job_list";
    if !crate::consolidation_tests::isolated(TEST) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    open_cron_page(&view, window, fixture(), true, Some("connection refused"));
    assert!(
        painted(window, &notice_selector(HermesPage::Automations)).is_some(),
        "the shared Hermes notice shows"
    );
    assert!(painted(window, "awili-cron-loading").is_none());
    assert!(painted(window, "awili-cron-empty").is_none());
    assert!(painted(window, "ui-row-cron-job-active").is_none());
    // Once the read succeeds the notice gives way to the same jobs.
    view.update(window, |app, cx| {
        app.cron_state.loading = false;
        app.cron_state.error = None;
        cx.notify();
    });
    window.run_until_parked();
    assert!(painted(window, &notice_selector(HermesPage::Automations)).is_none());
    assert!(painted(window, "ui-row-cron-job-active").is_some());
}

#[gpui::test]
fn cron_populated_list_fits_narrow_zoom_and_wheel_moves_only_the_list(
    cx: &mut gpui::TestAppContext,
) {
    const TEST: &str =
        "cron_panel_tests::cron_populated_list_fits_narrow_zoom_and_wheel_moves_only_the_list";
    if !crate::consolidation_tests::isolated(TEST) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    let jobs: Vec<ScheduledJob> = (0..8)
        .map(|i| {
            job(
                &format!("t{i}"),
                &format!("Scheduled task {i}"),
                "scheduled",
                true,
            )
        })
        .collect();
    const OUTER_STEPS: usize = 24;
    const OUTER_STEP_PX: f32 = 40.;
    const INNER_STEPS: usize = 6;
    const INNER_STEP_PX: f32 = 40.;
    const LIMIT_STEPS: usize = 4;
    open_cron_page(&view, window, jobs, false, None);
    crate::theme::set_ui_scale(1.5);
    window.simulate_resize(size(px(800.), px(480.)));
    window.run_until_parked();

    let start = Frame::read(&view, window);
    assert!(start.row.size.height >= crate::theme::target(), "{start:?}");
    assert!(
        start.row.left() >= start.list.left() && start.row.right() <= start.list.right(),
        "the row fits the list width: {start:?}"
    );
    for face in [start.pause, start.trigger] {
        assert!(face.size.height >= crate::theme::target(), "{face:?}");
        assert_contained(start.row, face, "job action (in its row)");
    }

    // Phase A: the list may start partly clipped by the Backstage body or the
    // window. Scroll the outer section with real wheels over the chrome above
    // the list until the visible part can hold both actions. Bounded, and the
    // traversal stops as soon as a step does not move the section.
    let faces = union(start.pause, start.trigger);
    let mut frame = start;
    for _ in 0..OUTER_STEPS {
        if frame.visible.size.height >= faces.size.height {
            break;
        }
        let Some(at) = outer_probe(&frame) else {
            break;
        };
        wheel(window, at, -OUTER_STEP_PX);
        let next = Frame::read(&view, window);
        let moved = next.outer != frame.outer;
        frame = next;
        if !moved {
            break;
        }
    }
    assert!(
        frame.visible.size.height >= faces.size.height,
        "outer wheels must reveal the first job's actions in the visible list: {frame:?}"
    );

    // Phase B: inner alignment. Real wheels at the visible list, bounded, until
    // both actions sit wholly inside list/body/window. Each step moves only the
    // list, and only while it has room in the needed direction.
    for _ in 0..INNER_STEPS {
        let faces = union(frame.pause, frame.trigger);
        if contains(frame.visible, faces) || faces.size.height > frame.visible.size.height {
            break;
        }
        let needed = if faces.bottom() > frame.visible.bottom() {
            faces.bottom() - frame.visible.bottom()
        } else {
            faces.top() - frame.visible.top()
        };
        let room = if needed > px(0.) {
            frame.inner > -frame.inner_max
        } else {
            frame.inner < px(0.)
        };
        if !room {
            break;
        }
        frame = wheel_list(&view, window, &frame, -f32::from(needed));
    }

    // Phase C: the faces the wheel and click checks rely on are physically
    // reachable: wholly inside the list, the body, and the window.
    let faces = union(frame.pause, frame.trigger);
    assert!(
        contains(frame.visible, faces),
        "the first job's Pause and Run now must sit wholly inside the visible list: faces {faces:?} / {frame:?}"
    );

    // Phase D: inside the visible list, a real wheel moves the row while the
    // list viewport, body, and outer section stay fixed.
    assert!(
        frame.inner > -frame.inner_max,
        "the list must have room to scroll for the inner wheel: {frame:?}"
    );
    let before = frame;
    frame = wheel_list(&view, window, &frame, -INNER_STEP_PX);
    assert!(
        frame.row.top() < before.row.top(),
        "the wheel moves the populated list: {before:?} -> {frame:?}"
    );

    // Phase E: one large wheel runs the list to its real scroll limit. The
    // bounded loop only repeats if that wheel did not reach the limit.
    for _ in 0..LIMIT_STEPS {
        if frame.inner <= -frame.inner_max {
            break;
        }
        frame = wheel_list(&view, window, &frame, -10_000.);
    }
    assert!(
        frame.inner <= -frame.inner_max,
        "the list must reach its real scroll limit: {frame:?}"
    );

    // Phase F: at the list limit the same visible-list wheel is handed to the
    // Backstage body, which scrolls the outer section.
    let before = frame;
    assert!(
        before.outer > -before.outer_max,
        "the outer section must have room for the handoff: {before:?}"
    );
    wheel(window, before.visible.center(), -OUTER_STEP_PX);
    let after = Frame::read(&view, window);
    assert_eq!(
        after.inner, before.inner,
        "the list holds its offset at the limit: {before:?} -> {after:?}"
    );
    assert!(
        after.outer < before.outer && after.section.top() < before.section.top(),
        "at the list limit the outer Backstage section scrolls: {before:?} -> {after:?}"
    );
    crate::theme::set_ui_scale(1.0);
}

/// Failure guard for one real Tab walk; reaching the target ends it first.
const MAX_FOCUS_STEPS: usize = 64;

/// A real key press and release on the focused control.
fn press(window: &mut VisualTestContext, key: &str) {
    window.simulate_keystrokes(key);
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse(key).unwrap(),
    });
    window.run_until_parked();
}

/// The part of the Cron list a keyboard focus can be revealed into right now:
/// the list, the Backstage body, and the live window, each read at this moment.
fn reachable(window: &mut VisualTestContext) -> Bounds<Pixels> {
    let list = painted(window, "awili-cron-list").expect("the Cron list");
    let body = painted(window, "awili-backstage-body").expect("the Backstage body");
    intersect(intersect(list, body), window_viewport(window))
}

/// Pointer-probes a control for its focus handle. The press focuses it, and a
/// release outside its face cancels the activation.
fn pointer_probe_focus(
    window: &mut VisualTestContext,
    face: Bounds<Pixels>,
    outside: gpui::Point<Pixels>,
) -> gpui::FocusHandle {
    assert!(
        !face.contains(&outside),
        "cancel point is outside the target: {face:?}"
    );
    let modifiers = gpui::Modifiers::default();
    window.simulate_mouse_down(face.center(), gpui::MouseButton::Left, modifiers);
    window.run_until_parked();
    let pressed = window
        .update(|window, cx| window.focused(cx))
        .expect("pointer down focuses the actual Cron control");
    window.simulate_mouse_up(outside, gpui::MouseButton::Left, modifiers);
    window.run_until_parked();
    assert_eq!(
        window.update(|window, cx| window.focused(cx)),
        Some(pressed.clone()),
        "mouse up outside cancels activation but leaves the target focused"
    );
    pressed
}

/// Real Tab presses from the focused control until `target` takes focus.
fn tab_to_focus(window: &mut VisualTestContext, target: &gpui::FocusHandle, label: &str) {
    let first = window
        .update(|window, cx| window.focused(cx))
        .expect("Tab traversal begins with a focused control");
    let mut visited = vec![first];
    for _ in 0..MAX_FOCUS_STEPS {
        if window.update(|window, _| target.is_focused(window)) {
            return;
        }
        window.simulate_keystrokes("tab");
        window.run_until_parked();
        let focused = window
            .update(|window, cx| window.focused(cx))
            .expect("Tab traversal retains an actual focused control");
        if focused == *target {
            return;
        }
        assert!(
            !visited.contains(&focused),
            "one complete Tab cycle ends without reaching the {label}"
        );
        visited.push(focused);
    }
    panic!("Tab traversal exceeded {MAX_FOCUS_STEPS} stops before the {label}");
}

/// The fixture with the active row already selected, so each keyboard action
/// can show that activation leaves the selection and the Cron owner alone.
fn open_keyboard_fixture(view: &Entity<AwiliApp>, window: &mut VisualTestContext) {
    open_cron_page(view, window, fixture(), false, None);
    view.update(window, |app, cx| {
        app.cron_state.selected_id = Some("active".to_string());
        cx.notify();
    });
    window.run_until_parked();
}

fn assert_selection_and_owner(
    view: &Entity<AwiliApp>,
    window: &mut VisualTestContext,
    label: &str,
) {
    assert_eq!(
        selected_id(view, window).as_deref(),
        Some("active"),
        "{label} leaves the existing row selection alone"
    );
    assert!(
        view.read_with(window, |app, _| app.mode == Mode::Cron),
        "{label} leaves Cron as the active owner"
    );
}

#[gpui::test]
fn cron_keyboard_tab_reaches_native_paused_resume_and_enter_activates_it(
    cx: &mut gpui::TestAppContext,
) {
    const TEST: &str =
        "cron_panel_tests::cron_keyboard_tab_reaches_native_paused_resume_and_enter_activates_it";
    if !crate::consolidation_tests::isolated(TEST) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    open_keyboard_fixture(&view, window);
    let capture = CaptureFixture::start();
    point_at(&view, window, &capture);
    let outside = gpui::point(px(0.), px(0.));

    // The target is probed for its real handle. Keyboard traversal then starts
    // from the earlier Pause stop of the active row, never from the target.
    let resume = painted(window, "ui-button-cron-resume-native-paused")
        .expect("Resume is offered on the native paused job");
    assert_contained(reachable(window), resume, "native-paused Resume");
    let resume_focus = pointer_probe_focus(window, resume, outside);
    let pause = painted(window, "ui-button-cron-pause-active").expect("Pause is offered");
    assert_contained(reachable(window), pause, "active Pause");
    let pause_focus = pointer_probe_focus(window, pause, outside);
    assert_ne!(
        resume_focus, pause_focus,
        "Pause and Resume are distinct stops"
    );
    assert_selection_and_owner(&view, window, "the pointer probes");
    assert_no_dispatch(&capture, "the pointer probes");

    tab_to_focus(window, &resume_focus, "native-paused Resume");
    let face = painted(window, "ui-button-cron-resume-native-paused")
        .expect("Resume stays rendered after Tab traversal");
    assert_contained(reachable(window), face, "Tab-focused native-paused Resume");
    assert!(window.update(|window, _| resume_focus.is_focused(window)));
    assert_no_dispatch(&capture, "Tab traversal");

    press(window, "enter");
    assert_dispatched(window, &capture, "/api/cron/jobs/native-paused/resume");
    assert_selection_and_owner(&view, window, "Enter on Resume");
}

#[gpui::test]
fn cron_keyboard_tab_reaches_native_paused_run_now_and_space_activates_it(
    cx: &mut gpui::TestAppContext,
) {
    const TEST: &str =
        "cron_panel_tests::cron_keyboard_tab_reaches_native_paused_run_now_and_space_activates_it";
    if !crate::consolidation_tests::isolated(TEST) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    open_keyboard_fixture(&view, window);
    let capture = CaptureFixture::start();
    point_at(&view, window, &capture);
    let outside = gpui::point(px(0.), px(0.));

    // A fresh instance: Resume's activation retires the list while loading, so
    // Run now is reached and activated here instead. Traversal starts from the
    // earlier Pause stop and passes the active Run now and native Resume stops.
    let run_now = painted(window, "ui-button-cron-trigger-native-paused")
        .expect("Run now is offered on the native paused job");
    assert_contained(reachable(window), run_now, "native-paused Run now");
    let run_now_focus = pointer_probe_focus(window, run_now, outside);
    let pause = painted(window, "ui-button-cron-pause-active").expect("Pause is offered");
    assert_contained(reachable(window), pause, "active Pause");
    let pause_focus = pointer_probe_focus(window, pause, outside);
    assert_ne!(
        run_now_focus, pause_focus,
        "Pause and Run now are distinct stops"
    );
    assert_selection_and_owner(&view, window, "the pointer probes");
    assert_no_dispatch(&capture, "the pointer probes");

    tab_to_focus(window, &run_now_focus, "native-paused Run now");
    let face = painted(window, "ui-button-cron-trigger-native-paused")
        .expect("Run now stays rendered after Tab traversal");
    assert_contained(reachable(window), face, "Tab-focused native-paused Run now");
    assert!(window.update(|window, _| run_now_focus.is_focused(window)));
    assert_no_dispatch(&capture, "Tab traversal");

    press(window, "space");
    assert_dispatched(window, &capture, "/api/cron/jobs/native-paused/trigger");
    assert_selection_and_owner(&view, window, "Space on Run now");
}
