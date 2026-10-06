#!/usr/bin/env python3
"""
Symmetric file encryption — AES-256-GCM.

Commands:
  keygen   Generate a new 256-bit key (saved to ~/.encryption_key by default)
  encrypt  Encrypt a file  → <file>.enc
  decrypt  Decrypt a file  → <file>  (strips .enc suffix)

Usage:
  encrypt.py keygen [--key-file PATH]
  encrypt.py encrypt <file> [--key-file PATH] [-o OUTPUT]
  encrypt.py decrypt <file.enc> [--key-file PATH] [-o OUTPUT]
"""

import argparse
import os
import secrets
import sys
from pathlib import Path

try:
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    from cryptography.exceptions import InvalidTag
except ImportError:
    sys.exit(
        "Error: 'cryptography' package not installed.\n"
        "Run:  pip install cryptography"
    )

# ── Constants ─────────────────────────────────────────────────────────────────

MAGIC = b"ENC1"      # 4-byte version header; bump on format changes
NONCE_SIZE = 12      # 96-bit nonce — recommended for AES-GCM
KEY_SIZE   = 32      # 256-bit key
TAG_SIZE   = 16      # GCM authentication tag (appended by the library)

DEFAULT_KEY_FILE = Path.home() / ".encryption_key"

# Minimum valid ciphertext length: magic + nonce + (empty plaintext → tag only)
MIN_ENC_SIZE = len(MAGIC) + NONCE_SIZE + TAG_SIZE


# ── Key management ────────────────────────────────────────────────────────────

def keygen(key_file: Path) -> None:
    """Generate a cryptographically random 256-bit key and write it to key_file."""
    if key_file.exists():
        answer = input(f"Key file '{key_file}' already exists. Overwrite? [y/N] ").strip()
        if answer.lower() != "y":
            print("Aborted.")
            return

    key = secrets.token_bytes(KEY_SIZE)
    key_file.parent.mkdir(parents=True, exist_ok=True)
    key_file.write_bytes(key)
    key_file.chmod(0o600)  # owner read/write only — keep other users out

    print(f"New key written to: {key_file}")
    print("⚠  Back this file up. Losing the key means losing access to all encrypted data.")


def load_key(key_file: Path) -> bytes:
    """Load and validate the key from key_file."""
    if not key_file.exists():
        sys.exit(
            f"Error: key file '{key_file}' not found.\n"
            f"Generate one with:  {Path(sys.argv[0]).name} keygen"
        )

    key = key_file.read_bytes()

    if len(key) != KEY_SIZE:
        sys.exit(
            f"Error: key file must be exactly {KEY_SIZE} bytes "
            f"(got {len(key)}). Has it been modified?"
        )

    return key


# ── Core encrypt / decrypt ────────────────────────────────────────────────────

def encrypt_file(src: Path, dst: Path, key: bytes) -> None:
    """
    Encrypt src → dst.

    File layout:
      [ MAGIC (4 B) | nonce (12 B) | ciphertext + GCM tag (16 B) ]

    AES-256-GCM provides both confidentiality and integrity in one pass.
    A fresh random nonce is generated for every encryption; reusing a
    nonce with the same key would be catastrophic, so we never do that.
    """
    if _same_path(src, dst):
        sys.exit("Error: source and destination are the same file.")

    nonce = secrets.token_bytes(NONCE_SIZE)
    plaintext = src.read_bytes()
    ciphertext = AESGCM(key).encrypt(nonce, plaintext, None)

    dst.write_bytes(MAGIC + nonce + ciphertext)
    print(f"Encrypted: {src}  →  {dst}")
    print(f"  {len(plaintext):,} bytes  →  {dst.stat().st_size:,} bytes")


def decrypt_file(src: Path, dst: Path, key: bytes) -> None:
    """
    Decrypt src → dst.

    Authentication is checked automatically by AES-GCM; if the file has
    been tampered with or the wrong key is used, decryption fails loudly
    before any plaintext is written.
    """
    if _same_path(src, dst):
        sys.exit("Error: source and destination are the same file.")

    data = src.read_bytes()

    if len(data) < MIN_ENC_SIZE:
        sys.exit("Error: file is too short to be a valid encrypted file.")

    if data[:len(MAGIC)] != MAGIC:
        sys.exit(
            "Error: not a recognised encrypted file "
            f"(expected header {MAGIC!r})."
        )

    nonce      = data[len(MAGIC) : len(MAGIC) + NONCE_SIZE]
    ciphertext = data[len(MAGIC) + NONCE_SIZE :]

    try:
        plaintext = AESGCM(key).decrypt(nonce, ciphertext, None)
    except InvalidTag:
        sys.exit(
            "Error: authentication failed — wrong key, or the file has been "
            "corrupted / tampered with. No output written."
        )

    dst.write_bytes(plaintext)
    print(f"Decrypted: {src}  →  {dst}")
    print(f"  {src.stat().st_size:,} bytes  →  {len(plaintext):,} bytes")


