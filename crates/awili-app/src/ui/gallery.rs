//! `ui::gallery::Gallery` — the kit's visual reference (KIT.md § The
//! gallery, batch K5).
//!
//! `AWILI_DESIGN_GALLERY=1` opens one window with this view instead of the
//! app: built only from `ui::` parts, with no app data. Every part shows in
//! every state — rest, hover (frozen), focused, selected, disabled with its
//! reason, a long-text case — and every `Tone`. Real pointer hover works on
//! the live samples too.
//!
//! The overlays are opened from buttons on the page (the Dialog traps
//! focus, so it is never open by default): "Open the dialog", "Open the
//! palette". The anchored Menu, the Popover and the narrow Sheet form sit
//! in fixed frames so they can be seen next to the rest.
//!
//! The last section shows the five mockup screens rebuilt from kit parts
//! (`ui::gallery_screens`).

use gpui::{
    div, prelude::*, AnyElement, App, Bounds, Context, ElementId, IntoElement, Pixels,
    ScrollHandle, SharedString, Window,
};

use crate::icons::Icon;
use crate::theme::{self, palette, Tone};
use crate::ui::banner::Banner;
use crate::ui::button::Button;
use crate::ui::card::Card;
use crate::ui::chart_legend::{ChartLegend, ChartLegendItem};
use crate::ui::chip::Chip;
use crate::ui::choice::Choice;
use crate::ui::code_block::CodeBlock;
use crate::ui::composer_frame::ComposerFrame;
use crate::ui::dialog::Dialog;
use crate::ui::divider::Divider;
use crate::ui::dot::Dot;
use crate::ui::empty_state::EmptyState;
use crate::ui::icon_button::IconButton;
use crate::ui::level::Level;
use crate::ui::menu::{Menu, MenuItem};
use crate::ui::nav_rail::NavRail;
use crate::ui::notice::{Notice, NoticeLifetime, NoticeSpec, NoticeStack, NoticeStackState};
use crate::ui::palette::{Palette, PaletteRow};
use crate::ui::panel::Panel;
use crate::ui::pill::Pill;
use crate::ui::popover::Popover;
use crate::ui::row::Row;
use crate::ui::scrollbar::{ScrollBar, ScrollBarState};
use crate::ui::search_field::SearchField;
use crate::ui::section::Section;
use crate::ui::setting_row::SettingRow;
use crate::ui::sheet::Sheet;
use crate::ui::skeleton::Skeleton;
use crate::ui::stepper::Stepper;
use crate::ui::tabs::{TabItem, Tabs};
use crate::ui::terminal_frame::TerminalFrame;
use crate::ui::text;
use crate::ui::text_field::TextField;
use crate::ui::thinking_dot::ThinkingDot;
use crate::ui::toggle::Toggle;

const TONES: [(Tone, &str); 4] = [
    (Tone::Neutral, "Neutral"),
    (Tone::Attention, "Attention"),
    (Tone::ActNow, "Act now"),
    (Tone::Contrast, "Contrast"),
];

const LONG: &str = "A deliberately long label that has to truncate with an ellipsis";

/// The gallery's own state: the open overlays, the live selections, the
/// notice stack and the page scroll.
pub struct Gallery {
    scroll: ScrollHandle,
    scroll_state: ScrollBarState,
    panel_scroll: ScrollHandle,
    notices: NoticeStackState,
    dialog_open: bool,
    dialog_long: bool,
    panel_scroll_state: ScrollBarState,
    screen_scroll: crate::ui::gallery_screens::ScreenScroll,
    palette_open: bool,
    palette_query: String,
    palette_selected: usize,
    menu_selected: usize,
    tab_selected: usize,
    section_open: bool,
    toggle_on: bool,
    choice_selected: usize,
    stepper_value: i64,
    level_value: f64,
    search_query: String,
    saved_stamp: u64,
}

impl Gallery {
    /// The gallery, with three notices in its stack.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let notices = NoticeStackState::new();
        for (i, (tone, text)) in [
            (Tone::Neutral, "Saved to Backstage"),
            (Tone::Attention, "The job's result is ready"),
            (Tone::ActNow, "Sign in to Claude to continue"),
        ]
        .into_iter()
        .enumerate()
        {
            notices.push(
                NoticeSpec::new(tone, text)
                    .id(format!("gallery-notice-{i}"))
                    .lifetime(NoticeLifetime::UntilDismissed),
                cx,
            );
        }
        Self {
            scroll: ScrollHandle::new(),
            scroll_state: ScrollBarState::new(),
            panel_scroll: ScrollHandle::new(),
            notices,
            dialog_open: false,
            dialog_long: false,
            panel_scroll_state: ScrollBarState::new(),
            screen_scroll: Default::default(),
            palette_open: false,
            palette_query: String::new(),
            palette_selected: 0,
            menu_selected: 0,
            tab_selected: 0,
            section_open: true,
            toggle_on: true,
            choice_selected: 0,
            stepper_value: 3,
            level_value: 0.4,
            search_query: String::new(),
            saved_stamp: 0,
        }
    }

    /// Open the gallery window (`AWILI_DESIGN_GALLERY=1`).
    pub fn open(cx: &mut App) {
        let bounds = Bounds::centered(
            None,
            gpui::size(gpui::px(theme::GALLERY_W), gpui::px(theme::GALLERY_H)),
            cx,
        );
        let _ = cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Awili design gallery".into()),
                    ..Default::default()
                }),
                app_id: Some("awili-gallery".into()),
                ..Default::default()
            },
            |_window, cx| cx.new(Gallery::new),
        );
    }
}

/// A labelled sample: the part over its `type_::CAPTION` state name.
fn sample(caption: &str, part: impl IntoElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(theme::z(theme::SP_4))
        .child(part)
        .child(text::caption(caption.to_string()))
        .into_any_element()
}

/// A gallery section: a heading and a wrapping row of samples.
fn section(id: &'static str, title: &str, samples: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(theme::z(theme::SP_12))
        .debug_selector(move || format!("ui-gallery-{id}"))
        .child(text::heading(title.to_string()))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .gap(theme::z(theme::SP_24))
                .children(samples),
        )
        .child(Divider::new())
        .into_any_element()
}

/// A fixed frame for a part that fills its parent (overlays, sheets).
fn frame(w: Pixels, h: Pixels, part: impl IntoElement) -> AnyElement {
    div()
        .relative()
        .w(w)
        .h(h)
        .rounded(theme::z(theme::RADIUS_L))
        .overflow_hidden()
        .bg(palette().canvas)
        .border_1()
        .border_color(palette().line)
        .child(part)
        .into_any_element()
}

