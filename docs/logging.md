# Logging

The TUI runs in raw mode and owns the terminal, so **`eprintln!` and `print!`
corrupt the display and must never be used**. Use `crate::log::warn(msg)`.

```rust
crate::log::warn("something unexpected happened");
```

`src/log.rs` is shared by `ptm` and `md` (see
[shared-files.md](shared-files.md)):

| Binary | Log file |
|--------|----------|
| `ptm` | `$XDG_DATA_HOME/ptm/ptm.log` |
| `md` | `$XDG_DATA_HOME/md/md.log` — `bin/md/log.rs` is a two-line wrapper that calls `log::set_app_name("md")` |

Both create the directory on demand, so the first message of a fresh install is
not lost. Lines carry a UTC timestamp:

```
2025-01-15T10:23:45Z WARN  row_to_todo: unknown todo status: "bogus" — defaulting to New
```

Logging failures are silently swallowed — a log call must never crash the app.

```bash
tail -f ~/.local/share/ptm/ptm.log   # ptm
tail -f ~/.local/share/md/md.log     # md
```

## `log::warn` vs `Overlay::Warn`

Use the **overlay** instead of the log whenever the condition is interactive
and the user would benefit from on-screen guidance (a missing dictionary, an
unsupported operation). Use the **log** for conditions the user cannot act on,
or that happen outside a key handler. See
[ui-conventions.md](ui-conventions.md#warn--error-modal-overlaywarn).

`ptm-sync` does not use this logger — it writes `ptm-sync.log` in the same data
directory. See [ptm-sync.md](ptm-sync.md#sync-log).
