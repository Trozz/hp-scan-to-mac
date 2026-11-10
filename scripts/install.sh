#!/usr/bin/env bash
# Builds hp-scan-to-mac and installs it as a per-user LaunchAgent (macOS).
#
# Settings live in ~/.config/hp-scan-to-mac/config.toml (created from
# config.example.toml on first install). Environment variables set when running
# this script are also passed to the service, e.g.:
#   HP_PRINTER=10.30.0.205 ./scripts/install.sh
# Use COMMAND=autoscan to run the feeder autoscan mode instead of Scan to Computer.
set -euo pipefail

LABEL="io.github.hp-scan-to-mac"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Library/Logs/hp-scan-to-mac.log"
CONFIG_DIR="$HOME/.config/hp-scan-to-mac"
COMMAND="${COMMAND:-run}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

cargo build --release --manifest-path "$ROOT/Cargo.toml"
mkdir -p "$BIN_DIR" "$(dirname "$PLIST")" "$(dirname "$LOG")" "$CONFIG_DIR"
install -m 755 "$ROOT/target/release/hp-scan-to-mac" "$BIN_DIR/hp-scan-to-mac"

if [[ ! -f "$CONFIG_DIR/config.toml" ]]; then
  install -m 600 "$ROOT/config.example.toml" "$CONFIG_DIR/config.toml"
  echo "Created $CONFIG_DIR/config.toml"
fi

# Keep secrets out of the plist: store them in files the config can reference.
for var in PAPERLESS_TOKEN S3_SECRET_ACCESS_KEY WEBHOOK_SECRET WEBHOOK_TOKEN; do
  value="${!var:-}"
  if [[ -n "$value" && "$value" != file:* && "$value" != keychain:* ]]; then
    file="$CONFIG_DIR/$(tr '[:upper:]_' '[:lower:]-' <<<"$var")"
    (umask 077 && printf '%s' "$value" >"$file")
    export "$var=file:$file"
  fi
done

xml_escape() { sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' <<<"$1"; }

env_entries=""
for var in HP_SCAN_CONFIG HP_PRINTER HP_PRINTER_MATCH HP_SCAN_NAME HP_SCAN_OUTPUT HP_SCAN_RESOLUTION \
           HP_SCAN_COLOR HP_SCAN_PAPER_SIZE HP_SCAN_FORMAT HP_SCAN_FILENAME HP_SCAN_LOCAL \
           HP_SCAN_EMULATED_DUPLEX HP_SCAN_NOTIFY HP_SCAN_POST_COMMAND \
           PAPERLESS_URL PAPERLESS_TOKEN PAPERLESS_TAGS \
           S3_ENDPOINT S3_BUCKET S3_REGION S3_PREFIX S3_ACCESS_KEY_ID S3_SECRET_ACCESS_KEY S3_PATH_STYLE \
           WEBHOOK_URL WEBHOOK_SECRET WEBHOOK_TOKEN HEALTH_LISTEN AUTOSCAN_DELAY RUST_LOG; do
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
    <string>$COMMAND</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
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
echo "Installed. Config: $CONFIG_DIR/config.toml  Logs: $LOG"
echo "After editing the config, run: launchctl kickstart -k gui/$(id -u)/$LABEL"
