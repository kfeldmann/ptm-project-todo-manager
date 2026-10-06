#!/usr/bin/env python3
"""
check_schema.py — Validate ptm SQLite database(s) against the current schema.

Checks for each database:
  • All expected tables present (and no unexpected ones)
  • Every column: name, type, NOT NULL flag, default value, PK position
  • Foreign key ON DELETE actions (critical: todos.project_id must be CASCADE)
  • Known data-corruption pattern: todos.archived must be INTEGER, not TEXT

Usage:
    python3 check_schema.py path/to/ptm.db [path2.db ...]

Exit codes:
    0 — every database matches the expected schema
    1 — one or more differences found
    2 — bad arguments
"""

import os
import sqlite3
import sys
from typing import Dict, List, NamedTuple, Optional


# ──────────────────────────────────────────────────────────────────────────────
# Expected schema
# Derived from the SCHEMA const and migration history in src/data/db.rs
# ──────────────────────────────────────────────────────────────────────────────

class Column(NamedTuple):
    name: str
    type: str                    # declared SQLite type
    notnull: int                 # 1 = explicitly NOT NULL; 0 = nullable / PK-implied
    dflt_value: Optional[str]   # raw default string as stored by SQLite, or None
    pk: int                      # 1-based PK position; 0 = not a key column


class FK(NamedTuple):
    from_col: str
    ref_table: str
    ref_col: str
    on_delete: str               # CASCADE | SET NULL | NO ACTION | RESTRICT


# Column order doesn't affect functionality in SQLite, so we compare by name.
# notnull notes:
#   - "TEXT PRIMARY KEY" without explicit NOT NULL → notnull=0 in PRAGMA table_info
#   - Composite-PK join tables declare NOT NULL explicitly → notnull=1
EXPECTED_COLUMNS: Dict[str, List[Column]] = {
    "tags": [
        Column("id",    "TEXT",    0, None,  1),
        Column("name",  "TEXT",    1, None,  0),   # NOT NULL UNIQUE
        Column("color", "TEXT",    0, None,  0),
    ],
    "projects": [
        Column("id",          "TEXT",    0, None,  1),
        Column("title",       "TEXT",    1, None,  0),
        Column("description", "TEXT",    1, "''",  0),
        Column("sort_order",  "REAL",    1, "0.0", 0),
        Column("archived",    "INTEGER", 1, "0",   0),
        Column("created_at",  "TEXT",    1, None,  0),
        Column("updated_at",  "TEXT",    1, None,  0),
    ],
    "project_tags": [
        Column("project_id", "TEXT", 1, None, 1),
        Column("tag_id",     "TEXT", 1, None, 2),
    ],
    "updates": [
        # label, sort_order, updated_at were added via ALTER TABLE in older DBs.
        # All three must be present regardless of how they were added.
        Column("id",         "TEXT", 0, None,  1),
        Column("project_id", "TEXT", 1, None,  0),
        Column("label",      "TEXT", 1, "''",  0),
        Column("body",       "TEXT", 1, None,  0),
        Column("sort_order", "REAL", 1, "0.0", 0),
        Column("created_at", "TEXT", 1, None,  0),
        Column("updated_at", "TEXT", 1, "''",  0),
    ],
    "todos": [
        # project_id is intentionally nullable (inbox todos have no project).
        # archived was added via ALTER TABLE; then the whole table was recreated
        # to change ON DELETE SET NULL → CASCADE on project_id.
        Column("id",         "TEXT",    0, None,    1),
        Column("project_id", "TEXT",    0, None,    0),
        Column("title",      "TEXT",    1, None,    0),
        Column("status",     "TEXT",    1, "'new'", 0),
        Column("reminder",   "TEXT",    0, None,    0),
        Column("sort_order", "REAL",    1, "0.0",   0),
        Column("archived",   "INTEGER", 1, "0",     0),
        Column("created_at", "TEXT",    1, None,    0),
        Column("updated_at", "TEXT",    1, None,    0),
    ],
    "references_table": [
        Column("id",         "TEXT", 0, None,  1),
        Column("title",      "TEXT", 1, None,  0),
        Column("body",       "TEXT", 1, "''",  0),
        Column("sort_order", "REAL", 1, "0.0", 0),
        Column("created_at", "TEXT", 1, None,  0),
        Column("updated_at", "TEXT", 1, None,  0),
    ],
    "reference_tags": [
        Column("reference_id", "TEXT", 1, None, 1),
        Column("tag_id",       "TEXT", 1, None, 2),
    ],
}

