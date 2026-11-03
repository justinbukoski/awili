//! `awili-selftest`: turns "can Awili do X right now?" into a versioned,
//! data-driven, machine-checked answer per executor + model.
//!
//! P0a (this library, pure — no I/O beyond the executor implementations):
//! - [`manifest`]: the probe data model loaded from TOML.
//! - [`evaluate`]: pure expectation evaluation and result reduction.
//! - [`report`]: probe results and run reports (JSON on disk, append-only).
//! - [`matrix`]: the cross-executor PASS/FAIL matrix rendered as markdown.
//! - [`redact`]: secret redaction applied to evidence before it is written.
//! - [`records`]: record checks — tool records the harness reads itself.
//!
//! P0b (runner): [`executor`] defines the single `ProbeExecutor` trait plus
//! the two P0b executors ([`executor::ScriptedExecutor`] for tests and
//! [`executor::CommandExecutor`] to drive `claude -p`, `codex exec`,
//! `zcode --prompt` from the CLI). The `awili-selftest` binary (in
//! `main.rs`) runs probes and renders the matrix.
//!
//! Source spec: `docs/daily-driver/specs/P0-selftest.md`.

#![forbid(unsafe_code)]

pub mod evaluate;
pub mod executor;
pub mod graduation;
pub mod graduation_math;
pub mod graduation_render;
pub mod manifest;
pub mod matrix;
pub mod records;
pub mod redact;
pub mod report;

pub mod app_observation;
