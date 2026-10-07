//! Diverged-database merge for ptm-sync.
//!
//! Merges a second ptm data directory (`--from`) into the local one.  The
//! design relies on two invariants of the ptm schema:
//!
//! 1. **All primary keys are UUID v4** — identical IDs mean "the same row";
//!    independently created rows can never collide.  The merge is therefore
//!    a set-union of insertions plus last-writer-wins (LWW) for shared rows.
//! 2. **Every mutable table carries `updated_at`** (UTC ISO 8601) — used as
//!    the LWW tiebreaker for rows that exist on both sides.
//!
//! Per-table semantics:
//!
//! | Table                              | New rows | Shared rows |
//! |------------------------------------|----------|-------------|
//! | `projects`                         | insert   | LWW on `updated_at` (title, description, archived) |
//! | `todos`                            | insert   | LWW on `updated_at` (project, title, status, reminder, archived) |
//! | `updates`                          | insert   | LWW on `updated_at` (label, body) |
//! | `references_table`                 | insert   | LWW on `updated_at` (title, body) |
//! | `tags`                             | insert   | keep local (no `updated_at`; fields are cosmetic) |
//! | `project_tags` / `reference_tags`  | union    | n/a (pure membership) |
//!
//! Tag dedup by name: if the two databases each created a tag with the same
//! name (different UUIDs), the incoming tag's memberships are remapped to the
//! local tag with the same name (case-insensitive) and the duplicate tag row
//! is not inserted — mirroring the app's case-insensitive rename checks.
//!
//! `sort_order` policy: shared rows keep the local ranking; new rows are
//! appended after the local rows of their ordering group (offset by the
//! group's local max plus their own foreign rank), and every group is then
//! re-spaced to 1.0, 2.0, … with `ROW_NUMBER()`.  Result: local relative
//! order is untouched, foreign-only rows slot in after it, and `ORDER BY
//! sort_order` consumers see a clean, tie-free ranking.
//!
//! Known limitation: a row deleted on one side is resurrected by the other
//! side's surviving copy.  True delete propagation would need tombstones.

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// Same schema as the main app (`src/data/db.rs`).  Running it on an existing
/// database is a no-op for tables that already exist.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tags (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    color       TEXT
);

