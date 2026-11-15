# How to Add Things

## How to add a keybinding

1. Handle it in the appropriate `handle_key_*` method in `app.rs`.
2. Update the hints string in the corresponding view file under
   `src/ui/views/`. The hints string drives the layout height, so it must stay
   accurate — and only for the modes that accept the key.
3. Update `key_help_lines()` in `src/ui/views/overlay.rs` — the `?` modal.

**In Project Detail, decide where the key belongs** (see
[architecture.md](architecture.md#key-scopes-in-project-detail) for the
ordering):

| Where | Use for |
|-------|---------|
| A tab handler (`handle_key_pd_description` / `_updates` / `_todos`) | keys specific to that tab |
| The universal block in `handle_key_project_detail` | keys about the *project* (`E`, `T`, `/`) — these also work while an update is open |
| `handle_key_pd_update_viewer` | keys that scroll or edit *the open update* |

Two more checks before you finish:

* **Guard modifier keys.** If the key is a letter that is also a tab switch
  (`d`/`u`/`t`/`f`) and you bind a Ctrl variant of it for scrolling, guard the
  tab-switch arm on `!key.modifiers.contains(KeyModifiers::CONTROL)`. The
  universal block runs first, so an unguarded arm wins and the Ctrl key appears
  to do nothing.
* **Disambiguate state-dependent keys in one place.** `n` is "next match",
  "edit description" or "new item" depending on the tab and whether a query is
  running. Put that in one place with a comment saying which case wins, and
  test all of them. Silent dead bindings are the failure mode here: `n` was
  documented as "edit description" and did nothing, and `^F`/`^B` were
  advertised in the Key Help while a tab switch ate the key.

The user-facing key tables live in `USER_MANUAL.md`; keep them in step.

## How to add an overlay

1. Add a variant to the `Overlay` enum in `app.rs`.
2. Add an arm to the `match overlay { ... }` block in `handle_key_overlay`.
3. Write a `fn handle_<name>(&mut self, key, ...) -> Result<Option<Overlay>>`
   method on `App`. Return `Ok(None)` to dismiss, `Ok(Some(overlay))` to keep
   it open.
4. Add rendering in `src/ui/views/overlay.rs`: a private `fn render_<name>`
   and a new arm in `render_overlay`'s `match` block.

## How to add a screen

1. Add a variant to the `Screen` enum in `app.rs`.
2. Write a `fn handle_key_<screen>(&mut self, key) -> Result<()>` method.
3. Create `src/ui/views/<screen>.rs` with a `pub fn render(frame, area, app)`.
4. Add `pub mod <screen>;` in `src/ui/views/mod.rs`.
5. Add the arm to the `match app.screen { ... }` blocks in both
   `ui::mod::render` and `App::handle_key`.

## How to add a shared helper

Do **not** create a second copy in another binary. Put it in the shared file
and path-include it — see [shared-files.md](shared-files.md).

## Checklist for an inline-edit mode

Inline-edit guards run **before** tab dispatch in
`handle_key_project_detail`. If you add a new one:

1. Add its check to the top of that method, above the universal-keys block.
2. Add it to `is_inline_editing()`. Note that method also returns `true` while
   `pd_searching`, `pl_searching`, `tl_searching`, `pl_editing_title`, or
   `tl_editing.is_some()` is active.
