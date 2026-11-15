# Shared Files (`#[path]`-included, never copied)

Seven files at `src/` are compiled into more than one binary. They are included
with `#[path = "…"] mod x;`, so **there is no copy under `src/bin/`** — a
`mod` declaration points back at the shared file. Editing what looks like a
copy is always wrong.

| File | Compiled into |
|------|---------------|
| `src/paths.rs` | all four binaries |
| `src/log.rs` | `ptm`, `md` |
| `src/crypto.rs` | `ptm-sync`, `enc` |
| `src/spell.rs` | `ptm`, `md` |
| `src/ui/theme.rs` | `ptm`, `md` |
| `src/ui/text.rs` | `ptm`, `md` |
| `src/ui/search.rs` | `ptm`, `md` |
| `src/ui/markdown.rs` | `ptm`, `md` |

`src/bin/<x>/` therefore contains only what is genuinely specific to that
binary: its `main.rs`, plus `md`'s two-line `log.rs` wrapper and its `ui.rs`
module tree.

**When two binaries need a helper, put it in the shared file and path-include
it.** A per-binary reimplementation is how the duplicate `~` expansion, the
four disagreeing XDG lookups, and the two drifted search-highlighters happened
(see `planning-docs/code-reviews/`).

## What each shared file owns

**`paths.rs`** — `expand_tilde` and the XDG data-directory lookup
(`data_home`, `app_data_dir`, `optional_app_data_dir`, `app_data_file`). The
two are
deliberately opposite in strictness: `app_data_dir` falls back to
`./.local/share` when neither variable is set (a TUI with nowhere to put its
database is worse than one in an odd directory), while `expand_tilde` errors
when `$HOME` is unset (a private key must not land in `.`). Only a leading `~`
or `~/` is expanded; `~x` stays a literal relative path.

**`crypto.rs`** — `load_key`, `generate_key`, `write_key`,
`check_key_permissions`, `encode_key_base64`, `decode_key_base64`, `encrypt`,
`decrypt`. The wire format is shared with `util/encrypt.py` — see
[enc.md](enc.md#file-format).

**`ui/text.rs`** — every width decision. See
[ui-conventions.md](ui-conventions.md#text-width--always-measure-in-columns).

**`ui/search.rs`** — match highlighting over rendered lines. See
[ui-conventions.md](ui-conventions.md#search-highlighting--uisearchrs).

**`log.rs`** — see [logging.md](logging.md).

**`spell.rs`** — the dictionary and checker behind `S`.

**`ui/theme.rs`** — the `Theme` palette; always used as `Theme::default()`.
