# AGENTS.md — ptm (Project & Task Manager)

A keyboard-driven terminal UI (TUI) for managing projects, todos, and journal
updates. Written in Rust using ratatui + crossterm; all data is stored locally
in SQLite via rusqlite.

The project management binary is `ptm`. Separate helper binaries are also included:

- `ptm-sync` - Cloud sync for ptm data
- `md` - Markdown viewer with spell-checking
- `enc` - Symmetric file encryption tool

---

## Build & Run

```bash
cargo build            # debug build  → target/debug/{ptm,ptm-sync,md,enc}
cargo build --release  # release build → target/release/{ptm,ptm-sync,md,enc}
cargo run              # build + run debug binary
./build/build-*        # for the user only. Build via Docker
./build/run            # for the user only. Run the build container interactively
```

There are some warnings when building `md` and `enc` because they reuse code
from `ptm` so some variables are unused. Ignore these.

No configuration is required on first launch. The database is created
automatically at `~/.local/share/ptm/ptm.db` (or `$XDG_DATA_HOME/ptm/ptm.db`).

---

## Repository Layout

```
src/
├── main.rs          — terminal setup, event loop, $EDITOR suspension
├── app.rs           — App struct (all state), handle_key_* methods
├── log.rs           — file-based logger (see Logging section below)
├── crypto.rs        — AES-256-GCM encrypt/decrypt + key helpers; shared by
│                      the `ptm-sync` and `enc` binaries (path-included, like
│                      spell.rs — edit this file, not a copy)
├── spell.rs         — spell checker; shared by `ptm` and `md` (path-included)
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
│   ├── markdown.rs  — Markdown → ratatui renderer; shared with `md` (path-included)
│   └── views/
│       ├── mod.rs               — shared helpers (hint_height)
│       ├── project_list.rs      — Project List screen renderer
│       ├── project_detail.rs    — Project Detail screen renderer
│       ├── todo_list.rs         — Global Todo List screen renderer
│       ├── reference_library.rs — Reference Library screen (placeholder)
│       └── overlay.rs           — all overlay renderers
└── bin/
    ├── enc/         — enc binary (file encryption CLI; see §enc below)
    │   └── main.rs     — clap subcommands: keygen/encrypt/decrypt/export/import
    ├── md/          — md binary (Markdown viewer + spell check)
    │   ├── main.rs     — TUI viewer: scroll, incremental search, $EDITOR, S:spell
    │   ├── ui.rs       — path-includes src/ui/theme.rs and src/ui/markdown.rs
    │   └── log.rs      — file logger → $XDG_DATA_HOME/md/md.log
    └── ptm_sync/    — ptm-sync binary (S3 backup sync; see §ptm-sync below)
        ├── main.rs     — CLI args (clap), Paths struct, shared utilities, entry point
        ├── ops.rs      — high-level --start and --end flows
        ├── archive.rs  — zip pack/unpack, local-backup creation and rotation
        ├── db.rs       — WAL detection and TRUNCATE checkpoint via rusqlite
        ├── merge.rs    — diverged-database merge (--merge --from <dir>)
        └── s3client.rs — thin S3 wrapper (HEAD / GET / PUT via rust-s3)
```

---

## enc

`enc` is a Rust replacement for `util/encrypt.py`: the same AES-256-GCM file
format, the same key file format, and the same CLI shape, plus `export` /
`import` for moving a key around as text. Files and keys are byte-compatible
with the Python tool in both directions, so either tool can read what the
other wrote.

### Commands

| Command | Effect |
|---------|--------|
| `enc keygen` | Generate a fresh 256-bit key → `~/.encryption_key` (mode 0600) |
| `enc encrypt <file>` | Encrypt → `<file>.enc` |
| `enc decrypt <file.enc>` | Decrypt → `<file>` (`.enc` stripped; else `.dec` appended) |
| `enc export` | Print the key as base64 on stdout |
| `enc import` | Read a base64 key from stdin, write it to the key file |

### Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--key-file` / `-k` | `~/.encryption_key` | Key file path (`~` is expanded; errors if `$HOME` is unset) |
| `--output` / `-o` | *(derived from input)* | Output path (encrypt / decrypt only) |
| `--force` / `-f` | off | Overwrite an existing output/key file without prompting |

### Behavior notes

