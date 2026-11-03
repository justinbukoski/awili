//! `ui::SettingRow` — the kit's settings row (KIT.md v3, batch K4).
//!
//! The label (`type_::UI`, strong, wrapping) and an optional help line
//! (`type_::LABEL`, `TEXT_DIM`, wrapping) on the left; the control on the
//! right. `.changed(bool)` shows the **Changed** `Chip.tone(Contrast)` and
//! the guarded Reset; `.saved(stamp)` flashes **Saved** for
//! [`SAVED_FOR`] each time the parent bumps the stamp (the part runs the
//! timer; there is no animation, so it is the same under Reduce Motion);
//! `.restart_needed()` shows the `Banner(Attention, "Restart Awili to
//! apply")`.
//!
//! **The control disables itself (K4b).** `.control(|reason| …)` builds
//! the control with the row's effective disabled reason — the row's own
//! `.disabled(reason)`, else an unmet `.depends_on(text, false)` — which
//! the kit control applies with `.disabled_if(reason)`. A part cannot
//! switch off an arbitrary child from outside (its clicks, tooltip and tab
//! stop are the child's), so the row hands the reason down and the control
//! dims, leaves the Tab order and shows the reason itself. Reset and the
//! marks take the same reason.
//!
//! **The guards.** Reset and Choice changes keep both existing guards
//! from the v1 settings (`settings_pages.rs`), which are the parent's
//! two-press state machine (arm on the first press, fire on the
//! second):
//!
//! - `.confirm_leave(armed)` — the `confirm_leave` guard
//!   (`settings_pages.rs:108`): leaving the row's guarded current value
//!   takes a second press on **Reset**. While `armed`, the reset paints
//!   "Click again to reset" (the v1 face).
//! - `.confirm_to(armed)` — the `confirm_to` guard: choosing the row's
//!   guarded target value takes a second press on the choice. While
//!   `armed`, the row paints the "Click again to confirm" hint; the
//!   [`super::choice::Choice`] itself paints the armed segment face
//!   through its own `.confirm_to(…)` (the segment's index).
//!
//! The guards stay the parent's: the part paints the parent's state and
//! fires the handlers on every press, exactly as `press_settings_reset` /
//! `press_settings_choice` do today.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    div, prelude::*, AnyElement, App, ClickEvent, ElementId, IntoElement, RenderOnce, SharedString,
    Window,
};

use crate::theme::{self, palette};
use crate::ui::banner::Banner;
use crate::ui::button::Button;
use crate::ui::chip::Chip;
use crate::ui::interaction::{child_id, ClickHandler, Common};
use crate::ui::text;

/// The part tag for the default `debug_selector`.
const SETTING_ROW: &str = "setting-row";
/// The child-id tags for the row's slots.
const SETTING_ROW_RESET: &str = "reset";
const SETTING_ROW_BANNER: &str = "banner";

/// The armed faces (the v1 words, kept verbatim).
const RESET_NORMAL: &str = "Reset";
const RESET_ARMED: &str = "Click again to reset";
const CONFIRM_TO_HINT: &str = "Click again to confirm";

/// How long the **Saved** mark shows after a save (KIT.md: 1.5 s).
pub const SAVED_FOR: Duration = Duration::from_millis(theme::motion::SAVED_FLASH_MS);

/// The reset button's face label for the parent's `confirm_leave` armed
/// state (the v1 words, verbatim): "Click again to reset" while the
/// guard waits on its second press, "Reset" otherwise.
pub fn reset_label(confirm_leave_armed: bool) -> &'static str {
    if confirm_leave_armed {
        RESET_ARMED
    } else {
        RESET_NORMAL
    }
}

/// The control builder: it receives the row's effective disabled reason.
type ControlBuilder = Box<dyn FnOnce(Option<SharedString>) -> AnyElement>;
/// The parent's Reset face, built with the row's effective disabled reason.
pub type ResetSlotBuilder = Box<dyn FnOnce(Option<SharedString>) -> AnyElement>;