/// The five interactive states of one part, plus the long-text case.
fn states<T>(make: impl Fn(&str, &'static str) -> T) -> Vec<AnyElement>
where
    T: IntoElement + StateOptions,
{
    vec![
        sample("Rest", make("rest", "Send")),
        sample("Hover", make("hover", "Send").hovered_opt(true)),
        sample("Focused", make("focused", "Send").focused_opt(true)),
        sample(
            "Disabled",
            make("disabled", "Send").disabled_opt("The job is running"),
        ),
        sample("Long text", narrow(make("long", LONG))),
    ]
}

/// A part in the gallery's narrow column, so long text has to truncate.
fn narrow(part: impl IntoElement) -> AnyElement {
    div()
        .flex()
        .w(theme::z(theme::GALLERY_NARROW))
        .child(part)
        .into_any_element()
}

/// The shared state builders, so `states` can drive any part.
trait StateOptions: Sized {
    fn hovered_opt(self, on: bool) -> Self;
    fn focused_opt(self, on: bool) -> Self;
    fn disabled_opt(self, reason: &str) -> Self;
}

macro_rules! state_options {
    ($($t:ty),*) => {$(
        impl StateOptions for $t {
            fn hovered_opt(self, on: bool) -> Self { self.hovered(on) }
            fn focused_opt(self, on: bool) -> Self { self.focused(on) }
            fn disabled_opt(self, reason: &str) -> Self { self.disabled(reason.to_string()) }
        }
    )*};
}
state_options!(Button, IconButton, Chip, Row);

impl Gallery {
    fn text_section(&self) -> AnyElement {
        let mut samples = vec![
            sample("title", text::title("Workshop")),
            sample("heading", text::heading("Today")),
            sample("body", text::body("A short line of prose.")),
            sample("ui", text::ui_text("Dense interface text")),
            sample("label", text::label("Control label")),
            sample("caption", text::caption("14:32 · 3 messages")),
            sample("reading", text::read("Long reading text sits in TEXT_2.")),
            sample("dim", text::ui_text("Secondary line").dim()),
            sample("strong", text::ui_text("Weight 600").strong()),
            sample("contrast", text::ui_text("More · Code").contrast()),
            sample("mono", text::ui_text("cargo test --locked").mono()),
        ];
        for (tone, name) in TONES {
            samples.push(sample(
                &format!("tone: {name}"),
                text::ui_text(name).tone(tone),
            ));
        }
        samples.push(sample(
            "Long text, one line: truncates (full text in the tooltip)",
            narrow(text::ui_text(LONG).id("gtext-long")),
        ));
        samples.push(sample(
            "Long text, .wrap(): wraps",
            narrow(text::ui_text(LONG).id("gtext-wrap").wrap()),
        ));
        section("text", "Type", samples)
    }

    fn status_section(&self) -> AnyElement {
        let mut samples: Vec<AnyElement> = TONES
            .iter()
            .map(|(tone, name)| {
                sample(
                    name,
                    Dot::new(*tone)
                        .label(*name)
                        .id(SharedString::from(format!("gallery-dot-{name}"))),
                )
            })
            .collect();
        for (tone, name) in TONES {
            samples.push(sample(
                &format!("{name}, large"),
                Dot::new(tone)
                    .large()
                    .label(name)
                    .id(SharedString::from(format!("gallery-dot-large-{name}"))),
            ));
        }
        samples.push(sample(
            "Act now, pulsing",
            Dot::new(Tone::ActNow).pulse().phase(4).label("Act now"),
        ));
        for frame in [0u8, 5, 11] {
            samples.push(sample(
                &format!("Thinking, frame {frame}"),
                ThinkingDot::new(frame),
            ));
        }
        samples.push(sample(
            "Thinking, still (Reduce Motion)",
            ThinkingDot::new(0).still(),
        ));
        samples.push(sample(
            "Divider, vertical",
            div()
                .flex()
                .h(theme::z(theme::SP_40))
                .gap(theme::z(theme::SP_12))
                .items_center()
                .child(text::label("Copy"))
                .child(div().h_full().flex().child(Divider::new().vertical()))
                .child(text::label("Retry")),
        ));
        section("status", "Status", samples)
    }

    fn buttons_section(&self) -> AnyElement {
        let mut samples = Vec::new();
        for (name, face) in [("primary", 0u8), ("ghost", 1), ("quiet", 2), ("lead", 3)] {
            for s in states(|id, label| {
                let b = Button::new(SharedString::from(format!("gb-{name}-{id}")), label);
                match face {
                    0 => b.primary(),
                    1 => b.ghost(),
                    2 => b.quiet(),
                    _ => b.lead(),
                }
            }) {
                samples.push(s);
            }
        }
        for (name, face) in [("primary", 0u8), ("ghost", 1), ("quiet", 2), ("lead", 3)] {
            let b = Button::new(SharedString::from(format!("gb-{name}-selected")), "Grid")
                .selected(true);
            samples.push(sample(
                &format!("Selected, {name}"),
                match face {
                    0 => b.primary(),
                    1 => b.ghost(),
                    2 => b.quiet(),
                    _ => b.lead(),
                },
            ));
        }
        samples.push(sample(
            "With icon",
            Button::new("gb-icon", "Search").icon(Icon::Search),
        ));
        section("buttons", "Buttons", samples)
    }

    fn icon_buttons_section(&self) -> AnyElement {
        // Every face through every state (the ghost face is the default).
        let mut samples = Vec::new();
        for (name, face) in [("ghost", 1u8), ("primary", 0), ("quiet", 2), ("lead", 3)] {
            samples.extend(states(|id, label| {
                let b = IconButton::new(SharedString::from(format!("gib-{name}-{id}")), Icon::Plus)
                    .label(label);
                match face {
                    0 => b.primary(),
                    1 => b.ghost(),
                    2 => b.quiet(),
                    _ => b.lead(),
                }
            }));
        }
        samples.push(sample(
            "Primary, round",
            IconButton::new("gib-send", Icon::ArrowUp)
                .label("Send")
                .primary()
                .round(),
        ));
        samples.push(sample(
            "Stop",
            IconButton::new("gib-stop", Icon::Square)
                .label("Stop")
                .ghost()
                .round(),
        ));
        samples.push(sample(
            "Quiet",
            IconButton::new("gib-quiet", Icon::MoreHorizontal)
                .label("More")
                .quiet(),
        ));
        samples.push(sample(
            "Lead",
            IconButton::new("gib-lead", Icon::Zap).label("Run").lead(),
        ));
        for (tone, name) in TONES.iter() {
            samples.push(sample(
                &format!("Badge: {name}"),
                IconButton::new(SharedString::from(format!("gib-badge-{name}")), Icon::Gear)
                    .label("Settings")
                    .badge(*tone),
            ));
        }
        section("icon-buttons", "Icon buttons", samples)
    }

    fn chips_section(&self) -> AnyElement {
        let mut samples = states(|id, label| {
            Chip::new(SharedString::from(format!("gc-{id}")), label).icon(Icon::Zap)
        });
        samples.push(sample(
            "File",
            Chip::new("gc-file", "file").file("render_pipeline_configuration.rs"),
        ));
        samples.push(sample(
            "Removable",
            Chip::new("gc-rm", "notes.md")
                .file("notes.md")
                .removable(|_e, _w, _cx| {}),
        ));
        for (tone, name) in TONES {
            samples.push(sample(
                name,
                Chip::new(SharedString::from(format!("gc-tone-{name}")), name).tone(tone),
            ));
        }
        section("chips", "Chips", samples)
    }

    fn banners_and_cards_section(&self) -> AnyElement {
        let mut samples: Vec<AnyElement> = TONES
            .iter()
            .map(|(tone, name)| {
                sample(
                    name,
                    div().w(theme::z(theme::PANEL_W)).child(
                        Banner::new(
                            SharedString::from(format!("gbn-{name}")),
                            *tone,
                            "Config file has an error on line 12",
                        )
                        .action(Button::new(
                            SharedString::from(format!("gbn-{name}-fix")),
                            "Open",
                        )),
                    ),
                )
            })
            .collect();
        for (tone, name) in TONES {
            samples.push(sample(
                &format!("Card: {name}"),
                div().w(theme::z(theme::PANEL_W)).child(
                    Card::new(SharedString::from(format!("gcd-{name}")))
                        .tone(tone)
                        .label(name)
                        .title("Approve the plan?")
                        .body("Three steps, about four minutes, no files outside the repo.")
                        .actions([
                            Button::new(SharedString::from(format!("gcd-{name}-ok")), "Approve")
                                .primary(),
                            Button::new(SharedString::from(format!("gcd-{name}-no")), "Not now"),
                        ]),
                ),
            ));
        }
        section("banners-cards", "Banners and cards", samples)
    }

    fn lists_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut samples = states(|id, label| {
            Row::new(SharedString::from(format!("gr-{id}")))
                .title(label)
                .subtitle("12 messages · today")
                .icon(Icon::FileText)
                .trailing(text::caption("14:32"))
        });
        samples = samples
            .into_iter()
            .map(|s| {
                div()
                    .w(theme::z(theme::PANEL_W))
                    .child(s)
                    .into_any_element()
            })
            .collect();
        samples.push(sample(
            "Selected",
            div().w(theme::z(theme::PANEL_W)).child(
                Row::new("gr-selected")
                    .title("The open conversation")
                    .subtitle("now")
                    .selected(true),
            ),
        ));
        let open = self.section_open;
        samples.push(sample(
            "Section",
            div().w(theme::z(theme::PANEL_W)).child(
                Section::new("gs-open", "Details for nerds")
                    .open(open)
                    .body([text::caption("Model: claude-opus-5-5 · 18k tokens")])
                    .on_toggle(cx.listener(|this, _e, _w, cx| {
                        this.section_open = !this.section_open;
                        cx.notify();
                    })),
            ),
        ));
        samples.push(sample(
            "Section, disabled",
            div()
                .w(theme::z(theme::PANEL_W))
                .child(Section::new("gs-off", "Advanced").disabled("Sign in first")),
        ));
        samples.push(sample(
            "Section, focused",
            div()
                .w(theme::z(theme::PANEL_W))
                .child(Section::new("gs-focus", "Sources").focused(true)),
        ));
        samples.push(sample(
            "Section, long title",
            narrow(Section::new("gs-long", LONG)),
        ));
        samples.push(sample(
            "Section, nerd (closed, placed last)",
            div()
                .w(theme::z(theme::PANEL_W))
                .child(Section::new("gs-nerd", "Details for nerds").nerd()),
        ));
        samples.push(sample(
            "Tabs, focused; long labels",
            div().w(theme::z(theme::PANEL_W)).child(
                Tabs::new(
                    "gt-focus",
                    vec![
                        TabItem::new("gtf-a", LONG),
                        TabItem::new("gtf-b", "Sources"),
                        TabItem::new("gtf-c", LONG),
                    ],
                    0,
                )
                .focused(true),
            ),
        ));
        samples.push(sample(
            "Tabs, disabled",
            div().w(theme::z(theme::PANEL_W)).child(
                Tabs::new(
                    "gt-off",
                    vec![
                        TabItem::new("gto-a", "Answer"),
                        TabItem::new("gto-b", "Sources").closable(|_e, _w, _cx| {}),
                    ],
                    0,
                )
                .disabled("The job is running"),
            ),
        ));
        let selected = self.tab_selected;
        samples.push(sample(
            "Tabs",
            div().w(theme::z(theme::PANEL_W + theme::SP_56)).child(
                Tabs::new(
                    "gt",
                    vec![
                        TabItem::new("gt-a", "Answer").dot(Tone::Neutral),
                        TabItem::new("gt-b", "Sources").dot(Tone::Attention),
                        TabItem::new("gt-c", "Steps").closable(|_e, _w, _cx| {}),
                        TabItem::new("gt-d", "Raw").disabled("Read only"),
                    ],
                    selected,
                )
                .on_select({
                    let g = cx.entity().downgrade();
                    move |i, _e, _w, cx| {
                        let _ = g.update(cx, |g, cx| {
                            g.tab_selected = i;
                            cx.notify();
                        });
                    }
                }),
            ),
        ));
        section("lists", "Lists and structure", samples)
    }

    fn surfaces_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let body = div()
            .flex()
            .flex_col()
            .gap(theme::z(theme::SP_8))
            .children((0..24).map(|i| {
                Row::new(SharedString::from(format!("gp-row-{i}")))
                    .title(format!("Source {i}"))
                    .subtitle("docs.rs · cited twice")
            }));
        let menu_selected = self.menu_selected;
        let anchor = Bounds::new(
            gpui::point(theme::z(theme::SP_20), theme::z(theme::SP_20)),
            gpui::size(theme::target() * 2., theme::target()),
        );
        let samples =
            vec![
                sample(
                    "Panel (tabs, scrolling body, scrollbar)",
                    div().flex().flex_col().h(theme::z(theme::PANEL_W)).child(
                        Panel::new("gpanel")
                            // A specimen: below 880 its Sheet form takes no focus.
                            .pointer_only()
                            .header("Reply", Some(SharedString::from("Inspector")))
                            .tabs(Tabs::new(
                                "gpanel-tabs",
                                vec![
                                    TabItem::new("gpt-a", "Answer"),
                                    TabItem::new("gpt-b", "Sources"),
                                ],
                                1,
                            ))
                            .body(body)
                            .scroll_handle(self.panel_scroll.clone())
                            .scroll_state(self.panel_scroll_state.clone())
                            .full_height()
                            .on_close(|_e, _w, _cx| {}),
                    ),
                ),
                sample(
                    "Pill: the title",
                    Pill::new("gpill")
                        .child(text::ui_text("PhotoCraft UI study").strong())
                        .child(text::ui_text("· Workshop").dim())
                        .child(
                            IconButton::new("gpill-c", Icon::ChevronDown)
                                .label("Conversations")
                                .round(),
                        ),
                ),
                sample(
                    "Pill: the island",
                    Pill::new("gpill-island")
                        .island()
                        .child(Dot::new(Tone::ActNow).label("Waiting for you"))
                        .child(text::label("A job is waiting for you"))
                        .child(Button::new("gpill-island-a", "Review").lead().in_pill()),
                ),
                sample(
                    "Pill: the task bar",
                    Pill::new("gpill-bar")
                        .bar()
                        .child(Button::new("gpill-a", "Copy").quiet().in_pill())
                        .child(Button::new("gpill-b", "Open report").lead().in_pill()),
                ),
                sample(
                    "Pill: focused; disabled; long text",
                    div()
                        .flex()
                        .flex_col()
                        .gap(theme::z(theme::SP_8))
                        .child(
                            Pill::new("gpill-focus")
                                .focused(true)
                                .child(text::ui_text("Focused")),
                        )
                        .child(
                            Pill::new("gpill-off")
                                .disabled("Nothing to show yet")
                                .child(text::ui_text("Disabled").dim()),
                        )
                        .child(narrow(
                            Pill::new("gpill-long").child(text::ui_text(LONG).strong()),
                        )),
                ),
                sample(
                    "Panel: focused, long title",
                    div().h(theme::z(theme::MENU_W)).child(
                        Panel::new("gpanel-focus")
                            .pointer_only()
                            .header(LONG, Some(SharedString::from(LONG)))
                            .focused(true)
                            .body(text::ui_text("A short body."))
                            .on_close(|_e, _w, _cx| {}),
                    ),
                ),
                sample(
                    "Panel: disabled",
                    div().h(theme::z(theme::MENU_W)).child(
                        Panel::new("gpanel-off")
                            .header("Sources", None)
                            .body_with(|reason| {
                                let t = text::ui_text("Read only while the job runs.");
                                if reason.is_some() {
                                    t.dim()
                                } else {
                                    t
                                }
                            })
                            .disabled("The job is running")
                            .on_close(|_e, _w, _cx| {}),
                    ),
                ),
                sample(
                    "Menu: disabled",
                    Menu::new("gmenu-off")
                        .item(MenuItem::new("Rename").icon(Icon::FileText))
                        .item(MenuItem::new("Archive").icon(Icon::Archive))
                        .disabled("Sign in first"),
                ),
                sample(
                    "Menu: focused, long item",
                    Menu::new("gmenu-long")
                        .item(MenuItem::new(LONG).icon(Icon::FileText))
                        .item(MenuItem::new("Archive").icon(Icon::Archive))
                        .focused(true),
                ),
                sample(
                    "Menu with a disabled item",
                    Menu::new("gmenu")
                        .item(MenuItem::new("Rename").icon(Icon::FileText).shortcut("⌘R"))
                        .item(MenuItem::new("Archive").icon(Icon::Archive))
                        .separator()
                        .item(MenuItem::new("Export").disabled("Nothing to export yet"))
                        .selected(menu_selected)
                        .on_move({
                            let g = cx.entity().downgrade();
                            move |i, _w, cx| {
                                let _ = g.update(cx, |g, cx| {
                                    g.menu_selected = i;
                                    cx.notify();
                                });
                            }
                        }),
                ),
                sample(
                    "Popover (anchored, settles after measuring)",
                    frame(
                        theme::z(theme::PANEL_W + theme::SP_56),
                        theme::z(theme::MENU_W),
                        Popover::new("gpop", anchor)
                            .content(text::ui_text("Anchored below its trigger.").wrap()),
                    ),
                ),
                sample(
                    "Popover: focused, long content; disabled",
                    div()
                        .flex()
                        .gap(theme::z(theme::SP_8))
                        .child(frame(
                            theme::z(theme::PANEL_W + theme::SP_56),
                            theme::z(theme::MENU_W),
                            Popover::new("gpop-focus", anchor)
                                .focused(true)
                                .content(text::ui_text(LONG).wrap()),
                        ))
                        .child(frame(
                            theme::z(theme::PANEL_W + theme::SP_56),
                            theme::z(theme::MENU_W),
                            Popover::new("gpop-off", anchor)
                                .disabled("Nothing to change yet")
                                .content(text::ui_text("Disabled: dimmed, never hidden.").wrap()),
                        )),
                ),
                sample(
                    "Sheet (the narrow form of a Panel)",
                    frame(
                        theme::z(theme::PANEL_W + theme::SP_56),
                        theme::z(theme::PANEL_W),
                        Sheet::new("gsheet")
                            // A specimen: it takes no focus from the page.
                            .pointer_only()
                            .header("Sources", None)
                            .body(text::ui_text("Below 880 px a panel becomes this sheet.").wrap())
                            .on_close(|_e, _w, _cx| {}),
                    ),
                ),
                sample(
                    "Sheet: long title, disabled",
                    frame(
                        theme::z(theme::PANEL_W + theme::SP_56),
                        theme::z(theme::MENU_W),
                        Sheet::new("gsheet-long")
                            .header(LONG, None)
                            .body_with(|reason| {
                                let t = text::ui_text("Read only while the job runs.").wrap();
                                if reason.is_some() {
                                    t.dim()
                                } else {
                                    t
                                }
                            })
                            .disabled("The job is running")
                            .on_close(|_e, _w, _cx| {}),
                    ),
                ),
                sample(
                    "Notices: every tone, an action and \"Don't show again\"; long text",
                    div()
                        .flex()
                        .flex_col()
                        .gap(theme::z(theme::SP_8))
                        .w(theme::z(theme::PANEL_W))
                        .children(TONES.iter().map(|(tone, name)| {
                            Notice::new(
                                SharedString::from(format!("gnotice-{name}")),
                                *tone,
                                format!("{name}: the job's result is ready"),
                            )
                            .action("Open", |_e, _w, _cx| {})
                            .dismiss_pref(
                                SharedString::from(format!("gallery.notice.{name}")),
                                |_e, _w, _cx| {},
                            )
                            .on_dismiss(|_e, _w, _cx| {})
                        }))
                        .child(
                            Notice::new("gnotice-long", Tone::Neutral, LONG)
                                .on_dismiss(|_e, _w, _cx| {}),
                        ),
                ),
                sample(
                    "Dialog and palette",
                    div()
                        .flex()
                        .gap(theme::z(theme::SP_8))
                        .child(Button::new("gopen-dialog", "Open the dialog").on_click(
                            cx.listener(|this, _e, _w, cx| {
                                this.dialog_open = true;
                                cx.notify();
                            }),
                        ))
                        .child(
                            Button::new("gopen-dialog-long", "Open the dialog (long text)")
                                .on_click(cx.listener(|this, _e, _w, cx| {
                                    this.dialog_open = true;
                                    this.dialog_long = true;
                                    cx.notify();
                                })),
                        )
                        .child(Button::new("gopen-palette", "Open the palette").on_click(
                            cx.listener(|this, _e, _w, cx| {
                                this.palette_open = true;
                                cx.notify();
                            }),
                        )),
                ),
            ];
        section("surfaces", "Surfaces", samples)
    }

    fn dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let long = self.dialog_long;
        let close = {
            let g = cx.entity().downgrade();
            move |_w: &mut Window, cx: &mut App| {
                let _ = g.update(cx, |g, cx| {
                    g.dialog_open = false;
                    g.dialog_long = false;
                    cx.notify();
                });
            }
        };
        div()
            .absolute()
            .size_full()
            .child(
                Dialog::new(
                    "gdialog",
                    if long {
                        LONG
                    } else {
                        "Delete this conversation?"
                    },
                    text::read(if long {
                        "Its 42 messages, their attachments, the job history that came out of \
                         them and every branch made from any of them are removed. This cannot \
                         be undone."
                    } else {
                        "Its 42 messages are removed. This cannot be undone."
                    })
                    .wrap(),
                )
                .cancel("Keep it", |_e, _w, _cx| {})
                .primary(
                    if long { LONG } else { "Delete conversation" },
                    |_e, _w, _cx| {},
                )
                .destructive()
                .on_close(close),
            )
            .into_any_element()
    }

    fn palette(&self, cx: &mut Context<Self>) -> AnyElement {
        let g = cx.entity().downgrade();
        let rows = |labels: &[&str]| labels.iter().map(|l| PaletteRow::new(*l)).collect();
        div()
            .absolute()
            .size_full()
            .child(
                Palette::new("gpalette")
                    .query(self.palette_query.clone())
                    .group(
                        "Actions",
                        rows(&["Switch model", "Ask the council", "New conversation"]),
                    )
                    .group(
                        "Jobs",
                        rows(&["Open the last job", "Straße report", "İstanbul notes"])
                            .into_iter()
                            .chain([
                                PaletteRow::new(LONG),
                                PaletteRow::new("Export the job log")
                                    .disabled("Nothing to export yet"),
                            ])
                            .collect(),
                    )
                    .selected(self.palette_selected)
                    .on_query({
                        let g = g.clone();
                        move |q, _w, cx| {
                            let _ = g.update(cx, |g, cx| {
                                g.palette_query = q;
                                cx.notify();
                            });
                        }
                    })
                    .on_move({
                        let g = g.clone();
                        move |i, _w, cx| {
                            let _ = g.update(cx, |g, cx| {
                                g.palette_selected = i;
                                cx.notify();
                            });
                        }
                    })
                    .on_close(move |_w, cx| {
                        let _ = g.update(cx, |g, cx| {
                            g.palette_open = false;
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }
}

impl Gallery {
    /// A closure that updates the gallery from a part's callback.
    fn on<T: 'static>(
        cx: &mut Context<Self>,
        f: impl Fn(&mut Self, T) + 'static,
    ) -> impl Fn(T, &mut Window, &mut App) + 'static {
        let g = cx.entity().downgrade();
        let f = std::rc::Rc::new(f);
        move |v: T, _w: &mut Window, cx: &mut App| {
            let f = f.clone();
            let _ = g.update(cx, move |g, cx| {
                f(g, v);
                cx.notify();
            });
        }
    }

    fn settings_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let row = |s: SettingRow| {
            div()
                .w(theme::z(theme::SETTINGS_ROW_W))
                .child(s)
                .into_any_element()
        };
        let toggle_on = self.toggle_on;
        let set_toggle = Self::on(cx, |g: &mut Self, ()| g.toggle_on = !g.toggle_on);
        let choice = self.choice_selected;
        let set_choice = Self::on(cx, |g: &mut Self, i: usize| g.choice_selected = i);
        let stepper = self.stepper_value;
        let set_stepper = Self::on(cx, |g: &mut Self, v: i64| g.stepper_value = v);
        let level = self.level_value;
        let set_level = Self::on(cx, |g: &mut Self, v: f64| g.level_value = v);
        let query = self.search_query.clone();
        let set_query = Self::on(cx, |g: &mut Self, q: String| g.search_query = q);
        let saved = self.saved_stamp;
        let save = Self::on(cx, |g: &mut Self, ()| g.saved_stamp += 1);
        let opts =
            || -> Vec<SharedString> { vec!["Ask".into(), "Go ahead".into(), "Never".into()] };

        let samples = vec![
            sample(
                "Toggle — live",
                row(SettingRow::new("gs-toggle", "Speak replies aloud")
                    .help("Reads each reply when it arrives.")
                    .control(move |r| {
                        Toggle::new("gs-toggle-c", toggle_on)
                            .on_toggle(move |_e, w, cx| set_toggle((), w, cx))
                            .disabled_if(r)
                    })),
            ),
            sample(
                "Toggle states: on, off, hover, focused, disabled",
                div()
                    .flex()
                    .gap(theme::z(theme::SP_12))
                    .child(Toggle::new("gt-on", true))
                    .child(Toggle::new("gt-off", false))
                    .child(Toggle::new("gt-hover", false).hovered(true))
                    .child(Toggle::new("gt-focus", true).focused(true))
                    .child(Toggle::new("gt-dis", true).disabled("Managed by your workspace"))
                    .child(Toggle::new("gt-dis-off", false).disabled("Managed by your workspace")),
            ),
            sample(
                "Choice — live; hover; armed; disabled",
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_8))
                    .child(
                        Choice::new("gc-live", opts(), choice)
                            .on_change(move |i, _e, w, cx| set_choice(i, w, cx)),
                    )
                    .child(Choice::new("gc-hover", opts(), 0).hovered(true))
                    .child(Choice::new("gc-armed", opts(), 0).confirm_to(Some(1)))
                    .child(Choice::new("gc-dis", opts(), 1).disabled("Set by your workspace"))
                    .child(Choice::new("gc-focus", opts(), 2).focused(true)),
            ),
            sample(
                "Choice — long options (each tooltips its full text)",
                div().w(theme::z(theme::PANEL_W)).child(Choice::new(
                    "gc-long",
                    vec![LONG.into(), "Go ahead".into(), LONG.into()],
                    1,
                )),
            ),
            sample(
                "Stepper — live (hold to repeat); at the bounds; disabled",
                div()
                    .flex()
                    .gap(theme::z(theme::SP_16))
                    .child(
                        Stepper::new("gst-live", stepper, 0, 20, 1)
                            .unit(" retries")
                            .on_change(move |v, _e, w, cx| set_stepper(v, w, cx)),
                    )
                    .child(Stepper::new("gst-min", 0, 0, 5, 1))
                    .child(Stepper::new("gst-max", 5, 0, 5, 1))
                    .child(Stepper::new("gst-dis", 3, 0, 5, 1).disabled("Locked"))
                    .child(Stepper::new("gst-focus", 2, 0, 5, 1).focused(true)),
            ),
            sample(
                "Level — live; log scale; disabled",
                div()
                    .flex()
                    .flex_col()
                    .w(theme::z(theme::PANEL_W))
                    .gap(theme::z(theme::SP_8))
                    .child(
                        Level::new("gl-live", level, 0.0, 1.0)
                            .on_change(move |v, _e, w, cx| set_level(v, w, cx)),
                    )
                    .child(Level::new("gl-log", 300.0, 30.0, 3000.0).log())
                    .child(Level::new("gl-notches", 0.5, 0.0, 1.0).notches(5))
                    .child(Level::new("gl-hover", 0.6, 0.0, 1.0).hovered(true))
                    .child(Level::new("gl-focus", 0.3, 0.0, 1.0).focused(true))
                    .child(Level::new("gl-dis", 0.4, 0.0, 1.0).disabled("Needs a model")),
            ),
            sample(
                "Search field — live; disabled",
                div()
                    .flex()
                    .flex_col()
                    .w(theme::z(theme::PANEL_W))
                    .gap(theme::z(theme::SP_8))
                    .child(
                        SearchField::new("gsf-live", "Search settings…")
                            .query(query)
                            .on_query(move |q, w, cx| set_query(q, w, cx)),
                    )
                    .child(SearchField::new("gsf-hover", "Search…").hovered(true))
                    .child(SearchField::new("gsf-focus", "Search…").focused(true))
                    .child(SearchField::new("gsf-long", "Search…").query(LONG))
                    .child(SearchField::new("gsf-dis", "Search…").disabled("Index is rebuilding")),
            ),
            sample(
                "Text field: value, placeholder, secret stored, secret not set, disabled",
                div()
                    .flex()
                    .flex_col()
                    .w(theme::z(theme::PANEL_W))
                    .gap(theme::z(theme::SP_8))
                    .child(TextField::new("gtf-val", "Name").value("Awili"))
                    .child(TextField::new("gtf-ph", "Your name"))
                    .child(TextField::new("gtf-sec", "API key").secret(true).on_clear(|_e, _w, _c| {}))
                    .child(TextField::new("gtf-none", "API key").secret(false))
                    .child(TextField::new("gtf-dis", "Name").value("Awili").disabled("Synced from your profile"))
                    .child(TextField::new("gtf-hover", "Name").value("Awili").hovered(true))
                    .child(TextField::new("gtf-focus", "Name").value("Awili").focused(true))
                    .child(TextField::new("gtf-long", "Name").value(LONG)),
            ),
            sample(
                "Text field holding an editor (the app's editor sits in the slot); a secret being replaced",
                div()
                    .flex()
                    .flex_col()
                    .w(theme::z(theme::PANEL_W))
                    .gap(theme::z(theme::SP_8))
                    .child(
                        TextField::new("gtf-edit", "Name")
                            .editor(|_r| text::body("Awili, being edited")),
                    )
                    .child(
                        TextField::new("gtf-replace", "API key")
                            .secret(true)
                            .editing(true)
                            .editor(|_r| text::body("Paste the new key").dim())
                            .on_replace(|_e, _w, _c| {})
                            .on_clear(|_e, _w, _c| {}),
                    ),
            ),
            sample(
                "Setting row: changed, armed reset, confirm hint, saved (click), restart, unmet dependency, disabled, its control focused, a long label and help",
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_8))
                    .child(row(SettingRow::new("gsr-changed", "Fleet route")
                        .control(|r| Toggle::new("gsr-c1", true).disabled_if(r))
                        .changed(true)
                        .on_reset(|_e, _w, _c| {})))
                    .child(row(SettingRow::new("gsr-armed", "Fleet route")
                        .control(|r| Toggle::new("gsr-c2", true).disabled_if(r))
                        .changed(true)
                        .confirm_leave(true)
                        .on_reset(|_e, _w, _c| {})))
                    .child(row(SettingRow::new("gsr-hint", "Mode")
                        .control(move |r| Choice::new("gsr-c3", opts(), 0).confirm_to(Some(1)).disabled_if(r))
                        .confirm_to(true)))
                    .child(row(SettingRow::new("gsr-saved", "Theme")
                        .control(move |r| {
                            Button::new("gsr-save", "Save")
                                .on_click(move |_e, w, cx| save((), w, cx))
                                .disabled_if(r)
                        })
                        .saved(saved)))
                    .child(row(SettingRow::new("gsr-restart", "Voice engine")
                        .control(|r| Toggle::new("gsr-c5", true).disabled_if(r))
                        .restart_needed(true)))
                    .child(row(SettingRow::new("gsr-dep", "Model")
                        .control(|r| Toggle::new("gsr-c6", false).disabled_if(r))
                        .depends_on("The fleet route must be Cloud.", false)))
                    .child(row(SettingRow::new("gsr-dis", "Model")
                        .control(|r| Toggle::new("gsr-c7", false).disabled_if(r))
                        .changed(true)
                        .on_reset(|_e, _w, _c| {})
                        .disabled("Locked by policy")))
                    .child(row(SettingRow::new("gsr-focus", "Speak replies aloud")
                        .control(|r| Toggle::new("gsr-c8", true).focused(true).disabled_if(r))))
                    .child(row(SettingRow::new("gsr-long", LONG)
                        .help("A long help line that wraps under the label instead of truncating, \
                               so the whole explanation stays readable at any width.")
                        .control(|r| Toggle::new("gsr-c9", false).disabled_if(r)))),
            ),
        ];
        section("settings", "Settings controls", samples)
    }

    fn frames_section(&self) -> AnyElement {
        let rail = |id: &'static str| {
            let item = |name: &str| SharedString::from(format!("{id}-{name}"));
            NavRail::new(id)
                .item(item("workshop"), Icon::Chat, "Workshop", true)
                .item(item("council"), Icon::People, "Council", false)
                .badge(1, Tone::Attention)
                .item(item("design"), Icon::Palette, "Design", false)
                .item(item("backstage"), Icon::Servers, "Backstage", false)
                .divider()
                .item(item("more"), Icon::MoreHorizontal, "More", false)
        };
        let rails = div()
            .flex()
            .items_start()
            .gap(theme::z(theme::SP_24))
            .child(sample(
                "Design hovered, a Council badge",
                rail("gn").item_hovered(2),
            ))
            .child(sample("Focused", rail("gn-focus").focused(true)))
            .child(sample("Disabled", rail("gn-off").disabled("Sign in first")))
            .child(sample(
                "More open",
                rail("gn-more").more(
                    Menu::new("gn-more-menu")
                        .item(MenuItem::new("Settings").icon(Icon::Gear))
                        .item(MenuItem::new("Help").icon(Icon::FileText)),
                ),
            ));
        let samples = vec![
            sample(
                "Composer",
                div().w(theme::z(theme::COMPOSER_MAX)).child(
                    ComposerFrame::new("gcf")
                        .field(text::read("Ask anything, or describe the work").dim())
                        .leading(vec![IconButton::new("gcf-attach", Icon::Plus)
                            .label("Attach")
                            .round()])
                        .trailing(vec![
                            IconButton::new("gcf-talk", Icon::Mic)
                                .label("Talk")
                                .round()
                                .into_any_element(),
                            IconButton::new("gcf-send", Icon::ArrowUp)
                                .label("Send")
                                .round()
                                .primary()
                                .into_any_element(),
                        ])
                        .footer(vec![
                            Chip::new("gcf-model", "Gemma · Mac Studio ▾"),
                            Chip::new("gcf-folder", "awili-ui-fishing ▾"),
                        ])
                        .footer_end(vec![Chip::new("gcf-status", "All good").dot(Tone::Neutral)]),
                ),
            ),
            sample("Nav rail", rails),
            sample(
                "Code block",
                div().w(theme::z(theme::READING_MAX)).child(CodeBlock::new(
                    "gcb",
                    "rust",
                    "fn main() {\n    println!(\"Stay on GPUI.\");\n}",
                )),
            ),
            sample(
                "Terminal frame (running)",
                div().w(theme::z(theme::READING_MAX)).child(
                    TerminalFrame::new("gtf")
                        .title("awili-ui-fishing")
                        .running(Tone::Neutral)
                        .on_stop(|_e, _w, _c| {})
                        .on_clear(|_e, _w, _c| {})
                        .body(|_r| text::ui_text("$ cargo test --locked").mono()),
                ),
            ),
            sample(
                "Terminal frame: idle; disabled",
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_8))
                    .w(theme::z(theme::READING_MAX))
                    .child(
                        TerminalFrame::new("gtf-idle")
                            .title("awili-ui-fishing")
                            .on_clear(|_e, _w, _c| {})
                            .body(|_r| text::ui_text("$").mono()),
                    )
                    .child(
                        TerminalFrame::new("gtf-off")
                            .title(LONG)
                            .on_clear(|_e, _w, _c| {})
                            .disabled("The session ended")
                            .body(|_r| text::ui_text("$ exit").mono()),
                    ),
            ),
            sample(
                "Chart legend",
                ChartLegend::new(
                    "gcl",
                    vec![
                        ChartLegendItem::input("Input tokens"),
                        ChartLegendItem::output("Output tokens"),
                    ],
                ),
            ),
        ];
        section("frames", "App frames", samples)
    }

    fn content_section(&self) -> AnyElement {
        section(
            "content",
            "Content states",
            vec![
                sample(
                    "Empty state",
                    div().w(theme::z(theme::READING_MAX)).child(
                        EmptyState::new(
                            "ges",
                            "No conversations yet",
                            "Start one below and it shows here.",
                        )
                        .glyph(Icon::MessageCircle)
                        .action(Button::new("ges-new", "New conversation").primary()),
                    ),
                ),
                sample(
                    "Skeleton",
                    div()
                        .w(theme::z(theme::PANEL_W))
                        .child(Skeleton::lines(3).blocks(1)),
                ),
            ],
        )
    }

    fn screens_section(&self) -> AnyElement {
        let screens = crate::ui::gallery_screens::screens(&self.screen_scroll)
            .into_iter()
            .map(|(_, caption, screen)| sample(caption, screen))
            .collect();
        section(
            "screens",
            "The five mockup screens, rebuilt from kit parts",
            screens,
        )
    }
}

