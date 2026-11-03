//! RC1-F4b: whether the in-app engine's model runs at home.
//!
//! The graduation ledger books an in-app ("On this Mac") run's tokens as
//! *local* only when the engine's `base_url` names this Mac or the home
//! network/fleet; anything else (a cloud URL, a public address, an unparsable
//! value) keeps today's "unknown" booking.

use std::net::{Ipv4Addr, Ipv6Addr};

/// The URL the engine's chat request goes to, as a string.
pub(crate) fn request_url(base_url: &str) -> String {
    awili_seam::chat_completions_url(base_url)
}

/// RC1-F4b-FIX1: the host the in-app engine's HTTP client connects to: the
/// transport's own request URL ([`awili_seam::chat_completions_url`]) parsed
/// by `reqwest::Url`, the type reqwest sends. Backslashes, userinfo,
/// percent-encoding, numeric IPv4 forms and IPv6 brackets therefore resolve
/// exactly as they do for the request. `None` when the URL does not parse.
pub(crate) fn engine_host(base_url: &str) -> Option<url::Host<String>> {
    let url = reqwest::Url::parse(&awili_seam::chat_completions_url(base_url)).ok()?;
    url.host().map(|h| h.to_owned())
}

pub(crate) fn v4_is_home(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    // Loopback, RFC 1918, link-local, and the 100.64/10 range the fleet's
    // tailnet uses.
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || (a == 100 && (64..128).contains(&b))
}

fn v6_is_home(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    // Loopback, unique-local fc00::/7, link-local fe80::/10.
    ip.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
}

/// `true` when `base_url`'s host ([`engine_host`]) is this Mac or the home
/// network: loopback, `localhost`, an RFC 1918 / link-local / tailnet
/// address, a name under `.local`, `.internal`, `.lan`, `.home.arpa` or
/// `.ts.net`, or a single-label fleet name (`hosta`, `hoste`: no public
/// name has one label).
pub(crate) fn engine_is_local(base_url: &str) -> bool {
    match engine_host(base_url) {
        Some(url::Host::Ipv4(v4)) => v4_is_home(v4),
        Some(url::Host::Ipv6(v6)) => v6_is_home(v6),
        Some(url::Host::Domain(host)) => domain_is_home(&host),
        None => false,
    }
}

pub(crate) fn domain_is_home(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return false;
    }
    host == "localhost"
        || !host.contains('.')
        || [
            ".localhost",
            ".local",
            ".internal",
            ".lan",
            ".home.arpa",
            ".ts.net",
        ]
        .iter()
        .any(|suffix| host.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::engine_is_local;

    #[test]
    fn rc1f4b_engine_address_is_local_only_at_home() {
        for url in [
            "http://127.0.0.1:8844",
            "http://localhost:11434/v1",
            "http://[::1]:8080/v1",
            "http://10.77.68.94:8844",
            "http://10.0.0.5",
            "http://172.16.3.4:9000",
            "http://172.31.255.1",
            "http://169.254.1.2",
            "http://100.101.102.103:8000",
            "http://[fd12:3456::1]:8000",
            "http://hoste.local:8844",
            "http://spark.internal:8000/v1",
            "http://router.lan",
            "http://nas.home.arpa",
            "https://hostb.tail-example.ts.net",
            "http://hosta:8000",
            "HTTP://hoste.Local:8844",
            "http://user@10.77.1.2:80",
        ] {
            assert!(engine_is_local(url), "{url} is at home");
        }
        for url in [
            "",
            "10.77.68.94:8844",
            "https://api.openai.com/v1",
            "https://api.deepseek.com",
            "http://8.8.8.8",
            "http://172.32.0.1",
            "http://100.128.0.1",
            "http://192.169.1.1",
            "http://[2001:db8::1]:8000",
            "https://example.local.evil.com",
            "https://evil.com/?h=10.77.1.1",
            "http://10.77.1.2.evil.com",
        ] {
            assert!(!engine_is_local(url), "{url:?} is not at home");
        }
    }

    /// RC1-F4b-FIX1 (Codex T2 P1 on 0925578): the classifier and the HTTP
    /// client agree on the host for every trick: backslash, userinfo with
    /// `@`, percent-encoding, numeric IPv4 forms, IPv6 brackets.
    #[test]
    fn rc1f4b_fix1_locality_reads_the_host_the_client_connects_to() {
        let client = reqwest::blocking::Client::new();
        let request_host = |base: &str| {
            client
                .post(awili_seam::chat_completions_url(base))
                .build()
                .ok()
                .and_then(|r| r.url().host_str().map(str::to_string))
        };
        let cases = [
            ("http://evil.com\\@10.77.1.2:8844", false),
            ("http://evil.com\\@10.77.1.2:8844/v1", false),
            ("http://10.77.1.2\\@evil.com:8844", true),
            ("http://user:pw@evil.com@10.77.1.2:8844", true),
            ("http://evil.com%2f@10.77.1.2:8844", true),
            ("http://evil.com%4010.77.1.2:8844", false),
            ("http://%31%39%32.168.1.2:8844", true),
            ("http://0xc0a80102:8844", true),
            ("http://3232235778:8844", true),
            ("http://[::1]:8080", true),
            ("http://[::ffff:c0a8:102]:8080", false),
            ("http://[2001:db8::1]:8080", false),
            ("http://10.77.1.2.nip.io:8844", false),
            ("http://hoste.LOCAL:8844", true),
            ("https://api.openai.com/v1", false),
        ];
        for (base, local) in cases {
            assert_eq!(engine_is_local(base), local, "{base}");
            assert_eq!(
                super::engine_host(base).map(|h| h.to_string()),
                request_host(base),
                "classifier and HTTP client see one host for {base}"
            );
        }
    }
}
