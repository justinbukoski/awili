//! Unit 5 §§4-5 (design of record r9 + verdict rulings 21/27/34-36/42):
//! the app-side per-call install authority — the live kill switch.
//!
//! Two sources of truth, neither impersonating the other (ruling 27):
//! the EXACT CURRENT config-file bytes govern dynamic Install trust, and
//! the [`RunningServerIdentity`] captured when the MCP runtime was
//! composed governs what is actually running. Every query re-reads the
//! file (one read, one hash, one parse of the same buffer), requires
//! EXACTLY ONE reserved-server entry (never first-match), requires it
//! enabled and Work-member and launch-identical to the running entry
//! (trust excluded — it is the switch), and wraps the §4 attestation
//! checks in the three-way before/after sandwich (config bytes, setup
//! bytes, coordinator revision). Any miss anywhere: no witness — fail
//! closed, cards return (unless some other grant scope independently
//! authorizes, ruling 8).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use awili_attest::setup_record::{load_setup_records_from, SetupRecords};
use awili_attest::InstallAuthorityView;
use awili_engine_authz::{Decision, InstallAuthorityQuery, InstallWitnessCallback};
use awili_engine_log::authz::InstallSeedCandidate;
use awili_engine_log::IntentTicket;
use awili_engine_worker::mcp_coordinator::McpFacade;
use awili_tools::ToolOrigin;
use sha2::{Digest, Sha256};

/// The runtime-relevant identity of the ADMITTED reserved-server entry
/// (ruling 42): direct equality over canonical in-memory values. `trust`
/// is deliberately absent (the live switch); `enabled` and Work
/// membership are live eligibility predicates checked per query, not
/// identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningServerIdentity {
    pub origin: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

impl RunningServerIdentity {
    /// Captured at composition from the exact admitted config entry.
    pub fn of(entry: &awili_config::McpServerConfig) -> Self {
        Self {
            origin: entry.origin(),
            command: entry.command.clone(),
            args: entry.args.clone(),
            env: entry.env.clone(),
        }
    }
}

/// The Work surface's install authority: one per MCP composition.
pub struct WorkInstallAuthority {
    running: RunningServerIdentity,
    facade: McpFacade<IntentTicket>,
    /// The setup-records path resolved at composition (the FILE is re-read
    /// and re-hashed per query; only the path is captured).
    setup_path: Option<PathBuf>,
    /// The config search paths, captured at construction from
    /// `Config::config_paths()` (production) — the WINNING path is still
    /// re-selected per query, so a file appearing earlier in the
    /// precedence order takes over live. Tests inject a temp path.
    config_paths: Vec<PathBuf>,
}

