//! Finds eSCL scanners via Bonjour / mDNS (`_uscan._tcp`).
//!
//! On macOS this asks the system Bonjour service through the `dns-sd` tool, which also
//! sees printers on other subnets when the router reflects mDNS. Elsewhere it uses mdns-sd
//! directly (in Docker that needs host networking).

use anyhow::{Result, bail};
use std::time::Duration;

const SERVICE: &str = "_uscan._tcp";
const BROWSE_TIME: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    pub uuid: String,
    pub address: String,
}

impl Found {
    pub fn matches(&self, filter: &str) -> bool {
        let filter = filter.to_lowercase();
        self.name.to_lowercase().contains(&filter) || self.uuid.to_lowercase() == filter
    }
}

/// Every scanner that answered within a few seconds.
pub async fn browse() -> Result<Vec<Found>> {
    if cfg!(target_os = "macos") {
        return bonjour::browse().await;
    }
    tokio::task::spawn_blocking(|| direct::browse(BROWSE_TIME)).await?
}

/// Finds one scanner, optionally filtered by name or exact UUID.
pub async fn find(filter: Option<&str>) -> Result<Found> {
    let mut candidates = browse()
        .await?
        .into_iter()
        .filter(|f| filter.is_none_or(|q| f.matches(q)));
    let Some(first) = candidates.next() else {
        match filter {
            Some(q) => bail!("no scanner matching \"{q}\" found on the network"),
            None => bail!("no scanner found on the network; set HP_PRINTER to its IP address"),
        }
    };
    if candidates.next().is_some() && filter.is_none() {
        tracing::warn!(
            "Several scanners found; using \"{}\". Set HP_PRINTER_MATCH to choose",
            first.name
        );
    }
    Ok(first)
}

mod bonjour {
    use super::{BROWSE_TIME, Found, SERVICE};
    use anyhow::{Context, Result};
    use std::collections::HashMap;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    /// Runs `dns-sd` (which never exits on its own) for `wait`, returning what it printed.
    async fn dns_sd(args: &[&str], wait: Duration) -> Result<Vec<String>> {
        let mut child = Command::new("dns-sd")
            .args(args)
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("running dns-sd")?;
        let mut reader = BufReader::new(child.stdout.take().context("no dns-sd output")?).lines();
        let mut lines = Vec::new();
        let _ = tokio::time::timeout(wait, async {
            while let Ok(Some(line)) = reader.next_line().await {
                lines.push(line);
            }
        })
        .await;
        let _ = child.kill().await;
        Ok(lines)
    }

    /// Decodes DNS-SD escapes such as `\032` (space).
    fn unescape(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            let digits: String = std::iter::from_fn(|| chars.next_if(|d| d.is_ascii_digit()))
                .take(3)
                .collect();
            match digits.parse::<u8>() {
                Ok(code) if digits.len() == 3 => out.push(code as char),
                _ => {
                    out.push_str(&digits);
                    if digits.is_empty()
                        && let Some(next) = chars.next()
                    {
                        out.push(next);
                    }
                }
            }
        }
        out
    }

    /// Parses `dns-sd -Z` zone output into (instance name, host, UUID).
    pub(super) fn parse_zone(lines: &[String]) -> Vec<(String, String, String)> {
        let suffix = format!(".{SERVICE}");
        let mut hosts: HashMap<String, String> = HashMap::new();
        let mut uuids: HashMap<String, String> = HashMap::new();
        for line in lines {
            let mut parts = line.split_whitespace();
            let (Some(owner), Some(kind)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Some(instance) = owner.strip_suffix(&suffix) else {
                continue;
            };
            let instance = unescape(instance);
            match kind {
                "SRV" => {
                    if let Some(host) = parts.nth(3) {
                        hosts.insert(instance, host.trim_end_matches('.').to_string());
                    }
                }
                "TXT" => {
                    let uuid = parts
                        .map(|p| p.trim_matches('"'))
                        .find_map(|p| p.strip_prefix("UUID="))
                        .unwrap_or_default();
                    uuids.insert(instance, uuid.to_string());
                }
                _ => {}
            }
        }
        let mut found: Vec<_> = hosts
            .into_iter()
            .map(|(name, host)| {
                let uuid = uuids.get(&name).cloned().unwrap_or_default();
                (name, host, uuid)
            })
            .collect();
        found.sort();
        found
    }

    /// Parses `dns-sd -G v4` output into the first IPv4 address.
    pub(super) fn parse_address(lines: &[String]) -> Option<String> {
        lines.iter().find_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            (fields.get(1) == Some(&"Add"))
                .then(|| fields.get(5))
                .flatten()
                .filter(|a| a.parse::<std::net::Ipv4Addr>().is_ok())
                .map(|a| a.to_string())
        })
    }

    pub async fn browse() -> Result<Vec<Found>> {
        let zone = dns_sd(&["-Z", SERVICE, "local."], BROWSE_TIME).await?;
        let mut found = Vec::new();
        for (name, host, uuid) in parse_zone(&zone) {
            let lines = dns_sd(&["-G", "v4", &format!("{host}.")], Duration::from_secs(2)).await?;
            match parse_address(&lines) {
                Some(address) => found.push(Found {
                    name,
                    uuid,
                    address,
                }),
                None => tracing::debug!("Could not resolve {host} for \"{name}\""),
            }
        }
        Ok(found)
    }
}

