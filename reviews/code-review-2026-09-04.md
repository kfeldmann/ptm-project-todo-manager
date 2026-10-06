## Code Review — `ptm`

**Build & lint:** clean build, 6 clippy warnings (all minor, all auto-fixable). No test suite.

---

### 🔴 High Priority

**1. N+1 queries in `repo::list_projects` and `repo::list_todos_global`**

`list_projects` fires two extra queries per project row (tags + `soonest_reminder`). `list_todos_global` calls `list_projects` (itself N+1) and then runs one more query per project for its todos — so at 20 projects you're doing 60+ round-trips per load. These hot paths run on every navigation.

*`repo.rs` — `list_projects` and `list_todos_global`*

Fix: pull tags in a single query with a `GROUP_CONCAT`, and embed `soonest_reminder` as a subquery in the main SELECT:
```sql
SELECT p.*, 
  (SELECT MIN(t.reminder) FROM todos t
   WHERE t.project_id = p.id
     AND t.reminder IS NOT NULL
     AND t.status NOT IN ('done','canceled')) AS soonest_reminder,
  GROUP_CONCAT(tg.name) AS tag_names
FROM projects p
LEFT JOIN project_tags pt ON pt.project_id = p.id
LEFT JOIN tags tg ON tg.id = pt.tag_id
...
GROUP BY p.id
```

**2. Reorder swaps are not wrapped in a transaction**

`reorder_project`, `reorder_todo`, and `reorder_update` all perform two separate `UPDATE` statements. If the process is interrupted between them the `sort_order` values will be inconsistent.

*`repo.rs` — all three `reorder_*` functions*

```rust
conn.execute_batch("BEGIN")?;
conn.execute("UPDATE ... SET sort_order = ?1 WHERE id = ?2", ...)?;
conn.execute("UPDATE ... SET sort_order = ?1 WHERE id = ?2", ...)?;
conn.execute_batch("COMMIT")?;
```

**3. `QuickCapture` and `ReminderInput` overlays bypass `InputState`**

Both overlays manage `input: String` directly with `push`/`pop`, while every other inline-edit mode uses the `InputState` abstraction. The result is that users can't move the cursor or use Ctrl+A/E in quick-capture or reminder dialogs.

*`app.rs` — `handle_quick_capture`, `handle_reminder_input`*  
*`overlay.rs` — `render_quick_capture`, `render_reminder_input`*

Replace the raw `String` fields with `InputState` and route keys through `handle_input_key`.

**4. Reminder input accepts arbitrary strings — no validation**

`handle_reminder_input` saves whatever the user types. `reminder_date_style` degrades to `DarkGray` on parse failure, so there's no visible error — the value is silently ignored in display but stored. Add a `NaiveDate::parse_from_str` check before saving and show an inline error if the format is wrong.

---

### 🟡 Medium Priority

**5. `TZ_OFFSET` is read from the environment on every render pass**

`reminder_date_style` (in `views/mod.rs`) calls `std::env::var("TZ_OFFSET")` for every todo item drawn, and `handle_key_pd_updates` (in `app.rs`) reads it again when a new update is created. This is two different code sites, and the former runs inside render loops.

Read it once at startup into `App` and pass it through, or use a `OnceLock`:
```rust
static TZ_OFFSET: OnceLock<i64> = OnceLock::new();
fn tz_offset() -> i64 {
    *TZ_OFFSET.get_or_init(|| std::env::var("TZ_OFFSET")
        .ok().and_then(|s| s.parse().ok()).unwrap_or(0))
}
```

**6. Migration relies on error-message string matching**

```rust
Err(e) if e.to_string().contains("duplicate column name") => {}
```
This is fragile — the SQLite error message text could differ across versions or locales. SQLite 3.37+ (released Dec 2021) supports `ALTER TABLE … ADD COLUMN IF NOT EXISTS`. Since `rusqlite` bundles a recent SQLite, that's available:
```sql
ALTER TABLE updates ADD COLUMN IF NOT EXISTS label TEXT NOT NULL DEFAULT '';
```
Remove the manual error-message pattern matching.

**7. `TodoStatus` doesn't implement `std::str::FromStr`**

