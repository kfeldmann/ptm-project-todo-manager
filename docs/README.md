# docs/ — agent documentation

`../AGENTS.md` is the index; it is force-fed into every session and carries
only the rules that apply everywhere. The files here are read on demand, one
area at a time.

| File | Area |
|------|------|
| `build.md` | build, test, harnesses |
| `repository-layout.md` | the source tree, directory by directory |
| `shared-files.md` | the eight `#[path]`-included files and what each owns |
| `architecture.md` | `app.rs` state, key dispatch, rendering, `PublishedMetrics`, `$EDITOR`, the event loop |
| `data-layer.md` | `repo.rs`, SQL conventions, schema changes, pagination |
| `ui-conventions.md` | theme, hint bars, text width, search highlight, tables, overlays |
| `adding-features.md` | recipes: keybinding, overlay, screen, inline-edit mode |
| `logging.md` | `log::warn`, log locations |
| `enc.md` | the `enc` CLI |
| `ptm-sync.md` | `ptm-sync` modes, flags, archive, merge semantics |
| `sync-internals.md` | WAL, locking, and the push/pull state machine |
| `enc-sync.md` | encryption format and shared key handling |

Not here: `../USER_MANUAL.md` (end users), `../planning-docs/` (design history,
code reviews, remediation plans — **history, not current rules**),
`../dev-harness/` (one runnable harness per review finding).

When you change behaviour, update the doc for that area in the same commit. If
a rule here contradicts the code, the code is right and the doc is a bug.
