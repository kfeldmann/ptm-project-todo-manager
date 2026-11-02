#!/usr/bin/env python3
"""
sync-state.py — reproductions for findings #3 and #12.

  3   The sync state machine reported "nothing to push" for state that had
      never been pushed.  `--merge` changed the local database outside any sync
      and then *deleted* `.db_hash` to force a push — but `--start` re-creates
      `.db_hash` from the current database whenever it is missing (it has to:
      a genuine first launch has no baseline), so the merged-but-unpushed state
      was recorded as the sync baseline and the next `--end` logged "No changes
      detected" and uploaded nothing.
  12  `--start` aborted on a network error when stdin was not a TTY, despite
      the function being named `prompt_continue_without_pull` and the
      documented behaviour being "allow concurrent launch".  A transient S3
      error therefore stopped `ptm` from starting at all when it was launched
      from a desktop launcher, a script, or a pipe.

FAIL means the finding is live.  Checks that need the S3 round trip print SKIP
when the /etc/hosts entry from README.md is missing; everything else runs
without it.

    python3 dev-harness/sync-state.py
"""

import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import fakes3  # noqa: E402
import ptmdb  # noqa: E402

PROFILE = os.environ.get("PTM_PROFILE", "debug")
results = ptmdb.Results("sync-state")
TS = "'2025-01-01T00:00:00Z'"


def ptm_sync(args, root, env=None):
    e = {"XDG_DATA_HOME": str(root)}
    e.update(env or {})
    return ptmdb.run([ptmdb.binary("ptm-sync", PROFILE)] + args, env=e)


def seed_db(db, project_id, title):
    c = ptmdb.new_db(db)
    c.execute(
        "INSERT INTO projects VALUES(?,'" + title + "','',1.0,0," + TS + "," + TS + ")",
        (project_id,),
    )
    c.close()


def merge_into(local_root, source_root, key, extra=()):
    return ptm_sync(
        ["--merge", "--from", str(source_root / "ptm"), "--encryption-key", str(key)]
        + list(extra),
        local_root,
    )


# ── #3: the merge result must reach S3 ───────────────────────────────────────

def check_merge_marks_state_for_upload(root):
    print("\n[3] --merge marks the merged state, and --start does not swallow the mark")
    root.mkdir(parents=True, exist_ok=True)
    key = ptmdb.make_key(root / "key.bin")

    # Two data directories that have diverged, as after working on two machines.
    local_root = root / "local"
    seed_db(ptmdb.data_dir(local_root) / "ptm.db", "p1", "Local project")
    source_root = root / "source"
    seed_db(ptmdb.data_dir(source_root) / "ptm.db", "p2", "Source project")

    cp = merge_into(local_root, source_root, key)
    results.check(cp.returncode == 0, "--merge succeeds", cp.stderr.strip()[-400:])

    local = ptmdb.data_dir(local_root)
    titles = [r[0] for r in ptmdb.rows(local / "ptm.db", "SELECT title FROM projects")]
    results.check(
        sorted(titles) == ["Local project", "Source project"],
        "the merge really changed the local database",
        f"titles = {titles}",
    )
    results.check(
        (local / ".needs_push").exists(),
        "the merge records that the local state is unpushed  "
        "[FAIL = finding #3 is live: nothing tells the next --end to push]",
        f"files: {sorted(p.name for p in local.iterdir())}",
    )

    # The bug: --start used to re-seed .db_hash here, making the merged state
    # look like the last synced state.
    cp = ptm_sync(
        ["--start", "--local-is-master", "--bucket-name", "b", "--prefix", "p",
         "--encryption-key", str(key)],
        local_root,
    )
    results.check(cp.returncode == 0, "--start --local-is-master succeeds", cp.stderr.strip()[-300:])
    results.check(
        (local / ".needs_push").exists(),
        "--start leaves the marker in place  "
        "[FAIL = finding #3 is live: --start cleared the pending-push state]",
        f"files: {sorted(p.name for p in local.iterdir())}",
    )

    # The end-to-end consequence, stated as the harness can see it without S3:
    # --end's change test is "marker set, or hash differs from the baseline".
    # A baseline equal to the current state with no marker means "skip upload".
    seeded = local / ".db_hash"
    if seeded.exists() and not (local / ".needs_push").exists():
        results.check(False, "the next --end would see work to do",
                      "no marker and .db_hash was re-seeded from the merged db")
    else:
        results.check(True, "the next --end would see work to do")


