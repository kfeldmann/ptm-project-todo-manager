# PTM Cloud Sync Tool Planning

The idea is to pull a backup from the cloud before starting ptm, and push a backup after quitting.

Rather than running ptm directly, use a wrapper script something like this:

```
#!/bin/sh

set -e

export XDG_DATA_HOME=/data/location
export AWS_PROFILE=my_profile
export AWS_REGION=my_region

ptm-sync --start --bucket-name my-bucket --prefix ptm-backups
# Set the trap after --start so we don't upload after a failed pull. Set it before ptm so we do checkpoint and push after a ptm crash
trap 'ptm-sync --end --bucket-name my-bucket --prefix ptm-backups' EXIT
ptm # whether ptm exits or crashes, the trap runs to handle the backup
```

**Some gotchas:**
- Need to remember the etag of the file we pulled at startup, check it again before pushing. If it's different, stop and warn
- Need to check for ptm.db-wal file. Do not push or pull in that case (it often means another ptm process has the DB open). There could also be a db-shm file, but this file doesn't cause the same concern. The wal file contains updates that are not yet in the db file. If we back up the db file (without the wal file), we will be missing data. The shm file doesn't have this risk, so I'm choosing to ignore it here. Later we will talk about how to determine whether the wal file exists because the db is in use, or because there was a crash.
- Race condition between checking etag of cloud object and uploading a new version. Since this is a single-user app, we will accept this risk for now
- Overwriting db file (or dictionary file) locally can cause corruption if interrupted. Instead, we can extract the backup in a parallel directory and move each of the files into place. The temp dir should be created inside the ptm data directory (e.g., ~/.local/share/ptm/.tmp-extract/) to ensure it is on the same filesystem as the target files
- Encryption, decryption, packaging, or extraction could fail, producing partial/corrupt data. `ptm-sync` should catch such failures, log the error, and also print an error message to stderr and stop (exit 1) before moving extracted files into place locally or uploading backups to S3.
- `ptm-sync --end` should treat a missing db file as an error and print messages to the sync log and to stderr and exit 1.
- `ptm-sync --start` should check permissions on the encryption key. If permissions allow group or other read or write, log and print an error, and exit 1 (before pulling)

**How to check whether the database is in use:**

Attempt to acquire SQLite's `EXCLUSIVE` locking mode — if it fails, another process has the DB open. In rusqlite:

```rust
conn.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;")?;
```
If this errors, the DB is in use.

If the connection succeeds, we can proceed to run `PRAGMA wal_checkpoint(TRUNCATE)` to checkpoint the database (we only connected in the first place because there is a wal file. Our connection was successful, which means the wal file is stale).

**Some desired features:**
- Default database location: `~/.local/share/ptm/ptm.db`. If the `XDG_DATA_HOME` environment variable is set, that path is used instead of `~/.local/share`.
- Compression and packaging of multiple files (zip using the `zip` crate, version 8). Should be extractable manually for disaster recovery
- Encryption - client-side (end-to-end) symmetric - AES-256-GCM. The key file contains only a raw 32-byte key. Key transfer from machine to machine happens out of band (user views a base64 encoding of the key on one screen and type it into the other computer)
- Backup the database file (`ptm.db`) and user dictionary (`user_words.txt`) file packaged together (zip). These files both live in the ptm data directory
- S3 versioning with lifecycle rule (for non-current versions: delete after 90 days, keeping a minimum of 10 previous versions)
- S3 object key: `<prefix>/ptm-backup.zip.enc` (where `<prefix>` is set by the --prefix argument)
- When pulling from the cloud, the user dictionary files can be merged: Since it's a plain line-per-word text file, merge (union + dedup + sort) instead of overwrite when extracting `user_words.txt`.
- For safety, store a copy of the local data (in a sub directory; zip, no encryption) before pulling from the cloud. Keep the last 5 copies (delete older); Security: The same data is (was) in the active db without encryption. The backups have the same permissions and are in a subdirectory next to the live db
- Write a sync log in the ptm data directory (`ptm-sync.log`)
- If ptm crashes, the ptm-sync could detect that the wal file is stale (no process attached) and then connect to the db, checkpoint the db, and then disconnect, causing SQLite to flush the wal into the database. Then it could push to the cloud as planned.

**Encryption:**

See `./encrypt.py`. The `ptm-sync` tool should implement the same encryption. The `encrypt.py` tool should be able to be used for key generation and for manual extraction of backups.

