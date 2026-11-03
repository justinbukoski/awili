//! Needs You — the app side of the attention feed (slice-4 spec, Part B).
//!
//! Network and sqlite live on ONE dedicated worker thread (the same posture as
//! the persistence worker, SF6): it owns the [`FeedMirror`] and a blocking
//! [`FeedClient`], polls the central service by cursor, and pushes immutable
//! [`FeedSnapshot`]s to the GPUI thread over a channel. The UI only ever reads
//! the latest snapshot and sends commands (ack, sync-now) down an mpsc — an
//! unreachable HOSTC can never stall a frame.
//!
//! The first snapshot is emitted from the mirror BEFORE any network IO, so the
//! panel renders instantly on launch and keeps rendering offline; the snapshot
//! carries `online` + `last_sync_ts` so the panel shows an honest sync age
//! instead of lying about freshness.

use std::sync::mpsc;
use std::time::Duration;

use awili_feed::{generate_event_id, FeedClient, FeedMirror, NeedsItem, NewEvent, Severity};

/// How often the worker re-syncs against the service (cursor polling, no
/// websockets in v1). Each pass is two cheap GETs against a LAN host.
const POLL_INTERVAL: Duration = Duration::from_secs(20);

/// The feed connection, in cheap-to-clone form (mirrors `MemoryCfg`).
#[derive(Clone)]
pub struct FeedCfg {
    pub base_url: String,
    /// `AWILI_FEED_TOKEN` — env-only by spec §8; `None` means every authed call
    /// will 401 (we warn at startup but still run: the panel degrades to the
    /// mirror's last contents, which is exactly the offline behaviour).
    pub token: Option<String>,
}

impl FeedCfg {
    /// Build from config + environment. `None` when `[feed] base_url` is empty
    /// or absent — the feed is disabled outright: no panel, no polling.
    pub fn load(config: &awili_config::Config) -> Option<FeedCfg> {
        let base_url = config.feed.base_url.trim();
        if base_url.is_empty() {
            return None;
        }
        let token = std::env::var("AWILI_FEED_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());
        if token.is_none() {
            eprintln!(
                "awili: [feed] is configured but AWILI_FEED_TOKEN is unset — the service will refuse us (401)"
            );
        }
        Some(FeedCfg {
            base_url: base_url.to_string(),
            token,
        })
    }
}

/// What the panel renders: an immutable copy of the mirror's Needs You state.
#[derive(Clone, Default)]
pub struct FeedSnapshot {
    /// Unacked attention+critical, collapsed by dedup_key, newest first.
    pub items: Vec<NeedsItem>,
    /// Unacked attention+critical EVENTS (not groups) — the chip badge.
    pub badge: usize,
    /// Unix seconds of the last successful sync (`None` = never).
    pub last_sync_ts: Option<i64>,
    /// Whether the most recent sync attempt reached the service.
    pub online: bool,
    /// RC2-R4 (RC-05): why the last sync failed, when the service answered
    /// (it has no feed, or it refused Awili) — said plainly instead of
    /// "offline". `None` when it synced or could not be reached.
    pub problem: Option<String>,
}

/// RC2-R4 (RC-05): the plain reason for a failed sync the service answered.
/// A 404 means the address answers but has no feed (the feed service is not
/// set up there — the state on hostb for RC1); 401/403 mean it refused the
/// token. Unreachable or anything else stays "offline" (`None`).
pub fn sync_problem(err: &awili_feed::FeedError, base_url: &str) -> Option<String> {
    match err {
        awili_feed::FeedError::Status(404) => Some(format!(
            "No feed service at {base_url}: it is not set up there. Turn the feed off in Settings (empty [feed] base_url)."
        )),
        awili_feed::FeedError::Status(401 | 403) => Some(format!(
            "The feed service at {base_url} refused Awili: AWILI_FEED_TOKEN is missing or wrong."
        )),
        _ => None,
    }
}

/// RC2-R4 (RC-05): the Needs You panel's sync line and whether it is a
/// warning.
pub fn sync_line(snapshot: &FeedSnapshot, now_ts: i64) -> (String, bool) {
    match (snapshot.online, &snapshot.problem, snapshot.last_sync_ts) {
        (true, _, _) => ("live".into(), false),
        (false, Some(p), _) => (p.clone(), true),
        (false, None, Some(ts)) => (
            format!("offline — last sync {} ago", age_label(now_ts, ts)),
            true,
        ),
        (false, None, None) => ("offline — never synced".into(), true),
    }
}

/// Commands the UI sends the worker.
enum FeedCmd {
    /// Sync immediately (the panel was just opened) instead of waiting out the
    /// poll interval.
    SyncNow,
    /// Ack these event_ids against the service; the mirror is updated only on
    /// success (spec §6) and the next snapshot drops the rows.
    Ack(Vec<String>),
}

/// The UI's handle to the feed worker. Sends never block; a dead worker
/// (mirror unavailable) just means commands go nowhere.
pub struct FeedHandle {
    tx: mpsc::Sender<FeedCmd>,
}

impl FeedHandle {
    pub fn sync_now(&self) {
        let _ = self.tx.send(FeedCmd::SyncNow);
    }

