use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

/// Open (or create) the SQLite database at `path`, enable WAL + FK pragmas,
/// and run the schema migration.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;

    // Guard each migration with a pragma_table_info check so we never rely on
    // the `IF NOT EXISTS` clause (added in SQLite 3.37), keeping compatibility
    // with older bundled or system SQLite versions.
    let label_existed: bool = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('updates') WHERE name = 'label'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !label_existed {
        conn.execute_batch(
            "ALTER TABLE updates ADD COLUMN label TEXT NOT NULL DEFAULT '';",
        )?;
    }

    // Check before altering so we know whether to initialise sort_order values.
    // When the column is first added, seed it from created_at rank so existing
    // updates retain their newest-first visual order.
    let sort_order_existed: bool = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('updates') WHERE name = 'sort_order'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;

    if !sort_order_existed {
        conn.execute_batch(
            "ALTER TABLE updates ADD COLUMN sort_order REAL NOT NULL DEFAULT 0.0;",
        )?;
        conn.execute_batch(
            "UPDATE updates SET sort_order = (
                SELECT COUNT(*) FROM updates u2
                WHERE u2.project_id = updates.project_id
                  AND u2.created_at > updates.created_at
            ) + 1;",
        )?;
    }

    let updated_at_existed: bool = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('updates') WHERE name = 'updated_at'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;

    if !updated_at_existed {
        conn.execute_batch(
            "ALTER TABLE updates ADD COLUMN updated_at TEXT NOT NULL DEFAULT '';",
        )?;
        // Backfill existing rows: treat created_at as the initial updated_at.
        conn.execute_batch(
            "UPDATE updates SET updated_at = created_at WHERE updated_at = '';",
        )?
    }

    let archived_todo_existed: bool = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('todos') WHERE name = 'archived'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !archived_todo_existed {
        conn.execute_batch(
            "ALTER TABLE todos ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;",
        )?;
    }

    // Migrate todos.project_id from ON DELETE SET NULL → ON DELETE CASCADE.
    // SQLite requires full table recreation to change a column constraint.
    // We detect the old definition by inspecting the stored schema text.
    let todos_needs_cascade: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type='table' AND name='todos' AND instr(sql,'ON DELETE SET NULL') > 0",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;

    if todos_needs_cascade {
        // PRAGMA foreign_keys must be OFF while recreating the table.
        // These pragmas cannot run inside a transaction, so we issue them
        // separately and wrap only the data-moving steps in a transaction.
        conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
        conn.execute_batch("
            BEGIN;
            CREATE TABLE todos_v2 (
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
            INSERT INTO todos_v2
                (id, project_id, title, status, reminder, sort_order, archived, created_at, updated_at)
            SELECT
                id, project_id, title, status, reminder, sort_order, archived, created_at, updated_at
            FROM todos;
            DROP TABLE todos;
            ALTER TABLE todos_v2 RENAME TO todos;
            COMMIT;
        ")?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    }

    // Repair data corrupted by a previous migration that used SELECT * to copy
    // into todos_v2, whose column order differed from the ALTER TABLE–extended
    // todos table.  The mismatch rotated three columns:
    //   archived   ← old created_at  (TEXT timestamp)
    //   created_at ← old updated_at
    //   updated_at ← old archived    (integer 0/1)
    // Detection: if any archived value has SQLite type TEXT the corruption is
    // present.  The UPDATE is a no-op once correctly valued rows are in place.
    let archived_corrupted: bool = conn.query_row(
        "SELECT COUNT(*) FROM todos WHERE typeof(archived) = 'text' LIMIT 1",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;

    if archived_corrupted {
        // All three assignments reference the PRE-update values (standard SQL
        // semantics), so this is not circular:
        //   new archived   = CAST(old updated_at AS INTEGER)  → 0 or 1
        //   new created_at = old archived column value        → original creation timestamp
        //   new updated_at = old created_at column value      → original update timestamp
        conn.execute_batch("
            UPDATE todos
            SET    archived   = CAST(updated_at AS INTEGER),
                   created_at = archived,
                   updated_at = created_at
            WHERE  typeof(archived) = 'text';
        ")?;
    }

    Ok(())
}

/// Opens an in-memory SQLite database with FK support and the full schema applied.
/// Only compiled in test builds; used by `repo` tests.
#[cfg(test)]
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    // WAL mode is incompatible with in-memory databases; FK support is enough.
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    // An in-memory DB is always brand-new, so we skip the ALTER TABLE migration
    // steps (which add columns to pre-existing tables) and apply the full schema
    // directly. This avoids relying on `ADD COLUMN IF NOT EXISTS` (SQLite 3.37+)
    // in environments that might have an older bundled or system SQLite.
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

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
