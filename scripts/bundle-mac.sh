#!/usr/bin/env bash
#
# bundle-mac.sh — package the release binary into Awili.app (slice-3 spec §4).
#
# The bundle exists for ONE reason: a bare Mach-O binary cannot prompt for
# microphone access. TCC only shows the mic permission dialog for a real app
# bundle that (a) declares NSMicrophoneUsageDescription in its Info.plist and
# (b) carries a code signature with a stable identifier. We use an *ad-hoc*
# signature (`-`) so no Developer ID / provisioning is needed — enough for local
# dev on the signing machine. Re-run after every `cargo build --release`.
#
# N1b: native notifications (UNUserNotificationCenter) also need the bundle.
# macOS 26+ may refuse them for an ad-hoc signature (Awili then falls back to
# Script Editor notifications). Set AWILI_CODESIGN_IDENTITY to sign with an
# identity instead, e.g.
#   AWILI_CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"
# The default stays ad-hoc.
#
# Usage: scripts/bundle-mac.sh [--no-build] [--install]
#   --no-build   skip the cargo build (bundle an existing binary)
#   --install    also install the bundle (backups first; see the --install block)
#
# Output: target/release/bundle.noindex/Awili.app
set -euo pipefail

# --- locate the repo root (this script's parent dir's parent) ----------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

APP_NAME="Awili"
BIN_NAME="awili-app"
# Dev bundle id by default. --release switches to the public id (see below).
BUNDLE_ID="dev.awili.AwiliDev"
# Honor CARGO_TARGET_DIR (worktrees keep targets off the internal disk).
# NOTE: when CARGO_TARGET_DIR is set this also moves the dev --install source
# (MCP_BIN / bundle) to that dir; that is intended.
RELEASE_DIR="${CARGO_TARGET_DIR:-$ROOT/target}/release"
# Where the built binaries live. release-mac.sh points this at a lipo-merged
# universal directory; default is cargo's release dir.
BIN_DIR="${AWILI_BIN_DIR:-$RELEASE_DIR}"
# The bundle lives under a .noindex parent so Spotlight/Launchpad never list
# the BUILD-TREE copy next to the installed ~/Applications app (Owner saw
# "two copies of the app", 2026-08-22). .noindex on the parent excludes the
# whole subtree from indexing; the installed copy is unaffected.
BUNDLE_DIR="$RELEASE_DIR/bundle.noindex"
# The bundle version is the workspace version (RC1 E1: Info.plist must say what
# Cargo.toml says, e.g. 0.2.0-rc.1) — never a literal in this script.
APP_VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' "$ROOT/Cargo.toml" | head -1)"
[[ -n "$APP_VERSION" ]] || { echo "bundle-mac.sh: could not read [workspace.package].version from Cargo.toml" >&2; exit 1; }
# Apple wants both plist version keys as period-separated integers, so a
# pre-release like 0.2.0-rc.1 cannot go in verbatim: CFBundleShortVersionString
# gets the numeric core (0.2.0), CFBundleVersion the core plus the pre-release
# number as a fourth component (0.2.0.1; a plain release gets .0), and the
# exact Cargo string travels in the custom key AwiliWorkspaceVersion — that is
# the key `Info.plist == Cargo.toml` (RC1 E1) is checked against.
# The fourth CFBundleVersion component orders every pre-release stage below
# the release and keeps stages apart: alpha.N -> 1000+N, beta.N -> 2000+N,
# rc.N -> 3000+N, any other tag -> a flat 5000, the release itself -> 9000. So
# 0.2.0-beta.1 = 0.2.0.2001 < 0.2.0-rc.1 = 0.2.0.3001 < 0.2.0 = 0.2.0.9000.
VERSION_CORE="${APP_VERSION%%-*}"
if [[ "$APP_VERSION" == *-* ]]; then
  PRE="${APP_VERSION#*-}"
  # Only the exact forms alpha.N / beta.N / rc.N (N < 1000, base-10 — a
  # leading zero is not octal) get a stage; any other tag is 5000 flat.
  if [[ "$PRE" =~ ^(alpha|beta|rc)\.([0-9]{1,3})$ ]]; then
    case "${BASH_REMATCH[1]}" in
      alpha) STAGE=1000 ;;
      beta)  STAGE=2000 ;;
      rc)    STAGE=3000 ;;
    esac
    BUILD_N=$((STAGE + 10#${BASH_REMATCH[2]}))
  else
    BUILD_N=5000
  fi
else
  BUILD_N=9000
fi
BUNDLE_VERSION="$VERSION_CORE.$BUILD_N"
[[ "$VERSION_CORE" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]] || { echo "bundle-mac.sh: version core '$VERSION_CORE' is not numeric dot-separated" >&2; exit 1; }
APP="$BUNDLE_DIR/$APP_NAME.app"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "bundle-mac.sh: this packages a macOS .app and must run on macOS (got $(uname -s))." >&2
  exit 1
fi

# --- build -------------------------------------------------------------------
# --install also replaces ~/Applications/Awili.app with this build.
# WHY THIS EXISTS (2026-08-24): both bundles carry the same bundle id, so
# LaunchServices resolves `open <any Awili.app>` to the INSTALLED copy —
# and if one is already running it is merely activated. A whole debugging
# session was spent staring at a two-day-old installed build while a fresh
# one sat unused in the build tree. Verify against what you actually ship.
INSTALL=0
NO_BUILD=0
RELEASE=0
for a in "$@"; do
  case "$a" in
    --install)  INSTALL=1 ;;
    --no-build) NO_BUILD=1 ;;
    --release)  RELEASE=1 ;;
    *) echo "bundle-mac.sh: unknown argument '$a' (expected --no-build, --install and/or --release)" >&2; exit 2 ;;
  esac
