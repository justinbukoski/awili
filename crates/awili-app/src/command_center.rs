//! Command Center view host (plan unit 21, Phase E).
//!
//! Unit 20 built the gateway-side `/api/status` decoder
//! ([`awili_gateway::rest::SystemStatus`]) and the `/api/analytics/usage`
//! condenser ([`awili_gateway::rest::UsageRollup`]); the unit-21 usage bar chart
//! ([`crate::usage_chart::usage_chart`]) renders the rollup. This module is the
//! Command Center panel that *hosts* those pieces: a system-status header
//! (gateway health pill + a labelled key/value grid derived from
//! [`SystemStatus`]) sitting above the usage chart, plus the deferred action
//! affordances.
//!
//! Following the unit-19/unit-21 convention, the data shaping is factored into
//! pure, `#[test]`-covered helpers ([`gateway_health`], [`config_summary`],
//! [`connectors_summary`], [`system_status_rows`]) — the decoded [`SystemStatus`]
//! is the single source of truth and the panel's copy is verifiable without a
//! running gateway. The render function is a thin, side-effect-free consumer of
//! that shaping and of [`usage_chart`].
//!
//! The **Restart-Gateway** and **Update-Hermes** actions are **[LIVE]**: their
//! `.on_click` spawns a blocking [`RestClient`](awili_gateway::rest::RestClient)
//! verb (`restart_gateway` / `update_hermes`) off the UI thread, which only
//! means anything against a live server. The [`command_center`] render now
//! attaches those clicks — the caller passes two `cx.listener` handlers that
//! land in [`crate::AwiliApp::restart_gateway_action`] /
//! [`crate::AwiliApp::update_hermes_action`], which drive the guarded reducer
//! ([`CommandCenterState::begin_restart`]/[`CommandCenterState::begin_update`]),
//! spawn the verb off-thread, then settle the chip and re-fetch. The gating and
//! chip copy stay pure and `#[test]`-covered: [`update_affordance`] (gates
//! *Update Hermes* on the `GET /api/hermes/update/check` result: visible on
//! `update_available`, clickable only when the gateway `can_apply` it, captioned
//! "N commits behind" plus a manual-apply hint) and [`action_chip_label`] (the
//! Idle/Running/Done/Failed chip copy the worker flips through) — alongside the
//! status + usage visualization.

use awili_gateway::rest::HermesUpdateCheck;
use awili_gateway::rest::SystemStatus;
use awili_gateway::rest::UsageRollup;
use gpui::{div, prelude::*, FontWeight, SharedString};

use crate::theme;
use crate::usage_chart::usage_chart;

/// Visual tone for the gateway-health pill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthTone {
    /// Gateway running and idle — accent/OK.
    Ok,
    /// Running but busy — warn.
    Warn,
    /// Not running — critical.
    Down,
}

/// The gateway-health pill model: a short human label plus its [`HealthTone`],
/// derived purely from [`SystemStatus`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayHealth {
    pub label: String,
    pub tone: HealthTone,
}

/// One labelled row in the status grid (`label: value`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusRow {
    pub label: String,
    pub value: String,
}

/// Classify the gateway's health for the header pill.
///
/// A stopped gateway is [`HealthTone::Down`] (its `gateway_state`, if any, is
/// surfaced verbatim, else `"Stopped"`); a running-but-`gateway_busy` gateway is
/// [`HealthTone::Warn`]; an otherwise-running gateway is [`HealthTone::Ok`].
///
/// Note `gateway_drainable` is deliberately *not* a health input: it reports
/// that the running gateway *can* be drained for a restart (the canonical live
/// status has `gateway_drainable: true` on a healthy `"running"` gateway), so it
/// drives the restart affordance, not the pill.
pub fn gateway_health(status: &SystemStatus) -> GatewayHealth {
    if !status.gateway_running {
        let label = status
            .gateway_state
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(title_case)
            .unwrap_or_else(|| "Stopped".to_string());
        return GatewayHealth {
            label,
            tone: HealthTone::Down,
        };
    }
    if status.gateway_busy {
        return GatewayHealth {
            label: "Busy".to_string(),
            tone: HealthTone::Warn,
        };
    }
    GatewayHealth {
        label: "Running".to_string(),
        tone: HealthTone::Ok,
    }
}

/// Summarise the config-version row: the text (e.g. `"v7"` or `"v7 → v9"`) and
/// whether an update is available. An update is available when Hermes reports
/// `can_update_hermes`, or when a `latest_config_version` exceeds the running
/// `config_version`.
pub fn config_summary(status: &SystemStatus) -> (String, bool) {
    let cur = status.config_version;
    let latest = status.latest_config_version;
    let behind = matches!((cur, latest), (Some(c), Some(l)) if l > c);
    let update = status.can_update_hermes || behind;
    let text = match (cur, latest) {
        (Some(c), Some(l)) if l > c => format!("v{c} → v{l}"),
        (Some(c), _) => format!("v{c}"),
        (None, Some(l)) => format!("v{l}"),
        (None, None) => "unknown".to_string(),
    };
    (text, update)
}

