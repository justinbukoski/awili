//! L1c — the one-time migration of the persisted Work executor and the one
//! source of truth for executor writes (S3-F06: the chip and
//! `app_state.work_executor` never disagree).
//!
//! Before L1, "gateway" was the product default AND the value a consumed
//! local Build reset to, so a persisted "gateway" is not evidence of a
//! choice. Once per install it becomes what the user called out in
//! `[work].default_executor` (when that parses), else "in-app" — "work
//! should always be local to the app unless otherwise called out" — with a
//! one-time notice when it became in-app. Every other persisted value is
//! kept. After the migration, a picked fleet
//! gateway persists and survives restarts. The app never rewrites
//! config.toml.

use awili_code::CodeEngine;

use crate::persistence::{Persistence, WORK_EXECUTOR_KEY};
use crate::work::{seed_executor, WorkExecutor, WorkState};

/// The app_state marker: `"1"` once this install has been migrated.
pub(crate) const MIGRATION_KEY: &str = "work_executor_l1";

/// The one-time toast after a persisted legacy "gateway" became "in-app".
pub(crate) const MIGRATION_NOTICE: &str =
    "Work now runs in Awili on this Mac. The fleet gateway is still in the executor menu.";

/// The outcome of the migration for one launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Migrated {
    /// The persisted executor the app seeds from (raw form).
    pub(crate) value: Option<String>,
    /// Whether the store must be rewritten to `value`.
    pub(crate) rewrite: bool,
    /// Whether the one-time notice shows on this launch.
    pub(crate) notice: bool,
}

/// Pure. `already` → unchanged, no rewrite, no notice. `Some("gateway")` →
/// the parsed `configured` seed (`[work].default_executor`), else in-app;
/// rewritten; the notice only when the result is in-app. Anything else
/// (absent, `agent:…`, `local-build:…`, `in-app`, garbage) → unchanged, no
/// rewrite, no notice.
pub(crate) fn migrate(
    persisted: Option<&str>,
    already: bool,
    configured: Option<&str>,
) -> Migrated {
    if !already && persisted == Some("gateway") {
        let next = seed_executor(None, configured).unwrap_or(WorkExecutor::InApp);
        return Migrated {
            value: Some(next.to_persist()),
            rewrite: true,
            notice: next == WorkExecutor::InApp,
        };
    }
    Migrated {
        value: persisted.map(str::to_string),
        rewrite: false,
        notice: false,
    }
}

/// Read the executor and the marker from `store`, apply [`migrate`] with
/// the config's `default_executor`, write the new value when it rewrites, then write the marker when it was absent. A
/// read or write error is logged and the unmigrated read is returned with
/// no notice (the marker stays absent, so the next launch tries again): a
/// migration never blocks launch.
pub(crate) fn migrate_store(store: &awili_persist::Store, configured: Option<&str>) -> Migrated {
    let unmigrated = |value: Option<String>| Migrated {
        value,
        rewrite: false,
        notice: false,
    };
    let persisted = match store.get_app_state(WORK_EXECUTOR_KEY) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("awili: work executor read failed ({e})");
            return unmigrated(None);
        }
    };
    let already = match store.get_app_state(MIGRATION_KEY) {
        Ok(v) => v.is_some(),
        Err(e) => {
            eprintln!("awili: executor migration marker read failed ({e})");
            return unmigrated(persisted);
        }
    };
    let out = migrate(persisted.as_deref(), already, configured);
    if out.rewrite {
        let value = out.value.as_deref().unwrap_or_default();
        if let Err(e) = store.set_app_state(WORK_EXECUTOR_KEY, value) {
            eprintln!("awili: work executor migration failed ({e})");
            return unmigrated(persisted);
        }
    }
    if !already {
        if let Err(e) = store.set_app_state(MIGRATION_KEY, "1") {
            eprintln!("awili: executor migration marker write failed ({e})");
        }
    }
    out
}

