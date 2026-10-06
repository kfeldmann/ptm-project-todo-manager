//! High-level --start and --end operational flows.

use anyhow::{Context, Result};

use crate::{
    archive, check_key_permissions, compute_hash, crypto, db, log_entry, merge_dictionaries,
    prompt_continue_without_pull, read_etag_file, write_etag_file, Args, Paths,
};
use crate::s3client::S3Client;

// ── --start ───────────────────────────────────────────────────────────────────

pub fn run_start(args: &Args, paths: &Paths) -> Result<()> {
    let key_path = args.key_path();
    check_key_permissions(&key_path)?;

    // ── Determine initial pull intent ─────────────────────────────────────
    let mut pull = !args.local_is_master;

    // ── WAL file handling ─────────────────────────────────────────────────
    if paths.db_wal.exists() {
        log_entry(paths, "WAL file found; checking if database is in use");
        match db::check_and_checkpoint_wal(&paths.db) {
            Ok(true) => {
                log_entry(paths, "Stale WAL detected; checkpointed and truncated");
                // pull intent unchanged
            }
            Ok(false) => {
                // Another ptm process has the database open.
                if args.block_concurrent {
                    anyhow::bail!(
                        "Database is currently in use by another ptm process. \
                         Omit --block-concurrent to allow concurrent launch."
                    );
                }
                log_entry(
                    paths,
                    "Database in use by another process; skipping pull, allowing launch",
                );
                pull = false;
            }
            Err(e) => {
                // Cannot determine WAL status; be conservative.
                log_entry(
                    paths,
                    &format!("WAL status check error (treating as in-use): {}", e),
                );
                pull = false;
            }
        }
    }

    // ── Pull logic ────────────────────────────────────────────────────────
    if pull {
        pull = evaluate_and_maybe_pull(args, paths)?;
    }

    // ── Seed db hash on first launch ──────────────────────────────────────
    // .db_hash records the state at the last successful push or pull so that
    // --end can detect whether a push is needed.  --start only creates it when
    // absent (first launch, or the file was deleted) so that an unsynced
    // push-failure is not silently reset by a subsequent session start.
    // do_pull and a successful --end are the only code paths that update it.
    if !paths.db_hash.exists() {
        let db_bytes = std::fs::read(&paths.db).unwrap_or_default();
        let dict_bytes = std::fs::read(&paths.dict).ok();
        let hash = compute_hash(&db_bytes, dict_bytes.as_deref());

        if args.dry_run {
            log_entry(paths, &format!("[dry-run] would store initial db hash: {}", hash));
        } else {
            std::fs::write(&paths.db_hash, &hash)
                .context("Failed to write .db_hash")?;
            log_entry(paths, "Stored initial db hash (first launch)");
        }
    } else {
        log_entry(paths, "db hash present; leaving unchanged");
    }

    if pull {
        log_entry(paths, "--start complete (pulled from S3)");
    } else {
        log_entry(paths, "--start complete (no pull)");
    }

    Ok(())
}

/// Work through the pull-decision tree.  Returns `true` if a pull was
/// actually performed, `false` otherwise.
fn evaluate_and_maybe_pull(args: &Args, paths: &Paths) -> Result<bool> {
    // ── Guard: protect an unsynced local database ─────────────────────────
    // If there are no etag files AND a local db exists AND we aren't forced,
    // skip the pull rather than risk clobbering data that has never been synced.
    let has_pushed_etag = paths.last_pushed_etag.exists();
    let has_pulled_etag = paths.last_pulled_etag.exists();
    let db_exists = paths.db.exists();

    if !has_pushed_etag && !has_pulled_etag && db_exists && !args.remote_is_master {
        let msg = "Local database exists but has never been synced. \
                   Skipping pull to protect local data. \
                   Use --remote-is-master to force a pull.";
        log_entry(paths, msg);
        eprintln!("Warning: {}", msg);
        return Ok(false);
    }

    // ── Guard: local data is ahead of remote (e.g. prior push failure) ──────
    // Compare the current db hash against the stored last-synced hash.  A
    // mismatch means local data has changed since the last successful push or
    // pull; pulling now would clobber those unsynced changes.
    if !args.remote_is_master && db_exists {
        if let Ok(stored) = std::fs::read_to_string(&paths.db_hash) {
            let stored = stored.trim();
            if !stored.is_empty() {
                let db_bytes = std::fs::read(&paths.db).unwrap_or_default();
                let dict_bytes = std::fs::read(&paths.dict).ok();
                let current = compute_hash(&db_bytes, dict_bytes.as_deref());
                if current != stored {
                    let msg = "Local data has unsynced changes since the last push; \
                               skipping pull to protect local data. \
                               Run ptm-sync --end to push first, or use \
                               --remote-is-master to force a pull.";
                    log_entry(paths, msg);
                    eprintln!("Warning: {}", msg);
                    return Ok(false);
                }
            }
        }
    }

    // ── Connect to S3 and fetch remote ETag ───────────────────────────────
    let s3 = match S3Client::new(&args.bucket_name, &args.prefix) {
        Ok(s) => s,
        Err(e) => {
            prompt_continue_without_pull(paths, &e.to_string())?;
            return Ok(false);
        }
    };

    let remote_etag = match s3.head_etag() {
        Ok(Some(etag)) => etag,
        Ok(None) => {
            log_entry(paths, "No remote backup found; nothing to pull");
            return Ok(false);
        }
        Err(e) => {
            prompt_continue_without_pull(paths, &e.to_string())?;
            return Ok(false);
        }
    };

    // ── Skip if we already have this version ──────────────────────────────
    if !args.remote_is_master {
        let last_pushed = read_etag_file(&paths.last_pushed_etag);
        let last_pulled = read_etag_file(&paths.last_pulled_etag);

        let already_current = last_pushed.as_deref() == Some(remote_etag.as_str())
            || last_pulled.as_deref() == Some(remote_etag.as_str());

        if already_current {
            log_entry(
                paths,
                &format!("Remote etag {} matches cached version; skipping pull", remote_etag),
            );
            return Ok(false);
        }
    }

    // ── Perform the pull ──────────────────────────────────────────────────
    do_pull(args, paths, &s3, &remote_etag)?;
    Ok(true)
}

