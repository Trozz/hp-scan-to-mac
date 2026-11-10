#!/usr/bin/env bash
# Stops the service (LaunchAgent on macOS, systemd user service on Linux),
# removes this computer from the printer and deletes the installed files.
# Your config in ~/.config/hp-scan-to-mac is kept.
set -euo pipefail

LABEL="io.github.hp-scan-to-mac"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
  Darwin)
    launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
    rm -f "$HOME/Library/LaunchAgents/$LABEL.plist"
    ;;
  Linux)
    systemctl --user disable --now hp-scan-to-mac.service 2>/dev/null || true
    rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/hp-scan-to-mac.service"
    systemctl --user daemon-reload 2>/dev/null || true
    ;;
  *)
    echo "Unsupported system: $(uname -s)" >&2
    exit 1
    ;;
esac
rm -f "$BIN_DIR/hp-scan-to-mac"
echo "Uninstalled."
