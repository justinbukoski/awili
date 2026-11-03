//! The composer status strip, the pure half (docs/v2/strip, DeepSeek's
//! design): what one row under the composer says, and the detail popover
//! behind it, from plain facts. No GPUI, no clock of its own (instants come
//! in), no IO except the two blocking Lemonade reads at the bottom.
//!
//! One line, three facts at most. Left is state, right is budget:
//!
//! - ready, local: a dim dot and the last reply's speed ("41 tok/s");
//! - streaming, local: a breathing dot and the live speed;
//! - cloud: "1.3K in \u{b7} 412 out" after a reply;
//! - unreachable: a red dot, "Hosta isn't responding" and Retry;
//! - not loaded (Lemonade, the model is not resident): a green dot,
//!   "<model> isn't loaded on Hosta" and Retry;
//! - loading: a green dot, "Loading <model>\u{2026}".
//!
//! Nothing is shown until a value exists. Awili never loads a model on a
//! Lemonade host: [`check_residency`] only reads `/api/v1/health`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::gateway_health::Reach;

/// How often an in-app engine's reachability is re-read while it is the
/// conversation's model and the window is visible.
pub(crate) const REACH_EVERY: Duration = Duration::from_secs(15);
/// How often a Lemonade host's stats are re-read, while a turn on it runs or
/// the popover is open.
pub(crate) const HOST_EVERY: Duration = Duration::from_secs(5);
/// The residency read before a send: long enough for a busy LAN host, short
/// enough that a hung one is told apart from a slow one.
const GUARD_TIMEOUT: Duration = Duration::from_millis(2500);
/// A speed needs this much generation time before it is worth showing.
const MIN_SPEED_WINDOW: Duration = Duration::from_millis(250);

// ---- words ------------------------------------------------------------------

/// The strip's idle-error line: "<host> isn't responding".
pub(crate) fn not_responding(host: &str) -> String {
    format!("{host} isn\u{2019}t responding")
}

/// The strip's green line: "<model> isn't loaded on <host>".
pub(crate) fn not_loaded(model: &str, host: &str) -> String {
    format!("{model} isn\u{2019}t loaded on {host}")
}

/// The strip's loading line: "Loading <model>\u{2026}".
pub(crate) fn loading(model: &str) -> String {
    format!("Loading {model}\u{2026}")
}

/// The error card for a turn that was not sent because a Lemonade host does
/// not have the model resident.
pub(crate) fn not_loaded_card(model: &str, host: &str) -> String {
    format!(
        "{model} isn\u{2019}t loaded on {host}, so nothing was sent. Awili doesn\u{2019}t load \
         models on that host (loading on demand has hung it before). Load the model on {host} \
         yourself, then send again."
    )
}

/// The error card for a Lemonade host that did not answer its health read.
pub(crate) fn host_silent_card(host: &str) -> String {
    format!(
        "{host} didn\u{2019}t answer in time, so nothing was sent. Awili doesn\u{2019}t send to \
         a host it can\u{2019}t see is ready. Try again in a moment."
    )
}

/// "just now" / "6 s ago" / "2 min ago" / "3 h ago".
pub(crate) fn age_text(age: Duration) -> String {
    let s = age.as_secs();
    if s < 5 {
        "just now".to_string()
    } else if s < 60 {
        format!("{s} s ago")
    } else if s < 3600 {
        format!("{} min ago", s / 60)
    } else {
        format!("{} h ago", s / 3600)
    }
}

/// A token count the way the strip says it: "412", "1.3K", "18.7K", "262K".
pub(crate) fn tokens_short(n: u64) -> String {
    awili_core::limits::compact(u32::try_from(n).unwrap_or(u32::MAX))
}

/// "41" on the strip: whole tokens per second.
pub(crate) fn speed_short(tps: f64) -> String {
    format!("{:.0}", tps)
}

/// "47.2" in the popover.
pub(crate) fn speed_long(tps: f64) -> String {
    format!("{tps:.1}")
}

/// "0.38" seconds for the first token.
pub(crate) fn first_token_text(secs: f64) -> String {
    format!("{secs:.2}")
}

// ---- segments ---------------------------------------------------------------

/// How one run of strip text is typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Weight {
    /// A value: medium weight, secondary gray (primary while it is live).
    Value,
    /// A unit ("tok/s", "in"): regular, dim.
    Unit,
    /// The word "of" and the middle dot: regular, dimmer still.
    Of,
    /// The window ("262K"): medium, dim.
    Window,
    /// A sentence ("Loading Qwen\u{2026}").
    Plain,
}

/// One run of strip text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seg {
    pub(crate) text: String,
    pub(crate) weight: Weight,
}

fn seg(text: impl Into<String>, weight: Weight) -> Seg {
    Seg {
        text: text.into(),
        weight,
    }
}

fn speed_segs(tps: f64) -> Vec<Seg> {
    vec![
        seg(speed_short(tps), Weight::Value),
        seg("tok/s", Weight::Unit),
    ]
}

fn io_segs(input: u64, output: u64) -> Vec<Seg> {
    vec![
        seg(tokens_short(input), Weight::Value),
        seg("in", Weight::Unit),
        seg("\u{b7}", Weight::Of),
        seg(tokens_short(output), Weight::Value),
        seg("out", Weight::Unit),
    ]
}

/// "18.7K of 262K" as segments, from the composer's own text.
pub(crate) fn context_segs(text: &str) -> Vec<Seg> {
    let mut words = text.split_whitespace();
    let used = words.next().unwrap_or_default().to_string();
    let rest: Vec<&str> = words.collect();
    let mut out = vec![seg(used, Weight::Value)];
    for (ix, word) in rest.iter().enumerate() {
        let weight = if ix + 1 == rest.len() {
            Weight::Window
        } else {
            Weight::Of
        };
        out.push(seg(*word, weight));
    }
    out
}

