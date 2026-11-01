#!/usr/bin/env python3
"""
ptmdb.py — shared helpers for the ptm remediation harnesses.

Import from a sibling harness:

    import sys, pathlib
    sys.path.insert(0, str(pathlib.Path(__file__).parent / "lib"))
    import ptmdb

Nothing here is imported by the application. These helpers exist so the
harnesses stay reproducible after the original session ends.
"""

import os
import shutil
import sqlite3
import subprocess
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

# The harnesses must run from inside the repo: they read src/ and drive
# target/. Fail with a clear message rather than an obscure error if the
# directory has been copied elsewhere.
if not (REPO / "Cargo.toml").is_file() or not (REPO / "src").is_dir():
    raise SystemExit(
        f"dev-harness must live inside the ptm repo (looked for Cargo.toml and "
        f"src/ under {REPO}).\n"
        f"The harnesses read src/ and run binaries from target/; copy the whole "
        f"repo, not just dev-harness/."
    )

# The full current schema, kept in sync with SCHEMA in src/data/db.rs.
SCHEMA = """
CREATE TABLE IF NOT EXISTS tags (
    id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, color TEXT
);
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY, title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '', sort_order REAL NOT NULL DEFAULT 0.0,
    archived INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS project_tags (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (project_id, tag_id)
);
CREATE TABLE IF NOT EXISTS updates (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    label TEXT NOT NULL DEFAULT '', body TEXT NOT NULL,
    sort_order REAL NOT NULL DEFAULT 0.0,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS todos (
    id TEXT PRIMARY KEY,
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'new'
        CHECK(status IN ('new','in_progress','done','canceled')),
    reminder TEXT, sort_order REAL NOT NULL DEFAULT 0.0,
    archived INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS references_table (
    id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL DEFAULT '',
    sort_order REAL NOT NULL DEFAULT 0.0,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS reference_tags (
    reference_id TEXT NOT NULL REFERENCES references_table(id) ON DELETE CASCADE,
    tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (reference_id, tag_id)
);
"""


def new_db(path, schema=SCHEMA, wal=True):
    """Create a database at `path` with the given schema."""
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(path), isolation_level=None)
    if wal:
        conn.execute("PRAGMA journal_mode=WAL")
    if schema:
        conn.executescript(schema)
    return conn


def legacy_db(path):
    """Create a database predating the updates.label / sort_order / updated_at
    and todos.archived columns — the exact shape ensure_schema_compat fixes."""
    return new_db(path, schema="""
        CREATE TABLE tags (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, color TEXT);
        CREATE TABLE projects (
            id TEXT PRIMARY KEY, title TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '', sort_order REAL NOT NULL DEFAULT 0.0,
            archived INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL
        );
        CREATE TABLE project_tags (
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            PRIMARY KEY (project_id, tag_id)
        );
        CREATE TABLE updates (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            body TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE todos (
            id TEXT PRIMARY KEY,
            project_id TEXT REFERENCES projects(id),
            title TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'new',
            reminder TEXT, sort_order REAL NOT NULL DEFAULT 0.0,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL
        );
        CREATE TABLE references_table (
            id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL DEFAULT '',
            sort_order REAL NOT NULL DEFAULT 0.0,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL
        );
        CREATE TABLE reference_tags (
            reference_id TEXT NOT NULL REFERENCES references_table(id) ON DELETE CASCADE,
            tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            PRIMARY KEY (reference_id, tag_id)
        );
    """)


def crash_db(db, sql):
    """Create a database whose committed state lives **only in its -wal**.

    Simulates a `ptm` session that was killed (or is still running): `sql` is
    executed on a WAL connection with autocheckpoint disabled, the three files
    are captured while the writer is live, the writer is closed (which would
    checkpoint), and the captured files are written back.

    Nothing may open the database after this returns — any read checkpoints the
    WAL and destroys the state.  `wal_size(db)` confirms the precondition.
    """
    db = Path(db)
    db.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(db), isolation_level=None)
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA wal_autocheckpoint=0")
    conn.executescript(sql)
    captured = [(db, db.read_bytes())]
    for suffix in ("-wal", "-shm"):
        side = Path(str(db) + suffix)
        captured.append((side, side.read_bytes()))
    conn.close()
    for path, data in captured:
        path.write_bytes(data)
    assert wal_size(db) > 0, "expected a non-empty WAL"
    return db


def data_dir(root, name="ptm"):
    """Return a fresh ptm data dir under `root`, creating it."""
    d = Path(root) / name
    d.mkdir(parents=True, exist_ok=True)
    return d


def binary(name="ptm-sync", profile="debug"):
    """Locate a built binary, erroring with a build hint if absent."""
    p = REPO / "target" / profile / name
    if not p.exists():
        raise SystemExit(
            f"missing {p}\n  build it first:  cargo build --bin {name}"
        )
    return p


