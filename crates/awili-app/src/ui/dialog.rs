//! `ui::Dialog` — the kit's modal confirm step (KIT.md v3, batch K3).
//!
//! A modal panel, `PANEL_W` wide, centred over a `SCRIM` (the existing
//! token; a click on the scrim does **not** close — the dialog is
//! modal, and only the buttons close it). **Focus is trapped** inside,
//! starting on the default button (the primary), and **restored to the
//! trigger** on close: the parent hands the dialog the trigger's focus
//! handle (`.trigger_focus(…)`), the dialog's close sequence runs
//! `on_close → focus trigger → action`, and Tab / Shift+Tab cycle the
//! dialog's buttons in the visual order [cancel, secondary, primary]
//! and wrap. Escape runs the cancel step; Enter runs the primary step.
//!
//! Buttons are the kit's `Button`: the primary is `primary()` (or
//! `ghost()` when `.destructive()` — the confirm action on a ghost
//! button that says what is lost and whether it can be undone, with no
//! red), the cancel is `ghost()`, the secondary `quiet()`.
//!
//! The parent creates the button focus handles (the dialog is a
//! stateless `RenderOnce` and its `new()` takes no `cx`) and hands them
//! in with `.focus_handles(&cancel, &secondary, &primary)`. Without
//! them, the buttons get their own anonymous handles and the trap
//! still works.

use gpui::{
    div, prelude::*, px, App, ClickEvent, ElementId, IntoElement, KeyDownEvent, RenderOnce,
    SharedString, Window,
};

use crate::theme::{self, palette};
use crate::ui::button::Button;
use crate::ui::focus_ring::focus_ring_style_on;
use crate::ui::interaction::{child_id, ClickHandler, DIALOG};

/// The dialog's close step (KIT.md § Dialog): called when the dialog
/// closes — Escape, the cancel or the primary (the scrim never closes
/// it: the dialog is modal).
type CloseHandler = Box<dyn Fn(&mut Window, &mut App) + 'static>;

/// A kit modal dialog. Build with [`Dialog::new(id, title, body)`] and
/// chain `.primary(…)`, `.cancel(…)`, `.secondary(…)`, `.destructive()`,
/// `.trigger_focus(…)`, `.focus_handles(…)` and `.on_close(…)`; the
/// parent owns the open state.
#[derive(IntoElement)]
pub struct Dialog {
    common: crate::ui::interaction::Common,
    title: SharedString,
    body: Option<gpui::AnyElement>,
    primary: Option<(SharedString, ClickHandler)>,
    cancel: Option<(SharedString, ClickHandler)>,
    secondary: Option<(SharedString, ClickHandler)>,
    destructive: bool,
    on_close: Option<CloseHandler>,
    trigger: Option<gpui::FocusHandle>,
    /// The button focus handles (cancel, secondary, primary), if the
    /// parent created them.
    handles: (
        Option<gpui::FocusHandle>,
        Option<gpui::FocusHandle>,
        Option<gpui::FocusHandle>,
    ),
}