/// A kit settings row. Build with [`SettingRow::new(id, label)`] and
/// chain the slots and the shared conventions; the parent owns the
/// control's value, the guards' arming and the save stamp.
#[derive(IntoElement)]
pub struct SettingRow {
    common: Common,
    label: SharedString,
    help: Option<SharedString>,
    control: Option<ControlBuilder>,
    changed: bool,
    on_reset: Option<ClickHandler>,
    /// The `confirm_leave` guard's armed state (the reset face).
    confirm_leave_armed: bool,
    /// The `confirm_to` guard's armed state (the hint face).
    confirm_to_armed: bool,
    /// The save stamp (0 = never saved): each new value flashes Saved.
    saved: u64,
    restart_needed: bool,
    /// `.depends_on(text, met)`: the reason and whether it is met.
    depends_on: Option<(SharedString, bool)>,
    /// The parent's persistent mark, on the label's line (`.label_mark`).
    label_mark: Option<AnyElement>,
    /// The parent's own Reset and its unseen twin (`.reset_slot`).
    reset_slot: Option<(Option<ResetSlotBuilder>, AnyElement)>,
    /// Visual notes under the help line (`.note`).
    notes: Vec<AnyElement>,
    /// The visual-only dim (`.visually_dimmed`): not a disabled state.
    visually_dimmed: bool,
}