mod direct {
    use super::{Found, SERVICE};
    use anyhow::Result;
    use mdns_sd::{ServiceDaemon, ServiceEvent};
    use std::time::{Duration, Instant};

    pub fn browse(wait: Duration) -> Result<Vec<Found>> {
        let mdns = ServiceDaemon::new()?;
        let service = format!("{SERVICE}.local.");
        let receiver = mdns.browse(&service)?;
        let deadline = Instant::now() + wait;
        let mut found: Vec<Found> = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Ok(event) = receiver.recv_timeout(left) else {
                break;
            };
            let ServiceEvent::ServiceResolved(info) = event else {
                continue;
            };
            let Some(ip) = info.get_addresses_v4().into_iter().next() else {
                continue;
            };
            let uuid = info
                .get_property_val_str("UUID")
                .unwrap_or_default()
                .to_string();
            if !uuid.is_empty() && found.iter().any(|f| f.uuid == uuid) {
                continue;
            }
            let name = info
                .fullname
                .strip_suffix(&format!(".{service}"))
                .unwrap_or(&info.fullname);
            found.push(Found {
                name: name.to_string(),
                uuid,
                address: ip.to_string(),
            });
        }
        let _ = mdns.shutdown();
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_name_substring_or_exact_uuid() {
        let f = Found {
            name: "HP OfficeJet Pro 7720 series [5A0912]".into(),
            uuid: "544e7a7e-6325-5d6b-6216-3082c094d979".into(),
            address: "10.30.0.205".into(),
        };
        assert!(f.matches("officejet"));
        assert!(f.matches("544E7A7E-6325-5D6B-6216-3082C094D979"));
        assert!(!f.matches("544e7a7e"));
        assert!(!f.matches("laserjet"));
    }

    #[test]
    fn parses_dns_sd_zone_and_address() {
        let zone: Vec<String> = [
            "_uscan._tcp                                     PTR     HP\\032OfficeJet\\032Pro\\0327720\\032series\\032[5A0912]._uscan._tcp",
            "HP\\032OfficeJet\\032Pro\\0327720\\032series\\032[5A0912]._uscan._tcp SRV     0 0 8080 HP9C7BEF5A0912.local. ; Replace with unicast FQDN of target host",
            "HP\\032OfficeJet\\032Pro\\0327720\\032series\\032[5A0912]._uscan._tcp TXT     \"txtvers=1\" \"note=Front room\" \"UUID=544e7a7e-6325-5d6b-6216-3082c094d979\" \"duplex=F\"",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            bonjour::parse_zone(&zone),
            [(
                "HP OfficeJet Pro 7720 series [5A0912]".to_string(),
                "HP9C7BEF5A0912.local".to_string(),
                "544e7a7e-6325-5d6b-6216-3082c094d979".to_string()
            )]
        );

        let address: Vec<String> = [
            "Timestamp     A/R  Flags         IF  Hostname                               Address                                      TTL",
            "16:31:11.624  Add  40000002      14  HP9C7BEF5A0912.local.                  10.30.0.205                                  120",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            bonjour::parse_address(&address).as_deref(),
            Some("10.30.0.205")
        );
    }
}