/// The display model for the **Update Hermes** [LIVE] affordance, derived
/// purely from a `GET /api/hermes/update/check` result
/// ([`HermesUpdateCheck`]). This is the code-verifiable half of the deferred
/// action: the async worker that actually POSTs `/api/hermes/update` consumes
/// `enabled` to decide whether the click does anything, and renders `detail`.
///
/// Per the plan (unit 21, Phase E): the chip is *shown* when
/// `update_available`, but only *clickable* when the gateway can apply it in
/// place (`can_apply` — true for git installs, false for e.g. a pip install
/// that reports an update yet needs a manual command).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateAffordance {
    /// Render the chip at all (an update is available).
    pub visible: bool,
    /// The chip is live-clickable (`update_available && can_apply`).
    pub enabled: bool,
    /// Sub-caption: "N commits behind", and/or a manual-apply hint when an
    /// update is available but the gateway cannot apply it itself.
    pub detail: Option<String>,
}

/// Shape the **Update Hermes** affordance from an optional update check.
///
/// `None` (no check fetched yet) yields a hidden, disabled chip with no detail
/// — the panel shows nothing until the worker has polled. Once a check is in
/// hand the chip is visible iff `update_available`, clickable iff additionally
/// `can_apply`, and captioned with how far behind the install is plus, when it
/// can't self-apply, the manual command (or install method) to run. `detail` is
/// a caption *for the chip*, so it is `None` whenever the chip is not visible.
pub fn update_affordance(check: Option<&HermesUpdateCheck>) -> UpdateAffordance {
    let Some(c) = check else {
        return UpdateAffordance {
            visible: false,
            enabled: false,
            detail: None,
        };
    };
    let visible = c.update_available;
    let enabled = c.update_available && c.can_apply;

    // The caption belongs to the chip; a hidden chip carries no detail.
    if !visible {
        return UpdateAffordance {
            visible,
            enabled,
            detail: None,
        };
    }

    let mut parts: Vec<String> = Vec::new();
    match c.behind {
        1 => parts.push("1 commit behind".to_string()),
        n if n > 1 => parts.push(format!("{n} commits behind")),
        _ => {}
    }
    // Update available but the gateway can't apply it in place: tell the user
    // how (the gateway's own `update_command`, else the install method).
    if !enabled {
        let hint = c
            .update_command
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|cmd| format!("update manually: {cmd}"))
            .or_else(|| {
                c.install_method
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(|m| format!("{m} install — apply manually"))
            })
            .unwrap_or_else(|| "cannot apply automatically".to_string());
        parts.push(hint);
    }

    let detail = if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    };
    UpdateAffordance {
        visible,
        enabled,
        detail,
    }
}

/// Lifecycle of a [LIVE] Command Center action (Restart Gateway / Update
/// Hermes) as its spawned worker progresses. The async task flips this on the
/// UI entity; the pure [`action_chip_label`] reducer turns it into chip copy so
/// the visual states are testable without a running gateway.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ActionPhase {
    /// Idle — ready to fire.
    #[default]
    Idle,
    /// Worker in flight.
    Running,
    /// Worker returned `Ok(())`.
    Done,
    /// Worker returned an error.
    Failed,
}

/// The chip label for an action given its `base` name and lifecycle
/// [`ActionPhase`]: an ellipsis while running, a check when done, a cross on
/// failure. Pure so the [LIVE] worker's visible states are verifiable.
pub fn action_chip_label(base: &str, phase: ActionPhase) -> String {
    match phase {
        ActionPhase::Idle => base.to_string(),
        ActionPhase::Running => format!("{base}…"),
        ActionPhase::Done => format!("{base} ✓"),
        ActionPhase::Failed => format!("{base} ✗"),
    }
}

/// Mutable action state for the [LIVE] Command Center — the pure reducer the
/// GPUI entity will hold and the async workers will drive. It carries the two
/// independent action lifecycles ([`ActionPhase`] for *Restart Gateway* and
/// *Update Hermes*) plus the last-fetched [`HermesUpdateCheck`] that gates the
/// update action ([`update_affordance`]).
///
/// Splitting this out keeps the click → spawn → settle transitions verifiable
/// without a GPUI context or a live gateway: the entity becomes a thin shell
/// that mutates this on click / on worker completion and re-renders through
/// [`command_center`], which already consumes exactly these three fields. The
/// transitions are deliberately guarded — a `begin_*` that returns `false` means
/// "spawn nothing" (an in-flight action, or an update the gateway can't apply),
/// and a `finish_*` on a non-running phase is ignored so a stray/duplicate
/// completion can't flip a fresh chip to Done/Failed.
#[derive(Clone, Debug, Default)]
pub struct CommandCenterState {
    /// Lifecycle of the Restart Gateway action.
    pub restart_phase: ActionPhase,
    /// Lifecycle of the Update Hermes action.
    pub update_phase: ActionPhase,
    /// Latest `GET /api/hermes/update/check` result gating the update action.
    pub update_check: Option<HermesUpdateCheck>,
}

impl CommandCenterState {
    /// Store the latest update check (from a `check_hermes_update` fetch),
    /// re-gating the Update Hermes action via [`update_affordance`].
    pub fn set_update_check(&mut self, check: Option<HermesUpdateCheck>) {
        self.update_check = check;
    }