done

# --release: the public, Developer ID-signed, notarizable bundle (driven by
# release-mac.sh). It uses a release bundle id distinct from the dev one so an
# installed dev app keeps its own TCC grants / notification identity. Data paths
# (Application Support/Awili) and the keychain service (secrets.rs
# SECRET_SERVICE = dev.awili.Awili) do not derive from the bundle id.
if [[ "$RELEASE" == "1" ]]; then
  [[ "$INSTALL" == "0" ]] || { echo "bundle-mac.sh: --release and --install are mutually exclusive" >&2; exit 2; }
  BUNDLE_ID="dev.awili.Awili"
  BUNDLE_DIR="$RELEASE_DIR/bundle-release.noindex"
  APP="$BUNDLE_DIR/$APP_NAME.app"
  case "${AWILI_CODESIGN_IDENTITY:-}" in
    "Developer ID Application:"*) ;;
    *) echo "bundle-mac.sh: --release needs AWILI_CODESIGN_IDENTITY=\"Developer ID Application: ...\"" >&2; exit 2 ;;
  esac
fi

# `--install` alone builds first; only an explicit --no-build skips the build.
if [[ "$NO_BUILD" == "0" ]]; then
  # Both shipped binaries, from the lockfile (RC1 E1): the app and the
  # connectors MCP server that --install places in ~/.local/bin.
  # U3d D9: bake the short commit hash into the binary (the About dialog's
  # "commit" line reads it via the AWILI_GIT_SHA build-time env); dev builds
  # without the env show "dev build" instead.
  GIT_SHA="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo dev)"
  echo "==> cargo build --release --locked -p $BIN_NAME -p awili-connectors-mcp (AWILI_GIT_SHA=$GIT_SHA)"
  ( cd "$ROOT" && AWILI_GIT_SHA="$GIT_SHA" cargo build --release --locked -p "$BIN_NAME" -p awili-connectors-mcp )
fi

if [[ ! -x "$BIN_DIR/$BIN_NAME" ]]; then
  echo "bundle-mac.sh: release binary not found at $BIN_DIR/$BIN_NAME" >&2
  echo "               run without --no-build, or 'cargo build --release' first." >&2
  exit 1
fi

mkdir -p "$BUNDLE_DIR"

# --- assemble the bundle -----------------------------------------------------
echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

# The executable. CFBundleExecutable below must match this filename.
cp "$BIN_DIR/$BIN_NAME" "$APP/Contents/MacOS/$APP_NAME"
chmod +x "$APP/Contents/MacOS/$APP_NAME"

# --release: the connectors MCP server ships INSIDE the bundle, next to the
# executable — connectors_mcp::resolve_binary looks in current_exe()'s dir
# first, so a DMG user needs no ~/.local/bin install.
if [[ "$RELEASE" == "1" ]]; then
  [[ -x "$BIN_DIR/awili-connectors-mcp" ]] || { echo "bundle-mac.sh: $BIN_DIR/awili-connectors-mcp missing" >&2; exit 1; }
  cp "$BIN_DIR/awili-connectors-mcp" "$APP/Contents/MacOS/awili-connectors-mcp"
  chmod +x "$APP/Contents/MacOS/awili-connectors-mcp"