impl Dialog {
    /// A modal dialog with the given title (also its accessible name)
    /// and body.
    pub fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        body: impl IntoElement,
    ) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            title: title.into(),
            body: Some(body.into_any_element()),
            primary: None,
            cancel: None,
            secondary: None,
            destructive: false,
            on_close: None,
            trigger: None,
            handles: (None, None, None),
        }
    }

    /// The confirm step (the default button; Enter runs it).
    pub fn primary(
        mut self,
        label: impl Into<SharedString>,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.primary = Some((label.into(), Box::new(f)));
        self
    }

    /// The cancel step (Escape runs it).
    pub fn cancel(
        mut self,
        label: impl Into<SharedString>,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.cancel = Some((label.into(), Box::new(f)));
        self
    }

    /// An optional third step (the kit's `Button.quiet()`).
    pub fn secondary(
        mut self,
        label: impl Into<SharedString>,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.secondary = Some((label.into(), Box::new(f)));
        self
    }

    /// Make the confirm step destructive: the primary paints as a
    /// ghost button — the label says what is lost and whether it can be
    /// undone, with no red.
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    /// Whether the confirm step is destructive (the primary is a ghost
    /// button).
    pub fn is_destructive(&self) -> bool {
        self.destructive
    }

    /// The close callback: the first step of every close (a button
    /// click, Escape or Enter). The parent sets its open state to
    /// closed there.
    pub fn on_close(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Box::new(f));
        self
    }

    /// The trigger's focus handle: on close, the dialog restores focus
    /// to it (before running the step's action).
    pub fn trigger_focus(mut self, handle: &gpui::FocusHandle) -> Self {
        self.trigger = Some(handle.clone());
        self
    }

    /// The button focus handles: `(cancel, secondary, primary)`. The
    /// parent creates them (the dialog's `new()` takes no `cx`) and the
    /// trap cycles the ones that are present, in the visual order
    /// [cancel, secondary, primary].
    pub fn focus_handles(
        mut self,
        cancel: Option<&gpui::FocusHandle>,
        secondary: Option<&gpui::FocusHandle>,
        primary: Option<&gpui::FocusHandle>,
    ) -> Self {
        self.handles = (cancel.cloned(), secondary.cloned(), primary.cloned());
        self
    }

    /// The button focus handles in the trap order
    /// [cancel, secondary, primary], skipping the absent ones.
    fn trap_order(&self) -> Vec<gpui::FocusHandle> {
        [
            self.handles.0.clone(),
            self.handles.1.clone(),
            self.handles.2.clone(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The default button's focus handle (the primary, else the
    /// cancel, else the secondary): the trap starts there.
    pub fn default_focus(&self) -> Option<gpui::FocusHandle> {
        self.handles
            .2
            .clone()
            .or_else(|| self.handles.0.clone())
            .or_else(|| self.handles.1.clone())
    }

    /// The dialog's title.
    pub fn title_text(&self) -> &SharedString {
        &self.title
    }
}

crate::kit_shared!(Dialog);

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Dialog({})", self.common.selector_for(DIALOG))
    }
}

/// The close sequence (like the menu's choose): close, restore focus to
/// the trigger, then run the step's action — in that order.
fn run_step(
    on_close: Option<&CloseHandler>,
    trigger: Option<&gpui::FocusHandle>,
    on_click: Option<&ClickHandler>,
    ev: &ClickEvent,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(on_close) = on_close {
        on_close(window, cx);
    }
    if let Some(trigger) = trigger {
        window.focus(trigger, cx);
    }
    if let Some(on_click) = on_click {
        on_click(ev, window, cx);
    }
}

/// The dialog's own button focus handles, kept in element state under the
/// dialog's id: without `.focus_handles(…)` the dialog is self-contained
/// (every action renders, the trap works, focus restores).
struct OwnedHandles {
    cancel: gpui::FocusHandle,
    secondary: gpui::FocusHandle,
    primary: gpui::FocusHandle,
}

/// One button step: its label, its handler and the focus handle it uses.
type Step = (SharedString, std::rc::Rc<ClickHandler>, gpui::FocusHandle);

impl RenderOnce for Dialog {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let p = palette();
        let common = self.common;
        let selector = common.selector_for(DIALOG);
        let title = self.title.clone();
        let body = self.body.unwrap_or_else(|| div().into_any_element());
        let destructive = self.destructive;
        // The close handler is shared through `Rc` by the buttons, Escape
        // and Enter (the handler itself is not `Clone`).
        let on_close = self.on_close.map(std::rc::Rc::new);
        let trigger = self.trigger.clone();

        // The parent's handles when given, else the dialog's own.
        let owned =
            window.use_keyed_state(child_id(&common.id, "handles"), cx, |_, cx| OwnedHandles {
                cancel: cx.focus_handle(),
                secondary: cx.focus_handle(),
                primary: cx.focus_handle(),
            });
        let (own_cancel, own_secondary, own_primary) = {
            let o = owned.read(cx);
            (o.cancel.clone(), o.secondary.clone(), o.primary.clone())
        };
        let step = |s: Option<(SharedString, ClickHandler)>,
                    given: Option<gpui::FocusHandle>,
                    own: gpui::FocusHandle|
         -> Option<Step> {
            s.map(|(label, handler)| (label, std::rc::Rc::new(handler), given.unwrap_or(own)))
        };
        let cancel = step(self.cancel, self.handles.0.clone(), own_cancel);
        let secondary = step(self.secondary, self.handles.1.clone(), own_secondary);
        let primary = step(self.primary, self.handles.2.clone(), own_primary);

