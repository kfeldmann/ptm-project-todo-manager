//! Path helpers shared by every ptm binary (`ptm`, `ptm-sync`, `md`, `enc`).
//!
//! This module is path-included (`#[path = ".../paths.rs"] mod paths;`) by the
//! helper binaries — the same pattern `crypto.rs` and `spell.rs` already use —
//! so each helper exists in exactly one place.  Before that, `ptm-sync` had its
//! own copy of `expand_tilde` that sliced the string at byte 2 (`&path[2..]`),
//! which panicked on a bare `"~"` (finding #5) and silently resolved `"~x"` to
//! `$HOME`.
//!
//! [`data_home`] and [`app_data_dir`] exist for the same reason (finding #19):
//! four copies of the XDG lookup had drifted apart, with three different
//! `$HOME` fallbacks (`.` in two, an error in one, silently skipped in the
//! fourth), so the same program could disagree with itself about where its
//! data lives.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// The XDG base data directory: `$XDG_DATA_HOME`, or `$HOME/.local/share`.
///
/// Returns `None` only when neither variable is set — the caller decides
/// whether that is an error (a private key must not land in `.`) or merely a
/// reason to skip a search path.
pub fn data_home() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        // The XDG spec says a relative `$XDG_DATA_HOME` must be ignored.
        let p = PathBuf::from(xdg);
        if p.is_absolute() {
            return Some(p);
        }
    }
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".local").join("share"))
}

/// The data directory belonging to one of the binaries: `<data_home>/<app>`.
///
/// `app` is `"ptm"`, `"ptm-sync"`'s host application, `"md"`, and so on.  Falls
/// back to a path under the current directory only when neither
/// `$XDG_DATA_HOME` nor `$HOME` is set, matching what `ptm` has always done —
/// the TUI has nowhere else to put its database and refusing to start would be
/// worse than an unusual location.  Callers that must not guess (key files) use
/// [`expand_tilde`] instead, which errors.
pub fn app_data_dir(app: &str) -> PathBuf {
    data_home()
        .unwrap_or_else(|| PathBuf::from(".").join(".local").join("share"))
        .join(app)
}

/// `<data_home>/<app>`, or `None` when there is no data home at all.
///
/// Used where the caller can genuinely carry on without a directory (the
/// spell checker's dictionary search path).
pub fn optional_app_data_dir(app: &str) -> Option<PathBuf> {
    data_home().map(|base| base.join(app))
}

/// Join `file` onto [`app_data_dir`].
pub fn app_data_file(app: &str, file: &str) -> PathBuf {
    app_data_dir(app).join(file)
}

