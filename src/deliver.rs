//! Turns scanned pages into files and sends them to every target a profile uses.

use crate::config::{OutputFormat, Profile, Settings};
use crate::paperless::Paperless;
use crate::pdf;
use crate::s3::S3;
use crate::webhook::Webhook;
use anyhow::Result;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

pub struct Delivery {
    paperless: Option<Paperless>,
    s3: Option<S3>,
    webhook: Option<Webhook>,
    notify: bool,
    post_command: Option<String>,
    tasks: Mutex<JoinSet<()>>,
}

struct OutFile {
    name: String,
    bytes: Vec<u8>,
    mime: &'static str,
}

/// File names for one scan, from the profile's strftime pattern.
fn file_names(pattern: &str, ext: &str, count: usize) -> Vec<String> {
    let stem = chrono::Local::now()
        .format(pattern)
        .to_string()
        .replace(['/', ':'], "-");
    if count == 1 {
        return vec![format!("{stem}.{ext}")];
    }
    (1..=count).map(|n| format!("{stem} p{n}.{ext}")).collect()
}

fn build_files(profile: &Profile, pages: Vec<pdf::Page>, format: OutputFormat) -> Vec<OutFile> {
    match format {
        OutputFormat::Pdf => {
            let name = file_names(&profile.filename, "pdf", 1).remove(0);
            vec![OutFile {
                name,
                bytes: pdf::build(&pages),
                mime: "application/pdf",
            }]
        }
        OutputFormat::Jpeg => file_names(&profile.filename, "jpg", pages.len())
            .into_iter()
            .zip(pages)
            .map(|(name, page)| OutFile {
                name,
                bytes: page.jpeg,
                mime: "image/jpeg",
            })
            .collect(),
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

fn notify(title: &str, message: &str) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        quote(message),
        quote(title)
    );
    let _ = std::process::Command::new("osascript")
        .args(["-e", &script])
        .spawn();
}

fn run_post_command(tasks: &Mutex<JoinSet<()>>, command: &str, path: &Path) {
    let child = tokio::process::Command::new("/bin/sh")
        .args(["-c", command, "hp-scan"])
        .arg(path)
        .env("HP_SCAN_FILE", path)
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return error!("Post command could not start: {e}"),
    };
    let path = path.display().to_string();
    let mut tasks = tasks.lock().unwrap();
    while tasks.try_join_next().is_some() {}
    tasks.spawn(async move {
        match tokio::time::timeout(Duration::from_secs(600), child.wait()).await {
            Ok(Ok(status)) if status.success() => info!("Post command finished for {path}"),
            Ok(Ok(status)) => warn!("Post command for {path} exited with {status}"),
            Ok(Err(e)) => warn!("Post command for {path} failed: {e}"),
            Err(_) => warn!("Post command for {path} timed out and was stopped"),
        }
    });
}

impl Delivery {
    pub fn new(settings: &Settings) -> Result<Self> {
        if settings.notify && !cfg!(target_os = "macos") {
            warn!("Notifications are only available on macOS");
        }
        Ok(Self {
            paperless: settings
                .paperless
                .as_ref()
                .map(Paperless::new)
                .transpose()?,
            s3: settings.s3.as_ref().map(S3::new).transpose()?,
            webhook: settings.webhook.as_ref().map(Webhook::new).transpose()?,
            notify: settings.notify,
            post_command: settings.post_command.clone(),
            tasks: Mutex::new(JoinSet::new()),
        })
    }

    /// Waits for post commands still running, so a short-lived run does not cut them off.
    pub async fn finish(&self) {
        let mut tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
        if tasks.is_empty() {
            return;
        }
        info!("Waiting for post commands to finish");
        while tasks.join_next().await.is_some() {}
    }

    pub fn notify(&self, title: &str, message: &str) {
        if self.notify {
            notify(title, message);
        }
    }

    /// Saves and uploads one scan. Local copies are only removed once every upload succeeded.
    pub async fn deliver(
        &self,
        profile: &Profile,
        pages: Vec<pdf::Page>,
        format: OutputFormat,
    ) -> Result<()> {
        let page_count = pages.len();
        let mut files_json = Vec::new();
        let mut deliveries = Vec::new();
        let mut all_ok = true;

        for file in build_files(profile, pages, format) {
            let path = write_unique(&profile.output, &file.name, &file.bytes).await?;
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let mut uploaded = true;

            if profile.paperless
                && let Some(paperless) = &self.paperless
            {
                match paperless.upload(&path, file.mime).await {
                    Ok(task) => {
                        info!("Sent {name} to Paperless (task {task})");
                        deliveries.push(json!({"target": "paperless", "file": name, "ok": true, "task_id": task}));
                    }
                    Err(e) => {
                        error!("Paperless upload of {name} failed: {e:#}");
                        deliveries.push(json!({"target": "paperless", "file": name, "ok": false, "error": format!("{e:#}")}));
                        uploaded = false;
                    }
                }
            }
            if profile.s3
                && let Some(s3) = &self.s3
            {
                match s3.put(&name, &file.bytes, file.mime).await {
                    Ok(key) => {
                        info!("Sent {name} to S3 ({key})");
                        deliveries
                            .push(json!({"target": "s3", "file": name, "ok": true, "key": key}));
                    }
                    Err(e) => {
                        error!("S3 upload of {name} failed: {e:#}");
                        deliveries.push(json!({"target": "s3", "file": name, "ok": false, "error": format!("{e:#}")}));
                        uploaded = false;
                    }
                }
            }
            all_ok &= uploaded;

            let keep = profile.local || !uploaded;
            if keep {
                info!("Saved {}", path.display());
                if !profile.local {
                    warn!("Kept {} because an upload failed", path.display());
                }
                if let Some(command) = &self.post_command {
                    run_post_command(&self.tasks, command, &path);
                }
            } else {
                let _ = tokio::fs::remove_file(&path).await;
            }
            files_json.push(json!({
                "name": name,
                "path": keep.then(|| path.display().to_string()),
                "size": file.bytes.len(),
                "sha256": hex::encode(Sha256::digest(&file.bytes)),
                "content_type": file.mime,
            }));
        }

        self.notify(
            "Scan received",
            &format!(
                "{page_count} page(s) from \"{}\"{}",
                profile.name,
                if all_ok { "" } else { ", an upload failed" }
            ),
        );
        if profile.webhook {
            self.send_event(json!({
                "event": "scan.completed",
                "profile": profile.name,
                "pages": page_count,
                "files": files_json,
                "deliveries": deliveries,
            }))
            .await;
        }
        Ok(())
    }

    pub async fn failed(&self, profile: &Profile, error: &anyhow::Error) {
        self.notify("Scan failed", &format!("{error:#}"));
        if profile.webhook {
            self.send_event(json!({"event": "scan.failed", "profile": profile.name, "error": format!("{error:#}")}))
                .await;
        }
    }

    async fn send_event(&self, mut event: serde_json::Value) {
        let Some(webhook) = &self.webhook else {
            return;
        };
        event["id"] = json!(uuid::Uuid::new_v4().to_string());
        event["timestamp"] = json!(chrono::Local::now().to_rfc3339());
        if let Err(e) = webhook.send(&event).await {
            error!("Webhook failed: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_pattern_and_number_jpeg_pages() {
        assert_eq!(file_names("Fixed", "pdf", 1), ["Fixed.pdf"]);
        assert_eq!(
            file_names("Fixed", "jpg", 2),
            ["Fixed p1.jpg", "Fixed p2.jpg"]
        );
        assert_eq!(file_names("a/b:c", "pdf", 1), ["a-b-c.pdf"]);
    }
}
