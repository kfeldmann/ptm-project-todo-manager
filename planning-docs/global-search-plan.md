# Global Full-Text Search — Plan

Status: approved design, not yet implemented
Milestone owner: ptm
Feature hotkey: `F` (Shift+f), available on all three screens

## 1. UX summary

`F` opens a fresh search overlay (no state carried over from a previous
session). The user types; results stream in one line per item. `j`/`k` (and
`↓`/`↑`) move the cursor; `ESC` cancels and closes; `Enter` on the query box
moves the cursor to the first result (no-op when there are no matches);
`Enter` on a selected result closes the overlay and deep-links to the item.

```
Search:_refactor___________

p|Nimbus refactoring
t|Do we need to refactor after the recent additions?
u|Today we put a lot of time into the project...
d|# Project Nimbus...
```

## 2. Decisions (agreed 20261122-183943-f720 session)

| Question | Decision |
|---|---|
| Hotkey | `F` (Shift+f). `f` is the Files tab in PD; `/` is already the *local* search on every screen, so reusing it here would be confusingly inconsistent. |
| Search index | Rust-side lowercase substring match, same as `list_projects` — SQL `LIKE` does not case-fold for non-ASCII. No FTS5 in this milestone. |
| Sources | Projects (title), Todos (title, Inbox included), Updates (body — label **not** searched), Descriptions (first match anywhere in body). `references_table` is deprecated and excluded. |
| Matching | Single substring, whole query, case-insensitive. Multi-word AND is a possible v2. Note in `USER_MANUAL.md`. |
| Highlighting | None (no `PublishedMetrics` involvement). |
| Display | Whole title for projects/todos; first non-empty line of the raw body (heading kept) for updates/descriptions. Empty bodies never match, so "what to show" is unreachable; skip defensively. Prefix `p|` `t|` `u|` `d|`. |
| Update labels | Not searched, not displayed. |
| Ordering | Pure date order, newest first, by `updated_at` — no grouping by type. |
| Cap | 100 results. |
| Archived / done / canceled | Included in results. If the destination's `A`/`C` toggle would currently hide the item, render it grayed out; `Enter` on a grayed item opens `Overlay::Warn` explaining which toggle to use. |
| Inbox todos | Deep-link to the Global Todo List with the cursor on that todo. |
| Empty query | Empty box, zero results. |

## 3. Data layer — `repo::global_search`

One new public function; no view or `app.rs` code runs SQL.

```rust
pub enum SearchHit {
    Project  { project_id: String, title: String,
               mtime: String, archived: bool },
    Todo     { project_id: Option<String>, todo_id: String, title: String,
               mtime: String, archived: bool, status: String },
    Update   { project_id: String, update_id: String, first_line: String,
               mtime: String },
    Desc     { project_id: String, first_line: String, mtime: String },
}

pub fn global_search(conn: &Connection, query: &str) -> Result<Vec<SearchHit>>
```

Four `SELECT`s — projects, todos, updates, descriptions — each pre-sorted
`ORDER BY updated_at DESC`, each with the same `LIMIT` (the global cap). The
top-100 *overall* under one common sort key is always contained in the
top-100 of *each* source under that key, so merging the four lists, re-sorting
by `mtime DESC` and trimming to the cap is exact. Statement count is constant
(four) regardless of row count — pin it with
`dev-harness/query-count.py`-style tests next to
`list_projects_search_is_case_insensitive`.

