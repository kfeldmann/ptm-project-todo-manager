# dev-harness

Standalone reproduction scripts for the findings in
[`code-review-20261101.md`](../code-review-20261101.md). They are **not** part
of the build, are not run by `cargo test`, and nothing in `src/` imports them.

They live in the repo (rather than `/tmp`) because the review's original
harnesses did not survive the session that wrote it: every claim in that
document had to be re-derived from scratch. These are meant to outlive the
sessions that fix the bugs, so the same script can be run **before** the fix
(to confirm the bug is real and still present) and **after** (to confirm it is
gone and stays gone).

## Requirements

- `python3` (3.8+), with the stdlib `sqlite3` module — nothing else.
- Built binaries: `cargo build` (debug) or `cargo build --release`.
  The scripts default to `target/debug`; set `PTM_PROFILE=release` to use
  `target/release`.

## Usage

```bash
python3 dev-harness/check-all.py            # run every harness
python3 dev-harness/wal-safety.py           # one harness
PTM_PROFILE=release python3 dev-harness/check-all.py
```

Each harness prints `PASS`/`FAIL` per check and exits non-zero if any check
fails. **A harness that reproduces a known bug will report FAIL until the bug
is fixed** — that is the intent. Read the harness header to see which finding
it covers and whether FAIL means "bug present".

## The one rule

Every harness works inside `ptmdb.TempWorkspace(...)` and sets `XDG_DATA_HOME`
(and `AWS_*`) in the child process environment only. None of them touch
`~/.local/share/ptm/`, your real key, or your real database. `ptm-sync`
resolves its data dir from `$XDG_DATA_HOME`, so this is sufficient isolation.
If you add a harness, keep that property.

`TempWorkspace` removes its directory on exit, so repeated runs do not
accumulate `$TMPDIR/ptm-*` directories. Set `PTM_KEEP_TEMP=1` to keep the
directory for post-mortem inspection:

```bash
PTM_KEEP_TEMP=1 python3 dev-harness/wal-safety.py
```

## Portability

Nothing is tied to this machine or to a particular container:

- **No hard-coded paths.** The repo root is derived from `__file__`
  (`ptmdb.REPO`), so the harnesses work from any checkout location and from
  any working directory.
- **No hard-coded hostnames.** The only name that appears anywhere is
  `ptm-harness.localhost`, which is an invented label, not a machine name.
- **No state outside the workspace**, apart from `/etc/hosts` (read-only, and
  only for `s3-roundtrip.py`) and `target/` (via `cargo build`).
- **No network.** Everything is local; `s3-roundtrip.py` talks to a fake
  endpoint on `127.0.0.1`.

Requirements: `python3` with the stdlib `sqlite3` module, plus `rustc` for
`panics.py` (it compiles a small snippet to test the byte-slicing behaviour
without needing a terminal). If `rustc` is unavailable that harness reports the
compile failure as a check failure rather than crashing.

If the container is recreated, re-run `cargo build` and then
`python3 dev-harness/check-all.py`. The only thing that does **not** survive is
the `/etc/hosts` entry, which is host state — `s3-roundtrip.py` will print
`SKIP` with the exact line to re-add.

## What each script covers

| Script | Findings | Covers |
|---|---|---|
| `wal-safety.py` | 1a, 1b, 1c, 2 | WAL liveness probe, checkpoint return value, non-atomic install, stale-snapshot push (drives the real binary against the fake S3 — needs the hosts entry) |
| `sync-state.py` | 3, 12, 2 (exit code) | `--merge` → `--start` → `--end` uploads the merged state; a refused `--end` exits non-zero and queues the work; non-TTY network-failure behaviour |
| `merge-robust.py` | 7, 8, 9 | legacy local db, dangling FK, `sort_order` policy for `updates` |
| `panics.py` | 4, 5, 13 | key-help wrap at narrow widths, `~` key path, non-ASCII date truncation |
| `keyhelp-widths.py` | 4 | exhaustive width sweep over the real `key_help_lines()` strings |
| `search-highlight.py` | 17 | `split_span` index drift under case-folding |
| `s3-roundtrip.py` | 2, 3, 1c (partial) | true `--end`/`--start` round trip (needs the hosts entry below) |