/// The CLI-only launch recovery (end of `AwiliApp::new`): `None` keeps the
/// current executor — the in-app default, an explicitly picked fleet gateway
/// (after the migration a persisted "gateway" IS a pick), or an agent whose
/// binary is installed. A local Build or an uninstalled agent is replaced by
/// the configured harness (`configured.cloned()`).
pub(crate) fn recovered_executor(
    current: &WorkExecutor,
    configured: Option<&WorkExecutor>,
    agent_installed: impl Fn(CodeEngine) -> bool,
) -> Option<WorkExecutor> {
    let keep = match current {
        WorkExecutor::InApp | WorkExecutor::InAppExtra { .. } | WorkExecutor::Gateway => true,
        WorkExecutor::Agent { engine, .. } => agent_installed(*engine),
        WorkExecutor::LocalBuild { .. } => false,
    };
    if keep {
        None
    } else {
        configured.cloned()
    }
}

/// Assign `exec` and persist it (`WORK_EXECUTOR_KEY`) in one step.
pub(crate) fn set_executor(
    work: &mut WorkState,
    persist: Option<&Persistence>,
    exec: WorkExecutor,
) {
    if let Some(p) = persist {
        p.set_state(WORK_EXECUTOR_KEY, exec.to_persist());
    }
    work.executor = exec;
}

impl crate::AwiliApp {
    /// The CLI-only launch (end of `AwiliApp::new`): onboarding complete, no
    /// HTTP providers, and a configured harness recipe. Applies
    /// [`recovered_executor`] and PERSISTS a replacement (S3-F06: before L1c
    /// the chip said the harness while `app_state` still said the old
    /// value). Returns whether this is a CLI-only launch (the caller then
    /// opens Work).
    pub(crate) fn recover_cli_only_executor(&mut self) -> bool {
        if !self.onboarding.is_complete() || !self.choices.is_empty() {
            return false;
        }
        let Some(configured) = self.onboarding_harness.clone() else {
            return false;
        };
        let next = recovered_executor(&self.work.executor, Some(&configured), |e| {
            crate::harness_setup::installed_binary(e, &self.work.agents).is_some()
        });
        if let Some(next) = next {
            self.set_work_executor_persisted(next);
        }
        true
    }

    /// Assign and persist the Work executor (every non-picker write).
    /// RC2-UX (S-N3): when this lands the conversation on screen, which is
    /// set to ask first, on a helper that cannot ask (GLM, DeepSeek, a local
    /// Build, Hermes for file changes), one plain notice says so — the
    /// default-helper switch, Settings, the first-run setup, a recipe save, a
    /// helper change and the launch recovery all come here (the picker shows
    /// its own notice).
    pub(crate) fn set_work_executor_persisted(&mut self, exec: WorkExecutor) {
        let changed = self.work.executor != exec;
        set_executor(&mut self.work, self.persist.as_ref(), exec);
        if !changed {
            return;
        }
        let conv = crate::ask_mode::AskKey::Work(self.sidebar.active_id().to_string());
        if self.work.ask_modes.mode_for(&conv) != awili_work::ApprovalMode::AskBeforeChanges {
            return;
        }
        let support = crate::ask_mode::work_support(&self.work.executor, self.work_engine_kind());
        if matches!(support, crate::ask_mode::AskSupport::Unavailable(_)) {
            self.work.no_ask_notice = Some(crate::ask_mode::no_ask_toast(
                support,
                &self.work.executor.label(),
            ));
        }
    }

    /// RC2-UX (S-N3): show the notice [`Self::set_work_executor_persisted`]
    /// left (called every frame; nothing when there is none).
    pub(crate) fn show_no_ask_notice(&mut self, cx: &mut gpui::Context<Self>) {
        if let Some(notice) = self.work.no_ask_notice.take() {
            self.show_info_toast(notice, cx);
        }
    }

