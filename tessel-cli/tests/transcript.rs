//! `tessel submit --transcript`: the real binary and daemon against the fake coordinator, which
//! runs the crate's own `Coordinator` core. The transcript is redacted, stored on a local branch
//! without touching the working tree, and named by hash in the submission's decision record.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]
#![expect(
    dead_code,
    reason = "each test crate compiles all of support, which the other test crate also uses"
)]

mod support;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use support::{git, Agent, Fake, REPO};
use tessel_coordinator::protocol::{
    ClaimId, ClientMsg, Fence, Mode, RequestId, Scope, ScopeClaim, ServerMsg, TranscriptRef,
};

const TOK1: &str = "tok-a1-S3CRETvalue";
const LIMIT: usize = 4 * 1024 * 1024;

async fn world() -> Result<(Fake, Agent)> {
    let fake = Fake::start(30_000, &[("a1", TOK1)]).await?;
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    Ok((fake, a1))
}

fn commit_a(agent: &Agent) -> Result<String> {
    let root = agent.root();
    std::fs::write(root.join("src/a.rs"), "pub fn a() { 1; }\n")?;
    git(&root, &["add", "src/a.rs"])?;
    git(&root, &["commit", "-q", "-m", "change a"])?;
    Ok(git(&root, &["rev-parse", "HEAD"])?.trim().to_string())
}

fn first_claim(agent: &Agent) -> Result<Value> {
    agent.status()?["state"]["claims"][0]
        .as_object()
        .map(|claim| Value::Object(claim.clone()))
        .context("no claim in status")
}

fn claim_id(agent: &Agent) -> Result<u64> {
    first_claim(agent)?["claim"].as_u64().context("claim id")
}

/// A claimed, committed change of `src/a.rs`, ready to submit; returns the claim id.
fn ready(agent: &Agent) -> Result<u64> {
    agent.start("fix a")?;
    let claimed = agent.tessel(&["claim", "src/a.rs"])?;
    assert_eq!(claimed.code, 0, "{}", claimed.all());
    commit_a(agent)?;
    claim_id(agent)
}

