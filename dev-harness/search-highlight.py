#!/usr/bin/env python3
"""
search-highlight.py — reproduction for finding #17.

`split_span` lowercased the text to find a case-insensitive match and then used
the *lowercased* offsets to slice the *original* text.  Case folding is not
length-preserving (`'İ'.to_lowercase()` is two chars, `i` + U+0307), so the
highlighted range drifts: searching `İ` in `İstanbul` highlighted `İs` instead
of `İ`.

The matcher is now a single shared helper, `ui::text::find_matches` /
`ui::text::split_matches`, used by both `ptm` (`views/project_detail.rs`) and
`md` (`bin/md/main.rs`) — the two copies had drifted and the fix would
otherwise have had to be made twice.

FAIL means the finding is live.

## How this is checked, and why it is shaped this way

The check compiles the **real `src/ui/text.rs`** with `rustc` and asserts on its
output, rather than re-typing the algorithm here.  A hand-written model keeps
failing after the source is fixed (which makes the harness useless as a
regression check) and keeps passing if the fix is later reverted (which makes
it useless as a reproduction).  `src/ui/text.rs` only depends on
`unicode-width`, which is already built under `target/`, so the compile is
cheap and offline.

    python3 dev-harness/search-highlight.py
"""

import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import ptmdb  # noqa: E402

results = ptmdb.Results("search-highlight")

TEXT_SRC = ptmdb.REPO / "src/ui/text.rs"
SEARCH_SRC = ptmdb.REPO / "src/ui/search.rs"


def build_and_run(root, body, call):
    """Compile the real `src/ui/text.rs` + `src/ui/search.rs` and run the probe.

    A scratch crate rather than a bare `rustc` call: the modules need
    `unicode-width` and `ratatui`, and letting cargo resolve them means the
    probe builds with the same compiler and the same already-built
    dependencies as the binaries.  The crate lives under the workspace's own
    `target/`, so nothing is rebuilt.
    """
    crate = root / "probe_search"
    (crate / "src").mkdir(parents=True, exist_ok=True)
    # The real modules address each other as `crate::ui::text::…`, so the
    # probe mirrors that shape instead of flattening them to the crate root.
    (crate / "src/main.rs").write_text(
        f'mod ui {{\n'
        f'    #[path = "{TEXT_SRC}"]\n    pub mod text;\n'
        f'    #[path = "{SEARCH_SRC}"]\n    pub mod search;\n'
        f'}}\n'
        f'use ui::text::*;\nuse ui::search::*;\n\n{body}\nfn main() {{\n{call}\n}}\n'
    )
    (crate / "Cargo.toml").write_text(
        '''[package]
name = "probe_search"
version = "0.0.0"
edition = "2021"

[dependencies]
unicode-width = "0.2"
ratatui = { version = "0.30.2", default-features = false, features = ["crossterm", "layout-cache"] }

[[bin]]
name = "probe_search"
path = "src/main.rs"
'''
    )
    env = dict(os.environ, CARGO_TARGET_DIR=str(ptmdb.REPO / "target"))
    cp = subprocess.run(
        ["cargo", "run", "--quiet", "--manifest-path", str(crate / "Cargo.toml")],
        capture_output=True, text=True, env=env, timeout=600,
    )
    if cp.returncode != 0:
        return False, cp.stdout, cp.stderr[-2000:]
    return True, cp.stdout, cp.stderr


def rust_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def legacy_match(text, query):
    """The *old* algorithm, for contrast: find in the lowercased string, then
    slice the original at those offsets.  Reported, not asserted — it is what
    the finding is about, and it must keep drifting so the behavioural check
    above is not vacuous."""
    lower_text = text.lower()
    idx = lower_text.find(query.lower())
    if idx < 0:
        return []
    return [text[idx:idx + len(query)]]


