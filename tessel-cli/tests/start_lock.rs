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

use anyhow::Result;
use support::{Agent, Fake};

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_starts_against_an_offline_coordinator_name_the_daemon_that_answers() -> Result<()> {
    let fake = Fake::start(30_000, &[("a1", TOK1)]).await?;
    fake.freeze_clock();
    fake.set_accepting(false);
    let a1 = Agent::new(&fake, "a1", TOK1)?;

    let (first, second) = std::thread::scope(|scope| {
        let one = scope.spawn(|| a1.tessel(&["start", "racer one"]));
        let two = scope.spawn(|| a1.tessel(&["start", "racer two"]));
        (one.join(), two.join())
    });
    let outputs = [
        first.map_err(|_| anyhow::anyhow!("first start panicked"))??,
        second.map_err(|_| anyhow::anyhow!("second start panicked"))??,
    ];

    let winner = a1.status()?["state"]["pid"].to_string();
    for done in &outputs {
        assert!(
            !done.all().contains("no daemon answers"),
            "a daemon answers for this worktree: {}",
            done.all()
        );
    }
    let losers: Vec<_> = outputs
        .iter()
        .filter(|done| done.stderr.contains("did not start"))
        .collect();
    for loser in losers {
        assert!(
            loser.stderr.contains(&format!("pid {winner}")),
            "the loser does not name the winner (pid {winner}): {}",
            loser.stderr
        );
    }
    Ok(())
}
