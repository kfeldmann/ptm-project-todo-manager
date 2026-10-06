//! Zip packaging and extraction, plus local backup rotation.

use anyhow::{Context, Result};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

use crate::Paths;

const DB_FILENAME: &str = "ptm.db";
const DICT_FILENAME: &str = "user_words.txt";

// ── Pack ──────────────────────────────────────────────────────────────────────

/// Create an in-memory zip archive containing `ptm.db` and, if provided,
/// `user_words.txt`.  The dict is optional — a missing dict is not an error.
pub fn pack(db_bytes: &[u8], dict_bytes: Option<&[u8]>) -> Result<Vec<u8>> {
    let cursor = Cursor::new(Vec::<u8>::new());
    let mut zip = ZipWriter::new(cursor);

    let options =
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    zip.start_file(DB_FILENAME, options)
        .context("Failed to start db file in zip")?;
    zip.write_all(db_bytes)
        .context("Failed to write db bytes to zip")?;

    if let Some(dict) = dict_bytes {
        zip.start_file(DICT_FILENAME, options)
            .context("Failed to start dict file in zip")?;
        zip.write_all(dict)
            .context("Failed to write dict bytes to zip")?;
    }

    let cursor = zip.finish().context("Failed to finalise zip archive")?;
    Ok(cursor.into_inner())
}

// ── Unpack ────────────────────────────────────────────────────────────────────

/// Extract `ptm.db` and optionally `user_words.txt` from a zip archive.
/// Returns `(db_bytes, Option<dict_bytes>)`.
/// `ptm.db` must be present; `user_words.txt` is optional.
/// Unknown entries are silently skipped.
pub fn unpack(zip_bytes: &[u8]) -> Result<(Vec<u8>, Option<Vec<u8>>)> {
    let cursor = Cursor::new(zip_bytes);
    let mut archive = ZipArchive::new(cursor).context("Failed to open zip archive")?;

    let mut db_bytes: Option<Vec<u8>> = None;
    let mut dict_bytes: Option<Vec<u8>> = None;

    for i in 0..archive.len() {
        let (name, content) = {
            let mut entry = archive
                .by_index(i)
                .with_context(|| format!("Failed to read zip entry {}", i))?;
            let name = entry.name().to_string();
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .with_context(|| format!("Failed to read zip entry '{}'", name))?;
            (name, buf)
        };

        match name.as_str() {
            DB_FILENAME => db_bytes = Some(content),
            DICT_FILENAME => dict_bytes = Some(content),
            _ => {} // ignore unknown entries
        }
    }

    let db_bytes = db_bytes.context("Zip archive does not contain 'ptm.db'")?;
    Ok((db_bytes, dict_bytes))
}

// ── Local backup ──────────────────────────────────────────────────────────────

/// Create an unencrypted local backup zip of the current db (and dict if present).
/// `timestamp` must be an ISO 8601 string with colons replaced by hyphens, e.g.
/// `"2025-01-15T10-23-45Z"`.
///
/// Returns the path to the newly created zip file.
pub fn create_local_backup(paths: &Paths, timestamp: &str) -> Result<PathBuf> {
    let db_bytes = std::fs::read(&paths.db)
        .with_context(|| format!("Cannot read {} for local backup", paths.db.display()))?;
    let dict_bytes = std::fs::read(&paths.dict).ok();

    let zip_bytes = pack(&db_bytes, dict_bytes.as_deref())
        .context("Failed to create local backup zip")?;

    std::fs::create_dir_all(&paths.local_backups)
        .context("Failed to create local-backups directory")?;

    let filename = format!("ptm-backup-{}.zip", timestamp);
    let backup_path = paths.local_backups.join(&filename);
    std::fs::write(&backup_path, &zip_bytes)
        .with_context(|| format!("Failed to write local backup: {}", backup_path.display()))?;

    Ok(backup_path)
}

