# Sync Internals — WAL, Locking, and Sync State

Hard-won invariants for `src/bin/ptm_sync/{db,ops,main}.rs`. Everything here
exists because the naive version was wrong in a way that silently lost data or
reported success while doing nothing. Read it before touching those files; the
feature-level view is in [ptm-sync.md](ptm-sync.md).

## SQLite behaviours that shape the design

**`PRAGMA wal_checkpoint(TRUNCATE)` reports failure in a result row, not as an
error.** It returns `(busy, log, checkpointed)` and raises nothing when it
cannot advance, so `execute_batch` reports success for a WAL that is still
full. Every checkpoint goes through `run_checkpoint`, which reads that row, and
callers must check `CheckpointReport::is_clean()` (`busy == 0` and
`log_frames <= 0`).

**A `BEGIN EXCLUSIVE; COMMIT;` probe releases the lock at the `COMMIT`.** It
does detect a database another process has open, but the answer is stale as
soon as it is returned. Work that must not race with a running `ptm` holds an
`ExclusiveLock` across the whole operation instead.

**Committed data may live only in `ptm.db-wal`.** `std::fs::read("ptm.db")`
then returns a copy containing none of it, so anything that needs a complete
copy uses `db::snapshot_bytes` (`VACUUM INTO`), never a raw file read.

Every connection here sets `busy_timeout` (5 s, matching `src/data/db.rs`);
without it a probe against a busy database fails instantly, making "in use"
indistinguishable from "broken".

## Concurrency behaviour

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
- **`--end` exits non-zero when another session is attached**, because the
  documented wrapper is `trap 'ptm-sync --end' EXIT` and a silent success means
  the session's work is never backed up. This holds regardless of
  `--local-is-master`: that flag is about *S3 authority* (push even when the
  remote has diverged), not about local concurrency, and a user who passed it
  explicitly asked for a push. The attached client is itself the signal that
  the other session will push when it exits; the error message says so instead
  of pretending the backup succeeded.
- **The pulled database is installed while holding an `ExclusiveLock`**:
  checkpoint, verify clean, delete the `-wal`/`-shm` sidecars, then `rename`.
  Renaming over a database with a live `-wal` lets the stale frames be replayed
  into the new file, which discards the pull while `--start` reports success.

## The sync state machine

Two files decide whether `--end` uploads, and **neither is sufficient alone**.

- **`.db_hash`** is the "last synced state" baseline: `--end` compares the
  current snapshot's hash against it. `--start` seeds it when it is absent,
  because a genuinely first launch has no baseline — which is exactly why
  *deleting* it cannot mean "please push". `--start` would recreate it from the
  current database and record the unpushed state as synced.
- **`.needs_push`** is the out-of-band "this is unpushed, whatever the hash
  says" marker. **Its existence is the signal**; the contents are a
  human-readable reason. It is written by `--merge`, by every refused `--end`
  (busy WAL, either ETag guard, a failed upload) and is cleared only by a
  confirmed upload or by the install during a pull. `--end` pushes when the
  marker is present even if the hash matches, and `--start` never seeds
  `.db_hash` while it is present.

### Failure semantics

These follow from the documented wrapper
(`trap 'ptm-sync --end' EXIT; ptm`), which discards the exit code:

- **A push that cannot run is an error** (non-zero exit) *and* leaves the
  marker, so the work is retried by the next `--end` rather than lost.
- **`--start` does not fail the launch** for a recoverable problem. A network
  error asks on a terminal and continues with a warning without one; only
  `--block-concurrent` (a deliberate request to abort on a concurrent session)
  turns a skip into a hard failure. A skipped pull never touches the local
  database, so continuing is safe.
- **`--dry-run` reports what it would do and writes nothing**, including the
  marker.

### Where it lives

| Concern | Location |
|---------|----------|
| `mark_needs_push`, `needs_push`, `clear_needs_push`, `prompt_continue_without_pull` | `src/bin/ptm_sync/main.rs` |
| `upload_decision`, `mark_refusal`, `push_refused` | `src/bin/ptm_sync/ops.rs` |
| `run_checkpoint`, `CheckpointReport`, `ExclusiveLock`, `snapshot_bytes` | `src/bin/ptm_sync/db.rs` |
