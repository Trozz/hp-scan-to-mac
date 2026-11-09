mod device;
mod jpeg;
mod paperless;
mod pdf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use device::{CompEvent, Destination, Device, EventPoll, InputSource, JobSettings, ScannerCaps};
use paperless::Paperless;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(
    version,
    about = "Receive \"Scan to Computer\" jobs from HP printers on macOS"
)]
struct Cli {
    /// Printer IP address or hostname
    #[arg(long, env = "HP_PRINTER", global = true)]
    printer: Option<String>,

    /// Name shown on the printer's "Scan to Computer" list (default: this Mac's name)
    #[arg(long, env = "HP_SCAN_NAME", global = true)]
    name: Option<String>,

    /// Folder for finished scans (default: ~/Documents/Scans)
    #[arg(long, env = "HP_SCAN_OUTPUT", global = true)]
    output: Option<PathBuf>,

    /// Scan resolution in DPI (capped to what the scanner supports)
    #[arg(long, env = "HP_SCAN_RESOLUTION", default_value_t = 300, global = true)]
    resolution: u32,

    #[arg(long, env = "HP_SCAN_COLOR", value_enum, default_value_t = ColorMode::Color, global = true)]
    color: ColorMode,

    /// Paperless-ngx base URL, e.g. http://paperless.local:8000
    #[arg(long, env = "PAPERLESS_URL", global = true)]
    paperless_url: Option<String>,

    /// Paperless-ngx API token
    #[arg(long, env = "PAPERLESS_TOKEN", global = true, hide_env_values = true)]
    paperless_token: Option<String>,

    /// Comma-separated Paperless tag IDs to apply to uploads
    #[arg(long, env = "PAPERLESS_TAGS", value_delimiter = ',', global = true)]
    paperless_tags: Vec<u32>,

    /// Delete the local file after a successful Paperless upload
    #[arg(long, env = "PAPERLESS_DELETE_LOCAL", global = true)]
    paperless_delete_local: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Clone, Copy)]
enum Command {
    /// Register with the printer and wait for scans (default)
    Run,
    /// Scan once right now (feeder if loaded, else glass) to check everything works
    TestScan,
    /// List "Scan to Computer" destinations registered on the printer
    List,
    /// Remove this Mac from the printer's "Scan to Computer" list
    Unregister,
}

#[derive(Clone, Copy, ValueEnum)]
enum ColorMode {
    Color,
    Gray,
    Bw,
}

impl ColorMode {
    fn escl(self) -> &'static str {
        match self {
            ColorMode::Color => "RGB24",
            ColorMode::Gray => "Grayscale8",
            ColorMode::Bw => "BlackAndWhite1",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Pdf,
    Jpeg,
}

struct App {
    device: Device,
    name: String,
    output: PathBuf,
    resolution: u32,
    color: ColorMode,
    caps: ScannerCaps,
    user_timeout: Duration,
    paperless: Option<Paperless>,
    paperless_delete_local: bool,
}

const EVENT_POLL: Duration = Duration::from_secs(120);

fn default_name() -> String {
    std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Mac".into())
}

fn default_output() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| ".".into());
    home.join("Documents").join("Scans")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();

    let cli = Cli::parse();
    let Some(printer) = cli.printer.as_deref() else {
        bail!("no printer given: pass --printer <ip> or set HP_PRINTER");
    };
    let device = Device::new(printer)?;
    let name = cli.name.clone().unwrap_or_else(default_name);

    match cli.command.unwrap_or(Command::Run) {
        Command::List => {
            for d in device.destinations().await? {
                println!("{}\t{}", d.name, d.path);
            }
            Ok(())
        }
        Command::Unregister => unregister(&device, &name).await,
        Command::TestScan => App::new(device, name, &cli).await?.test_scan().await,
        Command::Run => App::new(device, name, &cli).await?.run().await,
    }
}

async fn unregister(device: &Device, name: &str) -> Result<()> {
    for d in device
        .destinations()
        .await?
        .into_iter()
        .filter(|d| d.name == name)
    {
        device.unregister(&d).await?;
        info!("Removed \"{}\" from the printer", d.name);
    }
    Ok(())
}

impl App {
    async fn new(device: Device, name: String, cli: &Cli) -> Result<Self> {
        let caps = device
            .scanner_caps()
            .await
            .context("reading scanner capabilities")?;
        let user_timeout = device
            .user_action_timeout()
            .await
            .ok()
            .flatten()
            .unwrap_or(60);
        let paperless = match (&cli.paperless_url, &cli.paperless_token) {
            (Some(url), Some(token)) => {
                Some(Paperless::new(url, token, cli.paperless_tags.clone())?)
            }
            (None, None) => None,
            _ => bail!("Paperless needs both PAPERLESS_URL and PAPERLESS_TOKEN"),
        };
        let output = cli.output.clone().unwrap_or_else(default_output);
        tokio::fs::create_dir_all(&output)
            .await
            .with_context(|| format!("creating {}", output.display()))?;
        Ok(Self {
            device,
            name,
            output,
            resolution: cli.resolution,
            color: cli.color,
            caps,
            user_timeout: Duration::from_secs(user_timeout),
            paperless,
            paperless_delete_local: cli.paperless_delete_local,
        })
    }

