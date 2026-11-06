#!/usr/bin/env python3
"""
panics.py — reproductions for findings #4, #5 and #13.

  4   `wrap_key_row` sliced a `&str` on a byte bound (`&d[pos..pos + cut]`
      where `cut` is a *column* budget), so the Key Help overlay panicked
      whenever a break landed inside a multi-byte character — and looped
      forever at `desc_width == 0`.
  5   `--encryption-key '~'` panicked: `&self.encryption_key[2..]` on a 1-byte
      string.  `'~x'` silently resolved to `$HOME/`.
  13  reminder/date strings were truncated with `&r[..r.len().min(10)]`, a byte
      slice, so a non-ASCII reminder panicked at render time.  (`reminder` is
      a free-text TEXT column: `--merge` copies whatever another database
      holds, so the value is not guaranteed to be ASCII.  `import_projects.py`
      *does* validate `%Y-%m-%d` — the review says otherwise.)

FAIL means the finding is live.

## How #4 and #13 are checked, and why it is shaped this way

The first version of this script tested **hand-written copies** of the buggy
code (`fn wrap_key_row` copied into a scratch file, `s[..n]` re-typed inline).
That cannot distinguish a fixed tree from an unfixed one: the copy keeps
panicking after the source is repaired, so the check fails forever.  It was
rewritten to extract the **real functions out of `src/`** and compile those.
The extraction is textual (slice the source between two markers), so it tracks
the fix, and the check now genuinely reports "the code in `src/` is safe"
rather than "a copy of the code from 2026-11-01 is unsafe".

    python3 dev-harness/panics.py
"""

import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import ptmdb  # noqa: E402

PROFILE = os.environ.get("PTM_PROFILE", "debug")
results = ptmdb.Results("panics")

OVERLAY = ptmdb.REPO / "src/ui/views/overlay.rs"
TEXT_SRC = ptmdb.REPO / "src/ui/text.rs"
VIEWS_MOD = ptmdb.REPO / "src/ui/views/mod.rs"


# ── source extraction ────────────────────────────────────────────────────────

def slice_between(path, start_marker, end_marker):
    """Return the text of `path` from `start_marker` up to `end_marker`."""
    src = path.read_text()
    i = src.index(start_marker)
    j = src.index(end_marker, i)
    return src[i:j]


def overlay_consts():
    """Any `const KEY_COL_WIDTH: usize = N;` line, needed by the fixed code."""
    src = OVERLAY.read_text()
    m = re.search(r"^const KEY_COL_WIDTH: usize = \d+;", src, re.M)
    return (m.group(0) + "\n\n") if m else ""


def views_consts():
    """The `DATE_LEN` const the fixed helper uses (absent in the unfixed tree)."""
    m = re.search(r"^pub const DATE_LEN: usize = \d+;", VIEWS_MOD.read_text(), re.M)
    return (m.group(0) + "\n\n") if m else ""


def key_help_descriptions():
    """Extract every (key, description) pair from key_help_lines()."""
    src = OVERLAY.read_text()
    start = src.index("fn key_help_lines")
    body = src[start:src.index("\n}\n", start)]
    return re.findall(r'row!\(\s*"([^"]*)"\s*,\s*"([^"]*)"', body)


# A minimal stand-in for the slice of ratatui that the extracted functions
# touch.  It has to be kept in step with the imports in overlay.rs; if a new
# API appears there, rustc will say so and this preamble is the thing to
# extend.  It does not change the wrapping logic in any way.
STUBS = r'''
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Style;
impl Style {
    pub fn fg(self, _: Color) -> Style { self }
    pub fn bg(self, _: Color) -> Style { self }
    pub fn add_modifier(self, _: Modifier) -> Style { self }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Color { Reset, Black, Red, Green, Yellow, Blue, Magenta, Cyan, Gray, DarkGray, White }
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Modifier { BOLD }
#[derive(Clone, Debug, Default)]
pub struct Span<'a> { pub content: std::borrow::Cow<'a, str>, pub style: Style }
#[derive(Clone, Debug, Default)]
pub struct Line<'a> { pub spans: Vec<Span<'a>>, pub style: Style }
impl<'a> Line<'a> {
    pub fn from(spans: Vec<Span<'a>>) -> Line<'a> { Line { spans, style: Style } }
    pub fn raw<S: Into<std::borrow::Cow<'a, str>>>(s: S) -> Line<'a> {
        Line { spans: vec![Span { content: s.into(), style: Style }], style: Style }
    }
}
impl<'a> Span<'a> {
    pub fn styled<S: Into<std::borrow::Cow<'a, str>>>(s: S, style: Style) -> Span<'a> {
        Span { content: s.into(), style }
    }
}
'''


