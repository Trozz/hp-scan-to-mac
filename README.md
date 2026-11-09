# hp-scan-to-mac

A small Rust service that makes the **Scan to Computer** button on HP printers work with a Mac, without HP Smart or HP Easy Scan.

The service adds your Mac to the printer's "Scan to Computer" list. When you pick it on the printer's panel, the scan is pulled over the network and saved as a PDF (or JPEG) in a folder on your Mac. It can also send each scan to [Paperless-ngx](https://docs.paperless-ngx.com/).

Tested with an **HP OfficeJet Pro 7720 Wide Format All-in-One**. Other HP printers that offer `WalkupScanToComp` and eSCL over HTTP should work too.

## Features

- Shows up as a destination on the printer's panel (named after your Mac)
- Document feeder (multi-page) and glass, including "scan another page?" on the glass
- PDF by default; the panel's JPEG shortcuts save `.jpg` files
- Fixes the zero-height JPEG headers HP sends for feeder scans
- Re-registers automatically if the printer reboots or the Mac wakes from sleep
- Removes itself from the printer's list when stopped
- Optional upload to Paperless-ngx
- No runtime dependencies: one static binary

## Requirements

- macOS
- Rust toolchain (`rustup`) to build
- The printer's IP address (printer panel: Wireless or Network summary). Give the printer a fixed IP or DHCP reservation.

## Quick start

Try it in the foreground first:

```sh
cargo build --release
./target/release/hp-scan-to-mac --printer 10.30.0.205 run
```

On the printer choose **Scan > Computer**, pick your Mac and press **Start**. Scans land in `~/Documents/Scans`.

To check scanning without touching the printer panel:

```sh
./target/release/hp-scan-to-mac --printer 10.30.0.205 test-scan
```

### Run as a background service

```sh
HP_PRINTER=10.30.0.205 ./scripts/install.sh
```

This installs the binary to `~/.local/bin` and a LaunchAgent (`~/Library/LaunchAgents/io.github.hp-scan-to-mac.plist`) that starts at login and restarts if it stops. Logs go to `~/Library/Logs/hp-scan-to-mac.log`.

Any setting below can be passed to `install.sh` as an environment variable. Run it again to change settings. To remove it:

```sh
./scripts/uninstall.sh
```

## Settings

Every option can be a flag or an environment variable.

| Flag | Environment | Default | Notes |
| --- | --- | --- | --- |
| `--printer` | `HP_PRINTER` | required | Printer IP or hostname |
| `--name` | `HP_SCAN_NAME` | Mac's Computer Name | Name shown on the printer |
| `--output` | `HP_SCAN_OUTPUT` | `~/Documents/Scans` | Folder for scans |
| `--resolution` | `HP_SCAN_RESOLUTION` | `300` | DPI, capped to what the scan source supports (the 7720 feeder tops out at 300) |
| `--color` | `HP_SCAN_COLOR` | `color` | `color`, `gray` or `bw` |
| `--paperless-url` | `PAPERLESS_URL` | | e.g. `http://paperless.local:8000` |
| `--paperless-token` | `PAPERLESS_TOKEN` | | Paperless profile page > API token |
| `--paperless-tags` | `PAPERLESS_TAGS` | | Comma-separated tag IDs, e.g. `3,7` |
| `--paperless-delete-local` | `PAPERLESS_DELETE_LOCAL` | off | Delete the local file once Paperless accepts it |
| | `RUST_LOG` | `info` | `debug` for more detail |

Commands: `run` (default), `test-scan`, `list` (show destinations registered on the printer), `unregister`.

### Paperless-ngx

```sh
HP_PRINTER=10.30.0.205 \
PAPERLESS_URL=http://paperless.local:8000 \
PAPERLESS_TOKEN=xxxxxxxx \
./scripts/install.sh
```

Each scan is saved locally first, then posted to `/api/documents/post_document/`. Failed uploads are retried 3 times; the local file is always kept if the upload fails. The token ends up in the LaunchAgent plist, which the install script creates readable only by you.

## How it works

1. `POST /WalkupScanToComp/WalkupScanToCompDestinations` registers the Mac (reused if the name already exists).
2. `GET /EventMgmt/EventTable?timeout=1200` with `If-None-Match` long-polls for printer events.
3. On a `ScanEvent` for our destination, it reads the walk-up event (`ScanRequested`, `ScanNewPageRequested`, `ScanPagesComplete`) and the destination's chosen shortcut.
4. The scan runs over eSCL: `POST /eSCL/ScanJobs`, then `GET .../NextDocument` until the job has no more pages. The feeder is used if it has paper, otherwise the glass.
5. Pages are JPEGs; for PDF output they are embedded unchanged (no re-compression).

## Notes and limits

- **2-sided scanning:** the OfficeJet Pro 7720's duplex unit is for printing. Its document feeder scans one side only, so the service logs a warning and scans single-sided if 2-sided is requested. Printers with a duplex feeder get true 2-sided scans.
- **No OCR:** "Save as Editable Text" produces a normal PDF. Paperless does OCR if you use it.
- **Local Network permission:** recent macOS versions ask before apps reach devices on your network. If the log shows "No route to host" or similar, allow `hp-scan-to-mac` in System Settings > Privacy & Security > Local Network.
- **Folder access:** macOS protects `~/Documents`, `~/Desktop` and `~/Downloads`. If the log shows "Operation not permitted" when saving, allow access when prompted, or set `HP_SCAN_OUTPUT` to a folder outside those (e.g. `~/Scans`).
- The Mac must be awake to receive scans.

## Credits

The HP XML formats are based on [node-hp-scan-to](https://github.com/manuc66/node-hp-scan-to) by Emmanuel Counasse (MIT license).