impl WorkInstallAuthority {
    pub fn new(
        running: RunningServerIdentity,
        facade: McpFacade<IntentTicket>,
        setup_path: Option<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            running,
            facade,
            setup_path,
            config_paths: awili_config::Config::config_paths(),
        })
    }

    /// Test constructor: identical semantics over injected search paths.
    #[cfg(test)]
    pub(crate) fn with_paths_for_tests(
        running: RunningServerIdentity,
        facade: McpFacade<IntentTicket>,
        setup_path: Option<PathBuf>,
        config_paths: Vec<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            running,
            facade,
            setup_path,
            config_paths,
        })
    }

    /// One coherent capture of all three sandwich legs (r5 §4 as amended
    /// by the r5 verdict): config-file identity (exact bytes), setup-file
    /// identity (exact bytes), coordinator revision. The PARSED values
    /// come from the same buffers that were hashed.
    fn capture(&self) -> Option<CoherentRead> {
        // Winning config path with Config::load precedence; missing,
        // unreadable, and malformed are distinct and ALL refuse (r5 §4).
        let config_path = winning_config_path(&self.config_paths)?;
        let config_bytes = std::fs::read(config_path).ok()?;
        let config_hash = Sha256::digest(&config_bytes);
        let config = awili_config::Config::parse(std::str::from_utf8(&config_bytes).ok()?).ok()?;

        let (setup_hash, records) = match &self.setup_path {
            Some(p) => match std::fs::read(p) {
                Ok(bytes) => {
                    let h = Some(Sha256::digest(&bytes).to_vec());
                    let text = String::from_utf8(bytes).ok()?;
                    (h, load_setup_records_from(&text, p))
                }
                // Absent hashes as absent (a distinct identity); the
                // record lookup below then refuses on Absent.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    (None, SetupRecords::default())
                }
                Err(_) => return None,
            },
            None => (None, SetupRecords::default()),
        };
        let revision = self.facade.lifecycle().revision;
        Some(CoherentRead {
            config_hash: config_hash.to_vec(),
            config,
            setup_hash,
            records,
            revision,
        })
    }

    /// The identity legs only (for the post-check re-capture).
    fn capture_identities(&self) -> Option<(Vec<u8>, Option<Vec<u8>>, u64)> {
        let config_path = winning_config_path(&self.config_paths)?;
        let config_hash = Sha256::digest(std::fs::read(config_path).ok()?).to_vec();
        let setup_hash = match &self.setup_path {
            Some(p) => match std::fs::read(p) {
                Ok(b) => Some(Sha256::digest(&b).to_vec()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return None,
            },
            None => None,
        };
        Some((config_hash, setup_hash, self.facade.lifecycle().revision))
    }

    /// The current entry, iff it is the ONE reserved entry, enabled,
    /// Work-member, and launch-identical to the running composition
    /// (trust excluded). `None` = refuse.
    fn current_entry<'c>(
        &self,
        config: &'c awili_config::Config,
    ) -> Option<&'c awili_config::McpServerConfig> {
        let mut reserved = config
            .mcp
            .servers
            .iter()
            .filter(|s| s.name == awili_config::INSTALL_TRUSTED_SERVER);
        let entry = reserved.next()?;
        if reserved.next().is_some() {
            return None; // multiple reserved entries: never first-match
        }
        if !entry.enabled || !entry.offers_in(awili_config::McpMode::Work) {
            return None; // live eligibility predicates (ruling 42)
        }
        if RunningServerIdentity::of(entry) != self.running {
            return None; // the file describes a server that is not running
        }
        entry.is_install_trusted().then_some(entry)
    }

    /// Everything up to a verified, live, setup-matched launch — shared by
    /// the per-call witness query and the turn-open seed candidate.
    pub(crate) fn verified_now(
        &self,
        origin: &ToolOrigin,
    ) -> Option<(
        CoherentRead,
        std::sync::Arc<awili_tools::runtime::ConnectedLaunch>,
    )> {
        if origin.origin != format!("mcp:{}", awili_config::INSTALL_TRUSTED_SERVER) {
            return None;
        }
        let read = self.capture()?;
        let entry = self.current_entry(&read.config)?;
        let record = read.records.record(origin)?;
        let view = InstallAuthorityView::current(record, entry).ok()?;
        let (generation, _epoch) = self.facade.live_generation(origin)?;
        let connected = self.facade.connected(origin, generation)?;
        crate::mcp_catalog::install_grant_input(&view, &self.facade, &connected).ok()?;
        // Post-capture: any change to ANY leg between the two reads
        // refuses (the sandwich; byte-identical ABA is harmless).
        let (ch, sh, rev) = self.capture_identities()?;
        if ch != read.config_hash || sh != read.setup_hash || rev != read.revision {
            return None;
        }
        Some((read, connected))
    }

    /// The turn-open seed candidate (design §5.1): current authority +
    /// the committed catalog's floors, or None.
    pub fn seed_candidate(&self, origin: &ToolOrigin) -> Option<InstallSeedCandidate> {
        let (_read, connected) = self.verified_now(origin)?;
        let floors = awili_engine_worker::floor_owner::accepted_floors(origin)?;
        let awili_tools::attest::BoundLaunch::Verified(v) = connected.bind() else {
            return None;
        };
        InstallSeedCandidate::mint(v, floors).ok()
    }
}

/// Crate-internal (it crosses no boundary) — `verified_now` hands one out to
/// its callers within the crate.
pub(crate) struct CoherentRead {
    config_hash: Vec<u8>,
    config: awili_config::Config,
    setup_hash: Option<Vec<u8>>,
    records: SetupRecords,
    revision: u64,
}

impl InstallAuthorityQuery for WorkInstallAuthority {
    fn with_witness(&self, origin: &ToolOrigin, f: InstallWitnessCallback<'_>) -> Decision {
        match self.verified_now(origin) {
            Some((_read, connected)) => match connected.bind() {
                awili_tools::attest::BoundLaunch::Verified(v) => {
                    v.with_install_witness(|w| f(Some(w)))
                }
                awili_tools::attest::BoundLaunch::Unverified(_) => f(None),
            },
            None => f(None),
        }
    }
}

