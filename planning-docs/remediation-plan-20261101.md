# Remediation Plan — `code-review-20261101.md`

> Companion to [`code-review-20261101.md`](code-reviews/code-review-20261101.md).
> Verification harnesses live in [`dev-harness/`](../dev-harness/) and are
> runnable at any time with `python3 dev-harness/check-all.py`.

## Sessions

| # | Status | Session | Findings | Files touched |
|---|---|---|---|---|
| **A** | DONE | `ptm-sync` WAL safety | **1, 2** | `ptm_sync/db.rs`, `ops.rs`, `archive.rs` |
| **B** | DONE | Sync state machine & failure semantics | **3, 12**, exit-code half of 2 | `ptm_sync/ops.rs`, `main.rs`, `merge.rs` (1 line) |
| **C** | DONE | `merge` robustness | **7, 8, 9** | `ptm_sync/merge.rs` |
| **D** | DONE | Crash fixes | **4, 5, 13** | `views/overlay.rs`, `ptm_sync/main.rs`, `bin/enc/main.rs`, `paths.rs`, `views/{mod,project_list,todo_list,project_detail}.rs` |
| **E** | DONE | Key handling & render purity | **6, 14, 15, 23** | `app.rs`, `views/project_detail.rs`, `views/overlay.rs`, `dev-harness/update-viewer.py` |
| **F** | DONE | Data-layer correctness & perf | **10, 11** | `data/repo.rs`, `app.rs` (caller) |
| **G** | DONE | Unicode/display-width consistency | **16, 17** | `ui/text.rs` (new), `ui/{mod,markdown}.rs`, `input/mod.rs`, `views/*.rs`, `bin/md/{main,ui}.rs` |
| **H** | DONE | Dedup, permissions, docs sweep | **18, 19, 20, 21, 22, 24** | `app.rs`, `crypto.rs`, `log.rs`, `bin/md/log.rs`, `paths.rs`, `bin/{md,ptm_sync}/main.rs`, `data/repo.rs`, `ui/views/{overlay,project_detail}.rs`, `Cargo.toml`, `AGENTS.md`, `dev-harness/query-count.py` |

## Status at the time of writing

> **This section describes the starting state**, before session A. The
> per-session sections under §Completed Session log carry the current numbers.

- `cargo test --all-targets` → **651 tests, 0 failures** (37 + 145 + 337 + 132
  after session G; the starting point was 408 = 36 + 82 + 214 + 76).
  (After session D: **478**, all green. After session E: **495**, i.e. 37 + 82 + 244 + 132.
  After session F: **517**, i.e. 37 + 82 + 266 + 132. After session G: **651**,
  i.e. 37 + 145 + 337 + 132, with 2 ignored. After session H: **679**,
  i.e. 41 + 153 + 346 + 139, with 14 ignored — the ignored ones are child
  processes spawned by their parent test, so the count is not a regression.)
- `cargo check --all-targets` clean, and `cargo clippy --all-targets` at
  **zero warnings** as of session H (it was 32 when the review was written).
