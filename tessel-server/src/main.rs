//! `tessel-server serve --config <file>`, and `admin keygen` and `admin token` for the keys.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use tessel_server::admin::{self, TokenArgs};
use tessel_server::config::{Config, IdentityConfig};
use tessel_server::credential::{
    askpass_answer, askpass_prompt, ASKPASS_HOSTS_VAR, ASKPASS_TOKEN_FILE_VAR,
};
use tessel_server::identity::{self, Ed25519Verifier};
use tessel_server::repo::NoLander;
use tessel_server::server::{self, Deps};
use tessel_server::ws::Verifier;

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
    /// Keys and tokens for the `[identity]` table of the configuration.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Write a new key pair, `<kid>.key` (mode 0600) and `<kid>.pub`. Never overwrites.
    Keygen {
        #[command(flatten)]
        key: KeyArgs,
    },
    /// Print a token signed with the key of `--kid`, and nothing else, on standard output.
    Token {
        #[command(flatten)]
        key: KeyArgs,
        /// The tenant the token is for.
        #[arg(long)]
        tenant: String,
        /// The repository the token is for.
        #[arg(long)]
        repo: String,
        /// The agent the token names, or a reserved service name with `--service`.
        #[arg(long)]
        agent: String,
        /// How long the token is valid: 30m, 12h or 7d.
        #[arg(long, default_value = "24h", value_parser = admin::parse_ttl)]
        ttl: u64,
        /// Mint a token for a reserved service (lander, runner or server), not an agent.
        #[arg(long)]
        service: bool,
    },
}

#[derive(Args)]
struct KeyArgs {
    /// The configuration file with the `[identity]` table.
    #[arg(long)]
    config: PathBuf,
    /// The key id, a lowercase name such as `2026-10`.
    #[arg(long)]
    kid: String,
}

/// Answers git's prompt on standard output. Nothing is logged: git shows this process's standard
/// error to the user, and the answer is a credential.
fn askpass(prompt: &str, token_file: Option<&OsStr>) -> ExitCode {
    let hosts = std::env::var_os(ASKPASS_HOSTS_VAR);
    let Some(answer) = askpass_answer(prompt, token_file, hosts.as_deref()) else {
        return ExitCode::FAILURE;
    };
    match writeln!(std::io::stdout(), "{answer}") {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    // Git runs the askpass hook as `<program> <prompt>`, with no subcommand, so the lander's
    // environment variable is what says this process is the hook.
    let args: Vec<OsString> = std::env::args_os().collect();
    let token_file = std::env::var_os(ASKPASS_TOKEN_FILE_VAR);
    if let Some(prompt) = askpass_prompt(&args, token_file.as_deref()) {
        return askpass(&prompt, token_file.as_deref());
    }
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .init();
    let outcome = match cli.command {
        Command::Serve { config } => return serve(&config).await,
        Command::Admin { command } => run_admin(command),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run_admin(command: AdminCommand) -> Result<(), String> {
    match command {
        AdminCommand::Keygen { key } => {
            let identity = load_identity(&key.config)?;
            admin::keygen(&identity, &key.kid).map_err(|error| error.to_string())?;
            tracing::info!(kid = %key.kid, dir = %identity.keys.display(), "wrote a key pair");
            Ok(())
        }
        AdminCommand::Token {
            key,
            tenant,
            repo,
            agent,
            ttl,
            service,
        } => {
            let identity = load_identity(&key.config)?;
            let args = TokenArgs {
                kid: &key.kid,
                tenant: &tenant,
                repo: &repo,
                agent: &agent,
                ttl_ms: ttl,
                service,
            };
            let minted = admin::token(&identity, &args, identity::now_ms())
                .map_err(|error| error.to_string())?;
            print_token(&minted);
            Ok(())
        }
    }
}

#[expect(
    clippy::print_stdout,
    reason = "the token on standard output is the whole result of `admin token`"
)]
fn print_token(token: &str) {
    println!("{token}");
}

/// The configuration at `path` and its `[identity]` table, which the admin commands need.
fn load_identity(path: &Path) -> Result<IdentityConfig, String> {
    let config = Config::load(path).map_err(|error| error.to_string())?;
    let identity = admin::identity_of(&config, &path.display().to_string())
        .map_err(|error| error.to_string())?
        .clone();
    Ok(identity)
}

/// The verifier the `[identity]` table asks for, or none if the table is absent.
fn build_verifier(path: &Path, config: &Config) -> Result<Option<Arc<dyn Verifier>>, String> {
    let Some(identity) = &config.identity else {
        return Ok(None);
    };
    let verifier = Ed25519Verifier::load(&identity.keys, &identity.audience).map_err(|error| {
        format!(
            "the configuration {}: [identity] keys = {:?}: {error}",
            path.display(),
            identity.keys.display().to_string()
        )
    })?;
    tracing::info!(
        audience = %identity.audience,
        kids = ?verifier.kids(),
        "identity loaded"
    );
    Ok(Some(Arc::new(verifier)))
}

async fn serve(path: &Path) -> ExitCode {
    let config = match Config::load(path) {
        Ok(config) => config,
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let verifier = match build_verifier(path, &config) {
        Ok(verifier) => verifier,
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::FAILURE;
        }
    };
    // A repository that names a `remote` is landed by the lander `server::start` builds from the
    // configuration; the others get this port, which refuses every merge.
    let deps = Deps {
        verifier,
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