- **Prompts** happen on stderr so that `enc export > key.b64` stays clean.
  When stdin is not a terminal (scripts, pipes) an overwrite prompt is
  answered "no" — pass `--force` to overwrite non-interactively.
- **`enc export`** prints only the base64 key when stdout is piped. On a
  terminal it adds a header line and a warning on stderr (the key itself is
  still the only thing on stdout).
- **`enc import`** reads a single line interactively, but consumes the whole
  stream when piped, so keys wrapped with `fold` still work. Base64
  whitespace is ignored; the decoded key must be exactly 32 bytes.
- **Key permissions**: `keygen`/`import` create the key file with `0600` at
  creation time (never briefly world-readable). `encrypt`/`decrypt`/`export`
  print a warning if the key file has group/other bits set, but do not fail
  (matching `encrypt.py`); `ptm-sync` is the strict one.
- **No partial writes on failure**: decryption authenticates before writing,
  so a wrong key or tampered file leaves no output file behind.

### Shared implementation

The crypto lives in `src/crypto.rs` at the crate root and is path-included by
both binaries (`#[path = "../../crypto.rs"] mod crypto;`), the same pattern
`md` uses for `src/spell.rs`. Change the format there — never in a copy.
`crypto.rs` provides `load_key`, `generate_key`, `write_key`,
`check_key_permissions`, `encode_key_base64`, `decode_key_base64`,
`encrypt`, and `decrypt`.

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
| `--merge` | standalone | Merge another ptm data directory (see `--from`) into the local one; no S3 access |

Exactly one mode may be given per invocation.

#### Merge mode (`--merge --from <data-dir>`)

Merges a diverged ptm data directory into the local one.  Implemented in
`src/bin/ptm_sync/merge.rs`.  Semantics:

- **Row identity is UUID-based**: identical IDs on both sides are the same
  row; rows present on only one side are inserted into the other.
- **Last-writer-wins** on shared rows via `updated_at` (ISO 8601 UTC):
  `projects`, `todos`, `updates`, `references_table`.  Ties keep the local
  version.
- **Tags**: tags new by both id and name are inserted; a name collision
  (different UUIDs, same name) maps the incoming memberships onto the local
  tag — mirroring the app's case-insensitive rename checks.  Orphaned tags
  are garbage-collected after membership insertion.
- **Tag membership tables** (`project_tags`, `reference_tags`) are unioned
  with `INSERT OR IGNORE`.
- **`sort_order`**: shared rows keep the local ranking; foreign-only rows
  are appended after the local rows of their ordering group; every group is
  then re-spaced to 1.0, 2.0, … with `ROW_NUMBER()` over
  `(sort_order, created_at)`.  "After the local rows" is a rank *greater than
  the group's local maximum*, never `local_max + sort_order` — foreign
  `sort_order` values are another database's own numbering and may be
  negative, and `updates` are stored newest-first (`create_update` prepends
  with `MIN(sort_order) - 1.0`, so a group runs `0, -1, -2, …` in display
  order).  The per-group local maxima are captured into temp tables before
  any insert, so an offset never depends on how many foreign rows were
  written first.
- **Dangling references**: a source row whose reference does not resolve
  locally does not abort the merge.  `todos.project_id` is nulled (the todo
  lands in the Inbox) and counted; `updates.project_id` is `NOT NULL`, so such
  an update is skipped and counted; tag memberships whose owning row is
  missing are skipped.  The counts are printed and logged.  Note
  `INSERT OR IGNORE` does **not** suppress FK violations — they are errors,
  not conflicts — so the resolution happens in the `SELECT`.
- **Schema drift**: both databases are brought up to date before the merge —
  including the **local** one (`ensure_schema_compat` runs before
  `BEGIN IMMEDIATE`), because the merge SQL writes columns a database
  predating the migrations does not have.  Guarded `ALTER TABLE` migrations
  mirror `src/data/db.rs`; the source copy is snapshotted with `VACUUM INTO`
  and migrated in a temp dir.
- **Safety**: local WAL is checkpointed first (abort if another process has
  the db open); a `VACUUM INTO` snapshot is written to
  `local-backups/ptm-premerge-<ts>.db` (5 kept) before any change; the
  merge runs in a single `BEGIN IMMEDIATE` transaction and rolls back on
  any error.
