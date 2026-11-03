#!/usr/bin/env bash
#
# check-mac.sh — the macOS-side validation gate for the voice slice (review SF7).
#
# Linux CI type-checks everything EXCEPT the mac-only capture path (cpal/
# CoreAudio), the whisper `metal` feature, and the bundle/TCC machinery. Linux
# green is therefore NOT evidence the primary path works. This script is the
# automated half of the mac gate — run it on the target Mac (or a macOS CI
# runner; see .github/workflows/macos.yml):
#
#   1. cargo build --release --locked  — compiles cpal/CoreAudio + whisper-rs(metal)
#   2. cargo test --workspace --locked — the full suite on the actual target
#   3. clippy design gate (slice 2)    — -D warnings + -D clippy::disallowed_methods
#                                        on awili-app + awili-design-lint (raw
#                                        gpui::rgb/rgba/hsla calls are errors)
#   4. awili-design-lint check         — baseline ratchet: fail on any raw
#                                        colour/size finding not in
#                                        design-baseline.toml; the baseline
#                                        is additionally checked --reference
#                                        against the one committed on
#                                        origin/main (skip with a note if
#                                        origin/main has no baseline yet)
#   5. scripts/bundle-mac.sh        — bundle + plutil -lint + strict codesign
#                                     verify + effective-entitlement check
#
# It then prints the MANUAL accessibility checklist the slice spec keeps as a
# hard gate (mic prompt, relaunch persistence, dictation feel, Stop) — those
# cannot be asserted from a script and must be smoke-tested by hand.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "check-mac.sh: this is the macOS gate and must run on macOS (got $(uname -s))." >&2
  exit 1
fi

echo "==> [1/5] cargo build --release --locked (mac target: cpal + whisper metal)"
( cd "$ROOT" && cargo build --release --locked )

echo "==> [2/5] cargo test --workspace --locked"
( cd "$ROOT" && cargo test --workspace --locked )

echo "==> [3/5] clippy design gate (-D warnings -D clippy::disallowed_methods)"
( cd "$ROOT" && cargo clippy --locked -p awili-app -p awili-design-lint --all-targets -- -D warnings -D clippy::disallowed_methods )

echo "==> [4/5] awili-design-lint check (baseline ratchet)"
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

echo "==> [5/5] bundle + signature/entitlement validation"
"$SCRIPT_DIR/bundle-mac.sh" --no-build

cat <<'CHECKLIST'

All automated mac checks passed.

MANUAL gate (slice-3 spec — do these by hand on this Mac):
  [ ] open target/release/bundle.noindex/Awili.app  → the mic permission prompt appears
  [ ] grant, dictate a sentence        → ghost partials stream, click-stop commits
  [ ] quit and relaunch (same build)   → NO re-prompt (TCC grant persisted)
  [ ] rebuild + re-bundle + relaunch   → re-prompt EXPECTED (ad-hoc identity)
  [ ] read-aloud a reply, press ■ Stop → speech cuts off mid-sentence
CHECKLIST
