//! `tessel init`: from nothing to a worktree ready for `tessel start`, safe to rerun.
//!
//! It resolves the identity (flag, then `.tessel/config.toml`, then the `TESSEL_*` environment),
//! gets a token (kept in `.tessel/config.toml`, or from `TESSEL_TOKEN`), writes the config file
//! with mode 0600, keeps `.tessel/` out of git, and installs the Claude Code and git hooks.
//! The token never reaches the output: every error text passes through `Secrets::scrub`.

use std::fmt::Write as _;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context};
use serde::Deserialize;

use crate::config::Config;
use crate::githook;
use crate::hook::{self, Installed};
use crate::render::escape;
use crate::worktree::Worktree;

const ENV_COORDINATOR: &str = "TESSEL_COORDINATOR";
const ENV_REPO: &str = "TESSEL_REPO";
const ENV_AGENT: &str = "TESSEL_AGENT";
const ENV_TOKEN: &str = "TESSEL_TOKEN";

/// A directory with no `.tessel/config.toml`, so `Config::load` applies its validation to the
/// values handed over through the environment closure alone.
const NO_CONFIG_DIR: &str = "/nonexistent-tessel-init";

const EXCLUDE_LINE: &str = ".tessel/";

pub type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The flags of `tessel init`.
#[derive(Debug, Default, Clone, clap::Args)]
pub struct Flags {
    /// `ws://` or `wss://` base URL of the coordinator; else the file, else the environment
    /// (`TESSEL_COORDINATOR`).
    #[arg(long)]
    pub coordinator: Option<String>,
    /// Repository name; else the file, else `TESSEL_REPO`.
    #[arg(long)]
    pub repo: Option<String>,
    /// This agent's name; else the file, else `TESSEL_AGENT`.
    #[arg(long)]
    pub agent: Option<String>,
}

/// Every secret this run holds, so no error text can carry one.
#[derive(Default)]
struct Secrets(Vec<String>);

impl Secrets {
    fn add(&mut self, secret: &str) {
        if !secret.is_empty() {
            self.0.push(secret.to_string());
        }
    }

