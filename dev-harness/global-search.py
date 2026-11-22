#!/usr/bin/env python3
"""
global-search.py — end-to-end harness for the `F` global full-text search.

Covers the accepted behaviour from `planning-docs/global-search-plan.md`
(§1 UX, §2 decisions, §3 data layer):

- `F` opens a **fresh** search overlay from any screen: empty query, empty
  results, every time — nothing carries over from the previous session.
- Results stream in one line per item while typing, one hit per source with
  the `p|` / `t|` / `u|` / `d|` prefix, updates/descriptions showing the
  first non-empty raw line (blank leading lines skipped, headings kept).
- Update **labels** are neither searched nor displayed.
- Ordering is pure date order — `mtime` DESC, no grouping by type.
- `Enter` with no matches is a no-op and the overlay stays open.
- Deep-link: `Enter` on an update hit opens the project's update viewer.
- Grayed-out path: `Enter` on a hit the destination view would hide
  (here: an archived project with the Project List's `A` toggle off) opens
  the `Warn` naming the screen and the toggle — and does **not** deep-link.

The per-source matching, case-folding, the cap and the normalized `mtime`
are pinned by unit tests in `src/data/repo.rs` (`global_search_*`); this
harness drives the real TUI in a pty — the same driver shape as
`update-viewer.py` — and asserts only on what is visible on screen.

All checks pass against the current tree.

    python3 dev-harness/global-search.py
"""

import fcntl
import os
import pty
import re
import select
import struct
import subprocess
import sys
import termios
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import ptmdb  # noqa: E402

PROFILE = os.environ.get("PTM_PROFILE", "debug")
results = ptmdb.Results("global-search")

QUERY_MARKER = "nimbus"          # matches project/todo/update/description
DEEPLINK_MARKER = "deeplink"     # matches the update body only
ARCHIVED_MARKER = "grayarch"     # matches the archived project only
LABEL_MARKER = "labelnimbus"     # lives in the update label only (not searched)
PROJECT_TITLE = "Nimbus refactoring plan"
DESC_FIRST_LINE = "# Nimbus notes"
UPDATE_FIRST_LINE = "Nimbus update for the deeplink flow"
TODO_PROJECTED = "nimbus projected todo"
TODO_INBOX = "nimbus inbox todo"
ARCHIVED_TITLE = "GRAYARCH archived marker project"


class Tui:
    """Drive `ptm` inside a pty and scrape its screen.

    Mirrors the driver in `update-viewer.py` (the only other harness that
    needs a terminal).  The extra `repaint()` exists for *absence* checks:
    ratatui diffs against the previous frame, so a normal key press only
    emits what changed — a pty resize forces the next draw to repaint
    everything, which a cleared buffer then captures in isolation.
    """

    def __init__(self, argv, env, cols=100, rows=30):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

        def controlling_tty():
            """Child-side setup: make the pty the controlling terminal.

            Without this, `/dev/tty` in the child resolves to the *harness
            process's* terminal.  crossterm's `terminal::size()` — which
            ratatui's auto-resize uses — opens `/dev/tty` and runs
            `TIOCGWINSZ` on it, so the child would render at some foreign
            size and never notice our resize ioctl (see `repaint()`).
            """
            os.setsid()  # detach from the harness's session; become session leader
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)  # claim the pty as our tty

        self.proc = subprocess.Popen(
            [str(a) for a in argv], stdin=slave, stdout=slave, stderr=slave,
            env=env, close_fds=True, preexec_fn=controlling_tty,
        )
        os.close(slave)
        self.buf = ""
        self.rows, self.cols = rows, cols
        # Start in a known state: one frame, then wait.
        self.settle()

    def drain(self, seconds):
        """Read whatever the program writes for `seconds`.

        Answers cursor-position requests (`ESC [ 6 n`): ratatui queries the
        terminal before drawing and fails with "The cursor position could not
        be read within a normal duration" if nothing replies.  A real terminal
        answers `ESC [ <row> ; <col> R`.
        """
        deadline = time.time() + seconds
        while time.time() < deadline:
            r, _, _ = select.select([self.master], [], [], 0.05)
            if not r:
                continue
            try:
                chunk = os.read(self.master, 65536)
            except OSError:
                break
            if not chunk:
                break
            text = chunk.decode("utf-8", "replace")
            if "\x1b[6n" in text:
                os.write(self.master, b"\x1b[1;1R")
                text = text.replace("\x1b[6n", "")
            self.buf += text
        return self.buf

    def settle(self, seconds=0.45):
        """Let the program draw, then return the current screen text."""
        return self.drain(seconds)

    def press(self, *keys, settle=0.45):
        for k in keys:
            os.write(self.master, k.encode())
            time.sleep(0.05)
        return self.settle(settle)

    def repaint(self):
        """Force a full redraw and return only the current screen.

        Clears the buffer, toggles the pty width by one column (SIGWINCH ->
        ratatui clears and repaints everything on the next draw), then
        restores the width and returns what was written.  Both draws show the
        same state, so either is a valid snapshot; the width bump never
        changes any layout this harness asserts on (the search popup is
        capped at 74 columns).
        """
        self.buf = ""
        for cols in (self.cols + 1, self.cols):
            fcntl.ioctl(self.master, termios.TIOCSWINSZ,
                        struct.pack("HHHH", self.rows, cols, 0, 0))
            self.drain(0.45)
        return self.screen()

    def snap(self, *keys):
        """Press keys, then return the current screen as a clean snapshot."""
        self.press(*keys)
        return self.repaint()

    def screen(self):
        """Visible text of the last frame(s), with ANSI escapes stripped."""
        text = re.sub(r"\x1b\[[0-9;?]*[ -/]*[@-~]", " ", self.buf)
        text = re.sub(r"\x1b[()][A-Za-z0-9]", " ", text)
        text = text.replace("\x1b", " ")
        return text[-20000:]

    def close(self):
        try:
            self.proc.terminate()
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        try:
            os.close(self.master)
        except OSError:
            pass


