# AGENTS.md — ptm (Project & Task Manager)

A keyboard-driven terminal UI (TUI) for managing projects, todos, and journal
updates. Written in Rust using ratatui + crossterm; all data is stored locally
in SQLite via rusqlite.

---

## Build & Run

```bash
cargo build            # debug build  → target/debug/ptm  target/debug/ptm-sync
cargo build --release  # release build → target/release/ptm  target/release/ptm-sync
cargo run              # build + run debug binary
./build                # for the user only. Build via Docker
./run                  # for the user only. Run the build container interactively
```

No configuration is required on first launch. The database is created
automatically at `~/.local/share/ptm/ptm.db` (or `$XDG_DATA_HOME/ptm/ptm.db`).

---

## Repository Layout

```
src/
├── main.rs          — terminal setup, event loop, $EDITOR suspension
├── app.rs           — App struct (all state), handle_key_* methods
├── log.rs           — file-based logger (see Logging section below)
├── data/
│   ├── mod.rs       — re-exports db, models, repo
│   ├── db.rs        — SQLite connection, schema (CREATE TABLE IF NOT EXISTS)
│   ├── models.rs    — plain data structs: Project, Todo, Update, Tag, TodoStatus
│   └── repo.rs      — all database reads/writes; no UI dependencies
├── input/
│   └── mod.rs       — cursor-aware single-line `InputState` struct + `handle_input_key`
├── ui/
│   ├── mod.rs       — top-level render() dispatcher
│   ├── theme.rs     — Theme struct (Color palette); always used as Theme::default()
│   └── views/
│       ├── mod.rs               — shared helpers (hint_height)
│       ├── project_list.rs      — Project List screen renderer
│       ├── project_detail.rs    — Project Detail screen renderer
│       ├── todo_list.rs         — Global Todo List screen renderer
│       ├── reference_library.rs — Reference Library screen (placeholder)
│       └── overlay.rs           — all overlay renderers
└── bin/
    └── ptm_sync/    — ptm-sync binary (S3 backup sync; see §ptm-sync below)
        ├── main.rs     — CLI args (clap), Paths struct, shared utilities, entry point
        ├── ops.rs      — high-level --start and --end flows
        ├── archive.rs  — zip pack/unpack, local-backup creation and rotation
        ├── crypto.rs   — AES-256-GCM encrypt/decrypt (compatible with encrypt.py)
        ├── db.rs       — WAL detection and TRUNCATE checkpoint via rusqlite
        └── s3client.rs — thin S3 wrapper (HEAD / GET / PUT via rust-s3)
```

---

## ptm-sync

`ptm-sync` is a companion binary that backs up and restores the ptm data
directory to/from **S3**. It is intended to be called from a shell wrapper
that brackets the main `ptm` process:

```bash
# Typical wrapper pattern
ptm-sync --start --bucket-name my-bucket --prefix ptm-backups
trap 'ptm-sync --end --bucket-name my-bucket --prefix ptm-backups' EXIT
ptm
```

### Modes

| Flag | Phase | What it does |
|------|-------|--------------|
| `--start` | pre-launch | Pull the latest S3 backup (if needed), then store a startup hash |
| `--end` | post-exit | Hash-check the data; push a new backup to S3 if anything changed |

Only one of `--start` / `--end` may be given per invocation.

### CLI Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--bucket-name` | *(required)* | S3 bucket name |
| `--prefix` | *(required)* | S3 key prefix (e.g. `ptm-backups`) |
| `--encryption-key` | `~/.encryption_key` | Path to the 32-byte raw AES-256 key file |
| `--dry-run` | off | Perform all logic but make no changes to local files or S3 |
| `--local-is-master` | off | Treat local data as authoritative: skip pull on `--start`; force overwrite on `--end` |
| `--remote-is-master` | off | Treat remote data as authoritative: force pull on `--start`; skip push on `--end` |
| `--block-concurrent` | off | Abort `--start` if another ptm process has the database open (default: allow concurrency) |

