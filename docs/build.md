# Build & Run

```bash
cargo build            # debug build  → target/debug/{ptm,ptm-sync,md,enc}
cargo build --release  # release build → target/release/{ptm,ptm-sync,md,enc}
cargo run              # build + run debug binary
./build/build-*        # for the user only. Build via Docker
./build/run            # for the user only. Run the build container interactively
```

There are some warnings when building `md` and `enc` because they reuse code
from `ptm` so some variables are unused. Ignore these.

No configuration is required on first launch. The database is created
automatically at `~/.local/share/ptm/ptm.db` (or `$XDG_DATA_HOME/ptm/ptm.db`).

## Tests and harnesses

```bash
cargo test                          # unit tests (531 `#[test]` fns in src/)
python3 dev-harness/check-all.py    # every end-to-end harness
python3 dev-harness/check-all.py panics merge   # just these
```

`dev-harness/check-all.py` exits with the number of failing harnesses. Most
harnesses map to findings from `planning-docs/code-reviews/`; a few cover
later features (`global-search.py` exercises the `F` global search). See
`dev-harness/README.md`. `query-count.py` in particular counts SQL statements
via `SQLITE_TRACE_STMT` and pins the list-query contract (see
[data-layer.md](data-layer.md)).

Test-only features live in `[dev-dependencies]` (rusqlite's `trace` and
`hooks`): dev-dependency features are unified into test builds only, so the
shipped binaries do not carry them.