## What the round trip does and does not show

`s3-roundtrip.py` passes 12/12 against the current tree, which is a **good
sign, not a contradiction of the review**. It exercises the happy path:

- `--end` uploads and records the ETag;
- a second `--end` with nothing changed does not re-upload;
- `--start` pulls into an empty dir, and the result is intact with no stale
  `-wal`;
- `--start` with the db already held open exits 0 and leaves local data alone
  (the concurrent-launch promise in AGENTS.md).

It does **not** reproduce finding #1c. The corruption needs a `-wal` that is
live *at install time* — either pre-existing (a crash left one behind) or
created by a writer committing during the download. A closed writer checkpoints
and removes its WAL, so the scripted case is safe.

Racing a live writer across the rename was attempted and **deliberately
removed**: which side wins is undefined behaviour and gave different results on
different runs (once the pull was discarded, once it survived). A flaky
assertion is worse than none. The deterministic reproduction is in
`wal-safety.py`, which stages the exact state — stale `-wal` plus busy
checkpoint plus rename — rather than racing for it.

## What `wal-safety.py` asserts, and why it is shaped that way

The first version of this harness reproduced the bugs against **hand-rolled
SQLite sequences** — it copied `ptm.db` itself and renamed a staged file itself,
rather than asking `ptm-sync` to do it. That passed 7/10 on the unfixed tree and
would have gone green for a fix that only corrected the SQLite mechanics inside
`db.rs`, without changing what `ptm-sync` actually does. It was rewritten.

It now drives the **real binary** against `lib/fakes3.py` and asserts on the
state left on disk. The fixtures come from `ptmdb.crash_db()`, which builds a
database whose committed state exists **only in `ptm.db-wal`** — the state a
crashed `ptm` leaves behind. Two consequences worth knowing before editing it:

- **Never read a `crash_db()` database before the check that needs the WAL.**
  Any read — including a plain `sqlite3.connect(...).execute("SELECT ...")` —
  opens the database, recovers the WAL and checkpoints it, so the precondition
  silently disappears. Use `ptmdb.wal_size()` (a `stat`) to confirm the state,
  and read a *copy* if you need to inspect the main file.
- **The checks need the `/etc/hosts` entry.** `--start`/`--end` reach S3, so the
  harness SKIPs without it (exit 0). `s3-roundtrip.py` covers the same ground
  for the happy path; `wal-safety.py` is the authority for the failure modes.

## Verification status (2026-11-01, after session C — merge robustness)

Session A fixed findings 1b, 1c and 2's snapshot half; session B fixed 3 and 12
and settled 2's exit-code half; session C fixed 7, 8 and 9. `wal-safety.py` is
**23/23**, `sync-state.py` **20/20**, `merge-robust.py` **6/6**,
`s3-roundtrip.py` **12/12**. `panics.py` and `search-highlight.py` are
unchanged and still fail, because their findings belong to sessions D and G.

### Why `merge-robust.py`'s #9 check changed shape in session C

Its original expectation was `["L1", "L2", "L3", "R1", "R2"]`, justified by the
comment *"the visible order is L1 (0), L2 (-1), L3 (-2)"*. That comment is
backwards. `repo::create_update` prepends with `MIN(sort_order) - 1.0`,
`repo::list_updates` is `ORDER BY sort_order ASC`, and the Updates tab iterates
`pd_updates` forward with no `rev()` — so **ascending `sort_order` is the
on-screen order** and the fixture's local block displays as `L3, L2, L1`.
(Cross-check: replaying the *old* `MAX() + sort_order` algorithm by hand gives
`L3, L2, R2, L1, R1`, exactly the interleaved output this README records, which
is only possible if display order is ascending `sort_order`.)

Asserting the creation-order list therefore would have required the merge to
*reverse* the local block to pass — the opposite of the documented promise. The
check now compares against the display-order concatenation `L3, L2, L1, R2, R1`
and adds a companion invariant (neither history reordered internally). It was
re-run against the unfixed tree (1/4) to confirm it still catches all three
findings, and that the companion check alone does **not** detect #9 — it also
holds for the buggy interleaved result, which is why it is labelled as a
companion rather than a detector.

