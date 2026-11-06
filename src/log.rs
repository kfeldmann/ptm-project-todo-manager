//! Minimal file-based logger for use while a TUI is active.
//!
//! The TUI owns the terminal in raw mode, so writing to stderr corrupts the
//! display.  Warnings and errors are appended to
//! `<data home>/<app>/<app>.log` instead (`$XDG_DATA_HOME/ptm/ptm.log` for
//! `ptm`, `$XDG_DATA_HOME/md/md.log` for `md`), where they can be inspected
//! after the session ends.
//!
//! This file is shared: `md` path-includes it (`#[path = "../../log.rs"] mod
//! log;`) and picks its own directory with [`set_app_name`].  It used to have
//! a near-identical copy in `src/bin/md/log.rs` (finding #21); the two had
//! already drifted — only the `md` copy created the log directory, so the first
//! warning of a fresh install was silently dropped by `ptm`.
//!
//! Logging failures are swallowed by design: a log call must never crash the
//! TUI, and there is nowhere to report the failure anyway.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::SystemTime;
use chrono::DateTime;

/// Sub-directory (and file stem) of the data home this binary logs to.
static APP_NAME: OnceLock<&'static str> = OnceLock::new();

/// Select the data sub-directory used by [`warn`]: `ptm` logs to
/// `ptm/ptm.log`, `md` to `md/md.log`.
///
/// Each binary calls this once at startup.  Calling it again is a no-op — the
/// first value wins, which keeps a stray call from redirecting an
/// already-running process's log mid-session.  When it is never called the
/// default is `"ptm"`, so `ptm` itself has no reason to call it.
#[allow(dead_code)] // only the non-default apps (currently `md`) call it
pub fn set_app_name(app: &'static str) {
    let _ = APP_NAME.set(app);
}

fn app_name() -> &'static str {
    APP_NAME.get().copied().unwrap_or("ptm")
}

/// `<data home>/<app>/<app>.log`, or `None` when there is no data home.
fn log_path() -> Option<PathBuf> {
    let app = app_name();
    crate::paths::optional_app_data_dir(app).map(|dir| dir.join(format!("{app}.log")))
}

/// Append a `WARN` line to the log file.
///
/// Silently does nothing if the directory or file cannot be created (the TUI
/// must not crash over a logging failure).  The directory is created on demand:
/// on a fresh install it does not exist yet, and the alternative is losing the
/// first message — which is usually the one explaining why nothing works.
pub fn warn(msg: &str) {
    let Some(path) = log_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let ts = DateTime::<chrono::Utc>::from(SystemTime::now()).format("%Y-%m-%dT%H:%M:%SZ");
        let _ = writeln!(f, "{ts} WARN  {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `set_app_name` is process-wide and one-shot, so the tests that need a
    /// different app name run in a child process (see below).
    ///
    /// The test is selected by name *without* `--exact`, not by its full
    /// module path: this file is path-included as `log::shared` in `md` and as
    /// `log` in `ptm`, and `module_path!()` prefixes the crate name as well, so
    /// a written-out path silently selects nothing in one of the binaries.  A
    /// child that runs no test still exits 0, which would make the parent check
    /// vacuous — [`md_logs_under_its_own_directory`] therefore asserts that
    /// exactly one test ran.
    fn run_child(test: &str, app: &str, data_home: &std::path::Path) -> std::process::Output {
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([test, "--nocapture", "--ignored"]);
        cmd.env("PTM_TEST_APP_NAME", app);
        cmd.env("XDG_DATA_HOME", data_home);
        cmd.output().expect("failed to spawn the child test process")
    }

    /// The path is derived from the data home and the app name — the two
    /// copies this replaced disagreed on both (finding #21).
    #[test]
    fn log_path_is_app_scoped() {
        let path = log_path().expect("$HOME or $XDG_DATA_HOME must be set for tests");
        assert_eq!(path.file_name().unwrap(), "ptm.log");
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "ptm");
    }

    /// The child half of [`md_logs_under_its_own_directory`]; not run directly.
    #[test]
    #[ignore = "spawned by md_logs_under_its_own_directory"]
    fn child_writes_to_the_app_directory() {
        let app = std::env::var("PTM_TEST_APP_NAME")
            .expect("this test is only meaningful when spawned by run_child");
        let app: &'static str = Box::leak(app.into_boxed_str());
        set_app_name(app);

        let expected = crate::paths::optional_app_data_dir(app)
            .unwrap()
            .join(format!("{app}.log"));
        assert_eq!(log_path().unwrap(), expected);

        // The directory does not exist yet (a fresh install); `warn` must
        // create it rather than silently dropping the message.  This is the
        // drift the old `ptm` copy had: it never created the directory, so the
        // first warning of a new install was lost.
        assert!(!expected.parent().unwrap().exists());
        warn("fresh install");
        assert!(expected.exists(), "warn must create the log directory");
        let contents = std::fs::read_to_string(&expected).unwrap();
        assert!(contents.contains("WARN  fresh install"), "got: {contents}");
    }

    #[test]
    fn md_logs_under_its_own_directory() {
        let dir = std::env::temp_dir().join(format!("ptm-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out = run_child("child_writes_to_the_app_directory", "md", &dir);
        let _ = std::fs::remove_dir_all(&dir);
        // A filter that matches nothing exits 0 and runs nothing, which would
        // make this test pass without checking anything.
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("running 1 test") && stdout.contains("1 passed"),
            "the child ran no test — the filter was wrong:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "the child process expected md/md.log:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
