//! P3e: the connection card and its Test button recognise a gateway-only
//! setup. Pure helpers only — the card and the button both route through them.

use super::{
    connection_label, connection_test_target, gateway_connection_test, ConnectionTestTarget,
    CONNECTION_NOT_CONFIGURED,
};

const REST: &str = "http://127.0.0.1:8642/v1";
const DASH: &str = "http://127.0.0.1:9119";

/// Round 2: a configured gateway wins over an enabled REST Hermes — the
/// transport Work actually routes through — so the card names the gateway,
/// and loaded skills do not turn it back into a REST summary.
#[test]
fn label_prefers_gateway_when_both_are_set() {
    assert_eq!(
        connection_label(true, REST, DASH, 0, 0, 0),
        format!("Gateway: {DASH}")
    );
    assert_eq!(
        connection_label(true, REST, DASH, 3, 2, 7),
        format!("Gateway: {DASH}")
    );
}

/// REST Hermes enabled with no gateway: the skills summary when any are
/// loaded, else `Hermes: <base_url>` — exactly the pre-P3e label.
#[test]
fn label_uses_rest_when_gateway_unset() {
    assert_eq!(
        connection_label(true, REST, "", 0, 0, 0),
        format!("Hermes: {REST}")
    );
    assert_eq!(
        connection_label(true, REST, "", 3, 2, 7),
        "3 skills · 2 toolsets (7 tools)"
    );
    // Toolsets alone still count as "loaded".
    assert_eq!(
        connection_label(true, REST, "", 0, 1, 4),
        "0 skills · 1 toolsets (4 tools)"
    );
}

/// The D1 configuration: `[hermes].base_url` empty, `[gateway].dashboard_url`
/// set. The card names the gateway (whole URL) instead of "not configured",
/// and loaded skills do not turn it back into a REST summary.
#[test]
fn label_names_gateway_when_only_gateway_is_set() {
    assert_eq!(
        connection_label(false, "", DASH, 0, 0, 0),
        format!("Gateway: {DASH}")
    );
    let long = "http://hermes-dashboard.tail-example.ts.net:60142/some/long/path?token=abc";
    assert_eq!(
        connection_label(false, "", long, 5, 2, 9),
        format!("Gateway: {long}")
    );
}

/// Neither transport configured: the actionable not-configured message, never
/// the old `Hermes: not configured`.
#[test]
fn label_says_not_configured_when_neither() {
    let label = connection_label(false, "", "", 0, 0, 0);
    assert_eq!(label, CONNECTION_NOT_CONFIGURED);
    assert_eq!(
        label,
        "Not configured (set [gateway] or [hermes] in config.toml)"
    );
}

/// The Test button probes the transport the label names, for every input.
#[test]
fn test_target_matches_label_precedence() {
    let cases = [
        (true, DASH, ConnectionTestTarget::Gateway),
        (true, "", ConnectionTestTarget::Rest),
        (false, DASH, ConnectionTestTarget::Gateway),
        (false, "", ConnectionTestTarget::None),
    ];
    for (enabled, dash, want) in cases {
        assert_eq!(
            connection_test_target(enabled, dash),
            want,
            "{enabled} {dash:?}"
        );
        let label = connection_label(enabled, REST, dash, 0, 0, 0);
        let prefix = match want {
            ConnectionTestTarget::Rest => "Hermes: ",
            ConnectionTestTarget::Gateway => "Gateway: ",
            ConnectionTestTarget::None => CONNECTION_NOT_CONFIGURED,
        };
        assert!(label.starts_with(prefix), "{label:?} vs {want:?}");
    }
}

/// The gateway probe fetches `<dashboard_url>/` through the injected fetch: a
/// page with the token injection is reachable; a page without it, or a fetch
/// error, comes back verbatim.
#[test]
fn gateway_test_reports_scrape_result() {
    let mut seen = String::new();
    let ok = gateway_connection_test("http://127.0.0.1:9119/", |url| {
        seen = url.to_string();
        Ok(r#"<script>window.__HERMES_SESSION_TOKEN__="x";</script>"#.to_string())
    });
    assert_eq!(ok, Ok("Gateway reachable: session token found".to_string()));
    assert_eq!(seen, "http://127.0.0.1:9119/");

    let missing = gateway_connection_test(DASH, |_| Ok("<html>no token</html>".to_string()));
    assert_eq!(
        missing,
        Err("dashboard token not found in index HTML".to_string())
    );

    let connect_err = "dashboard unreachable at http://127.0.0.1:9119/: connection refused";
    let unreachable = gateway_connection_test(DASH, |_| Err(connect_err.to_string()));
    assert_eq!(unreachable, Err(connect_err.to_string()));
}

/// Round 2: a page that injects an empty session token is not a reachable
/// gateway — the probe reports a distinct error instead of success.
#[test]
fn gateway_test_rejects_empty_token() {
    let empty = gateway_connection_test(DASH, |_| {
        Ok(r#"<script>window.__HERMES_SESSION_TOKEN__="";</script>"#.to_string())
    });
    assert_eq!(empty, Err("dashboard session token is empty".to_string()));
}
