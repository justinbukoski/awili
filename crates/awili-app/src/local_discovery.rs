//! Explicit, bounded discovery of advertised LAN model listings. Never inference.
//! Run `scan` on a background OS thread. Dropping UI interest must set `cancel`;
//! callers should additionally reject progress/results from an old UI generation.
use awili_config::ProviderKind;
use std::{
    collections::HashSet,
    io::Read,
    net::{Ipv4Addr, SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

const PORTS: [u16; 5] = [11434, 1234, 8000, 8080, 13305];
const MAX_HOSTS: usize = 512;
const MAX_MODELS: usize = 128;
const MAX_BYTES: u64 = 256 * 1024;
const MAX_RESULTS: usize = 256;
const WORKERS: usize = 16;
const DEADLINE: Duration = Duration::from_secs(45);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiscoveryModel {
    pub id: String,
    /// Listings advertise availability, not inference readiness. No load request
    /// is issued; absence of an explicit boolean remains unknown.
    pub loaded: Option<bool>,
}
#[derive(Clone, Debug)]
pub(crate) struct DiscoveryResult {
    pub base_url: String,
    pub kind: ProviderKind,
    pub models: Vec<DiscoveryModel>,
}
#[derive(Clone, Debug)]
pub(crate) struct DiscoveryScope {
    pub interface: String,
    /// The interface's own network — carried for scope diagnostics; the UI
    /// renders `scanned_cidr` (the probe range) instead.
    #[allow(dead_code)] // diagnostic data — no current reader
    pub cidr: String,
    pub scanned_cidr: String,
    pub host_count: usize,
    /// Per-scope host cap was hit.
    #[allow(dead_code)] // diagnostic data — no current reader
    pub truncated: bool,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct DiscoveryProgress {
    pub completed: usize,
    pub total: usize,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct DiscoveryReport {
    pub scopes: Vec<DiscoveryScope>,
    pub results: Vec<DiscoveryResult>,
    pub completed: usize,
    pub total: usize,
    pub cancelled: bool,
    pub deadline_reached: bool,
    pub truncated: bool,
    pub errors: Vec<String>,
}
#[derive(Clone, Debug)]
struct Interface {
    name: String,
    address: Ipv4Addr,
    mask: Ipv4Addr,
}

fn private(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168)
}
#[cfg(unix)]
const IFF_UP: u32 = libc::IFF_UP as u32;
#[cfg(unix)]
const IFF_POINTOPOINT: u32 = libc::IFF_POINTOPOINT as u32;
// Same values on Linux and macOS; the Windows build has no `ifaddrs` flags and
// its `interfaces()` stub returns an error (TODO(windows-phase1): GetAdaptersAddresses).
#[cfg(not(unix))]
const IFF_UP: u32 = 0x1;
#[cfg(not(unix))]
const IFF_POINTOPOINT: u32 = 0x10;
fn allowed_interface(name: &str, flags: u32) -> bool {
    flags & IFF_UP != 0
        && flags & IFF_POINTOPOINT == 0
        && ![
            "utun",
            "tun",
            "tap",
            "wg",
            "tailscale",
            "ppp",
            "ipsec",
            "awdl",
            "llw",
            "docker",
            "veth",
            "virbr",
        ]
        .iter()
        .any(|p| name.starts_with(p))
}
/// Native enumeration only: no shell, routing probes, DNS or VPN range guesses.
#[cfg(unix)]
fn interfaces() -> Result<Vec<Interface>, String> {
    let mut first: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs initializes this pointer; its linked list stays alive
    // until the guard is dropped, including all sockaddr/name storage.
    if unsafe { libc::getifaddrs(&mut first) } != 0 {
        return Err("Could not enumerate local network interfaces".into());
    }
    struct Guard(*mut libc::ifaddrs);
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { libc::freeifaddrs(self.0) }
        }
    }
    let _guard = Guard(first);
    let mut out = Vec::new();
    let mut current = first;
    while !current.is_null() {
        let row = unsafe { &*current };
        if !row.ifa_addr.is_null()
            && !row.ifa_netmask.is_null()
            && !row.ifa_name.is_null()
            && unsafe { (*row.ifa_addr).sa_family as i32 } == libc::AF_INET
            && unsafe { (*row.ifa_netmask).sa_family as i32 } == libc::AF_INET
        {
            let name = unsafe { std::ffi::CStr::from_ptr(row.ifa_name) }
                .to_string_lossy()
                .into_owned();
            if allowed_interface(&name, row.ifa_flags) {
                let addr = unsafe { &*(row.ifa_addr as *const libc::sockaddr_in) };
                let mask = unsafe { &*(row.ifa_netmask as *const libc::sockaddr_in) };
                let address = Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes());
                if private(address) {
                    out.push(Interface {
                        name,
                        address,
                        mask: Ipv4Addr::from(mask.sin_addr.s_addr.to_ne_bytes()),
                    });
                }
            }
        }
        current = row.ifa_next;
    }
    out.sort_by(|a, b| (&a.name, a.address).cmp(&(&b.name, b.address)));
    Ok(out)
}
#[cfg(not(unix))]
fn interfaces() -> Result<Vec<Interface>, String> {
    Err("Local interface discovery is unavailable on this platform".into())
}