- All 24 findings are still live at the lines the review cites.
- **15 of the findings have been reproduced by the harnesses in `dev-harness/`**
  (#1b, #1c, #2, #3, #4, #5, #7, #8, #9, #12, #13, #16, #17).
  (After session F: **13**, i.e. + #10 and #11 via `query-count.py`.
  After session G: **15**, i.e. + #16 and #17 via `width-display.py` and
  `search-highlight.py`.)
  Two of them differ from the review's write-up — see
  [§Findings that need correcting](#findings-that-need-correcting-first).
- `git` is **not installed** in this environment. Sessions must therefore be
  run sequentially on one working tree; there are no branches or worktrees to
  isolate them. This is the main reason the grouping below is conservative
  about parallelism.

## Completed Session log

### A — `ptm-sync` WAL safety — **done**

Findings #1b, #1c and #2 fixed; #1a documented as not reproducing (its checks
now pin the behaviour that must hold instead of a defect).

- `db.rs` rewritten around three primitives: `CheckpointReport` (the
  `(busy, log, checkpointed)` row, never discarded), `ExclusiveLock` (held across
  the install rather than probed), and `snapshot_bytes` (`VACUUM INTO`, for
  anything that needs a complete copy of the database). `busy_timeout` is now
  set on every connection.
- `ops.rs`: `--start` treats a busy/non-advancing checkpoint as "live WAL";
  `do_pull` installs under an `ExclusiveLock` after a clean checkpoint, deleting
  the `-wal`/`-shm` sidecars before the rename, and refuses a backup that fails
  `integrity_check`. `--end` hashes and packs a `snapshot_bytes` snapshot, so the
  change test and the upload can never disagree, and a push that cannot run
  exits non-zero — including under `--local-is-master`, which is about S3
  authority, not local concurrency.
- `archive.rs`: local backups are snapshots too, so they are complete when taken
  mid-session.
- Tests: 408 → **441**, all green (36 + 82 + 214 + 109). New `db.rs` tests cover the checkpoint report,
  the install sequence, `snapshot_bytes`, `check_integrity`, and — with a
  **separate child process** holding the database, since POSIX locks are
  per-process — the "another process has it open" path. New `ops.rs` tests cover
  the `--end` refusal and hash/snapshot agreement.
- `wal-safety.py` is **23/23**; `s3-roundtrip.py` still **12/12**. The other
  harnesses are unchanged (their findings belong to B–H).
- Also verified by hand: the lost-pull scenario (a crashed session's WAL beside
  a pull) now installs the backup, leaves no `-wal`, and passes
  `PRAGMA integrity_check`; and a backup whose `ptm.db` is not a database is
  refused before the local file is touched.
- One harness had to change shape to pass, which the procedure requires be
  justified: `wal-safety.py` previously reproduced #1b/#1c/#2 against hand-rolled
  SQLite sequences rather than against `ptm-sync`. It went green for fixes that
  only touched `db.rs` internals. It now drives the real binary against
  `lib/fakes3.py` and asserts on what is left on disk. See
  `dev-harness/README.md`.
- **A correction made after review.** The first cut had `--end` exit 0 under
  `--local-is-master` when it could not push, on the reasoning that the flag
  means "another session owns the data and will push". That is wrong: the flag
  is about *S3 authority* — push regardless of what the remote holds — and a
  user who passed it asked for a forced push, so silently skipping one and
  reporting success is the worst available answer. The refusal is now
  unconditional; the attached client is itself the signal that the other session
  will push, and the message says so.
- **Residual hole found while re-checking that** (recorded in
  `dev-harness/README.md`, not fixed): the liveness check is gated on
  `ptm.db-wal` being non-empty, because probing for a lock creates the
  `-wal`/`-shm` sidecars. A concurrent session that has written nothing is
  therefore not detected, and `--end` exits 0 and uploads. The upload is still
  coherent (`snapshot_bytes` needs no exclusive lock), so this is a detection
  gap, not a data-safety one. Closing it means probing unconditionally and
  cleaning up the sidecars the probe creates.
- Deliberately **not** done here: `merge.rs`'s one-line `.db_hash` sentinel (#3,
  session B) and its `sort_order` policy (#9, session C). `merge.rs` changed only
  to follow `check_and_checkpoint_wal`'s new return type.

---

### B — Sync state machine & failure semantics — **done**

Findings #3 and #12 fixed, and #2's exit-code half settled so the semantics live
in one place.

- **#3 — the unsynced-changes guard.** The root cause was that "push this next
  time" was expressed as *deleting* `.db_hash`, which `--start` must re-seed
  (a first launch has no baseline). A deletion is indistinguishable from "never
  synced", so the merged state became the baseline and `--end` reported "no
  changes" forever. There is now an out-of-band `.needs_push` marker
  (`mark_needs_push` / `needs_push` / `clear_needs_push` in `main.rs`) that
  `--start` never touches: `--start` refuses to seed `.db_hash` while it is
  present, and `--end` pushes regardless of the hash while it is present. Only a
  confirmed upload — or the install during a pull, after which disk and remote
  agree — clears it.
- **#2's exit-code half.** A refused push already exited non-zero (session A),
  but the documented wrapper is `trap 'ptm-sync --end' EXIT`, which discards the
  code, so the refusal now also leaves the marker: busy WAL, both ETag guards
  and a failed upload all mark the state for the next `--end`. The push decision
  is now a testable value (`upload_decision` → `Skip` / `Push` /
  `PushBecauseMarked`) instead of an inline `if`. `--dry-run` still writes
  nothing, so its refusal only logs what it would have done (`mark_refusal`).
- **#12 — the launch must not be blocked.** `prompt_continue_without_pull` now
  *continues* when stdin is not a TTY (with a warning and a log line) instead of
  aborting; on a terminal the prompt defaults to continuing. The skipped pull
  never touches the local database, and `.db_hash` / `.needs_push` are unchanged,
  so the pull is simply retried on the next launch. Only `--block-concurrent`
  turns a skip into a hard failure, which is the case where the user asked for
  one.
- Tests: 441 → **450**, all green (36 + 82 + 214 + 118). New `ops.rs` tests
  cover the marker outranking a matching hash, `mark`/`clear` round trips (an
  empty marker file still counts), `--start` refusing to seed while a push is
  pending and still seeding on a genuine first launch, a refused push leaving
  the marker, and `--dry-run` neither setting nor clearing it. A new `main.rs`
  test exercises the real non-TTY network path (`cargo test` has no TTY on
  stdin).
- `sync-state.py` went from 2/5 to **20/20**, and now includes a real
  `--merge` → `--start` → `--end` round trip against `lib/fakes3.py` asserting
  the merged state actually reaches S3, a second `--end` not re-uploading, and a
  refused `--end` both exiting non-zero and queueing the work.
- **The harness had to change shape**, which the procedure requires be
  justified. Its old #3 check hand-built the state `merge.rs` used to produce
  (a database plus a *deleted* `.db_hash`) and then asserted `--start` left
  `.db_hash` absent. That is a test of the old mechanism, not of the behaviour:
  it fails for the correct fix and would have passed for any change that kept
  deleting the file. It now drives the real `--merge` and asserts what the
  review actually cares about — that the merged state gets uploaded — and it was
  re-run against the unfixed tree (13/17) to confirm it still catches the bug.
- `wal-safety.py` is still **23/23** and `s3-roundtrip.py` still **12/12**;
  `merge-robust.py`, `panics.py` and `search-highlight.py` are unchanged and
  still fail, because their findings belong to sessions C, D and G.
- AGENTS.md gains a §Sync State Machine section stating which file means what,
  and the merge section no longer claims `.db_hash` is deleted.

---

### C — `merge` robustness — **done**

Findings #7, #8 and #9 fixed; `merge.rs` is the only file touched.

- **#7 — the legacy *local* database.** `merge_dbs` now runs
  `ensure_schema_compat` on the local connection before `BEGIN IMMEDIATE`.
  The merge SQL writes columns a database predating the migrations does not
  have, so the first statement aborted with `no such column` (in practice
  `table todos has no column named archived`, not `updates.label` as the review
  states — the note below is right). Migrating before the transaction keeps the
  guarded `ALTER TABLE`s out of it.
- **#8 — dangling foreign references.** Resolved in the `SELECT`, not with
  `INSERT OR IGNORE`: an FK violation is an *error*, not a conflict, so
  `OR IGNORE` does not suppress it (verified — it still raises). `todos`
  resolves `project_id` against the local `projects` table and nulls it when it
  dangles (the todo lands in the Inbox); `updates.project_id` is `NOT NULL`, so
  such a row is skipped; tag memberships whose owning project/reference is
  missing are skipped. Both counts are printed and logged. The LWW `UPDATE`
  path needed the same treatment — it rewrites `project_id` too, so a *shared*
  todo whose incoming project dangles keeps its local project instead of
  aborting the merge.
- **#9 — `sort_order` policy.** Foreign rows are now ranked *after* the local
  block with `local_max + rank`, never `local_max + sort_order`. Two defects
  came from the old form: `updates` are stored newest-first
  (`create_update` prepends with `MIN(sort_order) - 1.0`), so a group runs
  `0, -1, -2, …` and `MAX()` is the *first* rendered row; and foreign
  `sort_order` values are arbitrary (they can be negative), so adding them to
  the local offset could sort a foreign row before every local one. The
  per-group local maxima are captured into temp tables *before* any insert, so
  an offset never depends on how many foreign rows were already written. The
  same policy is applied to `projects`, `todos`, `updates` and
  `references_table`; the final `ROW_NUMBER()` re-spacing (ordered by
  `sort_order, created_at`) is unchanged.
- Tests: 450 → **459**, all green (36 + 82 + 214 + 127). Eight new `merge.rs`
  tests: a legacy-shaped local db merges; a dangling todo project is imported
  into the Inbox; a dangling update project is skipped and counted; dangling
  tag memberships are skipped; a shared todo keeps its local project when the
  incoming one dangles; foreign updates are appended after the local block;
  foreign sort orders are ranked rather than added (negative values); foreign
  rows are appended to their own group (per-project and Inbox); updates are
  re-spaced to 1..N. Each was confirmed to fail against the unfixed tree.
- `merge-robust.py` went from 0/3 to **6/6**, and was re-run against the
  unfixed tree (**1/4**) to confirm it still catches all three findings.
- **The harness had to change shape, which the procedure requires be
  justified.** Its #9 expectation was `L1, L2, L3, R1, R2`, justified by a
  comment claiming *"the visible order is L1 (0), L2 (-1), L3 (-2)"*. That is
  backwards: `list_updates` is `ORDER BY sort_order ASC`, the Updates tab
  iterates forward with no `rev()`, and `create_update` prepends — so ascending
  `sort_order` is the on-screen order and the fixture displays as `L3, L2, L1`.
  Replaying the *old* algorithm by hand yields `L3, L2, R2, L1, R1`, which is
  exactly the interleaved output the README records, so the review and README
  both measured display order as ascending `sort_order`; only the harness's
  expected list disagreed. Keeping it would have required the merge to *reverse*
  the local block to pass. The check now asserts the display-order
  concatenation and adds a companion invariant (neither history reordered
  internally), which is labelled as a companion because it also holds for the
  buggy result.
- One existing test changed meaning: `failed_merge_rolls_back` used a dangling
  FK as its failure trigger, which #8 deliberately makes non-fatal. It now uses
  a `CHECK` violation (a bogus todo status), which no resolution rule can
  repair, so the rollback path stays covered.
- `wal-safety.py` **23/23**, `sync-state.py` **20/20**,
  `s3-roundtrip.py` **12/12** — unchanged. `panics.py` and
  `search-highlight.py` still fail; their findings are sessions D and G.
- AGENTS.md's merge section now states the `sort_order` policy, the dangling-
  reference rules (including that `INSERT OR IGNORE` does not help) and that
  the **local** schema is migrated too.

### D — Crash fixes — **done**

Findings #4, #5 and #13 fixed. All three were the same shape of bug — a **byte
offset used as a column index on a UTF-8 string** — but they needed three
different kinds of fix.

- **#4 — `wrap_key_row`.** `max_desc` is a column budget, yet the code sliced
  `&d[pos..pos + cut]` with it as a byte offset, so any break landing inside a
  multi-byte character panicked (the rows contain `→`). It now collects the
  description into `Vec<char>` and slices that, so a character is one column
  wherever it sits in the string. Two further defects came out of the rewrite:
  `desc_width == 0` was an **infinite loop** (cut 0 → `pos += 1`, remaining
  still longer than 0) — it now means "do not wrap", which is what the old
  early return intended; and the hard-break path skipped a character
  (`pos += cut + 1` past a non-space), which is gone. The key column is now
  the `KEY_COL_WIDTH` constant, used by both the wrapper and
  `render_key_help`'s `desc_width` arithmetic so the two cannot drift.
- **#5 — `expand_tilde`.** Rather than fix `ptm-sync`'s copy, it was extracted
  into `src/paths.rs`, path-included by `enc` and `ptm-sync` and compiled
  normally into `ptm` — the pattern `crypto.rs`/`spell.rs` already established.
  `Args::key_path` and `--from` both use it and both now return a `Result`:
  an unset `$HOME` is an error rather than a silent fallback to `.`, which is
  the *other* half of #5 (the old code fell back to the current directory, so
  a missing key was reported as "insecure permissions" on `./`).
