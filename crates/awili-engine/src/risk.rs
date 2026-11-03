//! Canonical tool-risk vocabulary (C1 contract v2 §3).
//!
//! Lives in the PURE engine crate so policy (`awili-engine-authz`), the
//! durable log (`awili-engine-log`), and the tool registry (`awili-tools`)
//! can all speak it WITHOUT any of them depending on an IO-bearing crate
//! just to name a risk class (Codex C2-u1 P1).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskClass {
    ReadOnly,
    Mutating,
    Irreversible,
    Privileged,
}

impl RiskClass {
    /// Frozen wire strings — B2 persists these and replay keys on them.
    pub fn as_str(self) -> &'static str {
        match self {
            RiskClass::ReadOnly => "read_only",
            RiskClass::Mutating => "mutating",
            RiskClass::Irreversible => "irreversible",
            RiskClass::Privileged => "privileged",
        }
    }

    /// Parse a declared class. Legacy vocabularies map conservatively and
    /// anything unrecognized is `Privileged` — classification may only
    /// escalate, never silently under-classify.
    pub fn parse_conservative(s: &str) -> Self {
        match s {
            "read_only" | "read" | "pure" | "none" => RiskClass::ReadOnly,
            "mutating" | "write" => RiskClass::Mutating,
            "irreversible" | "dangerous" => RiskClass::Irreversible,
            _ => RiskClass::Privileged,
        }
    }

    fn rank(self) -> u8 {
        match self {
            RiskClass::ReadOnly => 0,
            RiskClass::Mutating => 1,
            RiskClass::Irreversible => 2,
            RiskClass::Privileged => 3,
        }
    }

    /// Escalation-only combination.
    pub fn escalate(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}