fi

# The app icon — copied from the canonical source in assets/. CFBundleIconFile
# below names it (without the .icns extension). Warn (don't fail) if it's missing
# so a stripped checkout still bundles a runnable, if icon-less, app.
ICON_SRC="$ROOT/assets/Awili.icns"
if [[ -f "$ICON_SRC" ]]; then
  cp "$ICON_SRC" "$APP/Contents/Resources/Awili.icns"

  # Assert the FULL resolution stack is present, not just that a file exists.
  # An .icns missing a size still bundles, signs, and launches — macOS silently
  # rescales a neighbouring size and the icon just reads soft in that one slot,
  # which no other check would catch. The brand pack's own pre-compiled
  # awili.icns ships with exactly this defect (7 entries, no 64px = 32x32@2x),
  # which is why assets/Awili.icns is COMPILED from the .iconset instead of
  # copied. See assets/BRAND-NOTES.md.
  ICON_CHECK="$(mktemp -d)"
  if iconutil -c iconset -o "$ICON_CHECK/probe.iconset" "$ICON_SRC" 2>/dev/null; then
    n=$(find "$ICON_CHECK/probe.iconset" -name '*.png' | wc -l | tr -d ' ')
    if [[ "$n" -lt 10 ]]; then
      echo "bundle-mac.sh: app icon has $n/10 resolutions — the stack is incomplete." >&2
      echo "               rebuild it with:" >&2
      echo "               iconutil -c icns -o assets/Awili.icns assets/brand/app-icons/macos/awili.iconset" >&2
      rm -rf "$ICON_CHECK"
      exit 1
    fi
    echo "    icon: $n/10 resolutions present"
  else
    echo "bundle-mac.sh: WARNING — could not read $ICON_SRC as an icns." >&2
  fi
  rm -rf "$ICON_CHECK"
else
  echo "bundle-mac.sh: WARNING — icon not found at $ICON_SRC; bundle will have no icon." >&2
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>Awili</string>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundleIconFile</key>
    <string>Awili</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleVersion</key>
    <string>$BUNDLE_VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION_CORE</string>
    <key>AwiliWorkspaceVersion</key>
    <string>$APP_VERSION</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <!-- The string the TCC microphone-permission dialog shows the user. -->
    <key>NSMicrophoneUsageDescription</key>
    <string>Awili uses the microphone for hands-free voice dictation.</string>
    <key>NSLocalNetworkUsageDescription</key>
    <string>Awili connects to your chosen local models and configured services on your network.</string>
    <!-- N1: notifications arrive while the owner is away, so they persist
         (alert) until dismissed; changeable per app in System Settings. -->
    <key>NSUserNotificationAlertStyle</key>
    <string>alert</string>
</dict>
</plist>
PLIST

# Lint the plist before signing — a malformed plist signs fine but TCC then
# silently refuses to prompt (review SF7).
plutil -lint "$APP/Contents/Info.plist"

# --- ad-hoc code signature ---------------------------------------------------
# Entitlements grant mic access under the hardened runtime; harmless without it.
ENTITLEMENTS="$(mktemp -t awili-entitlements).plist"
cat > "$ENTITLEMENTS" <<'ENT'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.device.audio-input</key>
    <true/>
</dict>
</plist>
ENT

# N1b: ad-hoc by default; AWILI_CODESIGN_IDENTITY opts in to an identity.
SIGN_IDENTITY="${AWILI_CODESIGN_IDENTITY:--}"
if [[ "$SIGN_IDENTITY" == "-" ]]; then
  echo "==> codesign (ad-hoc)"
else
  echo "==> codesign (identity: $SIGN_IDENTITY)"
fi
if [[ "$RELEASE" == "1" ]]; then
  # Hardened runtime + secure timestamp (both required by notarization).
  # Entitlements: ONLY audio-input (microphone dictation under the hardened
  # runtime). No allow-jit / allow-unsigned-executable-memory /
  # disable-library-validation: nothing JITs (no MAP_JIT/dlopen of third-party
  # code in the tree; whisper.cpp + Metal are ahead-of-time compiled). Add one
  # only if a notarized build is observed to need it.
  # Sign inside-out: every Mach-O under Contents except the main executable
  # (helpers, any dylibs/frameworks) first, each with its own identifier and
  # the hardened runtime but NO entitlements; then the bundle (main exe).
  while IFS= read -r f; do
    [[ "$f" == "$APP/Contents/MacOS/$APP_NAME" ]] && continue
    if file -b "$f" | grep -q 'Mach-O'; then
      echo "    signing nested: ${f#$APP/}"
      codesign --force --sign "$SIGN_IDENTITY" --options runtime --timestamp "$f"
    fi
  done < <(find "$APP/Contents" -type f | sort)
  codesign --force --sign "$SIGN_IDENTITY" --options runtime --timestamp --entitlements "$ENTITLEMENTS" "$APP"
