#!/usr/bin/env python3
"""
panics.py — reproductions for findings #4, #5, #13.

  4   `wrap_key_row` slices a `&str` on a byte bound (`&d[pos..pos + cut]`), so
      the Key Help overlay panics when the break lands inside a multi-byte char.
  5   `--encryption-key '~'` panics: `&self.encryption_key[2..]` on a 1-byte str.
  13  reminder/date strings are truncated with `&r[..r.len().min(10)]`, a byte
      slice, so a non-ASCII reminder panics at render time.

For #4 this script extracts the real `row!(...)` descriptions out of
`src/ui/views/overlay.rs`, so the sweep cannot drift from the source.

    python3 dev-harness/panics.py
"""

import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import ptmdb  # noqa: E402

PROFILE = os.environ.get("PTM_PROFILE", "debug")
results = ptmdb.Results("panics")


# ── #4: key-help wrapping at narrow widths ───────────────────────────────────

def key_help_descriptions():
    """Extract every (key, description) pair from key_help_lines()."""
    src = (ptmdb.REPO / "src/ui/views/overlay.rs").read_text()
    start = src.index("fn key_help_lines")
    body = src[start:src.index("\n}\n", start)]
    return re.findall(r'row!\(\s*"([^"]*)"\s*,\s*"([^"]*)"', body)


def rust_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def check_key_help_wrap(root):
    print("\n[4] wrap_key_row panics when the break falls inside a multi-byte char")
    rows = key_help_descriptions()
    results.check(len(rows) > 20, f"extracted {len(rows)} key-help rows from overlay.rs")
    if len(rows) < 20:
        return

    # A faithful copy of wrap_key_row's slicing logic, run over the real
    # descriptions at every desc_width in 1..=60.
    prog = [r'''
fn wrap_key_row(k: &str, d: &'static str, max_desc: usize) -> Vec<String> {
    if max_desc == 0 || d.len() <= max_desc { return vec![format!("  {:12}{}", k, d)]; }
    let mut out = Vec::new(); let mut pos = 0usize; let mut first = true;
    while pos < d.len() {
        let remaining = &d[pos..];
        if remaining.len() <= max_desc {
            out.push(format!("  {:12}{}", if first { k } else { "" }, remaining));
            break;
        }
        let cut = remaining[..=max_desc].rfind(' ').unwrap_or(max_desc);
        out.push(format!("  {:12}{}", if first { k } else { "" }, &d[pos..pos + cut]));
        first = false;
        pos += cut + 1;
    }
    out
}
fn main() {
    let descs: Vec<(&'static str, &'static str)> = vec![''']
    prog += [f"        ({rust_str(k)}, {rust_str(d)}),\n" for k, d in rows]
    prog.append(r'''    ];
    std::panic::set_hook(Box::new(|_| {}));
    let mut bad = Vec::new();
    for w in 1..=60usize {
        for (k, d) in &descs {
            if std::panic::catch_unwind(|| wrap_key_row(k, d, w)).is_err() {
                bad.push(w);
                break;
            }
        }
    }
    bad.dedup();
    println!("{:?}", bad);
}''')

    src = Path(root) / "kh.rs"
    src.write_text("".join(prog))
    exe = Path(root) / "kh"
    cp = subprocess.run(
        ["rustc", "-O", "-o", str(exe), str(src)], capture_output=True, text=True
    )
    if cp.returncode != 0:
        results.check(False, "harness compiled", cp.stderr[-500:])
        return
    out = subprocess.run([str(exe)], capture_output=True, text=True).stdout.strip()
    panicking = [int(w) for w in re.findall(r"\d+", out)]

    results.check(
        not panicking,
        "no desc_width in 1..=60 panics  "
        f"[FAIL = finding #4 is live: panics at desc_width {panicking}]",
        f"panicking widths: {panicking}",
    )
    # The widths that matter in practice: a terminal narrow enough that the
    # description column collapses. Reported so the fix's test can pin them.
    print(f"        (observed panicking desc_widths: {panicking})")


# ── #5: --encryption-key '~' ────────────────────────────────────────────────

def check_tilde_key_path(root):
    print("\n[5] --encryption-key '~' and '~x'")
    dd = ptmdb.data_dir(root)
    c = ptmdb.new_db(dd / "ptm.db")
    c.execute(
        "INSERT INTO projects VALUES('p1','P','',1.0,0,'2025-01-01T00:00:00Z','2025-01-01T00:00:00Z')"
    )
    c.close()

    for arg, label in [("~", "bare '~'"), ("~x", "'~x' (no separator)")]:
        cp = ptmdb.run(
            [ptmdb.binary("ptm-sync", PROFILE), "--end",
             "--bucket-name", "b", "--prefix", "p", "--encryption-key", arg],
            env={"XDG_DATA_HOME": str(root)},
        )
        panicked = "panicked at" in cp.stderr
        results.check(
            not panicked,
            f"--encryption-key {label} does not panic  "
            "[FAIL = finding #5 is live]",
            cp.stderr.strip()[-300:],
        )
        # '~x' must be rejected, not silently resolved to $HOME.
        if arg == "~x":
            resolved_to_home = "Encryption key '/home/" in cp.stderr or "Encryption key '/root/" in cp.stderr
            results.check(
                not resolved_to_home,
                "--encryption-key '~x' is rejected instead of resolving to $HOME  "
                "[FAIL = finding #5 is live]",
                cp.stderr.strip()[-300:],
            )


# ── #13: non-ASCII date truncation ──────────────────────────────────────────

def check_non_ascii_truncation(root):
    print("\n[13] &s[..len().min(10)] panics on a non-ASCII reminder")
    src = Path(root) / "slice.rs"
    src.write_text(r'''
fn main() {
    // Exactly the expression used in views/mod.rs:24, project_list.rs:104,
    // todo_list.rs:119 and project_detail.rs:802.
    for s in ["2025-01-1é", "2025-01-01", "→2025-01-01"] {
        let n = s.len().min(10);
        let ok = std::panic::catch_unwind(|| s[..n].to_string()).is_ok();
        println!("{} {}", if ok { "ok  " } else { "PANIC" }, s);
    }
}''')
    exe = Path(root) / "slice"
    cp = subprocess.run(["rustc", "-O", "-o", str(exe), str(src)], capture_output=True, text=True)
    if cp.returncode != 0:
        results.check(False, "harness compiled", cp.stderr[-500:])
        return
    out = subprocess.run([str(exe)], capture_output=True, text=True)
    results.check(
        "PANIC" not in out.stdout,
        "byte-slicing a non-ASCII date at 10 bytes is safe  "
        "[FAIL = finding #13 is live: reminder strings from --merge or "
        "import_projects.py can panic the UI]",
        out.stdout.strip(),
    )


def main():
    with ptmdb.TempWorkspace("ptm-panics-") as root:
        print(f"working in {root}")
        print(f"binary profile: {PROFILE}")
        check_key_help_wrap(root)
        check_tilde_key_path(root)
        check_non_ascii_truncation(root)
        return results.finish()


if __name__ == "__main__":
    sys.exit(main())