        // The trap order [cancel, secondary, primary] (the visual order),
        // over the actions that exist; focus starts on the default (the
        // primary, else the cancel, else the secondary).
        let trap: Vec<gpui::FocusHandle> = [&cancel, &secondary, &primary]
            .into_iter()
            .flatten()
            .map(|(_, _, h)| h.clone())
            .collect();
        let default = primary
            .as_ref()
            .or(cancel.as_ref())
            .or(secondary.as_ref())
            .map(|(_, _, h)| h.clone());

        // Every action renders (kit `Button`s on their focus handles).
        let button = |tag: &str, s: &Step| {
            let (label, handler, handle) = s;
            let on_close = on_close.clone();
            let trigger = trigger.clone();
            let handler = handler.clone();
            Button::new(child_id(&common.id, tag), label.clone())
                .focus_handle(handle)
                // The dialog's double-click setting reaches its own buttons.
                .allow_double_click_if(common.allow_double_click)
                .on_click(move |ev, w, cx| {
                    run_step(
                        on_close.as_deref(),
                        trigger.as_ref(),
                        Some(handler.as_ref()),
                        ev,
                        w,
                        cx,
                    );
                })
        };
        let row = div()
            .flex()
            .w_full()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .when_some(cancel.as_ref(), |d, s| d.child(button("cancel", s).ghost()))
            .when_some(secondary.as_ref(), |d, s| {
                d.child(button("secondary", s).quiet())
            })
            .when_some(primary.as_ref(), |d, s| {
                let b = button("primary", s);
                d.child(if destructive { b.ghost() } else { b.primary() })
            });

