//! Whole-kit contract tests (KIT.md § Conventions): rules every part must
//! keep, checked part by part through real rendering and real input.
//!
//! **A part's own sub-controls follow its double-click setting.** With
//! `.allow_double_click_if(true)` a double click on a part's own button,
//! row or overlay layer fires its handler twice; without, once.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{div, prelude::*, px, Bounds, IntoElement, Point, TestAppContext};

use crate::ui::interaction::test_util::{double_click, double_click_on, render_part};

/// For `allow` false then true: render the part, double click where
/// `target` says, and expect one fire, then two.
fn check<E, F>(cx: &mut TestAppContext, what: &str, build: F, target: Target)
where
    E: IntoElement + 'static,
    F: Fn(bool, Rc<Cell<u32>>) -> E + Clone + 'static,
{
    for allow in [false, true] {
        let fired = Rc::new(Cell::new(0u32));
        let (f, b) = (fired.clone(), build.clone());
        let (_root, wcx) = render_part(cx, move || b(allow, f.clone()));
        wcx.run_until_parked();
        match target {
            Target::Selector(sel) => double_click_on(wcx, sel),
            Target::Point(x, y) => double_click(wcx, Point { x: px(x), y: px(y) }),
            Target::CornerOf(sel) => {
                let b = wcx
                    .debug_bounds(sel)
                    .unwrap_or_else(|| panic!("{sel} recorded no bounds"));
                double_click(
                    wcx,
                    b.origin
                        + Point {
                            x: px(4.0),
                            y: px(4.0),
                        },
                );
            }
        }
        assert_eq!(
            fired.get(),
            if allow { 2 } else { 1 },
            "{what}: allow={allow}"
        );
    }
}

