//! The gate a repo declares in `tessel.toml` at the root of its trunk commit, ported from the
//! steward's `tessel-config.ts` and `gate-plan.ts`.
//!
//! Strict by design: unknown keys, an oversized file, a command that is not an argv array of plain
//! strings, or a command outside [`CARGO_TEST_FLAGS`] make the whole file invalid, because a gate
//! that is half understood is not a gate. The file is untrusted data until it passes.

use toml::{Table, Value};

use crate::merge::BoundaryError;
use crate::types::Sha;

/// Where a repo declares its gate, at the root of the trunk commit.
pub const TESSEL_TOML_PATH: &str = "tessel.toml";
pub const MAX_CONFIG_BYTES: usize = 4096;

/// Nothing that can change which toolchain, config or compiler runs is on it: no `+toolchain`,
/// `--config`, `-Z`, `--manifest-path`, `--target`, no `--` passthrough, and no environment.
pub const CARGO_TEST_FLAGS: [&str; 10] = [
    "--workspace",
    "--locked",
    "--offline",
    "--no-fail-fast",
    "--all-targets",
    "--all-features",
    "--release",
    "--lib",
    "--bins",
    "--tests",
];

const MAX_TESTS: usize = 6;
const MAX_PNPM_DIRS: usize = 4;
const MAX_ARGV: usize = 24;
const MAX_ARG_LENGTH: usize = 200;

/// Instances a repo may ask for. `lite` is not listed: it is what a repo without a gate gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateInstance {
    Standard1,
    Standard2,
    Standard3,
    Standard4,
}

impl GateInstance {
    const NAMES: [(&'static str, GateInstance); 4] = [
        ("standard-1", GateInstance::Standard1),
        ("standard-2", GateInstance::Standard2),
        ("standard-3", GateInstance::Standard3),
        ("standard-4", GateInstance::Standard4),
    ];
}

/// One gate command: `argv` run with `dir` (relative to the repo root) as working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCommand {
    pub dir: String,
    pub argv: Vec<String>,
}

/// A validated `tessel.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateConfig {
    /// Checked when present and never used; a gate may leave it out.
    pub instance: Option<GateInstance>,
    /// Run `cargo fetch --locked --offline` before the tests.
    pub install_cargo: bool,
    /// Directories in which `pnpm install` runs (offline, frozen lockfile, no scripts).
    pub install_pnpm: Vec<String>,
    pub tests: Vec<TestCommand>,
}

/// Why a `tessel.toml` is not acceptable. The text is fixed by this module, never repo text.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{reason}")]
pub struct InvalidGate {
    pub reason: String,
}

fn invalid<T>(reason: impl Into<String>) -> Result<T, InvalidGate> {
    Err(InvalidGate {
        reason: reason.into(),
    })
}

fn has_unknown_key(table: &Table, allowed: &[&str]) -> bool {
    table.keys().any(|key| !allowed.contains(&key.as_str()))
}

fn is_allowed_command(argv: &[&str]) -> bool {
    match argv {
        // `cargo fmt --all --check` asks rustfmt to check and not to write. A repo's
        // `.cargo/config.toml` can alias `fmt` to a command that runs repo code, as `cargo test`
        // runs repo code, so this holds for cooperating agents. Every other fmt argv is refused:
        // a bare `cargo fmt` or `--all` alone rewrites files.
        ["pnpm" | "npm", "test"] | ["cargo", "fmt", "--all", "--check"] => true,
        ["cargo", "test", flags @ ..] => flags.iter().all(|flag| CARGO_TEST_FLAGS.contains(flag)),
        _ => false,
    }
}

/// `.` or plain path segments: each starts with a letter, digit or `_`, then letters, digits,
/// `.`, `_` or `-`. No `..`, no leading `.`, no empty segment, nothing absolute.
fn is_plain_dir(dir: &str) -> bool {
    dir == "."
        || dir.split('/').all(|segment| {
            let mut chars = segment.chars();
            chars
                .next()
                .is_some_and(|first| first.is_ascii_alphanumeric() || first == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
}

/// No quote, space, `$`, `;`, `&`, `|`, backtick, parenthesis or glob character is ever needed.
fn is_plain_arg(arg: &str) -> bool {
    !arg.is_empty()
        && arg.len() <= MAX_ARG_LENGTH
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '.' | '_' | '/' | '=' | ':' | '@' | '+' | ',' | '%' | '-')
        })
}

fn parse_dir(value: &Value, place: &str) -> Result<String, InvalidGate> {
    match value.as_str() {
        Some(dir) if is_plain_dir(dir) => Ok(dir.to_owned()),
        Some(_) | None => invalid(format!(
            "{place} must be a relative directory made of plain path segments"
        )),
    }
}

fn parse_argv(value: Option<&Value>, place: &str) -> Result<Vec<String>, InvalidGate> {
    let items = match value.and_then(Value::as_array) {
        Some(items) if !items.is_empty() && items.len() <= MAX_ARGV => items,
        Some(_) | None => {
            return invalid(format!(
                "{place} must be an array of 1 to {MAX_ARGV} strings"
            ))
        }
    };
    let mut argv = Vec::with_capacity(items.len());
    for item in items {
        match item.as_str() {
            Some(arg) if is_plain_arg(arg) => argv.push(arg.to_owned()),
            Some(_) | None => {
                return invalid(format!(
                    "{place} may hold only plain strings without shell characters"
                ))
            }
        }
    }
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    if !is_allowed_command(&words) {
        return invalid(format!(
            "{place} must be pnpm test, npm test, cargo fmt --all --check, \
             or cargo test with only {}",
            CARGO_TEST_FLAGS.join(" ")
        ));
    }
    Ok(argv)
}