    fn scrub(&self, text: &str) -> String {
        self.0.iter().fold(text.to_string(), |text, secret| {
            text.replace(secret, "[redacted]")
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileValues {
    coordinator: Option<String>,
    repo: Option<String>,
    agent: Option<String>,
    token: Option<String>,
}

/// The config file as found: its exact text and the values in it.
struct Existing {
    text: Option<String>,
    values: FileValues,
}

struct Names {
    coordinator: String,
    repo: String,
    agent: String,
}

/// Where the token comes from.
enum TokenSource {
    Kept(String),
    FromEnv(String),
}

struct Obtained {
    token: String,
    note: String,
}

/// Runs `tessel init` in the worktree containing `cwd` and returns the card to print.
pub fn run(cwd: &Path, flags: &Flags, env: Env<'_>) -> anyhow::Result<String> {
    let worktree = Worktree::discover(cwd)?;
    let path = worktree.dir().join("config.toml");
    let existing = read_existing(&path)?;
    let mut secrets = Secrets::default();
    for secret in [existing.values.token.as_deref(), env(ENV_TOKEN).as_deref()]
        .into_iter()
        .flatten()
    {
        secrets.add(secret);
    }
    let names = resolve_names(flags, &existing.values, env)?;
    validate(&names)?;
    let obtained = obtain(token_source(&existing.values, env, &names)?);
    let config_line = write_config(&path, &existing, &names, &obtained)
        .map_err(|e| anyhow!("{}", secrets.scrub(&format!("{e:#}"))))?;
    let ignore_line = ensure_ignored(&worktree.root)?;
    let hooks_line = install_hooks(&worktree)?;
    let git_hooks = githook::install(&worktree.root)?;
    Ok(card(
        &names,
        [config_line, ignore_line, hooks_line],
        &git_hooks,
    ))
}

fn resolved_env(names: &Names, token: &str, name: &str) -> Option<String> {
    match name {
        ENV_COORDINATOR => Some(names.coordinator.clone()),
        ENV_REPO => Some(names.repo.clone()),
        ENV_AGENT => Some(names.agent.clone()),
        ENV_TOKEN => Some(token.to_string()),
        _ => None,
    }
}

/// Applies `Config::load`'s rules to the resolved names; the token is a placeholder here.
fn validate(names: &Names) -> anyhow::Result<()> {
    Config::load(Path::new(NO_CONFIG_DIR), |name| {
        resolved_env(names, "placeholder", name)
    })?;
    Ok(())
}

fn read_existing(path: &Path) -> anyhow::Result<Existing> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Existing {
                text: None,
                values: FileValues::default(),
            })
        }
        Err(e) => bail!("cannot read {}: {e}", path.display()),
    };
    // The parser's own message quotes the offending line, which may hold the token.
    let values = toml::from_str(&text).map_err(|e| {
        let line = e.span().map_or(0, |span| {
            text[..span.start.min(text.len())].matches('\n').count() + 1
        });
        anyhow!(
            "invalid {}: {} (line {line}); fix or remove the file",
            path.display(),
            e.message()
        )
    })?;
    Ok(Existing {
        text: Some(text),
        values,
    })
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// Flag, then the file, then the environment.
fn resolve_names(flags: &Flags, file: &FileValues, env: Env<'_>) -> anyhow::Result<Names> {
    let pick = |flag: &Option<String>, from_file: &Option<String>, var: &str| {
        non_empty(flag.clone())
            .or_else(|| non_empty(from_file.clone()))
            .or_else(|| non_empty(env(var)))
    };
    let coordinator = pick(&flags.coordinator, &file.coordinator, ENV_COORDINATOR);
    let repo = pick(&flags.repo, &file.repo, ENV_REPO);
    let agent = pick(&flags.agent, &file.agent, ENV_AGENT);
    let (Some(coordinator), Some(repo), Some(agent)) =
        (coordinator.clone(), repo.clone(), agent.clone())
    else {
        let mut missing = Vec::new();
        for (value, flag) in [
            (&coordinator, "--coordinator <wss://…>"),
            (&repo, "--repo <name>"),
            (&agent, "--agent <name>"),
        ] {
            if value.is_none() {
                missing.push(flag);
            }
        }
        bail!("missing values; pass {}", missing.join(", "));
    };
    Ok(Names {
        coordinator,
        repo,
        agent,
    })
}

fn token_source(file: &FileValues, env: Env<'_>, names: &Names) -> anyhow::Result<TokenSource> {
    // A token is bound to one repo and one agent, so it is kept only for the identity the file
    // implies: each key from the file, else the environment, as `Config::load` reads it.
    let file_repo = non_empty(file.repo.clone()).or_else(|| non_empty(env(ENV_REPO)));
    let file_agent = non_empty(file.agent.clone()).or_else(|| non_empty(env(ENV_AGENT)));
    let same_identity = file_repo.as_deref() == Some(names.repo.as_str())
        && file_agent.as_deref() == Some(names.agent.as_str());
    let mut dropped = String::new();
    if let Some(token) = non_empty(file.token.clone()) {
        if same_identity {
            return Ok(TokenSource::Kept(token));
        }
        dropped = format!(
            "the token in .tessel/config.toml is for {}/{}, not {}/{}; ",
            escape(file_repo.as_deref().unwrap_or("?")),
            escape(file_agent.as_deref().unwrap_or("?")),
            names.repo,
            names.agent
        );
    }
    if let Some(token) = non_empty(env(ENV_TOKEN)) {
        return Ok(TokenSource::FromEnv(token));
    }
    bail!(
        "{dropped}no token: set {ENV_TOKEN} to a token minted for this repo and agent. The operator \
         mints one with: tessel-server admin token --config <file> --kid <kid> --tenant <t> \
         --repo {} --agent {}; hand the agent {ENV_TOKEN} only, never the signing keys",
        names.repo,
        names.agent
    )
}

fn obtain(source: TokenSource) -> Obtained {
    match source {
        TokenSource::Kept(token) => Obtained {
            token,
            note: "token kept".into(),
        },
        TokenSource::FromEnv(token) => Obtained {
            token,
            note: format!("token from {ENV_TOKEN}"),
        },
    }
}

fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn config_text(names: &Names, token: &str) -> String {
    format!(
        "coordinator = {}\nrepo = {}\nagent = {}\ntoken = {}\n",
        toml_string(&names.coordinator),
        toml_string(&names.repo),
        toml_string(&names.agent),
        toml_string(token)
    )
}

/// Creates `.tessel/` with mode 0700, or tightens an existing one; says what it changed.
fn private_dir(dir: &Path) -> anyhow::Result<Option<&'static str>> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => bail!("cannot create {}: {e}", dir.display()),
    }
    let meta = std::fs::metadata(dir).with_context(|| format!("cannot read {}", dir.display()))?;
    if meta.permissions().mode() & 0o777 == 0o700 {
        return Ok(None);
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("cannot set the mode of {}", dir.display()))?;
    Ok(Some(".tessel/ set to 0700"))
}

