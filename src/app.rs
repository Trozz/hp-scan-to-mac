//! The long-running service: registers profiles on the printer and handles scan requests.

use crate::config::{OutputFormat, PrinterSource, Profile, Settings};
use crate::deliver::Delivery;
use crate::device::{
    CompEvent, Destination, Device, EventPoll, InputSource, JobSettings, ScannerCaps,
};
use crate::health::Status;
use crate::{discovery, jpeg, pdf};
use anyhow::{Context, Result, bail};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

const EVENT_POLL: Duration = Duration::from_secs(120);
const DUPLEX_TIMEOUT: Duration = Duration::from_secs(600);

/// Front sides waiting for their backs (emulated duplex).
struct PendingFronts {
    profile: usize,
    pages: Vec<pdf::Page>,
    format: OutputFormat,
    since: Instant,
}

pub struct App {
    settings: Settings,
    device: RwLock<Arc<Device>>,
    caps: ScannerCaps,
    user_timeout: Duration,
    delivery: Arc<Delivery>,
    pending: Mutex<Option<PendingFronts>>,
    pub status: Arc<Status>,
}

/// Connects to the configured printer, or finds one on the network.
pub async fn connect(source: &PrinterSource) -> Result<Device> {
    match source {
        PrinterSource::Fixed(host) => Device::new(host),
        PrinterSource::Discover(filter) => {
            let found = discovery::find(filter.as_deref()).await?;
            info!(
                "Found \"{}\" at {} (UUID {})",
                found.name, found.address, found.uuid
            );
            Device::new(&found.address)
        }
    }
}

/// Merges back sides into front sides. A feeder returns a flipped stack last page first.
pub fn interleave<T>(fronts: Vec<T>, mut backs: Vec<T>, backs_reversed: bool) -> Vec<T> {
    if backs_reversed {
        backs.reverse();
    }
    if fronts.len() != backs.len() {
        warn!(
            "{} front side(s) but {} back side(s); keeping fronts then backs in scan order",
            fronts.len(),
            backs.len()
        );
        return fronts.into_iter().chain(backs).collect();
    }
    fronts
        .into_iter()
        .zip(backs)
        .flat_map(|(f, b)| [f, b])
        .collect()
}

impl App {
    pub async fn new(settings: Settings) -> Result<Self> {
        let device = connect(&settings.printer).await?;
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
        for profile in &settings.profiles {
            tokio::fs::create_dir_all(&profile.output)
                .await
                .with_context(|| format!("creating {}", profile.output.display()))?;
        }
        let status = Arc::new(Status::default());
        status.set_printer(device.host());
        Ok(Self {
            delivery: Arc::new(Delivery::new(&settings)?),
            settings,
            device: RwLock::new(Arc::new(device)),
            caps,
            user_timeout: Duration::from_secs(user_timeout),
            pending: Mutex::new(None),
            status,
        })
    }

    pub fn device(&self) -> Arc<Device> {
        self.device.read().unwrap().clone()
    }

    pub fn default_profile(&self) -> &Profile {
        &self.settings.profiles[0]
    }

    /// When discovering, looks the printer up again in case its address changed.
    async fn refresh_printer(&self) -> Result<()> {
        if matches!(self.settings.printer, PrinterSource::Fixed(_)) {
            return Ok(());
        }
        let device = connect(&self.settings.printer).await?;
        if device.host() != self.device().host() {
            info!("Printer moved to {}", device.host());
            self.status.set_printer(device.host());
            *self.device.write().unwrap() = Arc::new(device);
        }
        Ok(())
    }