- **#13 — date truncation.** (Note: the review says the value can arrive from
  `util/import_projects.py`, which in fact validates `%Y-%m-%d`. The
  unvalidated path is `--merge`, which copies the source database's `reminder`
  text verbatim into a column the schema does not constrain. The fix is the
  same either way.) A `date_prefix` helper in `ui/views/mod.rs`
  (with a `DATE_LEN` constant) counts characters via `char_indices`. Every
  display site now uses it: `reminder_date_style`, the Project List and Todo
  List reminder columns, the Todos-tab reminder column, and the three
  `upd.created_at` fallbacks in the Updates tab and viewer (which were already
  using `.chars().take(10)` — safe, but a second convention; they now share
  the helper).
- Tests: 459 → **478**, all green (37 + 82 + 227 + 132, one `#[ignore]`d child
  helper per binary). New tests: `paths.rs` (4, shared by all three binaries;
  the unset-`$HOME` case spawns a **child process**, because `remove_var` is
  process-wide and the harness runs tests on several threads), `overlay.rs`
  (8 — the first tests in that file: a 0..=60 `desc_width` sweep over the real
  `key_help_lines()` rows, `desc_width == 0` termination, word/character
  breaking, continuation alignment, multi-byte column counting), `views/mod.rs`
  (7 for `date_prefix`, including the exact `"2025-01-1é"` regression), and
  `ptm_sync/main.rs` (bare `~`, `~x`).
- `panics.py` went from 2/6 to **9/9**.
- **The harness had to change shape, which the procedure requires be
  justified.** Its #4 and #13 checks compiled *hand-written copies* of the
  buggy code — a `fn wrap_key_row` retyped into a scratch file, `s[..n]` typed
  out inline. That is a test of the copy: it cannot go green when the source is
  fixed, and it would pass for any change that left the copy alone. Both now
  extract the **real functions out of `src/`** (`slice_between` between two
  markers) and compile those. #13 gained a companion check that the historical
  `&s[..len().min(10)]` still panics on the same fixtures, so the assertion is
  demonstrably not vacuous. #5 gained a check that a bare `~` resolves to
  `$HOME` (not just "does not panic", which a `.` fallback would also pass).
  Re-run against the unfixed tree: **3/9**, failing exactly 4, 5 and 13.
- `wal-safety.py` **23/23**, `sync-state.py` **20/20**, `merge-robust.py`
  **6/6**, `s3-roundtrip.py` **12/12** — unchanged. `search-highlight.py`
  still fails; its finding is session G.
- `AGENTS.md` gains `src/paths.rs` in the tree, a §Key Help wrapping note
  (columns not bytes; `desc_width == 0` means no wrap), a correction to the
  §Reminder display snippet (which taught the panicking `&r[..10]` form), and
  a note that `--encryption-key` / `--from` resolve through `expand_tilde`.
  §enc's "Shared implementation" now covers `paths.rs` too.

### E — Key handling & render purity — **done**

Findings #6, #14, #15 and #23 fixed; all three of the session's files are
touched.

- **#6 / #23 — the update viewer was not a key scope, it was a trap.** The
  `pd_open_update` guard ran *above* the universal-key block, so every key in
  that block was dead while an update was open: `n`/`N` (match navigation)
  fell through to nothing, `E` and `T` were swallowed, and the `N` arm could
  never be reached (its `n` sibling's `pd_open_update.is_some()` condition was
  dead for the same reason). The guard now sits **below** the universal block,
  so the viewer keeps only what the block does not claim (`j`/`k`, `^f`/`^b`,
  `g`/`G`, `e`, `S`). `ESC` became "close the viewer, else go back" via a
  `close_update_viewer` helper, and switching to another tab closes the viewer
  so the next `u` does not silently reopen one.
- **A second, latent instance of the same bug.** The unguarded `f` tab-switch
  arm matched `^F` before the Description/viewer handlers could treat it as
  half-page scroll, so `^F` switched to the Files tab and `^B` did nothing. The
  four tab arms now carry `if !key.modifiers.contains(KeyModifiers::CONTROL)`.
  Not in the review; found while writing the tests.
- **`?` was never broken.** The review lists it with `n`/`N`, but it is handled
  globally in `App::handle_key` *before* the screen dispatch, so it worked in
  the unfixed tree. Recorded here (and in the harness README) so a future
  session does not "fix" it twice or use it as a probe that passes for the
  wrong reason.
- **#14 — render-time publishing: documented, not removed.** The exception is
  kept and made explicit. The `Cell`/`RefCell` fields scattered across `App`
  are consolidated into one `PublishedMetrics` struct with a written contract:
  renderers own it, values are one frame old and must be treated as hints, and
  a zeroed value must degrade to "no scrolling". That is the reason
  `nudge_scroll_into_view`'s `.max(1)` and the `.min(max_scroll)` clamps exist,
  which the review read as noise. The alternative — a separate measure pass —
  would run the markdown/layout code twice per frame to avoid a `Cell`; the
  decision and its rationale are recorded on the type and in AGENTS.md, which
  is the session's stated exit criterion.
- **#15 — `Warn` no longer eats the key.** The notice still closes on any key,
  but a *safe* key is now replayed through `App::handle_key`, so a reflexive
  second `e` at the edit-conflict warning opens the editor. Replay is a closed
  allow-list (`is_redispatchable`): plain navigation/editing characters plus
  `Esc`. Keys that open an overlay (`i`, `?`, `n`, `T`, `D`, `/`, `r`, `m`,
  `S`, `Enter`) are excluded — replaying one would raise a new overlay while
  the caller is still unwinding from the old one, and the nesting would have no
  bound.
