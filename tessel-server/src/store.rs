//! The durable store: one SQLite file per repository, and the exclusive lock on the directory
//! that holds them.
//!
//! A store keeps what the host asks of storage and nothing more, as the test kit's does: the core
//! state and the event log, both as (key, JSON) pairs under the keys `shell` names. The host never
//! asks to store a session (`Output::SetSession`), so no session is kept.
//!
//! Durability (C1). The file is in WAL mode with `synchronous=FULL`, so a commit that returned is
//! on disk. A commit is one transaction, and the adapter answers the host only after it returns.
//!
//! One writer (C2). The directory is locked with an advisory `flock` on `tessel-server.lock`,
//! which holds the holder's pid. SQLite's own exclusive locking mode would also refuse a second
//! writer, but it cannot say who holds the file, and it would lock out the read-only connections
//! that serve the summary. `flock` is released by the operating system when the process dies, so
//! a crash leaves no stale lock to clean up.
//!
//! State version (O1). `meta.state_version` says which shape the file has. `MIGRATIONS` is the
//! chain from one shape to the next; a file with a version this build does not know is refused,
//! never migrated down.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use tessel_coordinator::protocol::Event;
use tessel_coordinator::shell::{event_key, STATE_KEY};

use crate::id::RepoId;

/// The shape this build writes: the newest one in `MIGRATIONS`.
pub const STATE_VERSION: u32 = 1;

/// The lock file in the store directory.
const LOCK_FILE: &str = "tessel-server.lock";

/// How long a statement waits for a lock held by another connection of this process.
const BUSY_TIMEOUT_MS: u64 = 5_000;

/// The longest key a commit may store. The host's keys are `core` and `ev:` plus 20 digits.
const KEY_MAX_BYTES: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot create {path}: {source}")]
    CreateDir {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot use the lock file {path}: {source}")]
    LockFile {
        path: String,
        source: std::io::Error,
    },
    #[error(
        "the store directory {dir} is in use by tessel-server pid {pid} (lock file {lock}); stop \
         that server or give this one another store directory"
    )]
    Locked {
        dir: String,
        lock: String,
        pid: String,
    },
    #[error("store {path}: {operation} failed: {source}")]
    Sql {
        path: String,
        operation: &'static str,
        source: rusqlite::Error,
    },
    #[error(
        "store {path} has state version {found} and this build knows up to {known}; it will not \
         read a newer shape or migrate it down, so run a newer tessel-server"
    )]
    Newer {
        path: String,
        found: u32,
        known: u32,
    },
    #[error("store {path} is not a tessel-server store: it has tables but no state_version")]
    Foreign { path: String },
    #[error("store {path} has no state_version; it is not initialised, so a reader refuses it")]
    Uninitialised { path: String },
    #[error("store {path}: the event under key {key:?} does not parse: {source}")]
    CorruptEvent {
        path: String,
        key: String,
        source: serde_json::Error,
    },
    #[error(
        "store {path}: the journal mode is {mode:?}, not wal, so commits would not be durable as \
         designed; use a local disk"
    )]
    NotWal { path: String, mode: String },
}

/// The directory of stores, held exclusively for as long as this value lives.
#[derive(Debug)]
pub struct StoreDir {
    root: PathBuf,
    _lock: File,
}

impl StoreDir {
    /// Create `root` if it is missing and take the lock, or say who holds it.
    pub fn open(root: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(root).map_err(|source| StoreError::CreateDir {
            path: root.display().to_string(),
            source,
        })?;
        let lock_path = root.join(LOCK_FILE);
        let lock_error = |source| StoreError::LockFile {
            path: lock_path.display().to_string(),
            source,
        };
        let mut lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(lock_error)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(StoreError::Locked {
                    dir: root.display().to_string(),
                    lock: lock_path.display().to_string(),
                    pid: holder_pid(&mut lock),
                });
            }
            Err(TryLockError::Error(source)) => return Err(lock_error(source)),
        }
        lock.set_len(0).map_err(lock_error)?;
        writeln!(lock, "{}", std::process::id()).map_err(lock_error)?;
        lock.sync_all().map_err(lock_error)?;
        Ok(Self {
            root: root.to_path_buf(),
            _lock: lock,
        })
    }

    /// The store file of `id`.
    pub fn repo_file(&self, id: &RepoId) -> PathBuf {
        id.store_file(&self.root)
    }

    /// Open the store of `id` for writing, creating it and its tenant directory if they are new.
    pub fn open_repo(&self, id: &RepoId) -> Result<Store, StoreError> {
        let file = self.repo_file(id);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::CreateDir {
                path: parent.display().to_string(),
                source,
            })?;
        }
        Store::open(&file)
    }
}

/// The pid the holder wrote, or a stand-in while it has not written yet.
fn holder_pid(lock: &mut File) -> String {
    let mut text = String::new();
    if lock.read_to_string(&mut text).is_err() {
        return "unknown".to_string();
    }
    let pid = text.trim();
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return "unknown (still starting)".to_string();
    }
    pid.to_string()
}

