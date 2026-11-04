#!/bin/sh
# Removes what install.sh put in your home folder. Your Awili data is left alone.
set -eu
bindir="${XDG_BIN_HOME:-$HOME/.local/bin}"
datadir="${XDG_DATA_HOME:-$HOME/.local/share}"

rm -f "$bindir/awili-app" "$bindir/awili-connectors-mcp" "$datadir/applications/awili.desktop"
find "$datadir/icons/hicolor" -type f -name 'awili.*' -path '*/apps/*' -delete 2>/dev/null || true
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$datadir/applications" >/dev/null 2>&1 || true

echo "Awili is removed."
echo "Kept: $datadir/awili (conversations and settings) and ${XDG_CONFIG_HOME:-$HOME/.config}/awili."
echo "Delete those folders yourself to remove everything. Keys you saved are in your keyring under \"dev.awili.Awili\"."