- **Hint bar and Key Help made truthful.** The hint strings moved into a
  testable `hints_for(app)`; `n/N` is no longer advertised on the Todos tab
  (where `n` adds a todo); the viewer's string names the keys the viewer
  actually accepts. `key_help_lines()` gained an update-viewer section, lost
  the "Update viewer" qualifier from the tab-level `j/k`/`^f`/`g` rows, and
  says what `ESC` does in each place.
- **A third dead binding, found while making the hint bar truthful.** The
  manual documents `e` **or** `n` as "edit the description" on the Description
  tab, but the universal `n` arm consumed `n` for match navigation
  unconditionally and `handle_key_pd_description` has no `n` arm, so with no
  search running `n` did nothing. `n` now navigates matches when the query has
  any, edits the description on the Description tab otherwise, and still falls
  through to "new update"/"new todo" on the other tabs.
- Tests: 478 → **495**, all green (37 + 82 + 244 + 132). The first tests in
  `app.rs` (16) and `project_detail.rs` (5). They build a real `App` on an
  in-memory database (`App::with_conn`, newly factored out of `App::new`) and
  drive real `KeyEvent`s; `draw()` renders through `TestBackend` so the
  render-published metrics are populated exactly as in the TUI. Covered: `n`/`N`
  in the viewer, `?` in the viewer, `ESC` closing the viewer without leaving
  Project Detail, tab switches closing the viewer, `E`/`T` in the viewer, `^F`
  scrolling rather than switching tabs, `n` in all three of its meanings
  (next match / edit description / new item), `Warn` redispatch and its two
  negative cases, and the hint-bar strings. Each fix's test was confirmed to
  fail against the unfixed tree.
