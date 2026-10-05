//! `tessel`: claim files on the Tessel coordinator from an AI agent's git worktree.

mod commands;
mod config;
mod daemon;
mod hook;
mod reconcile;
mod render;
mod rpc;
mod scope;
mod state;
mod worktree;

use std::io::Write;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use tessel_coordinator::protocol::Mode;

#[derive(Parser)]
#[command(
    name = "tessel",
    version,
    about = "Claim files before editing them, with other agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start this worktree's daemon, connect, and record what you are about to do.
    Start {
        /// One line on what this work is for; other agents see it when they are denied.
        summary: String,
        /// Task reference, for example an issue id.
        #[arg(long)]
        task: Option<String>,
    },
    /// Claim scopes: `dir/`, `path/file.rs` or `path/file.rs::qualified::name`.
    Claim {
        #[arg(required = true)]
        scopes: Vec<String>,
        #[arg(long, value_enum, default_value_t = ModeArg::EditBody)]
        mode: ModeArg,
        /// Queue behind the holder instead of failing (only when you hold no other claim).
        #[arg(long)]
        wait: bool,
        /// Behaviour you rely on in the first scope but do not own. Repeatable.
        #[arg(long = "assume")]
        assume: Vec<String>,
    },
    /// Daemon state, connection, held claims and unread inbox items.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Notices from the coordinator. Marks them read.
    Inbox {
        /// Show already-read notices too.
        #[arg(long)]
        all: bool,
    },
    /// Release one claim, or all of them.
    Release { claim: Option<u64> },
    /// Release everything, close the socket and stop the daemon.
    Stop,
    /// Claude Code hook integration.
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
    #[command(hide = true)]
    Daemon {
        #[arg(long)]
        summary: String,
        #[arg(long)]
        task: Option<String>,
    },
}

#[derive(Subcommand)]
enum HookAction {
    /// The `PreToolUse` hook: reads the hook JSON on stdin.
    PreEdit,
    /// Add the hook to `.claude/settings.local.json` in this worktree.
    Install,
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Depend,
    EditBody,
    EditSignature,
    Create,
}

impl From<ModeArg> for Mode {
    fn from(arg: ModeArg) -> Self {
        match arg {
            ModeArg::Depend => Mode::Depend,
            ModeArg::EditBody => Mode::EditBody,
            ModeArg::EditSignature => Mode::EditSignature,
            ModeArg::Create => Mode::Create,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => return fail(&anyhow::Error::new(e).context("cannot start the async runtime")),
    };
    match runtime.block_on(commands::run(cli.command)) {
        Ok(code) => code,
        Err(e) => fail(&e),
    }
}

fn fail(error: &anyhow::Error) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "tessel: error: {error:#}");
    ExitCode::from(1)
}
