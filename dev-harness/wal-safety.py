#!/usr/bin/env python3
"""
wal-safety.py — reproductions for findings #1 (a/b/c) and #2.

Covers the four ways `ptm-sync` can destroy or silently fail to back up data:

  1a  WAL liveness probe is unsound  — does not reproduce as the review
      describes; `BEGIN EXCLUSIVE` does fail while another process has the
      database open.  The checks here pin the behaviour that must hold: the
      probe must not report a live database as free, and it must wait for the
      busy timeout instead of failing instantly.
  1b  `wal_checkpoint(TRUNCATE)`'s `(busy, log, checkpointed)` row is ignored,
      so a checkpoint that could not advance is reported as success.
  1c  the pulled database is renamed over a live db (non-atomic install).
  2   `--end` packs the main db file while committed data sits in the WAL,
      and a push that could not run still exits 0.

Each check states which finding it belongs to and what FAIL means.

The 1b/1c/2 checks run the **real binary** against a database whose committed
state lives only in `ptm.db-wal` (see `ptmdb.crash_db`) — the state a crashed
`ptm` leaves behind.  They assert on what `ptm-sync` left on disk, because a fix
that only makes the SQLite mechanics true has not shipped anything.

Requires the /etc/hosts entry documented in README.md; without it the harness
prints SKIP and exits 0.

    python3 dev-harness/wal-safety.py
"""

import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import fakes3  # noqa: E402
import ptmdb  # noqa: E402

PROFILE = os.environ.get("PTM_PROFILE", "debug")
results = ptmdb.Results("wal-safety")
TS = "'2025-01-01T00:00:00Z'"

BUCKET = None  # set in main()
S3 = None      # the running FakeS3, set in main()
KEY = None     # one key for the whole run, set in main()


def ptm_sync(args, root):
    """Run ptm-sync with an isolated data dir and the fake S3 endpoint.

    `root` is the XDG_DATA_HOME value, so the data dir is `<root>/ptm`.
    """
    return ptmdb.run(
        [ptmdb.binary("ptm-sync", PROFILE)] + args
        + ["--bucket-name", BUCKET, "--prefix", "ptm-backups",
           "--encryption-key", str(KEY)],
        env={
            "XDG_DATA_HOME": str(root),
            "AWS_REGION": S3.region,
            "AWS_ACCESS_KEY_ID": "harness",
            "AWS_SECRET_ACCESS_KEY": "harness",
        },
    )


def crash_db_with_rows(db, rows):
    """A database whose only copy of `rows` todos is in its WAL."""
    ptmdb.crash_db(
        db,
        "CREATE TABLE todos (id TEXT, title TEXT);"
        + "".join(f"INSERT INTO todos VALUES ('{i}', '{t}');" for i, t in rows),
    )


def seed_bucket(rows):
    """Push a backup containing exactly `rows` (id, title) todos."""
    with ptmdb.TempWorkspace("ptm-wal-seed-") as ws:
        db = ptmdb.data_dir(ws) / "ptm.db"
        conn = ptmdb.new_db(db)
        conn.executemany(
            "INSERT INTO todos (id,project_id,title,status,reminder,sort_order,"
            "archived,created_at,updated_at) VALUES(?,NULL,?,'new',NULL,1.0,0,"
            + TS + "," + TS + ")",
            rows,
        )
        conn.close()
        cp = ptm_sync(["--end", "--local-is-master"], ws)
        results.check(cp.returncode == 0, "seeded the bucket", cp.stderr.strip()[-300:])


# ── 1a: the liveness probe ────────────────────────────────────────────────────

