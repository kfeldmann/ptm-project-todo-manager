#!/usr/bin/env python3
"""
check-all.py — run every harness and summarise.

    python3 dev-harness/check-all.py              # all harnesses
    python3 dev-harness/check-all.py panics merge # only these
    PTM_PROFILE=release python3 dev-harness/check-all.py

Exit status is the number of failing harnesses (0 = all clean). While a
finding is unfixed its harness is expected to fail; see README.md.
"""

import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).parent

HARNESSES = [
    ("wal-safety.py", "findings 1a/1b/1c, 2"),
    ("sync-state.py", "findings 3, 12"),
    ("merge-robust.py", "findings 7, 8, 9"),
    ("panics.py", "findings 4, 5, 13"),
    ("keyhelp-widths.py", "finding 4 (diagnostic, always passes)"),
    ("search-highlight.py", "finding 17"),
    ("s3-roundtrip.py", "end-to-end S3 (skips without /etc/hosts)"),
]


def main(argv):
    wanted = [a for a in argv[1:] if not a.startswith("-")]
    selected = [
        (f, d) for f, d in HARNESSES
        if not wanted or any(w in f for w in wanted)
    ]
    if not selected:
        print(f"no harness matched {wanted}; available: {[f for f, _ in HARNESSES]}")
        return 2

    failed = []
    for script, covers in selected:
        print("=" * 72)
        print(f"{script}  —  {covers}")
        print("=" * 72)
        cp = subprocess.run([sys.executable, str(HERE / script)])
        if cp.returncode != 0:
            failed.append(script)
        print()

    print("=" * 72)
    if failed:
        print(f"{len(failed)} of {len(selected)} harnesses report failures:")
        for f in failed:
            print(f"  - {f}")
        print("\nA failure here means the corresponding finding is still live.")
    else:
        print(f"all {len(selected)} harnesses clean")
    return len(failed)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
