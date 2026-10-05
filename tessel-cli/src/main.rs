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
mod submit;
mod worktree;

use std::io::Write;
use std::path::PathBuf;
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
    /// Hand finished work to the steward to merge. Commit and push it to your fork first.
    Submit {
        /// The claim to submit; required when you hold several.
        #[arg(long)]
        claim: Option<u64>,
        /// What shows the work is right, for example "cargo test passed (42 tests)". At least
        /// one is required. Repeatable.
        #[arg(long = "evidence")]
        evidence: Vec<String>,
        /// An approach you tried and dropped: "<approach>::<reason>". Repeatable.
        #[arg(long = "rejected")]
        rejected: Vec<String>,
        /// The commit to merge, already pushed to your fork; defaults to HEAD.
        #[arg(long)]
        commit: Option<String>,
    },
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
    PreEdit {
        /// The worktree this hook guards; `hook install` writes it. Falls back to
        /// `$CLAUDE_PROJECT_DIR`.
        #[arg(long)]
        root: Option<PathBuf>,
    },
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
    // Claude Code lets a tool run when its hook exits with anything but 2, so the pre-edit hook
    // reports every failure as 2.
    let failure = match cli.command {
        Command::Hook {
            action: HookAction::PreEdit { .. },
        } => {
            block_on_panic();
            hook::EXIT_BLOCK
        }
        Command::Start { .. }
        | Command::Claim { .. }
        | Command::Status { .. }
        | Command::Inbox { .. }
        | Command::Release { .. }
        | Command::Submit { .. }
        | Command::Stop
        | Command::Hook {
            action: HookAction::Install,
        }
        | Command::Daemon { .. } => 1,
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            return fail(
                &anyhow::Error::new(e).context("cannot start the async runtime"),
                failure,
            )
        }
    };
    match runtime.block_on(commands::run(cli.command)) {
        Ok(code) => code,
        Err(e) => fail(&e, failure),
    }
}

/// A panic would otherwise end the process with 101, which lets the edit through.
fn block_on_panic() {
    std::panic::set_hook(Box::new(|info| {
        let _ = writeln!(
            std::io::stderr(),
            "tessel hook: internal error ({info}); blocking the edit"
        );
        #[expect(clippy::exit, reason = "the hook must exit 2 even when it panics")]
        std::process::exit(i32::from(hook::EXIT_BLOCK));
    }));
}

fn fail(error: &anyhow::Error, code: u8) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "tessel: error: {error:#}");
    ExitCode::from(code)
}