CREATE TABLE IF NOT EXISTS projects (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    sort_order  REAL NOT NULL DEFAULT 0.0,
    archived    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS project_tags (
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    tag_id      TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (project_id, tag_id)
);

CREATE TABLE IF NOT EXISTS updates (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    label       TEXT NOT NULL DEFAULT '',
    body        TEXT NOT NULL,
    sort_order  REAL NOT NULL DEFAULT 0.0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS todos (
    id          TEXT PRIMARY KEY,
    project_id  TEXT REFERENCES projects(id) ON DELETE CASCADE,
    title       TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'new'
                     CHECK(status IN ('new','in_progress','done','canceled')),
    reminder    TEXT,
    sort_order  REAL NOT NULL DEFAULT 0.0,
    archived    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS references_table (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL DEFAULT '',
    sort_order  REAL NOT NULL DEFAULT 0.0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS reference_tags (
    reference_id TEXT NOT NULL REFERENCES references_table(id) ON DELETE CASCADE,
    tag_id       TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (reference_id, tag_id)
);
"#;

// ── Public entry point ───────────────────────────────────────────────────────

/// Merge the ptm data directory at `from_dir` into the local one (`paths`).
///
/// Steps:
/// 1. Verify both directories contain `ptm.db`.
/// 2. Checkpoint the local WAL (aborts if another process holds the db open).
/// 3. Snapshot the local db with `VACUUM INTO` before touching anything.
/// 4. Copy the source db to a temp file and bring both schemas up to date
///    (guarded ALTER TABLE migrations, mirroring `migrate()` in the app).
/// 5. ATTACH the copy, run the merge inside one transaction, DETACH.
/// 6. Union-merge `user_words.txt` from both sides.
/// 7. Invalidate `.db_hash` so the next `--end` pushes the merged state.
pub fn run_merge(paths: &crate::Paths, from_dir: &Path, dry_run: bool) -> Result<()> {
    let from_db = from_dir.join("ptm.db");

    if !paths.db.exists() {
        bail!(
            "Local ptm.db not found at '{}' — nothing to merge into",
            paths.db.display()
        );
    }
    if !from_db.exists() {
        bail!(
            "Source ptm.db not found at '{}' — nothing to merge from",
            from_db.display()
        );
    }

    if dry_run {
        crate::log_entry(paths, "[dry-run] merge checked inputs; no changes made");
        println!("[dry-run] merge inputs verified; no changes made");
        return Ok(());
    }

    // ── Local WAL checkpoint ──────────────────────────────────────────────
    if paths.db_wal.exists() {
        crate::log_entry(paths, "WAL file found; checking if database is in use");
        match crate::db::check_and_checkpoint_wal(&paths.db) {
            Ok(true) => crate::log_entry(paths, "Local WAL checkpointed"),
            Ok(false) => bail!(
                "Local database is in use by another process; close it before merging"
            ),
            Err(e) => bail!("WAL status check failed: {:#}", e),
        }
    }

    // ── Pre-merge snapshot ────────────────────────────────────────────────
    let backup_path = snapshot_before_merge(&paths.db, &paths.local_backups)?;
    crate::log_entry(paths, &format!("Pre-merge snapshot: {}", backup_path.display()));
    println!("Pre-merge snapshot: {}", backup_path.display());

    // ── Prepare an up-to-date temp copy of the source ─────────────────────
    let tmp_dir = paths.data_dir.join(".tmp-merge");
    std::fs::create_dir_all(&tmp_dir).context("Failed to create temp merge directory")?;
    let tmp_src = tmp_dir.join("src.db");
    let result: Result<()> = (|| {
        vacuum_into(&from_db, &tmp_src)
            .with_context(|| format!("Failed to snapshot source db {}", from_db.display()))?;
        {
            let src_conn = open_rw(&tmp_src)?;
            ensure_schema_compat(&src_conn)?;
        } // connection dropped; migrations committed

        // ── Merge ─────────────────────────────────────────────────────────
        crate::log_entry(paths, "Merging databases");
        let stats = merge_dbs(&paths.db, &tmp_src).with_context(|| {
            format!("Merging '{}' into '{}'", from_db.display(), paths.db.display())
        })?;
        crate::log_entry(
            paths,
            &format!(
                "Merge stats: {} rows inserted, {} shared rows updated, \
                 {} tags added, {} tag memberships added",
                stats.rows_inserted, stats.rows_updated,
                stats.tags_added, stats.tag_memberships_added,
            ),
        );
        println!(
            "Merged: {} rows inserted, {} shared rows updated, \
             {} tags added, {} tag memberships added",
            stats.rows_inserted, stats.rows_updated,
            stats.tags_added, stats.tag_memberships_added,
        );
        Ok(())
    })();

    // Clean up temp dir (best effort — a leftover dir is harmless).
    let _ = std::fs::remove_dir_all(&tmp_dir);
    result?;

    // ── user_words.txt union merge ────────────────────────────────────────
    let from_dict = from_dir.join("user_words.txt");
    let words_merged = merge_user_words(&from_dict, &paths.dict)?;
    if words_merged > 0 {
        crate::log_entry(paths, &format!("Merged {} user_words.txt entries", words_merged));
        println!("Merged {} user_words.txt entries", words_merged);
    }

    // ── Invalidate the sync baseline ──────────────────────────────────────
    let _ = std::fs::remove_file(&paths.db_hash);
    crate::log_entry(paths, "Removed .db_hash; next --end will push the merged state");

    Ok(())
}

// ── Low-level helpers ────────────────────────────────────────────────────────

/// Open a database read-write (creating it only if it is brand new).
fn open_rw(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("Failed to open database: {}", path.display()))?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    Ok(conn)
}

/// Compact-copy `source` into `dest_path` using `VACUUM INTO`.
/// Works even if `source` is opened read-only and has a live WAL.
fn vacuum_into(source: &Path, dest_path: &Path) -> Result<()> {
    // Remove a stale target first — VACUUM INTO refuses to overwrite.
    let _ = std::fs::remove_file(dest_path);
    let conn = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("Failed to open source db: {}", source.display()))?;
    conn.execute("VACUUM INTO ?1", rusqlite::params![dest_path.to_string_lossy()])
        .with_context(|| format!("VACUUM INTO {} failed", dest_path.display()))?;
    Ok(())
}

/// Snapshot the local db into `backup_dir` as `ptm-premerge-<ts>.db`,
/// keeping only the most recent `keep` snapshots.
fn snapshot_before_merge(db_path: &Path, backup_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(backup_dir)
        .with_context(|| format!("Failed to create {}", backup_dir.display()))?;
    let ts = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
        .format("%Y-%m-%dT%H-%M-%SZ")
        .to_string();
    let dest = backup_dir.join(format!("ptm-premerge-{}.db", ts));
    vacuum_into(db_path, &dest)?;
    rotate_premerge_snapshots(backup_dir, 5)?;
    Ok(dest)
}

/// Delete the oldest `ptm-premerge-*.db` files beyond `keep`.
fn rotate_premerge_snapshots(backup_dir: &Path, keep: usize) -> Result<()> {
    let mut snapshots: Vec<PathBuf> = std::fs::read_dir(backup_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map_or(false, |n| n.to_string_lossy().starts_with("ptm-premerge-"))
                && p.extension().map_or(false, |x| x == "db")
        })
        .collect();
    snapshots.sort();
    for oldest in snapshots.drain(..snapshots.len().saturating_sub(keep)) {
        let _ = std::fs::remove_file(&oldest);
    }
    Ok(())
}

/// Bring a database's schema up to date with the columns the merge SQL
/// expects.  Mirrors the guarded ALTER TABLE migrations in `src/data/db.rs`
/// (minus the todos FK-constraint table recreation, which is irrelevant to
/// merging since we never delete).
pub fn ensure_schema_compat(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;

    let col_exists = |name: &str| -> Result<bool> {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('updates') WHERE name = ?1",
            rusqlite::params![name],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    };

    // updates.label
    if !col_exists("label")? {
        conn.execute_batch("ALTER TABLE updates ADD COLUMN label TEXT NOT NULL DEFAULT '';")?;
    }
    // updates.sort_order (seed from created_at rank, newest first)
    if !col_exists("sort_order")? {
        conn.execute_batch(
            "ALTER TABLE updates ADD COLUMN sort_order REAL NOT NULL DEFAULT 0.0;
             UPDATE updates SET sort_order = (
                 SELECT COUNT(*) FROM updates u2
                 WHERE u2.project_id = updates.project_id
                   AND u2.created_at > updates.created_at
             ) + 1;",
        )?;
    }
    // updates.updated_at (backfill from created_at)
    if !col_exists("updated_at")? {
        conn.execute_batch(
            "ALTER TABLE updates ADD COLUMN updated_at TEXT NOT NULL DEFAULT '';
             UPDATE updates SET updated_at = created_at WHERE updated_at = '';",
        )?;
    }
    // todos.archived
    let archived_existed: bool = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('todos') WHERE name = 'archived'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    if !archived_existed {
        conn.execute_batch("ALTER TABLE todos ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;")?;
    }

    // Repair data corrupted by the historical SELECT * migration (see
    // src/data/db.rs): archived held a TEXT timestamp instead of 0/1.
    let archived_corrupted: bool = conn.query_row(
        "SELECT COUNT(*) FROM todos WHERE typeof(archived) = 'text' LIMIT 1",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    if archived_corrupted {
        conn.execute_batch(
            "UPDATE todos
             SET    archived   = CAST(updated_at AS INTEGER),
                    created_at = archived,
                    updated_at = created_at
             WHERE  typeof(archived) = 'text';",
        )?;
    }

    Ok(())
}

// ── Merge core ───────────────────────────────────────────────────────────────

pub struct MergeStats {
    pub rows_inserted: usize,
    pub rows_updated: usize,
    pub tags_added: usize,
    pub tag_memberships_added: usize,
}

/// Open the local db read-write, ATTACH `src_db` (already migrated) as `src`,
/// and merge everything inside one transaction.
fn merge_dbs(local_db: &Path, src_db: &Path) -> Result<MergeStats> {
    let conn = open_rw(local_db)?;
    conn.execute(
        "ATTACH DATABASE ?1 AS src",
        rusqlite::params![src_db.to_string_lossy()],
    )
    .context("Failed to ATTACH source database")?;

    let mut stats = MergeStats {
        rows_inserted: 0,
        rows_updated: 0,
        tags_added: 0,
        tag_memberships_added: 0,
    };

    conn.execute_batch("BEGIN IMMEDIATE;")
        .context("Failed to begin merge transaction")?;
    if let Err(e) = merge_content(&conn, &mut stats) {
        let _ = conn.execute_batch("ROLLBACK;");
        return Err(anyhow::Error::from(e).context("Merge failed; transaction rolled back"));
    }
    conn.execute_batch("COMMIT;").context("Commit failed")?;

    let _ = conn.execute_batch("DETACH DATABASE src;");
    Ok(stats)
}

/// All merge statements, executed inside an open transaction on `conn`
/// with the source attached as `src`.
fn merge_content(conn: &Connection, stats: &mut MergeStats) -> Result<()> {
    // ── 1. Projects ───────────────────────────────────────────────────────
    stats.rows_inserted += conn.execute(
        "INSERT INTO projects (id, title, description, sort_order, archived, created_at, updated_at)
         SELECT id, title, description,
                COALESCE((SELECT MAX(sort_order) FROM main.projects), 0.0) + sort_order,
                archived, created_at, updated_at
         FROM src.projects
         WHERE id NOT IN (SELECT id FROM main.projects)",
        [],
    )?;

    stats.rows_updated += conn.execute(
        "UPDATE main.projects SET
            title       = s.title,
            description = s.description,
            archived    = s.archived,
            updated_at  = s.updated_at
         FROM src.projects s
         WHERE main.projects.id = s.id
           AND s.updated_at > main.projects.updated_at",
        [],
    )?;

    // ── 2. Todos (after projects so FKs resolve) ──────────────────────────
    stats.rows_inserted += conn.execute(
        "INSERT INTO todos (id, project_id, title, status, reminder, sort_order, archived, created_at, updated_at)
         SELECT id, project_id, title, status, reminder,
                COALESCE((
                    SELECT MAX(m.sort_order) FROM main.todos m
                    WHERE (m.project_id = src.todos.project_id)
                       OR (m.project_id IS NULL AND src.todos.project_id IS NULL)
                ), 0.0) + sort_order,
                archived, created_at, updated_at
         FROM src.todos
         WHERE id NOT IN (SELECT id FROM main.todos)",
        [],
    )?;

    stats.rows_updated += conn.execute(
        "UPDATE main.todos SET
            project_id = s.project_id,
            title      = s.title,
            status     = s.status,
            reminder   = s.reminder,
            archived   = s.archived,
            updated_at = s.updated_at
         FROM src.todos s
         WHERE main.todos.id = s.id
           AND s.updated_at > main.todos.updated_at",
        [],
    )?;

    // ── 3. Updates ────────────────────────────────────────────────────────
    stats.rows_inserted += conn.execute(
        "INSERT INTO updates (id, project_id, label, body, sort_order, created_at, updated_at)
         SELECT id, project_id, label, body,
                COALESCE((
                    SELECT MAX(m.sort_order) FROM main.updates m
                    WHERE m.project_id = src.updates.project_id
                ), 0.0) + sort_order,
                created_at, updated_at
         FROM src.updates
         WHERE id NOT IN (SELECT id FROM main.updates)",
        [],
    )?;

    stats.rows_updated += conn.execute(
        "UPDATE main.updates SET
            label      = s.label,
            body       = s.body,
            updated_at = s.updated_at
         FROM src.updates s
         WHERE main.updates.id = s.id
           AND s.updated_at > main.updates.updated_at",
        [],
    )?;

    // ── 4. References ─────────────────────────────────────────────────────
    stats.rows_inserted += conn.execute(
        "INSERT INTO references_table (id, title, body, sort_order, created_at, updated_at)
         SELECT id, title, body,
                COALESCE((SELECT MAX(sort_order) FROM main.references_table), 0.0) + sort_order,
                created_at, updated_at
         FROM src.references_table
         WHERE id NOT IN (SELECT id FROM main.references_table)",
        [],
    )?;

    stats.rows_updated += conn.execute(
        "UPDATE main.references_table SET
            title      = s.title,
            body       = s.body,
            updated_at = s.updated_at
         FROM src.references_table s
         WHERE main.references_table.id = s.id
           AND s.updated_at > main.references_table.updated_at",
        [],
    )?;

    // ── 5. Tags: insert only tags new by id AND name; the local tag wins on
    // name collisions (different UUID, same name) and memberships for the
    // dropped id are remapped by name in step 6. ──────────────────────────
    stats.tags_added += conn.execute(
        "INSERT INTO tags (id, name, color)
         SELECT id, name, color FROM src.tags
         WHERE id NOT IN (SELECT id FROM main.tags)
           AND LOWER(name) NOT IN (SELECT LOWER(name) FROM main.tags)",
        [],
    )?;

    // ── 6. Tag memberships, remapping name-duplicate tag ids ─────────────
    // Resolve each incoming membership's tag id: exact id if it exists
    // locally, otherwise the local tag with the same name; skip if neither.
    let add_memberships = |table: &str, fk_col: &str| -> Result<usize> {
        Ok(conn.execute(
            &format!(
                "INSERT OR IGNORE INTO {table} ({fk_col}, tag_id)
                 SELECT pt.{fk_col},
                        COALESCE(
                            (SELECT t.id FROM main.tags t WHERE t.id = pt.tag_id),
                            (SELECT t.id FROM main.tags t
                             JOIN src.tags s ON LOWER(t.name) = LOWER(s.name)
                             WHERE s.id = pt.tag_id)
                        )
                 FROM src.{table} pt
                 WHERE pt.tag_id IS NOT NULL"
            ),
            [],
        )?)
    };
    stats.tag_memberships_added += add_memberships("project_tags", "project_id")?;
    stats.tag_memberships_added += add_memberships("reference_tags", "reference_id")?;

    // ── 7. Garbage-collect tags with no remaining memberships ────────────
    conn.execute(
        "DELETE FROM tags
         WHERE id NOT IN (SELECT tag_id FROM main.project_tags)
           AND id NOT IN (SELECT tag_id FROM main.reference_tags)",
        [],
    )?;

    // ── 8. Re-space sort_order to 1.0, 2.0, … per ordering group ─────────
    respace(conn, "projects", None)?;
    respace(conn, "todos", Some("project_id"))?;
    respace(conn, "updates", Some("project_id"))?;
    respace(conn, "references_table", None)?;

    Ok(())
}

/// Re-space a table's `sort_order` to 1..N, ordering within each partition
/// (or the whole table when `partition` is None) by the existing
/// `sort_order` first and `created_at` as the tiebreaker.
fn respace(conn: &Connection, table: &str, partition: Option<&str>) -> Result<()> {
    let partition_clause = match partition {
        Some(col) => format!("PARTITION BY {}", col),
        None => String::new(),
    };
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE rank_{table} AS
             SELECT id, ROW_NUMBER() OVER ({partition_clause} ORDER BY sort_order, created_at) AS rn
             FROM {table};
         UPDATE {table} SET sort_order = (
             SELECT rn FROM temp.rank_{table} r WHERE r.id = {table}.id);
         DROP TABLE temp.rank_{table};"
    ))?;
    Ok(())
}

