//! SQLite WAL detection, checkpointing, and consistent snapshots.
//!
//! Three properties of SQLite make this module more careful than it looks:
//!
//! 1. **A checkpoint reports failure in its result row, not as an error.**
//!    `PRAGMA wal_checkpoint(TRUNCATE)` returns `(busy, log, checkpointed)`
//!    and raises nothing when it cannot advance, so `execute_batch` reports
//!    success for a WAL that is still full.  Every checkpoint here inspects
//!    that row instead.
//! 2. **A `BEGIN EXCLUSIVE; COMMIT;` probe releases the lock at the COMMIT.**
//!    It does detect a database another process has open — SQLite fails it
//!    with `database is locked` — but the answer is stale as soon as it is
//!    returned.  Work that must not race with a running `ptm` (replacing
//!    `ptm.db` with a pulled backup) therefore holds an [`ExclusiveLock`]
//!    across the whole operation instead of re-probing.
//! 3. **Committed data may live only in `ptm.db-wal`.**  `std::fs::read` on
//!    `ptm.db` then returns a copy with none of it, so anything that needs a
//!    complete copy asks for [`snapshot_bytes`] rather than reading the file.
//!
//! `busy_timeout` is set on every connection here.  Without it a probe against
//! a busy database fails instantly instead of waiting the way the app's own
//! connections do (`src/data/db.rs` uses 5 s).

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long to wait for a lock before giving up.  Matches `src/data/db.rs`.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

// ── Paths and sizes ───────────────────────────────────────────────────────────

/// `db_path` with `suffix` appended (`ptm.db` → `ptm.db-wal`), without going
/// through `String` so that non-UTF-8 paths survive.
fn with_suffix(db_path: &Path, suffix: &str) -> PathBuf {
    let mut os = db_path.as_os_str().to_owned();
    os.push(suffix);
    PathBuf::from(os)
}

/// Path of the `-wal` sidecar for `db_path`.
pub fn wal_path(db_path: &Path) -> PathBuf {
    with_suffix(db_path, "-wal")
}

/// Path of the `-shm` sidecar for `db_path`.
pub fn shm_path(db_path: &Path) -> PathBuf {
    with_suffix(db_path, "-shm")
}

/// Size of the `-wal` sidecar in bytes (0 when it does not exist).
pub fn wal_size(db_path: &Path) -> u64 {
    std::fs::metadata(wal_path(db_path))
        .map(|m| m.len())
        .unwrap_or(0)
}

// ── Connections ───────────────────────────────────────────────────────────────

fn open(db_path: &Path, flags: OpenFlags) -> Result<Connection> {
    let conn = Connection::open_with_flags(db_path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .with_context(|| format!("Failed to open database: {}", db_path.display()))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .context("Failed to set the SQLite busy timeout")?;
    Ok(conn)
}

/// True when `err` means "someone else is using the database" rather than
/// "something is wrong with the database".
fn is_lock_error(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy) | Some(rusqlite::ErrorCode::DatabaseLocked)
    )
}

// ── Checkpointing ─────────────────────────────────────────────────────────────

/// The `(busy, log, checkpointed)` row returned by
/// `PRAGMA wal_checkpoint(TRUNCATE)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointReport {
    /// Non-zero when the checkpoint could not finish because another
    /// connection was using the WAL.
    pub busy: i32,
    /// Frames still in the WAL afterwards — the number that matters: if it is
    /// not zero, `ptm.db` does not yet contain everything that was committed.
    /// SQLite reports `-1` when the database is not in WAL mode at all.
    pub log_frames: i64,
    /// Frames copied into the database file.
    pub checkpointed: i64,
}

impl CheckpointReport {
    /// True when the database file holds every committed transaction.
    pub fn is_clean(&self) -> bool {
        self.busy == 0 && self.log_frames <= 0
    }
}

fn run_checkpoint(conn: &Connection) -> Result<CheckpointReport> {
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok(CheckpointReport {
            busy: row.get(0)?,
            log_frames: row.get(1)?,
            checkpointed: row.get(2)?,
        })
    })
    .context("PRAGMA wal_checkpoint(TRUNCATE) failed")
}

