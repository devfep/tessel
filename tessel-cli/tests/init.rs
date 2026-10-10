//! `tessel init` end to end: the real binary in a temp git repository, with no daemon and no
//! network. The steward call is tested in `src/init.rs` with a fake transport.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]
#![expect(
    dead_code,
    reason = "each test crate compiles all of support, which the other test crates also use"
)]

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use support::{Agent, Done, Fake};

const TOKEN: &str = "tok-init-S3CRETvalue";
const OTHER_TOKEN: &str = "tok-other-S3CRETvalue";
const ADMIN: &str = "admin-S3CRETvalue";
const FLAGS: [&str; 6] = [
    "--coordinator",
    "wss://c.example.test",
    "--repo",
    "demo",
    "--agent",
    "a1",
];

async fn world() -> Result<(Fake, Agent)> {
    let fake = Fake::start(30_000, &[("a1", "unused")]).await?;
    let agent = Agent::new(&fake, "a1", "unused")?;
    Ok((fake, agent))
}

/// `tessel init <args>` in the agent's repository with exactly the `TESSEL_*` and steward
/// variables in `env`.
fn init(agent: &Agent, args: &[&str], env: &[(&str, &str)]) -> Result<Done> {
    let output = Command::new(env!("CARGO_BIN_EXE_tessel"))
        .arg("init")
        .args(args)
        .current_dir(agent.root())
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("TESSEL_COORDINATOR")
        .env_remove("TESSEL_REPO")
        .env_remove("TESSEL_AGENT")
        .env_remove("TESSEL_TOKEN")
        .env_remove("STEWARD_ADMIN_TOKEN")
        .envs(env.iter().copied())
        .output()
        .context("cannot run tessel")?;
    Ok(Done::from(output))
}

fn config_path(agent: &Agent) -> PathBuf {
    agent.root().join(".tessel/config.toml")
}

fn mode(path: &Path) -> Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

fn exclude_text(agent: &Agent) -> Result<String> {
    Ok(std::fs::read_to_string(
        agent.root().join(".git/info/exclude"),
    )?)
}

#[tokio::test]
async fn init_sets_the_worktree_up_and_never_prints_the_token() -> Result<()> {
    let (_fake, agent) = world().await?;
    let done = init(&agent, &FLAGS, &[("TESSEL_TOKEN", TOKEN)])?;
    assert_eq!(done.code, 0, "{}", done.all());
    let shown = done.all();
    assert!(!shown.contains(TOKEN), "{shown}");
    assert!(
        shown.starts_with("✓ tessel init: demo as a1 on wss://c.example.test\n"),
        "{shown}"
    );
    assert!(
        shown.contains("wrote .tessel/config.toml (token from TESSEL_TOKEN)"),
        "{shown}"
    );
    assert!(
        shown.contains("added .tessel/ to .git/info/exclude"),
        "{shown}"
    );
    assert!(
        shown.contains("added to .claude/settings.local.json"),
        "{shown}"
    );
    assert!(shown.contains("pre-commit: installed"), "{shown}");
    assert!(shown.contains("1  tessel start \""), "{shown}");
    assert!(shown.contains("2  edit: the hook claims"), "{shown}");

    let file = std::fs::read_to_string(config_path(&agent))?;
    assert_eq!(
        file,
        format!(
            "coordinator = \"wss://c.example.test\"\nrepo = \"demo\"\nagent = \"a1\"\n\
             token = \"{TOKEN}\"\n"
        )
    );
    assert_eq!(mode(&config_path(&agent))?, 0o600);
    assert_eq!(mode(&agent.root().join(".tessel"))?, 0o700);
    let settings = std::fs::read_to_string(agent.root().join(".claude/settings.local.json"))?;
    assert!(settings.contains("PreToolUse") && settings.contains("hook pre-edit"));
    for hook in ["pre-commit", "pre-push"] {
        assert!(
            agent.root().join(".git/hooks").join(hook).is_file(),
            "{hook}"
        );
    }
    assert!(exclude_text(&agent)?.lines().any(|line| line == ".tessel/"));
    Ok(())
}

