//! Uploads finished scans to Paperless-ngx via POST /api/documents/post_document/.

use anyhow::{Result, bail};
use reqwest::{Client, multipart};
use std::path::Path;
use std::time::Duration;

pub struct Paperless {
    url: String,
    token: String,
    tags: Vec<u32>,
    http: Client,
}

impl Paperless {
    pub fn new(url: &str, token: &str, tags: Vec<u32>) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            url: url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            tags,
            http,
        })
    }

    async fn upload_once(&self, path: &Path, mime: &str) -> Result<String> {
        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let title = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let part = multipart::Part::bytes(tokio::fs::read(path).await?)
            .file_name(file_name)
            .mime_str(mime)?;
        let mut form = multipart::Form::new()
            .part("document", part)
            .text("title", title);
        for tag in &self.tags {
            form = form.text("tags", tag.to_string());
        }
        let resp = self
            .http
            .post(format!("{}/api/documents/post_document/", self.url))
            .header("Authorization", format!("Token {}", self.token))
            .multipart(form)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Paperless returned {status}: {body}");
        }
        Ok(body.trim().trim_matches('"').to_string())
    }

    /// Uploads with a few retries; returns the Paperless consumption task id.
    pub async fn upload(&self, path: &Path, mime: &str) -> Result<String> {
        let mut attempt = 1;
        loop {
            match self.upload_once(path, mime).await {
                Ok(task) => return Ok(task),
                Err(e) if attempt < 3 => {
                    tracing::warn!("Paperless upload attempt {attempt} failed: {e:#}");
                    tokio::time::sleep(Duration::from_secs(5 * attempt)).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
}
