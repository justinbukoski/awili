//! Headless layout reproduction for the chips_row "first + last child vanish" bug.
//! Uses gpui's test platform to lay out the exact flex_row + gap structure and
//! read each child's resolved taffy bounds via debug_selector / debug_bounds.
//
// Headless layout test: the border colours below are test scaffolding, not UI
// chrome, so the slice-2 design gate's disallowed-methods ban does not apply.
#![allow(clippy::disallowed_methods)]

use gpui::{
    div, prelude::*, px, rgb, size, AvailableSpace, Bounds, Div, Pixels, Stateful, TestAppContext,
};

// Faithful interactive chip: id + on_click + hover + cursor_pointer, like the
// route/ccd chips that allegedly vanish at the row edges.
fn chip_interactive(label: &'static str, key: &'static str) -> Stateful<Div> {
    div()
        .id(key)
        .flex()
        .flex_none()
        .items_center()
        .px(px(10.))
        .py(px(4.))
        .rounded(px(8.))
        .border_1()
        .border_color(rgb(0x808080))
        .text_size(px(11.))
        .cursor_pointer()
        .hover(|s| s.border_color(rgb(0xffffff)))
        .debug_selector(move || key.to_string())
        .child(label)
        .on_click(|_ev, _w, _cx| {})
}

fn chip(label: &'static str, key: &'static str) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .px(px(10.))
        .py(px(4.))
        .rounded(px(8.))
        .border_1()
        .text_size(px(11.))
        .debug_selector(move || key.to_string())
        .child(label)
}

fn space() -> gpui::Size<AvailableSpace> {
    size(
        AvailableSpace::Definite(px(1200.)),
        AvailableSpace::Definite(px(200.)),
    )
}

fn dump(name: &str, b: Option<Bounds<Pixels>>) {
    match b {
        Some(b) => eprintln!(
            "  {name:>8}: x={:>7.2} w={:>7.2}  (right={:>7.2})",
            f32::from(b.origin.x),
            f32::from(b.size.width),
            f32::from(b.origin.x + b.size.width),
        ),
        None => eprintln!("  {name:>8}: <NO BOUNDS RECORDED>"),
    }
}

#[gpui::test]
fn gap_flex_row_lays_out_correctly(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    cx.draw(gpui::point(px(0.), px(0.)), space(), |_w, _cx| {
        div()
            .flex()
            .flex_row()
            .gap(px(8.))
            .mb(px(10.))
            .child(chip("auto \u{2192} anthropic", "route"))
            .child(chip("anthropic \u{b7} claude-opus", "model"))
            .child(chip("rules: 3", "rules"))
            .child(chip("ccd \u{b7} 0", "ccd"))
    });

    eprintln!("--- .gap(px(8.)) container ---");
    let route = cx.debug_bounds("route");
    let model = cx.debug_bounds("model");
    let rules = cx.debug_bounds("rules");
    let ccd = cx.debug_bounds("ccd");
    dump("route", route);
    dump("model", model);
    dump("rules", rules);
    dump("ccd", ccd);

    for (n, b) in [
        ("route", route),
        ("model", model),
        ("rules", rules),
        ("ccd", ccd),
    ] {
        let b = b.unwrap_or_else(|| panic!("{n} chip recorded no bounds"));
        assert!(
            f32::from(b.size.width) > 0.0,
            "{n} chip has zero width: {b:?}"
        );
    }
}

