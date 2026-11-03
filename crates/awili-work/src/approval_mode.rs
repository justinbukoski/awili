//! "Ask before changes" vs "Auto" (U4, spec D1/D2): the two Work/Code
//! postures. The mode belongs to a conversation (D2); the in-app engine
//! reads the shared cell at each approval gate (U4b2), so a switch applies
//! at the next gate.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// The two enforcement postures (spec D1: "Ask before changes" is the
/// default; "Auto" is the explicit bypass, still held by the authorizer's
/// non-overridable floors, D7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Stops for the user's OK before files change or commands run.
    #[default]
    AskBeforeChanges,
    /// Runs without asking.
    Auto,
}

/// The two cell values [`SharedApprovalMode`] writes; anything else read
/// back resolves to Ask (fail closed).
const CELL_ASK: u8 = 0;
const CELL_AUTO: u8 = 1;

impl ApprovalMode {
    /// Parse a `[work] default_approval_mode` value (spec D2): `"auto"`
    /// trimmed, ASCII case-insensitive; anything else — including `None` —
    /// means Ask. Never an error.
    pub fn from_config(raw: Option<&str>) -> Self {
        match raw
            .map(str::trim)
            .map(|s| s.eq_ignore_ascii_case("auto"))
            .unwrap_or(false)
        {
            true => Self::Auto,
            false => Self::AskBeforeChanges,
        }
    }

    /// The config-spelling of the mode (`"ask"` | `"auto"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AskBeforeChanges => "ask",
            Self::Auto => "auto",
        }
    }
}

/// A process-shared cell holding the mode a running in-app engine reads at
/// each approval gate (U4b2). Clones share the cell; every read/write is
/// `SeqCst`. `Default` is Ask.
#[derive(Clone, Debug, Default)]
pub struct SharedApprovalMode(Arc<AtomicU8>);

impl SharedApprovalMode {
    /// Start a new shared cell (clones share it) at `mode`.
    pub fn new(mode: ApprovalMode) -> Self {
        Self(Arc::new(AtomicU8::new(mode.cell())))
    }

    /// Current mode. An out-of-range cell value reads as Ask (fail closed).
    /// Takes `&self` so a worker holding one handle can read it at every
    /// approval gate without consuming it.
    pub fn get(&self) -> ApprovalMode {
        ApprovalMode::from_cell(self.0.load(Ordering::SeqCst))
    }

    /// Set the mode; every clone of this cell observes the switch at its
    /// next gate.
    pub fn set(&self, mode: ApprovalMode) {
        self.0.store(mode.cell(), Ordering::SeqCst);
    }
}

impl ApprovalMode {
    /// The wire value written into the cell.
    fn cell(self) -> u8 {
        match self {
            Self::AskBeforeChanges => CELL_ASK,
            Self::Auto => CELL_AUTO,
        }
    }

    /// Cell → mode: `0` = Ask, `1` = Auto, ANY other value = Ask.
    fn from_cell(cell: u8) -> Self {
        match cell {
            CELL_ASK => Self::AskBeforeChanges,
            CELL_AUTO => Self::Auto,
            _ => Self::AskBeforeChanges,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `from_config` parse table (spec D2): `"auto"` in any casing or
    /// with surrounding whitespace is Auto; everything else — `None`, empty
    /// string, other words, `"ask"` — is Ask.
    #[test]
    fn u4b1_mode_from_config_table() {
        let auto = [
            Some("auto"),
            Some("AUTO"),
            Some("Auto"),
            Some("aUtO"),
            Some(" auto "),
            Some("\tauto\n"),
        ];
        for raw in auto {
            assert_eq!(
                ApprovalMode::from_config(raw),
                ApprovalMode::Auto,
                "{raw:?} must parse as Auto"
            );
        }

        let ask = [
            None,
            Some("ask"),
            Some("ASK"),
            Some(""),
            Some("   "),
            Some("autos"),
            Some("aut"),
            Some("true"),
            Some("1"),
        ];
        for raw in ask {
            assert_eq!(
                ApprovalMode::from_config(raw),
                ApprovalMode::AskBeforeChanges,
                "{raw:?} must parse as Ask"
            );
        }

        assert_eq!(ApprovalMode::AskBeforeChanges.as_str(), "ask");
        assert_eq!(ApprovalMode::Auto.as_str(), "auto");
        assert_eq!(ApprovalMode::default(), ApprovalMode::AskBeforeChanges);
    }

    /// Clones share the cell; `Default` is Ask; and a cell value that no
    /// API can produce (out of range) still reads as Ask — fail closed.
    #[test]
    fn u4b1_shared_mode_is_shared_and_fails_to_ask() {
        let shared = SharedApprovalMode::default();
        assert_eq!(shared.clone().get(), ApprovalMode::AskBeforeChanges);

        let clone = shared.clone();
        shared.set(ApprovalMode::Auto);
        assert_eq!(
            clone.clone().get(),
            ApprovalMode::Auto,
            "the clone must observe the switch"
        );
        clone.set(ApprovalMode::AskBeforeChanges);
        assert_eq!(shared.clone().get(), ApprovalMode::AskBeforeChanges);

        let started = SharedApprovalMode::new(ApprovalMode::Auto);
        assert_eq!(
            started.clone().get(),
            ApprovalMode::Auto,
            "new(Auto) must read Auto"
        );

        // Out-of-range cell values (unreachable through the public API)
        // must read as Ask, never Auto.
        assert_eq!(
            ApprovalMode::from_cell(2),
            ApprovalMode::AskBeforeChanges,
            "an unknown cell value fails to Ask"
        );
    }

    /// `get(&self)` does not consume the handle: the U4b2 worker keeps ONE
    /// handle and reads it at EVERY approval gate, so repeated reads from
    /// the same binding — across clones flipping the cell — must all work.
    #[test]
    fn u4b1_fix1_shared_mode_reads_repeatedly_across_switches() {
        let m = SharedApprovalMode::new(ApprovalMode::AskBeforeChanges);
        let c = m.clone();

        // Two reads from the same binding: Ask, Ask.
        assert_eq!(m.get(), ApprovalMode::AskBeforeChanges);
        assert_eq!(m.get(), ApprovalMode::AskBeforeChanges);

        // The clone flips the cell to Auto; the SAME `m` binding still
        // reads — twice — and sees the switch.
        c.set(ApprovalMode::Auto);
        assert_eq!(m.get(), ApprovalMode::Auto);
        assert_eq!(m.get(), ApprovalMode::Auto);

        // And back: `m` reads a third time without being consumed.
        c.set(ApprovalMode::AskBeforeChanges);
        assert_eq!(m.get(), ApprovalMode::AskBeforeChanges);
    }
}