else
# No --deep: the bundle holds a single executable and nothing nested; --deep is
# deprecated and only papers over unsigned nested code we don't have.
codesign --force --sign "$SIGN_IDENTITY" --timestamp=none --entitlements "$ENTITLEMENTS" "$APP"
fi
rm -f "$ENTITLEMENTS"

# --- verification (review SF7) ----------------------------------------------
# Strict bundle verification, then prove the EFFECTIVE entitlements carry the
# audio-input grant — signing can succeed while silently dropping entitlements,
# and then the mic permission prompt never appears.
echo "==> codesign --verify --strict"
codesign --verify --strict --verbose=2 "$APP" || {
  echo "bundle-mac.sh: strict signature verification failed" >&2
  exit 1
}

echo "==> checking effective entitlements"
if ! codesign -d --entitlements :- "$APP" 2>/dev/null \
    | grep -q "com.apple.security.device.audio-input"; then
  echo "bundle-mac.sh: signed bundle is missing the audio-input entitlement" >&2
  exit 1
fi

if [[ "$RELEASE" == "1" ]]; then
  echo "==> release checks (hardened runtime, timestamp)"
  codesign --verify --deep --strict --verbose=2 "$APP"
  DV="$(codesign -dvv "$APP" 2>&1)"   # captured: grep -q under pipefail would SIGPIPE codesign
  grep -q 'flags=0x10000(runtime)' <<<"$DV" || { echo "bundle-mac.sh: hardened runtime flag missing" >&2; exit 1; }
  grep -q '^Timestamp=' <<<"$DV" || { echo "bundle-mac.sh: secure timestamp missing" >&2; exit 1; }
fi

echo "==> signature identity"
codesign -dv "$APP" 2>&1 | grep -E '^(Authority|TeamIdentifier|Signature)=' | sed 's/^/    /' || true

echo "==> done: $APP"
if [[ "$RELEASE" == "1" ]]; then exit 0; fi  # the dev-signing notes below do not apply
echo "    open with:  open \"$APP\""
echo "    (first launch will prompt for microphone access)"
echo
echo "    NOTE: an ad-hoc signature has no stable team identity, so TCC ties the"
echo "    mic grant to this exact binary — REBUILDING re-prompts. That is expected"
echo "    for local dev. Run scripts/check-mac.sh for the full mac-side gate."
echo "    NOTE: ad-hoc may be refused for native notifications on macOS 26+ (Awili"
echo "    then falls back to Script Editor notifications); set"
echo "    AWILI_CODESIGN_IDENTITY to sign with an identity."