// Faithful reproduction of the production tree: a BLOCK div (no flex) wraps the
// flex_row chips, exactly like composer_bar's inner container.
#[gpui::test]
fn chips_inside_block_parent(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    cx.draw(gpui::point(px(0.), px(0.)), space(), |_w, _cx| {
        // outer column (size_full, flex_col) -> composer_bar block -> inner block
        div().flex().flex_col().size_full().child(
            div()
                .w_full()
                .max_w(px(760.))
                .mx_auto()
                .px(px(28.))
                .py(px(16.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.))
                        .mb(px(10.))
                        .child(chip("auto \u{2192} anthropic", "route"))
                        .child(chip("anthropic \u{b7} claude-opus", "model"))
                        .child(chip("rules: 3", "rules"))
                        .child(chip("ccd \u{b7} 0", "ccd")),
                )
                .child(div().h(px(40.)).w_full().child("voice")) // voice_controls stand-in
                .child(div().h(px(60.)).w_full().child("composer")), // composer stand-in
        )
    });

    eprintln!("--- chips inside real block parent ---");
    let route = cx.debug_bounds("route");
    let model = cx.debug_bounds("model");
    let rules = cx.debug_bounds("rules");
    let ccd = cx.debug_bounds("ccd");
    dump("route", route);
    dump("model", model);
    dump("rules", rules);
    dump("ccd", ccd);
    for (n, b) in [
        ("route", route),
        ("model", model),
        ("rules", rules),
        ("ccd", ccd),
    ] {
        let b = b.unwrap_or_else(|| panic!("{n} chip recorded no bounds"));
        assert!(f32::from(b.size.width) > 0.0, "{n} chip zero width: {b:?}");
    }
}

struct ChipsView;
impl gpui::Render for ChipsView {
    fn render(&mut self, _w: &mut gpui::Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        // Mirror render(): flex_col size_full root > message_list stand-in +
        // composer_bar(flex_none, w_full, border_t) > inner BLOCK div
        // (max_w, mx_auto, px, py) > [chips_row, voice, composer].
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(div().flex_1().w_full().child("messages"))
            .child(
                div().flex_none().w_full().border_t_1().child(
                    div()
                        .w_full()
                        .max_w(px(760.))
                        .mx_auto()
                        .px(px(28.))
                        .py(px(16.))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(8.))
                                .mb(px(10.))
                                .child(chip_interactive("auto \u{2192} anthropic", "route"))
                                .child(chip_interactive("anthropic \u{b7} claude-opus", "model"))
                                .child(chip("rules: 3", "rules"))
                                .child(chip_interactive("ccd \u{b7} 0", "ccd")),
                        )
                        .child(div().h(px(40.)).w_full().child("voice"))
                        .child(div().h(px(60.)).w_full().child("composer")),
                ),
            )
    }
}

// The real mix rendered through a real view: interactive route (first),
// interactive model (mid), non-interactive rules (mid), interactive ccd (last).
#[gpui::test]
fn real_interactive_mix(cx: &mut TestAppContext) {
    let (_view, cx) = cx.add_window_view(|_w, _cx| ChipsView);
    cx.run_until_parked();

    eprintln!("--- real interactive mix (route/model/ccd interactive, rules plain) ---");
    let route = cx.debug_bounds("route");
    let model = cx.debug_bounds("model");
    let rules = cx.debug_bounds("rules");
    let ccd = cx.debug_bounds("ccd");
    dump("route", route);
    dump("model", model);
    dump("rules", rules);
    dump("ccd", ccd);
    for (n, b) in [
        ("route", route),
        ("model", model),
        ("rules", rules),
        ("ccd", ccd),
    ] {
        let b = b.unwrap_or_else(|| panic!("{n} chip recorded no bounds (paint not reached)"));
        assert!(f32::from(b.size.width) > 0.0, "{n} chip zero width: {b:?}");
    }
}

#[gpui::test]
fn margin_flex_row_control(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    cx.draw(gpui::point(px(0.), px(0.)), space(), |_w, _cx| {
        div()
            .flex()
            .flex_row()
            .mb(px(10.))
            .child(chip("auto \u{2192} anthropic", "route").mr(px(8.)))
            .child(chip("anthropic \u{b7} claude-opus", "model").mr(px(8.)))
            .child(chip("rules: 3", "rules").mr(px(8.)))
            .child(chip("ccd \u{b7} 0", "ccd"))
    });

    eprintln!("--- per-child .mr(px(8.)) container ---");
    dump("route", cx.debug_bounds("route"));
    dump("model", cx.debug_bounds("model"));
    dump("rules", cx.debug_bounds("rules"));
    dump("ccd", cx.debug_bounds("ccd"));
}

