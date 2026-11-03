//! RC2-R619 (usability RC2 round 5, Major 1: a cold first Gemma turn in Work
//! took 54 s — "what is in alpha.txt?", 57,589 tokens, the right panel on
//! "Reading your request… 85%" — and the owner would read that as hung).
//!
//! Almost all of a cold first turn is the model server reading what every
//! request of a folder begins with: the owner's standing rules, AGENTS.md,
//! Awili's brief and the tool list (about 18k tokens, 44 s on the Mac
//! Studio's Gemma). So when a Work conversation is on screen with the in-app
//! model as its helper, Awili starts that conversation's worker and has it
//! warm the server's cache ([`crate::work::GatewayCmd::Warm`]) — before the
//! owner has typed. His first question then reads only itself (about 3 s).
//! A question sent while the warm-up still reads waits for it in the same
//! worker (the server reads one request at a time well; two at once are
//! both slower) and its Progress line says what it waits for.
//!
//! Once per folder while the server can be expected to keep it
//! ([`WARM_FRESH`]); never while a run is going; never for another helper.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use awili_core::ConvKind;
use gpui::Context;

use crate::work::{GatewayCmd, Mode, WorkExecutor};
use crate::AwiliApp;

/// How long a warmed folder counts as warm: the Studio's mlx_lm keeps eight
/// prompt caches, so a folder not used for this long is warmed again.
pub(crate) const WARM_FRESH: Duration = Duration::from_secs(10 * 60);

/// RC2-R6M2: how long after a warm-up that was not read (the server was
/// down, or it was stopped) the same one is tried again.
pub(crate) const WARM_RETRY: Duration = Duration::from_secs(60);

/// The memory read a warm-up's rules come from asks this (the standing rules
/// do not depend on it; the read needs some query).
const WARM_QUERY: &str = "Awili Work";

/// RC2-R6M7 (RC2-R6 review P2 on 0dd1a343: warmth was kept by folder
/// alone, so after the model or its server changed a recently warmed folder
/// was not warmed for the new one): the model server and model a warm-up
/// is read by.
pub(crate) fn engine_id(base_url: &str, model: &str) -> String {
    format!("{}|{}", base_url.trim().trim_end_matches('/'), model.trim())
}

/// RC2-R6M7: what a Work folder's warmth is kept by: the model server,
/// the model and the folder.
pub(crate) fn work_warm_key(engine: &awili_config::EngineConfig, folder: &str) -> String {
    format!(
        "work|{}|{folder}",
        engine_id(&engine.base_url, &engine.model)
    )
}

/// The warm-ups of this session.
pub(crate) struct WarmState {
    /// Folders (and council members, RC2-R620) warmed, and when.
    /// RC2-R6M2 (RC2-R6 review P2 on 0fc8434b): recorded once the server
    /// has read the warm-up, never when it is only started.
    pub(crate) warmed: HashMap<String, Instant>,
    /// RC2-R6M2: the warm-ups started and not yet ended.
    pub(crate) in_flight: HashSet<String>,
    /// RC2-R6M2: warm-ups that ended unread, and when (tried again after
    /// [`WARM_RETRY`]).
    pub(crate) failed: HashMap<String, Instant>,
    /// RC2-R6M4: the Work warm-ups sent to an in-app worker and not yet
    /// answered, oldest first: that worker's serial and the folder (a
    /// worker answers its warm-ups in order).
    pub(crate) pending: Vec<(u64, String)>,
    /// RC2-R6M4: the serial of the in-app Work worker started last.
    pub(crate) worker_serial: u64,
    /// RC2-R6M7: the model server and model of that worker ([`engine_id`]).
    pub(crate) worker_engine: Option<String>,
    /// A warm-up is being prepared (its memory read is under way).
    pub(crate) preparing: bool,
    /// Whether warm-ups happen. Off under `cargo test` (a test turns it on),
    /// so no test reaches a model server it did not set up.
    pub(crate) enabled: bool,
}

impl Default for WarmState {
    fn default() -> Self {
        Self {
            warmed: HashMap::new(),
            in_flight: HashSet::new(),
            failed: HashMap::new(),
            pending: Vec::new(),
            worker_serial: 0,
            worker_engine: None,
            preparing: false,
            enabled: !cfg!(test),
        }
    }
}

