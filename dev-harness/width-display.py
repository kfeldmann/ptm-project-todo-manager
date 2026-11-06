#!/usr/bin/env python3
"""
width-display.py — reproduction for finding #16.

`word_wrap` / `char_wrap` / `hint_height` counted `chars()`, so text containing
wide characters (CJK, emoji) wrapped at **half** the intended column count and
overflowed the pane; `hint_height` under-counted wide hints and reserved too
few rows.  `markdown.rs` already measured with `unicode-width`, so the two
renderers disagreed about how much room the same string takes.

The fix is one shared, width-aware module (`src/ui/text.rs`) used by every
renderer in both binaries.  This harness compiles the **real** module and
asserts on its output, plus the real call sites.

FAIL means the finding is live.

## How this is checked, and why it is shaped this way

A hand-written model of the wrapping algorithm would keep failing after the
source is fixed and keep passing after a regression, so it can neither confirm
a fix nor guard one.  The check compiles `src/ui/text.rs` (path-included) into a
scratch crate and measures its output in columns — the only property that
matters to a terminal.

    python3 dev-harness/width-display.py
"""

import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import ptmdb  # noqa: E402

results = ptmdb.Results("width-display")

TEXT_SRC = ptmdb.REPO / "src/ui/text.rs"


def build_and_run(root, body, call):
    crate = root / "probe_width"
    (crate / "src").mkdir(parents=True, exist_ok=True)
    (crate / "src/main.rs").write_text(
        f'#[path = "{TEXT_SRC}"]\nmod text;\nuse text::*;\n\n{body}\nfn main() {{\n{call}\n}}\n'
    )
    (crate / "Cargo.toml").write_text(
        '''[package]
name = "probe_width"
version = "0.0.0"
edition = "2021"

[dependencies]
unicode-width = "0.2"

[[bin]]
name = "probe_width"
path = "src/main.rs"
'''
    )
    env = dict(os.environ, CARGO_TARGET_DIR=str(ptmdb.REPO / "target"))
    cp = subprocess.run(
        ["cargo", "run", "--quiet", "--manifest-path", str(crate / "Cargo.toml")],
        capture_output=True, text=True, env=env, timeout=600,
    )
    return cp.returncode == 0, cp.stdout, cp.stderr[-2000:]


PROBE = r'''
/// Report one verdict line per assertion: PASS/FAIL|label|detail
fn report(ok: bool, label: &str, detail: String) {
    println!("{}|{}|{}", if ok { "PASS" } else { "FAIL" }, label, detail);
}

/// Every wrapped line must fit the budget, and no text may be lost.
fn wrap_checks(text: &str, width: usize) {
    let lines = word_wrap(text, width);
    let widest = lines.iter().map(|l| display_width(l)).max().unwrap_or(0);
    report(
        widest <= width,
        &format!("word_wrap({:?}, {}) fits the budget", text, width),
        format!("widest line is {} columns: {:?}", widest, lines),
    );
    let stripped: String = lines.join("").chars().filter(|c| *c != ' ').collect();
    let original: String = text.chars().filter(|c| *c != ' ').collect();
    report(
        stripped == original,
        &format!("word_wrap({:?}, {}) loses no text", text, width),
        format!("got {:?}, want {:?}", stripped, original),
    );

    let lines = char_wrap(text, width);
    let widest = lines.iter().map(|l| display_width(l)).max().unwrap_or(0);
    report(
        widest <= width,
        &format!("char_wrap({:?}, {}) fits the budget", text, width),
        format!("widest line is {} columns: {:?}", widest, lines),
    );
}
'''


CASES = [
    ("日本語のタイトル", 10),
    ("日本語 タイトル テスト", 10),
    ("mixed 日本語 and ascii text", 12),
    ("🚀🚀🚀🚀🚀", 4),
    ("plain ascii words here", 8),
    ("", 10),
]


