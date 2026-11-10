//! HTTP client for the HP LEDM "WalkupScanToComp" and eSCL interfaces.
//!
//! XML templates follow node-hp-scan-to (MIT, Emmanuel Counasse), since HP
//! firmware is strict about namespaces.

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client, StatusCode, header};
use std::time::Duration;

const WALKUP_DESTINATIONS: &str = "/WalkupScanToComp/WalkupScanToCompDestinations";
const EVENT_TABLE: &str = "/EventMgmt/EventTable";

pub struct Device {
    host: String,
    base: String,
    http: Client,
}

#[derive(Debug, Clone)]
pub struct Destination {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Default)]
pub struct DestinationSettings {
    pub shortcut: Option<String>,
    pub duplex: bool,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub category: String,
    pub stamp: String,
    pub payloads: Vec<(String, String)>,
}

impl Event {
    fn payload(&self, type_contains: &str) -> Option<&str> {
        self.payloads
            .iter()
            .find(|(_, t)| t.contains(type_contains))
            .map(|(uri, _)| uri.as_str())
    }

    /// The comp-event URI when this is a scan event aimed at `destination_path`.
    pub fn scan_for(&self, destination_path: &str) -> Option<String> {
        if self.category != "ScanEvent" {
            return None;
        }
        if !self.payload("Destination")?.contains(destination_path) {
            return None;
        }
        self.payload("CompEvent").map(to_path)
    }
}

