//! The composer status strip, the app half (docs/v2/strip, DeepSeek's
//! design): the state it keeps, how it is fed, how it is polled, and how it
//! is drawn. The pure half (what it says in each state) is [`crate::strip`].
//!
//! Feeding: each turn's own events (a chunk of reply, the engine's usage,
//! the end) go to [`crate::strip::Telemetry`]; speed and first token are
//! measured here from those events. Server `timings` are not parsed.
//!
//! Polling, in-app engines only, never cloud models:
//! - `GET <base>/v1/models` (and, once it answers, `GET <base>/api/v1/health`
//!   to learn whether the host is Lemonade) every 15 s while the window is
//!   active and that engine is the conversation's model;
//! - `GET <base>/api/v1/system-stats` every 5 s, Lemonade hosts only, only
//!   while a turn on that engine runs or the popover is open.
//!
//! All of it is read-only. Nothing here sends a chat request or loads a model.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, AnyElement, Context, FontFeatures, FontWeight, IntoElement, SharedString,
};

use crate::gateway_health::{Reach, PROBE_TIMEOUT};
use crate::strip::{
    self, Detail, DotKind, EngineProbe, Inputs, LeftTone, Plan, Seg, State, Telemetry, View,
    Weight, Where,
};
use crate::theme;
use crate::work::{Mode, WorkExecutor};
use crate::workshop_composer::ModelOwner;

/// The strip's whole-row test selector.
pub(crate) const STRIP_SELECTOR: &str = "composer-status-strip";
/// The popover's test selector.
pub(crate) const POPOVER_SELECTOR: &str = "composer-status-popover";
/// The Retry link's test selector.
pub(crate) const RETRY_SELECTOR: &str = "composer-status-retry";
/// The popover's "Check now" link.
pub(crate) const CHECK_NOW_SELECTOR: &str = "composer-status-check-now";

/// Breathing frames per cycle.
const BREATH_FRAMES: u64 = 12;

/// What the app keeps for the strip.
pub(crate) struct StripState {
    pub(crate) telemetry: Telemetry,
    /// One entry per in-app server address.
    pub(crate) probes: HashMap<String, EngineProbe>,
    /// The pointer is over the strip / over the popover.
    pub(crate) hover_strip: bool,
    pub(crate) hover_pop: bool,
    /// A click pinned the popover open.
    pub(crate) pinned: bool,
    /// The popover is showing (hover or pin, after the close delay).
    pub(crate) pop_open: bool,
    pub(crate) pop_opened_at: Option<Instant>,
    /// The breathing dot's frame.
    pub(crate) phase: u8,
    clock_running: bool,
    #[cfg(not(test))]
    guard_gate_seq: u32,
    /// Network polling is on. Off under `cfg(test)` so no test ever reaches
    /// a configured host; the tests that exercise it turn it on and point it
    /// at loopback servers.
    pub(crate) poll: bool,
    /// Keyboard: the state area (opens the popover), Retry and Check now are
    /// tab stops, in that order after the model chip.
    pub(crate) focus_state: gpui::FocusHandle,
    pub(crate) focus_retry: gpui::FocusHandle,
    pub(crate) focus_check: gpui::FocusHandle,
}

/// Network polling is on in the app and off under test.
pub(crate) const POLL_DEFAULT: bool = !cfg!(test);

impl StripState {
    fn build(cx: &mut gpui::App) -> Self {
        Self {
            focus_state: cx.focus_handle(),
            focus_retry: cx.focus_handle(),
            focus_check: cx.focus_handle(),
            telemetry: Telemetry::default(),
            probes: HashMap::new(),
            hover_strip: false,
            hover_pop: false,
            pinned: false,
            pop_open: false,
            pop_opened_at: None,
            phase: 0,
            clock_running: false,
            #[cfg(not(test))]
            guard_gate_seq: u32::MAX,
            poll: POLL_DEFAULT,
        }
    }

    /// A private "gate" number for a council member waiting on its host read
    /// (counted down from the top, never a real memory-gate round).
    #[cfg(not(test))]
    pub(crate) fn next_guard_gate(&mut self) -> u32 {
        self.guard_gate_seq = self.guard_gate_seq.wrapping_sub(1);
        self.guard_gate_seq
    }

    /// The app's strip state. In the app (not under test) this also loads the
    /// persisted list of hosts known to be Lemonade.
    pub(crate) fn new(cx: &mut gpui::App) -> Self {
        if !cfg!(test) {
            if let Some(root) = crate::work::engine_data_root() {
                strip::init_known_store(root.join("known-lemonade-hosts.json"));
            }
        }
        Self::build(cx)
    }
}

