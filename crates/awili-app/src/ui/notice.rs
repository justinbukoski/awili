//! `ui::Notice` and `ui::NoticeStack` — the kit's corner toasts (KIT.md
//! v3, batch K3; they replace the single toast at `main.rs:4540`).
//!
//! A `Notice` is a floating corner card (the shared `Panel` face) with
//! a tone `Dot` (the `ActNow` dot **pulses**, driven by the parent
//! through `.phase(…)`), `type_::UI` text, an optional action button
//! and a `X` close `IconButton`. `.dismiss_pref(key)` adds a quiet
//! "Don't show again" button; its handler is the parent's persistence
//! hook (the `key` identifies the setting).
//!
//! A `NoticeStack` holds the live notices (the parent owns a
//! [`NoticeStackState`]) and shows **at most 3, the oldest going
//! first**. Each notice carries a **scope** ([`NoticeScope::Global`] or
//! [`NoticeScope::Page(id)`]) and a **lifetime** ([`NoticeLifetime::
//! UntilDismissed`] or [`NoticeLifetime::Timed(secs)`], default 12 s
//! for info and **never** for `ActNow`). Timed notices expire on a
//! background timer that removes them and refreshes the window — and
//! every removal is guarded by a **generation counter**, so an
//! expiring timer for a replaced notice never removes its successor
//! (this preserves the existing guard at `main.rs:4540`: monotonic,
//! bumped per notice; a removal only fires if still current).
//!
//! The parent places the stack's column in the corner; the stack
//! measures its content. The ActNow pulse phase is the parent's state
//! (`.phase(…)` on the stack is forwarded to the pulsing dot's
//! `.phase(…)`), exactly like the island's pulse.

use std::{cell::Cell, cell::RefCell, rc::Rc, time::Duration};

use gpui::{
    div, prelude::*, AnyElement, App, AsyncApp, ClickEvent, ElementId, IntoElement, Pixels,
    RenderOnce, SharedString, Window,
};

use crate::theme::{self, palette, Tone};
use crate::ui::button::Button;
use crate::ui::dot::Dot;
use crate::ui::icon_button::IconButton;
use crate::ui::interaction::{child_id, ClickHandler, NOTICE, NOTICE_STACK};

/// A click handler shared through `Rc`: the stack's entries are cloned
/// by `visible()` (the handlers themselves are not `Clone`).
type SharedClick = std::rc::Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// The scope a notice belongs to (KIT.md § Notice: `Global` or
/// `Page(id)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoticeScope {
    /// Visible in every stack.
    Global,
    /// Visible only in the stack of that page (never in a `Global` stack
    /// and never on another page).
    Page(SharedString),
}

/// The lifetime of a notice (KIT.md § Notice: `Until dismissed` or
/// `Timed(secs)`, default 12 s for info and never for `ActNow`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLifetime {
    /// Stays until the user dismisses it (the `ActNow` default).
    UntilDismissed,
    /// Expires after the given seconds (a background timer removes it).
    Timed(u64),
}

impl NoticeLifetime {
    /// The default lifetime for a tone: `ActNow` is never, the rest
    /// 12 s (KIT.md § Notice).
    pub fn default_for(tone: Tone) -> Self {
        if matches!(tone, Tone::ActNow) {
            Self::UntilDismissed
        } else {
            Self::Timed(theme::motion::NOTICE_TIMED_SECS)
        }
    }
}

/// A spec for a pushed notice (the `NoticeStackState::push` input).
pub struct NoticeSpec {
    tone: Tone,
    text: SharedString,
    id: Option<ElementId>,
    action: Option<(SharedString, SharedClick)>,
    lifetime: NoticeLifetime,
    scope: NoticeScope,
    dismiss_pref: Option<(SharedString, SharedClick)>,
}

impl std::fmt::Debug for NoticeSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoticeSpec")
            .field("tone", &self.tone)
            .field("text", &self.text)
            .field("lifetime", &self.lifetime)
            .field("scope", &self.scope)
            .finish()
    }
}

impl NoticeSpec {
    /// A notice with the given tone and text (also its accessible name);
    /// the lifetime is [`NoticeLifetime::default_for`] for the tone,
    /// the scope `Global`.
    pub fn new(tone: Tone, text: impl Into<SharedString>) -> Self {
        Self {
            tone,
            text: text.into(),
            id: None,
            action: None,
            lifetime: NoticeLifetime::default_for(tone),
            scope: NoticeScope::Global,
            dismiss_pref: None,
        }
    }

