//! The real binary: `tessel-server serve --config <file>` starts, creates the stores, says it
//! listens and that no identity is configured, answers 401 on `/ws`, and a second one on the same
//! directory refuses to start and names the first.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BINARY: &str = env!("CARGO_BIN_EXE_tessel-server");

fn write_config(dir: &Path) -> Result<std::path::PathBuf, std::io::Error> {
    let path = dir.join("tessel-server.toml");
    let store = dir.join("stores");
    std::fs::write(
        &path,
        format!(
            "listen = \"127.0.0.1:0\"\nstore_dir = {:?}\n\n[[repository]]\ntenant = \"acme\"\n\
             name = \"demo\"\nrun = \"test\"\n",
            store.display().to_string()
        ),
    )?;
    Ok(path)
}

/// A server process that is killed when the test ends, however it ends.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(config: &Path) -> Result<Server, std::io::Error> {
    Command::new(BINARY)
        .args(["serve", "--config"])
        .arg(config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map(Server)
}

/// Lines of the child's stderr, as they arrive.
fn lines(server: &mut Server) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let stderr = server.0.stderr.take();
    std::thread::spawn(move || {
        let Some(stderr) = stderr else { return };
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    rx
}

/// The lines up to and including the first one that has `needle`.
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

#[test]
fn serve_starts_creates_the_stores_and_answers_401_with_no_identity() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = write_config(dir.path())?;
    let mut child = spawn(&config)?;
    let log = lines(&mut child);
    let startup = until(&log, "listening")?;
    assert!(
        startup
            .iter()
            .any(|line| line.contains("no identity is configured")),
        "{startup:?}"
    );
    let addr = startup
        .last()
        .ok_or("no lines")?
        .split("addr=")
        .nth(1)
        .ok_or("no addr in the line")?
        .trim()
        .to_string();
    assert!(
        dir.path().join("stores/acme/demo.sqlite").is_file(),
        "the store is created"
    );

    let mut stream = TcpStream::connect(&addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "GET /tenant/acme/repo/demo/ws HTTP/1.1\r\nHost: x\r\nConnection: Upgrade\r\n\
         Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nAuthorization: Bearer anything\r\n\r\n"
    )?;
    let mut buffer = [0_u8; 512];
    let read = stream.read(&mut buffer)?;
    let answer = String::from_utf8_lossy(&buffer[..read]).into_owned();
    assert!(answer.starts_with("HTTP/1.1 401"), "{answer}");

    let mut second = spawn(&config)?;
    let refusal = lines(&mut second);
    let line = until(&refusal, "in use by tessel-server pid")?.concat();
    assert!(
        line.contains(&child.0.id().to_string()),
        "names the first server: {line}"
    );
    assert!(
        !second.0.wait()?.success(),
        "the second server exits with a failure"
    );
    Ok(())
}