impl WarmState {
    /// RC2-R6M2: whether the warm-up `key` is due at `now`: not under way,
    /// not read within [`WARM_FRESH`], and not left unread within
    /// [`WARM_RETRY`].
    pub(crate) fn is_due(&self, key: &str, now: Instant) -> bool {
        let within = |at: Option<&Instant>, span: Duration| {
            at.is_some_and(|at| now.saturating_duration_since(*at) < span)
        };
        !self.in_flight.contains(key)
            && !within(self.warmed.get(key), WARM_FRESH)
            && !within(self.failed.get(key), WARM_RETRY)
    }

    /// RC2-R6M2: the warm-up `key` was started.
    pub(crate) fn begin(&mut self, key: &str) {
        self.in_flight.insert(key.to_string());
    }

    /// RC2-R6M2: the warm-up `key` ended at `now`; `read` when the server
    /// read it through — only then is it counted warm.
    pub(crate) fn finish(&mut self, key: &str, read: bool, now: Instant) {
        self.in_flight.remove(key);
        if read {
            self.warmed.insert(key.to_string(), now);
            self.failed.remove(key);
        } else {
            self.warmed.remove(key);
            self.failed.insert(key.to_string(), now);
        }
    }

    /// RC2-R6M4: worker `serial` answered its oldest warm-up (`Some(read)`),
    /// or ended (`None`: each warm-up it had not answered was not read).
    pub(crate) fn worker_warmed(&mut self, serial: u64, read: Option<bool>, now: Instant) {
        let mine = |p: &(u64, String)| p.0 == serial;
        match read {
            Some(read) => {
                if let Some(i) = self.pending.iter().position(mine) {
                    let (_, key) = self.pending.remove(i);
                    self.finish(&key, read, now);
                }
            }
            None => {
                let (gone, kept): (Vec<_>, Vec<_>) = self.pending.drain(..).partition(mine);
                self.pending = kept;
                for (_, key) in gone {
                    self.finish(&key, false, now);
                }
            }
        }
    }
}

impl AwiliApp {
    /// The conversation and folder a warm-up is due for now, if any: Work on
    /// screen, the in-app model as its helper, no run going, and the folder
    /// not warmed lately.
    ///
    /// RC2-R6M4 (RC2-R6 review P2 on 9c0fa36d: a conversation whose worker
    /// stayed alive was never warmed again, so a question after a long idle
    /// spell read everything cold): a live, idle worker of that
    /// conversation is warmed again once [`WARM_FRESH`] has passed (its own
    /// turns count as reading the folder, `spawn_work_worker`).
    pub(crate) fn warm_due(&self, now: Instant) -> Option<(String, PathBuf)> {
        let w = &self.work;
        if !w.warm.enabled || w.warm.preparing || self.mode != Mode::Work {
            return None;
        }
        if w.running
            || w.gateway_stop_pending
            || w.agent_gate_pending
            || w.stopped_barrier.is_some()
            || w.queued_after_stop.is_some()
        {
            return None;
        }
        if !matches!(w.executor, WorkExecutor::InApp) {
            return None;
        }
        let engine = w.effective_engine();
        if engine.kind != awili_config::EngineKind::Awili || engine.base_url.trim().is_empty() {
            return None;
        }
        let conv = self.sidebar.active_id().to_string();
        if self
            .conversations
            .get(&conv)
            .is_none_or(|c| c.kind != ConvKind::Work || c.archived)
        {
            return None;
        }
        let folder = w.folder.clone()?;
        w.warm
            .is_due(&work_warm_key(&engine, &folder.display().to_string()), now)
            .then_some((conv, folder))
    }