### Why `sync-state.py` changed shape in session B

Its original #3 check hand-built the state `merge.rs` produced (a database plus
a *deleted* `.db_hash`) and asserted that `--start` left `.db_hash` absent.
That asserts the old *mechanism*: it fails for the correct fix (which no longer
deletes the file) and would pass for any change that kept deleting it. It now
drives the real `--merge`, asserts the behaviour the review is about — the
merged state reaches S3 — and was re-run against the unfixed tree (13/17) to
confirm it still catches the bug.

Where a harness result differs from the review's write-up, the review is the
thing that is wrong — these harnesses were run, the review's `/tmp`
reproductions were not preserved.

| Finding | Harness result | Note |
|---|---|---|
| 1a (WAL probe unsound) | **does not reproduce as described** | The review says `BEGIN EXCLUSIVE` *succeeds* with an idle-but-open session, so the probe wrongly concludes "no other process". A real child process holding the db open (`PRAGMA journal_mode=WAL` + a `SELECT`, no transaction) makes `BEGIN EXCLUSIVE` **fail** with `database is locked` after the busy timeout. `check_and_checkpoint_wal` therefore returns `Ok(false)` correctly, and `--end` logs "in use; skipping push". The probe is *not* the data-loss path. |
| 1b (checkpoint result ignored) | **reproduces; fixed in session A** | `wal_checkpoint(TRUNCATE)` returns `(busy=1, log=N, checkpointed=0)` and raises nothing, so `execute_batch` reports success while the WAL stays full. Confirmed. |
| 1c (non-atomic install) | **reproduces, narrower than claimed; fixed in session A** | The *lost-pull* variant is real and reproducible: rename over a db with a live `-wal` leaves the stale WAL, and after the live session exits the **local data wins and the pulled backup is gone**, with `--start` reporting success. The *"database disk image is malformed"* variant did **not** reproduce in several attempts (page sizes 4096/8192, reader/writer holders, differing schema): after replay, `integrity_check` returned `ok`. Treat "malformed" as unconfirmed and the lost-pull as the concrete bug. |
| 2 (`--end` packs WAL-less db) | **reproduces; fixed in session A** | The bytes `std::fs::read(&paths.db)` returns contain **zero** committed rows while the live connection sees all 50. |
| 2 (skipped push exit code) | **fixed in sessions A + B** | A skipped push exited **0**, so `trap 'ptm-sync --end' EXIT` saw success. A now exits non-zero; B additionally leaves a `.needs_push` marker, because the trap discards the code. |
| 3 (`.db_hash` seeding) | **reproduces; fixed in session B** | `--merge` deleted `.db_hash` to force a push, but `--start` re-created it from the current db, so the merged state became the sync baseline and the next `--end` logged "No changes detected". Replaced with a `.needs_push` marker that `--start` never touches. The harness now drives the real `--merge` and asserts the merged state reaches S3. |
| 4 (key-help panic) | **reproduces** | Panics at `desc_width` **1, 2, 5, 6, 19, 20** (the review quotes terminal widths 21/22/25/26/39/40; the panic depends on the computed description width, so the fix's test should sweep `desc_width`, not terminal width). Only 3 of the 62 descriptions contain multi-byte characters, all involving `→`. |
| 5 (`~` key path) | **reproduces** | `--encryption-key '~'` panics: `start byte index 2 is out of bounds for string of length 1`. `'~x'` silently resolves to `$HOME/`. |
| 7 (legacy local db) | **reproduces; fixed in session C** | Merge aborts with `table todos has no column named archived` — note the first failing column is `todos.archived`, not `updates.label` as the review states. The local connection is now migrated by `ensure_schema_compat` before the merge transaction. |
| 8 (dangling FK) | **reproduces; fixed in session C** | `FOREIGN KEY constraint failed`, whole merge rolled back. Resolved in the `SELECT`: `todos.project_id` is nulled (Inbox) and counted, an `updates` row whose `project_id` dangles is skipped and counted (the column is `NOT NULL`), and memberships whose owning row is missing are skipped. `INSERT OR IGNORE` does not help — FK violations are errors, not conflicts. |
| 9 (updates ordering) | **reproduces; fixed in session C** | Local `L1(0), L2(-1), L3(-2)` + foreign `R1(0), R2(-1)` rendered `L3, L2, R2, L1, R1` — interleaved. Now `L3, L2, L1, R2, R1`: the local block stays intact and the foreign rows follow it. See "Why `merge-robust.py`'s #9 check changed shape" above for the display-order question. |
| 12 (non-TTY abort) | **reproduces; fixed in session B** | `--start` exited 1 with "Network error and no interactive terminal; cannot pull" when stdin was a pipe. It now continues with a warning and leaves the local data untouched, so `ptm` still launches. |
| 13 (byte-sliced dates) | **reproduces** | `&s[..10]` panics on `"2025-01-1é"` (`not a char boundary`). Note `"→2025-01-01"` does *not* panic — the boundary has to land mid-character. |
| 17 (`split_span`) | **reproduces** | Searching `İ` in `İstanbul` highlights `İs` instead of `İ`. Searching `i` highlights `İ` correctly. |

Not covered by a harness yet (need a terminal or are judgement calls): #6, #10,
#11, #14, #15, #16, #18–#24. #10 (N+1 queries) and #11 (`rename_tag` error
swallowing) are worth adding as `repo`-level unit tests in the sessions that
fix them, since `repo.rs` is already well covered by `cargo test`.