/// Download, decrypt, unpack, and install the remote backup.
fn do_pull(args: &Args, paths: &Paths, s3: &S3Client, remote_etag: &str) -> Result<()> {
    // Create a local backup of whatever is currently on disk.
    let has_local_data = paths.db.exists() || paths.dict.exists();
    if has_local_data && !args.dry_run {
        let ts = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
            .format("%Y-%m-%dT%H-%M-%SZ")
            .to_string();
        match archive::create_local_backup(paths, &ts) {
            Ok(p) => {
                log_entry(paths, &format!("Local backup created: {}", p.display()));
                if let Err(e) = archive::rotate_local_backups(&paths.local_backups, 5) {
                    log_entry(paths, &format!("Warning: backup rotation failed: {}", e));
                }
            }
            Err(e) => {
                // A failed local backup is reported but does not block the pull.
                log_entry(paths, &format!("Warning: failed to create local backup: {}", e));
                eprintln!("Warning: failed to create local backup: {}", e);
            }
        }
    }

    if args.dry_run {
        log_entry(
            paths,
            &format!("[dry-run] would download and install backup (remote etag: {})", remote_etag),
        );
        return Ok(());
    }

    // Download.
    log_entry(paths, "Downloading backup from S3");
    let encrypted = s3.download().context("Failed to download backup from S3")?;

    // Decrypt.
    log_entry(paths, "Decrypting backup");
    let key = crypto::load_key(&args.key_path())?;
    let zip_bytes =
        crypto::decrypt(&encrypted, &key).context("Decryption failed")?;

    // Unpack.
    log_entry(paths, "Unpacking backup");
    let (db_bytes, backup_dict) =
        archive::unpack(&zip_bytes).context("Failed to unpack backup")?;

    // ── Stage into a temp directory on the same filesystem ────────────────
    let tmp_dir = paths.data_dir.join(".tmp-extract");
    std::fs::create_dir_all(&tmp_dir).context("Failed to create temp extract directory")?;

    let tmp_db = tmp_dir.join("ptm.db");
    std::fs::write(&tmp_db, &db_bytes)
        .context("Failed to write db to temp directory")?;

    // Merge the user dictionary.
    let merged_dict: Option<String> = match &backup_dict {
        Some(remote_dict_bytes) => {
            let remote_str = String::from_utf8_lossy(remote_dict_bytes);
            let local_str = std::fs::read_to_string(&paths.dict).unwrap_or_default();
            let merged = merge_dictionaries(&local_str, &remote_str);
            if merged.is_empty() { None } else { Some(merged) }
        }
        None => None, // no dict in backup — leave local dict untouched
    };

    if let Some(ref dict_content) = merged_dict {
        let tmp_dict = tmp_dir.join("user_words.txt");
        std::fs::write(&tmp_dict, dict_content.as_bytes())
            .context("Failed to write merged dict to temp directory")?;
        // Move dict into place before db so that if dict rename fails, db is still intact.
        std::fs::rename(&tmp_dict, &paths.dict)
            .context("Failed to move merged dict into place")?;
    }

    // Move db into place (atomic rename on same filesystem).
    std::fs::rename(&tmp_db, &paths.db)
        .context("Failed to move db into place")?;

    // Clean up temp dir (best effort — a leftover dir is harmless).
    let _ = std::fs::remove_dir(&tmp_dir);

    // Record the pulled ETag.
    write_etag_file(&paths.last_pulled_etag, remote_etag)
        .context("Failed to write .last_pulled_etag")?;

    // Record the hash of what was just installed so --end treats this pulled
    // state as the baseline.  Without this, --end would see current != last-push
    // hash and re-upload data we just downloaded.
    let dict_bytes_on_disk = std::fs::read(&paths.dict).ok();
    let installed_hash = compute_hash(&db_bytes, dict_bytes_on_disk.as_deref());
    std::fs::write(&paths.db_hash, &installed_hash)
        .context("Failed to write .db_hash after pull")?;

    log_entry(paths, &format!("Pull complete; etag: {}", remote_etag));

    Ok(())
}

