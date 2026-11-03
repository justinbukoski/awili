#!/bin/sh
# Builds Awili-<version>-linux-x86_64.tar.gz.
# Usage: scripts/package-linux.sh [path-to-awili-app] [output-dir]
#
# With no binary argument it BUILDS the release binary first, with every build
# path mapped to a neutral name so the binary does not carry the builder's home
# folder, checkout, cargo or rustup folders, or target folder:
#
#   CARGO_ENCODED_RUSTFLAGS = remap $HOME=~, <repo>=awili, $CARGO_HOME=~/.cargo,
#                             $RUSTUP_HOME=~/.rustup, <target dir>=target
#   CFLAGS / CXXFLAGS       = the same as -ffile-prefix-map (whisper-rs-sys builds C/C++)
#   cargo build --release -p awili-app --locked
#
# Needs: Linux x86_64, the repo's pinned Rust toolchain, clang 20 for libclang
# (LIBCLANG_PATH, e.g. /usr/lib64/llvm20/lib64 on Fedora), and the build
# packages listed in docs/linux/LINUX-STATUS.md. Pass a binary path to package
# one you built yourself (no remapping is then applied).
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out="${2:-$root/dist}"
version=$(awk '/^\[/ { in_ws = ($0 == "[workspace.package]") } in_ws && /^version[ \t]*=/ { gsub(/^[^"]*"|"[^"]*$/, ""); print; exit }' "$root/Cargo.toml")
[ -n "$version" ] || { echo "package-linux: no version in Cargo.toml" >&2; exit 1; }

if [ -n "${1:-}" ]; then
  bin="$1"
else
  target="${CARGO_TARGET_DIR:-$root/target}"
  cargo_home="${CARGO_HOME:-$HOME/.cargo}"
  rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
  us=$(printf '\037')
  CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=~${us}--remap-path-prefix=$root=awili${us}--remap-path-prefix=$cargo_home=~/.cargo${us}--remap-path-prefix=$rustup_home=~/.rustup${us}--remap-path-prefix=$target=target"
  cmap="-ffile-prefix-map=$HOME=~ -ffile-prefix-map=$root=awili -ffile-prefix-map=$cargo_home=~/.cargo -ffile-prefix-map=$rustup_home=~/.rustup -ffile-prefix-map=$target=target"
  export CARGO_ENCODED_RUSTFLAGS CARGO_TARGET_DIR="$target" CFLAGS="$cmap" CXXFLAGS="$cmap"
  (cd "$root" && cargo build --release -p awili-app --locked)
  bin="$target/release/awili-app"
fi
[ -x "$bin" ] || { echo "package-linux: $bin not found" >&2; exit 1; }

name="Awili-$version-linux-x86_64"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
d="$stage/$name"
mkdir -p "$d"

install -m 0755 "$bin" "$d/awili-app"
strip "$d/awili-app"
cp "$root/packaging/linux/awili.desktop" "$root/packaging/linux/README.txt" "$d/"
install -m 0755 "$root/packaging/linux/install.sh" "$root/packaging/linux/uninstall.sh" "$d/"
cp -R "$root/assets/brand/app-icons/linux/hicolor" "$d/icons"

mkdir -p "$out"
# Fixed owner and sorted names.
tar --owner=0 --group=0 --numeric-owner --sort=name -C "$stage" -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "$out/$name.tar.gz"
cat "$out/$name.tar.gz.sha256"
