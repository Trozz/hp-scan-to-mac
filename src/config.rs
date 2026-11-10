//! Settings come from an optional TOML file, overridden by flags / environment variables.

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    Color,
    Gray,
    Bw,
}

impl ColorMode {
    pub fn escl(self) -> &'static str {
        match self {
            ColorMode::Color => "RGB24",
            ColorMode::Gray => "Grayscale8",
            ColorMode::Bw => "BlackAndWhite1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum PaperSize {
    A4,
    Letter,
    Legal,
    Max,
}

impl PaperSize {
    /// Width and height in 1/300 inch, or `None` for the scanner maximum.
    pub fn dimensions(self) -> Option<(u32, u32)> {
        match self {
            PaperSize::A4 => Some((2480, 3508)),
            PaperSize::Letter => Some((2550, 3300)),
            PaperSize::Legal => Some((2550, 4200)),
            PaperSize::Max => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Pdf,
    Jpeg,
}

/// Flags shared by all commands. Every one can also be set in the config file.
#[derive(Args, Debug, Default)]
pub struct CliSettings {
    /// Config file (default: ~/.config/hp-scan-to-mac/config.toml if it exists)
    #[arg(long, env = "HP_SCAN_CONFIG", global = true)]
    pub config: Option<PathBuf>,

    /// Printer IP or hostname. Leave unset to find the printer automatically
    #[arg(long, env = "HP_PRINTER", global = true)]
    printer: Option<String>,

    /// When discovering, only use a printer whose name or UUID contains this text
    #[arg(long, env = "HP_PRINTER_MATCH", global = true)]
    printer_match: Option<String>,

    /// Name shown on the printer's "Scan to Computer" list (default: this computer's name)
    #[arg(long, env = "HP_SCAN_NAME", global = true)]
    name: Option<String>,

    /// Folder for finished scans (default: ~/Documents/Scans)
    #[arg(long, env = "HP_SCAN_OUTPUT", global = true)]
    output: Option<PathBuf>,

    /// Scan resolution in DPI (capped to what the scanner supports) [default: 300]
    #[arg(long, env = "HP_SCAN_RESOLUTION", global = true)]
    resolution: Option<u32>,

    /// [default: color]
    #[arg(long, env = "HP_SCAN_COLOR", value_enum, global = true)]
    color: Option<ColorMode>,

    /// Page size for scans from the glass [default: a4]
    #[arg(long, env = "HP_SCAN_PAPER_SIZE", value_enum, global = true)]
    paper_size: Option<PaperSize>,

    /// Always save as this format, ignoring the shortcut chosen on the printer
    #[arg(long, env = "HP_SCAN_FORMAT", value_enum, global = true)]
    format: Option<OutputFormat>,

    /// File name pattern (strftime) [default: "Scan %Y-%m-%d %H.%M.%S"]
    #[arg(long, env = "HP_SCAN_FILENAME", global = true)]
    filename: Option<String>,

    /// Keep a copy in the output folder (set false to only upload)
    #[arg(long, env = "HP_SCAN_LOCAL", global = true, num_args = 0..=1, require_equals = true, default_missing_value = "true", value_parser = clap::builder::BoolishValueParser::new())]
    local: Option<bool>,

    /// Add a second "<name> 2-sided" entry for scanning both sides with a one-sided feeder
    #[arg(long, env = "HP_SCAN_EMULATED_DUPLEX", global = true, num_args = 0..=1, require_equals = true, default_missing_value = "true", value_parser = clap::builder::BoolishValueParser::new())]
    emulated_duplex: Option<bool>,

    /// Show a macOS notification for each scan
    #[arg(long, env = "HP_SCAN_NOTIFY", global = true, num_args = 0..=1, require_equals = true, default_missing_value = "true", value_parser = clap::builder::BoolishValueParser::new())]
    notify: Option<bool>,

    /// Shell command run for each saved file; the path is in $1 and $HP_SCAN_FILE
    #[arg(long, env = "HP_SCAN_POST_COMMAND", global = true)]
    post_command: Option<String>,

    /// Paperless-ngx base URL, e.g. http://paperless.local:8000
    #[arg(long, env = "PAPERLESS_URL", global = true)]
    paperless_url: Option<String>,

    /// Paperless-ngx API token (or file:<path> / keychain:<service>)
    #[arg(long, env = "PAPERLESS_TOKEN", global = true, hide_env_values = true)]
    paperless_token: Option<String>,

    /// Comma-separated Paperless tag IDs
    #[arg(long, env = "PAPERLESS_TAGS", value_delimiter = ',', global = true)]
    paperless_tags: Option<Vec<u32>>,

    /// S3-compatible endpoint, e.g. https://s3.eu-west-2.amazonaws.com or http://minio:9000
    #[arg(long, env = "S3_ENDPOINT", global = true)]
    s3_endpoint: Option<String>,

    #[arg(long, env = "S3_BUCKET", global = true)]
    s3_bucket: Option<String>,

    /// [default: us-east-1]
    #[arg(long, env = "S3_REGION", global = true)]
    s3_region: Option<String>,

    /// Key prefix (folder) inside the bucket
    #[arg(long, env = "S3_PREFIX", global = true)]
    s3_prefix: Option<String>,

    #[arg(long, env = "S3_ACCESS_KEY_ID", global = true)]
    s3_access_key_id: Option<String>,

    /// Secret key (or file:<path> / keychain:<service>)
    #[arg(
        long,
        env = "S3_SECRET_ACCESS_KEY",
        global = true,
        hide_env_values = true
    )]
    s3_secret_access_key: Option<String>,