/// Resolve a leading `~` / `~/` to `$HOME`.
///
/// If `$HOME` is unset and `path` needs it, this is an error rather than a
/// silent fallback to the current directory: putting a private key — or a
/// backup source directory — into whatever directory happens to be current is
/// not a safe default.
///
/// A `~` that is not the whole first component (`"~x"`, `"a/~/b"`) is *not*
/// expanded; it stays a literal relative path, as in a POSIX shell.
///
/// Used by `enc` and `ptm-sync`; `ptm` has no user-supplied paths, so it is
/// dead code in that binary.
#[allow(dead_code)]
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

    /// Spawn this test binary with only `test` (and the ignored ones) running,
    /// after `configure` has set up the environment the case needs.
    ///
    /// `std::env::remove_var` would be process-wide and the test harness runs
    /// tests on several threads, so unsetting a variable in-process would race
    /// every other test that reads it.  A child process is the only way to
    /// observe those cases without touching the parent's environment.
    ///
    /// The filter is a bare test name, never a full `paths::tests::…` path:
    /// this file is compiled into all four binaries, and `module_path!()`
    /// prefixes the *crate* name (`md::paths::tests`, `ptm::paths::tests`, …),
    /// so any written-out path matches in at most one of them.  A filter that
    /// matches nothing exits 0 having run nothing, so the caller must assert
    /// that the child really ran — see [`assert_child_ran`].
    fn run_child(test: &str, configure: impl FnOnce(&mut std::process::Command)) -> std::process::Output {
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([test, "--nocapture", "--ignored"]);
        configure(&mut cmd);
        cmd.output().expect("failed to spawn the child test process")
    }

    /// Fail unless the child ran exactly the one test it was asked for.
    fn assert_child_ran(out: &std::process::Output) {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("running 1 test") && stdout.contains("1 passed"),
            "the child ran no test — the filter was wrong:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
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
        let out = run_child("child_reports_unset_home", |cmd| {
            cmd.env_remove("HOME");
        });
        assert_child_ran(&out);
        assert!(
            out.status.success(),
            "the child process expected an error when HOME is unset:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // ── finding #19: the XDG data-directory lookup ────────────────────────────

    /// The child half of [`data_dir_falls_back_to_home`]; not run directly.
    ///
    /// Environment variables are process-wide, so the "neither variable set"
    /// case can only be observed from a child process — the same reasoning as
    /// [`child_reports_unset_home`].
    #[test]
    #[ignore = "spawned by data_dir_falls_back_to_home"]
    fn child_reports_no_data_home() {
        assert!(std::env::var("XDG_DATA_HOME").is_err());
        assert!(std::env::var("HOME").is_err());
        assert!(data_home().is_none(), "no data home without either variable");
        assert!(optional_app_data_dir("ptm").is_none());
        // The lenient form still has to produce something usable: the TUI
        // cannot run without a database path.
        assert_eq!(
            app_data_dir("ptm"),
            PathBuf::from(".").join(".local").join("share").join("ptm")
        );
        assert_eq!(
            app_data_file("ptm", "ptm.db"),
            PathBuf::from(".").join(".local").join("share").join("ptm").join("ptm.db")
        );
    }

    #[test]
    fn data_dir_falls_back_to_home() {
        let out = run_child("child_reports_no_data_home", |cmd| {
            cmd.env_remove("HOME");
            cmd.env_remove("XDG_DATA_HOME");
        });
        assert_child_ran(&out);
        assert!(
            out.status.success(),
            "the child process expected the documented fallbacks:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn app_dirs_are_scoped_to_the_app_name() {
        // `$HOME`/`$XDG_DATA_HOME` are set in the test environment, so this
        // exercises the real lookup.  The point is that the four binaries now
        // derive their directories from one function: `ptm` and `ptm-sync`
        // must agree, and `md`/`enc` must not collide with them.
        let ptm = app_data_dir("ptm");
        assert_eq!(ptm.file_name().unwrap(), "ptm");
        assert_eq!(app_data_file("ptm", "ptm.db"), ptm.join("ptm.db"));
        assert_ne!(app_data_dir("md"), ptm);
        assert_eq!(app_data_dir("md").file_name().unwrap(), "md");
        // `ptm-sync` operates on `ptm`'s directory, not its own.
        assert_eq!(app_data_dir("ptm"), ptm);
    }

    /// The four binaries must resolve the *same* `ptm` directory.  Before
    /// #19 they each had their own copy with a different `$HOME` fallback, so
    /// `ptm-sync` could hash and pack a file `ptm` never wrote.
    #[test]
    fn xdg_data_home_wins_over_home() {
        // A child process is the only way to set `$XDG_DATA_HOME` without
        // racing the other tests (see `run_child`).
        let out = run_child("child_reports_xdg_data_home", |cmd| {
            cmd.env("XDG_DATA_HOME", "/tmp/ptm-xdg-test");
            cmd.env("HOME", "/tmp/ptm-home-test");
        });
        assert_child_ran(&out);
        assert!(
            out.status.success(),
            "the child process expected $XDG_DATA_HOME to win:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The child half of [`xdg_data_home_wins_over_home`]; not run directly.
    #[test]
    #[ignore = "spawned by xdg_data_home_wins_over_home"]
    fn child_reports_xdg_data_home() {
        assert_eq!(data_home().unwrap(), PathBuf::from("/tmp/ptm-xdg-test"));
        assert_eq!(
            app_data_dir("ptm"),
            PathBuf::from("/tmp/ptm-xdg-test").join("ptm")
        );
        // A relative $XDG_DATA_HOME is invalid per the spec and must be
        // ignored in favour of $HOME.
        std::env::set_var("XDG_DATA_HOME", "relative/path");
        assert_eq!(
            data_home().unwrap(),
            PathBuf::from("/tmp/ptm-home-test").join(".local").join("share")
        );
    }
}
