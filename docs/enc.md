# enc

`enc` is a Rust replacement for `util/encrypt.py`: the same AES-256-GCM file
format, the same key file format, and the same CLI shape, plus `export` /
`import` for moving a key around as text. Files and keys are **byte-compatible
with the Python tool in both directions**, so either tool can read what the
other wrote.

## Commands

| Command | Effect |
|---------|--------|
| `enc keygen` | Generate a fresh 256-bit key → `~/.encryption_key` (mode 0600) |
| `enc encrypt <file>` | Encrypt → `<file>.enc` |
| `enc decrypt <file.enc>` | Decrypt → `<file>` (`.enc` stripped; else `.dec` appended) |
| `enc export` | Print the key as base64 on stdout |
| `enc import` | Read a base64 key from stdin, write it to the key file |

## Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--key-file` / `-k` | `~/.encryption_key` | Key file path (`~` is expanded; errors if `$HOME` is unset) |
| `--output` / `-o` | *(derived from input)* | Output path (encrypt / decrypt only) |
| `--force` / `-f` | off | Overwrite an existing output/key file without prompting |

## Behaviour notes

- **Prompts** happen on stderr so that `enc export > key.b64` stays clean.
  When stdin is not a terminal (scripts, pipes) an overwrite prompt is
  answered "no" — pass `--force` to overwrite non-interactively.
- **`enc export`** prints only the base64 key when stdout is piped. On a
  terminal it adds a header line and a warning on stderr (the key itself is
  still the only thing on stdout).
- **`enc import`** reads a single line interactively, but consumes the whole
  stream when piped, so keys wrapped with `fold` still work. Base64
  whitespace is ignored; the decoded key must be exactly 32 bytes.
- **Key permissions**: `keygen`/`import` create the key file with `0600` at
  creation time (never briefly world-readable). `encrypt`/`decrypt`/`export`
  print a warning if the key file has group/other bits set, but do not fail
  (matching `encrypt.py`); **`ptm-sync` is the strict one** and refuses.
- **No partial writes on failure**: decryption authenticates before writing,
  so a wrong key or tampered file leaves no output file behind.

## File format

```
[ MAGIC (4 B: "ENC1") | nonce (12 B) | ciphertext + GCM tag (16 B) ]
```

The key file is exactly **32 bytes of raw key material** — not base64, not hex.
The format is shared with `util/encrypt.py` and `ptm-sync`; all three use
`src/crypto.rs`.

```bash
enc keygen                      # writes ~/.encryption_key
python3 util/encrypt.py keygen  # equivalent (Python tool)
```

## Implementation

The crypto lives in `src/crypto.rs` and is path-included by `ptm-sync` and
`enc`. **Change the format there — never in a copy.** See
[shared-files.md](shared-files.md).

Path handling is shared the same way: `src/paths.rs::expand_tilde` resolves
`--key-file`. Only a leading `~` or `~/` is expanded; `~x` stays a literal
relative path, and an unset `$HOME` is an error rather than a fallback to the
current directory.