// ---- telemetry ----------------------------------------------------------------

/// What the last finished reply measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ReplyStats {
    /// Generated tokens per second; none when the reply was too short to say.
    pub(crate) tokens_per_s: Option<f64>,
    /// Seconds from sending to the first token.
    pub(crate) first_token_s: Option<f64>,
    /// Prompt tokens, as the engine reported them.
    pub(crate) input: Option<u64>,
    /// Generated tokens (reported, else counted from the stream).
    pub(crate) output: Option<u64>,
}

/// A reply in flight.
#[derive(Clone, Copy, Debug)]
struct Live {
    started: Option<Instant>,
    first_token: Option<Instant>,
    last_token: Option<Instant>,
    chunks: u64,
    input: u64,
    output: u64,
}

/// What the strip needs to know about a reply in flight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LiveSnapshot {
    /// Tokens per second so far, once there is enough to say it.
    pub(crate) tokens_per_s: Option<f64>,
    /// Whether the first token has arrived.
    pub(crate) has_first_token: bool,
    /// Time since the turn was sent.
    pub(crate) waiting: Duration,
}

/// Per-conversation reply measurements, from each turn's own events.
#[derive(Debug, Default)]
pub(crate) struct Telemetry {
    convs: HashMap<String, Conv>,
}

#[derive(Debug, Default)]
struct Conv {
    live: Option<Live>,
    last: Option<ReplyStats>,
}

impl Telemetry {
    fn live_mut(&mut self, conv: &str, started: Option<Instant>) -> &mut Live {
        self.convs
            .entry(conv.to_string())
            .or_default()
            .live
            .get_or_insert(Live {
                started,
                first_token: None,
                last_token: None,
                chunks: 0,
                input: 0,
                output: 0,
            })
    }

    /// A turn was sent (the app derives the same from the run clock; tests
    /// start one directly).
    #[cfg(test)]
    pub(crate) fn turn_started(&mut self, conv: &str, started: Instant) {
        let c = self.convs.entry(conv.to_string()).or_default();
        if c.live.is_none() {
            c.live = Some(Live {
                started: Some(started),
                first_token: None,
                last_token: None,
                chunks: 0,
                input: 0,
                output: 0,
            });
        }
    }

    /// A chunk of the reply arrived.
    pub(crate) fn token(&mut self, conv: &str, started: Option<Instant>, now: Instant) {
        let live = self.live_mut(conv, started);
        live.first_token.get_or_insert(now);
        live.last_token = Some(now);
        live.chunks += 1;
    }

    /// The engine reported a request's token counts (a turn may make several
    /// requests; they add up).
    pub(crate) fn usage(&mut self, conv: &str, started: Option<Instant>, input: u64, output: u64) {
        let live = self.live_mut(conv, started);
        live.input += input;
        live.output += output;
    }

    /// The turn ended. A reply that produced nothing (a failure) leaves the
    /// previous reply's numbers alone.
    pub(crate) fn turn_ended(&mut self, conv: &str) {
        let Some(c) = self.convs.get_mut(conv) else {
            return;
        };
        let Some(live) = c.live.take() else {
            return;
        };
        if live.chunks == 0 && live.output == 0 {
            return;
        }
        let output = if live.output > 0 {
            live.output
        } else {
            live.chunks
        };
        let speed = match (live.first_token, live.last_token) {
            (Some(first), Some(last)) if last.duration_since(first) >= MIN_SPEED_WINDOW => {
                let secs = last.duration_since(first).as_secs_f64();
                // The tokens after the first one were generated in `secs`.
                Some(output.saturating_sub(1) as f64 / secs).filter(|v| *v > 0.0)
            }
            _ => None,
        };
        c.last = Some(ReplyStats {
            tokens_per_s: speed,
            first_token_s: live
                .first_token
                .zip(live.started)
                .map(|(t, s)| t.duration_since(s).as_secs_f64()),
            input: Some(live.input).filter(|n| *n > 0),
            output: Some(output),
        });
    }

    /// A reply in flight for `conv`, if one is.
    pub(crate) fn live(&self, conv: &str, now: Instant) -> Option<LiveSnapshot> {
        let live = self.convs.get(conv)?.live.as_ref()?;
        let tokens_per_s = match (live.first_token, live.last_token) {
            (Some(first), Some(last))
                if live.chunks >= 3 && last.duration_since(first) >= MIN_SPEED_WINDOW =>
            {
                Some((live.chunks - 1) as f64 / last.duration_since(first).as_secs_f64())
            }
            _ => None,
        };
        Some(LiveSnapshot {
            tokens_per_s,
            has_first_token: live.first_token.is_some(),
            waiting: live
                .started
                .map(|s| now.saturating_duration_since(s))
                .unwrap_or_default(),
        })
    }

    /// The conversation's last finished reply.
    pub(crate) fn last(&self, conv: &str) -> Option<ReplyStats> {
        self.convs.get(conv)?.last
    }

    /// Forget a conversation (deleted).
    #[allow(dead_code, reason = "called when a conversation is removed")]
    pub(crate) fn forget(&mut self, conv: &str) {
        self.convs.remove(conv);
    }
}

// ---- what a Lemonade host says ---------------------------------------------------

/// The host's own load, from `GET /api/v1/system-stats` (shape per Lemonade's
/// status bar: `cpu_percent`, `memory_gb`, `gpu_percent`, `vram_gb`,
/// `npu_percent`). Memory is unified on the box the strip is built for, so
/// the processor and VRAM figures are left out.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct HostStats {
    pub(crate) gpu_percent: Option<f64>,
    pub(crate) npu_percent: Option<f64>,
    pub(crate) memory_used_gb: Option<f64>,
    /// Only when the server reports a total.
    pub(crate) memory_total_gb: Option<f64>,
}

