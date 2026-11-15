# Encryption & Shared Key Handling

Backups are AES-256-GCM encrypted, and `enc` encrypts individual files. Both
use `src/crypto.rs`, path-included — see
[shared-files.md](shared-files.md). **Never re-implement the format in a
binary.**

## File format

```
[ MAGIC (4 B: "ENC1") | nonce (12 B) | ciphertext + GCM tag (16 B) ]
```

Compatible with `util/encrypt.py` and with the `enc` binary in both
directions; all three share `src/crypto.rs`. See [enc.md](enc.md) for the
`enc` CLI.

## Key file

The key file must be exactly **32 bytes of raw key material** — not base64, not
hex.

- `ptm-sync` requires permissions `600` or `400` and **refuses to run** if
  group or other bits are set.
- `enc` only warns (matching `encrypt.py`).
- `keygen` / `import` create the file with `0600` at creation time, so it is
  never briefly world-readable.

```bash
enc keygen                      # writes ~/.encryption_key
python3 util/encrypt.py keygen  # equivalent (Python tool)
```

## Path resolution

`--encryption-key` and `--from` are resolved by `paths::expand_tilde` — the
same helper `enc` uses. Only a leading `~` or `~/` is expanded; `~x` stays a
literal relative path, and an unset `$HOME` is an error rather than a fallback
to the current directory.

The strictness is deliberate and is the mirror image of `app_data_dir`, which
falls back to `./.local/share`: a TUI with nowhere to put its database is worse
than one in an odd directory, but a private key must not land in `.`. See
[shared-files.md](shared-files.md).