/// The key the probe table uses for an engine's server.
pub(crate) fn probe_key(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_ascii_lowercase()
}

/// What the strip is about right now.
#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub(crate) conv: String,
    pub(crate) place: Where,
    /// The in-app engine, for a local target.
    pub(crate) engine: Option<awili_config::EngineConfig>,
}

/// "Qwen3 Coder Next (Hosta)" -> "Qwen3 Coder Next".
fn model_title(label: &str) -> String {
    match label.rfind('(') {
        Some(open) if label.trim_end().ends_with(')') && open > 0 => {
            label[..open].trim().to_string()
        }
        _ => label.trim().to_string(),
    }
}

impl crate::AwiliApp {
    /// The conversation, model and engine the composer's next turn targets.
    pub(crate) fn strip_target(&self) -> Option<Target> {
        if !crate::consolidation::workshop_mode(self.mode) {
            return None;
        }
        let owner = self.composer_model_owner()?;
        let conv = match &owner {
            ModelOwner::Work(id) => id.clone(),
            ModelOwner::Native(tab) => self
                .work
                .code_tabs
                .iter()
                .find(|t| t.id == *tab)
                .and_then(|t| t.history.conv_id.clone())
                .unwrap_or_else(|| format!("tab:{tab}")),
        };
        // A conversation with no route of its own yet takes the ambient lane
        // (as `parked_lane` does).
        let target = self.composer_target_for(&owner).or_else(|| match &owner {
            ModelOwner::Work(id) if !self.work.unavailable_conversation_routes.contains_key(id) => {
                Some(self.work.executor.clone())
            }
            _ => None,
        })?;
        let local = |engine: awili_config::EngineConfig| -> Option<Target> {
            let label = engine.display_label();
            Some(Target {
                conv: conv.clone(),
                place: Where::Local {
                    host: strip::host_name(&label, &engine.base_url),
                    addr: strip::host_of(&engine.base_url).unwrap_or_default(),
                    model: model_title(&label),
                    model_id: engine.model.clone(),
                },
                engine: Some(engine),
            })
        };
        let cloud = |model: String, plan: Plan| Target {
            conv: conv.clone(),
            place: Where::Cloud { model, plan },
            engine: None,
        };
        match target {
            WorkExecutor::InApp => {
                // The bearer the engine really sends (config over the env key).
                let mut engine = self.work.engine.clone();
                if let Some(key) = &self.keys.engine {
                    engine.api_key = key.clone();
                }
                local(engine)
            }
            WorkExecutor::InAppExtra { id } => {
                let extra = self.work.extra_engines.iter().find(|e| e.id == id)?;
                local(extra.to_engine_config())
            }
            WorkExecutor::Agent { engine, model } => {
                use awili_code::CodeEngine as E;
                let (name, plan) = match engine {
                    E::Claude => ("Claude", Plan::Subscription),
                    E::Codex => ("Codex", Plan::Subscription),
                    E::Glm => ("GLM", Plan::Metered),
                    E::Dsh => ("DeepSeek", Plan::Metered),
                };
                Some(cloud(model.unwrap_or_else(|| name.to_string()), plan))
            }
            WorkExecutor::Gateway => Some(cloud("Fleet gateway".to_string(), Plan::Subscription)),
            WorkExecutor::LocalBuild { .. } => None,
        }
    }

