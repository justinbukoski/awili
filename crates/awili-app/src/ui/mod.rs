//! The Awili kit (docs/v2/KIT.md v3): the shared parts every screen is
//! built from. One file per part, re-exported here.
//!
//! Conventions (KIT.md § Conventions):
//!
//! - Parts read colours ONLY through `theme::palette()` and sizes ONLY
//!   through `theme::` tokens, scaled through `z()` or `target()` — a kit
//!   file adds zero design-lint findings. Because parts only reference
//!   palette tokens, light mode (slice 11) is a palette swap with no kit
//!   change.
//! - Interactive parts (K2+) take an `ElementId` in `new(id, …)`, set a
//!   default `debug_selector`, drop the second click of a double click, and
//!   carry `disabled(reason)`, `label`, `focused`, and `tab_index` — see the
//!   kit wrappers that ship in those batches.
//!
//! Batch K1 (foundations) ships: [`text`] and its shortcuts, the focus ring
//! ([`focus_ring`], [`focus_ring_style`]), [`Divider`], [`Dot`],
//! [`ThinkingDot`], and [`motion::reduce_motion`].
//!
//! Batch K2 (controls and structure) ships the shared interactive
//! conventions ([`interaction`]) and the interactive/structural parts:
//! [`Button`], [`IconButton`], [`Chip`], [`Banner`], [`Row`],
//! [`Section`], [`Card`], [`Tabs`] ([`TabItem`]) and [`ScrollBar`]
//! ([`ScrollBarState`]).
//!
//! Batch K3 (floating surfaces) ships the anchored and modal
//! surfaces: [`Panel`], [`Pill`], [`Popover`] ([`PopoverPlacement`],
//! [`PopoverState`]), [`Menu`] ([`MenuItem`]), [`Palette`]
//! ([`PaletteRow`], [`highlight_parts`]), [`Dialog`], [`Sheet`]
//! (`is_compact`, `COMPACT_W`) and [`Notice`] / [`NoticeStack`]
//! ([`NoticeStackState`], [`NoticeSpec`], [`NoticeScope`],
//! [`NoticeLifetime`]).
//!
//! Batch K4 (app frames and settings) ships the app frames
//! ([`ComposerFrame`], [`NavRail`] ([`RailItem`]), [`CodeBlock`],
//! [`TerminalFrame`]), the chart legend ([`ChartLegend`] /
//! [`ChartLegendItem`]), the settings controls ([`SettingRow`],
//! [`Toggle`], [`Choice`], [`Stepper`], [`Level`], [`TextField`],
//! [`SearchField`]), and the empty states ([`EmptyState`],
//! [`Skeleton`]).
//!
//! The parts are the kit's public API surface for the K2+ batches (and the
//! screens that consume them), so they are not yet referenced by screen
//! code in this batch. `dead_code` is allowed here on purpose; a later batch
//! that wires them into screens will drop the allow once they are used.

#![allow(dead_code, unused_imports)]

pub mod banner;
pub mod button;
pub mod card;
pub mod chart_legend;
pub mod chip;
pub mod choice;
pub mod code_block;
pub mod composer_frame;
pub mod dialog;
pub mod divider;
pub mod dot;
pub mod empty_state;
pub mod focus_ring;
pub mod gallery;
pub mod gallery_screens;
pub mod icon_button;
pub mod interaction;
#[cfg(test)]
mod kit_contract_tests;
pub mod level;
pub mod menu;
pub mod motion;
pub mod nav_rail;
pub mod notice;
pub mod palette;
pub mod panel;
pub mod pill;
pub mod popover;
pub mod row;
pub mod scrollbar;
pub mod search_field;
pub mod section;
pub mod setting_row;
pub mod sheet;
pub mod skeleton;
pub mod stepper;
pub mod tabs;
pub mod terminal_frame;
pub mod text;
pub mod text_field;
pub mod thinking_dot;
pub mod toggle;

pub use banner::Banner;
pub use button::{Button, Variant};
pub use card::Card;
pub use chart_legend::{ChartLegend, ChartLegendItem};
pub use chip::Chip;
pub use choice::Choice;
pub use code_block::CodeBlock;
pub use composer_frame::ComposerFrame;
pub use dialog::Dialog;
pub use divider::Divider;
pub use dot::Dot;
pub use empty_state::EmptyState;
pub use focus_ring::{focus_ring, focus_ring_r, focus_ring_style, focus_ring_style_on};
pub use icon_button::IconButton;
pub use interaction::Common;
pub use level::Level;
pub use menu::{Menu, MenuItem};
pub use nav_rail::{NavRail, RailItem};
pub use notice::{
    Notice, NoticeEntry, NoticeLifetime, NoticeScope, NoticeSpec, NoticeStack, NoticeStackState,
};
pub use palette::{highlight_parts, Palette, PaletteRow};
pub use panel::Panel;
pub use pill::Pill;
pub use popover::{place, Popover, PopoverPlacement, PopoverState};
pub use row::Row;
pub use scrollbar::{offset_for_pointer, thumb_metrics, ScrollBar, ScrollBarState};
pub use search_field::SearchField;
pub use section::Section;
pub use setting_row::SettingRow;
pub use sheet::{is_compact, Sheet};
pub use skeleton::Skeleton;
pub use stepper::Stepper;
pub use tabs::{TabItem, Tabs};
pub use terminal_frame::TerminalFrame;
pub use text::{body, caption, heading, label, read, text, title, ui_text, Text};
pub use text_field::TextField;
pub use thinking_dot::ThinkingDot;
pub use toggle::Toggle;