#[tokio::test]
async fn a_rerun_keeps_everything_and_changes_no_file() -> Result<()> {
    let (_fake, agent) = world().await?;
    let env = [("TESSEL_TOKEN", TOKEN)];
    assert_eq!(init(&agent, &FLAGS, &env)?.code, 0);
    let watched = [
        config_path(&agent),
        agent.root().join(".claude/settings.local.json"),
        agent.root().join(".git/hooks/pre-commit"),
        agent.root().join(".git/hooks/pre-push"),
        agent.root().join(".git/info/exclude"),
    ];
    let before = watched
        .iter()
        .map(std::fs::read)
        .collect::<std::io::Result<Vec<_>>>()?;

    let done = init(&agent, &FLAGS, &env)?;
    assert_eq!(done.code, 0, "{}", done.all());
    let shown = done.all();
    for line in [
        "kept .tessel/config.toml (token kept)",
        "ignored already",
        "already in .claude/settings.local.json",
        "pre-commit: already installed; pre-push: already installed",
    ] {
        assert!(shown.contains(line), "missing {line:?} in:\n{shown}");
    }
    assert!(
        !shown.contains("wrote") && !shown.contains("added"),
        "{shown}"
    );
    let after = watched
        .iter()
        .map(std::fs::read)
        .collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(before, after);
    Ok(())
}

#[tokio::test]
async fn no_token_anywhere_fails_with_both_ways_out_and_writes_nothing() -> Result<()> {
    let (_fake, agent) = world().await?;
    let done = init(&agent, &FLAGS, &[("STEWARD_ADMIN_TOKEN", ADMIN)])?;
    assert_ne!(done.code, 0, "{}", done.all());
    let shown = done.all();
    assert!(
        shown.contains("TESSEL_TOKEN") && shown.contains("--steward"),
        "{shown}"
    );
    assert!(
        !shown.contains("art_v") && !shown.contains(ADMIN),
        "{shown}"
    );
    assert!(!agent.root().join(".tessel").exists());
    Ok(())
}

#[tokio::test]
async fn a_steward_without_the_admin_token_names_it() -> Result<()> {
    let (_fake, agent) = world().await?;
    let mut args = FLAGS.to_vec();
    args.extend(["--steward", "https://steward.example.test"]);
    let done = init(&agent, &args, &[])?;
    assert_ne!(done.code, 0, "{}", done.all());
    assert!(done.all().contains("STEWARD_ADMIN_TOKEN"), "{}", done.all());
    Ok(())
}

#[tokio::test]
async fn a_missing_value_names_its_flag() -> Result<()> {
    let (_fake, agent) = world().await?;
    let env = [("TESSEL_TOKEN", TOKEN)];
    let done = init(
        &agent,
        &["--coordinator", "wss://c.example.test", "--repo", "demo"],
        &env,
    )?;
    assert_ne!(done.code, 0, "{}", done.all());
    let shown = done.all();
    assert!(shown.contains("--agent <name>"), "{shown}");
    assert!(
        !shown.contains("--repo") && !shown.contains("--coordinator"),
        "{shown}"
    );
    let done = init(&agent, &[], &env)?;
    let shown = done.all();
    for flag in ["--coordinator", "--repo", "--agent"] {
        assert!(shown.contains(flag), "{shown}");
    }
    assert!(!agent.root().join(".tessel").exists());
    Ok(())
}

#[tokio::test]
async fn the_environment_fills_what_no_flag_gives() -> Result<()> {
    let (_fake, agent) = world().await?;
    let env = [
        ("TESSEL_COORDINATOR", "wss://env.example.test/"),
        ("TESSEL_REPO", "envrepo"),
        ("TESSEL_TOKEN", TOKEN),
    ];
    let done = init(&agent, &["--agent", "flagged"], &env)?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(
        done.stdout
            .starts_with("✓ tessel init: envrepo as flagged on wss://env.example.test/\n"),
        "{}",
        done.stdout
    );
    Ok(())
}

