//! Read fixed scopes on separate read-only SQLite connections. Each worker holds
//! one snapshot for all its scopes. All snapshots are pinned before any scope is
//! read, but commits can land between workers' snapshots. On a database with no
//! concurrent changes, the ordered result matches a single connection's read.
//! The fan-out never writes.
//!
//! The reader trait ties its prepared state to the connection's lifetime so it
//! can hold SQLite statements and reuse them for every scope, without sharing
//! either the state or the connection between workers.
//! Each reader decides whether its worker pinned the required snapshot. The
//! result reaches that worker's `prepare` after the barrier; a mismatch can
//! be marked in the output so the caller can reread those scopes.

use crate::source::SourceError;
use rusqlite::{Connection, Transaction};
use std::{
    path::Path,
    sync::{Mutex, atomic::{AtomicUsize, Ordering}, mpsc},
    thread,
};

pub const PIN_SNAPSHOT: &str = "SELECT EXISTS(SELECT 1 FROM sqlite_schema)";

pub fn configured_workers() -> usize {
    let available = thread::available_parallelism().map_or(1, |n| n.get());
    worker_count(std::env::var("T3_AGENTS_READ_THREADS").ok().as_deref(), available)
}

fn worker_count(value: Option<&str>, available: usize) -> usize {
    value.and_then(|value| value.parse().ok())
        .filter(|n| (1..=16).contains(n))
        .unwrap_or_else(|| available.clamp(1, 4))
}

pub trait Reader: Sync {
    type Scope: Sync;
    type Output: Send;
    type Prepared<'db>;

    /// Once per worker, on its own read-only connection in autocommit. Pin
    /// the transaction before the barrier releases any scope reads.
    fn pin<'db>(&self, db: &'db Connection, _path: &Path) -> rusqlite::Result<(Transaction<'db>, bool)> {
        let tx = db.unchecked_transaction()?;
        // BEGIN is deferred. A read of this database pins its snapshot.
        tx.query_row(PIN_SNAPSHOT, [], |_| Ok(()))?;
        Ok((tx, true))
    }

    fn prepare<'db>(&self, db: &'db Connection, pinned: bool) -> rusqlite::Result<Self::Prepared<'db>>;
    fn read(&self, prepared: &mut Self::Prepared<'_>, scope: &Self::Scope) -> rusqlite::Result<Self::Output>;
}

#[derive(Debug)]
pub enum Error {
    Open(SourceError),
    Sqlite(rusqlite::Error),
    Spawn(std::io::Error),
}

