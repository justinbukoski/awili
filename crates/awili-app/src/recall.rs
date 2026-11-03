//! Per-turn memory integration: fetch this turn's standing directives and
//! recalled past context off the UI thread, and map a recall result to its chip
//! state. Extracted from `main.rs` as part of the full-review decomposition. Each
//! fetch is a single tightly-bounded attempt that degrades to `None` (offline)
//! rather than blocking the turn; `recall_tests` (kept in `main.rs`) pins the
//! chip-state mapping.
//!
//! P4b adapter routing: on the v6.5 backend the standing directives come from
//! the typed `bootstrap` verb's `mandatory_policy` lane (read BEFORE the
//! ordinary `items` lane) and the past context from the typed `recall`
//! verb's `items` lane. The v5 recall path stays ONLY as the explicit
//! backend fallback (`MemoryBackend::V5`), never a fallback from a rejected
//! v6.5 answer.

use awili_memory::{
    policy_directives, recall_from_v65, v65, Directive, MemoryBackend, MemoryClient, RecallEntry,
    RecallStatus, RulesStatus,
};
use awili_providers::{ChatProvider, ChatRequest, ProviderEvent};
use futures::stream::{BoxStream, StreamExt};

use crate::MemoryCfg;

/// The v6.5 adapter's boot lane (P4b): the standing directives for this turn.
///
/// On the v6.5 backend the TYPED `bootstrap` verb is the boot fetch: its
/// `mandatory_policy` lane is read first (the boot-gate contract) and mapped
/// to directives; the ordinary `items` lane is left to
/// [`fetch_recall_from_client`]. On the v5 backend the historical
/// `/v5/recall` buckets stay — the explicit backend fallback.
/// Why this turn has no directives (S5(a)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DirectivesError {
    /// Memory unreachable / token missing / non-2xx: the historical degrade.
    Offline,
    /// The service answered with a malformed or over-bound policy lane.
    PolicyRejected(String),
}

/// S5(a): the same fetch as before, but a malformed or over-bound policy lane
/// is a distinct outcome — `Ok(directives)` answers, `Err(PolicyRejected(…))`
/// means the turn must be refused, `Err(Offline)` means the historical
/// degrade. The v6.5 `bootstrap` verb already fails `Parse` on a missing,
/// null or wrong-typed `mandatory_policy` lane (S5(a) base contract);
/// [`policy_directives`] re-validates the rendered block, so BOTH halves of
/// the policy contract are checked before any turn ships.
pub(crate) fn fetch_directives_from_client(
    client: &MemoryClient,
    query: &str,
) -> Result<Vec<Directive>, DirectivesError> {
    match client.backend() {
        // RC1-M-FIX1: the policy lane comes from a recall that asks for it
        // (`v65::recall_policy`), not `/v6/bootstrap`, whose live answer no
        // longer carries a `mandatory_policy` lane (every gate would refuse
        // the turn) and which took 6 s.
        MemoryBackend::V65 => match v65::recall_policy(client, query) {
            Ok(rec) => {
                policy_directives(&rec.mandatory_policy).map_err(DirectivesError::PolicyRejected)
            }
            Err(v65::V65Error::Parse(m)) => {
                eprintln!("awili: turn refused — memory policy rejected: {m}");
                Err(DirectivesError::PolicyRejected(m))
            }
            Err(e) => {
                eprintln!("awili: rules injection skipped — memory unavailable: {e}");
                Err(DirectivesError::Offline)
            }
        },
        MemoryBackend::V5 => match client.directives_v5(query, 8) {
            Ok(directives) => Ok(directives),
            Err(e) => {
                eprintln!("awili: rules injection skipped — memory unavailable: {e}");
                Err(DirectivesError::Offline)
            }
        },
    }
}

/// The v6.5 adapter's recall lane (P4b): this turn's past context.
///
/// On the v6.5 backend the TYPED `recall` verb answers with the
/// relevance-ranked `items` lane, mapped to past-context entries (server
/// ranking preserved). A blank query recalls nothing — no request at all
/// (matches the v5 fallback's gate). On the v5 backend the historical
/// `notes` + `facts` buckets stay — the explicit backend fallback.
pub(crate) fn fetch_recall_from_client(
    client: &MemoryClient,
    query: &str,
) -> Option<Vec<RecallEntry>> {
    if query.trim().is_empty() {
        return Some(Vec::new());
    }
    match client.backend() {
        MemoryBackend::V65 => match v65::recall(client, query) {
            Ok(rec) => Some(recall_from_v65(&rec.items, 5)),
            Err(e) => {
                eprintln!("awili: recall skipped — memory unavailable: {e}");
                None
            }
        },
        MemoryBackend::V5 => match client.recall_past_v5(query, 5) {
            Ok(entries) => Some(entries),
            Err(e) => {
                eprintln!("awili: recall skipped — memory unavailable: {e}");
                None
            }
        },
    }
}

