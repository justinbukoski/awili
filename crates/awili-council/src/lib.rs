//! Awili Council: one message goes to several models at once; their answers
//! come back side by side and, when the owner asks, they answer each other.
//!
//! This crate is the pure core (no GPUI, no I/O): who sits on a council
//! ([`Member`]), its rounds and each member's turn ([`Round`], [`SeatTurn`]),
//! `@Name` mentions ([`resolve_mentions`], ported from the July
//! `lumi-council` rules), the stance a reply opens with ([`parse_stance`])
//! the one-line round summary ([`round_summary`]) and, RC2-CNCL4, what each
//! member is sent ([`plan_round`], [`trim_to_budget`]).
#![forbid(unsafe_code)]

mod mention;
mod prompt;
mod types;

pub use mention::{resolve_mentions, Mentions};
pub use prompt::{
    plan_round, respond_ask, sum_up_ask, trim_to_budget, trim_to_chars, Line, PlanOptions,
    PlanOutcome, RoundPlan, SeatPrompt, OLDER_LEFT_OUT,
};
pub use types::{
    parse_stance, round_summary, stance_and_body, Council, Member, Round, RoundKind, SeatTurn,
    Stance, TurnStatus,
};