    /// A stable id for the notice (re-pushing the same id **replaces**
    /// the notice and bumps its generation; without it, the stack
    /// assigns one per push).
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// An action button (a kit `Button.ghost()`), run on click (the
    /// notice stays — it dismisses only through its `X`).
    pub fn action(
        mut self,
        label: impl Into<SharedString>,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.action = Some((label.into(), std::rc::Rc::new(f)));
        self
    }

    /// Override the lifetime (default [`NoticeLifetime::default_for`]).
    pub fn lifetime(mut self, lifetime: NoticeLifetime) -> Self {
        self.lifetime = lifetime;
        self
    }

    /// The scope (default `Global`).
    pub fn scope(mut self, scope: NoticeScope) -> Self {
        self.scope = scope;
        self
    }

    /// Add the "Don't show again" button, with its persistence `key`
    /// (the parent's handler stores the preference under it).
    pub fn dismiss_pref(
        mut self,
        key: impl Into<SharedString>,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.dismiss_pref = Some((key.into(), std::rc::Rc::new(f)));
        self
    }
}

/// A live notice in a [`NoticeStackState`].
#[derive(Clone)]
pub struct NoticeEntry {
    id: ElementId,
    /// The generation (monotonic; bumped when the same id is replaced).
    gen: u64,
    tone: Tone,
    text: SharedString,
    action: Option<(SharedString, SharedClick)>,
    lifetime: NoticeLifetime,
    scope: NoticeScope,
    dismiss_pref: Option<(SharedString, SharedClick)>,
}

impl std::fmt::Debug for NoticeEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoticeEntry")
            .field("id", &self.id)
            .field("gen", &self.gen)
            .field("tone", &self.tone)
            .field("text", &self.text)
            .field("lifetime", &self.lifetime)
            .field("scope", &self.scope)
            .finish()
    }
}

impl NoticeEntry {
    /// The notice's id.
    pub fn id(&self) -> &ElementId {
        &self.id
    }

    /// The current generation (the guard: a removal fires only for it).
    pub fn generation(&self) -> u64 {
        self.gen
    }

    /// The notice's tone.
    pub fn tone(&self) -> Tone {
        self.tone
    }

    /// The notice's text.
    pub fn text(&self) -> &SharedString {
        &self.text
    }

    /// The lifetime.
    pub fn lifetime(&self) -> NoticeLifetime {
        self.lifetime
    }

    /// The scope.
    pub fn scope(&self) -> &NoticeScope {
        &self.scope
    }
}

/// The parent-owned state of a notice stack (like
/// [`super::scrollbar::ScrollBarState`]): the part is a stateless
/// `RenderOnce`, so the entries and their generations live here.
#[derive(Clone, Debug)]
pub struct NoticeStackState {
    entries: Rc<RefCell<Vec<NoticeEntry>>>,
    next_gen: Rc<Cell<u64>>,
    scope: Rc<RefCell<NoticeScope>>,
}

impl Default for NoticeStackState {
    fn default() -> Self {
        Self {
            entries: Rc::new(RefCell::new(Vec::new())),
            next_gen: Rc::new(Cell::new(0)),
            scope: Rc::new(RefCell::new(NoticeScope::Global)),
        }
    }
}

impl NoticeStackState {
    /// The empty stack (scope `Global`).
    pub fn new() -> Self {
        Self::default()
    }

    /// The stack's scope: `Page(x)` shows `Global` notices plus page x's;
    /// a `Global` stack (no current page) shows only `Global` notices.
    pub fn scope(&self) -> NoticeScope {
        self.scope.borrow().clone()
    }

    /// Set the stack's scope and drop the other pages' `Page` entries
    /// (KIT.md § Notice).
    pub fn set_scope(&self, scope: NoticeScope) {
        self.entries
            .borrow_mut()
            .retain(|e| Self::shows_in(&scope, &e.scope));
        *self.scope.borrow_mut() = scope;
    }

    /// Whether a notice of scope `notice` shows in a stack of scope
    /// `stack`: `Global` notices show everywhere; a `Page(x)` notice shows
    /// only in page x's stack (not in a `Global` stack, not on page y).
    fn shows_in(stack: &NoticeScope, notice: &NoticeScope) -> bool {
        match (stack, notice) {
            (_, NoticeScope::Global) => true,
            (NoticeScope::Page(a), NoticeScope::Page(b)) => a == b,
            (NoticeScope::Global, NoticeScope::Page(_)) => false,
        }
    }