        let panel = crate::ui::panel::float_face(
            div()
                .flex()
                .flex_col()
                .w_full()
                .px(theme::z(theme::SP_20))
                .pt(theme::z(theme::SP_20))
                .pb(theme::z(theme::SP_16))
                // The title wraps: a long one is never cut off (KIT.md § Long text).
                .child(crate::ui::text::heading(title).wrap())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .w_full()
                        .pt(theme::z(theme::SP_12))
                        .child(body)
                        .child(
                            div()
                                .flex()
                                .w_full()
                                .justify_end()
                                .mt(theme::z(theme::SP_16))
                                .child(row),
                        ),
                ),
        )
        .debug_selector(|| format!("ui-dialog-{id}-panel", id = common.id))
        .id(child_id(&common.id, "panel"))
        .w(theme::z(theme::PANEL_W));

        // The modal scrim (the existing `SCRIM` token): a click on it
        // does nothing — only the buttons close. It is the centring
        // container (the v1 settings-modal pattern) and occludes the
        // app behind it, so the dialog is truly modal.
        let scrim = div()
            .absolute()
            .top(gpui::Pixels::ZERO)
            .left(gpui::Pixels::ZERO)
            .size_full()
            .bg(p.scrim)
            .debug_selector(|| "ui-dialog-scrim".to_string())
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .child(panel);

        let mut root = div()
            .size_full()
            .relative()
            .debug_selector(|| selector.clone())
            .child(scrim);

        // The trap: focus starts on the default button (the part keeps it
        // there while nothing inside the dialog holds focus).
        if let Some(default) = default.clone() {
            let focused = window.focused(cx);
            if !focused.is_some_and(|f| trap.contains(&f)) {
                window.focus(&default, cx);
            }
        }

        // Enter runs the default step (the primary, else the cancel, else
        // the secondary), whatever button has focus: it is caught in the capture phase of the key-up — the
        // event GPUI turns into a keyboard click — so the focused button
        // never also fires. Space stays the focused button's own.
        if let Some((_, handler, _)) = primary
            .clone()
            .or_else(|| cancel.clone())
            .or_else(|| secondary.clone())
        {
            let on_close = on_close.clone();
            let trigger = trigger.clone();
            root = root.capture_key_up(move |ev: &gpui::KeyUpEvent, window, cx| {
                if ev.keystroke.key == "enter" && !ev.keystroke.modifiers.modified() {
                    window.prevent_default();
                    cx.stop_propagation();
                    let click = ClickEvent::Keyboard(gpui::KeyboardClickEvent::default());
                    run_step(
                        on_close.as_deref(),
                        trigger.as_ref(),
                        Some(handler.as_ref()),
                        &click,
                        window,
                        cx,
                    );
                }
            });
        }

        // Tab / Shift+Tab cycle the buttons and wrap — with one button,
        // focus stays on it (it never escapes the dialog). Escape runs the
        // cancel step (or, without one, just closes and restores focus).
        let cancel_handler = cancel.as_ref().map(|(_, h, _)| h.clone());
        root = root.on_key_down(move |ev, window, cx| match ev.keystroke.key.as_str() {
            "tab" if !trap.is_empty() => {
                let dir = if ev.keystroke.modifiers.shift { -1 } else { 1 };
                let cur = window.focused(cx);
                let next = match trap.iter().position(|h| cur.as_ref() == Some(h)) {
                    Some(pos) => (pos as isize + dir).rem_euclid(trap.len() as isize) as usize,
                    None => 0,
                };
                window.focus(&trap[next], cx);
                window.prevent_default();
                cx.stop_propagation();
            }
            "escape" => {
                let click = ClickEvent::Keyboard(gpui::KeyboardClickEvent::default());
                window.prevent_default();
                cx.stop_propagation();
                run_step(
                    on_close.as_deref(),
                    trigger.as_ref(),
                    cancel_handler.as_deref(),
                    &click,
                    window,
                    cx,
                );
            }
            _ => {}
        });

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// A long title wraps (KIT.md § Long text): the dialog grows rather than
    /// cutting it off.
    #[gpui::test]
    fn long_dialog_title_wraps(cx: &mut TestAppContext) {
        let height = |cx: &mut TestAppContext, title: &'static str| {
            let (_root, wcx) = render_part(cx, move || {
                Dialog::new("d", title, crate::ui::text::read("Body").wrap())
                    .cancel("Keep it", |_e, _w, _cx| {})
            });
            wcx.debug_bounds("ui-dialog-d-panel").unwrap().size.height
        };
        let short = height(cx, "Delete?");
        let long = height(
            cx,
            "Delete this conversation, its 42 messages, their attachments and every branch made from any of them?",
        );
        assert!(
            long > short,
            "the long title wrapped: {long:?} vs {short:?}"
        );
    }

    /// Focus starts on the default button (the primary) and Tab /
    /// Shift+Tab cycle the buttons in the visual order
    /// [cancel, secondary, primary], wrapping — the trap keeps focus
    /// inside the dialog.
    #[gpui::test]
    fn dialog_traps_focus_and_cycles_the_buttons(cx: &mut TestAppContext) {
        let cancel = cx.update(|cx| cx.focus_handle());
        let secondary = cx.update(|cx| cx.focus_handle());
        let primary = cx.update(|cx| cx.focus_handle());
        let (c, s, p) = (cancel.clone(), secondary.clone(), primary.clone());
        let (_root, wcx) = render_part(cx, move || {
            let (c, s, p) = (c.clone(), s.clone(), p.clone());
            div().flex().child(
                Dialog::new("confirm", "Stop the job?", "It stops on its current step.")
                    .focus_handles(Some(&c), Some(&s), Some(&p))
                    .cancel("Cancel", |_e, _w, _cx| {})
                    .secondary("Ask later", |_e, _w, _cx| {})
                    .primary("Stop job", |_e, _w, _cx| {})
                    .on_close(|_w, _cx| {}),
            )
        });
        wcx.run_until_parked(); // the settle frame (the trap's focus)

        // The trap starts on the default button (the primary).
        assert!(
            wcx.update(|w, cx| w.focused(cx)) == Some(primary.clone()),
            "focus starts on the primary"
        );

        // Tab: primary → cancel → secondary → primary (the visual
        // order [cancel, secondary, primary], wrapping).
        fn tab(wcx: &mut gpui::VisualTestContext, shift: bool) {
            let ks = if shift {
                gpui::Keystroke::parse("shift-tab").unwrap()
            } else {
                gpui::Keystroke::parse("tab").unwrap()
            };
            wcx.simulate_event(gpui::KeyDownEvent {
                keystroke: ks.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            wcx.simulate_event(gpui::KeyUpEvent { keystroke: ks });
            wcx.update(|w, _cx| w.refresh());
            wcx.run_until_parked();
        }

        tab(wcx, false);
        assert!(
            wcx.update(|w, cx| w.focused(cx)) == Some(cancel.clone()),
            "Tab from the primary lands on the cancel"
        );
        tab(wcx, false);
        assert!(
            wcx.update(|w, cx| w.focused(cx)) == Some(secondary.clone()),
            "Tab lands on the secondary"
        );
        tab(wcx, false);
        assert!(
            wcx.update(|w, cx| w.focused(cx)) == Some(primary.clone()),
            "Tab wraps back to the primary"
        );
        tab(wcx, true);
        assert!(
            wcx.update(|w, cx| w.focused(cx)) == Some(secondary.clone()),
            "Shift+Tab walks back"
        );
    }

    /// Escape runs the cancel step: the dialog closes, focus is
    /// restored to the trigger, then the cancel's action runs.
    #[gpui::test]
    fn dialog_escape_runs_cancel_and_restores_focus(cx: &mut TestAppContext) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let events_in = events.clone();
        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        let closed_in = closed.clone();
        let trigger = cx.update(|cx| cx.focus_handle());
        let cancel_h = cx.update(|cx| cx.focus_handle());
        let primary = cx.update(|cx| cx.focus_handle());
        let (t, c, p) = (trigger.clone(), cancel_h.clone(), primary.clone());
        let (_root, wcx) = render_part(cx, move || {
            // The parent owns the open state: once the dialog closed it
            // is not rendered again, so the focus restore to the trigger
            // is final (the trap does not re-capture it).
            if closed_in.get() {
                return div();
            }
            let events_in = events_in.clone();
            let (t, c, p) = (t.clone(), c.clone(), p.clone());
            div().flex().child(
                Dialog::new("confirm", "Stop the job?", "It stops on its current step.")
                    .focus_handles(Some(&c), None, Some(&p))
                    .trigger_focus(&t)
                    .cancel("Cancel", {
                        let events_in = events_in.clone();
                        move |_e, _w, _cx| {
                            events_in.borrow_mut().push("cancel".to_string());
                        }
                    })
                    .primary("Stop job", {
                        let events_in = events_in.clone();
                        move |_e, _w, _cx| {
                            events_in.borrow_mut().push("primary".to_string());
                        }
                    })
                    .on_close({
                        let events_in = events_in.clone();
                        let closed_in = closed_in.clone();
                        move |_w, _cx| {
                            events_in.borrow_mut().push("close".to_string());
                            closed_in.set(true);
                        }
                    }),
            )
        });
        // (The trap focuses the primary; Escape then runs the cancel.)
        let ks = gpui::Keystroke::parse("escape").unwrap();
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: ks.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent { keystroke: ks });
        assert_eq!(
            events.borrow().as_slice(),
            ["close", "cancel"],
            "Escape closes and runs the cancel step"
        );
        assert!(
            wcx.update(|w, cx| w.focused(cx)) == Some(trigger.clone()),
            "focus is back on the trigger"
        );
    }

    /// Enter runs the primary step: the dialog closes, focus is
    /// restored to the trigger, then the action runs.
    #[gpui::test]
    fn dialog_enter_runs_primary_and_restores_focus(cx: &mut TestAppContext) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let events_in = events.clone();
        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        let closed_in = closed.clone();
        let trigger = cx.update(|cx| cx.focus_handle());
        let primary = cx.update(|cx| cx.focus_handle());
        let (t, p) = (trigger.clone(), primary.clone());
        let (_root, wcx) = render_part(cx, move || {
            // The parent owns the open state: once the dialog closed it
            // is not rendered again, so the focus restore to the trigger
            // is final (the trap does not re-capture it).
            if closed_in.get() {
                return div();
            }
            let events_in = events_in.clone();
            let (t, p) = (t.clone(), p.clone());
            div().flex().child(
                Dialog::new("confirm", "Stop the job?", "It stops on its current step.")
                    .focus_handles(Some(&t), None, Some(&p))
                    .trigger_focus(&t)
                    .cancel("Cancel", {
                        let events_in = events_in.clone();
                        move |_e, _w, _cx| {
                            events_in.borrow_mut().push("cancel".to_string());
                        }
                    })
                    .primary("Stop job", {
                        let events_in = events_in.clone();
                        move |_e, _w, _cx| {
                            events_in.borrow_mut().push("primary".to_string());
                        }
                    })
                    .on_close({
                        let events_in = events_in.clone();
                        let closed_in = closed_in.clone();
                        move |_w, _cx| {
                            events_in.borrow_mut().push("close".to_string());
                            closed_in.set(true);
                        }
                    }),
            )
        });
        // The trap focuses the primary; Enter on it is a GPUI keyboard
        // click, and the button's handler runs the full step.
        let ks = gpui::Keystroke::parse("enter").unwrap();
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: ks.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent { keystroke: ks });
        assert_eq!(
            events.borrow().as_slice(),
            ["close", "primary"],
            "Enter closes and runs the primary step"
        );
        assert!(
            wcx.update(|w, cx| w.focused(cx)) == Some(trigger.clone()),
            "focus is back on the trigger"
        );
    }

    /// The dialog is modal: a click on the scrim does not close it, and
    /// the panel is `PANEL_W` wide and centred.
    #[gpui::test]
    fn dialog_is_modal_centered_and_panel_w_wide(cx: &mut TestAppContext) {
        // A corner of the window, away from the centred panel.
        const CORNER: f32 = 40.0;
        let closed = std::rc::Rc::new(std::cell::Cell::new(0));
        let closed_in = closed.clone();
        let primary = cx.update(|cx| cx.focus_handle());
        let p = primary.clone();
        // (The dialog's root is `size_full()`: it is the factory's root
        // element here, so it fills the window — a content-sized wrapper
        // would collapse it.)
        let (_root, wcx) = render_part(cx, move || {
            let closed_in = closed_in.clone();
            let p = p.clone();
            Dialog::new("confirm", "Stop the job?", "It stops on its current step.")
                .focus_handles(None, None, Some(&p))
                .primary("Stop job", |_e, _w, _cx| {})
                .on_close(move |_w, _cx| {
                    closed_in.set(1);
                })
        });
        // A click on the scrim (away from the panel) does not close.
        let at = gpui::point(px(CORNER), px(CORNER));
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert_eq!(closed.get(), 0, "the scrim does not close the modal");

        let panel = wcx
            .debug_bounds("ui-dialog-confirm-panel")
            .unwrap_or_else(|| panic!("the dialog panel recorded no bounds"));
        assert_eq!(
            f32::from(panel.size.width),
            f32::from(theme::z(theme::PANEL_W)),
            "the dialog panel is PANEL_W wide: {panel:?}",
        );
        let win = wcx.update(|w, _cx| w.bounds());
        let centre = f32::from(panel.origin.x) + f32::from(panel.size.width) / 2.0;
        assert!(
            (centre - f32::from(win.size.width) / 2.0).abs() < 2.0,
            "the panel is centred: {panel:?} in {win:?}",
        );
    }

    /// `.destructive()` paints the primary as a ghost button (the
    /// confirm action, with no red).
    #[gpui::test]
    fn dialog_destructive_paints_the_primary_as_ghost(cx: &mut TestAppContext) {
        let primary = cx.update(|cx| cx.focus_handle());
        let p = primary.clone();
        let (_root, wcx) = render_part(cx, move || {
            let p = p.clone();
            div().flex().child(
                Dialog::new(
                    "wipe",
                    "Delete the project?",
                    "Its runs and artefacts are gone.",
                )
                .focus_handles(None, None, Some(&p))
                .destructive()
                .primary("Delete, I know what I'm losing", |_e, _w, _cx| {}),
            )
        });
        let button = wcx
            .debug_bounds("ui-button-wipe-primary")
            .unwrap_or_else(|| panic!("the primary button recorded no bounds"));
        assert!(
            f32::from(button.size.width) > 0.0,
            "the primary button is painted: {button:?}",
        );
    }

    fn key(wcx: &mut gpui::VisualTestContext, k: &str) {
        let ks = gpui::Keystroke::parse(k).unwrap();
        wcx.simulate_event(gpui::KeyDownEvent {
            keystroke: ks.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        wcx.simulate_event(gpui::KeyUpEvent { keystroke: ks });
        wcx.run_until_parked();
    }

    /// A dialog that records its steps; `None` handles = self-contained.
    fn recording_dialog(
        events: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
        with_cancel: bool,
    ) -> Dialog {
        let push = |tag: &'static str| {
            let events = events.clone();
            move |_e: &ClickEvent, _w: &mut Window, _cx: &mut App| {
                events.borrow_mut().push(tag.to_string())
            }
        };
        let mut d = Dialog::new("self", "Stop the job?", "It stops on its current step.")
            .primary("Stop job", push("primary"));
        if with_cancel {
            d = d.cancel("Cancel", push("cancel"));
        }
        d
    }

    /// K3b item 3: without `.focus_handles(…)` the dialog is
    /// self-contained — every action renders, focus starts on the default
    /// (primary) button, Tab and Shift+Tab cycle and wrap, and focus
    /// never leaves the dialog.
    #[gpui::test]
    fn dialog_without_handles_is_self_contained(cx: &mut TestAppContext) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let ev = events.clone();
        let (_root, wcx) = render_part(cx, move || {
            div().size_full().child(recording_dialog(ev.clone(), true))
        });
        let id = ElementId::from("self");
        for tag in ["cancel", "primary"] {
            let sel: &'static str =
                Box::leak(format!("ui-button-{}", child_id(&id, tag)).into_boxed_str());
            assert!(wcx.debug_bounds(sel).is_some(), "the {tag} button renders");
        }
        let start = wcx
            .update(|w, cx| w.focused(cx))
            .expect("focus starts inside");
        key(wcx, "tab");
        let second = wcx.update(|w, cx| w.focused(cx)).expect("still inside");
        assert_ne!(start, second, "Tab moves to the other button");
        key(wcx, "tab");
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            Some(start.clone()),
            "Tab wraps"
        );
        key(wcx, "shift-tab");
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            Some(second),
            "Shift+Tab goes back"
        );
        assert!(events.borrow().is_empty(), "no step ran while moving focus");
    }

    /// K3b item 3: Enter runs the primary step even when another button
    /// has focus (and the focused button does not also fire); Escape runs
    /// cancel.
    #[gpui::test]
    fn dialog_enter_is_the_primary_and_escape_the_cancel(cx: &mut TestAppContext) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let ev = events.clone();
        let (_root, wcx) = render_part(cx, move || {
            div().size_full().child(recording_dialog(ev.clone(), true))
        });
        key(wcx, "shift-tab"); // primary → cancel
        key(wcx, "enter");
        assert_eq!(
            events.borrow().as_slice(),
            ["primary"],
            "Enter ran the primary only"
        );
        events.borrow_mut().clear();
        key(wcx, "escape");
        assert_eq!(events.borrow().as_slice(), ["cancel"], "Escape ran cancel");
    }

    /// K3b item 3: with a single button, Tab keeps focus on it — it never
    /// escapes the dialog.
    #[gpui::test]
    fn dialog_single_button_traps_tab(cx: &mut TestAppContext) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let ev = events.clone();
        let outside = cx.update(|cx| cx.focus_handle());
        let o = outside.clone();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .size_full()
                .child(crate::ui::button::Button::new("outside", "Outside").focus_handle(&o))
                .child(recording_dialog(ev.clone(), false))
        });
        let only = wcx
            .update(|w, cx| w.focused(cx))
            .expect("focus starts on the button");
        for k in ["tab", "shift-tab", "tab"] {
            key(wcx, k);
            assert_eq!(
                wcx.update(|w, cx| w.focused(cx)),
                Some(only.clone()),
                "{k} stays"
            );
        }
        assert_ne!(Some(only), Some(outside));
    }

    /// K3b round 2 (DeepSeek): without a primary, Enter runs the default
    /// step (the cancel); and a self-contained dialog still restores focus
    /// to the trigger on close.
    #[gpui::test]
    fn dialog_enter_default_and_restore_without_handles(cx: &mut TestAppContext) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        let trigger = cx.update(|cx| cx.focus_handle());
        let (ev, cl, t) = (events.clone(), closed.clone(), trigger.clone());
        let (_root, wcx) = render_part(cx, move || {
            if cl.get() {
                return div().size_full();
            }
            let (ev2, cl2) = (ev.clone(), cl.clone());
            div().size_full().child(
                Dialog::new("only", "Nothing to undo", "There is nothing to restore.")
                    .trigger_focus(&t)
                    .cancel("OK", move |_e, _w, _cx| ev2.borrow_mut().push("cancel"))
                    .on_close(move |_w, _cx| cl2.set(true)),
            )
        });
        key(wcx, "enter");
        assert_eq!(
            events.borrow().as_slice(),
            ["cancel"],
            "Enter ran the default (cancel)"
        );
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            Some(trigger),
            "focus is back on the trigger"
        );
    }
}
