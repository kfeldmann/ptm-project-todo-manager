#!/usr/bin/env python3
"""
sync-state.py — reproductions for findings #3 and #12.

  3   `--start` seeds `.db_hash` from the current db whenever the file is
      absent, so the state `--merge` produced is recorded as the sync baseline
      and the next `--end` decides "no changes" and never uploads the merge.
      `merge.rs` deletes `.db_hash` precisely so the merge gets pushed.
  12  `--start` aborts on a network error when stdin is not a TTY, despite the
      function being named `prompt_continue_without_pull` and the documented
      behaviour being "allow concurrent launch".

Both are observable without S3: #3 through the state files, #12 through the
exit code (any S3 attempt fails without credentials/network).

    python3 dev-harness/sync-state.py
"""

import os
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import ptmdb  # noqa: E402

PROFILE = os.environ.get("PTM_PROFILE", "debug")
results = ptmdb.Results("sync-state")
TS = "'2025-01-01T00:00:00Z'"


def ptm_sync(args, root, stdin=None):
    return ptmdb.run(
        [ptmdb.binary("ptm-sync", PROFILE)] + args,
        env={"XDG_DATA_HOME": str(root)},
    )


# ── #3: .db_hash seeding defeats the merge push ──────────────────────────────

def check_merge_state_never_pushed(root):
    print("\n[3] --start re-seeds .db_hash, so a merged-but-unpushed db never uploads")
    local = ptmdb.data_dir(root)
    db = local / "ptm.db"

    # A real merge leaves the local db changed and deletes .db_hash so that the
    # next --end pushes. Reproduce that end state directly: this harness is
    # about what --start does to it, not about the merge itself (see
    # merge-robust.py for that).
    c = ptmdb.new_db(db)
    c.execute("INSERT INTO projects VALUES('p1','Merged result','',1.0,0," + TS + "," + TS + ")")
    c.close()
    hash_file = local / ".db_hash"
    if hash_file.exists():
        hash_file.unlink()

    key = ptmdb.make_key(Path(root) / "key.bin")
    # --local-is-master skips the pull, so no S3 traffic is needed.
    cp = ptm_sync(
        ["--start", "--local-is-master", "--bucket-name", "b", "--prefix", "p",
         "--encryption-key", str(key)],
        root,
    )
    results.check(cp.returncode == 0, "--start --local-is-master succeeds", cp.stderr.strip()[-300:])

    seeded = hash_file.exists()
    results.check(
        not seeded,
        ".db_hash is still absent after --start when the db has never been pushed  "
        "[FAIL = finding #3 is live: the merge result is recorded as the sync baseline]",
        f".db_hash exists = {seeded}, contents = "
        f"{hash_file.read_text()[:16] + '...' if seeded else '(none)'}",
    )

    if seeded:
        import hashlib
        current = hashlib.sha256(db.read_bytes()).hexdigest()
        stored = hash_file.read_text().strip()
        results.check(
            current != stored,
            "the next --end would see a change to push",
            "current == stored, so --end logs 'No changes detected' and uploads nothing",
        )


# ── #12: non-TTY network failure aborts the launch ───────────────────────────

def check_non_tty_network_error(root):
    print("\n[12] a transient S3 error hard-fails the launch when stdin is not a TTY")
    local = ptmdb.data_dir(root)
    db = local / "ptm.db"
    c = ptmdb.new_db(db)
    c.execute("INSERT INTO projects VALUES('p1','P','',1.0,0," + TS + "," + TS + ")")
    c.close()
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


def main():
    with ptmdb.TempWorkspace("ptm-sync-state-") as root:
        print(f"working in {root}")
        print(f"binary profile: {PROFILE}")
        check_merge_state_never_pushed(root / "m")
        check_non_tty_network_error(root / "n")
        return results.finish()


if __name__ == "__main__":
    sys.exit(main())
