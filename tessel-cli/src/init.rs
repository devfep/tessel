//! `tessel init`: from nothing to a worktree ready for `tessel start`, safe to rerun.
//!
//! It resolves the identity (flag, then `.tessel/config.toml`, then the `TESSEL_*` environment),
//! gets a token (kept, from `TESSEL_TOKEN`, or minted by the steward), writes the config file
//! with mode 0600, keeps `.tessel/` out of git, and installs the Claude Code and git hooks.
//! The token never reaches the output: every error text passes through `Secrets::scrub`.

use std::fmt::Write as _;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context};
use serde::Deserialize;
use serde_json::Value;

use crate::config::Config;
use crate::githook;
use crate::hook::{self, Installed};
use crate::render::escape;
use crate::worktree::Worktree;

const ENV_COORDINATOR: &str = "TESSEL_COORDINATOR";
const ENV_REPO: &str = "TESSEL_REPO";
const ENV_AGENT: &str = "TESSEL_AGENT";
const ENV_TOKEN: &str = "TESSEL_TOKEN";
const ENV_ADMIN: &str = "STEWARD_ADMIN_TOKEN";

/// A directory with no `.tessel/config.toml`, so `Config::load` applies its validation to the
/// values handed over through the environment closure alone.
const NO_CONFIG_DIR: &str = "/nonexistent-tessel-init";

const EXCLUDE_LINE: &str = ".tessel/";
const MAX_ERROR_CHARS: usize = 200;

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
    /// Steward URL to mint a token from, using `STEWARD_ADMIN_TOKEN`, when no token is kept or
    /// in `TESSEL_TOKEN`.
    #[arg(long)]
    pub steward: Option<String>,
}

/// What the steward answered: the HTTP status and the body text.
pub struct Reply {
    pub status: u16,
    pub body: String,
}

/// How a POST reaches the steward. The one production implementation runs `curl`; tests answer
/// from memory, because the gate runs with `curl` off the `PATH`.
pub trait Transport {
    /// POSTs `{}` to `url` with `Authorization: Bearer <bearer>`.
    fn post(&self, url: &str, bearer: &str) -> anyhow::Result<Reply>;
}

/// Runs `curl` with its configuration on stdin, so the admin token is never in an argument list.
pub struct Curl;

impl Transport for Curl {
    fn post(&self, url: &str, bearer: &str) -> anyhow::Result<Reply> {
        let config = format!(
            "url = \"{url}\"\nrequest = \"POST\"\nheader = \"Authorization: Bearer {bearer}\"\n\
             header = \"Content-Type: application/json\"\ndata = \"{{}}\"\n"
        );
        let mut child = Command::new("curl")
            .args(["-sS", "--max-time", "60", "-K", "-", "-w", "\n%{http_code}"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot run curl; install it, or pass the token in TESSEL_TOKEN")?;
        child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("curl has no stdin"))?
            .write_all(config.as_bytes())?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
        }
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let (body, status) = text.rsplit_once('\n').unwrap_or(("", &text));
        let status = status
            .trim()
            .parse()
            .with_context(|| format!("curl reported the HTTP status {status:?}"))?;
        Ok(Reply {
            status,
            body: body.to_string(),
        })
    }
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
    Mint { steward: String, admin: String },
}

struct Obtained {
    token: String,
    note: String,
}

