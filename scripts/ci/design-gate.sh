#!/usr/bin/env bash
#
# design-gate.sh — the design ratchet gate (docs/v2/PLAN.md slice 2): from
# this slice on, no new raw colour or size may enter the Awili UI code.
#
# Linux-runnable (unlike check-mac.sh) — the two steps run on any host with
# the Rust toolchain pinned by rust-toolchain.toml:
#
#   1. cargo clippy -p awili-app -p awili-design-lint --all-targets
#        -- -D warnings -D clippy::disallowed_methods
#      — errors on any new call to gpui::rgb/rgba/hsla (see clippy.toml).
#   2. awili-design-lint check crates/awili-app/src --baseline design-baseline.toml
#      — errors on any raw colour/size finding whose fingerprint is not in
#        the baseline. The baseline can only shrink (via --update-baseline),
#        so the debt recorded in slice 2 can only get smaller. It is also
#        checked with --reference against the baseline committed on
#        origin/main: the working-tree file may only SHRINK against it. If
#        origin/main has no baseline yet, that comparison is skipped with a
#        printed note.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

echo "==> [1/2] clippy design gate (-D warnings -D clippy::disallowed_methods)"
( cd "$ROOT" && cargo clippy --locked -p awili-app -p awili-design-lint --all-targets -- -D warnings -D clippy::disallowed_methods )

echo "==> [2/2] awili-design-lint check (baseline ratchet)"
# Shrink-only reference (02b item 9): fetch the baseline committed on
# origin/main into a temp file and pass it as --reference. When origin/main
# has no baseline yet (e.g. this slice, before its merge) the comparison is
# skipped with a note.
REF_ARGS=()
REF_FILE=""
if ( cd "$ROOT" && git rev-parse --verify origin/main >/dev/null 2>&1 ); then
  REF_FILE="$(mktemp)"
  if ( cd "$ROOT" && git show origin/main:design-baseline.toml >"$REF_FILE" ) 2>/dev/null; then
    REF_ARGS=(--reference "$REF_FILE")
    echo "baseline reference: origin/main:design-baseline.toml (shrink-only)"
    trap 'rm -f "$REF_FILE"' EXIT
  else
    rm -f "$REF_FILE"
    REF_FILE=""
    echo "note: origin/main has no design-baseline.toml yet — skipping the shrink-only comparison"
  fi
fi
( cd "$ROOT" && cargo run --locked -q -p awili-design-lint -- check crates/awili-app/src --baseline design-baseline.toml ${REF_ARGS[@]+"${REF_ARGS[@]}"} )

echo "design-gate: ok"
