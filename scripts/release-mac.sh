#!/usr/bin/env bash
#
# release-mac.sh — Developer ID-signed, notarized, stapled Awili DMG.
#
# Usage: scripts/release-mac.sh [--arch arm64|universal] [--no-build]
#   --arch arm64      (default) native build only
#   --arch universal  also build x86_64-apple-darwin and lipo the two
#   --no-build        reuse binaries already in $CARGO_TARGET_DIR (or ./target)
#
# Environment:
#   AWILI_SIGN_IDENTITY      REQUIRED: your "Developer ID Application: Name (TEAMID)" identity
#   AWILI_NOTARY_PROFILE     REQUIRED: your notarytool keychain profile name
#   AWILI_RELEASE_DIR        output dir, default <repo>/dist
#   CARGO_TARGET_DIR         keep this OFF the internal disk
#   DEVELOPER_DIR            if the system Xcode lacks the Metal toolchain
#
# Flow: build -> bundle-mac.sh --release (nested code signed first, hardened
# runtime, timestamp, release bundle id dev.awili.Awili) -> notarize app zip ->
# staple app -> DMG (+ /Applications symlink) -> sign DMG -> notarize -> staple
# -> spctl / codesign / stapler verification. Never launches or installs the app.
# Output: Awili-<version>-macos-<arch>.dmg + .sha256 + notarytool log JSON.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ARCH=arm64
NO_BUILD=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch) ARCH="${2:-}"; shift 2 ;;
    --no-build) NO_BUILD=1; shift ;;
    *) echo "release-mac.sh: unknown argument '$1'" >&2; exit 2 ;;
  esac
done
[[ "$ARCH" == arm64 || "$ARCH" == universal ]] || { echo "release-mac.sh: --arch must be arm64 or universal" >&2; exit 2; }
[[ "$(uname -s)" == Darwin ]] || { echo "release-mac.sh: macOS only" >&2; exit 1; }

: "${AWILI_SIGN_IDENTITY:?set AWILI_SIGN_IDENTITY to your Developer ID Application identity}"
: "${AWILI_NOTARY_PROFILE:?set AWILI_NOTARY_PROFILE to your notarytool keychain profile}"
export AWILI_CODESIGN_IDENTITY="$AWILI_SIGN_IDENTITY"
PROFILE="$AWILI_NOTARY_PROFILE"
OUT="${AWILI_RELEASE_DIR:-$ROOT/dist}"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' "$ROOT/Cargo.toml" | head -1)"
[[ -n "$VERSION" ]] || { echo "release-mac.sh: no workspace version" >&2; exit 1; }
SHA="$(git -C "$ROOT" rev-parse --short=8 HEAD)"
DMG_NAME="Awili-$VERSION-macos-$ARCH.dmg"
mkdir -p "$OUT"

mkdir -p "$TARGET"
WORK="$(mktemp -d "$TARGET/release-mac.XXXXXX")"   # ours: removed on exit
trap 'rm -rf "$WORK"' EXIT

# --- build -------------------------------------------------------------------
build() { # build <extra cargo args...>
  # Strip build-machine paths (home dir, checkout) from the release binaries.
  # CARGO_ENCODED_RUSTFLAGS is 0x1f-separated, so paths with spaces stay whole.
  local us; us=$(printf '\037')
  ( cd "$ROOT" && AWILI_GIT_SHA="$SHA" \
      CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=~${us}--remap-path-prefix=$ROOT=awili" \
      cargo build --release --locked -p awili-app -p awili-connectors-mcp "$@" )
}
BIN_DIR="$TARGET/release"
if [[ "$NO_BUILD" == 0 ]]; then
  echo "==> build arm64 ($SHA)"; build
  if [[ "$ARCH" == universal ]]; then
    echo "==> build x86_64"; build --target x86_64-apple-darwin
  fi
fi
if [[ "$ARCH" == universal ]]; then
  BIN_DIR="$WORK/universal"; mkdir -p "$BIN_DIR"
  for b in awili-app awili-connectors-mcp; do
    lipo -create "$TARGET/release/$b" "$TARGET/x86_64-apple-darwin/release/$b" -output "$BIN_DIR/$b"
  done
fi

# --- bundle + sign -----------------------------------------------------------
AWILI_BIN_DIR="$BIN_DIR" "$SCRIPT_DIR/bundle-mac.sh" --release --no-build
APP="$TARGET/release/bundle-release.noindex/Awili.app"

# --- notarize + staple the app ----------------------------------------------
notarize() { # notarize <file> <log-name>
  local f="$1" name="$2" res id status
  res="$(xcrun notarytool submit "$f" --keychain-profile "$PROFILE" --wait --output-format json)"
  id="$(printf '%s' "$res" | /usr/bin/plutil -extract id raw -o - - 2>/dev/null || true)"
  status="$(printf '%s' "$res" | /usr/bin/plutil -extract status raw -o - - 2>/dev/null || true)"
  echo "    notarization $name: id=$id status=$status" >&2
  [[ -n "$id" ]] && xcrun notarytool log "$id" --keychain-profile "$PROFILE" "$OUT/notary-log-$name-$VERSION-$ARCH.json" >&2 || true
  [[ "$status" == "Accepted" ]] || { echo "release-mac.sh: notarization of $name was $status — see $OUT/notary-log-$name-$VERSION-$ARCH.json" >&2; exit 1; }
}
echo "==> notarize app"
ditto -c -k --keepParent "$APP" "$WORK/Awili-app.zip"
notarize "$WORK/Awili-app.zip" app
xcrun stapler staple "$APP"

# --- DMG ---------------------------------------------------------------------
echo "==> DMG"
STAGE="$WORK/dmg"; mkdir -p "$STAGE"
ditto "$APP" "$STAGE/Awili.app"
ln -s /Applications "$STAGE/Applications"
# Built in $WORK; moved to $OUT only after every check passes, so a rejected or
# unstapled DMG never sits in the output folder under its final name.
DMG="$WORK/$DMG_NAME"
hdiutil create -volname "Awili $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null
codesign --force --sign "$AWILI_CODESIGN_IDENTITY" --timestamp "$DMG"
echo "==> notarize DMG"
notarize "$DMG" dmg
xcrun stapler staple "$DMG"

# --- verify ------------------------------------------------------------------
echo "==> verify"
spctl -a -vvv -t install "$DMG"
codesign --verify --deep --strict --verbose=2 "$APP"
spctl -a -vvv "$APP"
xcrun stapler validate "$APP"
xcrun stapler validate "$DMG"
rm -f "$OUT/$DMG_NAME"
mv "$DMG" "$OUT/$DMG_NAME"
DMG="$OUT/$DMG_NAME"
( cd "$OUT" && shasum -a 256 "$DMG_NAME" | tee "$DMG_NAME.sha256" )
echo "==> done: $DMG"