    /// The entries currently visible in this scope (at most 3).
    pub fn visible(&self) -> Vec<NoticeEntry> {
        let scope = self.scope.borrow();
        self.entries
            .borrow()
            .iter()
            .filter(|e| Self::shows_in(&scope, &e.scope))
            .cloned()
            .collect()
    }

    /// Push a notice: the oldest goes first beyond 3; re-pushing an
    /// existing id replaces it and **bumps its generation** (the
    /// guard). Schedules the expiry timer for `Timed` lifetimes.
    pub fn push(&self, spec: NoticeSpec, cx: &mut App) {
        let id = spec
            .id
            .clone()
            .unwrap_or_else(|| ElementId::from(format!("notice-{}", self.next_gen.get() + 1)));
        let mut entries = self.entries.borrow_mut();
        let gen = match entries.iter().position(|e| e.id == id) {
            Some(pos) => {
                // The same id: a replacement takes the next value of the
                // one monotonic counter (never reset, never derived from
                // the old entry), so every old timer's guard fails.
                let next = self.next_gen.get() + 1;
                self.next_gen.set(next);
                let entry = Self::entry(&id, next, &spec);
                entries[pos] = entry;
                next
            }
            None => {
                let next = self.next_gen.get() + 1;
                self.next_gen.set(next);
                entries.push(Self::entry(&id, next, &spec));
                if entries.len() > MAX_VISIBLE {
                    entries.remove(0); // the oldest goes first
                }
                next
            }
        };
        drop(entries);
        if let NoticeLifetime::Timed(secs) = spec.lifetime {
            self.schedule_expiry(id.clone(), gen, secs, cx);
        }
    }

    fn entry(id: &ElementId, gen: u64, spec: &NoticeSpec) -> NoticeEntry {
        NoticeEntry {
            id: id.clone(),
            gen,
            tone: spec.tone,
            text: spec.text.clone(),
            action: spec.action.clone(),
            lifetime: spec.lifetime,
            scope: spec.scope.clone(),
            dismiss_pref: spec.dismiss_pref.clone(),
        }
    }

    /// Dismiss the notice with the given id (the `X` button).
    pub fn dismiss(&self, id: &ElementId) {
        self.entries.borrow_mut().retain(|e| &e.id != id);
    }

    /// The generation guard (the `main.rs:4540` guard): remove the
    /// notice only if its current generation is `gen` — an expiring
    /// timer of a replaced notice never removes its successor.
    pub fn remove_if_current(&self, id: &ElementId, gen: u64) -> bool {
        let mut entries = self.entries.borrow_mut();
        let Some(pos) = entries.iter().position(|e| e.id == *id) else {
            return false;
        };
        if entries[pos].gen == gen {
            entries.remove(pos);
            true
        } else {
            false
        }
    }

    /// The expiry timer (the kit's `schedule_hide` pattern): after
    /// `secs`, remove the notice **if still current** and refresh the
    /// windows.
    fn schedule_expiry(&self, id: ElementId, gen: u64, secs: u64, cx: &mut App) {
        let state = self.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            cx.background_executor()
                .timer(Duration::from_secs(secs))
                .await;
            cx.update(|app: &mut App| {
                if state.remove_if_current(&id, gen) {
                    for handle in app.windows() {
                        let _ = app.update_window(handle, |_view, w, _cx| {
                            w.refresh();
                        });
                    }
                }
            });
        })
        .detach();
    }
}

/// The stack's capacity (KIT.md § Notice: "at most 3, and the oldest
/// goes first").
const MAX_VISIBLE: usize = theme::NOTICE_MAX_VISIBLE;

/// A kit corner notice card. Build with [`Notice::new`]; for the stack
/// form, use [`NoticeStack`] instead (it builds these from its state).
#[derive(IntoElement)]
pub struct Notice {
    common: crate::ui::interaction::Common,
    tone: Tone,
    text: SharedString,
    phase: Option<u8>,
    action: Option<(SharedString, SharedClick)>,
    dismiss_pref: Option<(SharedString, SharedClick)>,
    on_dismiss: Option<ClickHandler>,
}