/// Runs `tessel init` in the worktree containing `cwd` and returns the card to print.
pub fn run(
    cwd: &Path,
    flags: &Flags,
    env: Env<'_>,
    transport: &dyn Transport,
) -> anyhow::Result<String> {
    let worktree = Worktree::discover(cwd)?;
    let path = worktree.dir().join("config.toml");
    let existing = read_existing(&path)?;
    let mut secrets = Secrets::default();
    for secret in [
        existing.values.token.as_deref(),
        env(ENV_TOKEN).as_deref(),
        env(ENV_ADMIN).as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        secrets.add(secret);
    }
    let names = resolve_names(flags, &existing.values, env)?;
    validate(&names)?;
    let source = token_source(flags, &existing.values, env, &names)?;
    let obtained = obtain(source, &names, transport, &mut secrets)?;
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

fn token_source(
    flags: &Flags,
    file: &FileValues,
    env: Env<'_>,
    names: &Names,
) -> anyhow::Result<TokenSource> {
    // A token is bound to one repo and one agent, so it is kept only for the same identity.
    let same_identity = file.repo.as_deref() == Some(names.repo.as_str())
        && file.agent.as_deref() == Some(names.agent.as_str());
    if let (Some(token), None, true) =
        (non_empty(file.token.clone()), &flags.steward, same_identity)
    {
        return Ok(TokenSource::Kept(token));
    }
    if let Some(token) = non_empty(env(ENV_TOKEN)) {
        return Ok(TokenSource::FromEnv(token));
    }
    let admin = non_empty(env(ENV_ADMIN));
    match (&flags.steward, admin) {
        (Some(steward), Some(admin)) => Ok(TokenSource::Mint {
            steward: steward.clone(),
            admin,
        }),
        (Some(_), None) => bail!(
            "--steward needs {ENV_ADMIN} in the environment to mint a token; set it, or put an \
             existing token in {ENV_TOKEN}"
        ),
        (None, _) => bail!(
            "no token: set {ENV_TOKEN}, or export {ENV_ADMIN} and pass --steward <https://…> so \
             init mints one. By hand: curl -X POST -H \"Authorization: Bearer ${ENV_ADMIN}\" \
             https://<steward>/repos/{}/agents/{}/identity, then put the `token` of the reply in \
             {ENV_TOKEN}",
            names.repo,
            names.agent
        ),
    }
}

fn obtain(
    source: TokenSource,
    names: &Names,
    transport: &dyn Transport,
    secrets: &mut Secrets,
) -> anyhow::Result<Obtained> {
    match source {
        TokenSource::Kept(token) => Ok(Obtained {
            token,
            note: "token kept".into(),
        }),
        TokenSource::FromEnv(token) => Ok(Obtained {
            token,
            note: format!("token from {ENV_TOKEN}"),
        }),
        TokenSource::Mint { steward, admin } => {
            let minted = mint(&steward, &admin, names, transport, secrets)?;
            let expires = minted.expires_at_ms.map_or_else(
                || "expiry unknown".to_string(),
                |ms| format!("expires {}", iso(ms)),
            );
            Ok(Obtained {
                token: minted.token,
                note: format!("token minted, {expires}"),
            })
        }
    }
}

struct Minted {
    token: String,
    expires_at_ms: Option<u64>,
}

fn steward_base(steward: &str) -> anyhow::Result<String> {
    let base = steward.trim_end_matches('/');
    if !(base.starts_with("https://") || is_local_http(base)) {
        bail!(
            "the steward URL must start with https:// (or http://localhost or http://127.0.0.1 \
             for a dev server)"
        );
    }
    if base
        .chars()
        .any(|c| c == '"' || c == '\\' || c.is_whitespace() || c.is_control())
    {
        bail!("the steward URL has a quote, a backslash, a space or a control character");
    }
    Ok(base.to_string())
}

/// Plain HTTP to this machine only: the admin token travels in the clear.
fn is_local_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        Some(_) | None => authority,
    };
    matches!(host, "localhost" | "127.0.0.1")
}

fn mint(
    steward: &str,
    admin: &str,
    names: &Names,
    transport: &dyn Transport,
    secrets: &mut Secrets,
) -> anyhow::Result<Minted> {
    if admin
        .chars()
        .any(|c| c == '"' || c == '\\' || c.is_control())
    {
        bail!("{ENV_ADMIN} has a character that cannot go in a header");
    }
    let base = steward_base(steward)?;
    let path = format!("/repos/{}/agents/{}/identity", names.repo, names.agent);
    let reply = transport
        .post(&format!("{base}{path}"), admin)
        .map_err(|e| anyhow!("POST {path} failed: {}", secrets.scrub(&format!("{e:#}"))))?;
    let body: Option<Value> = serde_json::from_str(&reply.body).ok();
    if reply.status != 201 {
        let detail = body
            .as_ref()
            .and_then(|v| v.get("error").and_then(Value::as_str))
            .map(|text| {
                escape(&secrets.scrub(text))
                    .chars()
                    .take(MAX_ERROR_CHARS)
                    .collect::<String>()
            })
            .unwrap_or_default();
        bail!(
            "POST {path} answered HTTP {} (expected 201): {detail}",
            reply.status
        );
    }
    let Some(body) = body else {
        bail!("POST {path} did not answer with JSON");
    };
    let Some(token) = body
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    else {
        bail!("POST {path} answered without a token");
    };
    secrets.add(token);
    Ok(Minted {
        token: token.to_string(),
        expires_at_ms: body.get("expires_at_ms").and_then(Value::as_u64),
    })
}