/// Keep only the `keep` most recent `.zip` files in `dir`; delete older ones.
/// Files are sorted lexicographically (ISO 8601 timestamps sort correctly this way).
pub fn rotate_local_backups(dir: &Path, keep: usize) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("Cannot read local-backups dir: {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "zip"))
        .collect();

    entries.sort(); // lexicographic order = chronological for our filenames

    if entries.len() > keep {
        for old in &entries[..entries.len() - keep] {
            std::fs::remove_file(old)
                .with_context(|| format!("Failed to delete old backup: {}", old.display()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write; // for write_all on ZipWriter
    use tempfile::TempDir;
    use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

    /// Build a `Paths` struct rooted in a temp directory.
    fn make_paths(dir: &TempDir) -> crate::Paths {
        let d = dir.path();
        crate::Paths {
            data_dir: d.to_path_buf(),
            db: d.join("ptm.db"),
            db_wal: d.join("ptm.db-wal"),
            dict: d.join("user_words.txt"),
            local_backups: d.join("local-backups"),
            last_pushed_etag: d.join(".last_pushed_etag"),
            last_pulled_etag: d.join(".last_pulled_etag"),
            db_hash: d.join(".db_hash"),
            sync_log: d.join("ptm-sync.log"),
        }
    }

    // ── pack / unpack round-trips ────────────────────────────────────────────────

    #[test]
    fn pack_unpack_db_only_round_trip() {
        let db = b"fake SQLite database bytes";
        let zip = pack(db, None).unwrap();
        let (got_db, got_dict) = unpack(&zip).unwrap();
        assert_eq!(got_db.as_slice(), db.as_slice());
        assert!(got_dict.is_none());
    }

    #[test]
    fn pack_unpack_db_and_dict_round_trip() {
        let db = b"db content here";
        let dict = b"apple\nbanana\ncherry\n";
        let zip = pack(db, Some(dict)).unwrap();
        let (got_db, got_dict) = unpack(&zip).unwrap();
        assert_eq!(got_db.as_slice(), db.as_slice());
        assert_eq!(got_dict.as_deref(), Some(dict.as_ref()));
    }

    #[test]
    fn pack_unpack_empty_db_bytes() {
        let zip = pack(b"", None).unwrap();
        let (got_db, _) = unpack(&zip).unwrap();
        assert!(got_db.is_empty());
    }

    // ── unpack error cases ──────────────────────────────────────────────────────────

    #[test]
    fn unpack_not_a_zip_returns_error() {
        assert!(unpack(b"this is not a zip archive").is_err());
    }

    #[test]
    fn unpack_zip_without_ptm_db_returns_error() {
        // Build a zip that contains only user_words.txt — missing ptm.db must fail.
        let cursor = std::io::Cursor::new(Vec::<u8>::new());
        let mut zw = ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zw.start_file("user_words.txt", opts).unwrap();
        zw.write_all(b"word\n").unwrap();
        let data = zw.finish().unwrap().into_inner();

        let err = unpack(&data).unwrap_err();
        assert!(
            err.to_string().contains("ptm.db"),
            "expected error to mention 'ptm.db', got: {}",
            err
        );
    }

    #[test]
    fn unpack_ignores_unknown_zip_entries() {
        // Extra files in the zip must not cause an error and must not appear in output.
        let cursor = std::io::Cursor::new(Vec::<u8>::new());
        let mut zw = ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zw.start_file("ptm.db", opts).unwrap();
        zw.write_all(b"db data").unwrap();
        zw.start_file("README.txt", opts).unwrap();
        zw.write_all(b"should be silently ignored").unwrap();
        let data = zw.finish().unwrap().into_inner();

        let (db, dict) = unpack(&data).unwrap();
        assert_eq!(db.as_slice(), b"db data");
        assert!(dict.is_none());
    }

    // ── rotate_local_backups ────────────────────────────────────────────────────────

    #[test]
    fn rotate_removes_oldest_when_over_limit() {
        let dir = TempDir::new().unwrap();
        let backup_dir = dir.path().join("backups");
        fs::create_dir_all(&backup_dir).unwrap();

        // Create 7 .zip files with lexicographically ordered timestamps
        for i in 0..7u8 {
            fs::write(
                backup_dir.join(format!("ptm-backup-2025-01-0{}T00-00-00Z.zip", i)),
                b"zip content",
            )
            .unwrap();
        }

        rotate_local_backups(&backup_dir, 5).unwrap();

        let mut remaining: Vec<String> = fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        remaining.sort();

        assert_eq!(remaining.len(), 5, "expected 5 files to survive, got: {:?}", remaining);
        // The two oldest (indices 0 and 1) must be gone; indices 2–6 survive.
        assert_eq!(remaining[0], "ptm-backup-2025-01-02T00-00-00Z.zip");
        assert_eq!(remaining[4], "ptm-backup-2025-01-06T00-00-00Z.zip");
    }

    #[test]
    fn rotate_does_nothing_when_count_at_or_below_limit() {
        let dir = TempDir::new().unwrap();
        let backup_dir = dir.path().join("backups");
        fs::create_dir_all(&backup_dir).unwrap();

        for i in 0..3u8 {
            fs::write(
                backup_dir.join(format!("ptm-backup-2025-01-0{}T00-00-00Z.zip", i)),
                b"zip",
            )
            .unwrap();
        }

        rotate_local_backups(&backup_dir, 5).unwrap();

        assert_eq!(fs::read_dir(&backup_dir).unwrap().count(), 3);
    }

    #[test]
    fn rotate_ignores_non_zip_files() {
        let dir = TempDir::new().unwrap();
        let backup_dir = dir.path().join("backups");
        fs::create_dir_all(&backup_dir).unwrap();

        for i in 0..4u8 {
            fs::write(
                backup_dir.join(format!("ptm-backup-2025-01-0{}T00-00-00Z.zip", i)),
                b"zip",
            )
            .unwrap();
        }
        // A non-.zip file — must not count toward the keep limit.
        fs::write(backup_dir.join("notes.txt"), b"note").unwrap();

        rotate_local_backups(&backup_dir, 5).unwrap();

        // All 4 .zip files are under the limit and must survive.
        let zip_count = fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |x| x == "zip"))
            .count();
        assert_eq!(zip_count, 4);
    }

    // ── create_local_backup ─────────────────────────────────────────────────────────

    #[test]
    fn create_local_backup_produces_readable_zip_containing_db() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("ptm.db"), b"test db content").unwrap();
        let paths = make_paths(&dir);

        let backup_path = create_local_backup(&paths, "2025-01-15T10-30-00Z").unwrap();

        assert!(backup_path.exists());
        assert_eq!(backup_path.extension().unwrap(), "zip");
        assert!(
            backup_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("2025-01-15T10-30-00Z"),
            "filename should embed the timestamp"
        );

        // The zip must be parseable and contain the original db bytes.
        let zip_bytes = fs::read(&backup_path).unwrap();
        let (db, _) = unpack(&zip_bytes).unwrap();
        assert_eq!(db.as_slice(), b"test db content");
    }

    #[test]
    fn create_local_backup_includes_dict_when_present() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("ptm.db"), b"db").unwrap();
        fs::write(dir.path().join("user_words.txt"), b"hello\nworld\n").unwrap();
        let paths = make_paths(&dir);

        let backup_path = create_local_backup(&paths, "2025-01-15T10-30-00Z").unwrap();
        let zip_bytes = fs::read(&backup_path).unwrap();
        let (_, dict) = unpack(&zip_bytes).unwrap();
        assert_eq!(dict.as_deref(), Some(b"hello\nworld\n".as_ref()));
    }

    #[test]
    fn create_local_backup_missing_db_returns_error() {
        let dir = TempDir::new().unwrap();
        let paths = make_paths(&dir); // no ptm.db written
        assert!(create_local_backup(&paths, "2025-01-01T00-00-00Z").is_err());
    }
}
