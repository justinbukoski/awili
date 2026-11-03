//! Native mouse selection for rendered transcript messages.
//!
//! GPUI's `StyledText` paints rich text but does not itself own a selection.
//! This wrapper keeps the message's existing element tree, records each text
//! layout in reading order, and adds the browser-like interaction around it:
//! drag to select, a visible highlight, and the native Copy/Select All actions.

use std::{
    cell::{Cell, RefCell},
    ops::Range,
    rc::Rc,
};

use gpui::{
    point, quad, AnyElement, App, BorderStyle, Bounds, ClipboardItem, CursorStyle, DispatchPhase,
    Edges, Element, ElementId, FocusHandle, GlobalElementId, Hitbox, HitboxBehavior,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, SharedString, StyledText, TextLayout, Window,
};

use crate::transcript_selection::{self, Pos, TranscriptSelection};
use crate::{context_menu, native_menu, theme};

#[cfg(test)]
pub(crate) mod geometry_probe {
    use super::*;
    pub struct Sample {
        pub source: String,
        pub text_bounds: Vec<Bounds<Pixels>>,
    }
    std::thread_local! {
        static SAMPLES: RefCell<Option<Vec<Sample>>> = const { RefCell::new(None) };
    }
    pub fn begin() {
        SAMPLES.with(|s| *s.borrow_mut() = Some(Vec::new()));
    }
    pub fn take() -> Vec<Sample> {
        SAMPLES.with(|s| s.borrow_mut().take().unwrap_or_default())
    }
    pub(super) fn record(source: &str, segments: &[TextSegment]) {
        SAMPLES.with(|s| {
            if let Some(samples) = s.borrow_mut().as_mut() {
                samples.push(Sample {
                    source: source.to_string(),
                    text_bounds: segments.iter().map(|s| s.layout.bounds()).collect(),
                });
            }
        });
    }
}

#[derive(Clone)]
struct TextSegment {
    layout: TextLayout,
    source: Range<usize>,
}

/// Collects the text layouts and the plain-text clipboard representation while
/// `markdown_ui` builds its normal rich element tree.
pub struct TextCollector {
    source: String,
    segments: Vec<TextSegment>,
    link_click_allowed: Rc<Cell<bool>>,
    selection_fill: gpui::Rgba,
}

impl Default for TextCollector {
    fn default() -> Self {
        Self {
            source: String::new(),
            segments: Vec::new(),
            link_click_allowed: Rc::new(Cell::new(true)),
            selection_fill: theme::p().selection,
        }
    }
}

impl TextCollector {
    pub fn new(selection_fill: gpui::Rgba) -> Self {
        Self {
            selection_fill,
            ..Self::default()
        }
    }

    pub fn capture(&mut self, styled: StyledText, text: &str) -> StyledText {
        if !self.source.is_empty() {
            self.source.push('\n');
        }
        let start = self.source.len();
        self.source.push_str(text);
        let end = self.source.len();
        self.segments.push(TextSegment {
            layout: styled.layout().clone(),
            source: start..end,
        });
        styled
    }

    /// Shared with interactive link runs so a text drag cannot also activate
    /// the link beneath it. A fresh mouse-down re-enables ordinary clicks.
    pub fn link_click_allowed(&self) -> Rc<Cell<bool>> {
        self.link_click_allowed.clone()
    }

    pub fn wrap(self, id: ElementId, element: AnyElement) -> SelectableMessage {
        SelectableMessage {
            id,
            element,
            source: self.source.into(),
            segments: self.segments,
            link_click_allowed: self.link_click_allowed,
            selection_fill: self.selection_fill,
            scope: None,
            msg: 0,
        }
    }

    /// U3c: like `wrap`, but message `msg` joins conversation `scope`'s
    /// shared selection, so one selection can span messages.
    pub fn wrap_in_transcript(
        self,
        id: ElementId,
        element: AnyElement,
        scope: &str,
        msg: usize,
    ) -> SelectableMessage {
        let scope = Some(SharedString::from(scope.to_string()));
        SelectableMessage {
            scope,
            msg,
            ..self.wrap(id, element)
        }
    }
}

struct PersistentState {
    /// The private one-message model used by `wrap` (no scope).
    selection: Rc<RefCell<TranscriptSelection>>,
    focus: FocusHandle,
    link_click_allowed: Rc<Cell<bool>>,
}

pub struct SelectableMessage {
    id: ElementId,
    element: AnyElement,
    source: SharedString,
    segments: Vec<TextSegment>,
    link_click_allowed: Rc<Cell<bool>>,
    selection_fill: gpui::Rgba,
    scope: Option<SharedString>,
    msg: usize,
}

pub struct PrepaintState {
    hitbox: Hitbox,
    selection: Rc<RefCell<TranscriptSelection>>,
    focus: FocusHandle,
}

impl IntoElement for SelectableMessage {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SelectableMessage {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let layout_id = self.element.request_layout(window, cx);
        // AnyElement does not expose the inspector-aware overload; its public
        // request call is the same path used by container elements.
        let _ = inspector_id;
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.element.prepaint(window, cx);
        #[cfg(test)]
        geometry_probe::record(&self.source, &self.segments);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        window.with_element_state::<PersistentState, _>(global_id.unwrap(), |state, window| {
            let mut state = state.unwrap_or_else(|| PersistentState {
                selection: Rc::default(),
                focus: cx.focus_handle(),
                link_click_allowed: self.link_click_allowed.clone(),
            });
            // U3c: the conversation's shared model, resolved here (the only
            // place with an `App`). Registering every frame lets `paint`
            // publish exactly the messages rendered now (stale ones drop out).
            let selection = match &self.scope {
                Some(scope) => transcript_selection::shared(scope, cx),
                None => state.selection.clone(),
            };
            selection.borrow_mut().register(self.msg, &self.source);
            // Markdown rebuilds its element tree on every refresh, including
            // the refresh emitted during a drag. Carry the link guard forward
            // so that redraw cannot turn a drag back into a click (or suppress
            // an ordinary click by resetting to the wrong default).
            self.link_click_allowed.set(state.link_click_allowed.get());
            state.link_click_allowed = self.link_click_allowed.clone();
            window.set_focus_handle(&state.focus, cx);
            (
                PrepaintState {
                    hitbox,
                    selection,
                    focus: state.focus.clone(),
                },
                state,
            )
        })
    }

    fn paint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.set_cursor_style(CursorStyle::IBeam, &prepaint.hitbox);