def seed_db(db_path):
    """Four sources plus one hit that must not exist.

    `updated_at` values put the update newest, the project (and its
    description, which shares the project's `mtime`) next, the projected todo
    next and the Inbox todo last — `global_search` is pure `mtime DESC` order
    with no grouping by type.
    """
    conn = ptmdb.new_db(db_path)
    conn.execute(
        "INSERT INTO projects (id, title, description, sort_order, archived, created_at, updated_at)"
        " VALUES ('p1', ?, ?, 1.0, 0, '2025-02-01T00:00:00Z', '2025-02-01T00:00:00Z')",
        (PROJECT_TITLE, f"\n{DESC_FIRST_LINE}\nsecond description line"),
    )
    # Archived, no description, no todos: searching its marker must yield
    # exactly one hit — the grayed-out `p|` row.
    conn.execute(
        "INSERT INTO projects (id, title, description, sort_order, archived, created_at, updated_at)"
        " VALUES ('p2', ?, '', 2.0, 1, '2025-04-01T00:00:00Z', '2025-04-01T00:00:00Z')",
        (ARCHIVED_TITLE,),
    )
    # The label is *not* searched; the body's first line carries the deep-link
    # marker (`deeplink`) — a query for the marker alone selects exactly this
    # row.  It also matches `nimbus`, and as the newest `nimbus` hit it must
    # sort above the project.
    conn.execute(
        "INSERT INTO updates (id, project_id, label, body, sort_order, created_at, updated_at)"
        " VALUES ('u1', 'p1', ?, ?, 0.0, '2025-03-01T00:00:00Z', '2025-03-01T00:00:00Z')",
        (LABEL_MARKER, f"{UPDATE_FIRST_LINE}\nburied second line"),
    )
    conn.execute(
        "INSERT INTO todos (id, project_id, title, status, sort_order, archived, created_at, updated_at)"
        " VALUES ('t1', 'p1', ?, 'new', 0.0, 0, '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        (TODO_PROJECTED,),
    )
    # An Inbox todo: `project_id IS NULL`.
    conn.execute(
        "INSERT INTO todos (id, project_id, title, status, sort_order, archived, created_at, updated_at)"
        " VALUES ('t2', NULL, ?, 'new', 0.0, 0, '2024-12-01T00:00:00Z', '2024-12-01T00:00:00Z')",
        (TODO_INBOX,),
    )
    conn.close()


