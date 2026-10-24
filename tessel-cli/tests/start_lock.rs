//! `tessel start` when it cannot get a daemon online: a worktree lock that another process holds,
//! and a daemon that runs but cannot connect.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]
#![expect(
    dead_code,
    reason = "each test crate compiles all of support, which the other test crates also use"
)]

mod support;

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use support::{eventually, Agent, Fake};

const TOK1: &str = "tok-a1-S3CRETvalue";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_says_the_lock_is_held_when_the_new_daemon_gives_way_and_none_comes_online(
) -> Result<()> {
    let fake = Fake::start(30_000, &[("a1", TOK1)]).await?;
    fake.freeze_clock();
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    let dir = a1.root().join(".tessel");
    std::fs::create_dir_all(&dir)?;
    let lock_path = dir.join("daemon.lock");
    // A daemon of an older `tessel` held this lock with `flock`; the new daemon sees it as held
    // and exits cleanly, and nothing answers on the socket.
    let holder = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    holder.lock()?;

    let done = a1.tessel(&["start", "behind a held lock"])?;

    assert_eq!(done.code, 1, "{}", done.all());
    assert!(
        done.stderr.contains(&lock_path.display().to_string()),
        "{}",
        done.stderr
    );
    assert!(
        done.stderr.contains("held by another process"),
        "{}",
        done.stderr
    );
    assert!(
        done.stderr.contains("`tessel stop`"),
        "the message does not say what to do: {}",
        done.stderr
    );
    assert!(
        !done.stderr.contains("running (pid"),
        "the exited daemon is named as running: {}",
        done.stderr
    );
    assert!(!done.all().contains("S3CRETvalue"), "{}", done.all());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_still_says_running_but_not_connected_for_a_daemon_that_is_alive() -> Result<()> {
    let fake = Fake::start(30_000, &[("a1", TOK1)]).await?;
    fake.freeze_clock();
    fake.set_accepting(false);
    let a1 = Agent::new(&fake, "a1", TOK1)?;

    let done = a1.tessel(&["start", "nobody to talk to"])?;

    assert_eq!(done.code, 1, "{}", done.all());
    assert!(
        done.stderr.contains("the daemon is running (pid"),
        "{}",
        done.stderr
    );
    assert!(done.stderr.contains("but not connected"), "{}", done.stderr);
    assert!(!done.stderr.contains("lock"), "{}", done.stderr);
    Ok(())
}

/// Puts a socket back once the second daemon has logged that it gave way. Until then the
/// second `start` finds no daemon at the path, so it spawns one; afterwards its probes answer.
fn restore_when_a_daemon_gives_way(
    hidden: PathBuf,
    socket: PathBuf,
    log: PathBuf,
) -> std::thread::JoinHandle<Result<()>> {
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if text.contains("another daemon holds the lock") {
                std::fs::rename(&hidden, &socket)?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        std::fs::rename(&hidden, &socket)?;
        anyhow::bail!("no daemon logged that it gave way")
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_start_names_the_offline_daemon_that_holds_the_lock_when_its_own_gives_way(
) -> Result<()> {
    let fake = Fake::start(30_000, &[("a1", TOK1)]).await?;
    fake.freeze_clock();
    fake.set_accepting(false);
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    let mut winner = a1.spawn_daemon("the first start")?;
    let state = eventually(Duration::from_secs(8), || {
        Ok(a1
            .status()
            .ok()
            .filter(|status| status["state"]["pid"].is_u64()))
    })
    .await?["state"]
        .clone();
    let socket = PathBuf::from(state["socket"].as_str().unwrap_or_default());
    let hidden = socket.with_extension("hidden");
    // The pre-check of the second start finds no daemon, as in a race where the first daemon
    // holds the lock but has not bound its socket yet. The first daemon keeps its socket open.
    std::fs::rename(&socket, &hidden)?;
    let log = a1.root().join(".tessel/daemon.log");
    let restorer = restore_when_a_daemon_gives_way(hidden, socket, log);

    let done = a1.tessel(&["start", "the second start"])?;

    let restored = restorer
        .join()
        .map_err(|_| anyhow::anyhow!("restorer panicked"))?;
    let winner_ran = winner.try_wait()?.is_none();
    winner.kill()?;
    winner.wait()?;
    restored?;
    assert!(winner_ran, "the first daemon exited: {}", done.all());
    assert_eq!(
        done.code,
        1,
        "the second start did not lose: {}",
        done.all()
    );
    assert!(
        done.stderr.contains("did not start"),
        "the second start does not say it lost: {}",
        done.stderr
    );
    assert!(
        done.stderr.contains(&format!("pid {}", state["pid"])),
        "the loser does not name the winner (pid {}): {}",
        state["pid"],
        done.stderr
    );
    assert!(
        !done.all().contains("no daemon answers"),
        "a daemon answers for this worktree: {}",
        done.all()
    );
    Ok(())
}