def make_key(path, mode=0o600):
    """Write a 32-byte AES key file with the given mode."""
    Path(path).write_bytes(os.urandom(32))
    os.chmod(path, mode)
    return Path(path)


def run(cmd, env=None, timeout=60, check=False):
    """Run a command with an isolated environment; return CompletedProcess."""
    e = dict(os.environ)
    e.update(env or {})
    cp = subprocess.run(
        [str(c) for c in cmd], env=e, capture_output=True, text=True, timeout=timeout
    )
    if check and cp.returncode != 0:
        raise SystemExit(
            f"command failed ({cp.returncode}): {' '.join(str(c) for c in cmd)}\n"
            f"stdout: {cp.stdout}\nstderr: {cp.stderr}"
        )
    return cp


class TempWorkspace:
    """A temporary working directory that is always removed.

    Use this instead of calling `tempfile.mkdtemp()` directly: harnesses are
    re-run often, and unremoved directories accumulate in $TMPDIR.

        with TempWorkspace("ptm-wal-safety-") as root:
            ...

    Set PTM_KEEP_TEMP=1 to keep the directory for post-mortem inspection.
    """

    def __init__(self, prefix):
        self.prefix = prefix
        self.path = None

    def __enter__(self):
        self.path = Path(tempfile.mkdtemp(prefix=self.prefix))
        return self.path

    def __exit__(self, *exc):
        if os.environ.get("PTM_KEEP_TEMP"):
            print(f"\n[PTM_KEEP_TEMP] keeping {self.path}")
            return False
        shutil.rmtree(self.path, ignore_errors=True)
        return False


class Holder:
    """A separate OS process that holds the database open, mimicking a running
    `ptm` session.  SQLite's fcntl locks are per-process, so a thread cannot
    stand in for this — it has to be a real child process.

    Usage:
        with Holder(db_path) as h:
            h.wait_ready()
            ...
    """

    def __init__(self, db_path, mode="idle", timeout=15, workdir=None):
        self.db_path = str(db_path)
        self.mode = mode  # "idle" | "reader" | "writer"
        # Put the ready-file beside the database so it is cleaned up with the
        # workspace, rather than in an orphaned mkdtemp() directory.
        base = Path(workdir) if workdir else Path(self.db_path).parent
        self.ready = base / f".holder-ready-{os.getpid()}-{id(self)}"
        self.timeout = timeout
        self.proc = None

    def _script(self):
        db, ready, mode = self.db_path, str(self.ready), self.mode
        return f'''
import sqlite3, time
c = sqlite3.connect({db!r}, isolation_level=None)
c.execute("PRAGMA journal_mode=WAL")
mode = {mode!r}
if mode == "reader":
    c.execute("BEGIN")
    c.execute("SELECT count(*) FROM sqlite_master").fetchall()
elif mode == "writer":
    c.execute("BEGIN")
    c.execute("CREATE TABLE IF NOT EXISTS holder_note (x)")
    c.execute("INSERT INTO holder_note VALUES (1)")
    c.execute("COMMIT")
else:
    c.execute("SELECT count(*) FROM sqlite_master").fetchall()
open({ready!r}, "w").write("1")
time.sleep(120)
'''

    def __enter__(self):
        self.proc = subprocess.Popen(["python3", "-c", self._script()])
        return self

    def wait_ready(self):
        deadline = time.time() + self.timeout
        while not self.ready.exists():
            if time.time() > deadline:
                raise SystemExit("holder process never became ready")
            time.sleep(0.05)
        return self

    def __exit__(self, *exc):
        if self.proc:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        try:
            self.ready.unlink()
        except OSError:
            pass
        return False


def rows(path, sql, params=()):
    conn = sqlite3.connect(str(path))
    try:
        return conn.execute(sql, params).fetchall()
    finally:
        conn.close()


def integrity(path):
    """Return PRAGMA integrity_check output, or the exception text."""
    try:
        conn = sqlite3.connect(str(path))
        try:
            return conn.execute("PRAGMA integrity_check").fetchone()[0]
        finally:
            conn.close()
    except Exception as exc:  # noqa: BLE001 - reported verbatim
        return f"{type(exc).__name__}: {exc}"


def wal_size(db_path):
    w = Path(str(db_path) + "-wal")
    return w.stat().st_size if w.exists() else 0


class Results:
    """Tiny pass/fail accumulator so each harness prints a summary and exits
    non-zero on failure — usable directly as a regression check."""

    def __init__(self, title):
        self.title = title
        self.failures = 0
        self.checks = 0

    def check(self, ok, label, detail=""):
        self.checks += 1
        if ok:
            print(f"  PASS  {label}")
        else:
            self.failures += 1
            print(f"  FAIL  {label}")
            if detail:
                for line in str(detail).rstrip().splitlines():
                    print(f"          {line}")
        return ok

    def finish(self):
        print(f"\n{self.title}: {self.checks - self.failures}/{self.checks} passed")
        return 1 if self.failures else 0
