# Code Review — `ptm` / `ptm-sync` / `md` / `enc`

> From clanky session: 20261101-152929-c8c3

**Build & test state (verified):** `cargo check --all-targets` clean; `cargo clippy --all-targets` → 32 warnings, all cosmetic (redundant closures, `map_or` → `is_some_and`, `max.max(0)`, unused `Theme` fields in `md`). `cargo test --all-targets` → **408 tests, 0 failures** (36 + 82 + 214 + 76 across the four binaries). Test coverage is genuinely good in the data layer, crypto, merge, archive, `InputState`, and markdown; it is **zero** in `app.rs`, `overlay.rs`, and all three view modules — which is exactly where the bugs below are.

**Note on method:** I attempted a parallel-subagent batch first; the jobs ran but the tool call hit its timeout with no captured output, so everything below is from my own reading plus **standalone reproductions** of each claim (Rust/SQLite/Python harnesses under `/tmp`). Every "verified" note is a result I actually observed, not a hypothesis.

---

## 🔴 Critical

### 1. `ptm-sync --start` can destroy the database (stale-WAL branch + non-atomic replace)

This is a data-corruption bug, and I reproduced it end-to-end.

Two independent defects combine:

**(a) WAL liveness detection is unsound** — `src/bin/ptm_sync/db.rs:32-49`

```rust
let result = conn.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;");
```

`BEGIN EXCLUSIVE; COMMIT;` acquires and *immediately releases* the lock, so it only detects a **concurrent writer**, never an idle-but-open session. Verified: with a second process holding the db open exactly the way `ptm` does (`PRAGMA journal_mode=WAL` + a `SELECT`, no open transaction), `BEGIN EXCLUSIVE` **succeeds** — so `ptm-sync` concludes "stale WAL, no other process" and proceeds to pull. That's precisely the "concurrent launch" case `--start` claims to support (AGENTS.md: *"`--start` skips the pull (allowing concurrent launch)"*).

**(b) `wal_checkpoint(TRUNCATE)`'s return value is ignored** — same file, line 40

`execute_batch` returns `Ok` even when the checkpoint is busy. Verified: with a reader pinning a read mark, `PRAGMA wal_checkpoint(TRUNCATE)` returns `(busy=1, log=…, checkpointed=0)` and **no error is raised**. The code logs *"Stale WAL detected; checkpointed and truncated"* while the WAL is still full. (`busy_timeout` is also never set here, so it fails instantly rather than waiting.)

**(c) The install is not atomic** — `src/bin/ptm_sync/ops.rs:265`

```rust
std::fs::rename(&tmp_db, &paths.db)
```

Renaming over a database whose `-wal`/`-shm` are still live is undefined. The stale WAL frames then get replayed into a *different* database.

**Reproduction (actually run):**

```
[ptm-sync] BEGIN EXCLUSIVE ok -> 'Stale WAL detected; checkpointed and truncated' (pull proceeds)
[ptm-sync] os.replace(staged, ptm.db) done; pull reports success
[after] files: ['ptm.db', 'ptm.db-shm', 'ptm.db-wal']
[after] *** DATABASE UNREADABLE: DatabaseError database disk image is malformed
```

`PRAGMA integrity_check` afterwards: `database disk image is malformed`; `sqlite_master` unreadable. A *different* variant (live session writes after the rename) silently **loses the entire pulled backup** — the local data and WAL win, and `--start` still reports success:

```
after live ptm exits, path contains: [('local-before-pull',), ('added by live ptm session',)]
=> the pulled remote backup is GONE; local data survives. Pull reported success.
```

**Fix:**
- Detect liveness by holding the lock for the duration of the operation, not by a `BEGIN/COMMIT` probe: keep an `EXCLUSIVE` connection open across the checkpoint *and* the file replacement.
- Check `wal_checkpoint(TRUNCATE)`'s `(busy, log, checkpointed)` tuple; if `busy != 0` or `log != 0`, treat as in-use and skip (or abort with `--block-concurrent`).
- Never `rename` over a live db. Use SQLite's own `VACUUM INTO` / backup API to write the pulled database *into* the existing connection, or refuse the pull while any `-wal` exists.
- Set `busy_timeout` before the probe (the app uses 5 s at `src/data/db.rs:12`; this file sets none).

---

### 2. `ptm-sync --end` uploads a stale snapshot whenever the WAL is non-empty