impl HostStats {
    /// Whether the server reported anything worth a block.
    pub(crate) fn any(&self) -> bool {
        self.gpu_percent.is_some() || self.npu_percent.is_some() || self.memory_used_gb.is_some()
    }
}

/// Parse a `/api/v1/system-stats` body. A zero memory reading is the status
/// bar's "unknown" and is dropped.
pub(crate) fn parse_system_stats(body: &str) -> Option<HostStats> {
    let v: Value = serde_json::from_str(body).ok()?;
    let obj = v.as_object()?;
    let num = |k: &str| obj.get(k).and_then(Value::as_f64);
    let stats = HostStats {
        gpu_percent: num("gpu_percent"),
        npu_percent: num("npu_percent"),
        memory_used_gb: num("memory_gb").filter(|g| *g > 0.0),
        memory_total_gb: num("memory_total_gb")
            .or_else(|| num("total_memory_gb"))
            .filter(|g| *g > 0.0),
    };
    stats.any().then_some(stats)
}

/// The models a Lemonade host has resident, from `/api/v1/health`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Resident {
    pub(crate) names: Vec<String>,
}

/// Parse a `/api/v1/health` body. `None` means this is not Lemonade (no
/// `status` plus `all_models_loaded`). Entries may be names or objects.
/// How a 2xx `/api/v1/health` body reads.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HealthShape {
    /// Lemonade, with these models resident.
    Lemonade(Resident),
    /// Another server's reply (not JSON, or no Lemonade keys).
    Other,
    /// Lemonade-looking but not understood: closed.
    Unparsable,
}

