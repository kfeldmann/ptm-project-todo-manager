#!/usr/bin/env python3
"""
query-count.py — reproductions for findings #10 and #11.

  10  `repo::list_projects` ran two extra statements *per project row*
      (`load_project_tags` + `load_soonest_reminder`), and
      `repo::list_todos_global` then ran one more per project for its todos.
      `reload_project_list` / `reload_todo_list` are called on every keystroke
      of the search box, so typing a 10-character query at 30 projects cost
      ~1000 statements.
  11  `rename_tag` probed for a name collision with
      `.map(|n| n > 0).unwrap_or(false)`, so *any* SQL failure — a locked
      database, a corrupt index, a denied read — was indistinguishable from
      "no collision": the rename proceeded anyway.

FAIL means the finding is live.

## How this is checked, and why it is shaped this way

Both findings are about *how many statements a call issues* and *whether an
error is propagated*, neither of which is visible from the outside: the binary
still shows the right list, and the UI still says "already exists". A
disk-state harness therefore cannot see either bug.

The check compiles the **real `src/data/repo.rs`** (via `#[path]`, so it is the
same file the binary builds) into a small Rust program and:

  * counts statements with SQLite's own `SQLITE_TRACE_STMT` callback, which
    fires once per statement *execution* — a re-used prepared statement fires
    again, so a fix cannot pass by caching inside rusqlite;
  * injects a failure with a SQLite authorizer (`hooks` feature) that denies
    reads of `tags.name` — the column the collision probe reads, and the one
    column the `UPDATE ... SET name = ?` does *not* read, so the rename itself
    would still succeed if the probe's error were swallowed.

Both are measured, not assumed: the statement counts are printed, and the #11
check would report PASS on a tree where `rename_tag` still swallows the error
only if the authorizer stopped firing.

    python3 dev-harness/query-count.py
"""

import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import ptmdb  # noqa: E402

PROFILE = os.environ.get("PTM_PROFILE", "debug")
results = ptmdb.Results("query-count")

REPO_SRC = ptmdb.REPO / "src"
DB_SRC = REPO_SRC / "data/db.rs"


def schema_const():
    """The real `SCHEMA` constant from `src/data/db.rs`, made public.

    The probe needs the schema to create its in-memory database. Lifting the
    constant textually (rather than re-typing the DDL here) means the harness
    cannot drift from the schema the binary uses; `const SCHEMA` becomes
    `pub const SCHEMA` so the `#[path]`-included module can be called from
    `main`.
    """
    src = DB_SRC.read_text()
    start = src.index('const SCHEMA: &str = r#"')
    end = src.index('"#;', start) + len('"#;')
    return src[start:end].replace("const SCHEMA", "pub const SCHEMA", 1)


def cargo_available():
    try:
        subprocess.run(["cargo", "--version"], capture_output=True, check=True)
        return True
    except (OSError, subprocess.CalledProcessError):
        return False


def write_probe(root, name, body, call):
    """A `[[bin]]` in a scratch crate that `#[path]`-includes the real repo.rs.

    Using a scratch crate (rather than `rustc` directly) is what makes it
    possible to depend on rusqlite: the crate is created inside the workspace's
    own `target/` cache so the dependency is already built and the check stays
    fast.
    """
    crate = root / f"probe_{name}"
    (crate / "src").mkdir(parents=True, exist_ok=True)

    # The repo module needs the rest of the crate's module tree (`crate::log`,
    # `crate::data::models`). Mirror just enough of it.
    # `data/db.rs` is path-included too, so the probe's schema comes from the
    # real file rather than a copy. `db.rs` only needs `anyhow` and `rusqlite`.
    (crate / "src/db_schema.rs").write_text(
        f'#[path = "{REPO_SRC}/data/db.rs"]\nmod db_impl;\n'
        + schema_const()
    )
    (crate / "src/main.rs").write_text(
        f'''#[path = "{REPO_SRC}/log.rs"]
mod log;
#[path = "{REPO_SRC}/data/mod.rs"]
mod data;
#[path = "db_schema.rs"]
mod db_schema;

{body}

fn main() {{
{call}
}}
'''
    )
    (crate / "Cargo.toml").write_text(
        f'''[package]
name = "probe_{name}"
version = "0.0.0"
edition = "2021"

[dependencies]
rusqlite = {{ version = "0.40.2", default-features = false, features = ["bundled", "trace", "hooks"] }}
uuid = {{ version = "1.26.0", features = ["v4"] }}
chrono = {{ version = "0.4.45", default-features = false, features = ["std"] }}
anyhow = "1.0"

[[bin]]
name = "probe_{name}"
path = "src/main.rs"
'''
    )
    return crate


