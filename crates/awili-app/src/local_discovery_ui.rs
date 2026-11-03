//! Explicit, read-only LAN discovery. Results enter only the settings draft.
use super::*;
use crate::ui;
use std::cell::{Cell, RefCell};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub(crate) struct UiState {
    running: bool,
    generation: u64,
    cancel: Arc<AtomicBool>,
    progress: (usize, usize),
    report: Option<local_discovery::DiscoveryReport>,
    selected: usize,
    focus: FocusHandle,
    results_scroll: ScrollHandle,
    reveal_key: Rc<RefCell<Option<LocalDiscoveryRevealKey>>>,
}

#[derive(Clone, PartialEq)]
struct LocalDiscoveryRevealKey {
    selected: usize,
    face: Bounds<Pixels>,
    inner_viewport: Bounds<Pixels>,
    outer_viewport: Bounds<Pixels>,
}

impl UiState {
    pub(crate) fn cancel_if_hidden(&self, visible: bool) {
        if !visible && self.running {
            self.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub(crate) fn new(cx: &mut Context<AwiliApp>) -> Self {
        Self {
            running: false,
            generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: (0, 0),
            report: None,
            selected: 0,
            focus: cx.focus_handle(),
            results_scroll: ScrollHandle::new(),
            reveal_key: Rc::default(),
        }
    }
}

impl Drop for UiState {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

enum ScanEvent {
    Progress(local_discovery::DiscoveryProgress),
    Done(local_discovery::DiscoveryReport),
}

impl AwiliApp {
    pub(crate) fn start_local_discovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_nav_focus_pending = false;
        self.local_discovery.focus.focus(window, cx);
        if self.local_discovery.running {
            return;
        }
        self.local_discovery.generation = self.local_discovery.generation.wrapping_add(1);
        let generation = self.local_discovery.generation;
        self.local_discovery.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.local_discovery.cancel.clone();
        self.local_discovery.running = true;
        self.local_discovery.report = None;
        self.local_discovery.progress = (0, 0);
        self.local_discovery.selected = 0;
        self.local_discovery.results_scroll = ScrollHandle::new();
        *self.local_discovery.reveal_key.borrow_mut() = None;
        let (tx, rx) = async_channel::bounded(8);
        std::thread::spawn(move || {
            let report = local_discovery::scan(&cancel, |p| {
                let _ = tx.try_send(ScanEvent::Progress(p));
            });
            let _ = tx.send_blocking(ScanEvent::Done(report));
        });
        cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                let done = matches!(event, ScanEvent::Done(_));
                if this
                    .update(cx, |this, cx| {
                        if this.local_discovery.generation != generation {
                            return;
                        }
                        match event {
                            ScanEvent::Progress(p) => {
                                this.local_discovery.progress = (p.completed, p.total)
                            }
                            ScanEvent::Done(report) => {
                                this.local_discovery.progress = (report.completed, report.total);
                                this.local_discovery.report = Some(report);
                                this.local_discovery.running = false;
                                this.reveal_local_discovery();
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                if done {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.local_discovery.generation == generation {
                    this.local_discovery.running = false;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn reveal_local_discovery(&self) {
        if self.settings.open && self.settings.tab == settings::Tab::Form {
            let count = self
                .settings
                .draft
                .as_ref()
                .map_or(0, |d| d.providers.len());
            self.settings_nav_scroll.scroll_to_item(7 + count.max(1));
        }
    }

    fn add_local_discovery_result(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(result) = self
            .local_discovery
            .report
            .as_ref()
            .and_then(|r| r.results.get(index))
        else {
            return;
        };
        let Some(draft) = self.settings.draft.as_mut() else {
            return;
        };
        // Never overwrite a manually configured endpoint or its credential.
        if draft
            .providers
            .iter()
            .any(|p| p.base_url.trim_end_matches('/') == result.base_url.trim_end_matches('/'))
        {
            return;
        }
        draft.providers.push(settings::ProviderDraft {
            name: format!("Local {}", result.base_url.trim_start_matches("http://")),
            kind: result.kind,
            base_url: result.base_url.clone(),
            models: result
                .models
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            enabled: true,
            api_key: None,
        });
        self.reveal_local_discovery();
        cx.notify();
    }

    pub(crate) fn local_discovery_rows(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let state = &self.local_discovery;
        let mut card_children = vec![
            ui::body("Find model services on this computer and nearby LAN addresses.")
                .into_any_element(),
            ui::caption(
                "Lists advertised models without loading them. Choose Add, then Save to keep a connection.",
            )
            .into_any_element(),
            ui::Button::new(
                "scan-local-models",
                if state.running {
                    "Scanning…"
                } else {
                    "Find local models"
                },
            )
            .primary()
            .on_click(cx.listener(|this, _, window, cx| {
                this.start_local_discovery(window, cx)
            }))
            .into_any_element(),
        ];

        let mut discovery = div().id("local-model-discovery").track_focus(&state.focus);

        discovery = discovery.on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
            if ev.keystroke.modifiers.control
                || ev.keystroke.modifiers.platform
                || ev.keystroke.modifiers.alt
            {
                return;
            }
            let count = this
                .local_discovery
                .report
                .as_ref()
                .map_or(0, |r| r.results.len());
            match ev.keystroke.key.as_str() {
                // RC2-APP (F0146): Shift-Tab is not caught here; it moves focus back.
                key if leaves_discovery(key, ev.keystroke.modifiers.shift) => {
                    this.local_discovery.cancel.store(true, Ordering::Relaxed);
                    this.settings_nav_focus.focus(window, cx);
                    cx.stop_propagation();
                }
                "up" | "down" if count > 0 => {
                    this.local_discovery.selected = if ev.keystroke.key == "up" {
                        (this.local_discovery.selected + count - 1) % count
                    } else {
                        (this.local_discovery.selected + 1) % count
                    };
                    this.reveal_local_discovery();
                    this.local_discovery
                        .results_scroll
                        .scroll_to_item(this.local_discovery.selected);
                    cx.stop_propagation();
                    cx.notify();
                }
                "enter" | "return" if count > 0 => {
                    this.add_local_discovery_result(this.local_discovery.selected, cx);
                    cx.stop_propagation();
                }
                "enter" | "return" => {
                    this.start_local_discovery(window, cx);
                    cx.stop_propagation();
                }
                _ => {}
            }
        }));
        if state.running {
            card_children.push(
                ui::Banner::new(
                    "local-discovery-progress",
                    theme::Tone::Neutral,
                    format!(
                        "Checked {} of {} addresses and ports",
                        state.progress.0, state.progress.1
                    ),
                )
                .into_any_element(),
            );
            card_children.push(
                ui::Button::new("cancel-local-discovery", "Cancel")
                    .quiet()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.local_discovery.cancel.store(true, Ordering::Relaxed);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        if let Some(report) = &state.report {
            let status = if report.cancelled {
                "Cancelled — partial results"
            } else if report.deadline_reached || report.truncated {
                "Finished with limited coverage"
            } else {
                "Scan complete"
            };
            let tone = if report.cancelled || report.deadline_reached || report.truncated {
                theme::Tone::Attention
            } else {
                theme::Tone::Neutral
            };
            card_children.push(
                ui::Banner::new(
                    "local-discovery-status",
                    tone,
                    format!(
                        "{status}: {} services. Up/Down to choose; Enter to add; Escape to return.",
                        report.results.len()
                    ),
                )
                .into_any_element(),
            );
            for (index, scope) in report.scopes.iter().enumerate() {
                card_children.push(
                    ui::Row::new(("local-model-scope", index))
                        .title(format!("{}: {}", scope.interface, scope.scanned_cidr))
                        .subtitle(format!("{} addresses scanned", scope.host_count))
                        .into_any_element(),
                );
            }
            for (index, error) in report.errors.iter().enumerate() {
                card_children.push(
                    ui::Banner::new(
                        ("local-model-error", index),
                        theme::Tone::Attention,
                        error.clone(),
                    )
                    .into_any_element(),
                );
            }
            let selected = state.selected;
            let focus = state.focus.clone();
            let reveal_key = state.reveal_key.clone();
            let inner_scroll = state.results_scroll.clone();
            let outer_scroll = self.settings_nav_scroll.clone();
            let owner = cx.entity().downgrade();
            let wheel_offset = Rc::new(Cell::new(inner_scroll.offset().y));
            let wheel_offset_for_listener = wheel_offset.clone();
            let wheel_offset_for_prepaint = wheel_offset.clone();
            let mut results = div()
                // GPUI's overflow handler applies the inner scroll first;
                // this bubble listener runs afterward on the same element.
                // Stop the event before it reaches the enclosing Settings
                // scroller when this event started with room in the inner
                // list. At an inner edge, leave the event unhandled so the
                // outer Settings pane can scroll.
                .on_scroll_wheel(cx.listener(
                    move |this, event: &gpui::ScrollWheelEvent, window, cx| {
                        let delta = event.delta.pixel_delta(window.line_height());
                        // This Div has overflow-y scrolling only and leaves
                        // GPUI's default axis restriction disabled. Match
                        // Div::paint_scroll_listener: use y when present,
                        // otherwise remap x-only wheel input onto y.
                        let effective_y = if delta.y == Pixels::ZERO {
                            delta.x
                        } else {
                            delta.y
                        };
                        let before = wheel_offset_for_listener.get();
                        let after = this.local_discovery.results_scroll.offset().y;
                        wheel_offset_for_listener.set(after);
                        if effective_y == Pixels::ZERO {
                            return;
                        }
                        let max = this.local_discovery.results_scroll.max_offset().y;
                        // GPUI applies the raw delta and clamps before this
                        // listener runs. The last prepainted offset records
                        // the true pre-event position; deriving it from the
                        // clamped result would mistake a crossing event for
                        // an event that started at the edge.
                        let edge_epsilon =
                            px(theme::SCROLL_EDGE_TOLERANCE_PHYSICAL_PX / window.scale_factor());
                        let started_with_room = if effective_y > Pixels::ZERO {
                            before < -edge_epsilon
                        } else {
                            before > -max + edge_epsilon
                        };
                        let moved_inner = (after - before).abs() > edge_epsilon;
                        if started_with_room && moved_inner {
                            cx.stop_propagation();
                        }
                    },
                ))
                .on_children_prepainted(move |_, window, cx| {
                    wheel_offset_for_prepaint.set(inner_scroll.offset().y);
                    if !focus.is_focused(window) {
                        *reveal_key.borrow_mut() = None;
                        return;
                    }
                    // GPUI's Div does not pass its child bounds to this
                    // callback when the same Div owns a tracked ScrollHandle.
                    // Read the tracked row bounds directly, then apply the
                    // current inner offset to put them in the painted window
                    // coordinate space used by both viewport handles.
                    let Some(mut face) = inner_scroll.bounds_for_item(selected) else {
                        *reveal_key.borrow_mut() = None;
                        return;
                    };
                    face.origin.y += inner_scroll.offset().y;
                    let inner_viewport = inner_scroll.bounds();
                    let outer_viewport = outer_scroll.bounds();
                    let inner_offset = inner_scroll.offset();
                    let outer_offset = outer_scroll.offset();
                    // Normalize both scroll deltas so a reader's manual wheel
                    // movement does not look like a new focus/layout request.
                    let normalized_face = Bounds::new(
                        point(
                            face.origin.x,
                            face.origin.y - inner_offset.y - outer_offset.y,
                        ),
                        face.size,
                    );
                    let normalized_inner = Bounds::new(
                        point(
                            inner_viewport.origin.x,
                            inner_viewport.origin.y - outer_offset.y,
                        ),
                        inner_viewport.size,
                    );
                    let key = LocalDiscoveryRevealKey {
                        selected,
                        face: normalized_face,
                        inner_viewport: normalized_inner,
                        outer_viewport,
                    };
                    {
                        let mut last = reveal_key.borrow_mut();
                        if last.as_ref() == Some(&key) {
                            return;
                        }
                        *last = Some(key.clone());
                    }

                    let key_cell = reveal_key.clone();
                    let focus = focus.clone();
                    let inner_scroll = inner_scroll.clone();
                    let outer_scroll = outer_scroll.clone();
                    let owner = owner.clone();
                    window.defer(cx, move |window, cx| {
                        if key_cell.borrow().as_ref() != Some(&key)
                            || !focus.is_focused(window)
                            || !owner
                                .read_with(cx, |this, _| {
                                    this.settings.open
                                        && this.settings.tab == settings::Tab::Form
                                        && this.settings.page == settings_catalog::Page::Models
                                        && this.local_discovery.selected == selected
                                })
                                .unwrap_or(false)
                        {
                            return;
                        }

                        // Bring the actual measured row into the results list,
                        // then move that result through the Settings pane's
                        // outer scroll viewport.
                        let inner_viewport = inner_scroll.bounds();
                        let mut inner_offset = inner_scroll.offset();
                        if face.top() < inner_viewport.top() {
                            inner_offset.y += inner_viewport.top() - face.top();
                        } else if face.bottom() > inner_viewport.bottom() {
                            inner_offset.y += inner_viewport.bottom() - face.bottom();
                        }
                        inner_offset.y = inner_offset
                            .y
                            .clamp(-inner_scroll.max_offset().y, Pixels::ZERO);
                        let inner_delta = inner_offset.y - inner_scroll.offset().y;
                        let shifted_face = Bounds::new(
                            point(face.origin.x, face.origin.y + inner_delta),
                            face.size,
                        );

                        let outer_viewport = outer_scroll.bounds();
                        let mut outer_offset = outer_scroll.offset();
                        if shifted_face.top() < outer_viewport.top() {
                            outer_offset.y += outer_viewport.top() - shifted_face.top();
                        } else if shifted_face.bottom() > outer_viewport.bottom() {
                            outer_offset.y += outer_viewport.bottom() - shifted_face.bottom();
                        }
                        outer_offset.y = outer_offset
                            .y
                            .clamp(-outer_scroll.max_offset().y, Pixels::ZERO);

                        let inner_changed = inner_offset != inner_scroll.offset();
                        let outer_changed = outer_offset != outer_scroll.offset();
                        if inner_changed {
                            inner_scroll.set_offset(inner_offset);
                        }
                        if outer_changed {
                            outer_scroll.set_offset(outer_offset);
                        }
                        if inner_changed || outer_changed {
                            window.refresh();
                        }
                    });
                })
                .id("local-discovery-results")
                .debug_selector(|| "local-discovery-results".to_string())
                .h(theme::z(theme::LOCAL_DISCOVERY_RESULTS_H))
                .flex_none()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_4))
                .track_scroll(&state.results_scroll);
            for (index, result) in report.results.iter().enumerate() {
                let added = self.settings.draft.as_ref().is_some_and(|d| {
                    d.providers.iter().any(|p| {
                        p.base_url.trim_end_matches('/') == result.base_url.trim_end_matches('/')
                    })
                });
                let mut models = result
                    .models
                    .iter()
                    .take(5)
                    .map(|m| m.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if result.models.len() > 5 {
                    models.push_str(&format!(" (+{} more)", result.models.len() - 5));
                }
                results = results.child(
                    ui::Row::new(("local-model-result", index))
                        .title(format!(
                            "{} — {}",
                            result.base_url,
                            if added {
                                "Added to draft"
                            } else {
                                "Add connection"
                            }
                        ))
                        .subtitle(models)
                        // Discovery URLs and model ids can be arbitrarily
                        // long. Let the shared Row truncate these two lines
                        // at narrow zoom levels; its title/subtitle tooltips
                        // retain the complete values without turning one
                        // result into a viewport-tall hit target.
                        .selected(index == state.selected)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.local_discovery.selected = index;
                            this.local_discovery.focus.focus(window, cx);
                            this.add_local_discovery_result(index, cx);
                        })),
                );
            }
            if !report.results.is_empty() {
                card_children.push(results.into_any_element());
            }
            if report.results.is_empty() {
                card_children.push(
                    ui::EmptyState::new(
                        "local-discovery-empty",
                        "No model lists found",
                        "A service may need authentication, use another port, or be outside this scan. You can still add its URL manually through the provider form.",
                    )
                    .top(theme::z(theme::SP_12))
                    .into_any_element(),
                );
            }
        }
        // The card is constructed after state-dependent children so a single
        // static Settings child remains the scroll/navigation target.
        discovery =
            discovery.child(ui::Card::new("local-model-discovery-card").children(card_children));
        vec![
            self.panel_section("Local models", false),
            discovery.into_any_element(),
        ]
    }
}

/// RC2-APP (F0146): the keys that leave the Local models list for the
/// settings navigation: Esc, and Tab — but not Shift-Tab, which goes on to
/// the ordinary previous-focus move.
fn leaves_discovery(key: &str, shift: bool) -> bool {
    key == "escape" || (key == "tab" && !shift)
}

#[cfg(test)]
mod rc2app_tests {
    /// RC2-APP (F0146): Shift-Tab is left to the ordinary focus move.
    #[test]
    fn rc2app_shift_tab_is_not_caught() {
        assert!(super::leaves_discovery("escape", false));
        assert!(super::leaves_discovery("tab", false));
        assert!(!super::leaves_discovery("tab", true));
        assert!(!super::leaves_discovery("up", false));
    }
}

#[cfg(test)]
mod rendered_tests {
    use super::*;
    use gpui::{px, size, Entity, Modifiers, TestAppContext, VisualTestContext};

    fn sel(selector: &str) -> &'static str {
        Box::leak(selector.to_string().into_boxed_str())
    }

    fn click(window: &mut VisualTestContext, selector: &str) {
        let bounds = window
            .debug_bounds(sel(selector))
            .unwrap_or_else(|| panic!("{selector} is rendered"));
        let physical = physical_window(window);
        assert!(
            fully_inside(bounds, physical),
            "the complete {selector} face is inside the physical window before clicking: {bounds:?} / {physical:?}"
        );
        window.simulate_click(bounds.center(), Modifiers::default());
        window.run_until_parked();
    }

    fn provider_count(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> usize {
        view.read_with(window, |app, _| {
            app.settings.draft.as_ref().unwrap().providers.len()
        })
    }

    fn fully_inside(
        face: gpui::Bounds<gpui::Pixels>,
        viewport: gpui::Bounds<gpui::Pixels>,
    ) -> bool {
        face.left() >= viewport.left()
            && face.right() <= viewport.right()
            && face.top() >= viewport.top()
            && face.bottom() <= viewport.bottom()
    }

    fn intersection(
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

    fn physical_window(window: &mut VisualTestContext) -> gpui::Bounds<gpui::Pixels> {
        let size = window.update(|window, _| window.viewport_size());
        gpui::Bounds::new(gpui::point(px(0.0), px(0.0)), size)
    }

    fn wheel(window: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>, delta_y: f32) {
        wheel_axes(window, at, 0.0, delta_y);
    }

    fn wheel_axes(
        window: &mut VisualTestContext,
        at: gpui::Point<gpui::Pixels>,
        delta_x: f32,
        delta_y: f32,
    ) {
        window.simulate_event(gpui::ScrollWheelEvent {
            position: at,
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(delta_x), px(delta_y))),
            touch_phase: gpui::TouchPhase::Moved,
            ..Default::default()
        });
        window.run_until_parked();
    }

    fn scroll_results_to_edge(
        view: &Entity<AwiliApp>,
        window: &mut VisualTestContext,
        toward_top: bool,
    ) {
        let scale = window.update(|window, _| window.scale_factor());
        let epsilon = px(theme::SCROLL_EDGE_TOLERANCE_PHYSICAL_PX / scale);
        for _ in 0..32 {
            let (offset, max) = view.read_with(window, |app, _| {
                (
                    app.local_discovery.results_scroll.offset().y,
                    app.local_discovery.results_scroll.max_offset().y,
                )
            });
            let at_edge = if toward_top {
                offset >= -epsilon
            } else {
                offset <= -max + epsilon
            };
            if at_edge {
                return;
            }

            let list = window
                .debug_bounds("local-discovery-results")
                .expect("the result list remains rendered while scrolling");
            let panel = window
                .debug_bounds("settings-panel-scroll")
                .expect("the real Settings viewport remains rendered");
            let outer = intersection(panel, physical_window(window))
                .expect("the Settings viewport intersects the physical window");
            let visible_list = intersection(list, outer)
                .expect("the nested result list has a physically exposed scroll region");
            wheel(
                window,
                visible_list.center(),
                if toward_top { 80.0 } else { -80.0 },
            );
        }
        let (offset, max) = view.read_with(window, |app, _| {
            (
                app.local_discovery.results_scroll.offset().y,
                app.local_discovery.results_scroll.max_offset().y,
            )
        });
        panic!(
            "real wheel input did not reach the {} results edge: offset={offset:?}, max={max:?}, epsilon={epsilon:?}",
            if toward_top { "top" } else { "bottom" }
        );
    }

    fn ensure_outer_wheel_room(
        view: &Entity<AwiliApp>,
        window: &mut VisualTestContext,
        delta_sign: f32,
    ) {
        let outer_max = view.read_with(window, |app, _| app.settings_nav_scroll.max_offset().y);
        let scale = window.update(|window, _| window.scale_factor());
        let epsilon = px(theme::SCROLL_EDGE_TOLERANCE_PHYSICAL_PX / scale);
        for _ in 0..8 {
            let offset = view.read_with(window, |app, _| app.settings_nav_scroll.offset().y);
            let available = if delta_sign > 0.0 {
                -offset
            } else {
                offset + outer_max
            };
            if available > epsilon + epsilon {
                return;
            }

            let panel = window
                .debug_bounds("settings-panel-scroll")
                .expect("the Settings viewport remains rendered");
            let list = window.debug_bounds("local-discovery-results");
            let physical = physical_window(window);
            let outer = intersection(panel, physical)
                .expect("the Settings viewport intersects the physical window");
            // Use the exposed strip above the results list when available;
            // it targets the parent scroller without manually changing either
            // ScrollHandle.
            let y = list
                .filter(|list| list.top() > outer.top() + px(2.0))
                .map_or(outer.top() + px(4.0), |list| {
                    outer.top() + (list.top() - outer.top()) / 2.0
                });
            let point = gpui::point(outer.center().x, y);
            assert!(
                list.is_none_or(|list| {
                    point.x < list.left()
                        || point.x > list.right()
                        || point.y < list.top()
                        || point.y > list.bottom()
                }),
                "outer-range setup wheel is outside the nested results hit region: point={point:?}, list={list:?}, outer={outer:?}"
            );
            wheel(window, point, if delta_sign > 0.0 { -80.0 } else { 80.0 });
        }
        let offset = view.read_with(window, |app, _| app.settings_nav_scroll.offset().y);
        let available = if delta_sign > 0.0 {
            -offset
        } else {
            offset + outer_max
        };
        assert!(
            available > epsilon + epsilon,
            "the outer Settings scroller has room for the edge-handoff wheel: offset={offset:?}, max={outer_max:?}, available={available:?}"
        );
    }

    fn wheel_until_visible(
        view: &Entity<AwiliApp>,
        window: &mut VisualTestContext,
        selector: &str,
    ) {
        let mut last_wheel = String::from("no wheel event sent");
        let mut wheel_trace = Vec::new();
        let step = px(80.0);
        let initial_face = window
            .debug_bounds(sel(selector))
            .unwrap_or_else(|| panic!("{selector} remains rendered"));
        let initial_panel = window
            .debug_bounds("settings-panel-scroll")
            .expect("the real Settings form scroll viewport is present");
        let initial_physical = physical_window(window);
        let initial_outer = intersection(initial_panel, initial_physical)
            .expect("the Settings viewport intersects the physical window");
        let initial_inner = selector
            .starts_with("ui-row-local-model-result-")
            .then(|| window.debug_bounds("local-discovery-results"))
            .flatten();
        let initial_inner_viewport =
            initial_inner.and_then(|bounds| intersection(bounds, initial_outer));
        let (inner_max, outer_max) = view.read_with(window, |app, _| {
            (
                app.local_discovery.results_scroll.max_offset().y,
                app.settings_nav_scroll.max_offset().y,
            )
        });
        let (inner_shift, inner_adjusted_face) = initial_inner
            .zip(initial_inner_viewport)
            .filter(|(bounds, _)| !fully_inside(initial_face, *bounds))
            .map_or((px(0.0), initial_face), |(_, viewport)| {
                let shift = if initial_face.top() < viewport.top() {
                    viewport.top() - initial_face.top()
                } else if initial_face.bottom() > viewport.bottom() {
                    viewport.bottom() - initial_face.bottom()
                } else {
                    px(0.0)
                };
                (
                    shift.abs(),
                    gpui::Bounds::new(
                        gpui::point(initial_face.origin.x, initial_face.origin.y + shift),
                        initial_face.size,
                    ),
                )
            });
        let outer_shift = if inner_adjusted_face.top() < initial_outer.top() {
            initial_outer.top() - inner_adjusted_face.top()
        } else if inner_adjusted_face.bottom() > initial_outer.bottom() {
            inner_adjusted_face.bottom() - initial_outer.bottom()
        } else {
            px(0.0)
        };
        let target_distance = inner_shift + outer_shift.abs();
        let measured_scroll_range = inner_max.abs() + outer_max.abs();
        let max_step_count = (f32::from(target_distance) / f32::from(step)).ceil() as usize + 2;
        let range_step_count =
            (f32::from(measured_scroll_range) / f32::from(step)).ceil() as usize + 2;
        let iteration_budget = max_step_count.min(range_step_count).max(1);
        for _ in 0..iteration_budget {
            let face = window
                .debug_bounds(sel(selector))
                .unwrap_or_else(|| panic!("{selector} remains rendered"));
            let panel = window
                .debug_bounds("settings-panel-scroll")
                .expect("the real Settings form scroll viewport is present");
            let physical = physical_window(window);
            let outer = intersection(panel, physical).unwrap_or_else(|| {
                let (
                    outer_handle_bounds,
                    outer_offset,
                    outer_max,
                    page_handle_bounds,
                    page_offset,
                    page_max,
                ) = view.read_with(window, |app, _| {
                    (
                        app.settings_nav_scroll.bounds(),
                        app.settings_nav_scroll.offset(),
                        app.settings_nav_scroll.max_offset(),
                        app.settings_page_scroll.bounds(),
                        app.settings_page_scroll.offset(),
                        app.settings_page_scroll.max_offset(),
                    )
                });
                let mut bounds = |selector: &'static str| window.debug_bounds(selector);
                panic!(
                    "Settings has no visible viewport at scale={:?}; target={face:?}, panel={panel:?}, physical_window={physical:?}, app_panel={:?}, content={:?}, page_scroll_selector={:?}, header={:?}, footer={:?}, status={:?}, settings_nav_scroll=(bounds={outer_handle_bounds:?}, offset={outer_offset:?}, max={outer_max:?}), settings_page_scroll=(bounds={page_handle_bounds:?}, offset={page_offset:?}, max={page_max:?})",
                    theme::ui_scale(),
                    bounds("sx-panel"),
                    bounds("sx-debug-content"),
                    bounds("sx-page"),
                    bounds("sx-debug-header"),
                    bounds("sx-debug-footer"),
                    bounds("sx-status"),
                )
            });
            let inner = selector
                .starts_with("ui-row-local-model-result-")
                .then(|| window.debug_bounds("local-discovery-results"))
                .flatten();
            let inner_viewport = inner.and_then(|bounds| intersection(bounds, outer));
            let (inner_offset, inner_max, outer_offset, outer_max) =
                view.read_with(window, |app, _| {
                    (
                        app.local_discovery.results_scroll.offset(),
                        app.local_discovery.results_scroll.max_offset(),
                        app.settings_nav_scroll.offset(),
                        app.settings_nav_scroll.max_offset(),
                    )
                });
            if fully_inside(face, outer)
                && (inner.is_none()
                    || inner_viewport.is_some_and(|viewport| fully_inside(face, viewport)))
            {
                return;
            }
            // Any positive visible intersection is a real wheel target. This
            // matters at a nested edge: requiring a whole hit target's worth
            // of exposed list would send the wheel to the outer pane even
            // when the selected row needs only a small inner correction.
            let inner_wheel_viewport = inner_viewport;
            let in_inner = inner_wheel_viewport.is_some()
                && inner.is_some_and(|bounds| !fully_inside(face, bounds));
            let outer_target = if inner_viewport.is_none() {
                inner.unwrap_or(face)
            } else {
                face
            };
            let delta_y = if let Some(viewport) = inner_wheel_viewport.filter(|_| in_inner) {
                if face.top() < viewport.top() {
                    f32::from((viewport.top() - face.top()).min(step))
                } else if face.bottom() > viewport.bottom() {
                    -f32::from((face.bottom() - viewport.bottom()).min(step))
                } else {
                    0.0
                }
            } else if outer_target.top() < outer.top() {
                f32::from((outer.top() - outer_target.top()).min(step))
            } else if outer_target.bottom() > outer.bottom() {
                -f32::from((outer_target.bottom() - outer.bottom()).min(step))
            } else {
                0.0
            };
            let at = inner_wheel_viewport.filter(|_| in_inner).map_or_else(
                || gpui::point(panel.left() + px(4.0), panel.center().y),
                |viewport| viewport.center(),
            );
            wheel(window, at, delta_y);
            let (inner_after, outer_after) = view.read_with(window, |app, _| {
                (
                    app.local_discovery.results_scroll.offset(),
                    app.settings_nav_scroll.offset(),
                )
            });
            let face_after = window.debug_bounds(sel(selector));
            assert!(
                inner_after != inner_offset
                    || outer_after != outer_offset
                    || face_after.is_some_and(|after| after != face),
                "the real wheel event makes measured progress toward revealing {selector}: face={face:?}, inner={inner_offset:?}->{inner_after:?}, outer={outer_offset:?}->{outer_after:?}, point={at:?}, delta_y={delta_y}"
            );
            let list_after = window.debug_bounds("local-discovery-results");
            wheel_trace.push(format!(
                "face_y={:?}->{:?} list_y={:?}->{:?} inner_y={:?}->{:?} outer_y={:?}->{:?} delta={delta_y}",
                face.origin.y,
                face_after.map(|bounds| bounds.origin.y),
                inner.map(|bounds| bounds.origin.y),
                list_after.map(|bounds| bounds.origin.y),
                inner_offset.y,
                inner_after.y,
                outer_offset.y,
                outer_after.y,
            ));
            last_wheel = format!(
                "scale={} selector={selector} face={face:?} list={inner:?} inner_viewport={inner_viewport:?} outer={outer:?} window={physical:?} inner_offset={inner_offset:?}->{inner_after:?} inner_max={inner_max:?} outer_offset={outer_offset:?}->{outer_after:?} outer_max={outer_max:?} point={at:?} delta_y={delta_y}",
                crate::theme::ui_scale()
            );
        }
        panic!(
            "real Settings scrolling reveals the full {selector} face; last wheel: {last_wheel}; wheel trace:\n{}",
            wheel_trace.join("\n")
        );
    }

    #[gpui::test]
    fn local_discovery_kit_controls_add_deduplicate_and_cancel(cx: &mut TestAppContext) {
        if !crate::consolidation_tests::isolated(
            "local_discovery_ui::rendered_tests::local_discovery_kit_controls_add_deduplicate_and_cancel",
        ) {
            return;
        }
        cx.executor().allow_parking();
        crate::theme::set_ui_scale(1.0);
        let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
        window.simulate_resize(size(px(1280.0), px(900.0)));
        view.update(window, |app, cx| {
            app.onboarding = crate::onboarding::Onboarding::existing();
            let doc = awili_config::ConfigDoc::networkless().unwrap();
            app.settings.open = true;
            app.settings.tab = settings::Tab::Form;
            app.settings.doc = Some(doc.clone());
            app.settings.draft = Some(settings::Draft::from_doc(&doc));
            app.settings_nav_focus_pending = false;
            app.settings_nav_scroll = ScrollHandle::new();
            app.settings_nav_scroll.scroll_to_item(7 + 1);
            app.settings
                .draft
                .as_mut()
                .unwrap()
                .providers
                .push(settings::ProviderDraft {
                    name: "Manual endpoint".into(),
                    kind: awili_config::ProviderKind::Ollama,
                    base_url: "http://127.0.0.1:11434/".into(),
                    models: "manual-model".into(),
                    enabled: false,
                    api_key: Some("keep-existing-key".into()),
                });
            app.reveal_local_discovery();
            cx.notify();
        });
        window.run_until_parked();
        // LocalDiscovery is mounted on the live Models & Fleet page, not the
        // default General page. Navigate through the real Settings sidebar.
        click(window, "sx-nav-3");
        assert_eq!(
            view.read_with(window, |app, _| app.settings.page),
            settings_catalog::Page::Models
        );
        assert!(window.debug_bounds("ui-button-scan-local-models").is_some());

        view.update(window, |app, cx| {
            app.local_discovery.running = true;
            app.local_discovery.generation = 1;
            app.local_discovery.cancel = Arc::new(AtomicBool::new(false));
            app.local_discovery.progress = (3, 12);
            app.local_discovery.selected = 0;
            app.local_discovery.report = Some(local_discovery::DiscoveryReport {
                results: std::iter::once(local_discovery::DiscoveryResult {
                        base_url: "http://127.0.0.1:11434".into(),
                        kind: awili_config::ProviderKind::Ollama,
                        models: vec![local_discovery::DiscoveryModel {
                            id: "existing-model".into(),
                            loaded: None,
                        }],
                    })
                    .chain(std::iter::once(local_discovery::DiscoveryResult {
                        base_url: "http://127.0.0.1:1234/v1".into(),
                        kind: awili_config::ProviderKind::OpenAi,
                        models: vec![
                            local_discovery::DiscoveryModel {
                                id: "small-model".into(),
                                loaded: None,
                            },
                            local_discovery::DiscoveryModel {
                                id: "large-model".into(),
                                loaded: None,
                            },
                        ],
                    }))
                    .chain((2..9).map(|index| local_discovery::DiscoveryResult {
                        base_url: format!(
                            "http://192.168.1.{index}:1234/v1/with-a-deliberately-long-discovery-endpoint-segment-{index}"
                        ),
                        kind: awili_config::ProviderKind::OpenAi,
                        models: vec![local_discovery::DiscoveryModel {
                            id: format!("long-discovered-model-name-{index}-that-wraps-in-the-row"),
                            loaded: None,
                        }],
                    }))
                    .collect(),
                cancelled: true,
                ..Default::default()
            });
            app.reveal_local_discovery();
            cx.notify();
        });
        window.run_until_parked();

        let scan = window
            .debug_bounds("ui-button-scan-local-models")
            .expect("the scanning control is rendered");
        let cancel = window
            .debug_bounds("ui-button-cancel-local-discovery")
            .expect("the cancellation control is rendered");
        let target = f32::from(crate::theme::target());
        for (name, bounds) in [("scan", scan), ("cancel", cancel)] {
            assert!(
                f32::from(bounds.size.width) >= target && f32::from(bounds.size.height) >= target,
                "{name} retains a KIT target-sized face: {bounds:?}"
            );
        }
        assert!(window
            .debug_bounds("ui-banner-local-discovery-status")
            .is_some());
        assert!(window.debug_bounds("local-discovery-results").is_some());
        wheel_until_visible(&view, window, "ui-button-cancel-local-discovery");
        let cancel_face = window
            .debug_bounds("ui-button-cancel-local-discovery")
            .unwrap();
        let settings_viewport = intersection(
            window.debug_bounds("settings-panel-scroll").unwrap(),
            physical_window(window),
        )
        .expect("visible Settings form viewport");
        assert!(
            fully_inside(cancel_face, settings_viewport),
            "the complete Cancel face is reachable inside the Settings and physical viewports before clicking: {cancel_face:?} / {settings_viewport:?}"
        );
        click(window, "ui-button-cancel-local-discovery");
        assert!(view.read_with(window, |app, _| app
            .local_discovery
            .cancel
            .load(Ordering::Relaxed)));

        wheel_until_visible(&view, window, "ui-row-local-model-result-1");
        click(window, "ui-row-local-model-result-1");
        assert_eq!(provider_count(&view, window), 2);
        view.read_with(window, |app, _| {
            let added = &app.settings.draft.as_ref().unwrap().providers[1];
            assert_eq!(added.name, "Local 127.0.0.1:1234/v1");
            assert_eq!(added.kind, awili_config::ProviderKind::OpenAi);
            assert_eq!(added.base_url, "http://127.0.0.1:1234/v1");
            assert_eq!(added.models, "small-model, large-model");
            assert!(added.enabled);
            assert_eq!(added.api_key, None);
            assert_eq!(
                app.settings.draft.as_ref().unwrap().providers[0]
                    .api_key
                    .as_deref(),
                Some("keep-existing-key"),
                "the duplicate URL never overwrites a manually configured endpoint or key"
            );
        });
        wheel_until_visible(&view, window, "ui-row-local-model-result-0");
        click(window, "ui-row-local-model-result-0");
        assert_eq!(
            provider_count(&view, window),
            2,
            "trailing-slash duplicate is a no-op"
        );

        for scale in [0.9_f32, 1.0, 1.25, 2.5] {
            crate::theme::set_ui_scale(scale);
            window.simulate_resize(size(px(900.0), px(712.0)));
            view.update(window, |app, cx| {
                app.reveal_local_discovery();
                cx.notify();
            });
            window.run_until_parked();
            let selected = view.read_with(window, |app, _| app.local_discovery.selected);
            if selected == 0 {
                // The focused result is the one that production reveal keeps
                // visible during manual scrolling. Move selection to row 8
                // through the actual wrapping key path before its wheel test.
                window.simulate_keystrokes("up");
                window.run_until_parked();
            }
            assert_eq!(
                view.read_with(window, |app, _| app.local_discovery.selected),
                8,
                "the rendered keyboard route selects the last result before the wheel test at {scale}×"
            );
            // Settle any deferred focus reveal and physically expose the
            // result before the separate one-event nested-scroll proof.
            wheel_until_visible(&view, window, "ui-row-local-model-result-8");
            let (before_inner_wheel, inner_max, before_outer_wheel, outer_max) =
                view.read_with(window, |app, _| {
                    (
                        app.local_discovery.results_scroll.offset(),
                        app.local_discovery.results_scroll.max_offset(),
                        app.settings_nav_scroll.offset(),
                        app.settings_nav_scroll.max_offset(),
                    )
                });
            assert!(
                inner_max.y > px(1.0),
                "the nine-result list has real vertical scroll range at {scale}×: {inner_max:?}"
            );
            let list = window.debug_bounds("local-discovery-results").unwrap();
            let panel = window.debug_bounds("settings-panel-scroll").unwrap();
            let outer = intersection(panel, physical_window(window))
                .expect("the real Settings viewport is visible before nested wheel input");
            let physical = physical_window(window);
            let face = window.debug_bounds("ui-row-local-model-result-8").unwrap();
            let inner_viewport = intersection(list, outer).unwrap_or_else(|| {
                panic!(
                    "the real result-list viewport is physically exposed before nested wheel input at {scale}×: face={face:?}, list={list:?}, outer={outer:?}, physical={physical:?}, inner_offset={before_inner_wheel:?}, inner_max={inner_max:?}"
                )
            });
            assert!(
                fully_inside(face, inner_viewport),
                "nested wheel proof starts with the whole selected result physically visible at {scale}×: {face:?} / {inner_viewport:?}"
            );
            let (available_inner_range, delta_sign) =
                if before_inner_wheel.y > -inner_max.y + px(1.0) {
                    (before_inner_wheel.y + inner_max.y, -1.0_f32)
                } else {
                    (-before_inner_wheel.y, 1.0_f32)
                };
            assert!(
                available_inner_range > px(1.0),
                "the chosen inner-wheel direction has measurable remaining range at {scale}×: offset={before_inner_wheel:?}, max={inner_max:?}, available={available_inner_range:?}"
            );
            let delta_magnitude = available_inner_range.min(px(24.0)) / 2.0;
            let delta_y = delta_sign * f32::from(delta_magnitude);
            let edge_epsilon = px(theme::SCROLL_EDGE_TOLERANCE_PHYSICAL_PX / scale);
            assert!(
                delta_magnitude > px(0.0)
                    && delta_magnitude + edge_epsilon < available_inner_range,
                "the real wheel request stays strictly inside the inner range by more than half a physical pixel at {scale}×: request={delta_magnitude:?}, available={available_inner_range:?}, epsilon={edge_epsilon:?}"
            );
            let available_outer_range = if delta_sign > 0.0 {
                -before_outer_wheel.y
            } else {
                before_outer_wheel.y + outer_max.y
            };
            assert!(
                available_outer_range > delta_magnitude + edge_epsilon,
                "the outer Settings scroll has room for the wheel too, so its unchanged offset proves inner-event ownership at {scale}×: offset={before_outer_wheel:?}, max={outer_max:?}, available={available_outer_range:?}, request={delta_magnitude:?}, epsilon={edge_epsilon:?}"
            );
            // Move in a direction with measured remaining range. At the list's
            // lower limit, a negative wheel is correctly ignored by GPUI, so
            // send a real opposite-direction event from the same exposed hit
            // region instead of treating saturation as a broken scroll.
            wheel(window, inner_viewport.center(), delta_y);
            let (after_inner_wheel, after_outer_wheel) = view.read_with(window, |app, _| {
                (
                    app.local_discovery.results_scroll.offset(),
                    app.settings_nav_scroll.offset(),
                )
            });
            assert_ne!(
                after_inner_wheel, before_inner_wheel,
                "real wheel input at the visible nested result-list intersection moves its ScrollHandle at {scale}×"
            );
            assert_eq!(
                after_outer_wheel, before_outer_wheel,
                "an inner-list wheel smaller than its measured remaining range is consumed without moving the Settings outer scroll at {scale}×: inner={before_inner_wheel:?}->{after_inner_wheel:?}, max={inner_max:?}, available={available_inner_range:?}, request={delta_y}, outer={before_outer_wheel:?}->{after_outer_wheel:?}"
            );

            let (before_x_inner, x_inner_max, before_x_outer, x_outer_max) =
                view.read_with(window, |app, _| {
                    (
                        app.local_discovery.results_scroll.offset().y,
                        app.local_discovery.results_scroll.max_offset().y,
                        app.settings_nav_scroll.offset().y,
                        app.settings_nav_scroll.max_offset().y,
                    )
                });
            let x_sign = delta_sign;
            let x_available = if x_sign > 0.0 {
                -before_x_inner
            } else {
                before_x_inner + x_inner_max
            };
            let x_outer_available = if x_sign > 0.0 {
                -before_x_outer
            } else {
                before_x_outer + x_outer_max
            };
            let x_magnitude = x_available.min(px(16.0)) / 2.0;
            assert!(
                x_magnitude > edge_epsilon
                    && x_magnitude + edge_epsilon < x_available
                    && x_magnitude + edge_epsilon < x_outer_available,
                "x-only wheel proof starts with inner and outer range beyond the half-physical-pixel tolerance at {scale}×: inner={before_x_inner:?}, max={x_inner_max:?}, available={x_available:?}, outer={before_x_outer:?}, outer_max={x_outer_max:?}, outer_available={x_outer_available:?}, request={x_magnitude:?}, epsilon={edge_epsilon:?}"
            );
            let x_list = window.debug_bounds("local-discovery-results").unwrap();
            let x_panel = window.debug_bounds("settings-panel-scroll").unwrap();
            let x_outer_viewport = intersection(x_panel, physical_window(window)).unwrap();
            let x_inner_viewport = intersection(x_list, x_outer_viewport).unwrap();
            wheel_axes(
                window,
                x_inner_viewport.center(),
                x_sign * f32::from(x_magnitude),
                0.0,
            );
            let (after_x_inner, after_x_outer) = view.read_with(window, |app, _| {
                (
                    app.local_discovery.results_scroll.offset().y,
                    app.settings_nav_scroll.offset().y,
                )
            });
            assert_ne!(
                after_x_inner, before_x_inner,
                "GPUI's x-only-to-y axis remapping moves the inner result list at {scale}×"
            );
            assert_eq!(
                after_x_outer, before_x_outer,
                "the x-only event is consumed by the inner result list while both scrollers have room at {scale}×: inner={before_x_inner:?}->{after_x_inner:?}, outer={before_x_outer:?}->{after_x_outer:?}"
            );
            wheel_until_visible(&view, window, "ui-row-local-model-result-8");
            for selector in [
                "ui-button-scan-local-models",
                "ui-button-cancel-local-discovery",
                "ui-row-local-model-result-8",
            ] {
                wheel_until_visible(&view, window, selector);
                let face = window.debug_bounds(sel(selector)).unwrap();
                let target = f32::from(crate::theme::target());
                assert!(
                    f32::from(face.size.width) >= target && f32::from(face.size.height) >= target,
                    "{selector} keeps its full target at scale {scale}: {face:?}"
                );
                let panel = window.debug_bounds("settings-panel-scroll").unwrap();
                let physical = physical_window(window);
                let outer = intersection(panel, physical).expect("visible Settings viewport");
                if selector == "ui-row-local-model-result-8" {
                    let list = window.debug_bounds("local-discovery-results").unwrap();
                    let viewport = intersection(list, outer)
                        .expect("result list intersects Settings and physical window");
                    assert!(
                        fully_inside(face, viewport),
                        "the complete result face is inside its inner list, Settings viewport, and physical window at {scale}×: face={face:?}, list={list:?}, outer={outer:?}"
                    );
                } else {
                    assert!(
                        fully_inside(face, outer),
                        "the complete control face is inside Settings and the physical window at {scale}×: face={face:?}, outer={outer:?}"
                    );
                }
            }

            // Re-enter through the rendered result's actual pointer handler,
            // which transfers focus to discovery just as a user does. The
            // Settings navigation root must not still own focus or have a
            // pending restore that can consume the first Down event.
            wheel_until_visible(&view, window, "ui-row-local-model-result-0");
            let first = window.debug_bounds("ui-row-local-model-result-0").unwrap();
            let list = window.debug_bounds("local-discovery-results").unwrap();
            let panel = window.debug_bounds("settings-panel-scroll").unwrap();
            let outer = intersection(panel, physical_window(window)).unwrap();
            let viewport = intersection(list, outer).unwrap();
            assert!(
                fully_inside(first, viewport),
                "the actual first result is visible before it transfers focus: {first:?} / {viewport:?}"
            );
            click(window, "ui-row-local-model-result-0");
            let (focus, nav_focus, pending, start) = view.read_with(window, |app, _| {
                (
                    app.local_discovery.focus.clone(),
                    app.settings_nav_focus.clone(),
                    app.settings_nav_focus_pending,
                    app.local_discovery.selected,
                )
            });
            assert_eq!(
                start, 0,
                "the clicked result is the keyboard starting point"
            );
            assert!(!pending, "Settings has no pending focus restoration");
            let (discovery_focused, nav_focused) =
                window.update(|window, _| (focus.is_focused(window), nav_focus.is_focused(window)));
            assert!(
                discovery_focused,
                "the real row click transfers focus to discovery"
            );
            assert!(!nav_focused, "Settings navigation no longer owns focus");

            // Advance and park each actual key event so focus routing or an
            // owner stealing one event is diagnosed at the exact step.
            for expected in 1..=8 {
                window.simulate_keystrokes("down");
                window.run_until_parked();
                let selected = view.read_with(window, |app, _| app.local_discovery.selected);
                assert_eq!(
                    selected, expected,
                    "each Down reaches discovery at {scale}× (expected {expected}, got {selected})"
                );
            }
            let last = window.debug_bounds("ui-row-local-model-result-8").unwrap();
            let list = window.debug_bounds("local-discovery-results").unwrap();
            let panel = window.debug_bounds("settings-panel-scroll").unwrap();
            let outer = intersection(panel, physical_window(window)).unwrap();
            let viewport = intersection(list, outer).unwrap();
            assert!(
                fully_inside(last, viewport),
                "keyboard selection reveals the complete last result in all nested physical viewports at {scale}×: face={last:?}, list={list:?}, outer={outer:?}"
            );

            // Exercise real wheel handoff at both inner-list edges. The
            // selected result is aligned with the edge under test; both the
            // inner and parent ScrollHandles reach their positions through
            // actual wheel events only.
            for (selector, toward_top, edge_delta) in [
                ("ui-row-local-model-result-0", true, 48.0_f32),
                ("ui-row-local-model-result-8", false, -48.0_f32),
            ] {
                wheel_until_visible(&view, window, selector);
                click(window, selector);
                let selected = view.read_with(window, |app, _| app.local_discovery.selected);
                assert_eq!(
                    selected,
                    if toward_top { 0 } else { 8 },
                    "the actual pointer action selects the result whose inner edge is being tested at {scale}×"
                );
                scroll_results_to_edge(&view, window, toward_top);
                ensure_outer_wheel_room(&view, window, edge_delta.signum());

                let (inner_before, inner_max, outer_before, outer_max) =
                    view.read_with(window, |app, _| {
                        (
                            app.local_discovery.results_scroll.offset().y,
                            app.local_discovery.results_scroll.max_offset().y,
                            app.settings_nav_scroll.offset().y,
                            app.settings_nav_scroll.max_offset().y,
                        )
                    });
                let epsilon = px(theme::SCROLL_EDGE_TOLERANCE_PHYSICAL_PX / scale);
                let at_edge = if toward_top {
                    inner_before >= -epsilon
                } else {
                    inner_before <= -inner_max + epsilon
                };
                assert!(
                    at_edge,
                    "real wheel input reached the tested {} inner edge at {scale}×: offset={inner_before:?}, max={inner_max:?}, epsilon={epsilon:?}",
                    if toward_top { "top" } else { "bottom" }
                );
                let outer_available = if edge_delta > 0.0 {
                    -outer_before
                } else {
                    outer_before + outer_max
                };
                let edge_request = px(edge_delta.abs());
                assert!(
                    outer_available > edge_request + epsilon,
                    "the outer scroll has room to receive an edge wheel at {scale}×: offset={outer_before:?}, max={outer_max:?}, available={outer_available:?}, request={edge_delta}"
                );

                let list = window.debug_bounds("local-discovery-results").unwrap();
                let panel = window.debug_bounds("settings-panel-scroll").unwrap();
                let physical = physical_window(window);
                let outer_viewport = intersection(panel, physical).unwrap();
                let inner_viewport = intersection(list, outer_viewport).unwrap();
                let face = window.debug_bounds(sel(selector)).unwrap();
                assert!(
                    fully_inside(face, inner_viewport)
                        && fully_inside(face, physical)
                        && fully_inside(inner_viewport, physical),
                    "the full target and actual nested wheel hit region are physically visible before edge handoff at {scale}×: face={face:?}, inner={inner_viewport:?}, outer={outer_viewport:?}, physical={physical:?}"
                );

                // Move a few physical pixels away from the edge with actual
                // wheel input. This direction has room inside the list, so
                // the inner list consumes it and the parent stays put. Then
                // send a larger opposite event that starts with inner room
                // but crosses the clamp; it must also stay with the inner list.
                let away_sign = -edge_delta.signum();
                let away_magnitude = px(4.0 / scale);
                wheel(
                    window,
                    inner_viewport.center(),
                    away_sign * f32::from(away_magnitude),
                );
                let (inner_away, outer_away) = view.read_with(window, |app, _| {
                    (
                        app.local_discovery.results_scroll.offset().y,
                        app.settings_nav_scroll.offset().y,
                    )
                });
                assert!(
                    if toward_top {
                        inner_away < inner_before - epsilon
                            && outer_away == outer_before
                    } else {
                        inner_away > inner_before + epsilon
                            && outer_away == outer_before
                    },
                    "a real wheel moving away from the inner edge is consumed by the inner list while the selected edge row remains aligned at {scale}×: inner={inner_before:?}->{inner_away:?}, outer={outer_before:?}->{outer_away:?}"
                );

                ensure_outer_wheel_room(&view, window, edge_delta.signum());
                let list = window.debug_bounds("local-discovery-results").unwrap();
                let panel = window.debug_bounds("settings-panel-scroll").unwrap();
                let physical = physical_window(window);
                let outer_viewport = intersection(panel, physical).unwrap();
                let inner_viewport = intersection(list, outer_viewport).unwrap();
                let face = window.debug_bounds(sel(selector)).unwrap();
                let crossing_point = inner_viewport.center();
                assert!(
                    crossing_point.x >= inner_viewport.left()
                        && crossing_point.x <= inner_viewport.right()
                        && crossing_point.y >= inner_viewport.top()
                        && crossing_point.y <= inner_viewport.bottom()
                        && fully_inside(inner_viewport, physical),
                    "the crossing wheel targets a point inside the actual nested list intersection within the physical window at {scale}×: point={crossing_point:?}, face={face:?}, inner={inner_viewport:?}, outer={outer_viewport:?}, physical={physical:?}"
                );
                let (before_cross_inner, before_cross_outer) = view.read_with(window, |app, _| {
                    (
                        app.local_discovery.results_scroll.offset().y,
                        app.settings_nav_scroll.offset().y,
                    )
                });
                let starts_inside = if toward_top {
                    before_cross_inner < -epsilon
                } else {
                    before_cross_inner > -inner_max + epsilon
                };
                assert!(
                    starts_inside,
                    "the crossing wheel starts with measurable inner room at {scale}×: offset={before_cross_inner:?}, max={inner_max:?}, epsilon={epsilon:?}"
                );
                let crossing_delta =
                    edge_delta.signum() * f32::from(away_magnitude + away_magnitude);
                wheel(window, crossing_point, crossing_delta);
                let (after_cross_inner, after_cross_outer) = view.read_with(window, |app, _| {
                    (
                        app.local_discovery.results_scroll.offset().y,
                        app.settings_nav_scroll.offset().y,
                    )
                });
                assert!(
                    if toward_top {
                        after_cross_inner >= -epsilon
                    } else {
                        after_cross_inner <= -inner_max + epsilon
                    },
                    "the crossing wheel reaches the inner {} edge at {scale}×: offset={before_cross_inner:?}->{after_cross_inner:?}, max={inner_max:?}, request={crossing_delta}, epsilon={epsilon:?}",
                    if toward_top { "top" } else { "bottom" }
                );
                assert_eq!(
                    after_cross_outer, before_cross_outer,
                    "a wheel that starts with inner room is consumed even when its raw delta crosses the {} edge at {scale}×: inner={before_cross_inner:?}->{after_cross_inner:?}, outer={before_cross_outer:?}->{after_cross_outer:?}",
                    if toward_top { "top" } else { "bottom" }
                );

                ensure_outer_wheel_room(&view, window, edge_delta.signum());
                let list = window.debug_bounds("local-discovery-results").unwrap();
                let panel = window.debug_bounds("settings-panel-scroll").unwrap();
                let physical = physical_window(window);
                let outer_viewport = intersection(panel, physical).unwrap();
                let inner_viewport = intersection(list, outer_viewport).unwrap();
                let face = window.debug_bounds(sel(selector)).unwrap();
                let exact_edge_point = inner_viewport.center();
                assert!(
                    exact_edge_point.x >= inner_viewport.left()
                        && exact_edge_point.x <= inner_viewport.right()
                        && exact_edge_point.y >= inner_viewport.top()
                        && exact_edge_point.y <= inner_viewport.bottom()
                        && fully_inside(inner_viewport, physical),
                    "the exact-edge wheel targets a point inside the actual nested list intersection within the physical window at {scale}×: point={exact_edge_point:?}, face={face:?}, inner={inner_viewport:?}, outer={outer_viewport:?}, physical={physical:?}"
                );

                wheel(window, exact_edge_point, edge_delta);
                let (inner_after, outer_after) = view.read_with(window, |app, _| {
                    (
                        app.local_discovery.results_scroll.offset().y,
                        app.settings_nav_scroll.offset().y,
                    )
                });
                assert!(
                    (inner_after - inner_before).abs() <= epsilon,
                    "the inner list remains clamped at its {} edge within half a physical pixel at {scale}×: {inner_before:?}->{inner_after:?}, max={inner_max:?}, epsilon={epsilon:?}",
                    if toward_top { "top" } else { "bottom" }
                );
                assert!(
                    if edge_delta > 0.0 {
                        outer_after > outer_before
                    } else {
                        outer_after < outer_before
                    },
                    "the same real edge wheel reaches the parent Settings scroll at {scale}×: inner={inner_before:?}->{inner_after:?}, outer={outer_before:?}->{outer_after:?}"
                );
            }
        }

        view.update(window, |app, cx| {
            app.local_discovery.running = false;
            app.local_discovery.report = Some(local_discovery::DiscoveryReport::default());
            cx.notify();
        });
        window.run_until_parked();
        assert!(window
            .debug_bounds("ui-empty-state-local-discovery-empty")
            .is_some());
        crate::theme::set_ui_scale(1.0);
    }
}
