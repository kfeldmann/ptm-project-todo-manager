# Architecture

## State (`app.rs`)

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

The Project Detail tab state is `pd_tab: PdTab` (`Description` | `Updates` |
`Todos` | `Files`). **There is no `pd_focus` field.**

## Key handling flow

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

### Key scopes in Project Detail

This is the one ordering that has been wrong before, so it is stated once here
and nowhere else. Guards run top to bottom; the first match wins.

```
handle_key_project_detail
  ├─ pd_editing_todo.is_some()   — intercepts all keys
  ├─ pd_adding_todo.is_some()    — intercepts all keys
  ├─ pd_editing_title.is_some()  — intercepts all keys
  ├─ pd_searching                — intercepts all keys (so `n` types into the query)
  ├─ UNIVERSAL keys              — Esc  E  T  /  n  N  d  u  t  f
  ├─ pd_open_update.is_some()    — handle_key_pd_update_viewer
  └─ pd_tab dispatch
       ├─ Description  →  handle_key_pd_description
       ├─ Updates      →  handle_key_pd_updates
       ├─ Todos        →  handle_key_pd_todos
       └─ Files        →  (no-op)
```

**The update viewer is not a separate key scope.** When `pd_open_update.is_some()`
the viewer handler receives everything the universal block did not claim — so if
a key should work in both the Updates tab and an open update, add it to **both**
`handle_key_pd_updates` and `handle_key_pd_update_viewer`.

Three rules follow, each of which was a live bug:

* **The universal block runs before the viewer guard.** It used to run after,
  which made every key in it dead while an update was open — `n`/`N` fell
  through to nothing, `E` and `T` were swallowed, and the `N` arm was
  unreachable. The viewer's hint bar advertises `n`/`N`, so the ordering was
  simply wrong.
* **Guard modifier keys.** `^F`/`^B` are half-page scroll in the Description
  tab and the viewer, but `f`/`b` alone also mean "switch to Files". The
  tab-switch arms therefore carry
  `if !key.modifiers.contains(KeyModifiers::CONTROL)`.
* **Closing the viewer is not "back".** `ESC` closes the viewer when one is
  open and otherwise leaves Project Detail. Switching to a tab other than
  Updates also closes it — see `App::switch_pd_tab` / `close_update_viewer`.

Adding a key: see [adding-features.md](adding-features.md#how-to-add-a-keybinding).

## Rendering flow

```
ui::render(frame, app)
  ├─ views::project_list::render   (or project_detail or todo_list)
  └─ views::overlay::render_overlay  (if overlay.is_some())
```

Each view function reads from `app` and writes to `frame`. Application state is
not mutated during rendering — with **one** deliberate, documented exception.

### The exception: `PublishedMetrics`

Key handling needs two things only the renderer can know:

* **Geometry** — how many lines the content has, how tall the viewport is, and
  therefore the largest legal scroll offset. That depends on the terminal
  width (markdown re-wrapping, hint-bar height) and on the tab layout.
* **Search-match positions** — where each match landed in the *wrapped*
  output, which again depends on the width.

Recomputing either in the key handler would duplicate the layout code and let
the two drift. So the views publish them into `app.metrics`
(`PublishedMetrics`, in `app.rs`) while they draw, and the key handlers read
them. It is `Cell`/`RefCell` because views take `&App` and `ui::render` only
has a shared reference.

The contract (also on the type itself):

* **Renderers own it.** Nothing else writes those fields; a key handler never
  sets them.
* **Values are one frame old.** A key event is handled after the frame that
  preceded it. Handlers must treat every value as a hint — clamp against it,
  never assume it is non-zero. `nudge_scroll_into_view`'s `.max(1)` and the
  `.min(max_scroll)` clamps are that rule, not defensive noise.
* **Losing it is harmless.** A zeroed value must degrade to "no scrolling",
  never to a panic or a stuck cursor.

Adding a field here is the right move only for a value the renderer genuinely
computes. Anything derivable from `App` (a list length, a cursor) belongs in
the key handler.

## Overlay dismissal and redispatch

`Overlay::Warn` closes on **any** key press, but the key is not swallowed: once
the notice is gone, a *safe* key is handed back to `App::handle_key` so a user
who reflexively presses `e` again at the edit-conflict warning gets the editor
instead of nothing.

Redispatch is deliberately narrow — `is_redispatchable` is a closed allow-list
of plain navigation/editing characters plus `Esc`. Keys that open an overlay
(`i`, `?`, `n`, `T`, `D`, `/`, `r`, `m`, `S`, `Enter`) are excluded: replaying
one would raise a new overlay while the caller is still unwinding from the old
one, and the nesting would have no bound.

## External editor (`$EDITOR`)

When code sets `app.pending_editor = Some(...)`, the main event loop in
`main.rs` detects it **before** polling for key events, then:

1. Suspends ratatui / restores the normal terminal.
2. Writes content to a `$TMPDIR/ptm_<uuid>.txt` file.
3. Spawns `$EDITOR` (falls back to `vi`) and waits for it to exit.
4. Reads the file back and calls `app.handle_editor_result(target, content)`.
5. Resumes ratatui.

**Do not open an editor from inside a `handle_key_*` method**; always set
`pending_editor` and return.

## Idle cost — draw on demand, never on a timer

Both event loops draw **only when something changed**: a `needs_draw` flag is
set for the first frame, after an `$EDITOR` round-trip, and after any event is
handled; otherwise the loop blocks in `event::poll(Duration::MAX)`.

`ptm`'s loop keeps a 200 ms tick that `md` does not, because its render output
is time-dependent: reminder colours are keyed on `today`
(`reminder_date_style` calls `SystemTime::now()`), so an idle redraw is what
re-colours a reminder that goes overdue at midnight. **If a renderer is ever
made clock-independent, drop its tick too; if a clock enters `md`'s render, it
needs one back.**

Two rules keep the flag honest:

* **Anything that mutates `App` outside the key handlers must set
  `needs_draw`.** That is why the `$EDITOR` branch sets it before its
  `continue`: without it the reloaded document would not be painted until the
  next keystroke.
* **Drain the event queue before drawing.** A paste arrives as a burst of
  bytes, and one draw per character costs a full markdown render each. The
  inner `poll(Duration::ZERO)` loop coalesces a burst into one draw; between
  keystrokes it is false, so ordinary typing still paints per key. The loop
  breaks out early on `should_quit` / `pending_editor` so the editor and the
  exit path never wait behind a queue.
