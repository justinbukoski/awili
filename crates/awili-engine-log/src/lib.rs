//! awili-engine-log — B2: the durable, append-only execution log.
//!
//! The crash-safety mechanism of the owned engine (ENGINE-PIVOT-PLAN r3.1
//! §B2). Invariant: `Intent` records are durably appended BEFORE a tool
//! side-effect is dispatched; on restart, [`replay::classify`] never lets an
//! un-finalized dispatch be silently re-executed.
//!
//! Layering: `awili-engine` stays pure; this crate owns serialization
//! (versioned DTOs — runtime enums are NOT the storage schema), framing
//! (length-prefix + CRC32, torn-write tolerant), storage (append-only trait;
//! mutation is unrepresentable), and the replay classifier. C1 consumes the
//! record vocabulary defined in [`schema`] as a frozen interface.

pub mod authz;
#[cfg(test)]
mod authz_tests;
pub mod frame;
#[cfg(test)]
mod frame_tests;
pub mod grant_service;
#[cfg(test)]
mod grant_service_tests;
#[cfg(test)]
mod install_seed_tests;
#[cfg(test)]
mod killpoints_tests;
#[cfg(test)]
mod rc2_eng1_tests;
pub mod remembered;
#[cfg(test)]
mod remembered_tests;
pub mod replay;
pub mod schema;
pub mod storage;
pub mod writer;

#[cfg(feature = "test-authority")]
pub use authz::test_bundle;
pub use authz::{
    AuthzEvidence, AuthzGate, GrantRef, HostApprovalCapability, MintedProof, RegistryAuthority,
    ResolvedTool, RiskClass,
};
pub use replay::{classify, classify_with_grants, IntentStatus, TurnRecovery};
pub use schema::{LogRecordV1, SCHEMA_VERSION};
pub use storage::{AppendOnlyLog, MemLog};
pub use writer::{call_fingerprint, canonicalize_args, ExecutionLog, IntentOutcome, IntentTicket};

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("storage: {0}")]
    Storage(String),
    #[error("corrupt record at seq {seq}: {reason}")]
    Corrupt { seq: u64, reason: String },
    /// PROVEN tail corruption (S5(c) F0539, RVS5C-FIX1): the final frame is
    /// complete but fails its CRC, or the bytes after an inflated header
    /// CRC-check as a complete payload (only the length is wrong). The
    /// grant-store repair also returns it for an UNCERTAIN tail it refuses
    /// to truncate. Refused — never appended behind, never migrated past,
    /// never truncated.
    #[error("corrupt tail: cannot prove a final torn frame")]
    CorruptTail,
    /// RVS5C-FIX1: an earlier append on this handle failed after bytes may
    /// have reached storage (offset = the store length before that append).
    /// Every later append on the handle is refused; reopen the log (which
    /// re-validates the tail) to continue.
    #[error(
        "log handle poisoned: an append at byte {offset} failed after writing; reopen required"
    )]
    Poisoned { offset: u64 },
    #[error("schema version {found} unsupported (max {max})")]
    Version { found: u32, max: u32 },
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("authorization evidence bound to a different call: {0}")]
    EvidenceMismatch(String),
}
