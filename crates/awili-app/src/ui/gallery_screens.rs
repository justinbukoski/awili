//! The gallery's five composed screens (KIT.md § The gallery, batch K5):
//! the Studio mockups — Workshop chat (`A-studio.html`), Workshop job
//! (`A-work.html`), Council (`A-council.html`), Design (`A-design.html`)
//! and Backstage (`A-backstage.html`) — rebuilt from kit parts only, with
//! the mockups' own sample text, in `SCREEN_W` × `SCREEN_H` frames. They
//! are the side-by-side check that the kit matches the mockups.
//!
//! Geometry follows `docs/v2/mockups/studio.css` through named tokens: the
//! rail at `EDGE`, vertically centred; the title pill centred at the top;
//! the island at the top right; the stage padded `STAGE_TOP` /
//! `STAGE_RIGHT` / `STAGE_BOTTOM` / `STAGE_LEFT` with a `READING_MAX`
//! thread spaced `SP_40`; the dock `SP_24` from the bottom between the
//! same left and right; the panel from `PANEL_TOP` to `SP_24` above the
//! bottom. Off-grid values snap down (DESIGN.md §10.3). Departures: the
//! app background is the flat `CANVAS` (GPUI draws no radial gradient —
//! DESIGN.md §10.4), and each mockup's bottom `.hint` line is a design
//! note, not app UI, so it is the screen's caption in the gallery.

use gpui::{div, prelude::*, AnyElement, IntoElement, Pixels, SharedString};

use crate::icons::Icon;
use crate::theme::{self, palette, Tone};
use crate::ui::button::Button;
use crate::ui::card::Card;
use crate::ui::chip::Chip;
use crate::ui::composer_frame::ComposerFrame;
use crate::ui::dot::Dot;
use crate::ui::empty_state::EmptyState;
use crate::ui::icon_button::IconButton;
use crate::ui::nav_rail::NavRail;
use crate::ui::panel::Panel;
use crate::ui::pill::Pill;
use crate::ui::row::Row;
use crate::ui::scrollbar::{ScrollBar, ScrollBarState};
use crate::ui::section::Section;
use crate::ui::tabs::{TabItem, Tabs};
use crate::ui::text;
use crate::ui::thinking_dot::ThinkingDot;

/// One scrolling area's state: its handle and its kit scrollbar's state.
#[derive(Clone, Default)]
pub struct ScrollArea {
    handle: gpui::ScrollHandle,
    state: ScrollBarState,
}

/// The screens' scrolling areas, owned by the gallery so their offsets and
/// scrollbars survive re-renders.
#[derive(Clone, Default)]
pub struct ScreenScroll {
    ws_stage: ScrollArea,
    ws_panel: ScrollArea,
    job_stage: ScrollArea,
    job_panel: ScrollArea,
    council_stage: ScrollArea,
    design_page: ScrollArea,
    bs_page: ScrollArea,
}

/// A scrolling area with the kit scrollbar over its right edge.
fn scrolled(
    id: &'static str,
    area: &ScrollArea,
    scroller: gpui::Stateful<gpui::Div>,
) -> AnyElement {
    div()
        .absolute()
        .top(Pixels::ZERO)
        .left(Pixels::ZERO)
        .size_full()
        .child(scroller.track_scroll(&area.handle))
        .child(ScrollBar::new(
            SharedString::from(format!("{id}-scrollbar")),
            area.handle.clone(),
            area.state.clone(),
        ))
        .into_any_element()
}

/// The five screens, in the mockups' order: (id, caption — the screen and
/// its mockup file, screen).
pub fn screens(s: &ScreenScroll) -> Vec<(&'static str, &'static str, AnyElement)> {
    vec![
        ("workshop", "Workshop — A-studio.html", workshop(s)),
        ("job", "Workshop job — A-work.html", job(s)),
        ("council", "Council — A-council.html", council(s)),
        ("design", "Design — A-design.html", design(s)),
        ("backstage", "Backstage — A-backstage.html", backstage(s)),
    ]
}

/// Each mockup's own bottom `.hint` line, word for word.
fn hint(id: &str) -> &'static str {
    match id {
        "workshop" => {
            "Workshop · click the reply to select it or clear it · the rail has Workshop, \
             Council, Design, Backstage, More"
        }
        "job" => {
            "Workshop · a job: the approval is inline in the conversation; the Job panel opens \
             from the rail, the island or ⌘J"
        }
        "council" => "Council · Focus and Hide only change this view, never which models are asked",
        "design" => {
            "Design · the section exists now; what backs it comes in a later release (purpose \
             to be confirmed)"
        }
        _ => {
            "Backstage · Skills, Automations, Sessions, Connections (was Messaging) and Health \
             (was System) in one page — sample content"
        }
    }
}

/// The screen frame: the app's `CANVAS`, the screen size, its corners.
fn frame(id: &'static str, children: Vec<AnyElement>) -> AnyElement {
    div()
        .relative()
        .w(theme::z(theme::SCREEN_W))
        .h(theme::z(theme::SCREEN_H))
        .flex_none()
        .rounded(theme::z(theme::RADIUS_L))
        .overflow_hidden()
        .bg(palette().canvas)
        .debug_selector(move || format!("ui-gallery-screen-{id}"))
        .children(children)
        // The mockup's `.hint` line, centred at the bottom (its `bottom: 6px`
        // snaps to `SP_4`). Its `#5f594f` is a design-note colour with no
        // token; the caption's `TEXT_DIM` is the nearest kit colour.
        .child(
            div()
                .absolute()
                .bottom(theme::z(theme::SP_4))
                .left(Pixels::ZERO)
                .w_full()
                .flex()
                .justify_center()
                .child(text::caption(hint(id))),
        )
        // The frame's edge is painted over the screen, so it takes no
        // layout space and every placement measures from the frame's own
        // corner, as in the mockup's window.
        .child(
            div()
                .absolute()
                .size_full()
                .rounded(theme::z(theme::RADIUS_L))
                .border_1()
                .border_color(palette().line),
        )
        .into_any_element()
}

