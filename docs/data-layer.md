# Data Layer (`repo.rs`)

All database I/O goes through functions in `repo.rs`. No view or `app.rs`
code runs SQL directly.

## Conventions

- IDs are UUID v4 strings (`Uuid::new_v4().to_string()`).
- Timestamps are UTC ISO 8601: `"%Y-%m-%dT%H:%M:%SZ"`.
- Sort order uses fractional indexing (`f64`). Reordering calls
  `repo::swap_project_order`, `repo::swap_todo_order`, or
  `repo::swap_update_order`, swapping the `sort_order` values of two
  adjacent rows. (There is no `reorder_project` / `reorder_todo`.)
- `todos.project_id IS NULL` means the todo is in the **Inbox**.
- Tags are shared globally. `repo::untag_project` garbage-collects orphaned
  tag rows automatically — never delete a `tags` row directly.

## Computed columns

`soonest_reminder` on `Project` bubbles up from todos: it is computed with a
subquery (`MIN(reminder)` over active — i.e. non-done and non-canceled — todos
only) and is **not** a stored column. The subquery exists once, as `SOONEST_REMINDER_COL`, inlined into the
project `SELECT`s. **Do not add a second definition.** See
`load_soonest_reminder` in `repo.rs`.

## List queries must issue a constant number of statements

`list_projects` and `list_todos_global` are re-run on every keystroke of a
search box (`reload_project_list` / `reload_todo_list`), so a per-row query is
a per-keystroke query.

The shapes to copy:

- **A per-row computed column** → inline a correlated subquery
  (`SOONEST_REMINDER_COL`).
- **A per-row collection** → load it for the whole result set in one
  `IN (…)` query (`load_tags_for_projects`), never one query per row.

`dev-harness/query-count.py` counts statements with SQLite's
`SQLITE_TRACE_STMT`; the `repo` tests pin the counts at 1 vs 30 rows.

## Errors are propagated, not defaulted away

A fallible lookup whose failure is collapsed into a "not found" / "no
conflict" answer silently changes behaviour — `rename_tag` used to do exactly
that. Where a caller must distinguish a *user* refusal from a *database*
failure, return a typed error (`RenameTagError::NameTaken` vs
`RenameTagError::Db`) instead of one string.

## Schema changes

- Use `CREATE TABLE IF NOT EXISTS`; running the schema again on an existing
  database must be a no-op.
- There is **no migration version table**. `src/data/db.rs::migrate` guards
  each step individually.
- For column additions, SQLite does **not** support `ADD COLUMN IF NOT EXISTS` (that clause is not
  part of SQLite's grammar for `ALTER TABLE`). Guard the migration with
  `SELECT COUNT(*) FROM pragma_table_info('<table>') WHERE name = '<col>'` and
  only run the `ALTER TABLE … ADD COLUMN` when the count is 0.
- `ptm-sync --merge` carries its own mirror of these guarded migrations
  (`ensure_schema_compat`) because it must bring a *foreign* database up to
  date. **Keep the two in step** — see [ptm-sync.md](ptm-sync.md#merge-mode).

## List pagination

ratatui's `List` scrolls by whole items: a multi-row item that overflows the
bottom is dropped entirely (leaving a blank gap), and footer rows appended
after the items (e.g. the "N archived — A to show" hint) can be pushed past the
bottom edge.

All list views therefore paginate themselves: build the items as
`Vec<Vec<Line>>`, compute the visible window with
`views::list_window(&heights, area_height, selected)`, render the `List` into
exactly `rows_used` rows, and fill any leftover rows with a "peek" at the next
item's first lines (see `project_list.rs`, `todo_list.rs`, and
`render_todos_tab` / `render_updates_tab` in `project_detail.rs`).

**Do not render a list widget straight into the full content area with raw
`ListItem`s.**

## Two list-state traps

**`tl_cursor` is a flat todo index** — it counts only todo rows, not group
header rows. Use `App::tl_current_todo()` / `tl_todo_at(flat_idx)` rather than
indexing `tl_groups` directly.

**A project created under a tag filter inherits that tag.** The inline create
handler in `handle_key_project_list` calls `repo::tag_project` with
`self.tag_filter` after `repo::create_project`, so the new project does not
vanish from the filtered list. With no filter active nothing is tagged.

Two consequences for that handler:

- The cursor must be found by id (`position(|p| p.id == created.id)`), **never
  `len - 1`** — with a filter or search the list does not end with the new
  project.
- `tag_project` is case-insensitive, so it reuses the filter's tag row instead
  of creating a case-variant duplicate.