impl Notice {
    /// A corner notice card with the given tone and text (also its
    /// accessible name; the tone dot is labeled with it).
    pub fn new(id: impl Into<ElementId>, tone: Tone, text: impl Into<SharedString>) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            tone,
            text: text.into(),
            phase: None,
            action: None,
            dismiss_pref: None,
            on_dismiss: None,
        }
    }

    /// The pulse phase of the `ActNow` dot (the parent drives it, like
    /// the island's pill: a re-render with the next frame's phase).
    pub fn phase(mut self, phase: u8) -> Self {
        self.phase = Some(phase);
        self
    }

    /// An action button (a kit `Button.ghost()`).
    pub fn action(
        mut self,
        label: impl Into<SharedString>,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.action = Some((label.into(), std::rc::Rc::new(f)));
        self
    }

    /// The "Don't show again" quiet button, with its persistence `key`
    /// (the handler stores the preference; the part labels the button).
    pub fn dismiss_pref(
        mut self,
        key: impl Into<SharedString>,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.dismiss_pref = Some((key.into(), std::rc::Rc::new(f)));
        self
    }

    /// The close handler (the `X` button; the stack wires this to
    /// [`NoticeStackState::dismiss`]).
    pub fn on_dismiss(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Box::new(f));
        self
    }

    /// The tone.
    pub fn tone(&self) -> Tone {
        self.tone
    }

    /// The text.
    pub fn text(&self) -> &SharedString {
        &self.text
    }
}

crate::kit_shared!(Notice);

impl std::fmt::Debug for Notice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Notice({})", self.common.selector_for(NOTICE))
    }
}

impl RenderOnce for Notice {
    fn render(self, _window: &mut Window, _cx: &mut gpui::App) -> impl IntoElement {
        let common = self.common;
        let selector = common.selector_for(NOTICE);
        let tone = self.tone;
        let text = self.text.clone();
        let phase = self.phase.unwrap_or(0);
        let action = self.action;
        let dismiss_pref = self.dismiss_pref;
        let on_dismiss = self.on_dismiss;

        // The tone dot: the ActNow dot pulses (the parent drives the
        // phase); the dot carries its own id and the notice's text as
        // its accessible label.
        // Disabled, the notice acts on nothing: every control is disabled
        // with its reason, the dot dims (no pulse, no id of its own) and the
        // text dims.
        let reason = common.disabled_reason.clone();
        // The notice's double-click setting reaches its own controls.
        let allow_double = common.allow_double_click;
        let mut dot = Dot::new(tone).label(text.clone());
        if reason.is_some() {
            dot = dot.dimmed();
        } else {
            dot = dot.id(child_id(&common.id, "dot"));
            if matches!(tone, Tone::ActNow) {
                dot = dot.pulse().phase(phase);
            }
        }

        let mut row = div()
            .flex()
            .items_center()
            .gap(theme::z(theme::SP_12))
            .child(dot)
            .child({
                let t = crate::ui::text::ui_text(text).wrap();
                if reason.is_some() {
                    t.dim()
                } else {
                    t
                }
            });
        if let Some((label, handler)) = action {
            let b = Button::new(child_id(&common.id, "action"), label)
                .ghost()
                .allow_double_click_if(allow_double);
            row = row.child(match reason.clone() {
                Some(r) => b.disabled(r),
                None => b.on_click(move |ev, w, cx| handler(ev, w, cx)),
            });
        }
        if let Some((key, handler)) = dismiss_pref {
            let b = Button::new(child_id(&common.id, "pref"), "Don't show again")
                .quiet()
                .allow_double_click_if(allow_double);
            row = row.child(match reason.clone() {
                Some(r) => b.disabled(r),
                None => b.on_click(move |ev, w, cx| {
                    handler(ev, w, cx);
                    let _ = &key;
                }),
            });
        }
        if let Some(on_dismiss) = on_dismiss {
            let b = IconButton::new(child_id(&common.id, "close"), crate::icons::Icon::X)
                .label("Dismiss")
                .quiet()
                .allow_double_click_if(allow_double);
            row = row.child(match reason.clone() {
                Some(r) => b.disabled(r),
                None => b.on_click(move |ev, w, cx| on_dismiss(ev, w, cx)),
            });
        }

        super::panel::float_face(
            div()
                .flex()
                .items_center()
                .px(theme::z(theme::SP_12))
                .py(theme::z(theme::SP_8))
                .child(row),
        )
        .id(common.id.clone())
        .debug_selector(|| selector)
        // The reason opens over the whole face (KIT.md: a disabled part's
        // reason is the tooltip that actually opens over it).
        .when_some(common.tooltip(), |d, tip| {
            d.tooltip(crate::icons::tip(tip.to_string()))
        })
    }
}

/// A kit notice stack: the corner column of the live notices (at most
/// 3, the oldest going first), from the parent's
/// [`NoticeStackState`]. The parent places it in the corner.
#[derive(IntoElement)]
pub struct NoticeStack {
    common: crate::ui::interaction::Common,
    state: NoticeStackState,
    phase: u8,
}