/// A held EXCLUSIVE lock on a database.
///
/// Acquiring it proves no other process has the database open, and — unlike a
/// `BEGIN EXCLUSIVE; COMMIT;` probe — it keeps holding the lock, so whatever
/// follows cannot race with a `ptm` session that starts in the meantime.
/// Released on drop.
pub struct ExclusiveLock {
    conn: Connection,
    db_path: PathBuf,
}

impl ExclusiveLock {
    /// Try to lock an existing database exclusively.
    ///
    /// `Ok(None)` means another process has the database open and nothing was
    /// changed; `Err` means the database could not be locked for some other
    /// reason (missing file, I/O error, …).
    pub fn acquire(db_path: &Path) -> Result<Option<Self>> {
        Self::acquire_with(db_path, false)
    }

    /// Try to lock `db_path` exclusively, creating it if it does not exist.
    ///
    /// Only for callers that are about to install a database anyway; probing a
    /// database that is not there must fail rather than create one.
    pub fn acquire_or_create(db_path: &Path) -> Result<Option<Self>> {
        Self::acquire_with(db_path, true)
    }

    fn acquire_with(db_path: &Path, create: bool) -> Result<Option<Self>> {
        let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE;
        if create {
            flags |= OpenFlags::SQLITE_OPEN_CREATE;
        }
        let conn = open(db_path, flags)?;

        // EXCLUSIVE locking mode keeps the lock between transactions instead of
        // dropping it at each COMMIT, so the lock outlives the statement below.
        conn.pragma_update(None, "locking_mode", "EXCLUSIVE")
            .context("Failed to set PRAGMA locking_mode=EXCLUSIVE")?;

        match conn.execute_batch("BEGIN EXCLUSIVE; COMMIT;") {
            Ok(()) => Ok(Some(Self {
                conn,
                db_path: db_path.to_path_buf(),
            })),
            Err(e) if is_lock_error(&e) => Ok(None),
            Err(e) => Err(e).with_context(|| {
                format!("Failed to lock database: {}", db_path.display())
            }),
        }
    }

    /// Fold the WAL into the database file.  The caller must check
    /// [`CheckpointReport::is_clean`] — this cannot fail for a busy WAL.
    pub fn checkpoint(&self) -> Result<CheckpointReport> {
        run_checkpoint(&self.conn)
    }

    /// Delete the `-wal` and `-shm` sidecars.
    ///
    /// Only meaningful while holding the lock and after a clean checkpoint:
    /// it is what lets `ptm.db` be replaced without a stale WAL being replayed
    /// into the new file.  SQLite tolerates the removal — the connection is
    /// closed without further access.
    pub fn remove_wal_files(&self) -> Result<()> {
        for path in [wal_path(&self.db_path), shm_path(&self.db_path)] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("Failed to remove {}", path.display()))
                }
            }
        }
        Ok(())
    }
}

// ── Public entry points ───────────────────────────────────────────────────────

/// Inspect the WAL file situation and act accordingly.
///
/// Returns:
///   `Ok(Some(report))` — no other process had the database open; we held the
///                        exclusive lock and ran `PRAGMA wal_checkpoint(TRUNCATE)`.
///                        The lock is released before returning, so the report
///                        is a statement about this moment only.  Callers must
///                        check [`CheckpointReport::is_clean`]: a checkpoint
///                        can leave frames behind and still report success.
///   `Ok(None)`         — another process has the database open; the WAL is
///                        live.  No changes were made.
///   `Err(_)`           — an unexpected error prevented the check.
///
/// Callers should only call this function when `ptm.db-wal` already exists.
pub fn check_and_checkpoint_wal(db_path: &Path) -> Result<Option<CheckpointReport>> {
    match ExclusiveLock::acquire(db_path)? {
        None => Ok(None),
        Some(lock) => Ok(Some(lock.checkpoint()?)),
    }
}