def check_probe_waits_and_refuses(root):
    print("\n[1a] the probe must not report a live database as free")
    dd = ptmdb.data_dir(root)
    db = dd / "ptm.db"
    conn = ptmdb.new_db(db, schema=None)
    conn.execute("CREATE TABLE t(x)")
    conn.execute("INSERT INTO t VALUES(1)")
    conn.close()

    # mode="writer" commits a row, so the database really has a WAL and the
    # push has to go through the liveness probe.
    with ptmdb.Holder(db, mode="writer") as h:
        h.wait_ready()
        assert ptmdb.wal_size(db) > 0, "precondition: the holder left a WAL"
        started = time.time()
        cp = ptm_sync(["--end"], root)
        elapsed = time.time() - started

    results.check(
        cp.returncode != 0,
        "a push against a held-open database does not report success",
        f"exit={cp.returncode} stderr={cp.stderr.strip()[:200]}",
    )
    results.check(
        elapsed >= 4.0,
        "the probe waits for busy_timeout instead of failing instantly  "
        "[FAIL = busy_timeout is unset, so 'busy' and 'broken' are indistinguishable]",
        f"elapsed = {elapsed:.1f}s (expected >= 4s)",
    )


# ── 1b: the checkpoint's (busy, log, checkpointed) row ───────────────────────

def check_checkpoint_reports_busy(root):
    print("\n[1b] wal_checkpoint(TRUNCATE) reports a busy checkpoint as a result row")
    db = Path(root) / "ckpt.db"
    conn = ptmdb.new_db(db, schema=None)
    conn.execute("CREATE TABLE t(x)")
    conn.execute("INSERT INTO t VALUES('seed')")
    conn.execute("BEGIN")
    for i in range(50):
        conn.execute("INSERT INTO t VALUES(?)", (i,))
    conn.execute("COMMIT")

    # A reader holding a read mark makes the checkpoint busy.
    with ptmdb.Holder(db, mode="reader") as h:
        h.wait_ready()
        reader = ptmdb.new_db(db, schema=None)
        busy, log, done = reader.execute(
            "PRAGMA wal_checkpoint(TRUNCATE)"
        ).fetchone()
        reader.close()

    results.check(
        busy != 0,
        "checkpoint reports busy != 0 while a reader pins a snapshot",
        f"got (busy={busy}, log={log}, checkpointed={done})",
    )
    results.check(
        ptmdb.wal_size(db) > 0,
        "WAL is left non-empty after the busy checkpoint (so 'checkpointed' is a lie)",
        f"wal size = {ptmdb.wal_size(db)}",
    )
    conn.close()


def check_end_empties_the_wal(root):
    print("\n[1b] --end leaves no frames behind in the WAL")
    dd = ptmdb.data_dir(root)
    db = dd / "ptm.db"
    crash_db_with_rows(db, [(f"t{i}", f"todo {i}") for i in range(30)])

    cp = ptm_sync(["--end", "--dry-run"], root)
    results.check(cp.returncode == 0, "--end --dry-run succeeds", cp.stderr.strip()[-300:])
    results.check(
        ptmdb.wal_size(db) == 0,
        "the WAL is empty after --end  [FAIL = finding #1b is live: the "
        "checkpoint's result is still being ignored]",
        f"wal size = {ptmdb.wal_size(db)}",
    )


# ── 2: --end packs a snapshot that is missing everything in the WAL ──────────

def check_end_uploads_complete_snapshot(root):
    print("\n[2] the uploaded backup contains the rows that were in the WAL")
    with ptmdb.TempWorkspace("ptm-wal-push-") as push:
        db = ptmdb.data_dir(push) / "ptm.db"
        crash_db_with_rows(db, [(f"t{i}", f"todo {i}") for i in range(50)])

        # The raw main file on its own really is empty — this is what --end used
        # to pack.  Reading a *copy* cannot checkpoint the original's WAL.
        raw = push / "raw.db"
        raw.write_bytes(db.read_bytes())
        try:
            raw_rows = ptmdb.rows(raw, "SELECT count(*) FROM todos")[0][0]
        except Exception as exc:  # noqa: BLE001
            raw_rows = f"<unreadable: {exc}>"
        results.check(
            raw_rows != 50,
            "precondition: the raw ptm.db file alone does not hold the 50 rows",
            f"raw rows = {raw_rows}",
        )

        cp = ptm_sync(["--end"], push)
        results.check(cp.returncode == 0, "--end succeeds", cp.stderr.strip()[-300:])

    # What landed in the bucket must be a complete database: pull it back into a
    # fresh data dir and count rows.
    with ptmdb.TempWorkspace("ptm-wal-pull-") as pull:
        cp = ptm_sync(["--start"], pull)
        results.check(cp.returncode == 0, "--start pulls it back", cp.stderr.strip()[-300:])
        pulled = ptmdb.data_dir(pull) / "ptm.db"
        rows = ptmdb.rows(pulled, "SELECT count(*) FROM todos")[0][0] if pulled.exists() else -1
        results.check(
            rows == 50,
            "the round-tripped database holds all 50 rows  "
            "[FAIL = finding #2 is live: --end uploaded a WAL-less snapshot]",
            f"rows = {rows} (expected 50)",
        )
        results.check(
            ptmdb.integrity(pulled) == "ok",
            "the round-tripped database passes integrity_check",
            ptmdb.integrity(pulled),
        )


