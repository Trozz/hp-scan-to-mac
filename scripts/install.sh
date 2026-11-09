#!/usr/bin/env bash
# Builds hp-scan-to-mac and installs it as a per-user LaunchAgent.
# Settings are read from the environment, e.g.:
#   HP_PRINTER=10.30.0.205 ./scripts/install.sh
set -euo pipefail

: "${HP_PRINTER:?set HP_PRINTER to the printer IP address}"

LABEL="io.github.hp-scan-to-mac"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Library/Logs/hp-scan-to-mac.log"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

cargo build --release --manifest-path "$ROOT/Cargo.toml"
mkdir -p "$BIN_DIR" "$(dirname "$PLIST")" "$(dirname "$LOG")"
install -m 755 "$ROOT/target/release/hp-scan-to-mac" "$BIN_DIR/hp-scan-to-mac"

xml_escape() { sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' <<<"$1"; }

env_entries=""
for var in HP_PRINTER HP_SCAN_NAME HP_SCAN_OUTPUT HP_SCAN_RESOLUTION HP_SCAN_COLOR \
           PAPERLESS_URL PAPERLESS_TOKEN PAPERLESS_TAGS PAPERLESS_DELETE_LOCAL RUST_LOG; do
  if [[ -n "${!var:-}" ]]; then
    env_entries+="    <key>$var</key><string>$(xml_escape "${!var}")</string>
"
  fi
done

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true

umask 077
cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$BIN_DIR/hp-scan-to-mac</string>
    <string>run</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
$env_entries  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>30</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict>
</plist>
PLIST

launchctl bootstrap "gui/$(id -u)" "$PLIST"
echo "Installed. Logs: $LOG"
echo "Choose 'Scan to Computer' on the printer and pick this Mac."