    /// Use path-style URLs (MinIO, some S3-compatible services) [default: false]
    #[arg(long, env = "S3_PATH_STYLE", global = true, num_args = 0..=1, require_equals = true, default_missing_value = "true", value_parser = clap::builder::BoolishValueParser::new())]
    s3_path_style: Option<bool>,

    /// URL to POST a JSON event to after each scan
    #[arg(long, env = "WEBHOOK_URL", global = true)]
    webhook_url: Option<String>,

    /// HMAC-SHA256 signing secret (or file:<path> / keychain:<service>)
    #[arg(long, env = "WEBHOOK_SECRET", global = true, hide_env_values = true)]
    webhook_secret: Option<String>,

    /// Bearer token (or file:<path> / keychain:<service>)
    #[arg(long, env = "WEBHOOK_TOKEN", global = true, hide_env_values = true)]
    webhook_token: Option<String>,

    /// Address for the health endpoint, e.g. 127.0.0.1:8090 (off by default)
    #[arg(long, env = "HEALTH_LISTEN", global = true)]
    health_listen: Option<SocketAddr>,

    /// Seconds to wait after paper is loaded before an autoscan starts [default: 5]
    #[arg(long, env = "AUTOSCAN_DELAY", global = true)]
    autoscan_delay: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSettings {
    printer: Option<String>,
    printer_match: Option<String>,
    name: Option<String>,
    output: Option<PathBuf>,
    resolution: Option<u32>,
    color: Option<ColorMode>,
    paper_size: Option<PaperSize>,
    format: Option<OutputFormat>,
    filename: Option<String>,
    local: Option<bool>,
    emulated_duplex: Option<bool>,
    notify: Option<bool>,
    post_command: Option<String>,
    paperless: Option<FilePaperless>,
    s3: Option<FileS3>,
    webhook: Option<FileWebhook>,
    health: Option<FileHealth>,
    autoscan: Option<FileAutoscan>,
    #[serde(default, rename = "profile")]
    profiles: Vec<FileProfile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePaperless {
    url: Option<String>,
    token: Option<String>,
    tags: Option<Vec<u32>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileS3 {
    endpoint: Option<String>,
    bucket: Option<String>,
    region: Option<String>,
    prefix: Option<String>,
    access_key_id: Option<String>,
    secret_access_key: Option<String>,
    path_style: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWebhook {
    url: Option<String>,
    secret: Option<String>,
    token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileHealth {
    listen: Option<SocketAddr>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileAutoscan {
    delay: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileProfile {
    name: String,
    output: Option<PathBuf>,
    resolution: Option<u32>,
    color: Option<ColorMode>,
    paper_size: Option<PaperSize>,
    format: Option<OutputFormat>,
    filename: Option<String>,
    local: Option<bool>,
    paperless: Option<bool>,
    s3: Option<bool>,
    webhook: Option<bool>,
    emulated_duplex: Option<bool>,
}

#[derive(Debug, Clone)]
pub enum PrinterSource {
    Fixed(String),
    Discover(Option<String>),
}

/// One entry on the printer's "Scan to Computer" list.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub output: PathBuf,
    pub resolution: u32,
    pub color: ColorMode,
    pub paper_size: PaperSize,
    pub format: Option<OutputFormat>,
    pub filename: String,
    pub local: bool,
    pub paperless: bool,
    pub s3: bool,
    pub webhook: bool,
    pub emulated_duplex: bool,
}

#[derive(Debug, Clone)]
pub struct PaperlessSettings {
    pub url: String,
    pub token: String,
    pub tags: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct S3Settings {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub path_style: bool,
}

#[derive(Debug, Clone)]
pub struct WebhookSettings {
    pub url: String,
    pub secret: Option<String>,
    pub token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub printer: PrinterSource,
    pub profiles: Vec<Profile>,
    pub notify: bool,
    pub post_command: Option<String>,
    pub paperless: Option<PaperlessSettings>,
    pub s3: Option<S3Settings>,
    pub webhook: Option<WebhookSettings>,
    pub health_listen: Option<SocketAddr>,
    pub autoscan_delay: u64,
}

pub const DEFAULT_FILENAME: &str = "Scan %Y-%m-%d %H.%M.%S";

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| ".".into())
}

/// Expands a leading `~/` so paths from config files and env vars behave like shell paths.
pub fn expand_tilde(path: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => home().join(rest),
        Err(_) => path.to_path_buf(),
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn default_name() -> String {
    command_output("scutil", &["--get", "ComputerName"])
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .or_else(|| command_output("hostname", &[]))
        .unwrap_or_else(|| "Scanner".into())
}

/// Resolves `file:<path>` and `keychain:<service>[/<account>]` references; other values are used as-is.
pub fn resolve_secret(value: &str) -> Result<String> {
    if let Some(path) = value.strip_prefix("file:") {
        let path = expand_tilde(Path::new(path));
        return std::fs::read_to_string(&path)
            .map(|s| s.trim().to_string())
            .with_context(|| format!("reading secret from {}", path.display()));
    }
    let Some(item) = value.strip_prefix("keychain:") else {
        return Ok(value.to_string());
    };
    let mut args = vec!["find-generic-password", "-w", "-s"];
    match item.split_once('/') {
        Some((service, account)) => args.extend([service, "-a", account]),
        None => args.push(item),
    }
    command_output("security", &args).with_context(|| format!("no Keychain item for {item}"))
}

fn validate_filename(pattern: &str) -> Result<()> {
    chrono::format::StrftimeItems::new(pattern)
        .parse()
        .map(|_| ())
        .map_err(|_| anyhow::anyhow!("invalid filename pattern: {pattern}"))
}

fn default_config_path() -> PathBuf {
    home().join(".config/hp-scan-to-mac/config.toml")
}

fn load_file(path: Option<&Path>) -> Result<FileSettings> {
    let explicit = path.is_some();
    let path = path.map(expand_tilde).unwrap_or_else(default_config_path);
    if !explicit && !path.exists() {
        return Ok(FileSettings::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

impl Settings {
    pub fn load(cli: &CliSettings) -> Result<Self> {
        Self::merge(load_file(cli.config.as_deref())?, cli)
    }

    fn merge(file: FileSettings, cli: &CliSettings) -> Result<Self> {
        let paperless = file.paperless.unwrap_or_default();
        let paperless = match (
            cli.paperless_url.clone().or(paperless.url),
            cli.paperless_token.clone().or(paperless.token),
        ) {
            (Some(url), Some(token)) => Some(PaperlessSettings {
                url: url.trim_end_matches('/').to_string(),
                token: resolve_secret(&token)?,
                tags: cli
                    .paperless_tags
                    .clone()
                    .or(paperless.tags)
                    .unwrap_or_default(),
            }),
            (None, None) => None,
            _ => bail!("Paperless needs both a URL and a token"),
        };

        let s3 = file.s3.unwrap_or_default();
        let s3_endpoint = cli.s3_endpoint.clone().or(s3.endpoint);
        let s3 = match s3_endpoint {
            None => None,
            Some(endpoint) => {
                let need =
                    |v: Option<String>, what: &str| v.with_context(|| format!("S3 needs {what}"));
                Some(S3Settings {
                    endpoint,
                    bucket: need(cli.s3_bucket.clone().or(s3.bucket), "a bucket")?,
                    region: cli
                        .s3_region
                        .clone()
                        .or(s3.region)
                        .unwrap_or_else(|| "us-east-1".into()),
                    prefix: cli.s3_prefix.clone().or(s3.prefix).unwrap_or_default(),
                    access_key_id: need(
                        cli.s3_access_key_id.clone().or(s3.access_key_id),
                        "an access key id",
                    )?,
                    secret_access_key: resolve_secret(&need(
                        cli.s3_secret_access_key.clone().or(s3.secret_access_key),
                        "a secret access key",
                    )?)?,
                    path_style: cli.s3_path_style.or(s3.path_style).unwrap_or(false),
                })
            }
        };

        let webhook = file.webhook.unwrap_or_default();
        let webhook = match cli.webhook_url.clone().or(webhook.url) {
            None => None,
            Some(url) => Some(WebhookSettings {
                url,
                secret: cli
                    .webhook_secret
                    .clone()
                    .or(webhook.secret)
                    .map(|s| resolve_secret(&s))
                    .transpose()?,
                token: cli
                    .webhook_token
                    .clone()
                    .or(webhook.token)
                    .map(|s| resolve_secret(&s))
                    .transpose()?,
            }),
        };

        let base = Profile {
            name: cli.name.clone().or(file.name).unwrap_or_else(default_name),
            output: expand_tilde(
                &cli.output
                    .clone()
                    .or(file.output)
                    .unwrap_or_else(|| home().join("Documents/Scans")),
            ),
            resolution: cli.resolution.or(file.resolution).unwrap_or(300),
            color: cli.color.or(file.color).unwrap_or(ColorMode::Color),
            paper_size: cli.paper_size.or(file.paper_size).unwrap_or(PaperSize::A4),
            format: cli.format.or(file.format),
            filename: cli
                .filename
                .clone()
                .or(file.filename)
                .unwrap_or_else(|| DEFAULT_FILENAME.into()),
            local: cli.local.or(file.local).unwrap_or(true),
            paperless: paperless.is_some(),
            s3: s3.is_some(),
            webhook: webhook.is_some(),
            emulated_duplex: false,
        };

        let mut profiles: Vec<Profile> = file
            .profiles
            .into_iter()
            .map(|p| Profile {
                name: p.name,
                output: p
                    .output
                    .map(|o| expand_tilde(&o))
                    .unwrap_or_else(|| base.output.clone()),
                resolution: p.resolution.unwrap_or(base.resolution),
                color: p.color.unwrap_or(base.color),
                paper_size: p.paper_size.unwrap_or(base.paper_size),
                format: p.format.or(base.format),
                filename: p.filename.unwrap_or_else(|| base.filename.clone()),
                local: p.local.unwrap_or(base.local),
                paperless: p.paperless.unwrap_or(base.paperless),
                s3: p.s3.unwrap_or(base.s3),
                webhook: p.webhook.unwrap_or(base.webhook),
                emulated_duplex: p.emulated_duplex.unwrap_or(false),
            })
            .collect();
        if profiles.is_empty() {
            profiles.push(base.clone());
        }
        if cli
            .emulated_duplex
            .or(file.emulated_duplex)
            .unwrap_or(false)
        {
            profiles.push(Profile {
                name: format!("{} 2-sided", base.name),
                emulated_duplex: true,
                ..base
            });
        }

        for p in &profiles {
            validate_filename(&p.filename)?;
            if p.paperless && paperless.is_none() {
                bail!(
                    "profile \"{}\" uses Paperless, which is not configured",
                    p.name
                );
            }
            if p.s3 && s3.is_none() {
                bail!("profile \"{}\" uses S3, which is not configured", p.name);
            }
            if !p.local && !p.paperless && !p.s3 {
                bail!(
                    "profile \"{}\" has local = false but no Paperless or S3 upload, so scans would be lost",
                    p.name
                );
            }
        }
        let mut names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        names.sort_unstable();
        if names.windows(2).any(|w| w[0] == w[1]) {
            bail!("profile names must be unique");
        }

        Ok(Settings {
            printer: match cli.printer.clone().or(file.printer) {
                Some(host) => PrinterSource::Fixed(host),
                None => PrinterSource::Discover(cli.printer_match.clone().or(file.printer_match)),
            },
            profiles,
            notify: cli.notify.or(file.notify).unwrap_or(false),
            post_command: cli.post_command.clone().or(file.post_command),
            paperless,
            s3,
            webhook,
            health_listen: cli.health_listen.or(file.health.and_then(|h| h.listen)),
            autoscan_delay: cli
                .autoscan_delay
                .or(file.autoscan.and_then(|a| a.delay))
                .unwrap_or(5),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_text: &str, cli: CliSettings) -> Result<Settings> {
        Settings::merge(toml::from_str(toml_text)?, &cli)
    }

    #[test]
    fn defaults_without_file() {
        let s = parse(
            "",
            CliSettings {
                name: Some("Moros".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(matches!(s.printer, PrinterSource::Discover(None)));
        assert_eq!(s.profiles.len(), 1);
        let p = &s.profiles[0];
        assert_eq!(p.name, "Moros");
        assert_eq!(p.resolution, 300);
        assert_eq!(p.paper_size, PaperSize::A4);
        assert!(p.local && !p.paperless);
    }

    #[test]
    fn cli_overrides_file_and_profiles_inherit() {
        let file = r#"
            printer = "10.0.0.1"
            name = "Desk"
            resolution = 200
            color = "gray"
            emulated_duplex = true
            [paperless]
            url = "http://p:8000/"
            token = "abc"
            tags = [1]
            [[profile]]
            name = "Desk Paperless"
            local = false
            [[profile]]
            name = "Desk Photo"
            color = "color"
            paperless = false
        "#;
        let cli = CliSettings {
            resolution: Some(150),
            ..Default::default()
        };
        let s = parse(file, cli).unwrap();
        assert!(matches!(&s.printer, PrinterSource::Fixed(h) if h == "10.0.0.1"));
        assert_eq!(s.paperless.as_ref().unwrap().url, "http://p:8000");
        let names: Vec<_> = s.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Desk Paperless", "Desk Photo", "Desk 2-sided"]);
        assert!(s.profiles.iter().all(|p| p.resolution == 150));
        assert!(!s.profiles[0].local && s.profiles[0].paperless);
        assert_eq!(s.profiles[1].color, ColorMode::Color);
        assert!(!s.profiles[1].paperless);
        assert!(s.profiles[2].emulated_duplex && s.profiles[2].color == ColorMode::Gray);
    }

    #[test]
    fn rejects_profile_that_would_lose_scans() {
        let err = parse("local = false", CliSettings::default()).unwrap_err();
        assert!(err.to_string().contains("would be lost"));
    }

    #[test]
    fn rejects_unknown_keys_and_bad_patterns() {
        assert!(parse("colour = \"gray\"", CliSettings::default()).is_err());
        assert!(parse("filename = \"%Q\"", CliSettings::default()).is_err());
    }

    #[test]
    fn s3_requires_bucket() {
        let err = parse(
            "[s3]\nendpoint = \"http://minio:9000\"",
            CliSettings::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("bucket"));
    }

    #[test]
    fn secrets_from_file() {
        let dir = std::env::temp_dir().join(format!("hpscan-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("token");
        std::fs::write(&f, "s3cret\n").unwrap();
        assert_eq!(
            resolve_secret(&format!("file:{}", f.display())).unwrap(),
            "s3cret"
        );
        assert_eq!(resolve_secret("plain").unwrap(), "plain");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
