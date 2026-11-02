//! High-level --start and --end operational flows.

use anyhow::{bail, Context, Result};

use crate::{
    archive, clear_needs_push, compute_hash, crypto, db, log_entry, mark_needs_push,
    merge_dictionaries, needs_push, prompt_continue_without_pull, read_etag_file,
    write_etag_file, Args, Paths,
};
use crate::s3client::S3Client;

/// Bail out unless both S3 connection arguments are present.
/// Only --merge may omit them.
fn require_s3_args(args: &Args) -> Result<()> {
    match (&args.bucket_name, &args.prefix) {
        (Some(b), Some(p)) if !b.trim().is_empty() && !p.trim().is_empty() => Ok(()),
        (None, None) => anyhow::bail!("--bucket-name and --prefix are required for --start/--end"),
        (Some(_), None) => anyhow::bail!("--prefix is required for --start/--end"),
        (None, Some(_)) => anyhow::bail!("--bucket-name is required for --start/--end"),
        _ => anyhow::bail!("--bucket-name and --prefix must be non-empty for --start/--end"),
    }
}

// ── --start ───────────────────────────────────────────────────────────────────

/// Bytes of the local database in the form every hash in this file is computed
/// from.
///
/// This is a `VACUUM INTO` snapshot, never `std::fs::read(ptm.db)`.  Three
/// reasons: the raw file can be missing committed data that is still in the WAL
/// (finding #2); a snapshot is byte-identical across checkpoints, so the same
/// logical state always hashes the same; and `--end` hashes a snapshot too, so
/// the change test and the upload always agree.  A missing or unreadable
/// database yields no bytes, which is what the callers want — they only use this
/// to decide whether to record a baseline.
fn current_db_bytes(paths: &Paths) -> Vec<u8> {
    if !paths.db.exists() {
        return Vec::new();
    }
    match db::snapshot_bytes(&paths.db) {
        Ok(bytes) => bytes,
        Err(e) => {
            log_entry(
                paths,
                &format!("Warning: could not snapshot ptm.db for hashing: {:#}", e),
            );
            Vec::new()
        }
    }
}