    async fn until_shutdown<F: std::future::Future<Output = ()>>(&self, work: F) -> Result<()> {
        if let Some(addr) = self.settings.health_listen {
            let status = self.status.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::health::serve(addr, status).await {
                    error!("Health endpoint stopped: {e:#}");
                }
            });
        }
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = work => {},
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
        info!("Shutting down");
        self.delivery.stop();
        self.flush_pending(true).await;
        self.delivery.finish().await;
        Ok(())
    }

    pub async fn run(&self) -> Result<()> {
        for p in &self.settings.profiles {
            info!("Profile \"{}\" saves to {}", p.name, p.output.display());
        }
        self.until_shutdown(self.serve_forever()).await?;
        self.unregister().await;
        Ok(())
    }

    pub async fn unregister(&self) {
        let device = self.device();
        let Ok(existing) = device.destinations().await else {
            return warn!("Could not reach the printer to unregister");
        };
        for d in existing
            .iter()
            .filter(|d| self.settings.profiles.iter().any(|p| p.name == d.name))
        {
            match device.unregister(d).await {
                Ok(()) => info!("Removed \"{}\" from the printer", d.name),
                Err(e) => warn!("Could not remove \"{}\": {e:#}", d.name),
            }
        }
    }

    async fn serve_forever(&self) {
        loop {
            if let Err(e) = self.serve().await {
                warn!("{e:#}; retrying in 15s");
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
            if let Err(e) = self.refresh_printer().await {
                warn!("{e:#}");
            }
        }
    }

    async fn serve(&self) -> Result<()> {
        let device = self.device();
        let mut destinations = Vec::new();
        for profile in &self.settings.profiles {
            let d = device.register(&profile.name).await?;
            info!("Registered \"{}\" on the printer", d.name);
            destinations.push(d);
        }
        let mut cursor = EventCursor::new(&device).await?;
        self.status.contact();
        info!("Waiting for \"Scan to Computer\" requests");
        loop {
            let found = cursor.next_scan(&device, &destinations, EVENT_POLL).await?;
            self.status.contact();
            let Some((index, comp)) = found else {
                self.flush_pending(false).await;
                for d in &destinations {
                    if device.destination_settings(d).await?.is_none() {
                        bail!("printer forgot \"{}\"", d.name);
                    }
                }
                continue;
            };
            let profile = &self.settings.profiles[index];
            if let Err(e) = self
                .handle_session(&device, index, &destinations[index], &comp)
                .await
            {
                error!("Scan failed: {e:#}");
                self.delivery.failed(profile, &e).await;
            }
            cursor = EventCursor::new(&device).await?;
        }
    }

    /// Polls the walk-up event until it leaves the `waiting` states.
    async fn wait_comp(
        &self,
        device: &Device,
        comp: &str,
        waiting: &[CompEvent],
    ) -> Result<Option<CompEvent>> {
        let deadline = Instant::now() + self.user_timeout + Duration::from_secs(30);
        while Instant::now() < deadline {
            let state = device.comp_event(comp).await?;
            if !waiting.contains(&state) {
                return Ok(Some(state));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Ok(None)
    }

    async fn handle_session(
        &self,
        device: &Device,
        index: usize,
        destination: &Destination,
        comp: &str,
    ) -> Result<()> {
        let state = self
            .wait_comp(device, comp, &[CompEvent::HostSelected])
            .await?;
        if !matches!(
            state,
            Some(CompEvent::ScanRequested | CompEvent::ScanNewPageRequested)
        ) {
            tracing::debug!("Walk-up event is {state:?}, nothing to scan");
            return Ok(());
        }
        let profile = &self.settings.profiles[index];
        let settings = device
            .destination_settings(destination)
            .await?
            .unwrap_or_default();
        let format = profile
            .format
            .unwrap_or(match settings.shortcut.as_deref() {
                Some(s) if s.contains("JPEG") => OutputFormat::Jpeg,
                _ => OutputFormat::Pdf,
            });
        let source = if device.adf_loaded().await? {
            InputSource::Feeder
        } else {
            InputSource::Platen
        };
        info!(
            "Scan requested on \"{}\": shortcut={}, source={source:?}",
            profile.name,
            settings.shortcut.as_deref().unwrap_or("none")
        );
        self.flush_other_pending(index).await;

        let job = self.job_settings(profile, source, settings.duplex);
        let mut pages = self.scan(device, &job).await?;
        if source == InputSource::Platen
            && let Err(e) = self
                .more_platen_pages(device, destination, &job, &mut pages)
                .await
        {
            error!(
                "Next page failed, keeping the {} page(s) already scanned: {e:#}",
                pages.len()
            );
        }

        if !profile.emulated_duplex {
            self.status.scanned(&profile.name);
            self.delivery.spawn(profile.clone(), pages, format);
            return Ok(());
        }
        let fronts = self.pending.lock().unwrap().take();
        let Some(fronts) = fronts else {
            info!(
                "Front sides scanned ({} page(s)). Flip the stack and choose \"{}\" again for the back sides",
                pages.len(),
                profile.name
            );
            self.delivery.notify(
                "Front sides scanned",
                "Flip the pages and scan again for the back sides",
            );
            *self.pending.lock().unwrap() = Some(PendingFronts {
                profile: index,
                pages,
                format,
                since: Instant::now(),
            });
            return Ok(());
        };
        info!(
            "Back sides scanned, merging {} + {} page(s)",
            fronts.pages.len(),
            pages.len()
        );
        let merged = interleave(fronts.pages, pages, source == InputSource::Feeder);
        self.status.scanned(&profile.name);
        self.delivery.spawn(profile.clone(), merged, fronts.format);
        Ok(())
    }

    /// Saves pending front sides on their own when a different profile is used.
    async fn flush_other_pending(&self, index: usize) {
        let taken = {
            let mut pending = self.pending.lock().unwrap();
            match pending.as_ref() {
                Some(p) if p.profile != index => pending.take(),
                _ => None,
            }
        };
        self.save_fronts(taken).await;
    }

    /// Saves pending front sides on their own if they waited too long (or always, on shutdown).
    async fn flush_pending(&self, all: bool) {
        let taken = {
            let mut pending = self.pending.lock().unwrap();
            match pending.as_ref() {
                Some(p) if all || p.since.elapsed() > DUPLEX_TIMEOUT => pending.take(),
                _ => None,
            }
        };
        self.save_fronts(taken).await;
    }

    async fn save_fronts(&self, fronts: Option<PendingFronts>) {
        let Some(fronts) = fronts else {
            return;
        };
        let profile = &self.settings.profiles[fronts.profile];
        info!("Back sides were not scanned; saving the front sides on their own");
        self.delivery
            .spawn(profile.clone(), fronts.pages, fronts.format);
    }

    /// Handles the printer's "scan another page?" prompt after glass scans.
    async fn more_platen_pages(
        &self,
        device: &Device,
        destination: &Destination,
        job: &JobSettings,
        pages: &mut Vec<pdf::Page>,
    ) -> Result<()> {
        loop {
            // Right after each page the printer posts a scan event while the walk-up
            // state still holds the previous answer. Let that pass, then wait for the
            // event caused by the user's choice.
            tokio::time::sleep(Duration::from_secs(3)).await;
            let mut cursor = EventCursor::new(device).await?;
            let state = loop {
                let wait = self.user_timeout + Duration::from_secs(30);
                let found = cursor
                    .next_scan(device, std::slice::from_ref(destination), wait)
                    .await?;
                let Some((_, comp)) = found else {
                    return Ok(());
                };
                let state = device.comp_event(&comp).await?;
                if !matches!(state, CompEvent::ScanRequested | CompEvent::HostSelected) {
                    break state;
                }
            };
            if state != CompEvent::ScanNewPageRequested {
                return Ok(());
            }
            info!("Next page requested");
            pages.extend(self.scan(device, job).await?);
        }
    }

    fn job_settings(&self, profile: &Profile, source: InputSource, duplex: bool) -> JobSettings {
        let caps = match source {
            InputSource::Platen => self.caps.platen,
            InputSource::Feeder => self.caps.adf,
        };
        let cap = |value: u32, max: u32| if max > 0 { value.min(max) } else { value };
        let resolution = cap(profile.resolution, caps.max_dpi);
        if resolution != profile.resolution {
            info!("Using {resolution} DPI, the most this scan source supports");
        }
        let max_width = if caps.max_width > 0 {
            caps.max_width
        } else {
            2550
        };
        let max_height = if caps.max_height > 0 {
            caps.max_height
        } else {
            4200
        };
        // The feeder measures each sheet, so it always gets the full area. Paper size applies to the glass.
        let (width, height) = match (source, profile.paper_size.dimensions()) {
            (InputSource::Platen, Some((w, h))) => (cap(w, max_width), cap(h, max_height)),
            _ => (max_width, max_height),
        };
        if duplex && !self.caps.adf_duplex {
            warn!("2-sided scanning was requested but this scanner's feeder is single-sided");
        }
        JobSettings {
            source,
            resolution,
            color_mode: profile.color.escl(),
            duplex: duplex && source == InputSource::Feeder && self.caps.adf_duplex,
            width,
            height,
        }
    }

    async fn scan(&self, device: &Device, job: &JobSettings) -> Result<Vec<pdf::Page>> {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let job_path = device.create_job(job).await?;
        info!("Scanning ({job_path})");
        let mut pages = Vec::new();
        let result = async {
            while let Some(mut bytes) = device.next_document(&job_path).await? {
                let info = jpeg::inspect_and_fix(&mut bytes)
                    .context("scanner returned an unreadable JPEG")?;
                info!(
                    "Received page {} ({}x{})",
                    pages.len() + 1,
                    info.width,
                    info.height
                );
                self.status.contact();
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
            device.cancel_job(&job_path).await;
            return Err(e);
        }
        if pages.is_empty() {
            bail!("scan job finished without any pages");
        }
        Ok(pages)
    }

    async fn source(&self, device: &Device) -> Result<InputSource> {
        Ok(if device.adf_loaded().await? {
            InputSource::Feeder
        } else {
            InputSource::Platen
        })
    }

    /// One scan with the default profile, without the printer panel.
    pub async fn test_scan(&self) -> Result<()> {
        let device = self.device();
        let source = self.source(&device).await?;
        let profile = self.default_profile();
        info!("Test scan from {source:?}");
        let pages = self
            .scan(&device, &self.job_settings(profile, source, false))
            .await?;
        self.delivery
            .deliver(profile, pages, profile.format.unwrap_or(OutputFormat::Pdf))
            .await?;
        self.delivery.finish().await;
        Ok(())
    }

    /// Scans automatically whenever paper is put in the feeder.
    pub async fn autoscan(&self) -> Result<()> {
        info!(
            "Autoscan: load paper into the feeder and it will scan after {}s",
            self.settings.autoscan_delay
        );
        self.until_shutdown(self.autoscan_forever()).await
    }

    async fn autoscan_forever(&self) {
        // Only arm once the feeder has been seen empty, so a jam or leftover sheet does not rescan forever.
        let mut armed = false;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let device = self.device();
            let loaded = match device.adf_loaded().await {
                Ok(l) => l,
                Err(e) => {
                    warn!("{e:#}; retrying in 15s");
                    tokio::time::sleep(Duration::from_secs(15)).await;
                    let _ = self.refresh_printer().await;
                    continue;
                }
            };
            self.status.contact();
            if !loaded {
                armed = true;
                continue;
            }
            if !armed {
                continue;
            }
            info!("Paper detected");
            tokio::time::sleep(Duration::from_secs(self.settings.autoscan_delay)).await;
            if !device.adf_loaded().await.unwrap_or(false) {
                continue;
            }
            armed = false;
            let profile = self.default_profile();
            let result = async {
                let pages = self
                    .scan(
                        &device,
                        &self.job_settings(profile, InputSource::Feeder, false),
                    )
                    .await?;
                self.status.scanned(&profile.name);
                self.delivery.spawn(
                    profile.clone(),
                    pages,
                    profile.format.unwrap_or(OutputFormat::Pdf),
                );
                anyhow::Ok(())
            }
            .await;
            if let Err(e) = result {
                error!("Autoscan failed: {e:#}");
                self.delivery.failed(profile, &e).await;
            }
        }
    }
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

    /// Waits up to `wait` for a new scan event aimed at one of `destinations`.
    /// Returns the destination's index and the walk-up event path.
    async fn next_scan(
        &mut self,
        device: &Device,
        destinations: &[Destination],
        wait: Duration,
    ) -> Result<Option<(usize, String)>> {
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
                    found = destinations
                        .iter()
                        .enumerate()
                        .find_map(|(i, d)| event.scan_for(&d.path).map(|comp| (i, comp)));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaves_flipped_feeder_stack() {
        assert_eq!(
            interleave(vec![1, 3, 5], vec![6, 4, 2], true),
            [1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn interleaves_glass_in_scan_order() {
        assert_eq!(interleave(vec![1, 3], vec![2, 4], false), [1, 2, 3, 4]);
    }

    #[test]
    fn mismatched_counts_keep_everything() {
        assert_eq!(interleave(vec![1, 3, 5], vec![4, 2], true), [1, 3, 5, 2, 4]);
    }
}