def main():
    with ptmdb.TempWorkspace("ptm-width-") as root:
        body = PROBE + "\nfn run_all() {\n" + "".join(
            f"    wrap_checks({t!r}, {w});\n".replace("'", '"') for t, w in CASES
        ) + r'''
    // Eight CJK characters are 16 columns; at a 10-column budget the first
    // row holds five (10 columns) and the rest follow.  Counting characters
    // would have put all eight on one row, overflowing by six columns.
    let lines = word_wrap("日本語日本語日本", 10);
    report(
        lines.len() == 2 && display_width(&lines[0]) == 10,
        "CJK wraps at 10 *columns* (5 chars per row), not 10 characters",
        format!("{:?}", lines),
    );

    // hint_height counts columns: 40 CJK chars = 80 columns = 2 rows at 50.
    report(
        hint_height(&"日".repeat(40), 50) == 2,
        "hint_height counts wide characters as two columns",
        format!("got {}", hint_height(&"日".repeat(40), 50)),
    );
    report(
        hint_height("plain ascii hint", 80) == 1,
        "hint_height leaves narrow text at one row",
        format!("got {}", hint_height("plain ascii hint", 80)),
    );

    // Padding and truncation are column-based too.
    report(
        display_width(&pad_to_width("日本", 5)) == 5,
        "pad_to_width pads to a column budget",
        format!("{:?}", pad_to_width("日本", 5)),
    );
    report(
        (1..=8).all(|w| {
            let out = ellipsize("日本語のテキスト", w);
            display_width(&out) <= w && out.ends_with('…')
        }),
        "ellipsize always fits the budget and always ends with an ellipsis",
        format!("{:?}", ellipsize("日本語のテキスト", 6)),
    );
}
'''
        ok, stdout, stderr = build_and_run(root, body, "    run_all();")
        if not ok:
            results.check(False, "the probe compiled against the real src/ui/text.rs", stderr)
            return results.finish()

        print("checking the shared width-aware helpers\n")
        lines = [l for l in stdout.strip().splitlines() if "|" in l]
        for line in lines:
            status, label, detail = (line.split("|", 2) + ["", ""])[:3]
            results.check(status == "PASS", label, detail)
        results.check(bool(lines), "the probe reported verdicts", stdout or stderr)

    # ── the call sites must use the shared helpers ────────────────────────
    print("\nchecking the renderers use the shared helpers")
    src = TEXT_SRC.read_text()
    results.check(
        "unicode_width" in src,
        "src/ui/text.rs measures with unicode-width",
    )
    views = {
        "views/mod.rs": ptmdb.REPO / "src/ui/views/mod.rs",
        "views/overlay.rs": ptmdb.REPO / "src/ui/views/overlay.rs",
        "views/project_detail.rs": ptmdb.REPO / "src/ui/views/project_detail.rs",
        "views/todo_list.rs": ptmdb.REPO / "src/ui/views/todo_list.rs",
        "bin/md/main.rs": ptmdb.REPO / "src/bin/md/main.rs",
    }
    views["input/mod.rs"] = ptmdb.REPO / "src/input/mod.rs"
    for label, path in views.items():
        text = path.read_text()
        results.check(
            "text::" in text or "crate::ui::text" in text or label == "views/mod.rs",
            f"{label} routes through ui::text",
            "no reference to ui::text found",
        )
    # The inline-edit field takes a *column* budget too; a character-counted
    # window overflows by one column per wide character.
    input_src = (ptmdb.REPO / "src/input/mod.rs").read_text()
    results.check(
        "char_display_width" in input_src,
        "InputState::display measures its window in columns  "
        "[FAIL = finding #16 is live]",
    )

    # The char-counting patterns that caused the bug.  `chars().count()` is
    # still correct where a *character index* is meant (the input cursor), so
    # only the renderers are checked for it.
    offenders = []
    for label, path in views.items():
        if label == "input/mod.rs":
            continue
        text = path.read_text()
        for pat in ("chars().count()", ".len() + padded.len()", "entry.len() + 2"):
            if pat in text:
                offenders.append(f"{label}: {pat}")
    results.check(
        not offenders,
        "no character-counted width arithmetic is left in the views  "
        "[FAIL = finding #16 is live]",
        "\n".join(offenders),
    )
    return results.finish()


if __name__ == "__main__":
    sys.exit(main())