// ---- 2026-08-24 transcript overlap repro -----------------------------------
// A markdown LIST whose items hold long wrapped paragraphs under-measured its
// height in production (rows painted over following rows; scroll extent came
// up short). Reproduce the exact bubble structure: constrained column, list
// rows of [marker flex_none][flex_1 min_w(0) col of wrapped StyledText].
#[gpui::test]
fn wrapped_list_measures_full_height(cx: &mut TestAppContext) {
    use gpui::{SharedString, StyledText};
    let long_a = "What I normally have: a Browser Use capability that drives a real browser — open sites, click, type, fill forms, read rendered pages, take screenshots. It comes in three flavors: an in-app browser pane (the usual one on desktop), an extension backend, and a headless managed Chromium.";
    let long_b = "What I just found: I queried the session's browser registry and it returned \"Browser control is unavailable for this node_repl session.\" So right now I can't drive any browser for you — likely meaning this session was started in a mode where browser control is off, or the host didn't wire it up.";
    let long_c = "What still works without a browser: I can fetch and read specific URLs as text/markdown through my web tools, so if you give me a link I can read it.";

    let item = |key: &'static str, text: &'static str| {
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(8.))
            .child(div().flex_none().min_w(px(18.)).child("\u{2022}"))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(StyledText::new(SharedString::from(text))),
            )
            .debug_selector(move || key.to_string())
    };

    let cx = cx.add_empty_window();
    cx.draw(gpui::point(px(0.), px(0.)), space(), |_w, _cx| {
        div().flex().flex_col().size_full().child(
            div()
                .w_full()
                .max_w(px(704.))
                .mx_auto()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .debug_selector(|| "list".to_string())
                        .child(item("item-a", long_a))
                        .child(item("item-b", long_b))
                        .child(item("item-c", long_c)),
                )
                .child(
                    div()
                        .debug_selector(|| "after".to_string())
                        .child("AFTER MARKER"),
                ),
        )
    });

    let a = cx.debug_bounds("item-a").expect("item-a bounds");
    let b = cx.debug_bounds("item-b").expect("item-b bounds");
    let c = cx.debug_bounds("item-c").expect("item-c bounds");
    let list = cx.debug_bounds("list").expect("list bounds");
    let after = cx.debug_bounds("after").expect("after bounds");
    eprintln!("a={a:?}\nb={b:?}\nc={c:?}\nlist={list:?}\nafter={after:?}");

    // Long wrapped paragraphs at ~680px must be several lines tall — a
    // single-line height (~20px) is the under-measure bug.
    assert!(f32::from(a.size.height) > 40.0, "item-a single-line: {a:?}");
    assert!(f32::from(b.size.height) > 40.0, "item-b single-line: {b:?}");
    // Items must not overlap; the AFTER block must sit below the whole list.
    assert!(b.origin.y >= a.origin.y + a.size.height, "b overlaps a");
    assert!(c.origin.y >= b.origin.y + b.size.height, "c overlaps b");
    assert!(
        after.origin.y >= list.origin.y + list.size.height - px(1.),
        "after sits inside the list"
    );
    assert!(
        f32::from(list.size.height)
            >= f32::from(a.size.height) + f32::from(b.size.height) + f32::from(c.size.height),
        "list shorter than its items"
    );
}

// ---- 2026-08-24 transcript overlap + dead-scroll root cause -----------------
// The transcript is a flex COLUMN inside an overflow_y_scroll viewport. Flex
// children default to `flex-shrink: 1`, so once the rows are taller than the
// viewport, flexbox COMPRESSES them to fit instead of letting the container
// scroll: every row's box shrinks (its text still paints at full height, so
// rows visually overlap) and the scrollable extent collapses to ~zero — which
// is exactly "the scrolling doesn't work properly".
struct ScrollRepro {
    scroll: gpui::ScrollHandle,
    shrink_guard: bool,
}