`--local-is-master` and `--remote-is-master` are mutually exclusive.

### S3 Object

A single object is used per prefix: `<prefix>/ptm-backup.zip.enc`.
ETag values from S3 HEAD responses are used to detect whether a pull or push
is actually needed — unchanged data is never re-uploaded.

### Encryption

Backups are AES-256-GCM encrypted. The on-disk format is:

```
[ MAGIC (4 B: "ENC1") | nonce (12 B) | ciphertext + GCM tag (16 B) ]
```

This format is compatible with `encrypt.py` in the repo root. The key file
must be exactly 32 bytes of raw key material (not base64, not hex) and must
have permissions `600` or `400` — ptm-sync refuses to run if group or other
bits are set.

Generate a key with:
```bash
python3 encrypt.py keygen        # writes ~/.encryption_key
```

### Archive Contents

Each backup is a zip archive (Deflate) containing:

| Entry | Description |
|-------|-------------|
| `ptm.db` | The SQLite database |
| `user_words.txt` | Custom spell-check dictionary (omitted from zip if absent) |

During a pull, `user_words.txt` from the remote backup is **merged** with the
local copy (union + dedup + sort) rather than overwritten. The database is
always replaced wholesale.

### Data Directory Files

All files live alongside `ptm.db` in `$XDG_DATA_HOME/ptm/` (default
`~/.local/share/ptm/`):

| File | Description |
|------|-------------|
| `.last_pushed_etag` | ETag of the last object we uploaded |
| `.last_pulled_etag` | ETag of the last object we downloaded |
| `.db_hash` | SHA-256 of `ptm.db ∥ user_words.txt` at the last successful push or pull; updated by `--end` on a successful push and by `do_pull` on a successful pull; seeded by `--start` only when absent (first launch) |
| `ptm-sync.log` | Append-only sync activity log |
| `local-backups/` | Rolling local zip snapshots created before each pull (up to 5 kept) |

### WAL Handling

Before pulling (`--start`) or pushing (`--end`), ptm-sync checks for a live
`ptm.db-wal` file:

- If the WAL exists but no other process has the database open, it runs
  `PRAGMA wal_checkpoint(TRUNCATE)` to fold it in before syncing.
- If another ptm process is running, `--start` skips the pull (allowing
  concurrent launch) unless `--block-concurrent` is given; `--end` silently
  skips the push (the other process will push when it exits).

### AWS Credentials

Region is read from `$AWS_REGION` or `$AWS_DEFAULT_REGION`, defaulting to
`us-east-1`. Credentials follow the standard AWS chain: environment variables
(`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`) → `~/.aws/credentials` (with
`$AWS_PROFILE`).

### Sync Log

All ptm-sync activity is appended to `ptm-sync.log` (same data directory).
Monitor it with:

```bash
tail -f ~/.local/share/ptm/ptm-sync.log
```

---

## Architecture

### State (`app.rs`)

`App` is one flat struct that holds **all** application state. There is no
separate view-model per screen; fields are prefixed by their owning context:

| Prefix | Context |
|--------|---------|
| `pl_` | Project List screen |
| `pd_` | Project Detail screen |
| `tl_` | Global Todo List screen |

Navigation state:

```rust
pub screen:  Screen          // ProjectList | ProjectDetail | TodoList
pub overlay: Option<Overlay> // only one overlay at a time
pub pending_editor: Option<PendingEditor> // triggers $EDITOR in main loop
```

### Key handling flow

```
App::handle_key(key)
  ├─ overlay.is_some()  →  handle_key_overlay  →  per-overlay handler
  ├─ 'i' (quick capture, when not inline-editing)
  ├─ '?' (key help overlay, when not inline-editing)
  └─ screen match
       ├─ ProjectList   →  handle_key_project_list
       ├─ ProjectDetail →  handle_key_project_detail
       └─ TodoList      →  handle_key_todo_list
```

**Project Detail** has tabs and an additional inline-edit guard layer:

