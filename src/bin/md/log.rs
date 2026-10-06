//! Minimal file-based logger — mirrors ptm's log.rs but appends to
//! `$XDG_DATA_HOME/md/md.log` instead of the ptm log file.

use chrono::DateTime;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::SystemTime;

fn log_path() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
                .join(".local")
                .join("share")
        });
    base.join("md").join("md.log")
}

/// Append a `WARN` line to the log file.  Silently swallowed — log failures
/// must never crash the TUI.
pub fn warn(msg: &str) {
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let ts = DateTime::<chrono::Utc>::from(SystemTime::now()).format("%Y-%m-%dT%H:%M:%SZ");
        let _ = writeln!(f, "{ts} WARN  {msg}");
    }
}
