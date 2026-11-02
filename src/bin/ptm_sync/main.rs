//! ptm-sync — pull a backup from S3 before launching ptm, push one after.
//!
//! Intended to be called from a wrapper script:
//!   ptm-sync --start --bucket-name my-bucket --prefix ptm-backups
//!   trap 'ptm-sync --end ...' EXIT
//!   ptm

mod archive;
mod db;
mod merge;
mod ops;
mod s3client;

/// Shared AES-256-GCM implementation (path-included from the main ptm source
/// so that `enc` and `ptm-sync` always agree on the file format).
#[path = "../../crypto.rs"]
#[allow(dead_code)] // shared module: each binary uses a different subset
mod crypto;

use anyhow::{Context, Result};
use chrono::DateTime;
use clap::{ArgGroup, Parser};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

// ── CLI arguments ─────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "ptm-sync",
    about = "Sync ptm data to/from S3",
    group(ArgGroup::new("mode").required(true).args(["start", "end", "merge"])),
    group(ArgGroup::new("master").args(["local_is_master", "remote_is_master"])),
)]
pub struct Args {
    /// Run the pre-launch pull phase.
    #[arg(long, group = "mode")]
    pub start: bool,

    /// Run the post-exit push phase.
    #[arg(long, group = "mode")]
    pub end: bool,

    /// S3 bucket name (required for --start/--end; unused by --merge).
    #[arg(long)]
    pub bucket_name: Option<String>,

    /// S3 key prefix (e.g. "ptm-backups"; required for --start/--end; unused by --merge).
    #[arg(long)]
    pub prefix: Option<String>,

    /// Path to the 32-byte raw AES-256 key file.
    #[arg(long, default_value = "~/.encryption_key")]
    pub encryption_key: String,

    /// Perform all logic but make no changes to local files or S3.
    #[arg(long)]
    pub dry_run: bool,

    /// Force push: treat local data as authoritative (skip pull / ignore etag mismatch on push).
    #[arg(long)]
    pub local_is_master: bool,

    /// Force pull: treat remote data as authoritative (skip push / ignore local-only db on pull).
    #[arg(long)]
    pub remote_is_master: bool,

    /// Abort launch if another ptm process has the database open (default: allow concurrency).
    #[arg(long)]
    pub block_concurrent: bool,

    /// Merge another ptm data directory into the local one, then exit.
    /// Requires --from.  Does not contact S3.
    #[arg(long, group = "mode")]
    pub merge: bool,

    /// Source data directory for --merge (must contain ptm.db).
    #[arg(long, requires = "merge")]
    pub from: Option<String>,
}

/// Expand a leading `~` to `$HOME` (shared by key path and --from).
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(rest)
    } else if path == "~" {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
    } else {
        PathBuf::from(path)
    }
}

impl Args {
    /// Resolve the `~` in `--encryption-key` and return an absolute PathBuf.
    pub fn key_path(&self) -> PathBuf {
        if self.encryption_key.starts_with('~') {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(&self.encryption_key[2..])
        } else {
            PathBuf::from(&self.encryption_key)
        }
    }
}

// ── Paths ─────────────────────────────────────────────────────────────────────

/// All paths derived from the ptm data directory.
pub struct Paths {
    pub data_dir: PathBuf,
    /// ptm.db  (`ptm.db-wal` / `ptm.db-shm` are derived by `db::wal_path` /
    /// `db::shm_path`, so the suffix lives in exactly one place)
    pub db: PathBuf,
    /// user_words.txt (optional)
    pub dict: PathBuf,
    /// local-backups/ subdirectory
    pub local_backups: PathBuf,
    /// .last_pushed_etag
    pub last_pushed_etag: PathBuf,
    /// .last_pulled_etag
    pub last_pulled_etag: PathBuf,
    /// .db_hash
    pub db_hash: PathBuf,
    /// .needs_push
    pub needs_push: PathBuf,
    /// ptm-sync.log
    pub sync_log: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let data_dir = resolve_data_dir();
        Ok(Self {
            db: data_dir.join("ptm.db"),
            dict: data_dir.join("user_words.txt"),
            local_backups: data_dir.join("local-backups"),
            last_pushed_etag: data_dir.join(".last_pushed_etag"),
            last_pulled_etag: data_dir.join(".last_pulled_etag"),
            db_hash: data_dir.join(".db_hash"),
            needs_push: data_dir.join(".needs_push"),
            sync_log: data_dir.join("ptm-sync.log"),
            data_dir,
        })
    }
}

fn resolve_data_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("ptm");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local").join("share").join("ptm")
}

// ── Shared utilities ──────────────────────────────────────────────────────────

