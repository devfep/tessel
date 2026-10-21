//! `tessel-runner [--config <file>]`: reads one request as JSON on standard input, runs it, and
//! writes one result as JSON on standard output.
//!
//! Exit codes: 0 when a run completed (pass or fail is in the result), 2 when it refused before
//! anything ran (the configuration, the user or the request), 3 when the run could not be
//! supervised. A refusal writes one fixed line to standard error and nothing to standard output.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use tessel_runner::{
    run, RequestRefusal, RunRequest, RunnerConfig, RunnerError, CONFIG_FILE_NAME, MAX_REQUEST_BYTES,
};

const USAGE: &str = "usage: tessel-runner [--config <file>] < request.json > result.json";
const EXIT_REFUSED: u8 = 2;
const EXIT_UNSUPERVISED: u8 = 3;

enum Failure {
    Usage,
    Runner(RunnerError),
    ResultNotWritten,
}

impl Failure {
    fn exit_code(&self) -> u8 {
        match self {
            Failure::Usage => EXIT_REFUSED,
            Failure::Runner(error) => error.exit_code(),
            Failure::ResultNotWritten => EXIT_UNSUPERVISED,
        }
    }

    fn message(&self) -> String {
        match self {
            Failure::Usage => USAGE.to_owned(),
            Failure::Runner(error) => format!("tessel-runner: {error}"),
            Failure::ResultNotWritten => "tessel-runner: cannot write the result".to_owned(),
        }
    }
}

fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            // Nothing is left to tell if standard error is closed too.
            writeln!(io::stderr(), "{}", failure.message()).ok();
            ExitCode::from(failure.exit_code())
        }
    }
}

fn execute() -> Result<(), Failure> {
    let path = config_path(&std::env::args().skip(1).collect::<Vec<_>>())?;
    let config = RunnerConfig::load(&path).map_err(Failure::Runner)?;
    let request = read_request().map_err(Failure::Runner)?;
    let result = run(&config, &request).map_err(Failure::Runner)?;
    let line = serde_json::to_string(&result).map_err(|_| Failure::ResultNotWritten)?;
    writeln!(io::stdout(), "{line}").map_err(|_| Failure::ResultNotWritten)
}

fn config_path(args: &[String]) -> Result<PathBuf, Failure> {
    match args {
        [] => Ok(PathBuf::from(CONFIG_FILE_NAME)),
        [flag, path] if flag == "--config" => Ok(PathBuf::from(path)),
        _ => Err(Failure::Usage),
    }
}

fn read_request() -> Result<RunRequest, RunnerError> {
    let limit = u64::try_from(MAX_REQUEST_BYTES + 1).unwrap_or(u64::MAX);
    let mut text = String::new();
    io::stdin()
        .take(limit)
        .read_to_string(&mut text)
        .map_err(|_| RunnerError::Request(RequestRefusal::Malformed))?;
    RunRequest::from_json(&text)
}