pub enum EventPoll {
    Unchanged,
    Changed { etag: String, events: Vec<Event> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompEvent {
    HostSelected,
    ScanRequested,
    ScanNewPageRequested,
    ScanPagesComplete,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSource {
    Platen,
    Feeder,
}

#[derive(Debug, Clone, Copy)]
pub struct JobSettings {
    pub source: InputSource,
    pub resolution: u32,
    pub color_mode: &'static str,
    pub duplex: bool,
    /// Scan area in 1/300 inch.
    pub width: u32,
    pub height: u32,
}

/// Strips scheme and host so absolute `Location` headers and plain paths are treated alike.
pub fn to_path(uri: &str) -> String {
    let Some(rest) = uri.split_once("://").map(|(_, r)| r) else {
        return uri.to_string();
    };
    rest.find('/').map_or("/".into(), |i| rest[i..].to_string())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn child_text<'a>(node: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|n| n.tag_name().name() == name)
        .and_then(|n| n.text())
        .map(str::trim)
}

fn descendant_text<'a>(doc: &'a roxmltree::Document<'a>, name: &str) -> Option<&'a str> {
    doc.descendants()
        .find(|n| n.tag_name().name() == name)
        .and_then(|n| n.text())
        .map(str::trim)
}

pub fn parse_destinations(xml: &str) -> Result<Vec<Destination>> {
    let doc = roxmltree::Document::parse(xml)?;
    Ok(doc
        .descendants()
        .filter(|n| n.tag_name().name() == "WalkupScanToCompDestination")
        .filter_map(|n| {
            Some(Destination {
                name: child_text(n, "Name")?.to_string(),
                path: to_path(child_text(n, "ResourceURI")?),
            })
        })
        .collect())
}

pub fn parse_destination_settings(xml: &str) -> Result<DestinationSettings> {
    let doc = roxmltree::Document::parse(xml)?;
    Ok(DestinationSettings {
        shortcut: descendant_text(&doc, "Shortcut").map(String::from),
        duplex: descendant_text(&doc, "ScanPlexMode") == Some("Duplex"),
    })
}

pub fn parse_events(xml: &str) -> Result<Vec<Event>> {
    let doc = roxmltree::Document::parse(xml)?;
    Ok(doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Event")
        .map(|ev| Event {
            category: child_text(ev, "UnqualifiedEventCategory")
                .unwrap_or_default()
                .to_string(),
            stamp: child_text(ev, "AgingStamp").unwrap_or_default().to_string(),
            payloads: ev
                .children()
                .filter(|p| p.tag_name().name() == "Payload")
                .filter_map(|p| {
                    Some((
                        child_text(p, "ResourceURI")?.to_string(),
                        child_text(p, "ResourceType")?.to_string(),
                    ))
                })
                .collect(),
        })
        .collect())
}

pub fn parse_comp_event(xml: &str) -> Result<CompEvent> {
    let doc = roxmltree::Document::parse(xml)?;
    Ok(match descendant_text(&doc, "WalkupScanToCompEventType") {
        Some("HostSelected") => CompEvent::HostSelected,
        Some("ScanRequested") => CompEvent::ScanRequested,
        Some("ScanNewPageRequested") => CompEvent::ScanNewPageRequested,
        Some("ScanPagesComplete") => CompEvent::ScanPagesComplete,
        _ => CompEvent::Other,
    })
}

pub fn parse_adf_loaded(xml: &str) -> Result<bool> {
    let doc = roxmltree::Document::parse(xml)?;
    Ok(descendant_text(&doc, "AdfState") == Some("ScannerAdfLoaded"))
}

pub fn parse_user_action_timeout(xml: &str) -> Result<Option<u64>> {
    let doc = roxmltree::Document::parse(xml)?;
    Ok(doc
        .descendants()
        .find(|n| n.tag_name().name() == "UserActionTimeout")
        .and_then(|n| child_text(n, "ValueFloat"))
        .and_then(|v| v.parse::<f64>().ok())
        .map(|v| v as u64))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SourceCaps {
    pub max_dpi: u32,
    pub max_width: u32,
    pub max_height: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ScannerCaps {
    pub adf_duplex: bool,
    pub platen: SourceCaps,
    pub adf: SourceCaps,
}

fn source_caps(node: Option<roxmltree::Node>) -> SourceCaps {
    let Some(node) = node else {
        return SourceCaps::default();
    };
    let max_of = |name: &str| {
        node.descendants()
            .filter(|n| n.tag_name().name() == name)
            .filter_map(|n| n.text()?.trim().parse().ok())
            .max()
            .unwrap_or(0)
    };
    SourceCaps {
        max_dpi: max_of("XResolution"),
        max_width: max_of("MaxWidth"),
        max_height: max_of("MaxHeight"),
    }
}

pub fn parse_scanner_caps(xml: &str) -> Result<ScannerCaps> {
    let doc = roxmltree::Document::parse(xml)?;
    let find = |name: &str| doc.descendants().find(|n| n.tag_name().name() == name);
    Ok(ScannerCaps {
        adf_duplex: find("AdfDuplexInputCaps").is_some(),
        platen: source_caps(find("PlatenInputCaps")),
        adf: source_caps(find("AdfSimplexInputCaps")),
    })
}

pub fn destination_xml(name: &str) -> String {
    let name = xml_escape(name);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<WalkupScanToCompDestination xmlns="http://www.hp.com/schemas/imaging/con/ledm/walkupscan/2010/09/28" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:schemaLocation="http://www.hp.com/schemas/imaging/con/ledm/walkupscan/2010/09/28 WalkupScanToComp.xsd">
  <Hostname xmlns="http://www.hp.com/schemas/imaging/con/dictionaries/2009/04/06">{name}</Hostname>
  <Name xmlns="http://www.hp.com/schemas/imaging/con/dictionaries/1.0/">{name}</Name>
  <LinkType>Network</LinkType>
</WalkupScanToCompDestination>"#
    )
}

pub fn escl_job_xml(s: &JobSettings) -> String {
    let source = match s.source {
        InputSource::Platen => "Platen",
        InputSource::Feeder => "Feeder",
    };
    let threshold = if s.color_mode == "BlackAndWhite1" {
        "\n\t<Threshold>128</Threshold>"
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ScanSettings xmlns="http://schemas.hp.com/imaging/escl/2011/05/03" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:schemaLocation="http://schemas.hp.com/imaging/escl/2011/05/03 Scan Schema - 0.26.xsd">
	<Version xmlns="http://www.pwg.org/schemas/2010/12/sm">2.62</Version>
	<Intent>Document</Intent>
	<ScanRegions xmlns="http://www.pwg.org/schemas/2010/12/sm" xmlns:n0="http://www.pwg.org/schemas/2010/12/sm" n0:MustHonor="false">
		<ScanRegion>
			<Height>{height}</Height>
			<Width>{width}</Width>
			<XOffset>0</XOffset>
			<YOffset>0</YOffset>
			<ContentRegionUnits>escl:ThreeHundredthsOfInches</ContentRegionUnits>
		</ScanRegion>
	</ScanRegions>
	<DocumentFormatExt>image/jpeg</DocumentFormatExt>
	<InputSource xmlns="http://www.pwg.org/schemas/2010/12/sm">{source}</InputSource>
	<XResolution>{res}</XResolution>
	<YResolution>{res}</YResolution>
	<ColorMode>{mode}</ColorMode>
	<Duplex>{duplex}</Duplex>
	<MultipickDetection>true</MultipickDetection>
	<ShowMultipickResolveDialog>true</ShowMultipickResolveDialog>
	<MultipickExclusionLength>0</MultipickExclusionLength>
	<AutoCrop>false</AutoCrop>
	<Brightness>996</Brightness>
	<CompressionFactor>0</CompressionFactor>
	<Contrast>996</Contrast>
	<Gamma>180</Gamma>
	<Highlight>1396</Highlight>
	<Shadow>70</Shadow>
	<Overscan>true</Overscan>{threshold}
</ScanSettings>"#,
        height = s.height,
        width = s.width,
        res = s.resolution,
        mode = s.color_mode,
        duplex = s.duplex,
    )
}

impl Device {
    pub fn new(host: &str) -> Result<Self> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            host: host.to_string(),
            base: format!("http://{host}"),
            http,
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    async fn get_text(&self, path: &str) -> Result<(StatusCode, String)> {
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        let status = resp.status();
        Ok((status, resp.text().await?))
    }

    async fn get_ok(&self, path: &str) -> Result<String> {
        let (status, body) = self.get_text(path).await?;
        if !status.is_success() {
            bail!("GET {path} returned {status}");
        }
        Ok(body)
    }

    /// POSTs XML expecting `201 Created`; retries up to `busy_retries` times while the device answers 503.
    async fn post_xml(&self, path: &str, xml: String, busy_retries: u32) -> Result<String> {
        let mut retries = 0;
        let resp = loop {
            let resp = self
                .http
                .post(format!("{}{path}", self.base))
                .header(header::CONTENT_TYPE, "text/xml")
                .body(xml.clone())
                .send()
                .await
                .with_context(|| format!("POST {path}"))?;
            if resp.status() != StatusCode::SERVICE_UNAVAILABLE || retries >= busy_retries {
                break resp;
            }
            retries += 1;
            tokio::time::sleep(Duration::from_secs(1)).await;
        };
        let status = resp.status();
        let location = resp
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(to_path);
        if status != StatusCode::CREATED {
            let body = resp.text().await.unwrap_or_default();
            bail!("POST {path} returned {status}: {body}");
        }
        location.ok_or_else(|| anyhow!("POST {path} returned no Location header"))
    }

    pub async fn user_action_timeout(&self) -> Result<Option<u64>> {
        parse_user_action_timeout(
            &self
                .get_ok("/WalkupScanToComp/WalkupScanToCompCaps")
                .await?,
        )
    }

    pub async fn destinations(&self) -> Result<Vec<Destination>> {
        parse_destinations(&self.get_ok(WALKUP_DESTINATIONS).await?)
    }

    /// Returns the existing destination with this name, or registers a new one.
    pub async fn register(&self, name: &str) -> Result<Destination> {
        if let Some(existing) = self
            .destinations()
            .await?
            .into_iter()
            .find(|d| d.name == name)
        {
            return Ok(existing);
        }
        let path = self
            .post_xml(WALKUP_DESTINATIONS, destination_xml(name), 0)
            .await?;
        Ok(Destination {
            name: name.to_string(),
            path,
        })
    }

    pub async fn unregister(&self, destination: &Destination) -> Result<()> {
        let resp = self
            .http
            .delete(format!("{}{}", self.base, destination.path))
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("DELETE {} returned {}", destination.path, resp.status());
        }
        Ok(())
    }

    /// `None` when the printer no longer knows this destination (e.g. after a reboot).
    pub async fn destination_settings(
        &self,
        destination: &Destination,
    ) -> Result<Option<DestinationSettings>> {
        let (status, body) = self.get_text(&destination.path).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            bail!("GET {} returned {status}", destination.path);
        }
        parse_destination_settings(&body).map(Some)
    }

    /// Long-polls the event table. `timeout` is how long the printer may hold the request.
    pub async fn poll_events(&self, etag: Option<&str>, timeout: Duration) -> Result<EventPoll> {
        let deciseconds = (timeout.as_millis() / 100).clamp(10, 1200);
        let mut req = self
            .http
            .get(format!("{}{EVENT_TABLE}?timeout={deciseconds}", self.base))
            .timeout(Duration::from_millis(deciseconds as u64 * 100 + 15_000));
        if let Some(etag) = etag {
            req = req.header(header::IF_NONE_MATCH, etag);
        }
        let resp = req.send().await.context("GET event table")?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(EventPoll::Unchanged);
        }
        if !resp.status().is_success() {
            bail!("GET event table returned {}", resp.status());
        }
        let etag = resp
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow!("event table response has no ETag"))?
            .to_string();
        let body = resp.text().await?;
        tracing::debug!("event table changed: {body}");
        let events = parse_events(&body)?;
        Ok(EventPoll::Changed { etag, events })
    }