    async fn run(&self) -> Result<()> {
        info!(
            "Saving scans to {}{}",
            self.output.display(),
            if self.paperless.is_some() {
                " and uploading to Paperless"
            } else {
                ""
            }
        );
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = self.serve_forever() => unreachable!(),
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
        info!("Shutting down");
        if let Err(e) = unregister(&self.device, &self.name).await {
            warn!("Could not unregister from the printer: {e:#}");
        }
        Ok(())
    }

    async fn serve_forever(&self) {
        loop {
            if let Err(e) = self.serve().await {
                warn!("{e:#}; retrying in 15s");
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    }

    async fn serve(&self) -> Result<()> {
        let destination = self.device.register(&self.name).await?;
        info!(
            "Registered as \"{}\" on the printer ({})",
            destination.name, destination.path
        );
        let mut cursor = EventCursor::new(&self.device).await?;
        info!("Waiting for \"Scan to Computer\" requests");
        loop {
            let Some(comp) = cursor
                .next_scan(&self.device, &destination, EVENT_POLL)
                .await?
            else {
                if self
                    .device
                    .destination_settings(&destination)
                    .await?
                    .is_none()
                {
                    bail!("printer forgot our registration");
                }
                continue;
            };
            if let Err(e) = self.handle_session(&destination, &comp).await {
                error!("Scan failed: {e:#}");
            }
            cursor = EventCursor::new(&self.device).await?;
        }
    }

    /// Polls the walk-up event until it leaves the `waiting` states.
    async fn wait_comp(&self, comp: &str, waiting: &[CompEvent]) -> Result<Option<CompEvent>> {
        let deadline = Instant::now() + self.user_timeout + Duration::from_secs(30);
        while Instant::now() < deadline {
            let state = self.device.comp_event(comp).await?;
            if !waiting.contains(&state) {
                return Ok(Some(state));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Ok(None)
    }

    async fn handle_session(&self, destination: &Destination, comp: &str) -> Result<()> {
        let state = self.wait_comp(comp, &[CompEvent::HostSelected]).await?;
        if !matches!(
            state,
            Some(CompEvent::ScanRequested | CompEvent::ScanNewPageRequested)
        ) {
            tracing::debug!("Walk-up event is {state:?}, nothing to scan");
            return Ok(());
        }
        let settings = self
            .device
            .destination_settings(destination)
            .await?
            .unwrap_or_default();
        let format = match settings.shortcut.as_deref() {
            Some(s) if s.contains("JPEG") => OutputFormat::Jpeg,
            _ => OutputFormat::Pdf,
        };
        let source = if self.device.adf_loaded().await? {
            InputSource::Feeder
        } else {
            InputSource::Platen
        };
        info!(
            "Scan requested: shortcut={}, source={source:?}",
            settings.shortcut.as_deref().unwrap_or("none")
        );
        let job = self.job_settings(source, settings.duplex);
        let mut pages = self.scan(&job).await?;

        if source == InputSource::Platen
            && let Err(e) = self.more_platen_pages(destination, &job, &mut pages).await
        {
            error!(
                "Next page failed, saving the {} page(s) already scanned: {e:#}",
                pages.len()
            );
        }
        self.save(pages, format).await
    }

    /// Handles the printer's "scan another page?" prompt after glass scans.
    async fn more_platen_pages(
        &self,
        destination: &Destination,
        job: &JobSettings,
        pages: &mut Vec<pdf::Page>,
    ) -> Result<()> {
        loop {
            // Right after each page the printer posts a scan event while the walk-up
            // state still holds the previous answer. Let that pass, then wait for the
            // event caused by the user's choice.
            tokio::time::sleep(Duration::from_secs(3)).await;
            let mut cursor = EventCursor::new(&self.device).await?;
            let state = loop {
                let wait = self.user_timeout + Duration::from_secs(30);
                let Some(comp) = cursor.next_scan(&self.device, destination, wait).await? else {
                    return Ok(());
                };
                let state = self.device.comp_event(&comp).await?;
                if !matches!(state, CompEvent::ScanRequested | CompEvent::HostSelected) {
                    break state;
                }
            };
            if state != CompEvent::ScanNewPageRequested {
                return Ok(());
            }
            info!("Next page requested");
            pages.extend(self.scan(job).await?);
        }
    }

    fn job_settings(&self, source: InputSource, duplex: bool) -> JobSettings {
        let max = match source {
            InputSource::Platen => self.caps.platen_max_dpi,
            InputSource::Feeder => self.caps.adf_max_dpi,
        };
        let resolution = if max > 0 {
            self.resolution.min(max)
        } else {
            self.resolution
        };
        if resolution != self.resolution {
            info!("Using {resolution} DPI, the most this scan source supports");
        }
        if duplex && !self.caps.adf_duplex {
            warn!("2-sided scanning was requested but this scanner's feeder is single-sided");
        }
        JobSettings {
            source,
            resolution,
            color_mode: self.color.escl(),
            duplex: duplex && source == InputSource::Feeder && self.caps.adf_duplex,
        }
    }

    async fn scan(&self, job: &JobSettings) -> Result<Vec<pdf::Page>> {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let job_path = self.device.create_job(job).await?;
        info!("Scanning ({job_path})");
        let mut pages = Vec::new();
        let result = async {
            while let Some(mut bytes) = self.device.next_document(&job_path).await? {
                let info = jpeg::inspect_and_fix(&mut bytes)
                    .context("scanner returned an unreadable JPEG")?;
                info!(
                    "Received page {} ({}x{})",
                    pages.len() + 1,
                    info.width,
                    info.height
                );
                pages.push(pdf::Page {
                    jpeg: bytes,
                    info,
                    dpi: job.resolution,
                });
            }
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = result {
            self.device.cancel_job(&job_path).await;
            return Err(e);
        }
        if pages.is_empty() {
            bail!("scan job finished without any pages");
        }
        Ok(pages)
    }

    async fn save(&self, pages: Vec<pdf::Page>, format: OutputFormat) -> Result<()> {
        let stem = chrono::Local::now()
            .format("Scan %Y-%m-%d %H.%M.%S")
            .to_string();
        let files: Vec<(String, Vec<u8>, &str)> = match format {
            OutputFormat::Pdf => {
                vec![(format!("{stem}.pdf"), pdf::build(&pages), "application/pdf")]
            }
            OutputFormat::Jpeg if pages.len() == 1 => pages
                .into_iter()
                .map(|p| (format!("{stem}.jpg"), p.jpeg, "image/jpeg"))
                .collect(),
            OutputFormat::Jpeg => pages
                .into_iter()
                .enumerate()
                .map(|(i, p)| (format!("{stem} p{}.jpg", i + 1), p.jpeg, "image/jpeg"))
                .collect(),
        };
        for (file_name, bytes, mime) in files {
            let path = write_unique(&self.output, &file_name, &bytes).await?;
            info!("Saved {}", path.display());
            self.upload(&path, mime).await;
        }
        Ok(())
    }

    async fn upload(&self, path: &Path, mime: &str) {
        let Some(paperless) = &self.paperless else {
            return;
        };
        match paperless.upload(path, mime).await {
            Ok(task) => {
                info!("Sent to Paperless (task {task})");
                if self.paperless_delete_local {
                    let _ = tokio::fs::remove_file(path).await;
                }
            }
            Err(e) => error!("Paperless upload failed, file kept locally: {e:#}"),
        }
    }

    async fn test_scan(&self) -> Result<()> {
        let source = if self.device.adf_loaded().await? {
            InputSource::Feeder
        } else {
            InputSource::Platen
        };
        info!("Test scan from {source:?}");
        let pages = self.scan(&self.job_settings(source, false)).await?;
        self.save(pages, OutputFormat::Pdf).await
    }
}

/// Writes via a hidden temp file and rename so folder watchers never see partial files.
async fn write_unique(dir: &Path, file_name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let (stem, ext) = file_name.rsplit_once('.').unwrap_or((file_name, ""));
    let mut path = dir.join(file_name);
    let mut n = 2;
    while tokio::fs::try_exists(&path).await? {
        path = dir.join(format!("{stem} ({n}).{ext}"));
        n += 1;
    }
    let tmp = dir.join(format!(".{file_name}.part"));
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(path)
}

/// Tracks the event table so each scan event is acted on once.
struct EventCursor {
    etag: String,
    seen: Vec<String>,
}

impl EventCursor {
    async fn new(device: &Device) -> Result<Self> {
        let (etag, events) = device.event_baseline().await?;
        Ok(Self {
            etag,
            seen: events.into_iter().map(|e| e.stamp).collect(),
        })
    }

    /// Waits up to `wait` for a new scan event aimed at `destination`; returns its comp-event path.
    async fn next_scan(
        &mut self,
        device: &Device,
        destination: &Destination,
        wait: Duration,
    ) -> Result<Option<String>> {
        let deadline = Instant::now() + wait;
        while let Some(remaining) = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
        {
            let EventPoll::Changed { etag, events } =
                device.poll_events(Some(&self.etag), remaining).await?
            else {
                continue;
            };
            self.etag = etag;
            let mut found = None;
            for event in events {
                if self.seen.contains(&event.stamp) {
                    continue;
                }
                if found.is_none() {
                    found = event.scan_for(&destination.path);
                }
                self.seen.push(event.stamp);
                if self.seen.len() > 500 {
                    self.seen.drain(..250);
                }
            }
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }
}