// ── --end ─────────────────────────────────────────────────────────────────────

pub fn run_end(args: &Args, paths: &Paths) -> Result<()> {
    // The database must exist — there is nothing meaningful to back up otherwise.
    if !paths.db.exists() {
        anyhow::bail!(
            "ptm.db does not exist at '{}'; nothing to back up",
            paths.db.display()
        );
    }

    let key_path = args.key_path();
    check_key_permissions(&key_path)?;

    // ── WAL file handling ─────────────────────────────────────────────────
    if paths.db_wal.exists() {
        log_entry(paths, "WAL file found; checking if database is in use");
        match db::check_and_checkpoint_wal(&paths.db) {
            Ok(true) => {
                log_entry(paths, "Stale WAL detected; checkpointed and truncated");
                // fall through to push logic
            }
            Ok(false) => {
                log_entry(
                    paths,
                    "Database in use by another process; skipping push",
                );
                return Ok(()); // exit 0 — another ptm is still running
            }
            Err(e) => {
                log_entry(paths, &format!("WAL status check error: {}; skipping push", e));
                return Ok(());
            }
        }
    }

    // ── --remote-is-master: skip push ─────────────────────────────────────
    if args.remote_is_master {
        log_entry(paths, "--remote-is-master set; skipping push");
        return Ok(());
    }

    // ── Hash check: skip upload if nothing changed ────────────────────────
    let db_bytes = std::fs::read(&paths.db).context("Failed to read ptm.db")?;
    let dict_bytes = std::fs::read(&paths.dict).ok();
    let current_hash = compute_hash(&db_bytes, dict_bytes.as_deref());

    let stored_hash = std::fs::read_to_string(&paths.db_hash)
        .unwrap_or_default();
    let stored_hash = stored_hash.trim();

    if !stored_hash.is_empty() && stored_hash == current_hash {
        log_entry(paths, "No changes detected since last sync; skipping upload");
        return Ok(());
    }
    log_entry(paths, "Changes detected; preparing upload");

    // ── Check remote ETag before deciding to push ─────────────────────────
    let s3 = S3Client::new(&args.bucket_name, &args.prefix)
        .context("Failed to connect to S3")?;

    let remote_etag = s3.head_etag().context("Failed to check remote backup")?;

    let stored_pulled_etag = read_etag_file(&paths.last_pulled_etag);
    let stored_pushed_etag = read_etag_file(&paths.last_pushed_etag);

    // Guard: if a remote backup exists but we have never pulled it, refuse to
    // overwrite it unless --local-is-master is set OR the remote is a backup
    // that we ourselves pushed (remote_etag == last_pushed_etag).
    let we_own_the_remote = stored_pushed_etag.is_some() && stored_pushed_etag == remote_etag;
    if stored_pulled_etag.is_none() && remote_etag.is_some() && !args.local_is_master && !we_own_the_remote {
        anyhow::bail!(
            "A remote backup exists but this machine has never pulled it. \
             Pull first, or use --local-is-master to force overwrite."
        );
    }

    // Guard: if the remote has changed since our last pull/push, refuse to overwrite.
    let can_push = remote_etag.is_none()
        || args.local_is_master
        || remote_etag == stored_pulled_etag
        || remote_etag == stored_pushed_etag;

    if !can_push {
        anyhow::bail!(
            "Remote backup has been updated since our last pull (ETag mismatch). \
             Pull first to reconcile, or use --local-is-master to force overwrite."
        );
    }

    // ── Package, encrypt, upload ──────────────────────────────────────────
    log_entry(paths, "Creating zip archive");
    let zip_bytes = archive::pack(&db_bytes, dict_bytes.as_deref())
        .context("Failed to create zip archive")?;

    log_entry(paths, "Encrypting backup");
    let key = crypto::load_key(&key_path)?;
    let encrypted = crypto::encrypt(&zip_bytes, &key)
        .context("Failed to encrypt backup")?;

    if args.dry_run {
        log_entry(
            paths,
            &format!("[dry-run] would upload {} bytes to S3", encrypted.len()),
        );
        return Ok(());
    }

    log_entry(paths, "Uploading to S3");
    let new_etag = s3.upload(&encrypted).context("Failed to upload backup to S3")?;

    write_etag_file(&paths.last_pushed_etag, &new_etag)
        .context("Failed to write .last_pushed_etag")?;

    // Update the db hash to reflect the successfully pushed state.
    // This is the only point (along with do_pull) where .db_hash is updated.
    // Keeping it here — after a confirmed upload — means any earlier failure
    // leaves the old hash in place, causing the next --end to retry the push.
    std::fs::write(&paths.db_hash, &current_hash)
        .context("Failed to write .db_hash")?;

    log_entry(paths, &format!("Upload complete; etag: {}", new_etag));
    log_entry(paths, "--end complete");

    Ok(())
}