    /// A user pick (the picker row, the one-click cloud buttons, the L1f
    /// hint): assign and persist, close the picker, then note the Ask support
    /// change (U4f: clear the conversation's plan, toast for an executor that
    /// cannot ask).
    pub(crate) fn pick_work_executor(&mut self, exec: WorkExecutor, cx: &mut gpui::Context<Self>) {
        // RC1-O: this conversation keeps the owner's pick over its last lane;
        // a local Build is a one-shot override, never kept (pre-review 3).
        let active = self.sidebar.active_id().to_string();
        if matches!(exec, WorkExecutor::LocalBuild { .. }) {
            self.work.chip_picked.remove(&active);
        } else {
            self.work.chip_picked.insert(active.clone(), exec.clone());
            // RC2-R1F2 (round 1 N2): the next new conversation starts on it.
            self.work.last_pick = Some(exec.clone());
        }
        // The route is the source of truth for the current conversation's
        // next turn. Keep the ambient executor only as the deliberate default
        // for a new thread / legacy callers; it must not override this pick at
        // Send time, and a next-target pick never clears a live plan owner.
        let _ = self.set_composer_target(
            &crate::workshop_composer::ModelOwner::Work(active.clone()),
            exec.clone(),
            cx,
        );
        // A picker choice is also the default for the next new thread, but
        // the ambient executor still names the live run's approval/Stop
        // owner. Do not overwrite it while that owner is active; the route
        // above is already the persisted current-conversation NEXT target.
        let active_work_owner = self.work.running
            || self.work.gateway_stop_pending
            || self.work.agent_gate_pending
            || self.work.active_work_route_turn.is_some()
            || self
                .work
                .conversation_routes
                .get(&active)
                .is_some_and(|route| route.active().is_some());
        if !active_work_owner {
            self.set_work_executor_persisted(exec.clone());
        } else if let Some(persist) = self.persist.as_ref() {
            // Keep the intentional default for a later/new thread without
            // changing the ambient field that names this live run's owner.
            persist.set_state(WORK_EXECUTOR_KEY, exec.to_persist());
        }
        if self
            .work
            .ask_modes
            .mode_for(&crate::ask_mode::AskKey::Work(active))
            == awili_work::ApprovalMode::AskBeforeChanges
        {
            let support = crate::ask_mode::work_support(&exec, self.work_engine_kind());
            if matches!(support, crate::ask_mode::AskSupport::Unavailable(_)) {
                self.show_toast(crate::ask_mode::no_ask_toast(support, &exec.label()), cx);
            }
        }
        // The picker's own notice says it; no second deferred one.
        self.work.no_ask_notice = None;
        self.work.executor_picker_open = false;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work::{GatewayCfg, HermesCfg};
    use awili_persist::Store;

    fn store() -> (tempfile::TempDir, std::path::PathBuf, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("awili.db");
        let store = Store::open(&path).unwrap();
        (dir, path, store)
    }

    fn work_state() -> WorkState {
        WorkState::new(
            HermesCfg {
                base_url: String::new(),
                api_key: String::new(),
                enabled: false,
                dashboard_url: String::new(),
            },
            GatewayCfg {
                dashboard_url: String::new(),
                enabled: false,
            },
            Default::default(),
        )
    }

    #[test]
    fn l1c_legacy_gateway_becomes_in_app_once() {
        assert_eq!(
            migrate(Some("gateway"), false, None),
            Migrated {
                value: Some("in-app".into()),
                rewrite: true,
                notice: true,
            }
        );
        // A config value that does not parse is no call-out.
        assert_eq!(
            migrate(Some("gateway"), false, Some("junk"))
                .value
                .as_deref(),
            Some("in-app")
        );
        // A post-L1 pick of the fleet gateway survives.
        assert_eq!(
            migrate(Some("gateway"), true, None),
            Migrated {
                value: Some("gateway".into()),
                rewrite: false,
                notice: false,
            }
        );
    }

    /// "Unless otherwise called out": a legacy "gateway" with an explicit
    /// `[work].default_executor` becomes that executor (no in-app notice);
    /// an explicit `default_executor = "gateway"` keeps the fleet gateway.
    #[test]
    fn l1c_config_call_out_wins_over_the_in_app_default() {
        assert_eq!(
            migrate(Some("gateway"), false, Some("agent:glm")),
            Migrated {
                value: Some("agent:glm".into()),
                rewrite: true,
                notice: false,
            }
        );
        assert_eq!(
            migrate(Some("gateway"), false, Some("gateway")),
            Migrated {
                value: Some("gateway".into()),
                rewrite: true,
                notice: false,
            }
        );
        assert_eq!(
            migrate(Some("gateway"), false, Some("in-app")),
            Migrated {
                value: Some("in-app".into()),
                rewrite: true,
                notice: true,
            }
        );
    }

    #[test]
    fn l1c_explicit_choices_survive() {
        for v in [
            "agent:glm",
            "agent:claude:sonnet",
            "local-build:t1",
            "in-app",
            "garbage",
        ] {
            assert_eq!(
                migrate(Some(v), false, None),
                Migrated {
                    value: Some(v.into()),
                    rewrite: false,
                    notice: false,
                },
                "{v}"
            );
        }
    }

    #[test]
    fn l1c_store_migration_writes_once() {
        let (_dir, path, store) = store();
        store.set_app_state(WORK_EXECUTOR_KEY, "gateway").unwrap();
        let first = migrate_store(&store, None);
        assert_eq!(first.value.as_deref(), Some("in-app"));
        assert!(first.notice);
        drop(store);

        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.get_app_state(WORK_EXECUTOR_KEY).unwrap().as_deref(),
            Some("in-app")
        );
        assert_eq!(
            store.get_app_state(MIGRATION_KEY).unwrap().as_deref(),
            Some("1")
        );
        let second = migrate_store(&store, None);
        assert_eq!(second.value.as_deref(), Some("in-app"));
        assert!(!second.notice);

        // After the migration, picking the fleet gateway survives a restart.
        store.set_app_state(WORK_EXECUTOR_KEY, "gateway").unwrap();
        let third = migrate_store(&store, None);
        assert_eq!(third.value.as_deref(), Some("gateway"));
        assert!(!third.notice);
    }