/// Judge a 2xx health body. Lemonade replies `{status, all_models_loaded:
/// [...]}`; an older build replies `{status, model_loaded: "name" | null}`
/// without the list, which counts as Lemonade (with that one model).
pub(crate) fn health_shape(body: &str) -> HealthShape {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return HealthShape::Other;
    };
    let Some(obj) = v.as_object() else {
        return HealthShape::Other;
    };
    if !obj.contains_key("status") {
        return HealthShape::Other;
    }
    let mut names = Vec::new();
    match obj.get("all_models_loaded") {
        Some(Value::Array(all)) => {
            for entry in all {
                match entry {
                    Value::String(s) => names.push(s.clone()),
                    Value::Object(o) => {
                        for key in ["model_name", "name", "id", "checkpoint"] {
                            if let Some(s) = o.get(key).and_then(Value::as_str) {
                                names.push(s.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
            // The single-model field also names a resident model.
            if let Some(s) = obj.get("model_loaded").and_then(Value::as_str) {
                if !names.iter().any(|n| n == s) {
                    names.push(s.to_string());
                }
            }
            HealthShape::Lemonade(Resident { names })
        }
        Some(_) => HealthShape::Unparsable,
        None => match obj.get("model_loaded") {
            Some(Value::String(s)) => HealthShape::Lemonade(Resident {
                names: vec![s.clone()],
            }),
            Some(Value::Null) => HealthShape::Lemonade(Resident::default()),
            Some(_) => HealthShape::Unparsable,
            None => HealthShape::Other,
        },
    }
}

/// Parse a `/api/v1/health` body. `None` means this is not (readable)
/// Lemonade.
#[cfg(test)]
pub(crate) fn parse_health(body: &str) -> Option<Resident> {
    match health_shape(body) {
        HealthShape::Lemonade(r) => Some(r),
        _ => None,
    }
}

fn norm_model(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    for prefix in ["user.", "extra."] {
        if let Some(rest) = n.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    n
}

impl Resident {
    /// Whether `model` is among the resident models.
    pub(crate) fn has(&self, model: &str) -> bool {
        let want = norm_model(model);
        !want.is_empty() && self.names.iter().any(|n| norm_model(n) == want)
    }
}

/// The Lemonade server's root: the engine's base URL without a trailing
/// `/v1`, `/api/v1` or `/`.
pub(crate) fn root_url(base_url: &str) -> String {
    let mut s = base_url.trim().trim_end_matches('/');
    for suffix in ["/api/v1", "/v1", "/api"] {
        if let Some(rest) = s.strip_suffix(suffix) {
            s = rest.trim_end_matches('/');
            break;
        }
    }
    s.to_string()
}

/// `<root>/api/v1/health`.
pub(crate) fn health_url(base_url: &str) -> String {
    format!("{}/api/v1/health", root_url(base_url))
}

/// `<root>/api/v1/system-stats`.
pub(crate) fn stats_url(base_url: &str) -> String {
    format!("{}/api/v1/system-stats", root_url(base_url))
}

/// What the safety read found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Residency {
    /// Not a Lemonade host (or it did not answer at all): nothing to guard.
    NotLemonade,
    /// Lemonade, and the model is resident: send.
    Resident(Resident),
    /// Lemonade, and the model is NOT resident: never send.
    NotResident(Resident),
    /// The host accepted the connection but did not answer in time: never
    /// send to a host that cannot be seen to be ready.
    Silent,
}

fn client(timeout: Duration) -> Option<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()
}

#[cfg(test)]
fn is_loopback(base_url: &str) -> bool {
    let host = host_of(base_url).unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host.as_str(), |(h, _)| h);
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

/// Hosts the app has seen answer like Lemonade. Persisted (see
/// [`init_known_store`]) so a restart does not forget them.
fn known_lemonade() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}

fn known_store() -> &'static std::sync::Mutex<Option<std::path::PathBuf>> {
    static PATH: std::sync::OnceLock<std::sync::Mutex<Option<std::path::PathBuf>>> =
        std::sync::OnceLock::new();
    PATH.get_or_init(Default::default)
}

/// Load the persisted known-Lemonade host list from `path` and keep writing
/// new entries there.
pub(crate) fn init_known_store(path: std::path::PathBuf) {
    if let Some(hosts) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
    {
        if let Ok(mut set) = known_lemonade().lock() {
            set.extend(hosts);
        }
    }
    if let Ok(mut p) = known_store().lock() {
        *p = Some(path);
    }
}

/// Remember that `base_url`'s host answered like Lemonade.
pub(crate) fn remember_lemonade(base_url: &str) {
    let key = root_url(base_url).to_ascii_lowercase();
    let Ok(mut set) = known_lemonade().lock() else {
        return;
    };
    if !set.insert(key) {
        return;
    }
    // Best effort, atomic: a failed write only loses the memory across a restart.
    let Some(path) = known_store().lock().ok().and_then(|p| p.clone()) else {
        return;
    };
    let mut hosts: Vec<&String> = set.iter().collect();
    hosts.sort();
    if let Ok(json) = serde_json::to_string(&hosts) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Forget the in-memory set (tests simulating a restart).
#[cfg(test)]
pub(crate) fn forget_known_for_test() {
    if let Ok(mut set) = known_lemonade().lock() {
        set.clear();
    }
    if let Ok(mut p) = known_store().lock() {
        *p = None;
    }
}

/// Whether the host was seen answering like Lemonade.
pub(crate) fn is_known_lemonade(base_url: &str) -> bool {
    known_lemonade()
        .lock()
        .is_ok_and(|set| set.contains(&root_url(base_url).to_ascii_lowercase()))
}

/// How old a reading the transport gate may reuse: a few seconds in the app
/// (so a tool loop does not read the host on every step), none under test.
pub(crate) fn transport_cache_age() -> Duration {
    if cfg!(test) {
        Duration::ZERO
    } else {
        Duration::from_secs(5)
    }
}

/// How long the pre-send host read waits (shorter under test, where the
/// mock servers answer instantly or not at all).
pub(crate) fn guard_timeout() -> Duration {
    if cfg!(test) {
        Duration::from_secs(5)
    } else {
        GUARD_TIMEOUT
    }
}

/// What one `GET /api/v1/health` taught.
enum HealthRead {
    /// Answered like Lemonade, with these models resident.
    Lemonade(Resident),
    /// A definitive negative: not Lemonade.
    Not,
    /// No usable answer (timeout, 5xx, redirect, broken body): closed.
    Silent,
    /// Nothing is listening (refused, reset, unresolvable).
    Refused,
}

fn read_health(base_url: &str, api_key: Option<&str>, timeout: Duration) -> HealthRead {
    // Under test nothing but loopback is ever contacted (a test that names a
    // fleet address must not reach it).
    #[cfg(test)]
    if !is_loopback(base_url) {
        return HealthRead::Not;
    }
    let Some(client) = client(timeout) else {
        return HealthRead::Silent;
    };
    let mut request = client.get(health_url(base_url));
    if let Some(key) = api_key.filter(|k| !k.trim().is_empty()) {
        request = request.bearer_auth(key);
    }
    let response = match request.send() {
        Ok(r) => r,
        // FAIL CLOSED: a host that accepts but does not answer in time (or a
        // connect that times out) may be a busy or hung Lemonade box, and a
        // chat request would make it load the model. Nothing is sent.
        Err(e) if e.is_timeout() => return HealthRead::Silent,
        // Connection refused / reset / unresolvable: there is nothing
        // listening to load anything, and the chat would fail the same way,
        // so the turn is left to report the connection error as it always
        // did. A host KNOWN to be Lemonade stays closed all the same.
        Err(_) => return HealthRead::Refused,
    };
    let status = response.status();
    if !status.is_success() {
        // A definitive negative is a 404: the server understood the request
        // and does not have the path. Anything else -- 401/403 (a host that
        // will not say), 5xx from a struggling host, a redirect -- is not an
        // answer: closed.
        if status == reqwest::StatusCode::NOT_FOUND {
            note_fact(base_url, None);
            return HealthRead::Not;
        }
        return HealthRead::Silent;
    }
    let Ok(body) = response.text() else {
        return HealthRead::Silent;
    };
    match health_shape(&body) {
        // 2xx with another server's shape: definitively not Lemonade.
        HealthShape::Other => {
            note_fact(base_url, None);
            HealthRead::Not
        }
        // Lemonade-looking and not understood: closed.
        HealthShape::Unparsable => HealthRead::Silent,
        HealthShape::Lemonade(resident) => {
            remember_lemonade(base_url);
            note_fact(base_url, Some(&resident));
            HealthRead::Lemonade(resident)
        }
    }
}

/// BLOCKING (worker threads only). Read `GET <root>/api/v1/health`, which
/// never loads a model, and judge whether a turn to `model` may be sent.
///
/// This is the only thing the pre-send guard calls on the host. It never
/// calls `/api/v1/load` and never a chat endpoint.
pub(crate) fn check_residency(
    base_url: &str,
    model: &str,
    api_key: Option<&str>,
    timeout: Duration,
) -> Residency {
    match read_health(base_url, api_key, timeout) {
        HealthRead::Lemonade(resident) if resident.has(model) => Residency::Resident(resident),
        HealthRead::Lemonade(resident) => Residency::NotResident(resident),
        HealthRead::Not => Residency::NotLemonade,
        HealthRead::Silent => Residency::Silent,
        HealthRead::Refused if is_known_lemonade(base_url) => Residency::Silent,
        HealthRead::Refused => Residency::NotLemonade,
    }
}

/// What the app last learned about a host, from any health read.
struct HostFact {
    /// `Some(models)` for Lemonade; `None` for a confirmed other server.
    resident: Option<Resident>,
    at: Instant,
}

fn facts() -> &'static std::sync::Mutex<HashMap<String, HostFact>> {
    static FACTS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, HostFact>>> =
        std::sync::OnceLock::new();
    FACTS.get_or_init(Default::default)
}

fn note_fact(base_url: &str, resident: Option<&Resident>) {
    if let Ok(mut f) = facts().lock() {
        f.insert(
            root_url(base_url).to_ascii_lowercase(),
            HostFact {
                resident: resident.cloned(),
                at: Instant::now(),
            },
        );
    }
}

fn ip_guarded(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => v4_guarded(v4),
        IpAddr::V6(v6) => v6_guarded(v6),
    }
}

fn v4_guarded(ip: std::net::Ipv4Addr) -> bool {
    // Loopback, RFC 1918, link-local, 100.64/10 (CGNAT, the tailnet) and the
    // unspecified address (0.0.0.0 connects to this machine).
    ip.is_unspecified() || crate::engine_locality::v4_is_home(ip)
}

fn v6_guarded(ip: std::net::Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return v4_guarded(v4);
    }
    let first = ip.segments()[0];
    // Loopback, unspecified, unique-local fc00::/7, link-local fe80::/10.
    ip.is_loopback()
        || ip.is_unspecified()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
}