# ── 1c: installing the pulled db over a live one ─────────────────────────────

def check_install_over_live_db(root):
    print("\n[1c] the pulled database survives a stale -wal beside it")
    seed_bucket([("pulled", "from backup")])

    # A local database whose committed state is only in its WAL — exactly the
    # state a crashed (or still running) ptm leaves behind.
    dd = ptmdb.data_dir(root)
    db = dd / "ptm.db"
    crash_db_with_rows(db, [(f"l{i}", f"local {i}") for i in range(30)])
    results.check(
        ptmdb.wal_size(db) > 0,
        "precondition: the local database has a live WAL at install time",
        f"wal = {ptmdb.wal_size(db)} bytes",
    )

    # --remote-is-master forces the pull even though local data exists.
    cp = ptm_sync(["--start", "--remote-is-master"], root)
    results.check(cp.returncode == 0, "--start succeeds", cp.stderr.strip()[-300:])

    titles = [r[0] for r in ptmdb.rows(db, "SELECT title FROM todos")]
    results.check(
        "from backup" in titles,
        "the pulled backup is what ends up on disk  "
        "[FAIL = finding #1c is live: the pull reported success but was discarded]",
        f"titles on disk: {titles[:6]}{' ...' if len(titles) > 6 else ''}",
    )
    results.check(
        ptmdb.wal_size(db) == 0,
        "no stale -wal survives the install",
        f"wal = {ptmdb.wal_size(db)} bytes",
    )
    results.check(
        ptmdb.integrity(db) == "ok",
        "database still passes integrity_check",
        ptmdb.integrity(db),
    )


# ── a backup that cannot be opened is never installed ───────────────────────

def check_corrupt_backup_is_refused(root):
    print("\n[2] a corrupt backup is refused and the local data survives")
    # Build an object that decrypts and unzips fine but whose ptm.db is garbage,
    # then upload it as the remote backup.  `enc` shares crypto.rs, so it can
    # produce a valid envelope around invalid content.
    import shutil
    import subprocess
    import zipfile

    bad_dir = ptmdb.TempWorkspace("ptm-wal-badzip-")
    with bad_dir as bad:
        zip_path = bad / "ptm-backup.zip"
        with zipfile.ZipFile(zip_path, "w") as zf:
            zf.writestr("ptm.db", b"this is not a sqlite database at all")
        enc = ptmdb.run([ptmdb.binary("enc", PROFILE), "encrypt", str(zip_path),
                         "--key-file", str(KEY)])
        results.check(enc.returncode == 0, "built a corrupt backup object",
                      enc.stderr.strip()[-300:])
        # Put it where S3 would serve it from.
        shutil.copy(str(zip_path) + ".enc", S3.store_path)

    dd = ptmdb.data_dir(root)
    db = dd / "ptm.db"
    conn = ptmdb.new_db(db)
    conn.execute(
        "INSERT INTO projects VALUES('keep','LOCAL-DATA','',1.0,0," + TS + "," + TS + ")"
    )
    conn.close()

    cp = ptm_sync(["--start", "--remote-is-master"], root)
    results.check(
        cp.returncode != 0,
        "--start fails loudly instead of installing a corrupt backup",
        f"exit={cp.returncode} stderr={cp.stderr.strip()[:200]}",
    )
    titles = [r[0] for r in ptmdb.rows(db, "SELECT title FROM projects")]
    results.check(
        titles == ["LOCAL-DATA"],
        "the local database is untouched  [FAIL = a corrupt backup overwrote good data]",
        f"titles = {titles}",
    )
    results.check(
        ptmdb.integrity(db) == "ok",
        "the local database still passes integrity_check",
        ptmdb.integrity(db),
    )