        let selection = prepaint.selection.clone();
        // U3c: the first message painted publishes this frame's sources.
        selection.borrow_mut().commit();
        let msg = self.msg;
        window.on_action(std::any::TypeId::of::<native_menu::Copy>(), {
            let selection = selection.clone();
            move |_, phase, _window, cx| {
                if phase == DispatchPhase::Bubble {
                    if let Some(selected) = selection.borrow().selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(selected));
                    }
                }
            }
        });
        window.on_action(std::any::TypeId::of::<native_menu::SelectAll>(), {
            let selection = selection.clone();
            move |_, phase, window, _cx| {
                if phase == DispatchPhase::Bubble {
                    selection.borrow_mut().select_all();
                    window.refresh();
                }
            }
        });

        let hitbox = prepaint.hitbox.clone();
        let segments = self.segments.clone();
        let focus = prepaint.focus.clone();
        let link_click_allowed = self.link_click_allowed.clone();
        window.on_mouse_event({
            let selection = selection.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                let mac = cfg!(target_os = "macos");
                if event.button != MouseButton::Left
                    || context_menu::is_context_click(event.button, &event.modifiers, mac)
                {
                    return;
                }
                if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                    if let Some(byte) = exact_index_for_position(&segments, event.position) {
                        // A multi-click selects a word or paragraph; never a link click.
                        link_click_allowed.set(event.click_count < 2);
                        let (clicks, extend) = (event.click_count, event.modifiers.shift);
                        selection
                            .borrow_mut()
                            .begin(Pos { msg, byte }, clicks, extend);
                        window.focus(&focus, cx);
                        window.prevent_default();
                        window.refresh();
                    }
                } else if phase == DispatchPhase::Capture
                    && !event.modifiers.shift
                    && !context_menu::is_open(cx)
                {
                    // (A click on an open menu's row keeps the selection it copies.)
                    selection.borrow_mut().clear();
                    window.refresh();
                }
            }
        });

        // U3a2: a context click selects the word under the pointer unless it
        // lands inside the selection (so a stale selection is never copied),
        // focuses this message and opens its menu.
        let hitbox = prepaint.hitbox.clone();
        let segments = self.segments.clone();
        let focus = prepaint.focus.clone();
        window.on_mouse_event({
            let selection = selection.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                let mac = cfg!(target_os = "macos");
                if phase != DispatchPhase::Bubble
                    || !hitbox.is_hovered(window)
                    || !context_menu::is_context_click(event.button, &event.modifiers, mac)
                {
                    return;
                }
                let at = Pos {
                    msg,
                    byte: index_for_position(&segments, event.position),
                };
                if !selection.borrow().covers(at) {
                    let mut model = selection.borrow_mut();
                    model.begin(at, 2, false);
                    model.end_drag();
                }
                // Focus first: the menu gives focus back here, so ⌘C works after it.
                window.focus(&focus, cx);
                let entries = transcript_selection::transcript_menu_entries(selection.clone(), msg);
                context_menu::open(event.position, entries, window, cx);
                cx.stop_propagation();
            }
        });

        let segments = self.segments.clone();
        let link_click_allowed = self.link_click_allowed.clone();
        window.on_mouse_event({
            let selection = selection.clone();
            move |event: &MouseMoveEvent, phase, window, _cx| {
                if phase == DispatchPhase::Bubble && selection.borrow().is_dragging() {
                    link_click_allowed.set(false);
                    // U3c: over this message's text rows the head follows the
                    // pointer; above the first or below the last message it
                    // clamps to the conversation's ends; gaps change nothing.
                    let y = event.position.y;
                    let rows = segments.iter().map(|s| s.layout.bounds());
                    let top = rows.clone().map(|b| b.top()).reduce(Pixels::min);
                    let bottom = rows.map(|b| b.bottom()).reduce(Pixels::max);
                    let mut model = selection.borrow_mut();
                    match (top, bottom) {
                        (Some(top), _) if y < top => model.drag_beyond(msg, true),
                        (_, Some(bottom)) if y > bottom => model.drag_beyond(msg, false),
                        _ => model.drag_to(Pos {
                            msg,
                            byte: index_for_position(&segments, event.position),
                        }),
                    }
                    window.refresh();
                }
            }
        });
        window.on_mouse_event({
            let selection = selection.clone();
            move |event: &MouseUpEvent, phase, window, _cx| {
                if event.button == MouseButton::Left
                    && phase == DispatchPhase::Bubble
                    && selection.borrow().is_dragging()
                {
                    selection.borrow_mut().end_drag();
                    window.refresh();
                }
            }
        });

        // Paint the selection behind the glyphs. Besides matching native text
        // selection, this lets each bubble use a deliberately high-contrast
        // fill without washing out the selected text itself.
        let range = selection.borrow().range_in(msg).unwrap_or_default();
        let selection_bounds = bounds_for_range(&self.segments, range);
        for bounds in &selection_bounds {
            window.paint_quad(quad(
                *bounds,
                Pixels::ZERO,
                self.selection_fill,
                Edges::default(),
                gpui::Hsla::transparent_black(),
                BorderStyle::default(),
            ));
        }
        self.element.paint(window, cx);
        // Nested markdown surfaces (notably code blocks) paint their own
        // background after the underlay. A crisp perimeter keeps the selected
        // range unmistakable there without tinting over the glyphs.
        let mut selection_border = self.selection_fill;
        selection_border.a = 0.92;
        for bounds in selection_bounds {
            window.paint_quad(quad(
                bounds,
                Pixels::ZERO,
                gpui::Hsla::transparent_black(),
                Edges::all(gpui::px(1.)),
                selection_border,
                BorderStyle::Solid,
            ));
        }
    }
}

fn exact_index_for_position(
    segments: &[TextSegment],
    position: gpui::Point<Pixels>,
) -> Option<usize> {
    segments.iter().find_map(|segment| {
        segment.layout.bounds().contains(&position).then(|| {
            let local = match segment.layout.index_for_position(position) {
                Ok(index) | Err(index) => index,
            };
            segment.source.start + local.min(segment.source.len())
        })
    })
}

