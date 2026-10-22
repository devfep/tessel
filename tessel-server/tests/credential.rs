//! The askpass hook as git runs it: the real `tessel-server` binary as `GIT_ASKPASS`, answering
//! the prompts of a real `git credential fill` in the lander's environment. No network is used.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tessel_lander::git::Git;
use tessel_server::credential::{with_askpass, ASKPASS_HOSTS_VAR, ASKPASS_TOKEN_FILE_VAR};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BINARY: &str = env!("CARGO_BIN_EXE_tessel-server");
const SECRET: &str = "ghs_not-a-real-token-0123456789";

fn token_file(dir: &Path) -> Result<PathBuf, std::io::Error> {
    let path = dir.join("token");
    std::fs::write(&path, format!("{SECRET}\n"))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(path)
}

const HOST: &str = "github.com";

fn askpass(prompt: &str, token_file: Option<&Path>) -> Result<Output, std::io::Error> {
    let mut command = Command::new(BINARY);
    command.arg(prompt).env_clear().env(ASKPASS_HOSTS_VAR, HOST);
    if let Some(file) = token_file {
        command.env(ASKPASS_TOKEN_FILE_VAR, file);
    }
    command.output()
}

#[test]
fn the_hook_prints_the_user_name_for_a_username_prompt_and_the_token_for_a_password_prompt(
) -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = token_file(dir.path())?;
    let user = askpass("Username for 'https://github.com': ", Some(&file))?;
    assert!(user.status.success());
    assert_eq!(String::from_utf8(user.stdout)?, "x-access-token\n");
    assert_eq!(user.stderr, b"");
    let password = askpass(
        "Password for 'https://x-access-token@github.com': ",
        Some(&file),
    )?;
    assert!(password.status.success());
    assert_eq!(String::from_utf8(password.stdout)?, format!("{SECRET}\n"));
    assert_eq!(password.stderr, b"", "nothing is logged beside the answer");
    Ok(())
}

#[test]
fn the_hook_prints_nothing_and_fails_for_a_prompt_that_asks_for_neither_user_nor_password(
) -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = token_file(dir.path())?;
    let cases = [(
        "Enter passphrase for key '/home/x/.ssh/id': ",
        Some(file.as_path()),
    )];
    for (prompt, variable) in cases {
        let output = askpass(prompt, variable)?;
        assert_eq!(output.status.code(), Some(1), "{prompt}");
        assert_eq!(output.stdout, b"", "{prompt}");
        assert_eq!(output.stderr, b"", "{prompt}");
    }
    Ok(())
}

#[test]
fn the_hook_prints_nothing_and_fails_for_a_prompt_naming_another_host() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = token_file(dir.path())?;
    for prompt in [
        "Username for 'https://evil.example': ",
        "Password for 'https://x-access-token@evil.example': ",
    ] {
        let output = askpass(prompt, Some(&file))?;
        assert_eq!(output.status.code(), Some(1), "{prompt}");
        assert_eq!(
            (output.stdout, output.stderr),
            (Vec::new(), Vec::new()),
            "{prompt}"
        );
    }
    Ok(())
}

#[test]
fn without_the_variable_the_binary_is_no_hook_and_prints_no_credential() -> TestResult {
    for prompt in [
        "Username for 'https://github.com': ",
        "Password for 'https://github.com': ",
    ] {
        let output = askpass(prompt, None)?;
        assert!(!output.status.success(), "{prompt}");
        assert_eq!(output.stdout, b"", "{prompt}");
    }
    Ok(())
}

#[test]
fn the_hook_fails_quietly_when_the_token_file_is_no_longer_private() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = token_file(dir.path())?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))?;
    let output = askpass("Password for 'https://github.com': ", Some(&file))?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!((output.stdout, output.stderr), (Vec::new(), Vec::new()));
    Ok(())
}

fn credential_fill(git: &Git, host: &str) -> Result<Output, std::io::Error> {
    let mut child = Command::new("git")
        .args(["credential", "fill"])
        .env_clear()
        .envs(git.env())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or(std::io::ErrorKind::BrokenPipe)?;
    stdin.write_all(format!("protocol=https\nhost={host}\n\n").as_bytes())?;
    drop(stdin);
    child.wait_with_output()
}

fn lander_git(dir: &Path) -> Result<Git, Box<dyn std::error::Error>> {
    Ok(Git::create_in(&dir.join("git"))?)
}

fn configured_git(dir: &Path, file: &Path) -> Result<Git, Box<dyn std::error::Error>> {
    let hosts = ["git.example.invalid".to_owned()];
    Ok(with_askpass(
        lander_git(dir)?,
        Path::new(BINARY),
        file,
        &hosts,
    ))
}

#[test]
fn git_gets_no_credential_for_a_host_the_landing_was_not_configured_for() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = token_file(dir.path())?;
    let git = configured_git(dir.path(), &file)?;
    for host in [
        "other.invalid",
        "git.example.invalid.evil.invalid",
        "evilgit.example.invalid",
    ] {
        let output = credential_fill(&git, host)?;
        assert!(!output.status.success(), "the hook refused {host}");
        let stdout = String::from_utf8(output.stdout)?;
        assert!(!stdout.contains(SECRET), "{host}: {stdout}");
        assert!(!stdout.contains("password="), "{host}: {stdout}");
    }
    Ok(())
}

#[test]
fn git_asks_the_hook_and_gets_the_user_name_and_the_token() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = token_file(dir.path())?;
    let git = configured_git(dir.path(), &file)?;
    let output = credential_fill(&git, "git.example.invalid")?;
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.lines().any(|line| line == "username=x-access-token"),
        "{stdout}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == format!("password={SECRET}")),
        "{stdout}"
    );
    Ok(())
}

#[test]
fn without_a_token_file_git_finds_no_credential_and_does_not_prompt() -> TestResult {
    let dir = tempfile::tempdir()?;
    let output = credential_fill(&lander_git(dir.path())?, "git.example.invalid")?;
    assert!(!output.status.success(), "no credential, so the fill fails");
    let stdout = String::from_utf8(output.stdout)?;
    assert!(!stdout.contains("password="), "{stdout}");
    Ok(())
}