#[tokio::test]
async fn an_invalid_value_is_refused_before_anything_is_written() -> Result<()> {
    let (_fake, agent) = world().await?;
    let env = [("TESSEL_TOKEN", TOKEN)];
    for (flag, value) in [
        ("--repo", "a/b"),
        ("--coordinator", "https://c.example.test"),
    ] {
        let mut args = FLAGS.to_vec();
        args.extend([flag, value]);
        let done = init(&agent, &args, &env)?;
        assert_ne!(done.code, 0, "{flag} {value}: {}", done.all());
    }
    assert!(!agent.root().join(".tessel").exists());
    Ok(())
}

#[tokio::test]
async fn the_file_token_is_kept_when_the_environment_differs() -> Result<()> {
    let (_fake, agent) = world().await?;
    assert_eq!(init(&agent, &FLAGS, &[("TESSEL_TOKEN", TOKEN)])?.code, 0);
    let before = std::fs::read_to_string(config_path(&agent))?;
    let done = init(&agent, &FLAGS, &[("TESSEL_TOKEN", OTHER_TOKEN)])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(
        done.stdout
            .contains("kept .tessel/config.toml (token kept)"),
        "{}",
        done.stdout
    );
    assert_eq!(std::fs::read_to_string(config_path(&agent))?, before);
    assert!(!done.all().contains(TOKEN) && !done.all().contains(OTHER_TOKEN));
    Ok(())
}

#[tokio::test]
async fn a_flag_changes_the_file_and_the_token_stays() -> Result<()> {
    let (_fake, agent) = world().await?;
    assert_eq!(init(&agent, &FLAGS, &[("TESSEL_TOKEN", TOKEN)])?.code, 0);
    let done = init(&agent, &["--agent", "a2"], &[])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(
        done.stdout
            .contains("wrote .tessel/config.toml (token kept)"),
        "{}",
        done.stdout
    );
    let file = std::fs::read_to_string(config_path(&agent))?;
    assert!(
        file.contains("agent = \"a2\"") && file.contains(TOKEN),
        "{file}"
    );
    Ok(())
}

#[tokio::test]
async fn a_config_file_with_loose_permissions_is_tightened() -> Result<()> {
    let (_fake, agent) = world().await?;
    assert_eq!(init(&agent, &FLAGS, &[("TESSEL_TOKEN", TOKEN)])?.code, 0);
    std::fs::set_permissions(config_path(&agent), std::fs::Permissions::from_mode(0o644))?;
    let done = init(&agent, &FLAGS, &[])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(
        done.stdout.contains("wrote .tessel/config.toml"),
        "{}",
        done.stdout
    );
    assert_eq!(mode(&config_path(&agent))?, 0o600);
    Ok(())
}

#[tokio::test]
async fn a_tracked_ignore_rule_is_respected_and_the_exclude_file_untouched() -> Result<()> {
    let (_fake, agent) = world().await?;
    std::fs::write(agent.root().join(".gitignore"), ".tessel/\n")?;
    let before = exclude_text(&agent)?;
    let done = init(&agent, &FLAGS, &[("TESSEL_TOKEN", TOKEN)])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.stdout.contains("ignored already"), "{}", done.stdout);
    assert_eq!(exclude_text(&agent)?, before);
    Ok(())
}

#[tokio::test]
async fn init_outside_a_git_worktree_fails_with_advice() -> Result<()> {
    let fake = Fake::start(30_000, &[("a1", "unused")]).await?;
    let agent = Agent::without_repo(&fake, "a1", "unused")?;
    let done = init(&agent, &FLAGS, &[("TESSEL_TOKEN", TOKEN)])?;
    assert_ne!(done.code, 0, "{}", done.all());
    assert!(
        done.stderr.contains("not inside a git worktree"),
        "{}",
        done.stderr
    );
    assert!(!done.all().contains(TOKEN));
    Ok(())
}