/// The left rail (`.rail { left: 20px; top: 50%; translateY(-50%) }`):
/// the v2 items with their mockup glyphs; `active` names the page.
fn rail(id: &'static str, active: usize, council_badge: bool) -> AnyElement {
    let items = [
        ("workshop", Icon::Chat, "Workshop"),
        ("council", Icon::People, "Council"),
        ("design", Icon::Palette, "Design"),
        ("backstage", Icon::Servers, "Backstage"),
    ];
    let mut rail = NavRail::new(SharedString::from(format!("{id}-rail")));
    for (i, (item, icon, label)) in items.iter().enumerate() {
        rail = rail.item(
            SharedString::from(format!("{id}-rail-{item}")),
            *icon,
            *label,
            i == active,
        );
    }
    rail = rail.divider().item(
        SharedString::from(format!("{id}-rail-more")),
        Icon::MoreHorizontal,
        "More",
        false,
    );
    if council_badge {
        rail = rail.badge(1, Tone::Attention);
    }
    div()
        .absolute()
        .left(theme::z(theme::EDGE))
        .top(Pixels::ZERO)
        .bottom(Pixels::ZERO)
        .flex()
        .items_center()
        .child(rail)
        .into_any_element()
}

/// The title pill (`.doc`, centred at the top): the title, a dim `· sub`
/// and, on a conversation, its caret.
fn doc_pill(id: &'static str, title: &str, sub: Option<&str>, caret: Option<&str>) -> AnyElement {
    let mut pill = Pill::new(SharedString::from(format!("{id}-doc")))
        .child(text::ui_text(title.to_string()).strong());
    if let Some(sub) = sub {
        pill = pill.child(text::ui_text(format!("· {sub}")).dim());
    }
    if let Some(caret) = caret {
        pill = pill.child(
            IconButton::new(SharedString::from(format!("{id}-caret")), Icon::ChevronDown)
                .label(caret.to_string())
                .round(),
        );
    }
    div()
        .absolute()
        .top(theme::z(theme::EDGE))
        .left(Pixels::ZERO)
        .w_full()
        .flex()
        .justify_center()
        .child(pill)
        .into_any_element()
}

/// The activity island (`.island`, top right): the pulsing act-now dot,
/// its line and an action.
fn island(id: &'static str, line: &str, action: Button) -> AnyElement {
    div()
        .absolute()
        .top(theme::z(theme::EDGE))
        .right(theme::z(theme::EDGE))
        .child(
            Pill::new(SharedString::from(format!("{id}-island")))
                .island()
                .child(
                    Dot::new(Tone::ActNow)
                        .pulse()
                        .phase(4)
                        .label("Waiting for you"),
                )
                .child(text::label(line.to_string()))
                .child(action.in_pill()),
        )
        .into_any_element()
}

/// The stage: padded clear of the rail, the pills, the panel (`right`)
/// and the dock, scrolling, with the centred thread at most `max` wide and
/// spaced `gap` (Workshop: `STAGE_RIGHT`, `READING_MAX`, `SP_40`).
fn stage(
    id: &'static str,
    area: &ScrollArea,
    right: f32,
    max: f32,
    gap: f32,
    thread: Vec<AnyElement>,
) -> AnyElement {
    let scroller = div()
        .id(SharedString::from(format!("{id}-stage")))
        .absolute()
        .top(Pixels::ZERO)
        .left(Pixels::ZERO)
        .size_full()
        .overflow_y_scroll()
        .pt(theme::z(theme::STAGE_TOP))
        .pr(theme::z(right))
        .pb(theme::z(theme::STAGE_BOTTOM))
        .pl(theme::z(theme::STAGE_LEFT))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(theme::z(gap))
                .w_full()
                .max_w(theme::z(max))
                .mx_auto()
                .children(thread),
        );
    scrolled(id, area, scroller)
}

/// The owner's message (`.you`): a raised bubble at the right, its
/// bottom-right corner tight.
fn you(line: &str) -> AnyElement {
    // `max-width: min(78%, 560px)`: the share caps the outer box, the
    // token the bubble.
    div()
        .flex()
        .justify_end()
        .child(
            div()
                .flex()
                .justify_end()
                .max_w(gpui::relative(theme::YOU_FRAC))
                .child(you_bubble(line)),
        )
        .into_any_element()
}

fn you_bubble(line: &str) -> AnyElement {
    div()
        .flex()
        .child(
            div()
                .max_w(theme::z(theme::YOU_MAX))
                .px(theme::z(theme::SP_16))
                .py(theme::z(theme::SP_8))
                .rounded_tl(theme::z(theme::RADIUS_L))
                .rounded_tr(theme::z(theme::RADIUS_L))
                .rounded_bl(theme::z(theme::RADIUS_L))
                .rounded_br(theme::z(theme::RADIUS_TIGHT))
                .bg(palette().raise)
                .child(text::body(line.to_string()).wrap()),
        )
        .into_any_element()
}