/// Builds the memory client for one fetch thread. Production passes
/// [`production_client`]; tests pass a non-capturing closure that pins a
/// v6.5 loopback stub from `mem.base_url`.
pub(crate) type MakeClient = fn(&MemoryCfg) -> MemoryClient;

/// The production client builder: the configured base URL + min priority,
/// plus the explicit v5 token when set (an empty token still falls back to
/// the boot-token file inside the client).
pub(crate) fn production_client(mem: &MemoryCfg) -> MemoryClient {
    MemoryClient::resolved(mem.base_url.clone(), mem.min_priority).with_token(mem.token.clone())
}

/// S5(a): the per-turn memory gate every Work (owned engine) and Chat
/// submit awaits BEFORE any provider request.
pub(crate) struct TurnGate {
    /// `None` when memory is disabled (chip hidden).
    pub(crate) rules: Option<RulesStatus>,
    /// `None` when memory or recall is disabled (chip hidden).
    pub(crate) recall: Option<RecallStatus>,
    /// `Ok(system)`: submit, with `system` as the pinned preamble (`None` =
    /// nothing to pin). `Err(reason)`: the turn MUST NOT reach a provider.
    pub(crate) outcome: Result<Option<String>, String>,
}

/// Run the per-turn memory gate: fetch directives and recall OFF the UI
/// thread (two bounded workers, awaited together), map the fetches to the
/// chip states, and build the ONE validated preamble block
/// ([`awili_memory::try_system_preamble`]) the turn will pin. A rejected
/// policy (malformed/over-bound `mandatory_policy` lane, or a preamble
/// bound error) yields `outcome: Err(reason)` with `rules: Rejected` —
/// the caller must then end the turn WITHOUT any provider request. An
/// unreachable service keeps the historical degrade: `rules: Offline` and
/// `outcome: Ok(None)` (the turn proceeds, the chip explains).
pub(crate) async fn gate_turn_memory(
    mem: MemoryCfg,
    query: String,
    make_client: MakeClient,
) -> TurnGate {
    gate_turn_memory_with(mem, query, make_client, usize::MAX).await
}

/// RC2-R1F7 (usability RC2 round 1, N5): the in-app engine's recalled notes
/// are cut at this many bytes each (else 1,500). A local model reads every
/// recalled word again on every turn; on a small local model, five full
/// notes (about 2,100 tokens) took 5.6-7.6 s to read before the first word,
/// five short ones (about 500 tokens) about 2 s. The model can still ask its
/// memory for more (the `recall` tool).
pub(crate) const INAPP_RECALL_NOTE_BYTES: usize = 400;

/// [`gate_turn_memory`] with each recalled note cut at `note_max_bytes`
/// (see [`awili_memory::try_system_preamble_with`]).
pub(crate) async fn gate_turn_memory_with(
    mem: MemoryCfg,
    query: String,
    make_client: MakeClient,
    note_max_bytes: usize,
) -> TurnGate {
    if !mem.enabled {
        return TurnGate {
            rules: None,
            recall: None,
            outcome: Ok(None),
        };
    }
    let (tx, rx) = async_channel::bounded(1);
    let mem_a = mem.clone();
    let query_a = query.clone();
    std::thread::spawn(move || {
        let result = fetch_directives_from_client(&make_client(&mem_a), &query_a);
        let _ = tx.send_blocking(result);
    });
    let (tx2, rx2) = async_channel::bounded(1);
    let mem_b = mem.clone();
    let recall_on = mem.recall;
    std::thread::spawn(move || {
        let result = if recall_on {
            fetch_recall_from_client(&make_client(&mem_b), &query)
        } else {
            Some(Vec::new())
        };
        let _ = tx2.send_blocking(result);
    });
    let directives = match rx.recv().await {
        Ok(r) => r,
        Err(_) => Err(DirectivesError::Offline),
    };
    // A closed channel (a panicked worker) reads as `None` — the historical
    // offline degrade, never a refusal.
    let recalled: Option<Vec<RecallEntry>> = rx2.recv().await.unwrap_or_default();
    let recall = mem.recall.then(|| recall_status(recalled.as_deref()));
    let recalled = recalled.unwrap_or_default();
    let (rules, dirs) = match directives {
        Err(DirectivesError::PolicyRejected(reason)) => {
            return TurnGate {
                rules: Some(RulesStatus::Rejected),
                recall,
                outcome: Err(reason),
            };
        }
        Err(DirectivesError::Offline) => (Some(RulesStatus::Offline), Vec::new()),
        Ok(d) => (Some(RulesStatus::Loaded(d.len())), d),
    };
    match awili_memory::try_system_preamble_with(&dirs, &recalled, None, note_max_bytes) {
        Ok(s) => TurnGate {
            rules,
            recall,
            outcome: Ok(s.filter(|s| !s.trim().is_empty())),
        },
        Err(reason) => TurnGate {
            rules: Some(RulesStatus::Rejected),
            recall,
            outcome: Err(reason),
        },
    }
}

