#!/bin/sh
# Installs Awili for the current user. No sudo, nothing outside your home folder.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
bindir="${XDG_BIN_HOME:-$HOME/.local/bin}"
datadir="${XDG_DATA_HOME:-$HOME/.local/share}"

if [ ! -x "$here/awili-app" ]; then
  echo "install.sh: awili-app not found next to this script" >&2
  exit 1
fi

mkdir -p "$bindir" "$datadir/applications"
install -m 0755 "$here/awili-app" "$bindir/awili-app"
if [ -f "$here/awili-connectors-mcp" ]; then
  install -m 0755 "$here/awili-connectors-mcp" "$bindir/awili-connectors-mcp"
fi

# The launcher entry points at the installed binary by absolute path. The path
# goes in a quoted Exec argument, so escape it the way the desktop-entry spec
# asks: % doubles, and \ " $ and a backtick take a backslash (a literal
# backslash is written four times). Generated line by line, no sed on the value.
esc=$(printf '%s' "$bindir" | sed -e 's/%/%%/g' -e 's/\\/\\\\\\\\/g' -e 's/"/\\"/g' -e 's/\$/\\$/g' -e 's/`/\\`/g')
{
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      Exec=*) printf 'Exec="%s/awili-app"\n' "$esc" ;;
      *) printf '%s\n' "$line" ;;
    esac
  done < "$here/awili.desktop"
} > "$datadir/applications/awili.desktop"
chmod 0644 "$datadir/applications/awili.desktop"

if [ -d "$here/icons" ]; then
  # find -exec (not a pipeline) so a failed copy stops the install under set -e.
  find "$here/icons" -type f -name 'awili.*' -exec sh -c '
    set -e
    src=$1; dst=$2; shift 2
    for f; do
      rel=${f#"$src"/}
      mkdir -p "$dst/$(dirname "$rel")"
      install -m 0644 "$f" "$dst/$rel"
    done
  ' sh "$here/icons" "$datadir/icons/hicolor" {} +
fi

# Best effort: refresh the desktop caches if the tools are present.
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$datadir/applications" >/dev/null 2>&1 || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$datadir/icons/hicolor" >/dev/null 2>&1 || true

echo "Installed Awili to $bindir/awili-app"
case ":$PATH:" in
  *":$bindir:"*) ;;
  *) echo "Note: $bindir is not on your PATH. Start Awili from your app menu, or run $bindir/awili-app." ;;
esac
echo "Your settings and conversations are not touched by install or uninstall."