/// Awili's reply (`.reply`): the `TITLE` heading, the `READ` prose, the
/// numbered points with their soft-blue markers, and the meta line; a
/// selected reply carries the selection wash and edge; its More button
/// sits at the top right.
fn reply(
    id: &'static str,
    heading: &str,
    body: &str,
    points: &[&str],
    meta: &str,
    selected: bool,
) -> AnyElement {
    let p = palette();
    let marker = |n: usize| {
        div()
            .flex_none()
            .size(theme::z(theme::MARKER))
            .rounded(theme::z(theme::RADIUS_PILL))
            .bg(p.contrast_soft)
            .flex()
            .items_center()
            .justify_center()
            .child(text::label(n.to_string()).strong().contrast())
    };
    let list = div()
        .flex()
        .flex_col()
        .gap(theme::z(theme::SP_8))
        .pt(theme::z(theme::SP_4))
        .children(points.iter().enumerate().map(|(i, point)| {
            div()
                .flex()
                .gap(theme::z(theme::SP_12))
                .child(marker(i + 1))
                .child(
                    text::read(point.to_string())
                        .text_color_override(p.text)
                        .wrap(),
                )
                .into_any_element()
        }));
    div()
        .relative()
        .flex()
        .flex_col()
        .px(theme::z(theme::SP_24))
        .py(theme::z(theme::SP_20))
        .mx(-theme::z(theme::SP_24))
        .my(-theme::z(theme::SP_20))
        .rounded(theme::z(theme::RADIUS_L))
        // The edge is always there (transparent at rest), so selecting a
        // reply never shifts its text.
        .border_1()
        .border_color(if selected {
            p.selection_edge
        } else {
            p.transparent
        })
        .when(selected, |d| d.bg(p.selection_fill))
        .debug_selector(move || format!("ui-gallery-reply-{id}"))
        .child(
            div()
                .absolute()
                .top(theme::z(theme::SP_12))
                .right(theme::z(theme::SP_8))
                .child(
                    IconButton::new(
                        SharedString::from(format!("{id}-more")),
                        Icon::MoreHorizontal,
                    )
                    .label("More for this reply")
                    .round(),
                ),
        )
        .child(
            div()
                .pb(theme::z(theme::SP_8))
                .child(text::title(heading.to_string()).wrap()),
        )
        .child(
            div()
                .pb(theme::z(theme::SP_12))
                .child(text::read(body.to_string()).wrap()),
        )
        .when(!points.is_empty(), |d| d.child(list))
        .child(
            div()
                .flex()
                .items_center()
                .gap(theme::z(theme::SP_4))
                .pt(theme::z(theme::SP_12))
                .child(Dot::new(Tone::Neutral))
                .child(text::label(meta.to_string()).dim()),
        )
        .into_any_element()
}

/// The dock (`.dock`): the task bar over the composer, `SP_24` from the
/// bottom, between the stage's left and `right`.
fn dock(id: &'static str, right: f32, actions: Vec<Button>, composer: ComposerFrame) -> AnyElement {
    div()
        .absolute()
        .bottom(theme::z(theme::SP_24))
        .left(theme::z(theme::STAGE_LEFT))
        .right(theme::z(right))
        .flex()
        .flex_col()
        .items_center()
        .gap(theme::z(theme::SP_8))
        .when(!actions.is_empty(), |d| {
            d.child(
                Pill::new(SharedString::from(format!("{id}-taskbar")))
                    .bar()
                    .children(actions.into_iter().map(Button::in_pill)),
            )
        })
        .child(
            div()
                .w_full()
                .max_w(theme::z(theme::COMPOSER_MAX))
                .child(composer),
        )
        .into_any_element()
}

/// The composer: Attach (the mockup's plus), the `READ`-size
/// placeholder, Talk (optional) and Send, and the footer chips.
fn composer(
    id: &'static str,
    placeholder: &str,
    talk: bool,
    footer: Vec<Chip>,
    footer_end: Vec<Chip>,
) -> ComposerFrame {
    let mut trailing: Vec<AnyElement> = Vec::new();
    if talk {
        trailing.push(
            IconButton::new(SharedString::from(format!("{id}-talk")), Icon::Mic)
                .label("Talk")
                .round()
                .into_any_element(),
        );
    }
    trailing.push(
        IconButton::new(SharedString::from(format!("{id}-send")), Icon::ArrowUp)
            .label("Send")
            .round()
            .primary()
            .into_any_element(),
    );
    ComposerFrame::new(SharedString::from(format!("{id}-composer")))
        .field(
            div()
                .py(theme::z(theme::SP_8))
                .child(text::read(placeholder.to_string()).dim()),
        )
        .leading(vec![IconButton::new(
            SharedString::from(format!("{id}-attach")),
            Icon::Plus,
        )
        .label("Attach")
        .round()])
        .trailing(trailing)
        .footer(footer)
        .footer_end(footer_end)
}

/// The floating panel (`.panel`): from `PANEL_TOP` to `SP_24` above the
/// bottom, `EDGE` from the right. The region spans the frame's width with
/// the panel at its right end, so in a compact window (< `COMPACT_W`) the
/// panel's Sheet form has the width to fill.
fn side(panel: Panel) -> AnyElement {
    div()
        .absolute()
        .top(theme::z(theme::PANEL_TOP))
        .bottom(theme::z(theme::SP_24))
        .left(Pixels::ZERO)
        .right(theme::z(theme::EDGE))
        .flex()
        .flex_col()
        .items_end()
        .child(panel.full_height())
        .into_any_element()
}

/// A label/value line inside an inspector section (`.kv`).
fn kv(key: &str, value: &str) -> AnyElement {
    div()
        .flex()
        .justify_between()
        .gap(theme::z(theme::SP_12))
        .child(text::ui_text(key.to_string()).dim())
        .child(text::ui_text(value.to_string()))
        .into_any_element()
}

