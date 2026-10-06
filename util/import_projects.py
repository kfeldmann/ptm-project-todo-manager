#!/usr/bin/env python3
"""
import_projects.py — Bulk-import projects from a YAML file into the ptm SQLite database.

Requirements:
    pip install pyyaml

Usage:
    python3 import_projects.py projects.yaml
    python3 import_projects.py projects.yaml --db /path/to/ptm.db
    python3 import_projects.py projects.yaml --dry-run
    python3 import_projects.py projects.yaml --skip-errors

YAML format:
    projects:
    - title: My Project           # required
      tags:
      - Tag 1
      - Tag 2
      description: |
        Description text.
      updates:
      - "[2024-01-10] Update body"   # label is optional
      - Update with no label (label defaults to today's date)
      - |
        [2024-01-11] Longer update
        spanning multiple lines
      todos:
      - "Todo title [2024-06-01]"  # trailing [YYYY-MM-DD] becomes the reminder
      - Todo without a reminder
"""
from __future__ import annotations

import argparse
import os
import re
import sqlite3
import sys
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import List, Optional

try:
    import yaml
except ImportError:
    sys.exit("PyYAML is required.  Install it with:  pip install pyyaml")


# ─── Schema (mirrors src/data/db.rs) ─────────────────────────────────────────

_SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS tags (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    color       TEXT
);
CREATE TABLE IF NOT EXISTS projects (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    sort_order  REAL NOT NULL DEFAULT 0.0,
    archived    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS project_tags (
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    tag_id      TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (project_id, tag_id)
);
CREATE TABLE IF NOT EXISTS updates (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    label       TEXT NOT NULL DEFAULT '',
    body        TEXT NOT NULL,
    sort_order  REAL NOT NULL DEFAULT 0.0,
    created_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS todos (
    id          TEXT PRIMARY KEY,
    project_id  TEXT REFERENCES projects(id) ON DELETE SET NULL,
    title       TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'new',
    reminder    TEXT,
    sort_order  REAL NOT NULL DEFAULT 0.0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS references_table (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL DEFAULT '',
    sort_order  REAL NOT NULL DEFAULT 0.0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS reference_tags (
    reference_id TEXT NOT NULL REFERENCES references_table(id) ON DELETE CASCADE,
    tag_id       TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (reference_id, tag_id)
);
"""

# Columns added after the initial schema via ALTER TABLE in the Rust migration.
# Each statement is attempted and silently skipped if the column already exists.
_COLUMN_MIGRATIONS = [
    "ALTER TABLE updates ADD COLUMN label TEXT NOT NULL DEFAULT '';",
    "ALTER TABLE updates ADD COLUMN sort_order REAL NOT NULL DEFAULT 0.0;",
]


# ─── Parsed data structures ───────────────────────────────────────────────────

@dataclass
class ParsedUpdate:
    label: str           # text from [label] prefix, or empty string
    body: str            # update body text


@dataclass
class ParsedTodo:
    title: str
    reminder: Optional[str]  # YYYY-MM-DD, or None


@dataclass
class ParsedProject:
    title: str
    description: str
    tags: List[str]
    updates: List[ParsedUpdate]
    todos: List[ParsedTodo]


# ─── Text parsing ─────────────────────────────────────────────────────────────

# Matches a leading [label] prefix.  Label may be empty: "[] body".
_LABEL_RE = re.compile(r'^\[([^\]]*)\]\s*(.*)', re.DOTALL)

# Matches a trailing [YYYY-MM-DD] at the end of a todo string.
_DATE_SUFFIX_RE = re.compile(r'\[(\d{4}-\d{2}-\d{2})\]\s*$')


def _parse_update_text(raw: str) -> ParsedUpdate:
    """
    Split '[label] body text' into (label, body).
    If no [label] prefix is present, label defaults to today's date (YYYY-MM-DD),
    matching the app's behaviour when creating a new update via the editor.
    """
    raw = raw.strip()
    m = _LABEL_RE.match(raw)
    if m:
        return ParsedUpdate(label=m.group(1).strip(), body=m.group(2).strip())
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    return ParsedUpdate(label=today, body=raw)


def _parse_todo_text(raw: str) -> ParsedTodo:
    """
    Split 'Title [YYYY-MM-DD]' into (title, reminder).
    The date bracket must be at the very end of the string.
    Returns reminder=None when no date is present.
    """
    raw = raw.strip()
    m = _DATE_SUFFIX_RE.search(raw)
    if m:
        return ParsedTodo(title=raw[: m.start()].strip(), reminder=m.group(1))
    return ParsedTodo(title=raw, reminder=None)


def _valid_date(s: str) -> bool:
    try:
        datetime.strptime(s, "%Y-%m-%d")
        return True
    except ValueError:
        return False


# ─── Validation ───────────────────────────────────────────────────────────────

def _require_list(value: object, field_name: str) -> list:
    if value is None:
        return []
    if not isinstance(value, list):
        raise ValueError(f"'{field_name}' must be a list, got {type(value).__name__}")
    return value


def validate_entry(data: dict, index: int) -> ParsedProject:
    """
    Validate one YAML project entry and return a ParsedProject.
    Raises ValueError with a descriptive message on any problem.
    """
    title = str(data.get("title") or "").strip()
    if not title:
        raise ValueError("missing required 'title' field")

    description = str(data.get("description") or "").strip()

    # Tags
    tags: List[str] = []
    for j, raw_tag in enumerate(_require_list(data.get("tags"), "tags")):
        t = str(raw_tag).strip()
        if not t:
            raise ValueError(f"tags[{j}] is empty")
        tags.append(t)

    # Updates
    updates: List[ParsedUpdate] = []
    for j, raw_upd in enumerate(_require_list(data.get("updates"), "updates")):
        u = _parse_update_text(str(raw_upd))
        if not u.body:
            raise ValueError(f"updates[{j}] has an empty body")
        updates.append(u)

    # Todos
    todos: List[ParsedTodo] = []
    for j, raw_todo in enumerate(_require_list(data.get("todos"), "todos")):
        t = _parse_todo_text(str(raw_todo))
        if not t.title:
            raise ValueError(f"todos[{j}] has an empty title")
        if t.reminder is not None and not _valid_date(t.reminder):
            raise ValueError(f"todos[{j}] has an invalid date: '{t.reminder}'")
        todos.append(t)

    return ParsedProject(
        title=title,
        description=description,
        tags=tags,
        updates=updates,
        todos=todos,
    )


# ─── Database helpers ─────────────────────────────────────────────────────────

def _now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _uid() -> str:
    return str(uuid.uuid4())


def open_db(path: Path) -> sqlite3.Connection:
    """Open (or create) the ptm database, run the schema and column migrations."""
    path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(path))
    conn.execute("PRAGMA journal_mode=WAL;")
    conn.execute("PRAGMA foreign_keys=ON;")
    # executescript commits any pending transaction first, then runs DDL.
    conn.executescript(_SCHEMA_SQL)
    for stmt in _COLUMN_MIGRATIONS:
        try:
            conn.execute(stmt)
            conn.commit()
        except sqlite3.OperationalError as exc:
            if "duplicate column name" not in str(exc):
                raise
    return conn


def _get_or_create_tag(conn: sqlite3.Connection, name: str) -> str:
    """Return the tag id for name (case-insensitive), creating the row if absent."""
    row = conn.execute(
        "SELECT id FROM tags WHERE LOWER(name) = LOWER(?)", (name,)
    ).fetchone()
    if row:
        return row[0]
    tid = _uid()
    conn.execute("INSERT INTO tags (id, name) VALUES (?, ?)", (tid, name))
    return tid


def insert_project(conn: sqlite3.Connection, p: ParsedProject, sort_order: float) -> None:
    """Insert a single project and all its children into the database."""
    now = _now()
    pid = _uid()

    conn.execute(
        """INSERT INTO projects
               (id, title, description, sort_order, archived, created_at, updated_at)
           VALUES (?, ?, ?, ?, 0, ?, ?)""",
        (pid, p.title, p.description, sort_order, now, now),
    )

    for tag_name in p.tags:
        tid = _get_or_create_tag(conn, tag_name)
        conn.execute(
            "INSERT OR IGNORE INTO project_tags (project_id, tag_id) VALUES (?, ?)",
            (pid, tid),
        )

    # Updates use ascending sort_order (1.0, 2.0, …) to preserve YAML order.
    # The app displays updates in sort_order ASC, so index 0 → top of the list.
    for i, u in enumerate(p.updates, start=1):
        conn.execute(
            """INSERT INTO updates (id, project_id, label, body, sort_order, created_at)
               VALUES (?, ?, ?, ?, ?, ?)""",
            (_uid(), pid, u.label, u.body, float(i), now),
        )

    for i, t in enumerate(p.todos, start=1):
        conn.execute(
            """INSERT INTO todos
                   (id, project_id, title, status, reminder, sort_order,
                    created_at, updated_at)
               VALUES (?, ?, ?, 'new', ?, ?, ?, ?)""",
            (_uid(), pid, t.title, t.reminder, float(i), now, now),
        )


# ─── CLI ──────────────────────────────────────────────────────────────────────

def _default_db_path() -> Path:
    xdg = os.environ.get("XDG_DATA_HOME", "")
    base = Path(xdg) if xdg else Path.home() / ".local" / "share"
    return base / "ptm" / "ptm.db"


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Bulk-import projects from a YAML file into the ptm database.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("yaml_file", help="Path to the YAML input file")
    parser.add_argument(
        "--db",
        metavar="PATH",
        default=str(_default_db_path()),
        help="Path to ptm.db  (default: %(default)s)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Validate the YAML and print a summary without touching the database",
    )
    parser.add_argument(
        "--skip-errors",
        action="store_true",
        help=(
            "Skip invalid entries and import the rest.  "
            "Default: abort before writing anything if any entry is invalid."
        ),
    )
    args = parser.parse_args()

    # ── Load YAML ────────────────────────────────────────────────────────────
    yaml_path = Path(args.yaml_file)
    if not yaml_path.is_file():
        sys.exit(f"Error: '{yaml_path}' not found.")

    try:
        with yaml_path.open("r", encoding="utf-8") as fh:
            doc = yaml.safe_load(fh)
    except yaml.YAMLError as exc:
        sys.exit(f"YAML parse error:\n  {exc}")

    raw_list = (doc or {}).get("projects")
    if not raw_list or not isinstance(raw_list, list):
        sys.exit("Error: no 'projects' list found in the YAML file.")

    # ── Validate all entries (no DB needed) ──────────────────────────────────
    parsed: List[ParsedProject] = []
    validation_errors: List[str] = []

    for i, entry in enumerate(raw_list):
        if not isinstance(entry, dict):
            msg = f"projects[{i}]: expected a mapping, got {type(entry).__name__}"
            validation_errors.append(msg)
            print(f"  ✗ {msg}", file=sys.stderr)
            continue
        try:
            parsed.append(validate_entry(entry, i))
        except ValueError as exc:
            title = entry.get("title") or f"projects[{i}]"
            msg = f"'{title}': {exc}"
            validation_errors.append(msg)
            print(f"  ✗ {msg}", file=sys.stderr)

    if validation_errors and not args.skip_errors:
        sys.exit(
            f"\n{len(validation_errors)} validation error(s) found.  "
            "Fix them and re-run, or pass --skip-errors to import the valid entries only."
        )

    if not parsed:
        sys.exit("Nothing to import.")

    # ── Dry run: print summary and exit ──────────────────────────────────────
    if args.dry_run:
        print("Dry run — no changes will be written to the database.\n")
        for p in parsed:
            print(
                f"  • '{p.title}'  "
                f"tags:{len(p.tags)}  updates:{len(p.updates)}  todos:{len(p.todos)}"
            )
            if p.description:
                preview = p.description[:72].replace("\n", " ")
                ellipsis = "…" if len(p.description) > 72 else ""
                print(f"    description: {preview!r}{ellipsis}")
            for u in p.updates:
                prefix = f"[{u.label}] " if u.label else ""
                preview = u.body[:60].replace("\n", " ")
                ellipsis = "…" if len(u.body) > 60 else ""
                print(f"    update:  {prefix}{preview}{ellipsis}")
            for t in p.todos:
                reminder = f"  [{t.reminder}]" if t.reminder else ""
                print(f"    todo:    {t.title}{reminder}")
        skipped = len(raw_list) - len(parsed)
        print(
            f"\n{len(parsed)} project(s) would be imported"
            + (f", {skipped} skipped due to errors" if skipped else "")
            + "."
        )
        return

    # ── Open DB and insert (single transaction) ───────────────────────────────
    db_path = Path(args.db)
    db_existed = db_path.exists()
    conn = open_db(db_path)

    if not db_existed:
        print(f"Created new database at '{db_path}'.")

    base_order: float = conn.execute(
        "SELECT COALESCE(MAX(sort_order), 0.0) FROM projects"
    ).fetchone()[0]

    try:
        with conn:  # commits on success, rolls back on any exception
            for i, p in enumerate(parsed, start=1):
                insert_project(conn, p, sort_order=base_order + float(i))
                print(
                    f"  ✓ '{p.title}'  "
                    f"tags:{len(p.tags)}  updates:{len(p.updates)}  todos:{len(p.todos)}"
                )
    except sqlite3.Error as exc:
        sys.exit(f"\nDatabase error: {exc}\nAll changes have been rolled back.")
    finally:
        conn.close()

    skipped = len(raw_list) - len(parsed)
    print(
        f"\nDone: {len(parsed)} project(s) imported"
        + (f", {skipped} skipped due to validation errors" if skipped else "")
        + "."
    )


if __name__ == "__main__":
    main()
