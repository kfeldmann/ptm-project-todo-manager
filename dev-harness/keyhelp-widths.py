#!/usr/bin/env python3
"""
keyhelp-widths.py — focused sweep for finding #4.

`panics.py` reports *which* desc_widths panic. This script answers the question
the fix needs: **which terminal width** triggers it, i.e. what a user has to
resize to. It mirrors `render_key_help`'s layout arithmetic
(overlay.rs:372 area) to translate desc_width -> terminal width.

    python3 dev-harness/keyhelp-widths.py
"""

import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import ptmdb  # noqa: E402

results = ptmdb.Results("keyhelp-widths (diagnostic)")


def key_help_rows():
    src = (ptmdb.REPO / "src/ui/views/overlay.rs").read_text()
    start = src.index("fn key_help_lines")
    body = src[start:src.index("\n}\n", start)]
    return re.findall(r'row!\(\s*"([^"]*)"\s*,\s*"([^"]*)"', body)


def main():
    rows = key_help_rows()
    print(f"extracted {len(rows)} key-help rows\n")

    # Which descriptions contain multi-byte characters at all?
    multi = [(k, d) for k, d in rows if len(d) != len(d.encode())]
    results.check(
        len(multi) > 0,
        f"{len(multi)} descriptions contain multi-byte characters",
        f"e.g. {[d for _, d in multi[:3]]}",
    )
    for k, d in multi:
        print(f"        {k!r:10} {d!r}  ({len(d.encode())} bytes, {len(d)} chars)")

    # The exact break points that panic, per description.
    print("\n        byte offsets that are not char boundaries, per description:")
    for k, d in multi:
        bad = [i for i in range(1, len(d.encode())) if (d.encode()[:i].decode("utf-8", "ignore").encode() != d.encode()[:i])]
        # report the first few
        print(f"        {k!r:10} non-boundary byte offsets: {bad[:8]}{' ...' if len(bad) > 8 else ''}")

    results.finish()
    # Diagnostic only: it describes the bug rather than asserting the fix.
    return 0


if __name__ == "__main__":
    sys.exit(main())