    #[test]
    fn l1c_absent_value_leaves_config_seed_in_charge() {
        let (_dir, _path, store) = store();
        let out = migrate_store(&store, Some("agent:glm"));
        assert_eq!(out.value, None);
        assert!(!out.notice);
        assert_eq!(
            store.get_app_state(MIGRATION_KEY).unwrap().as_deref(),
            Some("1")
        );
        assert_eq!(
            seed_executor(out.value.as_deref(), Some("agent:glm")),
            Some(WorkExecutor::Agent {
                engine: CodeEngine::Glm,
                model: None,
            })
        );
    }

    #[test]
    fn l1c_recovery_never_replaces_in_app_or_a_picked_gateway() {
        let glm = WorkExecutor::Agent {
            engine: CodeEngine::Glm,
            model: None,
        };
        let all = |_e: CodeEngine| true;
        let none = |_e: CodeEngine| false;
        assert_eq!(
            recovered_executor(&WorkExecutor::InApp, Some(&glm), all),
            None
        );
        assert_eq!(
            recovered_executor(&WorkExecutor::Gateway, Some(&glm), all),
            None
        );
        let claude = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        };
        assert_eq!(recovered_executor(&claude, Some(&glm), all), None);
        let codex = WorkExecutor::Agent {
            engine: CodeEngine::Codex,
            model: None,
        };
        assert_eq!(
            recovered_executor(&codex, Some(&glm), none),
            Some(glm.clone())
        );
        let build = WorkExecutor::LocalBuild {
            target_id: "t".into(),
        };
        assert_eq!(
            recovered_executor(&build, Some(&glm), all),
            Some(glm.clone())
        );
        assert_eq!(recovered_executor(&codex, None, none), None);
    }

    /// S3-F06: an executor write lands in `app_state`, so the chip and the
    /// stored value agree after a restart.
    #[test]
    fn l1c_executor_writes_are_persisted() {
        let (_dir, path, store) = store();
        let (persist, worker) = Persistence::spawn_worker(store);
        let mut work = work_state();
        let glm = WorkExecutor::Agent {
            engine: CodeEngine::Glm,
            model: None,
        };
        set_executor(&mut work, Some(&persist), glm.clone());
        drop(persist);
        worker.join().unwrap();
        assert_eq!(work.executor, glm);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.get_app_state(WORK_EXECUTOR_KEY).unwrap().as_deref(),
            Some("agent:glm")
        );
    }

    /// Every executor write in main.rs goes through the persisting setter
    /// (S3-F06), and the picker row goes through `pick_work_executor`.
    #[test]
    fn l1c_main_has_no_unpersisted_executor_write() {
        let main = include_str!("main.rs");
        for needle in [
            concat!("self.work.executor", " = "),
            concat!("this.work.executor", " = "),
            concat!("app.work.executor", " = configured"),
        ] {
            assert!(!main.contains(needle), "main.rs still has {needle}");
        }
        assert_eq!(
            main.matches("this.pick_work_executor(exec.clone(), cx)")
                .count(),
            1
        );
        assert_eq!(main.matches("app.recover_cli_only_executor()").count(), 1);
        assert_eq!(
            main.matches("app.show_toast(local_first_migrate::MIGRATION_NOTICE, cx)")
                .count(),
            1
        );
    }
}