/// Writes `text` to a private temp file and renames it over `path`; the temp file is removed on
/// failure.
fn replace_private(path: &Path, text: &str) -> anyhow::Result<()> {
    let temp = path.with_extension("toml.tmp");
    let write = || -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)
            .with_context(|| format!("cannot write {}", temp.display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    };
    let result = write();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Writes the file when its text or mode differs, and returns the card line.
fn write_config(
    path: &Path,
    existing: &Existing,
    names: &Names,
    obtained: &Obtained,
) -> anyhow::Result<String> {
    let text = config_text(names, &obtained.token);
    let mut note = obtained.note.clone();
    if let Some(dir) = path.parent() {
        if let Some(tightened) = private_dir(dir)? {
            note = format!("{note}; {tightened}");
        }
    }
    let mode_ok =
        std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o777 == 0o600);
    if existing.text.as_deref() == Some(text.as_str()) && mode_ok {
        return Ok(format!("kept .tessel/config.toml ({note})"));
    }
    replace_private(path, &text)?;
    Ok(format!("wrote .tessel/config.toml ({note})"))
}

fn git_output(root: &Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("cannot run git {}", args.join(" ")))
}

/// Adds `.tessel/` to `.git/info/exclude` unless git already ignores it there or elsewhere.
fn ensure_ignored(root: &Path) -> anyhow::Result<String> {
    let check = |root: &Path| -> anyhow::Result<Option<bool>> {
        let output = git_output(root, &["check-ignore", "-q", ".tessel"])?;
        match output.status.code() {
            Some(0) => Ok(Some(true)),
            Some(1) => Ok(Some(false)),
            Some(_) | None => bail!(
                "git check-ignore failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
    };
    if check(root)? == Some(true) {
        return Ok("ignored already".into());
    }
    let output = git_output(root, &["rev-parse", "--git-path", "info/exclude"])?;
    if !output.status.success() {
        bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let relative = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let exclude: PathBuf = root.join(relative);
    if let Some(dir) = exclude.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    append_exclude(&exclude).with_context(|| format!("cannot update {}", exclude.display()))?;
    if check(root)? != Some(true) {
        bail!(
            "added {EXCLUDE_LINE} to {} but git still does not ignore .tessel; add it to your \
             ignore rules by hand",
            exclude.display()
        );
    }
    Ok("added .tessel/ to .git/info/exclude".into())
}

/// Appends the rule without rewriting the file: its other bytes need not be valid UTF-8.
fn append_exclude(exclude: &Path) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(exclude)?;
    let len = file.metadata()?.len();
    let mut line = Vec::new();
    if len > 0 {
        file.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0_u8; 1];
        file.read_exact(&mut last)?;
        if last != *b"\n" {
            line.push(b'\n');
        }
    }
    line.extend_from_slice(EXCLUDE_LINE.as_bytes());
    line.push(b'\n');
    file.write_all(&line)
}

fn install_hooks(worktree: &Worktree) -> anyhow::Result<String> {
    let exe = std::env::current_exe().context("cannot locate the tessel binary")?;
    Ok(match hook::install(worktree, &exe)? {
        Installed::Added => "added to .claude/settings.local.json",
        Installed::Updated => "updated in .claude/settings.local.json",
        Installed::AlreadyPresent => "already in .claude/settings.local.json",
    }
    .to_string())
}

/// The git hook report, split into the per-hook status and everything else (warnings, notes).
fn git_hook_lines(report: &str) -> (String, Vec<String>) {
    let mut status = Vec::new();
    let mut notes = Vec::new();
    for line in report.lines() {
        if line.starts_with("pre-commit:") || line.starts_with("pre-push:") {
            status.push(line.to_string());
        } else {
            notes.push(line.to_string());
        }
    }
    (status.join("; "), notes)
}

fn card(names: &Names, lines: [String; 3], git_hooks: &str) -> String {
    let [config_line, ignore_line, hooks_line] = lines;
    let (git_status, notes) = git_hook_lines(git_hooks);
    let mut out = format!(
        "✓ tessel init: {} as {} on {}\n",
        escape(&names.repo),
        escape(&names.agent),
        escape(&names.coordinator)
    );
    let _ = writeln!(out, "  config     {config_line}");
    let _ = writeln!(out, "  ignore     {ignore_line}");
    let _ = writeln!(out, "  hooks      {hooks_line}");
    let _ = writeln!(out, "  git hooks  {git_status}");
    for note in notes {
        let _ = writeln!(out, "             {note}");
    }
    out.push_str(
        "next\n  1  tessel start \"<one line: what this work is for>\"\n  2  edit: the hook \
         claims each file as you touch it, or tessel claim <path>...\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        Names {
            coordinator: "wss://c.example.test".into(),
            repo: "demo".into(),
            agent: "a1".into(),
        }
    }

    #[test]
    fn strings_survive_the_toml_round_trip() {
        let nasty = "a\"b\\c\nd\te";
        let text = format!("token = {}\n", toml_string(nasty));
        let parsed: FileValues = toml::from_str(&text).unwrap();
        assert_eq!(parsed.token.as_deref(), Some(nasty));
    }

    fn file_with_token(token: &str) -> FileValues {
        FileValues {
            repo: Some("demo".into()),
            agent: Some("a1".into()),
            token: Some(token.into()),
            ..FileValues::default()
        }
    }

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_string())
        }
    }
    #[test]
    fn the_file_token_is_kept_over_the_environment_for_the_same_identity() {
        let env = env_of(&[(ENV_TOKEN, "from-env")]);
        let file = file_with_token("from-file");
        let kept = token_source(&file, &env, &names()).unwrap();
        assert!(matches!(kept, TokenSource::Kept(ref t) if t == "from-file"));
    }

    #[test]
    fn the_file_token_is_not_kept_for_another_agent_or_repo() {
        let env = env_of(&[]);
        let file = file_with_token("from-file");
        for (repo, agent) in [("demo", "a2"), ("other", "a1")] {
            let names = Names {
                repo: repo.into(),
                agent: agent.into(),
                ..names()
            };
            let kept = matches!(token_source(&file, &env, &names), Ok(TokenSource::Kept(_)));
            assert!(!kept, "{repo}/{agent}");
        }
    }

    #[test]
    fn a_failed_write_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::create_dir_all(path.join("inside")).unwrap();
        assert!(replace_private(&path, "x = 1\n").is_err());
        assert!(!dir.path().join("config.toml.tmp").exists());
    }

    #[test]
    fn no_token_anywhere_tells_the_operator_how_to_mint_one() {
        let text = match token_source(&FileValues::default(), &env_of(&[]), &names()) {
            Ok(_) => "no error".to_string(),
            Err(error) => error.to_string(),
        };
        assert!(text.contains(ENV_TOKEN), "{text}");
        assert!(
            text.contains("tessel-server admin token --config <file> --kid <kid> --tenant <t>"),
            "{text}"
        );
        assert!(text.contains("--repo demo --agent a1"), "{text}");
    }

    #[test]
    fn a_missing_value_names_its_flag() {
        let env = env_of(&[(ENV_REPO, "demo")]);
        let text = match resolve_names(&Flags::default(), &FileValues::default(), &env) {
            Ok(_) => "no error".to_string(),
            Err(error) => error.to_string(),
        };
        assert!(
            text.contains("--coordinator") && text.contains("--agent"),
            "{text}"
        );
        assert!(!text.contains("--repo"), "{text}");
    }

    #[test]
    fn a_flag_beats_the_file_and_the_file_beats_the_environment() {
        let env = env_of(&[
            (ENV_COORDINATOR, "wss://env"),
            (ENV_REPO, "env-repo"),
            (ENV_AGENT, "env-agent"),
        ]);
        let file = FileValues {
            repo: Some("file-repo".into()),
            agent: Some("file-agent".into()),
            ..FileValues::default()
        };
        let flags = Flags {
            agent: Some("flag-agent".into()),
            ..Flags::default()
        };
        let names = resolve_names(&flags, &file, &env).unwrap();
        assert_eq!(
            (
                names.coordinator.as_str(),
                names.repo.as_str(),
                names.agent.as_str()
            ),
            ("wss://env", "file-repo", "flag-agent")
        );
    }
}