    /// Current event-table ETag and events, used as a baseline so stale events are ignored.
    pub async fn event_baseline(&self) -> Result<(String, Vec<Event>)> {
        let resp = self
            .http
            .get(format!("{}{EVENT_TABLE}", self.base))
            .send()
            .await?;
        let etag = resp
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .ok_or_else(|| anyhow!("event table response has no ETag"))?;
        Ok((etag, parse_events(&resp.text().await?)?))
    }

    pub async fn comp_event(&self, path: &str) -> Result<CompEvent> {
        parse_comp_event(&self.get_ok(path).await?)
    }

    pub async fn adf_loaded(&self) -> Result<bool> {
        parse_adf_loaded(&self.get_ok("/eSCL/ScannerStatus").await?)
    }

    pub async fn scanner_caps(&self) -> Result<ScannerCaps> {
        parse_scanner_caps(&self.get_ok("/eSCL/ScannerCapabilities").await?)
    }

    pub async fn create_job(&self, settings: &JobSettings) -> Result<String> {
        self.post_xml("/eSCL/ScanJobs", escl_job_xml(settings), 30)
            .await
    }

    /// Next page of a job as JPEG bytes, or `None` once the job has no more pages.
    pub async fn next_document(&self, job_path: &str) -> Result<Option<Vec<u8>>> {
        let url = format!("{}{job_path}/NextDocument", self.base);
        for _ in 0..60 {
            let resp = self
                .http
                .get(&url)
                .timeout(Duration::from_secs(180))
                .send()
                .await
                .context("GET NextDocument")?;
            match resp.status() {
                StatusCode::OK => return Ok(Some(resp.bytes().await?.to_vec())),
                StatusCode::NOT_FOUND | StatusCode::GONE => return Ok(None),
                StatusCode::SERVICE_UNAVAILABLE => tokio::time::sleep(Duration::from_secs(1)).await,
                other => bail!("GET NextDocument returned {other}"),
            }
        }
        bail!("scanner stayed busy while waiting for the next page")
    }