- **New harness `dev-harness/update-viewer.py` (#6/#23), 4/4.** This finding
  had no harness because it needs a terminal; the harness drives the real
  binary in a **pty**, answers the `ESC[6n` cursor-position query (without which
  ratatui aborts), and asserts on markers in the rendered screen. Re-run
  against the unfixed tree: **2/4**, failing exactly the `d` and `E` checks.
  #14 and #15 have no external symptom and are covered by the unit tests
  instead; `?` is deliberately not used as a probe (see above).
- `wal-safety.py` **23/23**, `sync-state.py` **20/20**, `merge-robust.py`
  **6/6**, `s3-roundtrip.py` **12/12**, `panics.py` **9/9** — unchanged.
  `search-highlight.py` still fails; its finding is session G.
- `AGENTS.md` gains §The exception: `PublishedMetrics`, §Key scopes in Project
  Detail (with the ordering diagram and the Ctrl-modifier rule), and §Overlay
  dismissal and redispatch; §Rendering flow no longer claims rendering is
  unconditionally pure, §How to Add a Keybinding gains the scope and hint-bar
  rules, and §Warn documents the replay.

---

### F — Data-layer correctness & perf — **done**

Findings #10 and #11 fixed. `repo.rs` is where the fix lives; `app.rs` changed
in one place because #11's fix makes a distinction the caller has to honour.

- **#10 — the N+1.** `list_projects` ran `load_project_tags` and
  `load_soonest_reminder` **per project row**, and `list_todos_global` then ran
  one more query **per project** for its todos. Measured with SQLite's
  `SQLITE_TRACE_STMT` callback: **61 statements for 30 projects** (3 for one),
  and **92 for `list_todos_global` at 30 projects** (5 for one). Both are now
  constant:
  - The `soonest_reminder` subquery is a single `const SOONEST_REMINDER_COL`
    inlined into the project `SELECT`. This is now the *only* definition of
    "soonest reminder" in the data layer — the old standalone
    `load_soonest_reminder` was deleted rather than left beside it, so the two
    cannot drift. It keeps the "active todos only" rule, which is what makes
    the reminder vanish from the Project List when its todo is completed; tests
    assert the list and `get_project` agree.
  - Tags come from one `LEFT`-free `JOIN` with an `IN (…)` placeholder list
    (`load_tags_for_projects`), returning a `HashMap<project_id, Vec<Tag>>`.
    `ORDER BY t.name` keeps each project's list alphabetical, as before.
    `get_project` reuses the same helper for its single id, so there is one
    tag-loading path, not two.
  - `list_todos_global` issues **one** todo query for the inbox *and* every
    project, then buckets the rows in Rust. `NULL IN (…)` is never true, which
    is exactly the inbox-exclusion the old `tag_filter.is_none()` guard wanted.
    The `IN ()` form SQLite rejects is never generated: an empty project list
    with a tag filter returns early, and the empty-id tag query returns without
    running anything.
  - Result: **2** statements for `list_projects` and **3** for
    `list_todos_global`, at any row count.
- **#11 — the swallowed error.** `rename_tag`'s collision probe used
  `.map(|n| n > 0).unwrap_or(false)`, so a locked database, a corrupt index or
  a denied read was reported as "no collision" and the rename proceeded.
  The probe now propagates (`?`), and the two outcomes are **typed** rather
  than a single string: `RenameTagError::NameTaken` vs
  `RenameTagError::Db(anyhow::Error)`. The `app.rs` caller matches on that,
  so a database failure is surfaced as an error instead of being rewritten as
  `'X' already exists`. The stale `QueryReturnedNoRows` comment (which
  described a sentinel the code no longer used) is gone.
- Tests: 495 → **517**, all green (37 + 82 + 266 + 132). New `repo.rs` tests:
  - *Query counts* — a `count_statements` helper driven by
    `SQLITE_TRACE_STMT`, asserting the count is **equal at 1 and 21 projects**
    for both functions and small in absolute terms, plus the no-result and
    no-project fast paths. The helper needs the `trace` feature, which is
    declared as a **dev-dependency** feature so the shipped binaries do not
    carry it (same for `hooks`, used by the #11 test).
  - *Behaviour preserved* — tag order, untagged projects, tag filter + search
    combined, todo order within a band, archived filtering, inbox-only,
    empty database, and five reminder tests including "the list agrees with
    `get_project`" and "done/canceled todos are excluded".
  - *#11* — a SQLite authorizer denies reads of `tags.name` (the column the
    probe reads and the `UPDATE` does not), asserting `rename_tag` errors, does
    not rename, and does not say "already exists"; plus a test that the two
    error variants are distinguishable. Two `app.rs` tests cover the caller:
    one for the collision path (the overlay stays open with `'X' already
    exists`), one for the database-failure path (the error propagates instead).
    The latter denies only the **first** read of the column, because denying
    every read would not discriminate — the post-rename reloads read it too,
    so an error would surface either way.
  - Each fix's test was confirmed to fail against the unfixed tree by
    re-introducing the defect (counts 2 vs 22 and 4 vs 24; the authorizer case
    passed through).
- **New harness `dev-harness/query-count.py` (#10/#11), 8/8.** Neither finding
  has an external symptom — the list renders correctly and the UI says the same
  thing either way — so the harness compiles the **real `src/data/repo.rs`**
  (path-included, with the schema lifted textually out of `db.rs`) into a
  scratch crate and measures statement counts and error propagation directly.
  Re-run against the unfixed tree: **2/8**, failing exactly the #10 and #11
  checks.
- `wal-safety.py` **23/23**, `sync-state.py` **20/20**, `merge-robust.py`
  **6/6**, `s3-roundtrip.py` **12/12**, `panics.py` **9/9**,
  `update-viewer.py` **4/4** — unchanged. `search-highlight.py` still fails;
  its finding is session G.
- `cargo clippy --all-targets` reports **no new warnings** from `repo.rs`.

---

### G — Unicode/display-width consistency — **done**

Findings #16 and #17 fixed. Both were "the same string measured or sliced two
different ways", so the fix is one shared module rather than a patch per view.

- **#16 — width was counted in characters.** `word_wrap`, `char_wrap`,
  `hint_height` and the inline-edit window all counted `chars()`, while
  `markdown.rs` already measured with `unicode-width`. A CJK title therefore
  wrapped at half the intended column count and overflowed the pane; a wide
  hint string was given too few rows. The measurement now lives once, in
  **`src/ui/text.rs`**: `display_width` / `char_display_width` /
  `char_wrap` / `word_wrap` / `wrap_rows` / `split_at_width` / `pad_to_width` /
  `truncate_to_width` / `hint_height`. Every renderer in both binaries goes
  through it — `views/mod.rs` re-exports the wrapping helpers as `super::…`,
  `markdown.rs` imports the two width primitives instead of defining its own
  copies, and `md` path-includes the same file (`bin/md/ui.rs`). Two view-level
  char-counted computations that were genuinely wrong are fixed too:
  `update_content_lines` padded its date/label column with
  `format!("{:<width$}")` (character padding, so a wide label pushed the body
  column right) and now uses `pad_to_width`, and the global-todo group rule
  was sized from `project_title.len()` (bytes) and is now sized from
  `display_width`.
- **#17 — the highlighted range drifted under case folding.** `split_span`
  (in `views/project_detail.rs`, duplicated as `split_span_hits` in
  `bin/md/main.rs`) lowercased the text, searched the lowercased copy, then
  sliced the **original** at those offsets. Folding is not length-preserving
  (`'İ'` → `i` + U+0307), so searching `İ` in `İstanbul` highlighted `İs`, and
  a match starting after the fold highlighted the wrong region. The matcher is
  now `ui::text::find_matches` / `split_matches`, which records the original
  character index for every lowercased character, so a match always maps back
  onto whole original characters. Both call sites use it; the two copies are
  gone.
- **The inline-edit field took a column budget and spent characters.**
  `InputState::display(width)` overflowed by one column per wide character and
  could scroll the cursor character off the field. It now measures in columns
  (`char_display_width`) and reserves the cursor cell at its own width.
- Tests: 517 → **651**, all green (37 + 145 + 337 + 132, 2 ignored). The new
  coverage is where the bugs were:
  - `ui/text.rs` — 57 tests: wide characters as two columns in every wrapper,
    an over-wide character still making progress (the width analogue of the
    #4 non-termination bug), padding/truncation never splitting a wide
    character, and the case-fold mapping (`İ` at the start, `İ` mid-string, a
    match after the fold, reconstruction of the original from the segments).
  - `views/project_detail.rs` — the real `split_span` (not a model of it):
    no drift on `İ`, every character preserved, occurrences counted.
  - `views/overlay.rs` — the spell-check preview and the suggestion row wrap
    by columns.
  - `views/todo_list.rs` — the first tests in that file: a rendered frame
    (`TestBackend`) whose CJK todo title wraps onto several rows and whose CJK
    group header keeps its right border.
  - `input/mod.rs` — `display` never exceeds the column budget at any width or
    cursor position, and the cursor character stays visible in wide text.
  - `bin/md/main.rs` — the first tests in that file: the `md` highlighter does
    not drift, `hint_height_fn` counts columns, the preview wraps by columns.
  - **Each fix's test was confirmed to fail against the unfixed code** by
    re-introducing the defect (the drift test, the label-padding test, the
    preview and suggestion tests, the CJK-title render test, and the
    `InputState` budget test all fail on the old code).
- **New harness `dev-harness/width-display.py` (#16), 33/33.** #16 has no
  crash and, with a narrow enough terminal, no obvious symptom either, so the
  harness compiles the **real `src/ui/text.rs`** into a scratch crate and
  asserts that every wrapped row fits its column budget and that no text is
  lost, across CJK, emoji, mixed and empty input. It also checks that the
  renderers route through `ui::text` and that no character-counted width
  arithmetic is left in them.
- `search-highlight.py` **rewritten from 4/5 to 14/14.** The old version
  reimplemented the algorithm in Python, which can neither confirm a fix
  (the copy keeps failing) nor guard one (a reverted fix still passes). It now
  compiles the real `src/ui/text.rs` and asserts on its output, keeps a
  companion check that the *legacy* algorithm still mis-highlights the same
  fixtures (so the cases are not vacuous), and greps both call sites for the
  lowercased-offset pattern.
- `panics.py` still **9/9**. Its #4 check extracts `wrap_key_row` textually and
  compiles it standalone, so it had to build through a scratch cargo crate to
  resolve `crate::ui::text` — a harness change forced by the fix, and a
  legitimate one: the extracted code is still the real source.
- **A pre-existing red test was fixed on the way.** `cargo test --all-targets`
  was already failing before session G (`ops::tests::
  a_dry_run_end_neither_sets_nor_clears_the_marker`) because rustls cannot
  choose a `CryptoProvider` when both `ring` and `aws-lc-rs` are compiled in,
  and the provider was installed only in `main()`. `install_crypto_provider()`
  is now called from `S3Client::new`, which covers every caller including the
  tests. Not a Session G finding, but a session cannot exit "suite green"
  while the suite is red.
- `wal-safety.py` **23/23**, `sync-state.py` **20/20**, `merge-robust.py`
  **6/6**, `s3-roundtrip.py` **12/12**, `panics.py` **9/9**,
  `update-viewer.py` **4/4**, `query-count.py` **8/8**, `keyhelp-widths.py`
  diagnostic — unchanged.
- `cargo clippy --all-targets` reports **no new warnings**; the remaining ones
  are the pre-existing set in `markdown.rs`, `crypto.rs`, `merge.rs` and
  `archive.rs` that session H owns.

### H — Dedup, permissions, docs sweep — **done**

Findings #18, #19, #20, #21 and #22 fixed; #24 measured and **documented as a
known cost** rather than changed.  Clippy is now at zero warnings.

- **#19 — one XDG lookup.**  There were four copies with three different
  `$HOME` fallbacks (`.` in `app.rs` and `log.rs`, an error in `paths.rs`, a
  silent skip in `spell.rs`), so `ptm` and `ptm-sync` could disagree about
  where the database lives — fatal for `ptm-sync`, which hashes and packs the
  file `ptm` writes.  `src/paths.rs` now owns `data_home` / `app_data_dir` /
  `optional_app_data_dir` / `app_data_file`, and all four binaries use it
  (`md` path-includes `paths.rs` now too).  Two deliberate asymmetries, both
  documented in the file: `app_data_dir` falls back to `./.local/share` because
  a TUI with nowhere to put its database is worse than one in an odd directory,
  while `expand_tilde` still errors because a private key must not land in `.`.
  A relative `$XDG_DATA_HOME` is ignored per the spec.
- **#21 — one logger.**  `src/log.rs` is now the only implementation; `md`
  path-includes it and selects its directory with `set_app_name("md")`, so
  `bin/md/log.rs` is a five-line wrapper.  The two copies had already drifted:
  only `md`'s created the log directory, so `ptm` silently dropped the first
  warning of a fresh install.  `warn` now creates the directory on demand for
  both.
- **#20 — `write_key` re-asserts `0600`.**  `OpenOptions::mode()` only applies
  at creation, so `enc keygen --force` over a `0644` key left it world-readable.
  The mode is now re-applied with `File::set_permissions` on the open
  descriptor (not by path, so there is no swap window).  The regression test
  fails on the old code (`left: 420, right: 384`).
- **#18 — the status cycle is documented.**  Both Key Help rows now read
  `Cycle status: New → In Progress → Done → Canceled → New`, so nobody has to
  press `s` three times to discover the order.
- **#22 — stale comments and docs.**  `repo.rs`'s `swap_update_order` no longer
  claims to reset sort orders (it swaps two; updates are prepended, so a group
  runs `0, -1, -2, …`); `project_detail.rs`'s misplaced `current_marker` doc
  now sits on `current_match` and explains the n/N contract; the three mid-file
  `use` blocks in that file are hoisted to the top.  **`AGENTS.md` now matches
  the tree**: `reference_library.rs` (which never existed) is gone, `tz.rs`
  (which did exist) is listed, `input/mod.rs` is described as implemented, and
  the shared-file rule is spelled out as a gotcha.  A new test,
  `docs_tests::agents_md_tree_matches_the_source_tree`, parses the tree out of
  `AGENTS.md` and diffs it against `src/`, so the next drift fails `cargo test`
  instead of misdirecting a session.  It was confirmed to fail on the
  `reference_library.rs` line.
- **#24 — the duplicate crypto backend: measured, not changed.**  Two candidate
  fixes were implemented and tested; both are worse than the status quo, so the
  finding is now a documented decision in `Cargo.toml` and on
  `install_crypto_provider`:
  - *Keep `aws_lc_rs`, drop `ring`*: one backend, a 7 MB smaller binary,
    verified against real S3 (a 403 from `head_object`, i.e. a completed
    handshake).  Rejected because `aws-lc-sys` is C and needs cmake/nasm/perl,
    which `build/Dockerfile*` does not install — it would break the macOS
    zigbuild and musl cross-builds.
  - *rust-s3 0.38 + `aws-creds/rustls-tls-ring`*: `cargo tree -i aws-lc-sys`
    reports nothing.  Rejected because attohttpc 0.30.1 only understands
    `__rustls`, not `__rustls-ring`, so the build silently compiles **TLS
    out** — `head_object` fails with "TLS is disabled, activate one of the
    tls- features", which is a silent production downgrade dressed as a fix.
  - What *is* fixed is the panic: a test now pins that the provider is
    installed before credentials are read, so a broken `~/.aws/credentials`
    surfaces as an AWS error rather than a rustls abort.  The comment also
    corrects a claim inherited from session G: the install is needed even with
    a single backend, because `rustls/default` (via attohttpc) disables
    rustls's auto-detection.  Verified by removing the call from a
    single-backend build.
- **Clippy to zero.**  The `--fix`-able set is applied by hand (redundant
  closures in `markdown.rs`, `map_or` → `is_some_and` in `archive.rs` /
  `merge.rs`, `mem::replace` → `mem::take`, a useless `anyhow::Error::from`,
  needless borrows in `crypto.rs` tests).  The `md` build's seven "field is
  never read" warnings for `Theme` are an `#[allow(dead_code)]` on the
  path-include with a comment: `md` constructs a `Theme` because the markdown
  renderer takes one, but reads none of it.
- Tests: 651 → **679**, all green (41 + 153 + 346 + 139, 14 ignored — the
  ignored ones are child processes their parent spawns, the same pattern
  `paths.rs` already used for `$HOME`).  New coverage: the XDG lookup's three
  cases (`$XDG_DATA_HOME` wins, relative is ignored, neither variable set),
  `ptm`/`ptm-sync` agreeing on the data dir, `md` logging under `md/` and
  creating the directory, `write_key` restoring `0600`, the provider being
  installed before credentials are read, and the `AGENTS.md` tree check.
  Every fix's test was confirmed to fail against the unfixed code.
- Harnesses: **all 10 clean**.  `query-count.py` needed one line — its scratch
  crate mirrors the module tree and now has to include `paths.rs`, because
  `log.rs` depends on it.  That is the harness tracking a real source change,
  not a check being weakened.
- `s3-roundtrip.py` **skips** in this container: the `/etc/hosts` entry the
  plan documents (`127.0.0.1  b.ptm-harness.localhost`) is host state and does
  not survive a container rebuild.  The TLS path was therefore verified
  directly instead: a scratch crate with the same `rust-s3` + provider setup
  completes a real handshake against S3 (403 for bogus credentials), and a
  probe with no provider installed reproduces the rustls abort — which is what
  makes the `install_crypto_provider` comment's claim checkable rather than
  asserted.

---

## Grouping principle

Three rules drive the split:

1. **Same root cause or same file → same session.** Otherwise you fix half a
   mechanism and the second half regresses.
2. **Never mix a data-safety fix with a cosmetic sweep.** The first needs a
   reproduction and a careful re-verify; the second needs breadth.
3. **Every session must end shippable** — builds, `cargo test --all-targets`
   green, no half-migrated call contracts.

### Why these boundaries

**A is indivisible.** #1a (lock probe), #1b (ignored checkpoint tuple) and
#1c (rename over live db) are three faces of one mechanism, and #2 is the same
defect seen from `--end`. Fixing #1b alone changes the contract #1c and #2
depend on: `check_and_checkpoint_wal` returns a bare `bool` today, and needs to
hand back a still-held connection plus a `log_frames` count. Splitting this
creates a state where the checkpoint is verified but the install still
corrupts — worse than today, because it would be trusted.

**B follows A because both rewrite `ops.rs` control flow**, and #3 is the other
half of the same state machine: `.db_hash` is the only thing deciding whether
`--end` pushes. Grouping #3 with #12 is deliberate — both are "`--start`/`--end`
report success when they should refuse, or refuse when they should proceed."
B also owns the decision on #2's silent exit 0, so exit-code semantics are
settled in exactly one session.

**C is one file, one mental model** (SQL merge semantics), and its only
cross-dependency is B's one-line `.db_hash` sentinel in `merge.rs:200`. Run C
after B and the ordering is trivial.

**D is the "crash" bucket**: #4 (byte-sliced `&str` in `wrap_key_row`), #5
(`[2..]` on `"~"`), #13 (`&r[..10]` on a non-ASCII reminder). All three are
byte-index-on-UTF-8 bugs with pure-function fixes and unit tests. Cheap, high
user-visible value, no interaction with anything else. #5's *fix* is "reuse
`enc`'s `expand_tilde`", which means extracting it into a `#[path]`-included
shared module — the same move H wants for #19/#21, so D establishes the pattern
and H generalizes it.

**E belongs together because #6 and #23 are the same bug** — `n`/`N` are
dead in the update viewer because the `pd_open_update` early return at
`app.rs:1088` precedes the universal-key block. (`?` is *not* affected — it is
handled globally, before the screen dispatch; the session log records this.) The correct fix (move the
viewer guard below universal keys) immediately raises #14: `n`/`N` depend on
`pd_match_marks`, which the renderer publishes via `RefCell`. You cannot sanely
reorder those key paths without deciding what the publish/consume contract is.
#15 (Warn overlay eats the key) is the same overlay-semantics question plus a
doc line.

**F is self-contained** in `repo.rs` and has a measurable success criterion
(query count per keystroke of the search box), which makes it the best
candidate to run alongside D/E if worktrees ever become available — no file
overlap with any other session.

**G is a different kind of bug from D**: #16/#17 don't panic, they render
wrong, and the fix is one shared width-aware helper used everywhere. Doing it
in D would balloon a 3-line-per-fix session into a repo-wide refactor. It
turned out to be one new file (`ui/text.rs`) plus call-site changes — the
helpers existed already, they were just duplicated and character-based.

**H is the sweep** — it touches nearly every file, so it must be last or it
conflicts with everything. It also absorbs the `cargo clippy --fix` pass the
review suggests, plus the `AGENTS.md` corrections in #22
(`reference_library.rs` does not exist; `input/mod.rs` is fully implemented —
both actively mislead future sessions).

## Order

```
A → B → C            (critical path: data loss)
D → E → F → G → H    (D/E cheap and high value; F independent; G/H last)
```

D can be pulled to the front for a fast win before tackling A — it shares no
files with A/B/C.

## Per-session procedure

Every session runs the same four steps. Steps 1 and 4 are not optional: they
are what makes the review's claims falsifiable.

### 1. Reproduce first

```bash
cargo build                      # harnesses default to target/debug
python3 dev-harness/check-all.py <harness-name>
```

The harness prints `PASS`/`FAIL` per check and names the finding each check
belongs to. A `FAIL` on a check labelled *"FAIL = finding N is live"* means the
bug is still present — that is the expected state before you start.

If a finding has **no** harness yet, write one before fixing it, in
`dev-harness/`, following the conventions in `dev-harness/README.md`:

- work in a `tempfile.mkdtemp()` directory,
- pass `XDG_DATA_HOME` in the child environment only,
- never touch `~/.local/share/ptm/` or the user's real key,
- print `PASS`/`FAIL` via `ptmdb.Results` and exit non-zero on failure,
- say in the header which finding it covers and what `FAIL` means.

Use `ptmdb.Holder` when you need a database held open by another process —
SQLite's POSIX advisory locks are per-process, so a thread cannot stand in for
a running `ptm`.

### 2. Fix

One finding at a time within the session, `cargo test --all-targets` after
each.

### 3. Add a permanent regression test

The harness is a reproduction; the fix needs a test that lives in `cargo test`
so it cannot regress silently. `repo.rs`, `crypto.rs`, `merge.rs`, `archive.rs`
and `InputState` already have good coverage — extend those. `app.rs`,
`overlay.rs` and the three view modules have **zero** coverage, which is
exactly where #4, #6, #13, #14, #15 and #23 live; each of those fixes should
add the first unit tests in its file.

### 4. Re-run the harness

```bash
python3 dev-harness/check-all.py <harness-name>
```

All checks for that session's findings must now pass, and the other harnesses
must be no worse than before. Commit the harness alongside the fix — if a
harness has to change to pass, that is a signal the fix changed behaviour
rather than repairing it, and it needs justifying in the commit message.

## Session exit criteria

- **A** — `wal-safety.py` fully green. New test that spawns a *separate
  process* holding the db open (the note in `db.rs` correctly explains why an
  in-process test cannot cover this), plus an assertion that packed bytes come
  from a checkpointed db. Re-verify the lost-pull scenario and confirm
  `PRAGMA integrity_check` is `ok`.
- **B** — `sync-state.py` fully green. Test that `--merge` → `--start` →
  `--end` actually marks state for upload, and that `--end` with a busy WAL
  returns non-zero.
- **C** — `merge-robust.py` fully green: legacy local db merges, dangling-FK
  source merges, `updates` ordering is `L1, L2, L3, R1, R2`.
- **D** — *met.* `panics.py` fully green (9/9). The key-help test sweeps
  `desc_width` **0..=60** (0 added: it looped forever rather than panicking),
  not terminal width. `expand_tilde` handles `~`, `~/`, `~x` and unset `$HOME`;
  `enc`'s tests were not "ported" but **moved** into `src/paths.rs`, where all
  three binaries run them.
- **E** — *met.* `n`, `N`, `E`, `T` and the tab switches work inside the
  update viewer (`?` already did — see the session log). The hint bar and
  `key_help_lines()` are asserted against each other by a test. The
  render-purity decision — keep the exception, document it — is recorded on
  `PublishedMetrics` and in AGENTS.md. `update-viewer.py` is the harness.
- **F** — *met.* `query-count.py` **8/8** (2/8 against the unfixed tree),
  asserting the statement count is **equal at 1 and 30 projects** for both
  `list_projects` (2) and `list_todos_global` (3), not merely "small": a
  counting `Connection` wrapper was considered and rejected because it cannot
  see statements issued inside `repo.rs` and would miss the reminder subquery
  entirely. Counting is done by SQLite's `SQLITE_TRACE_STMT` callback instead.
  #11 is covered by the same harness via an authorizer that denies the probe's
  column.
- **G** — *met.* `search-highlight.py` **14/14** (real `ui::text`, not a
  model of it) and `width-display.py` **33/33**. CJK and emoji titles are
  covered at three levels: the pure helpers (`ui/text.rs`), the renderers that
  use them (`overlay.rs`, `todo_list.rs` via a rendered frame), and the
  inline-edit window (`input/mod.rs`). The two char-counted view computations
  that were genuinely wrong (`format!("{:<w$}")` label padding, the byte-sized
  group rule) are fixed as well, because "one shared width-aware helper used
  everywhere" is the exit criterion, not "the two cited functions".
- **H** — *met.* `cargo clippy --all-targets` at **zero** warnings (from 32);
  the AGENTS.md tree matches `src/` and is now **enforced** by a test rather
  than by review — `docs_tests::agents_md_tree_matches_the_source_tree`
  parses the tree block and diffs it against the filesystem, so a stale line
  fails `cargo test`.  #19/#21 are met by deletion rather than by a rule:
  there is one XDG lookup and one logger, and the three copies each replaced
  are gone.  #20's `0600` re-assert is tested by creating a `0644` key and
  overwriting it.  #24 is closed as a **documented decision**, not a code
  change: both available fixes were implemented and measured, and both are
  worse (one adds a C toolchain to the cross-build images; the other silently
  compiles TLS out), so the finding is recorded in `Cargo.toml` with the
  measurements and the panic is covered by a test instead.

## Findings that need correcting first

The harnesses disagree with the review in three places. Correct the review (or
at least carry these notes into the sessions) before acting on them, or a
session will chase a bug that is not there.

### #1a — the WAL liveness probe is not unsound

The review claims `BEGIN EXCLUSIVE; COMMIT;` *succeeds* while another process
holds the db open, so `ptm-sync` concludes "stale WAL" and proceeds. **It does
not.** With a real child process holding the db exactly as `ptm` does
(`PRAGMA journal_mode=WAL` + a `SELECT`, no open transaction), `BEGIN
EXCLUSIVE` fails with `database is locked` after the busy timeout, and
`check_and_checkpoint_wal` correctly returns `Ok(false)` — `--end` then logs
"Database in use by another process; skipping push".

Consequences for session A:

- Do **not** rewrite the probe on the assumption that it always succeeds.
- The real defect in this area is **#1b**: `execute_batch` hides the
  `(busy, log, checkpointed)` tuple, so a checkpoint that could not advance is
  reported as success. Fix that, and make the function return the counts.
- Keep the "hold the lock for the duration" idea — it is still the right shape
  for #1c — but justify it as removing the gap between probe and install, not
  as repairing a broken probe.
- `busy_timeout` genuinely is unset here (`src/data/db.rs:12` uses 5 s), so the
  probe fails *instantly* rather than waiting. That is a real, smaller issue.

### #1c — the corruption variant is unconfirmed

The lost-pull variant reproduces exactly as described: rename a staged db over
a db with a live `-wal`, let the live session exit, and the local data wins
while `--start` reports success.

The *`database disk image is malformed`* variant did **not** reproduce across
several attempts (page sizes 4096 and 8192, reader and writer holders,
differing schemas); `PRAGMA integrity_check` returned `ok` each time. Treat
"malformed" as unconfirmed, and make the fix's test target the lost-pull
variant, which is deterministic.

### #4 and #7 — details are slightly off

- **#4**: the panic depends on the computed `desc_width`, not the terminal
  width. The observed panicking `desc_width`s are **1, 2, 5, 6, 19, 20**. Write
  the regression test against `desc_width`. Session D also found that
  `desc_width == 0` **looped forever** (not a panic), so the sweep covers
  `0..=60`, not `1..=60`.
- **#7**: the first column to fail on a legacy local db is `todos.archived`,
  not `updates.label` (`table todos has no column named archived`). The fix —
  call `ensure_schema_compat` on the local connection — is unchanged, but the
  test should assert on a genuinely legacy shape (`dev-harness/lib/ptmdb.py`'s
  `legacy_db()` builds one).

## End-to-end S3 coverage: one hosts entry

The harnesses cover everything up to the S3 call — WAL handling, checkpointing,
hashing, staging, the local file install, and exit codes — and
`dev-harness/s3-roundtrip.py` completes the cycle with a real
`--end` → `--start` round trip against a local fake endpoint, **provided this
line is in `/etc/hosts`**:

```
127.0.0.1  b.ptm-harness.localhost
```

**The entry must name the subdomain, not the bare host.** `/etc/hosts` does
exact matching only — there are no wildcards — so a line for
`ptm-harness.localhost` alone will not resolve `b.ptm-harness.localhost`, and
the harness will still skip.

Why: `rust-s3` builds the request host as `<bucket>.<region-host>` and
`S3Client` never calls `.with_path_style()`, so the name that must resolve is
`b.ptm-harness.localhost`. `AWS_REGION=http://ptm-harness.localhost:<port>`
parses to `Region::Custom` (any unrecognised string does), giving
`scheme=http`, `host=ptm-harness.localhost:<port>`, and the request URL
`http://b.ptm-harness.localhost:<port>/ptm-backups/ptm-backup.zip.enc`.
The fake server in `dev-harness/lib/fakes3.py` handles HEAD/GET/PUT.
`HOSTALIASES` is not a substitute — glibc ignores it for names containing dots.

**Status: with the entry present `s3-roundtrip.py` passes 12/12**; in a
container that has been rebuilt since (the current one) it prints `SKIP` with
the line to re-add.  It verifies:

- `--end` uploads and records the ETag;
- a second `--end` with nothing changed does not re-upload;
- `--start` pulls into an empty dir, intact, with no stale `-wal`;
- `--start` with the db already held open exits 0 and leaves local data alone
  (the concurrent-launch promise in AGENTS.md).

Two things to keep in mind when reading that result:

1. **12/12 does not contradict the review.** It is the happy path. Finding #1c
   needs a `-wal` that is live *at install time*; a closed writer checkpoints
   and removes its WAL, so the scripted case is safe.
2. **The race variant is not asserted.** Racing a live writer across the
   rename was implemented and then removed: which side wins is undefined
   behaviour and gave different results on different runs. A flaky check is
   worse than none. `wal-safety.py` stages the same state deterministically
   and is the authority for #1c.

The value for sessions A and B is regression protection: those two behaviours
are not observable from the state files alone, which is why the rest of the
suite has to stop short of the network. If session A changes the WAL handling,
`--start` with the db in use must still exit 0 with local data intact — and
`s3-roundtrip.py` is what proves it.

If the entry cannot be added, the alternative is a small, deliberate product
change: honour an endpoint override and use path-style addressing when set.
That is a testability feature worth deciding on explicitly rather than
smuggling in — it touches `S3Client::new`, and `--start`/`--end` are exactly
the paths where a misconfiguration is most expensive. It is not needed to fix
any finding.

## Appendix: finding → harness map

| Finding | Harness | Currently |
|---|---|---|
| 1a | `wal-safety.py` | does not reproduce (see above) |
| 1b | `wal-safety.py` | PASS — fixed in A |
| 1c | `wal-safety.py` | PASS — fixed in A (lost-pull variant) |
| 2 | `wal-safety.py` | PASS — fixed in A (snapshot), exit code in A+B |
| 3 | `sync-state.py` | PASS — fixed in B |
| 4 | `panics.py`, `keyhelp-widths.py` | PASS — fixed in D |
| 5 | `panics.py` | PASS — fixed in D |
| 6 | `update-viewer.py` | PASS — fixed in E (pty harness) |
| 7 | `merge-robust.py` | PASS — fixed in C |
| 8 | `merge-robust.py` | PASS — fixed in C |
| 9 | `merge-robust.py` | PASS — fixed in C |
| 10 | `query-count.py` | PASS — fixed in F (2/3 statements at any row count) |
| 11 | `query-count.py` | PASS — fixed in F (probe error propagated) |
| 12 | `sync-state.py` | PASS — fixed in B |
| 13 | `panics.py` | PASS — fixed in D |
| 14 | — | documented (unit-tested via the handlers that read it) |
| 15 | — | PASS — fixed in E (unit test; no external symptom) |
| 16 | `width-display.py` | PASS — fixed in G (columns everywhere) |
| 17 | `search-highlight.py` | PASS — fixed in G (shared matcher) |
| 2, 3, 1c (partial) | `s3-roundtrip.py` | 12/12 when the `/etc/hosts` entry is present; **skips** in a rebuilt container (happy path; #1c race not asserted — see above) |
| 2 (exit code) | `sync-state.py`, `wal-safety.py` | PASS — fixed in A, marker added in B |
| 18 | — | documented (Key Help names the cycle order) |
| 19 | — | fixed in H (one XDG lookup in `paths.rs`; unit-tested) |
| 20 | — | fixed in H (`write_key` re-asserts `0600`; unit-tested) |
| 21 | — | fixed in H (one logger; `md` path-includes it) |
| 22 | — | fixed in H (comments + AGENTS.md tree; drift is now a test failure) |
| 23 | `update-viewer.py` | PASS — fixed in E |
| 24 | — | documented in H, not changed (both fixes measured and rejected) |