/// `YYYY-MM-DDTHH:MMZ` (UTC) for a Unix time in milliseconds.
fn iso(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60
    )
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
    use std::cell::RefCell;

    use super::*;

    const ADMIN: &str = "admin-S3CRET";
    const MINTED: &str = "art_v1.minted-S3CRET";
    const STEWARD: &str = "https://steward.example.test";

    struct Fake {
        reply: Result<(u16, String), String>,
        seen: RefCell<Vec<(String, String)>>,
    }

    impl Fake {
        fn new(status: u16, body: &str) -> Self {
            Self {
                reply: Ok((status, body.to_string())),
                seen: RefCell::new(Vec::new()),
            }
        }

        fn failing(why: &str) -> Self {
            Self {
                reply: Err(why.to_string()),
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl Transport for Fake {
        fn post(&self, url: &str, bearer: &str) -> anyhow::Result<Reply> {
            self.seen.borrow_mut().push((url.into(), bearer.into()));
            match &self.reply {
                Ok((status, body)) => Ok(Reply {
                    status: *status,
                    body: body.clone(),
                }),
                Err(why) => bail!("{why}"),
            }
        }
    }

    fn names() -> Names {
        Names {
            coordinator: "wss://c.example.test".into(),
            repo: "demo".into(),
            agent: "a1".into(),
        }
    }

    fn mint_with(fake: &Fake) -> anyhow::Result<Minted> {
        let mut secrets = Secrets::default();
        secrets.add(ADMIN);
        mint(STEWARD, ADMIN, &names(), fake, &mut secrets)
    }

    fn mint_error(fake: &Fake) -> String {
        match mint_with(fake) {
            Ok(_) => "minted".to_string(),
            Err(e) => format!("{e:#}"),
        }
    }

    #[test]
    fn a_201_with_a_token_is_minted_with_the_admin_token_as_bearer() {
        let fake = Fake::new(
            201,
            &format!(
                "{{\"token\":\"{MINTED}\",\"agent\":\"a1\",\"repo\":\"demo\",\
                 \"expires_at_ms\":1791601312074}}"
            ),
        );
        let minted = mint_with(&fake).unwrap();
        assert_eq!(minted.token, MINTED);
        assert_eq!(minted.expires_at_ms, Some(1_791_601_312_074));
        let seen = fake.seen.borrow();
        assert_eq!(
            seen.as_slice(),
            [(
                format!("{STEWARD}/repos/demo/agents/a1/identity"),
                ADMIN.to_string()
            )]
        );
    }

    #[test]
    fn a_401_reports_the_steward_error_without_the_admin_token() {
        let fake = Fake::new(401, &format!("{{\"error\":\"bad token {ADMIN}\"}}"));
        let text = mint_error(&fake);
        assert!(text.contains("HTTP 401"), "{text}");
        assert!(text.contains("bad token [redacted]"), "{text}");
        assert!(!text.contains(ADMIN), "{text}");
    }

    #[test]
    fn a_non_json_body_is_an_error() {
        let text = mint_error(&Fake::new(201, "<html>oops</html>"));
        assert!(text.contains("did not answer with JSON"), "{text}");
    }

    #[test]
    fn a_non_json_refusal_still_reports_the_status() {
        let text = mint_error(&Fake::new(502, "bad gateway"));
        assert!(text.contains("HTTP 502"), "{text}");
    }

    #[test]
    fn a_reply_without_a_token_is_an_error() {
        let text = mint_error(&Fake::new(201, "{\"agent\":\"a1\"}"));
        assert!(text.contains("without a token"), "{text}");
        let text = mint_error(&Fake::new(201, "{\"token\":\"\"}"));
        assert!(text.contains("without a token"), "{text}");
    }

    #[test]
    fn a_transport_failure_is_scrubbed_of_the_admin_token() {
        let text = mint_error(&Fake::failing(&format!("boom {ADMIN}")));
        assert!(text.contains("boom [redacted]"), "{text}");
        assert!(!text.contains(ADMIN), "{text}");
    }

    #[test]
    fn a_plain_http_steward_is_refused_unless_it_is_this_machine() {
        let mut secrets = Secrets::default();
        for url in [
            "http://steward.example.test",
            "http://localhost.example",
            "ftp://x",
        ] {
            let fake = Fake::new(201, "{}");
            assert!(
                mint(url, ADMIN, &names(), &fake, &mut secrets).is_err(),
                "{url}"
            );
            assert!(fake.seen.borrow().is_empty(), "{url} was called");
        }
        let fake = Fake::new(201, "{\"token\":\"t\"}");
        assert!(mint(
            "http://127.0.0.1:8787/",
            ADMIN,
            &names(),
            &fake,
            &mut secrets
        )
        .is_ok());
        assert_eq!(
            fake.seen.borrow()[0].0,
            "http://127.0.0.1:8787/repos/demo/agents/a1/identity"
        );
    }

    #[test]
    fn an_admin_token_with_a_quote_never_reaches_the_transport() {
        let fake = Fake::new(201, "{}");
        let mut secrets = Secrets::default();
        assert!(mint(STEWARD, "a\"b", &names(), &fake, &mut secrets).is_err());
        assert!(fake.seen.borrow().is_empty());
    }

    #[test]
    fn the_expiry_prints_as_a_utc_time() {
        assert_eq!(iso(0), "1970-01-01T00:00Z");
        assert_eq!(iso(1_791_601_312_074), "2026-10-10T03:01Z");
        assert_eq!(iso(951_782_400_000 + 60_000 * 61), "2000-02-29T01:01Z");
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

    fn steward_flags() -> Flags {
        Flags {
            steward: Some(STEWARD.into()),
            ..Flags::default()
        }
    }

    #[test]
    fn the_file_token_is_kept_unless_a_steward_is_given() {
        let env = env_of(&[(ENV_TOKEN, "from-env"), (ENV_ADMIN, "admin")]);
        let file = file_with_token("from-file");
        let kept = token_source(&Flags::default(), &file, &env, &names()).unwrap();
        assert!(matches!(kept, TokenSource::Kept(ref t) if t == "from-file"));
        let steward = token_source(&steward_flags(), &file, &env, &names()).unwrap();
        assert!(matches!(steward, TokenSource::FromEnv(ref t) if t == "from-env"));
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
            let kept = matches!(
                token_source(&Flags::default(), &file, &env, &names),
                Ok(TokenSource::Kept(_))
            );
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
    fn a_steward_mints_only_when_nothing_else_has_a_token() {
        let env = env_of(&[(ENV_ADMIN, "admin")]);
        let source = token_source(&steward_flags(), &FileValues::default(), &env, &names());
        assert!(matches!(source, Ok(TokenSource::Mint { .. })));
    }

    #[test]
    fn no_token_anywhere_names_both_ways_out() {
        let env = env_of(&[(ENV_ADMIN, "admin-S3CRET")]);
        let text = match token_source(&Flags::default(), &FileValues::default(), &env, &names()) {
            Ok(_) => "no error".to_string(),
            Err(error) => error.to_string(),
        };
        assert!(
            text.contains(ENV_TOKEN) && text.contains("--steward"),
            "{text}"
        );
        assert!(text.contains("/repos/demo/agents/a1/identity"), "{text}");
        assert!(!text.contains("admin-S3CRET"), "{text}");
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

    fn scratch_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        dir
    }

    #[test]
    fn a_minted_token_reaches_the_file_but_not_the_card() {
        let dir = scratch_repo();
        let fake = Fake::new(
            201,
            &format!("{{\"token\":\"{MINTED}\",\"expires_at_ms\":1791601312074}}"),
        );
        let flags = Flags {
            coordinator: Some("wss://c.example.test".into()),
            repo: Some("demo".into()),
            agent: Some("a1".into()),
            steward: Some(STEWARD.into()),
        };
        let env = env_of(&[(ENV_ADMIN, ADMIN)]);
        let card = run(dir.path(), &flags, &env, &fake).unwrap();
        assert!(
            card.contains("token minted, expires 2026-10-10T03:01Z"),
            "{card}"
        );
        assert!(!card.contains(MINTED) && !card.contains(ADMIN), "{card}");
        let file = std::fs::read_to_string(dir.path().join(".tessel/config.toml")).unwrap();
        assert!(file.contains(MINTED) && !file.contains(ADMIN), "{file}");
        let again = run(dir.path(), &Flags::default(), &env, &fake).unwrap();
        assert!(
            again.contains("kept .tessel/config.toml (token kept)"),
            "{again}"
        );
        assert_eq!(fake.seen.borrow().len(), 1, "a rerun must not mint again");
    }
}
