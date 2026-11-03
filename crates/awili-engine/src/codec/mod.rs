//! B3 dialect codecs: raw provider streams → [`crate::wire::WireDelta`].
//!
//! Codecs are pure (bytes/str in, deltas out — transport stays in
//! awili-providers) and fixture-grounded: every behavior here is asserted
//! against the live captures in `fixtures/` (see
//! docs/engine-wire-contract.md). Unknown provider fields are ignored by
//! construction; malformed frames fail typed, never panic (P6).

pub mod chat_completions;