/// Return a consistent copy of the database's committed state.
///
/// `VACUUM INTO` writes a complete, checkpointed database — including frames
/// that are still sitting in `ptm.db-wal` — without blocking a running `ptm`
/// and without copying a live file.  Use this instead of
/// `std::fs::read(ptm.db)` whenever the copy has to be complete; the raw file
/// may be missing everything since the last checkpoint.
pub fn snapshot_bytes(db_path: &Path) -> Result<Vec<u8>> {
    let tmp = temp_sibling(db_path, "snapshot");
    let _ = std::fs::remove_file(&tmp);

    let result = (|| {
        let tmp_str = tmp
            .to_str()
            .with_context(|| format!("Non-UTF-8 snapshot path: {}", tmp.display()))?;
        let conn = open(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.execute("VACUUM INTO ?1", rusqlite::params![tmp_str])
            .with_context(|| format!("VACUUM INTO {} failed", tmp.display()))?;
        drop(conn);
        std::fs::read(&tmp).with_context(|| format!("Failed to read {}", tmp.display()))
    })();

    let _ = std::fs::remove_file(&tmp);
    result
}

/// Verify that `db_path` is a readable SQLite database whose
/// `PRAGMA integrity_check` passes.  Used before installing a downloaded
/// backup over the local database.
pub fn check_integrity(db_path: &Path) -> Result<()> {
    let conn = open(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let verdict: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .with_context(|| format!("Not a readable database: {}", db_path.display()))?;
    if verdict != "ok" {
        bail!(
            "Database '{}' failed integrity_check: {}",
            db_path.display(),
            verdict
        );
    }
    Ok(())
}

/// A path beside `db_path` that no other process is using, for temporary
/// snapshots.  Leftovers from a crash are harmless and overwritten next time.
fn temp_sibling(db_path: &Path, tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    with_suffix(db_path, &format!(".{}-{}-{}", tag, std::process::id(), n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn rows(db_path: &Path, sql: &str) -> i64 {
        let conn = Connection::open(db_path).unwrap();
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// A database with `n` committed rows in `t`, closed cleanly — SQLite
    /// checkpoints the WAL on close, so the main file is complete.
    fn clean_db(dir: &TempDir, n: i64) -> PathBuf {
        let db_path = dir.path().join("ptm.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; \
             CREATE TABLE t (x INTEGER); \
             INSERT INTO t VALUES (1);",
        )
        .unwrap();
        conn.execute("DELETE FROM t", []).unwrap();
        for i in 0..n {
            conn.execute("INSERT INTO t VALUES (?1)", [i]).unwrap();
        }
        db_path
    }

    /// A database whose committed rows exist **only in the WAL**, as left
    /// behind by a `ptm` that was killed (or is still running).
    ///
    /// All three files are captured while the writer is live and written back
    /// afterwards.  Closing a writer checkpoints the WAL, and *so does any
    /// later read* — which is why nothing here touches the database once the
    /// files are restored.  A test that needs to read it does so itself.
    fn crashed_wal_db(dir: &TempDir, n: i64) -> PathBuf {
        let db_path = dir.path().join("ptm.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; \
             PRAGMA wal_autocheckpoint=0; \
             CREATE TABLE t (x INTEGER); \
             INSERT INTO t VALUES (1);",
        )
        .unwrap();
        conn.execute("DELETE FROM t", []).unwrap();
        for i in 0..n {
            conn.execute("INSERT INTO t VALUES (?1)", [i]).unwrap();
        }

        let files = [
            (db_path.clone(), fs::read(&db_path).unwrap()),
            (wal_path(&db_path), fs::read(wal_path(&db_path)).unwrap()),
            (shm_path(&db_path), fs::read(shm_path(&db_path)).unwrap()),
        ];
        assert!(!files[1].1.is_empty(), "expected a non-empty WAL");
        drop(conn);

        for (path, bytes) in files {
            fs::write(path, bytes).unwrap();
        }
        db_path
    }

    /// Copy the raw database file aside and report what it contains on its own,
    /// without touching `db_path` (a read would checkpoint the WAL).
    fn inspect_raw_file(dir: &TempDir, db_path: &Path, tag: &str) -> (i64, i64) {
        let copy = dir.path().join(format!("raw-{}.db", tag));
        fs::write(&copy, fs::read(db_path).unwrap()).unwrap();
        let conn = Connection::open(&copy).unwrap();
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 't'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let count = if tables == 0 {
            0
        } else {
            conn.query_row("SELECT count(*) FROM t", [], |r| r.get(0))
                .unwrap()
        };
        (tables, count)
    }

    // ── CheckpointReport ──────────────────────────────────────────────────────

    #[test]
    fn clean_report_requires_no_busy_and_no_frames() {
        assert!(CheckpointReport { busy: 0, log_frames: 0, checkpointed: 0 }.is_clean());
        assert!(!CheckpointReport { busy: 1, log_frames: 10, checkpointed: 0 }.is_clean());
        assert!(!CheckpointReport { busy: 0, log_frames: 3, checkpointed: 0 }.is_clean());
    }

    #[test]
    fn non_wal_database_counts_as_clean() {
        // SQLite reports (-1, -1) for the frame counts when there is no WAL.
        assert!(CheckpointReport { busy: 0, log_frames: -1, checkpointed: -1 }.is_clean());
    }

    // ── check_and_checkpoint_wal ──────────────────────────────────────────────

    #[test]
    fn stale_wal_is_checkpointed_and_reported_clean() {
        // Finding #1b: the checkpoint's (busy, log, checkpointed) row used to be
        // thrown away, so a checkpoint that did not advance was reported as
        // success.  The report must now say whether the main file is complete.
        let dir = TempDir::new().unwrap();
        let db_path = crashed_wal_db(&dir, 30);

        let report = check_and_checkpoint_wal(&db_path)
            .unwrap()
            .expect("no other process holds the db, so the lock must be acquirable");
        assert!(
            report.is_clean(),
            "checkpoint must be clean, got {:?}",
            report
        );
        assert_eq!(wal_size(&db_path), 0, "the WAL must be truncated");
    }

    #[test]
    fn checkpointed_bytes_contain_every_committed_row() {
        // Finding #2: --end used to zip the raw ptm.db bytes, which can contain
        // nothing at all while the committed rows sit in the WAL.  After a
        // verified checkpoint the same raw read must be complete — that is what
        // gets packed.
        let dir = TempDir::new().unwrap();
        let db_path = crashed_wal_db(&dir, 30);

        let (tables, count) = inspect_raw_file(&dir, &db_path, "before");
        assert_eq!(
            (tables, count),
            (0, 0),
            "the WAL held the schema, so the raw file alone is empty"
        );

        let report = check_and_checkpoint_wal(&db_path).unwrap().unwrap();
        assert!(report.is_clean(), "got {:?}", report);

        let (tables, count) = inspect_raw_file(&dir, &db_path, "after");
        assert_eq!(tables, 1);
        assert_eq!(count, 30, "the checkpointed file must hold every row");
    }

    #[test]
    fn checkpointing_twice_in_succession_still_succeeds() {
        // After a checkpoint the WAL is truncated; a second call must still
        // succeed (idempotent behaviour).
        let dir = TempDir::new().unwrap();
        let db_path = crashed_wal_db(&dir, 5);
        assert!(check_and_checkpoint_wal(&db_path).unwrap().unwrap().is_clean());
        assert!(check_and_checkpoint_wal(&db_path).unwrap().unwrap().is_clean());
    }

    #[test]
    fn database_without_a_wal_is_clean() {
        let dir = TempDir::new().unwrap();
        let db_path = clean_db(&dir, 3);
        let report = check_and_checkpoint_wal(&db_path).unwrap().unwrap();
        assert!(report.is_clean(), "got {:?}", report);
    }

    #[test]
    fn missing_database_file_returns_error() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("nonexistent.db");
        // SQLITE_OPEN_READ_WRITE without SQLITE_OPEN_CREATE — must fail.
        assert!(
            check_and_checkpoint_wal(&db_path).is_err(),
            "opening a non-existent db must return Err"
        );
    }

    #[test]
    fn corrupt_database_is_an_error_not_a_lock_report() {
        // A file that is not a database must not be reported as "in use":
        // callers would skip their work and call it a concurrent launch.
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("ptm.db");
        fs::write(&db_path, b"this is not a sqlite database").unwrap();
        assert!(check_and_checkpoint_wal(&db_path).is_err());
    }

    // ── ExclusiveLock ─────────────────────────────────────────────────────────

    #[test]
    fn exclusive_lock_checkpoints_the_database_it_locked() {
        let dir = TempDir::new().unwrap();
        let db_path = crashed_wal_db(&dir, 7);
        let lock = ExclusiveLock::acquire(&db_path).unwrap().unwrap();
        assert!(lock.checkpoint().unwrap().is_clean());
        assert_eq!(wal_size(&db_path), 0);
    }

    #[test]
    fn remove_wal_files_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let db_path = crashed_wal_db(&dir, 2);
        let lock = ExclusiveLock::acquire(&db_path).unwrap().unwrap();
        lock.checkpoint().unwrap();
        lock.remove_wal_files().unwrap();
        assert!(!wal_path(&db_path).exists());
        assert!(!shm_path(&db_path).exists());
        // Second call must not fail on the now-missing sidecars.
        lock.remove_wal_files().unwrap();
        drop(lock);
        assert!(!wal_path(&db_path).exists());
        assert_eq!(rows(&db_path, "SELECT count(*) FROM t"), 2);
    }

    #[test]
    fn replacing_the_database_under_a_held_lock_keeps_the_new_file() {
        // Finding #1c, deterministic variant: a stale WAL must not be replayed
        // into the file that replaces it.
        let dir = TempDir::new().unwrap();
        let db_path = crashed_wal_db(&dir, 30);

        let staged = dir.path().join("staged.db");
        let s = Connection::open(&staged).unwrap();
        s.execute_batch(
            "PRAGMA journal_mode=WAL; \
             CREATE TABLE t (x INTEGER); \
             INSERT INTO t VALUES (99);",
        )
        .unwrap();
        drop(s);

        let lock = ExclusiveLock::acquire(&db_path).unwrap().unwrap();
        assert!(lock.checkpoint().unwrap().is_clean());
        lock.remove_wal_files().unwrap();
        fs::rename(&staged, &db_path).unwrap();
        drop(lock);

        assert!(!wal_path(&db_path).exists(), "no stale WAL may survive");
        assert!(!shm_path(&db_path).exists(), "no stale shm may survive");
        check_integrity(&db_path).unwrap();
        assert_eq!(
            rows(&db_path, "SELECT x FROM t"),
            99,
            "the pulled database must be what ends up on disk"
        );
    }

    // ── snapshot_bytes ────────────────────────────────────────────────────────

    #[test]
    fn snapshot_includes_frames_that_are_still_in_the_wal() {
        let dir = TempDir::new().unwrap();
        let db_path = crashed_wal_db(&dir, 12);

        let bytes = snapshot_bytes(&db_path).unwrap();
        let copy = dir.path().join("snap.db");
        fs::write(&copy, &bytes).unwrap();

        assert_eq!(rows(&copy, "SELECT count(*) FROM t"), 12);
        check_integrity(&copy).unwrap();
    }

    #[test]
    fn snapshot_of_a_clean_database_keeps_the_data() {
        let dir = TempDir::new().unwrap();
        let db_path = clean_db(&dir, 4);
        let bytes = snapshot_bytes(&db_path).unwrap();
        let copy = dir.path().join("snap.db");
        fs::write(&copy, &bytes).unwrap();
        assert_eq!(rows(&copy, "SELECT count(*) FROM t"), 4);
    }

    #[test]
    fn snapshot_leaves_no_temporary_files_behind() {
        let dir = TempDir::new().unwrap();
        let db_path = clean_db(&dir, 1);
        snapshot_bytes(&db_path).unwrap();
        let leftovers: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("snapshot"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {:?}", leftovers);
    }

    #[test]
    fn snapshot_of_a_missing_database_is_an_error() {
        let dir = TempDir::new().unwrap();
        assert!(snapshot_bytes(&dir.path().join("nope.db")).is_err());
    }

    // ── check_integrity ───────────────────────────────────────────────────────

    #[test]
    fn integrity_check_accepts_a_real_database() {
        let dir = TempDir::new().unwrap();
        check_integrity(&clean_db(&dir, 2)).unwrap();
    }

    #[test]
    fn integrity_check_rejects_a_non_database() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ptm.db");
        fs::write(&path, b"not a database").unwrap();
        assert!(check_integrity(&path).is_err());
    }

    #[test]
    fn integrity_check_rejects_a_truncated_database() {
        let dir = TempDir::new().unwrap();
        let db_path = clean_db(&dir, 5);
        let mut bytes = fs::read(&db_path).unwrap();
        bytes.truncate(64); // keep the header, drop the pages
        fs::write(&db_path, &bytes).unwrap();
        assert!(check_integrity(&db_path).is_err());
    }

    // ── paths ─────────────────────────────────────────────────────────────────

    #[test]
    fn sidecar_paths_are_derived_from_the_db_path() {
        let db = Path::new("/tmp/x/ptm.db");
        assert_eq!(wal_path(db), Path::new("/tmp/x/ptm.db-wal"));
        assert_eq!(shm_path(db), Path::new("/tmp/x/ptm.db-shm"));
    }

    #[test]
    fn wal_size_is_zero_when_the_sidecar_is_absent() {
        let dir = TempDir::new().unwrap();
        let db_path = clean_db(&dir, 1);
        assert_eq!(wal_size(&db_path), 0);
    }

    #[test]
    fn wal_size_reports_the_sidecar_length() {
        let dir = TempDir::new().unwrap();
        let db_path = crashed_wal_db(&dir, 3);
        assert_eq!(
            wal_size(&db_path),
            fs::metadata(wal_path(&db_path)).unwrap().len()
        );
        assert!(wal_size(&db_path) > 0);
    }

    // ── the "another process has it open" path ────────────────────────────────
    //
    // POSIX advisory locks (what SQLite uses on Linux/macOS) are per-process:
    // two connections *within the same process* share lock state, so no thread
    // can stand in for a running `ptm`.  These two tests therefore re-execute
    // this test binary: `holder_child` is a no-op unless it is the child, and
    // the child does exactly what `ptm` does — open the database, set WAL mode,
    // run a SELECT, and stay alive.

    const HOLDER_DB: &str = "PTM_SYNC_TEST_HOLDER_DB";
    const HOLDER_READY: &str = "PTM_SYNC_TEST_HOLDER_READY";

    /// Not a test in its own right: the body only runs in the child process
    /// spawned by `lock_is_refused_while_another_process_holds_the_database`.
    #[test]
    fn holder_child() {
        let Ok(db_path) = std::env::var(HOLDER_DB) else {
            return;
        };
        let conn = Connection::open(&db_path).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))
            .unwrap();
        fs::write(std::env::var(HOLDER_READY).unwrap(), b"1").unwrap();
        // Held until the parent kills us.
        std::thread::sleep(Duration::from_secs(60));
        drop(conn);
    }

    /// A child process that holds `db_path` open the way a running `ptm` does.
    struct ProcessHolder {
        child: std::process::Child,
        ready: PathBuf,
    }

    impl ProcessHolder {
        fn start(dir: &TempDir, db_path: &Path) -> Self {
            let ready = dir.path().join(".holder-ready");
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "db::tests::holder_child",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(HOLDER_DB, db_path)
                .env(HOLDER_READY, &ready)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("failed to spawn the holder process");

            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while !ready.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the holder process never became ready"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            Self { child, ready }
        }
    }

    impl Drop for ProcessHolder {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = fs::remove_file(&self.ready);
        }
    }

    #[test]
    fn lock_is_refused_while_another_process_holds_the_database() {
        // Finding #1a/#1c: `--start` promises to allow a concurrent launch, so
        // the liveness probe has to be right about a database another process
        // has open.  An in-process test cannot prove this — see above.
        let dir = TempDir::new().unwrap();
        let db_path = clean_db(&dir, 3);
        let holder = ProcessHolder::start(&dir, &db_path);

        assert!(
            ExclusiveLock::acquire(&db_path).unwrap().is_none(),
            "a database held open by another process must not be lockable"
        );
        assert!(
            check_and_checkpoint_wal(&db_path).unwrap().is_none(),
            "the WAL must be reported live, not checkpointed"
        );

        drop(holder);

        // With the holder gone the same database locks normally.
        assert!(
            ExclusiveLock::acquire(&db_path).unwrap().is_some(),
            "the lock must be acquirable once the other process is gone"
        );
    }

    #[test]
    fn snapshot_reads_a_database_that_another_process_holds_open() {
        // `--end` and the local backup run while a `ptm` may still be alive in
        // another terminal, so the snapshot must not need an exclusive lock.
        let dir = TempDir::new().unwrap();
        let db_path = clean_db(&dir, 4);
        let _holder = ProcessHolder::start(&dir, &db_path);

        let bytes = snapshot_bytes(&db_path).expect("snapshot must not need a lock");
        let copy = dir.path().join("snap.db");
        fs::write(&copy, &bytes).unwrap();
        assert_eq!(rows(&copy, "SELECT count(*) FROM t"), 4);
        check_integrity(&copy).unwrap();
    }
}