def check_call_sites():
    """Both call sites must route through the shared matcher.

    The behavioural check above proves the helper is right; this proves the UI
    actually uses it.  It also rejects the two patterns that caused the drift
    (`lower_*[i..i + qlen]` slicing and `text.to_lowercase().chars()` offset
    arithmetic) so a re-introduced copy is caught even if the shared helper
    survives.
    """
    print("\nchecking the call sites route through the shared matcher")
    sites = {
        "ptm (views/project_detail.rs)": ptmdb.REPO / "src/ui/views/project_detail.rs",
        "md (bin/md/main.rs)": ptmdb.REPO / "src/bin/md/main.rs",
    }
    for label, path in sites.items():
        src = path.read_text()
        # Both renderers now go through `ui::search`, the single highlighter
        # that also handles phrase queries spanning several spans (a query with
        # a space never fit inside one span, so `/We the people` matched
        # nothing).  `ui::search` itself calls `text::find_matches`.
        results.check(
            "search::highlight(" in src or "ui::search::highlight(" in src,
            f"{label} uses ui::search::highlight",
        )
        # No per-binary copy of the matcher may come back: the two copies had
        # already drifted (md's miscounted occurrences within a line).
        results.check(
            "fn split_span" not in src and "fn compute_search_matches" not in src,
            f"{label} has no private highlighter copy left",
        )
        # The defect was indexing the *original* text with offsets taken from
        # the lowercased copy.  Lowercasing the query (`search.to_lowercase()`)
        # is correct and stays; `lower_text` / `lower_chars` may not come back.
        buggy = [
            pat for pat in (
                r"lower_(?:text|chars)\b",
                r"\[\s*i\s*\.\.\s*i\s*\+\s*qlen\s*\]",
            )
            if re.search(pat, src)
        ]
        results.check(
            not buggy,
            f"{label} has no lowercased-offset slicing left  "
            "[FAIL = finding #17 is live]",
            f"matched {buggy}",
        )


# (text, query, expected matched substrings) — the last is what the *fixed*
# matcher must report.  `İstanbul`/`i` and `İstanbul`/`İ` are the review's
# reproductions; the rest guard the ordinary path.
CASES = [
    ("İstanbul", "i", ["İ"]),
    ("İstanbul", "İ", ["İ"]),
    # A match that starts *after* the two-char fold of `İ`: the old code
    # sliced the original at the lowercased offset, so it reported `tanb`
    # instead of `stan`.
    ("İstanbul", "stan", ["stan"]),
    ("Café", "café", ["Café"]),
    ("STRASSE", "strasse", ["STRASSE"]),
    ("日本語テキスト", "語", ["語"]),
    ("aaa", "a", ["a", "a", "a"]),
    ("no match here", "zzz", []),
]


# (description, rendered-line span texts, query, current index, expected
# yellow text) — the two defects reported against `/` in the Project Detail
# Description tab and the update viewer.
#
# `render_markdown` emits one span per word, which is what makes a phrase query
# fail when matching is done span-by-span.
HIGHLIGHT_CASES = [
    # A phrase spans several spans.  Before the fix this matched nothing at all
    # and the highlight vanished as soon as a space was typed.
    (
        "phrase across per-word spans",
        ["We", " ", "the", " ", "people", " of the United States"],
        "We the people", 0, "We the people",
    ),
    # Several matches on one line: the current one must be yellow and it must
    # move with the pointer.  Before the fix `n` moved the pointer but left
    # every match gray.
    ("first of three on one line", ["alpha beta alpha alpha"], "alpha", 0, "alpha"),
    ("second of three on one line", ["alpha beta alpha alpha"], "alpha", 1, "alpha"),
    ("third of three on one line", ["alpha beta alpha alpha"], "alpha", 2, "alpha"),
    # The markdown renderer's markup spans must not break a match.
    ("phrase across a bold boundary", ["We the ", "people", " said"], "the people", 0, "the people"),
    # A wide character match must not be split.
    ("wide character", ["日本語テキスト"], "語", 0, "語"),
]


