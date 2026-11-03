//! Fixed supervised endpoint: `awili-build-endpoint --config
//! /opt/awili-build/runner.json submit|status|cancel|logs`, one JSON object
//! on stdin, one JSON object on stdout.
use anyhow::{ensure, Result};
use awili_dispatch::endpoint::{Endpoint, CAP, CONFIG};
use std::io::Read;

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Match the fixed SSH client's argv shape; arbitrary config is forbidden.
    ensure!(
        args.len() == 3 && args[0] == "--config" && args[1] == CONFIG,
        "fixed endpoint invocation required"
    );
    let mut raw = Vec::new();
    std::io::stdin()
        .take(CAP as u64 + 1)
        .read_to_end(&mut raw)?;
    ensure!(raw.len() <= CAP, "request too large");
    let payload: serde_json::Value = serde_json::from_slice(&raw)?;
    let value = Endpoint::installed().handle(&args[2], &payload)?;
    println!("{}", serde_json::to_string(&value)?);
    Ok(())
}

pub fn main() {
    if run().is_err() {
        // Subprocess/config errors must not echo spec, environment or credentials.
        eprintln!("supervised endpoint failed; reconcile the same request ID");
        std::process::exit(1);
    }
}