pub fn run_start(args: &Args, paths: &Paths) -> Result<()> {
    require_s3_args(args)?;
    let key_path = args.key_path()?;
    crypto::check_key_permissions(&key_path)?;

    // ── Determine initial pull intent ─────────────────────────────────────
    let mut pull = !args.local_is_master;

    // ── WAL file handling ─────────────────────────────────────────────────
    if db::wal_size(&paths.db) > 0 {
        log_entry(paths, "WAL file found; checking if database is in use");
        match db::check_and_checkpoint_wal(&paths.db) {
            Ok(Some(report)) if report.is_clean() => {
                log_entry(
                    paths,
                    &format!(
                        "Stale WAL detected; checkpointed {} frame(s) and truncated it",
                        report.checkpointed
                    ),
                );
                // pull intent unchanged
            }
            // Either another process has the database open (`Ok(None)`) or the
            // checkpoint could not advance (`busy` / frames left behind, which
            // SQLite does not report as an error).  Both mean the WAL is live,
            // so the pull must not proceed.
            Ok(Some(report)) => {
                let why = format!(
                    "WAL is live (checkpoint busy={}, {} frame(s) left)",
                    report.busy, report.log_frames
                );
                if args.block_concurrent {
                    bail!(
                        "Database is currently in use by another ptm process ({}). \
                         Omit --block-concurrent to allow concurrent launch.",
                        why
                    );
                }
                log_entry(
                    paths,
                    &format!("{}; skipping pull, allowing launch", why),
                );
                pull = false;
            }
            Ok(None) => {
                // Another ptm process has the database open.
                if args.block_concurrent {
                    bail!(
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
                    &format!("WAL status check error (treating as in-use): {:#}", e),
                );
                pull = false;
            }
        }
    }

    // ── Pull logic ────────────────────────────────────────────────────────
    let pulled = if pull {
        evaluate_and_maybe_pull(args, paths)?
    } else {
        false
    };

    // ── Seed db hash on first launch ──────────────────────────────────────
    // .db_hash records the state at the last successful push or pull so that
    // --end can detect whether a push is needed.  --start creates it when
    // absent (first launch) so that the very first session has a baseline.
    //
    // It must NOT create it when something has recorded that the local state is
    // unpushed — a --merge, or a --end that could not upload.  Seeding there
    // would record the unpushed state as the sync baseline and the next --end
    // would see "no changes" and upload nothing (finding #3).  The marker is
    // what makes that case distinguishable from a genuine first launch; see
    // `needs_push` in main.rs.
    if needs_push(paths) {
        log_entry(
            paths,
            "Local data is marked for upload; not seeding .db_hash (the next --end must push)",
        );
    } else if !paths.db_hash.exists() {
        let db_bytes = current_db_bytes(paths);
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

    if pulled {
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
    //
    // The `.needs_push` marker has to be checked too, not just the hash: a
    // state that changed and was *then* recorded as the baseline (by `--start`
    // seeding a missing `.db_hash`) matches the hash while being unpushed
    // (finding #3).  The marker is the only thing that remembers.
    if !args.remote_is_master && db_exists {
        if needs_push(paths) {
            let msg = "Local data is marked as not yet pushed; \
                       skipping pull to protect local data. \
                       Run ptm-sync --end to push first, or use \
                       --remote-is-master to force a pull.";
            log_entry(paths, msg);
            eprintln!("Warning: {}", msg);
            return Ok(false);
        }
        if let Ok(stored) = std::fs::read_to_string(&paths.db_hash) {
            let stored = stored.trim();
            if !stored.is_empty() {
                let db_bytes = current_db_bytes(paths);
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
    let bucket = args.bucket_name.as_deref().unwrap_or_default();
    let prefix = args.prefix.as_deref().unwrap_or_default();
    let s3 = match S3Client::new(bucket, prefix) {
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
    do_pull(args, paths, &s3, &remote_etag)
}

/// Download, decrypt, unpack, and install the remote backup.
///
/// Returns `true` when the pulled database was installed (or would have been,
/// under `--dry-run`), `false` when the install was refused because another
/// `ptm` session owns the database.
fn do_pull(args: &Args, paths: &Paths, s3: &S3Client, remote_etag: &str) -> Result<bool> {
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
        return Ok(true);
    }

    // Download.
    log_entry(paths, "Downloading backup from S3");
    let encrypted = s3.download().context("Failed to download backup from S3")?;

    // Decrypt.
    log_entry(paths, "Decrypting backup");
    let key = crypto::load_key(&args.key_path()?)?;
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

    // A backup that cannot be opened is never installed.  Checking here — before
    // the local database is touched — turns "restored a corrupt file over good
    // data" into a reported failure with the local data still in place.
    if let Err(e) = db::check_integrity(&tmp_db) {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        bail!("Downloaded backup is unusable, local data left untouched: {:#}", e);
    }

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

    // ── Install ───────────────────────────────────────────────────────────
    // Everything below happens while holding an EXCLUSIVE lock, so a `ptm`
    // that starts up mid-install cannot be writing to the database we are
    // about to replace.  Finding #1c was exactly that race: the lock probe
    // released the lock before the rename, and the stale `-wal` left beside
    // the old database was then replayed into the new one, discarding the
    // pulled data while `--start` reported success.
    let lock = match db::ExclusiveLock::acquire_or_create(&paths.db) {
        Ok(Some(lock)) => lock,
        // Another ptm started while we were downloading.
        Ok(None) => {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            log_entry(
                paths,
                "Database became in use during the pull; skipping install, allowing launch",
            );
            eprintln!(
                "Warning: the database is in use by another ptm process; \
                 the pulled backup was not installed."
            );
            return Ok(false);
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(e).context("Failed to lock the database for the install");
        }
    };

    let install = (|| {
        // Fold the WAL in and confirm it is empty: a non-empty WAL means
        // committed data that the file we are about to delete still owes.
        let report = lock.checkpoint()?;
        if !report.is_clean() {
            bail!(
                "cannot install the pulled backup: the local WAL could not be \
                 checkpointed (busy={}, {} frame(s) left)",
                report.busy,
                report.log_frames
            );
        }
        // Remove the sidecars so nothing is replayed into the new database.
        lock.remove_wal_files()?;

        if let Some(ref dict_content) = merged_dict {
            let tmp_dict = tmp_dir.join("user_words.txt");
            std::fs::write(&tmp_dict, dict_content.as_bytes())
                .context("Failed to write merged dict to temp directory")?;
            // Move dict into place before db so that if dict rename fails, db
            // is still intact.
            std::fs::rename(&tmp_dict, &paths.dict)
                .context("Failed to move merged dict into place")?;
        }

        // Move db into place (atomic rename on same filesystem).
        std::fs::rename(&tmp_db, &paths.db)
            .context("Failed to move db into place")?;
        Ok(())
    })();

    // Released either way: the install is over, successfully or not.
    drop(lock);

    // Clean up temp dir (best effort — a leftover dir is harmless).
    let _ = std::fs::remove_dir_all(&tmp_dir);
    install?;

    // Record the pulled ETag.
    write_etag_file(&paths.last_pulled_etag, remote_etag)
        .context("Failed to write .last_pulled_etag")?;

    // Record the hash of what was just installed so --end treats this pulled
    // state as the baseline.  Without this, --end would see current != last-push
    // hash and re-upload data we just downloaded.  The hash must come from the
    // same snapshot function --end uses, or the two can never agree.
    let installed_db_bytes = current_db_bytes(paths);
    let dict_bytes_on_disk = std::fs::read(&paths.dict).ok();
    let installed_hash = compute_hash(&installed_db_bytes, dict_bytes_on_disk.as_deref());
    std::fs::write(&paths.db_hash, &installed_hash)
        .context("Failed to write .db_hash after pull")?;

    // The database on disk is now exactly what the remote holds, so nothing is
    // waiting to be pushed — including a marker left by a `--merge` whose merge
    // this pull has just overwritten.  Leaving it would make the next `--end`
    // re-upload the same state for no reason.
    clear_needs_push(paths)
        .context("Failed to clear the upload marker after a pull")?;

    log_entry(paths, &format!("Pull complete; etag: {}", remote_etag));

    Ok(true)
}

// ── --end ─────────────────────────────────────────────────────────────────────

/// What `--end` should do about the upload, given the sync baseline.
///
/// Kept as a value (rather than inlined in `run_end`) so the decision can be
/// tested without a database, a key, or S3 — it is the part that finding #3 is
/// about, and getting it wrong loses data silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadDecision {
    /// The local state is what we last pushed or pulled: nothing to upload.
    Skip,
    /// The hash differs from the baseline: upload.
    Push,
    /// A `.needs_push` marker is set: upload even though the hash matches.
    PushBecauseMarked,
}

/// Decide whether `--end` uploads.
///
/// The marker wins over the hash.  Without that rule, a state that was recorded
/// as the baseline *after* it changed (by `--start` re-seeding a missing
/// `.db_hash`) is indistinguishable from a synced one, and the change is never
/// pushed (finding #3).  An absent or empty baseline means "never synced", which
/// always pushes.
fn upload_decision(marked: bool, stored_hash: &str, current_hash: &str) -> UploadDecision {
    if marked {
        return UploadDecision::PushBecauseMarked;
    }
    if !stored_hash.is_empty() && stored_hash == current_hash {
        return UploadDecision::Skip;
    }
    UploadDecision::Push
}

/// Record that a refused push left the local state unpushed.
///
/// The refusal is reported as an error, but that is not the end of the story:
/// this session's changes are still on disk and still unbacked-up.  The marker
/// is what makes the next `--end` push them even if `--start` runs first and
/// re-seeds `.db_hash` (finding #3).  A non-zero exit alone is not enough — the
/// documented wrapper is `trap 'ptm-sync --end' EXIT`, which discards it.
///
/// `--dry-run` promises to leave the data directory alone, so there it only
/// says what it would have done.
fn mark_refusal(paths: &Paths, dry_run: bool, why: &str) {
    if dry_run {
        log_entry(paths, &format!("[dry-run] would mark local data for upload: {}", why));
    } else {
        mark_needs_push(paths, why);
    }
}

/// A push that could not run.
///
/// Always an error: the documented wrapper runs `trap 'ptm-sync --end' EXIT`, so
/// a silent success means the session's work is never backed up and nobody is
/// told.  `--local-is-master` does **not** suppress this — that flag is about
/// S3 authority (push regardless of what the remote holds), and a user who
/// passed it has explicitly asked for a push, so quietly skipping one and
/// exiting 0 is the worst answer available.
///
/// In practice the only reachable caller is "another client is attached to the
/// database".  That condition is itself the signal that the other session will
/// push when it exits; the message says so rather than pretending the backup
/// succeeded.
fn push_refused(paths: &Paths, dry_run: bool, why: &str) -> Result<()> {
    mark_refusal(paths, dry_run, why);
    log_entry(paths, &format!("Push failed: {}", why));
    bail!(
        "Cannot back up: {}. This session's changes are NOT in S3. \
         Re-run ptm-sync --end once the database is free.",
        why
    )
}

pub fn run_end(args: &Args, paths: &Paths) -> Result<()> {
    require_s3_args(args)?;
    // The database must exist — there is nothing meaningful to back up otherwise.
    if !paths.db.exists() {
        anyhow::bail!(
            "ptm.db does not exist at '{}'; nothing to back up",
            paths.db.display()
        );
    }

    let key_path = args.key_path()?;
    crypto::check_key_permissions(&key_path)?;

    // ── --remote-is-master: skip push ─────────────────────────────────────
    // Decided before the WAL handling, which exists only to protect the push.
    if args.remote_is_master {
        log_entry(paths, "--remote-is-master set; skipping push");
        return Ok(());
    }

    // ── WAL file handling ─────────────────────────────────────────────────
    if db::wal_size(&paths.db) > 0 {
        log_entry(paths, "WAL file found; checking if database is in use");
        match db::check_and_checkpoint_wal(&paths.db) {
            Ok(Some(report)) if report.is_clean() => {
                log_entry(
                    paths,
                    &format!(
                        "Stale WAL detected; checkpointed {} frame(s) and truncated it",
                        report.checkpointed
                    ),
                );
                // fall through to push logic
            }
            // A checkpoint that could not advance leaves committed data in the
            // WAL.  That is not an error as far as SQLite is concerned, so it
            // has to be caught here (finding #1b).
            Ok(Some(report)) => {
                return push_refused(
                    paths,
                    args.dry_run,
                    &format!(
                        "the WAL could not be checkpointed (busy={}, {} frame(s) left)",
                        report.busy, report.log_frames
                    ),
                );
            }
            Ok(None) => {
                // The other session will push when it exits.
                return push_refused(
                    paths,
                    args.dry_run,
                    "the database is in use by another ptm process",
                );
            }
            Err(e) => {
                return Err(e)
                    .context("WAL status check failed; refusing to push a possibly stale snapshot");
            }
        }
    }

    // ── Hash check: skip upload if nothing changed ────────────────────────
    // `snapshot_bytes`, not `std::fs::read`: committed data can still be sitting
    // in `ptm.db-wal`, so the raw file may be missing everything since the last
    // checkpoint (finding #2).  The snapshot is both what gets hashed and what
    // gets packed, so the change test and the upload can never disagree.
    let db_bytes = db::snapshot_bytes(&paths.db)
        .context("Failed to snapshot ptm.db for upload")?;
    let dict_bytes = std::fs::read(&paths.dict).ok();
    let current_hash = compute_hash(&db_bytes, dict_bytes.as_deref());

    let stored_hash = std::fs::read_to_string(&paths.db_hash)
        .unwrap_or_default();
    let stored_hash = stored_hash.trim();

    // A pending `.needs_push` marker means the local state is known to be
    // unpushed even if the hash matches — `--start` may have re-seeded the
    // baseline (finding #3), or an earlier `--end` failed to upload.  The
    // marker outranks the hash.
    let marked = needs_push(paths);
    if marked {
        log_entry(
            paths,
            "Local data is marked for upload; pushing regardless of the db hash",
        );
    }

    match upload_decision(marked, stored_hash, &current_hash) {
        UploadDecision::Skip => {
            log_entry(paths, "No changes detected since last sync; skipping upload");
            return Ok(());
        }
        UploadDecision::Push => {}
        UploadDecision::PushBecauseMarked => {
            // Already logged above; the point is that the marker won.
        }
    }

    // ── Check remote ETag before deciding to push ─────────────────────────
    let bucket = args.bucket_name.as_deref().unwrap_or_default();
    let prefix = args.prefix.as_deref().unwrap_or_default();
    let s3 = S3Client::new(bucket, prefix)
        .context("Failed to connect to S3")?;

    let remote_etag = s3.head_etag().context("Failed to check remote backup")?;

    let stored_pulled_etag = read_etag_file(&paths.last_pulled_etag);
    let stored_pushed_etag = read_etag_file(&paths.last_pushed_etag);

    // Guard: if a remote backup exists but we have never pulled it, refuse to
    // overwrite it unless --local-is-master is set OR the remote is a backup
    // that we ourselves pushed (remote_etag == last_pushed_etag).
    let we_own_the_remote = stored_pushed_etag.is_some() && stored_pushed_etag == remote_etag;
    if stored_pulled_etag.is_none() && remote_etag.is_some() && !args.local_is_master && !we_own_the_remote {
        mark_refusal(paths, args.dry_run, "a push was refused because the remote has never been pulled");
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
        mark_refusal(paths, args.dry_run, "a push was refused because the remote has changed");
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
    let new_etag = match s3.upload(&encrypted) {
        Ok(etag) => etag,
        Err(e) => {
            // The upload itself failed: this session's work is not in S3, and
            // the shell wrapper's `trap … EXIT` will not look at the exit code.
            // Record the marker so the next `--end` pushes regardless of what
            // `.db_hash` says by then.
            mark_needs_push(paths, "the upload to S3 failed");
            return Err(e).context("Failed to upload backup to S3");
        }
    };

    write_etag_file(&paths.last_pushed_etag, &new_etag)
        .context("Failed to write .last_pushed_etag")?;

    // Update the db hash to reflect the successfully pushed state.
    // This is the only point (along with do_pull) where .db_hash is updated.
    // Keeping it here — after a confirmed upload — means any earlier failure
    // leaves the old hash in place, causing the next --end to retry the push.
    std::fs::write(&paths.db_hash, &current_hash)
        .context("Failed to write .db_hash")?;

    // The state in S3 and the state on disk now match, so nothing is pending.
    // Only a confirmed upload clears this; every failure path above leaves it
    // set so the next `--end` tries again.
    clear_needs_push(paths)
        .context("Failed to clear the upload marker after a successful push")?;

    log_entry(paths, &format!("Upload complete; etag: {}", new_etag));
    log_entry(paths, "--end complete");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::snapshot_bytes;
    use rusqlite::Connection;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    /// A `Paths` rooted in a temp dir, matching what `Paths::new` derives.
    /// A 32-byte key file with owner-only permissions, in `dir`.
    fn make_key(dir: &TempDir) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join("key.bin");
        std::fs::write(&path, [7u8; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    fn make_paths(dir: &TempDir) -> Paths {
        let d = dir.path();
        Paths {
            data_dir: d.to_path_buf(),
            db: d.join("ptm.db"),
            dict: d.join("user_words.txt"),
            local_backups: d.join("local-backups"),
            last_pushed_etag: d.join(".last_pushed_etag"),
            last_pulled_etag: d.join(".last_pulled_etag"),
            db_hash: d.join(".db_hash"),
            needs_push: d.join(".needs_push"),
            sync_log: d.join("ptm-sync.log"),
        }
    }

    /// Args for `run_end`, with the key file created by the caller.
    fn args(key: &Path) -> Args {
        Args {
            start: false,
            end: true,
            bucket_name: Some("bucket".into()),
            prefix: Some("prefix".into()),
            encryption_key: key.to_string_lossy().into_owned(),
            dry_run: true,
            local_is_master: false,
            remote_is_master: false,
            block_concurrent: false,
            merge: false,
            from: None,
        }
    }

    /// A database whose only copy of `n` rows is in its `-wal`.
    fn crashed_wal_db(db_path: &Path, n: i64) {
        let conn = Connection::open(db_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; \
             PRAGMA wal_autocheckpoint=0; \
             CREATE TABLE t (x INTEGER);",
        )
        .unwrap();
        for i in 0..n {
            conn.execute("INSERT INTO t VALUES (?1)", [i]).unwrap();
        }
        let wal = std::fs::read(crate::db::wal_path(db_path)).unwrap();
        let shm = std::fs::read(crate::db::shm_path(db_path)).unwrap();
        let main = std::fs::read(db_path).unwrap();
        drop(conn);
        std::fs::write(db_path, main).unwrap();
        std::fs::write(crate::db::wal_path(db_path), wal).unwrap();
        std::fs::write(crate::db::shm_path(db_path), shm).unwrap();
    }

    fn rows_in(db_path: &Path, sql: &str) -> i64 {
        let conn = Connection::open(db_path).unwrap();
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn count_in_bytes(bytes: &[u8], dir: &TempDir, tag: &str) -> i64 {
        let path: PathBuf = dir.path().join(format!("{}.db", tag));
        std::fs::write(&path, bytes).unwrap();
        rows_in(&path, "SELECT count(*) FROM t")
    }

    // ── current_db_bytes ──────────────────────────────────────────────────────

    #[test]
    fn current_db_bytes_includes_data_that_is_only_in_the_wal() {
        // Finding #2: this is the value that gets hashed and packed.  Reading
        // ptm.db directly would return a database with no table at all.
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        crashed_wal_db(&paths.db, 9);

        let bytes = current_db_bytes(&paths);
        assert_eq!(count_in_bytes(&bytes, &dir, "snap"), 9);
    }

    #[test]
    fn current_db_bytes_is_identical_before_and_after_a_checkpoint() {
        // The same logical state must hash the same way, or --end and --start
        // disagree about whether anything changed.
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        crashed_wal_db(&paths.db, 5);

        let before = current_db_bytes(&paths);
        crate::db::check_and_checkpoint_wal(&paths.db).unwrap().unwrap();
        let after = current_db_bytes(&paths);
        assert_eq!(before, after);
    }

    #[test]
    fn current_db_bytes_is_empty_when_the_database_is_absent() {
        let dir = TempDir::new().unwrap();
        assert!(current_db_bytes(&make_paths(&dir)).is_empty());
    }

    #[test]
    fn current_db_bytes_is_empty_for_an_unreadable_database() {
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        std::fs::write(&paths.db, b"not a database").unwrap();
        assert!(current_db_bytes(&paths).is_empty());
    }

    // ── run_end ───────────────────────────────────────────────────────────────

    #[test]
    fn run_end_checkpoints_the_wal_before_hashing() {
        // A --dry-run --end still does the WAL handling, and must leave the WAL
        // empty so the bytes that would be packed are complete (finding #1b/#2).
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        crashed_wal_db(&paths.db, 20);

        // Record the current state as already synced, so --end stops at the
        // change check instead of trying to reach S3.
        let synced = compute_hash(&current_db_bytes(&paths), None);
        std::fs::write(&paths.db_hash, &synced).unwrap();

        let mut a = args(&make_key(&dir));
        a.dry_run = true;
        run_end(&a, &paths).unwrap();

        assert_eq!(crate::db::wal_size(&paths.db), 0, "the WAL must be truncated");
        // Everything the push would have uploaded is in the main file now.
        let packed = std::fs::read(&paths.db).unwrap();
        assert_eq!(count_in_bytes(&packed, &dir, "packed"), 20);
    }

    #[test]
    fn run_end_refuses_to_push_when_the_wal_cannot_be_checkpointed() {
        // Finding #1b: a busy checkpoint is not an error to SQLite, so it used
        // to be reported as success and a stale snapshot was uploaded.
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        crashed_wal_db(&paths.db, 3);

        // Pin a read mark from this process; POSIX locks are per-process, so a
        // second connection here *can* make the checkpoint busy.
        let pinner = Connection::open(&paths.db).unwrap();
        pinner.execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;").unwrap();

        let mut a = args(&make_key(&dir));
        a.dry_run = true;
        let err = run_end(&a, &paths).unwrap_err();
        pinner.execute_batch("COMMIT").unwrap();

        let msg = format!("{:#}", err);
        assert!(
            msg.contains("in use") || msg.contains("checkpointed"),
            "expected a refusal about the WAL, got: {}",
            msg
        );
        assert_ne!(
            crate::db::wal_size(&paths.db),
            0,
            "the refusal must not pretend the WAL was emptied"
        );
    }

    #[test]
    fn run_end_still_refuses_under_local_is_master_when_the_db_is_in_use() {
        // --local-is-master is about S3 authority, not about local concurrency.
        // A user who passed it asked for a forced push; skipping one silently
        // and exiting 0 would be the worst possible answer.
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        crashed_wal_db(&paths.db, 3);

        let pinner = Connection::open(&paths.db).unwrap();
        pinner.execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;").unwrap();

        let mut a = args(&make_key(&dir));
        a.local_is_master = true;
        let err = run_end(&a, &paths).unwrap_err();

        pinner.execute_batch("COMMIT").unwrap();
        assert!(
            format!("{:#}", err).contains("in use") || format!("{:#}", err).contains("checkpointed"),
            "--local-is-master must not turn a refused push into a success: {:#}",
            err
        );
    }

    #[test]
    fn run_end_skips_everything_under_remote_is_master() {
        // A busy database is not an error for a caller that has declared the
        // remote authoritative: it is not going to push at all.
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        crashed_wal_db(&paths.db, 3);

        let pinner = Connection::open(&paths.db).unwrap();
        pinner.execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;").unwrap();

        let mut a = args(&make_key(&dir));
        a.remote_is_master = true;
        run_end(&a, &paths).unwrap();

        pinner.execute_batch("COMMIT").unwrap();
    }

    #[test]
    fn run_end_requires_an_existing_database() {
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        let err = run_end(&args(&make_key(&dir)), &paths).unwrap_err();
        assert!(format!("{:#}", err).contains("nothing to back up"));
    }


    // ── the unsynced-changes marker (finding #3) ──────────────────────────────

    /// A database with one project, closed cleanly, so the main file is
    /// complete and `snapshot_bytes` has something to hash.
    fn db_with_project(paths: &Paths, title: &str) {
        let conn = Connection::open(&paths.db).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (
                 id TEXT PRIMARY KEY, title TEXT NOT NULL,
                 description TEXT NOT NULL DEFAULT '',
                 sort_order REAL NOT NULL DEFAULT 0.0,
                 archived INTEGER NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO projects VALUES (?1, ?2, '', 1.0, 0,
                 '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
            rusqlite::params![format!("p-{}", title), title],
        )
        .unwrap();
    }

    fn hash_now(paths: &Paths) -> String {
        compute_hash(&current_db_bytes(paths), None)
    }

    #[test]
    fn a_marker_outranks_a_matching_hash() {
        // The exact shape of finding #3: the hash says "already synced" while
        // the state is known to be unpushed.  The marker has to win, or the
        // merge result is never uploaded.
        assert_eq!(
            upload_decision(true, "abc", "abc"),
            UploadDecision::PushBecauseMarked
        );
        assert_eq!(
            upload_decision(true, "", ""),
            UploadDecision::PushBecauseMarked
        );
    }

    #[test]
    fn without_a_marker_the_hash_decides() {
        assert_eq!(upload_decision(false, "abc", "abc"), UploadDecision::Skip);
        assert_eq!(upload_decision(false, "abc", "def"), UploadDecision::Push);
        // No baseline at all means "never synced", which always pushes.
        assert_eq!(upload_decision(false, "", "abc"), UploadDecision::Push);
    }

    #[test]
    fn mark_and_clear_round_trip() {
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        assert!(!needs_push(&paths));

        mark_needs_push(&paths, "merged");
        assert!(needs_push(&paths), "the marker must be visible to --end");

        clear_needs_push(&paths).unwrap();
        assert!(!needs_push(&paths));
        // Clearing twice is not an error: --end may run on a clean tree.
        clear_needs_push(&paths).unwrap();
    }

    #[test]
    fn an_empty_marker_file_still_counts() {
        // Existence is the signal, not the contents: a truncated or empty file
        // left by a crash must not be read as "nothing to push".
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        std::fs::write(&paths.needs_push, b"").unwrap();
        assert!(needs_push(&paths));
    }

    #[test]
    fn run_start_does_not_seed_the_hash_while_a_push_is_pending() {
        // Finding #3 end to end at the function level: --merge (or a failed
        // --end) marks the data, --start runs, and the unpushed state must not
        // be recorded as the sync baseline.
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        db_with_project(&paths, "merged");
        mark_needs_push(&paths, "the local database was merged");

        let mut a = args(&make_key(&dir));
        a.start = true;
        a.end = false;
        a.local_is_master = true; // skip the pull; this test is about the seed
        a.dry_run = false;
        run_start(&a, &paths).unwrap();

        assert!(
            !paths.db_hash.exists(),
            ".db_hash must stay absent so the next --end sees work to do"
        );
        assert!(needs_push(&paths), "the marker must survive --start");
    }

    #[test]
    fn run_start_still_seeds_the_hash_on_a_genuine_first_launch() {
        // The guard must not break the case it exists for: a brand-new data
        // directory has no baseline and needs one.
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        db_with_project(&paths, "first");

        let mut a = args(&make_key(&dir));
        a.start = true;
        a.end = false;
        a.local_is_master = true;
        a.dry_run = false;
        run_start(&a, &paths).unwrap();

        assert!(paths.db_hash.exists(), "a first launch needs a baseline");
        assert_eq!(
            std::fs::read_to_string(&paths.db_hash).unwrap().trim(),
            hash_now(&paths)
        );
    }

    #[test]
    fn a_refused_push_marks_the_data_for_the_next_run() {
        // Finding #3's other half: --end exits non-zero, but the wrapper's
        // `trap … EXIT` throws that away, so the refusal itself has to leave a
        // trace or the session's work is lost.
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        db_with_project(&paths, "unsaved");

        // A WAL has to exist for --end to probe, and a read mark held by
        // another connection in this process makes the checkpoint busy.
        let pinner = Connection::open(&paths.db).unwrap();
        pinner
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA wal_autocheckpoint=0;
                 INSERT INTO projects VALUES ('p2', 'x', '', 2.0, 0,
                     '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z');
                 BEGIN;
                 SELECT count(*) FROM sqlite_master;",
            )
            .unwrap();
        assert!(crate::db::wal_size(&paths.db) > 0, "precondition: a live WAL");

        // Not a dry run: --dry-run must not touch the data directory, and this
        // test is about the marker a *real* refusal leaves behind.  The refusal
        // happens at the WAL check, well before any S3 call.
        let mut a = args(&make_key(&dir));
        a.dry_run = false;
        let err = run_end(&a, &paths).unwrap_err();
        pinner.execute_batch("COMMIT").unwrap();

        let msg = format!("{:#}", err);
        assert!(
            msg.contains("in use") || msg.contains("checkpointed"),
            "expected a WAL refusal, got: {}",
            msg
        );
        assert!(
            needs_push(&paths),
            "a refused push must leave the data marked for the next --end"
        );
    }

    #[test]
    fn a_dry_run_end_neither_sets_nor_clears_the_marker() {
        // --dry-run promises to leave the data directory alone, so it must not
        // write the marker on a refusal — and only a confirmed upload may clear
        // it, which a dry run never performs.
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        db_with_project(&paths, "pending");

        // (a) a pending marker survives a dry run
        mark_needs_push(&paths, "merged");
        let mut a = args(&make_key(&dir));
        a.dry_run = true;
        let _ = run_end(&a, &paths);
        assert!(needs_push(&paths), "a dry run must not clear the marker");

        // (b) a refused push under --dry-run does not create one
        std::fs::remove_file(&paths.needs_push).unwrap();
        let pinner = Connection::open(&paths.db).unwrap();
        pinner
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA wal_autocheckpoint=0;
                 INSERT INTO projects VALUES ('p2', 'x', '', 2.0, 0,
                     '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z');
                 BEGIN;
                 SELECT count(*) FROM sqlite_master;",
            )
            .unwrap();
        let _ = run_end(&a, &paths);
        pinner.execute_batch("COMMIT").unwrap();
        assert!(
            !needs_push(&paths),
            "a --dry-run refusal must not touch the data directory"
        );
    }

    // ── snapshot/pack agreement ───────────────────────────────────────────────

    #[test]
    fn the_hash_and_the_uploaded_bytes_come_from_the_same_snapshot() {
        // --end hashes `db_bytes` and packs the same `db_bytes`.  If they came
        // from different reads (raw file vs snapshot) the change test and the
        // upload could disagree forever, re-uploading or never uploading.
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);
        crashed_wal_db(&paths.db, 11);

        let hashed = snapshot_bytes(&paths.db).unwrap();
        crate::db::check_and_checkpoint_wal(&paths.db).unwrap().unwrap();
        let packed = snapshot_bytes(&paths.db).unwrap();

        assert_eq!(hashed, packed);
        assert_eq!(
            compute_hash(&hashed, None),
            compute_hash(&packed, None),
            "the same state must hash identically before and after a checkpoint"
        );
    }
}
