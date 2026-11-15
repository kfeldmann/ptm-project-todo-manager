# ptm-sync

`ptm-sync` is a companion binary that backs up and restores the ptm data
directory to/from **S3**. It is intended to be called from a shell wrapper
that brackets the main `ptm` process:

```bash
# Typical wrapper pattern
ptm-sync --start --bucket-name my-bucket --prefix ptm-backups
trap 'ptm-sync --end --bucket-name my-bucket --prefix ptm-backups' EXIT
ptm
```

Source: `src/bin/ptm_sync/`. Hard-won invariants for the WAL, locking, and
sync-state behaviour live in [sync-internals.md](sync-internals.md) — **read
that before changing `ops.rs` or `db.rs`.** The key format and the
shared-crypto rules are in [enc-sync.md](enc-sync.md).

## Modes

| Flag | Phase | What it does |
|------|-------|--------------|
| `--start` | pre-launch | Pull the latest S3 backup (if needed), then store a startup hash |
| `--end` | post-exit | Hash-check the data; push a new backup to S3 if anything changed |
| `--merge` | standalone | Merge another ptm data directory (see `--from`) into the local one; no S3 access |

Exactly one mode may be given per invocation.

## CLI flags

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

## S3 object

A single object is used per prefix: `<prefix>/ptm-backup.zip.enc`.
ETag values from S3 HEAD responses are used to detect whether a pull or push is
actually needed — unchanged data is never re-uploaded.

Region is read from `$AWS_REGION` or `$AWS_DEFAULT_REGION`, defaulting to
`us-east-1`. Credentials follow the standard AWS chain: environment variables
(`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`) → `~/.aws/credentials` (with
`$AWS_PROFILE`).

## Archive contents

Each backup is a zip archive (Deflate) containing:

| Entry | Description |
|-------|-------------|
| `ptm.db` | The SQLite database — a `VACUUM INTO` snapshot, **not** the raw file |
| `user_words.txt` | Custom spell-check dictionary (omitted from zip if absent) |

The `ptm.db` entry is a `VACUUM INTO` snapshot because committed transactions
can still be sitting in `ptm.db-wal`, and the raw file may not contain them.
Local backups (`local-backups/`) are built the same way, so they are complete
even when taken while `ptm` is running.

During a pull, `user_words.txt` from the remote backup is **merged** with the
local copy (union + dedup + sort) rather than overwritten. The database is
always replaced wholesale — but only after the downloaded copy passes
`PRAGMA integrity_check`, so a corrupt or truncated backup is reported instead
of installed.

## Data directory files

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

## Sync log

All ptm-sync activity is appended to `ptm-sync.log` (same data directory).

```bash
tail -f ~/.local/share/ptm/ptm-sync.log
```

## Merge mode

`ptm-sync --merge --from <data-dir>` merges a diverged ptm data directory into the local one. Implemented in
`src/bin/ptm_sync/merge.rs` (the largest file in the binary — read it before
changing semantics).

### Semantics

- **Row identity is UUID-based**: identical IDs on both sides are the same
  row; rows present on only one side are inserted into the other.
- **Last-writer-wins** on shared rows via `updated_at` (ISO 8601 UTC):
  `projects`, `todos`, `updates`, `references_table`. Ties keep the local
  version.
- **Tags**: tags new by both id and name are inserted; a name collision
  (different UUIDs, same name) maps the incoming memberships onto the local
  tag — mirroring the app's case-insensitive rename checks. Orphaned tags are
  garbage-collected after membership insertion.
- **Tag membership tables** (`project_tags`, `reference_tags`) are unioned
  with `INSERT OR IGNORE`.
- **Dangling references**: a source row whose reference does not resolve
  locally does not abort the merge. `todos.project_id` is nulled (the todo
  lands in the Inbox) and counted; `updates.project_id` is `NOT NULL`, so such
  an update is skipped and counted; tag memberships whose owning row is
  missing are skipped. The counts are printed and logged. Note
  `INSERT OR IGNORE` does **not** suppress FK violations — they are errors,
  not conflicts — so the resolution happens in the `SELECT`.
- **`user_words.txt`** is union-merged (dedup + sort), like a pull.
- **`.needs_push` is written** so the next `--end` pushes the merged state.
  Deleting `.db_hash` would not work: `--start` re-seeds it from the current
  database whenever it is missing, so the merged-but-unpushed state would be
  recorded as the sync baseline and never uploaded.
- **Known limitation**: deletions are not propagated — a row deleted on one
  side is resurrected by the other side's surviving copy (tombstones would be
  needed for true delete propagation).

### `sort_order` re-spacing

Shared rows keep the local ranking; foreign-only rows are appended after the
local rows of their ordering group; every group is then re-spaced to
1.0, 2.0, … with `ROW_NUMBER()` over `(sort_order, created_at)`.

**"After the local rows" is a rank *greater than the group's local maximum*,
never `local_max + sort_order`.** Foreign `sort_order` values are another
database's own numbering and may be negative, and `updates` are stored
newest-first (`create_update` prepends with `MIN(sort_order) - 1.0`, so a group
runs `0, -1, -2, …` in display order). The per-group local maxima are captured
into temp tables **before** any insert, so an offset never depends on how many
foreign rows were written first.

### Schema drift

Both databases are brought up to date before the merge — **including the local
one** (`ensure_schema_compat` runs before `BEGIN IMMEDIATE`), because the merge
SQL writes columns a database predating the migrations does not have. The
guarded `ALTER TABLE` migrations mirror `src/data/db.rs`; the source copy is
snapshotted with `VACUUM INTO` and migrated in a temp dir.

### Safety

Local WAL is checkpointed first (abort if another process has the db open); a
`VACUUM INTO` snapshot is written to `local-backups/ptm-premerge-<ts>.db`
(5 kept) before any change; the merge runs in a single `BEGIN IMMEDIATE`
transaction and rolls back on any error. See
[sync-internals.md](sync-internals.md) for why each of those is load-bearing.