/// Workshop chat (`A-studio.html`).
fn workshop(s: &ScreenScroll) -> AnyElement {
    let inspector = Panel::new("ws-inspector")
        // A specimen: below 880 its Sheet form takes no focus from the page.
        .pointer_only()
        .header("Reply", Some(SharedString::from("Selected")))
        .scroll_handle(s.ws_panel.handle.clone())
        .scroll_state(s.ws_panel.state.clone())
        .body(
            div()
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_8))
                .child(
                    Section::new("ws-about", "About this answer")
                        .open(true)
                        .body([
                            kv("Model", "Gemma · Mac Studio"),
                            kv("Took", "4 seconds"),
                            kv("Cost", "Free (on your Mac)"),
                        ]),
                )
                .child(Section::new("ws-sources", "Sources").open(true).body([
                    Chip::new("ws-src-1", "REPORT.md · awili-ui-fishing").into_any_element(),
                    Chip::new("ws-src-2", "7 panel reviews").into_any_element(),
                ]))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(theme::z(theme::SP_8))
                        .px(theme::z(theme::SP_4))
                        .py(theme::z(theme::SP_4))
                        .child(Button::new("ws-branch", "Branch from here"))
                        .child(Button::new("ws-retry", "Retry"))
                        .child(Button::new("ws-council", "Ask the council").lead()),
                )
                .child(Section::new("ws-nerd", "Details for nerds").nerd().body([
                    kv("Tokens", "2,555 in · 1,172 out"),
                    kv("Endpoint", "hoste:8844"),
                ])),
        )
        .on_close(|_e, _w, _c| {});
    frame(
        "workshop",
        vec![
            stage(
                "ws",
                &s.ws_stage,
                theme::STAGE_RIGHT,
                theme::READING_MAX,
                theme::SP_40,
                vec![
                    you("What did the PhotoCraft study find? Short version."),
                    reply(
                        "ws-reply",
                        "Stay on GPUI. Borrow the plumbing, not the look.",
                        "Seven models reviewed it and all agreed. Awili feels heavy because \
                         everything is on screen at once — not because of the toolkit.",
                        &[
                            "One name for every action, so menus, shortcuts and agents always agree.",
                            "Let agents see the running app, so they can test it like you would.",
                            "One design language for every screen — and a light mode.",
                        ],
                        "Gemma · Mac Studio · 4 s",
                        true,
                    ),
                    you("And what would light mode look like?"),
                    ThinkingDot::new(5).into_any_element(),
                ],
            ),
            rail("ws", 0, true),
            doc_pill("ws", "PhotoCraft UI study", Some("Workshop"), Some("Conversations")),
            island(
                "ws",
                "Fix the zcode wrapper is waiting for you",
                Button::new("ws-review", "Review").lead(),
            ),
            dock(
                "ws",
                theme::STAGE_RIGHT,
                vec![
                    Button::new("ws-copy", "Copy").quiet(),
                    Button::new("ws-check", "Ask Codex to check").quiet(),
                    Button::new("ws-job", "Use another model").lead(),
                    Button::new("ws-report", "Open report").quiet(),
                ],
                composer(
                    "ws",
                    "Ask anything, or describe the work",
                    true,
                    vec![
                        Chip::new("ws-model", "Gemma · Mac Studio ▾"),
                        Chip::new("ws-folder", "awili-ui-fishing ▾").icon(Icon::Folder),
                    ],
                    vec![Chip::new("ws-status", "All good").dot(Tone::Neutral)],
                ),
            ),
            side(inspector),
        ],
    )
}

/// One History step (`.row.step`): its status dot, what was done, what it
/// found.
fn step(id: &'static str, title: &str, sub: &str, dot: Option<Tone>, selected: bool) -> AnyElement {
    let lead = match dot {
        Some(Tone::ActNow) => Dot::new(Tone::ActNow).large().label("Waiting for you"),
        Some(tone) => Dot::new(tone).large(),
        // The upcoming step.
        None => Dot::new(Tone::Neutral).large(),
    };
    Row::new(id)
        .leading(lead)
        .title(title.to_string())
        .strong()
        .subtitle(sub.to_string())
        .selected(selected)
        .into_any_element()
}

/// Workshop job (`A-work.html`).
fn job(s: &ScreenScroll) -> AnyElement {
    let panel = Panel::new("job-panel")
        .pointer_only()
        .header(
            "Fix the zcode wrapper",
            Some(SharedString::from("This job")),
        )
        .tabs(Tabs::new(
            "job-tabs",
            vec![
                TabItem::new("job-history", "History"),
                TabItem::new("job-terminal", "Terminal"),
                TabItem::new("job-previews", "Previews"),
                TabItem::new("job-context", "Context"),
            ],
            0,
        ))
        .scroll_handle(s.job_panel.handle.clone())
        .scroll_state(s.job_panel.state.clone())
        .body(
            div()
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_4))
                .child(step(
                    "job-s1",
                    "Read the zcode command",
                    "Found where it looks",
                    Some(Tone::Neutral),
                    false,
                ))
                .child(step(
                    "job-s2",
                    "Searched the ZCode app",
                    "The settings file moved",
                    Some(Tone::Neutral),
                    false,
                ))
                .child(step(
                    "job-s3",
                    "Built a fix in a scratch folder",
                    "2 links, nothing else changed",
                    Some(Tone::Neutral),
                    false,
                ))
                .child(step(
                    "job-s4",
                    "Tested GLM through the fix",
                    "GLM answered",
                    Some(Tone::Neutral),
                    false,
                ))
                .child(step(
                    "job-s5",
                    "Swap the real command",
                    "Waiting for you",
                    Some(Tone::ActNow),
                    true,
                ))
                .child(step(
                    "job-s6",
                    "Save a note and the docs",
                    "Next",
                    None,
                    false,
                ))
                .child(
                    Section::new("job-tools", "Tools used")
                        .body([kv("Read file", "3"), kv("Run command", "5")]),
                ),
        )
        .footer(
            div()
                .flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap(theme::z(theme::SP_8))
                .child(div().flex_1().min_w_0().child(
                    text::caption("Going back never undoes anything outside your Mac.").wrap(),
                ))
                .child(Button::new("job-panel-stop", "Stop")),
        )
        .on_close(|_e, _w, _c| {});
    frame(
        "job",
        vec![
            stage(
                "job",
                &s.job_stage,
                theme::STAGE_RIGHT,
                theme::READING_MAX,
                theme::SP_40,
                vec![
                    you(
                        "The zcode command stopped working after the ZCode update. Fix it so \
                         GLM reviews work again.",
                    ),
                    reply(
                        "job-reply",
                        "Found it. The update moved a settings file.",
                        "The command still looks in the old place and gives up. I built a fix \
                         in a scratch folder and tested it — GLM answered.",
                        &[],
                        "Qwen · Spark · 4 of 6 steps done",
                        false,
                    ),
                    Card::new("job-needs")
                        .tone(Tone::ActNow)
                        .label("Needs you")
                        .title("Swap in the fixed zcode command?")
                        .body(
                            "Every agent that talks to GLM uses it. The old one is kept as a \
                             backup, so this is easy to undo.",
                        )
                        .actions([
                            Button::new("job-allow", "Allow").primary(),
                            Button::new("job-not-now", "Not now"),
                            Button::new("job-more", "More choices ▾").quiet(),
                        ])
                        .into_any_element(),
                ],
            ),
            rail("job", 0, false),
            doc_pill(
                "job",
                "Fix the zcode wrapper",
                Some("in ~/.local/bin"),
                Some("Folder and projects"),
            ),
            island(
                "job",
                "Waiting for you · Qwen on Spark · 2m 14s",
                Button::new("job-stop", "Stop"),
            ),
            dock(
                "job",
                theme::STAGE_RIGHT,
                vec![
                    Button::new("job-change", "Show the change").quiet(),
                    Button::new("job-check", "Ask Codex to check").quiet(),
                    Button::new("job-log", "Copy the log").quiet(),
                ],
                composer(
                    "job",
                    "Reply, or steer the job",
                    false,
                    vec![
                        Chip::new("job-model", "Qwen · Spark ▾"),
                        Chip::new("job-ask", "Ask before changes ▾"),
                    ],
                    vec![Chip::new("job-waiting", "Waiting for you").dot(Tone::ActNow)],
                ),
            ),
            side(panel),
        ],
    )
}

