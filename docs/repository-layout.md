# Repository Layout

Shared files are marked **(shared)**. They are `#[path]`-included by more than
one binary — see [shared-files.md](shared-files.md) before editing one.

```
src/
├── main.rs          — terminal setup, event loop, $EDITOR suspension
├── app.rs           — App struct (all state), handle_key_* methods
├── log.rs           — file-based logger (shared: ptm + md)
├── crypto.rs        — AES-256-GCM encrypt/decrypt + key helpers (shared: ptm-sync + enc)
├── spell.rs         — spell checker (shared: ptm + md)
├── tz.rs            — local UTC offset in minutes, cached for 24h (a `date +%z`
│                      spawn per cache miss, never per render)
├── paths.rs         — `expand_tilde` plus the XDG lookup (`data_home`,
│                      `app_data_dir`, `optional_app_data_dir`,
│                      `app_data_file`) (shared: all four binaries)
├── data/
│   ├── mod.rs       — re-exports db, models, repo
│   ├── db.rs        — SQLite connection, schema (CREATE TABLE IF NOT EXISTS)
│   ├── models.rs    — plain data structs: Project, Todo, Update, Tag, TodoStatus
│   └── repo.rs      — all database reads/writes; no UI dependencies
├── input/
│   └── mod.rs       — cursor-aware single-line `InputState` (insert / delete /
│                      cursor movement, column-budgeted `display`) plus
│                      `handle_input_key`; fully implemented, not a placeholder
├── ui/
│   ├── mod.rs       — top-level render() dispatcher
│   ├── theme.rs     — Theme struct (Color palette); always Theme::default() (shared: md)
│   ├── text.rs      — width-aware text helpers (shared: md)
│   ├── markdown.rs  — Markdown → ratatui renderer (shared: md)
│   ├── search.rs    — search-match highlighting over *rendered* lines (shared: md)
│   └── views/
│       ├── mod.rs               — shared helpers (list pagination; re-exports ui::text)
│       ├── project_list.rs      — Project List screen renderer
│       ├── project_detail.rs    — Project Detail screen renderer
│       ├── todo_list.rs         — Global Todo List screen renderer
│       └── overlay.rs           — all overlay renderers
└── bin/
    ├── enc/         — enc binary (file encryption CLI; see enc.md)
    │   └── main.rs     — clap subcommands: keygen/encrypt/decrypt/export/import
    ├── md/          — md binary (Markdown viewer + spell check)
    │   ├── main.rs     — TUI viewer: scroll, incremental search, $EDITOR, S:spell
    │   ├── ui.rs       — path-includes src/ui/{theme,markdown,search,text}.rs
    │   └── log.rs      — thin wrapper over the shared src/log.rs (selects the
    │                     `md` directory → $XDG_DATA_HOME/md/md.log)
    └── ptm_sync/    — ptm-sync binary (S3 backup sync; see ptm-sync.md)
        ├── main.rs     — CLI args (clap), Paths struct, shared utilities, entry point
        ├── ops.rs      — high-level --start and --end flows
        ├── archive.rs  — zip pack/unpack, local-backup creation and rotation
        ├── db.rs       — WAL detection and TRUNCATE checkpoint via rusqlite
        ├── merge.rs    — diverged-database merge (--merge --from <dir>)
        └── s3client.rs — thin S3 wrapper (HEAD / GET / PUT via rust-s3)
```

## Other top-level directories

| Path | Contents |
|------|----------|
| `build/` | Dockerfiles and build/run scripts for cross-platform release builds |
| `dev-harness/` | Runnable end-to-end harnesses, one per code-review finding |
| `planning-docs/` | Design notes, code reviews, remediation plans |
| `util/` | Standalone Python helpers (`encrypt.py`, `import_projects.py`) |
| `USER_MANUAL.md` | End-user documentation — the user-facing key tables live here |
