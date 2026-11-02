#!/usr/bin/env python3
"""
update-viewer.py — reproduction for findings #6, #14, #15 and #23.

  6   `?`, `n` and `N` were dead inside the update viewer, and the hint bar
      advertised them anyway.  `E` and `T` were dead too.
  14  The renderers publish geometry and search-match positions into `App`
      during render (`PublishedMetrics`), which is the one documented exception
      to "rendering does not mutate state".
  15  The `Warn` overlay discarded the key that dismissed it.
  23  `handle_key_project_detail`'s `N` arm was unreachable in viewer mode, and
      the `n` arm's `pd_open_update.is_some()` condition was dead code.

Only #6/#23 are observable from the outside: they are the ones where a key
press produces no visible change.  #14 and #15 are internal contracts and are
covered by unit tests in `src/app.rs` instead (`?` is *not* a good probe for
#6 — it is handled globally, before the screen dispatch, so it worked even in
the unfixed tree).

## Why a pty

`ptm` is a raw-mode TUI: it needs a terminal on stdin.  This harness allocates
a real pty (`pty.openpty`), sets its window size, and drives the program with
keystrokes while scraping the rendered screen.  Nothing else in the suite needs
a terminal, which is why #6 had no harness before.

## What FAIL means

`FAIL` = the finding is live: a key that the viewer advertises does nothing.

    python3 dev-harness/update-viewer.py
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
results = ptmdb.Results("update-viewer")

DESC_MARKER = "DESCRIPTION-MARKER-42"
BODY_MARKER = "UPDATE-BODY-MARKER-99"


class Tui:
    """Drive `ptm` inside a pty and scrape its screen."""

    def __init__(self, argv, env, cols=100, rows=30):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.proc = subprocess.Popen(
            [str(a) for a in argv], stdin=slave, stdout=slave, stderr=slave,
            env=env, close_fds=True,
        )
        os.close(slave)
        self.buf = ""
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

    def screen(self):
        """Visible text of the last frame, with ANSI escapes stripped.

        The alternate screen means the buffer holds every frame ever drawn, so
        this keeps the tail — enough to assert on what is on screen now.
        """
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
    conn = ptmdb.new_db(db_path)
    conn.execute(
        "INSERT INTO projects (id, title, description, sort_order, archived, created_at, updated_at)"
        " VALUES ('p1', 'Harness Project', ?, 1.0, 0, '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        (DESC_MARKER,),
    )
    conn.execute(
        "INSERT INTO updates (id, project_id, label, body, sort_order, created_at, updated_at)"
        " VALUES ('u1', 'p1', '2025-01-01', ?, 0.0, '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        (BODY_MARKER,),
    )
    conn.close()


def open_viewer(tui):
    """Project List → open project → Updates tab → open the update viewer."""
    tui.press("\r")          # Enter: open the selected project
    tui.press("u")           # Updates tab
    tui.press("\r")          # Enter: open the update in the viewer
    return tui.screen()


def main():
    with ptmdb.TempWorkspace("ptm-viewer-") as root:
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
            screen = open_viewer(tui)
            results.check(
                BODY_MARKER in screen,
                "the update viewer opened (fixture reached)",
                f"screen tail:\n{screen[-1200:]}",
            )

            # ── finding #6/#23: `d` switches tabs, which the viewer ignored ──
            # The viewer guard used to run *before* the universal keys, so a
            # tab switch was dropped.  Switching away also closes the viewer.
            screen = tui.press("d")
            results.check(
                DESC_MARKER in screen,
                "d switches to the Description tab from inside the update viewer"
                "  [FAIL = finding #6/#23 is live]",
                f"screen tail:\n{screen[-1200:]}",
            )

            # Back into the viewer for the next probe.
            screen = open_viewer_again(tui)
            results.check(
                BODY_MARKER in screen,
                "the viewer re-opened for the second probe",
                f"screen tail:\n{screen[-1200:]}",
            )

            # ── finding #6: `E` edits the project title from the viewer ───────
            screen = tui.press("E")
            results.check(
                "editing title" in screen.lower() or "editing title" in screen,
                "E starts an inline project-title edit from the viewer"
                "  [FAIL = finding #6 is live: the viewer swallows universal keys]",
                f"screen tail:\n{screen[-1200:]}",
            )
            tui.press("\x1b")  # ESC: cancel the inline edit
        finally:
            tui.close()

    return results.finish()


def open_viewer_again(tui):
    """From the Description tab: back to Updates, reopen the viewer."""
    tui.press("u")
    tui.press("\r")
    return tui.screen()


if __name__ == "__main__":
    sys.exit(main())