def build_and_run(root, name, body, call):
    crate = write_probe(root, name, body, call)
    env = dict(os.environ, CARGO_TARGET_DIR=str(Path(ptmdb.REPO) / "target"))
    cp = subprocess.run(
        ["cargo", "run", "--quiet", "--manifest-path", str(crate / "Cargo.toml")],
        capture_output=True, text=True, env=env, timeout=600,
    )
    return cp.returncode == 0, cp.stdout, cp.stderr


# Shared scaffolding: an in-memory database, and a statement counter driven by
# SQLite's trace callback.
PROBE_PRELUDE = r'''
use rusqlite::{params, Connection};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn mk_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory db");
    conn.execute_batch("PRAGMA foreign_keys=ON;").expect("fk pragma");
    conn.execute_batch(crate::db_schema::SCHEMA).expect("schema");
    conn
}

fn count_statements<T>(conn: &Connection, f: impl FnOnce() -> T) -> (T, usize) {
    unsafe extern "C" fn cb(
        _evt: std::ffi::c_uint,
        ctx: *mut std::ffi::c_void,
        _stmt: *mut std::ffi::c_void,
        _sql: *mut std::ffi::c_void,
    ) -> std::ffi::c_int {
        let c = unsafe { &*(ctx as *const AtomicUsize) };
        c.fetch_add(1, Ordering::Relaxed);
        0
    }
    let counter = Arc::new(AtomicUsize::new(0));
    let handle = unsafe { conn.handle() };
    let rc = unsafe {
        rusqlite::ffi::sqlite3_trace_v2(
            handle, 1, Some(cb), Arc::as_ptr(&counter) as *mut std::ffi::c_void,
        )
    };
    assert_eq!(rc, 0);
    let before = counter.load(Ordering::Relaxed);
    let out = f();
    let after = counter.load(Ordering::Relaxed);
    unsafe { rusqlite::ffi::sqlite3_trace_v2(handle, 0, None, std::ptr::null_mut()) };
    (out, after - before)
}
'''


def check_statement_counts(root):
    print("\n[10] list_projects / list_todos_global must issue a constant number of statements")

    call = r'''
    let conn = mk_conn();
    let mut first = None;
    let mut last = None;
    for n in [1usize, 5, 30] {
        // Rebuild from scratch so each measurement is the same workload at a
        // different size.
        let conn2 = mk_conn();
        for i in 0..n {
            let p = crate::data::repo::create_project(&conn2, &format!("P{i}")).unwrap();
            crate::data::repo::tag_project(&conn2, &p.id, "work").unwrap();
            let t = crate::data::repo::create_todo(&conn2, Some(&p.id), "t").unwrap();
            crate::data::repo::set_todo_reminder(&conn2, &t.id, Some("2025-06-01")).unwrap();
        }
        let (projects, stmts) = count_statements(&conn2, || {
            crate::data::repo::list_projects(&conn2, false, None, None).unwrap()
        });
        assert_eq!(projects.len(), n);
        // The output must still be complete: the count is only meaningful if
        // the call still does its job.
        assert_eq!(projects[0].tags.len(), 1);
        assert_eq!(projects[0].soonest_reminder.as_deref(), Some("2025-06-01"));
        println!("list_projects {n} projects -> {stmts} statements");
        if n == 1 { first = Some(stmts); }
        if n == 30 { last = Some(stmts); }
    }
    println!("CONST {}", first == last);

    // Same for the global todo view.
    let mut gfirst = None;
    let mut glast = None;
    for n in [1usize, 5, 30] {
        let conn2 = mk_conn();
        for i in 0..n {
            let p = crate::data::repo::create_project(&conn2, &format!("P{i}")).unwrap();
            crate::data::repo::create_todo(&conn2, Some(&p.id), "t").unwrap();
            crate::data::repo::create_todo(&conn2, None, "inbox").unwrap();
        }
        let (groups, stmts) = count_statements(&conn2, || {
            crate::data::repo::list_todos_global(&conn2, false, None, None, false).unwrap()
        });
        let todos: usize = groups.iter().map(|g| g.todos.len()).sum();
        assert_eq!(todos, n * 2, "output incomplete");
        println!("list_todos_global {n} projects -> {stmts} statements");
        if n == 1 { gfirst = Some(stmts); }
        if n == 30 { glast = Some(stmts); }
    }
    println!("GCONST {}", gfirst == glast);
    '''

    ok, stdout, stderr = build_and_run(root, "counts", PROBE_PRELUDE, call)
    if not ok:
        results.check(False, "the probe compiled against the real repo.rs", stderr[-2000:])
        return

    print("        " + "\n        ".join(stdout.strip().splitlines()))

    counts = {
        (m.group(1), int(m.group(2)), int(m.group(3)))
        for m in __import__("re").finditer(
            r"(list_projects|list_todos_global) (\d+) projects -> (\d+) statements", stdout
        )
    }
    # Re-derive with named fields for a readable failure message.
    import re
    def series(fn):
        return {int(n): int(s) for f, n, s in counts if f == fn}

    lp = series("list_projects")
    lg = series("list_todos_global")

    results.check(
        len(set(lp.values())) == 1 and lp.get(1) is not None,
        "list_projects issues the same number of statements at 1, 5 and 30 projects  "
        f"[FAIL = finding #10 is live: {lp}]",
        f"statement counts by project count: {lp}",
    )
    results.check(
        len(set(lg.values())) == 1 and lg.get(1) is not None,
        "list_todos_global issues the same number of statements at 1, 5 and 30 projects  "
        f"[FAIL = finding #10 is live: {lg}]",
        f"statement counts by project count: {lg}",
    )
    # A constant count could in principle be a constant *large* count; pin the
    # absolute value so a regression to "3 queries per row, cached" is caught.
    results.check(
        lp.get(30, 99) <= 3,
        f"list_projects stays at a handful of statements at 30 projects (got {lp.get(30)})",
    )
    results.check(
        lg.get(30, 99) <= 4,
        f"list_todos_global stays at a handful of statements at 30 projects (got {lg.get(30)})",
    )


