mod app;
mod config;
mod deliver;
mod device;
mod discovery;
mod health;
mod jpeg;
mod ocr;
mod paperless;
mod pdf;
mod s3;
mod webhook;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::{CliSettings, Settings};

#[derive(Parser)]
#[command(version, about = "Receive \"Scan to Computer\" jobs from HP printers")]
struct Cli {
    #[command(flatten)]
    settings: CliSettings,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Clone, Copy)]
enum Command {
    /// Register with the printer and wait for scans (default)
    Run,
    /// Scan automatically whenever paper is loaded into the feeder
    Autoscan,
    /// Scan once right now (feeder if loaded, else glass) to check everything works
    TestScan,
    /// List scanners found on the network
    Discover,
    /// List "Scan to Computer" entries registered on the printer
    List,
    /// Remove this computer's entries from the printer
    Unregister,
    /// Exit 0 if the health endpoint of a running instance reports healthy
    Healthcheck,
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
    let command = cli.command.unwrap_or(Command::Run);
    if let Command::Discover = command {
        let found = discovery::browse().await?;
        for f in found {
            println!("{}\t{}\t{}", f.address, f.uuid, f.name);
        }
        return Ok(());
    }

    let settings = Settings::load(&cli.settings)?;
    match command {
        Command::Healthcheck => {
            let addr = settings
                .health_listen
                .context("the health endpoint is not enabled (HEALTH_LISTEN)")?;
            if !health::check(addr).await? {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::List => {
            for d in app::connect(&settings.printer)
                .await?
                .destinations()
                .await?
            {
                println!("{}\t{}", d.name, d.path);
            }
            Ok(())
        }
        Command::Unregister => {
            app::App::new(settings).await?.unregister().await;
            Ok(())
        }
        Command::TestScan => app::App::new(settings).await?.test_scan().await,
        Command::Autoscan => app::App::new(settings).await?.autoscan().await,
        Command::Run => app::App::new(settings).await?.run().await,
        Command::Discover => unreachable!(),
    }
}