/// Whether `base_url`'s host NAME says it is on this machine or the home
/// network: loopback, private LAN, CGNAT/tailnet, link-local, ULA, IPv4-mapped
/// IPv6, the unspecified address, `localhost`, single-label names, `*.ts.net`,
/// `*.internal`, `*.local`, `*.lan`, `*.home.arpa`, and any host already
/// known to be Lemonade. Parsed by `reqwest::Url` exactly as the request is
/// (backslashes, userinfo and numeric IPv4 forms such as `127.1` and
/// `0x7f.1` normalise before they are judged).
///
/// In-app engine hosts are ALWAYS checked, whatever this says. It decides
/// only the provider routes ([`provider_host_guarded`]).
pub(crate) fn guarded_host(base_url: &str) -> bool {
    if is_known_lemonade(base_url) {
        return true;
    }
    match crate::engine_locality::engine_host(base_url) {
        Some(url::Host::Ipv4(v4)) => v4_guarded(v4),
        Some(url::Host::Ipv6(v6)) => v6_guarded(v6),
        Some(url::Host::Domain(d)) => crate::engine_locality::domain_is_home(&d),
        None => false,
    }
}

/// Whether a PROVIDER route's host needs the check: a guarded name
/// ([`guarded_host`]), or a name that looks public but RESOLVES (off the UI
/// thread, short timeout) to a loopback, private, CGNAT, link-local or ULA
/// address (`hosta.example.org` pointing at 192.168.x, a tunnelled name).
/// A name that is public and resolves only to public addresses is skipped; a
/// resolution failure counts as "not guarded", so cloud APIs keep working
/// when the resolver is slow or offline.
pub(crate) fn provider_host_guarded(base_url: &str, timeout: Duration) -> bool {
    if guarded_host(base_url) {
        return true;
    }
    let Some(url::Host::Domain(name)) = crate::engine_locality::engine_host(base_url) else {
        return false;
    };
    let port = reqwest::Url::parse(&crate::engine_locality::request_url(base_url))
        .ok()
        .and_then(|u| u.port_or_known_default())
        .unwrap_or(443);
    resolve_host(&name, port, timeout).is_some_and(|addrs| addrs.into_iter().any(ip_guarded))
}

#[cfg(test)]
thread_local! {
    /// Name -> addresses a test pretends the resolver returns (no real DNS).
    static TEST_RESOLVE: std::cell::RefCell<HashMap<String, Vec<std::net::IpAddr>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Make `name` resolve to `addrs` for this test thread.
#[cfg(test)]
pub(crate) fn set_test_resolution(name: &str, addrs: &[&str]) {
    TEST_RESOLVE.with(|m| {
        m.borrow_mut().insert(
            name.to_string(),
            addrs.iter().map(|a| a.parse().unwrap()).collect(),
        )
    });
}

/// Resolve `name` without blocking the caller past `timeout`; `None` on
/// failure or timeout.
fn resolve_host(name: &str, port: u16, timeout: Duration) -> Option<Vec<std::net::IpAddr>> {
    #[cfg(test)]
    {
        let _ = (port, timeout);
        TEST_RESOLVE.with(|m| m.borrow().get(name).cloned())
    }
    #[cfg(not(test))]
    {
        use std::net::ToSocketAddrs;
        let (tx, rx) = std::sync::mpsc::channel();
        let name = name.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(
                (name.as_str(), port)
                    .to_socket_addrs()
                    .ok()
                    .map(|it| it.map(|a| a.ip()).collect::<Vec<_>>()),
            );
        });
        rx.recv_timeout(timeout).ok().flatten()
    }
}

/// A health reading no older than `max_age`, as a verdict for `model`:
/// the transport's per-host cache, so a tool loop does not read the host on
/// every step. `None`: nothing fresh, read again.
pub(crate) fn cached_residency(
    base_url: &str,
    model: &str,
    max_age: Duration,
) -> Option<Residency> {
    let key = root_url(base_url).to_ascii_lowercase();
    let f = facts().lock().ok()?;
    let fact = f.get(&key)?;
    if fact.at.elapsed() > max_age {
        return None;
    }
    Some(match &fact.resident {
        None => Residency::NotLemonade,
        Some(r) if r.has(model) => Residency::Resident(r.clone()),
        Some(r) => Residency::NotResident(r.clone()),
    })
}

