//! `ui::TerminalFrame` — the kit's terminal chrome (KIT.md v3, batch
//! K4).
//!
//! The chrome around the existing `TerminalView` (the renderer itself is
//! untouched — the parent builds the terminal into `.body(…)`): a
//! `PANEL_SOLID` `RADIUS_M` frame with a header row — the folder icon,
//! the folder title (it takes the room; long titles truncate with their
//! full text as the tooltip), the running `Dot` (its tone's meaning is
//! its tooltip) and, at the right, the Stop and Clear actions
//! (`IconButton.quiet`, each a 44 px hit).
//!
//! **Disabled (K4b).** The frame's reason is the reason on Stop and
//! Clear and the tooltip over the frame; no child tooltip competes. The
//! terminal is the parent's entity, so `.body(|reason| …)` builds it with
//! the reason — the parent shows it read-only (the frame cannot switch
//! off another view's input from outside).

use gpui::{
    div, prelude::*, AnyElement, App, ClickEvent, ElementId, IntoElement, RenderOnce, SharedString,
    Window,
};

use crate::icons::{self, Icon};
use crate::theme::{self, palette, Tone};
use crate::ui::dot::Dot;
use crate::ui::icon_button::IconButton;
use crate::ui::interaction::{child_id, Common};
use crate::ui::text;

/// The part tag for the default `debug_selector`.
const TERMINAL_FRAME: &str = "terminal-frame";

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type BodyBuilder = Box<dyn FnOnce(Option<SharedString>) -> AnyElement>;

/// The running dot's label for its tone (the tooltip; a meaning-carrying
/// dot must carry one).
pub fn running_label(tone: Tone) -> &'static str {
    match tone {
        Tone::ActNow => "Waiting for you",
        Tone::Attention => "Needs attention",
        Tone::Neutral => "Running",
        Tone::Contrast => "Selected",
    }
}

/// The kit's terminal chrome. Build with [`TerminalFrame::new`] and
/// chain the slots; the parent owns the terminal and the Stop / Clear
/// handlers.
#[derive(IntoElement)]
pub struct TerminalFrame {
    common: Common,
    title: Option<SharedString>,
    running: Option<Tone>,
    body: Option<BodyBuilder>,
    on_stop: Option<ClickHandler>,
    on_clear: Option<ClickHandler>,
}

/// The running dot as painted: labelled; enabled it has its own id (its
/// tooltip); disabled it dims and has none (disabled dims, never deletes).
pub(crate) fn running_dot(running: Option<Tone>, id: &ElementId, disabled: bool) -> Option<Dot> {
    let tone = running?;
    let dot = Dot::new(tone).label(running_label(tone));
    Some(if disabled {
        dot.dimmed()
    } else {
        dot.id(child_id(id, "dot"))
    })
}

impl TerminalFrame {
    /// An empty terminal frame (a `PANEL_SOLID` `RADIUS_M` shell).
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            common: Common {
                id: id.into(),
                ..Default::default()
            },
            title: None,
            running: None,
            body: None,
            on_stop: None,
            on_clear: None,
        }
    }

    /// The folder title (next to the `Folder` icon; the accessible name
    /// too).
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        let title = title.into();
        self.common.label = Some(title.clone());
        self.title = Some(title);
        self
    }

    /// The folder title, if any.
    pub fn title_text(&self) -> Option<SharedString> {
        self.title.clone()
    }

    /// The running `Dot`'s tone (`None` paints no dot).
    pub fn running(mut self, tone: Tone) -> Self {
        self.running = Some(tone);
        self
    }

    /// The terminal body, built with the frame's disabled reason (the
    /// parent shows the terminal read-only while it is `Some`).
    pub fn body<E: IntoElement + 'static>(
        mut self,
        build: impl FnOnce(Option<SharedString>) -> E + 'static,
    ) -> Self {
        self.body = Some(Box::new(move |r| build(r).into_any_element()));
        self
    }

    /// Whether the frame holds a body.
    pub fn has_body(&self) -> bool {
        self.body.is_some()
    }

    /// The Stop action's handler (the `Square` `IconButton.quiet`).
    pub fn on_stop(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_stop = Some(Box::new(f));
        self
    }

    /// The Clear action's handler (the `X` `IconButton.quiet`).
    pub fn on_clear(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_clear = Some(Box::new(f));
        self
    }
}

crate::kit_shared!(TerminalFrame, skip_label);

impl std::fmt::Debug for TerminalFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TerminalFrame({})",
            self.common.selector_for(TERMINAL_FRAME)
        )
    }
}

impl RenderOnce for TerminalFrame {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let p = palette();
        let common = self.common;
        let selector = common.selector_for(TERMINAL_FRAME);
        let reason = common.disabled_reason.clone();
        let disabled = reason.is_some();

        // The header row: folder + title (taking the room) + the running
        // dot … Stop, Clear at the right.
        let mut title = div().flex().flex_1().min_w_0();
        if let Some(t) = self.title.clone() {
            let mut slot = text::ui_text(t).strong().text_color_override(if disabled {
                p.text_dim
            } else {
                p.text
            });
            if !disabled {
                slot = slot.id(child_id(&common.id, "title"));
            }
            title = title.child(slot);
        }
        let mut header = div()
            .flex()
            .w_full()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .px(theme::z(theme::SP_12))
            .py(theme::z(theme::SP_4))
            .border_b_1()
            .border_color(p.line)
            .debug_selector(|| format!("{selector}-header"))
            .child(icons::icon(Icon::Folder, theme::ICON, p.text_dim))
            .child(title);
        if let Some(dot) = running_dot(self.running, &common.id, disabled) {
            header = header.child(
                div()
                    .debug_selector(|| format!("{selector}-dot"))
                    .flex()
                    .child(dot),
            );
        }
        let action = |tag: &str, icon: Icon, label: &str, handler: Option<ClickHandler>| {
            let b = IconButton::new(child_id(&common.id, tag), icon)
                .label(label.to_string())
                .quiet()
                .allow_double_click_if(common.allow_double_click);
            match (&reason, handler) {
                (Some(reason), _) => b.disabled(reason.clone()),
                (None, Some(h)) => b.on_click(h),
                (None, None) => b.disabled("This terminal action is unavailable"),
            }
        };
        header = header
            .child(action("stop", Icon::Square, "Stop", self.on_stop))
            .child(action(
                "clear",
                Icon::X,
                "Clear the terminal",
                self.on_clear,
            ));