fn plan(input: &[Interface]) -> (Vec<DiscoveryScope>, Vec<Ipv4Addr>, bool) {
    let mut hosts = vec![Ipv4Addr::LOCALHOST];
    let mut seen = HashSet::from([Ipv4Addr::LOCALHOST]);
    let mut scopes = vec![DiscoveryScope {
        interface: "loopback".into(),
        cidr: "127.0.0.1/32".into(),
        scanned_cidr: "127.0.0.1/32".into(),
        host_count: 1,
        truncated: false,
    }];
    let mut truncated = false;
    for iface in input {
        if !private(iface.address) {
            continue;
        }
        let mask = u32::from(iface.mask);
        let prefix = mask.leading_ones();
        if prefix == 0 || mask != u32::MAX.checked_shl(32 - prefix).unwrap_or(0) {
            continue;
        }
        let original = u32::from(iface.address) & mask;
        // Narrow a large network to the connected address's /24, never widen
        // a /25..32. Also never probe a public address from a malformed mask.
        let scan_prefix = prefix.max(24);
        let scan_mask = u32::MAX << (32 - scan_prefix);
        let network = u32::from(iface.address) & scan_mask;
        let end = network | !scan_mask;
        let before = hosts.len();
        let mut cut = prefix < 24;
        for raw in network..=end {
            if scan_prefix <= 30 && (raw == network || raw == end) {
                continue;
            }
            let ip = Ipv4Addr::from(raw);
            if !private(ip) || seen.contains(&ip) {
                continue;
            }
            if hosts.len() == MAX_HOSTS {
                cut = true;
                continue;
            }
            seen.insert(ip);
            hosts.push(ip);
        }
        truncated |= cut;
        scopes.push(DiscoveryScope {
            interface: iface.name.clone(),
            cidr: format!("{}/{prefix}", Ipv4Addr::from(original)),
            scanned_cidr: format!("{}/{scan_prefix}", Ipv4Addr::from(network)),
            host_count: hosts.len() - before,
            truncated: cut,
        });
    }
    (scopes, hosts, truncated)
}

#[derive(Clone, Copy)]
enum Listing {
    OpenAi,
    Ollama,
    Lemonade,
}
impl Listing {
    fn path(self) -> &'static str {
        match self {
            Self::OpenAi => "/v1/models",
            Self::Ollama => "/api/tags",
            Self::Lemonade => "/api/v1/models",
        }
    }
    fn base(self) -> &'static str {
        match self {
            Self::OpenAi => "/v1",
            Self::Ollama => "",
            Self::Lemonade => "/api/v1",
        }
    }
}
fn non_chat_listing(row: &serde_json::Value) -> bool {
    let excluded = |value: &str| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "image"
                | "tts"
                | "transcription"
                | "realtime-transcription"
                | "sd-cpp"
                | "whispercpp"
                | "kokoro"
                | "collection.omni"
        )
    };
    let recipe = row
        .get("recipe")
        .and_then(|v| v.as_str())
        .is_some_and(excluded);
    let labels = row.get("labels").is_some_and(|v| match v {
        serde_json::Value::String(s) => excluded(s),
        serde_json::Value::Array(values) => values.iter().filter_map(|v| v.as_str()).any(excluded),
        _ => false,
    });
    recipe || labels
}