`LIKE` cannot be used as the filter (non-ASCII case-folding, see
`repo.rs`'s comments on `list_projects`), so each statement selects the raw
text and the match is applied in Rust on `text.to_lowercase()`.

`mtime`: `updated_at` exists on all three tables. **Trap:** the `updates`
table's `updated_at` is `NOT NULL DEFAULT ''` — legacy rows sort after
everything in `DESC` order. Normalize with
`ORDER BY MAX(COALESCE(NULLIF(updated_at, ''), created_at)) DESC`
per source, and store the same normalized value in `mtime`. Keep the
normalization expression in exactly one place (a constant next to
`SOONEST_REMINDER_COL`), not duplicated four times.

`first_line`: skip leading empty lines, then take up to the first `\n`.
Keep the raw leading heading (`# Project Nimbus` is shown as-is per the
example). Truncation to display width happens in the renderer, not here.

Fresh overlay state (`query`, `hits`, `cursor`) is created on every
`F` press — nothing persists.

## 4. App layer — `Overlay::GlobalSearch`

```rust
Overlay::GlobalSearch { query: String, hits: Vec<SearchHit>, cursor: usize }
```

Follow the `TagPicker`/`ProjectPicker` overlay pattern:

- `handle_global_search(&mut self, key, ...) -> Result<Option<Overlay>>` on
  `App`, dispatched from the `match overlay { ... }` block in
  `handle_key_overlay`. `Ok(None)` dismisses.
- Chars append to `query` and re-run `repo::global_search` (constant
  statements per keystroke); `Backspace` pops and re-runs; `Esc` → `Ok(None)`;
  `j`/`k`/arrows move `cursor`.
- `Enter`:
  - no matches (or empty query) → no-op, overlay stays;
  - cursor in bounds and the hit is **openable** → close the overlay
    (`Ok(None)`) and call `self.deep_link_to_search_hit(&hit)`;
  - cursor on a **grayed-out** hit → close the overlay and push
    `Overlay::Warn { .. }` with a message naming the screen and the toggle
    that must be flipped (`A` or `C`). `Warn` closes on any key and its
    redispatch allow-list deliberately does not contain overlay openers, so
    the warning is purely informational — the user presses `F` or the toggle
    themselves. **State the message must be computed at press time** from the
    *current* toggles, not cached in the hit.

### The deep-link ordering trap

`switch_pd_tab` **closes the update viewer**, and `open_project_detail`
resets `pd_tab` to Todos and `pd_open_update` to `None`. So
`deep_link_to_search_hit` must, in order:

1. `open_project_detail(project_id)` — this is safe to call even when
   already in PD; it clears `pd_search`/`pd_searching` and all inline-edit
   state, which is exactly what we want.
2. Directly assign `self.pd_tab`, `pd_todo_cursor` / `pd_update_cursor` /
   `pd_desc_scroll`, and `pd_open_update` — **never via `switch_pd_tab`**.

This mirrors the existing TL→PD `Enter` jump (`handle_key_todo_list`), which
already does open-then-position.

| Hit kind | Destination |
|---|---|
| `Project` | PD, no tab forced (opens on Todos per `open_project_detail`) |
| `Todo` (projected) | PD, Todos tab, cursor on todo |
| `Todo` (Inbox, `project_id: None`) | Global Todo List, cursor on the todo (flat index via `tl_todo_at`-style lookup) |
| `Update` | PD, Updates tab, **and** open the update in the viewer (`pd_open_update`) with the match visible |
| `Desc` | PD, Description tab (optionally set `pd_desc_scroll` toward the match — v1 may skip; the `PublishedMetrics` machinery for match positions already exists if added later) |

### Grayed-out matrix

Grayed-outness is **evaluated at render time from live `App` state**, never
stored in `SearchHit` — toggles can change between query and `Enter`.

| Hit | Hidden when | Warn message says |
|---|---|---|
| Archived project | `!pl_show_archived` | "Archived projects are hidden. Press A on the Project List to show them." |
| Archived todo | `!pd_show_archived_todos` (PD destination) / `!tl_show_archived` (TL destination) | "This todo is archived. Press A …" |
| Done/canceled todo | `hide_done` | "Done and canceled todos are hidden. Press C …" |
| Archived project's *todos* | only by the todo's own `archived` flag — the project itself is always viewable in PD | — |

An archived project *is* openable in PD, which has no archived filter. Per
the agreed rule we gray it out and warn anyway (one consistent rule beats
two special cases).

## 5. Render layer — `views/overlay.rs`

`fn render_global_search(frame, area, app)` + an arm in `render_overlay`'s
match. A `Paragraph` for the query line, then a results `List`.

- One `Line` per hit: `p|…` / `t|…` / `u|…` / `d|…` prefix, then the text.
- Truncate in **display columns** via `ui/text.rs` — never `chars()`.
  CJK/emoji are two columns.
- Grayed-out hits use the theme's dim style; no highlight of the query text.
- Results overflow: paginate via `views::list_window(&heights, area_height,
  cursor)` — never render a `List` of raw `ListItem`s into the full area.

## 6. Keybinding wiring — three places + manual

Per `docs/adding-features.md`:

1. Handler: `handle_key_overlay` dispatch (all three screens — the overlay
   layer is screen-independent, so `F` is handled once there, not in the
   three screen handlers).
2. Hint string in the overlay's rendered hint bar (drives layout height).
3. `key_help_lines()` in `overlay.rs` — the `?` modal.
4. `USER_MANUAL.md`: global keys table gets `F`, plus a short section
   documenting single-substring semantics, `u|`/`d|` first-line display, and
   the grayed-out/warning behavior.

## 7. Work breakdown — three sessions

### Session 1 — data layer (≈ half day)

- `SearchHit` enum + `repo::global_search` (four statements, Rust matching,
  normalized `mtime`, per-source LIMIT).
- Unit tests in `repo.rs`: match in each source; Inbox todo matched; update
  label **not** matched; first-line extraction (blank leading lines, heading
  kept, empty body skipped); ordering by `mtime` DESC; cap 100 overall;
  statement count independent of row count; non-ASCII case-fold
  (e.g. `Ä` matches `ä`).
- Acceptance: `cargo test` green; query-count pinned at 4 statements.

### Session 2 — overlay and deep-linking (≈ half day)

- `Overlay::GlobalSearch`, `handle_global_search`, renderer, `F` wiring in
  the three places, hint bar, `key_help_lines()`.
- `deep_link_to_search_hit` for all five destination rows of the table
  above, including the ordering trap (update deep-link opens the viewer
  *after* the tab assignment).
- Grayed-out rendering + `Enter`-on-grayed `Overlay::Warn`.
- Acceptance: manual smoke test of every row of the deep-link and gray-out
  tables; fresh state on every `F`; `Esc` from every state.

### Session 3 — docs and harness (≈ quarter day)

- `USER_MANUAL.md` section; note the single-substring semantics.
- `docs/data-layer.md`: one paragraph on `global_search` (four statements,
  Rust matching, `mtime` normalization constant).
- End-to-end harness under `dev-harness/` (open `F`, type, verify results,
  deep-link into an update viewer, grayed-out warn path).
- Optional stretch: `pd_desc_scroll` aimed at the matched description line.

## 8. Explicitly out of scope

- FTS5 / tokenizer-based indexing (revisit if the Rust-side scan gets slow).
- `references_table` search; its eventual removal is a separate milestone.
- Multi-word AND queries; search-string highlighting; label search for
  updates; retained overlay state across sessions.
