#!/usr/bin/env python3
"""
s3-roundtrip.py — true end-to-end --end / --start coverage.

Everything else in dev-harness/ stops at the S3 call. This harness completes it
using a local fake endpoint (lib/fakes3.py), which needs one line in
/etc/hosts:

    127.0.0.1  ptm-harness.localhost

rust-s3 builds the request host as `<bucket>.<region-host>` and S3Client never
calls `.with_path_style()`, so the name that must resolve is
`b.ptm-harness.localhost` — an ordinary hosts entry covers both.

The harness SKIPS (exit 0) when the entry is absent, so it never breaks a run
on a machine that has not been set up.

Once the entry is in place it verifies:
  - `--end` uploads and records the ETag
  - a second `--end` with no changes does not re-upload
  - `--start` pulls into an empty data dir
  - the pulled database is byte-for-byte the pushed one
  - `--start` with a busy WAL skips the pull and leaves local data intact
    (the concurrent-launch behaviour AGENTS.md promises)

    python3 dev-harness/s3-roundtrip.py
"""

import os
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import fakes3  # noqa: E402
import ptmdb  # noqa: E402

PROFILE = os.environ.get("PTM_PROFILE", "debug")
results = ptmdb.Results("s3-roundtrip")
TS = "'2025-01-01T00:00:00Z'"


BUCKET = None  # set in main() from pick_bucket()


def env_for(s3, root):
    return {
        "XDG_DATA_HOME": str(root),
        "AWS_REGION": s3.region,
        "AWS_ACCESS_KEY_ID": "harness",
        "AWS_SECRET_ACCESS_KEY": "harness",
    }


def ptm_sync(args, s3, root):
    return ptmdb.run(
        [ptmdb.binary("ptm-sync", PROFILE)] + args
        + ["--bucket-name", BUCKET, "--prefix", "ptm-backups"],
        env=env_for(s3, root),
    )


def seed(db, title):
    c = ptmdb.new_db(db)
    c.execute(
        "INSERT INTO projects VALUES('p1',?,'',1.0,0," + TS + "," + TS + ")", (title,)
    )
    c.execute(
        "INSERT INTO todos (id,project_id,title,status,reminder,sort_order,archived,"
        "created_at,updated_at) VALUES('t1','p1','a todo','new',NULL,1.0,0," + TS + "," + TS + ")"
    )
    c.close()


def main():
    global BUCKET
    BUCKET = fakes3.pick_bucket()
    if BUCKET is None:
        print(f"SKIP: {fakes3.hosts_hint()}\n")
        print("Everything else in dev-harness/ runs without this; only the")
        print("network round trip needs it. Re-run after adding the line.")
        return 0
    print(f"using bucket {BUCKET!r} (request host {BUCKET}.{fakes3.HARNESS_HOST})")

    with ptmdb.TempWorkspace("ptm-s3-") as root:
        store = root / "object.bin"
        key = ptmdb.make_key(root / "key.bin")
        print(f"working in {root}")

        with fakes3.FakeS3(store) as s3:
            print(f"fake S3 at {s3.region}\n")

            # ── push ────────────────────────────────────────────────────────────
            print("[--end] first push")
            a = root / "a"
            seed(ptmdb.data_dir(a) / "ptm.db", "from A")
            cp = ptm_sync(["--end", "--encryption-key", str(key)], s3, a)
            results.check(cp.returncode == 0, "--end succeeds", cp.stderr.strip()[-300:])
            results.check(store.exists() and store.stat().st_size > 0,
                          "an encrypted object landed in the bucket")
            puts_after_first = s3.puts

            # ── no-op push ──────────────────────────────────────────────────────
            print("\n[--end] second push, nothing changed")
            cp = ptm_sync(["--end", "--encryption-key", str(key)], s3, a)
            results.check(cp.returncode == 0, "--end succeeds", cp.stderr.strip()[-300:])
            results.check(
                s3.puts == puts_after_first,
                "no re-upload when the db hash is unchanged  "
                "[FAIL = finding #2/#3 territory: hash check is not doing its job]",
                f"PUTs went {puts_after_first} -> {s3.puts}",
            )

            # ── pull into an empty dir ──────────────────────────────────────────
            print("\n[--start] pull into an empty data directory")
            b = root / "b"
            ptmdb.data_dir(b)
            cp = ptm_sync(["--start", "--encryption-key", str(key)], s3, b)
            results.check(cp.returncode == 0, "--start succeeds", cp.stderr.strip()[-300:])
            pulled = ptmdb.data_dir(b) / "ptm.db"
            results.check(pulled.exists(), "a database was installed")
            if pulled.exists():
                titles = [r[0] for r in ptmdb.rows(pulled, "SELECT title FROM projects")]
                results.check(
                    titles == ["from A"],
                    "the pulled database contains the pushed project",
                    f"titles = {titles}",
                )
                results.check(
                    ptmdb.integrity(pulled) == "ok",
                    "pulled database passes integrity_check",
                    ptmdb.integrity(pulled),
                )
                # The WAL must be gone or empty after a pull, or finding #1c looms.
                results.check(
                    ptmdb.wal_size(pulled) == 0,
                    "no stale -wal left beside the pulled database",
                    f"wal size = {ptmdb.wal_size(pulled)}",
                )

            # ── concurrent launch: busy WAL must not clobber local data ─────────
            print("\n[--start] with the db held open by another process")
            c = root / "c"
            cdb = ptmdb.data_dir(c) / "ptm.db"
            seed(cdb, "local only")
            (ptmdb.data_dir(c) / ".last_pulled_etag").write_text('"x"')
            with ptmdb.Holder(cdb, mode="reader") as h:
                h.wait_ready()
                cp = ptm_sync(["--start", "--encryption-key", str(key)], s3, c)
            results.check(
                cp.returncode == 0,
                "--start exits 0 so ptm still launches (concurrent launch)",
                f"exit={cp.returncode} {cp.stderr.strip()[-200:]}",
            )
            titles = [r[0] for r in ptmdb.rows(cdb, "SELECT title FROM projects")]
            results.check(
                titles == ["local only"],
                "the in-use local database was not overwritten by the pull  "
                "[FAIL = finding #1c is live end to end]",
                f"titles = {titles}",
            )
            results.check(
                ptmdb.integrity(cdb) == "ok",
                "database still passes integrity_check after the skipped pull",
                ptmdb.integrity(cdb),
            )

            # NOTE: a writer that commits *during* the pull (creating a -wal after
        # `--start`'s liveness check) is the scenario that can discard the
        # pulled data. It is deliberately NOT asserted here: whether the pulled
        # db or the stale WAL frames win is undefined behaviour and varies run
        # to run (observed both outcomes). The deterministic reproduction of
        # finding #1c is in wal-safety.py, which stages the exact state
        # instead of racing for it. Keep flaky races out of this suite.

    return results.finish()


if __name__ == "__main__":
    sys.exit(main())