fn index_for_position(segments: &[TextSegment], position: gpui::Point<Pixels>) -> usize {
    if let Some(index) = exact_index_for_position(segments, position) {
        return index;
    }
    // A horizontal excursion is still part of the same visual row. Ask the
    // text layout to clamp it so the left margin maps to the row start and the
    // right margin maps to its end (important for backwards drags and lists).
    if let Some(segment) = segments.iter().find(|segment| {
        let bounds = segment.layout.bounds();
        position.y >= bounds.top() && position.y <= bounds.bottom()
    }) {
        return segment.source.start
            + match segment.layout.index_for_position(position) {
                Ok(index) | Err(index) => index.min(segment.source.len()),
            };
    }
    // RC2-APP (F0229): above the first block is the very start of the
    // message, not the end of its first block.
    if let Some(first) = segments.first() {
        if position.y < first.layout.bounds().top() {
            return first.source.start;
        }
    }
    for pair in segments.windows(2) {
        if position.y < pair[1].layout.bounds().top() {
            return pair[0].source.end;
        }
    }
    segments.last().map_or(0, |segment| {
        if position.y < segment.layout.bounds().top() {
            segment.source.start
        } else {
            segment.source.end
        }
    })
}

fn bounds_for_range(segments: &[TextSegment], range: Range<usize>) -> Vec<Bounds<Pixels>> {
    let mut result = Vec::new();
    for segment in segments {
        if segment.source.start >= range.end || segment.source.end <= range.start {
            continue;
        }
        let layout = &segment.layout;
        let start = range.start.max(segment.source.start) - segment.source.start;
        let end = range.end.min(segment.source.end) - segment.source.start;
        let layout_bounds = layout.bounds();
        let line_height = layout.line_height();
        let mut line_start = 0;
        let mut row_top = layout_bounds.top();
        while line_start < end {
            let Some(line) = layout.line_layout_for_index(line_start) else {
                break;
            };
            let unwrapped = &line.unwrapped_layout;
            let line_end = line_start + line.len();
            let row_ends = line
                .wrap_boundaries()
                .iter()
                .map(|boundary| {
                    let glyph = &unwrapped.runs[boundary.run_ix].glyphs[boundary.glyph_ix];
                    (line_start + glyph.index, glyph.position.x)
                })
                .chain([(line_end, unwrapped.width)]);
            let mut row_start = line_start;
            let mut row_start_x = Pixels::ZERO;
            for (row_end, row_end_x) in row_ends {
                let selected_start = start.max(row_start);
                let selected_end = end.min(row_end);
                if selected_start < selected_end {
                    let x_for_index = |index| {
                        layout_bounds.left() + unwrapped.x_for_index(index - line_start)
                            - row_start_x
                    };
                    result.push(Bounds::from_corners(
                        point(x_for_index(selected_start), row_top),
                        point(x_for_index(selected_end), row_top + line_height),
                    ));
                }
                row_start = row_end;
                row_start_x = row_end_x;
                row_top += line_height;
            }
            line_start = line_end + 1;
        }
    }
    result
}

