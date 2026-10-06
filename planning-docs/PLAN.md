# Project Management TUI — Design Plan

## 1. Language & Library Recommendation

### Primary Recommendation: **Rust + ratatui**

| Concern | Choice | Rationale |
|---|---|---|
| Language | Rust | Memory safety without GC, excellent CLI/TUI ecosystem, cargo for dependencies |
| Terminal rendering | [ratatui](https://ratatui.rs/) | De-facto standard Rust TUI library; supports arbitrary widget placement, immediate-mode rendering |
| Terminal backend | [crossterm](https://github.com/crossterm-rs/crossterm) | Cross-platform (Linux/macOS/Windows) terminal control; ratatui uses it by default |
| Storage | [rusqlite](https://github.com/rusqlite/rusqlite) | SQLite bindings; perfect for local structured data with ad-hoc queries |
| Editor integration | [similar-string / external $EDITOR] | For multi-line text, drop into `$EDITOR` like many TUI tools |

### Alternative: **C + ncurses**
Viable and simpler runtime. ncurses gives precise control, but you own memory management, string handling, and data structures. SQLite has a first-class C API. Choose this path if minimal runtime size and zero external toolchain are priorities.

### Colors
Choose colors that will look good for a "daytime" theme. The terminal background will be white. All colors should be very readable against white with good contrast.

---

## 2. Data Model

### Entity Relationship

```
Tag ──< ProjectTag >── Project ──< Update
                           │
                           ├──< Todo
                           │
                           └──< Attachment (parent_type = project)

Tag ──< ReferenceTag >── Reference ──< Attachment (parent_type = reference)

Todo ──< Attachment (parent_type = todo)   [optional, future]
```

### Core Tables (SQLite schema)

```sql
CREATE TABLE tags (
    id          TEXT PRIMARY KEY,   -- UUID
    name        TEXT NOT NULL UNIQUE,
    color       TEXT                -- e.g. "#ff5f00" or ANSI color name
);

CREATE TABLE projects (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    description TEXT DEFAULT '',
    sort_order  REAL NOT NULL DEFAULT 0.0,  -- fractional index for drag-to-reorder
    archived    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE project_tags (
    project_id  TEXT REFERENCES projects(id) ON DELETE CASCADE,
    tag_id      TEXT REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (project_id, tag_id)
);

CREATE TABLE updates (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL  -- sorted newest-first (DESC) in all read queries
);

CREATE TABLE todos (
    id          TEXT PRIMARY KEY,
    project_id  TEXT REFERENCES projects(id) ON DELETE SET NULL,  -- NULL = inbox
    title       TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'new',  -- new | in_progress | canceled | done
    reminder    TEXT,           -- ISO date, nullable
    sort_order  REAL NOT NULL DEFAULT 0.0,   -- within project
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE references_table (   -- "references" is a reserved word in SQL
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL DEFAULT '',
    sort_order  REAL NOT NULL DEFAULT 0.0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE reference_tags (
    reference_id TEXT REFERENCES references_table(id) ON DELETE CASCADE,
    tag_id       TEXT REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (reference_id, tag_id)
);

CREATE TABLE attachments (
    id           TEXT PRIMARY KEY,
    parent_id    TEXT NOT NULL,
    parent_type  TEXT NOT NULL,  -- 'project' | 'todo' | 'reference'
    filename     TEXT NOT NULL,
    local_path   TEXT NOT NULL,  -- relative to data dir
    mime_type    TEXT,
    created_at   TEXT NOT NULL
);
```

### Todo Status Characters

| Status | Char | Display |
|---|---|---|
| New | `-` | `- Task text here` |
| In Progress | `o` | `o Task text here` |
| Canceled | `x` | `x Task text here` |
| Done | `✓` | `✓ Task text here` |

---

## 3. Priority & Sort System

**Core invariant:** A project's priority cascades down. When you reprioritize a project, all its todos float up or down in the global view automatically.

### Fractional Indexing

Use floating-point `sort_order` values. When inserting between two items with sort orders `a` and `b`, assign `(a + b) / 2`. This avoids renumbering the entire list on every drag. Only renumber (reset to integers 1.0, 2.0, …) when precision exhausts (values differ by < 1e-10).

### Global Todo Sort Key

```
effective_priority(todo) = (project.sort_order, todo.sort_order)
```

Sort ascending (lower number = higher priority). Todos in the same project rank amongst themselves; moving a project up moves all its todos above another project's todos. **Todos cannot be dragged across project boundaries in the global view** — only within their project's band.

### Inbox

The inbox is `todos` rows where `project_id IS NULL`. It has an implicit `sort_order` just like any project. In the global view, the inbox can be treated as a virtual project at a configurable priority position (default: top or bottom, user-settable).

---

## 4. Application Architecture

```
src/
├── main.rs              — entry point: init terminal, run event loop, restore terminal
├── app.rs               — App struct: current view stack, shared state, action dispatch
├── data/
│   ├── db.rs            — SQLite connection, migrations
│   ├── models.rs        — Plain data structs (Project, Todo, Reference, Tag, …)
│   └── repo.rs          — CRUD + query functions (no UI dependencies)
├── ui/
│   ├── theme.rs         — Colors, styles (supports 256-color and true-color)
│   ├── layout.rs        — Common layout helpers
│   ├── views/
│   │   ├── project_list.rs
│   │   ├── project_detail.rs
│   │   ├── todo_list.rs       — global todo list
│   │   ├── reference_list.rs
│   │   └── reference_detail.rs
│   └── components/
│       ├── status_bar.rs      — bottom bar: current view name + key hints
│       ├── search_box.rs      — inline /-search widget
│       ├── tag_filter.rs      — tag picker overlay
│       ├── project_picker.rs  — move-todo destination picker overlay
│       ├── quick_capture.rs   — inbox hotkey modal
│       └── confirm_dialog.rs
├── input/
│   └── keys.rs          — KeyEvent → Action enum mapping, per-view key tables
└── sync/                — (Phase 3, future)
    └── mod.rs
```

### View Stack

Navigation is a simple stack. `ESC` pops. Entering a project pushes `ProjectDetail`. Opening an editor for a field opens `$EDITOR` in the raw terminal (suspend ratatui, exec editor, resume ratatui on return).

```
[ProjectList]
      ↓ Enter
[ProjectDetail]
      ↓ u (edit update)
  $EDITOR (external)
```

### Event Loop (sketch)

```
loop {
    terminal.draw(|frame| current_view.render(frame, app_state))?;
    let event = crossterm::event::read()?;
    let action = keys::handle(event, current_view.id());
    app_state.dispatch(action, &mut db)?;
}
```

---

## 5. Views & Keybindings

### 5.0 Cursor & Selection (applies to all list views)

Every list view has a **cursor** — one row is always selected. The selected row
is highlighted (e.g. bold text + accent background color). `j`/`k` move the
cursor. All action keys (`Enter`, `e`, `s`, `a`, `d`, …) operate on the
currently selected item.

#### Reordering

No drag mode needed. Reordering uses two always-available keys:

- `J` (Shift+j) — move selected item **down** one position
- `K` (Shift+k) — move selected item **up** one position

Each press is an immediate committed move; `sort_order` is updated on the spot.
The cursor follows the item. To undo an accidental move, just press the
opposite key. No mode to enter or exit, no extra state to track.

---

### 5.1 Project List  *(default view)*

```
┌─────────────────────────────────────────────────────┐
│ Projects                          [tag: work] [all] │
├─────────────────────────────────────────────────────┤
│ Build analytics dashboard                           │
│=Rewrite auth service==================2025-08-01====│  ← cursor (selected; will be shown as bold text on shaded background)
│ Migrate database                      2025-07-20    │
│ [archived: 2 hidden]                                │
├─────────────────────────────────────────────────────┤
│ j/k:move  J/K:reorder  Enter:open  n:new  /:search  │
│ T:tags  a:archive  A:show-archived  D:delete        │
│ Tab:todos  i:capture  q:quit                        │
└─────────────────────────────────────────────────────┘
```

| Key | Action |
|---|---|
| `j` / `k` | Move cursor down / up |
| `J` / `K` | Move selected item down / up (reorder) |
| `Enter` | Open selected project detail |
| `n` | New project (inline title entry) |
| `/` | Search project titles (incremental) |
| `T` | Tag filter picker |
| `a` | Toggle archive status of selected project |
| `A` | Toggle show/hide archived |
| `D` | Delete selected (with confirm dialog) |
| `i` | Quick-capture inbox item (global, works in all views) |
| `t` | Switch to Global Todo view |
| `l` | Switch to Reference Library list |
| `Q` | Quit |
| `?` | Show all keyboard actions |

### 5.2 Project Detail

The screen has a fixed **header** (always visible) and four **tabs** whose content fills the remaining space.

#### Layout

```
┌──────────────────────────────────────────────────────┐
│ Rewrite auth service                          [work] │
│  Description   Updates  [Todos]  Attachments         │  ← tab bar; active tab highlighted
├──────────────────────────────────────────────────────┤
│   - Design Paseto schema                             │
│   o Audit existing JWT usage             2025-07-20  │
│   ✓ Write ADR                                        │
│                                                      │
├──────────────────────────────────────────────────────┤
│ ESC:back  E:title  T:tags  d/u/t/a:tab  j/k:nav      │
│ n:new  e:edit  s:status  J/K:reorder  m:move         │
│ r:reminder  D:delete                                 │
└──────────────────────────────────────────────────────┘
```

- **Row 1:** Project title. `E` (Shift+e) opens inline title editing.
- **Row 2:** Tags row + tab bar. Tags are displayed left; the four tab labels are shown in a single row. The active tab is highlighted. `T` (Shift+t) opens the tag picker overlay.

**Note:** archiving a project is done from the **Project List** screen only (press `ESC` to return, then `a`). This frees the `a` key for the Attachments tab.

#### Tab switching keys

| Key | Tab |
|---|---|
| `d` | Description |
| `u` | Updates |
| `t` | Todos |
| `a` | Attachments |

#### Keybindings (all tabs)

| Key | Action |
|---|---|
| `ESC` | Return to project list |
| `E` | Edit project title inline |
| `T` | Edit tags (tag picker overlay) |
| `d` / `u` / `t` / `a` | Switch to Description / Updates / Todos / Attachments tab |
| `j` / `k` | Move cursor within current tab |
| `J` / `K` | Move item's position in the list within current tab (for supported types) |
| `n` | Create new item in current tab |
| `e` | Edit selected item in current tab |
| `D` | Delete selected item (with confirm dialog) |
| `i` | Quick-capture inbox todo item (global) |

---

#### Description tab

Displays the full project description text (wrapping, not truncated). If empty, shows a placeholder.

| Key | Action |
|---|---|
| `e` | Edit description in `$EDITOR` |

(`n` behaves the same as `e`; useful when the description is empty.)

---

#### Updates tab

Updates are rendered in a **two-column layout** with no dividing border line between the columns.

```
┌──────────────────────────────────────────────────────┐
│ Rewrite auth service                          [work] │
│  Description  [Updates]  Todos  Attachments          │
├──────────────────────────────────────────────────────┤
│=2025-07-10===Kicked off design doc review; got=======│  ← selected: first line highlighted
│              stakeholder buy-in from both teams.     │
│                                                      │
│ 2025-07-08   Alignment meeting done                  │
│                                                      │
│ Post-mortem  Long label wraps within its column;     │
│ note         text column wraps here too.             │
├──────────────────────────────────────────────────────┤
│ ESC:back  E:title  T:tags  d/u/t/a:tab  j/k:nav      │
│ n:new  e:edit  D:delete                              │
└──────────────────────────────────────────────────────┘
```

**Column layout:**
- **Date/label column** (left): fixed width of 12 characters (accommodates a `YYYY-MM-DD` date plus 2 spaces padding). Content wraps within the column if longer than 12 characters; never truncates.
- **Text column** (right): fills the remaining terminal width. Wraps as needed; never truncates.
- A blank line separates consecutive Update entries for visual breathing room.
- The date/label is **top-aligned** in its column regardless of how many lines the text wraps to.

**Cursor/selection:** Only the **first rendered line** of the selected entry is highlighted (spanning the full terminal width). Continuation lines from wrapping are not highlighted.

**Creating updates:** pressing `n` opens `$EDITOR` with a template:

```
Date: 2025-07-10
---
(write your update here)
```

The `Date:` line is pre-filled with today's date (`YYYY-MM-DD`). The user may change it to any text — the value is not validated and is not constrained to a date format. Everything after the blank line becomes the body. Saving creates the new Update entry.

**Editing updates:** pressing `e` on a selected entry opens `$EDITOR` with the date/label and body pre-filled in the same `date: …` + blank line + body format. Saving overwrites the entry.

**Data model change:** the `updates` table needs a user-editable `label` column separate from the immutable `created_at` timestamp:

```sql
ALTER TABLE updates ADD COLUMN label TEXT NOT NULL DEFAULT '';
-- On creation, label is initialized to today's date (YYYY-MM-DD).
-- Thereafter it is fully user-editable and not constrained to a date.
```

| Key | Action |
|---|---|
| `n` | Add new update (opens `$EDITOR`; date auto-populated in template) |
| `e` | Edit selected update (date/label + body) in `$EDITOR` |
| `D` | Delete selected update (with confirm dialog) |

---

#### Todos tab

Same todo list behaviour as the current Project Detail implementation.

| Key | Action |
|---|---|
| `n` | Add new todo (inline title entry) |
| `e` | Edit selected todo title inline |
| `s` | Cycle todo status (`-` → `o` → `✓` → `x`) |
| `J` / `K` | Reorder todo down / up |
| `m` | Move todo to another project (project picker overlay) |
| `r` | Set / edit reminder date |
| `D` | Delete selected todo (with confirm dialog) |

---

#### Attachments tab

Lists files attached to this project.

| Key | Action |
|---|---|
| `n` | Add attachment (file path prompt) |
| `e` | Open selected attachment with OS viewer (`xdg-open` / `open`) |
| `D` | Delete selected attachment (removes file from data dir, with confirm) |

---

### 5.3 Global Todo List

```
┌─────────────────────────────────────────────────────┐
│ All Todos                          [tag: —] [active]│
├─────────────────────────────────────────────────────┤
│ ── Rewrite auth service ──────────────────────────  │
│   o Audit existing JWT usage            2025-07-20  │
│   - Design Paseto schema                            │
│ ── Build analytics dashboard ─────────────────────  │
│   - Define KPI dashboard layout                     │
│   - Hook up BigQuery source             2025-07-25  │
│ ── Inbox ────────────────────────────────────────── │
│   - Call dentist                                    │
├─────────────────────────────────────────────────────┤
│ j/k:move  J/K:reorder  s:status  e:edit  D:delete   │
│ n:new  r:reminder  m:move  /:search  T:tags         │
│ c:hide-done  Enter:open-project  Tab:projects       │
│ i:capture  q:quit                                   │
└─────────────────────────────────────────────────────┘
```

| Key | Action |
|---|---|
| `j`/`k` | Move cursor |
| `s` | Cycle status of selected todo (`-` → `o` → `x` / `✓`) |
| `e` | Edit selected todo title (inline) |
| `r` | Set/edit reminder date on selected todo |
| `n` | New todo (goes to inbox) |
| `J` / `K` | Move selected todo down / up within its project band (reorder) |
| `C` | Toggle hide completed/canceled |
| `/` | Search todo text |
| `T` | Tag filter (filters by project tag) |
| `m` | Move selected todo to a different project (or Inbox) |
| `Enter` | Jump to owning project detail |
| `i` | Quick-capture inbox item |
| `D` | Delete selected todo (with confirm dialog) |
| `p` | Switch to Project List |
| `Q` | Quit |
| `?` | Show all keyboard actions |

### 5.4 Reference List & Detail

Mirror of Project List / Project Detail but simpler:
- No todos, no updates
- Body is a free-form markdown/text document edited in `$EDITOR`
- Tags and attachments work identically

### 5.5 Move-to-Project Picker (`m`)

Available whenever a todo is selected (Global Todo view or Project Detail todo list).
Opens a small overlay picker — identical UX to the tag picker.

```
┌──────────────────────────────────┐
│ Move to…                         │
│ > auth_                          │
├──────────────────────────────────┤
│   Rewrite auth service           │
│   Build analytics dashboard      │
│   ── Inbox ──                    │
└──────────────────────────────────┘
```

- **Type to filter** — incremental search narrows the project list.
- **Inbox** is always listed as a destination (sets `project_id = NULL`).
- The todo's **current** project/Inbox is shown greyed-out and non-selectable.
- **Enter** commits the move: updates `project_id` and appends the todo at the
  end of the destination's `sort_order` band.
- **ESC** cancels with no change.
- Archived projects are excluded from the picker.

---

### 5.6 Quick Capture Modal (global `i`)

```
┌─────────────────────────────┐
│ Quick capture → Inbox       │
│ > _                         │
│ Enter:save  ESC:cancel      │
└─────────────────────────────┘
```

Appends a new todo to the inbox without leaving the current view.

---

## 6. Tags

### Global tag namespace

Tags are **shared across all entity types**. The `tags` table is a single pool;
`project_tags` and `reference_tags` both reference it. A tag first created on a
project (e.g. "Customer A") is immediately available when tagging a reference,
and vice versa. There is no such thing as a "project tag" vs a "reference tag" —
just tags.

### Tagging and untagging

Tags are never created or deleted explicitly by the user. The only operations
are **tag** (add a tag to a resource) and **untag** (remove a tag from a
resource). Tag rows are managed implicitly:

- **Tag**: if the named tag does not exist in `tags`, create it; then insert
  the join-table row. If the tag already exists, just insert the join row.
- **Untag**: remove the join-table row. Then check whether any join table
  still references that tag; if none do, delete the tag row. The user never
  sees this cleanup — orphaned tags simply disappear from autocomplete.

```sql
-- tag
INSERT OR IGNORE INTO tags (id, name) VALUES (?, ?);
INSERT OR IGNORE INTO project_tags (project_id, tag_id) VALUES (?, ?);

-- untag (then garbage-collect)
DELETE FROM project_tags WHERE project_id = ? AND tag_id = ?;
DELETE FROM tags WHERE id = ?
  AND NOT EXISTS (SELECT 1 FROM project_tags   WHERE tag_id = ?)
  AND NOT EXISTS (SELECT 1 FROM reference_tags WHERE tag_id = ?);
```

### Tag picker UI

Whenever the user opens the tag editor on any entity, an inline picker appears:

- **Type to filter** — autocompletes against the full `tags` table (all entity
  types). Matches shown as the user types.
- **Confirm selection** — associates the tag (creating the tag row if needed).
- **Confirm unknown name** — creates the tag and associates it in one step.
- **Select associated tag + `DEL`** — untagging (with implicit garbage-collect).
- **Highlight any tag + `r`** — rename it in place; the `tags` row is updated
  and all associations follow automatically (they reference by ID).
- **Highlight any tag + `c`** — set or change its display color.

No separate “manage tags” screen is needed.

### Todo tag inheritance

Todos do not have their own tag rows. They inherit the tags of their parent
project. In the global todo view, filtering by tag means: *show todos whose
project carries that tag*. Inbox todos (no project) are untagged and appear
only when no tag filter is active (or when the inbox is explicitly included).

---

## 8. Reminder Display

The soonest reminder date across all active todos bubbles up to the project list line:

```
 Rewrite auth service                2025-07-20
```

Color-code by urgency:
- Red: overdue
- Yellow: within 7 days
- Normal: further out

---

## 9. Attachment Handling

Attachments are files copied into a local data directory, e.g. `~/.local/share/ptm/attachments/<uuid>_<filename>`. On open, the app calls `xdg-open` (Linux), `open` (macOS), or `start` (Windows) so the OS picks the appropriate viewer. No content is read or rendered by the app itself.

---

## 10. Storage Layout

```
~/.local/share/ptm/           (or XDG_DATA_HOME/ptm)
├── ptm.db                    SQLite database
└── attachments/
    ├── a3f2…_design-doc.pdf
    └── b91c…_screenshot.png

~/.config/ptm/
└── config.toml               theme, editor override, inbox priority position
```

---

## 11. Phased Roadmap

### Phase 1 — Core (local, no attachments)
- [v] SQLite schema + migrations
- [v] Project CRUD (list, create, edit, archive, delete)
- [v] Todo CRUD with status cycling
- [v] Priority drag-to-reorder with fractional indexing
- [v] Global todo view with project-band grouping
- [v] Search (incremental, in-view)
- [v] Tag management
- [v] Quick-capture inbox modal
- [v] Move todo between projects / to Inbox (project picker overlay)
- [v] `$EDITOR` integration for long text fields

### Phase 2 — Polish & References
- [ ] Reference list + detail view
- [ ] Attachment support
- [v] Reminder dates + color urgency
- [v] Reminder bubble-up to project list
- [x] Date hotkey (insert today's date in any text field) - May not need this. Maybe prepopulate reminder with today, or a week out
- [x] 256-color theming + config file - I think we're good
- [v] Proper terminal resize handling - Seems good after Phase 1

### Phase 3 — Cloud Sync
Options to evaluate:
- **Litestream** — continuous SQLite replication to S3/R2; transparent, no schema changes
- **Turso (libSQL)** — hosted SQLite with embedded replica sync; minimal code change
- **Custom API** — REST/GraphQL backend + CRDT or last-write-wins merge; most control, most work
- **Local-first with conflict UI** — keep offline-first, surface merge conflicts in a dedicated review screen

Recommended path: start with Turso for near-zero operational overhead while keeping SQLite semantics locally.

---

## 12. Open Questions / Decisions Needed

1. **Language**: Rust (safer, richer ecosystem) vs C (simpler runtime, fewer abstractions)?
   ANSWER: Rust
2. **Multi-line body editing**: Always `$EDITOR`, or embed a simple editor widget (significant extra work)?
   ANSWER: always `$EDITOR`
3. **Inbox position in global sort**: Fixed top, fixed bottom, or user-configurable priority?
   ANSWER: always Top in todo view, probably it doesn't even show in project view
4. **Markdown rendering**: Render reference bodies with basic markdown (bold, italic, headings) in the detail view, or treat as plain text?
   ANSWER: Yes, render Markdown
5. ~~**Drag UX**~~ — Resolved: `j`/`k` moves cursor; `J`/`K` moves the selected item (immediate, no mode).
6. **Reminder notifications**: Out of scope for TUI (the tool only shows them), or spawn a background daemon?
   ANSWER: No active notification
7. **Windows support**: Target or explicitly skip?
   ANSWER: Skip
