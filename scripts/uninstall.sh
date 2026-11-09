#!/usr/bin/env bash
# Stops the LaunchAgent, removes this Mac from the printer and deletes the installed files.
set -euo pipefail

LABEL="io.github.hp-scan-to-mac"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
rm -f "$PLIST" "$BIN_DIR/hp-scan-to-mac"
echo "Uninstalled."