    /// Whether Update Hermes is currently actionable — an update is available
    /// *and* the gateway can apply it in place. Mirrors [`update_affordance`]
    /// over the held check.
    pub fn update_actionable(&self) -> bool {
        let aff = update_affordance(self.update_check.as_ref());
        aff.visible && aff.enabled
    }

    /// Begin the Restart Gateway worker. Returns `true` if the caller should
    /// spawn the blocking `restart_gateway` verb, `false` when a restart is
    /// already in flight — so a double-click can't launch two workers. On
    /// `true` the phase moves to [`ActionPhase::Running`].
    pub fn begin_restart(&mut self) -> bool {
        if self.restart_phase == ActionPhase::Running {
            return false;
        }
        self.restart_phase = ActionPhase::Running;
        true
    }

    /// Settle the Restart Gateway worker with its `Ok`/`Err` result. Ignored
    /// unless a restart is actually running, so a late completion can't flip a
    /// fresh Idle chip.
    pub fn finish_restart(&mut self, ok: bool) {
        if self.restart_phase == ActionPhase::Running {
            self.restart_phase = if ok {
                ActionPhase::Done
            } else {
                ActionPhase::Failed
            };
        }
    }

    /// Begin the Update Hermes worker. Returns `true` only when the action is
    /// [`update_actionable`](Self::update_actionable), not already running, and
    /// no gateway refresh is in flight (`refreshing`) — so a click on a
    /// hidden/dimmed chip, a double-click, or a click during the post-update
    /// re-fetch all spawn nothing. On `true` the phase moves to
    /// [`ActionPhase::Running`].
    ///
    /// The `refreshing` guard closes a race the other checks can't: after a
    /// successful update, `finish_update` moves the phase to `Done` while the
    /// held [`update_check`](Self::update_check) still reads actionable, and only
    /// the following refresh replaces it with the now-current check. During that
    /// async gap the phase is no longer `Running` and the stale check still says
    /// "actionable", so a second click on the `✓` chip would otherwise re-spawn
    /// against nothing. That gap is exactly when the panel is `loading`, so the
    /// caller passes its refresh-in-flight flag here to suppress it.
    pub fn begin_update(&mut self, refreshing: bool) -> bool {
        if refreshing || self.update_phase == ActionPhase::Running || !self.update_actionable() {
            return false;
        }
        self.update_phase = ActionPhase::Running;
        true
    }

    /// Settle the Update Hermes worker with its `Ok`/`Err` result. Ignored
    /// unless an update is actually running.
    pub fn finish_update(&mut self, ok: bool) {
        if self.update_phase == ActionPhase::Running {
            self.update_phase = if ok {
                ActionPhase::Done
            } else {
                ActionPhase::Failed
            };
        }
    }
}

/// Live store backing the *hosted* Command Center view: the pure action model
/// ([`CommandCenterState`], carrying the two action phases + the update check)
/// plus the last-fetched system status and usage rollup, with load/error flags.
///
/// [`crate::AwiliApp::refresh_command_center`] populates this from the gateway on
/// view-open (same non-blocking shape as the other panel refreshes). Opening the
/// view only *reads* gateway state — the [LIVE] restart/update *actions* fire
/// solely on a deliberate chip click (via [`command_center`]'s `on_*` listeners
/// into [`crate::AwiliApp::restart_gateway_action`] /
/// [`crate::AwiliApp::update_hermes_action`]), driven through the guarded
/// [`CommandCenterState`] reducer so a double-click can't launch two workers.
#[derive(Default)]
pub struct CommandCenterData {
    /// Action lifecycles + the update check that gates *Update Hermes*.
    pub state: CommandCenterState,
    /// Last `GET /api/status` result (the status header + grid source).
    pub status: Option<SystemStatus>,
    /// Last `GET /api/analytics/usage` rollup (the usage chart source).
    pub usage: Option<UsageRollup>,
    /// A fetch is in flight (drives the loading affordance, guards re-entry).
    pub loading: bool,
    /// The last fetch's error, if the status call failed.
    pub error: Option<String>,
}

/// Summarise the messaging connectors as `"k/n connected"` (a connector counts
/// as connected when its `PlatformStatus::state` is `"connected"`). With no
/// platforms reported the summary is `"none"`.
pub fn connectors_summary(status: &SystemStatus) -> String {
    let n = status.gateway_platforms.len();
    if n == 0 {
        return "none".to_string();
    }
    let connected = status
        .gateway_platforms
        .values()
        .filter(|p| p.state.as_deref() == Some("connected"))
        .count();
    format!("{connected}/{n} connected")
}

