//! Posts a JSON event after each scan, optionally HMAC-signed or with a bearer token.

use crate::config::WebhookSettings;
use anyhow::{Result, bail};
use hmac::{Hmac, KeyInit, Mac};
use reqwest::Client;
use sha2::Sha256;
use std::time::Duration;

pub const SIGNATURE_HEADER: &str = "X-Signature-256";

pub struct Webhook {
    settings: WebhookSettings,
    http: Client,
}

pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

impl Webhook {
    pub fn new(settings: &WebhookSettings) -> Result<Self> {
        Ok(Self {
            settings: settings.clone(),
            http: Client::builder().timeout(Duration::from_secs(30)).build()?,
        })
    }

    async fn send_once(&self, id: &str, body: &[u8]) -> Result<()> {
        let mut req = self
            .http
            .post(&self.settings.url)
            .header("Content-Type", "application/json")
            .header("Idempotency-Key", id)
            .body(body.to_vec());
        if let Some(secret) = &self.settings.secret {
            req = req.header(SIGNATURE_HEADER, sign(secret, body));
        }
        if let Some(token) = &self.settings.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            bail!("webhook returned {}", resp.status());
        }
        Ok(())
    }

    /// Sends the event with a few retries. The same `id` is reused so receivers can de-duplicate.
    pub async fn send(&self, event: &serde_json::Value) -> Result<()> {
        let id = event["id"].as_str().unwrap_or_default().to_string();
        let body = serde_json::to_vec(event)?;
        let mut attempt = 1;
        loop {
            match self.send_once(&id, &body).await {
                Ok(()) => return Ok(()),
                Err(e) if attempt < 3 => {
                    tracing::warn!("Webhook attempt {attempt} failed: {e:#}");
                    tokio::time::sleep(Duration::from_secs(5 * attempt)).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_known_vector() {
        // RFC 4231 test case 2.
        assert_eq!(
            sign("Jefe", b"what do ya want for nothing?"),
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