**Optimizations:**
- A way to determine that the data in the cloud is the same version we pushed. In other words, if we exit ptm and push a fresh backup, then 5 minutes later start ptm again, it would be nice to avoid the pulling process since it would be (at best) a no-op. Maybe we can check the etag value after uploading and store that as `.last_pushed_etag` (the pull would store `.last_pulled_etag`). The pull would compare the cloud to the last pushed etag, and the push process would compare the cloud to the last pulled etag. These etag files will be inside the ptm data directory, but not included in the backup package (only two files are included: the db file and the user dictionary)
- A way to avoid uploading if the local DB has not changed (ptm was run but no writes occurred): Make a hash of the DB + user dictionary combined before startup and then again after shutdown and compare. If they differ, push a backup. Store the hash as `.db_hash` in the ptm data directory.

**Command-line arguments and environment variables:**
- `$XDG_DATA_HOME` (same as ptm)
- --dry-run - Run through all the logic without changing anything in the cloud or local files. Read-only operations are performed locally and in the cloud. Prints out what would have happened and why. Also check existence of encryption key
- --start|--end - are we running before or after ptm? Required.
- --local-is-master|--remote-is-master - Force push or pull for correction or recovery from deadlock. Allows trampling of the "not master" side.
- --block-concurrent - If ptm.db-wal file is found, skip pulling backup, print warning and exit 1, preventing launch of ptm
    Optional. If not used, allow concurrency: If ptm.db-wal file is found, skip pulling backup and allow launch of ptm (exit 0)
- (AWS env vars, particularly for region and profile)
- --bucket-name `<name>` - s3 bucket name
- --prefix `<prefix>` - s3 object prefix
- --encryption-key `<filepath>` - defaults to `~/.encryption_key`

**Operational Flow (including optimizations proposed above):**
- if --start:
    - if --local-is-master: - pull=False
    - else: pull=True
    - if ptm.db-wal exists: # determine whether this is because a ptm process has the db open, or if it is stale from a crash. See "How to check whether the database is in use" above. The detection and checkpoint happen together
        - if db is in use:
            - if --block-concurrent:
                - Warning message to stderr; exit 1
            - else: # another ptm is running; allow launch but skip pull
                - pull=False
        - else: # stale WAL from a previous crash
            # if the exclusive-mode connection succeeded above, run `PRAGMA wal_checkpoint(TRUNCATE)` to flush and remove wal
            # pull=True remains; fall through to pull logic
    - if pull==True:
        - if no pushed etag stored AND no pulled etag stored AND local db file exists AND NOT --remote-is-master: pull=False # protect local db which has never sync'd
        # if no local db file, pull remains True even if no pushed etag is stored
        - fetch remote etag; if no remote object: pull=False
        - if network error: alert user; ask for confirmation to continue without pulling; pull=False
        - if (`remote_etag` == `last_pushed_etag` OR `remote_etag` == `last_pulled_etag`) AND NOT --remote-is-master: pull=False
        - if pull==True:
            - make local backup of existing db and user dictionary (zip). Named with the date-time (ISO 8601 with hyphens replacing colons, safe for filenames)
            - check for older local backups. If more than 5, delete oldest extras
            - pull from cloud
            - decrypt and expand, move files into place
            - store etag of pulled object
    - take hash of db file + user dictionary combined and store it
    - exit 0
- elif --end:
    - if ptm.db-wal exists: # determine stale file or in-use by other ptm process(es)
        - if stale:
            - connect to db and run `PRAGMA wal_checkpoint(TRUNCATE)` to flush and remove wal
            # fall through to push logic
        - else: # another ptm process is running
            - exit 0
    - if --remote-is-master:
        - exit 0
    - take hash of db file + user dictionary
    - if hash differs from stored hash or if there is no stored hash:
        - check etag of remote object
        - if network error: alert user (stderr); exit 1
        - if no stored pulled etag AND remote object exists AND NOT --local-is-master:
            - alert (stderr); exit 1
        - if remote etag matches stored pulled etag OR no remote object OR --local-is-master:
            - package db and user dictionary (zip then encrypt)
            - upload to S3
            - fetch and store etag of pushed object (store what comes from S3 - don't compute it locally)
            - exit 0
        - else: # etag mismatch (remote has been updated since our last pull)
            - Warning message
            - exit 1

**Some special use cases:**

First time setup of a laptop. Cloud backups and encryption key exist.
- Load existing encryption key onto laptop (user types key by hand; `~/.encryption_key`)
- Configure AWS credentials in `~/.aws`
- Install ptm, ptm-sync, and the wrapper script
- With no existing local db, `ptm-sync --start` will pull the cloud backup 
- Make changes to the data within ptm
- After quitting, if remote etag matches stored pulled etag, `ptm-sync --end` will push a backup

First rollout of cloud sync. Existing local db file. No cloud backups or encryption key.
- Generate encryption key using `encrypt.py keygen`
- Configure AWS credentials in `~/.aws`
- Install ptm-sync, and the wrapper script
- `ptm-sync --start` will not pull (no remote object), but will store the hash of the local db + dictionary
- Make changes to the data within ptm
- `ptm-sync --end` will upload backup