        let body = self.body.map(|build| build(reason.clone()));
        let mut root = div()
            .id(common.id.clone())
            .flex()
            .flex_col()
            .w_full()
            .rounded(theme::z(theme::RADIUS_M))
            .overflow_hidden()
            .bg(p.panel_solid)
            .border_1()
            .border_color(p.line)
            .debug_selector(|| selector.clone())
            .child(header)
            .when_some(body, |d, body| {
                d.child(
                    div()
                        .flex()
                        .w_full()
                        .flex_col()
                        .px(theme::z(theme::SP_8))
                        .pb(theme::z(theme::SP_8))
                        .child(body),
                )
            });

        if let Some(tip) = common.tooltip() {
            root = root.tooltip(icons::tip(tip.to_string()));
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// A disabled frame's running dot dims and has no id; enabled it keeps
    /// its tone and its own id.
    #[test]
    fn disabled_running_dot_dims_and_has_no_id() {
        let id = ElementId::from("tf");
        let on = running_dot(Some(Tone::Neutral), &id, false).unwrap();
        assert!(on.has_id() && !on.is_dimmed());
        let off = running_dot(Some(Tone::ActNow), &id, true).unwrap();
        assert!(!off.has_id() && off.is_dimmed());
        assert_eq!(off.color(), theme::palette().text_dim);
        assert!(running_dot(None, &id, false).is_none());
    }

    use crate::ui::interaction::test_util::{render_part, tooltip_at};

    fn click(wcx: &mut gpui::VisualTestContext, sel: &'static str) {
        let at = wcx
            .debug_bounds(sel)
            .unwrap_or_else(|| panic!("{sel} recorded no bounds"))
            .center();
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
        wcx.run_until_parked();
    }

    /// The header lays out with the actions at the right edge; the
    /// actions fire; the running dot's meaning is the tooltip that opens.
    #[gpui::test]
    fn terminal_frame_header_and_actions(cx: &mut TestAppContext) {
        let fired = Rc::new(RefCell::new(Vec::new()));
        let f = fired.clone();
        let (_root, wcx) = render_part(cx, move || {
            let (f1, f2) = (f.clone(), f.clone());
            div()
                .flex()
                .w(theme::z(theme::PANEL_W * 2.))
                .pt(theme::z(theme::SP_40))
                .child(
                    TerminalFrame::new("term")
                        .title("awili-ui-fishing")
                        .running(Tone::Neutral)
                        .on_stop(move |_e, _w, _c| f1.borrow_mut().push("stop"))
                        .on_clear(move |_e, _w, _c| f2.borrow_mut().push("clear"))
                        .body(|_r| div().h(theme::z(theme::EDGE))),
                )
        });
        let header = wcx.debug_bounds("ui-terminal-frame-term-header").unwrap();
        let clear = wcx.debug_bounds("ui-icon-button-term-clear").unwrap();
        assert!(
            (header.origin.x + header.size.width) - (clear.origin.x + clear.size.width)
                <= theme::z(theme::SP_12 + theme::SP_4) + gpui::px(1.),
            "the actions sit at the right (padding plus the focus ring's room): {clear:?} in {header:?}"
        );
        click(wcx, "ui-icon-button-term-stop");
        click(wcx, "ui-icon-button-term-clear");
        assert_eq!(&*fired.borrow(), &["stop", "clear"]);
        assert_eq!(
            tooltip_at(wcx, "ui-terminal-frame-term-dot").as_deref(),
            Some(running_label(Tone::Neutral))
        );
    }

    /// K4b: disabled — Stop and Clear carry the frame's real reason (the
    /// tooltip that opens) and fire nothing; the title has no competing
    /// tooltip; the body is built with the reason.
    #[gpui::test]
    fn terminal_frame_disabled_hands_down_its_reason(cx: &mut TestAppContext) {
        let fired = Rc::new(Cell::new(0u32));
        let seen = Rc::new(RefCell::new(None::<SharedString>));
        let (f, s) = (fired.clone(), seen.clone());
        let (_root, wcx) = render_part(cx, move || {
            let (f1, s1) = (f.clone(), s.clone());
            div().flex().pt(theme::z(theme::SP_40)).child(
                TerminalFrame::new("dead")
                    .title("a-very-long-folder-name-that-truncates-in-the-header")
                    .on_stop(move |_e, _w, _c| f1.set(f1.get() + 1))
                    .body(move |r| {
                        *s1.borrow_mut() = r;
                        div().h(theme::z(theme::EDGE))
                    })
                    .disabled("The session ended"),
            )
        });
        click(wcx, "ui-icon-button-dead-stop");
        assert_eq!(fired.get(), 0);
        assert_eq!(
            tooltip_at(wcx, "ui-icon-button-dead-stop").as_deref(),
            Some("The session ended")
        );
        assert_eq!(
            tooltip_at(wcx, "ui-terminal-frame-dead-header").as_deref(),
            Some("The session ended"),
            "no title tooltip competes"
        );
        assert_eq!(
            seen.borrow().as_deref(),
            Some("The session ended"),
            "the body got it"
        );
    }
}
