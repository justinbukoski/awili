//! `awili-core` — pure, UI-agnostic state for the Awili composer slice.
//!
//! Two pieces, both free of any GPUI / windowing dependency so they unit-test in
//! isolation:
//!
//! - [`EditBuffer`] — a caret-aware, char-indexed, UTF-8-safe text buffer.
//!   ADAPTED FROM Hub `crates/hub-app/src/ui/peers.rs::edit_buffer_at` (the
//!   char-index / `byte_of_char` / `line_start` / `vertical_move` logic) and the
//!   `caret_line_col` / `split_at_col` render helpers from `mailui.rs`. Copied &
//!   adapted into a struct — Awili does NOT depend on the `hub` crate (a shared
//!   text-input crate is a later refactor, per slice-1 spec §3).
//! - [`ConversationState`] — the ordered message log the composer appends to.
//! - [`ConversationList`] (slice 7) — the sidebar's metadata layer: per-
//!   conversation id/title/timestamps with SOFT archive (no delete exists).

//! - [`pricing`] (slice 8) — USD token rates + cost arithmetic for the ticker.

pub mod context;
pub mod conversation;
pub mod convs;
pub mod edit;
pub mod limits;
pub mod pricing;
pub mod provenance;

pub use conversation::{ConversationState, Message, MessageKind, Role};
pub use convs::{
    auto_title, new_conversation_id, ConvKind, ConversationList, ConversationMeta, DEFAULT_TITLE,
};
pub use edit::{EditAction, EditBuffer, Motion, UndoKind};
pub use provenance::{CcdRef, CostRate, Locality, Provenance, Usage};
