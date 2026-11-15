# AGENTS.md — ptm (Project & Task Manager)

A keyboard-driven terminal UI (TUI) for managing projects, todos, and journal
updates. Written in Rust using ratatui + crossterm; all data is stored locally
in SQLite via rusqlite.

The project management binary is `ptm`. Separate helper binaries are also included:

- `ptm-sync` — Cloud sync for ptm data
- `md` — Markdown viewer with spell-checking
- `enc` — Symmetric file encryption tool

```bash
cargo build            # debug build  → target/debug/{ptm,ptm-sync,md,enc}
cargo build --release  # release build → target/release/{ptm,ptm-sync,md,enc}
cargo test             # unit tests
python3 dev-harness/check-all.py   # end-to-end harnesses
```

Warnings when building `md` and `enc` are expected: they reuse code from `ptm`,
so some variables are unused. Ignore them. No configuration is needed on first
launch — the database is created at `~/.local/share/ptm/ptm.db` (or
`$XDG_DATA_HOME/ptm/ptm.db`).

**This file is an index.** It is force-fed into every session, so it carries
only the rules that apply everywhere. Everything else is one file away — read
the relevant doc *before* working in that area, not after.

---

## Three rules that apply everywhere

1. **Shared files are `#[path]`-included, never copied.** `paths.rs`,
   `log.rs`, `crypto.rs`, `spell.rs`, `ui/{theme,text,search,markdown}.rs` are
   compiled into more than one binary. Editing a "copy" under `src/bin/` is
   always wrong — there is no copy, only a `mod` declaration pointing back.
   → [docs/shared-files.md](docs/shared-files.md)
2. **Never `println!` / `eprintln!`.** The TUI owns the terminal in raw mode;
   use `crate::log::warn(msg)`. → [docs/logging.md](docs/logging.md)
3. **Measure text in display columns, never chars or bytes.** CJK and emoji
   occupy two columns, combining marks none. Use `ui/text.rs`.
   → [docs/ui-conventions.md](docs/ui-conventions.md#text-width--always-measure-in-columns)

---

## Doc map

| Doc | Read it when you are… |
|-----|-----------------------|
| [docs/build.md](docs/build.md) | building, testing, or running harnesses |
| [docs/repository-layout.md](docs/repository-layout.md) | orienting yourself in the tree |
| [docs/shared-files.md](docs/shared-files.md) | adding a helper two binaries need, or editing anything in `src/*.rs` at the crate root |
| [docs/architecture.md](docs/architecture.md) | touching `app.rs`, key dispatch, rendering, `PublishedMetrics`, `$EDITOR`, or the event loop |
| [docs/data-layer.md](docs/data-layer.md) | writing SQL, adding a column, or touching `repo.rs` |
| [docs/ui-conventions.md](docs/ui-conventions.md) | rendering: theme, hint bars, text width, search highlight, tables, overlays |
| [docs/adding-features.md](docs/adding-features.md) | adding a keybinding, overlay, screen, or inline-edit mode |
| [docs/logging.md](docs/logging.md) | logging, or choosing between `log::warn` and an overlay |
| [docs/enc.md](docs/enc.md) | working on the `enc` binary |
| [docs/ptm-sync.md](docs/ptm-sync.md) | working on `ptm-sync` modes, flags, archive, or merge |
| [docs/sync-internals.md](docs/sync-internals.md) | changing WAL handling, locking, or the push/pull state machine |
| [docs/enc-sync.md](docs/enc-sync.md) | changing the encryption format or key handling |

`docs/README.md` lists the same map with a one-line scope for each file.

`USER_MANUAL.md` is the **end-user** documentation and owns the user-facing key
tables. `planning-docs/` holds design notes, code reviews, and remediation
plans — history, not current rules. `dev-harness/` holds one runnable harness
per review finding.

---

## Conventions that bite in any area

- **Keybinding changes touch three places**: the handler in `app.rs`, the hint
  string in the view, and `key_help_lines()` in `overlay.rs`. The hint bar's
  height drives the layout, so a stale string is both a lie and a rendering
  bug. → [docs/adding-features.md](docs/adding-features.md)
- **`pending_editor` vs immediate action** — any `$EDITOR` invocation sets
  `pending_editor` and returns; the main loop opens the editor next iteration.
  Never spawn an editor from inside a `handle_key_*` method.
- **`pd_tab` (not `pd_focus`)** — the Project Detail tab state is
  `pd_tab: PdTab`. There is no `pd_focus` field.
- **Reorder helpers use swap** — `repo::swap_project_order`,
  `repo::swap_todo_order`, `repo::swap_update_order`. There is no
  `reorder_project` / `reorder_todo`.
- **Schema changes** — `CREATE TABLE IF NOT EXISTS`, no migration version
  table, and SQLite has no `ADD COLUMN IF NOT EXISTS`; guard with
  `pragma_table_info`. → [docs/data-layer.md](docs/data-layer.md#schema-changes)
- **List views paginate themselves** via `views::list_window`; never render a
  `List` with raw `ListItem`s into the full content area.
  → [docs/data-layer.md](docs/data-layer.md#list-pagination)

---

## Keybinding patterns

The user-facing key tables are in `USER_MANUAL.md`. These are the *patterns*
new keys should follow, not a list of every binding.

| Pattern | Meaning |
|---------|---------|
| `j` / `k` | cursor down / up in any list |
| `J` / `K` | reorder item down / up |
| `Enter` | confirm / open |
| `ESC` | cancel / go back |
| `D` | delete selected item (always with a confirm dialog) |
| `Q` | quit (Project List, Todo List) |
| `e` / `E` | edit selected item / edit project title |
| `n` | new item in context — **and** next search match where a query is active |
| `N` | previous search match |
| `d` / `u` / `t` / `f` | switch Project Detail tab |
| `s` | cycle todo status |
| `a` / `A` | archive selected / show-or-hide archived |
| `C` | toggle hide done/canceled todos |
| `i` / `?` | quick capture / key help (global, outside overlays) |

`n` is the one to be careful with: it is "next match", "edit description" or
"new item" depending on the tab and whether a query is running. See
[docs/adding-features.md](docs/adding-features.md#how-to-add-a-keybinding).