/// How long a health reading stays good for deciding a background request.
const FACT_FRESH: Duration = Duration::from_secs(20);

/// Whether a BACKGROUND request (a warm-up) to `model` on this host may go:
/// only when a recent health read says the host is another kind of server,
/// or Lemonade with the model resident. No reading, or a stale one: no.
/// Purely in-memory: it never touches the network or the UI thread's time.
pub(crate) fn warm_allowed(base_url: &str, model: &str) -> bool {
    #[cfg(test)]
    {
        // Tests have no poller: read the (loopback) host once to fill it.
        let key = root_url(base_url).to_ascii_lowercase();
        let missing = facts()
            .lock()
            .map(|f| !f.contains_key(&key))
            .unwrap_or(true);
        if missing {
            // Nothing listening is no Lemonade host either (a warm-up there only
            // fails); a refused connection counts as a confirmed other server.
            if matches!(
                read_health(base_url, None, Duration::from_secs(5)),
                HealthRead::Refused
            ) && !is_known_lemonade(base_url)
            {
                note_fact(base_url, None);
            }
        }
    }
    let key = root_url(base_url).to_ascii_lowercase();
    let Ok(f) = facts().lock() else {
        return false;
    };
    match f.get(&key) {
        Some(fact) if fact.at.elapsed() <= FACT_FRESH => match &fact.resident {
            None => true,
            Some(r) => r.has(model),
        },
        _ => false,
    }
}

/// BLOCKING. Read `GET <root>/api/v1/system-stats` (never loads a model).
pub(crate) fn fetch_host_stats(
    base_url: &str,
    api_key: Option<&str>,
    timeout: Duration,
) -> Option<HostStats> {
    let mut request = client(timeout)?.get(stats_url(base_url));
    if let Some(key) = api_key.filter(|k| !k.trim().is_empty()) {
        request = request.bearer_auth(key);
    }
    let response = request.send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    parse_system_stats(&response.text().ok()?)
}

/// What one reachability tick learned about an engine.
#[derive(Clone, Debug)]
pub(crate) struct Reading {
    pub(crate) reach: Reach,
    /// `Some` once the host answered `/api/v1/health` like Lemonade.
    pub(crate) resident: Option<Resident>,
    /// `Some(false)` once the host answered health as something else.
    pub(crate) lemonade: bool,
}

/// BLOCKING. The 15 s tick for one engine: `GET /v1/models` for
/// reachability, then (when it answers) `GET /api/v1/health` to learn
/// whether it is Lemonade and what is resident. Read-only throughout.
pub(crate) fn read_engine(base_url: &str, api_key: Option<&str>, timeout: Duration) -> Reading {
    let reach = match crate::local_model_health::probe_model_endpoint(base_url, timeout) {
        Ok(()) => Reach::Up,
        Err(detail) => Reach::Down(detail),
    };
    if reach != Reach::Up {
        return Reading {
            reach,
            resident: None,
            lemonade: false,
        };
    }
    let resident = match read_health(base_url, api_key, timeout) {
        HealthRead::Lemonade(resident) => Some(resident),
        _ => None,
    };
    Reading {
        reach,
        lemonade: resident.is_some(),
        resident,
    }
}

/// What the app remembers about one engine's endpoint.
#[derive(Clone, Debug)]
pub(crate) struct EngineProbe {
    pub(crate) reach: Reach,
    pub(crate) checked_at: Option<Instant>,
    pub(crate) lemonade: bool,
    pub(crate) resident: Option<Resident>,
    pub(crate) host: Option<HostStats>,
    pub(crate) host_at: Option<Instant>,
    pub(crate) reach_in_flight: bool,
    pub(crate) host_in_flight: bool,
}

impl Default for EngineProbe {
    fn default() -> Self {
        Self {
            reach: Reach::Unknown,
            checked_at: None,
            lemonade: false,
            resident: None,
            host: None,
            host_at: None,
            reach_in_flight: false,
            host_in_flight: false,
        }
    }
}

impl EngineProbe {
    /// Whether the 15 s reachability read is due.
    pub(crate) fn reach_due(&self, now: Instant) -> bool {
        !self.reach_in_flight
            && self
                .checked_at
                .is_none_or(|t| now.saturating_duration_since(t) >= REACH_EVERY)
    }

    /// Whether the 5 s host-stats read is due: a Lemonade host, and a turn on
    /// it running or the popover open (`wanted`).
    pub(crate) fn host_due(&self, now: Instant, wanted: bool) -> bool {
        wanted
            && self.lemonade
            && self.reach == Reach::Up
            && !self.host_in_flight
            && self
                .host_at
                .is_none_or(|t| now.saturating_duration_since(t) >= HOST_EVERY)
    }

    /// Record a reachability reading.
    pub(crate) fn apply(&mut self, reading: Reading, now: Instant) {
        self.reach_in_flight = false;
        self.checked_at = Some(now);
        self.reach = reading.reach;
        self.lemonade = reading.lemonade;
        self.resident = reading.resident;
        if !self.lemonade {
            self.host = None;
        }
    }
}

// ---- the derived view -------------------------------------------------------------

/// The state dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DotKind {
    /// Quiet: a dim dot.
    Idle,
    /// Streaming: a breathing dot.
    Live,
    /// Green: needs attention, not urgent (loading, not loaded).
    Attention,
    /// Red: the server is not responding.
    Alert,
}

/// Where the model runs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Where {
    /// An in-app engine on a server: `host` is the name the strip uses
    /// ("Hosta"), `addr` the server address for the popover.
    Local {
        host: String,
        addr: String,
        model: String,
    },
    /// A cloud or subscription model.
    Cloud { model: String, plan: Plan },
}