    pub fn ack(&self, event_ids: Vec<String>) {
        let _ = self.tx.send(FeedCmd::Ack(event_ids));
    }
}

/// Spawn the feed worker. The returned receiver yields a [`FeedSnapshot`]
/// after every state change; drain it from a GPUI task and repaint.
pub fn start(cfg: FeedCfg) -> (FeedHandle, async_channel::Receiver<FeedSnapshot>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<FeedCmd>();
    let (snap_tx, snap_rx) = async_channel::unbounded::<FeedSnapshot>();
    std::thread::spawn(move || worker(cfg, cmd_rx, snap_tx));
    (FeedHandle { tx: cmd_tx }, snap_rx)
}

/// The worker loop: emit the mirror's current state, then alternate
/// sync-and-emit with waiting for a command or the poll tick.
fn worker(cfg: FeedCfg, rx: mpsc::Receiver<FeedCmd>, tx: async_channel::Sender<FeedSnapshot>) {
    let mut mirror = match FeedMirror::open_default() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("awili: feed mirror unavailable ({e}) — Needs You will stay empty");
            return;
        }
    };
    let base_url = cfg.base_url.clone();
    let client = FeedClient::new(cfg.base_url, cfg.token);
    let mut problem: Option<String>;

    // Instant start: last sync's events render before any network IO happens.
    let mut online = false;
    if !emit(&mirror, online, None, &tx) {
        return;
    }
    loop {
        match mirror.sync(&client, unix_now()) {
            Ok(_) => {
                online = true;
                problem = None;
            }
            Err(e) => {
                problem = sync_problem(&e, &base_url);
                if online {
                    // Log the transition, not every failed 20s retry.
                    eprintln!("awili: feed sync failed (panel falls back to mirror): {e}");
                }
                online = false;
            }
        }
        if !emit(&mirror, online, problem.clone(), &tx) {
            return;
        }
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(FeedCmd::SyncNow) => {}
            Ok(FeedCmd::Ack(ids)) => {
                for id in &ids {
                    match client.ack(id) {
                        // Mirror updated ON SUCCESS only — a failed ack keeps
                        // the row (honest: the service still considers it open).
                        Ok(receipt) => {
                            let _ = mirror.mark_acked(id, receipt.acked_ts);
                        }
                        Err(e) => {
                            eprintln!("awili: ack {id} failed: {e}");
                            online = false;
                            break;
                        }
                    }
                }
                if !emit(&mirror, online, problem.clone(), &tx) {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Send the mirror's current state to the UI. `false` when the UI side is gone
/// (window closed) — the worker should exit.
fn emit(
    mirror: &FeedMirror,
    online: bool,
    problem: Option<String>,
    tx: &async_channel::Sender<FeedSnapshot>,
) -> bool {
    let snapshot = FeedSnapshot {
        items: mirror.needs_you_local().unwrap_or_default(),
        badge: mirror.unacked_count().unwrap_or(0),
        last_sync_ts: mirror.last_sync_ts(),
        online,
        problem,
    };
    tx.send_blocking(snapshot).is_ok()
}

/// Post one of Awili's OWN events (spec §7) — fire-and-forget on a short-lived
/// thread, exactly like the chat-log path: a feed outage must never affect the
/// turn that triggered the event.
pub fn post_event(
    cfg: &FeedCfg,
    severity: Severity,
    title: String,
    body: String,
    dedup_key: Option<String>,
) {
    let cfg = cfg.clone();
    std::thread::spawn(move || {
        let client = FeedClient::new(cfg.base_url, cfg.token);
        let event = NewEvent {
            event_id: generate_event_id("awili"),
            source: "awili".to_string(),
            severity,
            title,
            body,
            dedup_key,
            created_ts: Some(unix_now()),
        };
        if let Err(e) = client.post_event(&event) {
            eprintln!("awili: feed self-event dropped ({}): {e}", event.title);
        }
    });
}

/// Unix seconds now.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Compact age for a row / the sync line: "42s", "3m", "5h", "2d".
pub fn age_label(now_ts: i64, ts: i64) -> String {
    let d = (now_ts - ts).max(0);
    match d {
        0..=59 => format!("{d}s"),
        60..=3599 => format!("{}m", d / 60),
        3600..=86_399 => format!("{}h", d / 3600),
        _ => format!("{}d", d / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_label_buckets() {
        assert_eq!(age_label(1000, 1000), "0s");
        assert_eq!(age_label(1059, 1000), "59s");
        assert_eq!(age_label(1060, 1000), "1m");
        assert_eq!(age_label(1000 + 59 * 60, 1000), "59m");
        assert_eq!(age_label(1000 + 3600, 1000), "1h");
        assert_eq!(age_label(1000 + 23 * 3600, 1000), "23h");
        assert_eq!(age_label(1000 + 86_400, 1000), "1d");
        assert_eq!(age_label(1000 + 10 * 86_400, 1000), "10d");
        // A clock that runs backwards (NTP step) must not render negative ages.
        assert_eq!(age_label(1000, 2000), "0s");
    }

    /// Pin the EXACT upper edge of the minute and hour buckets. The bucket
    /// boundaries are inclusive ranges (`60..=3599`, `3600..=86_399`); the sibling
    /// `age_label_buckets` test pins each bucket's LOWER edge (1m at 60, 1h at
    /// 3600, 1d at 86_400) and a mid-range value (59m at 3540, 23h at 82_800) but
    /// never the last second the bucket owns. So a mutation that shrinks a range's
    /// upper bound (e.g. `60..=3599` -> `60..=3598`) drops that final second
    /// through to the `_ => "{d}d"` arm — rendering a 59-minute-old feed row as
    /// "0d" — while every existing assertion stays green. These two inputs are the
    /// sole values that catch such a shrink.
    #[test]
    fn age_label_pins_upper_edge_of_minute_and_hour_buckets() {
        // 3599s = the last second before rolling to 1h; must still read "59m".
        assert_eq!(age_label(1000 + 3599, 1000), "59m");
        // 86_399s = the last second before rolling to 1d; must still read "23h".
        assert_eq!(age_label(1000 + 86_399, 1000), "23h");
    }

    #[test]
    fn feed_cfg_disabled_when_base_url_empty() {
        let config = awili_config::Config::parse("[feed]\nbase_url = \"\"").unwrap();
        assert!(FeedCfg::load(&config).is_none());
        let config = awili_config::Config::parse("").unwrap();
        assert!(FeedCfg::load(&config).is_none());
        let config = awili_config::Config::parse("[feed]\nbase_url = \"http://h:8787\"").unwrap();
        assert!(FeedCfg::load(&config).is_some());
    }

    #[test]
    fn feed_cfg_trims_url_and_reads_token() {
        // Guard the primary activation path (previously only `is_some()` was
        // asserted): a configured base_url is stored TRIMMED, a whitespace-only
        // base_url disables the feed via `.trim()`, and AWILI_FEED_TOKEN drives
        // `token` (Some when set, None when unset) — the credential contract the
        // panel relies on to know whether authed calls will 401.
        let prior = std::env::var("AWILI_FEED_TOKEN").ok();

        // Whitespace-only base_url trims to empty -> feed disabled outright.
        let ws = awili_config::Config::parse("[feed]\nbase_url = \"   \"").unwrap();
        assert!(FeedCfg::load(&ws).is_none());

        // Non-empty (padded) base_url + token set -> Some, url stored trimmed,
        // token carried through verbatim.
        std::env::set_var("AWILI_FEED_TOKEN", "tok-abc");
        let cfg = awili_config::Config::parse("[feed]\nbase_url = \"  http://h:8787  \"").unwrap();
        let loaded = FeedCfg::load(&cfg).expect("configured feed must load");
        assert_eq!(loaded.base_url, "http://h:8787");
        assert_eq!(loaded.token.as_deref(), Some("tok-abc"));

        // Same config, token unset -> Some but token None (panel warns, still runs).
        std::env::remove_var("AWILI_FEED_TOKEN");
        let loaded = FeedCfg::load(&cfg).expect("feed loads even without a token");
        assert_eq!(loaded.token, None);

        // An empty token is treated as unset (filtered), not carried as Some("").
        std::env::set_var("AWILI_FEED_TOKEN", "");
        assert_eq!(FeedCfg::load(&cfg).unwrap().token, None);

        match prior {
            Some(v) => std::env::set_var("AWILI_FEED_TOKEN", v),
            None => std::env::remove_var("AWILI_FEED_TOKEN"),
        }
    }

    /// RC2-R4 (RC-05): the feed service is not set up on hostb for RC1. An
    /// address that answers 404 (no feed there) or refuses the token is named
    /// plainly in the panel instead of "offline"; an unreachable one stays
    /// "offline".
    #[test]
    fn rc2r4_a_service_without_the_feed_says_it_is_not_set_up() {
        use awili_feed::FeedError;
        let url = "http://10.77.68.86:8787";
        let missing = sync_problem(&FeedError::Status(404), url).unwrap();
        assert!(
            missing.contains("No feed service at http://10.77.68.86:8787"),
            "{missing}"
        );
        assert!(missing.contains("not set up"), "{missing}");
        let refused = sync_problem(&FeedError::Status(401), url).unwrap();
        assert!(refused.contains("AWILI_FEED_TOKEN"), "{refused}");
        assert_eq!(sync_problem(&FeedError::Status(500), url), None);
        assert_eq!(sync_problem(&FeedError::Parse("x".into()), url), None);

        let mut snap = FeedSnapshot {
            problem: Some(missing.clone()),
            ..FeedSnapshot::default()
        };
        assert_eq!(sync_line(&snap, 1_000), (missing, true));
        snap.problem = None;
        assert_eq!(sync_line(&snap, 1_000).0, "offline \u{2014} never synced");
        snap.last_sync_ts = Some(1_000 - 120);
        assert!(sync_line(&snap, 1_000)
            .0
            .starts_with("offline \u{2014} last sync"));
        snap.online = true;
        assert_eq!(sync_line(&snap, 1_000), ("live".to_string(), false));
    }

    /// RC2-R4 (RC-05): the example configuration leaves the feed off (no
    /// panel, no polling) while its service is not set up.
    #[test]
    fn rc2r4_the_example_config_leaves_the_feed_off() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config.example.toml");
        let text = std::fs::read_to_string(path).unwrap();
        let config = awili_config::Config::parse(&text).unwrap();
        assert!(
            config.feed.base_url.trim().is_empty(),
            "{}",
            config.feed.base_url
        );
        assert!(FeedCfg::load(&config).is_none());
    }
}