# The extracted code refers to the shared helpers as `crate::ui::text::…`, so
# the probe crate has to provide that path.  `src/ui/text.rs` is path-included
# unchanged — the same file the binaries compile.
CRATE_PRELUDE = '''
pub mod ui {
    pub mod text {
        #[path = "TEXT_SRC_PLACEHOLDER"]
        mod text_impl;
        pub use text_impl::*;
    }
}
'''


def compile_and_run(root, name, body, call):
    """Build `STUBS + body + main` in a scratch cargo crate and run it.

    A scratch crate rather than a bare `rustc` call: the extracted code calls
    into `crate::ui::text`, which needs `unicode-width`, and letting cargo
    resolve that keeps the probe on the same compiler and dependency build as
    the binaries.  `CARGO_TARGET_DIR` points at the workspace's own `target/`
    so nothing is rebuilt.

    Returns (ok, stdout, stderr).
    """
    crate = root / f"probe_{name}"
    (crate / "src").mkdir(parents=True, exist_ok=True)
    prelude = CRATE_PRELUDE.replace("TEXT_SRC_PLACEHOLDER", str(TEXT_SRC))
    (crate / "src/main.rs").write_text(
        f"{prelude}\n{STUBS}\n{body}\nfn main() {{\n{call}\n}}\n"
    )
    (crate / "Cargo.toml").write_text(
        f'''[package]
name = "probe_{name}"
version = "0.0.0"
edition = "2021"

[dependencies]
unicode-width = "0.2"

[[bin]]
name = "probe_{name}"
path = "src/main.rs"
'''
    )
    env = dict(os.environ, CARGO_TARGET_DIR=str(ptmdb.REPO / "target"))
    cp = subprocess.run(
        ["cargo", "run", "--quiet", "--manifest-path", str(crate / "Cargo.toml")],
        capture_output=True, text=True, env=env, timeout=600,
    )
    if cp.returncode != 0:
        return False, cp.stdout, cp.stderr[-1500:]
    return True, cp.stdout, cp.stderr


# ── #4: key-help wrapping at narrow widths ───────────────────────────────────

def check_key_help_wrap(root):
    print("\n[4] wrap_key_row must not panic or hang at any desc_width")
    rows = key_help_descriptions()
    results.check(len(rows) > 20, f"extracted {len(rows)} key-help rows from overlay.rs")
    if len(rows) < 20:
        return

    # The real wrapping code, lifted out of overlay.rs between its section
    # markers — not a copy typed by hand.  The region is delimited by the two
    # function definitions so the extraction works on the fixed *and* the
    # unfixed tree (the fix added `KEY_COL_WIDTH`, the bug had no such const).
    body = slice_between(OVERLAY, "fn wrap_key_row", "fn key_help_lines")
    body_all = slice_between(OVERLAY, "fn wrap_key_row", "// ─── Reminder Input")
    consts = overlay_consts()

    # Sweep the *real* strings at every desc_width in 1..=60.  A panic or an
    # empty result is a failure; `desc_width == 0` is included because the old
    # loop never terminated there (the sweep would hang, so the run below is
    # given a timeout).
    call = r'''
    let descs: Vec<(&'static str, &'static str)> = vec![''' + "".join(
        '        (' + rust_str(k) + ', ' + rust_str(d) + '),\n' for k, d in rows
    ) + r'''    ];
    std::panic::set_hook(Box::new(|_| {}));
    let mut bad = Vec::new();
    for w in 0..=60usize {
        for (k, d) in &descs {
            let n = std::panic::catch_unwind(|| {
                wrap_key_row(k, d, w, Style::default(), Style::default()).len()
            });
            match n {
                Ok(0) | Err(_) => { bad.push(w); break; }
                Ok(_) => {}
            }
        }
    }
    bad.dedup();
    println!("{:?}", bad);'''

    ok, stdout, stderr = compile_and_run(root, "kh", consts + body, call)
    if not ok:
        results.check(False, "harness compiled against the real overlay.rs", stderr)
        return

    bad = [int(w) for w in re.findall(r"\d+", stdout)]
    results.check(
        not bad,
        "no desc_width in 0..=60 panics or renders nothing  "
        f"[FAIL = finding #4 is live: broken at desc_width {bad}]",
        f"broken widths: {bad}",
    )
    print(f"        (desc_widths that panic or render nothing: {bad})")

    # Whole-overlay smoke test: every desc_width produces at least one line.
    body_all = consts + body_all
    call_all = r'''
    std::panic::set_hook(Box::new(|_| {}));
    let mut bad = Vec::new();
    for w in 0..=60usize {
        match std::panic::catch_unwind(|| key_help_lines(w).len()) {
            Ok(0) | Err(_) => bad.push(w),
            Ok(_) => {}
        }
    }
    println!("{:?}", bad);'''
    ok, stdout, stderr = compile_and_run(root, "kh_all", body_all, call_all)
    if not ok:
        results.check(False, "key_help_lines harness compiled", stderr)
        return
    bad = [int(w) for w in re.findall(r"\d+", stdout)]
    results.check(
        not bad,
        f"key_help_lines renders all {len(rows)} rows at every desc_width 0..=60  "
        f"[FAIL = finding #4 is live: broken at {bad}]",
        f"broken widths: {bad}",
    )


