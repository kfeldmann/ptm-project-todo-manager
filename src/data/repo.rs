use anyhow::Result;
use rusqlite::{params, Connection};
use uuid::Uuid;
use chrono::{DateTime, Utc};
use std::time::SystemTime;

use crate::data::models::*;

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn now_str() -> String {
    DateTime::<Utc>::from(SystemTime::now()).format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// The `soonest_reminder` column expression, inlined into the project SELECTs.
///
/// This is the only definition of "soonest reminder" in the data layer: it used
/// to be a separate `load_soonest_reminder` query run once per project row
/// (finding #10), which made `list_projects` — and therefore every keystroke in
/// a search box — cost two round trips per project. It is a correlated
/// subquery, so SQLite evaluates it per row without an extra statement.
///
/// `Project::soonest_reminder` is computed, never stored; this is the predicate
/// that decides it. Note it counts only *active* todos (not done, not canceled),
/// which is what makes the reminder disappear from the Project List once its
/// todo is completed.
const SOONEST_REMINDER_COL: &str =
    "(SELECT MIN(t.reminder) FROM todos t
      WHERE t.project_id = p.id AND t.reminder IS NOT NULL
        AND t.status NOT IN ('done','canceled'))";

/// Load the tags of one project, alphabetically. One statement.
fn load_project_tags(conn: &Connection, project_id: &str) -> Result<Vec<Tag>> {
    let mut tags = load_tags_for_projects(conn, std::slice::from_ref(&project_id.to_string()))?;
    Ok(tags.remove(project_id).unwrap_or_default())
}

/// Load the tags of many projects at once, keyed by project id.
///
/// One statement for the whole set, however many projects are asked for — the
/// bulk form of [`load_project_tags`], and the fix for finding #10's per-row
/// tag query. Projects with no tags are absent from the map rather than mapped
/// to an empty `Vec`; callers use `.unwrap_or_default()`.
///
/// An empty `ids` slice issues no statement at all (there is nothing to bind).
fn load_tags_for_projects(
    conn: &Connection,
    ids: &[String],
) -> Result<std::collections::HashMap<String, Vec<Tag>>> {
    let mut by_project: std::collections::HashMap<String, Vec<Tag>> = Default::default();
    if ids.is_empty() {
        return Ok(by_project);
    }

    // `ORDER BY t.name` keeps each project's tag list alphabetical, as the
    // per-project query did.
    let placeholders = std::iter::repeat_n("?", ids.len()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT pt.project_id, t.id, t.name, t.color FROM tags t
         JOIN project_tags pt ON pt.tag_id = t.id
         WHERE pt.project_id IN ({placeholders})
         ORDER BY t.name"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            Tag { id: row.get(1)?, name: row.get(2)?, color: row.get(3)? },
        ))
    })?;
    for row in rows {
        let (project_id, tag) = row?;
        by_project.entry(project_id).or_default().push(tag);
    }
    Ok(by_project)
}

fn row_to_todo(row: &rusqlite::Row) -> rusqlite::Result<Todo> {
    let status_str: String = row.get(3)?;
    let archived_int: i64  = row.get(8)?;
    Ok(Todo {
        id:         row.get(0)?,
        project_id: row.get(1)?,
        title:      row.get(2)?,
        status:     status_str.parse::<TodoStatus>().unwrap_or_else(|e| {
            crate::log::warn(&format!("row_to_todo: {e} — defaulting to New"));
            TodoStatus::New
        }),
        reminder:   row.get(4)?,
        sort_order: row.get(5)?,
        archived:   archived_int != 0,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

// ─── Projects ────────────────────────────────────────────────────────────────

pub fn list_projects(
    conn: &Connection,
    include_archived: bool,
    tag_filter: Option<&str>,
    search: Option<&str>,
) -> Result<Vec<Project>> {
    let archived_clause = if include_archived { "" } else { "AND p.archived = 0" };
    let tag_clause = if tag_filter.is_some() {
        "AND EXISTS (
            SELECT 1 FROM project_tags pt
            JOIN tags t ON t.id = pt.tag_id
            WHERE pt.project_id = p.id AND LOWER(t.name) = LOWER(?1)
         )"
    } else {
        ""
    };

    // Two statements per call, however many rows come back: the project rows
    // (with the soonest reminder inlined) and one bulk tag query for all of
    // them. This function runs on every keystroke of the search box, so a
    // per-row query is a per-keystroke query — finding #10.
    let sql = format!(
        "SELECT p.id, p.title, p.description, p.sort_order, p.archived,
                p.created_at, p.updated_at, {SOONEST_REMINDER_COL}
         FROM projects p
         WHERE 1=1 {archived_clause} {tag_clause}
         ORDER BY p.sort_order ASC, p.created_at ASC"
    );

    let mut stmt = conn.prepare(&sql)?;

    type Row = (String, String, String, f64, bool, String, String, Option<String>);

    let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<Row> {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
            row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?))
    };

    let rows: Vec<Row> = if let Some(tag) = tag_filter {
        stmt.query_map(params![tag], map_row)?.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        stmt.query_map([], map_row)?.collect::<rusqlite::Result<Vec<_>>>()?
    };

    // The search filter is applied in Rust (it is a substring test on the
    // lowercased title, which SQL `LIKE` would not reproduce for non-ASCII
    // input), so drop the non-matches before the tag query — there is no point
    // fetching tags for rows the caller will not see.
    let search_lower = search.map(|s| s.to_lowercase());
    let rows: Vec<Row> = match search_lower {
        Some(ref q) => rows
            .into_iter()
            .filter(|(_, title, ..)| title.to_lowercase().contains(q.as_str()))
            .collect(),
        None => rows,
    };

    let ids: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
    let mut tags_by_project = load_tags_for_projects(conn, &ids)?;

    Ok(rows
        .into_iter()
        .map(|(id, title, description, sort_order, archived, created_at, updated_at, soonest_reminder)| {
            let tags = tags_by_project.remove(&id).unwrap_or_default();
            Project {
                id, title, description, sort_order, archived,
                created_at, updated_at, tags, soonest_reminder,
            }
        })
        .collect())
}

/// Load one project, with its tags and soonest reminder.
///
/// Two statements: the row (with the reminder inlined) and the tag query. The
/// single-project form of [`list_projects`].
pub fn get_project(conn: &Connection, id: &str) -> Result<Project> {
    let (title, description, sort_order, archived, created_at, updated_at, soonest_reminder) =
        conn.query_row(
            &format!(
                "SELECT p.title, p.description, p.sort_order, p.archived,
                        p.created_at, p.updated_at, {SOONEST_REMINDER_COL}
                 FROM projects p WHERE p.id = ?1"
            ),
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                      row.get(4)?, row.get(5)?, row.get(6)?)),
        )?;
    let tags = load_project_tags(conn, id)?;
    Ok(Project {
        id: id.to_string(), title, description, sort_order, archived,
        created_at, updated_at, tags, soonest_reminder,
    })
}

pub fn count_archived(conn: &Connection) -> Result<u32> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM projects WHERE archived = 1",
        [], |r| r.get(0),
    )?)
}

pub fn create_project(conn: &Connection, title: &str) -> Result<Project> {
    let id  = new_id();
    let now = now_str();
    let max: f64 = conn.query_row(
        "SELECT COALESCE(MAX(sort_order), 0.0) FROM projects", [], |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO projects (id,title,description,sort_order,archived,created_at,updated_at)
         VALUES (?1,?2,'',?3,0,?4,?4)",
        params![id, title, max + 1.0, now],
    )?;
    get_project(conn, &id)
}

pub fn update_project_title(conn: &Connection, id: &str, title: &str) -> Result<()> {
    conn.execute(
        "UPDATE projects SET title = ?1, updated_at = ?2 WHERE id = ?3",
        params![title, now_str(), id],
    )?;
    Ok(())
}

pub fn update_project_description(conn: &Connection, id: &str, desc: &str) -> Result<()> {
    conn.execute(
        "UPDATE projects SET description = ?1, updated_at = ?2 WHERE id = ?3",
        params![desc, now_str(), id],
    )?;
    Ok(())
}

pub fn toggle_archive_project(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE projects SET archived = 1 - archived, updated_at = ?1 WHERE id = ?2",
        params![now_str(), id],
    )?;
    Ok(())
}