/// Build the labelled key/value rows for the status grid, in display order.
/// Every value is a plain owned `String` so the grid renders without touching a
/// live gateway.
pub fn system_status_rows(status: &SystemStatus) -> Vec<StatusRow> {
    let mut rows = Vec::new();

    let version = match (status.version.as_deref(), status.release_date.as_deref()) {
        (Some(v), Some(d)) if !v.is_empty() && !d.is_empty() => format!("{v} ({d})"),
        (Some(v), _) if !v.is_empty() => v.to_string(),
        _ => "unknown".to_string(),
    };
    rows.push(StatusRow {
        label: "Version".to_string(),
        value: version,
    });

    let pid = status
        .gateway_pid
        .map(|p| format!("pid {p}"))
        .unwrap_or_else(|| "—".to_string());
    rows.push(StatusRow {
        label: "Gateway".to_string(),
        value: pid,
    });

    rows.push(StatusRow {
        label: "Active agents".to_string(),
        value: status.active_agents.to_string(),
    });
    rows.push(StatusRow {
        label: "Active sessions".to_string(),
        value: status.active_sessions.to_string(),
    });

    let (config_text, _) = config_summary(status);
    rows.push(StatusRow {
        label: "Config".to_string(),
        value: config_text,
    });

    rows.push(StatusRow {
        label: "Connectors".to_string(),
        value: connectors_summary(status),
    });

    let auth = if status.auth_required {
        if status.auth_providers.is_empty() {
            "required".to_string()
        } else {
            status.auth_providers.join(", ")
        }
    } else {
        "not required".to_string()
    };
    rows.push(StatusRow {
        label: "Auth".to_string(),
        value: auth,
    });

    rows
}