/// One Council track (`.card.track`): the header first — the model,
/// where it ran, Focus and Hide — then its answer.
fn track(id: &'static str, model: &str, where_: &str, heading: &str, body: &str) -> AnyElement {
    Card::new(id)
        // Flush: the header and the body bring the mockup's own padding
        // (`.th` 8 8 8 20, `.tb` 16 20 20), divided by the header's line.
        .flush()
        .children([
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(theme::z(theme::SP_8))
                .py(theme::z(theme::SP_8))
                .pr(theme::z(theme::SP_8))
                .pl(theme::z(theme::SP_20))
                .border_b_1()
                .border_color(palette().line)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(text::ui_text(model.to_string()).strong())
                        .child(text::caption(where_.to_string())),
                )
                .child(
                    div()
                        .flex()
                        .child(
                            Button::new(SharedString::from(format!("{id}-focus")), "Focus").quiet(),
                        )
                        .child(
                            Button::new(SharedString::from(format!("{id}-hide")), "Hide").quiet(),
                        ),
                )
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_4))
                .pt(theme::z(theme::SP_16))
                .px(theme::z(theme::SP_20))
                .pb(theme::z(theme::SP_20))
                .child(text::heading(heading.to_string()).wrap())
                .child(text::read(body.to_string()).wrap())
                .into_any_element(),
        ])
        .into_any_element()
}

/// The card grid (the mockup's `grid-template-columns: repeat(auto-fit,
/// minmax(300px, 1fr))`, `gap: 16px`). In the fixed screen frame both
/// Council's 1120 and Backstage's 960 measure fit three 300 px columns,
/// so auto-fit resolves to three equal columns; a short last row keeps
/// its cells one column wide, as in the mockup.
const GRID_COLS: u16 = theme::GALLERY_GRID_COLS;

fn grid() -> gpui::Div {
    div()
        .grid()
        .grid_cols(GRID_COLS)
        .gap(theme::z(theme::SP_16))
}

/// Council (`A-council.html`).
fn council(s: &ScreenScroll) -> AnyElement {
    let agree = Card::new("council-agree")
        .tone(Tone::Attention)
        .children([div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .child(Dot::new(Tone::Attention).label("All three agree"))
            .child(text::ui_text("All three say stay on GPUI.").strong())
            .child(text::ui_text(
                "They differ on what to fix first — that one's your call.",
            ))
            .into_any_element()]);
    let tracks = grid()
        .child(track(
            "council-codex",
            "Codex",
            "GPT-6.1 · cloud",
            "Fix the basics first.",
            "Type, spacing and shared controls come before anything else. A panel-heavy \
             layout doesn't need egui — Zed already does it on GPUI.",
        ))
        .child(track(
            "council-deepseek",
            "DeepSeek",
            "V4.1 Flash · cloud",
            "Adobe's structure, not its density.",
            "Keep the power behind ⌘K and selection, not on screen. Inspector and History \
             earn their place; layers don't.",
        ))
        .child(track(
            "council-gemma",
            "Gemma",
            "4 26B · Mac Studio",
            "Industrial means cramped.",
            "Add whitespace, and make the app readable by agents. If the UI is just pixels, \
             the agent is blind.",
        ));
    frame(
        "council",
        vec![
            stage(
                "council",
                &s.council_stage,
                theme::PAGE_PAD_R,
                theme::WIDE_MAX,
                theme::SP_24,
                vec![
                    div()
                        .flex()
                        .flex_col()
                        .gap(theme::z(theme::SP_4))
                        .child(text::caption("You asked three models · Round 1 · 1m 48s"))
                        .child(text::title("Should Awili switch its UI toolkit to egui?").wrap())
                        .into_any_element(),
                    agree.into_any_element(),
                    tracks.into_any_element(),
                ],
            ),
            rail("council", 1, false),
            doc_pill(
                "council",
                "Should Awili switch toolkits?",
                Some("Council"),
                None,
            ),
            dock(
                "council",
                theme::PAGE_PAD_R,
                vec![
                    Button::new("council-answer", "Let them answer each other").lead(),
                    Button::new("council-add", "Add a model").quiet(),
                    Button::new("council-copy", "Copy all").quiet(),
                ],
                composer(
                    "council",
                    "Ask all of them a follow-up",
                    false,
                    vec![Chip::new("council-models", "3 models ▾")],
                    Vec::new(),
                ),
            ),
        ],
    )
}