impl SettingRow {
    /// A settings row with its label (the accessible name too).
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        Self {
            common: Common {
                id: id.into(),
                label: Some(label.clone()),
                ..Default::default()
            },
            label,
            help: None,
            control: None,
            changed: false,
            on_reset: None,
            confirm_leave_armed: false,
            confirm_to_armed: false,
            saved: 0,
            restart_needed: false,
            depends_on: None,
            label_mark: None,
            reset_slot: None,
            notes: Vec::new(),
            visually_dimmed: false,
        }
    }

    /// The optional help line (it wraps, so its full text is always on
    /// screen).
    pub fn help(mut self, help: impl Into<SharedString>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// The help line, if any.
    pub fn help_text(&self) -> Option<SharedString> {
        self.help.clone()
    }

    /// The control at the row's right, built with the row's effective
    /// disabled reason (`None` when the row is live): apply it with the
    /// kit's `.disabled_if(reason)` —
    /// `.control(|r| Toggle::new("t", on).disabled_if(r))`.
    pub fn control<E: IntoElement + 'static>(
        mut self,
        build: impl FnOnce(Option<SharedString>) -> E + 'static,
    ) -> Self {
        self.control = Some(Box::new(move |reason| build(reason).into_any_element()));
        self
    }

    /// Whether the row holds a control.
    pub fn has_control(&self) -> bool {
        self.control.is_some()
    }

    /// The value has changed from its default: the **Changed**
    /// `Chip.tone(Contrast)` shows, and the Reset (when `.on_reset` is
    /// set) shows beside it.
    pub fn changed(mut self, changed: bool) -> Self {
        self.changed = changed;
        self
    }

    /// Whether the row paints the Changed / Reset face.
    pub fn is_changed(&self) -> bool {
        self.changed
    }

    /// The Reset's handler. The kit's double-click guard rides on the
    /// `Button`; the guards' arming is the parent's state
    /// (`.confirm_leave(…)` paints the armed face).
    pub fn on_reset(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_reset = Some(Box::new(f));
        self
    }

    /// The **confirm_leave** guard (the v1 `settings_pages.rs:108`):
    /// `armed` is the parent's arming state. While armed, the reset paints
    /// "Click again to reset".
    pub fn confirm_leave(mut self, armed: bool) -> Self {
        self.confirm_leave_armed = armed;
        self
    }

    /// The **confirm_to** guard: `armed` is the parent's arming state for
    /// the row's guarded target value. While armed, the row paints the
    /// "Click again to confirm" hint.
    pub fn confirm_to(mut self, armed: bool) -> Self {
        self.confirm_to_armed = armed;
        self
    }

    /// Whether the row paints the armed confirm-to hint.
    pub fn is_confirm_to_armed(&self) -> bool {
        self.confirm_to_armed
    }

    /// The **Saved** flash: the parent bumps `stamp` on every save
    /// (`0` = never saved). Each new stamp shows the mark for
    /// [`SAVED_FOR`]; the part runs the timer.
    pub fn saved(mut self, stamp: u64) -> Self {
        self.saved = stamp;
        self
    }

    /// The **Restart Awili to apply** `Banner(Attention)`.
    pub fn restart_needed(mut self, needed: bool) -> Self {
        self.restart_needed = needed;
        self
    }

    /// Whether the row paints the restart banner.
    pub fn is_restart_needed(&self) -> bool {
        self.restart_needed
    }

    /// A dependency: `.depends_on(text, met)` — while `met` is `false`,
    /// the row is reduced emphasis, shows the reason, and its control and
    /// Reset are disabled with that reason.
    pub fn depends_on(mut self, text: impl Into<SharedString>, met: bool) -> Self {
        self.depends_on = Some((text.into(), met));
        self
    }

    /// The parent's persistent mark, painted on the label's line beside the
    /// label. Unlike `.saved(stamp)` there is no timer: the parent passes the
    /// mark while it holds (`None` when it does not).
    pub fn label_mark(mut self, mark: Option<AnyElement>) -> Self {
        self.label_mark = mark;
        self
    }

    /// The parent's own Reset in the Reset slot. The builder receives the
    /// row's effective disabled reason, so its face can use `.disabled_if`.
    /// Otherwise `placeholder` paints in the same slot while at the default.
    /// This replaces the row's `.changed` / `.on_reset` face.
    pub fn reset_slot(mut self, reset: Option<ResetSlotBuilder>, placeholder: AnyElement) -> Self {
        self.reset_slot = Some((reset, placeholder));
        self
    }

    /// A visual note under the help line. It neither disables the control
    /// nor the Reset (that is `.depends_on(text, false)`).
    pub fn note(mut self, note: impl IntoElement) -> Self {
        self.notes.push(note.into_any_element());
        self
    }

    /// The visual-only dim: reduced emphasis (the label fades and the line
    /// is at 55%). The control and the Reset stay live and clickable.
    pub fn visually_dimmed(mut self, dimmed: bool) -> Self {
        self.visually_dimmed = dimmed;
        self
    }

    /// The dependency's `(reason, met)` pair, if any.
    pub fn depends_on_of(&self) -> Option<(SharedString, bool)> {
        self.depends_on.clone()
    }

    /// Whether the row is reduced-emphasis (a dependency is unmet).
    pub fn is_dimmed(&self) -> bool {
        self.depends_on.as_ref().is_some_and(|(_, met)| !met)
    }

    /// The reason the row's control and Reset are disabled: the row's
    /// own reason, else an unmet dependency's.
    pub fn effective_reason(&self) -> Option<SharedString> {
        self.common.disabled_reason.clone().or_else(|| {
            self.depends_on
                .as_ref()
                .and_then(|(text, met)| (!met).then(|| text.clone()))
        })
    }
}

crate::kit_shared!(SettingRow, skip_label);

impl std::fmt::Debug for SettingRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SettingRow({:?})", self.common.selector_for(SETTING_ROW))
    }
}

/// The Saved flash's state under the row's id: the last stamp seen and
/// whether the mark is still showing.
#[derive(Default)]
struct SavedFlash {
    seen: Cell<u64>,
    showing: Cell<bool>,
}