fn parse_install(value: Option<&Value>) -> Result<(bool, Vec<String>), InvalidGate> {
    let Some(value) = value else {
        return Ok((false, Vec::new()));
    };
    let Some(table) = value
        .as_table()
        .filter(|table| !has_unknown_key(table, &["cargo", "pnpm"]))
    else {
        return invalid("install must be a table with only the keys cargo and pnpm");
    };
    let cargo = match table.get("cargo") {
        None => false,
        Some(Value::Boolean(cargo)) => *cargo,
        Some(_) => return invalid("install.cargo must be true or false"),
    };
    let dirs = match table.get("pnpm") {
        None => &Vec::new(),
        Some(Value::Array(dirs)) if dirs.len() <= MAX_PNPM_DIRS => dirs,
        Some(_) => {
            return invalid(format!(
                "install.pnpm must be an array of at most {MAX_PNPM_DIRS} directories"
            ))
        }
    };
    let mut pnpm = Vec::with_capacity(dirs.len());
    for dir in dirs {
        pnpm.push(parse_dir(dir, "install.pnpm")?);
    }
    Ok((cargo, pnpm))
}

fn parse_tests(value: Option<&Value>) -> Result<Vec<TestCommand>, InvalidGate> {
    let entries = match value.and_then(Value::as_array) {
        Some(entries) if !entries.is_empty() && entries.len() <= MAX_TESTS => entries,
        Some(_) | None => return invalid(format!("test must be 1 to {MAX_TESTS} [[test]] tables")),
    };
    let mut tests = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(table) = entry
            .as_table()
            .filter(|table| !has_unknown_key(table, &["dir", "argv"]))
        else {
            return invalid("each [[test]] may hold only the keys dir and argv");
        };
        let dir = match table.get("dir") {
            Some(dir) => parse_dir(dir, "test.dir")?,
            None => ".".to_owned(),
        };
        let argv = parse_argv(table.get("argv"), "test.argv")?;
        tests.push(TestCommand { dir, argv });
    }
    Ok(tests)
}

/// Validates the text of a `tessel.toml`.
///
/// ```toml
/// instance = "standard-4"
///
/// [install]
/// cargo = true
/// pnpm = ["tessel-steward"]
///
/// [[test]]
/// argv = ["cargo", "test", "--workspace", "--locked", "--offline"]
/// ```
pub fn parse_gate_config(text: &str) -> Result<GateConfig, InvalidGate> {
    if text.len() > MAX_CONFIG_BYTES {
        return invalid(format!(
            "{TESSEL_TOML_PATH} is larger than {MAX_CONFIG_BYTES} bytes"
        ));
    }
    let Ok(document) = text.parse::<Table>() else {
        return invalid(format!("{TESSEL_TOML_PATH} is not valid TOML"));
    };
    if has_unknown_key(&document, &["instance", "install", "test"]) {
        return invalid("only the keys instance, install and test are known");
    }
    let instance = match document.get("instance") {
        None => None,
        Some(value) => {
            let named = value.as_str().and_then(|name| {
                GateInstance::NAMES
                    .iter()
                    .find_map(|(known, instance)| (*known == name).then_some(*instance))
            });
            let Some(named) = named else {
                return invalid(
                    "instance, when given, must be one of standard-1, standard-2, standard-3, \
                     standard-4",
                );
            };
            Some(named)
        }
    };
    let (install_cargo, install_pnpm) = parse_install(document.get("install"))?;
    let tests = parse_tests(document.get("test"))?;
    Ok(GateConfig {
        instance,
        install_cargo,
        install_pnpm,
        tests,
    })
}

/// How a run is gated, decided from the trunk commit before any repo code runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatePlan {
    /// The trunk has no `tessel.toml`.
    Legacy,
    /// The trunk has a `tessel.toml` that is not acceptable. Never a fallback to the legacy gate.
    Invalid,
    Configured(GateConfig),
}

/// What reading one file of the trunk found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrunkFile {
    Missing,
    Found(Vec<u8>),
    /// The file is larger than the platform will read into memory.
    TooLarge,
}

/// Reads files of the trunk (never of a fork) at an exact commit. The platform behind it (an
/// Artifacts repo handle, a bare repository on disk) is the caller's.
pub trait TrunkFiles {
    fn read_file(&self, commit: &Sha, path: &str) -> Result<TrunkFile, BoundaryError>;
}

/// Reads `tessel.toml` at `commit` of the trunk, so a fork can neither supply nor change the gate
/// it is judged by. A missing file is [`GatePlan::Legacy`]; an oversized, undecodable or invalid
/// file is [`GatePlan::Invalid`]. Only a failure of the reader is an error.
pub fn read_gate_plan(trunk: &dyn TrunkFiles, commit: &Sha) -> Result<GatePlan, BoundaryError> {
    let bytes = match trunk.read_file(commit, TESSEL_TOML_PATH)? {
        TrunkFile::Missing => return Ok(GatePlan::Legacy),
        TrunkFile::TooLarge => return Ok(GatePlan::Invalid),
        TrunkFile::Found(bytes) => bytes,
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(GatePlan::Invalid);
    };
    Ok(parse_gate_config(&text).map_or(GatePlan::Invalid, GatePlan::Configured))
}