impl Render for Gallery {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette();
        let page = div()
            .id("ui-gallery-page")
            .size_full()
            // Both axes: the 1200-wide screens and the wide samples scroll
            // sideways in a narrow window (860) instead of being clipped.
            .overflow_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_32))
                    .p(theme::z(theme::SP_40))
                    .min_w(theme::z(theme::GALLERY_PAGE_MIN_W))
                    .child(text::title("Awili design gallery"))
                    .child(text::caption(
                        "Every kit part, every state. Hover is live; the frozen hover shows \
                         the same paint. Parts the mockup gives no hover face (Tabs, Section, \
                         SettingRow, the Stepper frame, TerminalFrame, Notice and the floating \
                         faces) have no hover sample.",
                    ))
                    .child(self.text_section())
                    .child(self.status_section())
                    .child(self.buttons_section())
                    .child(self.icon_buttons_section())
                    .child(self.chips_section())
                    .child(self.banners_and_cards_section())
                    .child(self.lists_section(cx))
                    .child(self.surfaces_section(cx))
                    .child(self.settings_section(cx))
                    .child(self.frames_section())
                    .child(self.content_section())
                    .child(self.screens_section()),
            );
        let notices = div()
            .absolute()
            .top(theme::z(theme::EDGE))
            .right(theme::z(theme::EDGE))
            .child(NoticeStack::new("gnotices", self.notices.clone()));
        div()
            .size_full()
            .relative()
            .bg(p.canvas)
            .text_color(p.text)
            .debug_selector(|| "ui-gallery".to_string())
            .child(page)
            .child(notices)
            .when(self.dialog_open, |d| d.child(self.dialog(cx)))
            .when(self.palette_open, |d| d.child(self.palette(cx)))
            .child(ScrollBar::new(
                "ui-gallery-scrollbar",
                self.scroll.clone(),
                self.scroll_state.clone(),
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use std::ops::Deref;

    /// Every section and every screen, by selector.
    const LAID_OUT: [&str; 18] = [
        "ui-gallery",
        "ui-gallery-text",
        "ui-gallery-status",
        "ui-gallery-buttons",
        "ui-gallery-icon-buttons",
        "ui-gallery-chips",
        "ui-gallery-banners-cards",
        "ui-gallery-lists",
        "ui-gallery-surfaces",
        "ui-gallery-settings",
        "ui-gallery-frames",
        "ui-gallery-content",
        "ui-gallery-screens",
        "ui-gallery-screen-workshop",
        "ui-gallery-screen-job",
        "ui-gallery-screen-council",
        "ui-gallery-screen-design",
        "ui-gallery-screen-backstage",
    ];

    /// The gallery in a window `width` wide and `height` tall.
    fn gallery(
        cx: &mut TestAppContext,
        width: f32,
        height: f32,
    ) -> (gpui::Entity<Gallery>, &mut VisualTestContext) {
        let window = cx.open_window(gpui::size(gpui::px(width), gpui::px(height)), |_w, cx| {
            Gallery::new(cx)
        });
        let view = window.root(cx).unwrap();
        let wcx = VisualTestContext::from_window(*window.deref(), cx).into_mut();
        wcx.run_until_parked();
        (view, wcx)
    }

    /// A gallery window tall enough that the whole page is on screen (the
    /// lower sections are far down a normal window's scroll).
    fn tall_gallery(cx: &mut TestAppContext) -> (gpui::Entity<Gallery>, &mut VisualTestContext) {
        gallery(cx, theme::GALLERY_W, theme::GALLERY_H * 40.0)
    }

    /// A real click (down and up) at the centre of `selector`.
    fn click(wcx: &mut VisualTestContext, selector: &'static str) {
        let at = wcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} laid out"))
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

    /// KIT.md § Tests: the gallery renders headless at 1280 wide with every
    /// section and screen laid out, and the screens at their `SCREEN_W` ×
    /// `SCREEN_H` frame.
    #[gpui::test]
    fn gallery_renders_at_1280(cx: &mut TestAppContext) {
        let (_view, wcx) = gallery(cx, theme::GALLERY_W, theme::GALLERY_H);
        for sel in LAID_OUT {
            assert!(wcx.debug_bounds(sel).is_some(), "1280: {sel} laid out");
        }
        let screen = wcx.debug_bounds("ui-gallery-screen-workshop").unwrap();
        assert_eq!(
            f32::from(screen.size.width),
            f32::from(theme::z(theme::SCREEN_W))
        );
        assert_eq!(
            f32::from(screen.size.height),
            f32::from(theme::z(theme::SCREEN_H))
        );
    }

    /// At 860 (below `COMPACT_W`) every section still lays out, the panels
    /// paint their Sheet form (the scrim is the sheet's), and the page
    /// scrolls sideways to the 1200-wide screens instead of clipping them.
    #[gpui::test]
    fn gallery_at_860_shows_sheets_and_scrolls_sideways(cx: &mut TestAppContext) {
        let (view, wcx) = gallery(cx, 860.0, theme::GALLERY_H);
        for sel in LAID_OUT {
            assert!(wcx.debug_bounds(sel).is_some(), "860: {sel} laid out");
        }
        assert!(
            wcx.debug_bounds("ui-sheet-scrim").is_some(),
            "860: the panels paint the Sheet form"
        );
        // Opened at 860, no static specimen takes focus: the Panels' Sheet
        // forms are pointer-only.
        assert!(
            wcx.update(|w, cx| w.focused(cx)).is_none(),
            "860: no specimen takes focus"
        );
        // The Job screen's panel is a sheet here, and keeps its footer's Stop.
        let job_panel = wcx
            .debug_bounds("ui-panel-job-panel")
            .expect("the job panel");
        let stop = wcx
            .debug_bounds("ui-button-job-panel-stop")
            .expect("860: the sheet keeps the job panel's footer");
        assert!(
            job_panel.contains(&stop.center()),
            "{stop:?} inside {job_panel:?}"
        );
        let max = view.read_with(wcx, |g, _| g.scroll.max_offset());
        assert!(
            f32::from(max.x) > 0.0,
            "860: the page scrolls sideways (max offset {max:?})"
        );

        // The Panel sample is the Sheet form here: it fills its sample box
        // (narrower than `PANEL_W`); at 1280 it is a `PANEL_W` panel whose
        // body scrolls inside the same box.
        let panel_box = f32::from(theme::z(theme::PANEL_W));
        let narrow = wcx.debug_bounds("ui-panel-gpanel").unwrap();
        assert!(
            f32::from(narrow.size.width) < panel_box,
            "860: sheet form {narrow:?}"
        );
        assert_eq!(
            f32::from(narrow.size.height),
            panel_box,
            "860: fills its box"
        );
        let (_wide, wcx) = gallery(cx, theme::GALLERY_W, theme::GALLERY_H);
        let wide = wcx.debug_bounds("ui-panel-gpanel").unwrap();
        assert_eq!(
            f32::from(wide.size.width),
            panel_box,
            "1280: a PANEL_W panel"
        );
        assert_eq!(
            f32::from(wide.size.height),
            panel_box,
            "1280: scrolls inside its box"
        );
    }

    /// The long-text samples sit in the narrow column, so the parts truncate
    /// inside it rather than growing past it.
    #[gpui::test]
    fn gallery_long_text_samples_stay_in_the_narrow_column(cx: &mut TestAppContext) {
        let (_view, wcx) = tall_gallery(cx);
        let narrow = f32::from(theme::z(theme::GALLERY_NARROW));
        for sel in [
            "ui-button-gb-primary-long",
            "ui-button-gb-ghost-long",
            "ui-button-gb-quiet-long",
            "ui-button-gb-lead-long",
            "ui-chip-gc-long",
        ] {
            let b = wcx
                .debug_bounds(sel)
                .unwrap_or_else(|| panic!("{sel} laid out"));
            assert!(
                f32::from(b.size.width) <= narrow + 0.5,
                "{sel} is {} wide, more than the {narrow} px column",
                f32::from(b.size.width)
            );
        }
        // The one-line text slot truncates: one line tall and inside the
        // column, while the same text with `.wrap()` takes several lines.
        let one = wcx
            .debug_bounds("ui-text-gtext-long")
            .expect("the truncating text");
        let wrapped = wcx
            .debug_bounds("ui-text-gtext-wrap")
            .expect("the wrapping text");
        assert!(f32::from(one.size.width) <= narrow + 0.5, "{one:?}");
        assert_eq!(
            one.size.height,
            theme::z(theme::type_::UI.line),
            "one line: {one:?}"
        );
        assert!(
            wrapped.size.height > one.size.height,
            "wrapped is taller: {wrapped:?}"
        );
    }

    /// The overlays open from their buttons on the page, by a real click,
    /// and close again: Escape closes the dialog and the palette.
    #[gpui::test]
    fn gallery_opens_and_closes_the_dialog_and_the_palette(cx: &mut TestAppContext) {
        let (view, wcx) = tall_gallery(cx);
        assert!(
            wcx.debug_bounds("ui-dialog-gdialog-panel").is_none(),
            "closed at first"
        );

        click(wcx, "ui-button-gopen-dialog");
        assert!(
            wcx.debug_bounds("ui-dialog-gdialog-panel").is_some(),
            "clicking Open the dialog opens it"
        );
        wcx.simulate_keystrokes("escape");
        wcx.run_until_parked();
        assert!(
            !view.read_with(wcx, |g, _| g.dialog_open),
            "Escape closes the dialog"
        );
        assert!(
            wcx.debug_bounds("ui-dialog-gdialog-panel").is_none(),
            "and it is gone"
        );

        click(wcx, "ui-button-gopen-palette");
        assert!(
            wcx.debug_bounds("ui-palette-gpalette-panel").is_some(),
            "clicking Open the palette opens it"
        );
        wcx.simulate_keystrokes("escape");
        wcx.run_until_parked();
        assert!(
            !view.read_with(wcx, |g, _| g.palette_open),
            "Escape closes the palette"
        );
        assert!(
            wcx.debug_bounds("ui-palette-gpalette-panel").is_none(),
            "and it is gone"
        );
    }

    /// Narrowing a focused gallery from 1280 to 860 leaves focus where it
    /// was: the Panels turn into their Sheet forms, which are pointer-only
    /// specimens and take no focus.
    #[gpui::test]
    fn narrowing_the_gallery_keeps_focus(cx: &mut TestAppContext) {
        let (_view, wcx) = gallery(cx, theme::GALLERY_W, theme::GALLERY_H);
        wcx.update(|w, cx| w.focus_next(cx));
        wcx.run_until_parked();
        let before = wcx.update(|w, cx| w.focused(cx));
        assert!(before.is_some(), "something on the page holds focus");
        wcx.simulate_resize(gpui::size(gpui::px(860.0), gpui::px(theme::GALLERY_H)));
        wcx.run_until_parked();
        assert!(wcx.debug_bounds("ui-sheet-scrim").is_some(), "now compact");
        assert_eq!(
            wcx.update(|w, cx| w.focused(cx)),
            before,
            "focus stayed put"
        );
    }

    /// The live settings samples drive the gallery: clicking the live
    /// toggle flips its state (the control is built through the row's
    /// closure).
    #[gpui::test]
    fn gallery_live_toggle_flips(cx: &mut TestAppContext) {
        let (view, wcx) = tall_gallery(cx);
        let before = view.read_with(wcx, |g, _| g.toggle_on);
        click(wcx, "ui-toggle-gs-toggle-c");
        assert_ne!(view.read_with(wcx, |g, _| g.toggle_on), before);
    }
}