def check_merge_start_end_pushes(root, s3, bucket, key):
    print("\n[3] --merge → --start → --end actually uploads the merged state")
    local_root = root / "cycle"
    seed_db(ptmdb.data_dir(local_root) / "ptm.db", "p1", "Local project")
    source_root = root / "cycle-source"
    seed_db(ptmdb.data_dir(source_root) / "ptm.db", "p2", "Source project")

    env = {
        "AWS_REGION": s3.region,
        "AWS_ACCESS_KEY_ID": "harness",
        "AWS_SECRET_ACCESS_KEY": "harness",
    }
    s3args = ["--bucket-name", bucket, "--prefix", "ptm-backups",
              "--encryption-key", str(key)]

    cp = merge_into(local_root, source_root, key)
    results.check(cp.returncode == 0, "--merge succeeds", cp.stderr.strip()[-400:])

    puts_before = s3.puts
    cp = ptm_sync(["--start", "--local-is-master"] + s3args, local_root, env)
    results.check(cp.returncode == 0, "--start succeeds", cp.stderr.strip()[-300:])

    cp = ptm_sync(["--end"] + s3args, local_root, env)
    results.check(cp.returncode == 0, "--end succeeds", cp.stderr.strip()[-300:])
    results.check(
        s3.puts > puts_before,
        "the merged state was uploaded  "
        "[FAIL = finding #3 is live: --end logged 'No changes detected']",
        f"PUTs went {puts_before} -> {s3.puts}; "
        f"log: {[l for l in (ptmdb.data_dir(local_root) / 'ptm-sync.log').read_text().splitlines() if 'change' in l.lower()][-2:]}",
    )
    results.check(
        not (ptmdb.data_dir(local_root) / ".needs_push").exists(),
        "a successful push clears the marker",
    )

    # And a second --end must not re-upload: the marker being gone has to leave
    # the hash check in charge again.
    puts_after = s3.puts
    cp = ptm_sync(["--end"] + s3args, local_root, env)
    results.check(cp.returncode == 0, "the second --end succeeds", cp.stderr.strip()[-300:])
    results.check(
        s3.puts == puts_after,
        "no re-upload once the merged state is in S3",
        f"PUTs went {puts_after} -> {s3.puts}",
    )


# ── #2 (exit code half): a refused push must leave the work for next time ────

def check_refused_end_exit_code(root, s3, bucket, key):
    print("\n[2] --end that cannot push exits non-zero AND leaves the work queued")
    local_root = root / "busy"
    local = ptmdb.data_dir(local_root)
    seed_db(local / "ptm.db", "p1", "Unbacked-up work")

    env = {
        "AWS_REGION": s3.region,
        "AWS_ACCESS_KEY_ID": "harness",
        "AWS_SECRET_ACCESS_KEY": "harness",
    }
    s3args = ["--bucket-name", bucket, "--prefix", "ptm-backups",
              "--encryption-key", str(key)]

    # A writer holder leaves a live WAL, which is what makes --end probe for
    # another process at all (see README, "Known limitation").
    with ptmdb.Holder(local / "ptm.db", mode="writer") as h:
        h.wait_ready()
        assert ptmdb.wal_size(local / "ptm.db") > 0, "precondition: a live WAL"
        puts_before = s3.puts
        cp = ptm_sync(["--end"] + s3args, local_root, env)

    results.check(
        cp.returncode != 0,
        "ptm-sync --end exits non-zero when it cannot push  "
        "[FAIL = finding #2 is live: the wrapper's trap sees success]",
        f"exit={cp.returncode} stderr={cp.stderr.strip()[:300]}",
    )
    results.check(
        s3.puts == puts_before,
        "nothing was uploaded",
        f"PUTs went {puts_before} -> {s3.puts}",
    )
    # The exit code alone is not enough: the documented wrapper is
    # `trap 'ptm-sync --end' EXIT`, which discards it.  The refusal has to
    # record that the work is still pending, or a --start before the next
    # --end would record it as synced and it would never be pushed.
    results.check(
        (local / ".needs_push").exists(),
        "the refusal leaves the data marked for upload  "
        "[FAIL = the session's work is silently unbacked-up]",
        f"files: {sorted(p.name for p in local.iterdir())}",
    )