/// A transcript file outside the agent's repository, so the repository stays clean.
fn transcript_file(dir: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let path = dir.join("session.md");
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn submit_with(agent: &Agent, path: &Path) -> Result<support::Done> {
    let path = path.to_str().context("temp path is not UTF-8")?;
    agent.tessel(&["submit", "--evidence", "e1", "--transcript", path])
}

/// The exact bytes the transcript branch of `claim` holds.
fn stored(agent: &Agent, claim: u64) -> Result<Vec<u8>> {
    let spec = format!("tessel/transcript-{claim}:transcripts/{claim}.md");
    let output = Command::new("git")
        .arg("-C")
        .arg(agent.root())
        .args(["show", &spec])
        .output()?;
    if !output.status.success() {
        bail!(
            "git show {spec}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}

fn branch_exists(agent: &Agent, claim: u64) -> bool {
    let full = format!("refs/heads/tessel/transcript-{claim}");
    git(&agent.root(), &["show-ref", "--verify", "--quiet", &full]).is_ok()
}

fn sha256_hex(bytes: &[u8]) -> Result<String> {
    let mut hex = String::new();
    for byte in &Sha256::digest(bytes) {
        write!(hex, "{byte:02x}")?;
    }
    Ok(hex)
}

/// The `transcript` of every `Submit` the fake coordinator received from `agent`, in order.
fn sent_transcripts(fake: &Fake, agent: &str) -> Vec<Option<TranscriptRef>> {
    fake.received(agent)
        .into_iter()
        .filter_map(|msg| match msg {
            ClientMsg::Submit { decisions, .. } => Some(decisions.transcript),
            ClientMsg::Hello { .. }
            | ClientMsg::Claim { .. }
            | ClientMsg::Amend { .. }
            | ClientMsg::Heartbeat
            | ClientMsg::Release { .. }
            | ClientMsg::Review { .. }
            | ClientMsg::OpenRace { .. }
            | ClientMsg::JoinRace { .. }
            | ClientMsg::PickWinner { .. }
            | ClientMsg::Watch { .. } => None,
        })
        .collect()
}

const STAYS_LOCAL: &str = "it stays in this repository, because no transcript copy exists yet";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_redacted_transcript_is_stored_on_its_branch_and_named_by_hash() -> Result<()> {
    let (fake, a1) = world().await?;
    let claim = ready(&a1)?;
    let outside = tempfile::tempdir()?;
    let original = b"# session\ntoken art_vABC.def here\nAuthorization: Bearer xyz123\nend\n";
    let path = transcript_file(outside.path(), original)?;

    let done = submit_with(&a1, &path)?;
    assert_eq!(done.code, 0, "{}", done.all());

    let bytes = stored(&a1, claim)?;
    let text = String::from_utf8(bytes.clone())?;
    assert!(!text.contains("art_vABC.def"), "{text}");
    assert!(!text.contains("xyz123"), "{text}");
    assert!(text.contains("art_v…[redacted]"), "{text}");
    assert!(
        text.contains("Authorization: Bearer [redacted]\n"),
        "{text}"
    );
    assert!(
        text.starts_with("# session\n") && text.ends_with("end\n"),
        "{text}"
    );
    assert_eq!(
        sent_transcripts(&fake, "a1"),
        vec![Some(TranscriptRef {
            repo: format!("{REPO}--a1"),
            path: format!("transcripts/{claim}.md"),
            sha256: sha256_hex(&bytes)?,
        })]
    );
    assert_eq!(git(&a1.root(), &["status", "--porcelain"])?, "");
    assert!(done.stdout.contains(STAYS_LOCAL), "{}", done.stdout);
    let sha = sha256_hex(&bytes)?;
    assert!(
        done.stdout.contains(&format!(
            "transcript: tessel/transcript-{claim} holds transcripts/{claim}.md (sha256 {}…); ",
            &sha[..12]
        )),
        "{}",
        done.stdout
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_transcript_none_is_sent_and_nothing_is_said() -> Result<()> {
    let (fake, a1) = world().await?;
    let claim = ready(&a1)?;
    let done = a1.tessel(&["submit", "--evidence", "e1"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(sent_transcripts(&fake, "a1"), vec![None]);
    assert!(!done.stdout.contains("transcript"), "{}", done.stdout);
    assert!(!branch_exists(&a1, claim));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_transcript_is_refused_before_anything_is_sent() -> Result<()> {
    let (fake, a1) = world().await?;
    let claim = ready(&a1)?;
    let outside = tempfile::tempdir()?;
    let path = transcript_file(outside.path(), b"")?;

    let done = submit_with(&a1, &path)?;
    assert_eq!(done.code, 1, "{}", done.all());
    assert!(
        done.stderr.contains("the transcript file is empty"),
        "{}",
        done.all()
    );
    assert_eq!(sent_transcripts(&fake, "a1"), []);
    assert!(!branch_exists(&a1, claim));
    let head = git(&a1.root(), &["rev-parse", "HEAD"])?;
    assert!(git(&a1.root(), &["notes", "show", head.trim()]).is_err());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transcript_over_four_mib_is_refused_and_one_at_four_mib_is_sent() -> Result<()> {
    let (fake, a1) = world().await?;
    let claim = ready(&a1)?;
    let outside = tempfile::tempdir()?;
    let path = transcript_file(outside.path(), &vec![b'a'; LIMIT + 1])?;

    let done = submit_with(&a1, &path)?;
    assert_eq!(done.code, 1, "{}", done.all());
    assert!(
        done.stderr.contains(&format!("at most {LIMIT} bytes")),
        "{}",
        done.all()
    );
    assert_eq!(sent_transcripts(&fake, "a1"), []);
    assert!(!branch_exists(&a1, claim));

    let path = transcript_file(outside.path(), &vec![b'a'; LIMIT])?;
    let done = submit_with(&a1, &path)?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(stored(&a1, claim)?.len(), LIMIT);
    assert_eq!(sent_transcripts(&fake, "a1").len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submitting_again_with_a_changed_transcript_replaces_the_branch() -> Result<()> {
    let (fake, a1) = world().await?;
    let claim = ready(&a1)?;
    let outside = tempfile::tempdir()?;
    let path = transcript_file(outside.path(), b"first session\n")?;
    let first = submit_with(&a1, &path)?;
    assert_eq!(first.code, 0, "{}", first.all());
    assert!(!first.stdout.contains("replaced"), "{}", first.stdout);

    fake.push(
        "a1",
        ServerMsg::SubmitRejected {
            claim: ClaimId(claim),
            reason: "tests failed".into(),
            reviewer: None,
            note: None,
        },
    );
    support::eventually(std::time::Duration::from_secs(8), || {
        Ok((first_claim(&a1)?["submitted"] == false).then_some(()))
    })
    .await?;

    let path = transcript_file(outside.path(), b"second session\n")?;
    let second = submit_with(&a1, &path)?;
    assert!(
        second.stdout.contains(&format!(
            "transcript branch tessel/transcript-{claim} replaced\n"
        )),
        "{}",
        second.all()
    );
    assert_eq!(stored(&a1, claim)?, b"second session\n");
    let sent = sent_transcripts(&fake, "a1");
    let hashes: Vec<String> = sent
        .iter()
        .map(|t| t.as_ref().map(|t| t.sha256.clone()).unwrap_or_default())
        .collect();
    assert_eq!(
        hashes,
        vec![
            sha256_hex(b"first session\n")?,
            sha256_hex(b"second session\n")?
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_transcript_line_comes_before_the_note_and_a_refusal_keeps_the_branch() -> Result<()> {
    let (fake, a1) = world().await?;
    let claim = ready(&a1)?;
    // An amend the daemon does not know about retires the fence it holds, so its submit is stale.
    let held = first_claim(&a1)?;
    fake.act(
        "a1",
        ClientMsg::Amend {
            req: RequestId(902),
            claim: ClaimId(claim),
            fence: Fence(held["fence"].as_u64().context("fence")?),
            add: vec![ScopeClaim {
                scope: Scope::File {
                    path: "src/b.rs".into(),
                },
                mode: Mode::EditBody,
            }],
        },
    );
    let outside = tempfile::tempdir()?;
    let path = transcript_file(outside.path(), b"refused session\n")?;

    let done = submit_with(&a1, &path)?;
    assert_eq!(done.code, 6, "{}", done.all());
    let transcript_at = done
        .stdout
        .find("transcript: tessel/transcript-")
        .context(done.stdout.clone())?;
    let note_at = done
        .stdout
        .find("note written on")
        .context(done.stdout.clone())?;
    assert!(transcript_at < note_at, "{}", done.stdout);
    assert_eq!(stored(&a1, claim)?, b"refused session\n");
    Ok(())
}