`src/bin/ptm_sync/ops.rs:332` and `:383`

```rust
let db_bytes = std::fs::read(&paths.db).context("Failed to read ptm.db")?;
...
let zip_bytes = archive::pack(&db_bytes, dict_bytes.as_deref())
```

The backup packs the raw `ptm.db` file, but committed data can live entirely in `ptm.db-wal` — and the checkpoint that was supposed to fold it in (finding #1b) silently no-ops. The `--end` hash check then compares against the same stale bytes, so the mismatch is never noticed.

**Verified:** 50 todos written in WAL mode; `PRAGMA` reports 50 rows; the raw `ptm.db` file copy (what gets zipped) contains **0** rows. In a second run the WAL was 424 KB against a 4 KB `ptm.db`.

Worse, when `--end` is skipped because the WAL is busy (`ops.rs:311-322`) it **returns exit code 0**, so the shell wrapper's `trap 'ptm-sync --end' EXIT` sees success and the session's work is never backed up at all.

**Fix:** after the checkpoint, assert `log_frames == 0` and `db_wal` size `== 0` before packing; if not, either fail loudly (non-zero exit) or open a read-only connection and `VACUUM INTO` a temp file to get a consistent snapshot. Same applies to `create_local_backup` (`archive.rs:110`) and `snapshot_before_merge` — the latter already does the right thing via `VACUUM INTO`.

---

### 3. `--start` seeds `.db_hash` from the *current* db, defeating the unsynced-changes guard

`src/bin/ptm_sync/ops.rs:77-91`

```rust
if !paths.db_hash.exists() {
    let db_bytes = std::fs::read(&paths.db).unwrap_or_default();
    let hash = compute_hash(&db_bytes, dict_bytes.as_deref());
    ...
    std::fs::write(&paths.db_hash, &hash)
```

The comment says this is "first launch" only — but `paths.db_hash` is also absent whenever `--merge` deleted it (`merge.rs:200`) or a user removed it. In those cases `--start` snapshots the **merged-but-unpushed** state as the sync baseline, so the next `--end` sees "no changes detected" and **never uploads the merge result**. The whole point of `merge.rs` deleting `.db_hash` (*"next --end will push the merged state"*) is silently undone by the next launch.

**Fix:** have `--merge` write a sentinel (e.g. `.db_hash` containing a value that can never match a real hash, or a separate `.needs_push` marker) instead of deleting the file, and make `--start` respect it.

---

## 🟠 High

### 4. Panic in the Key Help overlay at narrow terminal widths

`src/ui/views/overlay.rs:430` and `:434`

```rust
let cut = remaining[..=max_desc].rfind(' ').unwrap_or(max_desc);
let chunk: &'static str = &d[pos..pos + cut];
```

`max_desc` is a **byte** count used as a slice bound on a `&str` containing multi-byte characters (`→` is 3 bytes, in three of the key-help rows). When the boundary lands inside a character this panics.

**Verified** with a faithful copy of the function over all 56 `key_help_lines()` descriptions: pressing `?` panics at terminal widths **21, 22, 25, 26, 39, and 40** (`→ Todo List`, `→ Project List`, `Quick capture todo → Inbox`). At other narrow widths (3–18 for the longer descriptions) it doesn't panic but **silently drops characters** — verified `"Toggle hide done / canceled (Todos tab)"` at `max_desc=7` renders as `"Toggle hide done / cancele (Todos tab)"`.

Note `wrap_key_row` also mixes `d.len()` (bytes) with `max_desc` (a column count), so wide/CJK descriptions would wrap wrong even without the panic.

**Fix:** operate on `Vec<char>` (or use `char_indices()` to find a safe boundary) throughout, and add a `#[test]` that runs `key_help_lines` at `desc_width` 1..=60.

### 5. `--encryption-key '~'` panics

`src/bin/ptm_sync/main.rs:98-105`

```rust
if self.encryption_key.starts_with('~') {
    PathBuf::from(home).join(&self.encryption_key[2..])   // <-- [2..] on "~" is out of bounds
```

**Verified:** `--encryption-key '~'` → `panicked at 'start byte index 2 is out of bounds for string of length 1'`. `--encryption-key '~x'` silently resolves to `$HOME/` instead of erroring. `enc` has a correct `expand_tilde` (`src/bin/enc/main.rs:109`) that handles `~` and `~/` properly and errors when `$HOME` is unset — `ptm-sync` should just use the same function instead of its own copy.

### 6. `render_key_help` mutates state during render, and `?` doesn't work in the update viewer

`src/app.rs:1087-1090` returns into `handle_key_pd_update_viewer` **before** the universal-keys block, so `?` (key help) and `E`/`T` are dead while an update is open. Meanwhile the hint bar (`project_detail.rs:236`) advertises `?:help` in exactly that mode:

```rust
PdTab::Updates if app.pd_open_update.is_some() => {
    "  ESC:close  n/N:next/prev match  ?:help".into()
}
```

`n`/`N` are also advertised there but only work via the universal block — which the viewer bypasses for `N` (line 1137 is unreachable in viewer mode; `N` is handled inside the viewer only if present, and it isn't). Verified: the viewer handler contains **no** `n`, `N`, or `?` arms.

**Fix:** move the viewer guard below the universal-keys block, or add the missing arms to `handle_key_pd_update_viewer`.

### 7. `merge` fails outright on a legacy **local** database

`src/bin/ptm_sync/merge.rs:167` runs `ensure_schema_compat` only on the **source** copy; `merge_dbs` opens the local db with plain `open_rw` (`:341`). If the local db predates the `updates.label` / `sort_order` / `updated_at` columns — the exact situation the migrations exist for — the merge aborts.

**Verified:** against a legacy-shaped local db the first `UPDATE … SET label = s.label` raises `no such column: label`. The pre-merge snapshot is written first, so the user gets a confusing "snapshot created, then fatal error".

**Fix:** call `ensure_schema_compat(&conn)` on the local connection inside `merge_dbs` before `BEGIN IMMEDIATE`.

### 8. `merge` aborts on a foreign row with a dangling FK

`src/bin/ptm_sync/merge.rs:406-415` inserts todos with `project_id` taken from the source. `open_rw` sets `PRAGMA foreign_keys=ON`, so a source db containing a todo whose project was deleted (possible in any db that predates the `ON DELETE CASCADE` migration) aborts the whole merge.

**Verified:** `IntegrityError: FOREIGN KEY constraint failed`. Since the merge is one transaction, nothing is lost — but the user has no way forward.

**Fix:** `LEFT JOIN` against `main.projects` on insert and null out unresolvable `project_id` values (or `INSERT OR IGNORE`), and log a warning with a count.

---

## 🟡 Medium

### 9. `merge` sort_order policy is wrong for `updates` and for filtered groups

`src/bin/ptm_sync/merge.rs:435-441`

```sql
COALESCE((SELECT MAX(m.sort_order) FROM main.updates m
          WHERE m.project_id = src.updates.project_id), 0.0) + sort_order
```

`updates` are stored **newest-first** (`repo.rs:270`, `min_order - 1.0`), so local sort_orders run `0, -1, -2, …` and `MAX()` returns the *newest* row, not the last. Foreign rows therefore land **inside** the local block instead of after it.

**Verified:** local `L1(0), L2(-1), L3(-2)` + foreign `R1(0), R2(-1)` produce display order `L3, R2, L2, R1, L1` — the two histories are interleaved. The module docstring explicitly promises *"foreign-only rows slot in after it"*.

Related, same lines: the `MAX()` is computed over `main.<table>` **before** any foreign row is inserted, but `COALESCE(..., 0.0) + sort_order` assumes foreign `sort_order` starts at ~1. For `projects`/`references_table` this means a foreign-only row can sort *before* local rows when local orders are negative (verified: `L1(1.0)`, foreign `A(30), B(20), C(10)` → `11, 21, 31` is fine, but with negative local orders it isn't). Use `MIN(sort_order)` for updates, and re-space *after* computing a single offset (`max + 1 - min_foreign`).

### 10. `repo::list_projects` and `list_todos_global` are N+1

`src/data/repo.rs:115-116`

```rust
let tags = load_project_tags(conn, &id)?;
let soonest_reminder = load_soonest_reminder(conn, &id)?;
```

Two extra round-trips per project row, and `list_todos_global` (`:605-640`) calls `list_projects` and then runs **one more query per project** for its todos. `reload_project_list` / `reload_todo_list` run on every keystroke of the search box (`app.rs:877-879`), so typing a 10-character query at 30 projects is ~1000 queries. The `soonest_reminder` subquery is already computed per row in SQL in `load_soonest_reminder` — it can be inlined into the main `SELECT`; tags can come from one `LEFT JOIN` + `GROUP_CONCAT` (or one `IN (...)` query for the whole result set).

### 11. `rename_tag` swallows its own error

`src/data/repo.rs:537-545`

```rust
let collision: bool = conn.query_row(...).map(|n| n > 0).unwrap_or(false);
```

A genuine SQL failure (locked db, corrupt index) is silently converted to "no collision", and the rename proceeds. Also the comment below it is stale — it mentions `QueryReturnedNoRows` but the code uses `anyhow::bail!`.

**Fix:** `let n: i64 = conn.query_row(...)?;` and compare.

### 12. `--start` blocks on a network error even though `ptm` is supposed to launch

`src/bin/ptm_sync/main.rs:219-236` — `prompt_continue_without_pull` **aborts** when stdin isn't a TTY. In the documented wrapper (`trap … EXIT; ptm`) stdin *is* usually a TTY, but when `ptm` is launched from a desktop launcher, a script, or over a pipe, a transient S3 error hard-fails the launch. The function name promises "continue"; the non-TTY path does the opposite. Prefer continuing with a warning in the non-TTY case (the data-loss guards are already in place), and reserve abort for `--block-concurrent`.

### 13. Inconsistent truncation of reminder/date strings

`src/ui/views/mod.rs:24`, `project_list.rs:104`, `todo_list.rs:119`, `project_detail.rs:802` all do `&r[..r.len().min(10)]` — a **byte** slice. `reminder` is written by `handle_reminder_input` (validated `%Y-%m-%d`, safe) but also by `util/import_projects.py` and by `--merge` from another db; any non-ASCII value panics. **Verified:** `"2025-01-1é"[..10]` → *"not a char boundary"*. Elsewhere the codebase uses the safe `.chars().take(10)` (`app.rs:1326`, `project_detail.rs:468`). Standardise on one helper.

### 14. `render_key_help` writes to `app` during render

`overlay.rs:372` → `app.key_help_max_scroll.set(max_scroll)`, and `project_detail.rs:420/704` → `publish_match_marks(app, …)`. AGENTS.md states *"Each view function is a pure render … No state is mutated during rendering."* It's the documented architecture being violated via interior mutability. It works, but it means the key handler reads a value published by the *previous* frame, which is why `nudge_scroll_into_view` needs its `max(1)` and clamping dance. Worth either documenting as an intentional exception in AGENTS.md or moving the computation into a shared "measure" pass.

### 15. `Warn` overlay swallows any key including the one the user pressed

`app.rs:400-402` — `Overlay::Warn { .. } => { self.overlay = None; }` discards the key. That's intended ("dismissed by any key press"), but combined with `main.rs`'s edit-conflict path it means a user who hits the conflict warning and reflexively presses `e` again gets nothing. Minor UX, worth a note in the docs.

---

## 🟢 Low / polish

16. **`word_wrap`/`char_wrap` ignore display width.** They count `chars()`, so CJK/emoji titles wrap at half the intended column count and overflow. `markdown.rs` correctly uses `unicode-width`; the views don't. Same for `hint_height` (`views/mod.rs:47`) — `text.chars().count()` over-estimates for wide text and under-estimates nothing, so it errs safe, but the inconsistency is worth fixing in one place.

17. **`split_span` index mismatch on case-folding.** `lower_text` can be longer than `text_chars` (`'İ'` lowercases to two chars). The loop bound uses `tlen = text_chars.len()` (correct, avoids the panic), but the *highlighted range* is then wrong. **Verified:** searching `i` in `İstanbul` highlights `İ`, searching `İ` highlights `İs`. Low impact, but the same code is duplicated in `src/bin/md/main.rs:1118` and `src/ui/views/project_detail.rs:71` — extract one helper.

18. **`TodoStatus::cycle` ordering** (`models.rs:26`) is New → InProgress → Done → Canceled → New, so `s` three times to reach Done. The help text says "Cycle todo status" without stating the order. Fine, just document it.

19. **`data_path()` duplicates `resolve_data_dir()`** (`app.rs:241` vs `ptm_sync/main.rs:148`) and `spell.rs:224`'s `xdg_data_home()`. Three copies of the same XDG logic with slightly different `$HOME` fallbacks (`.` vs erroring). Extract to a shared module.

20. **`crypto::write_key` doesn't re-assert `0600` on overwrite.** `OpenOptions::mode()` only applies at creation. **Verified:** a `0644` key file stays `0644` after `enc keygen --force`. `check_key_permissions` warns on use, so this is not a silent hole, but `keygen --force` should `fchmod` after opening.

21. **`log.rs` and `md/log.rs` are near-duplicates** (`log.rs` vs `src/bin/md/log.rs`) — same as the `crypto.rs`/`spell.rs` situation that AGENTS.md already solved with `#[path]` includes. Do the same here.

22. **Stale comments / docs.** `repo.rs:283` — a doc comment *"Reset sort_order to 1.0, 2.0, … for all updates belonging to `project_id`"* sits directly above `swap_update_order`, which does nothing of the sort. `repo.rs:540` references a sentinel `QueryReturnedNoRows` that no longer exists. `project_detail.rs:194` doc-comments `current_marker` but sits above `current_match`. AGENTS.md §"Repository Layout" lists `src/ui/views/reference_library.rs` and describes `input/mod.rs` as a placeholder — neither file/description matches the tree (there is no `reference_library.rs`; `input/mod.rs` is fully implemented).

23. **`handle_key_project_detail`'s `N` arm is unreachable in viewer mode** (see #6) and `KeyCode::Char('n')` is guarded by `pd_open_update.is_some() || matches!(self.pd_tab, PdTab::Description)` at `app.rs:1128` — a condition that is checked *after* the `pd_open_update` early-return at `:1088`, making half of it dead.

24. **`Cargo.toml`** declares `rustls` with `ring` but `aws-lc-sys` is still pulled in (visible in the build log) — `main.rs` installs the `ring` provider manually and ignores the error. Consider `default-features = false` on the rust-s3/rustls chain to drop the second crypto backend (build time and binary size).

---

## What's genuinely good

Worth saying, because it's unusual:

- **`crypto.rs` is byte-compatible with `util/encrypt.py`** — I compared the formats line by line: `ENC1 ‖ 12-byte nonce ‖ ciphertext ‖ 16-byte tag`, fresh `Nonce::generate()` per call, no AAD on either side, key file = 32 raw bytes. Decryption authenticates *before* writing (verified: no output file on a wrong key). Nonce generation uses `getrandom`, not a counter — no reuse risk.
- **`enc`'s `confirm()` correctly treats non-TTY as "no"**, prompts on stderr so `enc export > key.b64` stays clean, and `import` handles both interactive and piped input. That's careful CLI design.
- **`merge.rs`'s safety envelope** — `VACUUM INTO` snapshot *before* any change, single `BEGIN IMMEDIATE` with explicit `ROLLBACK`, source migrated in a temp copy, `ATTACH`-based set operations rather than row-by-row Rust loops. The bugs above are in the *details*, not the design.
- **`list_window` / `list_window_reserved`** are a genuinely nice solution to a real ratatui limitation, and they're backed by 20+ unit tests including the off-by-one cases that motivated them.
- **`InputState`** is correct for multi-byte input (char-index cursor, `char_to_byte` conversion) and has 34 tests. `insert`/`delete_back`/`delete_forward`/`display` all behave at boundaries.
- **`tz.rs`** uses a `OnceLock`-style cache (24h TTL) instead of spawning `date` per render — and it *does* handle the `Mutex` poisoning case with `unwrap_or_else(|e| e.into_inner())`.

---

## Suggested order of work

1. **Finding #1** — the `--start` corruption path. This is the only one that loses user data. Fix the lock probe, check the checkpoint return value, and stop renaming over a live db.
2. **Finding #2** — `--end` uploading a WAL-less snapshot (silent backup of nothing). Same root cause as #1b; a `log_frames == 0` assertion after the checkpoint fixes both.
3. **Finding #3** — the `.db_hash` seeding undoing `--merge`. Three-line fix, high impact.
4. **Findings #4, #5** — the two panics; both are small, contained fixes with obvious tests.
5. **Findings #7, #8** — `merge` robustness on legacy/dirty databases.
6. **Finding #6** — dead keybindings the hints advertise (users will notice).
7. **Findings #9, #10** — merge ordering and the N+1; both are contained SQL changes.

The remaining items are polish and can be batched into a `cargo clippy --fix` pass plus a docs sweep.


---

_session total: 18526585 prompt tokens · 312967 completion tokens_

---

_Last turn ended with stop reason `stop`._