# ── 2 (exit code): a push that could not run ─────────────────────────────────

def check_skipped_push_exit_code(root):
    print("\n[2] a push that could not run exits non-zero")
    dd = ptmdb.data_dir(root)
    db = dd / "ptm.db"
    conn = ptmdb.new_db(db, schema=None)
    conn.execute("CREATE TABLE todos (id TEXT, title TEXT)")
    conn.execute("INSERT INTO todos VALUES('a','a')")
    conn.close()

    # mode="writer" so the holder leaves a WAL behind: the liveness check only
    # runs when one exists (see the README's "Known limitation" note).
    with ptmdb.Holder(db, mode="writer") as h:
        h.wait_ready()
        assert ptmdb.wal_size(db) > 0, "precondition: the holder left a WAL"
        cp = ptm_sync(["--end"], root)

    results.check(
        cp.returncode != 0,
        "ptm-sync --end exits non-zero when it cannot push  "
        "[FAIL = finding #2 is live: the session's work is silently unbacked-up]",
        f"exit={cp.returncode}  stderr={cp.stderr.strip()[:200]}",
    )

    # --local-is-master is about S3 authority (push regardless of the remote),
    # not about local concurrency, so it must not turn a refused push into a
    # success: the user asked for a forced push and would get nothing.
    #
    # The holder must be a *writer* here, because the liveness check only runs
    # when a WAL exists (see the README's "Known limitation" note).  An idle or
    # read-only holder leaves no WAL, and --end never reaches the check at all.
    with ptmdb.Holder(db, mode="writer") as h:
        h.wait_ready()
        assert ptmdb.wal_size(db) > 0, "precondition: the holder left a WAL"
        cp = ptm_sync(["--end", "--local-is-master"], root)
    results.check(
        cp.returncode != 0,
        "--end --local-is-master still exits non-zero when it cannot push  "
        "[FAIL = the flag is being read as 'someone else will push']",
        f"exit={cp.returncode}  stderr={cp.stderr.strip()[:200]}",
    )


def main():
    global BUCKET, S3, KEY
    BUCKET = fakes3.pick_bucket()
    if BUCKET is None:
        print(f"SKIP: {fakes3.hosts_hint()}\n")
        print("The 1a/1b/1c/2 checks drive the real binary against a fake S3")
        print("endpoint, so they need that entry. See dev-harness/README.md.")
        return 0

    with ptmdb.TempWorkspace("ptm-wal-safety-") as root:
        print(f"working in {root}")
        print(f"binary profile: {PROFILE}")
        KEY = ptmdb.make_key(root / "key.bin")
        with fakes3.FakeS3(root / "object.bin") as s3:
            S3 = s3
            print(f"fake S3 at {s3.region}")
            # Each check gets its own data dir so they cannot interfere.
            for name, fn in [
                ("1a-probe", check_probe_waits_and_refuses),
                ("1b-busy", check_checkpoint_reports_busy),
                ("1b-empty", check_end_empties_the_wal),
                ("2-snapshot", check_end_uploads_complete_snapshot),
                ("1c-install", check_install_over_live_db),
                ("corrupt-backup", check_corrupt_backup_is_refused),
                ("2-exit", check_skipped_push_exit_code),
            ]:
                with ptmdb.TempWorkspace(f"ptm-wal-{name}-") as sub:
                    fn(sub)
        return results.finish()


if __name__ == "__main__":
    sys.exit(main())