EXPECTED_FKS: Dict[str, List[FK]] = {
    "project_tags": [
        FK("project_id", "projects", "id", "CASCADE"),
        FK("tag_id",     "tags",     "id", "CASCADE"),
    ],
    "updates": [
        FK("project_id", "projects", "id", "CASCADE"),
    ],
    "todos": [
        # This was the key migration: ON DELETE SET NULL → ON DELETE CASCADE.
        FK("project_id", "projects", "id", "CASCADE"),
    ],
    "reference_tags": [
        FK("reference_id", "references_table", "id", "CASCADE"),
        FK("tag_id",       "tags",             "id", "CASCADE"),
    ],
}


# ──────────────────────────────────────────────────────────────────────────────
# Checking logic
# ──────────────────────────────────────────────────────────────────────────────

def check_database(db_path: str) -> List[str]:
    """Return a list of human-readable difference strings. Empty = schema OK."""
    if not os.path.isfile(db_path):
        return [f"File not found: {db_path}"]

    diffs: List[str] = []

    try:
        conn = sqlite3.connect(db_path)
        conn.row_factory = sqlite3.Row
        # Prevent any accidental writes for the duration of this connection.
        conn.execute("PRAGMA query_only = ON;")
    except sqlite3.OperationalError as exc:
        return [f"Cannot open: {exc}"]

    try:
        _check_tables(conn, diffs)
        for table in sorted(set(EXPECTED_COLUMNS) & _get_tables(conn)):
            _check_columns(conn, table, diffs)
            if table in EXPECTED_FKS:
                _check_fks(conn, table, diffs)
        _check_data_integrity(conn, diffs)
    finally:
        conn.close()

    return diffs


def _get_tables(conn: sqlite3.Connection):
    cur = conn.execute(
        "SELECT name FROM sqlite_master "
        "WHERE type='table' AND name NOT LIKE 'sqlite_%'"
    )
    return {row["name"] for row in cur.fetchall()}


def _check_tables(conn: sqlite3.Connection, diffs: List[str]) -> None:
    actual = _get_tables(conn)
    expected = set(EXPECTED_COLUMNS)
    for t in sorted(expected - actual):
        diffs.append(f"[tables]  MISSING    {t}")
    for t in sorted(actual - expected):
        diffs.append(f"[tables]  UNEXPECTED {t}")


def _check_columns(conn: sqlite3.Connection, table: str, diffs: List[str]) -> None:
    cur = conn.execute(f"PRAGMA table_info('{table}')")
    actual: Dict[str, sqlite3.Row] = {row["name"]: row for row in cur.fetchall()}
    expected: Dict[str, Column] = {col.name: col for col in EXPECTED_COLUMNS[table]}

    for col_name in sorted(set(expected) - set(actual)):
        diffs.append(f"[{table}]  MISSING column    '{col_name}'")

    for col_name in sorted(set(actual) - set(expected)):
        diffs.append(f"[{table}]  UNEXPECTED column '{col_name}'")

    for col_name in sorted(set(expected) & set(actual)):
        exp = expected[col_name]
        act = actual[col_name]

        if act["type"].upper() != exp.type.upper():
            diffs.append(
                f"[{table}.{col_name}]  type:    "
                f"got '{act['type']}', want '{exp.type}'"
            )
        if act["notnull"] != exp.notnull:
            got_s  = "NOT NULL" if act["notnull"] else "nullable"
            want_s = "NOT NULL" if exp.notnull      else "nullable"
            diffs.append(
                f"[{table}.{col_name}]  notnull: got {got_s}, want {want_s}"
            )
        if act["dflt_value"] != exp.dflt_value:
            diffs.append(
                f"[{table}.{col_name}]  default: "
                f"got {act['dflt_value']!r}, want {exp.dflt_value!r}"
            )
        if act["pk"] != exp.pk:
            diffs.append(
                f"[{table}.{col_name}]  pk pos:  got {act['pk']}, want {exp.pk}"
            )