    /// The strip's view for the composer now, if there is anything to say
    /// about the target (a configured model).
    pub(crate) fn strip_view(&self, now: Instant) -> Option<(Target, View)> {
        let target = self.strip_target()?;
        let running = self.strip_turn_running(&target.conv);
        let probe = target
            .engine
            .as_ref()
            .and_then(|e| self.strip.probes.get(&probe_key(&e.base_url)));
        let context = self.composer_context_parts();
        let spend = match &target.place {
            Where::Cloud {
                plan: Plan::Metered,
                ..
            } => {
                let usd = self.cost.today_usd(crate::feed::unix_now());
                (usd > 0.0).then(|| awili_core::pricing::format_usd(usd))
            }
            _ => None,
        };
        let inputs = Inputs {
            place: target.place.clone(),
            running,
            live: self.strip.telemetry.live(&target.conv, now).or_else(|| {
                // Sent, nothing back yet: the run clock says how long it has waited.
                let started = running.then(|| self.strip_started()).flatten()?;
                Some(strip::LiveSnapshot {
                    tokens_per_s: None,
                    has_first_token: false,
                    waiting: now.saturating_duration_since(started),
                })
            }),
            last: self.strip.telemetry.last(&target.conv),
            reach: probe.map(|p| p.reach.clone()).unwrap_or(Reach::Unknown),
            reach_age: probe
                .and_then(|p| p.checked_at)
                .map(|t| now.saturating_duration_since(t)),
            lemonade: probe.is_some_and(|p| p.lemonade),
            resident: probe.and_then(|p| p.resident.clone()),
            host: probe.and_then(|p| p.host),
            host_age: probe
                .and_then(|p| p.host_at)
                .map(|t| now.saturating_duration_since(t)),
            context_text: context.map(|(used, limit, reported)| {
                awili_core::limits::used_of_limit(used, limit, reported)
            }),
            context_used: context.map(|(used, _, _)| u64::from(used)),
            context_window: context.map(|(_, limit, _)| u64::from(limit)),
            spend_today: spend,
            loading_after: Duration::from_millis(theme::STRIP_LOADING_AFTER_MS),
        };
        let view = strip::derive(&inputs);
        Some((target, view))
    }

    /// A turn is running in `conv`.
    fn strip_turn_running(&self, conv: &str) -> bool {
        match self.mode {
            Mode::Work => self.work.running && self.work_conv_id() == conv,
            Mode::Code => self
                .work
                .code_tabs
                .get(self.work.code_active)
                .is_some_and(|tab| tab.streaming),
            _ => false,
        }
    }

    // ---- feeding ----------------------------------------------------------

    /// When the running turn was sent (Work turns only; a Code tab does not
    /// keep it, so its first-token time stays unknown).
    fn strip_started(&self) -> Option<Instant> {
        (self.mode == Mode::Work)
            .then_some(self.work.run_started)
            .flatten()
    }

