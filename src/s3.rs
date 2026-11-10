//! Uploads to S3-compatible storage with presigned PUT requests.

use crate::config::S3Settings;
use anyhow::{Context, Result, bail};
use reqwest::Client;
use rusty_s3::actions::{PutObject, S3Action as _};
use rusty_s3::{Bucket, Credentials, UrlStyle};
use std::time::Duration;

pub struct S3 {
    bucket: Bucket,
    credentials: Credentials,
    prefix: String,
    http: Client,
}

impl S3 {
    pub fn new(settings: &S3Settings) -> Result<Self> {
        let endpoint = settings
            .endpoint
            .parse()
            .context("invalid S3 endpoint URL")?;
        let style = if settings.path_style {
            UrlStyle::Path
        } else {
            UrlStyle::VirtualHost
        };
        Ok(Self {
            bucket: Bucket::new(
                endpoint,
                style,
                settings.bucket.clone(),
                settings.region.clone(),
            )?,
            credentials: Credentials::new(
                settings.access_key_id.clone(),
                settings.secret_access_key.clone(),
            ),
            prefix: settings.prefix.trim_matches('/').to_string(),
            http: Client::builder()
                .timeout(Duration::from_secs(300))
                .build()?,
        })
    }

    fn key(&self, file_name: &str) -> String {
        if self.prefix.is_empty() {
            return file_name.to_string();
        }
        format!("{}/{file_name}", self.prefix)
    }

    async fn put_once(&self, key: &str, bytes: &[u8], mime: &str) -> Result<()> {
        let url = PutObject::new(&self.bucket, Some(&self.credentials), key)
            .sign(Duration::from_secs(900));
        let resp = self
            .http
            .put(url)
            .header("Content-Type", mime)
            .body(bytes.to_vec())
            .send()
            .await
            // The URL holds a temporary signature, so keep it out of logs.
            .map_err(|e| e.without_url())?;
        if !resp.status().is_success() {
            let status = resp.status();
            bail!(
                "S3 returned {status}: {}",
                resp.text().await.unwrap_or_default()
            );
        }
        Ok(())
    }

    /// Uploads with a few retries; returns the object key.
    pub async fn put(&self, file_name: &str, bytes: &[u8], mime: &str) -> Result<String> {
        let key = self.key(file_name);
        let mut attempt = 1;
        loop {
            match self.put_once(&key, bytes, mime).await {
                Ok(()) => return Ok(key),
                Err(e) if attempt < 3 => {
                    tracing::warn!("S3 upload attempt {attempt} failed: {e:#}");
                    tokio::time::sleep(Duration::from_secs(5 * attempt)).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
}
