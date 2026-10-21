//! `tessel-server serve --config <file>`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tessel_server::config::Config;
use tessel_server::repo::NoLander;
use tessel_server::server::{self, Deps};

#[derive(Parser)]
#[command(
    name = "tessel-server",
    version,
    about = "The Tessel coordinator server"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the repositories the configuration lists.
    Serve {
        /// The configuration file.
        #[arg(long)]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .init();
    match Cli::parse().command {
        Command::Serve { config } => serve(&config).await,
    }
}

async fn serve(path: &std::path::Path) -> ExitCode {
    let config = match Config::load(path) {
        Ok(config) => config,
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::FAILURE;
        }
    };
    // No identity and no lander are configured yet: the routes answer 401 and a merge is refused.
    let deps = Deps {
        verifier: None,
        calls: Arc::new(NoLander),
    };
    let mut running = match server::start(&config, deps).await {
        Ok(running) => running,
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::FAILURE;
        }
    };
    running.wait().await;
    ExitCode::FAILURE
}