fn parse(bytes: &[u8], listing: Listing) -> Option<(Vec<DiscoveryModel>, bool)> {
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let rows = match listing {
        Listing::Ollama => json.get("models"),
        _ => json.get("data"),
    }?
    .as_array()?;
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    let mut truncated = false;
    for row in rows {
        if non_chat_listing(row) {
            continue;
        }
        let Some(id) = (match listing {
            Listing::Ollama => row.get("name"),
            _ => row.get("id"),
        })
        .and_then(|v| v.as_str()) else {
            continue;
        };
        if id.is_empty() || id.len() > 512 || id.trim() != id
            || id.contains(',') || id.chars().any(|c| c.is_control() || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')) {
            continue;
        }
        if !seen.insert(id.to_owned()) {
            continue;
        }
        if models.len() == MAX_MODELS {
            truncated = true;
            break;
        }
        models.push(DiscoveryModel {
            id: id.to_owned(),
            loaded: row.get("loaded").and_then(|v| v.as_bool()),
        });
    }
    if models.is_empty() {
        None
    } else {
        Some((models, truncated))
    }
}
fn probe(
    client: &reqwest::blocking::Client,
    host: Ipv4Addr,
    port: u16,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Option<(DiscoveryResult, bool)> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() || cancel.load(Ordering::Relaxed) {
        return None;
    }
    let address = SocketAddr::from((host, port));
    drop(TcpStream::connect_timeout(&address, remaining.min(Duration::from_millis(200))).ok()?);
    let routes = match port {
        11434 => [Listing::Ollama, Listing::OpenAi, Listing::Lemonade],
        13305 => [Listing::Lemonade, Listing::OpenAi, Listing::Ollama],
        _ => [Listing::OpenAi, Listing::Ollama, Listing::Lemonade],
    };
    for listing in routes {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || cancel.load(Ordering::Relaxed) {
            break;
        }
        let url = format!("http://{host}:{port}{}", listing.path());
        let Ok(response) = client
            .get(url)
            .timeout(remaining.min(Duration::from_secs(2)))
            .send()
        else {
            continue;
        };
        if !response.status().is_success()
            || response.content_length().is_some_and(|n| n > MAX_BYTES)
        {
            continue;
        }
        let mut bytes = Vec::new();
        if response
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            continue;
        }
        if let Some((models, truncated)) = parse(&bytes, listing) {
            return Some((
                DiscoveryResult {
                    base_url: format!("http://{host}:{port}{}", listing.base()),
                    kind: match listing {
                        Listing::Ollama => ProviderKind::Ollama,
                        _ => ProviderKind::OpenAi,
                    },
                    models,
                },
                truncated,
            ));
        }
    }
    None
}

