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

fn load_project_tags(conn: &Connection, project_id: &str) -> Result<Vec<Tag>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.color FROM tags t
         JOIN project_tags pt ON pt.tag_id = t.id
         WHERE pt.project_id = ?1
         ORDER BY t.name",
    )?;
    let tags = stmt
        .query_map(params![project_id], |row| {
            Ok(Tag { id: row.get(0)?, name: row.get(1)?, color: row.get(2)? })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(tags)
}

fn load_soonest_reminder(conn: &Connection, project_id: &str) -> Result<Option<String>> {
    let r: rusqlite::Result<Option<String>> = conn.query_row(
        "SELECT MIN(reminder) FROM todos
         WHERE project_id = ?1 AND reminder IS NOT NULL
           AND status NOT IN ('done','canceled')",
        params![project_id],
        |row| row.get(0),
    );
    Ok(r.unwrap_or(None))
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

    let sql = format!(
        "SELECT id, title, description, sort_order, archived, created_at, updated_at
         FROM projects p
         WHERE 1=1 {archived_clause} {tag_clause}
         ORDER BY p.sort_order ASC, p.created_at ASC"
    );

    let mut stmt = conn.prepare(&sql)?;

    type Row7 = (String, String, String, f64, bool, String, String);

    let rows: Vec<Row7> = if let Some(tag) = tag_filter {
        stmt.query_map(params![tag], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?,
                row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
        })?.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?,
                row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
        })?.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let search_lower = search.map(|s| s.to_lowercase());
    let mut projects = Vec::new();

    for (id, title, description, sort_order, archived, created_at, updated_at) in rows {
        if let Some(ref q) = search_lower {
            if !title.to_lowercase().contains(q.as_str()) {
                continue;
            }
        }
        let tags = load_project_tags(conn, &id)?;
        let soonest_reminder = load_soonest_reminder(conn, &id)?;
        projects.push(Project {
            id, title, description, sort_order, archived,
            created_at, updated_at, tags, soonest_reminder,
        });
    }
    Ok(projects)
}

pub fn get_project(conn: &Connection, id: &str) -> Result<Project> {
    let (title, description, sort_order, archived, created_at, updated_at) = conn.query_row(
        "SELECT title, description, sort_order, archived, created_at, updated_at
         FROM projects WHERE id = ?1",
        params![id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?,
                   row.get(3)?, row.get(4)?, row.get(5)?)),
    )?;
    let tags = load_project_tags(conn, id)?;
    let soonest_reminder = load_soonest_reminder(conn, id)?;
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

/// Rename a tag globally (affects every project using it).  Returns an error
/// message string (not a rusqlite error) if the name is already taken.
pub fn rename_tag(conn: &Connection, tag_id: &str, new_name: &str) -> Result<()> {
    // Reject if another tag already has that name (case-insensitive).
    let collision: bool = conn.query_row(
        "SELECT COUNT(*) FROM tags WHERE LOWER(name) = LOWER(?1) AND id != ?2",
        params![new_name, tag_id],
        |r| r.get::<_, u32>(0),
    ).map(|n| n > 0).unwrap_or(false);
    if collision {
        // Signal collision via a rusqlite QueryReturnedNoRows (sentinel).
        // Callers check for this via a wrapper that converts to a user message.
        anyhow::bail!("tag name already exists");
    }
    conn.execute(
        "UPDATE tags SET name = ?1 WHERE id = ?2",
        params![new_name, tag_id],
    )?;
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
    let status_clause   = if hide_done       { " AND status NOT IN ('done','canceled')" } else { "" };
    let archived_clause = if include_archived { "" } else { " AND archived = 0" };
    let search_lower  = search.map(|s| s.to_lowercase());

    let mut groups: Vec<TodoGroup> = Vec::new();

    // Inbox (only shown when no tag filter is active)
    if tag_filter.is_none() {
        let sql = format!(
            "SELECT id, project_id, title, status, reminder, sort_order, created_at, updated_at, archived
             FROM todos WHERE project_id IS NULL{status_clause}{archived_clause} ORDER BY sort_order ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut todos: Vec<Todo> = stmt
            .query_map([], row_to_todo)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if let Some(ref q) = search_lower {
            todos.retain(|t| t.title.to_lowercase().contains(q.as_str()));
        }
        if !todos.is_empty() {
            groups.push(TodoGroup { project_id: None, project_title: "Inbox".to_string(), todos });
        }
    }

    // Active projects in priority order
    let projects = list_projects(conn, false, tag_filter, None)?;
    for project in projects {
        let sql = format!(
            "SELECT id, project_id, title, status, reminder, sort_order, created_at, updated_at, archived
             FROM todos WHERE project_id = ?1{status_clause}{archived_clause} ORDER BY sort_order ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut todos: Vec<Todo> = stmt
            .query_map(params![project.id], row_to_todo)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if let Some(ref q) = search_lower {
            todos.retain(|t| t.title.to_lowercase().contains(q.as_str()));
        }
        if !todos.is_empty() {
            groups.push(TodoGroup {
                project_id: Some(project.id),
                project_title: project.title,
                todos,
            });
        }
    }

    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::db;
    use crate::data::models::TodoStatus;

    fn mk_conn() -> rusqlite::Connection {
        db::open_in_memory().expect("in-memory db")
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

    #[test]
    fn rename_tag_rejects_duplicate_name() {
        let conn = mk_conn();
        let p = create_project(&conn, "P").unwrap();
        let id1 = tag_project(&conn, &p.id, "tag-a").unwrap();
        tag_project(&conn, &p.id, "tag-b").unwrap();

        let err = rename_tag(&conn, &id1, "tag-b").unwrap_err();
        assert!(err.to_string().contains("already exists"));
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

    // ── Global Todo List ─────────────────────────────────────────────────────

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