/// How a cloud model is paid for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    Subscription,
    Metered,
}

/// Everything the derivation reads.
#[derive(Clone, Debug)]
pub(crate) struct Inputs {
    pub(crate) place: Where,
    /// A turn is running in this conversation.
    pub(crate) running: bool,
    pub(crate) live: Option<LiveSnapshot>,
    pub(crate) last: Option<ReplyStats>,
    /// Reachability for a local engine (the 15 s read).
    pub(crate) reach: Reach,
    pub(crate) reach_age: Option<Duration>,
    /// The engine is Lemonade, and what it has resident (None: unknown).
    pub(crate) lemonade: bool,
    pub(crate) resident: Option<Resident>,
    pub(crate) host: Option<HostStats>,
    pub(crate) host_age: Option<Duration>,
    /// The composer's own "18.7K of 262K" and its numbers.
    pub(crate) context_text: Option<String>,
    pub(crate) context_used: Option<u64>,
    pub(crate) context_window: Option<u64>,
    /// Today's spend for a metered cloud model.
    pub(crate) spend_today: Option<String>,
    /// How long ago the strip's reading was taken.
    pub(crate) loading_after: Duration,
}

/// The state the strip is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Ready,
    Streaming,
    Loading,
    NotLoaded,
    Unreachable,
}

impl State {
    /// The popover's state word.
    pub(crate) fn word(self) -> &'static str {
        match self {
            State::Ready => "ready",
            State::Streaming => "streaming",
            State::Loading => "loading",
            State::NotLoaded => "not loaded",
            State::Unreachable => "no response",
        }
    }
}

/// Key-value rows: a label and its typed value.
pub(crate) type Rows = Vec<(String, Vec<Seg>)>;

/// The popover's content.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Detail {
    pub(crate) title: String,
    pub(crate) state: State,
    pub(crate) sub: Option<(String, String)>,
    pub(crate) reply: Rows,
    pub(crate) context: Option<ContextDetail>,
    pub(crate) host: Option<(String, Rows)>,
    pub(crate) cloud: Rows,
    /// "as of 6 s ago"
    pub(crate) age: Option<String>,
    /// Offer "Check now" (a local engine).
    pub(crate) check_now: bool,
}

/// The context bar.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ContextDetail {
    pub(crate) text: Vec<Seg>,
    pub(crate) percent: String,
    pub(crate) fraction: f32,
}

/// The strip itself.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct View {
    pub(crate) state: State,
    pub(crate) dot: DotKind,
    pub(crate) left: Vec<Seg>,
    /// The left words are an error sentence (red) or an attention sentence
    /// (green) rather than a value.
    pub(crate) left_tone: LeftTone,
    pub(crate) right: Vec<Seg>,
    /// Show the Retry link at the right.
    pub(crate) retry: bool,
    pub(crate) detail: Detail,
    /// Screen-reader label.
    pub(crate) label: String,
}

/// How the left sentence is coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LeftTone {
    Values,
    Alert,
    Attention,
}

fn percent(v: f64) -> Vec<Seg> {
    vec![
        seg(format!("{v:.0}"), Weight::Value),
        seg("%", Weight::Unit),
    ]
}