/// A page (`.page`): padded `STAGE_TOP` / `PAGE_PAD_R` / `PAGE_PAD_B` /
/// `STAGE_LEFT`, scrolling, its content centred at most `PAGE_MAX` wide.
fn page(id: &'static str, area: &ScrollArea, content: AnyElement) -> AnyElement {
    let scroller = div()
        .id(SharedString::from(format!("{id}-page")))
        .absolute()
        .top(Pixels::ZERO)
        .left(Pixels::ZERO)
        .size_full()
        .overflow_y_scroll()
        .pt(theme::z(theme::STAGE_TOP))
        .pr(theme::z(theme::PAGE_PAD_R))
        .pb(theme::z(theme::PAGE_PAD_B))
        .pl(theme::z(theme::STAGE_LEFT))
        .child(
            div()
                .w_full()
                .max_w(theme::z(theme::PAGE_MAX))
                .mx_auto()
                .child(content),
        );
    scrolled(id, area, scroller)
}

/// Design (`A-design.html`): the section exists; what backs it comes later.
fn design(s: &ScreenScroll) -> AnyElement {
    frame(
        "design",
        vec![
            page(
                "design",
                &s.design_page,
                EmptyState::new(
                    "design-empty",
                    "Design is coming in a later release.",
                    "This is where you'll make images, layouts and mockups with Awili.",
                )
                .glyph(Icon::Palette)
                // The mockup's `margin-top: 12vh` of its own window (here the
                // frame), below the page's top padding.
                .top(theme::z(theme::DESIGN_EMPTY_TOP))
                .into_any_element(),
            ),
            rail("design", 2, false),
            doc_pill("design", "Design", None, None),
        ],
    )
}

/// A Backstage card (`.card` with its `.sechead`): the heading, its quiet
/// action, and its rows.
fn backstage_card(
    id: &'static str,
    heading: &str,
    action: &str,
    rows: Vec<AnyElement>,
) -> AnyElement {
    // One column, so the card's own gap never lands between the rows: the
    // heading sits `SP_4` above them (`.sechead { margin-bottom: 4px }`)
    // and the rows touch, as in the mockup.
    Card::new(id)
        .children([div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pb(theme::z(theme::SP_4))
                    .child(text::heading(heading.to_string()))
                    .child(
                        Button::new(
                            SharedString::from(format!("{id}-action")),
                            action.to_string(),
                        )
                        .quiet(),
                    ),
            )
            .children(rows)
            .into_any_element()])
        .into_any_element()
}

/// A Backstage row: an optional leading status dot, the line and its
/// detail, and the trail (text, a dot, or a dot and text).
fn bs_row(
    id: &'static str,
    lead: Option<Tone>,
    title: &str,
    sub: &str,
    trail_dot: Option<Tone>,
    trail: Option<&str>,
) -> AnyElement {
    let mut row = Row::new(id)
        .title(title.to_string())
        .subtitle(sub.to_string());
    if let Some(tone) = lead {
        row = row.leading(match tone {
            Tone::Neutral => Dot::new(tone),
            Tone::ActNow => Dot::new(tone).label("Waiting for you"),
            _ => Dot::new(tone).label("Worth a look"),
        });
    }
    if trail_dot.is_some() || trail.is_some() {
        let mut t = div().flex().items_center().gap(theme::z(theme::SP_8));
        if let Some(tone) = trail_dot {
            t = t.child(Dot::new(tone).label("Worth a look"));
        }
        if let Some(trail) = trail {
            t = t.child(text::label(trail.to_string()).dim());
        }
        row = row.trailing(t);
    }
    row.into_any_element()
}

