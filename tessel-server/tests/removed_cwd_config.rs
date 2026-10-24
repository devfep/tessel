//! `store_dir` and `runner_config` given as relative paths still work after the directory the
//! server started in is removed. This file holds one test because it changes the working
//! directory of its whole process.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

#[expect(dead_code, reason = "this file uses a part of the shared fixtures")]
mod support;

use std::path::Path;

use tessel_server::config::Config;
use tessel_server::landing::LocalRunner;
use tessel_server::store::StoreDir;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn relative_store_and_runner_paths_work_after_the_start_directory_is_removed() -> TestResult {
    let repos = support::Repos::new();
    repos.write_runner_config(0o600);
    let start = repos.root.path().join("start");
    std::fs::create_dir(&start)?;
    std::env::set_current_dir(&start)?;
    let mut config: Config = toml::from_str(
        "listen = \"127.0.0.1:0\"\nstore_dir = \"../stores\"\n\
         [[repository]]\ntenant = \"acme\"\nname = \"r\"\nrun = \"t\"\n",
    )?;
    config.lander = Some(repos.lander());
    if let Some(lander) = config.lander.as_mut() {
        lander.runner_config = Path::new("..").join("runner.toml");
    }

    let resolved = config.with_absolute_paths()?;
    std::fs::remove_dir(&start)?;

    let runner_config = resolved
        .lander
        .map(|lander| lander.runner_config)
        .ok_or("no lander")?;
    assert!(runner_config.is_absolute(), "{}", runner_config.display());
    assert!(resolved.store_dir.is_absolute());
    LocalRunner::new(runner_config)?;
    StoreDir::open(&resolved.store_dir)?;
    Ok(())
}
