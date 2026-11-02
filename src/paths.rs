//! Path helpers shared by every ptm binary (`ptm`, `ptm-sync`, `enc`).
//!
//! This module is path-included (`#[path = ".../paths.rs"] mod paths;`) by the
//! helper binaries — the same pattern `crypto.rs` and `spell.rs` already use —
//! so `expand_tilde` exists in exactly one place.  Before that, `ptm-sync` had
//! its own copy that sliced the string at byte 2 (`&path[2..]`), which panicked
//! on a bare `"~"` (finding #5) and silently resolved `"~x"` to `$HOME`.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Resolve a leading `~` / `~/` to `$HOME`.
///
/// If `$HOME` is unset and `path` needs it, this is an error rather than a
/// silent fallback to the current directory: putting a private key — or a
/// backup source directory — into whatever directory happens to be current is
/// not a safe default.
///
/// A `~` that is not the whole first component (`"~x"`, `"a/~/b"`) is *not*
/// expanded; it stays a literal relative path, as in a POSIX shell.
pub fn expand_tilde(path: &str) -> Result<PathBuf> {
    if path == "~" || path.starts_with("~/") {
        let home = std::env::var("HOME").with_context(|| {
            format!(
                "HOME is not set, so '{}' cannot be resolved; \
                 pass an explicit path instead",
                path
            )
        })?;
        // `"~"` and `"~/"` both mean the home directory itself; don't let
        // `join("")` leave a trailing separator in the path we report.
        let rest = path.strip_prefix("~/").unwrap_or("");
        return Ok(if rest.is_empty() {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(rest)
        });
    }
    Ok(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_forms() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/key").unwrap(), PathBuf::from(&home).join("key"));
        assert_eq!(expand_tilde("~").unwrap(), PathBuf::from(&home));
        // "~/" alone is the home directory itself, with no trailing separator.
        assert_eq!(expand_tilde("~/").unwrap(), PathBuf::from(&home));
    }

    #[test]
    fn leaves_non_home_forms_alone() {
        assert_eq!(expand_tilde("/abs/key").unwrap(), PathBuf::from("/abs/key"));
        assert_eq!(
            expand_tilde("relative/key").unwrap(),
            PathBuf::from("relative/key")
        );
        // A bare "~" inside a name, or a "~" that is not the whole first
        // component, must not be expanded.
        assert_eq!(expand_tilde("a/~/b").unwrap(), PathBuf::from("a/~/b"));
        // Finding #5: "~x" is a literal path, never $HOME.
        assert_eq!(expand_tilde("~x").unwrap(), PathBuf::from("~x"));
    }

    /// Run a child process with `$HOME` removed.
    ///
    /// `std::env::remove_var` would be process-wide and the test harness runs
    /// tests on several threads, so unsetting it in-process would race every
    /// other test that reads it.  A child process is the only way to observe
    /// the unset case without touching the parent's environment.
    fn run_without_home() -> std::process::Output {
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        // `--exact` plus the test name runs only this test, in a process that
        // inherits the cleared environment.
        cmd.args(["--exact", "paths::tests::child_reports_unset_home", "--nocapture", "--ignored"]);
        cmd.env_remove("HOME");
        cmd.output().expect("failed to spawn the child test process")
    }

    /// The child half of [`errors_when_home_is_unset`]; not run directly.
    #[test]
    #[ignore = "spawned by errors_when_home_is_unset"]
    fn child_reports_unset_home() {
        assert!(std::env::var("HOME").is_err(), "child must start without HOME");
        assert!(expand_tilde("~/key").is_err(), "expected an error when HOME is unset");
        assert!(expand_tilde("~").is_err(), "expected an error when HOME is unset");
        assert!(expand_tilde("/abs/key").is_ok(), "absolute paths must not need HOME");
        assert!(expand_tilde("~x").is_ok(), "a literal ~x must not need HOME");
    }

    #[test]
    fn errors_when_home_is_unset() {
        let out = run_without_home();
        assert!(
            out.status.success(),
            "the child process expected an error when HOME is unset:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
