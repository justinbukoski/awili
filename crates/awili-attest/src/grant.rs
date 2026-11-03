//! The install authority view (design r9 §2.6, rulings E and G).
//!
//! The control-plane half of the unit-5 grant boundary: the loader's
//! CURRENT installed record and the CURRENT complete `McpServerConfig`,
//! bound to one origin identity at construction. The other half — the
//! observation and the coordinator's current liveness — is composed in
//! `awili-app::mcp_catalog::install_grant_input`, which queries the facade
//! INSIDE the boundary (there is no liveness value a caller could retain
//! or replay).

use awili_attest_types::SetupRecordFacts;
use awili_config::{McpServerConfig, McpTrust};
use awili_engine::identity::ToolOrigin;

use crate::setup_record::InstalledSetupRecord;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityRefusal {
    /// `trust == Install` on a config whose name is not the reserved
    /// server — a programmatically constructed Install value refuses
    /// LOUDLY here (row 39), never degrades to Prompt
    /// (`docs/reviews/codex-fix-review-2026-08-13.md`, unit-5 obligations).
    #[error(
        "malformed install trust: config {name:?} claims Install but is not the reserved server"
    )]
    MalformedInstallTrust { name: String },
    /// The installed record's origin is not the config's origin: the two
    /// control-plane sources disagree about which server this is.
    #[error("setup record is for {record:?} but the config names {config:?}")]
    RecordOriginMismatch {
        record: ToolOrigin,
        config: ToolOrigin,
    },
}

/// Constructed by the unit-5 seed site AT GRANT TIME — never cached across
/// a settings reload. Fields private; the ONE constructor refuses
/// malformed authority.
#[derive(Debug)]
pub struct InstallAuthorityView<'a> {
    record: &'a InstalledSetupRecord,
    config: &'a McpServerConfig,
    origin: ToolOrigin, // computed from the config, never caller-supplied
}

impl<'a> InstallAuthorityView<'a> {
    /// The ONE constructor (design r9 §2.6). Computes the origin from the
    /// config (`McpServerConfig::origin()`,
    /// `crates/awili-config/src/lib.rs:341` — `"mcp:" + name`), refuses
    /// `trust == Install && !is_install_trusted()` (the name-and-trust
    /// conjunction of `lib.rs:354-355`, preserved intact), and refuses a
    /// record whose origin differs from the config's.
    pub fn current(
        record: &'a InstalledSetupRecord,
        config: &'a McpServerConfig,
    ) -> Result<InstallAuthorityView<'a>, AuthorityRefusal> {
        if config.trust == McpTrust::Install && !config.is_install_trusted() {
            return Err(AuthorityRefusal::MalformedInstallTrust {
                name: config.name.clone(),
            });
        }
        let origin = ToolOrigin::new(config.origin());
        if record.facts().origin != origin {
            return Err(AuthorityRefusal::RecordOriginMismatch {
                record: record.facts().origin.clone(),
                config: origin,
            });
        }
        Ok(InstallAuthorityView {
            record,
            config,
            origin,
        })
    }

    pub fn record(&self) -> &SetupRecordFacts {
        self.record.facts()
    }

    /// The identity the view is FOR — grant clause 4 compares it against
    /// the observation's origin.
    pub fn origin(&self) -> &ToolOrigin {
        &self.origin
    }

    /// True iff `config.trust == Install` AND `config.is_install_trusted()`
    /// — the `lib.rs:354-355` conjunction. `Prompt` is authorized for
    /// nothing permanent.
    pub fn install_authorized(&self) -> bool {
        self.config.trust == McpTrust::Install && self.config.is_install_trusted()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup_record::load_setup_records;
    use std::io::Write;

    fn mac_shell_record() -> (tempfile::TempDir, crate::setup_record::SetupRecords) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("attested-servers.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(
            br#"
[[server]]
origin        = "mcp:mac-shell"
program       = "/usr/local/lib/awili/mac-shell"
launch_kind   = "direct"
requirement   = 'cdhash H"4dc1e6f6ab3f835b521f1e9da8fa5ef2a484f7d9"'
protocol_name = "mac-shell"
trust         = "install"
"#,
        )
        .unwrap();
        let records = load_setup_records(&path);
        assert!(records.problems().is_empty());
        (dir, records)
    }

    fn config(name: &str, trust: McpTrust) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: "/usr/local/lib/awili/mac-shell".to_string(),
            args: Vec::new(),
            env: Default::default(),
            trust,
            modes: Vec::new(),
            enabled: true,
        }
    }

    #[test]
    fn a_programmatically_malformed_install_config_refuses_at_construction() {
        let (_d, records) = mac_shell_record();
        let rec = records
            .record(&ToolOrigin::new("mcp:mac-shell"))
            .expect("record");
        let cfg = config("other", McpTrust::Install);
        match InstallAuthorityView::current(rec, &cfg) {
            Err(AuthorityRefusal::MalformedInstallTrust { name }) => assert_eq!(name, "other"),
            other => panic!("expected MalformedInstallTrust, got {other:?}"),
        }
    }

    #[test]
    fn a_record_for_another_origin_refuses_at_construction() {
        let (_d, records) = mac_shell_record();
        let rec = records
            .record(&ToolOrigin::new("mcp:mac-shell"))
            .expect("record");
        let cfg = config("elsewhere", McpTrust::Prompt);
        match InstallAuthorityView::current(rec, &cfg) {
            Err(AuthorityRefusal::RecordOriginMismatch { record, config }) => {
                assert_eq!(record.origin, "mcp:mac-shell");
                assert_eq!(config.origin, "mcp:elsewhere");
            }
            other => panic!("expected RecordOriginMismatch, got {other:?}"),
        }
    }

    #[test]
    fn the_reserved_install_config_authorizes_and_prompt_does_not() {
        let (_d, records) = mac_shell_record();
        let rec = records
            .record(&ToolOrigin::new("mcp:mac-shell"))
            .expect("record");

        let install = config("mac-shell", McpTrust::Install);
        let view = InstallAuthorityView::current(rec, &install).expect("view");
        assert!(view.install_authorized());
        assert_eq!(view.origin().origin, "mcp:mac-shell");

        let prompt = config("mac-shell", McpTrust::Prompt);
        let view = InstallAuthorityView::current(rec, &prompt).expect("view");
        assert!(!view.install_authorized());
    }
}