- **`user_words.txt`** is union-merged (dedup + sort), like a pull.
- **`.needs_push` is written** so the next `--end` pushes the merged state.
  Deleting `.db_hash` would not work: `--start` re-seeds it from the current
  database whenever it is missing, so the merged-but-unpushed state would be
  recorded as the sync baseline and never uploaded.
- **Known limitation**: deletions are not propagated — a row deleted on one
  side is resurrected by the other side's surviving copy (tombstones would
  be needed for true delete propagation).

### CLI Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--bucket-name` | *(required for `--start`/`--end`)* | S3 bucket name |
| `--prefix` | *(required for `--start`/`--end`)* | S3 key prefix (e.g. `ptm-backups`) |
| `--from` | *(required for `--merge`)* | Source ptm data directory to merge from (must contain `ptm.db`) |
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

This format is compatible with `util/encrypt.py` and with the `enc` binary
(see §enc above); all three share `src/crypto.rs`. The key file must be
exactly 32 bytes of raw key material (not base64, not hex) and must have
permissions `600` or `400` — ptm-sync refuses to run if group or other bits
are set.

Generate a key with:
```bash
enc keygen                      # writes ~/.encryption_key
python3 util/encrypt.py keygen  # equivalent (Python tool)
```

### Archive Contents

Each backup is a zip archive (Deflate) containing:

| Entry | Description |
|-------|-------------|
| `ptm.db` | The SQLite database |
| `user_words.txt` | Custom spell-check dictionary (omitted from zip if absent) |

During a pull, `user_words.txt` from the remote backup is **merged** with the
local copy (union + dedup + sort) rather than overwritten. The database is
always replaced wholesale — but only after the downloaded copy passes
`PRAGMA integrity_check`, so a corrupt or truncated backup is reported instead
of installed.

The `ptm.db` entry is a `VACUUM INTO` snapshot, not the raw file: committed
transactions can still be sitting in `ptm.db-wal`, and the raw file may not
contain them. Local backups (`local-backups/`) are built the same way, so they
are complete even when taken while `ptm` is running.

### Data Directory Files

All files live alongside `ptm.db` in `$XDG_DATA_HOME/ptm/` (default
`~/.local/share/ptm/`):

| File | Description |
|------|-------------|
| `.last_pushed_etag` | ETag of the last object we uploaded |
| `.last_pulled_etag` | ETag of the last object we downloaded |
| `.db_hash` | SHA-256 of `ptm.db ∥ user_words.txt` at the last successful push or pull; updated by `--end` on a successful push and by `do_pull` on a successful pull; seeded by `--start` only when absent (first launch) and no push is pending |
| `.needs_push` | Present when the local state is known to be unpushed, whatever `.db_hash` says. Written by `--merge`, by a refused `--end` (busy WAL, ETag guards, failed upload) and cleared only by a confirmed upload or an install during a pull. Its existence is the signal; the contents are a human-readable reason |
| `ptm-sync.log` | Append-only sync activity log |
| `local-backups/` | Rolling local zip snapshots created before each pull (up to 5 kept); also holds `ptm-premerge-*.db` snapshots created before each merge (up to 5 kept) |

### WAL Handling

All of this lives in `src/bin/ptm_sync/db.rs`. Three SQLite behaviours shape it:

- **`PRAGMA wal_checkpoint(TRUNCATE)` reports failure in a result row, not as an
  error.** It returns `(busy, log, checkpointed)` and raises nothing when it
  cannot advance, so `execute_batch` reports success for a WAL that is still
  full. Every checkpoint goes through `run_checkpoint`, which reads that row,
  and callers must check `CheckpointReport::is_clean()` (`busy == 0` and
  `log_frames <= 0`).
- **A `BEGIN EXCLUSIVE; COMMIT;` probe releases the lock at the `COMMIT`.** It
  does detect a database another process has open, but the answer is stale as
  soon as it is returned. Work that must not race with a running `ptm` holds an
  `ExclusiveLock` across the whole operation instead.
- **Committed data may live only in `ptm.db-wal`.** `std::fs::read("ptm.db")`
  then returns a copy containing none of it, so anything that needs a complete
  copy uses `db::snapshot_bytes` (`VACUUM INTO`), never a raw file read.

Behaviour:

- If `ptm.db-wal` is non-empty and no other process has the database open,
  ptm-sync runs `wal_checkpoint(TRUNCATE)` and verifies the result is clean
  before syncing.