/// S5 INT1: the ONE refusal text every Work executor and Chat show when the
/// memory gate rejects the policy.
pub(crate) fn policy_refusal(reason: &str) -> String {
    format!("{POLICY_REFUSAL_PREFIX}{reason}")
}

/// RC2-UX (D-R1 S1 badge): how a refused turn's error begins (the Work
/// header then reads "not sent", not "failed": nothing ran).
pub(crate) const POLICY_REFUSAL_PREFIX: &str = "memory policy rejected — turn not sent: ";

/// S5 INT1: pin the gate's validated, ordered policy block into an agent
/// request's `system` slot, AHEAD of any system text the lane already set
/// (e.g. the plan-first directive). RC1-M: Claude and Codex get `system`
/// through their system channel (`agent_launch::launch`); the engines with
/// none get it as the leading `[System context]` block of the prompt
/// (`code_provider::compose_prompt`). `None` or blank pins nothing.
pub(crate) fn pin_policy(request: &mut ChatRequest, policy: Option<&str>) {
    let Some(policy) = policy.filter(|p| !p.trim().is_empty()) else {
        return;
    };
    request.system = Some(
        match request.system.take().filter(|s| !s.trim().is_empty()) {
            Some(lane) => format!("{policy}\n\n{lane}"),
            None => policy.to_string(),
        },
    );
}

/// S5(a): open the Chat turn's provider stream only when the gate allowed
/// the turn. A refused gate never calls `provider.stream`; it yields one
/// `Error` then `Done`, so the caller's loop ends the turn with an error
/// bubble. An accepted gate's policy block is pinned into the request
/// (S5 INT1) unless the owner's own hooks bring his memory
/// (`hooks_provide_memory`, RC1-N-FIX1): then the hooks deliver the rules and
/// Awili adds neither the policy nor recall.
pub(crate) fn open_gated_stream(
    provider: &dyn ChatProvider,
    mut request: ChatRequest,
    outcome: &Result<Option<String>, String>,
) -> BoxStream<'static, ProviderEvent> {
    match outcome {
        Ok(policy) => {
            // RC1-N-FIX1: no memory of Awili's where the backend's own hooks
            // bring the owner's (decided before pinning, never by parsing
            // the rendered preamble back out).
            if !provider.memory_from_hooks() {
                pin_policy(&mut request, policy.as_deref());
            }
            provider.stream(request)
        }
        Err(reason) => futures::stream::iter(vec![
            ProviderEvent::Error(policy_refusal(reason)),
            ProviderEvent::Done,
        ])
        .boxed(),
    }
}

/// Map a recall fetch result to the chip state: an answered service is
/// `Loaded(n)` (even when n is 0), an offline/errored one is `Offline`.
pub(crate) fn recall_status(recalled: Option<&[RecallEntry]>) -> RecallStatus {
    match recalled {
        Some(entries) => RecallStatus::Loaded(entries.len()),
        None => RecallStatus::Offline,
    }
}

/// The `recall:` chip's label, or `None` when the chip is hidden (slice 9 §5):
/// before the first turn there is nothing to report, and `Loaded(0)` — service
/// up, nothing recalled — is the quiet normal case that must not occupy the
/// status row. `Offline` always shows: a degraded service deserves a chip.
pub(crate) fn recall_chip_label(status: Option<&RecallStatus>) -> Option<String> {
    match status {
        None | Some(RecallStatus::Loaded(0)) => None,
        Some(s) => Some(s.chip_label()),
    }
}