    /// Start the warm-up that is due, if one is (the 1 s health tick asks):
    /// read the standing rules as a turn does (recalled notes are no part of
    /// what a folder's requests begin with), compose the system text a turn
    /// would pin, and start the conversation's worker with it.
    pub(crate) fn maybe_warm_inapp(&mut self, cx: &mut Context<Self>) {
        let Some((conv, folder)) = self.warm_due(Instant::now()) else {
            return;
        };
        if !crate::work::inapp_memory_on(&self.memory, self.work.agents.memory.as_ref()) {
            let system = self.work_system_for_engine(None, Some(&folder));
            self.start_warm(conv, folder, system, cx);
            return;
        }
        self.work.warm.preparing = true;
        let mem = crate::MemoryCfg {
            enabled: true,
            recall: false,
            ..self.memory.clone()
        };
        cx.spawn(async move |this, cx| {
            let gate = crate::recall::gate_turn_memory_with(
                mem,
                WARM_QUERY.to_string(),
                crate::recall::production_client,
                crate::recall::INAPP_RECALL_NOTE_BYTES,
            )
            .await;
            this.update(cx, |app, cx| {
                app.work.warm.preparing = false;
                // A refused policy refuses turns too: nothing to warm.
                let Ok(policy) = gate.outcome else {
                    return;
                };
                // Still due, for the same conversation and folder.
                if app.warm_due(Instant::now()) != Some((conv.clone(), folder.clone())) {
                    return;
                }
                let system = app.work_system_for_engine(policy, Some(&folder));
                app.start_warm(conv, folder, system, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Start `conv`'s worker with a warm-up of `system` (the conversation's
    /// next question finds it alive and is offered to it).
    fn start_warm(
        &mut self,
        conv: String,
        folder: PathBuf,
        system: Option<String>,
        cx: &mut Context<Self>,
    ) {
        // RC2-R6M2: under way; counted warm once the worker says it was
        // read (`spawn_work_worker`'s event loop).
        let engine = self.work.effective_engine();
        let key = work_warm_key(&engine, &folder.display().to_string());
        self.work.warm.begin(&key);
        // SAFETY: a warm-up is a chat request. It goes only to a host a recent
        // health read showed to be another kind of server, or Lemonade with the
        // model resident (never to a host that may load it on demand). This is
        // a cache lookup: nothing blocks the UI thread.
        if !crate::strip::warm_allowed(&engine.base_url, &engine.model) {
            self.work.warm.finish(&key, false, Instant::now());
            return;
        }
        // RC2-R6M7: a live worker that reads from another model or server
        // is retired, as a turn retires one of another model (D6).
        if self.work.worker_conv.as_deref() == Some(conv.as_str())
            && !self.work.gateway_stop_pending
            && self.work.warm.worker_engine.as_deref()
                != Some(engine_id(&engine.base_url, &engine.model).as_str())
        {
            if let Some(tx) = self.work.gateway_cmd_tx.take() {
                let _ = tx.send(GatewayCmd::Shutdown);
                self.work.session_id = None;
                self.work.worker_identity = None;
            }
        }
        // RC2-R6M4: this conversation's worker is alive and idle: it
        // warms again (the question that follows waits for it).
        if self.work.worker_conv.as_deref() == Some(conv.as_str()) {
            if let Some(tx) = &self.work.gateway_cmd_tx {
                let cmd = GatewayCmd::Warm {
                    system: system.clone(),
                };
                if tx.send(cmd).is_ok() {
                    let serial = self.work.warm.worker_serial;
                    self.work.warm.pending.push((serial, key));
                    return;
                }
                self.work.gateway_cmd_tx = None;
            }
        }
        crate::work::retire_worker_of_other_conversation(&mut self.work, &conv);
        crate::ask_mode::install_conversation_mode(
            &self.work.approval_handle,
            &self.work.ask_modes,
            &conv,
        );
        let cancel = Arc::new(AtomicBool::new(false));
        if self.spawn_work_worker(
            GatewayCmd::Warm { system },
            Some(folder),
            conv,
            cancel,
            true,
            cx,
        ) {
            let serial = self.work.warm.worker_serial;
            self.work.warm.pending.push((serial, key));
        } else {
            self.work.warm.finish(&key, false, Instant::now());
        }
    }
}

/// RC2-R620: a council member on Awili's tool engine (the in-app model, or
/// another HTTP model) due a warm-up: its key in [`WarmState::warmed`],
/// where it answers, its window, and what its next turn opens with.
pub(crate) struct CouncilWarm {
    pub(crate) key: String,
    pub(crate) target: crate::council_exec::EngineTarget,
    pub(crate) window: u32,
    /// The engine this member answers on (its own timeout and limits).
    pub(crate) base: awili_config::EngineConfig,
    pub(crate) seat: awili_council::SeatPrompt,
}

impl AwiliApp {
    /// RC2-R620 (usability RC2 round 5, Major 1: in a council's first round
    /// Gemma's first words came after 24-42 s): the members of the council on
    /// screen that answer on Awili's tool engine and were not warmed lately,
    /// with the seat their next turn opens with (as the Codex and Claude
    /// members are made ready, `council_prewarm`). None while it answers.
    pub(crate) fn council_warm_due(&self, now: Instant) -> Vec<CouncilWarm> {
        let w = &self.work.warm;
        if !w.enabled || w.preparing || self.mode != Mode::Council {
            return Vec::new();
        }
        let conv = self.sidebar.active_id().to_string();
        let Some(page) = self.councils.get(&conv).filter(|p| !p.council.is_running()) else {
            return Vec::new();
        };
        let opts = awili_council::PlanOptions::new(&crate::council_run::owner_name());
        let plan = awili_council::plan_round(
            &page.council,
            awili_council::RoundKind::Ask,
            Some(""),
            &opts,
        );
        let setup = self.member_setup();
        plan.seats
            .into_iter()
            .filter_map(|seat| {
                let member = page.council.member(&seat.member)?;
                let target = crate::council_exec::engine_target(&member.route, &setup)?;
                // RC2-R6M7: kept by the model as well as its server.
                let key = format!(
                    "council:{conv}:{}:{}",
                    member.id,
                    engine_id(&target.base_url, &target.model)
                );
                w.is_due(&key, now).then(|| CouncilWarm {
                    key,
                    target,
                    window: crate::council_run::window_tokens(
                        &member.route,
                        &self.work.engine,
                        &self.work.extra_engines,
                    ),
                    base: crate::council_run::engine_for_route(
                        &member.route,
                        &self.work.engine,
                        &self.work.extra_engines,
                    )
                    .unwrap_or_default(),
                    seat,
                })
            })
            .collect()
    }

    /// RC2-R620: warm the due members of the council on screen (the 1 s
    /// health tick asks): the standing rules read as a turn reads them, the
    /// system text a member's turn would pin, a warm-up worker each.
    ///
    /// RC2-R6M2 (RC2-R6 review P2 on 0fc8434b: a member was counted warm for
    /// ten minutes the moment its warm-up was started, even when the server
    /// never read it): each member is under way until its worker ends, and
    /// counted warm only when the worker says the server read it through.
    pub(crate) fn maybe_warm_council(&mut self, cx: &mut Context<Self>) {
        let mut due = self.council_warm_due(Instant::now());
        // SAFETY: a warm-up is a chat request: only to a host a recent health
        // read showed safe (see `strip::warm_allowed`); a cache lookup.
        due.retain(|d| {
            let ok = crate::strip::warm_allowed(&d.target.base_url, &d.target.model);
            if !ok {
                self.work.warm.begin(&d.key);
                self.work.warm.finish(&d.key, false, Instant::now());
            }
            ok
        });
        if due.is_empty() {
            return;
        }
        for d in &due {
            self.work.warm.begin(&d.key);
        }
        let keys: Vec<String> = due.iter().map(|d| d.key.clone()).collect();
        let agents_block = crate::council_exec::owner_agents_block(&self.work.agents);
        // Each member's warm-up: its key, and whether the server read it.
        let start = move |policy: Option<String>| {
            due.into_iter()
                .map(|d| {
                    let system = crate::council_run::engine_member_system(
                        policy.as_deref(),
                        agents_block.as_deref(),
                        &d.seat,
                    );
                    let log_key = format!("council-warm-{}", uuid::Uuid::new_v4());
                    let started = crate::council_exec::warm_engine_member(
                        &d.target,
                        &d.base,
                        d.window,
                        &crate::council_exec::member_folder(true),
                        &log_key,
                        Some(system).filter(|s| !s.is_empty()),
                    );
                    async move {
                        let read = match started {
                            Ok(done) => done.await.unwrap_or(false),
                            Err(e) => {
                                eprintln!("awili: a council warm-up did not start: {e}");
                                false
                            }
                        };
                        (d.key, read)
                    }
                })
                .collect::<futures::stream::FuturesUnordered<_>>()
        };
        // The memory read as a council round makes it (`[memory]` decides),
        // without the recalled notes.
        self.work.warm.preparing = true;
        let mem = crate::MemoryCfg {
            recall: false,
            ..self.memory.clone()
        };
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            let gate = crate::recall::gate_turn_memory_with(
                mem,
                WARM_QUERY.to_string(),
                crate::recall::production_client,
                crate::recall::INAPP_RECALL_NOTE_BYTES,
            )
            .await;
            this.update(cx, |app, _| app.work.warm.preparing = false)
                .ok();
            let Ok(policy) = gate.outcome else {
                // A refused policy refuses turns too: nothing was read.
                this.update(cx, |app, _| {
                    for key in &keys {
                        app.work.warm.finish(key, false, Instant::now());
                    }
                })
                .ok();
                return;
            };
            let mut warmups = start(policy);
            while let Some((key, read)) = warmups.next().await {
                this.update(cx, |app, _| {
                    app.work.warm.finish(&key, read, Instant::now())
                })
                .ok();
            }
        })
        .detach();
    }
}

#[cfg(test)]
#[path = "inapp_warm_tests.rs"]
mod tests;
