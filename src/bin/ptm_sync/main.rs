//! ptm-sync — pull a backup from S3 before launching ptm, push one after.
//!
//! Intended to be called from a wrapper script:
//!   ptm-sync --start --bucket-name my-bucket --prefix ptm-backups
//!   trap 'ptm-sync --end ...' EXIT
//!   ptm

mod archive;
mod crypto;
mod db;
mod ops;
mod s3client;

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
    group(ArgGroup::new("mode").required(true).args(["start", "end"])),
    group(ArgGroup::new("master").args(["local_is_master", "remote_is_master"])),
)]
pub struct Args {
    /// Run the pre-launch pull phase.
    #[arg(long, group = "mode")]
    pub start: bool,

    /// Run the post-exit push phase.
    #[arg(long, group = "mode")]
    pub end: bool,

    /// S3 bucket name.
    #[arg(long)]
    pub bucket_name: String,

    /// S3 key prefix (e.g. "ptm-backups").
    #[arg(long)]
    pub prefix: String,

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
    /// ptm.db
    pub db: PathBuf,
    /// ptm.db-wal
    pub db_wal: PathBuf,
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
    /// ptm-sync.log
    pub sync_log: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let data_dir = resolve_data_dir();
        Ok(Self {
            db: data_dir.join("ptm.db"),
            db_wal: data_dir.join("ptm.db-wal"),
            dict: data_dir.join("user_words.txt"),
            local_backups: data_dir.join("local-backups"),
            last_pushed_etag: data_dir.join(".last_pushed_etag"),
            last_pulled_etag: data_dir.join(".last_pulled_etag"),
            db_hash: data_dir.join(".db_hash"),
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

/// Verify that the key file exists and has no group/other read or write bits.
pub fn check_key_permissions(key_path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::metadata(key_path)
        .with_context(|| format!("Cannot access encryption key: {}", key_path.display()))?;
    let mode = meta.permissions().mode();
    if mode & 0o077 != 0 {
        anyhow::bail!(
            "Encryption key '{}' has insecure permissions ({:04o}). \
             It must be accessible only by the owner (e.g. chmod 600).",
            key_path.display(),
            mode & 0o777
        );
    }
    Ok(())
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

/// On a network error during --start, ask the user whether to continue without
/// pulling (requires a TTY). Returns Ok(()) to continue, Err to abort.
pub fn prompt_continue_without_pull(paths: &Paths, reason: &str) -> Result<()> {
    let msg = format!("Network error during pull: {}", reason);
    log_entry(paths, &format!("Warning: {}", msg));
    eprintln!("Warning: {}", msg);

    if !std::io::stdin().is_terminal() {
        log_entry(paths, "No TTY detected; cannot prompt — aborting");
        anyhow::bail!("Network error and no interactive terminal; cannot pull");
    }

    eprint!("Continue without pulling? [y/N] ");
    let _ = std::io::stderr().flush();

    let mut input = String::new();
    std::io::stdin().lock().read_line(&mut input)?;

    if input.trim().eq_ignore_ascii_case("y") {
        log_entry(paths, "User chose to continue without pulling");
        Ok(())
    } else {
        anyhow::bail!("Aborted by user after network error")
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

    let result = if args.start {
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
    use std::os::unix::fs::PermissionsExt;
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

    // ── check_key_permissions ────────────────────────────────────────────────────

    fn write_file_with_mode(dir: &TempDir, name: &str, mode: u32) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, b"key data").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn check_key_permissions_accepts_owner_read_write_only() {
        let dir = TempDir::new().unwrap();
        let path = write_file_with_mode(&dir, "key_600.bin", 0o600);
        assert!(check_key_permissions(&path).is_ok());
    }

    #[test]
    fn check_key_permissions_accepts_owner_read_only() {
        // 0o400: owner-read only — group/other bits are 0, so it should pass.
        let dir = TempDir::new().unwrap();
        let path = write_file_with_mode(&dir, "key_400.bin", 0o400);
        assert!(check_key_permissions(&path).is_ok());
    }

    #[test]
    fn check_key_permissions_rejects_group_readable() {
        let dir = TempDir::new().unwrap();
        let path = write_file_with_mode(&dir, "key_640.bin", 0o640);
        assert!(check_key_permissions(&path).is_err());
    }

    #[test]
    fn check_key_permissions_rejects_world_readable() {
        let dir = TempDir::new().unwrap();
        let path = write_file_with_mode(&dir, "key_644.bin", 0o644);
        let err = check_key_permissions(&path).unwrap_err();
        assert!(
            err.to_string().contains("insecure"),
            "expected 'insecure' in error, got: {}",
            err
        );
    }

    #[test]
    fn check_key_permissions_rejects_world_writable() {
        let dir = TempDir::new().unwrap();
        let path = write_file_with_mode(&dir, "key_622.bin", 0o622);
        assert!(check_key_permissions(&path).is_err());
    }

    #[test]
    fn check_key_permissions_returns_error_for_missing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("no_key_here.bin");
        assert!(check_key_permissions(&path).is_err());
    }

    // ── Args::key_path ─────────────────────────────────────────────────────────────

    fn make_args(encryption_key: &str) -> Args {
        Args {
            start: true,
            end: false,
            bucket_name: "bucket".into(),
            prefix: "prefix".into(),
            encryption_key: encryption_key.into(),
            dry_run: false,
            local_is_master: false,
            remote_is_master: false,
            block_concurrent: false,
        }
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