# ── Helpers ───────────────────────────────────────────────────────────────────

def _same_path(a: Path, b: Path) -> bool:
    try:
        return a.resolve() == b.resolve()
    except OSError:
        return False


def _default_enc_output(src: Path) -> Path:
    """foo.pdf → foo.pdf.enc"""
    return src.with_name(src.name + ".enc")


def _default_dec_output(src: Path) -> Path:
    """foo.pdf.enc → foo.pdf   |   foo.enc → foo   |   anything else → foo.dec"""
    if src.suffix == ".enc":
        stripped = src.with_suffix("")        # drop .enc
        if stripped.name:                     # guard against bare ".enc"
            return stripped
    return src.with_name(src.name + ".dec")


def _check_overwrite(dst: Path, force: bool) -> None:
    if dst.exists() and not force:
        answer = input(f"Output file '{dst}' already exists. Overwrite? [y/N] ").strip()
        if answer.lower() != "y":
            sys.exit("Aborted.")


# ── CLI ───────────────────────────────────────────────────────────────────────

def build_parser() -> argparse.ArgumentParser:
    # Shared optional args inherited by every subcommand via parents=[]
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument(
        "--key-file", "-k",
        type=Path,
        default=DEFAULT_KEY_FILE,
        metavar="PATH",
        help=f"path to key file  (default: {DEFAULT_KEY_FILE})",
    )
    common.add_argument(
        "--force", "-f",
        action="store_true",
        help="overwrite output file without prompting",
    )

    parser = argparse.ArgumentParser(
        description="Symmetric file encryption — AES-256-GCM",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
examples:
  %(prog)s keygen                            # generate key → ~/.encryption_key
  %(prog)s keygen --key-file ./project.key   # custom key location

  %(prog)s encrypt report.pdf                # → report.pdf.enc
  %(prog)s encrypt report.pdf -o out.enc     # custom output name
  %(prog)s encrypt archive.tar.gz            # → archive.tar.gz.enc

  %(prog)s decrypt report.pdf.enc            # → report.pdf
  %(prog)s decrypt report.pdf.enc -o /tmp/r  # custom output path
""",
    )

    sub = parser.add_subparsers(dest="command", required=True)

    # keygen ──────────────────────────────────────────────────────────────────
    sub.add_parser("keygen", parents=[common], help="generate a new AES-256 key")

    # encrypt ─────────────────────────────────────────────────────────────────
    enc = sub.add_parser("encrypt", parents=[common], help="encrypt a file  (→ <file>.enc)")
    enc.add_argument("file", type=Path, help="plaintext file to encrypt")
    enc.add_argument(
        "--output", "-o",
        type=Path, metavar="PATH",
        help="output path  (default: <file>.enc)",
    )

    # decrypt ─────────────────────────────────────────────────────────────────
    dec = sub.add_parser("decrypt", parents=[common], help="decrypt a file  (→ strips .enc suffix)")
    dec.add_argument("file", type=Path, help="encrypted file to decrypt")
    dec.add_argument(
        "--output", "-o",
        type=Path, metavar="PATH",
        help="output path  (default: <file> with .enc stripped)",
    )

    return parser


def main() -> None:
    args = build_parser().parse_args()

    if args.command == "keygen":
        keygen(args.key_file)

    elif args.command == "encrypt":
        if not args.file.exists():
            sys.exit(f"Error: '{args.file}' not found.")
        output = args.output or _default_enc_output(args.file)
        _check_overwrite(output, args.force)
        key = load_key(args.key_file)
        encrypt_file(args.file, output, key)

    elif args.command == "decrypt":
        if not args.file.exists():
            sys.exit(f"Error: '{args.file}' not found.")
        output = args.output or _default_dec_output(args.file)
        _check_overwrite(output, args.force)
        key = load_key(args.key_file)
        decrypt_file(args.file, output, key)


if __name__ == "__main__":
    main()
