# hp-scan-to-mac

A small Rust service that makes the **Scan to Computer** button on HP printers work without HP Smart or HP Easy Scan. Built for macOS, and it also runs on Linux and in Docker.

It adds your computer to the printer's "Scan to Computer" list. When you pick it on the printer's panel, the scan is pulled over the network and saved as a PDF (or JPEG). It can also upload each scan to [Paperless-ngx](https://docs.paperless-ngx.com/) or S3-compatible storage, and post a webhook.

Tested with an **HP OfficeJet Pro 7720 Wide Format All-in-One**. Other HP printers that offer `WalkupScanToComp` and eSCL over HTTP should work too.

## Features

- Appears on the printer's panel, with optional extra entries (profiles) such as "Moros Paperless" or "Moros Gray"
- Document feeder (multi-page) and glass, including "scan another page?" on the glass
- 2-sided scanning with a one-sided feeder (emulated duplex)
- Finds the printer automatically (Bonjour), and follows it if its IP address changes
- PDF or JPEG; A4 / Letter / Legal page size for the glass; file name patterns
- Targets: local folder, Paperless-ngx, S3-compatible storage (AWS, MinIO, R2, ...), signed webhooks
- Optional OCR with [OCRmyPDF](https://ocrmypdf.readthedocs.io/): searchable PDFs, pages rotated upright and deskewed
- Optional desktop notifications (macOS, and Linux with `notify-send`) and a command to run on each scan
- Autoscan mode: scans as soon as paper is loaded into the feeder
- Health endpoint, Docker image, LaunchAgent installer
- Secrets from a file or the macOS Keychain
- Removes itself from the printer's list when stopped

## Quick start

```sh
cargo build --release
./target/release/hp-scan-to-mac discover     # find the printer
./target/release/hp-scan-to-mac run          # register and wait for scans
```

On the printer choose **Scan > Computer**, pick your Mac and press **Start**. Scans land in `~/Documents/Scans`.

`test-scan` scans once without touching the printer panel (feeder if loaded, otherwise the glass).

### Run as a background service

```sh
./scripts/install.sh
```

This installs the binary to `~/.local/bin`, creates `~/.config/hp-scan-to-mac/config.toml` from [config.example.toml](config.example.toml) if it doesn't exist, and sets up a service that starts at login and restarts if it stops. To remove it: `./scripts/uninstall.sh` (your config is kept). Use `COMMAND=autoscan ./scripts/install.sh` for autoscan mode instead.

- **macOS:** a LaunchAgent. Logs: `~/Library/Logs/hp-scan-to-mac.log`. After editing the config: `launchctl kickstart -k gui/$(id -u)/io.github.hp-scan-to-mac`.
- **Linux:** a systemd user service. Logs: `journalctl --user -u hp-scan-to-mac -f`. After editing the config: `systemctl --user restart hp-scan-to-mac`. To keep it running when you're not logged in (headless machines): `loginctl enable-linger $USER`. Environment variables given to `install.sh` are stored in `~/.config/hp-scan-to-mac/service.env`.

## Configuration

Settings come from `~/.config/hp-scan-to-mac/config.toml` (or the file in `HP_SCAN_CONFIG` / `--config`). Flags and environment variables override the file. [config.example.toml](config.example.toml) documents every option.

| Config file | Environment / flag | Default |
| --- | --- | --- |
| `printer` | `HP_PRINTER` / `--printer` | found automatically |
| `printer_match` | `HP_PRINTER_MATCH` | first printer found |
| `name` | `HP_SCAN_NAME` | computer name |
| `output` | `HP_SCAN_OUTPUT` | `~/Documents/Scans` |
| `resolution` | `HP_SCAN_RESOLUTION` | `300` (capped to the scanner's maximum) |
| `color` | `HP_SCAN_COLOR` | `color` (`gray`, `bw`) |
| `paper_size` | `HP_SCAN_PAPER_SIZE` | `a4` (`letter`, `legal`, `max`) |
| `format` | `HP_SCAN_FORMAT` | follows the printer's shortcut (`pdf`, `jpeg`) |
| `filename` | `HP_SCAN_FILENAME` | `Scan %Y-%m-%d %H.%M.%S` ([strftime](https://docs.rs/chrono/latest/chrono/format/strftime/index.html)) |
| `local` | `HP_SCAN_LOCAL` | `true` |
| `emulated_duplex` | `HP_SCAN_EMULATED_DUPLEX` | `false` |
| `notify` | `HP_SCAN_NOTIFY` | `false` |
| `post_command` | `HP_SCAN_POST_COMMAND` | none |
| `ocr` | `HP_SCAN_OCR` | `false` |
| `ocr_language` | `HP_SCAN_OCR_LANGUAGE` | `eng` |
| `[paperless] url, token, tags` | `PAPERLESS_URL`, `PAPERLESS_TOKEN`, `PAPERLESS_TAGS` | off |
| `[s3] endpoint, bucket, region, prefix, access_key_id, secret_access_key, path_style` | `S3_ENDPOINT`, `S3_BUCKET`, `S3_REGION`, `S3_PREFIX`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`, `S3_PATH_STYLE` | off |
| `[webhook] url, secret, token` | `WEBHOOK_URL`, `WEBHOOK_SECRET`, `WEBHOOK_TOKEN` | off |
| `[health] listen` | `HEALTH_LISTEN` | off |
| `[autoscan] delay` | `AUTOSCAN_DELAY` | `5` seconds |

Every environment variable also has a matching `--flag` (see `--help`). Profiles can only be set in the config file.

**Secrets** (Paperless token, S3 secret, webhook secret and token) can be a plain value, `file:/path/to/file` or `keychain:<service>[/<account>]`. To store one in the Keychain:

```sh
security add-generic-password -s hp-scan-to-mac -a paperless -w
# then: token = "keychain:hp-scan-to-mac/paperless"
```

### Profiles

Each `[[profile]]` is one entry on the printer's list. It inherits the top-level settings and can change `output`, `resolution`, `color`, `paper_size`, `format`, `filename`, `local`, `paperless`, `s3`, `webhook`, `emulated_duplex`, `ocr` and `ocr_language`. Uploads that are configured are on for every profile unless the profile turns them off.

```toml
[[profile]]
name = "Moros"

[[profile]]
name = "Moros Paperless"
local = false
paperless = true
```

When any profile is defined, only the profiles are registered. The printer allows up to 15 entries in total, across all computers.

### Uploads

- Files are always written to the output folder first. With `local = false` the local copy is deleted only after every upload for that file succeeded; if any upload fails, the file is kept and the log says so. Uploads are retried 3 times.
- **Paperless-ngx:** posted to `/api/documents/post_document/` with the file name as title and the configured tags.
- **S3:** uploaded to `<prefix>/<file name>`. Set `path_style = true` for MinIO and similar.
- **Webhook:** a JSON `POST` after each scan (`scan.completed`, or `scan.failed` if scanning failed) with the files (name, size, SHA-256, local path if kept, whether OCR was applied) and each upload's result (Paperless task id, S3 key). Headers: `Idempotency-Key` (the event id), `X-Signature-256: sha256=<hex HMAC of the body>` when a secret is set, and `Authorization: Bearer` when a token is set.

### OCR

With `ocr = true`, each PDF is run through `ocrmypdf --rotate-pages --deskew` before it is saved or uploaded, so every target gets a searchable PDF with pages turned upright and straightened. Install OCRmyPDF first:

```sh
brew install ocrmypdf            # macOS
sudo apt install ocrmypdf        # Debian / Ubuntu
```

The service won't start if OCR is on and `ocrmypdf` is missing. For other languages install the Tesseract language pack (e.g. `brew install tesseract-lang`, `apt install tesseract-ocr-deu`) and set `ocr_language = "eng+deu"`. If OCR fails or takes longer than 5 minutes, the original PDF is delivered and the log says why. JPEG output is never OCR'd. Paperless skips its own OCR for PDFs that already have text.

### Emulated duplex (2-sided)

With `emulated_duplex = true`, an extra "<name> 2-sided" entry appears on the printer. Choose it to scan the front sides, flip the whole stack, and choose it again to scan the back sides. The pages are merged into one document in the right order. If the backs aren't scanned within 10 minutes, or you scan with a different entry, the fronts are saved on their own.

### Autoscan

`hp-scan-to-mac autoscan` scans with the first profile whenever paper is loaded into the feeder, without using the printer panel. It waits `delay` seconds after paper is detected, and only re-arms once the feeder has been emptied. Run it instead of `run`, not alongside it.

### Health

With `HEALTH_LISTEN=127.0.0.1:8090`, `GET /health` returns JSON and status 200 when the printer was reached in the last 5 minutes, otherwise 503. `hp-scan-to-mac healthcheck` checks it and exits non-zero when unhealthy.

## Docker

```sh
docker build -t hp-scan-to-mac .                        # add --build-arg OCR=true for OCR
docker run -d --init --name hp-scan --restart unless-stopped \
  -e HP_PRINTER=10.30.0.205 -e HP_SCAN_NAME=Scanner \
  -v "$PWD/scans:/scans" hp-scan-to-mac
```

See [compose.example.yaml](compose.example.yaml). The image saves to `/scans`, runs as UID 1000, serves the health endpoint on port 8090 and has a Docker `HEALTHCHECK`. Mount a config file and set `HP_SCAN_CONFIG=/config/config.toml` for profiles.

The container only makes outgoing connections, so the default bridge network works when `HP_PRINTER` is set. Automatic printer discovery needs `network_mode: host` (Linux hosts). Set `HP_SCAN_NAME`, otherwise the container ID is shown on the printer.

The [Docker workflow](.github/workflows/docker.yml) publishes `linux/amd64` and `linux/arm64` images to GitHub Container Registry on pushes to `main` and on `v*` tags, in two variants: `latest` (about 120 MB) and `latest-ocr` with OCRmyPDF and English Tesseract data (about 365 MB). Version tags get the same `-ocr` suffix.

## Commands

`run` (default), `autoscan`, `test-scan`, `discover`, `list` (entries registered on the printer), `unregister`, `healthcheck`.

## How it works

1. `POST /WalkupScanToComp/WalkupScanToCompDestinations` registers each profile (reused if the name already exists).
2. `GET /EventMgmt/EventTable?timeout=1200` with `If-None-Match` long-polls for printer events.
3. On a `ScanEvent` for one of our entries, it reads the walk-up event (`ScanRequested`, `ScanNewPageRequested`, `ScanPagesComplete`) and the entry's chosen shortcut.
4. The scan runs over eSCL: `POST /eSCL/ScanJobs`, then `GET .../NextDocument` until the job has no more pages. The feeder is used if it has paper, otherwise the glass.
5. Pages are JPEGs (feeder pages get their height fixed from the DNL marker). For PDF output they are embedded unchanged.

## Notes and limits

- **2-sided on the 7720:** its duplex unit is for printing; the feeder scans one side. Use emulated duplex. Printers with a duplex feeder do true 2-sided scans when 2-sided is chosen on the panel.
- **Page size** applies to the glass. The feeder measures each sheet, so feeder scans always match the paper.
- **Local Network permission:** recent macOS versions ask before apps reach devices on your network. If the log shows "No route to host" or similar, allow `hp-scan-to-mac` in System Settings > Privacy & Security > Local Network.
- **Folder access:** macOS protects `~/Documents`, `~/Desktop` and `~/Downloads`. If the log shows "Operation not permitted" when saving, allow access when prompted, or use a folder outside those (e.g. `~/Scans`).
- **Discovery on macOS** uses the system's Bonjour service, so it also finds printers on another subnet when your router reflects mDNS. On Linux it sends mDNS queries itself, so the printer must be on the same network (or reflected).
- **Notifications on Linux** use `notify-send` (package `libnotify-bin` on Debian/Ubuntu) and need a desktop session, so they don't show in Docker or on a headless server.
- The computer must be awake to receive scans.

## Credits

The HP XML formats are based on [node-hp-scan-to](https://github.com/manuc66/node-hp-scan-to) by Emmanuel Counasse (MIT license).
