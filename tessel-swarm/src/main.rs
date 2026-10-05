//! `tessel-swarm`: run a seeded workload of scripted agents with and without coordination and
//! write the evidence. See the README for what each number means.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tessel_swarm::endpoint::Token;
use tessel_swarm::guard::ScratchRepo;
use tessel_swarm::live::Steward;
use tessel_swarm::on::Policy;
use tessel_swarm::report;
use tessel_swarm::run::{self, Spec};
use tessel_swarm::tasks;

#[derive(Parser)]
#[command(
    name = "tessel-swarm",
    version,
    about = "Scripted-agent workloads for Tessel's A/B evidence"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the tasks a seed generates, as JSON.
    Tasks {
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long, default_value_t = 10)]
        tasks: usize,
        #[arg(long, default_value_t = 0.5)]
        overlap: f64,
    },
    /// Run the workload and write the results.
    Run(Box<RunArgs>),
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Mode {
    On,
    Off,
    Both,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Target {
    /// The real coordinator core and a git steward, in this process.
    Local,
    /// A deployed coordinator and steward. Needs --coordinator (the swarm deployment, never production), --steward and `STEWARD_ADMIN_TOKEN`.
    Live,
}

#[derive(clap::Args)]
struct RunArgs {
    #[arg(long, value_enum, default_value = "both")]
    mode: Mode,
    #[arg(long, value_enum, default_value = "local")]
    target: Target,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    #[arg(long, default_value_t = 10)]
    tasks: usize,
    /// Chance that a task targets a function an earlier task also targeted, 0 to 1.
    #[arg(long, default_value_t = 0.5)]
    overlap: f64,
    #[arg(long, default_value_t = 4)]
    agents: usize,
    /// What a denied agent does: queue behind the holder, or pick other work.
    #[arg(long, value_enum, default_value = "wait")]
    policy: Policy,
    /// Time an agent spends on one task between claiming and committing.
    #[arg(long, default_value_t = 300)]
    work_ms: u64,
    #[arg(long, default_value_t = 120)]
    task_timeout_s: u64,
    /// How often a skipped task may be denied before its agent gives up on it.
    #[arg(long, default_value_t = 400)]
    max_denials: u32,
    /// Do not run the scripted reviewer. A submission held for review then stays held and its
    /// agent times out.
    #[arg(long)]
    no_reviewer: bool,
    /// Scratch repository name; must be `swarm-<suffix>`. Default: swarm-s<seed>-<time>.
    #[arg(long)]
    repo: Option<String>,
    /// Live only: ws:// or wss:// origin of the coordinator Worker.
    #[arg(long)]
    coordinator: Option<String>,
    /// Live only: https:// origin of the steward Worker.
    #[arg(long)]
    steward: Option<String>,
    #[arg(long, default_value = "swarm-results")]
    out: PathBuf,
}

fn write_out(text: &str) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{text}")?;
    Ok(())
}

fn save(path: &Path, value: &serde_json::Value) -> Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(value)? + "\n")
        .with_context(|| format!("cannot write {}", path.display()))
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Tasks {
            seed,
            tasks,
            overlap,
        } => {
            let list = tasks::generate(seed, tasks, overlap)?;
            write_out(&serde_json::to_string_pretty(&list)?)
        }
        Command::Run(args) => run_command(&args).await,
    }
}

async fn run_command(args: &RunArgs) -> Result<()> {
    if args.agents == 0 {
        bail!("--agents must be at least 1");
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let repo = run::resolve_repo(args.repo.as_deref(), args.seed, now)?;
    if args.target == Target::Live && args.mode != Mode::Off {
        check_live_args(args)?;
    }
    let spec = Spec {
        seed: args.seed,
        tasks: args.tasks,
        overlap: args.overlap,
        agents: args.agents,
        policy: args.policy,
        work_ms: args.work_ms,
        task_timeout: Duration::from_secs(args.task_timeout_s),
        max_denials: args.max_denials,
        scripted_reviewer: !args.no_reviewer,
    };
    let list = tasks::generate(spec.seed, spec.tasks, spec.overlap)?;
    std::fs::create_dir_all(&args.out)?;
    let header = report::header_of(&spec.on_config(), spec.seed, spec.tasks, spec.overlap);
    let stem = format!("seed{}", spec.seed);
    let off = if args.mode == Mode::On {
        None
    } else {
        Some(run::run_off(&spec, &list).await?)
    };
    if let Some(off) = &off {
        save(
            &args.out.join(format!("{stem}-off.json")),
            &report::off_json(&header, off),
        )?;
    }
    if args.mode == Mode::Off {
        return write_out(&format!(
            "wrote {}",
            args.out.join(format!("{stem}-off.json")).display()
        ));
    }
    let (target, on) = match args.target {
        Target::Local => ("local", run::run_on_local(&spec, &list, &repo).await?),
        Target::Live => ("live", run_live(args, &spec, &list, &repo).await?),
    };
    save(
        &args.out.join(format!("{stem}-on.json")),
        &report::on_json(&header, target, spec.policy, &on),
    )?;
    std::fs::write(
        args.out.join(format!("{stem}-on-events.json")),
        serde_json::to_string(&on.events)? + "\n",
    )?;
    let Some(off) = off else {
        return write_out(&format!(
            "wrote {}",
            args.out.join(format!("{stem}-on.json")).display()
        ));
    };
    let table = report::ab_markdown(&header, target, spec.policy, &off, &on);
    std::fs::write(args.out.join(format!("{stem}-ab.md")), &table)?;
    write_out(&table)
}

/// Everything about a live run that can be refused without doing any work, so a refused run
/// writes nothing.
fn check_live_args(args: &RunArgs) -> Result<()> {
    let (Some(coordinator), Some(_)) = (&args.coordinator, &args.steward) else {
        bail!("--target live needs --coordinator and --steward");
    };
    tessel_swarm::guard::check_coordinator(coordinator)
}

async fn run_live(
    args: &RunArgs,
    spec: &Spec,
    list: &[tasks::Task],
    repo: &ScratchRepo,
) -> Result<tessel_swarm::on::OnResult> {
    let (Some(coordinator), Some(steward)) = (&args.coordinator, &args.steward) else {
        bail!("--target live needs --coordinator and --steward");
    };
    let admin = std::env::var("STEWARD_ADMIN_TOKEN")
        .context("set STEWARD_ADMIN_TOKEN for --target live")?;
    let steward = Steward::new(steward, Token::new(admin))?;
    write_out(&format!("live target: creating scratch repo {repo}"))?;
    run::run_on_live(spec, list, repo, &steward, coordinator).await
}