pub fn delete_project(conn: &Connection, id: &str) -> Result<()> {
    // Collect tag IDs before the CASCADE wipes project_tags.
    let mut stmt = conn.prepare(
        "SELECT tag_id FROM project_tags WHERE project_id = ?1",
    )?;
    let tag_ids: Vec<String> = stmt
        .query_map(params![id], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;

    // Delete todos explicitly so they are removed rather than orphaned into
    // the Inbox.  (The schema has been updated to ON DELETE CASCADE, but this
    // explicit delete is kept as a safety net for any database that predates
    // the migration.)
    conn.execute("DELETE FROM todos WHERE project_id = ?1", params![id])?;

    conn.execute("DELETE FROM projects WHERE id = ?1", params![id])?;

    // GC any tags that are now orphaned (this project may have been the only one).
    for tag_id in &tag_ids {
        gc_tag(conn, tag_id)?;
    }
    Ok(())
}

/// Swap the `sort_order` values of two projects so that J/K always reorders
/// within the visible list, regardless of which filters are active.
pub fn swap_project_order(conn: &Connection, id_a: &str, id_b: &str) -> Result<()> {
    let order_a: f64 = conn.query_row(
        "SELECT sort_order FROM projects WHERE id = ?1", params![id_a], |r| r.get(0),
    )?;
    let order_b: f64 = conn.query_row(
        "SELECT sort_order FROM projects WHERE id = ?1", params![id_b], |r| r.get(0),
    )?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("UPDATE projects SET sort_order = ?1 WHERE id = ?2", params![order_b, id_a])?;
    tx.execute("UPDATE projects SET sort_order = ?1 WHERE id = ?2", params![order_a, id_b])?;
    tx.commit()?;
    Ok(())
}

// ─── Updates ─────────────────────────────────────────────────────────────────

pub fn list_updates(conn: &Connection, project_id: &str) -> Result<Vec<Update>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, label, body, sort_order, created_at, updated_at FROM updates
         WHERE project_id = ?1 ORDER BY sort_order ASC",
    )?;
    let updates = stmt
        .query_map(params![project_id], |row| {
            Ok(Update {
                id: row.get(0)?, project_id: row.get(1)?,
                label: row.get(2)?, body: row.get(3)?,
                sort_order: row.get(4)?, created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(updates)
}

pub fn create_update(conn: &Connection, project_id: &str, label: &str, body: &str) -> Result<Update> {
    let id  = new_id();
    let now = now_str();
    // Prepend: new updates appear at the top (smallest sort_order within the project).
    let min_order: f64 = conn
        .query_row(
            "SELECT COALESCE(MIN(sort_order), 1.0) FROM updates WHERE project_id = ?1",
            params![project_id],
            |r| r.get(0),
        )
        .unwrap_or(1.0);
    let sort_order = min_order - 1.0;
    conn.execute(
        "INSERT INTO updates (id, project_id, label, body, sort_order, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![id, project_id, label, body, sort_order, now, now],
    )?;
    Ok(Update {
        id, project_id: project_id.to_string(),
        label: label.to_string(), body: body.to_string(),
        sort_order, created_at: now.clone(), updated_at: now,
    })
}

/// Reset sort_order to 1.0, 2.0, … for all updates belonging to `project_id`.
/// Swap the `sort_order` values of two updates so that J/K always reorders
/// within the visible list.
pub fn swap_update_order(conn: &Connection, id_a: &str, id_b: &str) -> Result<()> {
    let order_a: f64 = conn.query_row(
        "SELECT sort_order FROM updates WHERE id = ?1", params![id_a], |r| r.get(0),
    )?;
    let order_b: f64 = conn.query_row(
        "SELECT sort_order FROM updates WHERE id = ?1", params![id_b], |r| r.get(0),
    )?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("UPDATE updates SET sort_order = ?1 WHERE id = ?2", params![order_b, id_a])?;
    tx.execute("UPDATE updates SET sort_order = ?1 WHERE id = ?2", params![order_a, id_b])?;
    tx.commit()?;
    Ok(())
}

pub fn delete_update(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM updates WHERE id = ?1", params![id])?;
    Ok(())
}

/// Update both the label and body of an existing update entry.
pub fn update_update(conn: &Connection, id: &str, label: &str, body: &str) -> Result<()> {
    conn.execute(
        "UPDATE updates SET label = ?1, body = ?2, updated_at = ?3 WHERE id = ?4",
        params![label, body, now_str(), id],
    )?;
    Ok(())
}

// ─── Todos ────────────────────────────────────────────────────────────────────

/// List todos for a single project (`project_id = Some(id)`) or the inbox (`None`).
pub fn list_todos(conn: &Connection, project_id: Option<&str>, hide_done: bool, include_archived: bool) -> Result<Vec<Todo>> {
    let done_clause     = if hide_done       { " AND status NOT IN ('done','canceled')" } else { "" };
    let archived_clause = if include_archived { "" } else { " AND archived = 0" };
    let sql_with_project = format!(
        "SELECT id, project_id, title, status, reminder, sort_order, created_at, updated_at, archived
         FROM todos WHERE project_id = ?1{}{} ORDER BY sort_order ASC",
        done_clause, archived_clause
    );
    let sql_inbox = format!(
        "SELECT id, project_id, title, status, reminder, sort_order, created_at, updated_at, archived
         FROM todos WHERE project_id IS NULL{}{} ORDER BY sort_order ASC",
        done_clause, archived_clause
    );
    let sql = if project_id.is_some() { &sql_with_project } else { &sql_inbox };
    let mut stmt = conn.prepare(sql)?;
    let todos = if let Some(pid) = project_id {
        stmt.query_map(params![pid], row_to_todo)?.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        stmt.query_map([], row_to_todo)?.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(todos)
}

pub fn get_todo(conn: &Connection, id: &str) -> Result<Todo> {
    Ok(conn.query_row(
        "SELECT id, project_id, title, status, reminder, sort_order, created_at, updated_at, archived
         FROM todos WHERE id = ?1",
        params![id], row_to_todo,
    )?)
}

pub fn create_todo(conn: &Connection, project_id: Option<&str>, title: &str) -> Result<Todo> {
    let id  = new_id();
    let now = now_str();
    let max: f64 = if let Some(pid) = project_id {
        conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0.0) FROM todos WHERE project_id = ?1",
            params![pid], |r| r.get(0),
        )?
    } else {
        conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0.0) FROM todos WHERE project_id IS NULL",
            [], |r| r.get(0),
        )?
    };

    if let Some(pid) = project_id {
        conn.execute(
            "INSERT INTO todos (id,project_id,title,status,sort_order,created_at,updated_at)
             VALUES (?1,?2,?3,'new',?4,?5,?5)",
            params![id, pid, title, max + 1.0, now],
        )?;
    } else {
        conn.execute(
            "INSERT INTO todos (id,project_id,title,status,sort_order,created_at,updated_at)
             VALUES (?1,NULL,?2,'new',?3,?4,?4)",
            params![id, title, max + 1.0, now],
        )?;
    }
    get_todo(conn, &id)
}

pub fn update_todo_title(conn: &Connection, id: &str, title: &str) -> Result<()> {
    conn.execute(
        "UPDATE todos SET title = ?1, updated_at = ?2 WHERE id = ?3",
        params![title, now_str(), id],
    )?;
    Ok(())
}

pub fn cycle_todo_status(conn: &Connection, id: &str) -> Result<TodoStatus> {
    let todo = get_todo(conn, id)?;
    let next = todo.status.cycle();
    conn.execute(
        "UPDATE todos SET status = ?1, updated_at = ?2 WHERE id = ?3",
        params![next.as_str(), now_str(), id],
    )?;
    Ok(next)
}

pub fn delete_todo(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM todos WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn toggle_archive_todo(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE todos SET archived = 1 - archived, updated_at = ?1 WHERE id = ?2",
        params![now_str(), id],
    )?;
    Ok(())
}

/// Count archived todos for a single project (`project_id = Some(id)`) or the inbox (`None`).
pub fn count_archived_todos(conn: &Connection, project_id: Option<&str>) -> Result<u32> {
    let n: i64 = if let Some(pid) = project_id {
        conn.query_row(
            "SELECT COUNT(*) FROM todos WHERE archived = 1 AND project_id = ?1",
            params![pid],
            |r| r.get(0),
        )?
    } else {
        conn.query_row(
            "SELECT COUNT(*) FROM todos WHERE archived = 1 AND project_id IS NULL",
            [],
            |r| r.get(0),
        )?
    };
    Ok(n as u32)
}

