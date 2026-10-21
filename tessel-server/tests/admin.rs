//! The real binary's `admin` commands, and `serve` with an `[identity]` table: keys on disk, a
//! token on standard output and nowhere else, and a server that admits that token.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use data_encoding::BASE64URL_NOPAD;
use tessel_server::id::RepoId;
use tessel_server::identity::{self, Ed25519Verifier};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BINARY: &str = env!("CARGO_BIN_EXE_tessel-server");
const AUD: &str = "tessel-test";

fn write_config(dir: &Path, identity: bool) -> Result<PathBuf, std::io::Error> {
    let path = dir.join("tessel-server.toml");
    let table = if identity {
        format!(
            "[identity]\nkeys = {:?}\naudience = \"{AUD}\"\n\n",
            dir.join("keys").display().to_string()
        )
    } else {
        String::new()
    };
    std::fs::write(
        &path,
        format!(
            "listen = \"127.0.0.1:0\"\nstore_dir = {:?}\n\n{table}[[repository]]\n\
             tenant = \"acme\"\nname = \"demo\"\nrun = \"test\"\n",
            dir.join("stores").display().to_string()
        ),
    )?;
    Ok(path)
}

fn admin(config: &Path, args: &[&str]) -> Result<Output, std::io::Error> {
    Command::new(BINARY)
        .arg("admin")
        .args(args)
        .arg("--config")
        .arg(config)
        .stdin(Stdio::null())
        .output()
}

fn token_args<'a>(kid: &'a str, agent: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["token", "--kid", kid, "--tenant", "acme", "--repo", "demo"];
    args.extend(["--agent", agent]);
    args.extend(extra);
    args
}

fn keygen(config: &Path, kid: &str) -> Result<Output, std::io::Error> {
    admin(config, &["keygen", "--kid", kid])
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn admin_keygen_writes_a_seed_with_mode_0600_and_refuses_to_overwrite() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = write_config(dir.path(), true)?;
    let made = keygen(&config, "k1")?;
    assert!(made.status.success(), "{}", text(&made.stderr));
    assert!(made.stdout.is_empty(), "keygen prints nothing on stdout");
    let seed_file = dir.path().join("keys/k1.key");
    let mode = std::fs::metadata(&seed_file)?.permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    assert!(dir.path().join("keys/k1.pub").is_file());
    let before = std::fs::read(&seed_file)?;
    let again = keygen(&config, "k1")?;
    assert!(!again.status.success());
    assert!(
        text(&again.stderr).contains("already exists"),
        "{}",
        text(&again.stderr)
    );
    assert_eq!(std::fs::read(&seed_file)?, before, "the seed is untouched");
    let seed = text(&before);
    assert!(
        !text(&again.stderr).contains(seed.trim()),
        "the seed is never printed"
    );
    Ok(())
}

#[test]
fn admin_token_prints_only_the_token() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = write_config(dir.path(), true)?;
    assert!(keygen(&config, "k1")?.status.success());
    let minted = admin(&config, &token_args("k1", "a1", &[]))?;
    assert!(minted.status.success(), "{}", text(&minted.stderr));
    let stdout = text(&minted.stdout);
    let token = stdout.strip_suffix('\n').ok_or("no final newline")?;
    assert!(
        !token.is_empty() && !token.contains(char::is_whitespace),
        "one token and nothing else: {stdout:?}"
    );
    assert_eq!(text(&minted.stderr), "", "a successful mint logs nothing");
    let seed = std::fs::read_to_string(dir.path().join("keys/k1.key"))?;
    assert!(!stdout.contains(seed.trim()));
    let verifier = Ed25519Verifier::load(&dir.path().join("keys"), AUD)?;
    let repo = RepoId::new("acme", "demo")?;
    let agent = verifier.verify_at(token, &repo, identity::now_ms());
    assert_eq!(agent.map(|agent| agent.0), Ok("a1".to_string()));
    Ok(())
}

#[test]
fn admin_token_refuses_a_reserved_name_without_service_and_a_service_without_one() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = write_config(dir.path(), true)?;
    assert!(keygen(&config, "k1")?.status.success());
    for (agent, extra) in [
        ("lander", vec![]),
        ("a1", vec!["--service"]),
        ("0 0", vec![]),
    ] {
        let refused = admin(&config, &token_args("k1", agent, &extra))?;
        assert!(!refused.status.success(), "{agent} {extra:?}");
        assert!(refused.stdout.is_empty(), "no token on stdout for {agent}");
    }
    let service = admin(&config, &token_args("k1", "lander", &["--service"]))?;
    assert!(service.status.success(), "{}", text(&service.stderr));
    let missing_key = admin(&config, &token_args("k2", "a1", &[]))?;
    assert!(!missing_key.status.success());
    assert!(
        text(&missing_key.stderr).contains("k2.key"),
        "{}",
        text(&missing_key.stderr)
    );
    Ok(())
}