    pub async fn cancel_job(&self, job_path: &str) {
        let _ = self
            .http
            .delete(format!("{}{job_path}", self.base))
            .send()
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_path_handles_absolute_and_relative() {
        assert_eq!(
            to_path("http://10.0.0.1:8080/eSCL/ScanJobs/abc"),
            "/eSCL/ScanJobs/abc"
        );
        assert_eq!(to_path("/eSCL/ScanJobs/abc"), "/eSCL/ScanJobs/abc");
    }

    #[test]
    fn parses_scan_event() {
        let xml = r#"<?xml version="1.0"?>
<ev:EventTable xmlns:ev="http://www.hp.com/schemas/imaging/con/ledm/events/2007/09/16" xmlns:dd="http://www.hp.com/schemas/imaging/con/dictionaries/1.0/">
  <ev:Event>
    <dd:UnqualifiedEventCategory>ScanEvent</dd:UnqualifiedEventCategory>
    <dd:AgingStamp>1391-5</dd:AgingStamp>
    <ev:Payload>
      <dd:ResourceURI>/WalkupScanToComp/WalkupScanToCompDestinations/1c8-7a</dd:ResourceURI>
      <dd:ResourceType>wus:WalkupScanToCompDestination</dd:ResourceType>
    </ev:Payload>
    <ev:Payload>
      <dd:ResourceURI>/WalkupScanToComp/WalkupScanToCompEvent</dd:ResourceURI>
      <dd:ResourceType>wus:WalkupScanToCompEvent</dd:ResourceType>
    </ev:Payload>
  </ev:Event>
  <ev:Event>
    <dd:UnqualifiedEventCategory>JobEvent</dd:UnqualifiedEventCategory>
  </ev:Event>
</ev:EventTable>"#;
        let events = parse_events(xml).unwrap();
        assert_eq!(events.len(), 2);
        let ours = "/WalkupScanToComp/WalkupScanToCompDestinations/1c8-7a";
        assert_eq!(
            events[0].scan_for(ours).as_deref(),
            Some("/WalkupScanToComp/WalkupScanToCompEvent")
        );
        assert_eq!(
            events[0].scan_for("/WalkupScanToComp/WalkupScanToCompDestinations/other"),
            None
        );
        assert_eq!(events[1].scan_for(ours), None);
    }

    #[test]
    fn parses_destinations_and_settings() {
        let list = r#"<wus:WalkupScanToCompDestinations xmlns:wus="w" xmlns:dd="d" xmlns:dd3="d3">
<wus:WalkupScanToCompDestination><dd:ResourceURI>http://p/WalkupScanToComp/WalkupScanToCompDestinations/1</dd:ResourceURI><dd:Name>Moros</dd:Name><dd3:Hostname>Moros</dd3:Hostname></wus:WalkupScanToCompDestination>
</wus:WalkupScanToCompDestinations>"#;
        let d = parse_destinations(list).unwrap();
        assert_eq!(d[0].name, "Moros");
        assert_eq!(
            d[0].path,
            "/WalkupScanToComp/WalkupScanToCompDestinations/1"
        );

        let one = r#"<wus:WalkupScanToCompDestination xmlns:wus="w" xmlns:dd="d" xmlns:scantype="s">
<wus:WalkupScanToCompSettings><scantype:ScanSettings><dd:ScanPlexMode>Simplex</dd:ScanPlexMode></scantype:ScanSettings><wus:Shortcut>SaveJPEG</wus:Shortcut></wus:WalkupScanToCompSettings>
</wus:WalkupScanToCompDestination>"#;
        let s = parse_destination_settings(one).unwrap();
        assert_eq!(s.shortcut.as_deref(), Some("SaveJPEG"));
        assert!(!s.duplex);
    }

    #[test]
    fn parses_caps_per_source() {
        let xml = r#"<scan:ScannerCapabilities xmlns:scan="s">
<scan:Platen><scan:PlatenInputCaps><scan:MaxWidth>2550</scan:MaxWidth><scan:MaxHeight>4217</scan:MaxHeight>
<scan:XResolution>600</scan:XResolution><scan:XResolution>1200</scan:XResolution></scan:PlatenInputCaps></scan:Platen>
<scan:Adf><scan:AdfSimplexInputCaps><scan:MaxWidth>2550</scan:MaxWidth><scan:MaxHeight>4200</scan:MaxHeight>
<scan:XResolution>300</scan:XResolution></scan:AdfSimplexInputCaps></scan:Adf></scan:ScannerCapabilities>"#;
        let caps = parse_scanner_caps(xml).unwrap();
        assert!(!caps.adf_duplex);
        assert_eq!(
            (
                caps.platen.max_dpi,
                caps.platen.max_width,
                caps.platen.max_height
            ),
            (1200, 2550, 4217)
        );
        assert_eq!((caps.adf.max_dpi, caps.adf.max_height), (300, 4200));
    }

    #[test]
    fn destination_name_is_escaped() {
        assert!(destination_xml("A&B").contains(">A&amp;B</Name>"));
    }
}