`TodoStatus::from_str` and `from_str_as_str` silently default to `New` on unrecognised input. Implementing the standard `FromStr` trait:
- makes the intent explicit (`"done".parse::<TodoStatus>()`),
- lets you return an `Err` for truly unrecognised values (and log/warn from `repo.rs` row-mapping), and
- is discoverable by other Rust developers.

**8. `#[allow(dead_code)]` is too broad**

`models.rs` and `repo.rs` both have a file-level `#![allow(dead_code)]`. This suppresses warnings for genuinely dead code (e.g., `Tag::color`, the entire `references_table` schema, `reference_tags`). Target it more narrowly with `#[allow(dead_code)]` on individual items, or remove it and address each warning.

**9. The `update_update` function does not update any timestamp**

The `updates` table has no `updated_at` column, so edits are invisible in the data layer. Consider either:
- adding `updated_at` to the `updates` table (ALTER TABLE migration), or
- documenting the conscious omission.

---

### 🟢 Low Priority / Polish

**10. Six auto-fixable Clippy warnings**

Running `cargo clippy --fix` resolves five of them automatically:
- Three `implicit_saturating_sub` (`cursor -= 1` guards → `.saturating_sub(1)`)
- One `needless_borrowed_reference` in `handle_project_picker`
- One `manual_div_ceil` in `hint_height`
- One `too_many_arguments` on `handle_tag_picker` (not auto-fixable — bundle the args into a struct or use the `Overlay` variant directly)

**11. No tests**

There are zero unit tests. Several pure functions are readily testable and have subtle edge cases:
- `InputState`: cursor bounds on insert/delete/move at string boundaries with multi-byte chars
- `parse_update_template`: no separator, empty body, prefix-less first line
- `word_wrap` / `char_wrap`: empty input, single overlong word, multi-space input (currently `split(' ')` on `"foo  bar"` drops the extra space)
- `hint_height`: width=0 edge case
- `build_picker_items`: inbox visibility logic, search filtering
- `TodoStatus::from_str` / `as_str` round-trip

**12. `word_wrap` collapses multiple consecutive spaces**

`split(' ')` on `"foo  bar"` produces `["foo", "", "bar"]`. The empty word is inserted as `current = ""`, so the two spaces collapse to one. Use `split_whitespace` if normalisation is desired, or `splitn`-style splitting that preserves spacing.

**13. `open_project_detail` always resets the tab to `Todos`**

When navigating from a Todo List `Enter` into a project it always lands on the Todos tab, which is correct. But navigating `ESC` back to the project list and then back into the same project also resets the tab. Saving and restoring the previous tab per-project-id would improve the feel for power users.

**14. `screen.clone()` / `pd_tab.clone()` in key handlers**

Both `Screen` and `PdTab` are small enums — cloning them is a borrow-checker workaround. A cleaner alternative is to copy the discriminant first:
```rust
let screen = std::mem::replace(&mut self.screen, self.screen.clone());
```
Or, restructure the match so that mutable borrows happen after the immutable read (which is usually possible with a small refactor). This is minor but keeps the ownership model clear.

**15. `inner_of` in `overlay.rs` is a trivial forwarding wrapper**

```rust
fn inner_of(block: &Block, area: Rect) -> Rect { block.inner(area) }
```
This adds no abstraction. Call `block.inner(area)` directly.

**16. `AGENTS.md` describes `input/mod.rs` as a "placeholder"**

The module is fully implemented (`InputState`, `handle_input_key`). The comment in `AGENTS.md` should be updated.

---

### Summary

| Severity | Count | Category |
|----------|-------|----------|
| 🔴 High | 4 | N+1 queries, missing transactions, InputState inconsistency, no date validation |
| 🟡 Medium | 5 | Perf/DRY (TZ_OFFSET), migration fragility, missing FromStr, broad dead_code, missing updated_at |
| 🟢 Low | 6 | Clippy cleanup, no tests, word_wrap edge case, UX nits, trivial helper |

The most impactful changes to make first are **#1** (N+1 queries — straightforward SQL consolidation), **#2** (add transactions to reorders — 3 lines each), and **#3** (use `InputState` in quick-capture and reminder overlays — directly improves user experience).