// ── user_words.txt ───────────────────────────────────────────────────────────

/// Union-merge the source directory's `user_words.txt` into the local one.
/// Returns the number of words added to the local file.
fn merge_user_words(from_dict: &Path, local_dict: &Path) -> Result<usize> {
    let from_str = std::fs::read_to_string(from_dict).unwrap_or_default();
    if from_str.trim().is_empty() {
        return Ok(0); // nothing on the source side — leave local untouched
    }
    let local_str = std::fs::read_to_string(local_dict).unwrap_or_default();
    let merged = crate::merge_dictionaries(&local_str, &from_str);
    let before = local_str.lines().filter(|l| !l.trim().is_empty()).count();
    let after = merged.lines().filter(|l| !l.trim().is_empty()).count();
    if after > before {
        std::fs::write(local_dict, merged.as_bytes())
            .with_context(|| format!("Failed to write {}", local_dict.display()))?;
        Ok(after - before)
    } else {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::TempDir;

    /// Create a fresh db with the current schema and return its connection.
    fn fresh_db(dir: &TempDir, name: &str) -> (PathBuf, Connection) {
        let path = dir.path().join(name);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        (path, conn)
    }

    fn insert_project(conn: &Connection, id: &str, title: &str, updated: &str, order: f64) {
        conn.execute(
            "INSERT INTO projects (id, title, description, sort_order, archived, created_at, updated_at)
             VALUES (?1, ?2, '', ?3, 0, '2025-01-01T00:00:00Z', ?4)",
            params![id, title, order, updated],
        )
        .unwrap();
    }

    fn insert_todo(
        conn: &Connection,
        id: &str,
        project_id: Option<&str>,
        title: &str,
        status: &str,
        updated: &str,
        order: f64,
    ) {
        conn.execute(
            "INSERT INTO todos (id, project_id, title, status, sort_order, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, '2025-01-01T00:00:00Z', ?6)",
            params![id, project_id, title, status, order, updated],
        )
        .unwrap();
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    // ── insert-only merge ────────────────────────────────────────────────────

    #[test]
    fn disjoint_databases_union() {
        let dir = TempDir::new().unwrap();
        let (local, lc) = fresh_db(&dir, "local.db");
        let (src, sc) = fresh_db(&dir, "src.db");

        insert_project(&lc, "p1", "Local project", "2025-01-01T00:00:00Z", 1.0);
        insert_project(&sc, "p2", "Source project", "2025-01-02T00:00:00Z", 1.0);
        insert_todo(&lc, "t1", Some("p1"), "Local task", "new", "2025-01-01T00:00:00Z", 1.0);
        insert_todo(&sc, "t2", Some("p2"), "Source task", "done", "2025-01-02T00:00:00Z", 1.0);
        drop(lc);
        drop(sc);

        let stats = merge_dbs(&local, &src).unwrap();
        assert_eq!(stats.rows_inserted, 2, "one project + one todo inserted");
        assert_eq!(stats.rows_updated, 0);

        let conn = Connection::open(&local).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM projects"), 2);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM todos"), 2);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM todos WHERE status='done' AND project_id='p2'"),
            1,
            "source todo keeps its project link"
        );
    }

    // ── last-writer-wins ─────────────────────────────────────────────────────

    #[test]
    fn shared_row_takes_newer_side() {
        let dir = TempDir::new().unwrap();
        let (local, lc) = fresh_db(&dir, "local.db");
        let (src, sc) = fresh_db(&dir, "src.db");

        // Same id on both sides; source is newer.
        insert_project(&lc, "p1", "Old title", "2025-01-01T00:00:00Z", 1.0);
        insert_project(&sc, "p1", "New title", "2025-06-01T00:00:00Z", 1.0);
        drop(lc);
        drop(sc);

        let stats = merge_dbs(&local, &src).unwrap();
        assert_eq!(stats.rows_inserted, 0);
        assert_eq!(stats.rows_updated, 1);

        let conn = Connection::open(&local).unwrap();
        let title: String = conn
            .query_row("SELECT title FROM projects WHERE id='p1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "New title");
    }

    #[test]
    fn shared_row_keeps_local_when_newer() {
        let dir = TempDir::new().unwrap();
        let (local, lc) = fresh_db(&dir, "local.db");
        let (src, sc) = fresh_db(&dir, "src.db");

        insert_project(&lc, "p1", "Local newer", "2025-06-01T00:00:00Z", 1.0);
        insert_project(&sc, "p1", "Source older", "2025-01-01T00:00:00Z", 1.0);
        drop(lc);
        drop(sc);

        merge_dbs(&local, &src).unwrap();

        let conn = Connection::open(&local).unwrap();
        let title: String = conn
            .query_row("SELECT title FROM projects WHERE id='p1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Local newer");
    }

    #[test]
    fn equal_updated_at_keeps_local() {
        let dir = TempDir::new().unwrap();
        let (local, lc) = fresh_db(&dir, "local.db");
        let (src, sc) = fresh_db(&dir, "src.db");

        insert_project(&lc, "p1", "Local", "2025-01-01T00:00:00Z", 1.0);
        insert_project(&sc, "p1", "Source", "2025-01-01T00:00:00Z", 1.0);
        drop(lc);
        drop(sc);

        merge_dbs(&local, &src).unwrap();

        let conn = Connection::open(&local).unwrap();
        let title: String = conn
            .query_row("SELECT title FROM projects WHERE id='p1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Local");
    }

    // ── todo row content merges field-by-field via LWW ───────────────────────

    #[test]
    fn todo_status_change_propagates() {
        let dir = TempDir::new().unwrap();
        let (local, lc) = fresh_db(&dir, "local.db");
        let (src, sc) = fresh_db(&dir, "src.db");

        insert_project(&lc, "p1", "P1", "2025-01-01T00:00:00Z", 1.0);
        insert_project(&sc, "p1", "P1", "2025-01-01T00:00:00Z", 1.0);
        insert_todo(&lc, "t1", Some("p1"), "Task", "new", "2025-01-01T00:00:00Z", 1.0);
        insert_todo(&sc, "t1", Some("p1"), "Task", "done", "2025-01-05T00:00:00Z", 1.0);
        drop(lc);
        drop(sc);

        merge_dbs(&local, &src).unwrap();

        let conn = Connection::open(&local).unwrap();
        let status: String = conn
            .query_row("SELECT status FROM todos WHERE id='t1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "done");
    }

    // ── tags ─────────────────────────────────────────────────────────────────

    #[test]
    fn tags_and_memberships_merge() {
        let dir = TempDir::new().unwrap();
        let (local, lc) = fresh_db(&dir, "local.db");
        let (src, sc) = fresh_db(&dir, "src.db");

        insert_project(&lc, "p1", "P1", "2025-01-01T00:00:00Z", 1.0);
        insert_project(&sc, "p1", "P1", "2025-01-01T00:00:00Z", 1.0);

        lc.execute("INSERT INTO tags (id, name) VALUES ('tag-local', 'rust')", []).unwrap();
        lc.execute(
            "INSERT INTO project_tags (project_id, tag_id) VALUES ('p1', 'tag-local')",
            [],
        )
        .unwrap();
        sc.execute("INSERT INTO tags (id, name) VALUES ('tag-src', 'sqlite')", []).unwrap();
        sc.execute(
            "INSERT INTO project_tags (project_id, tag_id) VALUES ('p1', 'tag-src')",
            [],
        )
        .unwrap();
        drop(lc);
        drop(sc);

        let stats = merge_dbs(&local, &src).unwrap();
        assert_eq!(stats.tags_added, 1);
        assert_eq!(stats.tag_memberships_added, 1);

        let conn = Connection::open(&local).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM tags"), 2);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM project_tags"), 2);
    }

    #[test]
    fn duplicate_tag_names_are_deduped_and_memberships_remapped() {
        let dir = TempDir::new().unwrap();
        let (local, lc) = fresh_db(&dir, "local.db");
        let (src, sc) = fresh_db(&dir, "src.db");

        insert_project(&lc, "p1", "P1", "2025-01-01T00:00:00Z", 1.0);
        insert_project(&sc, "p1", "P1", "2025-01-01T00:00:00Z", 1.0);

        // Both sides created a tag named "rust" with different UUIDs.
        lc.execute("INSERT INTO tags (id, name) VALUES ('tag-a', 'rust')", []).unwrap();
        lc.execute(
            "INSERT INTO project_tags (project_id, tag_id) VALUES ('p1', 'tag-a')",
            [],
        )
        .unwrap();
        sc.execute("INSERT INTO tags (id, name) VALUES ('tag-b', 'rust')", []).unwrap();
        sc.execute(
            "INSERT INTO project_tags (project_id, tag_id) VALUES ('p1', 'tag-b')",
            [],
        )
        .unwrap();
        drop(lc);
        drop(sc);

        let stats = merge_dbs(&local, &src).unwrap();
        assert_eq!(stats.tags_added, 0, "duplicate name must not create a second tag");
        assert_eq!(stats.tag_memberships_added, 0, "membership remaps onto existing tag");

        let conn = Connection::open(&local).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM tags"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM project_tags WHERE tag_id='tag-a'"), 1);
    }

    // ── sort_order respacing ─────────────────────────────────────────────────

    #[test]
    fn sort_order_is_respaced_without_ties_or_gaps() {
        let dir = TempDir::new().unwrap();
        let (local, lc) = fresh_db(&dir, "local.db");
        let (src, sc) = fresh_db(&dir, "src.db");

        insert_project(&lc, "p1", "P1", "2025-01-01T00:00:00Z", 1.0);
        insert_project(&sc, "p1", "P1", "2025-01-01T00:00:00Z", 1.0);
        insert_todo(&lc, "t1", Some("p1"), "A", "new", "2025-01-01T00:00:00Z", 5.0);
        insert_todo(&lc, "t2", Some("p1"), "B", "new", "2025-01-02T00:00:00Z", 10.0);
        // Source: same project, one shared todo + one new, overlapping orders.
        insert_todo(&sc, "t2", Some("p1"), "B", "new", "2025-01-02T00:00:00Z", 5.0);
        insert_todo(&sc, "t3", Some("p1"), "C", "new", "2025-01-03T00:00:00Z", 7.0);
        drop(lc);
        drop(sc);

        merge_dbs(&local, &src).unwrap();

        let conn = Connection::open(&local).unwrap();
        let mut stmt = conn
            .prepare("SELECT id, sort_order FROM todos WHERE project_id='p1' ORDER BY sort_order")
            .unwrap();
        let rows: Vec<(String, f64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, "t1", "local order preserved first");
        assert_eq!(rows[1].0, "t2");
        assert_eq!(rows[2].0, "t3", "new source todo appended");
        for (i, (_, order)) in rows.iter().enumerate() {
            let expected = (i + 1) as f64;
            assert_eq!(*order, expected, "sort_order must be respaced to 1..N");
        }
    }

    // ── transaction atomicity ────────────────────────────────────────────────

    #[test]
    fn failed_merge_rolls_back() {
        let dir = TempDir::new().unwrap();
        let (local, lc) = fresh_db(&dir, "local.db");
        let (src, sc) = fresh_db(&dir, "src.db");

        insert_project(&lc, "p1", "P1", "2025-01-01T00:00:00Z", 1.0);
        // Corrupt the source: todo referencing a nonexistent project violates
        // FKs during the merge.  (FKs are disabled only for this fixture
        // write; the merge itself re-enables them via open_rw.)
        sc.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        insert_todo(&sc, "t1", Some("missing-project"), "Orphan", "new", "2025-01-01T00:00:00Z", 1.0);
        drop(lc);
        drop(sc);

        assert!(merge_dbs(&local, &src).is_err(), "FK violation must fail the merge");

        let conn = Connection::open(&local).unwrap();
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM todos"),
            0,
            "rolled-back transaction must not leave partial data"
        );
    }

    // ── ensure_schema_compat on a legacy database ────────────────────────────

    #[test]
    fn legacy_db_gains_missing_columns() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("legacy.db");
        let conn = Connection::open(&path).unwrap();
        // Pre-sort_order, pre-updated_at, pre-archived schema.
        conn.execute_batch(
            "CREATE TABLE updates (
                id          TEXT PRIMARY KEY,
                project_id  TEXT NOT NULL,
                body        TEXT NOT NULL,
                created_at  TEXT NOT NULL
             );
             CREATE TABLE todos (
                id          TEXT PRIMARY KEY,
                project_id  TEXT,
                title       TEXT NOT NULL,
                status      TEXT NOT NULL DEFAULT 'new',
                reminder    TEXT,
                sort_order  REAL NOT NULL DEFAULT 0.0,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
             );
             INSERT INTO updates (id, project_id, body, created_at) VALUES
                ('u1', 'p1', 'newest', '2025-01-03T00:00:00Z'),
                ('u2', 'p1', 'oldest', '2025-01-01T00:00:00Z');",
        )
        .unwrap();
        drop(conn);

        {
            let conn = Connection::open(&path).unwrap();
            ensure_schema_compat(&conn).unwrap();
        }

        let conn = Connection::open(&path).unwrap();
        // label column added with default
        let label: String = conn
            .query_row("SELECT label FROM updates WHERE id='u1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(label, "");
        // updated_at backfilled from created_at
        let updated: String = conn
            .query_row("SELECT updated_at FROM updates WHERE id='u1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(updated, "2025-01-03T00:00:00Z");
        // sort_order seeded from created_at rank (newest first)
        let so_newest: f64 = conn
            .query_row("SELECT sort_order FROM updates WHERE id='u1'", [], |r| r.get(0))
            .unwrap();
        let so_oldest: f64 = conn
            .query_row("SELECT sort_order FROM updates WHERE id='u2'", [], |r| r.get(0))
            .unwrap();
        assert!(so_newest < so_oldest, "newest update must have smaller sort_order");
        // todos.archived added
        let archived: i64 = conn
            .query_row("SELECT archived FROM todos LIMIT 1", [], |r| r.get(0))
            .unwrap_or(0);
        let _ = archived; // table is empty; presence of the column is what matters
        assert!(conn.prepare("SELECT archived FROM todos").is_ok());
    }

    // ── user_words.txt merge ─────────────────────────────────────────────────

    #[test]
    fn user_words_union_merges() {
        let dir = TempDir::new().unwrap();
        let local = dir.path().join("user_words.txt");
        let from = dir.path().join("from_words.txt");
        std::fs::write(&local, b"apple\nbanana\n").unwrap();
        std::fs::write(&from, b"banana\ncherry\n").unwrap();

        let added = merge_user_words(&from, &local).unwrap();
        assert_eq!(added, 1);
        let content = std::fs::read_to_string(&local).unwrap();
        assert!(content.contains("apple"));
        assert!(content.contains("cherry"));
        assert_eq!(content.matches("banana").count(), 1);
    }

    #[test]
    fn snapshot_rotation_keeps_newest_five() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("ptm.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES (1);").unwrap();
        }
        let backup_dir = dir.path().join("local-backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        for i in 0..7u8 {
            std::fs::write(
                backup_dir.join(format!("ptm-premerge-2025-01-0{}T00-00-00Z.db", i)),
                b"old",
            )
            .unwrap();
        }
        let snap = snapshot_before_merge(&db_path, &backup_dir).unwrap();
        assert!(snap.exists());
        let mut left: Vec<String> = std::fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(left.len(), 5, "rotation must keep only 5; got {:?}", left);
    }

    #[test]
    fn user_words_missing_source_is_a_noop() {
        let dir = TempDir::new().unwrap();
        let local = dir.path().join("user_words.txt");
        std::fs::write(&local, b"apple\n").unwrap();
        let from = dir.path().join("does_not_exist.txt");

        let added = merge_user_words(&from, &local).unwrap();
        assert_eq!(added, 0);
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "apple\n");
    }
}