/// RC2-APP (F0152): the config file that wins, with the config loader's
/// precedence: the FIRST candidate that exists in any form — a dangling
/// link or an unreadable file included, which then refuses when it is
/// read, never falls through to a lower-priority file. A candidate whose
/// existence cannot even be checked refuses too (`None`).
fn winning_config_path(paths: &[PathBuf]) -> Option<&PathBuf> {
    for path in paths {
        match std::fs::symlink_metadata(path) {
            Ok(_) => return Some(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        }
    }
    None
}

#[cfg(test)]
mod rc2app_tests {
    use super::*;

    #[cfg(unix)]
    /// RC2-APP (F0152): a higher-priority config that is a dangling link or
    /// unreadable is the winner (and refuses when read) — it never lets a
    /// lower-priority file decide.
    #[test]
    fn rc2app_a_broken_higher_config_is_not_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let high = dir.path().join("high.toml");
        let low = dir.path().join("low.toml");
        std::fs::write(&low, "x").unwrap();
        let missing = dir.path().join("missing.toml");
        let paths = vec![missing.clone(), high.clone(), low.clone()];
        assert_eq!(winning_config_path(&paths), Some(&low), "absent ones skip");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &high).unwrap();
        assert_eq!(
            winning_config_path(&paths),
            Some(&high),
            "a dangling link wins"
        );
        assert!(std::fs::read(&high).is_err(), "and refuses when read");
        assert_eq!(winning_config_path(&[missing]), None);
    }
}

#[cfg(test)]
mod tests {
    //! Impl review r1 P1-4: the REAL authority implementation under test —
    //! exact-byte config selection, single-reserved-entry rule, live
    //! eligibility gates, running-identity mismatch and restoration, the
    //! setup leg, and the coordinator leg (dead generation).

    use super::*;
    use awili_engine_log::IntentTicket;
    use awili_engine_worker::mcp_coordinator::McpCoordinator;
    use std::io::Write as _;

    const CONFIG_OK: &str = r#"
[[mcp.servers]]
name    = "mac-shell"
command = "/usr/local/lib/awili/mac-shell"
args    = []
trust   = "install"
modes   = ["work"]
"#;

