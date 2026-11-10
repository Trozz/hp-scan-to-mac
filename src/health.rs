//! Minimal HTTP health endpoint: `GET /health` returns JSON, 200 when the printer was reached recently.

use anyhow::Result;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const STALE_AFTER: Duration = Duration::from_secs(300);

#[derive(Default)]
struct Inner {
    printer: String,
    last_contact: Option<Instant>,
    last_scan: Option<String>,
}

#[derive(Default)]
pub struct Status(Mutex<Inner>);

impl Status {
    pub fn set_printer(&self, printer: &str) {
        self.0.lock().unwrap().printer = printer.to_string();
    }

    pub fn contact(&self) {
        self.0.lock().unwrap().last_contact = Some(Instant::now());
    }

    pub fn scanned(&self, what: &str) {
        let mut inner = self.0.lock().unwrap();
        inner.last_scan = Some(format!("{} {what}", chrono::Local::now().to_rfc3339()));
        inner.last_contact = Some(Instant::now());
    }

    /// (healthy, JSON body)
    pub fn report(&self) -> (bool, String) {
        let inner = self.0.lock().unwrap();
        let age = inner.last_contact.map(|t| t.elapsed());
        let healthy = age.is_some_and(|a| a < STALE_AFTER);
        let body = serde_json::json!({
            "status": if healthy { "ok" } else { "unhealthy" },
            "printer": inner.printer,
            "last_contact_seconds_ago": age.map(|a| a.as_secs()),
            "last_scan": inner.last_scan,
        });
        (healthy, body.to_string())
    }
}

pub async fn serve(addr: SocketAddr, status: Arc<Status>) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("Health endpoint on http://{addr}/health");
    loop {
        let (mut stream, _) = listener.accept().await?;
        let status = status.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let path = request.split_whitespace().nth(1).unwrap_or("");
            let (code, body) = match path {
                "/health" | "/" => match status.report() {
                    (true, body) => ("200 OK", body),
                    (false, body) => ("503 Service Unavailable", body),
                },
                _ => ("404 Not Found", "{}".to_string()),
            };
            let response = format!(
                "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}

/// Used by `healthcheck` (e.g. Docker HEALTHCHECK): exits non-zero unless the endpoint reports healthy.
pub async fn check(addr: SocketAddr) -> Result<bool> {
    let host = if addr.ip().is_unspecified() {
        "127.0.0.1".to_string()
    } else {
        addr.ip().to_string()
    };
    let url = format!("http://{host}:{}/health", addr.port());
    let resp = reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
    Ok(resp.status().is_success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_only_after_recent_contact() {
        let s = Status::default();
        assert!(!s.report().0);
        s.contact();
        let (ok, body) = s.report();
        assert!(ok);
        assert!(body.contains("\"status\":\"ok\""));
    }
}
