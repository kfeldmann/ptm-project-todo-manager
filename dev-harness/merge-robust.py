#!/usr/bin/env python3
"""
merge-robust.py — reproductions for findings #7, #8, #9.

  7  `--merge` aborts on a legacy *local* database (only the source copy is
     brought up to date before merging)
  8  `--merge` aborts on a foreign row whose project_id dangles
  9  `sort_order` policy is wrong for `updates` (newest-first ordering means
     MAX() picks the newest row, so foreign rows land *inside* the local block
     instead of after it)

`--merge` needs no S3, so these run the real binary end to end.

    python3 dev-harness/merge-robust.py
"""

import os
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import ptmdb  # noqa: E402

PROFILE = os.environ.get("PTM_PROFILE", "debug")
results = ptmdb.Results("merge-robust")

TS = "'2025-01-01T00:00:00Z'"


def merge(local_root, src_root):
    return ptmdb.run(
        [ptmdb.binary("ptm-sync", PROFILE), "--merge", "--from", str(src_root)],
        env={"XDG_DATA_HOME": str(local_root)},
    )


def make_source(root, populate):
    src = Path(root) / "src"
    src.mkdir(parents=True, exist_ok=True)
    conn = ptmdb.new_db(src / "ptm.db")
    populate(conn)
    conn.close()
    return src


# ── #7: legacy local database ────────────────────────────────────────────────

def check_legacy_local(root):
    print("\n[7] --merge against a legacy local database")
    local = ptmdb.data_dir(root / "l7")
    conn = ptmdb.legacy_db(local / "ptm.db")
    conn.execute(
        "INSERT INTO projects VALUES('p1','Local','',1.0,0," + TS + "," + TS + ")"
    )
    conn.execute(
        "INSERT INTO updates (id,project_id,body,created_at) VALUES('u1','p1','local'," + TS + ")"
    )
    conn.close()

    src = make_source(
        root / "s7",
        lambda c: (
            c.execute("INSERT INTO projects VALUES('p2','Remote','',2.0,0," + TS + "," + TS + ")"),
            c.execute(
                "INSERT INTO updates (id,project_id,label,body,sort_order,created_at,updated_at) "
                "VALUES('u2','p2','','remote',1.0," + TS + "," + TS + ")"
            ),
        ),
    )

    cp = merge(root / "l7", src)
    results.check(
        cp.returncode == 0,
        "--merge succeeds when the LOCAL db predates the label/sort_order/updated_at columns  "
        "[FAIL = finding #7 is live]",
        f"exit={cp.returncode}\n{cp.stderr.strip()[-400:]}",
    )
    if cp.returncode == 0:
        results.check(
            len(ptmdb.rows(local / "ptm.db", "SELECT id FROM projects")) == 2,
            "both projects present after the merge",
        )


# ── #8: dangling foreign key in the source ───────────────────────────────────

def check_dangling_fk(root):
    print("\n[8] --merge with a source todo whose project was deleted")
    local = ptmdb.data_dir(root / "l8")
    c = ptmdb.new_db(local / "ptm.db")
    c.execute("INSERT INTO projects VALUES('p1','Local','',1.0,0," + TS + "," + TS + ")")
    c.close()

    def populate(conn):
        # Legacy-shaped source: todos.project_id has no ON DELETE CASCADE, so a
        # todo can survive its project. That is the situation the review cites.
        conn.execute("PRAGMA foreign_keys=OFF")
        conn.execute(
            "INSERT INTO todos (id,project_id,title,status,reminder,sort_order,archived,created_at,updated_at) "
            "VALUES('t1','ghost','orphan todo','new',NULL,1.0,0," + TS + "," + TS + ")"
        )

    src = make_source(root / "s8", populate)
    cp = merge(root / "l8", src)
    results.check(
        cp.returncode == 0,
        "--merge succeeds when a foreign todo's project_id dangles  "
        "[FAIL = finding #8 is live]",
        f"exit={cp.returncode}\n{cp.stderr.strip()[-400:]}",
    )
    if cp.returncode == 0:
        rows = ptmdb.rows(local / "ptm.db", "SELECT project_id FROM todos WHERE id='t1'")
        results.check(
            rows and rows[0][0] is None,
            "the orphaned todo was imported with project_id NULLed (Inbox)",
            f"project_id = {rows}",
        )


# ── #9: sort_order policy for updates ────────────────────────────────────────

def check_update_ordering(root):
    print("\n[9] foreign updates land inside the local block, not after it")
    local = ptmdb.data_dir(root / "l9")
    c = ptmdb.new_db(local / "ptm.db")
    c.execute("INSERT INTO projects VALUES('p1','P','',1.0,0," + TS + "," + TS + ")")
    # updates are stored newest-first: create_update uses MIN(sort_order) - 1.0,
    # so the visible order is L1 (0), L2 (-1), L3 (-2).
    for uid, title, order in [("L1", "local one", 0.0),
                              ("L2", "local two", -1.0),
                              ("L3", "local three", -2.0)]:
        c.execute(
            "INSERT INTO updates (id,project_id,label,body,sort_order,created_at,updated_at) "
            f"VALUES('{uid}','p1','','{title}',{order},{TS},{TS})"
        )
    c.close()

    def populate(conn):
        conn.execute("INSERT INTO projects VALUES('p1','P','',1.0,0," + TS + "," + TS + ")")
        for uid, title, order in [("R1", "remote one", 0.0),
                                  ("R2", "remote two", -1.0)]:
            conn.execute(
                "INSERT INTO updates (id,project_id,label,body,sort_order,created_at,updated_at) "
                f"VALUES('{uid}','p1','','{title}',{order},{TS},{TS})"
            )

    src = make_source(root / "s9", populate)
    cp = merge(root / "l9", src)
    if cp.returncode != 0:
        results.check(False, "--merge succeeded", f"exit={cp.returncode}\n{cp.stderr.strip()[-400:]}")
        return

    order = [
        r[0] for r in ptmdb.rows(
            local / "ptm.db", "SELECT id FROM updates ORDER BY sort_order ASC"
        )
    ]
    # Documented promise (merge.rs docstring): local rows keep their ranking and
    # foreign-only rows slot in *after* them.
    expected = ["L1", "L2", "L3", "R1", "R2"]
    results.check(
        order == expected,
        f"display order is {expected} (foreign rows after the local block)  "
        "[FAIL = finding #9 is live: histories are interleaved]",
        f"got {order}",
    )


def main():
    with ptmdb.TempWorkspace("ptm-merge-") as root:
        print(f"working in {root}")
        print(f"binary profile: {PROFILE}")
        check_legacy_local(root)
        check_dangling_fk(root)
        check_update_ordering(root)
        return results.finish()


if __name__ == "__main__":
    sys.exit(main())