    fn write_config(dir: &std::path::Path, body: &str) -> PathBuf {
        let p = dir.join("config.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    fn write_setup(dir: &std::path::Path) -> PathBuf {
        let p = dir.join("attested-servers.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        write!(
            f,
            r#"
[[server]]
origin        = "mcp:mac-shell"
program       = "/usr/local/lib/awili/mac-shell"
launch_kind   = "direct"
requirement   = 'cdhash H"00"'
protocol_name = "mac-shell"
trust         = "install"
"#
        )
        .unwrap();
        p
    }

    struct Rig {
        _dir: tempfile::TempDir,
        config_path: PathBuf,
        authority: Arc<WorkInstallAuthority>,
        // DROP ORDER: the sink (and command receiver) must go BEFORE the
        // coordinator — its Drop waits for the event stream to close,
        // which only happens once the sender is gone (the scripted-
        // runtime hazard).
        sink: awili_tools::runtime::McpEventSink<IntentTicket>,
        _cmds: async_channel::Receiver<awili_tools::runtime::McpCommand<IntentTicket>>,
        _coord: McpCoordinator<IntentTicket>,
        facade: McpFacade<IntentTicket>,
        generation: awili_engine::generation::ConnectionGeneration,
        origin: ToolOrigin,
    }

    /// A full rig: config + setup on disk, a scripted coordinator driven
    /// to Ready for mcp:mac-shell with the setup record's real facts.
    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let config_path = write_config(dir.path(), CONFIG_OK);
        let setup_path = write_setup(dir.path());
        let config = awili_config::Config::parse(CONFIG_OK).unwrap();
        let entry = config
            .mcp
            .servers
            .iter()
            .find(|s| s.name == "mac-shell")
            .unwrap();
        let running = RunningServerIdentity::of(entry);

        let (ctx, cmds) =
            async_channel::bounded::<awili_tools::runtime::McpCommand<IntentTicket>>(8);
        let (sink, stream) = awili_tools::runtime::event_channel::<IntentTicket>(16);
        let handle = awili_tools::runtime::McpRuntimeHandle::from_parts(
            ctx,
            stream,
            std::thread::spawn(|| {}),
        );
        let coord = McpCoordinator::new(handle);
        let facade = coord.facade();

        let origin = ToolOrigin::new("mcp:mac-shell");
        let records = awili_attest::setup_record::load_setup_records(&setup_path);
        let rec = records.record(&origin).expect("record");
        let generation = awili_engine::generation::process().next();
        let token = awili_tools::attest::LaunchToken::mint_verified(rec.facts(), generation);
        sink.emit_server_connected_blocking(
            token,
            "mac-shell".into(),
            vec![awili_tools::runtime::AdvertisedTool {
                name: "run".into(),
                arg_schema_canonical: r#"{"type":"object","properties":{}}"#.into(),
                risk: awili_engine::RiskClass::Privileged,
                description: String::new(),
            }],
            Vec::new(),
            Some(1),
        )
        .expect("emit connected");
        sink.send_blocking(awili_tools::runtime::McpEvent::RootsAccepted {
            origin: origin.clone(),
            generation,
            epoch: 1,
        })
        .expect("emit accepted");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while facade.live_generation(&origin) != Some((generation, 1)) {
            assert!(std::time::Instant::now() < deadline, "never Ready");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let authority = WorkInstallAuthority::with_paths_for_tests(
            running,
            facade.clone(),
            Some(setup_path),
            vec![config_path.clone()],
        );
        Rig {
            _dir: dir,
            config_path,
            authority,
            _coord: coord,
            sink,
            _cmds: cmds,
            facade,
            generation,
            origin,
        }
    }

    #[test]
    fn the_full_chain_issues_and_each_leg_refuses() {
        let rig = rig();
        // Happy: the whole chain — one reserved entry, enabled, Work,
        // identity match, trust install, Valid record, live generation,
        // sandwich stable — yields a verified launch.
        assert!(rig.authority.verified_now(&rig.origin).is_some());

        // LIVE KILL: trust flipped to prompt in the CURRENT file.
        std::fs::write(
            &rig.config_path,
            CONFIG_OK.replace(r#"trust   = "install""#, r#"trust   = "prompt""#),
        )
        .unwrap();
        assert!(rig.authority.verified_now(&rig.origin).is_none());
        // LIVE ENABLE: back to install — the identical composition
        // re-enables without a restart.
        std::fs::write(&rig.config_path, CONFIG_OK).unwrap();
        assert!(rig.authority.verified_now(&rig.origin).is_some());

        // Duplicate reserved entries: never first-match.
        std::fs::write(
            &rig.config_path,
            format!("{CONFIG_OK}{}", CONFIG_OK.replace("install", "prompt")),
        )
        .unwrap();
        assert!(rig.authority.verified_now(&rig.origin).is_none());

        // Disabled: live eligibility gate.
        std::fs::write(
            &rig.config_path,
            CONFIG_OK.replace(
                r#"modes   = ["work"]"#,
                "modes   = [\"work\"]\nenabled = false",
            ),
        )
        .unwrap();
        assert!(rig.authority.verified_now(&rig.origin).is_none());

        // Work membership removed: refuses; unrelated modes are irrelevant.
        std::fs::write(
            &rig.config_path,
            CONFIG_OK.replace(r#"modes   = ["work"]"#, r#"modes   = ["chat"]"#),
        )
        .unwrap();
        assert!(rig.authority.verified_now(&rig.origin).is_none());
        std::fs::write(
            &rig.config_path,
            CONFIG_OK.replace(
                r#"modes   = ["work"]"#,
                r#"modes   = ["chat", "work", "code"]"#,
            ),
        )
        .unwrap();
        assert!(rig.authority.verified_now(&rig.origin).is_some());
        // Launch-identity mismatch: the file describes a DIFFERENT server;
        // restoring trust alone cannot override it (ruling 42).
        std::fs::write(
            &rig.config_path,
            CONFIG_OK.replace("/usr/local/lib/awili/mac-shell", "/tmp/imposter"),
        )
        .unwrap();
        assert!(rig.authority.verified_now(&rig.origin).is_none());
        std::fs::write(&rig.config_path, CONFIG_OK).unwrap();
        assert!(rig.authority.verified_now(&rig.origin).is_some());
        // Malformed config: distinct from missing, equally refusing.
        std::fs::write(&rig.config_path, "not [ valid { toml").unwrap();
        assert!(rig.authority.verified_now(&rig.origin).is_none());
        // Missing config: refuses.
        std::fs::remove_file(&rig.config_path).unwrap();
        assert!(rig.authority.verified_now(&rig.origin).is_none());
        std::fs::write(&rig.config_path, CONFIG_OK).unwrap();
        // Setup-record leg: replaced identity refuses (the launch's facts
        // no longer match the CURRENT record).
        let setup = rig.authority.setup_path.clone().unwrap();
        let original = std::fs::read(&setup).unwrap();
        std::fs::write(
            &setup,
            String::from_utf8(original.clone())
                .unwrap()
                .replace("cdhash H\"00\"", "cdhash H\"ff\""),
        )
        .unwrap();
        assert!(rig.authority.verified_now(&rig.origin).is_none());
        std::fs::write(&setup, original).unwrap();
        assert!(rig.authority.verified_now(&rig.origin).is_some());
        // Coordinator leg: the generation dies — no witness.
        rig.sink
            .send_blocking(awili_tools::runtime::McpEvent::ServerDied {
                origin: rig.origin.clone(),
                generation: rig.generation,
                cause: awili_tools::runtime::DeathCause::TransportFailed,
                selected_class: Some(awili_tools::attest::AttestationClass::Verified),
                detail: "scripted death".into(),
            })
            .expect("emit died");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while rig.facade.live_generation(&rig.origin).is_some() {
            assert!(std::time::Instant::now() < deadline, "never died");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(rig.authority.verified_now(&rig.origin).is_none());
    }
}