def _check_fks(conn: sqlite3.Connection, table: str, diffs: List[str]) -> None:
    cur = conn.execute(f"PRAGMA foreign_key_list('{table}')")
    # Key by from-column; assumes at most one FK per column (true for this schema).
    actual_fks: Dict[str, sqlite3.Row] = {}
    for row in cur.fetchall():
        actual_fks[row["from"]] = row

    expected_fks: Dict[str, FK] = {fk.from_col: fk for fk in EXPECTED_FKS[table]}

    for col in sorted(set(expected_fks) - set(actual_fks)):
        exp = expected_fks[col]
        diffs.append(
            f"[{table}.{col}]  MISSING FK "
            f"→ {exp.ref_table}({exp.ref_col}) ON DELETE {exp.on_delete}"
        )
    for col in sorted(set(actual_fks) - set(expected_fks)):
        act = actual_fks[col]
        diffs.append(
            f"[{table}.{col}]  UNEXPECTED FK "
            f"→ {act['table']}({act['to']}) ON DELETE {act['on_delete']}"
        )
    for col in sorted(set(expected_fks) & set(actual_fks)):
        exp = expected_fks[col]
        act = actual_fks[col]
        if act["table"] != exp.ref_table:
            diffs.append(
                f"[{table}.{col}]  FK ref table:  "
                f"got '{act['table']}', want '{exp.ref_table}'"
            )
        if act["to"] != exp.ref_col:
            diffs.append(
                f"[{table}.{col}]  FK ref column: "
                f"got '{act['to']}', want '{exp.ref_col}'"
            )
        if act["on_delete"].upper() != exp.on_delete.upper():
            diffs.append(
                f"[{table}.{col}]  FK ON DELETE:  "
                f"got '{act['on_delete']}', want '{exp.on_delete}'"
            )


def _check_data_integrity(conn: sqlite3.Connection, diffs: List[str]) -> None:
    """Check for the column-rotation data corruption the migration code repairs.

    A previous migration bug caused todos.archived to receive TEXT timestamp
    values instead of INTEGER 0/1.  If any such rows remain, the repair code
    in db.rs is still needed.
    """
    try:
        cur = conn.execute(
            "SELECT COUNT(*) FROM todos WHERE typeof(archived) = 'text'"
        )
        n = cur.fetchone()[0]
        if n > 0:
            diffs.append(
                f"[data/todos.archived]  {n} row(s) have TEXT value instead of "
                f"INTEGER 0/1 — column-rotation corruption not yet repaired"
            )
    except sqlite3.Error:
        pass  # todos table absence is already reported by _check_tables


# ──────────────────────────────────────────────────────────────────────────────
# Entry point
# ──────────────────────────────────────────────────────────────────────────────

def main() -> int:
    if len(sys.argv) < 2:
        print(
            "Usage: python3 check_schema.py path/to/ptm.db [path2.db ...]\n\n"
            "Compares each database against the current ptm schema and prints any\n"
            "differences.  Exits 0 if all match, 1 if any differences are found.",
            file=sys.stderr,
        )
        return 2

    all_clean = True
    for db_path in sys.argv[1:]:
        bar = "═" * 64
        print(f"\n{bar}")
        print(f"  {db_path}")
        print(bar)

        diffs = check_database(db_path)

        if not diffs:
            print("  ✓  Schema matches exactly — no differences found.")
        else:
            all_clean = False
            print(f"  ✗  {len(diffs)} difference(s) found:\n")
            for d in diffs:
                print(f"     •  {d}")

    print()
    return 0 if all_clean else 1


if __name__ == "__main__":
    sys.exit(main())
