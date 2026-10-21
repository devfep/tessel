//! `tessel submit --transcript`: redacts a session transcript, stores it on a local branch of its
//! own without touching the working tree or the index, and names it by hash in the decision
//! record. It is prepared for the evidence record (P1-EVIDENCE); nothing copies it today. A
//! transcript is untrusted text: nothing here reads meaning into it.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context};
use sha2::{Digest, Sha256};
use tessel_coordinator::protocol::TranscriptRef;

use crate::render::escape;

/// The largest transcript this CLI accepts.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

const TOKEN_PREFIX: &[u8] = b"art_v";
const TOKEN_REDACTED: &str = "art_v…[redacted]";
const BEARER: &[u8] = b"bearer ";
const BEARER_REDACTED: &[u8] = b"[redacted]";

/// A transcript read, redacted and hashed, not yet stored.
pub struct Prepared {
    bytes: Vec<u8>,
    sha256: String,
}

/// Reads the transcript at `path`, redacts it and checks the size limit.
pub fn prepare(path: &Path) -> anyhow::Result<Prepared> {
    let shown = escape(&path.display().to_string());
    let raw = std::fs::read(path).with_context(|| format!("cannot read the transcript {shown}"))?;
    if raw.is_empty() {
        bail!("the transcript file is empty: {shown}");
    }
    let bytes = redact(&raw);
    if bytes.len() > MAX_BYTES {
        bail!(
            "the transcript {shown} is {} bytes after redaction; the limit is \
             {MAX_BYTES} bytes (4 MiB). Trim it and submit again",
            bytes.len()
        );
    }
    let mut sha256 = String::with_capacity(64);
    for byte in &Sha256::digest(&bytes) {
        write!(sha256, "{byte:02x}")?;
    }
    Ok(Prepared { bytes, sha256 })
}

/// Replaces every `art_v` token (`art_v[\w.~+/=-]*`) and every token after
/// `Bearer ` up to whitespace. The scheme word is matched in any case, as HTTP allows.
pub fn redact(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut at = 0;
    while let Some(rest) = text.get(at..).filter(|rest| !rest.is_empty()) {
        let bearer = bearer_token_len(rest);
        if bearer > 0 {
            out.extend_from_slice(&rest[..BEARER.len()]);
            out.extend_from_slice(BEARER_REDACTED);
            at += BEARER.len() + bearer;
        } else if let Some(token) = rest.strip_prefix(TOKEN_PREFIX) {
            out.extend_from_slice(TOKEN_REDACTED.as_bytes());
            at += TOKEN_PREFIX.len() + token.iter().take_while(|b| is_token_byte(**b)).count();
        } else {
            out.push(rest[0]);
            at += 1;
        }
    }
    out
}

/// The length of the token after a leading `Bearer `, or 0 when `text` does not start with one.
fn bearer_token_len(text: &[u8]) -> usize {
    match text.get(..BEARER.len()) {
        Some(word) if word.eq_ignore_ascii_case(BEARER) => text[BEARER.len()..]
            .iter()
            .take_while(|b| !b.is_ascii_whitespace())
            .count(),
        Some(_) | None => 0,
    }
}

/// JavaScript's `[\w.~+/=-]`: ASCII letters, digits, `_` and `.~+/=-`.
fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_.~+/=-".contains(&byte)
}

/// Stores the transcript as `transcripts/<claim>.md` in a parentless commit on the branch
/// `tessel/transcript-<claim>` of the repository at `root`, replacing that branch if it exists.
/// Returns the reference to send, for the fork `fork`, and the lines to print.
pub fn store(
    root: &Path,
    prepared: &Prepared,
    claim: u64,
    agent: &str,
    fork: &str,
) -> anyhow::Result<(TranscriptRef, String)> {
    let branch = format!("tessel/transcript-{claim}");
    let file = format!("{claim}.md");
    let blob = git(
        root,
        &["hash-object", "-w", "--no-filters", "--stdin"],
        &prepared.bytes,
    )?;
    let inner = git(
        root,
        &["mktree"],
        format!("100644 blob {blob}\t{file}\n").as_bytes(),
    )?;
    let outer = git(
        root,
        &["mktree"],
        format!("040000 tree {inner}\ttranscripts\n").as_bytes(),
    )?;
    let message = format!("Transcript of claim {claim} by {agent}");
    let commit = git(root, &["commit-tree", &outer, "-m", &message], &[])?;
    let full = format!("refs/heads/{branch}");
    let replaced = ref_exists(root, &full)?;
    git(root, &["update-ref", &full, &commit], &[])?;
    let path = format!("transcripts/{file}");
    let mut text = String::new();
    if replaced {
        writeln!(text, "transcript branch {branch} replaced")?;
    }
    writeln!(
        text,
        "transcript: {branch} holds {path} (sha256 {}…); it stays in this repository, because no \
         transcript copy exists yet (P1-EVIDENCE)",
        &prepared.sha256[..12],
    )?;
    let reference = TranscriptRef {
        repo: fork.to_string(),
        path,
        sha256: prepared.sha256.clone(),
    };
    Ok((reference, text))
}

/// `git show-ref --verify --quiet` exits 0 when the ref exists and 1 when it does not.
fn ref_exists(root: &Path, full: &str) -> anyhow::Result<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show-ref", "--verify", "--quiet", full])
        .status()
        .context("cannot run git; is git installed?")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(_) | None => bail!("git show-ref {full} failed while storing the transcript"),
    }
}

/// Runs git in `root` with `input` on stdin and returns its trimmed stdout.
fn git(root: &Path, args: &[&str], input: &[u8]) -> anyhow::Result<String> {
    let what = args.first().copied().unwrap_or_default();
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run git; is git installed?")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .with_context(|| format!("cannot write the transcript to git {what}"))?;
    }
    let output = child
        .wait_with_output()
        .with_context(|| format!("git {what} did not finish"))?;
    if !output.status.success() {
        bail!(
            "git {what} failed while storing the transcript: {}",
            escape(String::from_utf8_lossy(&output.stderr).trim())
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redacted(text: &str) -> String {
        String::from_utf8(redact(text.as_bytes())).unwrap_or_default()
    }

    #[test]
    fn art_v_and_bearer_tokens_are_redacted() {
        assert_eq!(redacted("x art_vAB.c~+/=-_9 y"), "x art_v…[redacted] y");
        assert_eq!(redacted("art_v"), "art_v…[redacted]");
        assert_eq!(redacted("art_vAB,cd"), "art_v…[redacted],cd");
    }

    #[test]
    fn the_token_after_bearer_is_redacted_up_to_whitespace() {
        assert_eq!(
            redacted("Authorization: Bearer abc.def\tnext"),
            "Authorization: Bearer [redacted]\tnext"
        );
        assert_eq!(
            redacted("authorization: bearer abc"),
            "authorization: bearer [redacted]"
        );
        assert_eq!(redacted("Bearer \nBearer"), "Bearer \nBearer");
    }

    #[test]
    fn text_without_secrets_is_unchanged() {
        let text = "plain text, with ünïcödé and art_ and Bearer";
        assert_eq!(redacted(text), text);
    }
}