/// Scan only when explicitly requested. Progress counts completed host:port
/// probes, not models. A deadline/cancelled scan returns useful partial results.
pub(crate) fn scan(
    cancel: &AtomicBool,
    on_progress: impl Fn(DiscoveryProgress) + Sync,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if cancel.load(Ordering::Relaxed) {
        report.cancelled = true;
        return report;
    }
    let input = match interfaces() {
        Ok(v) => v,
        Err(e) => {
            report.errors.push(e);
            Vec::new()
        }
    };
    let (scopes, hosts, truncated) = plan(&input);
    report.scopes = scopes;
    report.truncated = truncated;
    // Port-major order gives every host a chance at common services before
    // attempting less common ports, rather than exhausting each host serially.
    let tasks: Vec<_> = PORTS
        .into_iter()
        .flat_map(|port| hosts.iter().copied().map(move |ip| (ip, port)))
        .collect();
    report.total = tasks.len();
    let client = match reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            report
                .errors
                .push("Could not create discovery HTTP client".into());
            return report;
        }
    };
    let next = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let found = Mutex::new(Vec::new());
    let cut = AtomicBool::new(false);
    let deadline = Instant::now() + DEADLINE;
    on_progress(DiscoveryProgress {
        completed: 0,
        total: tasks.len(),
    });
    // Serialize progress callbacks so a delayed worker cannot publish a lower
    // count after a newer update. Never hold this lock while doing network I/O.
    let progress_lock = Mutex::new(());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| loop {
                if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
                    break;
                }
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(&(host, port)) = tasks.get(index) else {
                    break;
                };
                if let Some((result, truncated)) = probe(&client, host, port, cancel, deadline) {
                    let mut rows = found.lock().unwrap();
                    if rows.len() < MAX_RESULTS {
                        rows.push(result);
                    } else {
                        cut.store(true, Ordering::Relaxed);
                    }
                    if truncated {
                        cut.store(true, Ordering::Relaxed);
                    }
                }
                let _guard = progress_lock.lock().unwrap();
                let count = completed.fetch_add(1, Ordering::Relaxed) + 1;
                on_progress(DiscoveryProgress {
                    completed: count,
                    total: tasks.len(),
                });
            });
        }
    });
    report.results = found.into_inner().unwrap();
    report.results.sort_by(|a, b| a.base_url.cmp(&b.base_url));
    report.completed = completed.load(Ordering::Relaxed);
    report.cancelled = cancel.load(Ordering::Relaxed);
    report.deadline_reached = report.completed < report.total && Instant::now() >= deadline;
    report.truncated |= cut.load(Ordering::Relaxed) || report.deadline_reached || report.cancelled;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    fn iface(ip: &str, mask: &str) -> Interface {
        Interface {
            name: "en0".into(),
            address: ip.parse().unwrap(),
            mask: mask.parse().unwrap(),
        }
    }
    #[test]
    fn subnet_boundaries_and_large_scope_are_honest() {
        let (s, h, t) = plan(&[iface("10.77.3.200", "255.255.255.128")]);
        assert!(!t);
        assert_eq!(s[1].host_count, 126);
        assert!(h[1..].iter().all(|ip| (129..255).contains(&ip.octets()[3])));
        let (s, h, t) = plan(&[iface("10.42.8.22", "255.0.0.0")]);
        assert!(t);
        assert_eq!(s[1].scanned_cidr, "10.42.8.0/24");
        assert_eq!(h.len(), 255);
    }
    #[test]
    fn excludes_public_cgnat_vpn_and_invalid_masks() {
        assert!(
            plan(&[
                iface("100.64.1.2", "255.255.255.0"),
                iface("8.8.8.8", "255.255.255.0"),
                iface("10.1.1.1", "255.0.255.0")
            ])
            .1
            .len()
                == 1
        );
        for name in ["utun0", "tun0", "tailscale0", "wg0", "awdl0"] {
            assert!(!allowed_interface(name, IFF_UP));
        }
        assert!(!allowed_interface("en0", IFF_UP | IFF_POINTOPOINT));
        assert!(allowed_interface("en0", IFF_UP));
    }
    #[test]
    fn host_cap_dedup_and_small_subnets() {
        let (s, h, t) = plan(&[
            iface("10.1.1.1", "255.255.255.0"),
            iface("10.1.1.2", "255.255.255.0"),
            iface("10.1.2.1", "255.255.255.0"),
            iface("10.1.3.1", "255.255.255.0"),
        ]);
        assert_eq!(h.len(), 512);
        assert!(t);
        assert_eq!(s[2].host_count, 0);
        assert_eq!(plan(&[iface("10.77.1.2", "255.255.255.254")]).1.len(), 3);
        assert_eq!(plan(&[iface("10.77.1.2", "255.255.255.255")]).1.len(), 2);
    }
    #[test]
    fn parsers_advertise_without_assuming_loaded() {
        let (m, t) = parse(
            br#"{"data":[{"id":"a"},{"id":"a"},{"id":"b","loaded":false}]}"#,
            Listing::OpenAi,
        )
        .unwrap();
        assert!(!t);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].loaded, None);
        assert_eq!(m[1].loaded, Some(false));
        assert_eq!(
            parse(br#"{"models":[{"name":"local:latest"}]}"#, Listing::Ollama)
                .unwrap()
                .0[0]
                .id,
            "local:latest"
        );
        assert_eq!(Listing::Lemonade.base(), "/api/v1");
        assert_eq!(Listing::OpenAi.base(), "/v1");
        assert!(parse(br#"{"error":"no"}"#, Listing::OpenAi).is_none());
        assert!(parse(&vec![b' '; MAX_BYTES as usize + 1], Listing::OpenAi).is_none());
    }
    #[test]
    fn rejects_ids_that_cannot_roundtrip_through_model_fields() {
        let value = serde_json::json!({"data":[{"id":"a,b"},{"id":" leading"},{"id":"line\nfeed"},{"id":"invisible\u{200b}"},{"id":"valid/model"}]});
        let (models, _) = parse(&serde_json::to_vec(&value).unwrap(), Listing::OpenAi).unwrap();
        assert_eq!(
            models,
            vec![DiscoveryModel {
                id: "valid/model".into(),
                loaded: None
            }]
        );
    }
    #[test]
    fn mixed_lemonade_catalog_excludes_explicit_non_chat_models() {
        let value = serde_json::json!({"data":[{"id":"chat","labels":["llm"]},{"id":"image","labels":["image"]},{"id":"voice","recipe":"kokoro"},{"id":"speech","recipe":"whispercpp"},{"id":"bundle","recipe":"collection.omni"},{"id":"sd","recipe":"sd-cpp"},{"id":"audio","labels":["realtime-transcription"]},{"bad":"row"},{"id":"unknown"}]});
        let (models, _) = parse(&serde_json::to_vec(&value).unwrap(), Listing::Lemonade).unwrap();
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["chat", "unknown"]
        );
        let generic = serde_json::json!({"data":[{"id":"unknown","labels":["tts"]}]});
        assert!(parse(&serde_json::to_vec(&generic).unwrap(), Listing::OpenAi).is_none());
        let unknown = serde_json::json!({"data":[{"id":"generic-unknown"}]});
        assert!(parse(&serde_json::to_vec(&unknown).unwrap(), Listing::OpenAi).is_some());
    }
    #[test]
    fn model_cap_and_pre_cancel_without_network() {
        let rows: Vec<_> = (0..200)
            .map(|i| serde_json::json!({"id":format!("m{i}")}))
            .collect();
        let (m, t) = parse(
            &serde_json::to_vec(&serde_json::json!({"data":rows})).unwrap(),
            Listing::Lemonade,
        )
        .unwrap();
        assert_eq!(m.len(), MAX_MODELS);
        assert!(t);
        let report = scan(&AtomicBool::new(true), |_| {
            panic!("cancelled scan must not start")
        });
        assert!(report.cancelled);
        assert_eq!(report.total, 0);
    }
}