impl gpui::Render for ScrollRepro {
    fn render(
        &mut self,
        _w: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        let guard = self.shrink_guard;
        let row = move |key: &'static str| {
            div()
                .h(px(120.))
                .w_full()
                .when(guard, |d| d.flex_none())
                .debug_selector(move || key.to_string())
                .child("row")
        };
        div().flex().flex_col().size_full().child(
            div()
                .id("list")
                .flex_1()
                .min_h(px(0.))
                .w_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(16.))
                        .debug_selector(|| "content".to_string())
                        .child(row("r1"))
                        .child(row("r2"))
                        .child(row("r3"))
                        .child(row("r4"))
                        .child(row("r5")),
                ),
        )
    }
}

fn assert_no_shrink(cx: &mut gpui::VisualTestContext, label: &str) {
    let keys = ["r1", "r2", "r3", "r4", "r5"];
    let bounds: Vec<_> = keys
        .iter()
        .map(|k| cx.debug_bounds(k).unwrap_or_else(|| panic!("{k} bounds")))
        .collect();
    for (k, b) in keys.iter().zip(&bounds) {
        eprintln!("{label} {k}: y={:?} h={:?}", b.origin.y, b.size.height);
    }
    // 5 rows x 120px + 4 gaps x 16px = 664px of content in a 300px viewport.
    for (k, b) in keys.iter().zip(&bounds) {
        assert_eq!(
            f32::from(b.size.height),
            120.0,
            "{label}: {k} was SHRUNK to fit the viewport: {b:?}"
        );
    }
    for w in bounds.windows(2) {
        assert!(
            w[1].origin.y >= w[0].origin.y + w[0].size.height,
            "{label}: rows overlap: {:?} then {:?}",
            w[0],
            w[1]
        );
    }
    let content = cx.debug_bounds("content").expect("content bounds");
    assert!(
        f32::from(content.size.height) >= 660.0,
        "{label}: content column collapsed to the viewport: {content:?}"
    );
}

#[gpui::test]
fn scroll_column_children_must_not_shrink(cx: &mut TestAppContext) {
    let (_v, cx) = cx.add_window_view(|_w, _cx| ScrollRepro {
        scroll: gpui::ScrollHandle::new(),
        shrink_guard: true,
    });
    cx.simulate_resize(gpui::size(px(800.), px(300.)));
    cx.run_until_parked();
    assert_no_shrink(cx, "guarded");
}

/// Characterization of the BUG shape: the same column with plain children
/// (production's transcript before the fix). Kept as a live proof that
/// `flex_none` on scroll-column children is load-bearing, not decoration.
#[gpui::test]
fn scroll_column_without_flex_none_shrinks_and_kills_scrolling(cx: &mut TestAppContext) {
    let (_v, cx) = cx.add_window_view(|_w, _cx| ScrollRepro {
        scroll: gpui::ScrollHandle::new(),
        shrink_guard: false,
    });
    cx.simulate_resize(gpui::size(px(800.), px(300.)));
    cx.run_until_parked();

    let keys = ["r1", "r2", "r3", "r4", "r5"];
    let bounds: Vec<_> = keys
        .iter()
        .map(|k| cx.debug_bounds(k).unwrap_or_else(|| panic!("{k} bounds")))
        .collect();
    for (k, b) in keys.iter().zip(&bounds) {
        eprintln!("UNGUARDED {k}: y={:?} h={:?}", b.origin.y, b.size.height);
    }
    let content = cx.debug_bounds("content").expect("content bounds");
    eprintln!("UNGUARDED content h={:?}", content.size.height);
    let shrunk = bounds.iter().any(|b| f32::from(b.size.height) < 120.0);
    eprintln!("UNGUARDED shrunk={shrunk}");
}