/// `open` must return a read-only connection. `workers` must be at least one.
/// Errors from started workers are checked in worker creation order, after which
/// the thread scope joins any remaining workers before returning.
pub fn read_scopes<R: Reader>(
    path: &Path,
    workers: usize,
    scopes: &[R::Scope],
    open: impl Fn(&Path) -> Result<Connection, SourceError> + Sync,
    reader: &R,
) -> Result<Vec<R::Output>, Error> {
    assert!(workers > 0, "fan-out needs at least one worker");
    let next = AtomicUsize::new(0);
    let start_reads = Mutex::new(false);
    let mut batches = thread::scope(|pool| {
        let mut start = start_reads.lock().unwrap();
        let (ready, arrivals) = mpsc::channel();
        let mut jobs = Vec::with_capacity(workers);
        let (open, next, start_reads) = (&open, &next, &start_reads);
        for _ in 0..workers {
            let ready = ready.clone();
            let job = thread::Builder::new().spawn_scoped(pool, move || {
                let db = open(path).map_err(Error::Open)?;
                let (tx, pinned) = reader.pin(&db, path).map_err(Error::Sqlite)?;
                let _ = ready.send(());
                drop(ready);

                let mut batches = Vec::new();
                let start = *start_reads.lock().unwrap();
                if start {
                    let mut prepared = reader.prepare(&tx, pinned).map_err(Error::Sqlite)?;
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(scope) = scopes.get(index) else { break };
                        let result = reader.read(&mut prepared, scope).map_err(Error::Sqlite)?;
                        batches.push((index, result));
                    }
                }
                tx.rollback().map_err(Error::Sqlite)?;
                Ok(batches)
            }).map_err(Error::Spawn)?;
            jobs.push(job);
        }
        drop(ready);
        // A failed startup drops its sender too. Release the barrier in either case;
        // only start reading when all snapshots are ready.
        *start = arrivals.into_iter().count() == workers;
        drop(start);
        let mut batches = Vec::with_capacity(scopes.len());
        for job in jobs {
            batches.extend(job.join().unwrap()?);
        }
        Ok(batches)
    })?;
    batches.sort_unstable_by_key(|(index, _)| *index);
    Ok(batches.into_iter().map(|(_, result)| result).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{OpenFlags, Statement};
    use std::{fs, path::PathBuf};

    #[test]
    fn worker_count_switch() {
        for (available, default) in [(0, 1), (1, 1), (2, 2), (64, 4)] {
            for value in [None, Some(""), Some("0"), Some("17"), Some("x"), Some("2.0"), Some(" 2"), Some("-1")] {
                assert_eq!(worker_count(value, available), default, "{value:?}, {available} processors");
            }
            for workers in 1..=16 {
                assert_eq!(worker_count(Some(&workers.to_string()), available), workers);
            }
        }
    }

    struct Fixture { dir: PathBuf, path: PathBuf }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!("t3-agents-fanout-{}-{}",
                std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&dir).unwrap();
            let path = dir.join("rows.sqlite");
            let db = Connection::open(&path).unwrap();
            db.execute_batch("CREATE TABLE items (key INTEGER NOT NULL); BEGIN").unwrap();
            for key in 1..=24 {
                for _ in 0..key {
                    db.execute("INSERT INTO items VALUES (?1)", [key]).unwrap();
                }
            }
            db.execute_batch("COMMIT").unwrap();
            Self { dir, path }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // A WAL database keeps its log and index after read-only connections close.
            for side in ["-wal", "-shm"] {
                let mut name = self.path.clone().into_os_string();
                name.push(side);
                let _ = fs::remove_file(name);
            }
            fs::remove_file(&self.path).unwrap();
            fs::remove_dir(&self.dir).unwrap();
        }
    }

    fn open(path: &Path) -> Result<Connection, SourceError> {
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| SourceError {
            kind: "unreadable", path: path.to_owned(), detail: e.to_string(),
        })
    }

    #[derive(Default)]
    struct Count { prepares: AtomicUsize }

    impl Reader for Count {
        type Scope = i64;
        type Output = i64;
        type Prepared<'db> = Statement<'db>;

        fn prepare<'db>(&self, db: &'db Connection, _pinned: bool) -> rusqlite::Result<Self::Prepared<'db>> {
            self.prepares.fetch_add(1, Ordering::Relaxed);
            db.prepare("SELECT count(*) FROM items WHERE key=?1 HAVING count(*) > 0")
        }

        fn read(&self, prepared: &mut Self::Prepared<'_>, key: &i64) -> rusqlite::Result<i64> {
            prepared.query_row([key], |row| row.get(0))
        }
    }

    #[test]
    fn ordered_read_only_results_and_one_prepare_per_worker() {
        let fixture = Fixture::new();
        let before = fs::read(&fixture.path).unwrap();
        let keys: Vec<_> = (1..=24).rev().collect();
        for workers in [1, 2, 3, 8, 16] {
            for scopes in [&keys[..], &keys[..2], &keys[..1], &keys[..0]] {
                let reader = Count::default();
                assert_eq!(read_scopes(&fixture.path, workers, scopes, open, &reader).unwrap(), scopes);
                assert_eq!(reader.prepares.load(Ordering::Relaxed), workers);
            }
        }
        assert_eq!(fs::read(&fixture.path).unwrap(), before);
    }

    #[test]
    fn scope_error_keeps_its_cause_and_leaves_file_readable() {
        let fixture = Fixture::new();
        let before = fs::read(&fixture.path).unwrap();
        let reader = Count::default();
        let error = read_scopes(&fixture.path, 3, &[4, 0, 7], open, &reader).unwrap_err();
        assert!(matches!(error, Error::Sqlite(rusqlite::Error::QueryReturnedNoRows)), "{error:?}");
        assert_eq!(read_scopes(&fixture.path, 1, &[7, 4], open, &reader).unwrap(), [7, 4]);
        assert_eq!(fs::read(&fixture.path).unwrap(), before);
    }

    struct PinReader {
        entered: AtomicUsize,
        proofs: Mutex<std::collections::HashMap<thread::ThreadId, bool>>,
        count: Count,
        first_reads: std::sync::Barrier,
        workers: usize,
        fail_first: bool,
    }

    impl PinReader {
        fn new(workers: usize, fail_first: bool) -> Self {
            Self { entered: AtomicUsize::new(0), proofs: Mutex::default(), count: Count::default(),
                first_reads: std::sync::Barrier::new(workers), workers, fail_first }
        }
    }

    struct PinPrepared<'db> {
        query: Statement<'db>,
        received: bool,
        expected: bool,
        first: bool,
    }

    impl Reader for PinReader {
        type Scope = i64;
        type Output = (i64, bool, bool);
        type Prepared<'db> = PinPrepared<'db>;

        fn pin<'db>(&self, db: &'db Connection, _: &Path) -> rusqlite::Result<(Transaction<'db>, bool)> {
            assert!(db.is_autocommit() && db.is_readonly("main")?);
            let index = self.entered.fetch_add(1, Ordering::Relaxed);
            let tx = db.unchecked_transaction()?;
            tx.query_row(PIN_SNAPSHOT, [], |_| Ok(()))?;
            if self.fail_first && index == 0 { return Err(rusqlite::Error::InvalidQuery); }
            let pinned = index % 2 == 0;
            self.proofs.lock().unwrap().insert(thread::current().id(), pinned);
            Ok((tx, pinned))
        }

        fn prepare<'db>(&self, db: &'db Connection, pinned: bool) -> rusqlite::Result<Self::Prepared<'db>> {
            let proofs = self.proofs.lock().unwrap();
            assert_eq!(proofs.len(), self.workers);
            Ok(PinPrepared { query: self.count.prepare(db, pinned)?, received: pinned,
                expected: proofs[&thread::current().id()], first: true })
        }

        fn read(&self, prepared: &mut Self::Prepared<'_>, key: &i64) -> rusqlite::Result<Self::Output> {
            if std::mem::take(&mut prepared.first) { self.first_reads.wait(); }
            let count = self.count.read(&mut prepared.query, key)?;
            Ok((count, prepared.received, prepared.expected))
        }
    }

    #[test]
    fn pin_hook_finishes_before_prepare_and_preserves_each_proof() {
        let fixture = Fixture::new();
        let workers = 4;
        let reader = PinReader::new(workers, false);
        let keys = [4, 1, 3, 2];
        let rows = read_scopes(&fixture.path, workers, &keys, open, &reader).unwrap();
        assert_eq!(reader.entered.load(Ordering::Relaxed), workers);
        assert_eq!(reader.count.prepares.load(Ordering::Relaxed), workers);
        assert_eq!(rows.iter().map(|row| row.0).collect::<Vec<_>>(), keys);
        assert!(rows.iter().all(|&(_, received, expected)| received == expected), "{rows:?}");
        assert_eq!(rows.iter().filter(|row| row.1).count(), workers / 2);
    }

    #[test]
    fn pin_error_stops_all_reads_and_releases_transactions() {
        let fixture = Fixture::new();
        let before = fs::read(&fixture.path).unwrap();
        let reader = PinReader::new(4, true);
        let error = read_scopes(&fixture.path, 4, &[4, 1, 3, 2], open, &reader).unwrap_err();
        assert!(matches!(error, Error::Sqlite(rusqlite::Error::InvalidQuery)), "{error:?}");
        assert_eq!(reader.entered.load(Ordering::Relaxed), 4);
        assert_eq!(reader.count.prepares.load(Ordering::Relaxed), 0);
        assert_eq!(fs::read(&fixture.path).unwrap(), before);
        let db = Connection::open(&fixture.path).unwrap();
        db.execute_batch("BEGIN EXCLUSIVE; INSERT INTO items VALUES (1); ROLLBACK").unwrap();
        assert_eq!(fs::read(&fixture.path).unwrap(), before);
        assert_eq!(read_scopes(&fixture.path, 1, &[1], open, &Count::default()).unwrap(), [1]);
    }

    /// The workers close their connections together, as every `--tools` read does. On a WAL
    /// database SQLite 3.51.1 could deadlock there (see `source::open_read_only`): this
    /// failed within a few hundred reads before the fix. A hang fails the test instead.
    #[test]
    fn workers_closing_together_on_a_wal_database_never_hang() {
        let fixture = Fixture::new();
        Connection::open(&fixture.path).unwrap()
            .query_row("PRAGMA journal_mode=WAL", [], |_| Ok(())).unwrap();
        let path = fixture.path.clone();
        let (done, finished) = mpsc::channel();
        // Not a scoped thread: a deadlocked worker must not keep the test from failing.
        thread::spawn(move || {
            for _ in 0..2000 {
                let read = read_scopes(&path, 4, &[1, 2, 3, 4], crate::recap::open, &Count::default());
                assert_eq!(read.unwrap(), [1, 2, 3, 4]);
            }
            let _ = done.send(());
        });
        match finished.recv_timeout(std::time::Duration::from_secs(60)) {
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("fan-out workers hung while closing"),
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the reads failed"),
            Ok(()) => {}
        }
    }
}