/// A step of the chain: brings a store from the version before it to the version of its position.
type Step = fn(&Transaction<'_>) -> rusqlite::Result<()>;

/// Step `i` brings version `i` to version `i + 1`; version 0 is a file with nothing in it.
const MIGRATIONS: &[Step] = &[first_shape];

/// Version 1: the version record, and the key-value table the host's commits fill.
fn first_shape(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(&format!(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
         CREATE TABLE kv (
             key TEXT PRIMARY KEY CHECK (length(key) <= {KEY_MAX_BYTES}),
             json TEXT NOT NULL
         ) WITHOUT ROWID;"
    ))
}

/// One repository's store.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    /// Open `path` for writing: create it if it is new, check its shape, and migrate it up.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut store = Self::connect(path, flags)?;
        store.refuse_unknown(MIGRATIONS)?;
        store.make_durable()?;
        store.migrate(MIGRATIONS)?;
        Ok(store)
    }

    /// Open `path` to read it while the server writes it: WAL lets a reader run beside the
    /// writer. A shape this build does not know, or a file that was never initialised, is refused.
    pub fn open_read_only(path: &Path) -> Result<Self, StoreError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let store = Self::connect(path, flags)?;
        match store.state_version()? {
            Some(version) if version <= STATE_VERSION => Ok(store),
            Some(found) => Err(store.newer(found, STATE_VERSION)),
            None => Err(StoreError::Uninitialised {
                path: store.path_text(),
            }),
        }
    }

    fn connect(path: &Path, flags: OpenFlags) -> Result<Self, StoreError> {
        let text = path.display().to_string();
        let conn = Connection::open_with_flags(path, flags).map_err(|source| StoreError::Sql {
            path: text.clone(),
            operation: "open",
            source,
        })?;
        let store = Self {
            conn,
            path: path.to_path_buf(),
        };
        store.sql("set the busy timeout", |conn| {
            conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))
        })?;
        Ok(store)
    }

    fn path_text(&self) -> String {
        self.path.display().to_string()
    }

    fn sql<T>(
        &self,
        operation: &'static str,
        run: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, StoreError> {
        run(&self.conn).map_err(|source| StoreError::Sql {
            path: self.path_text(),
            operation,
            source,
        })
    }

    fn newer(&self, found: u32, known: u32) -> StoreError {
        StoreError::Newer {
            path: self.path_text(),
            found,
            known,
        }
    }

    /// WAL and `synchronous=FULL` (C1), each read back: a file that cannot be put in WAL mode
    /// (a network file system, say) is refused rather than run with weaker durability.
    fn make_durable(&self) -> Result<(), StoreError> {
        let mode: String = self.sql("set the journal mode", |conn| {
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        })?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::NotWal {
                path: self.path_text(),
                mode,
            });
        }
        self.sql("set synchronous", |conn| {
            conn.pragma_update(None, "synchronous", "FULL")
        })
    }

    /// The version in the file: `None` for a file with nothing in it.
    fn state_version(&self) -> Result<Option<u32>, StoreError> {
        let tables: i64 = self.sql("read the schema", |conn| {
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
        })?;
        if tables == 0 {
            return Ok(None);
        }
        let has_meta: bool = self.sql("look for meta", |conn| {
            conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'meta')",
                [],
                |row| row.get(0),
            )
        })?;
        if !has_meta {
            return Err(StoreError::Foreign {
                path: self.path_text(),
            });
        }
        let version: Option<String> = self.sql("read state_version", |conn| {
            conn.query_row(
                "SELECT value FROM meta WHERE key = 'state_version'",
                [],
                |row| row.get(0),
            )
            .optional()
        })?;
        match version.map(|text| text.parse::<u32>()) {
            Some(Ok(version)) => Ok(Some(version)),
            Some(Err(_)) | None => Err(StoreError::Foreign {
                path: self.path_text(),
            }),
        }
    }

    /// The version in the file, refused if `steps` do not reach it: a newer shape is not touched.
    fn refuse_unknown(&self, steps: &[Step]) -> Result<u32, StoreError> {
        let known = u32::try_from(steps.len()).unwrap_or(u32::MAX);
        let current = self.state_version()?.unwrap_or(0);
        if current > known {
            return Err(self.newer(current, known));
        }
        Ok(current)
    }

    /// Bring the file up to the last of `steps`, one transaction per step.
    fn migrate(&mut self, steps: &[Step]) -> Result<(), StoreError> {
        let current = self.refuse_unknown(steps)?;
        for (index, step) in steps.iter().enumerate().skip(current as usize) {
            let next = index + 1;
            let path = self.path_text();
            let result = self.conn.transaction().and_then(|tx| {
                step(&tx)?;
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES ('state_version', ?1) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![next.to_string()],
                )?;
                tx.commit()
            });
            result.map_err(|source| StoreError::Sql {
                path,
                operation: "migrate",
                source,
            })?;
        }
        Ok(())
    }

    /// The shape of the file, for a test or a log line.
    pub fn version(&self) -> Result<u32, StoreError> {
        Ok(self.state_version()?.unwrap_or(0))
    }

    /// The stored core state, or `None` for a repository with none.
    pub fn load(&self) -> Result<Option<String>, StoreError> {
        self.sql("load the state", |conn| {
            conn.query_row("SELECT json FROM kv WHERE key = ?1", [STATE_KEY], |row| {
                row.get(0)
            })
            .optional()
        })
    }

    /// Whether the repository has stored state.
    pub fn has_state(&self) -> Result<bool, StoreError> {
        self.sql("look for the state", |conn| {
            conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM kv WHERE key = ?1)",
                [STATE_KEY],
                |row| row.get(0),
            )
        })
    }

    /// Store every entry in one transaction: all of them are durable when this returns, or none.
    pub fn commit(&mut self, entries: &[(String, String)]) -> Result<(), StoreError> {
        let path = self.path_text();
        let fail = |source| StoreError::Sql {
            path: path.clone(),
            operation: "commit",
            source,
        };
        let tx = self.conn.transaction().map_err(fail)?;
        {
            let mut put = tx
                .prepare_cached(
                    "INSERT INTO kv (key, json) VALUES (?1, ?2) \
                     ON CONFLICT(key) DO UPDATE SET json = excluded.json",
                )
                .map_err(fail)?;
            for (key, json) in entries {
                put.execute(params![key, json]).map_err(fail)?;
            }
        }
        tx.commit().map_err(fail)
    }

    /// Up to `limit` stored events with `seq >= from_seq`, in `seq` order. The event keys carry
    /// the `seq` zero-padded, so key order is `seq` order.
    pub fn read_events(&self, from_seq: u64, limit: usize) -> Result<Vec<Event>, StoreError> {
        let rows = self.sql("read events", |conn| {
            let mut read = conn.prepare_cached(
                "SELECT key, json FROM kv WHERE key >= ?1 AND key <= ?2 ORDER BY key LIMIT ?3",
            )?;
            let limit = i64::try_from(limit).unwrap_or(i64::MAX);
            let rows = read.query_map(
                params![event_key(from_seq), event_key(u64::MAX), limit],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })?;
        let mut events = Vec::with_capacity(rows.len());
        for (key, json) in rows {
            let event = serde_json::from_str(&json).map_err(|source| StoreError::CorruptEvent {
                path: self.path_text(),
                key,
                source,
            })?;
            events.push(event);
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory() -> Store {
        Store {
            conn: Connection::open_in_memory().unwrap(),
            path: PathBuf::from("memory"),
        }
    }

    fn add_note_table(tx: &Transaction<'_>) -> rusqlite::Result<()> {
        tx.execute_batch("CREATE TABLE note (text TEXT)")
    }

    fn fails_after_writing(tx: &Transaction<'_>) -> rusqlite::Result<()> {
        tx.execute_batch("CREATE TABLE half (x TEXT)")?;
        tx.execute_batch("this is not sql")
    }

    #[test]
    fn the_newest_step_of_the_chain_is_the_version_this_build_writes() {
        assert_eq!(MIGRATIONS.len(), STATE_VERSION as usize);
    }

    #[test]
    fn a_chain_runs_each_step_once_in_order_and_stops_at_its_end() {
        let mut store = in_memory();
        store.migrate(&[first_shape]).unwrap();
        assert_eq!(store.version().unwrap(), 1);
        store.migrate(&[first_shape, add_note_table]).unwrap();
        assert_eq!(store.version().unwrap(), 2);
        store.migrate(&[first_shape, add_note_table]).unwrap();
        assert_eq!(store.version().unwrap(), 2);
        let tables: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'note'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
    }

    #[test]
    fn a_step_that_fails_leaves_the_file_at_the_version_before_it() {
        let mut store = in_memory();
        store.migrate(&[first_shape]).unwrap();
        let error = store
            .migrate(&[first_shape, fails_after_writing])
            .unwrap_err();
        assert!(
            matches!(
                error,
                StoreError::Sql {
                    operation: "migrate",
                    ..
                }
            ),
            "{error}"
        );
        assert_eq!(store.version().unwrap(), 1);
        let half: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'half'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(half, 0, "the failed step's first statement was rolled back");
    }

    #[test]
    fn a_file_newer_than_the_chain_is_refused_untouched() {
        let mut store = in_memory();
        store.migrate(&[first_shape, add_note_table]).unwrap();
        let error = store.migrate(&[first_shape]).unwrap_err();
        assert!(
            matches!(
                error,
                StoreError::Newer {
                    found: 2,
                    known: 1,
                    ..
                }
            ),
            "{error}"
        );
        assert_eq!(store.version().unwrap(), 2);
    }

    #[test]
    fn a_store_is_wal_and_fully_synchronous() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("s.sqlite")).unwrap();
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let synchronous: i64 = store
            .conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        assert_eq!(synchronous, 2, "FULL is 2");
    }
}