pub(crate) fn selected_text(source: &str, range: Range<usize>) -> Option<&str> {
    source.get(range).filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        div, px, Context, InteractiveElement as _, KeyBinding, Modifiers, ParentElement as _,
        Render, Styled as _, TestAppContext,
    };

    struct Harness;

    fn bind_copy_keys(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.bind_keys([
                KeyBinding::new("cmd-a", native_menu::SelectAll, None),
                KeyBinding::new("cmd-c", native_menu::Copy, None),
            ]);
        });
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let mut collector = TextCollector::default();
            let text = "select this message";
            let styled = collector.capture(StyledText::new(text), text);
            let body = div()
                .debug_selector(|| "selection-target".into())
                .child(styled)
                .into_any_element();
            collector.wrap("selection-harness".into(), body)
        }
    }

    #[gpui::test]
    fn transcript_selection_routes_mouse_and_native_copy(cx: &mut TestAppContext) {
        bind_copy_keys(cx);
        let (_view, window) = cx.add_window_view(|_window, _cx| Harness);
        window.run_until_parked();
        let bounds = window
            .debug_bounds("selection-target")
            .expect("text bounds");
        let y = bounds.origin.y + bounds.size.height / 2.;
        let start = point(bounds.origin.x + px(1.), y);
        let end = point(bounds.origin.x + px(48.), y);

        window.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        window.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());
        window.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        window.simulate_keystrokes("cmd-c");

        let copied = window
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("drag selection should be copied");
        assert!(!copied.is_empty());
        assert!("select this message".contains(&copied));
        assert_ne!(copied, "select this message");

        assert_focused_message_select_all(cx);
        assert_backwards_margin_drag(cx);
        assert_multi_block_selection(cx);
    }

    fn assert_focused_message_select_all(cx: &mut TestAppContext) {
        bind_copy_keys(cx);
        let (_view, window) = cx.add_window_view(|_window, _cx| Harness);
        window.run_until_parked();
        let bounds = window
            .debug_bounds("selection-target")
            .expect("text bounds");
        window.simulate_click(bounds.center(), Modifiers::default());
        window.simulate_keystrokes("cmd-a cmd-c");

        assert_eq!(
            window.read_from_clipboard().and_then(|item| item.text()),
            Some("select this message".to_string())
        );
    }

    fn assert_backwards_margin_drag(cx: &mut TestAppContext) {
        bind_copy_keys(cx);
        let (_view, window) = cx.add_window_view(|_window, _cx| Harness);
        window.run_until_parked();
        let bounds = window
            .debug_bounds("selection-target")
            .expect("text bounds");
        let y = bounds.origin.y + bounds.size.height / 2.;
        let start = point(bounds.origin.x + px(70.), y);
        let left_margin = point(bounds.origin.x - px(30.), y);

        window.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        window.simulate_mouse_move(left_margin, Some(MouseButton::Left), Modifiers::default());
        window.simulate_mouse_up(left_margin, MouseButton::Left, Modifiers::default());
        window.simulate_keystrokes("cmd-c");

        let copied = window
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("backwards selection should be copied");
        assert!("select this message".starts_with(&copied));
        assert_ne!(copied, "select this message");
    }

    #[gpui::test]
    fn dragging_disables_activation_for_interactive_link_runs(cx: &mut TestAppContext) {
        struct LinkHarness {
            gate: Rc<RefCell<Option<Rc<Cell<bool>>>>>,
        }

        impl Render for LinkHarness {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                let mut collector = TextCollector::default();
                let text = "drag this link";
                let styled = collector.capture(StyledText::new(text), text);
                let gate = collector.link_click_allowed();
                *self.gate.borrow_mut() = Some(gate);
                let body = div()
                    .debug_selector(|| "link-selection-target".into())
                    .child(styled)
                    .into_any_element();
                collector.wrap("link-selection-harness".into(), body)
            }
        }

        let gate = Rc::new(RefCell::new(None));
        let gate_for_view = gate.clone();
        let (_view, window) = cx.add_window_view(|_window, _cx| LinkHarness {
            gate: gate_for_view,
        });
        window.run_until_parked();
        let bounds = window
            .debug_bounds("link-selection-target")
            .expect("link text bounds");
        let y = bounds.origin.y + bounds.size.height / 2.;
        let start = point(bounds.origin.x + px(2.), y);
        let end = point(bounds.origin.x + px(50.), y);
        window.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        window.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());
        window.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());

        assert_eq!(
            gate.borrow().as_ref().map(|value| value.get()),
            Some(false),
            "the InteractiveText link callback must stay gated after drag redraws"
        );

        window.simulate_click(start, Modifiers::default());
        assert_eq!(
            gate.borrow().as_ref().map(|value| value.get()),
            Some(true),
            "a fresh click must re-enable the link through its redraw"
        );
    }

    fn assert_multi_block_selection(cx: &mut TestAppContext) {
        struct MultiBlockHarness;

        impl Render for MultiBlockHarness {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                let mut collector = TextCollector::default();
                let first = collector.capture(StyledText::new("first block"), "first block");
                let second = collector.capture(StyledText::new("second block"), "second block");
                let body = div()
                    .flex()
                    .flex_col()
                    .child(div().debug_selector(|| "first-block".into()).child(first))
                    .child(div().debug_selector(|| "second-block".into()).child(second))
                    .into_any_element();
                collector.wrap("multi-block-harness".into(), body)
            }
        }

        bind_copy_keys(cx);
        let (_view, window) = cx.add_window_view(|_window, _cx| MultiBlockHarness);
        window.run_until_parked();
        let first = window.debug_bounds("first-block").expect("first block");
        let second = window.debug_bounds("second-block").expect("second block");
        window.simulate_mouse_down(
            point(first.origin.x + px(1.), first.center().y),
            MouseButton::Left,
            Modifiers::default(),
        );
        let second_right_margin = point(second.right() + px(30.), second.center().y);
        window.simulate_mouse_move(
            second_right_margin,
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        window.simulate_mouse_up(second_right_margin, MouseButton::Left, Modifiers::default());
        window.simulate_keystrokes("cmd-c");

        let copied = window
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("multi-block selection should copy");
        assert!(copied.contains('\n'));
        assert!(copied.starts_with("first"));
        assert!(copied.ends_with("block"));
    }

    #[test]
    fn invalid_utf8_boundaries_never_reach_clipboard_slicing() {
        let source = "\u{1f600} safe";
        assert_eq!(selected_text(source, 0..4), Some("\u{1f600}"));
        assert_eq!(selected_text(source, 1..4), None);
        assert_eq!(selected_text(source, 0..100), None);
    }

    // ---- U3c: one conversation, several messages, one shared model ---------

    const U3C_MSGS: [&str; 3] = ["u3c-msg-0", "u3c-msg-1", "u3c-msg-2"];

    /// The messages of one conversation scope laid out like the transcript:
    /// a column with gaps, 100 px below the window top, each message as wide
    /// as its text.
    struct Conversation {
        texts: Vec<&'static str>,
        /// Each message's link-click gate from the last render.
        gates: Vec<Rc<Cell<bool>>>,
    }

    impl Render for Conversation {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            self.gates.clear();
            let mut rows = Vec::new();
            for (i, text) in self.texts.iter().enumerate() {
                let mut collector = TextCollector::default();
                let styled = collector.capture(StyledText::new(*text), text);
                self.gates.push(collector.link_click_allowed());
                let body = div().debug_selector(move || U3C_MSGS[i].into());
                let body = body.child(styled).into_any_element();
                let id = ElementId::NamedInteger("u3c-msg".into(), i as u64);
                rows.push(collector.wrap_in_transcript(id, body, "u3c-conversation", i));
            }
            let column = div().pt(px(100.)).flex().flex_col().items_start();
            column.gap(px(20.)).children(rows)
        }
    }

    fn conversation<'a>(
        cx: &'a mut TestAppContext,
        texts: &[&'static str],
    ) -> (gpui::Entity<Conversation>, &'a mut gpui::VisualTestContext) {
        bind_copy_keys(cx);
        let texts = texts.to_vec();
        let (view, window) = cx.add_window_view(move |_, _| Conversation {
            texts,
            gates: Vec::new(),
        });
        window.run_until_parked();
        (view, window)
    }

    /// The point just right of the boundary before byte `byte` of message
    /// `msg` (the test text system has a fixed glyph advance).
    fn at(window: &mut gpui::VisualTestContext, texts: &[&str], msg: usize, byte: usize) -> Point {
        let b = window.debug_bounds(U3C_MSGS[msg]).expect("message drawn");
        let advance = b.size.width / texts[msg].len() as f32;
        point(b.left() + advance * byte as f32 + px(1.), b.center().y)
    }

    type Point = gpui::Point<Pixels>;

    fn drag(window: &mut gpui::VisualTestContext, from: Point, to: Point, mods: Modifiers) {
        window.simulate_mouse_down(from, MouseButton::Left, mods);
        window.simulate_mouse_move(to, Some(MouseButton::Left), mods);
        window.simulate_mouse_up(to, MouseButton::Left, mods);
    }

    /// What ⌘C would copy: the conversation model's selected text, `"<none>"`
    /// when empty. Only `u3c_cmd_a_in_one_message_copies_conversation`
    /// dispatches the real actions: gpui's process-wide action profiler
    /// panics when parallel tests dispatch actions at the same instant.
    fn copy(window: &mut gpui::VisualTestContext) -> String {
        let model = window.update(|_, cx| transcript_selection::shared("u3c-conversation", cx));
        let text = model.borrow().selected_text();
        text.unwrap_or_else(|| "<none>".into())
    }

    const TWO: [&str; 2] = ["alpha bravo", "charlie delta"];

    /// RC2-APP (F0229): a context click above a message's first block (inside
    /// the message) maps to the very start of the message, so the word it
    /// selects is the first one, not the last word of the first block. Read
    /// through the shared model (no action dispatch; see `copy`).
    #[gpui::test]
    fn rc2app_a_context_click_above_the_first_block_takes_the_first_word(cx: &mut TestAppContext) {
        struct TwoBlocks;
        impl Render for TwoBlocks {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                let mut collector = TextCollector::default();
                let first = collector.capture(StyledText::new("first block"), "first block");
                let second = collector.capture(StyledText::new("second block"), "second block");
                let body = div()
                    .pt(px(40.))
                    .flex()
                    .flex_col()
                    .child(div().debug_selector(|| "rc2app-first".into()).child(first))
                    .child(
                        div()
                            .debug_selector(|| "rc2app-second".into())
                            .child(second),
                    )
                    .into_any_element();
                let column = div().pt(px(100.)).flex().flex_col().items_start();
                column.child(collector.wrap_in_transcript(
                    "rc2app-two-blocks".into(),
                    body,
                    "rc2app-f0229",
                    0,
                ))
            }
        }
        let (_view, window) = cx.add_window_view(|_, _| TwoBlocks);
        window.run_until_parked();
        let first = window.debug_bounds("rc2app-first").expect("first block");
        // Inside the message (its top padding), above its first block.
        let above = point(first.left() + px(30.), first.top() - px(20.));
        window.simulate_mouse_down(above, MouseButton::Right, Modifiers::default());
        window.simulate_mouse_up(above, MouseButton::Right, Modifiers::default());
        let model = window.update(|_, cx| transcript_selection::shared("rc2app-f0229", cx));
        let selected = model.borrow().selected_text().unwrap_or_default();
        assert_eq!(
            selected, "first",
            "a context click above the first block takes the first word"
        );
    }

    #[gpui::test]
    fn u3c_drag_across_two_messages_copies_both(cx: &mut TestAppContext) {
        let (_view, window) = conversation(cx, &TWO);
        let (from, to) = (at(window, &TWO, 0, 6), at(window, &TWO, 1, 7));
        drag(window, from, to, Modifiers::default());
        assert_eq!(copy(window), "bravo\n\ncharlie");
        // Backwards gives the same text.
        drag(window, to, from, Modifiers::default());
        assert_eq!(copy(window), "bravo\n\ncharlie");
    }

    #[gpui::test]
    fn u3c_cmd_a_in_one_message_copies_conversation(cx: &mut TestAppContext) {
        let (_view, window) = conversation(cx, &TWO);
        let p = at(window, &TWO, 1, 2);
        window.simulate_click(p, Modifiers::default());
        window.simulate_keystrokes("cmd-a cmd-c");
        let copied = window.read_from_clipboard().and_then(|i| i.text());
        assert_eq!(copied.as_deref(), Some("alpha bravo\n\ncharlie delta"));
    }

    #[gpui::test]
    fn u3c_double_click_word_in_reply(cx: &mut TestAppContext) {
        let texts = ["the quick brown fox"];
        let (_view, window) = conversation(cx, &texts);
        let p = at(window, &texts, 0, 5);
        for click_count in [2, 3] {
            window.simulate_event(MouseDownEvent {
                position: p,
                modifiers: Modifiers::default(),
                button: MouseButton::Left,
                click_count,
                first_mouse: false,
            });
            window.simulate_mouse_up(p, MouseButton::Left, Modifiers::default());
            let want = if click_count == 2 { "quick" } else { texts[0] };
            assert_eq!(copy(window), want, "click_count {click_count}");
        }
    }

    #[gpui::test]
    fn u3c_drag_above_first_message_clamps_to_start(cx: &mut TestAppContext) {
        let (_view, window) = conversation(cx, &TWO);
        let from = at(window, &TWO, 1, 7);
        drag(window, from, point(from.x, px(40.)), Modifiers::default());
        assert_eq!(copy(window), "alpha bravo\n\ncharlie");
    }

    #[gpui::test]
    fn u3c_drag_below_last_message_clamps_to_end(cx: &mut TestAppContext) {
        let (_view, window) = conversation(cx, &TWO);
        let from = at(window, &TWO, 0, 6);
        let last = window.debug_bounds(U3C_MSGS[1]).expect("message drawn");
        drag(
            window,
            from,
            point(from.x, last.bottom() + px(60.)),
            Modifiers::default(),
        );
        assert_eq!(copy(window), "bravo\n\ncharlie delta");
    }

    #[gpui::test]
    fn u3c_drag_into_the_gap_keeps_the_head(cx: &mut TestAppContext) {
        let (_view, window) = conversation(cx, &TWO);
        let (from, to) = (at(window, &TWO, 0, 6), at(window, &TWO, 1, 7));
        let first = window.debug_bounds(U3C_MSGS[0]).expect("message drawn");
        let gap = point(to.x, first.bottom() + px(10.));
        window.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        window.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
        window.simulate_mouse_move(gap, Some(MouseButton::Left), Modifiers::default());
        window.simulate_mouse_up(gap, MouseButton::Left, Modifiers::default());
        assert_eq!(copy(window), "bravo\n\ncharlie");
    }

    #[gpui::test]
    fn u3c_shortened_conversation_drops_stale_messages(cx: &mut TestAppContext) {
        let texts = ["one", "two", "three"];
        let (view, window) = conversation(cx, &texts);
        let (from, to) = (at(window, &texts, 0, 1), at(window, &texts, 2, 2));
        drag(window, from, to, Modifiers::default());
        assert_eq!(copy(window), "ne\n\ntwo\n\nth");
        // The conversation loses its last message (an edit / retry).
        view.update(window, |c, cx| {
            c.texts.truncate(2);
            cx.notify();
        });
        window.run_until_parked();
        assert_eq!(
            copy(window),
            "<none>",
            "the selection covered a removed message"
        );
        let model = window.update(|_, cx| transcript_selection::shared("u3c-conversation", cx));
        model.borrow_mut().select_all(); // what ⌘A runs
        assert_eq!(copy(window), "one\n\ntwo");
    }

    #[gpui::test]
    fn u3c_new_message_keeps_the_selection(cx: &mut TestAppContext) {
        let (view, window) = conversation(cx, &TWO);
        let (a, b) = (at(window, &TWO, 0, 0), at(window, &TWO, 0, 5));
        drag(window, a, b, Modifiers::default());
        // A reply streams in below; the earlier selection survives.
        view.update(window, |c, cx| {
            c.texts.push("echo");
            cx.notify();
        });
        window.run_until_parked();
        assert_eq!(copy(window), "alpha");
    }

    #[gpui::test]
    fn u3c_shift_click_extends_across_messages(cx: &mut TestAppContext) {
        let (_view, window) = conversation(cx, &TWO);
        let p = at(window, &TWO, 0, 2);
        window.simulate_click(p, Modifiers::default());
        let p = at(window, &TWO, 1, 3);
        window.simulate_click(p, Modifiers::shift());
        assert_eq!(copy(window), "pha bravo\n\ncha");
    }

    #[gpui::test]
    fn u3c_click_outside_clears_the_selection(cx: &mut TestAppContext) {
        let (_view, window) = conversation(cx, &TWO);
        let (a, b) = (at(window, &TWO, 0, 0), at(window, &TWO, 0, 5));
        drag(window, a, b, Modifiers::default());
        assert_eq!(copy(window), "alpha");
        window.simulate_click(point(px(900.), px(600.)), Modifiers::default());
        assert_eq!(copy(window), "<none>");
    }

    #[gpui::test]
    fn u3c_ctrl_click_is_a_context_click_on_mac(cx: &mut TestAppContext) {
        let (_view, window) = conversation(cx, &TWO);
        let (a, b) = (at(window, &TWO, 0, 0), at(window, &TWO, 0, 5));
        drag(window, a, b, Modifiers::default());
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        // macOS: ctrl-click inside the selection is a context click (U3a2's
        // menu), so it keeps it; elsewhere it is a plain click: a caret.
        let p = at(window, &TWO, 0, 2);
        window.simulate_mouse_down(p, MouseButton::Left, ctrl);
        window.simulate_mouse_up(p, MouseButton::Left, ctrl);
        let want = if cfg!(target_os = "macos") {
            "alpha"
        } else {
            "<none>"
        };
        assert_eq!(copy(window), want);
    }

    #[gpui::test]
    fn u3c_multi_click_never_activates_a_link(cx: &mut TestAppContext) {
        let texts = ["the quick brown fox"];
        let (view, window) = conversation(cx, &texts);
        let p = at(window, &texts, 0, 5);
        window.simulate_event(MouseDownEvent {
            position: p,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        });
        window.simulate_mouse_up(p, MouseButton::Left, Modifiers::default());
        let gate = view.read_with(window, |c, _| c.gates[0].get());
        assert!(
            !gate,
            "a double-click selects the word and never follows a link"
        );
        window.simulate_click(p, Modifiers::default());
        assert!(
            view.read_with(window, |c, _| c.gates[0].get()),
            "a plain click may"
        );
    }

    #[gpui::test]
    fn u3c_right_click_inside_the_selection_keeps_it(cx: &mut TestAppContext) {
        let (_view, window) = conversation(cx, &TWO);
        let (a, b) = (at(window, &TWO, 0, 0), at(window, &TWO, 0, 5));
        drag(window, a, b, Modifiers::default());
        // A right-click is not a Left click: it neither clears nor re-anchors.
        // (The transcript menu itself is U3a2's, on the U3a1 context_menu API.)
        let p = at(window, &TWO, 0, 2);
        window.simulate_mouse_down(p, MouseButton::Right, Modifiers::default());
        window.simulate_mouse_up(p, MouseButton::Right, Modifiers::default());
        assert_eq!(copy(window), "alpha");
    }

    // ---- U3a2: the transcript context menu, through the one root host ------

    /// The U3c conversation under a root that mounts the context-menu host,
    /// as `AwiliApp` does.
    struct Hosted {
        conversation: gpui::Entity<Conversation>,
    }

    impl Render for Hosted {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let host = crate::context_menu::host_element(window, cx);
            div()
                .size_full()
                .relative()
                .on_mouse_down(MouseButton::Right, |_, _, _| {
                    BUBBLED.with(|n| n.set(n.get() + 1))
                })
                .child(self.conversation.clone())
                .children(host)
        }
    }

    thread_local! {
        /// Right mouse-downs that bubbled past the messages to the root.
        static BUBBLED: Cell<usize> = const { Cell::new(0) };
    }

    const THREE: [&str; 3] = ["alpha bravo", "charlie delta", "echo"];

    fn hosted(cx: &mut TestAppContext) -> &mut gpui::VisualTestContext {
        let (_view, window) = cx.add_window_view(|_, cx| {
            let texts = THREE.to_vec();
            let gates = Vec::new();
            let conversation = gpui::AppContext::new(cx, |_| Conversation { texts, gates });
            Hosted { conversation }
        });
        // A menu lives only in the active window (`context_menu::host_element`).
        window.update(|window, _| window.activate_window());
        window.run_until_parked();
        window.write_to_clipboard(ClipboardItem::new_string("<clip>".into()));
        window
    }

    fn context_click(
        window: &mut gpui::VisualTestContext,
        p: Point,
        mods: Modifiers,
        button: MouseButton,
    ) {
        window.simulate_mouse_down(p, button, mods);
        window.simulate_mouse_up(p, button, mods);
    }

    fn right_click(window: &mut gpui::VisualTestContext, msg: usize, byte: usize) {
        let p = at(window, &THREE, msg, byte);
        context_click(window, p, Modifiers::default(), MouseButton::Right);
    }

    /// (selection as ⌘C would copy it, clipboard, menu rows or `None` if closed)
    fn state(
        window: &mut gpui::VisualTestContext,
    ) -> (String, String, Option<Vec<(String, bool)>>) {
        let clip = window.read_from_clipboard().and_then(|i| i.text());
        let rows = window.update(|_, cx| crate::context_menu::rows(cx));
        (copy(window), clip.unwrap_or_default(), rows)
    }

    fn menu(copy: bool, message: bool) -> Option<Vec<(String, bool)>> {
        let rows = [
            ("Copy", copy),
            ("Copy Message", message),
            ("-", false),
            ("Select All", true),
        ];
        Some(rows.iter().map(|(l, e)| (l.to_string(), *e)).collect())
    }

    fn s(
        selection: &str,
        clip: &str,
        rows: Option<Vec<(String, bool)>>,
    ) -> (String, String, Option<Vec<(String, bool)>>) {
        (selection.to_string(), clip.to_string(), rows)
    }

    const ROWS: [&str; 3] = [
        "ui-row-context-menu-item-0",
        "ui-row-context-menu-item-1",
        "ui-row-context-menu-item-2",
    ];

    fn click_row(window: &mut gpui::VisualTestContext, row: usize) {
        let p = window
            .debug_bounds(ROWS[row])
            .expect("menu row drawn")
            .center();
        window.simulate_click(p, Modifiers::default());
    }

    /// Drive a KIT Menu key through the same down/up pair as its component
    /// keyboard test, then let the parent's selection callback repaint.
    fn menu_key(window: &mut gpui::VisualTestContext, key: &str) {
        let keystroke = gpui::Keystroke::parse(key).unwrap();
        window.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        window.simulate_event(gpui::KeyUpEvent { keystroke });
        window.update(|window, _cx| window.refresh());
        window.run_until_parked();
    }

    #[gpui::test]
    fn u3a2_right_click_word_then_copy(cx: &mut TestAppContext) {
        let window = hosted(cx);
        let click_point = at(window, &THREE, 1, 2);
        right_click(window, 1, 2);
        // Nothing was selected: the word under the pointer is (D3).
        assert_eq!(state(window), s("charlie", "<clip>", menu(true, true)));
        window.run_until_parked();
        assert!(
            window.debug_bounds("context-menu").is_some()
                && window.debug_bounds("ui-menu-context-menu-panel").is_some(),
            "the root host draws the KIT menu panel"
        );
        let panel = window.debug_bounds("ui-menu-context-menu-panel").unwrap();
        let face = window
            .debug_bounds("ui-popover-context-menu-panel")
            .expect("the point-anchored popover face is drawn");
        let point_row = window.debug_bounds(ROWS[0]).unwrap();
        let window_size = window.update(|window, _| window.viewport_size());
        let scale_factor = window.update(|window, _| window.scale_factor());
        let window_bounds = Bounds::new(point(px(0.), px(0.)), window_size);
        let edge = f32::from(theme::z(theme::EDGE));
        let clamp_axis = |value: f32, extent: f32, face_extent: f32| {
            if extent - edge - face_extent < edge {
                edge
            } else {
                value.clamp(edge, extent - edge - face_extent)
            }
        };
        let click_y = f32::from(click_point.y);
        let face_height = f32::from(face.size.height);
        let above_y = click_y - face_height;
        let preferred_y =
            if click_y + face_height > f32::from(window_size.height) - edge && above_y >= edge {
                above_y
            } else {
                click_y
            };
        let expected_origin = point(
            px(clamp_axis(
                f32::from(click_point.x),
                f32::from(window_size.width),
                f32::from(face.size.width),
            )),
            px(clamp_axis(
                preferred_y,
                f32::from(window_size.height),
                face_height,
            )),
        );
        assert!(
            face.left() >= window_bounds.left()
                && face.right() <= window_bounds.right()
                && face.top() >= window_bounds.top()
                && face.bottom() <= window_bounds.bottom()
                && panel.left() >= face.left()
                && panel.right() <= face.right()
                && panel.top() >= face.top()
                && panel.bottom() <= face.bottom(),
            "the KIT popover face is fully clamped and contains its menu: {face:?} / {panel:?} / {window_bounds:?}"
        );
        assert!(
            point_row.size.height >= theme::target(),
            "context actions use target-sized KIT Rows: {point_row:?}"
        );
        // Pinned GPUI rounds absolute layout edges to device pixels when it
        // returns debug bounds (`gpui::taffy::layout_bounds`); allow at most
        // one physical pixel between the exact EDGE-clamped placement and the
        // rendered face. Keep the full-face viewport checks above exact.
        let within_one_physical_pixel =
            |actual: f32, expected: f32| (actual - expected).abs() * scale_factor <= 1.0;
        let edge_tolerance = px(1.0 / scale_factor);
        let edge = theme::z(theme::EDGE);
        assert!(
            within_one_physical_pixel(f32::from(face.origin.x), f32::from(expected_origin.x))
                && within_one_physical_pixel(
                    f32::from(face.origin.y),
                    f32::from(expected_origin.y),
                )
                && face.left() + edge_tolerance >= edge
                && face.top() + edge_tolerance >= edge
                && face.right() <= window_size.width - edge + edge_tolerance
                && face.bottom() <= window_size.height - edge + edge_tolerance,
            "the zero-gap face matches the click or exact viewport EDGE clamp within one physical pixel: {face:?} / {expected_origin:?} / scale {scale_factor}"
        );
        window.simulate_keystrokes("down enter");
        assert_eq!(state(window), s("charlie", "charlie", None));
        assert_eq!(
            BUBBLED.with(Cell::get),
            0,
            "the context click stops at the message"
        );
    }

    #[gpui::test]
    fn u3a2_menu_choice_restores_message_focus_before_running_action(cx: &mut TestAppContext) {
        let window = hosted(cx);
        let p = at(window, &THREE, 1, 2);
        window.simulate_click(p, Modifiers::default());
        let message_focus = window
            .update(|window, cx| window.focused(cx))
            .expect("the clicked transcript message owns focus");
        let observed = Rc::new(Cell::new(false));
        let action_observed = observed.clone();
        let run: crate::context_menu::MenuRun = Rc::new(move |window, cx| {
            action_observed.set(
                !crate::context_menu::is_open(cx)
                    && window.focused(cx).as_ref() == Some(&message_focus),
            );
        });
        window.update(|window, cx| {
            crate::context_menu::open(
                p,
                vec![crate::context_menu::MenuEntry::item(
                    "Check focus",
                    None,
                    true,
                    run,
                )],
                window,
                cx,
            );
        });
        window.run_until_parked();
        let row = window.debug_bounds("ui-row-context-menu-item-0").unwrap();
        window.simulate_click(row.center(), Modifiers::default());
        assert!(
            observed.get(),
            "the menu closes and restores the clicked message before its action"
        );
    }

    #[gpui::test]
    fn u3a2_disabled_menu_item_is_painted_inert_and_skipped(cx: &mut TestAppContext) {
        let window = hosted(cx);
        let runs = Rc::new(Cell::new(0));
        let disabled_runs = runs.clone();
        let enabled_runs = runs.clone();
        let entries = vec![
            crate::context_menu::MenuEntry::item(
                "Unavailable",
                None,
                false,
                Rc::new(move |_, _| disabled_runs.set(disabled_runs.get() + 1)),
            ),
            crate::context_menu::MenuEntry::item(
                "Available",
                None,
                true,
                Rc::new(move |_, _| enabled_runs.set(enabled_runs.get() + 1)),
            ),
        ];
        window.update(|window, cx| {
            crate::context_menu::open(point(px(600.), px(400.)), entries, window, cx);
        });
        window.run_until_parked();
        let disabled = window.debug_bounds("ui-row-context-menu-item-0").unwrap();
        window.simulate_click(disabled.center(), Modifiers::default());
        assert_eq!(runs.get(), 0, "disabled item runs no action");
        assert!(
            window.update(|_, cx| crate::context_menu::is_open(cx)),
            "attempting the disabled row leaves the menu open"
        );
        window.simulate_keystrokes("down enter");
        assert_eq!(runs.get(), 1, "keyboard skips disabled and chooses enabled");
        assert!(!window.update(|_, cx| crate::context_menu::is_open(cx)));
    }

    #[gpui::test]
    fn u3a2_pointer_anchored_menu_clamps_its_full_face_to_window_edges(cx: &mut TestAppContext) {
        let window = hosted(cx);
        let window_size = window.update(|window, _| window.viewport_size());
        let point = point(window_size.width - px(1.), window_size.height - px(1.));
        window.update(|window, cx| {
            crate::context_menu::open(
                point,
                vec![crate::context_menu::MenuEntry::item(
                    "Edge action",
                    None,
                    true,
                    Rc::new(|_, _| {}),
                )],
                window,
                cx,
            );
        });
        window.run_until_parked();
        let panel = window
            .debug_bounds("ui-menu-context-menu-panel")
            .expect("the edge-anchored menu is painted");
        assert!(
            panel.left() >= px(0.)
                && panel.top() >= px(0.)
                && panel.right() <= window_size.width
                && panel.bottom() <= window_size.height,
            "pointer-anchored KIT menu remains fully inside the actual window: {panel:?} / {window_size:?}"
        );
    }

    #[gpui::test]
    fn u3a2_copy_message_copies_plain_text(cx: &mut TestAppContext) {
        let window = hosted(cx);
        right_click(window, 0, 1);
        assert_eq!(state(window), s("alpha", "<clip>", menu(true, true)));
        window.simulate_keystrokes("down down enter");
        assert_eq!(state(window), s("alpha", "alpha bravo", None));
    }

    #[gpui::test]
    fn u3a2_select_all_from_menu_covers_conversation(cx: &mut TestAppContext) {
        let window = hosted(cx);
        right_click(window, 2, 1);
        assert_eq!(state(window), s("echo", "<clip>", menu(true, true)));
        click_row(window, 2);
        let all = "alpha bravo\n\ncharlie delta\n\necho";
        assert_eq!(state(window), s(all, "<clip>", None));
    }

    #[gpui::test]
    fn u3a2_keyboard_select_all_crosses_separator_and_restores_clicked_message_focus(
        cx: &mut TestAppContext,
    ) {
        let window = hosted(cx);
        let clicked = at(window, &THREE, 1, 2);

        // Capture the actual transcript message focus handle before its
        // context menu moves focus into the KIT Menu.
        window.simulate_click(clicked, Modifiers::default());
        window.run_until_parked();
        let message_focus = window
            .update(|window, cx| window.focused(cx))
            .expect("the clicked transcript message owns focus");

        right_click(window, 1, 2);
        assert_eq!(state(window), s("charlie", "<clip>", menu(true, true)));
        assert!(window.update(|_, cx| crate::context_menu::is_open(cx)));

        // The rendered menu order is Copy, Copy Message, separator, Select All.
        // Starting with no highlight, three actual Down events cross the
        // separator and clamp on the final enabled row.
        menu_key(window, "down");
        menu_key(window, "down");
        menu_key(window, "down");
        assert_eq!(
            window.update(|_, cx| crate::context_menu::rows(cx)),
            menu(true, true),
            "navigation keeps the transcript menu open"
        );
        menu_key(window, "down");
        assert_eq!(
            window.update(|_, cx| crate::context_menu::rows(cx)),
            menu(true, true),
            "Down at the last enabled item clamps instead of wrapping"
        );
        menu_key(window, "enter");

        let all = "alpha bravo\n\ncharlie delta\n\necho";
        assert_eq!(state(window), s(all, "<clip>", None));
        assert!(
            window.update(|_, cx| !crate::context_menu::is_open(cx)),
            "Enter closes the actual transcript menu"
        );
        assert_eq!(
            window.update(|window, cx| window.focused(cx)),
            Some(message_focus),
            "Enter restores focus to the clicked transcript message"
        );
    }

    #[gpui::test]
    fn u3a2_right_click_inside_selection_keeps_cross_message_selection(cx: &mut TestAppContext) {
        let window = hosted(cx);
        let (a, b) = (at(window, &THREE, 0, 6), at(window, &THREE, 1, 7));
        drag(window, a, b, Modifiers::default());
        assert_eq!(state(window), s("bravo\n\ncharlie", "<clip>", None));
        right_click(window, 1, 2);
        assert_eq!(
            state(window),
            s("bravo\n\ncharlie", "<clip>", menu(true, true))
        );
        // A mouse click on the Copy row: the row's mouse-down must not clear it.
        click_row(window, 0);
        let copied = "bravo\n\ncharlie";
        assert_eq!(state(window), s(copied, copied, None));
    }

    #[gpui::test]
    fn u3a2_right_click_outside_selection_never_copies_it(cx: &mut TestAppContext) {
        let window = hosted(cx);
        let (a, b) = (at(window, &THREE, 0, 0), at(window, &THREE, 0, 5));
        drag(window, a, b, Modifiers::default());
        assert_eq!(state(window), s("alpha", "<clip>", None));
        // The U3c review's position check: outside the selection, the word
        // under the pointer replaces it, so Copy never copies "alpha".
        right_click(window, 2, 1);
        assert_eq!(state(window), s("echo", "<clip>", menu(true, true)));
        window.simulate_keystrokes("down enter");
        assert_eq!(state(window), s("echo", "echo", None));
    }

    #[gpui::test]
    fn u3a2_ctrl_click_opens_menu_on_mac(cx: &mut TestAppContext) {
        let window = hosted(cx);
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        let p = at(window, &THREE, 1, 2);
        context_click(window, p, ctrl, MouseButton::Left);
        // macOS: a context click. Elsewhere: a plain click (a caret, no menu).
        let want = match cfg!(target_os = "macos") {
            true => s("charlie", "<clip>", menu(true, true)),
            false => s("<none>", "<clip>", None),
        };
        assert_eq!(state(window), want);
    }

    #[gpui::test]
    fn u3a2_escape_and_click_outside_close_and_keep_the_selection(cx: &mut TestAppContext) {
        let window = hosted(cx);
        right_click(window, 1, 2);
        assert_eq!(state(window), s("charlie", "<clip>", menu(true, true)));
        window.simulate_keystrokes("escape");
        assert_eq!(state(window), s("charlie", "<clip>", None));
        let refocused = window.update(|window, cx| window.focused(cx).is_some());
        assert!(refocused, "focus goes back to the message, so cmd-C works");
        right_click(window, 0, 1);
        assert_eq!(state(window), s("alpha", "<clip>", menu(true, true)));
        window.simulate_click(point(px(900.), px(600.)), Modifiers::default());
        assert_eq!(state(window), s("alpha", "<clip>", None));
    }
}