/// The `rules:` chip's label, or `None` when the chip is hidden. `None` covers
/// two honest non-degraded states: before the first turn (nothing queried yet)
/// and when memory is disabled — neither is "offline", so neither shows a chip.
/// Unlike recall, a `Loaded(0)` rules turn DOES show ("rules: 0" — the service
/// answered with no standing directives, which is worth stating): the rules chip
/// is a persistent per-turn indicator once the first turn has run, whereas recall
/// stays quiet when it found nothing.
pub(crate) fn rules_chip_label(status: Option<&RulesStatus>) -> Option<String> {
    status.map(RulesStatus::chip_label)
}

// ---- RC2-UX (R5-9): the rules/recall chip belongs to its conversation ----------

/// The `app_state` key of one conversation's last rules/recall counts.
pub(crate) fn memory_chip_key(conv_id: &str) -> String {
    format!("memory_chip:{conv_id}")
}

/// What the rules and recall chips show for one conversation: its own last
/// send (round 5 Minor 9: they showed the last send anywhere, and nothing
/// after a restart).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MemoryChip {
    pub(crate) rules: Option<RulesStatus>,
    pub(crate) recall: Option<RecallStatus>,
}

/// RC2-FIXB3: the saved value of a conversation whose last send left no
/// counts to keep ([`MemoryChip::from_saved`] reads it as no chip).
pub(crate) const NO_SAVED_CHIP: &str = r#"{"rules":null,"recall":null}"#;

impl MemoryChip {
    /// The counts to keep across a relaunch (`{"rules":19,"recall":5}`):
    /// only what the memory service answered; an offline or refused turn is
    /// not kept (it says nothing about the next one).
    pub(crate) fn to_saved(&self) -> Option<String> {
        let rules = match &self.rules {
            Some(RulesStatus::Loaded(n)) => Some(*n),
            _ => None,
        };
        let recall = match &self.recall {
            Some(RecallStatus::Loaded(n)) => Some(*n),
            _ => None,
        };
        (rules.is_some() || recall.is_some())
            .then(|| serde_json::json!({ "rules": rules, "recall": recall }).to_string())
    }

    /// Read back [`Self::to_saved`].
    pub(crate) fn from_saved(saved: &str) -> Self {
        let v: serde_json::Value = serde_json::from_str(saved).unwrap_or_default();
        let n = |k: &str| {
            v.get(k)
                .and_then(serde_json::Value::as_u64)
                .map(|n| n as usize)
        };
        Self {
            rules: n("rules").map(RulesStatus::Loaded),
            recall: n("recall").map(RecallStatus::Loaded),
        }
    }
}

impl crate::AwiliApp {
    /// After a turn's memory gate: the chips it set belong to `conv_id`
    /// (kept, and saved for a relaunch).
    pub(crate) fn note_memory_chip(&mut self, conv_id: &str) {
        let chip = MemoryChip {
            rules: self.rules.clone(),
            recall: self.recall.clone(),
        };
        self.keep_memory_chip(conv_id, chip);
    }

    /// RC2-FIXC1 (RC2-FIXB review P2: a Work send with memory off skipped
    /// the gate and left the older counts saved): a send that runs no memory
    /// gate leaves `conv_id` no counts — none on screen, none after a
    /// relaunch.
    pub(crate) fn clear_memory_chip(&mut self, conv_id: &str) {
        self.keep_memory_chip(conv_id, MemoryChip::default());
    }

    /// `conv_id`'s chips after a send: kept, and saved for a relaunch.
    fn keep_memory_chip(&mut self, conv_id: &str, chip: MemoryChip) {
        // RC2-FIXB3 (RC2-UX review P2: an older count came back after a
        // relaunch): a send with nothing to keep (memory offline, refused or
        // off) clears the saved counts, so a relaunch shows no older ones.
        if let Some(persist) = self.persist.as_ref() {
            let saved = chip.to_saved().unwrap_or_else(|| NO_SAVED_CHIP.to_string());
            persist.set_state(&memory_chip_key(conv_id), saved);
        }
        self.memory_chips.insert(conv_id.to_string(), chip);
    }

