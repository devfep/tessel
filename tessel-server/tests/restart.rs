//! The M3 done-check's loop, once: the real `tessel-server` binary is killed with SIGKILL while a
//! submitted commit's tests are running, started again on the same directories, and the claim
//! lands exactly once. The runner's command is a script on `PATH` that waits for a marker file,
//! so the kill falls inside the test run and not before or after it.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

#[expect(
    dead_code,
    reason = "each test crate compiles all of support and uses only some of it"
)]
mod support;

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::protocol::{
    AgentId, ClientMsg, CommitId, DecisionRecord, EventKind, Intent, Mode, OnConflict, RequestId,
    Scope, ScopeClaim, ServerMsg, PROTOCOL_VERSION,
};
use tessel_server::admin::{self, TokenArgs};
use tessel_server::config::IdentityConfig;
use tessel_server::id::RepoId;
use tessel_server::identity;
use tessel_server::keys;
use tessel_server::store::Store;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use support::{git, Repos};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BINARY: &str = env!("CARGO_BIN_EXE_tessel-server");
const REPO: &str = "restart";
const WAIT: Duration = Duration::from_secs(120);

/// A server process that is killed when the test ends, however it ends.
struct Server {
    child: Child,
    addr: SocketAddr,
    log: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start(config: &Path, path: &str, home: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut child = Command::new(BINARY)
            .args(["serve", "--config"])
            .arg(config)
            .env_clear()
            .env("PATH", path)
            .env("HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let (lines, heard) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    return;
                }
            }
        });
        let mut seen = Vec::new();
        let addr = loop {
            let line = heard
                .recv_timeout(Duration::from_secs(30))
                .map_err(|_| format!("the server never listened; it said {seen:?}"))?;
            if let Some((_, addr)) = line.split_once("listening addr=") {
                break addr.trim().parse()?;
            }
            seen.push(line);
        };
        let log = Arc::new(Mutex::new(seen));
        let kept = Arc::clone(&log);
        // Keeps reading so the server never blocks on a full pipe.
        std::thread::spawn(move || {
            for line in heard {
                kept.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(line);
            }
        });
        Ok(Self { child, addr, log })
    }

    /// What the server has logged so far.
    fn log(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// SIGKILL: the process gets no chance to clean up.
    fn kill9(mut self) -> TestResult {
        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The directories and files of one deployment under the fixture's root.
struct Deployment {
    repos: Repos,
    config: PathBuf,
    path: String,
    home: PathBuf,
    marks: PathBuf,
    token: String,
}

impl Deployment {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let repos = Repos::new();
        let root = repos.root.path().to_path_buf();
        let (bin, marks, home) = (root.join("bin"), root.join("marks"), root.join("home"));
        for dir in [&bin, &marks, &home] {
            std::fs::create_dir_all(dir)?;
        }
        let waits = format!(
            "#!/bin/sh\necho started > {marks}/started\nwhile [ ! -e {marks}/release ] \
             && [ -d {marks} ]; do \
             sleep 0.1; done\nexit 0\n",
            marks = marks.display()
        );
        std::fs::write(bin.join("cargo"), waits)?;
        std::fs::set_permissions(bin.join("cargo"), std::fs::Permissions::from_mode(0o755))?;
        repos.write_runner_config(0o600);
        let identity = IdentityConfig {
            keys: root.join("keys"),
            audience: "restart-test".to_string(),
        };
        keys::generate(&identity.keys, "k1")?;
        let args = TokenArgs {
            kid: "k1",
            tenant: "acme",
            repo: REPO,
            agent: "a1",
            ttl_ms: 3_600_000,
            service: false,
        };
        let token = admin::token(&identity, &args, identity::now_ms())?;
        let lander = repos.lander();
        let config = root.join("tessel-server.toml");
        std::fs::write(
            &config,
            format!(
                "listen = \"127.0.0.1:0\"\nstore_dir = {store:?}\n\n[[repository]]\n\
                 tenant = \"acme\"\nname = {REPO:?}\nrun = \"restart\"\nremote = {remote:?}\n\
                 fork = {fork:?}\n\n[lander]\nwork_dir = {work:?}\nrunner_config = {runner:?}\n\n\
                 [identity]\nkeys = {keys:?}\naudience = \"restart-test\"\n",
                store = root.join("stores").display().to_string(),
                remote = repos.main.display().to_string(),
                fork = format!("{}/{{agent}}.git", repos.forks.display()),
                work = lander.work_dir.display().to_string(),
                runner = lander.runner_config.display().to_string(),
                keys = identity.keys.display().to_string(),
            ),
        )?;
        let path = format!("{}:{}", bin.display(), std::env::var("PATH")?);
        Ok(Self {
            repos,
            config,
            path,
            home,
            marks,
            token,
        })
    }

    fn start(&self) -> Result<Server, Box<dyn std::error::Error>> {
        Server::start(&self.config, &self.path, &self.home)
    }

    fn events(&self) -> Result<Vec<EventKind>, Box<dyn std::error::Error>> {
        let file = RepoId::new("acme", REPO)?.store_file(&self.repos.root.path().join("stores"));
        let events = Store::open_read_only(&file)?.read_events(0, 1000)?;
        Ok(events.into_iter().map(|event| event.kind).collect())
    }

    /// The candidate refs on the trunk: the heads `Prepare` pushed and no `Land` has deleted.
    fn candidate_refs(&self) -> String {
        git(
            &self.repos.main,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/tessel/candidate",
            ],
        )
    }

    /// What `docs/dogfood.md` tells the operator to run before an upgrade, as `rusqlite` runs it:
    /// the JSON type of the stored `merge_in_flight`, `null` with no merge in flight.
    fn merge_in_flight(&self) -> Result<String, Box<dyn std::error::Error>> {
        let file = RepoId::new("acme", REPO)?.store_file(&self.repos.root.path().join("stores"));
        let conn = rusqlite::Connection::open_with_flags(
            file,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        Ok(conn.query_row(
            "select json_type(json, '$.merge_in_flight') from kv where key = 'core'",
            [],
            |row| row.get(0),
        )?)
    }

    /// What `docs/dogfood.md` tells the operator to read for a `Land` in doubt: the head and base
    /// the `Land` journaled, as `rusqlite` runs the runbook's query.
    fn journaled_land(&self) -> Result<(String, String), Box<dyn std::error::Error>> {
        let file = RepoId::new("acme", REPO)?.store_file(&self.repos.root.path().join("stores"));
        let conn = rusqlite::Connection::open_with_flags(
            file,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        Ok(conn.query_row(
            "select json_extract(json, '$.merge_in_flight.stage.head'), \
                    json_extract(json, '$.merge_in_flight.stage.base') \
             from kv where key = 'core'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    }

    fn calls_left(&self) -> Vec<String> {
        let dir = self.repos.clones(REPO);
        let entries = std::fs::read_dir(dir).into_iter().flatten().flatten();
        let names = entries.map(|entry| entry.file_name().to_string_lossy().into_owned());
        names.filter(|name| name.starts_with("call-")).collect()
    }
}

async fn wait_for(what: &str, mut ready: impl FnMut() -> bool) -> TestResult {
    let until = Instant::now() + WAIT;
    while !ready() {
        if Instant::now() > until {
            return Err(format!("never saw {what}").into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

/// An agent that claims `src/a.rs` and submits `commit`, and is told it was accepted.
async fn submit(server: &Server, token: &str, commit: &str) -> TestResult {
    let mut request =
        format!("ws://{}/tenant/acme/repo/{REPO}/ws", server.addr).into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse()?);
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await?;
    let edit = ScopeClaim {
        scope: Scope::File {
            path: "src/a.rs".to_string(),
        },
        mode: Mode::EditBody,
    };
    let hello = ClientMsg::Hello {
        agent: AgentId("a1".to_string()),
        base: CommitId("ab".repeat(20)),
        protocol: PROTOCOL_VERSION,
    };
    let claim = ClientMsg::Claim {
        req: RequestId(1),
        intent: Intent {
            summary: "edit src/a.rs".to_string(),
            task_ref: None,
            assumptions: Vec::new(),
        },
        scopes: vec![edit.clone()],
        on_conflict: OnConflict::Fail,
    };
    socket
        .send(Message::text(serde_json::to_string(&hello)?))
        .await?;
    socket
        .send(Message::text(serde_json::to_string(&claim)?))
        .await?;
    let mut granted = None;
    while granted.is_none() {
        let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await?
            .ok_or("the socket closed")??;
        if let Message::Text(text) = frame {
            if let ServerMsg::Granted { claim, fence, .. } = serde_json::from_str(&text)? {
                granted = Some((claim, fence));
            }
        }
    }
    let (claim, fence) = granted.ok_or("no grant")?;
    let submit = ClientMsg::Submit {
        req: RequestId(2),
        claim,
        fence,
        fork_commit: CommitId(commit.to_string()),
        touched: vec![edit],
        decisions: DecisionRecord {
            evidence: vec!["the tests passed".to_string()],
            ..DecisionRecord::default()
        },
    };
    socket
        .send(Message::text(serde_json::to_string(&submit)?))
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_server_killed_mid_merge_and_restarted_lands_the_claim_once() -> TestResult {
    let deployment = Deployment::new()?;
    let before = deployment.repos.main_head();
    let commit = deployment.repos.fork_commit(&[("src/a.rs", "two\n")]);
    let first = deployment.start()?;
    submit(&first, &deployment.token, &commit).await?;
    let started = deployment.marks.join("started");
    wait_for("the runner's command to start", || started.exists()).await?;
    assert_eq!(
        deployment.repos.main_head(),
        before,
        "the tests run before main moves"
    );
    assert_eq!(
        deployment.calls_left().len(),
        1,
        "the merge's clones are on disk"
    );
    assert_eq!(
        deployment.candidate_refs().lines().count(),
        1,
        "the head is on the candidate ref while its tests run"
    );
    assert_eq!(
        deployment.merge_in_flight()?,
        "object",
        "the stored state says a merge is in flight"
    );

    first.kill9()?;
    std::fs::write(deployment.marks.join("release"), "go")?;
    assert_eq!(
        deployment.repos.main_head(),
        before,
        "a killed server landed nothing"
    );

    let second = deployment.start()?;
    let moved = wait_for("main to move", || deployment.repos.main_head() != before).await;
    if let Err(error) = moved {
        return Err(format!("{error}; the second server logged {:#?}", second.log()).into());
    }
    let landed = deployment.repos.main_head();
    wait_for("the Merged event", || {
        deployment.events().is_ok_and(|events| {
            events
                .iter()
                .any(|kind| matches!(kind, EventKind::Merged { .. }))
        })
    })
    .await?;
    let events = deployment.events()?;
    let merges: Vec<_> = events
        .iter()
        .filter(|kind| matches!(kind, EventKind::Merged { .. }))
        .collect();
    assert_eq!(merges.len(), 1, "the claim merged once: {events:?}");
    if let Some(EventKind::Merged { head, .. }) = merges.first() {
        assert_eq!(
            head.0, landed,
            "the claim was told the head that is on main"
        );
    }
    let rejected = events
        .iter()
        .any(|kind| matches!(kind, EventKind::SubmitRejected { .. }));
    assert!(!rejected, "nothing was rejected: {events:?}");
    assert_eq!(
        git(
            &deployment.repos.main,
            &["rev-list", "--count", &format!("{before}..main")]
        ),
        "1",
        "main took one commit"
    );
    assert_eq!(deployment.repos.show_on_main("src/a.rs"), "two");
    wait_for("the clones to be removed", || {
        deployment.calls_left().is_empty()
    })
    .await?;
    assert_eq!(
        deployment.candidate_refs(),
        "",
        "the landing after the restart deleted the candidate ref"
    );
    assert_eq!(
        deployment.merge_in_flight()?,
        "null",
        "and the stored state says nothing is in flight"
    );
    second.kill9()?;
    Ok(())
}

/// A `post-receive` hook on the trunk that records each push of `main` and holds the first one
/// open until the test releases it. A push of a candidate ref passes. `git push` returns only when
/// the hook exits, so the lander is inside the push of its `Land`, after the trunk moved and
/// before it reads the trunk back or the server answers.
/// The wait also ends when the marks directory is gone, so a failed test leaves no hook looping;
/// while the test runs the directory exists, so the passing path is unchanged.
fn hold_pushes_open(deployment: &Deployment) -> TestResult {
    let marks = deployment.marks.display();
    let hook = format!(
        "#!/bin/sh\nwhile read old new ref; do\n[ \"$ref\" = refs/heads/main ] || continue\n\
         echo push >> {marks}/pushes\n: > {marks}/hook-started\nwhile [ ! -e \
         {marks}/hook-release ] && [ -d {marks} ]; do /bin/sleep 0.1; done\n\
         : > {marks}/hook-done\ndone\nexit 0\n"
    );
    let path = deployment.repos.main.join("hooks/post-receive");
    std::fs::create_dir_all(path.parent().ok_or("no hooks dir")?)?;
    std::fs::write(&path, hook)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn pushes_seen(deployment: &Deployment) -> usize {
    let log = std::fs::read_to_string(deployment.marks.join("pushes")).unwrap_or_default();
    log.lines().count()
}

/// A server killed with `SIGKILL` after the lander's push reached the trunk and before the server
/// answered the `Land`. The restart finds the `Land` marker and reconciles it, counting nothing:
/// the lander finds the journaled head on the trunk by ancestry, and the core lands the claim from
/// the base the `Land` leased. The claim ends landed exactly once: one new commit on the trunk,
/// one `Merged` event naming it, one release, and no second push. The candidate ref the killed
/// `Land` did not get to delete stays until a later `Prepare` sweeps it.
#[tokio::test]
async fn a_server_killed_after_the_push_and_restarted_counts_the_landing_once() -> TestResult {
    let deployment = Deployment::new()?;
    std::fs::write(deployment.marks.join("release"), "go")?;
    hold_pushes_open(&deployment)?;
    let before = deployment.repos.main_head();
    let commit = deployment.repos.fork_commit(&[("src/a.rs", "two\n")]);
    let first = deployment.start()?;
    submit(&first, &deployment.token, &commit).await?;
    let pushing = deployment.marks.join("hook-started");
    wait_for("the push to reach the trunk's hook", || pushing.exists()).await?;
    let landed = deployment.repos.main_head();
    assert_ne!(landed, before, "post-receive runs after the trunk moved");
    assert_eq!(pushes_seen(&deployment), 1);
    let candidates = deployment.candidate_refs();
    assert_eq!(
        candidates.lines().count(),
        1,
        "the killed Land left its candidate ref"
    );
    let (head, base) = deployment.journaled_land()?;
    assert_eq!(base, before, "the Land leased the main it found");
    let on_main = Command::new("git")
        .current_dir(&deployment.repos.main)
        .args(["merge-base", "--is-ancestor", &head, "main"])
        .status()?;
    assert!(
        on_main.success(),
        "the runbook's read finds the journaled head on main"
    );

    first.kill9()?;
    let unanswered = deployment.events()?;
    assert!(
        !unanswered
            .iter()
            .any(|kind| matches!(kind, EventKind::Merged { .. })),
        "the killed server never recorded the landing: {unanswered:?}"
    );
    std::fs::write(deployment.marks.join("hook-release"), "go")?;
    let finished = deployment.marks.join("hook-done");
    wait_for("the held push to finish", || finished.exists()).await?;

    let second = deployment.start()?;
    let recorded = wait_for("the landing to be recorded", || {
        deployment.events().is_ok_and(|events| {
            events.iter().any(|kind| {
                matches!(
                    kind,
                    EventKind::Merged { .. } | EventKind::SubmitRejected { .. }
                )
            })
        })
    })
    .await;
    if let Err(error) = recorded {
        return Err(format!("{error}; the second server logged {:#?}", second.log()).into());
    }
    let events = deployment.events()?;
    let mut merged = Vec::new();
    for kind in &events {
        if let EventKind::Merged { head, .. } = kind {
            merged.push(head.0.as_str());
        }
    }
    assert_eq!(merged, [landed.as_str()], "one landing, at the pushed head");
    let released = events
        .iter()
        .filter(|kind| matches!(kind, EventKind::ClaimReleased { .. }))
        .count();
    assert_eq!(released, 1, "the claim was released once: {events:?}");
    let rejected = events
        .iter()
        .any(|kind| matches!(kind, EventKind::SubmitRejected { .. }));
    assert!(!rejected, "nothing was rejected: {events:?}");
    assert_eq!(
        deployment.repos.main_head(),
        landed,
        "main did not move again"
    );
    assert_eq!(
        git(
            &deployment.repos.main,
            &["rev-list", "--count", &format!("{before}..main")]
        ),
        "1",
        "main took one commit"
    );
    assert_eq!(
        pushes_seen(&deployment),
        1,
        "the restart did not push again"
    );
    assert_eq!(
        deployment.candidate_refs(),
        candidates,
        "the reconcile keeps the candidate ref, for a later Prepare to sweep"
    );
    wait_for("the clones to be removed", || {
        deployment.calls_left().is_empty()
    })
    .await?;
    second.kill9()?;
    Ok(())
}