```
handle_key_project_detail
  ├─ pd_editing_todo.is_some()   →  inline todo-title edit (intercepts all keys)
  ├─ pd_adding_todo.is_some()    →  inline new-todo entry  (intercepts all keys)
  ├─ pd_editing_title.is_some()  →  inline title edit      (intercepts all keys)
  ├─ pd_searching                →  search/highlight mode  (intercepts all keys)
  ├─ pd_open_update.is_some()    →  update viewer          (intercepts all keys)
  ├─ Universal keys (ESC, E, T, /, d, u, t, f)
  └─ pd_tab dispatch
       ├─ Description  →  handle_key_pd_description
       ├─ Updates      →  handle_key_pd_updates
       ├─ Todos        →  handle_key_pd_todos
       └─ Files        →  (no-op)
```

The inline-edit, search, and update-viewer guards all take priority over the
tab dispatch. Universal keys (`E`, `T`, `/`, tab-switches `d`/`u`/`t`/`f`)
run before tab dispatch and work in every tab.

### Rendering flow

```
ui::render(frame, app)
  ├─ views::project_list::render   (or project_detail or todo_list)
  └─ views::overlay::render_overlay  (if overlay.is_some())
```

Each view function is a pure render; it reads from `app` and writes to
`frame`. No state is mutated during rendering.

### External editor (`$EDITOR`)

When code sets `app.pending_editor = Some(...)`, the main event loop in
`main.rs` detects it **before** polling for key events, then:
1. Suspends ratatui / restores the normal terminal.
2. Writes content to a `$TMPDIR/ptm_<uuid>.txt` file.
3. Spawns `$EDITOR` (falls back to `vi`) and waits for it to exit.
4. Reads the file back and calls `app.handle_editor_result(target, content)`.
5. Resumes ratatui.

Do **not** open an editor from inside a `handle_key_*` method; always set
`pending_editor` and return.

---

## Data Layer (`repo.rs`)

All database I/O goes through functions in `repo.rs`. No view or `app.rs`
code runs SQL directly.

Key conventions:
- IDs are UUID v4 strings (`Uuid::new_v4().to_string()`).
- Timestamps are UTC ISO 8601: `"%Y-%m-%dT%H:%M:%SZ"`.
- Sort order uses fractional indexing (`f64`). Reordering calls
  `repo::swap_project_order`, `repo::swap_todo_order`, or
  `repo::swap_update_order`, swapping the `sort_order` values of two
  adjacent rows.
- `todos.project_id IS NULL` means the todo is in the **Inbox**.
- `soonest_reminder` on `Project` is computed with a subquery
  (`MIN(reminder)` over active todos) — it is not a stored column.
- Tags are shared globally. `repo::untag_project` garbage-collects orphaned
  tag rows automatically.

Schema changes must use `CREATE TABLE IF NOT EXISTS` (or `ALTER TABLE` with
care); there is no migration version table yet.

---

## UI Conventions

### Hint bar
Every screen/focus mode has a hint string. Its height is computed dynamically
by `super::hint_height(hints, area.width)` (wraps at terminal width) so the
main content area shrinks accordingly. Always update the hints string when
adding or removing keybindings.

```rust
// pattern used in every view
let hints = if <editing_mode> { "  Enter:save  ESC:cancel" } else { "  j/k:..." };
let chunks = Layout::default()
    .constraints([Constraint::Min(3), Constraint::Length(super::hint_height(hints, area.width))])
    ...
```

### Theme
Colors come from `Theme::default()` (defined in `ui/theme.rs`). Always use
theme fields rather than hard-coding colors, except for transient states like
`Color::DarkGray` for placeholder text.

### Color Palette
The target terminal is **Apple Terminal.app on macOS**, which supports
**256-color (xterm-256color)** but **not 24-bit true color**. `$COLORTERM` is
not set in this environment.

- ✅ Use `Color::Indexed(n)` or ratatui named colors (`Color::Red`,
  `Color::LightBlue`, etc.) — these map to the 256-color palette.