/// Count archived todos across all non-archived projects and the inbox.
pub fn count_archived_todos_global(conn: &Connection) -> Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM todos t
         WHERE t.archived = 1
           AND (t.project_id IS NULL
             OR EXISTS (SELECT 1 FROM projects p WHERE p.id = t.project_id AND p.archived = 0))",
        [],
        |r| r.get(0),
    )?;
    Ok(n as u32)
}

/// Count done/canceled non-archived todos for a specific project (or inbox when `None`).
pub fn count_done_todos(conn: &Connection, project_id: Option<&str>) -> Result<u32> {
    let n: i64 = if let Some(pid) = project_id {
        conn.query_row(
            "SELECT COUNT(*) FROM todos WHERE archived = 0 AND project_id = ?1 AND status IN ('done','canceled')",
            params![pid],
            |r| r.get(0),
        )?
    } else {
        conn.query_row(
            "SELECT COUNT(*) FROM todos WHERE archived = 0 AND project_id IS NULL AND status IN ('done','canceled')",
            [],
            |r| r.get(0),
        )?
    };
    Ok(n as u32)
}

/// Count done/canceled non-archived todos across all non-archived projects and the inbox.
pub fn count_done_todos_global(conn: &Connection) -> Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM todos t
         WHERE t.archived = 0
           AND t.status IN ('done','canceled')
           AND (t.project_id IS NULL
             OR EXISTS (SELECT 1 FROM projects p WHERE p.id = t.project_id AND p.archived = 0))",
        [],
        |r| r.get(0),
    )?;
    Ok(n as u32)
}

/// Append the todo at the end of `project_id`'s band (or the inbox when `None`).
pub fn move_todo(conn: &Connection, todo_id: &str, project_id: Option<&str>) -> Result<()> {
    let max: f64 = if let Some(pid) = project_id {
        conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0.0) FROM todos WHERE project_id = ?1",
            params![pid], |r| r.get(0),
        )?
    } else {
        conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0.0) FROM todos WHERE project_id IS NULL",
            [], |r| r.get(0),
        )?
    };
    conn.execute(
        "UPDATE todos SET project_id = ?1, sort_order = ?2, updated_at = ?3 WHERE id = ?4",
        params![project_id, max + 1.0, now_str(), todo_id],
    )?;
    Ok(())
}

/// Swap the `sort_order` values of two todos so that J/K always reorders
/// within the visible list, regardless of which filters are active.
pub fn swap_todo_order(conn: &Connection, id_a: &str, id_b: &str) -> Result<()> {
    let order_a: f64 = conn.query_row(
        "SELECT sort_order FROM todos WHERE id = ?1", params![id_a], |r| r.get(0),
    )?;
    let order_b: f64 = conn.query_row(
        "SELECT sort_order FROM todos WHERE id = ?1", params![id_b], |r| r.get(0),
    )?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("UPDATE todos SET sort_order = ?1 WHERE id = ?2", params![order_b, id_a])?;
    tx.execute("UPDATE todos SET sort_order = ?1 WHERE id = ?2", params![order_a, id_b])?;
    tx.commit()?;
    Ok(())
}

pub fn set_todo_reminder(conn: &Connection, todo_id: &str, reminder: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE todos SET reminder = ?1, updated_at = ?2 WHERE id = ?3",
        params![reminder, now_str(), todo_id],
    )?;
    Ok(())
}

// ─── Tags ────────────────────────────────────────────────────────────────────

pub fn list_all_tags(conn: &Connection) -> Result<Vec<Tag>> {
    let mut stmt = conn.prepare("SELECT id, name, color FROM tags ORDER BY name")?;
    let tags = stmt
        .query_map([], |row| Ok(Tag { id: row.get(0)?, name: row.get(1)?, color: row.get(2)? }))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(tags)
}

/// Add `tag_name` to a project; creates the tag row if it doesn't exist.
/// Returns the tag id.
pub fn tag_project(conn: &Connection, project_id: &str, tag_name: &str) -> Result<String> {
    let tag_id: String = match conn.query_row(
        "SELECT id FROM tags WHERE LOWER(name) = LOWER(?1)",
        params![tag_name], |r| r.get(0),
    ) {
        Ok(id) => id,
        Err(_) => {
            let id = new_id();
            conn.execute("INSERT INTO tags (id, name) VALUES (?1, ?2)", params![id, tag_name])?;
            id
        }
    };
    conn.execute(
        "INSERT OR IGNORE INTO project_tags (project_id, tag_id) VALUES (?1, ?2)",
        params![project_id, tag_id],
    )?;
    Ok(tag_id)
}

/// Why [`rename_tag`] refused.
///
/// The two cases are deliberately distinct values rather than one string: a
/// taken name is something the user can fix by typing a different name, while a
/// database failure is not. Collapsing them (finding #11) meant a locked or
/// corrupt database silently performed the rename *and* the UI reported the
/// unrelated SQL error as `'X' already exists`.
#[derive(Debug)]
pub enum RenameTagError {
    /// Another tag already holds the requested name (case-insensitive).
    NameTaken,
    /// The database could not be read or written; nothing was renamed.
    Db(anyhow::Error),
}

impl std::fmt::Display for RenameTagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NameTaken => write!(f, "tag name already exists"),
            Self::Db(e)     => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RenameTagError {}

/// Rename a tag globally (affects every project using it).
///
/// Fails with [`RenameTagError::NameTaken`] when another tag already holds
/// `new_name` (case-insensitively, mirroring the app's rename checks), and with
/// [`RenameTagError::Db`] when the collision probe or the `UPDATE` itself fails.
/// A failed probe is never treated as "no collision".
pub fn rename_tag(
    conn: &Connection,
    tag_id: &str,
    new_name: &str,
) -> std::result::Result<(), RenameTagError> {
    let taken: u32 = conn
        .query_row(
            "SELECT COUNT(*) FROM tags WHERE LOWER(name) = LOWER(?1) AND id != ?2",
            params![new_name, tag_id],
            |r| r.get(0),
        )
        .map_err(|e| RenameTagError::Db(e.into()))?;
    if taken > 0 {
        return Err(RenameTagError::NameTaken);
    }
    conn.execute(
        "UPDATE tags SET name = ?1 WHERE id = ?2",
        params![new_name, tag_id],
    )
    .map_err(|e| RenameTagError::Db(e.into()))?;
    Ok(())
}

/// Remove a tag from a project; garbage-collects orphaned tag rows.
pub fn untag_project(conn: &Connection, project_id: &str, tag_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM project_tags WHERE project_id = ?1 AND tag_id = ?2",
        params![project_id, tag_id],
    )?;
    gc_tag(conn, tag_id)
}

fn gc_tag(conn: &Connection, tag_id: &str) -> Result<()> {
    let count: u32 = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM project_tags   WHERE tag_id = ?1)
              + (SELECT COUNT(*) FROM reference_tags WHERE tag_id = ?1)",
        params![tag_id], |r| r.get(0),
    )?;
    if count == 0 {
        conn.execute("DELETE FROM tags WHERE id = ?1", params![tag_id])?;
    }
    Ok(())
}

// ─── Global Todo View ────────────────────────────────────────────────────────

/// A project band for the global todo list: inbox (project_id = None) or a project.
pub struct TodoGroup {
    #[allow(dead_code)] // Not yet consumed by the todo-list view; reserved for future project navigation.
    pub project_id: Option<String>,
    pub project_title: String,
    pub todos: Vec<Todo>,
}