def check_rename_tag_error(root):
    print("\n[11] rename_tag must not treat a failed collision probe as 'no collision'")

    call = r'''
    use rusqlite::hooks::{AuthAction, Authorization};

    let conn = mk_conn();
    let p = crate::data::repo::create_project(&conn, "P").unwrap();
    let tag_id = crate::data::repo::tag_project(&conn, &p.id, "old-name").unwrap();

    // Deny reads of `tags.name` only: the collision probe reads it, the
    // `UPDATE tags SET name = ?` writes it without reading it.
    conn.authorizer(Some(|ctx: rusqlite::hooks::AuthContext<'_>| {
        match ctx.action {
            AuthAction::Read { table_name: "tags", column_name: "name" } => Authorization::Deny,
            _ => Authorization::Allow,
        }
    })).unwrap();

    // The probe must fail...
    let denied: Result<i64, _> =
        conn.query_row("SELECT COUNT(*) FROM tags WHERE LOWER(name) = LOWER('x')", [], |r| r.get(0));
    println!("PROBE_DENIED {}", denied.is_err());

    // ...so rename_tag must return an error rather than renaming.
    let outcome = crate::data::repo::rename_tag(&conn, &tag_id, "new-name");
    println!("RENAME_ERR {}", outcome.is_err());
    println!("RENAME_MSG {}", outcome.as_ref().err().map(|e| e.to_string()).unwrap_or_default());

    // Did it rename anyway? Ask directly, with the authorizer removed.
    conn.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>).ok();
    let _ = conn.authorizer(Some(|_ctx: rusqlite::hooks::AuthContext<'_>| Authorization::Allow));
    let now: String = conn
        .query_row("SELECT name FROM tags WHERE id = ?1", params![tag_id], |r| r.get(0))
        .unwrap();
    println!("NAME_AFTER {now}");
    '''

    ok, stdout, stderr = build_and_run(root, "renametag", PROBE_PRELUDE, call)
    if not ok:
        results.check(False, "the rename_tag probe compiled against the real repo.rs", stderr[-2000:])
        return

    print("        " + "\n        ".join(stdout.strip().splitlines()))
    vals = dict(
        line.split(" ", 1) for line in stdout.strip().splitlines() if " " in line
    )

    probe_denied = vals.get("PROBE_DENIED") == "true"
    results.check(
        probe_denied,
        "the authorizer really denies the probe (otherwise this check proves nothing)",
        "the collision probe's SELECT did not fail, so the scenario was not set up",
    )
    if not probe_denied:
        return

    results.check(
        vals.get("RENAME_ERR") == "true",
        "rename_tag reports the failure instead of treating it as 'no collision'  "
        "[FAIL = finding #11 is live]",
        f"rename_tag returned Ok; tag is now {vals.get('NAME_AFTER')!r}",
    )
    results.check(
        "already exists" not in vals.get("RENAME_MSG", ""),
        "a database failure is not reported as a name collision",
        f"message: {vals.get('RENAME_MSG')!r}",
    )
    results.check(
        vals.get("NAME_AFTER") == "old-name",
        "the tag was not renamed by a call that reported an error",
        f"tag name is {vals.get('NAME_AFTER')!r}",
    )


def main():
    if not cargo_available():
        print("SKIP  cargo is not available; cannot build the probe")
        return 0
    with tempfile.TemporaryDirectory(prefix="ptm-query-count-") as tmp:
        root = Path(tmp)
        check_statement_counts(root)
        check_rename_tag_error(root)
    return results.finish()


if __name__ == "__main__":
    sys.exit(main())
