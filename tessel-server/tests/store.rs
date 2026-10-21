//! The store: durability, one transaction per commit, the version chain with a fixture per
//! shape, a newer shape refused, and the exclusive lock on the directory.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

use std::path::{Path, PathBuf};

use tessel_coordinator::protocol::EventKind;
use tessel_coordinator::shell::{event_key, load_core, STATE_KEY};
use tessel_server::id::RepoId;
use tessel_server::store::{Store, StoreDir, StoreError, STATE_VERSION};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Every shape that ever shipped, with the file that holds it. The files are frozen: each was
/// written by the store of its version from a test-kit session of two agents with one claim
/// each, and is never regenerated. A new migration adds a line and a file here.
const FIXTURES: &[(&str, u32)] = &[("state-v1.sqlite", 1)];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn event_json(seq: u64) -> String {
    format!(r#"{{"seq":{seq},"at_ms":{seq},"run":"r","event":"agent_connected","agent":"a{seq}"}}"#)
}

fn entry(seq: u64) -> (String, String) {
    (event_key(seq), event_json(seq))
}

#[test]
fn every_shape_that_shipped_has_a_fixture_that_opens_and_reads_back() -> TestResult {
    let versions: Vec<u32> = FIXTURES.iter().map(|(_, version)| *version).collect();
    let expected: Vec<u32> = (1..=STATE_VERSION).collect();
    assert_eq!(versions, expected, "one fixture per version, in order");
    for (name, version) in FIXTURES {
        let dir = tempfile::tempdir()?;
        let copy = dir.path().join(name);
        std::fs::copy(fixture_dir().join(name), &copy)?;
        let store = Store::open(&copy)?;
        assert_eq!(
            store.version()?,
            STATE_VERSION,
            "{name} migrates up to this build"
        );
        let state = store.load()?.ok_or("the fixture has state")?;
        load_core(Some(&state), None, None, Vec::new(), Vec::new())
            .map_err(|e| format!("{name} state does not load: {e:?}"))?;
        let json: serde_json::Value = serde_json::from_str(&state)?;
        assert_eq!(
            json["claims"].as_object().map(serde_json::Map::len),
            Some(2),
            "{name}"
        );
        let events = store.read_events(0, 100)?;
        let seqs: Vec<u64> = events.iter().map(|event| event.seq).collect();
        assert_eq!(seqs, [0, 1, 2, 3], "{name} v{version}");
        assert!(matches!(events[0].kind, EventKind::AgentConnected { .. }));
    }
    Ok(())
}

#[test]
fn a_fixture_file_is_in_the_list_or_it_is_not_a_fixture() -> TestResult {
    let mut found = Vec::new();
    for item in std::fs::read_dir(fixture_dir())? {
        found.push(item?.file_name().to_string_lossy().into_owned());
    }
    found.sort();
    let listed: Vec<&str> = FIXTURES.iter().map(|(name, _)| *name).collect();
    assert_eq!(found, listed);
    Ok(())
}

#[test]
fn a_store_with_a_newer_version_than_this_build_is_refused_and_left_alone() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("s.sqlite");
    Store::open(&file)?;
    let newer = STATE_VERSION + 1;
    rusqlite::Connection::open(&file)?.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'state_version'",
        [newer.to_string()],
    )?;
    for result in [Store::open(&file), Store::open_read_only(&file)] {
        let error = result.expect_err("a newer shape is refused");
        assert!(
            matches!(error, StoreError::Newer { found, known, .. }
                if found == newer && known == STATE_VERSION),
            "{error}"
        );
        assert!(error.to_string().contains("migrate it down"), "{error}");
    }
    let kept: String = rusqlite::Connection::open(&file)?.query_row(
        "SELECT value FROM meta WHERE key = 'state_version'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(kept, newer.to_string(), "the file was not migrated down");
    Ok(())
}

#[test]
fn a_file_that_is_not_a_tessel_store_is_refused_untouched() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("other.sqlite");
    rusqlite::Connection::open(&file)?.execute_batch("CREATE TABLE notes (x TEXT)")?;
    let error = Store::open(&file).expect_err("foreign file");
    assert!(matches!(error, StoreError::Foreign { .. }), "{error}");
    Ok(())
}

#[test]
fn a_commit_is_durable_for_a_reader_and_after_a_reopen() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("s.sqlite");
    let mut store = Store::open(&file)?;
    assert_eq!(store.load()?, None);
    assert!(!store.has_state()?);
    store.commit(&[
        (STATE_KEY.to_string(), "{}".to_string()),
        entry(0),
        entry(1),
    ])?;
    let reader = Store::open_read_only(&file)?;
    assert_eq!(reader.load()?.as_deref(), Some("{}"));
    assert_eq!(
        reader.read_events(0, 10)?.len(),
        2,
        "a reader sees it while the writer is open"
    );
    drop(store);
    let reopened = Store::open(&file)?;
    assert!(reopened.has_state()?);
    assert_eq!(reopened.read_events(0, 10)?.len(), 2);
    Ok(())
}