def rust_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


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

    # `~` must actually resolve to $HOME, not to the current directory.
    cp = ptmdb.run(
        [ptmdb.binary("ptm-sync", PROFILE), "--end",
         "--bucket-name", "b", "--prefix", "p", "--encryption-key", "~"],
        env={"XDG_DATA_HOME": str(root)},
    )
    home = os.environ.get("HOME", "")
    results.check(
        f"Encryption key '{home}'" in cp.stderr,
        "a bare '~' resolves to $HOME  [FAIL = finding #5 is live]",
        cp.stderr.strip()[-300:],
    )


# ── #13: non-ASCII date truncation ──────────────────────────────────────────

def check_non_ascii_truncation(root):
    print("\n[13] date truncation must not split a multi-byte character")

    # The real helper from views/mod.rs, plus the expression it replaced, so
    # the check states both halves: the old form panics on these inputs, the
    # current one does not.
    body = slice_between(VIEWS_MOD, "pub fn date_prefix", "/// Returns how many terminal rows")
    call = r'''
    let cases = ["2025-01-1é", "2025-01-01", "→2025-01-01", "2025-01-01é extra", "é"];
    std::panic::set_hook(Box::new(|_| {}));
    let mut bad = Vec::new();
    for s in cases {
        if std::panic::catch_unwind(|| date_prefix(s).to_string()).is_err() {
            bad.push(s);
        }
    }
    // The historical expression, for contrast (it is what the finding is
    // about).  Reported, not asserted: the fix is the helper above.
    let mut old_panics = Vec::new();
    for s in cases {
        let n = s.len().min(10);
        if std::panic::catch_unwind(|| s[..n].to_string()).is_err() {
            old_panics.push(s);
        }
    }
    println!("BAD {:?}", bad);
    println!("OLD {:?}", old_panics);'''
    ok, stdout, stderr = compile_and_run(root, "dp", views_consts() + body, call)
    if not ok:
        results.check(False, "harness compiled against the real views/mod.rs", stderr)
        return

    bad = re.search(r"BAD (\[.*\])", stdout)
    old = re.search(r"OLD (\[.*\])", stdout)
    results.check(
        bad is not None and bad.group(1) == "[]",
        "date_prefix is safe on non-ASCII dates  "
        "[FAIL = finding #13 is live: reminder strings from --merge or "
        "import_projects.py can panic the UI]",
        bad.group(1) if bad else stdout,
    )
    # The companion shows the check is not vacuous: the old expression still
    # panics on the same inputs, so a regression to it would be caught.
    results.check(
        old is not None and old.group(1) != "[]",
        "the byte-slice form still panics on these inputs (companion: the "
        "fixtures exercise the bug)",
        old.group(1) if old else stdout,
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