- ❌ Do **not** use `Color::Rgb(r, g, b)` — 24-bit sequences are silently
  ignored by this terminal, producing no color change.

### Reminder display
Reminder dates are stored as `YYYY-MM-DD` strings and truncated to 10 chars
on display. They are rendered with a leading two-space pad:

```rust
.map(|r| format!("  {}", &r[..r.len().min(10)]))
```

Do **not** add emoji or icons before reminder dates (spacing issues on some
terminals).

### Overlays
All overlays are rendered centered over the full terminal area by
`overlay::render_overlay`. Only one `Overlay` variant can be active at a time.
The overlay intercepts **all** key events until dismissed.

---

## Keybinding Conventions

| Pattern | Meaning |
|---------|---------|
| `j` / `k` | cursor down / up in any list |
| `J` / `K` (Shift) | reorder item down / up |
| `Enter` | confirm / open |
| `ESC` | cancel / go back |
| `D` (Shift) | delete selected item (always with a confirm dialog) |
| `Q` (Shift) | quit (Project List, Todo List) |
| `n` | new item in context (project inline in PL, todo inline in PD/TL, update in `$EDITOR` in Updates tab, quick-capture in Todo List) |
| `e` | edit selected item (project title inline in PL; todo title inline in PD/TL; selected update in `$EDITOR` in Updates tab) |
| `E` (Shift) | edit project title inline (Project Detail) |
| `d` / `u` / `t` / `f` | switch Project Detail tab (Description / Updates / Todos / Files) |
| `t` | go to Todo List (from Project List); switch to Todos tab (from Project Detail) |
| `p` | go to Project List (from Todo List) |
| `r` | set/clear reminder date; rename tag (Tag Picker, when search is empty) |
| `m` | move todo to another project (opens Project Picker overlay) |
| `T` (Shift) | tag editor (Project Detail) / tag filter (Project List, Todo List) |
| `s` | cycle todo status |
| `S` (Shift) | spell check selected item (todo title, update body, project description) |
| `i` | quick capture (global, outside overlays) |
| `?` | open key help overlay (global, outside overlays) |
| `C` (Shift) | toggle hide done/canceled todos (Project Detail Todos tab, Todo List) |
| `a` | archive/toggle selected item (todos in PD/TL, projects in PL); accept all occurrences (Spell Check) |
| `A` (Shift) | show / hide archived items |

---

## How to Add a Keybinding

1. Handle it in the appropriate `handle_key_*` method in `app.rs`.
2. Update the hints string in the corresponding view file under
   `src/ui/views/`. Remember: hints update the layout height, so the string
   must be kept accurate.
3. Update the `key_help_lines()` function in `src/ui/views/overlay.rs` to
   reflect the change. This is the content shown by the `?` Key Help modal.

For Project Detail, decide whether the key should work in a specific tab handler
(`handle_key_pd_description`, `handle_key_pd_updates`, `handle_key_pd_todos`) or
as a **universal key** (runs before tab dispatch). Keys like `E` and `T` that act
on the project itself are universal and live in the universal-keys block in
`handle_key_project_detail`.

## Warn / Error Modal (`Overlay::Warn`)

A lightweight, reusable notice overlay for conditions that need user attention
but require no input — missing features, configuration problems, etc.

```rust
self.overlay = Some(Overlay::Warn {
    title: " My Warning Title ".into(),   // shown in the border
    body:  "Line one.\nLine two.".into(), // supports \n-separated lines
});
```

- Dismissed by **any key press**.
- Border is rendered in yellow via `Color::Yellow`.
- Body text is word-wrapped; keep lines under ~58 characters for best fit.
- Use this instead of `crate::log::warn` whenever the condition is
  interactive and the user would benefit from on-screen guidance (e.g.,
  a missing dictionary, an unsupported operation).

---

## How to Add an Overlay

