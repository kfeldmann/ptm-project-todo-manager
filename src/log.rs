/// Minimal file-based logger for use while the TUI is active.
///
/// The TUI owns the terminal in raw mode, so writing to stderr corrupts the
/// display. All warnings and errors are appended to
/// `$XDG_DATA_HOME/ptm/ptm.log` (falling back to `~/.local/share/ptm/ptm.log`)
/// instead, where they can be inspected after the session ends.
use std::io::Write as _;
use std::path::PathBuf;
use std::time::SystemTime;
use chrono::DateTime;

fn log_path() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
                .join(".local")
                .join("share")
        });
    base.join("ptm").join("ptm.log")
}

/// Append a `WARN` line to the log file. Silently does nothing if the file
/// cannot be opened (the TUI must not crash over a logging failure).
pub fn warn(msg: &str) {
    let path = log_path();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let ts = DateTime::<chrono::Utc>::from(SystemTime::now()).format("%Y-%m-%dT%H:%M:%SZ");
        let _ = writeln!(f, "{ts} WARN  {msg}");
    }
}
