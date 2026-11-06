//! `md`'s logger.
//!
//! A thin wrapper over the shared logger in `src/log.rs` (finding #21): the two
//! files used to be near-identical copies that had already drifted.  `md` logs
//! to `$XDG_DATA_HOME/md/md.log`; this module selects that directory once and
//! delegates everything else.

#[path = "../../log.rs"]
mod shared;

/// Point the shared logger at `md`'s data directory.
///
/// `main` calls this before any warning can be emitted.  [`warn`] calls it too
/// as belt and braces: it is idempotent, and it keeps a warning emitted during
/// startup — before `main` reaches the call — from landing in `ptm`'s log.
pub fn init() {
    shared::set_app_name("md");
}

/// Append a `WARN` line to `md.log`.
pub fn warn(msg: &str) {
    init();
    shared::warn(msg);
}