/// Returns all todos grouped for the global view: inbox first, then projects in sort order.
pub fn list_todos_global(
    conn: &Connection,
    hide_done: bool,
    search: Option<&str>,
    tag_filter: Option<&str>,
    include_archived: bool,
) -> Result<Vec<TodoGroup>> {
    let status_clause   = if hide_done       { " AND t.status NOT IN ('done','canceled')" } else { "" };
    let archived_clause = if include_archived { "" } else { " AND t.archived = 0" };
    let search_lower  = search.map(|s| s.to_lowercase());

    // Three statements per call, however many projects there are: the project
    // list (itself constant — see `list_projects`), one query for every todo in
    // the inbox *and* in those projects, and the bulk tag load inside
    // `list_projects`. The per-project todo query this replaces was the other
    // half of finding #10.
    let projects = list_projects(conn, false, tag_filter, None)?;
    let project_ids: Vec<String> = projects.iter().map(|p| p.id.clone()).collect();

    // The inbox is only shown when no tag filter is active; `NULL IN (...)` is
    // never true, so selecting on `project_id IN (…)` naturally excludes it.
    let select_inbox = tag_filter.is_none();
    let id_placeholders = std::iter::repeat_n("?", project_ids.len()).collect::<Vec<_>>().join(",");

    let where_ids = match (select_inbox, project_ids.is_empty()) {
        (true,  true)  => "t.project_id IS NULL".to_string(),
        (true,  false) => format!("(t.project_id IS NULL OR t.project_id IN ({id_placeholders}))"),
        (false, true)  => return Ok(Vec::new()),
        (false, false) => format!("t.project_id IN ({id_placeholders})"),
    };

    let sql = format!(
        "SELECT id, project_id, title, status, reminder, sort_order, created_at, updated_at, archived
         FROM todos t WHERE {where_ids}{status_clause}{archived_clause}
         ORDER BY sort_order ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let all_todos: Vec<Todo> = stmt
        .query_map(rusqlite::params_from_iter(project_ids.iter()), row_to_todo)?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Bucket by project, dropping non-matching titles as we go. `None` is the
    // inbox.
    let mut inbox: Vec<Todo> = Vec::new();
    let mut by_project: std::collections::HashMap<String, Vec<Todo>> = Default::default();
    for todo in all_todos {
        if let Some(ref q) = search_lower {
            if !todo.title.to_lowercase().contains(q.as_str()) {
                continue;
            }
        }
        match &todo.project_id {
            None => inbox.push(todo),
            Some(pid) => by_project.entry(pid.clone()).or_default().push(todo),
        }
    }

    // Assemble in display order: the inbox band first, then the projects in
    // their sort order, skipping bands with no matching todos.
    let mut groups: Vec<TodoGroup> = Vec::new();
    if !inbox.is_empty() {
        groups.push(TodoGroup {
            project_id: None,
            project_title: "Inbox".to_string(),
            todos: inbox,
        });
    }
    for project in projects {
        if let Some(todos) = by_project.remove(&project.id) {
            if !todos.is_empty() {
                groups.push(TodoGroup {
                    project_id: Some(project.id),
                    project_title: project.title,
                    todos,
                });
            }
        }
    }

    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::db;
    use crate::data::models::TodoStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn mk_conn() -> rusqlite::Connection {
        db::open_in_memory().expect("in-memory db")
    }

    // ── Statement counting (#10) ──────────────────────────────────────────────
    //
    // Finding #10 is "N+1 queries": the number of statements executed per call is
    // the measurable success criterion, so the regression tests count them. The
    // count comes from SQLite's own `SQLITE_TRACE_STMT` callback, which fires
    // once per statement *execution* (a re-used prepared statement fires again),
    // so a test cannot be fooled by caching inside rusqlite.
    //
    // The `trace` feature is a **dev-dependency** feature: it is unified into
    // test builds only, so the shipped binaries do not carry the callback
    // machinery. See Cargo.toml.

    /// Count the SQL statements executed on `conn` while `f` runs.
    ///
    /// Returns `(result, statement_count)`. The callback is left installed on
    /// the connection (it only bumps an `AtomicUsize`), so tests may call this
    /// several times against the same connection; each call measures its own
    /// window.
    fn count_statements<T>(
        conn: &Connection,
        f: impl FnOnce() -> T,
    ) -> (T, usize) {
        let counter = Arc::new(AtomicUsize::new(0));

        unsafe extern "C" fn trace_cb(
            _evt: std::ffi::c_uint,
            ctx: *mut std::ffi::c_void,
            _stmt: *mut std::ffi::c_void,
            _sql: *mut std::ffi::c_void,
        ) -> std::ffi::c_int {
            // SAFETY: `ctx` is the `Arc<AtomicUsize>` handed to `sqlite3_trace_v2`
            // below, and the `Arc` outlives the callback because it is a local of
            // the enclosing call.
            let counter = unsafe { &*(ctx as *const AtomicUsize) };
            counter.fetch_add(1, Ordering::Relaxed);
            0
        }

        let handle = unsafe { conn.handle() };
        let rc = unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                handle,
                1, // SQLITE_TRACE_STMT
                Some(trace_cb),
                Arc::as_ptr(&counter) as *mut std::ffi::c_void,
            )
        };
        assert_eq!(rc, 0, "sqlite3_trace_v2 failed");

        let before = counter.load(Ordering::Relaxed);
        let out = f();
        let after = counter.load(Ordering::Relaxed);

        // Detach the callback: the `Arc` must not outlive this call.
        unsafe {
            rusqlite::ffi::sqlite3_trace_v2(handle, 0, None, std::ptr::null_mut());
        }

        (out, after - before)
    }

    // ── Projects ────────────────────────────────────────────────────────────────

    #[test]
    fn create_and_get_project() {
        let conn = mk_conn();
        let p = create_project(&conn, "My Project").unwrap();
        assert_eq!(p.title, "My Project");
        assert!(!p.archived);
        assert!(p.description.is_empty());

        let fetched = get_project(&conn, &p.id).unwrap();
        assert_eq!(fetched.id, p.id);
        assert_eq!(fetched.title, "My Project");
    }

    #[test]
    fn list_projects_empty_database() {
        let conn = mk_conn();
        let projects = list_projects(&conn, false, None, None).unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn list_projects_excludes_archived_by_default() {
        let conn = mk_conn();
        let p = create_project(&conn, "Test").unwrap();
        toggle_archive_project(&conn, &p.id).unwrap();

        assert!(list_projects(&conn, false, None, None).unwrap().is_empty());
        assert_eq!(list_projects(&conn, true, None, None).unwrap().len(), 1);
    }

    #[test]
    fn list_projects_search_is_case_insensitive() {
        let conn = mk_conn();
        create_project(&conn, "Alpha Project").unwrap();
        create_project(&conn, "Beta Project").unwrap();
        create_project(&conn, "Gamma Work").unwrap();

        let r = list_projects(&conn, false, None, Some("project")).unwrap();
        assert_eq!(r.len(), 2);

        let r2 = list_projects(&conn, false, None, Some("ALPHA")).unwrap();
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].title, "Alpha Project");
    }

    #[test]
    fn list_projects_tag_filter() {
        let conn = mk_conn();
        let p1 = create_project(&conn, "Tagged").unwrap();
        create_project(&conn, "Untagged").unwrap();
        tag_project(&conn, &p1.id, "important").unwrap();

        let r = list_projects(&conn, false, Some("important"), None).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "Tagged");
    }

    #[test]
    fn update_project_title_and_description() {
        let conn = mk_conn();
        let p = create_project(&conn, "Old Title").unwrap();
        update_project_title(&conn, &p.id, "New Title").unwrap();
        update_project_description(&conn, &p.id, "Some description").unwrap();

        let updated = get_project(&conn, &p.id).unwrap();
        assert_eq!(updated.title, "New Title");
        assert_eq!(updated.description, "Some description");
    }

    #[test]
    fn toggle_archive_flips_state() {
        let conn = mk_conn();
        let p = create_project(&conn, "Test").unwrap();
        assert!(!p.archived);

        toggle_archive_project(&conn, &p.id).unwrap();
        assert!(get_project(&conn, &p.id).unwrap().archived);

        toggle_archive_project(&conn, &p.id).unwrap();
        assert!(!get_project(&conn, &p.id).unwrap().archived);
    }

    #[test]
    fn delete_project_removes_it_from_list() {
        let conn = mk_conn();
        let p = create_project(&conn, "Doomed").unwrap();
        delete_project(&conn, &p.id).unwrap();
        assert!(list_projects(&conn, false, None, None).unwrap().is_empty());
    }

    #[test]
    fn delete_project_cascades_todos() {
        let conn = mk_conn();
        let p = create_project(&conn, "Doomed").unwrap();
        let t = create_todo(&conn, Some(&p.id), "Owned Task").unwrap();
        delete_project(&conn, &p.id).unwrap();

        // Todos must be deleted with the project, not moved to the Inbox.
        let inbox = list_todos(&conn, None, false, false).unwrap();
        assert!(!inbox.iter().any(|todo| todo.id == t.id), "todo must not appear in inbox");
    }

    #[test]
    fn delete_project_cascades_updates() {
        let conn = mk_conn();
        let p = create_project(&conn, "Doomed").unwrap();
        create_update(&conn, &p.id, "", "Some update body").unwrap();
        delete_project(&conn, &p.id).unwrap();

        // Updates must be deleted with the project (ON DELETE CASCADE).
        let updates = list_updates(&conn, &p.id).unwrap();
        assert!(updates.is_empty(), "updates must be deleted with the project");
    }

    #[test]
    fn delete_project_gc_orphaned_tags() {
        let conn = mk_conn();
        let p = create_project(&conn, "Doomed").unwrap();
        tag_project(&conn, &p.id, "solo-tag").unwrap();
        assert_eq!(list_all_tags(&conn).unwrap().len(), 1);

        delete_project(&conn, &p.id).unwrap();

        // The tag was only used by the deleted project; it should be GC'd.
        assert!(list_all_tags(&conn).unwrap().is_empty());
    }

    #[test]
    fn delete_project_keeps_tag_used_by_other_project() {
        let conn = mk_conn();
        let p1 = create_project(&conn, "Doomed").unwrap();
        let p2 = create_project(&conn, "Survivor").unwrap();
        tag_project(&conn, &p1.id, "shared").unwrap();
        tag_project(&conn, &p2.id, "shared").unwrap();
        assert_eq!(list_all_tags(&conn).unwrap().len(), 1);

        delete_project(&conn, &p1.id).unwrap();

        // p2 still uses the tag; it must not be GC'd.
        assert_eq!(list_all_tags(&conn).unwrap().len(), 1);
    }

    #[test]
    fn count_archived_reflects_archived_projects() {
        let conn = mk_conn();
        assert_eq!(count_archived(&conn).unwrap(), 0);

        let p1 = create_project(&conn, "P1").unwrap();
        let p2 = create_project(&conn, "P2").unwrap();
        toggle_archive_project(&conn, &p1.id).unwrap();
        toggle_archive_project(&conn, &p2.id).unwrap();
        assert_eq!(count_archived(&conn).unwrap(), 2);
    }

    #[test]
    fn swap_project_order_exchanges_positions() {
        let conn = mk_conn();
        let p1 = create_project(&conn, "Alpha").unwrap();
        create_project(&conn, "Beta").unwrap();
        let p3 = create_project(&conn, "Gamma").unwrap();

        // Swap Alpha and Gamma — Beta should stay in place between them.
        swap_project_order(&conn, &p1.id, &p3.id).unwrap();

        let titles: Vec<String> = list_projects(&conn, false, None, None)
            .unwrap().into_iter().map(|p| p.title).collect();
        assert_eq!(titles, ["Gamma", "Beta", "Alpha"]);
    }

    #[test]
    fn swap_project_order_adjacent() {
        let conn = mk_conn();
        let p1 = create_project(&conn, "First").unwrap();
        let p2 = create_project(&conn, "Second").unwrap();
        create_project(&conn, "Third").unwrap();

        swap_project_order(&conn, &p1.id, &p2.id).unwrap();

        let titles: Vec<String> = list_projects(&conn, false, None, None)
            .unwrap().into_iter().map(|p| p.title).collect();
        assert_eq!(titles, ["Second", "First", "Third"]);
    }

    // ── Updates ─────────────────────────────────────────────────────────────────

    #[test]
    fn create_and_list_updates_for_project() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        create_update(&conn, &p.id, "Day 1", "First entry").unwrap();
        create_update(&conn, &p.id, "Day 2", "Second entry").unwrap();

        let updates = list_updates(&conn, &p.id).unwrap();
        assert_eq!(updates.len(), 2);
    }

    #[test]
    fn new_update_is_prepended_to_the_list() {
        // Each new update gets a lower sort_order, so it appears first.
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let u1 = create_update(&conn, &p.id, "First",  "body").unwrap();
        let u2 = create_update(&conn, &p.id, "Second", "body").unwrap();
        let u3 = create_update(&conn, &p.id, "Third",  "body").unwrap();

        let updates = list_updates(&conn, &p.id).unwrap();
        assert_eq!(updates[0].id, u3.id);
        assert_eq!(updates[1].id, u2.id);
        assert_eq!(updates[2].id, u1.id);
    }

    #[test]
    fn delete_update_removes_it() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let u = create_update(&conn, &p.id, "Label", "Body").unwrap();
        delete_update(&conn, &u.id).unwrap();
        assert!(list_updates(&conn, &p.id).unwrap().is_empty());
    }

    #[test]
    fn update_update_changes_label_and_body() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let u = create_update(&conn, &p.id, "Old Label", "Old Body").unwrap();
        update_update(&conn, &u.id, "New Label", "New Body").unwrap();

        let updates = list_updates(&conn, &p.id).unwrap();
        assert_eq!(updates[0].label, "New Label");
        assert_eq!(updates[0].body, "New Body");
    }

    #[test]
    fn swap_update_order_adjacent() {
        // Updates are prepended, so after creating First, Second, Third
        // the list order is [Third, Second, First].
        // Swapping Second and First should yield [Third, First, Second].
        let conn = mk_conn();
        let p  = create_project(&conn, "P").unwrap();
        let u1 = create_update(&conn, &p.id, "First",  "body").unwrap();
        let u2 = create_update(&conn, &p.id, "Second", "body").unwrap();
        create_update(&conn, &p.id, "Third",  "body").unwrap();

        swap_update_order(&conn, &u1.id, &u2.id).unwrap();

        let labels: Vec<String> = list_updates(&conn, &p.id)
            .unwrap().into_iter().map(|u| u.label).collect();
        assert_eq!(labels, ["Third", "First", "Second"]);
    }

    // ── Todos ──────────────────────────────────────────────────────────────────

    #[test]
    fn create_project_todo_has_correct_defaults() {
        let conn = mk_conn();
        let p = create_project(&conn, "Project").unwrap();
        let t = create_todo(&conn, Some(&p.id), "Task 1").unwrap();

        assert_eq!(t.title, "Task 1");
        assert_eq!(t.status, TodoStatus::New);
        assert_eq!(t.project_id.as_deref(), Some(p.id.as_str()));
        assert!(t.reminder.is_none());
    }

    #[test]
    fn create_inbox_todo_has_null_project_id() {
        let conn = mk_conn();
        let t = create_todo(&conn, None, "Inbox Task").unwrap();
        assert_eq!(t.title, "Inbox Task");
        assert!(t.project_id.is_none());
    }

    #[test]
    fn list_todos_for_project_excludes_inbox() {
        let conn = mk_conn();
        let p = create_project(&conn, "Project").unwrap();
        create_todo(&conn, Some(&p.id), "Task A").unwrap();
        create_todo(&conn, Some(&p.id), "Task B").unwrap();
        create_todo(&conn, None, "Inbox Task").unwrap();

        let todos = list_todos(&conn, Some(&p.id), false, false).unwrap();
        assert_eq!(todos.len(), 2);
        assert!(todos.iter().all(|t| t.project_id.as_deref() == Some(p.id.as_str())));
    }

    #[test]
    fn list_todos_inbox_excludes_project_todos() {
        let conn = mk_conn();
        let p = create_project(&conn, "Project").unwrap();
        create_todo(&conn, Some(&p.id), "Project Task").unwrap();
        create_todo(&conn, None, "Inbox A").unwrap();
        create_todo(&conn, None, "Inbox B").unwrap();

        let inbox = list_todos(&conn, None, false, false).unwrap();
        assert_eq!(inbox.len(), 2);
        assert!(inbox.iter().all(|t| t.project_id.is_none()));
    }

    #[test]
    fn list_todos_hide_done_filters_done_and_canceled() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let t_new  = create_todo(&conn, Some(&p.id), "New").unwrap();
        let t_prog = create_todo(&conn, Some(&p.id), "In Progress").unwrap();
        let t_done = create_todo(&conn, Some(&p.id), "Done").unwrap();
        let t_cncl = create_todo(&conn, Some(&p.id), "Canceled").unwrap();

        cycle_todo_status(&conn, &t_prog.id).unwrap(); // New → InProgress
        cycle_todo_status(&conn, &t_done.id).unwrap(); // New → InProgress
        cycle_todo_status(&conn, &t_done.id).unwrap(); // InProgress → Done
        cycle_todo_status(&conn, &t_cncl.id).unwrap(); // New → InProgress
        cycle_todo_status(&conn, &t_cncl.id).unwrap(); // InProgress → Done
        cycle_todo_status(&conn, &t_cncl.id).unwrap(); // Done → Canceled

        assert_eq!(list_todos(&conn, Some(&p.id), false, false).unwrap().len(), 4);

        let active = list_todos(&conn, Some(&p.id), true, false).unwrap();
        assert_eq!(active.len(), 2);
        let active_ids: Vec<&str> = active.iter().map(|t| t.id.as_str()).collect();
        assert!(active_ids.contains(&t_new.id.as_str()));
        assert!(active_ids.contains(&t_prog.id.as_str()));
    }

    #[test]
    fn update_todo_title_changes_the_title() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let t = create_todo(&conn, Some(&p.id), "Old").unwrap();
        update_todo_title(&conn, &t.id, "New Title").unwrap();
        assert_eq!(get_todo(&conn, &t.id).unwrap().title, "New Title");
    }

    #[test]
    fn cycle_todo_status_follows_the_full_sequence() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let t = create_todo(&conn, Some(&p.id), "Task").unwrap();
        assert_eq!(t.status, TodoStatus::New);

        assert_eq!(cycle_todo_status(&conn, &t.id).unwrap(), TodoStatus::InProgress);
        assert_eq!(cycle_todo_status(&conn, &t.id).unwrap(), TodoStatus::Done);
        assert_eq!(cycle_todo_status(&conn, &t.id).unwrap(), TodoStatus::Canceled);
        assert_eq!(cycle_todo_status(&conn, &t.id).unwrap(), TodoStatus::New);
    }

    #[test]
    fn delete_todo_removes_it() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let t = create_todo(&conn, Some(&p.id), "Task").unwrap();
        delete_todo(&conn, &t.id).unwrap();
        assert!(list_todos(&conn, Some(&p.id), false, false).unwrap().is_empty());
    }

    #[test]
    fn move_todo_from_inbox_to_project() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let t = create_todo(&conn, None, "Inbox Task").unwrap();
        move_todo(&conn, &t.id, Some(&p.id)).unwrap();

        assert!(list_todos(&conn, None, false, false).unwrap().is_empty());
        let project_todos = list_todos(&conn, Some(&p.id), false, false).unwrap();
        assert_eq!(project_todos.len(), 1);
        assert_eq!(project_todos[0].project_id.as_deref(), Some(p.id.as_str()));
    }

    #[test]
    fn move_todo_from_project_to_inbox() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let t = create_todo(&conn, Some(&p.id), "Project Task").unwrap();
        move_todo(&conn, &t.id, None).unwrap();

        assert!(list_todos(&conn, Some(&p.id), false, false).unwrap().is_empty());
        let inbox = list_todos(&conn, None, false, false).unwrap();
        assert_eq!(inbox.len(), 1);
        assert!(inbox[0].project_id.is_none());
    }

    #[test]
    fn set_and_clear_todo_reminder() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let t = create_todo(&conn, Some(&p.id), "Task").unwrap();

        set_todo_reminder(&conn, &t.id, Some("2025-12-31")).unwrap();
        assert_eq!(get_todo(&conn, &t.id).unwrap().reminder.as_deref(), Some("2025-12-31"));

        set_todo_reminder(&conn, &t.id, None).unwrap();
        assert!(get_todo(&conn, &t.id).unwrap().reminder.is_none());
    }

    // ── Tags ───────────────────────────────────────────────────────────────────

    #[test]
    fn tag_project_creates_tag_and_attaches_it() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        tag_project(&conn, &p.id, "rust").unwrap();

        let project = get_project(&conn, &p.id).unwrap();
        assert_eq!(project.tags.len(), 1);
        assert_eq!(project.tags[0].name, "rust");
    }

    #[test]
    fn tag_project_reuses_existing_tag_case_insensitively() {
        let conn = mk_conn();
        let p1 = create_project(&conn, "P1").unwrap();
        let p2 = create_project(&conn, "P2").unwrap();

        let id1 = tag_project(&conn, &p1.id, "Rust").unwrap();
        let id2 = tag_project(&conn, &p2.id, "rust").unwrap();
        assert_eq!(id1, id2, "same tag object should be reused");

        // Only one tag row should exist.
        assert_eq!(list_all_tags(&conn).unwrap().len(), 1);
    }

    #[test]
    fn untag_project_removes_link_and_gc_orphan() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let tag_id = tag_project(&conn, &p.id, "todelete").unwrap();
        untag_project(&conn, &p.id, &tag_id).unwrap();

        assert!(get_project(&conn, &p.id).unwrap().tags.is_empty());
        // The tags row itself should be garbage-collected.
        assert!(list_all_tags(&conn).unwrap().is_empty());
    }

    #[test]
    fn untag_project_keeps_tag_when_another_project_uses_it() {
        let conn = mk_conn();
        let p1 = create_project(&conn, "P1").unwrap();
        let p2 = create_project(&conn, "P2").unwrap();

        let tag_id = tag_project(&conn, &p1.id, "shared").unwrap();
        tag_project(&conn, &p2.id, "shared").unwrap();
        untag_project(&conn, &p1.id, &tag_id).unwrap();

        // Tag still exists because p2 still uses it.
        let tags = list_all_tags(&conn).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "shared");
    }

    #[test]
    fn rename_tag_succeeds() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let tag_id = tag_project(&conn, &p.id, "old-name").unwrap();
        rename_tag(&conn, &tag_id, "new-name").unwrap();

        let project = get_project(&conn, &p.id).unwrap();
        assert_eq!(project.tags[0].name, "new-name");
    }

    // Finding #11: `rename_tag` probed for a name collision with
    // `.map(|n| n > 0).unwrap_or(false)`, so *any* SQL failure — a locked
    // database, a corrupt index, a denied read — was indistinguishable from
    // "no collision" and the rename went ahead. The probe must propagate its
    // error instead of answering "false".
    #[test]
    fn rename_tag_propagates_probe_errors_instead_of_treating_them_as_no_collision() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let tag_id = tag_project(&conn, &p.id, "old-name").unwrap();

        // Deny reads of `tags.name` — the column the collision probe reads —
        // and nothing else. The `UPDATE tags SET name = ...` that performs the
        // rename writes that column but never reads it, so if the probe's
        // error is swallowed the rename still succeeds: the only difference is
        // whether the failure was reported. (Denying the whole table would not
        // work: the UPDATE's own `WHERE id = ?` reads `tags.id`.)
        conn.authorizer(Some(|ctx: rusqlite::hooks::AuthContext<'_>| {
            match ctx.action {
                rusqlite::hooks::AuthAction::Read { table_name: "tags", column_name: "name" } =>
                    rusqlite::hooks::Authorization::Deny,
                _ => rusqlite::hooks::Authorization::Allow,
            }
        })).unwrap();

        let result = rename_tag(&conn, &tag_id, "new-name");
        assert!(
            result.is_err(),
            "rename_tag must not treat a failed collision probe as 'no collision' (#11)"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            !msg.contains("already exists"),
            "a genuine SQL failure must not be reported as a name collision, got: {msg}"
        );
    }

    #[test]
    fn rename_tag_rejects_duplicate_name() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let id1 = tag_project(&conn, &p.id, "tag-a").unwrap();
        tag_project(&conn, &p.id, "tag-b").unwrap();

        let err = rename_tag(&conn, &id1, "tag-b").unwrap_err();
        assert!(matches!(err, RenameTagError::NameTaken), "got {err:?}");
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn rename_tag_reports_name_taken_and_db_failure_as_different_values() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let id1 = tag_project(&conn, &p.id, "tag-a").unwrap();
        tag_project(&conn, &p.id, "tag-b").unwrap();

        // Same refusal, distinguishable from a database failure.
        assert!(matches!(
            rename_tag(&conn, &id1, "tag-b"),
            Err(RenameTagError::NameTaken)
        ));
        // A free name is not a refusal at all.
        assert!(rename_tag(&conn, &id1, "tag-c").is_ok());
    }

    #[test]
    fn list_all_tags_returns_results_alphabetically() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        tag_project(&conn, &p.id, "zebra").unwrap();
        tag_project(&conn, &p.id, "alpha").unwrap();
        tag_project(&conn, &p.id, "middle").unwrap();

        let names: Vec<String> = list_all_tags(&conn)
            .unwrap().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["alpha", "middle", "zebra"]);
    }

    // ── Soonest reminder ──────────────────────────────────────────────────────

    #[test]
    fn soonest_reminder_returns_earliest_date_across_active_todos() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let t1 = create_todo(&conn, Some(&p.id), "A").unwrap();
        let t2 = create_todo(&conn, Some(&p.id), "B").unwrap();
        let t3 = create_todo(&conn, Some(&p.id), "C").unwrap();

        set_todo_reminder(&conn, &t1.id, Some("2025-06-01")).unwrap();
        set_todo_reminder(&conn, &t2.id, Some("2025-03-15")).unwrap(); // earliest
        set_todo_reminder(&conn, &t3.id, Some("2025-09-30")).unwrap();

        assert_eq!(
            get_project(&conn, &p.id).unwrap().soonest_reminder.as_deref(),
            Some("2025-03-15"),
        );
    }

    #[test]
    fn soonest_reminder_is_the_same_from_the_list_and_from_get_project() {
        // The list query inlines the reminder subquery that `get_project` also
        // uses (#10); the two must never disagree.
        let conn = mk_conn();
        let with   = create_project(&conn, "With reminder").unwrap();
        let without = create_project(&conn, "Without reminder").unwrap();
        let t = create_todo(&conn, Some(&with.id), "t").unwrap();
        set_todo_reminder(&conn, &t.id, Some("2025-04-02")).unwrap();

        let listed = list_projects(&conn, false, None, None).unwrap();
        for project in &listed {
            let fetched = get_project(&conn, &project.id).unwrap();
            assert_eq!(
                project.soonest_reminder, fetched.soonest_reminder,
                "list_projects and get_project disagree for {}",
                project.title,
            );
        }
        let listed_with = listed.iter().find(|p| p.id == with.id).unwrap();
        let listed_without = listed.iter().find(|p| p.id == without.id).unwrap();
        assert_eq!(listed_with.soonest_reminder.as_deref(), Some("2025-04-02"));
        assert_eq!(listed_without.soonest_reminder, None);
    }

    #[test]
    fn soonest_reminder_from_the_list_ignores_done_and_canceled_todos() {
        // The inlined subquery must keep the "active todos only" rule that the
        // per-row query had.
        let conn = mk_conn();
        let p  = create_project(&conn, "P").unwrap();
        let done = create_todo(&conn, Some(&p.id), "done").unwrap();
        let live = create_todo(&conn, Some(&p.id), "live").unwrap();
        set_todo_reminder(&conn, &done.id, Some("2025-01-01")).unwrap(); // earlier, but done
        set_todo_reminder(&conn, &live.id, Some("2025-08-08")).unwrap();

        cycle_todo_status(&conn, &done.id).unwrap(); // New → InProgress
        cycle_todo_status(&conn, &done.id).unwrap(); // InProgress → Done

        let listed = list_projects(&conn, false, None, None).unwrap();
        assert_eq!(listed[0].soonest_reminder.as_deref(), Some("2025-08-08"));
    }

    #[test]
    fn soonest_reminder_from_the_list_is_none_when_every_todo_is_done() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let t = create_todo(&conn, Some(&p.id), "t").unwrap();
        set_todo_reminder(&conn, &t.id, Some("2025-01-01")).unwrap();
        cycle_todo_status(&conn, &t.id).unwrap(); // → InProgress
        cycle_todo_status(&conn, &t.id).unwrap(); // → Done

        assert_eq!(list_projects(&conn, false, None, None).unwrap()[0].soonest_reminder, None);
    }

    #[test]
    fn soonest_reminder_from_the_list_is_per_project() {
        let conn = mk_conn();
        let a = create_project(&conn, "A").unwrap();
        let b = create_project(&conn, "B").unwrap();
        let ta = create_todo(&conn, Some(&a.id), "ta").unwrap();
        let tb = create_todo(&conn, Some(&b.id), "tb").unwrap();
        set_todo_reminder(&conn, &ta.id, Some("2025-09-09")).unwrap();
        set_todo_reminder(&conn, &tb.id, Some("2025-02-02")).unwrap();

        let listed = list_projects(&conn, false, None, None).unwrap();
        let by_id = |id: &str| listed.iter().find(|p| p.id == id).unwrap().soonest_reminder.clone();
        assert_eq!(by_id(&a.id).as_deref(), Some("2025-09-09"));
        assert_eq!(by_id(&b.id).as_deref(), Some("2025-02-02"));
    }

    #[test]
    fn soonest_reminder_from_the_list_ignores_inbox_todos() {
        let conn = mk_conn();
        create_project(&conn, "P").unwrap();
        let inbox = create_todo(&conn, None, "inbox").unwrap();
        set_todo_reminder(&conn, &inbox.id, Some("2025-01-01")).unwrap();

        assert_eq!(list_projects(&conn, false, None, None).unwrap()[0].soonest_reminder, None);
    }

    #[test]
    fn soonest_reminder_excludes_done_todos() {
        let conn = mk_conn();
        let p  = create_project(&conn, "P").unwrap();
        let t1 = create_todo(&conn, Some(&p.id), "Done task").unwrap();   // will be marked done
        let t2 = create_todo(&conn, Some(&p.id), "Active task").unwrap();

        set_todo_reminder(&conn, &t1.id, Some("2025-01-01")).unwrap(); // earlier, but done
        set_todo_reminder(&conn, &t2.id, Some("2025-06-01")).unwrap();

        cycle_todo_status(&conn, &t1.id).unwrap(); // New → InProgress
        cycle_todo_status(&conn, &t1.id).unwrap(); // InProgress → Done

        assert_eq!(
            get_project(&conn, &p.id).unwrap().soonest_reminder.as_deref(),
            Some("2025-06-01"),
        );
    }

    // ── Query counts (#10) ────────────────────────────────────────────────────
    //
    // These pin the shape of the fix, not just its output: `list_projects` and
    // `list_todos_global` must run a **constant** number of statements however
    // many rows they return. They are the functions `reload_project_list` /
    // `reload_todo_list` call on every keystroke of a search box, so a per-row
    // round trip is a per-keystroke round trip.

    #[test]
    fn list_projects_runs_a_constant_number_of_statements() {
        let conn = mk_conn();
        for i in 0..30 {
            let p = create_project(&conn, &format!("Project {i}")).unwrap();
            tag_project(&conn, &p.id, "work").unwrap();
            tag_project(&conn, &p.id, "urgent").unwrap();
            let t = create_todo(&conn, Some(&p.id), "Task").unwrap();
            set_todo_reminder(&conn, &t.id, Some("2025-06-01")).unwrap();
        }

        let (one, one_n)   = count_statements(&conn, || list_projects(&conn, false, None, None).unwrap());
        assert_eq!(one.len(), 30);
        assert_eq!(one[0].tags.len(), 2, "tags must still be populated");
        assert_eq!(one[0].soonest_reminder.as_deref(), Some("2025-06-01"),
                   "soonest_reminder must still be populated");

        // Two: the project rows (reminders inlined) and one bulk tag query.
        assert_eq!(
            one_n, 2,
            "list_projects must run exactly two statements (rows + bulk tags) for 30 projects"
        );
    }

    #[test]
    fn list_projects_statement_count_is_independent_of_row_count() {
        let conn = mk_conn();
        create_project(&conn, "one").unwrap();
        let (_, with_one) = count_statements(&conn, || list_projects(&conn, false, None, None).unwrap());

        for i in 0..20 { create_project(&conn, &format!("p{i}")).unwrap(); }
        let (_, with_many) = count_statements(&conn, || list_projects(&conn, false, None, None).unwrap());

        assert_eq!(
            with_one, with_many,
            "list_projects must not issue per-row statements (#10): \
             {with_one} statements for 1 project vs {with_many} for 21"
        );
    }

    #[test]
    fn list_todos_global_statement_count_is_independent_of_row_count() {
        let conn = mk_conn();

        let p = create_project(&conn, "only").unwrap();
        create_todo(&conn, Some(&p.id), "t").unwrap();
        let (_, with_one) =
            count_statements(&conn, || list_todos_global(&conn, false, None, None, false).unwrap());

        for i in 0..20 {
            let p = create_project(&conn, &format!("p{i}")).unwrap();
            create_todo(&conn, Some(&p.id), "t").unwrap();
            create_todo(&conn, None, "inbox").unwrap();
        }
        let (groups, with_many) =
            count_statements(&conn, || list_todos_global(&conn, false, None, None, false).unwrap());
        // 21 project bands plus the inbox band.
        assert_eq!(groups.len(), 22);
        assert_eq!(
            with_one, with_many,
            "list_todos_global must not issue per-project statements (#10): \
             {with_one} statements for 1 project vs {with_many} for 21"
        );
    }

    #[test]
    fn get_project_runs_a_constant_number_of_statements() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        for name in ["a", "b", "c", "d", "e"] { tag_project(&conn, &p.id, name).unwrap(); }
        for i in 0..5 {
            let t = create_todo(&conn, Some(&p.id), &format!("t{i}")).unwrap();
            set_todo_reminder(&conn, &t.id, Some("2025-06-01")).unwrap();
        }

        let (project, n) = count_statements(&conn, || get_project(&conn, &p.id).unwrap());
        assert_eq!(project.tags.len(), 5);
        assert_eq!(project.soonest_reminder.as_deref(), Some("2025-06-01"));
        assert!(n <= 3, "get_project ran {n} statements; expected a constant handful");
    }

    // ── Global Todo List ─────────────────────────────────────────────────────

    #[test]
    fn list_projects_keeps_each_projects_tags_alphabetical() {
        // The bulk tag query replaced a per-project `ORDER BY t.name`; the
        // grouping happens in Rust now, so the ordering has to be asserted.
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        for name in ["zebra", "alpha", "middle"] { tag_project(&conn, &p.id, name).unwrap(); }

        let projects = list_projects(&conn, false, None, None).unwrap();
        let names: Vec<&str> = projects[0].tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["alpha", "middle", "zebra"]);
    }

    #[test]
    fn list_projects_gives_untagged_projects_an_empty_tag_list() {
        let conn = mk_conn();
        create_project(&conn, "Tagged").unwrap();
        let bare = create_project(&conn, "Bare").unwrap();
        let tagged = list_projects(&conn, false, None, None).unwrap()[0].id.clone();
        tag_project(&conn, &tagged, "work").unwrap();
        assert_ne!(tagged, bare.id);

        let projects = list_projects(&conn, false, None, None).unwrap();
        let bare_row = projects.iter().find(|p| p.id == bare.id).unwrap();
        assert!(bare_row.tags.is_empty(), "a project with no tags must get an empty Vec");
    }

    #[test]
    fn list_projects_tag_filter_and_search_still_combine() {
        let conn = mk_conn();
        let work = create_project(&conn, "Work Project").unwrap();
        tag_project(&conn, &work.id, "work").unwrap();
        let play = create_project(&conn, "Play Project").unwrap();
        tag_project(&conn, &play.id, "play").unwrap();

        let tagged = list_projects(&conn, false, Some("work"), None).unwrap();
        assert_eq!(tagged.len(), 1);
        assert_eq!(tagged[0].id, work.id);
        assert_eq!(tagged[0].tags.len(), 1, "tags must survive a tag filter");
        assert_eq!(tagged[0].tags[0].name, "work");

        // Filter and search together.
        assert_eq!(list_projects(&conn, false, Some("work"), Some("work")).unwrap().len(), 1);
        assert_eq!(list_projects(&conn, false, Some("work"), Some("play")).unwrap().len(), 0);
    }

    #[test]
    fn list_projects_empty_result_issues_no_tag_query() {
        // Nothing to look tags up for; the bulk query must be skipped rather
        // than run with an empty `IN ()` (which SQLite rejects).
        let conn = mk_conn();
        create_project(&conn, "P").unwrap();
        let (projects, n) =
            count_statements(&conn, || list_projects(&conn, false, None, Some("no-such-title")).unwrap());
        assert!(projects.is_empty());
        assert_eq!(n, 1, "an empty result needs only the row query");
    }

    #[test]
    fn global_list_keeps_todo_order_within_each_band() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        for title in ["first", "second", "third"] { create_todo(&conn, Some(&p.id), title).unwrap(); }

        let groups = list_todos_global(&conn, false, None, None, false).unwrap();
        let titles: Vec<&str> = groups[0].todos.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["first", "second", "third"]);
    }

    #[test]
    fn global_list_excludes_archived_todos_unless_requested() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let keep = create_todo(&conn, Some(&p.id), "keep").unwrap();
        let gone = create_todo(&conn, Some(&p.id), "gone").unwrap();
        toggle_archive_todo(&conn, &gone.id).unwrap();

        let visible = list_todos_global(&conn, false, None, None, false).unwrap();
        assert_eq!(visible[0].todos.len(), 1);
        assert_eq!(visible[0].todos[0].id, keep.id);

        let all = list_todos_global(&conn, false, None, None, true).unwrap();
        assert_eq!(all[0].todos.len(), 2);
    }

    #[test]
    fn global_list_with_only_the_inbox() {
        // No projects at all: the `IN ()` clause must not be generated.
        let conn = mk_conn();
        create_todo(&conn, None, "inbox task").unwrap();

        let groups = list_todos_global(&conn, false, None, None, false).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].project_title, "Inbox");
        assert_eq!(groups[0].todos.len(), 1);
    }

    #[test]
    fn global_list_with_no_rows_at_all() {
        let conn = mk_conn();
        assert!(list_todos_global(&conn, false, None, None, false).unwrap().is_empty());
        // And with a tag filter, which takes the early-return path.
        assert!(list_todos_global(&conn, false, None, Some("none"), false).unwrap().is_empty());
    }

    #[test]
    fn global_list_tag_filter_skips_the_todo_query_when_no_project_matches() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        tag_project(&conn, &p.id, "work").unwrap();
        create_todo(&conn, Some(&p.id), "t").unwrap();
        create_todo(&conn, None, "inbox").unwrap();

        let (groups, n) = count_statements(&conn, || {
            list_todos_global(&conn, false, None, Some("no-such-tag"), false).unwrap()
        });
        assert!(groups.is_empty());
        // One statement: the project rows. With no projects there is nothing to
        // load tags or todos for, so both follow-up queries are skipped.
        assert_eq!(n, 1, "a filtered-out project list needs only the row query");
    }

    #[test]
    fn global_list_inbox_group_appears_first() {
        let conn = mk_conn();
        let p = create_project(&conn, "My Project").unwrap();
        create_todo(&conn, Some(&p.id), "Project Task").unwrap();
        create_todo(&conn, None, "Inbox Task").unwrap();

        let groups = list_todos_global(&conn, false, None, None, false).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].project_title, "Inbox");
        assert_eq!(groups[1].project_title, "My Project");
    }

    #[test]
    fn global_list_hide_done_filters_out_completed_todos() {
        let conn = mk_conn();
        let p  = create_project(&conn, "P").unwrap();
        let t1 = create_todo(&conn, Some(&p.id), "Active").unwrap();
        let t2 = create_todo(&conn, Some(&p.id), "Done").unwrap();

        cycle_todo_status(&conn, &t2.id).unwrap(); // New → InProgress
        cycle_todo_status(&conn, &t2.id).unwrap(); // InProgress → Done

        let with_done    = list_todos_global(&conn, false, None, None, false).unwrap();
        let without_done = list_todos_global(&conn, true,  None, None, false).unwrap();

        assert_eq!(with_done[0].todos.len(), 2);
        assert_eq!(without_done[0].todos.len(), 1);
        assert_eq!(without_done[0].todos[0].id, t1.id);
    }

    #[test]
    fn global_list_search_filter_matches_by_title() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        create_todo(&conn, Some(&p.id), "Buy groceries").unwrap();
        create_todo(&conn, Some(&p.id), "Call dentist").unwrap();
        create_todo(&conn, None, "Buy flowers").unwrap();

        let results = list_todos_global(&conn, false, Some("buy"), None, false).unwrap();
        let total: usize = results.iter().map(|g| g.todos.len()).sum();
        assert_eq!(total, 2);

        // Project group should only include the matching todo.
        let project_group = results.iter().find(|g| g.project_title == "P").unwrap();
        assert_eq!(project_group.todos.len(), 1);
        assert_eq!(project_group.todos[0].title, "Buy groceries");
    }

    #[test]
    fn global_list_tag_filter_excludes_inbox_group() {
        let conn = mk_conn();
        let p = create_project(&conn, "Tagged Project").unwrap();
        tag_project(&conn, &p.id, "work").unwrap();
        create_todo(&conn, Some(&p.id), "Project Task").unwrap();
        create_todo(&conn, None, "Inbox Task").unwrap();

        let results = list_todos_global(&conn, false, None, Some("work"), false).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].project_title, "Tagged Project");
    }

    #[test]
    fn global_list_excludes_projects_with_no_matching_todos() {
        let conn = mk_conn();
        let p_with    = create_project(&conn, "With Tasks").unwrap();
        let _p_empty  = create_project(&conn, "Empty Project").unwrap();
        create_todo(&conn, Some(&p_with.id), "Task").unwrap();

        let groups = list_todos_global(&conn, false, None, None, false).unwrap();
        let titles: Vec<&str> = groups.iter().map(|g| g.project_title.as_str()).collect();
        assert!(!titles.contains(&"Empty Project"));
        assert!(titles.contains(&"With Tasks"));
    }
}