    /// The chips of the conversation on screen (nothing before its first
    /// send).
    pub(crate) fn memory_chip_here(&self) -> MemoryChip {
        self.memory_chips
            .get(self.sidebar.active_id())
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod rules_chip_tests {
    use super::*;

    #[test]
    fn rules_chip_hidden_before_first_turn_and_when_disabled() {
        // The reported nit: a fresh conversation (nothing queried yet) and a
        // memory-disabled session both hold `None`, so the chip is hidden — never
        // the misleading red "rules: offline" for a service that never answered.
        assert_eq!(rules_chip_label(None), None);
    }

    #[test]
    fn rules_chip_shows_offline_only_for_a_genuine_unreachable_turn() {
        // An enabled turn whose directive fetch errored is a real degrade — the
        // warning chip must show so the skipped injection is visible (no silent
        // compaction). This is the ONLY path to `Some(Offline)`.
        assert_eq!(
            rules_chip_label(Some(&RulesStatus::Offline)).as_deref(),
            Some("Rules: memory offline")
        );
    }

    #[test]
    fn rules_chip_shows_the_count_including_zero() {
        // Unlike recall (which stays quiet on `Loaded(0)`), the rules chip is a
        // persistent per-turn indicator: an answered service with zero directives
        // still shows "rules: 0" so the state is never ambiguous with "hidden".
        assert_eq!(
            rules_chip_label(Some(&RulesStatus::Loaded(0))).as_deref(),
            Some("0 standing rules")
        );
        assert_eq!(
            rules_chip_label(Some(&RulesStatus::Loaded(3))).as_deref(),
            Some("3 standing rules")
        );
    }
}

#[cfg(test)]
mod p4b_routing_tests {
    //! P4b lane routing: v6.5 takes the typed `bootstrap`/`recall` verbs, v5
    //! stays on `/v5/recall` — the explicit backend fallback. Hermetic: a
    //! loopback stub (127.0.0.1:0) and `with_v65` pinning.

    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// One v6.5 envelope: the mandatory-policy lane, then the items lane.
    const V65_FIXTURE: &str = r#"{"mandatory_policy":[{"id":"p1","kind":"directive","subject":"policy","authority":"owner_instruction","object":"NEVER shut down the fleet"}],"items":[{"id":"i1","subject":"decision","object":{"content":"we ship P4b","importance":0.9}}]}"#;
    /// The historical v5 recall shape (`rule` alias on the constraint row).
    const V5_FIXTURE: &str = r#"{"constraints":[{"id":1,"rule":"one app one database","priority":100}],"directives":[],"facts":[],"notes":[{"id":2,"content":"a note","importance":0.5}],"reflections":[]}"#;

    /// Per-route request counters (the routing proof).
    #[derive(Default)]
    struct Counts {
        bootstrap: Mutex<u32>,
        recall_v6: Mutex<u32>,
        recall_v5: Mutex<u32>,
    }

    fn spawn_stub(counts: Arc<Counts>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let counts = Arc::clone(&counts);
                thread::spawn(move || {
                    let mut data = Vec::new();
                    let mut buf = [0u8; 4096];
                    while data.windows(4).all(|w| w != b"\r\n\r\n") {
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        data.extend_from_slice(&buf[..n]);
                    }
                    let req = String::from_utf8_lossy(&data);
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let (counter, body) = match path.as_str() {
                        "/v6/bootstrap" => (&counts.bootstrap, V65_FIXTURE),
                        "/v6/recall" => (&counts.recall_v6, V65_FIXTURE),
                        "/v5/recall" => (&counts.recall_v5, V5_FIXTURE),
                        _ => return, // unknown route: silence is the refusal
                    };
                    *counter.lock().unwrap() += 1;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        format!("http://{addr}")
    }

    fn v65_client(base: &str) -> MemoryClient {
        MemoryClient::new(base, 90).with_v65(base, Some("reader"), Some("writer"))
    }

    /// RC1-M-FIX1: the S5 rules come from ONE recall that asks for the
    /// policy lane — not `/v6/bootstrap`, whose live answer no longer has a
    /// `mandatory_policy` lane (2026-10-03) and which took 6 s.
    #[test]
    fn v65_boot_uses_the_recall_verb_policy_lane_only() {
        let counts = Arc::new(Counts::default());
        let client = v65_client(&spawn_stub(Arc::clone(&counts)));
        let dirs = fetch_directives_from_client(&client, "boot").unwrap();
        assert_eq!(dirs.len(), 1, "policy lane maps only");
        assert_eq!(dirs[0].text, "NEVER shut down the fleet");
        assert_eq!(*counts.recall_v6.lock().unwrap(), 1, "one policy recall");
        assert_eq!(*counts.bootstrap.lock().unwrap(), 0, "no bootstrap");
        assert_eq!(*counts.recall_v5.lock().unwrap(), 0, "no v5 at v6.5 boot");
    }

    #[test]
    fn v65_recall_uses_the_recall_verb_items_lane_only() {
        let counts = Arc::new(Counts::default());
        let client = v65_client(&spawn_stub(Arc::clone(&counts)));
        let entries = fetch_recall_from_client(&client, "what did we decide").unwrap();
        assert_eq!(entries.len(), 1, "items lane only");
        assert_eq!(entries[0].text, "we ship P4b");
        assert_eq!(*counts.recall_v6.lock().unwrap(), 1, "recall answers query");
        assert_eq!(*counts.bootstrap.lock().unwrap(), 0, "no bootstrap");
        assert_eq!(*counts.recall_v5.lock().unwrap(), 0, "no v5 at v6.5 recall");
    }

    #[test]
    fn blank_query_recalls_nothing_without_any_request() {
        let counts = Arc::new(Counts::default());
        let client = v65_client(&spawn_stub(Arc::clone(&counts)));
        assert_eq!(fetch_recall_from_client(&client, "   "), Some(Vec::new()));
        let c = &*counts;
        assert_eq!(
            *c.bootstrap.lock().unwrap()
                + *c.recall_v6.lock().unwrap()
                + *c.recall_v5.lock().unwrap(),
            0
        );
    }

    #[test]
    fn v5_backend_keeps_the_v5_path_for_both_lanes() {
        let counts = Arc::new(Counts::default());
        // `new` stays v5-pure: the explicit fallback, unchanged from before.
        let client = MemoryClient::new(spawn_stub(Arc::clone(&counts)), 90).with_token("v5-token");
        let dirs = fetch_directives_from_client(&client, "q").unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].text, "one app one database");
        let entries = fetch_recall_from_client(&client, "q").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "a note");
        assert_eq!(*counts.recall_v5.lock().unwrap(), 2, "both lanes ride v5");
        assert_eq!(*counts.bootstrap.lock().unwrap(), 0, "no v6.5 verbs on v5");
        assert_eq!(*counts.recall_v6.lock().unwrap(), 0);
    }
}

#[cfg(test)]
mod rc2ux_chip_tests {
    use super::*;
    use gpui::AppContext;