def main():
    body = ""
    call_lines = []
    for text, query, want in CASES:
        want_lit = "vec![" + ", ".join(rust_str(w) for w in want) + "]"
        call_lines.append(
            f'    check({rust_str(text)}, {rust_str(query)}, {want_lit});'
        )

    body = r'''
fn check(text: &str, query: &str, want: Vec<&str>) {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let got: Vec<String> = split_matches(text, &q)
        .into_iter()
        .filter(|(_, m)| *m)
        .map(|(seg, _)| seg)
        .collect();
    let joined: String = split_matches(text, &q)
        .into_iter()
        .map(|(seg, _)| seg)
        .collect();
    let ok = got == want && joined == text;
    println!("{}|{}|{}|{:?}|{:?}", if ok {"PASS"} else {"FAIL"}, text, query, got, want);
}
'''
    call = "\n".join(call_lines)
    with ptmdb.TempWorkspace("ptm-search-") as root:
        ok, stdout, stderr = build_and_run(root, body, call)
        if not ok:
            results.check(False, "the probe compiled against the real src/ui/text.rs", stderr)
            return results.finish()

        print("checking split_matches case-fold handling\n")
        for line in stdout.strip().splitlines():
            parts = line.split("|")
            if len(parts) != 5:
                continue
            status, text, query, got, want = parts
            results.check(
                status == "PASS",
                f"search {query!r} in {text!r} highlights {want}",
                f"got {got}",
            )
        # The probe reports one line per case; a silent failure (no output at
        # all) must not be mistaken for success.
        results.check(
            len(stdout.strip().splitlines()) == len(CASES),
            f"all {len(CASES)} cases reported a verdict",
            stdout or stderr,
        )

    # Companion: the old algorithm still drifts on these fixtures, so the
    # behavioural check above is not vacuous.
    drifted = [t for t, q, want in CASES if legacy_match(t, q) != want]
    results.check(
        bool(drifted),
        "the legacy algorithm still mis-highlights these fixtures "
        "(companion: the cases exercise the bug)",
        drifted,
    )
    for text, query, want in CASES:
        got = legacy_match(text, query)
        if got != want:
            print(f"        legacy {query!r} in {text!r} -> {got} (correct: {want})")

    check_highlight_cases()
    check_call_sites()
    return results.finish()


def check_highlight_cases():
    """Drive the real `ui::search::highlight` on the reported defects."""
    call_lines = []
    for label, spans, query, current, want in HIGHLIGHT_CASES:
        spans_lit = "vec![" + ", ".join(rust_str(x) for x in spans) + "]"
        call_lines.append(
            f'    check_hl({rust_str(label)}, {spans_lit}, {rust_str(query)}, '
            f'{current}, {rust_str(want)});'
        )
    body = r'''
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

fn check_hl(label: &str, spans: Vec<&str>, query: &str, current: usize, want: &str) {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let line = Line::from(spans.iter().map(|s| Span::raw(s.to_string())).collect::<Vec<_>>());
    let cur = Style::default().bg(Color::Yellow);
    let other = Style::default().bg(Color::DarkGray);
    let hl = highlight(vec![line], &q, Some(current), cur, other);
    let yellow: String = hl.lines[0].spans.iter()
        .filter(|s| s.style.bg == Some(Color::Yellow))
        .map(|s| s.content.as_ref()).collect::<String>();
    let joined: String = hl.lines[0].spans.iter()
        .map(|s| s.content.as_ref()).collect::<String>();
    let original: String = spans.concat();
    let ok = yellow == want && joined == original;
    println!("{}|{}|{}|{}", if ok {"PASS"} else {"FAIL"}, label, yellow, want);
}
'''
    call = "\n".join(call_lines)
    with ptmdb.TempWorkspace("ptm-search-hl-") as root:
        ok, stdout, stderr = build_and_run(root, body, call)
        if not ok:
            results.check(False, "the highlight probe compiled against src/ui/search.rs", stderr)
            return
        print("checking current-match highlighting and phrase search\n")
        seen = 0
        for line in stdout.strip().splitlines():
            parts = line.split("|")
            if len(parts) != 4:
                continue
            seen += 1
            status, label, got, want = parts
            results.check(
                status == "PASS",
                f"{label}: the yellow match is {want!r}",
                f"got {got!r}",
            )
        results.check(
            seen == len(HIGHLIGHT_CASES),
            f"all {len(HIGHLIGHT_CASES)} highlight cases reported a verdict",
            stdout or stderr,
        )


if __name__ == "__main__":
    sys.exit(main())