- If another ptm process is running, or the checkpoint cannot advance, `--start`
  skips the pull (allowing concurrent launch) unless `--block-concurrent` is
  given. A network error during the pull is treated the same way: on a terminal
  it asks, without one it continues with a warning — `--start` runs immediately
  before `ptm`, so failing it means `ptm` does not start at all.
- The "is another process running?" check is gated on `ptm.db-wal` being
  non-empty, because probing for a lock creates the `-wal`/`-shm` sidecars. A
  concurrent session that has not written anything is therefore not detected by
  `--end` (the upload is still coherent — `snapshot_bytes` does not need an
  exclusive lock — it is just not reported as concurrent).
- `--end` **exits non-zero** in that case, because the documented wrapper is
  `trap 'ptm-sync --end' EXIT` and a silent success means the session's work is
  never backed up. This holds regardless of `--local-is-master`: that flag is
  about *S3 authority* (push even when the remote has diverged), not about local
  concurrency, and a user who passed it explicitly asked for a push. The
  attached client is itself the signal that the other session will push when it
  exits; the error message says so instead of pretending the backup succeeded.
- The pulled database is installed while holding an `ExclusiveLock`: checkpoint,
  verify clean, delete the `-wal`/`-shm` sidecars, then `rename`. Renaming over
  a database with a live `-wal` lets the stale frames be replayed into the new
  file, which discards the pull while `--start` reports success.
- Every connection here sets `busy_timeout` (5 s, matching `src/data/db.rs`);
  without it a probe against a busy database fails instantly, making "in use"
  indistinguishable from "broken".

### Sync State Machine

Two files decide whether `--end` uploads, and neither is sufficient alone.

- **`.db_hash`** is the "last synced state" baseline: `--end` compares the
  current snapshot's hash against it. `--start` seeds it when it is absent,
  because a genuinely first launch has no baseline — which is exactly why
  *deleting* it cannot mean "please push". `--start` would recreate it from the
  current database and record the unpushed state as synced.
- **`.needs_push`** is the out-of-band "this is unpushed, whatever the hash
  says" marker. Its existence is the signal. It is written by `--merge`, by
  every refused `--end` (busy WAL, either ETag guard, a failed upload) and is
  cleared only by a confirmed upload or by the install during a pull. `--end`
  pushes when the marker is present even if the hash matches, and `--start`
  never seeds `.db_hash` while it is present.

Failure semantics follow from the documented wrapper
(`trap 'ptm-sync --end' EXIT; ptm`), which discards the exit code:

- A push that cannot run is an **error** (non-zero exit) *and* leaves the
  marker, so the work is retried by the next `--end` rather than lost.
- `--start` is on the launch path, so it **does not fail the launch** for a
  recoverable problem. A network error asks on a terminal and continues with a
  warning without one; only `--block-concurrent` (a deliberate request to abort
  on a concurrent session) turns a skip into a hard failure. A skipped pull
  never touches the local database, so continuing is safe.
- `--dry-run` reports what it would do and writes nothing, including the
  marker.

The corresponding functions live in `src/bin/ptm_sync/main.rs`
(`mark_needs_push`, `needs_push`, `clear_needs_push`,
`prompt_continue_without_pull`) and `src/bin/ptm_sync/ops.rs`
(`upload_decision`, `mark_refusal`, `push_refused`).

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
| `n` / `N` | next / previous search match (Project Detail Description tab and update viewer; wraps like vim) |
| `e` | edit selected item (project title inline in PL; todo title inline in PD/TL; description in `$EDITOR` in PD Description tab; selected update in `$EDITOR` in Updates tab) |
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
- **List pagination** — ratatui's `List` scrolls by whole items: a multi-row
  item that overflows the bottom is dropped entirely (leaving a blank gap),
  and footer rows appended after the items (e.g. the "N archived — A to
  show" hint) can be pushed past the bottom edge. All list views therefore
  paginate themselves: build the items as `Vec<Vec<Line>>`, compute the
  visible window with `views::list_window(&heights, area_height,
  selected)`, render the `List` into exactly `rows_used` rows, and fill any
  leftover rows with a "peek" at the next item's first lines (see
  `project_list.rs`, `todo_list.rs`, and `render_todos_tab` /
  `render_updates_tab` in `project_detail.rs`). Don't render a list widget
  straight into the full content area with raw `ListItem`s.
