//! Live proof that a gateway `session.create{cwd}` actually SCOPES server-side
//! tool execution to the chosen folder — not just that the request is accepted.
//!
//! This is the regression guard for the §3 "folder-scope" seam (Awili — Current
//! Architecture doc). The existing unit test only proves `session.create`
//! deserializes; this drives a real run and checks the file lands in `cwd`.
//!
//! It is an EXAMPLE (not a `#[test]`) on purpose: it needs a live Hermes gateway,
//! so it must not run in offline `cargo test`. Run it on demand:
//!
//!   HERMES_HOST=127.0.0.1 HERMES_PORT=9119 \
//!   HERMES_TOKEN=$(curl -fsS http://127.0.0.1:9119/ | grep -o '__HERMES_SESSION_TOKEN__="[^"]*"' | cut -d'"' -f2) \
//!   cargo run -p awili-gateway --example cwd_proof
//!
//! Exits 0 and prints `CWD_PROOF_OK` only if the agent wrote the marker file
//! INSIDE the session cwd and NOT in the process cwd.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use awili_gateway::{gateway_url, GatewayClient};

fn main() {
    let host = std::env::var("HERMES_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("HERMES_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9119);
    let token = std::env::var("HERMES_TOKEN").expect("set HERMES_TOKEN");

    // A fresh, uniquely-named scope dir so a stale file can't fake a pass.
    let marker = format!("cwd-proof-{}", std::process::id());
    let scope: PathBuf = std::env::temp_dir().join(&marker);
    let _ = std::fs::remove_dir_all(&scope);
    std::fs::create_dir_all(&scope).expect("create scope dir");
    let scope_str = scope.to_string_lossy().to_string();
    let proof_file = scope.join("cwd-proof.txt");
    // The same relative filename in the PROCESS cwd — must NOT be written here.
    // RC2-SVC4 (F0683): a leak is judged by CONTENT (this run's unique
    // marker), so a stray file left by an earlier run neither hides a leak
    // nor fakes one.
    let stray = std::env::current_dir().unwrap().join("cwd-proof.txt");

    eprintln!("scope cwd = {scope_str}");
    let ws_url = gateway_url(&host, port, &token);
    let mut client = GatewayClient::connect(&ws_url).expect("connect");

    let session = client.session_create(&scope_str).expect("session.create");
    eprintln!("session: {} (cwd={scope_str})", session.session_id);

    // Relative path on purpose — only honored if the tool runtime is chdir'd to cwd.
    let prompt = format!(
        "Use the shell to write the exact text {marker} into a file named \
         cwd-proof.txt in the current directory (a relative path, no leading \
         slash). Then run pwd. Do nothing else."
    );
    client
        .prompt_submit(&session.session_id, &prompt)
        .expect("prompt.submit");
    eprintln!("submitted; awaiting completion …");

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut done = false;
    while Instant::now() < deadline && !done {
        match client.recv_event(Duration::from_millis(250)) {
            Some(ev) => match ev.event_type.as_str() {
                // Auto-approve any tool gate so the run can proceed unattended.
                "approval.request" => {
                    eprintln!("approval.request -> once");
                    let _ = client.approval_respond(&session.session_id, "once");
                }
                "message.complete" => done = true,
                other => eprintln!("event: {other}"),
            },
            None => {
                if !client.is_connected() {
                    eprintln!("(disconnected)");
                    break;
                }
            }
        }
    }

    let _ = client.session_close(&session.session_id);
    client.close();

    // --- assertions: the file must be IN the scope, NOT in the process cwd ---
    let in_scope = std::fs::read_to_string(&proof_file)
        .map(|s| s.contains(&marker))
        .unwrap_or(false);
    let leaked = leaked_to(&stray, &marker);

    eprintln!("--- result ---");
    eprintln!("proof in scope  : {in_scope} ({})", proof_file.display());
    eprintln!("leaked to cwd   : {leaked} ({})", stray.display());

    let _ = std::fs::remove_dir_all(&scope);
    if leaked {
        let _ = std::fs::remove_file(&stray);
    }

    if in_scope && !leaked {
        println!("CWD_PROOF_OK");
    } else {
        eprintln!("CWD_PROOF_FAIL: cwd scoping did not hold");
        std::process::exit(1);
    }
}

/// RC2-SVC4 (F0683): did THIS run's agent write into `stray`? True only when
/// the file holds this run's unique `marker` — a pre-existing file from an
/// earlier run (another pid, another marker) is not this run's leak, and its
/// presence can no longer mask one: a leak overwrites or appends the marker.
fn leaked_to(stray: &std::path::Path, marker: &str) -> bool {
    std::fs::read_to_string(stray)
        .map(|s| s.contains(marker))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RC2-SVC4 (F0683): a stray file that already existed no longer
    /// suppresses leak detection; only this run's marker counts.
    #[test]
    fn rc2svc4_a_pre_existing_stray_file_does_not_hide_a_leak() {
        let dir = std::env::temp_dir().join(format!("rc2svc4-cwd-proof-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stray = dir.join("cwd-proof.txt");
        assert!(!leaked_to(&stray, "cwd-proof-1"), "no file, no leak");
        std::fs::write(&stray, "cwd-proof-0 from an earlier run").unwrap();
        assert!(
            !leaked_to(&stray, "cwd-proof-1"),
            "an old file is not this run's leak"
        );
        std::fs::write(&stray, "cwd-proof-1").unwrap();
        assert!(
            leaked_to(&stray, "cwd-proof-1"),
            "this run's marker is a leak"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