#[derive(Clone, Copy)]
enum Target {
    Selector(&'static str),
    Point(f32, f32),
    CornerOf(&'static str),
}

fn count(f: &Rc<Cell<u32>>) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) {
    let f = f.clone();
    move |_e, _w, _c| f.set(f.get() + 1)
}

#[gpui::test]
fn menu_rows_follow_the_double_click_setting(cx: &mut TestAppContext) {
    use crate::ui::menu::{Menu, MenuItem};
    check(
        cx,
        "menu row",
        |allow, f| {
            Menu::new("m")
                .item(MenuItem::new("Rename").on_click(count(&f)))
                .allow_double_click_if(allow)
        },
        Target::Selector("ui-row-m-item-0"),
    );
}

#[gpui::test]
fn popover_outside_click_follows_the_double_click_setting(cx: &mut TestAppContext) {
    use crate::ui::popover::Popover;
    check(
        cx,
        "popover outside click",
        |allow, f| {
            let f = f.clone();
            Popover::new(
                "pop",
                Bounds {
                    origin: Point {
                        x: px(40.),
                        y: px(40.),
                    },
                    size: gpui::size(px(100.), px(20.)),
                },
            )
            .content(crate::ui::text::ui_text("Row"))
            .allow_double_click_if(allow)
            .on_close(move |_e, _w, _c| f.set(f.get() + 1))
        },
        Target::Point(460.0, 380.0),
    );
}

#[gpui::test]
fn palette_rows_and_scrim_follow_the_double_click_setting(cx: &mut TestAppContext) {
    use crate::ui::palette::{Palette, PaletteRow};
    check(
        cx,
        "palette row",
        |allow, f| {
            div().size_full().child(
                Palette::new("cmd")
                    .group("Actions", vec![PaletteRow::new("Run").on_click(count(&f))])
                    .allow_double_click_if(allow),
            )
        },
        Target::Selector("ui-palette-row-cmd-row-0"),
    );
    check(
        cx,
        "palette scrim",
        |allow, f| {
            let f = f.clone();
            div().size_full().child(
                Palette::new("cmd")
                    .group("Actions", vec![PaletteRow::new("Run")])
                    .allow_double_click_if(allow)
                    .on_close(move |_w, _c| f.set(f.get() + 1)),
            )
        },
        Target::CornerOf("ui-palette-scrim"),
    );
}

#[gpui::test]
fn dialog_buttons_follow_the_double_click_setting(cx: &mut TestAppContext) {
    use crate::ui::dialog::Dialog;
    check(
        cx,
        "dialog cancel",
        |allow, f| {
            Dialog::new("d", "Delete?", crate::ui::text::read("Body").wrap())
                .cancel("Keep it", count(&f))
                .allow_double_click_if(allow)
        },
        Target::Selector("ui-button-d-cancel"),
    );
}

#[gpui::test]
fn notice_controls_follow_the_double_click_setting(cx: &mut TestAppContext) {
    use crate::theme::Tone;
    use crate::ui::notice::Notice;
    check(
        cx,
        "notice action",
        |allow, f| {
            Notice::new("n", Tone::Neutral, "Saved")
                .action("Open", count(&f))
                .allow_double_click_if(allow)
        },
        Target::Selector("ui-button-n-action"),
    );
    check(
        cx,
        "notice dismiss",
        |allow, f| {
            Notice::new("n", Tone::Neutral, "Saved")
                .on_dismiss(count(&f))
                .allow_double_click_if(allow)
        },
        Target::Selector("ui-icon-button-n-close"),
    );
}

#[gpui::test]
fn search_field_clear_follows_the_double_click_setting(cx: &mut TestAppContext) {
    use crate::ui::search_field::SearchField;
    check(
        cx,
        "search clear",
        |allow, f| {
            let f = f.clone();
            div().w(px(320.)).child(
                SearchField::new("s", "Search…")
                    .query("abc")
                    .on_query(move |q, _w, _c| {
                        if q.is_empty() {
                            f.set(f.get() + 1);
                        }
                    })
                    .allow_double_click_if(allow),
            )
        },
        Target::Selector("ui-icon-button-s-clear"),
    );
}

#[gpui::test]
fn terminal_frame_actions_follow_the_double_click_setting(cx: &mut TestAppContext) {
    use crate::theme::Tone;
    use crate::ui::terminal_frame::TerminalFrame;
    check(
        cx,
        "terminal stop",
        |allow, f| {
            div().w(px(560.)).child(
                TerminalFrame::new("tf")
                    .title("awili")
                    .running(Tone::Neutral)
                    .on_stop(count(&f))
                    .allow_double_click_if(allow)
                    .body(|_r| crate::ui::text::ui_text("$")),
            )
        },
        Target::Selector("ui-icon-button-tf-stop"),
    );
}

#[gpui::test]
fn tabs_new_follows_the_double_click_setting(cx: &mut TestAppContext) {
    use crate::ui::tabs::{TabItem, Tabs};
    check(
        cx,
        "tabs +",
        |allow, f| {
            Tabs::new("t", vec![TabItem::new("a", "Answer")], 0)
                .on_new(count(&f))
                .allow_double_click_if(allow)
        },
        Target::Selector("ui-icon-button-t-tab-new"),
    );
}

#[gpui::test]
fn sheet_close_and_scrim_follow_the_double_click_setting(cx: &mut TestAppContext) {
    use crate::ui::sheet::Sheet;
    check(
        cx,
        "sheet close",
        |allow, f| {
            Sheet::new("s")
                .header("Sources", None)
                .allow_double_click_if(allow)
                .on_close(count(&f))
        },
        Target::Selector("ui-icon-button-s-close"),
    );
    check(
        cx,
        "sheet scrim",
        |allow, f| {
            Sheet::new("s")
                .header("Sources", None)
                .allow_double_click_if(allow)
                .on_close(count(&f))
        },
        Target::CornerOf("ui-sheet-scrim"),
    );
}

// Composed parts hand the setting to the kit parts they build.

#[gpui::test]
fn anchored_menu_outside_click_follows_the_double_click_setting(cx: &mut TestAppContext) {
    use crate::ui::menu::{Menu, MenuItem};
    check(
        cx,
        "anchored menu outside click",
        |allow, f| {
            let f = f.clone();
            div().size_full().relative().child(
                Menu::new("m")
                    .item(MenuItem::new("Rename"))
                    .anchor(Bounds {
                        origin: Point {
                            x: px(40.),
                            y: px(40.),
                        },
                        size: gpui::size(px(100.), px(20.)),
                    })
                    .allow_double_click_if(allow)
                    .on_close(move |_w, _c| f.set(f.get() + 1)),
            )
        },
        Target::Point(460.0, 380.0),
    );
}

#[gpui::test]
fn palette_search_clear_follows_the_double_click_setting(cx: &mut TestAppContext) {
    use crate::ui::palette::{Palette, PaletteRow};
    check(
        cx,
        "palette search clear",
        |allow, f| {
            let f = f.clone();
            div().size_full().child(
                Palette::new("cmd")
                    .query("ru")
                    .group("Actions", vec![PaletteRow::new("Run")])
                    .allow_double_click_if(allow)
                    .on_query(move |q, _w, _c| {
                        if q.is_empty() {
                            f.set(f.get() + 1);
                        }
                    }),
            )
        },
        Target::Selector("ui-icon-button-cmd-search-clear"),
    );
}

#[gpui::test]
fn notice_stack_controls_follow_the_double_click_setting(cx: &mut TestAppContext) {
    use crate::theme::Tone;
    use crate::ui::notice::{NoticeLifetime, NoticeSpec, NoticeStack, NoticeStackState};
    for allow in [false, true] {
        let fired = Rc::new(Cell::new(0u32));
        let state = NoticeStackState::new();
        let (s, f) = (state.clone(), fired.clone());
        cx.update(|app| {
            s.push(
                NoticeSpec::new(Tone::Neutral, "Saved")
                    .id("n1")
                    .lifetime(NoticeLifetime::UntilDismissed)
                    .action("Open", move |_e, _w, _c| f.set(f.get() + 1)),
                app,
            );
        });
        let (_root, wcx) = render_part(cx, move || {
            div()
                .flex()
                .child(NoticeStack::new("corner", state.clone()).allow_double_click_if(allow))
        });
        double_click_on(wcx, "ui-button-n1-action");
        assert_eq!(
            fired.get(),
            if allow { 2 } else { 1 },
            "notice stack action: allow={allow}"
        );
    }
}

// A disabled part takes no focus, so it never shows the frozen ring; the
// same part enabled does (the control).

/// Every part with a frozen `.focused(true)`, enabled or disabled.
fn focus_specimen(which: &str, disabled: bool) -> gpui::AnyElement {
    use crate::icons::Icon;
    use crate::ui::{
        button::Button,
        chip::Chip,
        choice::Choice,
        icon_button::IconButton,
        level::Level,
        nav_rail::NavRail,
        panel::Panel,
        pill::Pill,
        row::Row,
        search_field::SearchField,
        section::Section,
        stepper::Stepper,
        tabs::{TabItem, Tabs},
        text_field::TextField,
        toggle::Toggle,
    };
    const R: &str = "Read only";
    macro_rules! d {
        ($p:expr) => {
            if disabled {
                $p.focused(true).disabled(R).into_any_element()
            } else {
                $p.focused(true).into_any_element()
            }
        };
    }
    match which {
        "button" => d!(Button::new("x", "Run")),
        "icon_button" => d!(IconButton::new("x", Icon::Gear).label("Settings")),
        "chip" => d!(Chip::new("x", "notes.md")),
        "row" => d!(Row::new("x").title("Row")),
        "section" => d!(Section::new("x", "Sources")),
        "tabs" => d!(Tabs::new("x", vec![TabItem::new("a", "Answer")], 0)),
        "pill" => d!(Pill::new("x").child(crate::ui::text::ui_text("Pill"))),
        "toggle" => d!(Toggle::new("x", true)),
        "choice" => d!(Choice::new("x", vec!["A".into(), "B".into()], 0)),
        "stepper" => d!(Stepper::new("x", 1, 0, 5, 1)),
        "level" => d!(Level::new("x", 0.5, 0.0, 1.0)),
        "nav_rail" => d!(NavRail::new("x").item("a", Icon::Chat, "Workshop", true)),
        "panel" => d!(Panel::new("x").header("Sources", None)),
        "text_field" => d!(TextField::new("x", "Name").value("Awili")),
        "search_field" => d!(SearchField::new("x", "Search…")),
        other => panic!("unknown part {other}"),
    }
}

#[gpui::test]
fn disabled_parts_never_show_the_frozen_focus_ring(cx: &mut TestAppContext) {
    for which in [
        "button",
        "icon_button",
        "chip",
        "row",
        "section",
        "tabs",
        "pill",
        "toggle",
        "choice",
        "stepper",
        "level",
        "nav_rail",
        "panel",
        "text_field",
        "search_field",
    ] {
        for disabled in [false, true] {
            let (_root, wcx) = render_part(cx, move || {
                div()
                    .w(px(480.))
                    .h(px(320.))
                    .child(focus_specimen(which, disabled))
            });
            assert_eq!(
                wcx.debug_bounds("ui-focus-ring").is_some(),
                !disabled,
                "{which}: disabled={disabled}"
            );
        }
    }
}