#[test]
fn a_commit_replaces_the_state_and_appends_events() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut store = Store::open(&dir.path().join("s.sqlite"))?;
    store.commit(&[(STATE_KEY.to_string(), "1".to_string()), entry(0)])?;
    store.commit(&[(STATE_KEY.to_string(), "2".to_string()), entry(1)])?;
    assert_eq!(store.load()?.as_deref(), Some("2"));
    assert_eq!(store.read_events(0, 10)?.len(), 2);
    store.commit(&[])?;
    assert_eq!(store.load()?.as_deref(), Some("2"));
    Ok(())
}

#[test]
fn events_are_read_in_seq_order_from_a_seq_and_up_to_a_limit() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut store = Store::open(&dir.path().join("s.sqlite"))?;
    let entries: Vec<_> = [10, 2, 1, 0, 11].into_iter().map(entry).collect();
    store.commit(&entries)?;
    store.commit(&[(STATE_KEY.to_string(), "{}".to_string())])?;
    let seqs = |from, limit| -> Result<Vec<u64>, StoreError> {
        Ok(store
            .read_events(from, limit)?
            .iter()
            .map(|e| e.seq)
            .collect())
    };
    assert_eq!(
        seqs(0, 100)?,
        [0, 1, 2, 10, 11],
        "numeric order, not text order"
    );
    assert_eq!(seqs(2, 100)?, [2, 10, 11]);
    assert_eq!(seqs(3, 2)?, [10, 11]);
    assert_eq!(seqs(0, 2)?, [0, 1]);
    assert_eq!(seqs(12, 5)?, Vec::<u64>::new());
    assert_eq!(seqs(0, 0)?, Vec::<u64>::new());
    Ok(())
}

#[test]
fn an_event_that_does_not_parse_fails_the_read_and_names_its_key() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut store = Store::open(&dir.path().join("s.sqlite"))?;
    store.commit(&[(event_key(0), "not json".to_string())])?;
    let error = store.read_events(0, 10).expect_err("corrupt event");
    assert!(matches!(error, StoreError::CorruptEvent { .. }), "{error}");
    Ok(())
}

#[test]
fn a_commit_is_one_transaction_all_or_nothing() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut store = Store::open(&dir.path().join("s.sqlite"))?;
    let too_long = "k".repeat(300);
    let error = store
        .commit(&[entry(0), (too_long, "x".to_string())])
        .expect_err("a key over the limit fails the commit");
    assert!(
        matches!(
            error,
            StoreError::Sql {
                operation: "commit",
                ..
            }
        ),
        "{error}"
    );
    assert_eq!(
        store.read_events(0, 10)?.len(),
        0,
        "the first entry was not stored"
    );
    store.commit(&[entry(0)])?;
    assert_eq!(store.read_events(0, 10)?.len(), 1, "the store still works");
    Ok(())
}

#[test]
fn a_store_directory_has_one_holder_and_the_refusal_names_it() -> TestResult {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("stores");
    let first = StoreDir::open(&root)?;
    let error = StoreDir::open(&root).expect_err("a second holder is refused");
    assert!(matches!(error, StoreError::Locked { .. }), "{error}");
    let message = error.to_string();
    assert!(
        message.contains(&std::process::id().to_string()),
        "pid: {message}"
    );
    assert!(
        message.contains(root.to_str().unwrap_or_default()),
        "path: {message}"
    );
    drop(first);
    StoreDir::open(&root)?;
    Ok(())
}

#[test]
fn repositories_of_one_directory_get_files_of_their_own() -> TestResult {
    let dir = tempfile::tempdir()?;
    let stores = StoreDir::open(dir.path())?;
    let a = RepoId::new("acme", "demo")?;
    let b = RepoId::new("other", "demo")?;
    let mut store_a = stores.open_repo(&a)?;
    let store_b = stores.open_repo(&b)?;
    store_a.commit(&[(STATE_KEY.to_string(), "a".to_string())])?;
    assert_eq!(store_b.load()?, None);
    assert!(stores.repo_file(&a).is_file());
    assert_ne!(stores.repo_file(&a), stores.repo_file(&b));
    Ok(())
}

/// The tables, indexes and their SQL, in a fixed order.
fn schema_of(file: &Path) -> Result<Vec<(String, String, String)>, rusqlite::Error> {
    let conn =
        rusqlite::Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut read = conn
        .prepare("SELECT type, name, coalesce(sql, '') FROM sqlite_master ORDER BY type, name")?;
    let rows = read.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    rows.collect()
}

#[test]
fn a_fresh_store_has_the_schema_of_the_newest_fixture() -> TestResult {
    let (newest, _) = FIXTURES.last().ok_or("no fixture")?;
    let dir = tempfile::tempdir()?;
    let fresh = dir.path().join("fresh.sqlite");
    drop(Store::open(&fresh)?);
    let frozen = dir.path().join(newest);
    std::fs::copy(fixture_dir().join(newest), &frozen)?;
    assert_eq!(
        schema_of(&fresh)?,
        schema_of(&frozen)?,
        "the schema changed without a new state version and a new fixture"
    );
    Ok(())
}