1. Add a variant to the `Overlay` enum in `app.rs`.
2. Add an arm to the `match overlay { ... }` block in `handle_key_overlay`.
3. Write a `fn handle_<name>(&mut self, key, ...) -> Result<Option<Overlay>>`
   method on `App`. Return `Ok(None)` to dismiss, `Ok(Some(overlay))` to keep
   it open.
4. Add rendering in `src/ui/views/overlay.rs`: a private `fn render_<name>`
   and a new arm in `render_overlay`'s `match` block.

## How to Add a Screen

1. Add a variant to the `Screen` enum in `app.rs`.
2. Write a `fn handle_key_<screen>(&mut self, key) -> Result<()>` method.
3. Create `src/ui/views/<screen>.rs` with a `pub fn render(frame, area, app)`.
4. Add `pub mod <screen>;` in `src/ui/views/mod.rs`.
5. Add the arm to the `match app.screen { ... }` blocks in both
   `ui::mod::render` and `App::handle_key`.

---

## Logging

The TUI runs in raw mode and owns the terminal, so **`eprintln!` and `print!`
corrupt the display and must never be used**. Use `crate::log::warn(msg)` instead.

```rust
crate::log::warn("something unexpected happened");
```

Lines are appended to `$XDG_DATA_HOME/ptm/ptm.log` (default:
`~/.local/share/ptm/ptm.log`) with a UTC timestamp:

```
2025-01-15T10:23:45Z WARN  row_to_todo: unknown todo status: "bogus" — defaulting to New
```

Logging failures are silently swallowed — a log call must never crash the app.
The log file can be monitored during development with:

```bash
tail -f ~/.local/share/ptm/ptm.log
```

---

## Common Gotchas

- **`tl_cursor` is a flat todo index** — it counts only todo rows, not group
  header rows. Use `App::tl_current_todo()` / `tl_todo_at(flat_idx)` rather
  than indexing `tl_groups` directly.
- **Inline-edit guards run before tab dispatch** in `handle_key_project_detail`.
  If you add a new inline-edit mode, add a corresponding check at the top of
  that method and update `is_inline_editing()`. Note that `is_inline_editing()`
  also returns `true` while `pd_searching`, `pl_searching`, `tl_searching`,
  `pl_editing_title`, or `tl_editing.is_some()` is active.
- **`pending_editor` vs immediate action** — any `$EDITOR` invocation must go
  through `pending_editor`. Setting it and returning causes the main loop to
  open the editor on the next iteration.
- **`pd_tab` (not `pd_focus`)** — the Project Detail tab state is `pd_tab: PdTab`
  with variants `Description`, `Updates`, `Todos`, `Files`. There is no
  `pd_focus` field.
- **Tag garbage collection** — `repo::untag_project` deletes the `tags` row
  when no join-table reference remains. Don't delete tag rows directly.
- **Reminder bubble-up** — `Project::soonest_reminder` is computed from
  active (non-done, non-canceled) todos only. See `load_soonest_reminder` in
  `repo.rs`.
- **Update viewer vs. Updates tab** — when `pd_open_update.is_some()`, all
  keys go to `handle_key_pd_update_viewer`, not `handle_key_pd_updates`. If
  a key should work in both contexts, add it to both handlers.
- **Reorder helpers use swap** — reordering calls `repo::swap_project_order`,
  `repo::swap_todo_order`, or `repo::swap_update_order` (not
  `reorder_project` / `reorder_todo`). Each function swaps the `sort_order`
  values of two adjacent rows.
- **Schema changes** — use `CREATE TABLE IF NOT EXISTS`; running the schema
  again on an existing database must be a no-op. For column additions, SQLite
  does **not** support `ADD COLUMN IF NOT EXISTS` (that clause is not part of
  SQLite's grammar for `ALTER TABLE`). Instead, guard the migration with a
  `SELECT COUNT(*) FROM pragma_table_info('<table>') WHERE name = '<col>'`
  check and only run the `ALTER TABLE ... ADD COLUMN` when the count is 0.