## Known limitation: a concurrent session is only detected via a WAL

`--end`'s "is another ptm running?" check is gated on `ptm.db-wal` being
non-empty (`if db::wal_size(&paths.db) > 0`).  The gate exists because opening a
connection to probe for a lock *creates* `-wal`/`-shm` sidecars, so an
unconditional probe would litter a clean data directory on every `--end`.

The consequence, measured rather than assumed: with a second client attached,

| holder mode | WAL present | `--end` |
|---|---|---|
| `writer` (committed a row) | yes | exits 1 — "database is in use by another ptm process" |
| `idle` / `reader` (no writes) | no | **exits 0 and uploads** |

So the gate only catches a concurrent session that has *written* something.
A second `ptm` sitting idle on a cleanly-checkpointed database is not detected.

This is not a data-safety hole — `snapshot_bytes` uses `VACUUM INTO`, which
takes a consistent snapshot without needing an exclusive lock, so what gets
uploaded is coherent. It is a detection hole: the push is silent rather than
reported as "another session is attached". Fixing it properly means probing
unconditionally *and* cleaning up the sidecars the probe creates, which is
S3-independent but touches every `--end`. Not done in session A.

## Known limitations

- **S3 needs one hosts entry for the round trip, and it must name the
  subdomain.** `rust-s3` builds the request host as `<bucket>.<region-host>`
  and `S3Client` never calls `.with_path_style()`, so `ptm-sync` can only reach
  a local fake endpoint if `b.ptm-harness.localhost` resolves. `/etc/hosts` is
  **exact-match — there are no wildcards**, so an entry for
  `ptm-harness.localhost` alone does *not* cover `b.ptm-harness.localhost`.
  Add:

  ```
  127.0.0.1  b.ptm-harness.localhost
  ```

  `s3-roundtrip.py` accepts a hosts entry for any bucket in
  `fakes3.CANDIDATE_BUCKETS` (`b`, `ptm`, `harness`, `ptm-sync`, `test`) and
  uses whichever resolves, so any of those prefixed names works.

  It then runs a real `--end` → `--start` cycle against `lib/fakes3.py`.
  Without a matching entry it prints `SKIP` and exits 0, so it never breaks a
  run on a machine that is not set up. Every other harness works without it:
  they cover WAL handling, checkpointing, hashing, staging and the local file
  install, and use `--local-is-master` / `--remote-is-master` where they need
  to skip the network leg.
  `HOSTALIASES` is not a substitute — glibc ignores it for names containing
  dots, which these are.
- **`Holder` uses a child process, not a thread.** SQLite's POSIX advisory
  locks are per-process, so two connections in one process share lock state
  and cannot block each other. This is why `db.rs`'s own unit tests skip the
  `Ok(false)` path; the harness supplies what the unit tests cannot.