if [[ "$INSTALL" == "1" ]]; then
  # Preserve the install scope already in use. This Mac historically installed
  # AwiliDev system-wide; default to the per-user Applications folder only when
  # neither system-wide bundle exists.
  if [[ -d "/Applications/AwiliDev.app" || -d "/Applications/Awili.app" ]]; then
    INSTALL_ROOT="/Applications"
  else
    INSTALL_ROOT="$HOME/Applications"
  fi
  [[ -w "$INSTALL_ROOT" || ! -e "$INSTALL_ROOT" ]] || {
    echo "bundle-mac.sh: $INSTALL_ROOT is not writable; cannot migrate the installed app" >&2
    exit 1
  }
  DEST="$INSTALL_ROOT/$APP_NAME.app"
  STAGE="$INSTALL_ROOT/.$APP_NAME.app.staging"
  PREV="$INSTALL_ROOT/.$APP_NAME.app.previous"
  for LEGACY in "$INSTALL_ROOT/AwiliDev.app" "$HOME/Applications/AwiliDev.app"; do
    if [[ -d "$LEGACY" && ! -w "$(dirname "$LEGACY")" ]]; then
      echo "bundle-mac.sh: cannot move legacy app at $LEGACY" >&2
      exit 1
    fi
  done
  echo "==> installing to $DEST"
  # Everything this install ships must exist BEFORE anything is replaced, so
  # a missing piece cannot leave a half-upgraded machine.
  MCP_BIN="$BIN_DIR/awili-connectors-mcp"
  [[ -x "$MCP_BIN" ]] || {
    echo "bundle-mac.sh: $MCP_BIN is missing — nothing installed; run 'cargo build --release --locked -p awili-connectors-mcp' (or bundle-mac.sh without --no-build) and re-run --install" >&2
    exit 1
  }
  mkdir -p "$INSTALL_ROOT"
  # RC1 E4: every install first backs up what it replaces — the sqlite DB (with
  # its -wal/-shm sidecars) and, below, the previous bundle — under one
  # timestamped directory. Prior apps are verified ZIP archives: unpacking one
  # with ditto -x -k restores it without leaving extra launchable apps around.
  # The DB is WAL-mode: copying awili.db and awili.db-wal separately while the
  # app writes can pair a database with a WAL from a different moment. Refuse
  # while Awili runs (before anything is created), then take the copy through
  # SQLite's online backup API, which yields one consistent file (sqlite3
  # ships with macOS).
  if pgrep -xq "$APP_NAME"; then
    echo "bundle-mac.sh: $APP_NAME is running — quit it before --install so the DB backup is consistent" >&2
    exit 1
  fi
  BACKUP_DIR="$HOME/Library/Application Support/Awili/backups"
  SNAP="$BACKUP_DIR/$(date +%Y%m%d-%H%M%S)-$$"
  if [[ -e "$SNAP" ]]; then
    echo "bundle-mac.sh: snapshot dir $SNAP already exists; refusing to overwrite a backup" >&2
    exit 1
  fi
  mkdir -p "$SNAP"
  # A previous run interrupted after moving the installed app aside leaves it
  # at $PREV: that is the only copy of the previous bundle, so it goes into
  # this snapshot instead of being deleted below.
  if [[ -d "$PREV" ]]; then
    mv "$PREV" "$SNAP/$APP_NAME.app.interrupted"
    echo "    kept an interrupted install's previous bundle at $SNAP/$APP_NAME.app.interrupted"
  fi
  DB="$HOME/Library/Application Support/Awili/awili.db"
  if [[ -f "$DB" ]]; then
    sqlite3 "$DB" ".backup '$SNAP/awili.db'" || { echo "bundle-mac.sh: sqlite3 .backup failed" >&2; exit 1; }
    echo "    backed up DB (sqlite3 .backup) to $SNAP/awili.db"
  fi
  # Hashes for the record and for rollback checks: what was installed before
  # and what this run ships.
  [[ -x "$DEST/Contents/MacOS/$APP_NAME" ]] && shasum -a 256 "$DEST/Contents/MacOS/$APP_NAME" > "$SNAP/previous-executable.sha256"
  shasum -a 256 "$APP/Contents/MacOS/$APP_NAME" > "$SNAP/installed-executable.sha256"
  rm -rf "$STAGE" "$PREV"
  ditto "$APP" "$STAGE"
  codesign --verify --strict "$STAGE" >/dev/null 2>&1 || {
    rm -rf "$STAGE"
    echo "bundle-mac.sh: staged app failed signature verification" >&2
    exit 1
  }
  # Connectors server first (the smaller, more failure-prone step: a PATH dir
  # permission or a busy binary); the app is only swapped once it is in place,
  # and if the swap then fails the old connector copy is put back too.
  MCP_DEST="$HOME/.local/bin/awili-connectors-mcp"
  mkdir -p "$HOME/.local/bin"
  HAD_MCP=0
  [[ -x "$MCP_DEST" ]] && { cp -p "$MCP_DEST" "$SNAP/awili-connectors-mcp"; HAD_MCP=1; }
  # Put ~/.local/bin back the way it was: the saved copy when there was one,
  # otherwise no connector at all (a half-written new one must not stay).
  # Returns non-zero — and says so — when the restore itself fails, so the
  # installer never claims a restore it could not verify.
  restore_connector() {
    if [[ "$HAD_MCP" == "1" && -f "$SNAP/awili-connectors-mcp" ]]; then
      if install -m755 "$SNAP/awili-connectors-mcp" "$MCP_DEST" \
        && cmp -s "$SNAP/awili-connectors-mcp" "$MCP_DEST"; then
        echo "    restored the previous awili-connectors-mcp from $SNAP" >&2
      else
        echo "bundle-mac.sh: FAILED to restore awili-connectors-mcp to $MCP_DEST — restore it by hand from $SNAP/awili-connectors-mcp" >&2
        return 1
      fi
    else
      rm -f "$MCP_DEST" || { echo "bundle-mac.sh: FAILED to remove the half-installed $MCP_DEST" >&2; return 1; }
    fi
  }
  if ! install -m755 "$MCP_BIN" "$MCP_DEST"; then
    restore_connector || true
    rm -rf "$STAGE"
    echo "bundle-mac.sh: could not install awili-connectors-mcp to $MCP_DEST; app untouched" >&2
    exit 1
  fi
  echo "    installed awili-connectors-mcp to ~/.local/bin"
  if [[ -d "$DEST" ]] && ! mv "$DEST" "$PREV"; then
    restore_connector || true
    rm -rf "$STAGE"
    echo "bundle-mac.sh: could not move the installed $APP_NAME.app aside; app untouched" >&2
    exit 1
  fi
  if mv "$STAGE" "$DEST"; then
    if [[ -d "$PREV" ]]; then
      mv "$PREV" "$SNAP/$APP_NAME.app"
      /usr/bin/python3 "$SCRIPT_DIR/archive-mac-app.py" --source "$SNAP/$APP_NAME.app" --archive "$SNAP/$APP_NAME.app.zip"
      echo "    previous bundle archived at $SNAP/$APP_NAME.app.zip"
    fi
    if [[ -d "$SNAP/$APP_NAME.app.interrupted" ]]; then
      /usr/bin/python3 "$SCRIPT_DIR/archive-mac-app.py" --source "$SNAP/$APP_NAME.app.interrupted" --archive "$SNAP/$APP_NAME.app.interrupted.zip"
    fi
  else
    # Restore whatever previous bundle exists: this run's, else the one an
    # interrupted earlier run left and this run moved into the snapshot.
    APP_RESTORED=0
    if [[ -d "$PREV" ]]; then
      mv "$PREV" "$DEST" && APP_RESTORED=1 || echo "bundle-mac.sh: FAILED to move the previous bundle back from $PREV — restore it by hand" >&2
    elif [[ -d "$SNAP/$APP_NAME.app.interrupted" ]]; then
      mv "$SNAP/$APP_NAME.app.interrupted" "$DEST" && APP_RESTORED=1 || echo "bundle-mac.sh: FAILED to move the previous bundle back from $SNAP/$APP_NAME.app.interrupted — restore it by hand" >&2
    fi
    restore_connector || true
    if [[ "$APP_RESTORED" == "1" ]]; then
      echo "bundle-mac.sh: install failed; the previous $APP_NAME.app was restored (connector status above)" >&2
    else
      echo "bundle-mac.sh: install failed and the previous $APP_NAME.app was NOT restored — see the FAILED line above (connector status above)" >&2
    fi
    exit 1
  fi
  LEGACY_INDEX=0
  for LEGACY in "$INSTALL_ROOT/AwiliDev.app" "$HOME/Applications/AwiliDev.app"; do
    if [[ -d "$LEGACY" ]]; then
      LEGACY_INDEX=$((LEGACY_INDEX + 1))
      /usr/bin/python3 "$SCRIPT_DIR/archive-mac-app.py" --source "$LEGACY" --archive "$BACKUP_DIR/AwiliDev-before-rename-$(date +%Y%m%d-%H%M%S)-$$-$LEGACY_INDEX.zip"
      echo "    archived legacy $LEGACY out of Applications"
    fi
  done
  echo "    installed: $(stat -f '%Sm' "$DEST/Contents/MacOS/$APP_NAME")"
  shasum -a 256 "$DEST/Contents/MacOS/$APP_NAME" | sed 's/^/    sha256 /'
  # The installed executable was verified above. Do not retain a second app
  # bundle in the build tree after a successful install.
  cmp -s "$APP/Contents/MacOS/$APP_NAME" "$DEST/Contents/MacOS/$APP_NAME"
  /usr/bin/python3 "$SCRIPT_DIR/archive-mac-app.py" --source "$APP" --unregister-only
  rm -rf "$APP"
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$DEST"
  # The connectors MCP server is not inside the bundle (P12 deviations); the app
  # resolves it from the agent search paths, so it is installed beside the
  # vendor CLIs in ~/.local/bin when this build produced it.
fi