impl NoticeStack {
    /// A stack over the parent's state (the part is a stateless
    /// `RenderOnce`).
    pub fn new(id: impl Into<ElementId>, state: NoticeStackState) -> Self {
        Self {
            common: crate::ui::interaction::Common {
                id: id.into(),
                ..Default::default()
            },
            state,
            phase: 0,
        }
    }

    /// The pulse phase, forwarded to the pulsing `ActNow` dots (the
    /// parent drives it, like the island's pill).
    pub fn phase(mut self, phase: u8) -> Self {
        self.phase = phase;
        self
    }
}

crate::kit_shared!(NoticeStack);

impl std::fmt::Debug for NoticeStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NoticeStack({})", self.common.selector_for(NOTICE_STACK))
    }
}

impl RenderOnce for NoticeStack {
    fn render(self, _window: &mut Window, _cx: &mut gpui::App) -> impl IntoElement {
        let common = self.common;
        let selector = common.selector_for(NOTICE_STACK);
        let state = self.state.clone();
        let phase = self.phase;

        let mut col = div().flex().flex_col().gap(theme::z(theme::SP_8));
        for entry in state.visible() {
            let id = entry.id.clone();
            let mut notice =
                Notice::new(entry.id.clone(), entry.tone(), entry.text().clone()).phase(phase);
            // The stack's double-click setting reaches every notice's controls.
            notice = notice.allow_double_click_if(common.allow_double_click);
            // A disabled stack hands its reason to every notice.
            if let Some(reason) = common.disabled_reason.clone() {
                notice = notice.disabled(reason);
            }
            if let Some((label, handler)) = entry.action.clone() {
                notice = notice.action(label, move |ev, w, cx| handler(ev, w, cx));
            }
            if let Some((key, handler)) = entry.dismiss_pref.clone() {
                notice = notice.dismiss_pref(key, move |ev, w, cx| handler(ev, w, cx));
            }
            notice = notice.on_dismiss({
                let state = state.clone();
                move |_ev, w, _cx| {
                    state.dismiss(&id);
                    w.refresh();
                }
            });
            col = col.child(notice);
        }

        div()
            .flex()
            .flex_col()
            .debug_selector(|| selector)
            .child(col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// The default lifetimes: `ActNow` is never, the rest 12 s.
    #[test]
    fn notice_default_lifetimes() {
        assert_eq!(
            NoticeLifetime::default_for(Tone::ActNow),
            NoticeLifetime::UntilDismissed,
            "ActNow notices never expire"
        );
        assert_eq!(
            NoticeLifetime::default_for(Tone::Attention),
            NoticeLifetime::Timed(12),
            "info/attention notices expire after 12 s"
        );
        assert_eq!(
            NoticeLifetime::default_for(Tone::Neutral),
            NoticeLifetime::Timed(12)
        );
        assert_eq!(
            NoticeLifetime::default_for(Tone::Contrast),
            NoticeLifetime::Timed(12)
        );
    }

    /// At most 3 notices: pushing a fourth evicts the oldest.
    #[gpui::test]
    fn notice_stack_holds_at_most_three_and_evicts_the_oldest(cx: &mut TestAppContext) {
        let state = NoticeStackState::new();
        let s = state.clone();
        cx.update(|app| {
            for i in 0..4 {
                s.push(
                    NoticeSpec::new(Tone::Neutral, format!("notice {i}")).id(format!("n{i}")),
                    app,
                );
            }
        });
        let visible = state.visible();
        assert_eq!(visible.len(), 3, "at most 3 notices");
        assert_eq!(
            visible
                .iter()
                .map(|e| e.text().to_string())
                .collect::<Vec<_>>(),
            vec!["notice 1", "notice 2", "notice 3"],
            "the oldest (notice 0) went first"
        );
    }

    /// The generation guard: re-pushing the same id bumps the
    /// generation, and a stale removal (the old generation) never
    /// removes the newer notice; the current one does.
    #[gpui::test]
    fn notice_generation_guard_keeps_a_newer_notice(cx: &mut TestAppContext) {
        let state = NoticeStackState::new();
        let s = state.clone();
        cx.update(|app| {
            s.push(
                NoticeSpec::new(Tone::Neutral, "old")
                    .id("job")
                    .lifetime(NoticeLifetime::Timed(1)),
                app,
            );
        });
        let gen1 = state.visible()[0].generation();
        let s2 = state.clone();
        cx.update(|app| {
            s2.push(
                NoticeSpec::new(Tone::Neutral, "new")
                    .id("job")
                    .lifetime(NoticeLifetime::Timed(1)),
                app,
            );
        });
        let gen2 = state.visible()[0].generation();
        assert!(
            gen2 > gen1,
            "the replacement bumps the generation: {gen1} → {gen2}"
        );
        assert!(
            !state.remove_if_current(&ElementId::from("job"), gen1),
            "the stale timer's generation does not remove the newer notice"
        );
        assert_eq!(state.visible().len(), 1, "the newer notice survived");
        assert!(
            state.remove_if_current(&ElementId::from("job"), gen2),
            "the current generation removes the notice"
        );
        assert!(state.visible().is_empty(), "and it is gone");
    }

    /// Scopes: a `Page` stack sees its own page's notices and the
    /// global ones, and `set_scope` drops the other pages' entries.
    #[gpui::test]
    fn notice_scopes_filter_the_visible_entries(cx: &mut TestAppContext) {
        let state = NoticeStackState::new();
        let s = state.clone();
        cx.update(|app| {
            s.push(
                NoticeSpec::new(Tone::Neutral, "global").scope(NoticeScope::Global),
                app,
            );
            s.push(
                NoticeSpec::new(Tone::Neutral, "workshop")
                    .scope(NoticeScope::Page("workshop".into())),
                app,
            );
            s.push(
                NoticeSpec::new(Tone::Neutral, "council")
                    .scope(NoticeScope::Page("council".into())),
                app,
            );
        });
        state.set_scope(NoticeScope::Page("workshop".into()));
        let visible = state.visible();
        assert_eq!(
            visible
                .iter()
                .map(|e| e.text().to_string())
                .collect::<Vec<_>>(),
            vec!["global", "workshop"],
            "the page stack sees global + its own page, not other pages"
        );
        // The other page's entry was dropped by set_scope.
        state.set_scope(NoticeScope::Page("council".into()));
        let visible = state.visible();
        assert_eq!(
            visible
                .iter()
                .map(|e| e.text().to_string())
                .collect::<Vec<_>>(),
            vec!["global"],
            "set_scope drops the other page's entries"
        );
    }

    /// A timed notice expires on the background clock: the window
    /// refreshes and the notice is gone (the `run_until_parked` frame
    /// re-renders the stack without it).
    #[gpui::test]
    fn notice_timed_expiry_removes_the_notice(cx: &mut TestAppContext) {
        let state = NoticeStackState::new();
        let s = state.clone();
        cx.update(|cx| {
            cx.spawn(async move |cx: &mut AsyncApp| {
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
                cx.update(|app| {
                    s.push(
                        NoticeSpec::new(Tone::Neutral, "fading").lifetime(NoticeLifetime::Timed(1)),
                        app,
                    );
                });
            })
            .detach();
        });
        // Let the spawned push happen (its 10 ms timer) — but not the
        // 1 s expiry timer.
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        assert_eq!(state.visible().len(), 1, "the notice was pushed");
        // The expiry timer: 1 s. Advance past it, let it run.
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        assert!(
            state.visible().is_empty(),
            "the timed notice expired and was removed"
        );
    }

    /// The end-to-end guard: a replaced timed notice's stale timer
    /// fires first and removes nothing; the successor's timer removes
    /// it.
    #[gpui::test]
    fn notice_expiry_guard_keeps_a_replaced_notice(cx: &mut TestAppContext) {
        let state = NoticeStackState::new();
        let s = state.clone();
        cx.update(|cx| {
            cx.spawn(async move |cx: &mut AsyncApp| {
                cx.update(|app| {
                    s.push(
                        NoticeSpec::new(Tone::Neutral, "first")
                            .id("job")
                            .lifetime(NoticeLifetime::Timed(1)),
                        app,
                    );
                });
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                cx.update(|app| {
                    s.push(
                        NoticeSpec::new(Tone::Neutral, "second")
                            .id("job")
                            .lifetime(NoticeLifetime::Timed(1)),
                        app,
                    );
                });
            })
            .detach();
        });
        // t=0.5: the replacement happened. t=1.0: the first timer fires
        // (stale generation — it removes nothing).
        cx.executor().advance_clock(Duration::from_millis(1200));
        cx.run_until_parked();
        assert_eq!(
            state.visible().len(),
            1,
            "the stale timer's guard kept the replaced notice"
        );
        assert_eq!(
            state.visible()[0].text().to_string(),
            "second",
            "the successor is the one still showing"
        );
        // t=1.5: the successor's timer fires (current generation).
        cx.executor().advance_clock(Duration::from_millis(600));
        cx.run_until_parked();
        assert!(
            state.visible().is_empty(),
            "the successor's own timer removed it"
        );
    }

    /// The stack renders the live notices as cards; the `ActNow`
    /// notice's dot pulses with the given phase and is exactly `DOT`
    /// wide (the part passes its own id to the dot).
    #[gpui::test]
    fn notice_stack_renders_cards_and_the_act_now_dot(cx: &mut TestAppContext) {
        let state = NoticeStackState::new();
        let s = state.clone();
        cx.update(|app| {
            s.push(
                NoticeSpec::new(Tone::ActNow, "The job failed").id("a1"),
                app,
            );
            s.push(NoticeSpec::new(Tone::Neutral, "Saved"), app);
        });
        let s2 = state.clone();
        let (_root, wcx) = render_part(cx, move || {
            let s2 = s2.clone();
            div().flex().child(NoticeStack::new("corner", s2).phase(7))
        });
        // The dot's debug_selector is the constant "ui-dot" (K2).
        let dot = wcx
            .debug_bounds("ui-dot")
            .unwrap_or_else(|| panic!("the ActNow notice's dot recorded no bounds"));
        assert_eq!(
            (f32::from(dot.size.width), f32::from(dot.size.height)),
            (
                f32::from(theme::z(theme::DOT)),
                f32::from(theme::z(theme::DOT))
            ),
            "the dot is exactly DOT: {dot:?}",
        );
        let card = wcx
            .debug_bounds("ui-notice-a1")
            .unwrap_or_else(|| panic!("the ActNow card recorded no bounds"));
        assert!(
            f32::from(card.size.width) > f32::from(theme::z(theme::DOT)),
            "the card wraps the dot and its text: {card:?}",
        );
    }

    /// A real click (down and up) at the centre of `selector`.
    fn click(wcx: &mut gpui::VisualTestContext, selector: &'static str) {
        let at = wcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} recorded no bounds"))
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
    }

    /// A disabled stack hands its reason to every notice, and a disabled
    /// notice acts on nothing: its action, "Don't show again" and × fire no
    /// handler (the notice stays), and Tab lands on none of them.
    #[gpui::test]
    fn disabled_notice_stack_acts_on_nothing(cx: &mut TestAppContext) {
        let state = NoticeStackState::new();
        let s = state.clone();
        let fired = std::rc::Rc::new(std::cell::Cell::new(0));
        let (f1, f2) = (fired.clone(), fired.clone());
        cx.update(|app| {
            s.push(
                NoticeSpec::new(Tone::Attention, "The job's result is ready")
                    .id("n1")
                    .action("Open", move |_e, _w, _cx| f1.set(f1.get() + 1))
                    .dismiss_pref("ready", move |_e, _w, _cx| f2.set(f2.get() + 1)),
                app,
            );
        });
        let s2 = state.clone();
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .child(NoticeStack::new("corner", s2.clone()).disabled("Paused"))
        });
        for sel in [
            "ui-button-n1-action",
            "ui-button-n1-pref",
            "ui-icon-button-n1-close",
        ] {
            click(wcx, sel);
        }
        assert_eq!(fired.get(), 0, "no control fires");
        assert_eq!(state.visible().len(), 1, "the notice stays");
        // The dot dims but is still drawn, and has no tooltip of its own (no
        // id while disabled).
        assert!(
            wcx.debug_bounds("ui-dot").is_some(),
            "the dimmed dot is drawn"
        );
        // The dot has no tooltip of its own: the notice's reason opens over
        // it, and over the text.
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at(wcx, "ui-dot").as_deref(),
            Some("Paused")
        );
        assert_eq!(
            crate::ui::interaction::test_util::tooltip_at(wcx, "ui-notice-n1").as_deref(),
            Some("Paused")
        );
        wcx.update(|w, cx| w.focus_next(cx));
        assert!(
            wcx.update(|w, cx| w.focused(cx)).is_none(),
            "Tab lands on none"
        );
    }

    /// The close button removes the notice from the stack (and
    /// refreshes); the "Don't show again" button fires its handler.
    #[gpui::test]
    fn notice_close_and_dismiss_pref(cx: &mut TestAppContext) {
        let state = NoticeStackState::new();
        let s = state.clone();
        let pref = std::rc::Rc::new(std::cell::Cell::new(0));
        let pref_in = pref.clone();
        cx.update(|app| {
            s.push(
                NoticeSpec::new(Tone::Neutral, "Saved")
                    .id("n1")
                    .dismiss_pref("autosave", move |_e, _w, _cx| {
                        pref_in.set(1);
                    }),
                app,
            );
        });
        let s2 = state.clone();
        let (_root, wcx) = render_part(cx, move || {
            let s2 = s2.clone();
            div().flex().child(NoticeStack::new("corner", s2))
        });
        // "Don't show again" fires its handler (the notice stays).
        let b = wcx
            .debug_bounds("ui-button-n1-pref")
            .unwrap_or_else(|| panic!("the dismiss-pref button recorded no bounds"));
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: b.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: b.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert_eq!(pref.get(), 1, "the dismiss-pref handler fired");
        assert_eq!(state.visible().len(), 1, "the notice stays after the pref");

        // The X button dismisses it.
        let x = wcx
            .debug_bounds("ui-icon-button-n1-close")
            .unwrap_or_else(|| panic!("the close button recorded no bounds"));
        wcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: x.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        wcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: x.center(),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
        assert!(
            state.visible().is_empty(),
            "the X button dismissed the notice"
        );
    }

    /// K3b item 1, the review's exact repro: push A, B, C; replace A;
    /// dismiss and re-push C. The generation counter is monotonic, so C's
    /// old timer cannot remove the new C.
    #[gpui::test]
    fn notice_generations_are_monotonic(cx: &mut TestAppContext) {
        let state = NoticeStackState::new();
        let push = |id: &'static str, cx: &mut TestAppContext| {
            let s = state.clone();
            cx.update(|app| {
                s.push(
                    NoticeSpec::new(Tone::Neutral, id)
                        .id(id)
                        .lifetime(NoticeLifetime::Timed(60)),
                    app,
                )
            });
        };
        let gen_of = |id: &str| {
            state
                .visible()
                .iter()
                .find(|e| e.id() == &ElementId::from(id.to_string()))
                .map(|e| e.generation())
        };
        push("a", cx);
        push("b", cx);
        push("c", cx);
        let old_c = gen_of("c").unwrap();
        push("a", cx); // replace A
        state.dismiss(&ElementId::from("c"));
        push("c", cx); // re-push C
        let new_c = gen_of("c").unwrap();
        assert!(
            new_c > old_c,
            "the new C has a newer generation: {old_c} → {new_c}"
        );
        assert!(
            !state.remove_if_current(&ElementId::from("c"), old_c),
            "C's old timer does not remove the new C"
        );
        assert!(gen_of("c").is_some(), "the new C survived");
        let all: Vec<u64> = state.visible().iter().map(|e| e.generation()).collect();
        let mut dedup = all.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(
            dedup.len(),
            all.len(),
            "no two entries share a generation: {all:?}"
        );
    }

    /// K3b item 2: scopes do not leak. A `Global` stack shows only the
    /// `Global` notices; page x's stack shows `Global` + page x, never
    /// page y; `set_scope` drops what the new scope would not show.
    #[gpui::test]
    fn notice_scopes_do_not_leak(cx: &mut TestAppContext) {
        let state = NoticeStackState::new();
        let s = state.clone();
        cx.update(|app| {
            s.push(NoticeSpec::new(Tone::Neutral, "g").id("g"), app);
            s.push(
                NoticeSpec::new(Tone::Neutral, "x")
                    .id("x")
                    .scope(NoticeScope::Page("x".into())),
                app,
            );
            s.push(
                NoticeSpec::new(Tone::Neutral, "y")
                    .id("y")
                    .scope(NoticeScope::Page("y".into())),
                app,
            );
        });
        let ids = |st: &NoticeStackState| -> Vec<String> {
            st.visible().iter().map(|e| e.id().to_string()).collect()
        };
        assert_eq!(
            ids(&state),
            vec!["g".to_string()],
            "a Global stack shows only Global"
        );
        let on_x = state.clone();
        *on_x.scope.borrow_mut() = NoticeScope::Page("x".into());
        assert_eq!(
            ids(&on_x),
            vec!["g".to_string(), "x".to_string()],
            "page x: Global + x"
        );
        state.set_scope(NoticeScope::Page("x".into()));
        assert_eq!(ids(&state), vec!["g".to_string(), "x".to_string()]);
        state.set_scope(NoticeScope::Page("y".into()));
        assert_eq!(
            ids(&state),
            vec!["g".to_string()],
            "page x's notice was dropped on leaving x; page y's was dropped earlier"
        );
        state.set_scope(NoticeScope::Global);
        assert_eq!(ids(&state), vec!["g".to_string()]);
    }
}