    const CHILD: &str = "AWILI_RC2UX_CHIP_TEST_HOME";

    fn isolated(test: &str) -> bool {
        if std::env::var_os(CHILD).is_some() {
            return true;
        }
        let home = tempfile::tempdir().unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, home.path())
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
            "isolated test failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        false
    }

    /// What is kept across a relaunch: only answered counts.
    #[test]
    fn rc2ux_a_chip_keeps_its_counts() {
        let chip = MemoryChip {
            rules: Some(RulesStatus::Loaded(19)),
            recall: Some(RecallStatus::Loaded(5)),
        };
        let saved = chip.to_saved().unwrap();
        assert_eq!(MemoryChip::from_saved(&saved), chip);
        let offline = MemoryChip {
            rules: Some(RulesStatus::Offline),
            recall: Some(RecallStatus::Offline),
        };
        assert_eq!(offline.to_saved(), None);
        assert_eq!(MemoryChip::from_saved("not json"), MemoryChip::default());
    }

    /// RC2-UX (R5-9, round 5 Minor 9 / round 6 Minor 5): the chip line shows
    /// the rules and notes of the conversation on screen — another
    /// conversation shows its own (none before its first send) — and a
    /// reopened conversation shows them again after a relaunch.
    #[gpui::test]
    fn rc2ux_the_chips_belong_to_their_conversation(cx: &mut gpui::TestAppContext) {
        if !isolated("recall::rc2ux_chip_tests::rc2ux_the_chips_belong_to_their_conversation") {
            return;
        }
        cx.executor().allow_parking();
        let texts = |app: &crate::AwiliApp| -> Vec<String> {
            app.collect_status_parts()
                .into_iter()
                .map(|(t, _)| t)
                .collect()
        };
        let entity = cx.new(crate::AwiliApp::new);
        let (a, b) = entity.update(cx, |app, _| {
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.mode = crate::work::Mode::Work;
            let a = app
                .conversations
                .create_kind(1, awili_core::ConvKind::Work)
                .id;
            let b = app
                .conversations
                .create_kind(2, awili_core::ConvKind::Work)
                .id;
            if let Some(p) = &app.persist {
                p.send(crate::persistence::PersistCmd::CreateConversation {
                    id: a.clone(),
                    title: "a".into(),
                    created_ts: 1,
                    kind: "work".into(),
                });
            }
            app.set_active_conversation(&a);
            app.rules = Some(RulesStatus::Loaded(19));
            app.recall = Some(RecallStatus::Loaded(5));
            app.note_memory_chip(&a);
            (a, b)
        });
        entity.update(cx, |app, _| {
            let shown = texts(app);
            assert!(
                shown.contains(&"19 standing rules".to_string()),
                "{shown:?}"
            );
            assert!(shown.contains(&"5 notes recalled".to_string()), "{shown:?}");
            app.set_active_conversation(&b);
            let shown = texts(app);
            assert!(
                !shown
                    .iter()
                    .any(|t| t.contains("standing rules") || t.contains("recalled")),
                "B has not sent yet: {shown:?}"
            );
        });
        let store = awili_persist::Store::open_default().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while store.get_app_state(&memory_chip_key(&a)).unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline, "never saved");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let relaunched = cx.new(crate::AwiliApp::new);
        relaunched.update(cx, |app, _| {
            app.set_active_conversation(&a);
            let shown = texts(app);
            assert!(
                shown.contains(&"19 standing rules".to_string()),
                "{shown:?}"
            );
            assert!(shown.contains(&"5 notes recalled".to_string()), "{shown:?}");
        });
    }

    /// RC2-FIXB3 (RC2-UX review P2: an older count came back after an
    /// offline send): a conversation whose last send found memory offline
    /// shows no older counts after a relaunch — the counts its earlier send
    /// saved are cleared, not left to come back as its last result.
    #[gpui::test]
    fn rc2fixb_an_offline_send_leaves_no_older_counts_after_a_relaunch(
        cx: &mut gpui::TestAppContext,
    ) {
        if !isolated(
            "recall::rc2ux_chip_tests::rc2fixb_an_offline_send_leaves_no_older_counts_after_a_relaunch",
        ) {
            return;
        }
        cx.executor().allow_parking();
        let texts = |app: &crate::AwiliApp| -> Vec<String> {
            app.collect_status_parts()
                .into_iter()
                .map(|(t, _)| t)
                .collect()
        };
        let counts = |shown: &[String]| {
            shown
                .iter()
                .any(|t| t.contains("standing rule") || t.contains("recalled"))
        };
        let entity = cx.new(crate::AwiliApp::new);
        let a = entity.update(cx, |app, _| {
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.mode = crate::work::Mode::Work;
            let a = app
                .conversations
                .create_kind(1, awili_core::ConvKind::Work)
                .id;
            if let Some(p) = &app.persist {
                p.send(crate::persistence::PersistCmd::CreateConversation {
                    id: a.clone(),
                    title: "a".into(),
                    created_ts: 1,
                    kind: "work".into(),
                });
            }
            app.set_active_conversation(&a);
            // A send with memory up…
            app.rules = Some(RulesStatus::Loaded(19));
            app.recall = Some(RecallStatus::Loaded(5));
            app.note_memory_chip(&a);
            // …then a send with memory offline.
            app.rules = Some(RulesStatus::Offline);
            app.recall = Some(RecallStatus::Offline);
            app.note_memory_chip(&a);
            let shown = texts(app);
            assert!(!counts(&shown), "the offline send shows: {shown:?}");
            a
        });
        let store = awili_persist::Store::open_default().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while store
            .get_app_state(&memory_chip_key(&a))
            .unwrap()
            .as_deref()
            != Some(NO_SAVED_CHIP)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the older counts were never cleared: {:?}",
                store.get_app_state(&memory_chip_key(&a)).unwrap()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let relaunched = cx.new(crate::AwiliApp::new);
        relaunched.update(cx, |app, _| {
            app.set_active_conversation(&a);
            let shown = texts(app);
            assert!(!counts(&shown), "no older counts: {shown:?}");
        });
    }
}