def main():
    with ptmdb.TempWorkspace("ptm-globsearch-") as root:
        data = ptmdb.data_dir(root)
        db = data / "ptm.db"
        seed_db(db)
        env = dict(os.environ)
        env["XDG_DATA_HOME"] = str(root)
        env["TERM"] = "xterm-256color"
        env.pop("EDITOR", None)
        env.pop("PTM_DICT_DIR", None)

        tui = Tui([ptmdb.binary("ptm", PROFILE)], env)
        try:
            # -- `F` opens a fresh search overlay ----------------------------
            tui.press("F")
            results.check(
                "Search:" in tui.repaint(),
                "F opens the global search overlay from the Project List",
                f"screen:\n{tui.screen()[-1200:]}",
            )

            # -- every source matches, with its prefix and display text ------
            screen = tui.snap(QUERY_MARKER)
            for fragment in (
                f"p|{PROJECT_TITLE}",
                f"t|{TODO_PROJECTED}",
                f"t|{TODO_INBOX}",
                f"u|{UPDATE_FIRST_LINE}",
                f"d|{DESC_FIRST_LINE}",
            ):
                results.check(
                    fragment in screen,
                    f"{fragment[:2]}… row appears: {fragment!r}",
                    f"screen:\n{screen[-1200:]}",
                )

            # The description's *blank leading line* is skipped: the row
            # starts at the heading, not at the empty line.
            results.check(
                "d| " not in screen and f"d|{DESC_FIRST_LINE}" in screen,
                "the description row skips blank leading lines and keeps the heading",
                f"screen:\n{screen[-1200:]}",
            )

            # -- pure date order, newest first, no grouping by type ----------
            order = [
                screen.find(f"u|{UPDATE_FIRST_LINE}"),
                screen.find(f"p|{PROJECT_TITLE}"),
                screen.find(f"d|{DESC_FIRST_LINE}"),
                screen.find(f"t|{TODO_PROJECTED}"),
                screen.find(f"t|{TODO_INBOX}"),
            ]
            results.check(
                all(p >= 0 for p in order) and order == sorted(order),
                "results are in mtime order: update, project/description, projected todo, Inbox todo"
                "  (mtime DESC, no grouping by type)",
                f"positions: {order}\nscreen:\n{screen[-1200:]}",
            )

            # -- results stream: one keystroke more empties the list ---------
            screen = tui.snap("x")   # query is now "nimbusx": no matches
            results.check(
                "u|" not in screen and "p|" not in screen and "Search: nimbusx_" in screen,
                "extending the query to a non-match clears the results",
                f"screen:\n{screen[-1200:]}",
            )
            screen = tui.snap("\x7f")  # Backspace: back to "nimbus"
            results.check(
                f"u|{UPDATE_FIRST_LINE}" in screen,
                "Backspace restores the results (the search re-runs per keystroke)",
                f"screen:\n{screen[-1200:]}",
            )

            # -- update labels are not searched ------------------------------
            backspaces = "\x7f" * len(QUERY_MARKER)
            screen = tui.snap(backspaces, LABEL_MARKER)
            results.check(
                "Search: " + LABEL_MARKER + "_" in screen
                and "u|" not in screen and "p|" not in screen
                and "t|" not in screen and "d|" not in screen,
                "the update label is not searched (a query matching only the label finds nothing)",
                f"screen:\n{screen[-1200:]}",
            )

            # -- Enter with no matches is a no-op: the overlay stays ---------
            screen = tui.snap("\r")
            results.check(
                f"Search: {LABEL_MARKER}_" in screen,
                "Enter on the query box with no matches does nothing and the overlay stays open",
                f"screen:\n{screen[-1200:]}",
            )

            # -- every F press starts fresh: no query carries over -----------
            tui.press("\x1b")        # ESC closes the search overlay
            screen = tui.snap("F", "z")
            results.check(
                "Search: z_" in screen and LABEL_MARKER not in screen,
                "a reopened search overlay is empty — no state carried over"
                "  (if state leaked, the query would read 'labelnimbusz')",
                f"screen:\n{screen[-1200:]}",
            )

            # -- deep-link: Enter on an update hit opens the update viewer ---
            screen = tui.snap("\x7f", DEEPLINK_MARKER)  # clear "z", search the body marker
            results.check(
                f"u|{UPDATE_FIRST_LINE}" in screen
                and "p|" not in screen and "t|" not in screen and "d|" not in screen,
                "the deep-link query selects exactly the update row (fixture)",
                f"screen:\n{screen[-1200:]}",
            )
            screen = tui.snap("\r", "\r")  # focus the first result, then open it
            results.check(
                UPDATE_FIRST_LINE in screen and "j/k:scroll" in screen and "e:edit" in screen,
                "Enter on an update hit deep-links into the update viewer (viewer hint bar visible)",
                f"screen:\n{screen[-1200:]}",
            )

            # -- grayed-out hit: Enter warns instead of deep-linking ---------
            # ESC leaves the viewer (into Project Detail), ESC again returns
            # to the Project List — the archived project is hidden there.
            screen = tui.snap("\x1b", "\x1b", "F", ARCHIVED_MARKER)
            results.check(
                f"p|{ARCHIVED_TITLE}" in screen,
                "the archived project matches (archived items are included in the results)",
                f"screen:\n{screen[-1200:]}",
            )
            screen = tui.snap("\r", "\r")  # focus the hit, then try to open it
            results.check(
                "Archived result" in screen and "Press A on the Project List" in screen,
                "Enter on a grayed-out hit opens the Warn naming the screen and the A toggle",
                f"screen:\n{screen[-1200:]}",
            )
            # Any key dismisses the Warn (and is re-dispatched); the deep-link
            # must NOT have happened.
            screen = tui.snap("\x1b")
            results.check(
                "Q:quit" in screen and "Search:" not in screen and "j/k:scroll" not in screen,
                "the warning path does not deep-link — the Project List is still showing",
                f"screen:\n{screen[-1200:]}",
            )
        finally:
            tui.close()

    return results.finish()


if __name__ == "__main__":
    sys.exit(main())