/// Capitalise the first letter of each `_`/`-`/space-separated word (for turning
/// a raw `gateway_state` like `"exited_unexpectedly"` into `"Exited
/// Unexpectedly"`).
fn title_case(s: &str) -> String {
    s.split(['_', '-', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(first) => first.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn tone_color(tone: HealthTone) -> gpui::Rgba {
    match tone {
        HealthTone::Ok => theme::p().accent,
        HealthTone::Warn => theme::p().sev_attention,
        HealthTone::Down => theme::p().sev_critical,
    }
}

/// Render the Command Center panel: gateway-health pill + status grid, the
/// unit-21 usage chart, and the [LIVE] action affordances. A thin consumer of
/// [`system_status_rows`]/[`gateway_health`] and [`usage_chart`]; the only
/// side effects are the two `on_*` click handlers (typically `cx.listener`s
/// into [`crate::AwiliApp::restart_gateway_action`] /
/// [`crate::AwiliApp::update_hermes_action`]) attached to the action chips.
///
/// *Restart Gateway* is always clickable — the reducer guards double-fire.
/// *Update Hermes* is clickable only when [`update_affordance`] reports the
/// chip both `visible` and `enabled` (an available update the gateway can apply
/// in place); a visible-but-`!enabled` update renders as a dimmed, inert chip
/// with the manual-apply hint, so `on_update` is wired only where a click can
/// actually do something.
pub fn command_center(
    status: &SystemStatus,
    rollup: &UsageRollup,
    update: Option<&HermesUpdateCheck>,
    restart_phase: ActionPhase,
    update_phase: ActionPhase,
    on_restart: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    on_update: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let health = gateway_health(status);
    let pill = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(theme::z(6.))
        .px(theme::z(10.))
        .py(theme::z(3.))
        .rounded(theme::z(theme::RADIUS))
        .bg(theme::p().chip_bg)
        .child(
            div()
                .w(theme::z(8.))
                .h(theme::z(8.))
                .rounded(theme::z(4.))
                .bg(tone_color(health.tone)),
        )
        .child(
            div()
                .text_size(theme::z(theme::LABEL_SIZE))
                .text_color(theme::p().text)
                .font_weight(FontWeight::MEDIUM)
                .child(SharedString::from(health.label)),
        );

    let header = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(theme::z(theme::TEXT_SIZE))
                .text_color(theme::p().text)
                .font_weight(FontWeight::SEMIBOLD)
                .child(SharedString::from("Command Center")),
        )
        .child(pill);

    // Status grid: one `label   value` row per entry, label muted/left, value
    // in the primary text colour.
    let rows: Vec<gpui::AnyElement> = system_status_rows(status)
        .into_iter()
        .map(|r| {
            div()
                .flex()
                .flex_row()
                .justify_between()
                .gap(theme::z(16.))
                .text_size(theme::z(theme::LABEL_SIZE))
                .child(
                    div()
                        .text_color(theme::p().muted)
                        .child(SharedString::from(r.label)),
                )
                .child(
                    div()
                        .text_color(theme::p().text)
                        .child(SharedString::from(r.value)),
                )
                .into_any_element()
        })
        .collect();

    let grid = div()
        .flex()
        .flex_col()
        .gap(theme::z(4.))
        .p(theme::z(12.))
        .rounded(theme::z(theme::RADIUS))
        .bg(theme::p().raised)
        .children(rows);

    // Usage chart (or a placeholder when there's no usage yet).
    let usage: gpui::AnyElement = match usage_chart(rollup) {
        Some(chart) => chart.into_any_element(),
        None => div()
            .text_size(theme::z(theme::LABEL_SIZE))
            .text_color(theme::p().faint)
            .child(SharedString::from("No usage recorded yet."))
            .into_any_element(),
    };

    // [LIVE] actions. Chip copy runs through [`action_chip_label`] so the
    // worker's Idle/Running/Done/Failed phase shows through. "Restart Gateway"
    // is always offered and clickable (the reducer guards double-fire).
    // "Update Hermes" is gated purely on the update check ([`update_affordance`])
    // — hidden until one is fetched, a dimmed inert chip when an update is
    // available but the gateway cannot apply it in place (captioned with how to
    // apply it manually), and clickable only when it can self-apply.
    let mut action_chips = vec![clickable_action_chip(
        "cc-restart-gateway",
        &action_chip_label("Restart Gateway", restart_phase),
        on_restart,
    )];
    let aff = update_affordance(update);
    if aff.visible {
        let label = action_chip_label("Update Hermes", update_phase);
        if aff.enabled {
            action_chips.push(clickable_action_chip("cc-update-hermes", &label, on_update));
        } else {
            // Visible but the gateway can't self-apply: dimmed and inert; the
            // caption below tells the user how to apply it manually.
            action_chips.push(static_action_chip(&label, false));
        }
    }
    let update_detail = aff.detail;

    let mut actions = div().flex().flex_col().gap(theme::z(6.)).child(
        div()
            .flex()
            .flex_row()
            .gap(theme::z(8.))
            .children(action_chips),
    );
    if let Some(detail) = update_detail {
        actions = actions.child(
            div()
                .text_size(theme::z(theme::LABEL_SIZE))
                .text_color(theme::p().muted)
                .child(SharedString::from(detail)),
        );
    }
    let actions = actions.child(
        div()
            .text_size(theme::z(theme::LABEL_SIZE))
            .text_color(theme::p().faint)
            .child(SharedString::from("Actions run against the live gateway.")),
    );

    div()
        .flex()
        .flex_col()
        .gap(theme::z(12.))
        .p(theme::z(16.))
        .child(header)
        .child(grid)
        .child(usage)
        .child(actions)
}

/// A static, non-interactive action affordance (see the [LIVE] note on
/// [`command_center`]). Used for the dimmed *Update Hermes* chip when an update
/// is available but the gateway can't apply it in place — `enabled == false`
/// dims it and it takes no click (the caption tells the user how to apply it
/// manually).
fn static_action_chip(label: &str, enabled: bool) -> gpui::AnyElement {
    let text = if enabled {
        theme::p().chip_text
    } else {
        theme::p().faint
    };
    div()
        .px(theme::z(10.))
        .py(theme::z(4.))
        .rounded(theme::z(theme::RADIUS))
        .bg(theme::p().chip_bg)
        .text_size(theme::z(theme::LABEL_SIZE))
        .text_color(text)
        .child(SharedString::from(label.to_string()))
        .into_any_element()
}

/// A live, clickable action chip. `id` is the stable element id GPUI needs for
/// hit-testing; `on_click` is a view listener (from `cx.listener`) that fires
/// the [LIVE] verb. Mirrors [`static_action_chip`]'s look with a pointer cursor
/// and a hover lift so it reads as pressable.
fn clickable_action_chip(
    id: &'static str,
    label: &str,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::AnyElement {
    div()
        .id(id)
        .cursor_pointer()
        .px(theme::z(10.))
        .py(theme::z(4.))
        .rounded(theme::z(theme::RADIUS))
        .bg(theme::p().chip_bg)
        .text_size(theme::z(theme::LABEL_SIZE))
        .text_color(theme::p().chip_text)
        .hover(|s| s.bg(theme::p().raised))
        .child(SharedString::from(label.to_string()))
        .on_click(on_click)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_gateway::rest::PlatformStatus;
    use std::collections::BTreeMap;

    /// An all-default [`SystemStatus`]. It does not derive `Default`, but every
    /// field is `#[serde(default)]`, so decoding an empty object yields the
    /// zero value — the same fixture-driven construction the gateway tests use.
    fn base() -> SystemStatus {
        awili_gateway::rest::decode_status("{}").unwrap()
    }

    fn platform(state: &str) -> PlatformStatus {
        PlatformStatus {
            state: Some(state.to_string()),
            error_code: None,
            error_message: None,
            updated_at: None,
        }
    }

    #[test]
    fn gateway_health_down_when_not_running_uses_state_or_stopped() {
        let mut s = base();
        s.gateway_running = false;
        let h = gateway_health(&s);
        assert_eq!(h.tone, HealthTone::Down);
        assert_eq!(h.label, "Stopped");

        s.gateway_state = Some("exited_unexpectedly".to_string());
        let h = gateway_health(&s);
        assert_eq!(h.tone, HealthTone::Down);
        assert_eq!(h.label, "Exited Unexpectedly");
    }

    #[test]
    fn gateway_health_empty_state_string_falls_back_to_stopped() {
        // A down gateway that reports an EMPTY `gateway_state` (`""` — a present
        // but blank field in the JSON, not an absent one) must still show the
        // meaningful "Stopped" label, never a blank pill. The `.filter(|s|
        // !s.is_empty())` guard is what turns the empty string back into `None`
        // so the `unwrap_or_else` fallback fires; without it `title_case("")`
        // would render an empty label.
        let mut s = base();
        s.gateway_running = false;
        s.gateway_state = Some(String::new());
        let h = gateway_health(&s);
        assert_eq!(h.tone, HealthTone::Down);
        assert_eq!(h.label, "Stopped");
    }

    #[test]
    fn gateway_health_ok_warn_transitions() {
        let mut s = base();
        s.gateway_running = true;
        assert_eq!(gateway_health(&s).tone, HealthTone::Ok);
        assert_eq!(gateway_health(&s).label, "Running");

        // `gateway_drainable` is NOT a health signal — a healthy running gateway
        // reports it (canonical live status), so the pill stays OK/Running.
        s.gateway_drainable = true;
        assert_eq!(gateway_health(&s).tone, HealthTone::Ok);
        assert_eq!(gateway_health(&s).label, "Running");

        // Only `gateway_busy` warns.
        s.gateway_busy = true;
        assert_eq!(gateway_health(&s).tone, HealthTone::Warn);
        assert_eq!(gateway_health(&s).label, "Busy");
    }

    /// An all-default [`HermesUpdateCheck`] via the decoder (every field is
    /// `#[serde(default)]`), the same fixture style as [`base`].
    fn check() -> HermesUpdateCheck {
        awili_gateway::rest::decode_hermes_update_check("{}").unwrap()
    }

    #[test]
    fn update_affordance_none_is_hidden() {
        let a = update_affordance(None);
        assert!(!a.visible);
        assert!(!a.enabled);
        assert_eq!(a.detail, None);
    }

    #[test]
    fn update_affordance_git_install_is_visible_enabled_and_counts_commits() {
        let mut c = check();
        c.update_available = true;
        c.can_apply = true; // git install
        c.behind = 3;
        let a = update_affordance(Some(&c));
        assert!(a.visible);
        assert!(a.enabled);
        // Purely the "N commits behind" caption — no manual hint on can_apply.
        assert_eq!(a.detail.as_deref(), Some("3 commits behind"));

        // Singular grammar at exactly one behind.
        c.behind = 1;
        assert_eq!(
            update_affordance(Some(&c)).detail.as_deref(),
            Some("1 commit behind")
        );

        // Up to date / unknown: no caption.
        c.behind = 0;
        assert_eq!(update_affordance(Some(&c)).detail, None);
    }

    #[test]
    fn update_affordance_available_but_cannot_apply_is_visible_disabled_with_manual_hint() {
        // A pip install can report an update yet not self-apply: chip visible,
        // NOT enabled, captioned with how to apply it manually.
        let mut c = check();
        c.update_available = true;
        c.can_apply = false;
        c.behind = 2;
        c.update_command = Some("pip install -U hermes".to_string());
        let a = update_affordance(Some(&c));
        assert!(a.visible);
        assert!(!a.enabled);
        assert_eq!(
            a.detail.as_deref(),
            Some("2 commits behind · update manually: pip install -U hermes")
        );

        // No update_command: fall back to the install method.
        c.update_command = None;
        c.install_method = Some("pip".to_string());
        assert_eq!(
            update_affordance(Some(&c)).detail.as_deref(),
            Some("2 commits behind · pip install — apply manually")
        );

        // Neither: generic hint.
        c.install_method = None;
        assert_eq!(
            update_affordance(Some(&c)).detail.as_deref(),
            Some("2 commits behind · cannot apply automatically")
        );
    }

    #[test]
    fn update_affordance_cannot_apply_shows_hint_alone_when_not_behind() {
        // update_available && !can_apply with behind == 0 — a real state (a
        // pip-style update reporting a newer version but no git commit-behind
        // count). The chip is visible-but-dimmed; its caption is the manual hint
        // ALONE, with no phantom "commits behind" prefix and no orphan " · "
        // separator (the single-element join path no other test exercises). The
        // dimmed chip must still explain itself so the user isn't left with a
        // greyed action and no reason.
        let mut c = check();
        c.update_available = true;
        c.can_apply = false;
        c.behind = 0;
        c.update_command = Some("pip install -U hermes".to_string());
        let a = update_affordance(Some(&c));
        assert!(a.visible);
        assert!(!a.enabled);
        assert_eq!(
            a.detail.as_deref(),
            Some("update manually: pip install -U hermes")
        );

        // Falls back to the install method, still hint-only (no prefix).
        c.update_command = None;
        c.install_method = Some("pip".to_string());
        assert_eq!(
            update_affordance(Some(&c)).detail.as_deref(),
            Some("pip install — apply manually")
        );

        // Neither hint source: the generic fallback, still alone.
        c.install_method = None;
        assert_eq!(
            update_affordance(Some(&c)).detail.as_deref(),
            Some("cannot apply automatically")
        );
    }

    #[test]
    fn update_affordance_not_available_is_hidden_with_no_detail() {
        // A caption belongs to a visible chip. A not-available check is hidden
        // and disabled regardless of can_apply / behind, and carries no detail
        // (so the render can't leave an orphan caption under a missing chip).
        let mut c = check();
        c.update_available = false;
        c.can_apply = true;
        c.behind = 5;
        let a = update_affordance(Some(&c));
        assert!(!a.visible);
        assert!(!a.enabled);
        assert_eq!(a.detail, None);
    }

    #[test]
    fn action_chip_label_reflects_lifecycle() {
        assert_eq!(
            action_chip_label("Restart Gateway", ActionPhase::Idle),
            "Restart Gateway"
        );
        assert_eq!(
            action_chip_label("Restart Gateway", ActionPhase::Running),
            "Restart Gateway…"
        );
        assert_eq!(
            action_chip_label("Update Hermes", ActionPhase::Done),
            "Update Hermes ✓"
        );
        assert_eq!(
            action_chip_label("Update Hermes", ActionPhase::Failed),
            "Update Hermes ✗"
        );
        // Idle is the Default phase.
        assert_eq!(ActionPhase::default(), ActionPhase::Idle);
    }

    #[test]
    fn config_summary_flags_update_when_behind_or_hermes_reports_it() {
        let mut s = base();
        s.config_version = Some(7);
        s.latest_config_version = Some(9);
        let (text, update) = config_summary(&s);
        assert_eq!(text, "v7 → v9");
        assert!(update);

        s.latest_config_version = Some(7);
        let (text, update) = config_summary(&s);
        assert_eq!(text, "v7");
        assert!(!update);

        // can_update_hermes forces the flag even when versions match.
        s.can_update_hermes = true;
        let (_, update) = config_summary(&s);
        assert!(update);
    }

    #[test]
    fn config_summary_unknown_when_no_versions() {
        let s = base();
        let (text, update) = config_summary(&s);
        assert_eq!(text, "unknown");
        assert!(!update);
    }

    #[test]
    fn config_summary_latest_below_current_shows_no_upgrade_arrow() {
        // A rollback / stale-latest report: `latest_config_version` is *below*
        // the running `config_version`. The upgrade arm is guarded by strictly
        // `l > c`, so this must fall through to the bare `v{c}` arm — no bogus
        // "v9 → v7" arrow, no update flag. The existing tests only cover l>c
        // and l==c, so the strict `>` guard is unpinned against a `>`->`!=`
        // mutation (which l<c is the sole input to catch).
        let mut s = base();
        s.config_version = Some(9);
        s.latest_config_version = Some(7);
        let (text, update) = config_summary(&s);
        assert_eq!(text, "v9");
        assert!(!update);
    }

    #[test]
    fn connectors_summary_counts_connected() {
        let mut s = base();
        assert_eq!(connectors_summary(&s), "none");

        let mut m = BTreeMap::new();
        m.insert("telegram".to_string(), platform("disabled"));
        m.insert("email".to_string(), platform("connected"));
        m.insert("discord".to_string(), platform("connected"));
        s.gateway_platforms = m;
        assert_eq!(connectors_summary(&s), "2/3 connected");
    }

    #[test]
    fn system_status_rows_shape_and_order() {
        let mut s = base();
        s.version = Some("0.9.1".to_string());
        s.release_date = Some("2026-06-29".to_string());
        s.gateway_pid = Some(4242);
        s.active_agents = 2;
        s.active_sessions = 5;
        s.config_version = Some(7);
        s.latest_config_version = Some(7);
        s.auth_required = false;

        let rows = system_status_rows(&s);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Version",
                "Gateway",
                "Active agents",
                "Active sessions",
                "Config",
                "Connectors",
                "Auth",
            ]
        );
        assert_eq!(rows[0].value, "0.9.1 (2026-06-29)");
        assert_eq!(rows[1].value, "pid 4242");
        assert_eq!(rows[2].value, "2");
        assert_eq!(rows[3].value, "5");
        assert_eq!(rows[4].value, "v7");
        assert_eq!(rows[5].value, "none");
        assert_eq!(rows[6].value, "not required");
    }

    #[test]
    fn system_status_rows_auth_lists_providers_and_handles_missing_version() {
        let mut s = base();
        s.auth_required = true;
        s.auth_providers = vec!["google".to_string(), "github".to_string()];
        let rows = system_status_rows(&s);
        assert_eq!(rows[0].value, "unknown"); // no version -> "unknown"
        assert_eq!(rows[1].value, "—"); // no pid -> em dash
        assert_eq!(rows[6].value, "google, github");

        // auth required with no providers falls back to "required".
        s.auth_providers.clear();
        let rows = system_status_rows(&s);
        assert_eq!(rows[6].value, "required");
    }

    #[test]
    fn system_status_rows_empty_version_string_falls_back_to_unknown() {
        // A present-but-blank version string must not leak a lone " (date)" (from
        // the compound arm) or an empty "" (from the version-only arm) into the
        // Version row. Both arms carry a `!v.is_empty()` guard, so `Some("")` with
        // a real release_date skips them and lands on "unknown" — same as `None`.
        // The missing-version test only exercises `None`; the empty-string path is
        // otherwise unpinned.
        let mut s = base();
        s.version = Some(String::new());
        s.release_date = Some("2026-07-09".to_string());
        let rows = system_status_rows(&s);
        assert_eq!(rows[0].value, "unknown");
    }

    /// A git-install check with an applicable update — the only shape that makes
    /// Update Hermes actionable.
    fn applicable_check() -> HermesUpdateCheck {
        let mut c = check();
        c.update_available = true;
        c.can_apply = true;
        c.behind = 2;
        c
    }

    #[test]
    fn state_defaults_idle_with_no_check() {
        let st = CommandCenterState::default();
        assert_eq!(st.restart_phase, ActionPhase::Idle);
        assert_eq!(st.update_phase, ActionPhase::Idle);
        assert!(st.update_check.is_none());
        assert!(!st.update_actionable());
    }

    #[test]
    fn command_center_data_defaults_are_empty_idle_and_not_loading() {
        // The hosted view's initial contract: on first paint nothing is loading
        // or errored and both [LIVE] actions are Idle — so the panel shows a
        // plain notice (not a stale or mid-restart chip) until a fetch lands.
        let d = CommandCenterData::default();
        assert!(d.status.is_none());
        assert!(d.usage.is_none());
        assert!(!d.loading);
        assert!(d.error.is_none());
        assert_eq!(d.state.restart_phase, ActionPhase::Idle);
        assert_eq!(d.state.update_phase, ActionPhase::Idle);
        assert!(d.state.update_check.is_none());
        // An empty rollup is the render fallback when no usage was fetched; it
        // must be genuinely empty so `usage_chart` shows its placeholder.
        let empty = UsageRollup::default();
        assert!(empty.per_day.is_empty());
        assert!(empty.top_models.is_empty());
    }

    #[test]
    fn begin_restart_flips_to_running_then_settles() {
        let mut st = CommandCenterState::default();
        assert!(st.begin_restart());
        assert_eq!(st.restart_phase, ActionPhase::Running);
        st.finish_restart(true);
        assert_eq!(st.restart_phase, ActionPhase::Done);

        // A failed restart settles to Failed.
        let mut st = CommandCenterState::default();
        assert!(st.begin_restart());
        st.finish_restart(false);
        assert_eq!(st.restart_phase, ActionPhase::Failed);
    }

    #[test]
    fn begin_restart_guards_double_fire() {
        let mut st = CommandCenterState::default();
        assert!(st.begin_restart());
        // Second click while running spawns nothing and leaves the phase alone.
        assert!(!st.begin_restart());
        assert_eq!(st.restart_phase, ActionPhase::Running);
    }

    #[test]
    fn finish_restart_ignored_when_not_running() {
        // A stray completion on a fresh Idle chip must not flip it to Done.
        let mut st = CommandCenterState::default();
        st.finish_restart(true);
        assert_eq!(st.restart_phase, ActionPhase::Idle);

        // Nor re-settle an already-Done chip on a duplicate completion.
        let mut st = CommandCenterState::default();
        st.begin_restart();
        st.finish_restart(true);
        st.finish_restart(false);
        assert_eq!(st.restart_phase, ActionPhase::Done);
    }

    #[test]
    fn begin_update_requires_actionable_check() {
        // No check held → not actionable → spawns nothing, stays Idle.
        let mut st = CommandCenterState::default();
        assert!(!st.begin_update(false));
        assert_eq!(st.update_phase, ActionPhase::Idle);

        // A pip install (update_available but !can_apply) is visible-but-dimmed:
        // clicking it must still spawn nothing.
        let mut c = check();
        c.update_available = true;
        c.can_apply = false;
        st.set_update_check(Some(c));
        assert!(!st.begin_update(false));
        assert_eq!(st.update_phase, ActionPhase::Idle);

        // An applicable git update is actionable → begins.
        st.set_update_check(Some(applicable_check()));
        assert!(st.update_actionable());
        assert!(st.begin_update(false));
        assert_eq!(st.update_phase, ActionPhase::Running);
    }

    #[test]
    fn begin_update_guards_double_fire_and_finish_settles() {
        let mut st = CommandCenterState::default();
        st.set_update_check(Some(applicable_check()));
        assert!(st.begin_update(false));
        // Double-click while running → no second worker.
        assert!(!st.begin_update(false));
        assert_eq!(st.update_phase, ActionPhase::Running);
        st.finish_update(true);
        assert_eq!(st.update_phase, ActionPhase::Done);
        // A duplicate completion after Done must not re-settle it to Failed.
        st.finish_update(false);
        assert_eq!(st.update_phase, ActionPhase::Done);

        // A failed update after a real start settles to Failed.
        let mut st = CommandCenterState::default();
        st.set_update_check(Some(applicable_check()));
        assert!(st.begin_update(false));
        st.finish_update(false);
        assert_eq!(st.update_phase, ActionPhase::Failed);

        // finish_update on a never-started action is ignored.
        let mut st = CommandCenterState::default();
        st.finish_update(false);
        assert_eq!(st.update_phase, ActionPhase::Idle);
    }

    #[test]
    fn begin_update_refuses_during_post_update_refresh() {
        // The race the `refreshing` guard closes: a successful update settles the
        // phase to Done while the held check still reads actionable, and only the
        // trailing refresh replaces it with the now-current one. In that window
        // the phase is not Running and the stale check still says "actionable",
        // so without the guard a second click on the ✓ chip would re-spawn.
        let mut st = CommandCenterState::default();
        st.set_update_check(Some(applicable_check()));
        assert!(st.begin_update(false));
        st.finish_update(true);
        assert_eq!(st.update_phase, ActionPhase::Done);
        // Stale check is still actionable (refresh hasn't landed yet)...
        assert!(st.update_actionable());
        // ...but a click while the refresh is in flight spawns nothing.
        assert!(!st.begin_update(true));
        assert_eq!(st.update_phase, ActionPhase::Done);
    }
}