fn exp_ms_of(token: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let (payload, _) = token.split_once('.').ok_or("no dot")?;
    let json: serde_json::Value =
        serde_json::from_slice(&BASE64URL_NOPAD.decode(payload.as_bytes())?)?;
    json["exp_ms"].as_u64().ok_or_else(|| "no exp_ms".into())
}

#[test]
fn admin_token_lifetime_takes_minutes_hours_and_days_and_defaults_to_a_day() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = write_config(dir.path(), true)?;
    assert!(keygen(&config, "k1")?.status.success());
    for (ttl, ms) in [
        (Some("30m"), 30 * 60_000_u64),
        (Some("12h"), 12 * 3_600_000),
        (Some("7d"), 7 * 86_400_000),
        (None, 86_400_000),
    ] {
        let extra: Vec<&str> = ttl.map_or_else(Vec::new, |ttl| vec!["--ttl", ttl]);
        let before = identity::now_ms();
        let minted = admin(&config, &token_args("k1", "a1", &extra))?;
        let after = identity::now_ms();
        assert!(minted.status.success(), "{ttl:?}: {}", text(&minted.stderr));
        let exp = exp_ms_of(text(&minted.stdout).trim())?;
        assert!((before + ms..=after + ms).contains(&exp), "{ttl:?}: {exp}");
    }
    for bad in ["30s", "0m", "-1h", "x", ""] {
        let refused = admin(&config, &token_args("k1", "a1", &["--ttl", bad]))?;
        assert!(!refused.status.success(), "{bad:?}");
        assert_eq!(text(&refused.stdout), "", "{bad:?} prints no token");
    }
    Ok(())
}

/// A server process that is killed when the test ends, however it ends.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_serve(config: &Path) -> Result<(Server, mpsc::Receiver<String>), std::io::Error> {
    let mut child = Command::new(BINARY)
        .args(["serve", "--config"])
        .arg(config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Some(stderr) = stderr else { return };
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    Ok((Server(child), rx))
}

fn until(rx: &mpsc::Receiver<String>, needle: &str) -> Result<Vec<String>, String> {
    let mut seen = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(line) => {
                let found = line.contains(needle);
                seen.push(line);
                if found {
                    return Ok(seen);
                }
            }
            Err(_) => return Err(format!("never saw {needle:?}; saw {seen:?}")),
        }
    }
}

fn get(addr: &str, path: &str, token: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let auth = token.map_or_else(String::new, |t| format!("Authorization: Bearer {t}\r\n"));
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: x\r\n{auth}Connection: close\r\n\r\n"
    )?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer)?;
    Ok(answer)
}

#[test]
fn admin_a_configuration_without_identity_or_with_a_missing_key_directory_names_the_file_and_key(
) -> TestResult {
    let dir = tempfile::tempdir()?;
    let bare = write_config(dir.path(), false)?;
    let refused = keygen(&bare, "k1")?;
    assert!(!refused.status.success());
    let message = text(&refused.stderr);
    assert!(
        message.contains("tessel-server.toml") && message.contains("[identity]"),
        "{message}"
    );
    let configured = write_config(dir.path(), true)?;
    let (mut server, log) = spawn_serve(&configured)?;
    let lines = until(&log, "keys").map(|lines| lines.concat());
    let stopped = server.0.wait()?;
    assert!(
        !stopped.success(),
        "a server with no key directory does not start"
    );
    let message = lines?;
    assert!(
        message.contains("tessel-server.toml") && message.contains("[identity] keys"),
        "{message}"
    );
    Ok(())
}

#[test]
fn admin_a_server_started_with_identity_admits_a_minted_token_and_logs_neither_it_nor_the_seed(
) -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = write_config(dir.path(), true)?;
    assert!(keygen(&config, "k1")?.status.success());
    let minted = admin(&config, &token_args("k1", "a1", &[]))?;
    let stdout = text(&minted.stdout);
    let token = stdout.trim().to_string();
    let seed = std::fs::read_to_string(dir.path().join("keys/k1.key"))?;
    let (server, log) = spawn_serve(&config)?;
    let startup = until(&log, "listening")?;
    assert!(
        !startup
            .iter()
            .any(|line| line.contains("no identity is configured")),
        "{startup:?}"
    );
    assert!(
        startup.iter().any(|line| line.contains("identity loaded")),
        "{startup:?}"
    );
    let last = startup.last().ok_or("no lines")?;
    let addr = last
        .split("addr=")
        .nth(1)
        .ok_or("no addr")?
        .trim()
        .to_string();
    let summary = "/tenant/acme/repo/demo/summary";
    assert!(get(&addr, summary, Some(&token))?.starts_with("HTTP/1.1 200"));
    assert!(get(&addr, summary, None)?.starts_with("HTTP/1.1 401"));
    assert!(get(&addr, summary, Some("nonsense"))?.starts_with("HTTP/1.1 401"));
    drop(server);
    let mut everything = startup.concat();
    while let Ok(line) = log.recv_timeout(Duration::from_secs(5)) {
        everything.push_str(&line);
    }
    assert!(!everything.contains(&token), "the token is never logged");
    assert!(
        !everything.contains(seed.trim()),
        "the seed is never logged"
    );
    Ok(())
}