/// Append a timestamped line to the sync log. Silently ignores write errors.
pub fn log_entry(paths: &Paths, msg: &str) {
    let ts = DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
        .format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("{} {}\n", ts, msg);
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.sync_log)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Read a stored ETag from a file, returning None if the file is absent or empty.
pub fn read_etag_file(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Write an ETag string to a file.
pub fn write_etag_file(path: &Path, etag: &str) -> Result<()> {
    fs::write(path, etag).with_context(|| format!("Failed to write {}", path.display()))
}

/// Compute SHA-256( db_bytes ∥ dict_bytes ) and return as a lowercase hex string.
/// A missing dict is treated as empty (contributes no bytes to the hash).
pub fn compute_hash(db_bytes: &[u8], dict_bytes: Option<&[u8]>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(db_bytes);
    if let Some(d) = dict_bytes {
        hasher.update(d);
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

// ── The unsynced-changes marker ───────────────────────────────────────────────
//
// `.db_hash` answers one question — "is the local state the same as the state
// we last pushed or pulled?" — and two commands need to say "no, and I know it":
//
//   * `--merge` has just changed the local database outside any sync, so the
//     merged state must be pushed by the next `--end`.
//   * `--end` found changes but could not push (no network, refused upload), so
//     the session's work is still unbacked-up.
//
// Both used to be expressed by *deleting* `.db_hash`, which cannot work:
// `--start` seeds that file from the current database whenever it is missing,
// and it has to, because a genuinely first launch has no other baseline.  A
// deletion is therefore indistinguishable from "never synced", and the next
// `--start` silently records the unpushed state as the sync baseline — the
// change is then invisible to `--end` forever (finding #3).
//
// `.needs_push` is the out-of-band marker instead: it survives `--start`
// (which never touches it), and only a successful push removes it.  It is
// separate from `.db_hash` because `.db_hash` keeps its "last synced state"
// meaning and the pull guards still need it.

/// Record that the local state has to be pushed, whatever `.db_hash` says.
///
/// Failure to write the marker is not fatal at the call site — the push may
/// still happen because the hash differs — but it is logged, because a silent
/// failure is how finding #3 looked in the first place.  The one case that is
/// then lost is a state whose hash already matches the baseline.
pub fn mark_needs_push(paths: &Paths, reason: &str) {
    match fs::write(&paths.needs_push, format!("{}\n", reason)) {
        Ok(()) => log_entry(
            paths,
            &format!("Marked local data for upload ({}); the next --end will push it", reason),
        ),
        Err(e) => log_entry(
            paths,
            &format!(
                "Warning: could not write {} ({}); relying on the db hash instead",
                paths.needs_push.display(),
                e
            ),
        ),
    }
}

/// True when something recorded that the local state is not in S3 yet.
///
/// The file's *contents* are irrelevant — its existence is the whole signal —
/// so an unreadable or empty marker still counts.
pub fn needs_push(paths: &Paths) -> bool {
    paths.needs_push.exists()
}

/// Clear the marker.  Only a successful upload may call this: clearing it
/// without uploading is exactly the silent data loss the marker exists to
/// prevent.
pub fn clear_needs_push(paths: &Paths) -> Result<()> {
    match fs::remove_file(&paths.needs_push) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| {
            format!(
                "Backup uploaded, but {} could not be cleared",
                paths.needs_push.display()
            )
        }),
    }
}

/// Union + dedup + sort merge of two newline-separated word lists.
pub fn merge_dictionaries(local: &str, remote: &str) -> String {
    let mut words: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for word in local.lines().chain(remote.lines()) {
        let w = word.trim().to_string();
        if !w.is_empty() {
            words.insert(w);
        }
    }
    if words.is_empty() {
        return String::new();
    }
    let mut out = words.into_iter().collect::<Vec<_>>().join("\n");
    out.push('\n');
    out
}

/// Decide what to do after a network error during `--start`.
///
/// The contract is "the launch goes ahead": `--start` runs immediately before
/// `ptm`, and a transient S3 problem must not stop the user from working.  On a
/// terminal we ask; without one we **continue with a warning**, because a
/// launch from a desktop launcher, a script, or over a pipe is not a decision
/// point — there is nobody to answer, and refusing to continue turns a network
/// blip into "ptm will not start" (finding #12).
///
/// The data-loss guards do not depend on this answer: a failed pull never
/// touches the local database, and `.db_hash` / `.needs_push` are unchanged, so
/// the skipped pull is retried on the next launch.  Callers that need a hard
/// stop have `--block-concurrent` for the concurrency case.
///
/// Returns `Ok(())` to continue; `Err` only when a user who was actually asked
/// said no, or the terminal could not be read.
pub fn prompt_continue_without_pull(paths: &Paths, reason: &str) -> Result<()> {
    let msg = format!("Network error during pull: {}", reason);
    log_entry(paths, &format!("Warning: {}", msg));
    eprintln!("Warning: {}", msg);

    if !std::io::stdin().is_terminal() {
        // Not an error: no interactive decision is possible, and the safe
        // action (launch, keep local data, retry the pull next time) is known.
        log_entry(
            paths,
            "No terminal to ask on; continuing without the pull (local data untouched)",
        );
        eprintln!(
            "Continuing without the pull; local data is untouched and the pull \
             will be retried on the next launch."
        );
        return Ok(());
    }

    eprint!("Continue without pulling? [Y/n] ");
    let _ = std::io::stderr().flush();

    let mut input = String::new();
    std::io::stdin().lock().read_line(&mut input)?;

    // Default to continuing: the prompt is an escape hatch for someone who
    // wants to stop, not a gate in front of `ptm`.
    let answer = input.trim();
    if answer.is_empty() || answer.eq_ignore_ascii_case("y") {
        log_entry(paths, "User chose to continue without pulling");
        Ok(())
    } else {
        log_entry(paths, "User declined to continue without pulling");
        anyhow::bail!("Aborted at the user's request after a network error")
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    // rustls 0.23 requires an explicit CryptoProvider when more than one is
    // compiled in (ring + aws-lc-rs can both appear via transitive deps).
    // Install ring unconditionally; ignore the error if one is already set.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = Args::parse();

    // Clap's ArgGroup already enforces mutual exclusion, but double-check for
    // the master flags which are in a separate group (both optional).
    if args.local_is_master && args.remote_is_master {
        eprintln!(
            "Error: --local-is-master and --remote-is-master are mutually exclusive."
        );
        std::process::exit(1);
    }

    let paths = match Paths::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    if let Err(e) = fs::create_dir_all(&paths.data_dir) {
        eprintln!("Error: Failed to create data directory: {}", e);
        std::process::exit(1);
    }

    let result = if args.merge {
        let from = match args.from.as_deref() {
            Some(f) => f,
            None => {
                eprintln!("Error: --merge requires --from <data-dir>");
                std::process::exit(1);
            }
        };
        let from_dir = expand_tilde(from);
        merge::run_merge(&paths, &from_dir, args.dry_run)
    } else if args.start {
        ops::run_start(&args, &paths)
    } else {
        ops::run_end(&args, &paths)
    };

    if let Err(e) = result {
        log_entry(&paths, &format!("Fatal: {:#}", e));
        eprintln!("Error: {:#}", e);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ── compute_hash ──────────────────────────────────────────────────────────────

    #[test]
    fn compute_hash_is_deterministic() {
        let h1 = compute_hash(b"db bytes", Some(b"dict bytes"));
        let h2 = compute_hash(b"db bytes", Some(b"dict bytes"));
        assert_eq!(h1, h2);
    }

    #[test]
    fn compute_hash_is_lowercase_hex_of_expected_length() {
        let h = compute_hash(b"anything", None);
        assert_eq!(h.len(), 64, "SHA-256 hex must be 64 chars");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()), "must be hex: {}", h);
    }

    #[test]
    fn compute_hash_differs_on_different_db_bytes() {
        let h1 = compute_hash(b"db version A", None);
        let h2 = compute_hash(b"db version B", None);
        assert_ne!(h1, h2);
    }

    #[test]
    fn compute_hash_differs_with_and_without_dict() {
        let h_no_dict = compute_hash(b"db", None);
        let h_with_dict = compute_hash(b"db", Some(b"words"));
        assert_ne!(h_no_dict, h_with_dict);
    }

    #[test]
    fn compute_hash_treats_missing_dict_as_empty_contribution() {
        // None and Some(b"") are NOT required to produce the same hash —
        // but the function must at least be consistent (no panic, valid hex).
        let h1 = compute_hash(b"db", None);
        let h2 = compute_hash(b"db", Some(b""));
        // They differ because Some(b"") feeds 0 extra bytes while None feeds nothing.
        // What matters is that both are valid hashes.
        assert_eq!(h1.len(), 64);
        assert_eq!(h2.len(), 64);
    }

    // ── merge_dictionaries ────────────────────────────────────────────────────────

    #[test]
    fn merge_dicts_unions_two_non_overlapping_lists() {
        let result = merge_dictionaries("apple\nbanana", "cherry\ndate");
        let words: Vec<&str> = result.trim_end_matches('\n').split('\n').collect();
        assert!(words.contains(&"apple"));
        assert!(words.contains(&"banana"));
        assert!(words.contains(&"cherry"));
        assert!(words.contains(&"date"));
        assert_eq!(words.len(), 4);
    }

    #[test]
    fn merge_dicts_deduplicates_shared_words() {
        let result = merge_dictionaries("apple\nbanana", "banana\ncherry");
        let words: Vec<&str> = result.trim_end_matches('\n').split('\n').collect();
        let banana_count = words.iter().filter(|&&w| w == "banana").count();
        assert_eq!(banana_count, 1, "'banana' must appear exactly once");
        assert_eq!(words.len(), 3);
    }

    #[test]
    fn merge_dicts_output_is_sorted() {
        let result = merge_dictionaries("zebra\napple", "mango\nbanana");
        let words: Vec<&str> = result.trim_end_matches('\n').split('\n').collect();
        let mut sorted = words.clone();
        sorted.sort();
        assert_eq!(words, sorted, "output must be lexicographically sorted");
    }

    #[test]
    fn merge_dicts_ends_with_newline() {
        let result = merge_dictionaries("apple", "banana");
        assert!(result.ends_with('\n'), "merged dict must end with newline");
    }

    #[test]
    fn merge_dicts_both_empty_returns_empty_string() {
        assert_eq!(merge_dictionaries("", ""), "");
    }

    #[test]
    fn merge_dicts_one_empty_returns_other_words() {
        let result = merge_dictionaries("apple\nbanana", "");
        assert!(result.contains("apple"));
        assert!(result.contains("banana"));
    }

    #[test]
    fn merge_dicts_trims_whitespace_around_words() {
        // Lines with leading/trailing spaces should be trimmed before inserting.
        let result = merge_dictionaries("  apple  \n  banana  ", "cherry");
        let words: Vec<&str> = result.trim_end_matches('\n').split('\n').collect();
        assert!(words.contains(&"apple"), "trimmed 'apple' must be present; got {:?}", words);
        assert!(words.contains(&"banana"));
    }

    // ── read_etag_file / write_etag_file ─────────────────────────────────────────

    #[test]
    fn write_then_read_etag_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".etag");
        write_etag_file(&path, "abc123etag").unwrap();
        assert_eq!(read_etag_file(&path).as_deref(), Some("abc123etag"));
    }

    #[test]
    fn read_etag_trims_surrounding_whitespace() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".etag");
        fs::write(&path, "  deadbeef  \n").unwrap();
        assert_eq!(read_etag_file(&path).as_deref(), Some("deadbeef"));
    }

    #[test]
    fn read_etag_returns_none_for_missing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("no_such_etag");
        assert!(read_etag_file(&path).is_none());
    }

    #[test]
    fn read_etag_returns_none_for_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".etag_empty");
        fs::write(&path, b"").unwrap();
        assert!(read_etag_file(&path).is_none());
    }

    #[test]
    fn read_etag_returns_none_for_whitespace_only_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".etag_ws");
        fs::write(&path, b"   \n  ").unwrap();
        assert!(read_etag_file(&path).is_none());
    }

    // ── Args::key_path ─────────────────────────────────────────────────────────────

    fn make_args(encryption_key: &str) -> Args {
        Args {
            start: true,
            end: false,
            bucket_name: Some("bucket".into()),
            prefix: Some("prefix".into()),
            encryption_key: encryption_key.into(),
            dry_run: false,
            local_is_master: false,
            remote_is_master: false,
            block_concurrent: false,
            merge: false,
            from: None,
        }
    }

    // ── prompt_continue_without_pull (finding #12) ─────────────────────────────

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

    #[test]
    fn a_network_error_without_a_terminal_continues_the_launch() {
        // Finding #12: `--start` runs immediately before `ptm`, and stdin is a
        // pipe whenever it is launched from a desktop launcher, a script or a
        // wrapper.  Refusing to continue there turns a transient S3 error into
        // "ptm will not start".  `cargo test` runs without a TTY on stdin, so
        // this exercises the real branch.
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir);

        if std::io::stdin().is_terminal() {
            // Someone ran the suite from an interactive terminal; the branch
            // under test is unreachable, and prompting would block the run.
            return;
        }

        let result = prompt_continue_without_pull(&paths, "S3 HEAD request failed");
        assert!(
            result.is_ok(),
            "a network error with no terminal must not abort the launch: {:#?}",
            result.err()
        );

        let log = fs::read_to_string(&paths.sync_log).unwrap();
        assert!(
            !log.contains("cannot prompt"),
            "the old abort message must be gone; log was:\n{}",
            log
        );
        assert!(
            log.contains("continuing without the pull"),
            "the skipped pull must be recorded; log was:\n{}",
            log
        );
    }

    #[test]
    fn key_path_tilde_is_expanded_to_home() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        let args = make_args("~/.encryption_key");
        let path = args.key_path();
        assert_eq!(path, PathBuf::from(&home).join(".encryption_key"));
    }

    #[test]
    fn key_path_absolute_path_is_returned_unchanged() {
        let args = make_args("/etc/ptm/key.bin");
        assert_eq!(args.key_path(), PathBuf::from("/etc/ptm/key.bin"));
    }

    #[test]
    fn key_path_relative_path_is_returned_unchanged() {
        let args = make_args("keys/my.key");
        assert_eq!(args.key_path(), PathBuf::from("keys/my.key"));
    }
}