/// Derive the strip from the facts.
pub(crate) fn derive(i: &Inputs) -> View {
    let (model, host_name, addr, local) = match &i.place {
        Where::Local { host, addr, model } => {
            (model.clone(), host.clone(), Some(addr.clone()), true)
        }
        Where::Cloud { model, .. } => (model.clone(), String::new(), None, false),
    };

    let streaming = i.running && i.live.is_some_and(|l| l.has_first_token);
    let waiting = i.running && i.live.is_none_or(|l| !l.has_first_token);
    let waited_long = i
        .live
        .is_some_and(|l| l.waiting >= i.loading_after)
        // A turn whose first event has not arrived yet has no snapshot; the
        // app starts the turn's clock when it is sent, so this is rare.
        ;
    let unreachable = local && matches!(i.reach, Reach::Down(_));
    let model_missing =
        local && i.lemonade && i.resident.as_ref().is_some_and(|r| !r.has(&model)) && !i.running;

    let state = if local && unreachable && !streaming {
        State::Unreachable
    } else if model_missing {
        State::NotLoaded
    } else if streaming && local || !local && i.running {
        State::Streaming
    } else if local && waiting && waited_long {
        State::Loading
    } else {
        State::Ready
    };

    let mut left: Vec<Seg> = Vec::new();
    let mut left_tone = LeftTone::Values;
    let mut retry = false;
    let mut show_context = true;
    let dot = match state {
        State::Ready => {
            if local {
                if let Some(tps) = i.last.and_then(|l| l.tokens_per_s) {
                    left = speed_segs(tps);
                }
            } else if let Some((inp, out)) = i
                .last
                .and_then(|l| l.input.zip(l.output).or(l.output.map(|o| (0, o))))
                .filter(|(inp, out)| inp + out > 0)
            {
                left = if inp > 0 {
                    io_segs(inp, out)
                } else {
                    vec![
                        seg(tokens_short(out), Weight::Value),
                        seg("out", Weight::Unit),
                    ]
                };
            }
            DotKind::Idle
        }
        State::Streaming => {
            if local {
                if let Some(tps) = i.live.and_then(|l| l.tokens_per_s) {
                    left = speed_segs(tps);
                }
            }
            DotKind::Live
        }
        State::Loading => {
            left = vec![seg(loading(&model), Weight::Plain)];
            left_tone = LeftTone::Attention;
            DotKind::Attention
        }
        State::NotLoaded => {
            left = vec![seg(not_loaded(&model, &host_name), Weight::Plain)];
            left_tone = LeftTone::Attention;
            retry = true;
            show_context = false;
            DotKind::Attention
        }
        State::Unreachable => {
            left = vec![seg(not_responding(&host_name), Weight::Plain)];
            left_tone = LeftTone::Alert;
            retry = true;
            show_context = false;
            DotKind::Alert
        }
    };

    let right = if show_context {
        i.context_text
            .as_deref()
            .map(context_segs)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // ---- the popover ----
    let mut reply: Vec<(String, Vec<Seg>)> = Vec::new();
    if let Some(last) = i.last {
        if local {
            if let Some(tps) = last.tokens_per_s {
                reply.push((
                    "Speed".into(),
                    vec![
                        seg(speed_long(tps), Weight::Value),
                        seg("tok/s", Weight::Unit),
                    ],
                ));
            }
            if let Some(ft) = last.first_token_s {
                reply.push((
                    "First token".into(),
                    vec![
                        seg(first_token_text(ft), Weight::Value),
                        seg("s", Weight::Unit),
                    ],
                ));
            }
        }
        if let Some(out) = last.output {
            let value = match last.input {
                Some(inp) => vec![
                    seg(group(inp), Weight::Value),
                    seg("in", Weight::Unit),
                    seg("\u{b7}", Weight::Of),
                    seg(group(out), Weight::Value),
                    seg("out", Weight::Unit),
                ],
                None => vec![seg(group(out), Weight::Value), seg("out", Weight::Unit)],
            };
            reply.push(("Tokens".into(), value));
        }
    }

    let context = if show_context {
        match (i.context_used, i.context_window, i.context_text.as_deref()) {
            (Some(used), Some(window), Some(text)) if window > 0 => {
                let fraction = (used as f64 / window as f64).clamp(0.0, 1.0) as f32;
                Some(ContextDetail {
                    text: context_segs(text),
                    percent: format!("{:.1}%", fraction * 100.0),
                    fraction,
                })
            }
            _ => None,
        }
    } else {
        None
    };

    let host_block = if local && i.lemonade {
        i.host.filter(HostStats::any).map(|h| {
            let mut rows = Vec::new();
            if let Some(g) = h.gpu_percent {
                rows.push(("Graphics".to_string(), percent(g)));
            }
            if let Some(n) = h.npu_percent {
                rows.push(("Neural processor".to_string(), percent(n)));
            }
            if let Some(used) = h.memory_used_gb {
                let mut v = vec![seg(format!("{used:.1}"), Weight::Value)];
                match h.memory_total_gb {
                    Some(total) => v.push(seg(format!("of {total:.0} GB"), Weight::Unit)),
                    None => v.push(seg("GB", Weight::Unit)),
                }
                rows.push(("Memory".to_string(), v));
            }
            (host_name.clone(), rows)
        })
    } else {
        None
    };

    let mut cloud = Vec::new();
    if let Where::Cloud { plan, .. } = &i.place {
        match plan {
            Plan::Subscription => {
                cloud.push(("Plan".to_string(), vec![seg("Included", Weight::Value)]));
            }
            Plan::Metered => {
                cloud.push((
                    "Plan".to_string(),
                    vec![seg("Pay as you go", Weight::Value)],
                ));
            }
        }
        if let Some(spend) = &i.spend_today {
            cloud.push(("Today".to_string(), vec![seg(spend.clone(), Weight::Value)]));
        }
    }

    // The age of the freshest reading the popover shows.
    let age = if local {
        match (i.host_age, i.reach_age) {
            (Some(h), _) if host_block.is_some() => Some(h),
            (_, r) => r,
        }
    } else {
        None
    }
    .map(|a| {
        if state == State::Unreachable {
            format!("last checked {}", age_text(a))
        } else {
            format!("as of {}", age_text(a))
        }
    });

    let detail = Detail {
        title: model.clone(),
        state,
        sub: addr.map(|a| (host_name.clone(), a)),
        reply,
        context,
        host: host_block,
        cloud,
        age,
        check_now: local,
    };

    let label = {
        let mut s = format!("Model status: {}", state.word());
        if !left.is_empty() {
            let words: Vec<&str> = left.iter().map(|s| s.text.as_str()).collect();
            s.push_str(", ");
            s.push_str(&words.join(" "));
        }
        s
    };

    View {
        state,
        dot,
        left,
        left_tone,
        right,
        retry,
        detail,
        label,
    }
}

/// 1912 -> "1,912".
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (ix, ch) in s.chars().enumerate() {
        if ix > 0 && (s.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// The name the strip calls a server by: the parenthesised part of the
/// engine's label ("Qwen3 Coder Next (Hosta)" -> "Hosta"), else the label,
/// else the address's host.
pub(crate) fn host_name(label: &str, base_url: &str) -> String {
    if let (Some(open), Some(close)) = (label.rfind('('), label.rfind(')')) {
        if open + 1 < close {
            let inner = label[open + 1..close].trim();
            if !inner.is_empty() {
                return inner.to_string();
            }
        }
    }
    host_of(base_url).unwrap_or_else(|| "The server".to_string())
}

/// `host:port` of a base URL.
pub(crate) fn host_of(base_url: &str) -> Option<String> {
    // Parsed as the request is (userinfo, backslashes, numeric forms).
    if let Ok(url) = reqwest::Url::parse(&crate::engine_locality::request_url(base_url)) {
        if let Some(host) = url.host_str() {
            return Some(match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_string(),
            });
        }
    }
    let rest = base_url
        .trim()
        .split("://")
        .nth(1)
        .or(Some(base_url.trim()))?;
    let host = rest.split('/').next()?.trim();
    (!host.is_empty()).then(|| host.to_string())
}

#[cfg(test)]
#[path = "strip_tests.rs"]
pub(crate) mod tests;