impl RenderOnce for SettingRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette();
        let reason = self.effective_reason();
        let dimmed = reason.is_some();
        let common = self.common;
        let selector = common.selector_for(SETTING_ROW);

        // The Saved flash: a new stamp shows the mark and starts its
        // timer; the timer hides it unless a newer save came since.
        let flash = window
            .use_keyed_state(child_id(&common.id, "saved"), cx, |_, _| {
                Rc::new(SavedFlash::default())
            })
            .read(cx)
            .clone();
        if self.saved != 0 && flash.seen.get() != self.saved {
            let stamp = self.saved;
            flash.seen.set(stamp);
            flash.showing.set(true);
            let flash_timer = flash.clone();
            window
                .spawn(cx, async move |cx| {
                    cx.background_executor().timer(SAVED_FOR).await;
                    if flash_timer.seen.get() == stamp {
                        flash_timer.showing.set(false);
                        let _ = cx.update(|window, _| window.refresh());
                    }
                })
                .detach();
        }
        let saved_showing = flash.showing.get();

        // The left: the label and the help, both wrapping (long text is
        // never lost) — reduced emphasis when dimmed (disabled or the
        // visual-only dim).
        let visually_dimmed = self.visually_dimmed;
        let label_color = if dimmed || visually_dimmed {
            p.text_dim
        } else {
            p.text
        };
        let label = text::ui_text(self.label.clone())
            .strong()
            .wrap()
            .text_color_override(label_color);
        // The parent's persistent mark sits on the label's own line.
        let label_line: AnyElement = match self.label_mark {
            Some(mark) => div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_baseline()
                .gap(theme::z(theme::SP_8))
                .child(label)
                .child(mark)
                .into_any_element(),
            None => label.into_any_element(),
        };
        let mut left = div()
            .flex()
            .flex_col()
            .gap(theme::z(theme::SP_4))
            .flex_grow(1.)
            .min_w_0()
            .child(label_line);
        if let Some(help) = self.help.clone() {
            left = left.child(text::label(help).dim().wrap());
        }
        if let Some((text_reason, false)) = self.depends_on.clone() {
            left = left.child(text::label(text_reason).dim().wrap());
        }
        for note in self.notes {
            left = left.child(note);
        }

        // The right: the control (built with the row's reason), the
        // marks, the reset (disabled with the reason, never deleted).
        let mut right = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(theme::z(theme::SP_8));
        if let Some(build) = self.control {
            right = right.child(build(reason.clone()));
        }
        if self.confirm_to_armed && !dimmed {
            right = right.child(
                div()
                    .debug_selector(|| format!("{selector}-confirm-hint"))
                    .flex()
                    .child(text::label(CONFIRM_TO_HINT.to_string()).dim()),
            );
        }
        if self.changed {
            right = right.child(
                Chip::new(child_id(&common.id, "changed"), "Changed")
                    .tone(theme::Tone::Contrast)
                    .disabled_if(reason.clone()),
            );
        }
        if saved_showing {
            right = right.child(
                Chip::new(child_id(&common.id, "saved"), "Saved")
                    .tone(theme::Tone::Contrast)
                    .disabled_if(reason.clone()),
            );
        }
        if let Some((reset, placeholder)) = self.reset_slot {
            // The parent's own Reset, or its unseen twin in the same slot.
            right = right.child(
                reset
                    .map(|build| build(reason.clone()))
                    .unwrap_or(placeholder),
            );
        } else if self.changed {
            if let Some(on_reset) = self.on_reset {
                let allow_double = common.allow_double_click;
                let reset = Button::new(
                    child_id(&common.id, SETTING_ROW_RESET),
                    reset_label(self.confirm_leave_armed),
                )
                .ghost()
                .allow_double_click_if(allow_double);
                right = right.child(match reason.clone() {
                    Some(reason) => reset.disabled(reason),
                    None => reset.on_click(move |ev, window, cx| {
                        if allow_double || ev.click_count() <= 1 {
                            on_reset(ev, window, cx);
                        }
                    }),
                });
            }
        }

        let line = div()
            .debug_selector(|| format!("{selector}-line"))
            .when(visually_dimmed, |d| {
                d.opacity(theme::SETTING_ROW_VISUAL_DIM_OPACITY)
            })
            .flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap(theme::z(theme::SP_12))
            .px(theme::z(theme::SP_12))
            .py(theme::z(theme::SP_8))
            .min_h(theme::target())
            .rounded(theme::z(theme::RADIUS_M))
            .child(left)
            .child(right);

        let mut root = div()
            .id(common.id.clone())
            .flex()
            .flex_col()
            .w_full()
            .gap(theme::z(theme::SP_4))
            .debug_selector(|| selector.clone())
            .child(line)
            .when(self.restart_needed, |d| {
                let banner = Banner::new(
                    child_id(&common.id, SETTING_ROW_BANNER),
                    theme::Tone::Attention,
                    "Restart Awili to apply",
                );
                // A dimmed row's reason is its only tooltip.
                d.child(if dimmed { banner.quiet_dot() } else { banner })
            });

        // The row's own tooltip: the reason when dimmed (the children
        // carry the same reason, so nothing competes).
        if let Some(reason) = reason {
            root = root.tooltip(crate::icons::tip(reason.to_string()));
        } else if let Some(tip) = common.tooltip.clone() {
            root = root.tooltip(crate::icons::tip(tip.to_string()));
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::{render_part, tooltip_at};
    use crate::ui::toggle::Toggle;

    fn click(wcx: &mut gpui::VisualTestContext, sel: &'static str, count: usize) {
        let at = wcx
            .debug_bounds(sel)
            .unwrap_or_else(|| panic!("{sel} recorded no bounds"))
            .center();
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: count,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: count,
        });
        wcx.run_until_parked();
    }

    fn sample_row() -> SettingRow {
        SettingRow::new("route", "Fleet route")
            .help("Where the household fleet sends its work.")
            .control(|r| {
                Toggle::new("route-toggle", true)
                    .label("Fleet route")
                    .disabled_if(r)
            })
            .changed(true)
            .on_reset(|_e, _w, _c| {})
    }

    /// The row lays out headless with the label, the help line and the
    /// control; `.changed(true)` shows the Changed chip and the Reset.
    #[gpui::test]
    fn setting_row_renders_its_slots(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || div().flex().child(sample_row()));
        for sel in [
            "ui-setting-row-route",
            "ui-setting-row-route-line",
            "ui-chip-route-changed",
            "ui-button-route-reset",
            "ui-toggle-route-toggle",
        ] {
            assert!(wcx.debug_bounds(sel).is_some(), "{sel} laid out");
        }
    }

    /// The guard faces follow the parent's arming.
    #[gpui::test]
    fn setting_row_guard_faces_follow_the_parent(cx: &mut TestAppContext) {
        assert_eq!(reset_label(true), RESET_ARMED);
        assert_eq!(reset_label(false), RESET_NORMAL);
        let choice = |id: &'static str| {
            move |r: Option<SharedString>| {
                crate::ui::choice::Choice::new(id, vec!["Ask".into(), "Go ahead".into()], 0)
                    .disabled_if(r)
            }
        };
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .child(
                    SettingRow::new("hinted", "Mode")
                        .control(choice("hc"))
                        .confirm_to(true),
                )
                .child(SettingRow::new("plain", "Mode").control(choice("pc")))
        });
        assert!(wcx
            .debug_bounds("ui-setting-row-hinted-confirm-hint")
            .is_some());
        assert!(wcx
            .debug_bounds("ui-setting-row-plain-confirm-hint")
            .is_none());
    }

    /// K4b (Codex P1): a disabled row — or an unmet dependency — hands its
    /// reason to the control and the Reset: clicking either fires
    /// nothing, Tab reaches neither, and the reason is the tooltip that
    /// opens over the control, the Reset and the row.
    #[gpui::test]
    fn disabled_row_disables_its_control_and_reset(cx: &mut TestAppContext) {
        for (id, dep) in [("locked", false), ("needs", true)] {
            let toggled = Rc::new(Cell::new(0u32));
            let reset = Rc::new(Cell::new(0u32));
            let handle = cx.update(|cx| cx.focus_handle());
            let (t, r, h) = (toggled.clone(), reset.clone(), handle.clone());
            let reason = if dep {
                "Needs the Cloud route."
            } else {
                "Locked by policy"
            };
            let (_root, wcx) = render_part(cx, move || {
                let (t, r, h) = (t.clone(), r.clone(), h.clone());
                let mut row = SettingRow::new(id, "Model")
                    .control(move |reason| {
                        Toggle::new("tg", true)
                            .focus_handle(&h)
                            .on_toggle(move |_e, _w, _c| t.set(t.get() + 1))
                            .disabled_if(reason)
                    })
                    .changed(true)
                    .on_reset(move |_e, _w, _c| r.set(r.get() + 1));
                row = if dep {
                    row.depends_on("Needs the Cloud route.", false)
                } else {
                    row.disabled("Locked by policy")
                };
                div().flex().pt(theme::z(theme::SP_40)).child(row)
            });
            let reset_sel: &'static str =
                Box::leak(format!("ui-button-{id}-reset").into_boxed_str());
            click(wcx, "ui-toggle-tg", 1);
            click(wcx, reset_sel, 1);
            assert_eq!((toggled.get(), reset.get()), (0, 0), "{id}: nothing fires");
            wcx.update(|w, cx| w.focus_next(cx));
            wcx.run_until_parked();
            assert_ne!(
                wcx.update(|w, cx| w.focused(cx)),
                Some(handle.clone()),
                "{id}: no tab stop"
            );
            assert_eq!(
                tooltip_at(wcx, "ui-toggle-tg").as_deref(),
                Some(reason),
                "{id}: control"
            );
            assert_eq!(
                tooltip_at(wcx, reset_sel).as_deref(),
                Some(reason),
                "{id}: reset"
            );
        }
    }

    /// K4b (Codex P2): each new save stamp shows Saved for `SAVED_FOR`,
    /// then it goes; the same stamp does not flash again.
    #[gpui::test]
    fn saved_flashes_for_its_time(cx: &mut TestAppContext) {
        let stamp = Rc::new(Cell::new(1u64));
        let s = stamp.clone();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .child(SettingRow::new("sv", "Model").saved(s.get()))
        });
        assert!(
            wcx.debug_bounds("ui-chip-sv-saved").is_some(),
            "Saved shows"
        );
        wcx.executor()
            .advance_clock(SAVED_FOR + Duration::from_millis(50));
        wcx.run_until_parked();
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        assert!(
            wcx.debug_bounds("ui-chip-sv-saved").is_none(),
            "and goes after its time"
        );
        stamp.set(2);
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        assert!(
            wcx.debug_bounds("ui-chip-sv-saved").is_some(),
            "a new save shows it again"
        );
    }

    /// `.restart_needed()` paints the banner; `.depends_on(text, false)`
    /// dims the row and keeps the reason.
    #[gpui::test]
    fn setting_row_restart_and_depends_on(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(
                SettingRow::new("faces", "Model")
                    .restart_needed(true)
                    .depends_on("The fleet route must be Cloud.", false),
            )
        });
        assert!(wcx.debug_bounds("ui-banner-faces-banner").is_some());
        let dimmed = SettingRow::new("dim", "Model").depends_on("Needs Cloud.", false);
        assert!(dimmed.is_dimmed());
        assert_eq!(dimmed.effective_reason().as_deref(), Some("Needs Cloud."));
        assert!(!SettingRow::new("ok", "Model")
            .depends_on("Needs Cloud.", true)
            .is_dimmed());
    }

    /// K4b round 2 (Codex): a dimmed row's restart banner shows no dot
    /// tooltip of its own — the reason is the only tooltip there.
    #[gpui::test]
    fn dimmed_row_banner_has_no_competing_tooltip(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .w(theme::z(theme::PANEL_W * 2.))
                .pt(theme::z(theme::SP_40))
                .child(
                    SettingRow::new("rs", "Model")
                        .restart_needed(true)
                        .disabled("Locked by policy"),
                )
        });
        let banner = wcx.debug_bounds("ui-banner-rs-banner").unwrap();
        // The dot sits at the banner's left padding.
        let at = gpui::point(
            banner.origin.x + theme::z(theme::SP_16) + theme::z(theme::DOT) / 2.,
            banner.center().y,
        );
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at_point(wcx, at).as_deref(),
            Some("Locked by policy")
        );
    }

    /// The confirm_leave path: the part fires Reset on every press (the
    /// parent arms on the first and resets on the second) and paints the
    /// armed face while the parent says it is armed.
    #[gpui::test]
    fn armed_reset_fires_its_second_press(cx: &mut TestAppContext) {
        let armed = Rc::new(Cell::new(false));
        let reset_done = Rc::new(Cell::new(false));
        let (a, r) = (armed.clone(), reset_done.clone());
        let (_root, wcx) = render_part(cx, move || {
            let (a2, r2) = (a.clone(), r.clone());
            div().flex().pt(theme::z(theme::SP_40)).child(
                SettingRow::new("ar", "Model")
                    .changed(true)
                    .confirm_leave(a.get())
                    .on_reset(move |_e, _w, _c| {
                        // The parent's two-press machine.
                        if a2.get() {
                            r2.set(true);
                            a2.set(false);
                        } else {
                            a2.set(true);
                        }
                    }),
            )
        });
        click(wcx, "ui-button-ar-reset", 1);
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        assert!(armed.get() && !reset_done.get(), "the first press armed");
        assert_eq!(
            tooltip_at(wcx, "ui-button-ar-reset").as_deref(),
            Some(RESET_ARMED),
            "the armed face"
        );
        click(wcx, "ui-button-ar-reset", 1);
        assert!(reset_done.get(), "the second press reset");
    }

    /// The Reset slot keeps its room: the parent's own Reset and the unseen
    /// twin it passes for a default row lay out at the same size and in the
    /// same place, in rows of the same width.
    #[gpui::test]
    fn reset_slot_keeps_the_same_room_changed_or_not(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .flex_col()
                .w(theme::z(theme::PANEL_W * 2.))
                .child(SettingRow::new("slot-live", "Model").reset_slot(
                    Some(Box::new(|reason| {
                        Button::new("slot-live-reset", "Reset")
                            .disabled_if(reason)
                            .reserve_selected_check(true)
                            .selector("sx-test-reset")
                            .into_any_element()
                    })),
                    div().into_any_element(),
                ))
                .child(
                    SettingRow::new("slot-idle", "Model").reset_slot(
                        None,
                        div()
                            .debug_selector(|| "sx-test-slot".to_string())
                            .invisible()
                            .child(
                                Button::new("slot-idle-face", "Reset").reserve_selected_check(true),
                            )
                            .into_any_element(),
                    ),
                )
        });
        let reset = wcx
            .debug_bounds("sx-test-reset")
            .expect("the parent's Reset shows");
        let slot = wcx
            .debug_bounds("sx-test-slot")
            .expect("the unseen twin keeps the slot");
        assert_eq!(reset.size, slot.size, "the same room: {reset:?} {slot:?}");
        assert_eq!(
            reset.origin.x, slot.origin.x,
            "the same place in a row of the same width"
        );
    }

    /// The persistent mark stays on its label's line after the Saved flash
    /// has gone: the parent owns it, and it has no timer.
    #[gpui::test]
    fn persistent_mark_outlives_the_saved_flash(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().pt(theme::z(theme::SP_40)).child(
                SettingRow::new("pm", "Model")
                    .help("Where it is kept.")
                    .saved(1)
                    .label_mark(Some(
                        div()
                            .debug_selector(|| "sx-test-mark".to_string())
                            .child("\u{2713} Saved")
                            .into_any_element(),
                    )),
            )
        });
        assert!(wcx.debug_bounds("sx-test-mark").is_some(), "the mark shows");
        assert!(
            wcx.debug_bounds("ui-chip-pm-saved").is_some(),
            "the timed Saved flash is initially visible too"
        );
        wcx.executor()
            .advance_clock(SAVED_FOR + Duration::from_millis(500));
        wcx.run_until_parked();
        wcx.update(|w, _| w.refresh());
        wcx.run_until_parked();
        let mark = wcx
            .debug_bounds("sx-test-mark")
            .expect("the mark outlives the flash");
        let line = wcx
            .debug_bounds("ui-setting-row-pm-line")
            .expect("the row's line");
        assert!(
            f32::from(mark.origin.y) - f32::from(line.origin.y) < f32::from(line.size.height) / 2.,
            "it sits on the label's line, at the top of the row: mark {mark:?}, line {line:?}"
        );
        assert!(
            wcx.debug_bounds("ui-chip-pm-saved").is_none(),
            "the timed flash expires independently"
        );
    }

    /// The visual-only dim fades the row and shows its note, but it is not
    /// a disabled state: the control stays clickable and the row reports no
    /// reason. A `.depends_on(text, false)` row is still disabled.
    #[gpui::test]
    fn visual_dim_is_clickable_and_not_disabled(cx: &mut TestAppContext) {
        let toggled = Rc::new(Cell::new(0u32));
        let t = toggled.clone();
        let (_root, wcx) = render_part(cx, move || {
            let t = t.clone();
            div().flex().pt(theme::z(theme::SP_40)).child(
                SettingRow::new("vd", "Model")
                    .help("Where it is kept.")
                    .visually_dimmed(true)
                    .note(
                        div()
                            .debug_selector(|| "sx-test-dim-note".to_string())
                            .child("Turn Notifications on to choose these."),
                    )
                    .control(move |reason| {
                        Toggle::new("vd-tg", true)
                            .on_toggle(move |_e, _w, _c| t.set(t.get() + 1))
                            .disabled_if(reason)
                    }),
            )
        });
        assert!(
            wcx.debug_bounds("sx-test-dim-note").is_some(),
            "the note shows"
        );
        assert!(
            SettingRow::new("vd", "Model")
                .visually_dimmed(true)
                .effective_reason()
                .is_none(),
            "a visual dim is not a disabled state"
        );
        assert!(
            SettingRow::new("dep", "Model")
                .depends_on("Needs the Cloud route.", false)
                .effective_reason()
                .is_some(),
            "a dependency that is not met still disables"
        );
        click(wcx, "ui-toggle-vd-tg", 1);
        assert_eq!(toggled.get(), 1, "the dimmed control still takes a click");
    }

    /// A parent's Reset factory receives the row's dependency reason, so a
    /// custom face stays inert when the row is disabled.
    #[gpui::test]
    fn custom_reset_receives_dependency_reason_and_stays_disabled(cx: &mut TestAppContext) {
        let reason_seen = Rc::new(std::cell::RefCell::new(None));
        let reason_capture = reason_seen.clone();
        let resets = Rc::new(Cell::new(0u32));
        let reset_capture = resets.clone();
        let (_root, wcx) = render_part(cx, move || {
            let reason_capture = reason_capture.clone();
            let reset_capture = reset_capture.clone();
            div().flex().pt(theme::z(theme::SP_40)).child(
                SettingRow::new("custom-dep", "Model")
                    .depends_on("Needs the Cloud route.", false)
                    .reset_slot(
                        Some(Box::new(move |reason| {
                            *reason_capture.borrow_mut() = reason.clone();
                            let reset_capture = reset_capture.clone();
                            Button::new("custom-dep-reset", "Reset")
                                .selector("sx-custom-dep-reset")
                                .disabled_if(reason)
                                .on_click(move |_event, _window, _cx| {
                                    reset_capture.set(reset_capture.get() + 1)
                                })
                                .into_any_element()
                        })),
                        div().into_any_element(),
                    ),
            )
        });
        assert_eq!(
            reason_seen.borrow().as_deref(),
            Some("Needs the Cloud route."),
            "the row gives the parent Reset its effective reason"
        );
        assert!(
            wcx.debug_bounds("sx-custom-dep-reset").is_some(),
            "the reset face remains present"
        );
        click(wcx, "sx-custom-dep-reset", 1);
        assert_eq!(resets.get(), 0, "the disabled reset does not fire");
    }
}