    /// The conversation key a Code tab's turns are measured under.
    pub(crate) fn strip_tab_conv(&self, tab_id: u64) -> String {
        self.work
            .code_tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.history.conv_id.clone())
            .unwrap_or_else(|| format!("tab:{tab_id}"))
    }

    /// A chunk of reply arrived in `conv`.
    pub(crate) fn strip_on_token(&mut self, conv: &str, cx: &mut Context<Self>) {
        let started = self.strip_started();
        self.strip.telemetry.token(conv, started, Instant::now());
        self.ensure_strip_clock(cx);
    }

    /// The engine reported a request's usage in `conv`.
    pub(crate) fn strip_on_usage(&mut self, conv: &str, input: u64, output: u64) {
        let started = self.strip_started();
        self.strip.telemetry.usage(conv, started, input, output);
    }

    /// The turn in `conv` ended.
    pub(crate) fn strip_on_end(&mut self, conv: &str) {
        self.strip.telemetry.turn_ended(conv);
    }

    // ---- polling ------------------------------------------------------------

    /// One 1 s tick (from the health tick): start whichever reads are due.
    pub(crate) fn strip_tick(&mut self, cx: &mut Context<Self>) {
        if !self.strip.poll || !self.window_active {
            return;
        }
        let Some(target) = self.strip_target() else {
            return;
        };
        let Some(engine) = target.engine.clone().filter(|e| e.is_configured()) else {
            return;
        };
        let now = Instant::now();
        let key = probe_key(&engine.base_url);
        let running_here = self.strip_turn_running(&target.conv);
        let probe = self.strip.probes.entry(key.clone()).or_default();
        let reach_due = probe.reach_due(now);
        let host_due = probe.host_due(now, running_here || self.strip.pop_open);
        if reach_due {
            probe.reach_in_flight = true;
            self.spawn_strip_read(
                key.clone(),
                engine.base_url.clone(),
                engine.api_key.clone(),
                cx,
            );
        }
        if host_due {
            if let Some(p) = self.strip.probes.get_mut(&key) {
                p.host_in_flight = true;
            }
            self.spawn_strip_host_read(key, engine.base_url.clone(), engine.api_key.clone(), cx);
        }
        if running_here {
            // The "Loading" words wait for their moment.
            cx.notify();
        }
    }

    /// "Check now" / Retry: read the current engine again, right away.
    pub(crate) fn strip_check_now(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.strip_target().and_then(|t| t.engine) else {
            return;
        };
        let key = probe_key(&engine.base_url);
        let probe = self.strip.probes.entry(key.clone()).or_default();
        if probe.reach_in_flight {
            return;
        }
        probe.reach_in_flight = true;
        self.spawn_strip_read(
            key.clone(),
            engine.base_url.clone(),
            engine.api_key.clone(),
            cx,
        );
        let want_host = self.strip.pop_open;
        if let Some(p) = self.strip.probes.get_mut(&key) {
            if want_host && p.lemonade && !p.host_in_flight {
                p.host_in_flight = true;
                self.spawn_strip_host_read(key, engine.base_url, engine.api_key.clone(), cx);
            }
        }
        cx.notify();
    }

    fn spawn_strip_read(
        &mut self,
        key: String,
        base_url: String,
        api_key: String,
        cx: &mut Context<Self>,
    ) {
        let (tx, rx) = async_channel::bounded::<strip::Reading>(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(strip::read_engine(&base_url, Some(&api_key), PROBE_TIMEOUT));
        });
        cx.spawn(async move |this, cx| {
            if let Ok(reading) = rx.recv().await {
                this.update(cx, |app, cx| {
                    app.strip_apply_reading(&key, reading, Instant::now());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn spawn_strip_host_read(
        &mut self,
        key: String,
        base_url: String,
        api_key: String,
        cx: &mut Context<Self>,
    ) {
        let (tx, rx) = async_channel::bounded::<Option<strip::HostStats>>(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(strip::fetch_host_stats(
                &base_url,
                Some(&api_key),
                PROBE_TIMEOUT,
            ));
        });
        cx.spawn(async move |this, cx| {
            if let Ok(stats) = rx.recv().await {
                this.update(cx, |app, cx| {
                    app.strip_apply_host(&key, stats, Instant::now());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Record a reachability reading.
    pub(crate) fn strip_apply_reading(&mut self, key: &str, reading: strip::Reading, now: Instant) {
        self.strip
            .probes
            .entry(key.to_string())
            .or_default()
            .apply(reading, now);
    }

    /// Record a host-stats reading (None: the read failed; the old block goes
    /// stale rather than vanishing, and its age says so).
    pub(crate) fn strip_apply_host(
        &mut self,
        key: &str,
        stats: Option<strip::HostStats>,
        now: Instant,
    ) {
        let p = self.strip.probes.entry(key.to_string()).or_default();
        p.host_in_flight = false;
        if let Some(stats) = stats {
            p.host = Some(stats);
            p.host_at = Some(now);
        }
    }

    /// The pre-send guard learned what a Lemonade host has resident: the
    /// strip says so at once.
    pub(crate) fn strip_note_residency(&mut self, base_url: &str, resident: strip::Resident) {
        let p = self.strip.probes.entry(probe_key(base_url)).or_default();
        p.lemonade = true;
        p.resident = Some(resident);
    }

    // ---- the breathing clock ----------------------------------------------------

    /// Run the strip's clock while a reply streams: the breath, and the live
    /// speed ticking. Under Reduce Motion the dot holds still and the number
    /// ticks twice a second.
    pub(crate) fn ensure_strip_clock(&mut self, cx: &mut Context<Self>) {
        if self.strip.clock_running {
            return;
        }
        self.strip.clock_running = true;
        cx.spawn(async move |this, cx| loop {
            crate::ui::motion::refresh();
            let reduced = crate::ui::motion::reduce_motion();
            let step = if reduced {
                Duration::from_millis(500)
            } else {
                Duration::from_millis(theme::STRIP_BREATH_MS / BREATH_FRAMES)
            };
            cx.background_executor().timer(step).await;
            let keep = this
                .update(cx, |app, cx| {
                    let live = app
                        .strip_target()
                        .is_some_and(|t| app.strip_turn_running(&t.conv));
                    if !live {
                        app.strip.clock_running = false;
                        cx.notify();
                        return false;
                    }
                    if !reduced {
                        app.strip.phase = ((u64::from(app.strip.phase) + 1) % BREATH_FRAMES) as u8;
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !keep {
                break;
            }
        })
        .detach();
    }

    /// The breathing dot's opacity for the current frame: a triangle between
    /// half and full, still at full under Reduce Motion.
    pub(crate) fn strip_breath(&self) -> f32 {
        if crate::ui::motion::reduce_motion() {
            return 1.0;
        }
        let half = (BREATH_FRAMES / 2) as f32;
        let d = (f32::from(self.strip.phase) - half).abs();
        0.5 + 0.5 * (1.0 - d / half)
    }

    // ---- the popover's open state -------------------------------------------------

    /// A key on one of the strip's tab stops: Enter or Space does the stop's
    /// job, Escape closes a pinned popover.
    fn strip_key(&mut self, ev: &gpui::KeyDownEvent, job: StripKey, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "enter" | "space" => match job {
                StripKey::Open => self.strip_toggle_pin(cx),
                StripKey::Check => self.strip_check_now(cx),
            },
            "escape" if self.strip.pop_open || self.strip.pinned => self.strip_close(cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Close the popover whatever opened it.
    pub(crate) fn strip_close(&mut self, cx: &mut Context<Self>) {
        self.strip.pinned = false;
        self.strip.pop_open = false;
        self.strip.pop_opened_at = None;
        cx.notify();
    }

    fn strip_hover(
        &mut self,
        over_strip: Option<bool>,
        over_pop: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        if let Some(h) = over_strip {
            self.strip.hover_strip = h;
        }
        if let Some(h) = over_pop {
            self.strip.hover_pop = h;
        }
        let want = self.strip.hover_strip || self.strip.hover_pop || self.strip.pinned;
        if want && !self.strip.pop_open {
            self.strip.pop_open = true;
            self.strip.pop_opened_at = Some(Instant::now());
            self.strip_fade_in(cx);
            // Read the host once on opening, not only on the 5 s beat.
            self.strip_tick(cx);
        } else if !want && self.strip.pop_open {
            // A short grace so the pointer can cross the gap to the popover.
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(140))
                    .await;
                this.update(cx, |app, cx| {
                    let still = app.strip.hover_strip || app.strip.hover_pop || app.strip.pinned;
                    if !still && app.strip.pop_open {
                        app.strip.pop_open = false;
                        app.strip.pop_opened_at = None;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// A click on the strip pins the popover open (or releases it).
    pub(crate) fn strip_toggle_pin(&mut self, cx: &mut Context<Self>) {
        self.strip.pinned = !self.strip.pinned;
        if self.strip.pinned {
            if !self.strip.pop_open {
                self.strip.pop_open = true;
                self.strip.pop_opened_at = Some(Instant::now());
                self.strip_fade_in(cx);
            }
        } else if !(self.strip.hover_strip || self.strip.hover_pop) {
            self.strip.pop_open = false;
            self.strip.pop_opened_at = None;
        }
        cx.notify();
    }

    /// Repaint through the popover's 130 ms fade.
    fn strip_fade_in(&mut self, cx: &mut Context<Self>) {
        if crate::ui::motion::reduce_motion() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let (fade, frame) = (theme::STRIP_FADE_MS, theme::STRIP_FADE_FRAME_MS);
            let frames = fade / frame + 1;
            for _ in 0..frames {
                cx.background_executor()
                    .timer(Duration::from_millis(theme::STRIP_FADE_FRAME_MS))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn strip_pop_opacity(&self) -> f32 {
        if crate::ui::motion::reduce_motion() {
            return 1.0;
        }
        match self.strip.pop_opened_at {
            Some(at) => {
                let fade = theme::STRIP_FADE_MS as f32;
                let elapsed = at.elapsed().as_millis() as f32;
                (elapsed / fade).clamp(0.0, 1.0)
            }
            None => 1.0,
        }
    }

    // ---- drawing ------------------------------------------------------------------

    /// The strip for the composer footer: (left, right) pieces. Empty when
    /// there is no model to speak of.
    pub(crate) fn strip_elements(
        &self,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, Vec<AnyElement>) {
        let narrow = self.window_w < theme::COMPACT_W;
        let Some((_, view)) = self.strip_view(Instant::now()) else {
            return (vec![self.composer_model_chip(narrow, cx)], Vec::new());
        };
        let p = theme::p();
        let breath = self.strip_breath();
        let live = view.state == State::Streaming;

        // The dot.
        let dot_color = match view.dot {
            DotKind::Idle => p.control_line,
            DotKind::Live => p.text_2,
            DotKind::Attention => p.attention,
            DotKind::Alert => p.act_now,
        };
        let dot = div()
            .debug_selector(|| "composer-status-dot".to_string())
            .flex_none()
            .size(theme::z(theme::STRIP_DOT))
            .rounded(theme::z(theme::RADIUS_PILL))
            .bg(dot_color)
            .when(view.dot == DotKind::Live, |d| d.opacity(breath));

        // Left: dot and words.
        let left_color = match view.left_tone {
            LeftTone::Values => p.text_2,
            LeftTone::Alert => p.act_now_text,
            LeftTone::Attention => p.attention,
        };
        let left = div()
            .id("composer-status-left")
            .debug_selector(|| STRIP_SELECTOR.to_string())
            .track_focus(&self.strip.focus_state.clone().tab_stop(true))
            .focus_visible(crate::ui::focus_ring::focus_ring_style_on(p.panel_solid))
            .on_key_down(cx.listener(|app, ev: &gpui::KeyDownEvent, _, cx| {
                app.strip_key(ev, StripKey::Open, cx)
            }))
            .flex()
            .flex_none()
            .items_center()
            .h(theme::z(theme::STRIP_H))
            .pl(theme::z(theme::SP_8))
            .gap(theme::z(theme::STRIP_DOT_GAP))
            .child(dot)
            .child(segs_row(
                &view.left,
                live,
                left_color,
                view.left_tone != LeftTone::Values,
            ))
            .on_hover(cx.listener(|app, over: &bool, _, cx| {
                app.strip_hover(Some(*over), None, cx);
            }))
            .on_click(cx.listener(|app, _, _, cx| app.strip_toggle_pin(cx)))
            .into_any_element();

        // Right: the context, or Retry.
        let retry = view.retry.then(|| {
            div()
                .id("composer-status-retry")
                .debug_selector(|| RETRY_SELECTOR.to_string())
                .track_focus(&self.strip.focus_retry.clone().tab_stop(true))
                .focus_visible(crate::ui::focus_ring::focus_ring_style_on(p.panel_solid))
                .on_key_down(cx.listener(|app, ev: &gpui::KeyDownEvent, _, cx| {
                    app.strip_key(ev, StripKey::Check, cx)
                }))
                .cursor_pointer()
                .text_color(p.contrast_text)
                .font_weight(FontWeight::MEDIUM)
                .hover(|s| s.underline())
                .child("Retry")
                .on_click(cx.listener(|app, _, _, cx| {
                    cx.stop_propagation();
                    app.strip_check_now(cx);
                }))
        });
        let popover = if self.strip.pop_open {
            // An ordinary absolute child (not `deferred`): its Check now link
            // then sits in the normal Tab order, right after Retry.
            Some(
                div()
                    .absolute()
                    .right(gpui::Pixels::ZERO)
                    .bottom(theme::z(theme::STRIP_H + theme::STRIP_POP_GAP))
                    .child(self.strip_popover(&view.detail, cx)),
            )
        } else {
            None
        };
        let right = div()
            .id("composer-status-right")
            .debug_selector(|| "composer-status-right".to_string())
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .h(theme::z(theme::STRIP_H))
            .gap(theme::z(theme::SP_8))
            .text_size(theme::z(theme::STRIP_TEXT))
            .font_features(tabular())
            .child(
                div()
                    .id("composer-context-use")
                    .debug_selector(|| "ui-text-composer-context-use".to_string())
                    .flex()
                    .items_center()
                    .child(segs_row(&view.right, live, p.text_dim, false)),
            )
            .children(retry)
            .children(popover)
            .on_hover(cx.listener(|app, over: &bool, _, cx| {
                app.strip_hover(Some(*over), None, cx);
            }))
            .on_click(cx.listener(|app, _, _, cx| app.strip_toggle_pin(cx)))
            .into_any_element();

        let chip = self.composer_model_chip(narrow, cx);
        (vec![chip, left], vec![right])
    }

    fn strip_popover(&self, d: &Detail, cx: &mut Context<Self>) -> AnyElement {
        let p = theme::p();
        let rule = || {
            div()
                .h(theme::z(theme::STRIP_RULE_H))
                .mt(theme::z(theme::STRIP_RULE_TOP))
                .mb(theme::z(theme::STRIP_RULE_BOTTOM))
                .bg(p.line)
        };
        let section = |title: &str| {
            div()
                .text_size(theme::z(theme::STRIP_SEC_TEXT))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.text_dim)
                .child(title.to_string())
        };
        let row = |k: &str, v: &[Seg]| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(theme::z(theme::STRIP_ROW_H))
                .text_size(theme::z(theme::STRIP_TEXT))
                .child(div().text_color(p.text_dim).child(k.to_string()))
                .child(segs_row(v, false, p.text_2, false))
        };
        let state_dot = match d.state {
            State::Ready => p.control_line,
            State::Streaming => p.text_2,
            State::Loading | State::NotLoaded => p.attention,
            State::Unreachable => p.act_now,
        };

        let mut body = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(theme::z(theme::STRIP_TITLE_TEXT))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.text)
                        .child(d.title.clone()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme::z(theme::SP_4))
                        .text_size(theme::z(theme::STRIP_SEC_TEXT))
                        .text_color(p.text_dim)
                        .child(
                            div()
                                .size(theme::z(theme::STRIP_DOT))
                                .rounded(theme::z(theme::RADIUS_PILL))
                                .bg(state_dot),
                        )
                        .child(d.state.word()),
                ),
        );
        if let Some((host, addr)) = &d.sub {
            body = body.child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(theme::z(theme::SP_8))
                    .text_size(theme::z(theme::STRIP_TEXT))
                    .text_color(p.text_dim)
                    .child(div().font_weight(FontWeight::MEDIUM).child(host.clone()))
                    .child(
                        div()
                            .font_family(theme::MONO_FONT.to_string())
                            .text_size(theme::z(theme::STRIP_SEC_TEXT))
                            .text_color(p.faint)
                            .child(addr.clone()),
                    ),
            );
        }
        if !d.reply.is_empty() {
            let mut block = div().flex().flex_col().child(section("Last reply"));
            for (k, v) in &d.reply {
                block = block.child(row(k, v));
            }
            body = body.child(rule()).child(block);
        }
        if let Some(ctx) = &d.context {
            body = body.child(rule()).child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_4))
                    .child(section("Context"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_size(theme::z(theme::STRIP_TEXT))
                            .child(segs_row(&ctx.text, false, p.text_2, false))
                            .child(div().text_color(p.text_dim).child(ctx.percent.clone())),
                    )
                    .child(
                        div()
                            .h(theme::z(theme::STRIP_METER_H))
                            .rounded(theme::z(theme::RADIUS_PILL))
                            .bg(p.raise)
                            .child(
                                div()
                                    .h_full()
                                    .w(gpui::relative(ctx.fraction))
                                    .rounded(theme::z(theme::RADIUS_PILL))
                                    .bg(p.text_dim),
                            ),
                    ),
            );
        }
        if let Some((name, rows)) = &d.host {
            let mut block = div().flex().flex_col().child(section(name));
            for (k, v) in rows {
                block = block.child(row(k, v));
            }
            body = body.child(rule()).child(block);
        }
        if !d.cloud.is_empty() {
            let mut block = div().flex().flex_col();
            for (k, v) in &d.cloud {
                block = block.child(row(k, v));
            }
            body = body.child(rule()).child(block);
        }
        // The foot: the reading's age, and "Check now".
        if d.age.is_some() || d.check_now {
            body = body.child(
                div()
                    .mt(theme::z(theme::STRIP_RULE_BOTTOM))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(theme::z(theme::SP_8))
                    .text_size(theme::z(theme::STRIP_FOOT_TEXT))
                    .text_color(p.faint)
                    .children(d.age.clone().map(|a| div().child(a)))
                    .children(d.check_now.then(|| {
                        div()
                            .id("composer-status-check-now")
                            .debug_selector(|| CHECK_NOW_SELECTOR.to_string())
                            .track_focus(&self.strip.focus_check.clone().tab_stop(true))
                            .focus_visible(crate::ui::focus_ring::focus_ring_style_on(
                                p.panel_solid,
                            ))
                            .on_key_down(cx.listener(|app, ev: &gpui::KeyDownEvent, _, cx| {
                                app.strip_key(ev, StripKey::Check, cx)
                            }))
                            .cursor_pointer()
                            .text_color(p.contrast_text)
                            .font_weight(FontWeight::MEDIUM)
                            .hover(|s| s.underline())
                            .child("Check now")
                            .on_click(cx.listener(|app, _, _, cx| app.strip_check_now(cx)))
                    })),
            );
        }

        div()
            .id("composer-status-popover")
            .debug_selector(|| POPOVER_SELECTOR.to_string())
            .occlude()
            .w(theme::z(theme::STRIP_POP_W))
            .pt(theme::z(theme::STRIP_POP_PAD_T))
            .pb(theme::z(theme::STRIP_POP_PAD_B))
            .px(theme::z(theme::STRIP_POP_PAD_X))
            .rounded(theme::z(theme::RADIUS_M))
            .border_1()
            .border_color(p.control_line)
            .bg(p.panel_solid)
            .shadow(theme::float_shadow())
            .text_color(p.text_2)
            .text_size(theme::z(theme::STRIP_TEXT))
            .font_features(tabular())
            .opacity(self.strip_pop_opacity())
            .on_hover(cx.listener(|app, over: &bool, _, cx| {
                app.strip_hover(None, Some(*over), cx);
            }))
            .child(body)
            .into_any_element()
    }

    // ---- the Lemonade safety guard -----------------------------------------------

    /// The sentence to refuse with when `engine` is a Lemonade host that does
    /// not have its model resident (or cannot be seen to be ready), or `None`
    /// when a request may go. BLOCKING: a read of `/api/v1/health`, which
    /// never loads a model.
    pub(crate) fn lemonade_refusal(
        engine: &awili_config::EngineConfig,
        timeout: Duration,
    ) -> Option<(String, Option<strip::Resident>)> {
        let key = Some(engine.api_key.as_str()).filter(|k| !k.trim().is_empty());
        let verdict = strip::check_residency(&engine.base_url, &engine.model, key, timeout);
        Self::refusal_for(engine, verdict)
    }

    /// The refusal (card, residents) a verdict calls for, or `None` to send.
    fn refusal_for(
        engine: &awili_config::EngineConfig,
        verdict: strip::Residency,
    ) -> Option<(String, Option<strip::Resident>)> {
        let label = engine.display_label();
        let host = strip::host_name(&label, &engine.base_url);
        match verdict {
            strip::Residency::NotLemonade | strip::Residency::Resident(_) => None,
            strip::Residency::NotResident(resident) => Some((
                strip::not_loaded_card(&model_title(&label), &host),
                Some(resident),
            )),
            strip::Residency::Silent => Some((strip::host_silent_card(&host), None)),
        }
    }

    /// THE BACKSTOP, at the transport: the gate every HTTP request the in-app
    /// engine sends passes (first requests, tool-loop steps, retries, resumes
    /// after an approval or a question, warm-ups). A recent reading of the
    /// host (at most `max_age` old) is reused, so a tool loop does not read
    /// the host on every step; otherwise the host is read, on the engine
    /// worker's own thread, never the UI's. A refused request is never sent
    /// and ends the turn with the card.
    pub(crate) fn transport_preflight(
        engine: &awili_config::EngineConfig,
        max_age: Duration,
    ) -> awili_seam::Preflight {
        let engine = engine.clone();
        std::sync::Arc::new(move || {
            let verdict = strip::cached_residency(&engine.base_url, &engine.model, max_age);
            let refusal = match verdict {
                Some(v) => Self::refusal_for(&engine, v),
                None => Self::lemonade_refusal(&engine, strip::guard_timeout()),
            };
            match refusal {
                None => Ok(()),
                Some((card, _)) => Err(card),
            }
        })
    }
}

/// What Enter or Space does on a strip tab stop.
#[derive(Clone, Copy)]
enum StripKey {
    Open,
    Check,
}

/// Tabular figures, so a ticking number never shifts its neighbours.
fn tabular() -> FontFeatures {
    FontFeatures(std::sync::Arc::new(vec![("tnum".to_string(), 1)]))
}

/// One run of segments, typed per the design: values medium, units regular
/// and dim, "of" quieter again.
fn segs_row(segs: &[Seg], live: bool, sentence: gpui::Rgba, bold_sentence: bool) -> AnyElement {
    let p = theme::p();
    let mut row = div()
        .flex()
        .items_center()
        .gap(theme::z(theme::SP_4))
        .text_size(theme::z(theme::STRIP_TEXT))
        .line_height(theme::z(theme::STRIP_H))
        .font_features(tabular())
        .whitespace_nowrap();
    for s in segs {
        let text: SharedString = s.text.clone().into();
        let el = match s.weight {
            Weight::Value => div().font_weight(FontWeight::MEDIUM).text_color(if live {
                p.text
            } else {
                p.text_2
            }),
            Weight::Unit => div().text_color(p.text_dim),
            Weight::Of => div().text_color(p.faint),
            Weight::Window => div().font_weight(FontWeight::MEDIUM).text_color(p.text_dim),
            Weight::Plain => div()
                .text_color(sentence)
                .when(bold_sentence, |d| d.font_weight(FontWeight::MEDIUM)),
        };
        row = row.child(el.child(text));
    }
    row.into_any_element()
}

#[cfg(test)]
#[path = "strip_app_tests.rs"]
mod tests;
