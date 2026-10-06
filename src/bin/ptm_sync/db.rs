//! SQLite WAL detection and checkpoint.
//!
//! Attempting an EXCLUSIVE lock is the canonical way to determine whether
//! another SQLite client has the database open.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// Inspect the WAL file situation and act accordingly.
///
/// Returns:
///   `Ok(true)`  — WAL was stale (no other process had it open); we
///                 successfully acquired EXCLUSIVE mode and ran
///                 `PRAGMA wal_checkpoint(TRUNCATE)`.  The connection is
///                 then dropped, releasing the lock.
///   `Ok(false)` — Another process has the database open; the WAL is live.
///                 No changes were made.
///   `Err(_)`    — An unexpected error prevented the check.
///
/// Callers should only call this function when `ptm.db-wal` already exists.
pub fn check_and_checkpoint_wal(db_path: &Path) -> Result<bool> {
    // Open without creating (db must already exist).
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("Failed to open database: {}", db_path.display()))?;

    // Attempt to acquire an exclusive lock.  If another process has any lock
    // (SHARED, RESERVED, PENDING, or EXCLUSIVE), this will fail.
    let result = conn.execute_batch(
        "PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;",
    );

    match result {
        Ok(_) => {
            // We hold the exclusive lock — the WAL is stale.
            // Checkpoint and truncate the WAL so subsequent plain opens are clean.
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .with_context(|| "WAL checkpoint failed")?;
            // conn drops here, releasing the exclusive lock.
            Ok(true)
        }
        Err(_) => {
            // Could not acquire exclusive lock — another process has the db open.
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a SQLite database in WAL mode with some data, then close the
    /// connection.  SQLite runs a passive checkpoint on close, but the WAL file
    /// may still be present on disk.
    fn create_wal_db(dir: &TempDir) -> std::path::PathBuf {
        let db_path = dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; \
             CREATE TABLE t (x INTEGER); \
             INSERT INTO t VALUES (1);",
        )
        .unwrap();
        // Connection drops here; SQLite passively checkpoints the WAL.
        db_path
    }

    // ── check_and_checkpoint_wal ──────────────────────────────────────────────────

    #[test]
    fn stale_wal_with_no_other_connection_returns_true() {
        // No other connection holds a lock, so EXCLUSIVE mode must be
        // acquirable and the function must return Ok(true).
        let dir = TempDir::new().unwrap();
        let db_path = create_wal_db(&dir);
        let result = check_and_checkpoint_wal(&db_path).unwrap();
        assert!(result, "expected Ok(true): no other connection holds the db");
    }

    #[test]
    fn calling_twice_in_succession_still_succeeds() {
        // After a checkpoint the WAL is truncated; a second call must still
        // succeed (idempotent behaviour).
        let dir = TempDir::new().unwrap();
        let db_path = create_wal_db(&dir);
        assert!(check_and_checkpoint_wal(&db_path).unwrap());
        assert!(check_and_checkpoint_wal(&db_path).unwrap());
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

    // NOTE: the Ok(false) / "database in use by another process" path is not
    // unit-tested here.  POSIX advisory locks (used by SQLite on Linux/macOS)
    // are per-process: two connections *within the same process* share the same
    // lock state, so one thread cannot reliably block another thread's EXCLUSIVE
    // request via fcntl locks.  This path is exercised in integration/manual
    // testing where a separate OS process holds the database open.
}