/// Backstage (`A-backstage.html`).
fn backstage(s: &ScreenScroll) -> AnyElement {
    let n = Some(Tone::Neutral);
    let cards = grid()
        .child(backstage_card(
            "bs-automations",
            "Automations",
            "See all",
            vec![
                bs_row(
                    "bs-a1",
                    n,
                    "Morning brief",
                    "Every weekday at 7:00",
                    Some(Tone::Attention),
                    Some("Skipped"),
                ),
                bs_row(
                    "bs-a2",
                    n,
                    "Nightly backups check",
                    "Every day at 2:00",
                    None,
                    Some("Ran 4 h ago"),
                ),
                bs_row(
                    "bs-a3",
                    n,
                    "Inbox tidy",
                    "Every hour",
                    None,
                    Some("Ran 12 min ago"),
                ),
            ],
        ))
        .child(backstage_card(
            "bs-sessions",
            "Sessions",
            "See all",
            vec![
                bs_row(
                    "bs-s1",
                    Some(Tone::ActNow),
                    "Fix the zcode wrapper",
                    "Waiting for you · 2 min",
                    None,
                    Some("Review"),
                ),
                bs_row(
                    "bs-s2",
                    n,
                    "Kit batch K1",
                    "Qwen on Spark · running 40 min",
                    None,
                    Some("Open"),
                ),
                bs_row(
                    "bs-s3",
                    n,
                    "PhotoCraft UI study",
                    "Finished yesterday",
                    None,
                    Some("Open"),
                ),
            ],
        ))
        .child(backstage_card(
            "bs-skills",
            "Skills",
            "Add a skill",
            vec![
                bs_row(
                    "bs-k1",
                    None,
                    "Write in my voice",
                    "Used 12 times this week",
                    None,
                    None,
                ),
                bs_row(
                    "bs-k2",
                    None,
                    "Check the fleet",
                    "Used 4 times this week",
                    None,
                    None,
                ),
                bs_row(
                    "bs-k3",
                    None,
                    "Grant writing",
                    "New",
                    Some(Tone::Attention),
                    None,
                ),
            ],
        ))
        .child(backstage_card(
            "bs-connections",
            "Connections",
            "Manage",
            vec![
                bs_row("bs-c1", n, "Messages", "Connected", None, None),
                bs_row("bs-c2", n, "Email", "Connected", None, None),
                bs_row("bs-c3", n, "Slack", "Not set up", None, Some("Set up")),
            ],
        ))
        .child(backstage_card(
            "bs-health",
            "Health",
            "Details",
            vec![
                bs_row(
                    "bs-h1",
                    n,
                    "Mac Studio",
                    "Gemma ready · 31 GB free",
                    None,
                    None,
                ),
                bs_row("bs-h2", n, "Spark", "Qwen ready · busy", None, None),
                bs_row("bs-h3", n, "Memory", "Connected", None, None),
            ],
        ));
    let content = div()
        .flex()
        .flex_col()
        .gap(theme::z(theme::SP_24))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(theme::z(theme::SP_4))
                .child(text::title("All running. One thing worth a look."))
                .child(
                    text::body(
                        "Skills, automations, sessions, connections and health — in one place.",
                    )
                    .reading(),
                ),
        )
        .child(
            Card::new("bs-look")
                .tone(Tone::Attention)
                .label("Worth a look")
                .title("The morning brief automation skipped today")
                .body("It runs at 7:00 and needs the Mac awake. It will try again tomorrow.")
                .actions([
                    Button::new("bs-run", "Run it now"),
                    Button::new("bs-open", "Open the automation").quiet(),
                ]),
        )
        .child(cards);
    frame(
        "backstage",
        vec![
            page("backstage", &s.bs_page, content.into_any_element()),
            rail("backstage", 3, false),
            doc_pill(
                "bs",
                "Backstage",
                Some("everything running behind the conversation"),
                None,
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Bounds, TestAppContext, VisualTestContext};

    use crate::ui::interaction::test_util::render_part_at;

    /// The bounds of `selector`, relative to the screen frame's origin.
    fn at(wcx: &mut VisualTestContext, frame: &'static str, selector: &'static str) -> Bounds<f32> {
        let f = wcx
            .debug_bounds(frame)
            .unwrap_or_else(|| panic!("{frame} laid out"));
        let b = wcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} laid out"));
        Bounds {
            origin: gpui::point(
                f32::from(b.origin.x - f.origin.x),
                f32::from(b.origin.y - f.origin.y),
            ),
            size: gpui::size(f32::from(b.size.width), f32::from(b.size.height)),
        }
    }

    /// `got` is `want` (within half a pixel, for centring).
    fn near(what: &str, got: f32, want: f32) {
        assert!(
            (got - want).abs() <= 0.5,
            "{what}: {got}, the mockup has {want}"
        );
    }

    /// One screen alone in a window its own size (wide, so panels stay
    /// panels).
    fn screen(cx: &mut TestAppContext, index: usize) -> &mut VisualTestContext {
        let size = gpui::size(
            gpui::px(theme::SCREEN_W * 1.25),
            gpui::px(theme::SCREEN_H * 1.25),
        );
        let (_handle, wcx) = render_part_at(cx, size, move || {
            div().child(screens(&ScreenScroll::default()).swap_remove(index).2)
        });
        wcx
    }

    /// studio.css geometry, Workshop: the rail at `EDGE`, vertically
    /// centred; the title pill centred at the top; the island at the top
    /// right; the panel from `PANEL_TOP` to `SP_24` above the bottom,
    /// `EDGE` from the right; the composer `SP_24` above the bottom,
    /// centred between `STAGE_LEFT` and `STAGE_RIGHT`.
    #[gpui::test]
    fn workshop_screen_follows_the_mockup_geometry(cx: &mut TestAppContext) {
        let wcx = screen(cx, 0);
        let f = "ui-gallery-screen-workshop";
        let (w, h) = (
            f32::from(theme::z(theme::SCREEN_W)),
            f32::from(theme::z(theme::SCREEN_H)),
        );
        let z = |v: f32| f32::from(theme::z(v));

        let rail = at(wcx, f, "ui-nav-rail-ws-rail");
        near("rail left", rail.origin.x, z(theme::EDGE));
        near("rail centre y", rail.center().y, h / 2.0);

        let doc = at(wcx, f, "ui-pill-ws-doc");
        near("title pill centre x", doc.center().x, w / 2.0);
        near("title pill top", doc.origin.y, z(theme::EDGE));

        let island = at(wcx, f, "ui-pill-ws-island");
        near(
            "island right",
            island.origin.x + island.size.width,
            w - z(theme::EDGE),
        );
        near("island top", island.origin.y, z(theme::EDGE));

        let panel = at(wcx, f, "ui-panel-ws-inspector");
        near("panel top", panel.origin.y, z(theme::PANEL_TOP));
        near(
            "panel bottom",
            panel.origin.y + panel.size.height,
            h - z(theme::SP_24),
        );
        near(
            "panel right",
            panel.origin.x + panel.size.width,
            w - z(theme::EDGE),
        );

        let composer = at(wcx, f, "ui-composer-frame-ws-composer");
        near(
            "composer bottom",
            composer.origin.y + composer.size.height,
            h - z(theme::SP_24),
        );
        let dock_mid = (z(theme::STAGE_LEFT) + w - z(theme::STAGE_RIGHT)) / 2.0;
        near("composer centre x", composer.center().x, dock_mid);
    }

    /// Council's own insets (A-council.html): no panel, so the dock runs
    /// to `PAGE_PAD_R` from the right and the composer centres there.
    #[gpui::test]
    fn council_screen_uses_its_own_insets(cx: &mut TestAppContext) {
        let wcx = screen(cx, 2);
        let f = "ui-gallery-screen-council";
        let w = f32::from(theme::z(theme::SCREEN_W));
        let z = |v: f32| f32::from(theme::z(v));
        let composer = at(wcx, f, "ui-composer-frame-council-composer");
        let dock_mid = (z(theme::STAGE_LEFT) + w - z(theme::PAGE_PAD_R)) / 2.0;
        near("council composer centre x", composer.center().x, dock_mid);
    }

    /// Every screen: the rail at `EDGE`, vertically centred, and the title
    /// pill centred at the top.
    #[gpui::test]
    fn every_screen_places_the_rail_and_the_title_pill(cx: &mut TestAppContext) {
        let z = |v: f32| f32::from(theme::z(v));
        let (w, h) = (z(theme::SCREEN_W), z(theme::SCREEN_H));
        for (i, frame, rail, doc) in [
            (
                0,
                "ui-gallery-screen-workshop",
                "ui-nav-rail-ws-rail",
                "ui-pill-ws-doc",
            ),
            (
                1,
                "ui-gallery-screen-job",
                "ui-nav-rail-job-rail",
                "ui-pill-job-doc",
            ),
            (
                2,
                "ui-gallery-screen-council",
                "ui-nav-rail-council-rail",
                "ui-pill-council-doc",
            ),
            (
                3,
                "ui-gallery-screen-design",
                "ui-nav-rail-design-rail",
                "ui-pill-design-doc",
            ),
            (
                4,
                "ui-gallery-screen-backstage",
                "ui-nav-rail-backstage-rail",
                "ui-pill-bs-doc",
            ),
        ] {
            let wcx = screen(cx, i);
            let r = at(wcx, frame, rail);
            near(&format!("{frame} rail left"), r.origin.x, z(theme::EDGE));
            near(&format!("{frame} rail centre y"), r.center().y, h / 2.0);
            let d = at(wcx, frame, doc);
            near(&format!("{frame} title centre x"), d.center().x, w / 2.0);
            near(&format!("{frame} title top"), d.origin.y, z(theme::EDGE));
        }
    }

    /// Workshop's inspector matches `A-studio.html`: "About this answer" and
    /// "Sources" open, "Details for nerds" closed (only its header shows).
    #[gpui::test]
    fn workshop_nerd_details_start_closed(cx: &mut TestAppContext) {
        let wcx = screen(cx, 0);
        let f = "ui-gallery-screen-workshop";
        let about = at(wcx, f, "ui-section-ws-about");
        let nerd = at(wcx, f, "ui-section-ws-nerd");
        assert!(
            nerd.size.height < about.size.height,
            "the nerd section is closed: {nerd:?} vs the open {about:?}"
        );
    }

    /// Job (`A-work.html`): the panel from `PANEL_TOP` to `SP_24` above the
    /// bottom, `EDGE` from the right, with its footer's Stop inside it.
    #[gpui::test]
    fn job_screen_panel_and_footer(cx: &mut TestAppContext) {
        let wcx = screen(cx, 1);
        let f = "ui-gallery-screen-job";
        let z = |v: f32| f32::from(theme::z(v));
        let (w, h) = (z(theme::SCREEN_W), z(theme::SCREEN_H));
        let panel = at(wcx, f, "ui-panel-job-panel");
        near("job panel top", panel.origin.y, z(theme::PANEL_TOP));
        near(
            "job panel bottom",
            panel.origin.y + panel.size.height,
            h - z(theme::SP_24),
        );
        near(
            "job panel right",
            panel.origin.x + panel.size.width,
            w - z(theme::EDGE),
        );
        let stop = at(wcx, f, "ui-button-job-panel-stop");
        assert!(
            stop.origin.y + stop.size.height <= panel.origin.y + panel.size.height
                && stop.origin.x >= panel.origin.x,
            "the footer's Stop is inside the panel: {stop:?} in {panel:?}"
        );
    }

    /// Council: the three tracks are equal grid columns of one row, equal
    /// height (grid rows stretch their cells).
    #[gpui::test]
    fn council_tracks_are_equal_grid_cells(cx: &mut TestAppContext) {
        let wcx = screen(cx, 2);
        let f = "ui-gallery-screen-council";
        let cards = [
            "ui-card-council-codex",
            "ui-card-council-deepseek",
            "ui-card-council-gemma",
        ]
        .map(|s| at(wcx, f, s));
        for c in &cards[1..] {
            near("track width", c.size.width, cards[0].size.width);
            near("track height", c.size.height, cards[0].size.height);
            near("track row", c.origin.y, cards[0].origin.y);
        }
    }

    /// Design: the empty state's glyph starts `EMPTY_TOP` of the frame below
    /// the page's top padding (the mockup's `margin: 12vh auto 0` inside
    /// `.page { padding-top: 96px }`).
    #[gpui::test]
    fn design_empty_state_sits_12vh_below_the_page_padding(cx: &mut TestAppContext) {
        let wcx = screen(cx, 3);
        let f = "ui-gallery-screen-design";
        let z = |v: f32| f32::from(theme::z(v));
        let glyph = at(wcx, f, "ui-empty-state-design-empty-glyph");
        // The mockup's own arithmetic, not the token: 96 + 12 % of 760.
        let twelve_vh = z(theme::SCREEN_H) * theme::EMPTY_TOP;
        near("glyph top", glyph.origin.y, z(theme::STAGE_TOP) + twelve_vh);
    }

    /// Backstage: five cards in three equal columns; the short second row
    /// keeps its cells one column wide, aligned under the first row's.
    #[gpui::test]
    fn backstage_cards_are_a_three_column_grid(cx: &mut TestAppContext) {
        let wcx = screen(cx, 4);
        let f = "ui-gallery-screen-backstage";
        let cards = [
            "ui-card-bs-automations",
            "ui-card-bs-sessions",
            "ui-card-bs-skills",
            "ui-card-bs-connections",
            "ui-card-bs-health",
        ]
        .map(|s| at(wcx, f, s));
        for c in &cards[1..] {
            near("card width", c.size.width, cards[0].size.width);
        }
        near("row 2, column 1", cards[3].origin.x, cards[0].origin.x);
        near("row 2, column 2", cards[4].origin.x, cards[1].origin.x);
        assert!(
            cards[3].origin.y > cards[0].origin.y,
            "Connections starts row 2"
        );
    }
}