# ── #12: non-TTY network failure must not block the launch ───────────────────

def check_non_tty_network_error(root):
    print("\n[12] a transient S3 error does not block the launch when stdin is not a TTY")
    local = ptmdb.data_dir(root)
    seed_db(local / "ptm.db", "p1", "P")
    # Pretend we have synced before, so --start attempts a real pull and hits
    # the network path instead of the "never synced" guard.
    (local / ".last_pulled_etag").write_text('"deadbeef"')
    key = ptmdb.make_key(Path(root) / "key.bin")

    cp = ptmdb.run(
        [ptmdb.binary("ptm-sync", PROFILE), "--start",
         "--bucket-name", "ptm-harness-no-such-bucket", "--prefix", "p",
         "--encryption-key", str(key)],
        env={
            "XDG_DATA_HOME": str(root),
            # Unreachable credentials/region: forces the network-error path.
            "AWS_REGION": "localhost:1",
            "AWS_ACCESS_KEY_ID": "x",
            "AWS_SECRET_ACCESS_KEY": "y",
        },
        # stdin is a pipe, not a TTY — exactly the desktop-launcher case.
    )
    stderr = cp.stderr.strip()
    results.check(
        cp.returncode == 0,
        "ptm-sync --start exits 0 so `ptm` still launches  "
        "[FAIL = finding #12 is live: a transient S3 error blocks the launch]",
        f"exit={cp.returncode}\n{stderr[-400:]}",
    )
    log = (local / "ptm-sync.log")
    text = log.read_text() if log.exists() else ""
    results.check(
        "cannot prompt" not in text,
        "the log does not record 'cannot prompt — aborting'",
        [l for l in text.splitlines() if "prompt" in l or "abort" in l.lower()],
    )
    results.check(
        "continuing" in text.lower() or "continuing" in stderr.lower(),
        "the reason the pull was skipped is reported",
        f"stderr={stderr[-300:]}\nlog={[l for l in text.splitlines() if 'continu' in l.lower()]}",
    )
    # The skipped pull must not touch the local data — that is what makes
    # "continue anyway" the safe answer.
    titles = [r[0] for r in ptmdb.rows(local / "ptm.db", "SELECT title FROM projects")]
    results.check(
        titles == ["P"],
        "the local database is untouched by the failed pull",
        f"titles = {titles}",
    )


def main():
    bucket = fakes3.pick_bucket()
    with ptmdb.TempWorkspace("ptm-sync-state-") as root:
        print(f"working in {root}")
        print(f"binary profile: {PROFILE}")
        check_merge_marks_state_for_upload(root / "m")
        check_non_tty_network_error(root / "n")

        if bucket is None:
            print("\n[3] SKIP: the --merge → --start → --end round trip needs the")
            print("    /etc/hosts entry (see dev-harness/README.md):")
            print("    " + fakes3.hosts_hint().replace("\n", "\n    "))
        else:
            key = ptmdb.make_key(root / "cycle-key.bin")
            with fakes3.FakeS3(root / "cycle-object.bin") as s3:
                check_merge_start_end_pushes(root / "c", s3, bucket, key)
            with fakes3.FakeS3(root / "busy-object.bin") as s3:
                check_refused_end_exit_code(root / "b", s3, bucket, key)
        return results.finish()


if __name__ == "__main__":
    sys.exit(main())
